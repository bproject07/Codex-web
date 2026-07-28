use std::net::SocketAddr;

use axum::{
    Json, Router,
    body::Bytes,
    extract::{ConnectInfo, DefaultBodyLimit, Path, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    config::AgentKind,
    peer::{
        MAX_PEER_ARTIFACT_BYTES, PeerAction, PeerBroker, PeerError, PeerErrorKind, PeerStatus,
        PeerThread, SessionPurpose,
    },
    peer_cli::{INTERNAL_PEER_PATH, InternalPeerRequest, InternalPeerResponse},
    registry::RegistryError,
    routes::AppState,
};

const MAX_PEER_COMMAND_BODY: usize = 256 * 1024;
const MAX_PEER_DISPATCH_BODY: usize = 256 * 1024;
const MAX_INTERNAL_PEER_BODY: usize = MAX_PEER_ARTIFACT_BYTES * 6 + 4 * 1024;
const CAPABILITY_SCHEME: &str = "CWT-Capability ";

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CreateThreadRequest {
    source_terminal_id: Uuid,
    directory_id: Option<String>,
    target_agent: AgentKind,
    action: PeerAction,
    instruction: String,
    source_ready: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CreateTurnRequest {
    action: PeerAction,
    instruction: String,
    source_ready: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct DispatchTurnRequest {
    turn_id: Uuid,
    handoff_revision: u32,
    handoff: String,
    reviewer_ready: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ReturnTurnRequest {
    turn_id: Uuid,
    source_ready: bool,
}

#[derive(Debug, Serialize)]
struct PeerApiError {
    error: String,
}

pub fn protected_router() -> Router<AppState> {
    Router::new()
        .route("/api/peer/threads", get(list_threads).post(create_thread))
        .route(
            "/api/peer/threads/{id}",
            get(get_thread).delete(close_thread),
        )
        .route("/api/peer/threads/{id}/turns", post(create_turn))
        .route("/api/peer/threads/{id}/dispatch", post(dispatch_turn))
        .route("/api/peer/threads/{id}/return", post(return_turn))
}

pub fn internal_router(broker: PeerBroker) -> Router {
    Router::new()
        .route(INTERNAL_PEER_PATH, post(internal_request))
        .layer(DefaultBodyLimit::max(MAX_INTERNAL_PEER_BODY))
        .with_state(broker)
}

async fn list_threads(State(state): State<AppState>) -> Json<Vec<PeerThread>> {
    Json(state.peers.list_threads())
}

async fn get_thread(State(state): State<AppState>, Path(thread_id): Path<Uuid>) -> Response {
    match state.peers.get_thread(thread_id) {
        Ok(thread) => Json(thread).into_response(),
        Err(error) => peer_error_response(error),
    }
}

async fn create_thread(State(state): State<AppState>, body: Bytes) -> Response {
    let request: CreateThreadRequest = match parse_body(&body, MAX_PEER_COMMAND_BODY) {
        Ok(request) => request,
        Err(response) => return *response,
    };
    if request.action == PeerAction::Recheck {
        return api_error(
            StatusCode::BAD_REQUEST,
            "Recheck requires an existing peer thread.",
        );
    }
    if !request.source_ready {
        return api_error(
            StatusCode::BAD_REQUEST,
            "Confirm that the source terminal is at an empty agent prompt.",
        );
    }

    let source = match state.sessions.get(request.source_terminal_id) {
        Some(source) => source,
        None => return api_error(StatusCode::NOT_FOUND, "Source terminal not found."),
    };
    let source_snapshot = source.snapshot();
    if source_snapshot.purpose != SessionPurpose::Interactive || !source.is_running() {
        return api_error(
            StatusCode::CONFLICT,
            "The source must be a running interactive terminal.",
        );
    }
    let Some(source_session_id) = source_snapshot.session_id else {
        return api_error(
            StatusCode::CONFLICT,
            "The source terminal has no active session generation.",
        );
    };
    let reviewer_directory_id = request
        .directory_id
        .as_deref()
        .unwrap_or(&source_snapshot.directory_id);
    let project = match state.directories.resolve_id(reviewer_directory_id).await {
        Ok(project) => project,
        Err(_) => {
            return if request.directory_id.is_some() {
                api_error(
                    StatusCode::BAD_REQUEST,
                    "The selected reviewer working directory is not available.",
                )
            } else {
                api_error(
                    StatusCode::CONFLICT,
                    "The source terminal working directory is no longer available.",
                )
            };
        }
    };

    let provisioning_state = state.clone();
    match tokio::spawn(async move {
        provision_thread(
            &provisioning_state,
            source,
            source_session_id,
            project,
            request,
        )
        .await
    })
    .await
    {
        Ok(Ok(linked)) => (StatusCode::CREATED, Json(linked)).into_response(),
        Ok(Err(response)) => response,
        Err(error) => {
            tracing::error!(%error, "dedicated peer provisioning task failed");
            api_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "The dedicated reviewer could not be provisioned safely.",
            )
        }
    }
}

async fn provision_thread(
    state: &AppState,
    source: crate::session::SessionManager,
    source_session_id: Uuid,
    project: std::path::PathBuf,
    request: CreateThreadRequest,
) -> Result<PeerThread, Response> {
    let thread = state
        .peers
        .create_thread(
            request.source_terminal_id,
            request.target_agent,
            request.action,
            request.instruction,
        )
        .map_err(peer_error_response)?;
    let provisioning = state
        .peers
        .begin_reviewer_provisioning(thread.id)
        .map_err(peer_error_response)?;
    let reviewer_terminal_id = provisioning
        .thread()
        .reviewer_terminal_id
        .expect("a reviewer provisioning lease always owns a terminal identity");

    let reviewer = match state
        .sessions
        .create_peer_in(
            request.target_agent,
            project,
            thread.id,
            request.source_terminal_id,
            source_session_id,
            reviewer_terminal_id,
        )
        .await
    {
        Ok(reviewer) => reviewer,
        Err(error) => {
            rollback_provisioning(state, provisioning).await;
            return Err(registry_create_error(error));
        }
    };
    let Some(reviewer_session_id) = reviewer.session_id else {
        tracing::error!(
            thread_id = %thread.id,
            terminal_id = %reviewer.terminal_id,
            "new dedicated peer terminal has no session generation"
        );
        rollback_provisioning(state, provisioning).await;
        return Err(api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "The dedicated reviewer started without a valid session generation.",
        ));
    };

    if let Err(error) =
        source.write_automation_prompt(source_session_id, &source_prompt(provisioning.thread()))
    {
        tracing::warn!(
            %error,
            thread_id = %thread.id,
            "failed to deliver the peer handoff request to the source terminal"
        );
        rollback_provisioning(state, provisioning).await;
        return Err(api_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "The source terminal could not receive the peer request.",
        ));
    }

    match state.peers.complete_reviewer_provisioning(provisioning) {
        Ok(linked) => Ok(linked),
        Err(error) => {
            rollback_reviewer(state, &thread, reviewer.terminal_id, reviewer_session_id).await;
            Err(peer_error_response(error))
        }
    }
}

async fn rollback_provisioning(state: &AppState, provisioning: crate::peer::PeerProvisioning) {
    let thread = provisioning.thread().clone();
    let reviewer_terminal_id = thread
        .reviewer_terminal_id
        .expect("a reviewer provisioning lease always owns a terminal identity");
    let owned_reviewer = state.sessions.get(reviewer_terminal_id).filter(|reviewer| {
        reviewer.purpose()
            == &SessionPurpose::Peer {
                thread_id: thread.id,
                parent_terminal_id: thread.source_terminal_id,
            }
    });
    let deletion_result = if let Some(reviewer) = owned_reviewer {
        state
            .sessions
            .delete_peer(
                reviewer_terminal_id,
                thread.id,
                thread.source_terminal_id,
                reviewer.snapshot().session_id,
            )
            .await
    } else {
        Ok(())
    };

    let provisioning_result = state.peers.abort_reviewer_provisioning(provisioning);
    if let Err(error) = deletion_result {
        tracing::error!(
            %error,
            thread_id = %thread.id,
            terminal_id = %reviewer_terminal_id,
            "failed to roll back a provisioned dedicated peer terminal; the thread remains retryable"
        );
        return;
    }
    if let Err(error) = provisioning_result {
        tracing::error!(
            %error,
            thread_id = %thread.id,
            terminal_id = %reviewer_terminal_id,
            "failed to release the dedicated reviewer provisioning lease"
        );
        return;
    }
    if let Err(error) = state.peers.close_thread(thread.id) {
        tracing::error!(
            %error,
            thread_id = %thread.id,
            "failed to remove a rolled-back peer thread"
        );
    }
}

async fn rollback_reviewer(
    state: &AppState,
    thread: &PeerThread,
    reviewer_terminal_id: Uuid,
    reviewer_session_id: Uuid,
) {
    if let Err(error) = state
        .sessions
        .delete_peer(
            reviewer_terminal_id,
            thread.id,
            thread.source_terminal_id,
            Some(reviewer_session_id),
        )
        .await
    {
        tracing::error!(
            %error,
            thread_id = %thread.id,
            terminal_id = %reviewer_terminal_id,
            "failed to roll back a dedicated peer terminal; the thread remains retryable"
        );
        return;
    }
    if let Err(error) = state.peers.close_thread(thread.id) {
        tracing::error!(
            %error,
            thread_id = %thread.id,
            "failed to roll back a peer thread"
        );
    }
}

async fn create_turn(
    State(state): State<AppState>,
    Path(thread_id): Path<Uuid>,
    body: Bytes,
) -> Response {
    let request: CreateTurnRequest = match parse_body(&body, MAX_PEER_COMMAND_BODY) {
        Ok(request) => request,
        Err(response) => return *response,
    };
    if !request.source_ready {
        return api_error(
            StatusCode::BAD_REQUEST,
            "Confirm that the source terminal is at an empty agent prompt.",
        );
    }
    let thread = match state
        .peers
        .create_turn(thread_id, request.action, request.instruction)
    {
        Ok(thread) => thread,
        Err(error) => return peer_error_response(error),
    };
    let Some(source) = state.sessions.get(thread.source_terminal_id) else {
        let failed = state.peers.fail_turn(
            thread.id,
            thread.current_turn.id,
            "The source terminal is no longer available.".to_owned(),
        );
        return failed
            .map(Json)
            .map(IntoResponse::into_response)
            .unwrap_or_else(peer_error_response);
    };
    let source_snapshot = source.snapshot();
    let Some(source_session_id) = source_snapshot.session_id else {
        let failed = state.peers.fail_turn(
            thread.id,
            thread.current_turn.id,
            "The source terminal has no active session generation.".to_owned(),
        );
        return failed
            .map(Json)
            .map(IntoResponse::into_response)
            .unwrap_or_else(peer_error_response);
    };
    if let Err(error) = source.write_automation_prompt(source_session_id, &source_prompt(&thread)) {
        tracing::warn!(
            %error,
            thread_id = %thread.id,
            "failed to deliver the peer follow-up request to the source terminal"
        );
        let failed = state.peers.fail_turn(
            thread.id,
            thread.current_turn.id,
            "The source terminal could not receive the follow-up request.".to_owned(),
        );
        return failed
            .map(Json)
            .map(IntoResponse::into_response)
            .unwrap_or_else(peer_error_response);
    }
    (StatusCode::CREATED, Json(thread)).into_response()
}

async fn dispatch_turn(
    State(state): State<AppState>,
    Path(thread_id): Path<Uuid>,
    body: Bytes,
) -> Response {
    let request: DispatchTurnRequest = match parse_body(&body, MAX_PEER_DISPATCH_BODY) {
        Ok(request) => request,
        Err(response) => return *response,
    };
    if !request.reviewer_ready {
        return api_error(
            StatusCode::BAD_REQUEST,
            "Confirm that the reviewer terminal is at an empty agent prompt.",
        );
    }
    let thread = match state.peers.dispatch_turn(
        thread_id,
        request.turn_id,
        request.handoff_revision,
        request.handoff,
    ) {
        Ok(thread) => thread,
        Err(error) => return peer_error_response(error),
    };
    let Some(reviewer_id) = thread.reviewer_terminal_id else {
        return api_error(
            StatusCode::CONFLICT,
            "The dedicated reviewer terminal is unavailable.",
        );
    };
    let Some(reviewer) = state.sessions.get(reviewer_id) else {
        return mark_delivery_failed(
            &state.peers,
            &thread,
            "The dedicated reviewer terminal is no longer available.",
        );
    };
    let reviewer_snapshot = reviewer.snapshot();
    let Some(reviewer_session_id) = reviewer_snapshot.session_id else {
        return mark_delivery_failed(
            &state.peers,
            &thread,
            "The dedicated reviewer terminal has no active session generation.",
        );
    };
    if let Err(error) =
        reviewer.write_automation_prompt(reviewer_session_id, &reviewer_prompt(&thread))
    {
        tracing::warn!(
            %error,
            thread_id = %thread.id,
            "failed to deliver the approved handoff to the reviewer terminal"
        );
        return mark_delivery_failed(
            &state.peers,
            &thread,
            "The reviewer terminal could not receive the approved handoff.",
        );
    }
    Json(thread).into_response()
}

async fn return_turn(
    State(state): State<AppState>,
    Path(thread_id): Path<Uuid>,
    body: Bytes,
) -> Response {
    let request: ReturnTurnRequest = match parse_body(&body, MAX_PEER_COMMAND_BODY) {
        Ok(request) => request,
        Err(response) => return *response,
    };
    if !request.source_ready {
        return api_error(
            StatusCode::BAD_REQUEST,
            "Confirm that the source terminal is at an empty agent prompt.",
        );
    }
    let existing = match state.peers.get_thread(thread_id) {
        Ok(thread) => thread,
        Err(error) => return peer_error_response(error),
    };
    if existing.current_turn.id != request.turn_id || existing.status != PeerStatus::ResponseReady {
        return api_error(
            StatusCode::CONFLICT,
            "The requested peer response is not ready to return.",
        );
    }
    let Some(source) = state.sessions.get(existing.source_terminal_id) else {
        return api_error(
            StatusCode::CONFLICT,
            "The source terminal is no longer available.",
        );
    };
    let source_snapshot = source.snapshot();
    if !source.is_running() {
        return api_error(StatusCode::CONFLICT, "The source terminal is not running.");
    }
    let Some(source_session_id) = source_snapshot.session_id else {
        return api_error(
            StatusCode::CONFLICT,
            "The source terminal has no active session generation.",
        );
    };

    let delivery = match state
        .peers
        .begin_return_delivery(thread_id, request.turn_id)
    {
        Ok(delivery) => delivery,
        Err(error) => return peer_error_response(error),
    };
    if let Err(error) =
        source.write_automation_prompt(source_session_id, &return_prompt(delivery.thread()))
    {
        tracing::warn!(
            %error,
            %thread_id,
            "failed to notify the source terminal about the peer response"
        );
        if let Err(rollback_error) = state.peers.abort_return_delivery(delivery) {
            tracing::error!(
                error = %rollback_error,
                %thread_id,
                "failed to roll back peer response delivery"
            );
            return api_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "The peer response delivery could not be rolled back safely.",
            );
        }
        return api_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "The peer response is stored and can be retried, but the source terminal could not be notified.",
        );
    }
    match state.peers.complete_return_delivery(delivery) {
        Ok(returned) => Json(returned).into_response(),
        Err(error) => peer_error_response(error),
    }
}

