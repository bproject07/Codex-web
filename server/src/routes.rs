use std::{path::PathBuf, sync::Arc};

use axum::{
    Json, Router,
    extract::{Path, State},
    http::{HeaderValue, StatusCode, header},
    middleware::{self, Next},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
};
use serde::Serialize;
use tokio_util::sync::CancellationToken;
use tower_http::services::{ServeDir, ServeFile};

use crate::{
    auth::{self, AuthState},
    config::Config,
    registry::{MAX_SESSIONS, RegistryError, SessionRegistry},
    session::SessionSnapshot,
    websocket,
};
use uuid::Uuid;

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    pub auth: AuthState,
    pub sessions: SessionRegistry,
    pub shutdown: CancellationToken,
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
}

#[derive(Debug, Serialize)]
struct ErrorResponse {
    error: &'static str,
}

pub fn build_router(state: AppState, static_directory: Option<PathBuf>) -> Router {
    let protected_api = Router::new()
        .route("/api/health", get(health))
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
        max_sessions: MAX_SESSIONS,
    })
}

async fn primary_session(State(state): State<AppState>) -> Json<SessionSnapshot> {
    Json(state.sessions.primary().snapshot())
}

async fn sessions(State(state): State<AppState>) -> Json<Vec<SessionSnapshot>> {
    Json(state.sessions.list())
}

async fn create_session(State(state): State<AppState>) -> Response {
    match state.sessions.create().await {
        Ok(session) => (StatusCode::CREATED, Json(session)).into_response(),
        Err(RegistryError::LimitReached) => (
            StatusCode::CONFLICT,
            Json(ErrorResponse {
                error: "The maximum number of terminal sessions is already running.",
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
        Err(error) => {
            tracing::error!(%error, "terminal session create request failed");
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(ErrorResponse {
                    error: "Codex could not be started. Check the session list and server log.",
                }),
            )
                .into_response()
        }
    }
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
    match state.sessions.restart(terminal_id).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(RegistryError::NotFound) => session_not_found(),
        Err(error) => {
            tracing::error!(%error, %terminal_id, "session restart request failed");
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(ErrorResponse {
                    error: "Codex could not be restarted. Check the session status and server log.",
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
    match state.sessions.terminate(terminal_id).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(RegistryError::NotFound) => session_not_found(),
        Err(error) => {
            tracing::error!(%error, %terminal_id, "session terminate request failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    error: "Codex could not be terminated. Check the server log.",
                }),
            )
                .into_response()
        }
    }
}

async fn delete_session(State(state): State<AppState>, Path(terminal_id): Path<Uuid>) -> Response {
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
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
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
