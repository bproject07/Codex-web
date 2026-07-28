use std::{path::PathBuf, sync::Arc};

use axum::{
    Json, Router,
    body::Bytes,
    extract::{Path, State},
    http::{HeaderValue, StatusCode, header},
    middleware::{self, Next},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;
use tower_http::services::{ServeDir, ServeFile};

use crate::{
    agents::{AgentCatalog, AgentCatalogResponse},
    auth::{self, AuthState},
    config::{AgentKind, Config},
    filesystem::{
        DirectoryBrowser, DirectoryError, DirectoryListing, FilesystemRoots, decode_directory_id,
    },
    peer::{PeerBroker, PeerStatus, SessionPurpose},
    peer_routes,
    registry::{RegistryError, SessionRegistry},
    session::SessionSnapshot,
    updater::{UpdateManager, UpdateStatus},
    websocket,
    workspaces::{WorkspaceError, WorkspaceLibrary, WorkspaceStore},
};
use uuid::Uuid;

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    pub auth: AuthState,
    pub sessions: SessionRegistry,
    pub peers: PeerBroker,
    pub agents: AgentCatalog,
    pub directories: DirectoryBrowser,
    pub workspaces: WorkspaceStore,
    pub updates: UpdateManager,
    pub shutdown: CancellationToken,
    pub readiness_nonce: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct HealthResponse {
    status: &'static str,
    codex_installed: bool,
    session_running: bool,
    connected_clients: usize,
    session_count: usize,
    running_sessions: usize,
    max_sessions: usize,
    server_version: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    readiness_nonce: Option<String>,
}

#[derive(Debug, Serialize)]
struct ErrorResponse {
    error: &'static str,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
#[serde(rename_all = "camelCase")]
struct CreateSessionRequest {
    agent: Option<AgentKind>,
    directory_id: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
#[serde(rename_all = "camelCase")]
struct ListDirectoryRequest {
    directory_id: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct ResolveDirectoryRequest {
    path: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
#[serde(rename_all = "camelCase")]
struct UpsertFavoriteRequest {
    directory_id: String,
    label: Option<String>,
    preferred_agent: Option<AgentKind>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
#[serde(rename_all = "camelCase")]
struct ApplyUpdateRequest {
    expected_version: String,
    confirm_session_termination: bool,
}

const MAX_CREATE_SESSION_BODY: usize = 256 * 1024;
const MAX_DIRECTORY_ID_BODY: usize = 256 * 1024;
const MAX_RESOLVE_PATH_BODY: usize = 256 * 1024;
const MAX_FAVORITE_BODY: usize = 256 * 1024;
const MAX_UPDATE_BODY: usize = 16 * 1024;

pub fn build_router(state: AppState, static_directory: Option<PathBuf>) -> Router {
    let protected_api = Router::new()
        .route("/api/health", get(health))
        .route("/api/agents", get(agents))
        .route("/api/agent-catalog", get(agent_catalog))
        .route("/api/update", get(update_status))
        .route("/api/update/check", post(check_for_update))
        .route("/api/update/apply", post(apply_update))
        .route("/api/filesystem/roots", get(filesystem_roots))
        .route("/api/filesystem/list", post(list_directory))
        .route("/api/filesystem/resolve", post(resolve_directory))
        .route("/api/workspaces", get(workspaces))
        .route(
            "/api/workspaces/favorites",
            axum::routing::put(upsert_favorite),
        )
        .route(
            "/api/workspaces/favorites/{id}",
            axum::routing::delete(delete_favorite),
        )
        .route("/api/session", get(primary_session))
        .route("/api/session/restart", post(restart_primary_session))
        .route("/api/session/terminate", post(terminate_primary_session))
        .route("/api/sessions", get(sessions).post(create_session))
        .route(
            "/api/sessions/{id}",
            get(session_by_id).delete(delete_session),
        )
        .route("/api/sessions/{id}/restart", post(restart_session))
        .route("/api/sessions/{id}/terminate", post(terminate_session))
        .merge(peer_routes::protected_router())
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            auth::require_http_auth,
        ));

    let router = Router::new()
        .merge(protected_api)
        .route("/ws", get(websocket::upgrade))
        .with_state(state);

    let router = match static_directory {
        Some(directory) => {
            let index = directory.join("index.html");
            router
                .fallback_service(ServeDir::new(directory).not_found_service(ServeFile::new(index)))
        }
        None => router.fallback(development_fallback),
    };

    router.layer(middleware::from_fn(security_headers))
}

async fn health(State(state): State<AppState>) -> Json<HealthResponse> {
    let running_sessions = state.sessions.running_sessions();
    Json(HealthResponse {
        status: "ok",
        codex_installed: state.sessions.codex_installed(),
        session_running: running_sessions > 0,
        connected_clients: state.sessions.connected_clients(),
        session_count: state.sessions.session_count(),
        running_sessions,
        max_sessions: state.sessions.max_sessions(),
        server_version: env!("CARGO_PKG_VERSION"),
        readiness_nonce: state.readiness_nonce.clone(),
    })
}

async fn update_status(State(state): State<AppState>) -> Json<UpdateStatus> {
    Json(state.updates.status().await)
}

async fn check_for_update(State(state): State<AppState>) -> Json<UpdateStatus> {
    let _ = state.updates.check_now().await;
    Json(state.updates.status().await)
}

async fn apply_update(State(state): State<AppState>, body: Bytes) -> Response {
    let request: ApplyUpdateRequest = match parse_json_body(
        &body,
        MAX_UPDATE_BODY,
        false,
        "The update request is too large.",
        "The update request is invalid.",
    ) {
        Ok(request) => request,
        Err(error) => return request_parse_error_response(error),
    };
    if request.expected_version.len() > 64 || request.expected_version.trim().is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                error: "The expected update version is invalid.",
            }),
        )
            .into_response();
    }
    match state
        .updates
        .begin_apply(
            &request.expected_version,
            request.confirm_session_termination,
        )
        .await
    {
        Ok(()) => (StatusCode::ACCEPTED, Json(state.updates.status().await)).into_response(),
        Err(error) => {
            tracing::warn!(%error, "update apply request was refused");
            (
                StatusCode::CONFLICT,
                Json(ErrorResponse {
                    error: "The update could not be started. Check update status and server logs.",
                }),
            )
                .into_response()
        }
    }
}

