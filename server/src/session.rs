use std::{
    collections::VecDeque,
    io::{ErrorKind, Read, Write},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{Receiver, RecvTimeoutError, SyncSender, TrySendError, sync_channel},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail};
use bytes::Bytes;
use portable_pty::{MasterPty, PtySize};
use serde::Serialize;
use tokio::sync::{OwnedSemaphorePermit, Semaphore, broadcast, watch};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::{
    protocol::validate_resize,
    terminal::{self, TerminalConfig},
};

pub const OUTPUT_BUFFER_LIMIT: usize = 16 * 1024 * 1024;
pub const MAX_CONNECTED_CLIENTS: usize = 4;
const OUTPUT_REPLAY_LIMIT: usize = 2 * 1024 * 1024;
const INPUT_QUEUE_CAPACITY: usize = 256;
const EVENT_BROADCAST_CAPACITY: usize = 64;
const MAX_PUBLIC_ERROR_LENGTH: usize = 512;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Lifecycle {
    Idle,
    Starting,
    Running,
    Terminating,
    Terminated,
    Exited,
    Failed,
}

impl Lifecycle {
    pub fn can_transition_to(self, next: Self) -> bool {
        if self == next {
            return true;
        }

        matches!(
            (self, next),
            (Self::Idle, Self::Starting | Self::Terminated)
                | (
                    Self::Starting,
                    Self::Running | Self::Failed | Self::Exited | Self::Terminating
                )
                | (
                    Self::Running,
                    Self::Exited | Self::Terminating | Self::Failed
                )
                | (Self::Terminating, Self::Terminated | Self::Exited)
                | (
                    Self::Terminated | Self::Exited | Self::Failed,
                    Self::Starting
                )
                | (Self::Exited | Self::Failed, Self::Terminated)
        )
    }
}

#[derive(Debug, Clone, Copy)]
pub struct SessionStateMachine {
    state: Lifecycle,
}

impl SessionStateMachine {
    fn new() -> Self {
        Self {
            state: Lifecycle::Idle,
        }
    }

    pub fn state(&self) -> Lifecycle {
        self.state
    }

    pub fn transition(&mut self, next: Lifecycle) -> Result<()> {
        if !self.state.can_transition_to(next) {
            bail!("invalid session transition: {:?} -> {:?}", self.state, next);
        }
        self.state = next;
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionSnapshot {
    pub terminal_id: Uuid,
    pub name: String,
    pub is_primary: bool,
    pub created_at: u64,
    pub session_id: Option<Uuid>,
    pub status: Lifecycle,
    pub connected: bool,
    pub connected_clients: usize,
    pub started_at: Option<u64>,
    pub pid: Option<u32>,
    pub exit_code: Option<u32>,
    pub project: String,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone)]
pub struct OutputChunk {
    pub sequence: u64,
    pub session_id: Uuid,
    pub data: Bytes,
}

#[derive(Debug, Clone)]
pub struct OutputSnapshot {
    pub session_id: Option<Uuid>,
    pub chunks: Vec<OutputChunk>,
    pub last_sequence: u64,
}

#[derive(Debug)]
pub struct BoundedOutputBuffer {
    maximum_bytes: usize,
    current_bytes: usize,
    next_sequence: u64,
    session_id: Option<Uuid>,
    chunks: VecDeque<OutputChunk>,
}

impl BoundedOutputBuffer {
    pub fn new(maximum_bytes: usize) -> Self {
        assert!(maximum_bytes > 0, "output buffer must not be empty");
        Self {
            maximum_bytes,
            current_bytes: 0,
            next_sequence: 1,
            session_id: None,
            chunks: VecDeque::new(),
        }
    }

    pub fn reset(&mut self, session_id: Uuid) {
        self.current_bytes = 0;
        self.session_id = Some(session_id);
        self.chunks.clear();
    }

