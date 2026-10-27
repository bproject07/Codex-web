use std::net::{IpAddr, SocketAddr};

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
    auth::{AuthDecision, AuthState, origin_is_allowed},
    protocol::{
        ClientControl, GuestControl, MAX_INPUT_MESSAGE_SIZE, MAX_WEBSOCKET_MESSAGE_SIZE,
        ServerControl, parse_control_message, parse_guest_control,
    },
    registry::SessionRegistry,
    routes::AppState,
    session::{OUTPUT_READ_CHUNK, OUTPUT_REPLAY_LIMIT, OutputChunk, SessionManager},
    sharing::{Guest, GuestConnection},
};

const OUTPUT_WEBSOCKET_BATCH_SIZE: usize = 32 * 1024;
const OUTPUT_WINDOW: usize = 4 * 1024 * 1024;
const LIVE_DELTA_MAX: usize = 1024 * 1024;
const _: () = assert!(OUTPUT_REPLAY_LIMIT <= OUTPUT_WINDOW);

struct OutputFlow {
    enabled: bool,
    outstanding: usize,
}

impl OutputFlow {
    fn can_send(&self) -> bool {
        !self.enabled || self.outstanding <= OUTPUT_WINDOW - OUTPUT_READ_CHUNK
    }

    fn sent(&mut self, bytes: usize) {
        if self.enabled {
            self.outstanding += bytes;
            debug_assert!(self.outstanding <= OUTPUT_WINDOW);
        }
    }

    fn acknowledge(&mut self, bytes: u32) -> bool {
        let bytes = bytes as usize;
        if !self.enabled || bytes == 0 || bytes > self.outstanding {
            return false;
        }
        self.outstanding -= bytes;
        true
    }
}

#[derive(Deserialize)]
pub struct WebSocketQuery {
    token: Option<String>,
    #[serde(rename = "terminalId")]
    terminal_id: Option<String>,
    #[serde(rename = "flowControl")]
    flow_control: Option<u8>,
}

pub async fn upgrade(
    websocket: WebSocketUpgrade,
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Query(query): Query<WebSocketQuery>,
    headers: HeaderMap,
) -> Response {
    let origin = headers
        .get(header::ORIGIN)
        .and_then(|value| value.to_str().ok());
    let host = headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok());
    if let Err(status) = authorize_upgrade(
        &state.auth,
        peer.ip(),
        query.token.as_deref(),
        origin,
        host,
        state.config.host,
    ) {
        if status == StatusCode::FORBIDDEN {
            tracing::warn!(client = %peer.ip(), "rejected WebSocket origin");
        }
        return status.into_response();
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
            handle_socket(
                socket,
                state,
                session,
                peer,
                terminal_id,
                Access::Owner {
                    _permit: client_permit,
                },
                query.flow_control == Some(1),
            )
        })
        .into_response()
}

fn authorize_upgrade(
    auth: &AuthState,
    address: IpAddr,
    candidate: Option<&str>,
    origin_header: Option<&str>,
    host_header: Option<&str>,
    bind_host: IpAddr,
) -> Result<(), StatusCode> {
    if !origin_is_allowed(origin_header, host_header, bind_host) {
        return Err(StatusCode::FORBIDDEN);
    }

    match auth.authenticate(address, candidate) {
        AuthDecision::Allowed => Ok(()),
        AuthDecision::Invalid => Err(StatusCode::UNAUTHORIZED),
        AuthDecision::Blocked => Err(StatusCode::TOO_MANY_REQUESTS),
    }
}