async fn primary_session(State(state): State<AppState>) -> Json<SessionSnapshot> {
    Json(state.sessions.primary().snapshot())
}

async fn sessions(State(state): State<AppState>) -> Json<Vec<SessionSnapshot>> {
    Json(state.sessions.list())
}

async fn agents(State(state): State<AppState>) -> Json<Vec<AgentKind>> {
    Json(state.agents.ready_agents().await)
}

async fn agent_catalog(State(state): State<AppState>) -> Json<AgentCatalogResponse> {
    Json(state.agents.snapshot().await)
}

async fn filesystem_roots(State(state): State<AppState>) -> Json<FilesystemRoots> {
    Json(state.directories.roots())
}

async fn list_directory(State(state): State<AppState>, body: Bytes) -> Response {
    let request: ListDirectoryRequest = match parse_json_body(
        &body,
        MAX_DIRECTORY_ID_BODY,
        true,
        "The directory request is too large.",
        "The directory request is invalid.",
    ) {
        Ok(request) => request,
        Err(error) => return request_parse_error_response(error),
    };
    let result = match request.directory_id.as_deref() {
        Some(directory_id) => state.directories.list_id(directory_id).await,
        None => state.directories.list_default().await,
    };
    directory_listing_response(result)
}

async fn resolve_directory(State(state): State<AppState>, body: Bytes) -> Response {
    let request: ResolveDirectoryRequest = match parse_json_body(
        &body,
        MAX_RESOLVE_PATH_BODY,
        false,
        "The directory path request is too large.",
        "The directory path request is invalid.",
    ) {
        Ok(request) => request,
        Err(error) => return request_parse_error_response(error),
    };
    directory_listing_response(state.directories.resolve_display_path(&request.path).await)
}

