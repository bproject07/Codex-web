//! Bearer-protected HTTP API for context transfers ("Continue in <agent>").
//!
//! Every handler is a thin, cancellation-safe wrapper: target start and
//! cleanup run in spawned tasks that survive a disconnected browser request,
//! and every state change is applied by the coordinator with exact attempt,
//! terminal, and generation identity. Responses never contain artifact text
//! except on the explicit draft endpoint.

use axum::{
    Json, Router,
    body::Bytes,
    extract::{Path, Query, State},
    http::{HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    config::AgentKind,
    filesystem::DirectoryError,
    peer::MAX_PEER_ARTIFACT_BYTES,
    routes::AppState,
    transfer::{
        TargetAdmission, TargetRequest, TransferError, TransferErrorCode, TransferFilter,
        TransferMode, TransferSnapshot,
    },
};

const MAX_CREATE_BODY: usize = 64 * 1024;
const MAX_DRAFT_BODY: usize = MAX_PEER_ARTIFACT_BYTES * 6 + 16 * 1024;
const MAX_TARGET_BODY: usize = 256 * 1024;
const MAX_SMALL_BODY: usize = 16 * 1024;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CreateTransferRequest {
    transfer_id: Uuid,
    source_terminal_id: Uuid,
    source_session_id: Option<Uuid>,
    mode: TransferMode,
    #[serde(default)]
    source_ready: bool,
    instruction: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct DraftRequest {
    expected_revision: u32,
    content: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct DraftResponse {
    draft_revision: u32,
    content: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct TargetRequestBody {
    target_request_id: Uuid,
    target: TargetSelection,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
enum TargetSelection {
    #[serde(rename_all = "camelCase")]
    New {
        agent: AgentKind,
        directory_id: Option<String>,
    },
    #[serde(rename_all = "camelCase")]
    Existing { terminal_id: Uuid, session_id: Uuid },
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct DeliverRequest {
    expected_revision: u32,
    target_session_id: Uuid,
    #[serde(default)]
    target_ready: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CleanupRequest {
    target_terminal_id: Uuid,
    target_session_id: Option<Uuid>,
}

/// Abandon carries an optional free-text reason for the browser's own
/// bookkeeping; the server accepts and ignores it (never logged).
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AbandonRequest {
    #[serde(default)]
    #[allow(dead_code)]
    reason: Option<String>,
}

#[derive(Debug, Serialize)]
struct TransferApiError {
    error: String,
    code: TransferErrorCode,
}

pub fn protected_router() -> Router<AppState> {
    Router::new()
        .route("/api/transfers", get(list_transfers).post(create_transfer))
        .route("/api/transfers/{id}", get(get_transfer))
        .route("/api/transfers/{id}/draft", get(get_draft).put(put_draft))
        .route("/api/transfers/{id}/target", post(choose_target))
        .route("/api/transfers/{id}/deliver", post(deliver))
        .route("/api/transfers/{id}/cleanup", post(cleanup))
        .route("/api/transfers/{id}/abandon", post(abandon))
}

async fn list_transfers(
    State(state): State<AppState>,
    Query(filter): Query<TransferFilter>,
) -> Response {
    no_store(Json(state.transfers.list(&filter)).into_response())
}

async fn get_transfer(State(state): State<AppState>, Path(id): Path<Uuid>) -> Response {
    snapshot_response(state.transfers.get(id))
}

async fn create_transfer(State(state): State<AppState>, body: Bytes) -> Response {
    let request: CreateTransferRequest = match parse_body(&body, MAX_CREATE_BODY) {
        Ok(request) => request,
        Err(response) => return *response,
    };
    match state.transfers.create(
        request.transfer_id,
        request.source_terminal_id,
        request.source_session_id,
        request.mode,
        request.source_ready,
        request.instruction,
    ) {
        Ok(snapshot) => no_store((StatusCode::CREATED, Json(snapshot)).into_response()),
        Err(error) => transfer_error_response(error),
    }
}

async fn get_draft(State(state): State<AppState>, Path(id): Path<Uuid>) -> Response {
    match state.transfers.draft(id) {
        Ok((draft_revision, content)) => no_store(
            Json(DraftResponse {
                draft_revision,
                content,
            })
            .into_response(),
        ),
        Err(error) => transfer_error_response(error),
    }
}

async fn put_draft(State(state): State<AppState>, Path(id): Path<Uuid>, body: Bytes) -> Response {
    let request: DraftRequest = match parse_body(&body, MAX_DRAFT_BODY) {
        Ok(request) => request,
        Err(response) => return *response,
    };
    snapshot_response(
        state
            .transfers
            .edit_draft(id, request.expected_revision, request.content),
    )
}

async fn choose_target(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    body: Bytes,
) -> Response {
    let request: TargetRequestBody = match parse_body(&body, MAX_TARGET_BODY) {
        Ok(request) => request,
        Err(response) => return *response,
    };
    let target = match request.target {
        TargetSelection::New {
            agent,
            directory_id,
        } => {
            // Resolve the directory the same way New terminal does: decode and
            // canonicalize again immediately before use; default to the source's.
            let directory_id = match directory_id {
                Some(directory_id) => directory_id,
                None => match state
                    .transfers
                    .get(id)
                    .ok()
                    .and_then(|snapshot| state.sessions.get(snapshot.source_terminal_id))
                {
                    Some(source) => source.snapshot().directory_id,
                    None => {
                        return transfer_error_response(TransferError::from_code(
                            TransferErrorCode::InvalidDirectory,
                        ));
                    }
                },
            };
            let project_dir = match state.directories.resolve_id(&directory_id).await {
                Ok(path) => path,
                Err(error) => return directory_error_response(error),
            };
            TargetRequest::New {
                agent,
                project_dir,
                directory_id,
            }
        }
        TargetSelection::Existing {
            terminal_id,
            session_id,
        } => TargetRequest::Existing {
            terminal_id,
            session_id,
        },
    };

    let admission = match state
        .transfers
        .begin_target(id, request.target_request_id, target)
    {
        Ok(admission) => admission,
        Err(error) => return transfer_error_response(error),
    };
    match admission {
        TargetAdmission::Settled(snapshot) => no_store(Json(snapshot).into_response()),
        TargetAdmission::Wait(mut completed) => {
            // A duplicate of an in-flight request waits for that start to
            // settle and then reports the shared outcome.
            let _ = completed.wait_for(|done| *done).await;
            snapshot_response(state.transfers.get(id))
        }
        TargetAdmission::Start {
            session,
            token,
            snapshot: _,
        } => {
            let start_state = state.clone();
            let outcome = tokio::spawn(async move {
                let result = start_state.sessions.start_transfer_target(session).await;
                let started = result.as_ref().ok().cloned();
                let completed = start_state.transfers.complete_target_start(token, result);
                if let Some(started) = started {
                    record_recent(&start_state, started.directory_id.as_str(), started.agent).await;
                }
                completed
            })
            .await;
            match outcome {
                Ok(result) => snapshot_response(result),
                Err(error) => {
                    tracing::error!(%error, "transfer target start task failed");
                    snapshot_response(state.transfers.get(id))
                }
            }
        }
    }
}

async fn record_recent(state: &AppState, directory_id: &str, agent: AgentKind) {
    if let Ok(path) = crate::filesystem::decode_directory_id(directory_id)
        && let Err(error) = state
            .workspaces
            .record_recent(state.directories.describe(&path), agent)
            .await
    {
        // The PTY is already live; persistence must never fail the launch.
        tracing::warn!(%error, "transfer target started but Recent workspace state could not be saved");
    }
}

async fn deliver(State(state): State<AppState>, Path(id): Path<Uuid>, body: Bytes) -> Response {
    let request: DeliverRequest = match parse_body(&body, MAX_SMALL_BODY) {
        Ok(request) => request,
        Err(response) => return *response,
    };
    snapshot_response(state.transfers.deliver(
        id,
        request.expected_revision,
        request.target_session_id,
        request.target_ready,
    ))
}

async fn cleanup(State(state): State<AppState>, Path(id): Path<Uuid>, body: Bytes) -> Response {
    let request: CleanupRequest = match parse_body(&body, MAX_SMALL_BODY) {
        Ok(request) => request,
        Err(response) => return *response,
    };
    let ticket = match state.transfers.begin_cleanup(
        id,
        request.target_terminal_id,
        request.target_session_id,
    ) {
        Ok(ticket) => ticket,
        Err(error) => return transfer_error_response(error),
    };
    let cleanup_state = state.clone();
    let outcome = tokio::spawn(async move {
        let result = cleanup_state
            .sessions
            .cleanup_transfer_target(ticket.terminal_id, id, ticket.session_id)
            .await;
        cleanup_state.transfers.complete_cleanup(ticket, result)
    })
    .await;
    match outcome {
        Ok(result) => snapshot_response(result),
        Err(error) => {
            tracing::error!(%error, "transfer cleanup task failed");
            snapshot_response(state.transfers.get(id))
        }
    }
}

async fn abandon(State(state): State<AppState>, Path(id): Path<Uuid>, body: Bytes) -> Response {
    let _request: AbandonRequest = match parse_optional_body(&body, MAX_SMALL_BODY) {
        Ok(request) => request,
        Err(response) => return *response,
    };
    snapshot_response(state.transfers.abandon(id))
}

fn snapshot_response(result: Result<TransferSnapshot, TransferError>) -> Response {
    match result {
        Ok(snapshot) => no_store(Json(snapshot).into_response()),
        Err(error) => transfer_error_response(error),
    }
}

fn transfer_error_response(error: TransferError) -> Response {
    let status = match error.code() {
        TransferErrorCode::TransferNotFound => StatusCode::NOT_FOUND,
        TransferErrorCode::InvalidInput => StatusCode::BAD_REQUEST,
        TransferErrorCode::PayloadTooLarge => StatusCode::PAYLOAD_TOO_LARGE,
        TransferErrorCode::Unauthorized => StatusCode::FORBIDDEN,
        TransferErrorCode::TransferLimitReached
        | TransferErrorCode::TransferHistoryFull
        | TransferErrorCode::ShuttingDown => StatusCode::SERVICE_UNAVAILABLE,
        _ => StatusCode::CONFLICT,
    };
    no_store(
        (
            status,
            Json(TransferApiError {
                error: error.message().to_owned(),
                code: error.code(),
            }),
        )
            .into_response(),
    )
}

fn directory_error_response(error: DirectoryError) -> Response {
    let (status, message) = match error {
        DirectoryError::InvalidId | DirectoryError::InvalidPath => (
            StatusCode::BAD_REQUEST,
            "The directory selection is invalid.",
        ),
        DirectoryError::NotFound => (StatusCode::CONFLICT, "The directory was not found."),
        DirectoryError::NotDirectory => (
            StatusCode::BAD_REQUEST,
            "The selected path is not a directory.",
        ),
        DirectoryError::Inaccessible => (
            StatusCode::FORBIDDEN,
            "The directory cannot be read by the server.",
        ),
        DirectoryError::Io(_) | DirectoryError::Join(_) => {
            tracing::error!(%error, "transfer target directory request failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "The directory could not be read. Check the server log.",
            )
        }
    };
    no_store(
        (
            status,
            Json(TransferApiError {
                error: message.to_owned(),
                code: TransferErrorCode::InvalidDirectory,
            }),
        )
            .into_response(),
    )
}

fn parse_body<T: serde::de::DeserializeOwned>(
    body: &[u8],
    maximum_bytes: usize,
) -> Result<T, Box<Response>> {
    if body.len() > maximum_bytes {
        return Err(Box::new(no_store(
            (
                StatusCode::PAYLOAD_TOO_LARGE,
                Json(TransferApiError {
                    error: "The transfer request is too large.".to_owned(),
                    code: TransferErrorCode::PayloadTooLarge,
                }),
            )
                .into_response(),
        )));
    }
    serde_json::from_slice(body).map_err(|_| {
        Box::new(no_store(
            (
                StatusCode::BAD_REQUEST,
                Json(TransferApiError {
                    error: "The transfer request is invalid.".to_owned(),
                    code: TransferErrorCode::InvalidInput,
                }),
            )
                .into_response(),
        ))
    })
}

fn parse_optional_body<T: serde::de::DeserializeOwned + Default>(
    body: &[u8],
    maximum_bytes: usize,
) -> Result<T, Box<Response>> {
    if body.iter().all(u8::is_ascii_whitespace) {
        return Ok(T::default());
    }
    parse_body(body, maximum_bytes)
}

fn no_store(mut response: Response) -> Response {
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_bodies_reject_unknown_fields_and_parse_target_modes() {
        let new_target: TargetRequestBody = serde_json::from_str(
            r#"{"targetRequestId":"11111111-1111-4111-8111-111111111111","target":{"mode":"new","agent":"kilo","directoryId":"abc"}}"#,
        )
        .expect("new target body");
        assert!(matches!(
            new_target.target,
            TargetSelection::New {
                agent: AgentKind::Kilo,
                directory_id: Some(ref id)
            } if id == "abc"
        ));
        let existing: TargetRequestBody = serde_json::from_str(
            r#"{"targetRequestId":"11111111-1111-4111-8111-111111111111","target":{"mode":"existing","terminalId":"22222222-2222-4222-8222-222222222222","sessionId":"33333333-3333-4333-8333-333333333333"}}"#,
        )
        .expect("existing target body");
        assert!(matches!(existing.target, TargetSelection::Existing { .. }));
        assert!(
            serde_json::from_str::<TargetRequestBody>(
                r#"{"targetRequestId":"11111111-1111-4111-8111-111111111111","target":{"mode":"new","agent":"kilo","command":"evil"}}"#,
            )
            .is_err(),
            "a browser may never supply a command"
        );
        assert!(
            serde_json::from_str::<CreateTransferRequest>(
                r#"{"transferId":"11111111-1111-4111-8111-111111111111","sourceTerminalId":"22222222-2222-4222-8222-222222222222","sourceSessionId":null,"mode":"manual","instruction":"x","extra":1}"#,
            )
            .is_err()
        );
        let deliver: DeliverRequest = serde_json::from_str(
            r#"{"expectedRevision":2,"targetSessionId":"33333333-3333-4333-8333-333333333333"}"#,
        )
        .expect("deliver body");
        assert!(!deliver.target_ready, "readiness defaults to false");
        let error = serde_json::to_value(TransferApiError {
            error: "x".to_owned(),
            code: TransferErrorCode::QueueRejected,
        })
        .expect("error wire form");
        assert_eq!(error["code"], "queue_rejected");
    }
}
