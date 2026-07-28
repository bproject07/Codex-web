use std::net::SocketAddr;

use axum::{
    extract::{
        ConnectInfo, Query, State,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
};
use bytes::{Bytes, BytesMut};
use futures_util::{SinkExt, StreamExt, stream::SplitSink};
use serde::Deserialize;
use tokio::sync::{OwnedSemaphorePermit, broadcast};
use uuid::Uuid;

use crate::{
    auth::{AuthDecision, origin_is_allowed},
    protocol::{
        ClientControl, MAX_INPUT_MESSAGE_SIZE, MAX_WEBSOCKET_MESSAGE_SIZE, ServerControl,
        parse_control_message,
    },
    registry::SessionRegistry,
    routes::AppState,
    session::{OutputChunk, SessionManager},
};

const OUTPUT_WEBSOCKET_BATCH_SIZE: usize = 32 * 1024;

#[derive(Deserialize)]
pub struct WebSocketQuery {
    token: Option<String>,
    #[serde(rename = "terminalId")]
    terminal_id: Option<String>,
}

pub async fn upgrade(
    websocket: WebSocketUpgrade,
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Query(query): Query<WebSocketQuery>,
    headers: HeaderMap,
) -> Response {
    match state.auth.authenticate(peer.ip(), query.token.as_deref()) {
        AuthDecision::Allowed => {}
        AuthDecision::Invalid => return StatusCode::UNAUTHORIZED.into_response(),
        AuthDecision::Blocked => return StatusCode::TOO_MANY_REQUESTS.into_response(),
    }

    let origin = headers
        .get(header::ORIGIN)
        .and_then(|value| value.to_str().ok());
    let host = headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok());
    if !origin_is_allowed(origin, host, state.config.host) {
        tracing::warn!(client = %peer.ip(), "rejected WebSocket origin");
        return StatusCode::FORBIDDEN.into_response();
    }

    let session = match resolve_session(&state.sessions, query.terminal_id.as_deref()) {
        Ok(session) => session,
        Err(status) => return status.into_response(),
    };
    let terminal_id = session.snapshot().terminal_id;

    let Some(client_permit) = session.try_acquire_client() else {
        return StatusCode::TOO_MANY_REQUESTS.into_response();
    };

    websocket
        .max_message_size(MAX_WEBSOCKET_MESSAGE_SIZE)
        .max_frame_size(MAX_WEBSOCKET_MESSAGE_SIZE)
        .on_upgrade(move |socket| {
            handle_socket(socket, state, session, peer, terminal_id, client_permit)
        })
        .into_response()
}