async fn workspaces(State(state): State<AppState>) -> Json<WorkspaceLibrary> {
    Json(state.workspaces.snapshot().await)
}

async fn upsert_favorite(State(state): State<AppState>, body: Bytes) -> Response {
    let request: UpsertFavoriteRequest = match parse_json_body(
        &body,
        MAX_FAVORITE_BODY,
        false,
        "The favorite request is too large.",
        "The favorite request is invalid.",
    ) {
        Ok(request) => request,
        Err(error) => return request_parse_error_response(error),
    };
    let path = match state.directories.resolve_id(&request.directory_id).await {
        Ok(path) => path,
        Err(error) => return directory_error_response(error),
    };
    let directory = state.directories.describe(&path);
    match state
        .workspaces
        .upsert_favorite(directory, request.label, request.preferred_agent)
        .await
    {
        Ok(favorite) => Json(favorite).into_response(),
        Err(error) => workspace_error_response(error),
    }
}

async fn delete_favorite(State(state): State<AppState>, Path(favorite_id): Path<Uuid>) -> Response {
    match state.workspaces.delete_favorite(favorite_id).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => workspace_error_response(error),
    }
}

async fn create_session(State(state): State<AppState>, body: Bytes) -> Response {
    if body.len() > MAX_CREATE_SESSION_BODY {
        return (
            StatusCode::PAYLOAD_TOO_LARGE,
            Json(ErrorResponse {
                error: "The terminal session request is too large.",
            }),
        )
            .into_response();
    }
    let request = match parse_create_session_request(&body) {
        Ok(request) => request,
        Err(()) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(ErrorResponse {
                    error: "The terminal session request is invalid.",
                }),
            )
                .into_response();
        }
    };
    let requested_agent = request.agent;
    let selected_project = match request.directory_id.as_deref() {
        Some(directory_id) => match state.directories.resolve_id(directory_id).await {
            Ok(path) => Some(path),
            Err(error) => return directory_error_response(error),
        },
        None => None,
    };
    let recent_project = selected_project
        .clone()
        .unwrap_or_else(|| state.config.project_dir.clone());
    match state
        .sessions
        .create_in(requested_agent, selected_project)
        .await
    {
        Ok(session) => {
            let recent_directory = state.directories.describe(&recent_project);
            if let Err(error) = state
                .workspaces
                .record_recent(recent_directory, session.agent)
                .await
            {
                // The PTY is already live. Persistence must not turn a successful
                // launch into an apparent failure or terminate the process.
                tracing::warn!(%error, "terminal started but Recent workspace state could not be saved");
            }
            (StatusCode::CREATED, Json(session)).into_response()
        }
        Err(RegistryError::LimitReached) => (
            StatusCode::CONFLICT,
            Json(ErrorResponse {
                error: "Managed terminal session capacity has been reached.",
            }),
        )
            .into_response(),
        Err(RegistryError::ShuttingDown) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(ErrorResponse {
                error: "The server is shutting down.",
            }),
        )
            .into_response(),
        Err(RegistryError::AgentUnavailable) => (
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                error: "The requested terminal agent is not configured.",
            }),
        )
            .into_response(),
        Err(error) => {
            tracing::error!(%error, "terminal session create request failed");
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(ErrorResponse {
                    error: "The terminal agent could not be started. Check the session list and server log.",
                }),
            )
                .into_response()
        }
    }
}