async fn close_thread(State(state): State<AppState>, Path(thread_id): Path<Uuid>) -> Response {
    close_thread_by_id(&state, thread_id).await
}

pub(crate) async fn close_thread_by_id(state: &AppState, thread_id: Uuid) -> Response {
    let close_state = state.clone();
    match tokio::spawn(async move { close_thread_owned(&close_state, thread_id).await }).await {
        Ok(response) => response,
        Err(error) => {
            tracing::error!(%error, %thread_id, "dedicated peer close task failed");
            api_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "The peer thread could not be closed safely.",
            )
        }
    }
}

async fn close_thread_owned(state: &AppState, thread_id: Uuid) -> Response {
    let closing = match state.peers.begin_close(thread_id) {
        Ok(closing) => closing,
        Err(error) => return peer_error_response(error),
    };
    if let Some(reviewer_id) = closing.thread().reviewer_terminal_id
        && let Some(reviewer) = state.sessions.get(reviewer_id)
    {
        let reviewer_session_id = reviewer.snapshot().session_id;
        if let Err(error) = state
            .sessions
            .delete_peer(
                reviewer_id,
                closing.thread().id,
                closing.thread().source_terminal_id,
                reviewer_session_id,
            )
            .await
        {
            tracing::warn!(
                %error,
                %thread_id,
                "failed to terminate a dedicated peer terminal"
            );
            if let Err(rollback_error) = state.peers.abort_close(closing) {
                tracing::error!(
                    error = %rollback_error,
                    %thread_id,
                    "failed to roll back peer thread close"
                );
            }
            return api_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "The dedicated peer terminal could not be closed.",
            );
        }
    }
    match state.peers.finalize_close(closing) {
        Ok(_) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => peer_error_response(error),
    }
}

