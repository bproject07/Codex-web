use crate::{
    routes::AppState,
    sharing::{Approval, Guest},
    sharing_store::{Error, Result},
};
use axum::{
    Json, Router,
    body::Bytes,
    extract::{
        ConnectInfo, DefaultBodyLimit, Path, Query, State,
        ws::{Message, WebSocketUpgrade},
    },
    http::{HeaderMap, StatusCode, header},
    middleware,
    response::{IntoResponse, Response},
    routing::{delete, get, post},
};
use futures_util::StreamExt;
use serde::Deserialize;
use std::net::SocketAddr;
use uuid::Uuid;

impl IntoResponse for Error {
    fn into_response(self) -> Response {
        let status = match self.0 {
            "invalid_credential" | "access_ended" | "invalid_invitation" => {
                StatusCode::UNAUTHORIZED
            }
            "access_denied" | "awaiting_admission" | "view_only" => StatusCode::FORBIDDEN,
            "not_found" => StatusCode::NOT_FOUND,
            "rate_limited" | "connection_limit" => StatusCode::TOO_MANY_REQUESTS,
            "storage_unavailable"
            | "storage_busy"
            | "sharing_closed"
            | "unsafe_state_directory"
            | "unsafe_state_file"
            | "sharing_store_in_use"
            | "sharing_schema_too_new"
            | "storage_maintenance" => StatusCode::SERVICE_UNAVAILABLE,
            "invalid_mode"
            | "invalid_name"
            | "invalid_proposal"
            | "invalid_body"
            | "invalid_cursor"
            | "single_line_only"
            | "confirm_empty_prompt"
            | "invalid_storage_policy"
            | "confirmation_required" => StatusCode::BAD_REQUEST,
            _ => StatusCode::CONFLICT,
        };
        (
            status,
            Json(serde_json::json!({"error":self.0,"code":self.0})),
        )
            .into_response()
    }
}
pub fn owner_router() -> Router<AppState> {
    Router::new()
        .route("/api/sharing/storage", get(storage_status))
        .route("/api/sharing/storage/repair", post(repair_storage))
        .route("/api/sharing/storage/compact", post(compact_storage))
        .route("/api/sharing/storage/policy", post(storage_policy))
        .route(
            "/api/sharing/storage/cleanup/preview",
            post(cleanup_preview),
        )
        .route("/api/sharing/storage/cleanup", post(cleanup_storage))
        .route(
            "/api/sessions/{id}/sharing",
            get(owner_view).delete(revoke_all),
        )
        .route(
            "/api/sessions/{id}/sharing/record",
            post(record).delete(stop_recording),
        )
        .route("/api/sessions/{id}/sharing/status", get(indicator))
        .route("/api/sharing/prune-retired", post(prune_retired))
        .route("/api/sessions/{id}/sharing/invitations", post(invite))
        .route(
            "/api/sessions/{id}/sharing/guests/{guest}/admit",
            post(admit),
        )
        .route("/api/sessions/{id}/sharing/guests/{guest}", delete(revoke))
        .route(
            "/api/sessions/{id}/sharing/proposals/{proposal}/approve",
            post(approve),
        )
        .route(
            "/api/sessions/{id}/sharing/proposals/{proposal}/reject",
            post(reject),
        )
        .route("/api/sharing/archives", get(owner_archives))
        .route(
            "/api/sharing/archives/{generation}",
            get(owner_archive).delete(delete_archive),
        )
        .route(
            "/api/sharing/archives/{generation}/proposals",
            get(archive_proposals),
        )
        .layer(DefaultBodyLimit::max(64 * 1024))
        .layer(middleware::map_response(no_store))
}
pub fn guest_router() -> Router<AppState> {
    Router::new()
        .route("/api/share/redeem", post(redeem))
        .route("/api/share/me", get(guest_view))
        .route("/api/share/proposals", post(propose))
        .route("/api/share/archives", get(guest_archives))
        .route("/api/share/archives/{generation}", get(guest_archive))
        .route("/ws/share", get(upgrade))
        .layer(DefaultBodyLimit::max(64 * 1024))
        .layer(middleware::map_response(no_store))
}
async fn no_store(mut response: Response) -> Response {
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("no-store"),
    );
    response.headers_mut().insert(
        header::REFERRER_POLICY,
        header::HeaderValue::from_static("no-referrer"),
    );
    response
}
fn parse<T: serde::de::DeserializeOwned>(body: &[u8]) -> Result<T> {
    if body.len() > 64 * 1024 {
        return Err(Error("invalid_body"));
    }
    serde_json::from_slice(body).map_err(|_| Error("invalid_body"))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct InviteRequest {
    mode: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RedeemRequest {
    invitation: String,
    credential: String,
    name: String,
    previous_credential: Option<String>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ProposalRequest {
    request_id: Uuid,
    text: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RejectRequest {
    revision: i64,
}
#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct ArchiveQuery {
    #[serde(default)]
    after: i64,
}

async fn owner_view(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<serde_json::Value>> {
    Ok(Json(state.sharing.owner_view(id).await?))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Confirmation {
    confirm: bool,
}

async fn storage_status(State(state): State<AppState>) -> Result<Json<serde_json::Value>> {
    Ok(Json(state.sharing.storage_status().await?))
}

async fn repair_storage(State(state): State<AppState>, body: Bytes) -> Result<StatusCode> {
    if !parse::<Confirmation>(&body)?.confirm {
        return Err(Error("confirmation_required"));
    }
    state.sharing.repair_storage().await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn compact_storage(
    State(state): State<AppState>,
    body: Bytes,
) -> Result<Json<serde_json::Value>> {
    if !parse::<Confirmation>(&body)?.confirm {
        return Err(Error("confirmation_required"));
    }
    Ok(Json(state.sharing.store()?.compact().await?))
}

async fn storage_policy(State(state): State<AppState>, body: Bytes) -> Result<StatusCode> {
    let policy: crate::sharing_storage::Policy = parse(&body)?;
    state
        .sharing
        .store()?
        .run(move |db| {
            crate::sharing_storage::save_policy(db, &policy)?;
            crate::sharing_storage::retention(db)
        })
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn cleanup_preview(
    State(state): State<AppState>,
    body: Bytes,
) -> Result<Json<crate::sharing_storage::CleanupPreview>> {
    let filter: crate::sharing_storage::CleanupFilter = parse(&body)?;
    Ok(Json(
        state
            .sharing
            .store()?
            .run(move |db| crate::sharing_storage::preview(db, &filter))
            .await?,
    ))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CleanupRequest {
    filter: crate::sharing_storage::CleanupFilter,
    fingerprint: String,
}

async fn cleanup_storage(
    State(state): State<AppState>,
    body: Bytes,
) -> Result<Json<crate::sharing_storage::CleanupPreview>> {
    let request: CleanupRequest = parse(&body)?;
    if request.fingerprint.len() != 64 {
        return Err(Error("invalid_body"));
    }
    Ok(Json(
        state
            .sharing
            .store()?
            .run(move |db| {
                crate::sharing_storage::enqueue_cleanup(db, &request.filter, &request.fingerprint)
            })
            .await?,
    ))
}
async fn record(State(state): State<AppState>, Path(id): Path<Uuid>) -> Result<StatusCode> {
    state.sharing.record(id)?;
    Ok(StatusCode::NO_CONTENT)
}
async fn stop_recording(State(state): State<AppState>, Path(id): Path<Uuid>) -> Result<StatusCode> {
    state.sharing.stop_recording(id)?;
    Ok(StatusCode::NO_CONTENT)
}
async fn indicator(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<serde_json::Value>> {
    Ok(Json(state.sharing.indicator(id).await?))
}
async fn prune_retired(State(state): State<AppState>) -> Result<Json<serde_json::Value>> {
    Ok(Json(state.sharing.prune_retired().await?))
}
async fn invite(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    body: Bytes,
) -> Result<Json<serde_json::Value>> {
    let request: InviteRequest = parse(&body)?;
    Ok(Json(state.sharing.invite(id, request.mode).await?))
}
async fn admit(
    State(state): State<AppState>,
    Path((id, guest)): Path<(Uuid, Uuid)>,
) -> Result<StatusCode> {
    state.sharing.admit(id, guest).await?;
    Ok(StatusCode::NO_CONTENT)
}
async fn revoke(
    State(state): State<AppState>,
    Path((id, guest)): Path<(Uuid, Uuid)>,
) -> Result<StatusCode> {
    state.sharing.revoke(id, Some(guest)).await?;
    Ok(StatusCode::NO_CONTENT)
}
async fn revoke_all(State(state): State<AppState>, Path(id): Path<Uuid>) -> Result<StatusCode> {
    state.sharing.revoke(id, None).await?;
    Ok(StatusCode::NO_CONTENT)
}
async fn approve(
    State(state): State<AppState>,
    Path((id, proposal)): Path<(Uuid, Uuid)>,
    body: Bytes,
) -> Result<Json<serde_json::Value>> {
    let request: Approval = parse(&body)?;
    Ok(Json(state.sharing.approve(id, proposal, request).await?))
}
async fn reject(
    State(state): State<AppState>,
    Path((id, proposal)): Path<(Uuid, Uuid)>,
    body: Bytes,
) -> Result<Json<serde_json::Value>> {
    let request: RejectRequest = parse(&body)?;
    Ok(Json(
        state.sharing.reject(id, proposal, request.revision).await?,
    ))
}
async fn redeem(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    body: Bytes,
) -> Result<Json<Guest>> {
    state.sharing.check_attempt(peer.ip())?;
    let request: RedeemRequest = parse(&body)?;
    match state
        .sharing
        .redeem(
            request.invitation,
            request.credential,
            request.name,
            request.previous_credential,
        )
        .await
    {
        Ok(guest) => Ok(Json(guest)),
        Err(error) => {
            if matches!(error.0, "invalid_credential" | "invalid_invitation") {
                state.sharing.failed_attempt(peer.ip());
            }
            Err(error)
        }
    }
}
async fn guest(state: &AppState, headers: &HeaderMap, peer: SocketAddr) -> Result<Guest> {
    state.sharing.check_attempt(peer.ip())?;
    let token = headers
        .get(header::AUTHORIZATION)
        .and_then(|h| h.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer "))
        .filter(|s| s.len() <= 128)
        .ok_or(Error("invalid_credential"))?;
    match state.sharing.authenticate(token.to_owned()).await {
        Ok(guest) => Ok(guest),
        Err(error) => {
            if matches!(error.0, "invalid_credential") {
                state.sharing.failed_attempt(peer.ip());
            }
            Err(error)
        }
    }
}
async fn guest_view(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>> {
    let guest = guest(&state, &headers, peer).await?;
    Ok(Json(state.sharing.guest_view(guest).await?))
}
async fn propose(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<serde_json::Value>> {
    let guest = guest(&state, &headers, peer).await?;
    let request: ProposalRequest = parse(&body)?;
    Ok(Json(
        state
            .sharing
            .propose(guest, request.request_id, request.text)
            .await?,
    ))
}
async fn owner_archives(State(state): State<AppState>) -> Result<Json<serde_json::Value>> {
    Ok(Json(state.sharing.archives(None).await?))
}
async fn owner_archive(
    State(state): State<AppState>,
    Path(generation): Path<Uuid>,
    Query(query): Query<ArchiveQuery>,
) -> Result<Json<serde_json::Value>> {
    Ok(Json(
        state.sharing.archive(None, generation, query.after).await?,
    ))
}
async fn delete_archive(
    State(state): State<AppState>,
    Path(generation): Path<Uuid>,
) -> Result<StatusCode> {
    state.sharing.delete_archive(generation).await?;
    Ok(StatusCode::NO_CONTENT)
}
async fn archive_proposals(
    State(state): State<AppState>,
    Path(generation): Path<Uuid>,
) -> Result<Json<serde_json::Value>> {
    Ok(Json(state.sharing.archive_proposals(generation).await?))
}
async fn guest_archives(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>> {
    let guest = guest(&state, &headers, peer).await?;
    Ok(Json(state.sharing.archives(Some(guest)).await?))
}
async fn guest_archive(
    State(state): State<AppState>,
    Path(generation): Path<Uuid>,
    Query(query): Query<ArchiveQuery>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>> {
    let guest = guest(&state, &headers, peer).await?;
    Ok(Json(
        state
            .sharing
            .archive(Some(guest), generation, query.after)
            .await?,
    ))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Authenticate {
    credential: String,
}
async fn upgrade(
    websocket: WebSocketUpgrade,
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Response {
    if !crate::auth::origin_is_allowed(
        headers.get(header::ORIGIN).and_then(|h| h.to_str().ok()),
        headers.get(header::HOST).and_then(|h| h.to_str().ok()),
        state.config.host,
    ) {
        return StatusCode::FORBIDDEN.into_response();
    }
    if let Err(error) = state.sharing.check_attempt(peer.ip()) {
        return error.into_response();
    }
    websocket
        .max_message_size(4096)
        .max_frame_size(4096)
        .on_upgrade(move |mut socket| async move {
            // Guest secrets travel in the first bounded frame, never in the URL.
            let initial =
                tokio::time::timeout(std::time::Duration::from_secs(5), socket.next()).await;
            let Ok(Some(Ok(Message::Text(text)))) = initial else {
                return;
            };
            let Ok(request) = parse::<Authenticate>(text.as_bytes()) else {
                return;
            };
            let guest = match state.sharing.authenticate(request.credential).await {
                Ok(guest) => guest,
                Err(_) => {
                    state.sharing.failed_attempt(peer.ip());
                    return;
                }
            };
            let Ok(connection) = state.sharing.connect(guest.clone()).await else {
                return;
            };
            crate::websocket::serve_guest(socket, state, peer, guest, connection).await;
        })
        .into_response()
}