fn parse_json_body<T: serde::de::DeserializeOwned + Default>(
    body: &[u8],
    maximum_bytes: usize,
    allow_empty: bool,
    too_large_message: &'static str,
    invalid_message: &'static str,
) -> Result<T, (StatusCode, &'static str)> {
    if body.len() > maximum_bytes {
        return Err((StatusCode::PAYLOAD_TOO_LARGE, too_large_message));
    }
    if body.iter().all(u8::is_ascii_whitespace) {
        if allow_empty {
            return Ok(T::default());
        }
        return Err((StatusCode::BAD_REQUEST, invalid_message));
    }
    serde_json::from_slice(body).map_err(|_| (StatusCode::BAD_REQUEST, invalid_message))
}

fn request_parse_error_response(error: (StatusCode, &'static str)) -> Response {
    (error.0, Json(ErrorResponse { error: error.1 })).into_response()
}

fn parse_create_session_request(body: &[u8]) -> Result<CreateSessionRequest, ()> {
    if body.iter().all(u8::is_ascii_whitespace) {
        return Ok(CreateSessionRequest::default());
    }
    serde_json::from_slice(body).map_err(|_| ())
}

fn directory_listing_response(result: Result<DirectoryListing, DirectoryError>) -> Response {
    match result {
        Ok(listing) => Json(listing).into_response(),
        Err(error) => directory_error_response(error),
    }
}

fn directory_error_response(error: DirectoryError) -> Response {
    let (status, message) = match error {
        DirectoryError::InvalidId | DirectoryError::InvalidPath => (
            StatusCode::BAD_REQUEST,
            "The directory selection is invalid.",
        ),
        DirectoryError::NotFound => (StatusCode::NOT_FOUND, "The directory was not found."),
        DirectoryError::NotDirectory => (
            StatusCode::BAD_REQUEST,
            "The selected path is not a directory.",
        ),
        DirectoryError::Inaccessible => (
            StatusCode::FORBIDDEN,
            "The directory cannot be read by the server.",
        ),
        DirectoryError::Io(_) | DirectoryError::Join(_) => {
            tracing::error!(%error, "directory request failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "The directory could not be read. Check the server log.",
            )
        }
    };
    (status, Json(ErrorResponse { error: message })).into_response()
}

fn workspace_error_response(error: WorkspaceError) -> Response {
    let (status, message) = match error {
        WorkspaceError::InvalidLabel => (StatusCode::BAD_REQUEST, "The favorite label is invalid."),
        WorkspaceError::StateTooLarge => (
            StatusCode::INSUFFICIENT_STORAGE,
            "The workspace state exceeds the server storage limit.",
        ),
        WorkspaceError::FavoriteLimitReached => (
            StatusCode::CONFLICT,
            "The maximum number of favorites has been reached.",
        ),
        WorkspaceError::FavoriteNotFound => (StatusCode::NOT_FOUND, "The favorite was not found."),
        WorkspaceError::UnsupportedVersion(_)
        | WorkspaceError::InvalidState(_)
        | WorkspaceError::UnsafeStateLocation(_)
        | WorkspaceError::Io(_)
        | WorkspaceError::Json(_)
        | WorkspaceError::Join(_) => {
            tracing::error!(%error, "workspace state request failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "The workspace state could not be saved. Check the server log.",
            )
        }
    };
    (status, Json(ErrorResponse { error: message })).into_response()
}

async fn session_by_id(State(state): State<AppState>, Path(terminal_id): Path<Uuid>) -> Response {
    match state.sessions.get(terminal_id) {
        Some(session) => Json(session.snapshot()).into_response(),
        None => session_not_found(),
    }
}

async fn restart_primary_session(State(state): State<AppState>) -> Response {
    let terminal_id = state.sessions.primary().snapshot().terminal_id;
    restart_by_id(&state, terminal_id).await
}

async fn restart_session(State(state): State<AppState>, Path(terminal_id): Path<Uuid>) -> Response {
    restart_by_id(&state, terminal_id).await
}

async fn restart_by_id(state: &AppState, terminal_id: Uuid) -> Response {
    if state
        .sessions
        .get(terminal_id)
        .is_some_and(|session| matches!(session.purpose(), SessionPurpose::Peer { .. }))
    {
        return (
            StatusCode::CONFLICT,
            Json(ErrorResponse {
                error: "A dedicated peer terminal cannot be restarted; close its peer thread instead.",
            }),
        )
            .into_response();
    }
    if has_open_peer_thread(state, terminal_id) {
        return (
            StatusCode::CONFLICT,
            Json(ErrorResponse {
                error: "Close this terminal's peer threads before restarting the source session.",
            }),
        )
            .into_response();
    }
    match state.sessions.restart(terminal_id).await {
        Ok(()) => {
            if let Some(snapshot) = state
                .sessions
                .get(terminal_id)
                .map(|session| session.snapshot())
                && let Ok(path) = decode_directory_id(&snapshot.directory_id)
                && let Err(error) = state
                    .workspaces
                    .record_recent(state.directories.describe(&path), snapshot.agent)
                    .await
            {
                tracing::warn!(
                    %error,
                    %terminal_id,
                    "terminal restarted but Recent workspace state could not be saved"
                );
            }
            StatusCode::NO_CONTENT.into_response()
        }
        Err(RegistryError::NotFound) => session_not_found(),
        Err(RegistryError::PeerSessionsActive) => (
            StatusCode::CONFLICT,
            Json(ErrorResponse {
                error: "Close this terminal's peer threads before restarting the source session.",
            }),
        )
            .into_response(),
        Err(RegistryError::PeerSessionManaged) => (
            StatusCode::CONFLICT,
            Json(ErrorResponse {
                error: "A dedicated peer terminal cannot be restarted; close its peer thread instead.",
            }),
        )
            .into_response(),
        Err(error) => {
            tracing::error!(%error, %terminal_id, "session restart request failed");
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(ErrorResponse {
                    error: "The terminal agent could not be restarted. Check the session status and server log.",
                }),
            )
                .into_response()
        }
    }
}