async fn handle_socket(
    socket: WebSocket,
    state: AppState,
    session: SessionManager,
    peer: SocketAddr,
    terminal_id: Uuid,
    _client_permit: OwnedSemaphorePermit,
) {
    tracing::info!(client = %peer.ip(), %terminal_id, "terminal client connected");
    session.notify_client_count_changed();

    let (mut sender, mut receiver) = socket.split();
    let mut output_receiver = session.subscribe_output();
    let mut event_receiver = session.subscribe_events();
    let session_shutdown = session.shutdown_signal();
    let mut current_session_id = None;
    let mut last_sequence = 0;

    if send_control(
        &mut sender,
        ServerControl::Session {
            session: session.snapshot(),
        },
    )
    .await
    .is_err()
        || send_replay(
            &session,
            &mut sender,
            &mut current_session_id,
            &mut last_sequence,
        )
        .await
        .is_err()
    {
        tracing::info!(client = %peer.ip(), %terminal_id, "terminal client disconnected during setup");
        drop(_client_permit);
        session.notify_client_count_changed();
        return;
    }

    loop {
        tokio::select! {
            _ = state.shutdown.cancelled() => {
                let _ = sender.send(Message::Close(None)).await;
                break;
            }
            _ = session_shutdown.cancelled() => {
                let _ = sender.send(Message::Close(None)).await;
                break;
            }
            incoming = receiver.next() => {
                match incoming {
                    Some(Ok(message)) => {
                        if !handle_client_message(
                            message,
                            &state.sessions,
                            terminal_id,
                            &session,
                            &mut sender,
                        ).await {
                            break;
                        }
                    }
                    Some(Err(error)) => {
                        tracing::warn!(client = %peer.ip(), %error, "WebSocket receive error");
                        break;
                    }
                    None => break,
                }
            }
            output = output_receiver.changed() => {
                if output.is_err() {
                    break;
                }
                if send_pending_output(
                    &session,
                    &mut sender,
                    &mut current_session_id,
                    &mut last_sequence,
                ).await.is_err() {
                    break;
                }
            }
            event = event_receiver.recv() => {
                match event {
                    Ok(()) | Err(broadcast::error::RecvError::Lagged(_)) => {
                        let snapshot = session.snapshot();
                        if snapshot.session_id != current_session_id
                            && send_replay(
                                &session,
                                &mut sender,
                                &mut current_session_id,
                                &mut last_sequence,
                            ).await.is_err()
                        {
                            break;
                        }
                        if send_control(
                            &mut sender,
                            ServerControl::Session { session: snapshot },
                        )
                        .await
                        .is_err()
                        {
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        }
    }

    drop(receiver);
    drop(sender);
    drop(_client_permit);
    session.notify_client_count_changed();
    tracing::info!(client = %peer.ip(), %terminal_id, "terminal client disconnected");
}

fn resolve_session(
    registry: &SessionRegistry,
    requested_terminal_id: Option<&str>,
) -> Result<SessionManager, StatusCode> {
    let Some(requested_terminal_id) = requested_terminal_id else {
        return Ok(registry.primary());
    };
    let terminal_id =
        Uuid::parse_str(requested_terminal_id).map_err(|_| StatusCode::BAD_REQUEST)?;
    registry.get(terminal_id).ok_or(StatusCode::NOT_FOUND)
}

async fn handle_client_message(
    message: Message,
    registry: &SessionRegistry,
    terminal_id: Uuid,
    session: &SessionManager,
    sender: &mut SplitSink<WebSocket, Message>,
) -> bool {
    match message {
        Message::Binary(data) => {
            if data.len() > MAX_INPUT_MESSAGE_SIZE {
                return send_protocol_error(
                    sender,
                    "input_too_large",
                    "Terminal input message is too large.",
                )
                .await;
            }

            if let Err(error) = session.write_input(&data) {
                tracing::warn!(%error, "terminal input could not be forwarded");
                return send_protocol_error(sender, "input_failed", &error.to_string()).await;
            }
            true
        }
        Message::Text(text) => {
            match parse_control_message(text.as_str()) {
                Ok(ClientControl::Resize { cols, rows }) => {
                    if let Err(error) = session.resize(cols, rows) {
                        tracing::warn!(%error, "terminal resize failed");
                        return send_protocol_error(sender, "resize_failed", &error.to_string())
                            .await;
                    }
                }
                Ok(ClientControl::Ping) => {
                    if send_control(sender, ServerControl::Pong).await.is_err() {
                        return false;
                    }
                }
                Ok(ClientControl::Restart) => {
                    if let Err(error) = registry.restart(terminal_id).await {
                        tracing::warn!(%error, %terminal_id, "WebSocket restart rejected or failed");
                        return send_protocol_error(sender, "restart_failed", &error.to_string())
                            .await;
                    }
                }
                Err(error) => {
                    tracing::warn!(%error, "invalid WebSocket control message");
                    return send_protocol_error(
                        sender,
                        "invalid_control",
                        "Invalid terminal control message.",
                    )
                    .await;
                }
            }
            true
        }
        Message::Ping(data) => sender.send(Message::Pong(data)).await.is_ok(),
        Message::Pong(_) => true,
        Message::Close(_) => false,
    }
}

async fn send_pending_output(
    session: &SessionManager,
    sender: &mut SplitSink<WebSocket, Message>,
    current_session_id: &mut Option<Uuid>,
    last_sequence: &mut u64,
) -> Result<(), axum::Error> {
    let Some(delta) = session.output_since(*current_session_id, *last_sequence) else {
        send_replay(session, sender, current_session_id, last_sequence).await?;
        return Ok(());
    };

    send_output_batches(sender, delta.chunks).await?;
    *current_session_id = delta.session_id;
    *last_sequence = delta.last_sequence;
    Ok(())
}

async fn send_replay(
    session: &SessionManager,
    sender: &mut SplitSink<WebSocket, Message>,
    current_session_id: &mut Option<Uuid>,
    last_sequence: &mut u64,
) -> Result<(), axum::Error> {
    let snapshot = session.output_snapshot();
    send_control(
        sender,
        ServerControl::ReplayStart {
            session_id: snapshot.session_id,
        },
    )
    .await?;

    send_output_batches(sender, snapshot.chunks).await?;

    send_control(
        sender,
        ServerControl::ReplayEnd {
            last_sequence: snapshot.last_sequence,
        },
    )
    .await?;
    *current_session_id = snapshot.session_id;
    *last_sequence = snapshot.last_sequence;
    Ok(())
}

async fn send_output_batches(
    sender: &mut SplitSink<WebSocket, Message>,
    chunks: Vec<OutputChunk>,
) -> Result<(), axum::Error> {
    for batch in coalesce_output_chunks(chunks) {
        sender.send(Message::Binary(batch)).await?;
    }
    Ok(())
}

fn coalesce_output_chunks(chunks: Vec<OutputChunk>) -> Vec<Bytes> {
    let mut batches = Vec::new();
    let mut batch = BytesMut::with_capacity(OUTPUT_WEBSOCKET_BATCH_SIZE);

    for chunk in chunks {
        let mut remaining = chunk.data.as_ref();
        while !remaining.is_empty() {
            let available = OUTPUT_WEBSOCKET_BATCH_SIZE.saturating_sub(batch.len());
            let length = available.min(remaining.len());
            batch.extend_from_slice(&remaining[..length]);
            remaining = &remaining[length..];

            if batch.len() == OUTPUT_WEBSOCKET_BATCH_SIZE {
                batches.push(batch.split().freeze());
            }
        }
    }

    if !batch.is_empty() {
        batches.push(batch.freeze());
    }
    batches
}

async fn send_protocol_error(
    sender: &mut SplitSink<WebSocket, Message>,
    code: &'static str,
    message: &str,
) -> bool {
    send_control(
        sender,
        ServerControl::Error {
            code,
            message: message.chars().take(256).collect(),
        },
    )
    .await
    .is_ok()
}

async fn send_control(
    sender: &mut SplitSink<WebSocket, Message>,
    control: ServerControl,
) -> Result<(), axum::Error> {
    let text = serde_json::to_string(&control).expect("server control messages are serializable");
    sender.send(Message::Text(text.into())).await
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::{
        config::{AgentKind, ShellKind},
        terminal::TerminalConfig,
    };

    fn registry() -> SessionRegistry {
        SessionRegistry::new(TerminalConfig {
            project_dir: PathBuf::from("."),
            command: "codex".to_owned(),
            arguments: Vec::new(),
            agent: AgentKind::Codex,
            shell: ShellKind::Powershell,
        })
    }

    #[test]
    fn missing_terminal_id_selects_the_primary_session() {
        let registry = registry();
        let primary_id = registry.primary().snapshot().terminal_id;

        let selected = resolve_session(&registry, None).expect("primary session");
        assert_eq!(selected.snapshot().terminal_id, primary_id);
    }

    #[test]
    fn rejects_malformed_and_unknown_terminal_ids() {
        let registry = registry();

        assert!(matches!(
            resolve_session(&registry, Some("not-a-uuid")),
            Err(StatusCode::BAD_REQUEST)
        ));
        assert!(matches!(
            resolve_session(&registry, Some(&Uuid::new_v4().to_string())),
            Err(StatusCode::NOT_FOUND)
        ));
    }

    #[test]
    fn output_chunks_are_coalesced_without_exceeding_the_frame_target() {
        let session_id = Uuid::new_v4();
        let chunks = vec![
            OutputChunk {
                sequence: 1,
                session_id,
                data: Bytes::from(vec![b'a'; 20 * 1024]),
            },
            OutputChunk {
                sequence: 2,
                session_id,
                data: Bytes::from(vec![b'b'; 20 * 1024]),
            },
            OutputChunk {
                sequence: 3,
                session_id,
                data: Bytes::from_static(b"done"),
            },
        ];

        let batches = coalesce_output_chunks(chunks);
        assert_eq!(batches.len(), 2);
        assert!(
            batches
                .iter()
                .all(|batch| batch.len() <= OUTPUT_WEBSOCKET_BATCH_SIZE)
        );

        let combined: Vec<u8> = batches
            .iter()
            .flat_map(|batch| batch.iter().copied())
            .collect();
        let mut expected = vec![b'a'; 20 * 1024];
        expected.extend(vec![b'b'; 20 * 1024]);
        expected.extend_from_slice(b"done");
        assert_eq!(combined, expected);
    }

    #[test]
    fn output_batching_handles_empty_boundary_and_oversized_chunks() {
        let session_id = Uuid::new_v4();
        for length in [
            0,
            1,
            OUTPUT_WEBSOCKET_BATCH_SIZE - 1,
            OUTPUT_WEBSOCKET_BATCH_SIZE,
            OUTPUT_WEBSOCKET_BATCH_SIZE + 1,
            OUTPUT_WEBSOCKET_BATCH_SIZE * 2 + 17,
        ] {
            let expected: Vec<u8> = (0..length).map(|index| (index % 251) as u8).collect();
            let chunks = vec![OutputChunk {
                sequence: 1,
                session_id,
                data: Bytes::copy_from_slice(&expected),
            }];

            let batches = coalesce_output_chunks(chunks);
            assert!(
                batches
                    .iter()
                    .all(|batch| !batch.is_empty() && batch.len() <= OUTPUT_WEBSOCKET_BATCH_SIZE)
            );
            let combined: Vec<u8> = batches
                .iter()
                .flat_map(|batch| batch.iter().copied())
                .collect();
            assert_eq!(combined, expected);
        }
    }
}