    pub fn append(&mut self, session_id: Uuid, data: &[u8]) -> Option<OutputChunk> {
        if data.is_empty() || self.session_id != Some(session_id) {
            return None;
        }

        let retained_data = if data.len() > self.maximum_bytes {
            &data[data.len() - self.maximum_bytes..]
        } else {
            data
        };

        let chunk = OutputChunk {
            sequence: self.next_sequence,
            session_id,
            data: Bytes::copy_from_slice(retained_data),
        };
        self.next_sequence = self.next_sequence.saturating_add(1);
        self.current_bytes = self.current_bytes.saturating_add(chunk.data.len());
        self.chunks.push_back(chunk.clone());

        while self.current_bytes > self.maximum_bytes {
            if let Some(discarded) = self.chunks.pop_front() {
                self.current_bytes = self.current_bytes.saturating_sub(discarded.data.len());
            } else {
                break;
            }
        }

        Some(chunk)
    }

    pub fn snapshot(&self) -> OutputSnapshot {
        OutputSnapshot {
            session_id: self.session_id,
            chunks: self.chunks.iter().cloned().collect(),
            last_sequence: self.next_sequence.saturating_sub(1),
        }
    }

    pub fn snapshot_tail(&self, maximum_bytes: usize) -> OutputSnapshot {
        assert!(maximum_bytes > 0, "snapshot tail must not be empty");
        let mut remaining = maximum_bytes;
        let mut chunks = Vec::new();

        for chunk in self.chunks.iter().rev() {
            if remaining == 0 {
                break;
            }
            if chunk.data.len() <= remaining {
                chunks.push(chunk.clone());
                remaining -= chunk.data.len();
            } else {
                let mut tail = chunk.clone();
                tail.data = chunk.data.slice(chunk.data.len() - remaining..);
                chunks.push(tail);
                remaining = 0;
            }
        }
        chunks.reverse();

        OutputSnapshot {
            session_id: self.session_id,
            chunks,
            last_sequence: self.next_sequence.saturating_sub(1),
        }
    }

    pub fn snapshot_since(
        &self,
        session_id: Option<Uuid>,
        last_sequence: u64,
    ) -> Option<OutputSnapshot> {
        if self.session_id != session_id {
            return None;
        }

        let current_last_sequence = self.next_sequence.saturating_sub(1);
        if last_sequence > current_last_sequence {
            return None;
        }
        let first_available_sequence = self
            .chunks
            .front()
            .map_or(self.next_sequence, |chunk| chunk.sequence);
        if last_sequence.saturating_add(1) < first_available_sequence {
            return None;
        }

        Some(OutputSnapshot {
            session_id: self.session_id,
            chunks: self
                .chunks
                .iter()
                .filter(|chunk| chunk.sequence > last_sequence)
                .cloned()
                .collect(),
            last_sequence: current_last_sequence,
        })
    }

    #[cfg(test)]
    fn byte_len(&self) -> usize {
        self.current_bytes
    }
}

#[derive(Clone)]
pub struct SessionManager {
    inner: Arc<SessionInner>,
}

struct SessionInner {
    terminal_id: Uuid,
    name: String,
    is_primary: bool,
    created_at: u64,
    terminal_config: TerminalConfig,
    operation: Mutex<()>,
    record: Mutex<SessionRecord>,
    output: Mutex<BoundedOutputBuffer>,
    output_sender: watch::Sender<u64>,
    event_sender: broadcast::Sender<()>,
    client_slots: Arc<Semaphore>,
    codex_installed: AtomicBool,
    shutting_down: AtomicBool,
    shutdown_signal: CancellationToken,
}

struct SessionRecord {
    machine: SessionStateMachine,
    session_id: Option<Uuid>,
    started_at: Option<u64>,
    pid: Option<u32>,
    exit_code: Option<u32>,
    last_error: Option<String>,
    active: Option<ActiveProcess>,
}

struct ActiveProcess {
    session_id: Uuid,
    master: Box<dyn MasterPty + Send>,
    input_sender: SyncSender<Vec<u8>>,
    termination_sender: SyncSender<TerminationRequest>,
}

struct TerminationRequest {
    acknowledgement: SyncSender<std::result::Result<(), String>>,
}

impl SessionManager {
    pub fn new(terminal_config: TerminalConfig) -> Self {
        Self::new_managed(
            terminal_config,
            Uuid::new_v4(),
            "Terminal 1".to_owned(),
            true,
        )
    }