async fn internal_request(
    State(broker): State<PeerBroker>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if !peer.ip().is_loopback() {
        return StatusCode::FORBIDDEN.into_response();
    }
    let Some(capability) = capability_header(&headers) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let request = match serde_json::from_slice::<InternalPeerRequest>(&body) {
        Ok(request) => request,
        Err(_) => return StatusCode::BAD_REQUEST.into_response(),
    };
    let result = match request {
        InternalPeerRequest::Submit { turn_id, content } => broker
            .submit(capability, turn_id, content)
            .map(|_| InternalPeerResponse {
                content: None,
                error: None,
            }),
        InternalPeerRequest::Receive { turn_id } => {
            broker
                .receive(capability, turn_id)
                .map(|artifact| InternalPeerResponse {
                    content: Some(artifact.content),
                    error: None,
                })
        }
    };
    match result {
        Ok(response) => no_store(Json(response).into_response()),
        Err(error) => no_store(internal_error_response(error)),
    }
}

fn capability_header(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix(CAPABILITY_SCHEME)
}

fn no_store(mut response: Response) -> Response {
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

fn parse_body<T: serde::de::DeserializeOwned>(
    body: &[u8],
    maximum_bytes: usize,
) -> Result<T, Box<Response>> {
    if body.len() > maximum_bytes {
        return Err(Box::new(api_error(
            StatusCode::PAYLOAD_TOO_LARGE,
            "The peer request is too large.",
        )));
    }
    serde_json::from_slice(body).map_err(|_| {
        Box::new(api_error(
            StatusCode::BAD_REQUEST,
            "The peer request is invalid.",
        ))
    })
}

fn mark_delivery_failed(state: &PeerBroker, thread: &PeerThread, message: &str) -> Response {
    state
        .fail_turn(thread.id, thread.current_turn.id, message.to_owned())
        .map(Json)
        .map(IntoResponse::into_response)
        .unwrap_or_else(peer_error_response)
}

fn registry_create_error(error: RegistryError) -> Response {
    match error {
        RegistryError::LimitReached => api_error(
            StatusCode::CONFLICT,
            "Close a terminal tab before starting a dedicated reviewer.",
        ),
        RegistryError::AgentUnavailable => api_error(
            StatusCode::BAD_REQUEST,
            "The requested reviewer agent is not configured.",
        ),
        RegistryError::InvalidPeerParent => api_error(
            StatusCode::CONFLICT,
            "The source terminal is no longer available for peer review.",
        ),
        RegistryError::InvalidPeerSession => api_error(
            StatusCode::CONFLICT,
            "The dedicated reviewer session no longer matches its peer thread.",
        ),
        RegistryError::PeerSessionManaged => api_error(
            StatusCode::CONFLICT,
            "The dedicated reviewer is managed by its peer thread.",
        ),
        RegistryError::ShuttingDown => api_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "The server is shutting down.",
        ),
        other => {
            tracing::warn!(error = %other, "dedicated peer terminal creation failed");
            api_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "The dedicated reviewer terminal could not be started.",
            )
        }
    }
}