async fn handle_socket(
    socket: WebSocket,
    state: AppState,
    session: SessionManager,
    peer: SocketAddr,
    terminal_id: Uuid,
    access: Access,
    flow_control: bool,
) {
    // Also cancel a slow replay/write, rather than waiting for its whole batch.
    let cancellation = access.cancellation();
    let lifetime = match &access {
        Access::Guest(guest, _) => std::time::Duration::from_millis(
            guest
                .expires_at
                .saturating_sub(crate::sharing_store::now())
                .max(0) as u64,
        ),
        Access::Owner { .. } => std::time::Duration::from_secs(365 * 24 * 3600),
    };
    tokio::select! {
        biased;
        _ = cancellation.cancelled() => {}
        _ = tokio::time::sleep(lifetime), if access.generation().is_some() => {}
        _ = handle_socket_inner(socket, state, session, peer, terminal_id, access, flow_control) => {}
    }
}

async fn handle_socket_inner(
    socket: WebSocket,
    state: AppState,
    session: SessionManager,
    peer: SocketAddr,
    terminal_id: Uuid,
    access: Access,
    flow_control: bool,
) {
    tracing::info!(client = %peer.ip(), %terminal_id, "terminal client connected");
    session.notify_client_count_changed();

    let (mut sender, mut receiver) = socket.split();
    let mut output_receiver = session.subscribe_output();
    let mut event_receiver = session.subscribe_events();
    let session_shutdown = session.shutdown_signal();
    let mut current_session_id = None;
    let mut last_sequence = 0;
    let mut flow = OutputFlow {
        enabled: flow_control,
        outstanding: 0,
    };
    let mut output_pending = true;
    let mut replay_required = false;
    let pin = access.generation();
    let revoke = access.cancellation();
    let mut expiry_tick = tokio::time::interval(std::time::Duration::from_secs(1));

    if flow.enabled
        && send_control(
            &mut sender,
            ServerControl::FlowControl {
                window_bytes: OUTPUT_WINDOW,
            },
        )
        .await
        .is_err()
    {
        drop(access);
        session.notify_client_count_changed();
        return;
    }

    if send_metadata(&mut sender, &session, &access).await.is_err()
        || send_replay(
            &session,
            &mut sender,
            &mut current_session_id,
            &mut last_sequence,
            &mut flow,
            pin,
        )
        .await
        .is_err()
    {
        tracing::info!(client = %peer.ip(), %terminal_id, "terminal client disconnected during setup");
        drop(access);
        session.notify_client_count_changed();
        return;
    }

    loop {
        tokio::select! {
            _ = revoke.cancelled() => {
                let _ = send_message(&mut sender, Message::Close(None)).await;
                break;
            }
            _ = expiry_tick.tick(), if pin.is_some() => {
                if access.expired() || session.snapshot().session_id != pin {
                    let _ = send_message(&mut sender, Message::Close(None)).await;
                    break;
                }
            }
            _ = state.shutdown.cancelled() => {
                let _ = send_message(&mut sender, Message::Close(None)).await;
                break;
            }
            _ = session_shutdown.cancelled() => {
                let _ = send_message(&mut sender, Message::Close(None)).await;
                break;
            }
            incoming = receiver.next() => {
                match incoming {
                    Some(Ok(message)) => {
                        let keep = if let Access::Guest(_, connection) = &access {
                            connection.touch(None);
                            handle_guest_message(message, connection, &mut sender, &mut flow).await
                        } else { handle_client_message(
                            message,
                            &state.sessions,
                            terminal_id,
                            &session,
                            &mut sender,
                            &mut flow,
                        ).await };
                        if !keep {
                            break;
                        }
                        // Credit acknowledgements must wake a finite pending delta
                        // even when the PTY has produced no further output.
                        output_pending = true;
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
                output_pending = true;
            }
            _ = std::future::ready(()), if output_pending &&
                ((!replay_required && flow.can_send()) ||
                 (replay_required && (!flow.enabled || flow.outstanding == 0))) => {
                match send_pending_output(
                    &session,
                    &mut sender,
                    &mut current_session_id,
                    &mut last_sequence,
                    &mut flow,
                    &mut replay_required,
                    pin,
                ).await {
                    Ok(pending) => output_pending = pending,
                    Err(_) => break,
                }
            }
            event = event_receiver.recv() => {
                match event {
                    Ok(()) | Err(broadcast::error::RecvError::Lagged(_)) => {
                        let snapshot = session.snapshot();
                        if snapshot.session_id != current_session_id {
                            output_pending = true;
                        }
                        if send_metadata(&mut sender, &session, &access)
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
    drop(access);
    session.notify_client_count_changed();
    tracing::info!(client = %peer.ip(), %terminal_id, "terminal client disconnected");
}

enum Access {
    Owner { _permit: OwnedSemaphorePermit },
    Guest(Guest, GuestConnection),
}
impl Access {
    fn generation(&self) -> Option<Uuid> {
        match self {
            Self::Owner { .. } => None,
            Self::Guest(guest, _) => Some(guest.session_id),
        }
    }
    fn cancellation(&self) -> tokio_util::sync::CancellationToken {
        match self {
            Self::Owner { .. } => tokio_util::sync::CancellationToken::new(),
            Self::Guest(_, connection) => connection.cancel.clone(),
        }
    }
    fn expired(&self) -> bool {
        matches!(self, Self::Guest(guest,_) if guest.expires_at <= crate::sharing_store::now())
    }
}

pub async fn serve_guest(
    socket: WebSocket,
    state: AppState,
    peer: SocketAddr,
    guest: Guest,
    connection: GuestConnection,
) {
    let Some(session) = state.sessions.get(guest.terminal_id) else {
        return;
    };
    let terminal_id = guest.terminal_id;
    handle_socket(
        socket,
        state,
        session,
        peer,
        terminal_id,
        Access::Guest(guest, connection),
        true,
    )
    .await;
}

async fn send_metadata(
    sender: &mut SplitSink<WebSocket, Message>,
    session: &SessionManager,
    access: &Access,
) -> Result<(), axum::Error> {
    let snapshot = session.snapshot();
    if let Access::Guest(guest, _) = access {
        if snapshot.session_id != Some(guest.session_id) || access.expired() {
            return Err(websocket_timeout_error());
        }
        send_control(
            sender,
            ServerControl::SharedSession {
                session_id: guest.session_id,
                agent: snapshot.agent.label().to_owned(),
                status: snapshot.status,
                cols: snapshot.cols,
                rows: snapshot.rows,
                archive_incomplete: snapshot.archive_incomplete,
            },
        )
        .await
    } else {
        send_control(sender, ServerControl::Session { session: snapshot }).await
    }
}

async fn handle_guest_message(
    message: Message,
    connection: &GuestConnection,
    sender: &mut SplitSink<WebSocket, Message>,
    flow: &mut OutputFlow,
) -> bool {
    match message {
        Message::Text(text) if text.len() <= 4096 => match parse_guest_control(&text) {
            Ok(GuestControl::Ping {}) => send_control(sender, ServerControl::Pong).await.is_ok(),
            Ok(GuestControl::OutputAck { bytes }) => flow.acknowledge(bytes),
            Ok(GuestControl::Presence { visible }) => {
                connection.touch(Some(visible));
                true
            }
            Err(_) => false,
        },
        Message::Ping(data) => send_message(sender, Message::Pong(data)).await.is_ok(),
        Message::Pong(_) => true,
        _ => false,
    }
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
    flow: &mut OutputFlow,
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
                Ok(ClientControl::OutputAck { bytes }) => {
                    if !flow.acknowledge(bytes) {
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
        Message::Ping(data) => send_message(sender, Message::Pong(data)).await.is_ok(),
        Message::Pong(_) => true,
        Message::Close(_) => false,
    }
}

async fn send_pending_output(
    session: &SessionManager,
    sender: &mut SplitSink<WebSocket, Message>,
    current_session_id: &mut Option<Uuid>,
    last_sequence: &mut u64,
    flow: &mut OutputFlow,
    replay_required: &mut bool,
    pin: Option<Uuid>,
) -> Result<bool, axum::Error> {
    if *replay_required {
        send_replay(
            session,
            sender,
            current_session_id,
            last_sequence,
            flow,
            pin,
        )
        .await?;
        *replay_required = false;
        return Ok(true);
    }
    let budget = if flow.enabled {
        OUTPUT_WINDOW.saturating_sub(flow.outstanding)
    } else {
        LIVE_DELTA_MAX
    };
    let Some(delta) = session.output_since_limited(
        *current_session_id,
        *last_sequence,
        budget.min(LIVE_DELTA_MAX),
    ) else {
        *replay_required = true;
        return Ok(true);
    };
    if pin.is_some() && delta.session_id != pin {
        return Err(websocket_timeout_error());
    }

    let bytes = delta.chunks.iter().map(|chunk| chunk.data.len()).sum();
    send_output_batches(sender, delta.chunks).await?;
    flow.sent(bytes);
    *current_session_id = delta.session_id;
    *last_sequence = delta.last_sequence;
    Ok(bytes > 0)
}

async fn send_replay(
    session: &SessionManager,
    sender: &mut SplitSink<WebSocket, Message>,
    current_session_id: &mut Option<Uuid>,
    last_sequence: &mut u64,
    flow: &mut OutputFlow,
    pin: Option<Uuid>,
) -> Result<(), axum::Error> {
    let snapshot = session.output_snapshot();
    if pin.is_some() && snapshot.session_id != pin {
        return Err(websocket_timeout_error());
    }
    send_control(
        sender,
        ServerControl::ReplayStart {
            session_id: snapshot.session_id,
        },
    )
    .await?;

    let bytes = snapshot.chunks.iter().map(|chunk| chunk.data.len()).sum();
    send_output_batches(sender, snapshot.chunks).await?;
    flow.sent(bytes);

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
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        for batch in coalesce_output_chunks(chunks) {
            send_message(sender, Message::Binary(batch)).await?;
        }
        Ok(())
    })
    .await
    .map_err(|_| websocket_timeout_error())?
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
    send_message(sender, Message::Text(text.into())).await
}

async fn send_message(
    sender: &mut SplitSink<WebSocket, Message>,
    message: Message,
) -> Result<(), axum::Error> {
    tokio::time::timeout(std::time::Duration::from_secs(10), sender.send(message))
        .await
        .map_err(|_| websocket_timeout_error())?
}

fn websocket_timeout_error() -> axum::Error {
    axum::Error::new(std::io::Error::new(
        std::io::ErrorKind::TimedOut,
        "WebSocket send timed out",
    ))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    #[test]
    fn output_window_requires_real_bounded_acknowledgements() {
        let mut flow = OutputFlow {
            enabled: true,
            outstanding: 0,
        };
        flow.sent(OUTPUT_REPLAY_LIMIT);
        assert!(!flow.can_send());
        assert!(!flow.acknowledge(0));
        assert!(!flow.acknowledge((OUTPUT_WINDOW + 1) as u32));
        assert!(flow.acknowledge(OUTPUT_READ_CHUNK as u32));
        assert!(flow.can_send());
        assert!(flow.acknowledge((OUTPUT_REPLAY_LIMIT - OUTPUT_READ_CHUNK) as u32));
        assert!(!flow.acknowledge(1));
    }
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
    fn rejected_origins_do_not_consume_the_auth_failure_budget() {
        let auth = AuthState::new("0123456789abcdef".to_owned());
        let address = "127.0.0.1".parse().expect("valid IP");
        let bind_host = "127.0.0.1".parse().expect("valid IP");

        for _ in 0..10 {
            assert_eq!(
                authorize_upgrade(
                    &auth,
                    address,
                    Some("wrong-token-value"),
                    Some("https://evil.example"),
                    Some("127.0.0.1:8787"),
                    bind_host,
                ),
                Err(StatusCode::FORBIDDEN)
            );
        }

        assert_eq!(
            authorize_upgrade(
                &auth,
                address,
                Some("wrong-token-value"),
                Some("http://localhost:5173"),
                Some("127.0.0.1:8787"),
                bind_host,
            ),
            Err(StatusCode::UNAUTHORIZED)
        );
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