    pub fn new_managed(
        terminal_config: TerminalConfig,
        terminal_id: Uuid,
        name: String,
        is_primary: bool,
    ) -> Self {
        let (output_sender, _) = watch::channel(0);
        let (event_sender, _) = broadcast::channel(EVENT_BROADCAST_CAPACITY);

        Self {
            inner: Arc::new(SessionInner {
                terminal_id,
                name,
                is_primary,
                created_at: unix_time_millis(),
                terminal_config,
                operation: Mutex::new(()),
                record: Mutex::new(SessionRecord {
                    machine: SessionStateMachine::new(),
                    session_id: None,
                    started_at: None,
                    pid: None,
                    exit_code: None,
                    last_error: None,
                    active: None,
                }),
                output: Mutex::new(BoundedOutputBuffer::new(OUTPUT_BUFFER_LIMIT)),
                output_sender,
                event_sender,
                client_slots: Arc::new(Semaphore::new(MAX_CONNECTED_CLIENTS)),
                codex_installed: AtomicBool::new(false),
                shutting_down: AtomicBool::new(false),
                shutdown_signal: CancellationToken::new(),
            }),
        }
    }

    pub async fn start(&self) -> Result<()> {
        let manager = self.clone();
        tokio::task::spawn_blocking(move || manager.start_blocking())
            .await
            .context("session start task failed")?
    }

    pub async fn restart(&self) -> Result<()> {
        let manager = self.clone();
        tokio::task::spawn_blocking(move || manager.restart_blocking())
            .await
            .context("session restart task failed")?
    }

    pub async fn terminate(&self) -> Result<()> {
        let manager = self.clone();
        tokio::task::spawn_blocking(move || manager.terminate_blocking())
            .await
            .context("session terminate task failed")?
    }

    pub async fn shutdown(&self) {
        if self.inner.shutting_down.swap(true, Ordering::SeqCst) {
            return;
        }

        self.inner.shutdown_signal.cancel();
        if let Err(error) = self.terminate().await {
            tracing::error!(%error, "failed to terminate Codex during shutdown");
        }
    }

    pub fn write_input(&self, data: &[u8]) -> Result<()> {
        let input_sender = {
            let record = lock(&self.inner.record);
            let Some(active) = record.active.as_ref() else {
                bail!("Codex is not running");
            };
            active.input_sender.clone()
        };

        match input_sender.try_send(data.to_vec()) {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(_)) => bail!("PTY input queue is full"),
            Err(TrySendError::Disconnected(_)) => bail!("PTY input stream is closed"),
        }
    }

    pub fn resize(&self, cols: u16, rows: u16) -> Result<()> {
        validate_resize(cols, rows)?;
        let record = lock(&self.inner.record);
        let Some(active) = record.active.as_ref() else {
            bail!("Codex is not running");
        };

        active
            .master
            .resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .context("failed to resize the PTY")
    }

    pub fn snapshot(&self) -> SessionSnapshot {
        let record = lock(&self.inner.record);
        let connected_clients = self.connected_clients();
        SessionSnapshot {
            terminal_id: self.inner.terminal_id,
            name: self.inner.name.clone(),
            is_primary: self.inner.is_primary,
            created_at: self.inner.created_at,
            session_id: record.session_id,
            status: record.machine.state(),
            connected: connected_clients > 0,
            connected_clients,
            started_at: record.started_at,
            pid: record.pid,
            exit_code: record.exit_code,
            project: self
                .inner
                .terminal_config
                .project_dir
                .to_string_lossy()
                .into_owned(),
            last_error: record.last_error.clone(),
        }
    }