fn peer_error_response(error: PeerError) -> Response {
    let status = match error.kind() {
        PeerErrorKind::NotFound => StatusCode::NOT_FOUND,
        PeerErrorKind::Unauthorized => StatusCode::FORBIDDEN,
        PeerErrorKind::InvalidInput => StatusCode::BAD_REQUEST,
        PeerErrorKind::PayloadTooLarge => StatusCode::PAYLOAD_TOO_LARGE,
        PeerErrorKind::InvalidState
        | PeerErrorKind::Conflict
        | PeerErrorKind::ReviewerNotBound
        | PeerErrorKind::SessionInactive
        | PeerErrorKind::LimitReached => StatusCode::CONFLICT,
    };
    api_error(status, &error.to_string())
}

fn internal_error_response(error: PeerError) -> Response {
    let status = match error.kind() {
        PeerErrorKind::Unauthorized => StatusCode::UNAUTHORIZED,
        PeerErrorKind::NotFound => StatusCode::NOT_FOUND,
        PeerErrorKind::PayloadTooLarge => StatusCode::PAYLOAD_TOO_LARGE,
        PeerErrorKind::InvalidInput => StatusCode::BAD_REQUEST,
        _ => StatusCode::CONFLICT,
    };
    (
        status,
        Json(InternalPeerResponse {
            content: None,
            error: Some(error.to_string()),
        }),
    )
        .into_response()
}