async fn terminate_primary_session(State(state): State<AppState>) -> Response {
    let terminal_id = state.sessions.primary().snapshot().terminal_id;
    terminate_by_id(&state, terminal_id).await
}

async fn terminate_session(
    State(state): State<AppState>,
    Path(terminal_id): Path<Uuid>,
) -> Response {
    terminate_by_id(&state, terminal_id).await
}

async fn terminate_by_id(state: &AppState, terminal_id: Uuid) -> Response {
    if state
        .sessions
        .get(terminal_id)
        .is_some_and(|session| matches!(session.purpose(), SessionPurpose::Peer { .. }))
    {
        return (
            StatusCode::CONFLICT,
            Json(ErrorResponse {
                error: "A dedicated peer terminal is managed by its peer thread; close the thread instead.",
            }),
        )
            .into_response();
    }
    if has_open_peer_thread(state, terminal_id) {
        return (
            StatusCode::CONFLICT,
            Json(ErrorResponse {
                error: "Close this terminal's peer threads before terminating the source session.",
            }),
        )
            .into_response();
    }
    match state.sessions.terminate(terminal_id).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(RegistryError::NotFound) => session_not_found(),
        Err(RegistryError::PeerSessionsActive) => (
            StatusCode::CONFLICT,
            Json(ErrorResponse {
                error: "Close this terminal's peer threads before terminating the source session.",
            }),
        )
            .into_response(),
        Err(RegistryError::PeerSessionManaged) => (
            StatusCode::CONFLICT,
            Json(ErrorResponse {
                error: "A dedicated peer terminal is managed by its peer thread; close the thread instead.",
            }),
        )
            .into_response(),
        Err(error) => {
            tracing::error!(%error, %terminal_id, "session terminate request failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    error: "The terminal agent could not be terminated. Check the server log.",
                }),
            )
                .into_response()
        }
    }
}