    pub fn output_snapshot(&self) -> OutputSnapshot {
        lock(&self.inner.output).snapshot_tail(OUTPUT_REPLAY_LIMIT)
    }

    pub fn output_since(
        &self,
        session_id: Option<Uuid>,
        last_sequence: u64,
    ) -> Option<OutputSnapshot> {
        lock(&self.inner.output).snapshot_since(session_id, last_sequence)
    }

    pub fn subscribe_output(&self) -> watch::Receiver<u64> {
        self.inner.output_sender.subscribe()
    }

    pub fn subscribe_events(&self) -> broadcast::Receiver<()> {
        self.inner.event_sender.subscribe()
    }

    pub fn shutdown_signal(&self) -> CancellationToken {
        self.inner.shutdown_signal.clone()
    }

    pub fn notify_client_count_changed(&self) {
        self.notify_event();
    }

    pub fn try_acquire_client(&self) -> Option<OwnedSemaphorePermit> {
        self.inner.client_slots.clone().try_acquire_owned().ok()
    }

    pub fn connected_clients(&self) -> usize {
        MAX_CONNECTED_CLIENTS.saturating_sub(self.inner.client_slots.available_permits())
    }

    pub fn codex_installed(&self) -> bool {
        self.inner.codex_installed.load(Ordering::Relaxed)
    }

    pub fn is_running(&self) -> bool {
        lock(&self.inner.record).machine.state() == Lifecycle::Running
    }

    fn start_blocking(&self) -> Result<()> {
        let _operation = lock(&self.inner.operation);
        self.start_locked()
    }

    fn restart_blocking(&self) -> Result<()> {
        let _operation = lock(&self.inner.operation);
        tracing::info!("restarting Codex session");
        self.terminate_locked()?;
        self.start_locked()
    }

    fn terminate_blocking(&self) -> Result<()> {
        let _operation = lock(&self.inner.operation);
        self.terminate_locked()
    }

    fn start_locked(&self) -> Result<()> {
        if self.inner.shutting_down.load(Ordering::SeqCst) {
            bail!("server is shutting down");
        }

        {
            let record = lock(&self.inner.record);
            if record.active.is_some() {
                bail!("Codex is already running");
            }
        }

        let session_id = Uuid::new_v4();
        {
            let mut record = lock(&self.inner.record);
            record.machine.transition(Lifecycle::Starting)?;
            record.session_id = Some(session_id);
            record.started_at = Some(unix_time_millis());
            record.pid = None;
            record.exit_code = None;
            record.last_error = None;
        }
        lock(&self.inner.output).reset(session_id);
        self.notify_event();

        let resolved = match terminal::preflight(&self.inner.terminal_config) {
            Ok(resolved) => {
                self.inner.codex_installed.store(true, Ordering::Relaxed);
                resolved
            }
            Err(error) => {
                self.inner.codex_installed.store(false, Ordering::Relaxed);
                self.mark_start_failed(session_id, &error);
                return Err(error);
            }
        };

        tracing::info!(
            command = %resolved.path().display(),
            project = %self.inner.terminal_config.project_dir.display(),
            "starting Codex in PTY"
        );

        let spawned = match terminal::spawn_resolved(&self.inner.terminal_config, &resolved) {
            Ok(spawned) => spawned,
            Err(error) => {
                self.mark_start_failed(session_id, &error);
                return Err(error);
            }
        };

        let terminal::SpawnedTerminal {
            master,
            reader,
            writer,
            child,
            pid,
        } = spawned;
        let (input_sender, input_receiver) = sync_channel(INPUT_QUEUE_CAPACITY);
        let (termination_sender, termination_receiver) = sync_channel(1);

        {
            let mut record = lock(&self.inner.record);
            if record.session_id != Some(session_id) {
                bail!("session changed while Codex was starting");
            }
            record.active = Some(ActiveProcess {
                session_id,
                master,
                input_sender,
                termination_sender,
            });
            record.pid = pid;
            record.machine.transition(Lifecycle::Running)?;
        }
        self.notify_event();

        if let Some(pid) = pid {
            tracing::info!(pid, %session_id, "Codex PTY process started");
        } else {
            tracing::info!(%session_id, "Codex PTY process started");
        }

        self.spawn_writer_thread(session_id, writer, input_receiver);
        self.spawn_reader_thread(session_id, reader);
        self.spawn_wait_thread(session_id, pid, child, termination_receiver);
        Ok(())
    }