fn api_error(status: StatusCode, message: &str) -> Response {
    (
        status,
        Json(PeerApiError {
            error: message.to_owned(),
        }),
    )
        .into_response()
}

fn source_prompt(thread: &PeerThread) -> String {
    let turn = &thread.current_turn;
    format!(
        "[CWT peer {}] Prepare a concise, self-contained Markdown handoff for an independent {} by {}. Use your current conversation context; include the user goal, decisions, relevant paths or files, completed work and tests, open risks, and this instruction: {}. Do not merely print the handoff. Submit it through the local CWT bridge by running the executable in CWT_PEER_HELPER with `__cwt-peer submit --turn {} --stdin` and piping the Markdown to standard input, or replace `--stdin` with `--file PATH`. Never print CWT_PEER_CAPABILITY.",
        turn.sequence,
        action_label(turn.action),
        thread.target_agent.label(),
        compact_for_prompt(&turn.instruction),
        turn.id,
    )
}

fn reviewer_prompt(thread: &PeerThread) -> String {
    let turn = &thread.current_turn;
    format!(
        "[CWT peer {}] A supervised {} handoff is ready. Retrieve it by running the executable in CWT_PEER_HELPER with `__cwt-peer receive --turn {}`. Inspect the stated workspace and evidence independently; no Git or repository is assumed. Then submit a concise Markdown response through `__cwt-peer submit --turn {} --stdin` with piped standard input, or replace `--stdin` with `--file PATH`. Never print CWT_PEER_CAPABILITY.",
        turn.sequence,
        action_label(turn.action),
        turn.id,
        turn.id,
    )
}