async fn delete_session(State(state): State<AppState>, Path(terminal_id): Path<Uuid>) -> Response {
    if let Some(session) = state.sessions.get(terminal_id) {
        let snapshot = session.snapshot();
        match snapshot.purpose {
            SessionPurpose::Peer {
                thread_id,
                parent_terminal_id,
            } => {
                return match state.peers.get_thread(thread_id) {
                    Ok(_) => peer_routes::close_thread_by_id(&state, thread_id).await,
                    Err(_) => match state
                        .sessions
                        .delete_peer(
                            terminal_id,
                            thread_id,
                            parent_terminal_id,
                            snapshot.session_id,
                        )
                        .await
                    {
                        Ok(()) => StatusCode::NO_CONTENT.into_response(),
                        Err(RegistryError::NotFound) => session_not_found(),
                        Err(error) => {
                            tracing::error!(%error, %terminal_id, "orphaned peer terminal delete failed");
                            (
                                StatusCode::INTERNAL_SERVER_ERROR,
                                Json(ErrorResponse {
                                    error: "The terminal session could not be deleted.",
                                }),
                            )
                                .into_response()
                        }
                    },
                };
            }
            SessionPurpose::Interactive => {
                if has_open_peer_thread(&state, terminal_id) {
                    return (
                        StatusCode::CONFLICT,
                        Json(ErrorResponse {
                            error: "Close this terminal's peer threads before deleting the source session.",
                        }),
                    )
                        .into_response();
                }
            }
        }
    }
    match state.sessions.delete(terminal_id).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(RegistryError::NotFound) => session_not_found(),
        Err(RegistryError::PrimaryCannotBeDeleted) => (
            StatusCode::CONFLICT,
            Json(ErrorResponse {
                error: "The primary terminal session cannot be deleted.",
            }),
        )
            .into_response(),
        Err(RegistryError::PeerSessionsActive) => (
            StatusCode::CONFLICT,
            Json(ErrorResponse {
                error: "Close this terminal's peer threads before deleting the source session.",
            }),
        )
            .into_response(),
        Err(RegistryError::PeerSessionManaged) => (
            StatusCode::CONFLICT,
            Json(ErrorResponse {
                error: "A dedicated peer terminal must be closed through its peer thread.",
            }),
        )
            .into_response(),
        Err(error) => {
            tracing::error!(%error, %terminal_id, "session delete request failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    error: "The terminal session could not be deleted. Check the server log.",
                }),
            )
                .into_response()
        }
    }
}

fn has_open_peer_thread(state: &AppState, terminal_id: Uuid) -> bool {
    state.peers.list_threads().iter().any(|thread| {
        thread.source_terminal_id == terminal_id && thread.status != PeerStatus::Closed
    })
}

fn session_not_found() -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(ErrorResponse {
            error: "Terminal session not found.",
        }),
    )
        .into_response()
}

async fn development_fallback() -> impl IntoResponse {
    (
        StatusCode::NOT_FOUND,
        Html(
            r#"<!doctype html>
<html lang="en">
<meta charset="utf-8">
<title>Codex Web Terminal — Unofficial</title>
<body style="font:16px system-ui;background:#0c0c0c;color:#eee;padding:2rem">
<h1>Frontend build not found</h1>
<p>For development, run <code>npm run dev</code> in the <code>web</code> directory and open the Vite URL.</p>
<p>For production, build the frontend and keep its <code>web</code> directory next to the server executable.</p>
</body>
</html>"#,
        ),
    )
}