    fn terminate_locked(&self) -> Result<()> {
        let active = {
            let mut record = lock(&self.inner.record);
            let active = record.active.take();
            if active.is_some() {
                record.machine.transition(Lifecycle::Terminating)?;
            } else if record.machine.state() != Lifecycle::Terminated {
                record.machine.transition(Lifecycle::Terminated)?;
            }
            active
        };
        self.notify_event();

        let Some(active) = active else {
            return Ok(());
        };

        let session_id = active.session_id;
        tracing::info!(%session_id, "terminating Codex PTY process");
        let (acknowledgement, acknowledgement_receiver) = sync_channel(1);
        let request_result = active
            .termination_sender
            .send(TerminationRequest { acknowledgement });
        let kill_result = match request_result {
            Ok(()) => match acknowledgement_receiver.recv_timeout(Duration::from_secs(5)) {
                Ok(Ok(())) => Ok(()),
                Ok(Err(message)) => Err(anyhow::anyhow!(message))
                    .context("failed to terminate the Codex PTY process"),
                Err(RecvTimeoutError::Timeout) => Err(anyhow::anyhow!(
                    "timed out while terminating the Codex PTY process"
                )),
                Err(RecvTimeoutError::Disconnected) => Ok(()),
            },
            // The waiter drops this channel only after the process has exited.
            Err(_) => Ok(()),
        };
        drop(active);

        {
            let mut record = lock(&self.inner.record);
            if record.session_id == Some(session_id)
                && record.machine.state() == Lifecycle::Terminating
            {
                record.machine.transition(Lifecycle::Terminated)?;
            }
        }
        self.notify_event();
        kill_result
    }

    fn mark_start_failed(&self, session_id: Uuid, error: &anyhow::Error) {
        let mut record = lock(&self.inner.record);
        if record.session_id == Some(session_id) {
            let _ = record.machine.transition(Lifecycle::Failed);
            record.last_error = Some(public_error(error));
        }
        drop(record);
        self.notify_event();
        tracing::error!(%error, %session_id, "failed to start Codex PTY session");
    }

    fn spawn_writer_thread(
        &self,
        session_id: Uuid,
        writer: Box<dyn Write + Send>,
        receiver: Receiver<Vec<u8>>,
    ) {
        let manager = self.clone();
        std::thread::Builder::new()
            .name("codex-pty-writer".to_owned())
            .spawn(move || writer_loop(manager, session_id, writer, receiver))
            .expect("failed to create PTY writer thread");
    }

    fn spawn_reader_thread(&self, session_id: Uuid, reader: Box<dyn Read + Send>) {
        let manager = self.clone();
        std::thread::Builder::new()
            .name("codex-pty-reader".to_owned())
            .spawn(move || reader_loop(manager, session_id, reader))
            .expect("failed to create PTY reader thread");
    }

    fn spawn_wait_thread(
        &self,
        session_id: Uuid,
        pid: Option<u32>,
        child: Box<dyn portable_pty::Child + Send + Sync>,
        termination_receiver: Receiver<TerminationRequest>,
    ) {
        let manager = self.clone();
        std::thread::Builder::new()
            .name("codex-pty-waiter".to_owned())
            .spawn(move || child_wait_loop(manager, session_id, pid, child, termination_receiver))
            .expect("failed to create PTY waiter thread");
    }