fn return_prompt(thread: &PeerThread) -> String {
    format!(
        "[CWT peer {}] The dedicated {} reviewer has returned a response. Retrieve it by running the executable in CWT_PEER_HELPER with `__cwt-peer receive --turn {}`. Evaluate the findings against your current context and present the useful conclusion to the user. Never print CWT_PEER_CAPABILITY.",
        thread.current_turn.sequence,
        thread.target_agent.label(),
        thread.current_turn.id,
    )
}

fn action_label(action: PeerAction) -> &'static str {
    match action {
        PeerAction::Review => "review",
        PeerAction::Verify => "verification",
        PeerAction::Ask => "question",
        PeerAction::Handoff => "handoff",
        PeerAction::Recheck => "recheck",
    }
}

fn compact_for_prompt(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use axum::{
        body::{Body, to_bytes},
        http::Request,
    };
    use tower::ServiceExt;

    use super::*;
    use crate::peer::{CWT_PEER_CAPABILITY_ENV, SessionPurpose};

    #[test]
    fn browser_instruction_cannot_add_terminal_control_lines() {
        assert_eq!(
            compact_for_prompt("review this\r\n\u{1b}[2Jthen that"),
            "review this [2Jthen that"
        );
    }

    #[test]
    fn initial_recheck_is_rejected_before_creating_a_thread() {
        let body = serde_json::to_vec(&serde_json::json!({
            "sourceTerminalId": Uuid::new_v4(),
            "targetAgent": "claude",
            "action": "recheck",
            "instruction": "Check again",
            "sourceReady": true
        }))
        .expect("request");
        let request: CreateThreadRequest =
            serde_json::from_slice(&body).expect("valid request shape");

        assert_eq!(request.action, PeerAction::Recheck);
    }

    #[tokio::test]
    async fn private_bridge_accepts_only_a_scoped_capability_from_loopback() {
        let broker = PeerBroker::new(
            "127.0.0.1:43123".parse().expect("loopback endpoint"),
            "codex-web".into(),
        );
        let source_terminal_id = Uuid::new_v4();
        let activation = broker
            .activate_session(
                source_terminal_id,
                Uuid::new_v4(),
                &SessionPurpose::Interactive,
            )
            .expect("source activation");
        let capability = activation
            .environment()
            .iter()
            .find(|(name, _)| name == CWT_PEER_CAPABILITY_ENV)
            .and_then(|(_, value)| value.to_str())
            .expect("capability");
        let thread = broker
            .create_thread(
                source_terminal_id,
                AgentKind::Claude,
                PeerAction::Review,
                "Review the implementation.".to_owned(),
            )
            .expect("thread");
        let payload = serde_json::to_vec(&InternalPeerRequest::Submit {
            turn_id: thread.current_turn.id,
            content: "Bounded handoff".to_owned(),
        })
        .expect("bridge payload");

        let response = internal_router(broker.clone())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(INTERNAL_PEER_PATH)
                    .header(
                        header::AUTHORIZATION,
                        format!("{CAPABILITY_SCHEME}{capability}"),
                    )
                    .extension(ConnectInfo(SocketAddr::from((
                        std::net::Ipv4Addr::LOCALHOST,
                        45_010,
                    ))))
                    .body(Body::from(payload.clone()))
                    .expect("request"),
            )
            .await
            .expect("bridge response");
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), MAX_INTERNAL_PEER_BODY)
            .await
            .expect("response body");
        let decoded: InternalPeerResponse = serde_json::from_slice(&body).expect("response JSON");
        assert!(decoded.error.is_none());
        assert_eq!(
            broker.get_thread(thread.id).expect("updated thread").status,
            PeerStatus::AwaitingPreview
        );

        let remote = internal_router(broker)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(INTERNAL_PEER_PATH)
                    .header(
                        header::AUTHORIZATION,
                        format!("{CAPABILITY_SCHEME}{capability}"),
                    )
                    .extension(ConnectInfo(SocketAddr::from((
                        std::net::Ipv4Addr::new(192, 0, 2, 10),
                        45_011,
                    ))))
                    .body(Body::from(payload))
                    .expect("remote request"),
            )
            .await
            .expect("remote bridge response");
        assert_eq!(remote.status(), StatusCode::FORBIDDEN);
    }
}