async fn security_headers(request: axum::extract::Request, next: Next) -> Response {
    let request_path = request.uri().path().to_owned();
    let mut response = next.run(request).await;
    let is_html = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.starts_with("text/html"));
    let headers = response.headers_mut();
    if is_html || request_path == "/" || request_path.ends_with(".html") {
        headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    } else if request_path.starts_with("/assets/") {
        headers.insert(
            header::CACHE_CONTROL,
            HeaderValue::from_static("public, max-age=31536000, immutable"),
        );
    }
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(
            "default-src 'self'; connect-src 'self' ws: wss:; img-src 'self' data:; \
             style-src 'self' 'unsafe-inline'; script-src 'self'; font-src 'self' data:; \
             object-src 'none'; base-uri 'none'; frame-ancestors 'none'",
        ),
    );
    headers.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_create_session_body_remains_backward_compatible() {
        let request = parse_create_session_request(b" \r\n").expect("legacy empty request");

        assert_eq!(request.agent, None);
        assert_eq!(request.directory_id, None);
    }

    #[test]
    fn create_session_body_accepts_only_the_agent_field() {
        let request =
            parse_create_session_request(br#"{"agent":"claude"}"#).expect("valid request");

        assert_eq!(request.agent, Some(AgentKind::Claude));
        assert_eq!(request.directory_id, None);
        assert!(
            parse_create_session_request(br#"{"agent":"claude","command":"malicious"}"#).is_err()
        );
        assert!(parse_create_session_request(br#"{"agent":"unknown"}"#).is_err());
    }

    #[test]
    fn create_session_accepts_only_agent_and_opaque_directory_id() {
        let request =
            parse_create_session_request(br#"{"agent":"agy","directoryId":"u1.L3Byb2plY3Rz"}"#)
                .expect("valid request");

        assert_eq!(request.agent, Some(AgentKind::Agy));
        assert_eq!(request.directory_id.as_deref(), Some("u1.L3Byb2plY3Rz"));
        assert!(
            parse_create_session_request(
                br#"{"directoryId":"u1.L3Byb2plY3Rz","arguments":["--unsafe"]}"#
            )
            .is_err()
        );
    }

    #[test]
    fn oversized_workspace_state_has_a_distinct_http_error() {
        let response = workspace_error_response(WorkspaceError::StateTooLarge);

        assert_eq!(response.status(), StatusCode::INSUFFICIENT_STORAGE);
    }

    #[cfg(windows)]
    #[test]
    fn maximum_windows_directory_ids_fit_every_request_contract() {
        use std::{ffi::OsString, os::windows::ffi::OsStringExt};

        let mut path_units = vec![b'C' as u16, b':' as u16, b'\\' as u16];
        while path_units.len() + 2 <= 32_767 {
            path_units.extend([b'a' as u16, b'\\' as u16]);
        }
        if path_units.len() < 32_767 {
            path_units.push(b'a' as u16);
        }
        let path = PathBuf::from(OsString::from_wide(&path_units));
        let directory_id = crate::filesystem::encode_directory_id(&path);

        let session_body = serde_json::to_vec(&serde_json::json!({ "directoryId": directory_id }))
            .expect("session JSON");
        assert!(session_body.len() <= MAX_CREATE_SESSION_BODY);
        assert_eq!(
            parse_create_session_request(&session_body)
                .expect("maximum session request")
                .directory_id
                .as_deref(),
            Some(directory_id.as_str())
        );

        let list_body = serde_json::to_vec(&serde_json::json!({
            "directoryId": directory_id
        }))
        .expect("list JSON");
        let list: ListDirectoryRequest = parse_json_body(
            &list_body,
            MAX_DIRECTORY_ID_BODY,
            false,
            "too large",
            "invalid",
        )
        .expect("maximum list request");
        assert_eq!(list.directory_id.as_deref(), Some(directory_id.as_str()));

        let favorite_body = serde_json::to_vec(&serde_json::json!({
            "directoryId": directory_id,
            "label": "Long path",
            "preferredAgent": "codex"
        }))
        .expect("favorite JSON");
        let favorite: UpsertFavoriteRequest = parse_json_body(
            &favorite_body,
            MAX_FAVORITE_BODY,
            false,
            "too large",
            "invalid",
        )
        .expect("maximum favorite request");
        assert_eq!(favorite.directory_id.as_str(), directory_id.as_str());
    }
}