    fn publish_output(&self, session_id: Uuid, data: &[u8]) {
        let is_current = {
            let record = lock(&self.inner.record);
            record.session_id == Some(session_id)
                && matches!(
                    record.machine.state(),
                    Lifecycle::Starting | Lifecycle::Running | Lifecycle::Terminating
                )
        };
        if !is_current {
            return;
        }

        if let Some(chunk) = lock(&self.inner.output).append(session_id, data) {
            self.inner.output_sender.send_replace(chunk.sequence);
        }
    }

    fn process_exited(
        &self,
        session_id: Uuid,
        exit_code: Option<u32>,
        wait_error: Option<std::io::Error>,
    ) {
        let mut record = lock(&self.inner.record);
        if record.session_id != Some(session_id) {
            return;
        }

        record.active.take();
        record.exit_code = exit_code;
        let target = if matches!(
            record.machine.state(),
            Lifecycle::Terminating | Lifecycle::Terminated
        ) {
            Lifecycle::Terminated
        } else {
            Lifecycle::Exited
        };
        let _ = record.machine.transition(target);
        if let Some(error) = wait_error.as_ref() {
            record.last_error = Some("The Codex process wait operation failed.".to_owned());
            tracing::error!(%error, %session_id, "failed while waiting for Codex process");
        }
        drop(record);

        self.notify_event();
        if let Some(exit_code) = exit_code {
            tracing::info!(%session_id, exit_code, "Codex process exited");
        } else {
            tracing::info!(%session_id, "Codex process exited without an exit code");
        }
    }

    fn record_stream_error(&self, session_id: Uuid, message: &'static str) {
        let mut record = lock(&self.inner.record);
        if record.session_id == Some(session_id) {
            record.last_error = Some(message.to_owned());
        }
        drop(record);
        self.notify_event();
    }

    fn notify_event(&self) {
        let _ = self.inner.event_sender.send(());
    }
}

fn writer_loop(
    manager: SessionManager,
    session_id: Uuid,
    mut writer: Box<dyn Write + Send>,
    receiver: Receiver<Vec<u8>>,
) {
    while let Ok(data) = receiver.recv() {
        if let Err(error) = writer.write_all(&data).and_then(|()| writer.flush()) {
            tracing::error!(
                error_kind = ?error.kind(),
                %session_id,
                "PTY input write failed"
            );
            manager.record_stream_error(session_id, "The PTY input stream failed.");
            break;
        }
    }
}

fn reader_loop(manager: SessionManager, session_id: Uuid, mut reader: Box<dyn Read + Send>) {
    let mut buffer = [0_u8; 16 * 1024];
    loop {
        match reader.read(&mut buffer) {
            Ok(0) => break,
            Ok(length) => manager.publish_output(session_id, &buffer[..length]),
            Err(error) if error.kind() == ErrorKind::Interrupted => continue,
            Err(error) if error.kind() == ErrorKind::BrokenPipe => break,
            Err(error) => {
                tracing::error!(
                    error_kind = ?error.kind(),
                    %session_id,
                    "PTY output read failed"
                );
                manager.record_stream_error(session_id, "The PTY output stream failed.");
                break;
            }
        }
    }
}

fn child_wait_loop(
    manager: SessionManager,
    session_id: Uuid,
    pid: Option<u32>,
    mut child: Box<dyn portable_pty::Child + Send + Sync>,
    termination_receiver: Receiver<TerminationRequest>,
) {
    #[cfg(not(windows))]
    let _ = pid;

    loop {
        match termination_receiver.recv_timeout(Duration::from_millis(50)) {
            Ok(request) => {
                #[cfg(windows)]
                if let Some(pid) = pid
                    && let Err(error) = terminate_windows_process_tree(pid)
                {
                    // The direct portable-pty kill and ConPTY handle closure
                    // below remain as fallbacks.
                    tracing::warn!(pid, %error, "Windows process-tree termination failed");
                }

                let kill_result = child.kill().map_err(|error| error.to_string());
                let _ = request.acknowledgement.send(kill_result);
                match child.wait() {
                    Ok(status) => {
                        manager.process_exited(session_id, Some(status.exit_code()), None)
                    }
                    Err(error) => manager.process_exited(session_id, None, Some(error)),
                }
                return;
            }
            Err(RecvTimeoutError::Timeout) => match child.try_wait() {
                Ok(Some(status)) => {
                    manager.process_exited(session_id, Some(status.exit_code()), None);
                    return;
                }
                Ok(None) => {}
                Err(error) => {
                    manager.process_exited(session_id, None, Some(error));
                    return;
                }
            },
            Err(RecvTimeoutError::Disconnected) => {
                let _ = child.kill();
                match child.wait() {
                    Ok(status) => {
                        manager.process_exited(session_id, Some(status.exit_code()), None)
                    }
                    Err(error) => manager.process_exited(session_id, None, Some(error)),
                }
                return;
            }
        }
    }
}

#[cfg(windows)]
fn terminate_windows_process_tree(pid: u32) -> Result<()> {
    let status = std::process::Command::new("taskkill.exe")
        .args(["/PID", &pid.to_string(), "/T", "/F"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .context("failed to start taskkill.exe")?;

    if !status.success() {
        bail!("taskkill.exe exited with status {status}");
    }
    Ok(())
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn public_error(error: &anyhow::Error) -> String {
    let text = error.to_string();
    text.chars().take(MAX_PUBLIC_ERROR_LENGTH).collect()
}

fn unix_time_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_valid_session_state_transitions() {
        let mut machine = SessionStateMachine::new();
        machine.transition(Lifecycle::Starting).expect("start");
        machine.transition(Lifecycle::Running).expect("running");
        machine
            .transition(Lifecycle::Terminating)
            .expect("terminating");
        machine
            .transition(Lifecycle::Terminated)
            .expect("terminated");
        machine.transition(Lifecycle::Starting).expect("restart");
        machine.transition(Lifecycle::Failed).expect("failure");
        assert_eq!(machine.state(), Lifecycle::Failed);
    }

    #[test]
    fn rejects_invalid_session_state_transitions() {
        let mut machine = SessionStateMachine::new();
        assert!(machine.transition(Lifecycle::Running).is_err());
        assert_eq!(machine.state(), Lifecycle::Idle);
    }

    #[test]
    fn output_buffer_never_exceeds_its_limit() {
        let session_id = Uuid::new_v4();
        let mut buffer = BoundedOutputBuffer::new(8);
        buffer.reset(session_id);
        buffer.append(session_id, b"12345");
        buffer.append(session_id, b"67890");

        let snapshot = buffer.snapshot();
        assert!(buffer.byte_len() <= 8);
        assert_eq!(snapshot.chunks.len(), 1);
        assert_eq!(&snapshot.chunks[0].data[..], b"67890");
    }

    #[test]
    fn oversized_output_chunk_keeps_only_its_tail() {
        let session_id = Uuid::new_v4();
        let mut buffer = BoundedOutputBuffer::new(4);
        buffer.reset(session_id);
        buffer.append(session_id, b"abcdefgh");

        let snapshot = buffer.snapshot();
        assert_eq!(&snapshot.chunks[0].data[..], b"efgh");
        assert_eq!(buffer.byte_len(), 4);
    }

    #[test]
    fn replay_snapshot_keeps_only_the_requested_byte_tail() {
        let session_id = Uuid::new_v4();
        let mut buffer = BoundedOutputBuffer::new(16);
        buffer.reset(session_id);
        buffer.append(session_id, b"aaaa");
        buffer.append(session_id, b"bbbb");
        let latest = buffer.append(session_id, b"cccc").expect("latest chunk");

        let snapshot = buffer.snapshot_tail(5);
        let replay: Vec<u8> = snapshot
            .chunks
            .iter()
            .flat_map(|chunk| chunk.data.iter().copied())
            .collect();

        assert_eq!(replay, b"bcccc");
        assert_eq!(snapshot.last_sequence, latest.sequence);
        assert_eq!(buffer.byte_len(), 12);
    }

    #[test]
    fn buffer_rejects_output_from_an_old_session() {
        let current_session = Uuid::new_v4();
        let old_session = Uuid::new_v4();
        let mut buffer = BoundedOutputBuffer::new(16);
        buffer.reset(current_session);

        assert!(buffer.append(old_session, b"old output").is_none());
        assert!(buffer.snapshot().chunks.is_empty());
    }

    #[test]
    fn output_delta_returns_only_chunks_after_the_client_sequence() {
        let session_id = Uuid::new_v4();
        let mut buffer = BoundedOutputBuffer::new(32);
        buffer.reset(session_id);
        let first = buffer.append(session_id, b"one").expect("first chunk");
        buffer.append(session_id, b"two").expect("second chunk");
        let third = buffer.append(session_id, b"three").expect("third chunk");

        let delta = buffer
            .snapshot_since(Some(session_id), first.sequence)
            .expect("available delta");

        assert_eq!(delta.chunks.len(), 2);
        assert_eq!(&delta.chunks[0].data[..], b"two");
        assert_eq!(&delta.chunks[1].data[..], b"three");
        assert_eq!(delta.last_sequence, third.sequence);
    }

    #[test]
    fn output_delta_requires_replay_after_retained_history_was_lost() {
        let session_id = Uuid::new_v4();
        let mut buffer = BoundedOutputBuffer::new(5);
        buffer.reset(session_id);
        let discarded = buffer.append(session_id, b"old").expect("old chunk");
        buffer.append(session_id, b"newer").expect("new chunk");

        assert!(
            buffer
                .snapshot_since(Some(session_id), discarded.sequence.saturating_sub(1))
                .is_none()
        );
    }

    #[test]
    fn output_delta_accepts_the_exact_retained_boundary_and_current_sequence() {
        let session_id = Uuid::new_v4();
        let mut buffer = BoundedOutputBuffer::new(5);
        buffer.reset(session_id);
        let discarded = buffer.append(session_id, b"old").expect("old chunk");
        let retained = buffer.append(session_id, b"newer").expect("retained chunk");

        let boundary = buffer
            .snapshot_since(Some(session_id), discarded.sequence)
            .expect("exact retained boundary");
        assert_eq!(boundary.chunks.len(), 1);
        assert_eq!(boundary.chunks[0].sequence, retained.sequence);

        let current = buffer
            .snapshot_since(Some(session_id), retained.sequence)
            .expect("current sequence");
        assert!(current.chunks.is_empty());
        assert_eq!(current.last_sequence, retained.sequence);
    }

    #[test]
    fn output_delta_requires_replay_when_the_session_changed() {
        let current_session = Uuid::new_v4();
        let previous_session = Uuid::new_v4();
        let mut buffer = BoundedOutputBuffer::new(16);
        buffer.reset(current_session);
        buffer.append(current_session, b"current output");

        assert!(buffer.snapshot_since(Some(previous_session), 0).is_none());
    }

    #[test]
    fn output_delta_requires_replay_for_an_impossible_future_sequence() {
        let session_id = Uuid::new_v4();
        let mut buffer = BoundedOutputBuffer::new(16);
        buffer.reset(session_id);
        let current = buffer
            .append(session_id, b"current output")
            .expect("current chunk");

        assert!(
            buffer
                .snapshot_since(Some(session_id), current.sequence + 1)
                .is_none()
        );
    }

    #[test]
    fn reset_session_without_output_has_an_empty_current_delta() {
        let first_session = Uuid::new_v4();
        let second_session = Uuid::new_v4();
        let mut buffer = BoundedOutputBuffer::new(16);
        buffer.reset(first_session);
        let previous = buffer
            .append(first_session, b"previous")
            .expect("previous chunk");
        buffer.reset(second_session);

        let delta = buffer
            .snapshot_since(Some(second_session), previous.sequence)
            .expect("empty current delta");
        assert!(delta.chunks.is_empty());
        assert_eq!(delta.last_sequence, previous.sequence);
    }
}
