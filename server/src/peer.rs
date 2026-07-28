use std::{
    collections::HashMap,
    ffi::OsString,
    fmt,
    net::SocketAddr,
    path::PathBuf,
    sync::{Arc, Mutex, MutexGuard},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};
use subtle::ConstantTimeEq;
use uuid::Uuid;

use crate::config::{AgentKind, MAX_CONFIGURED_SESSIONS};

pub const CWT_PEER_ENDPOINT_ENV: &str = "CWT_PEER_ENDPOINT";
pub const CWT_PEER_HELPER_ENV: &str = "CWT_PEER_HELPER";
pub const CWT_TERMINAL_ID_ENV: &str = "CWT_TERMINAL_ID";
pub const CWT_SESSION_ID_ENV: &str = "CWT_SESSION_ID";
pub const CWT_PEER_CAPABILITY_ENV: &str = "CWT_PEER_CAPABILITY";

pub const MAX_PEER_ARTIFACT_BYTES: usize = 64 * 1024;
pub const MAX_PEER_INSTRUCTION_BYTES: usize = 4 * 1024;
pub const MAX_PEER_ERROR_BYTES: usize = 512;
pub const MAX_PEER_THREADS: usize = MAX_CONFIGURED_SESSIONS;
pub const MAX_PEER_TURNS_PER_THREAD: usize = 32;

const CAPABILITY_BYTES: usize = 32;
const SOURCE_SESSION_ENDED_ERROR: &str =
    "The source terminal session ended before the peer turn completed.";
const REVIEWER_SESSION_ENDED_ERROR: &str =
    "The dedicated reviewer session ended before the peer turn completed.";

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum SessionPurpose {
    #[default]
    Interactive,
    Peer {
        #[serde(rename = "threadId")]
        thread_id: Uuid,
        #[serde(rename = "parentTerminalId")]
        parent_terminal_id: Uuid,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PeerAction {
    Review,
    Verify,
    Ask,
    Handoff,
    Recheck,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PeerStatus {
    PreparingHandoff,
    AwaitingPreview,
    Reviewing,
    ResponseReady,
    Returned,
    Failed,
    Closed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PeerArtifactKind {
    Handoff,
    Response,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PeerArtifact {
    pub turn_id: Uuid,
    pub kind: PeerArtifactKind,
    pub content: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PeerTurn {
    pub id: Uuid,
    pub sequence: u32,
    pub action: PeerAction,
    pub instruction: String,
    pub status: PeerStatus,
    pub handoff: Option<String>,
    pub handoff_revision: u32,
    pub response: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PeerThread {
    pub id: Uuid,
    pub source_terminal_id: Uuid,
    pub reviewer_terminal_id: Option<Uuid>,
    pub target_agent: AgentKind,
    pub status: PeerStatus,
    pub current_turn: PeerTurn,
    pub created_at: u64,
    pub updated_at: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PeerErrorKind {
    NotFound,
    Unauthorized,
    InvalidInput,
    PayloadTooLarge,
    InvalidState,
    Conflict,
    LimitReached,
    ReviewerNotBound,
    SessionInactive,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerError {
    kind: PeerErrorKind,
    message: &'static str,
}

impl PeerError {
    fn new(kind: PeerErrorKind, message: &'static str) -> Self {
        Self { kind, message }
    }

    pub fn kind(&self) -> PeerErrorKind {
        self.kind
    }
}

impl fmt::Display for PeerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message)
    }
}

impl std::error::Error for PeerError {}

#[derive(Clone)]
pub struct PeerBroker {
    inner: Arc<BrokerInner>,
}

struct BrokerInner {
    endpoint: SocketAddr,
    helper_path: PathBuf,
    state: Mutex<BrokerState>,
}

#[derive(Default)]
struct BrokerState {
    capabilities: HashMap<(Uuid, Uuid), CapabilityRecord>,
    threads: HashMap<Uuid, ThreadRecord>,
    capability_activation_disabled: bool,
}

struct CapabilityRecord {
    secret: String,
    purpose: SessionPurpose,
}

struct ThreadRecord {
    id: Uuid,
    source_terminal_id: Uuid,
    reviewer_terminal_id: Option<Uuid>,
    target_agent: AgentKind,
    status: PeerStatus,
    turns: Vec<TurnRecord>,
    provisioning_token: Option<Uuid>,
    return_delivery_token: Option<Uuid>,
    close_token: Option<Uuid>,
    created_at: u64,
    updated_at: u64,
}

struct TurnRecord {
    id: Uuid,
    sequence: u32,
    action: PeerAction,
    instruction: String,
    status: PeerStatus,
    handoff: Option<String>,
    handoff_revision: u32,
    response: Option<String>,
    error: Option<String>,
    source_session_id: Uuid,
    reviewer_session_id: Option<Uuid>,
}

pub struct SessionActivation {
    environment: Vec<(OsString, OsString)>,
}

pub struct PeerReturnDelivery {
    thread: PeerThread,
    token: Uuid,
    inner: Arc<BrokerInner>,
}

impl PeerReturnDelivery {
    pub fn thread(&self) -> &PeerThread {
        &self.thread
    }
}

pub struct PeerProvisioning {
    thread: PeerThread,
    token: Uuid,
    inner: Arc<BrokerInner>,
}

impl PeerProvisioning {
    pub fn thread(&self) -> &PeerThread {
        &self.thread
    }
}

pub struct PeerClose {
    thread: PeerThread,
    token: Uuid,
    inner: Arc<BrokerInner>,
}

impl Drop for PeerProvisioning {
    fn drop(&mut self) {
        let mut state = lock(&self.inner.state);
        let Some(thread) = state.threads.get_mut(&self.thread.id) else {
            return;
        };
        if thread.provisioning_token == Some(self.token) {
            thread.provisioning_token = None;
            thread.updated_at = unix_time_millis();
        }
    }
}

impl PeerClose {
    pub fn thread(&self) -> &PeerThread {
        &self.thread
    }
}

impl Drop for PeerReturnDelivery {
    fn drop(&mut self) {
        let mut state = lock(&self.inner.state);
        let Some(thread) = state.threads.get_mut(&self.thread.id) else {
            return;
        };
        if thread.return_delivery_token == Some(self.token) {
            thread.return_delivery_token = None;
            thread.updated_at = unix_time_millis();
        }
    }
}

impl Drop for PeerClose {
    fn drop(&mut self) {
        let mut state = lock(&self.inner.state);
        let Some(thread) = state.threads.get_mut(&self.thread.id) else {
            return;
        };
        if thread.close_token == Some(self.token) {
            thread.close_token = None;
            thread.updated_at = unix_time_millis();
        }
    }
}

impl SessionActivation {
    pub fn environment(&self) -> &[(OsString, OsString)] {
        &self.environment
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapabilitySubject {
    terminal_id: Uuid,
    session_id: Uuid,
    purpose: SessionPurpose,
}

impl CapabilitySubject {
    pub fn terminal_id(&self) -> Uuid {
        self.terminal_id
    }

    pub fn session_id(&self) -> Uuid {
        self.session_id
    }

    pub fn purpose(&self) -> &SessionPurpose {
        &self.purpose
    }
}

impl PeerBroker {
    pub fn new(endpoint: SocketAddr, helper_path: PathBuf) -> Self {
        Self {
            inner: Arc::new(BrokerInner {
                endpoint,
                helper_path,
                state: Mutex::new(BrokerState::default()),
            }),
        }
    }

    pub fn activate_session(
        &self,
        terminal_id: Uuid,
        session_id: Uuid,
        purpose: &SessionPurpose,
    ) -> Result<SessionActivation> {
        if !self.inner.endpoint.ip().is_loopback() || self.inner.endpoint.port() == 0 {
            bail!("the peer helper endpoint must be a loopback socket with a non-zero port");
        }
        if self.inner.helper_path.as_os_str().is_empty() {
            bail!("the peer helper executable path must not be empty");
        }

        let capability = generate_capability()?;
        let mut state = lock(&self.inner.state);
        if state.capability_activation_disabled {
            bail!("peer communication is shutting down");
        }
        if let SessionPurpose::Peer {
            thread_id,
            parent_terminal_id,
        } = purpose
        {
            let thread = state
                .threads
                .get(thread_id)
                .context("the peer thread does not exist")?;
            if thread.source_terminal_id != *parent_terminal_id
                || thread.status == PeerStatus::Closed
                || thread.close_token.is_some()
                || thread.return_delivery_token.is_some()
                || thread
                    .reviewer_terminal_id
                    .is_some_and(|reviewer_terminal_id| reviewer_terminal_id != terminal_id)
            {
                bail!("the peer session purpose does not match an active thread");
            }
        }
        state
            .capabilities
            .retain(|(active_terminal_id, _), _| *active_terminal_id != terminal_id);
        state.capabilities.insert(
            (terminal_id, session_id),
            CapabilityRecord {
                secret: capability.clone(),
                purpose: purpose.clone(),
            },
        );

        Ok(SessionActivation {
            environment: vec![
                (
                    OsString::from(CWT_PEER_ENDPOINT_ENV),
                    OsString::from(self.inner.endpoint.to_string()),
                ),
                (
                    OsString::from(CWT_PEER_HELPER_ENV),
                    self.inner.helper_path.as_os_str().to_os_string(),
                ),
                (
                    OsString::from(CWT_TERMINAL_ID_ENV),
                    OsString::from(terminal_id.to_string()),
                ),
                (
                    OsString::from(CWT_SESSION_ID_ENV),
                    OsString::from(session_id.to_string()),
                ),
                (
                    OsString::from(CWT_PEER_CAPABILITY_ENV),
                    OsString::from(capability),
                ),
            ],
        })
    }

    pub fn revoke_session(&self, terminal_id: Uuid, session_id: Uuid) {
        let mut state = lock(&self.inner.state);
        let Some(capability) = state.capabilities.remove(&(terminal_id, session_id)) else {
            return;
        };

        match capability.purpose {
            SessionPurpose::Interactive => {
                for thread in state.threads.values_mut() {
                    if thread.source_terminal_id != terminal_id {
                        continue;
                    }
                    let should_fail = thread.turns.last().is_some_and(|turn| {
                        turn.source_session_id == session_id
                            && matches!(
                                turn.status,
                                PeerStatus::PreparingHandoff
                                    | PeerStatus::AwaitingPreview
                                    | PeerStatus::Reviewing
                                    | PeerStatus::ResponseReady
                            )
                    });
                    if should_fail {
                        fail_current_turn_for_ended_session(thread, SOURCE_SESSION_ENDED_ERROR);
                    }
                }
            }
            SessionPurpose::Peer {
                thread_id,
                parent_terminal_id,
            } => {
                let Some(thread) = state.threads.get_mut(&thread_id) else {
                    return;
                };
                if thread.source_terminal_id != parent_terminal_id
                    || thread.reviewer_terminal_id != Some(terminal_id)
                {
                    return;
                }
                let should_fail = thread.turns.last().is_some_and(|turn| match turn.status {
                    PeerStatus::PreparingHandoff | PeerStatus::AwaitingPreview => {
                        turn.reviewer_session_id.is_none()
                    }
                    PeerStatus::Reviewing => turn.reviewer_session_id == Some(session_id),
                    PeerStatus::ResponseReady
                    | PeerStatus::Returned
                    | PeerStatus::Failed
                    | PeerStatus::Closed => false,
                });
                if should_fail {
                    fail_current_turn_for_ended_session(thread, REVIEWER_SESSION_ENDED_ERROR);
                }
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn has_active_session(&self, terminal_id: Uuid, session_id: Uuid) -> bool {
        lock(&self.inner.state)
            .capabilities
            .contains_key(&(terminal_id, session_id))
    }

    pub fn begin_shutdown(&self) {
        let mut state = lock(&self.inner.state);
        state.capability_activation_disabled = true;
        state.capabilities.clear();
    }

    pub fn authenticate_capability(
        &self,
        candidate: &str,
    ) -> std::result::Result<CapabilitySubject, PeerError> {
        if candidate.is_empty() || candidate.len() > 512 {
            return Err(unauthorized());
        }
        let state = lock(&self.inner.state);
        state
            .capabilities
            .iter()
            .find_map(|((terminal_id, session_id), record)| {
                constant_time_secret_match(&record.secret, candidate).then(|| CapabilitySubject {
                    terminal_id: *terminal_id,
                    session_id: *session_id,
                    purpose: record.purpose.clone(),
                })
            })
            .ok_or_else(unauthorized)
    }

    pub fn list_threads(&self) -> Vec<PeerThread> {
        let state = lock(&self.inner.state);
        let mut threads: Vec<_> = state.threads.values().map(thread_view).collect();
        threads.sort_by(|left, right| {
            right
                .updated_at
                .cmp(&left.updated_at)
                .then_with(|| left.id.as_bytes().cmp(right.id.as_bytes()))
        });
        threads
    }

    pub fn get_thread(&self, thread_id: Uuid) -> std::result::Result<PeerThread, PeerError> {
        lock(&self.inner.state)
            .threads
            .get(&thread_id)
            .map(thread_view)
            .ok_or_else(thread_not_found)
    }

    pub fn create_thread(
        &self,
        source_terminal_id: Uuid,
        target_agent: AgentKind,
        action: PeerAction,
        instruction: String,
    ) -> std::result::Result<PeerThread, PeerError> {
        if action == PeerAction::Recheck {
            return Err(PeerError::new(
                PeerErrorKind::InvalidInput,
                "recheck requires an existing peer thread",
            ));
        }
        let instruction = validate_instruction(instruction)?;
        let mut state = lock(&self.inner.state);
        if state
            .threads
            .values()
            .filter(|thread| thread.status != PeerStatus::Closed)
            .count()
            >= MAX_PEER_THREADS
        {
            return Err(PeerError::new(
                PeerErrorKind::LimitReached,
                "the active peer thread limit has been reached",
            ));
        }
        let source_session_id =
            active_interactive_session(&state, source_terminal_id).ok_or_else(|| {
                PeerError::new(
                    PeerErrorKind::SessionInactive,
                    "the source terminal has no active interactive session",
                )
            })?;
        let now = unix_time_millis();
        let thread_id = Uuid::new_v4();
        let thread = ThreadRecord {
            id: thread_id,
            source_terminal_id,
            reviewer_terminal_id: None,
            target_agent,
            status: PeerStatus::PreparingHandoff,
            turns: vec![TurnRecord {
                id: Uuid::new_v4(),
                sequence: 1,
                action,
                instruction,
                status: PeerStatus::PreparingHandoff,
                handoff: None,
                handoff_revision: 0,
                response: None,
                error: None,
                source_session_id,
                reviewer_session_id: None,
            }],
            provisioning_token: None,
            return_delivery_token: None,
            close_token: None,
            created_at: now,
            updated_at: now,
        };
        let view = thread_view(&thread);
        state.threads.insert(thread_id, thread);
        Ok(view)
    }

    pub fn create_turn(
        &self,
        thread_id: Uuid,
        action: PeerAction,
        instruction: String,
    ) -> std::result::Result<PeerThread, PeerError> {
        let instruction = validate_instruction(instruction)?;
        let mut state = lock(&self.inner.state);
        let (source_terminal_id, reviewer_terminal_id, previous_status, next_sequence, turn_count) = {
            let thread = state.threads.get(&thread_id).ok_or_else(thread_not_found)?;
            ensure_thread_accepts_operations(thread)?;
            (
                thread.source_terminal_id,
                thread.reviewer_terminal_id,
                thread.status,
                thread
                    .turns
                    .last()
                    .map_or(1, |turn| turn.sequence.saturating_add(1)),
                thread.turns.len(),
            )
        };
        if previous_status != PeerStatus::Returned && previous_status != PeerStatus::Failed {
            return Err(invalid_state(
                "a new peer turn requires the previous turn to be returned or failed",
            ));
        }
        let reviewer_terminal_id = reviewer_terminal_id.ok_or_else(reviewer_not_bound)?;
        let source_session_id =
            active_interactive_session(&state, source_terminal_id).ok_or_else(|| {
                PeerError::new(
                    PeerErrorKind::SessionInactive,
                    "the source terminal has no active interactive session",
                )
            })?;
        if active_peer_session(&state, reviewer_terminal_id, thread_id, source_terminal_id)
            .is_none()
        {
            return Err(PeerError::new(
                PeerErrorKind::SessionInactive,
                "the linked peer terminal is no longer active",
            ));
        }
        if turn_count >= MAX_PEER_TURNS_PER_THREAD {
            return Err(PeerError::new(
                PeerErrorKind::LimitReached,
                "the peer turn limit has been reached",
            ));
        }

        let now = unix_time_millis();
        let thread = state
            .threads
            .get_mut(&thread_id)
            .ok_or_else(thread_not_found)?;
        thread.turns.push(TurnRecord {
            id: Uuid::new_v4(),
            sequence: next_sequence,
            action,
            instruction,
            status: PeerStatus::PreparingHandoff,
            handoff: None,
            handoff_revision: 0,
            response: None,
            error: None,
            source_session_id,
            reviewer_session_id: None,
        });
        thread.status = PeerStatus::PreparingHandoff;
        thread.updated_at = now;
        Ok(thread_view(thread))
    }

    pub fn bind_reviewer(
        &self,
        thread_id: Uuid,
        reviewer_terminal_id: Uuid,
    ) -> std::result::Result<PeerThread, PeerError> {
        let mut state = lock(&self.inner.state);
        let source_terminal_id = state
            .threads
            .get(&thread_id)
            .ok_or_else(thread_not_found)
            .and_then(|thread| {
                ensure_thread_accepts_operations(thread)?;
                Ok(thread.source_terminal_id)
            })?;
        if source_terminal_id == reviewer_terminal_id {
            return Err(PeerError::new(
                PeerErrorKind::InvalidInput,
                "the source terminal cannot be its own reviewer",
            ));
        }
        if active_peer_session(&state, reviewer_terminal_id, thread_id, source_terminal_id)
            .is_none()
        {
            return Err(PeerError::new(
                PeerErrorKind::SessionInactive,
                "the reviewer terminal has no matching active peer session",
            ));
        }

        let thread = state
            .threads
            .get_mut(&thread_id)
            .ok_or_else(thread_not_found)?;
        if thread.status == PeerStatus::Closed {
            return Err(invalid_state("the peer thread is closed"));
        }
        match thread.reviewer_terminal_id {
            Some(existing) if existing != reviewer_terminal_id => {
                return Err(PeerError::new(
                    PeerErrorKind::Conflict,
                    "the peer thread is already bound to another reviewer terminal",
                ));
            }
            _ => thread.reviewer_terminal_id = Some(reviewer_terminal_id),
        }
        thread.updated_at = unix_time_millis();
        Ok(thread_view(thread))
    }

    pub fn begin_reviewer_provisioning(
        &self,
        thread_id: Uuid,
    ) -> std::result::Result<PeerProvisioning, PeerError> {
        let mut state = lock(&self.inner.state);
        let thread = state
            .threads
            .get_mut(&thread_id)
            .ok_or_else(thread_not_found)?;
        ensure_thread_accepts_operations(thread)?;
        if thread.reviewer_terminal_id.is_some() {
            return Err(PeerError::new(
                PeerErrorKind::Conflict,
                "the peer thread already owns a reviewer terminal",
            ));
        }

        let reviewer_terminal_id = Uuid::new_v4();
        if reviewer_terminal_id == thread.source_terminal_id {
            return Err(PeerError::new(
                PeerErrorKind::Conflict,
                "the reviewer terminal identity could not be allocated",
            ));
        }
        let token = Uuid::new_v4();
        thread.reviewer_terminal_id = Some(reviewer_terminal_id);
        thread.provisioning_token = Some(token);
        thread.updated_at = unix_time_millis();
        Ok(PeerProvisioning {
            thread: thread_view(thread),
            token,
            inner: self.inner.clone(),
        })
    }

    pub fn complete_reviewer_provisioning(
        &self,
        provisioning: PeerProvisioning,
    ) -> std::result::Result<PeerThread, PeerError> {
        let mut state = lock(&self.inner.state);
        let thread = state
            .threads
            .get(&provisioning.thread.id)
            .ok_or_else(thread_not_found)?;
        validate_reviewer_provisioning(thread, &provisioning)?;
        let reviewer_terminal_id = thread.reviewer_terminal_id.ok_or_else(reviewer_not_bound)?;
        if active_peer_session(
            &state,
            reviewer_terminal_id,
            thread.id,
            thread.source_terminal_id,
        )
        .is_none()
        {
            return Err(PeerError::new(
                PeerErrorKind::SessionInactive,
                "the reviewer terminal has no matching active peer session",
            ));
        }

        let thread = state
            .threads
            .get_mut(&provisioning.thread.id)
            .ok_or_else(thread_not_found)?;
        validate_reviewer_provisioning(thread, &provisioning)?;
        thread.provisioning_token = None;
        thread.updated_at = unix_time_millis();
        Ok(thread_view(thread))
    }

    pub fn abort_reviewer_provisioning(
        &self,
        provisioning: PeerProvisioning,
    ) -> std::result::Result<PeerThread, PeerError> {
        let mut state = lock(&self.inner.state);
        let thread = state
            .threads
            .get_mut(&provisioning.thread.id)
            .ok_or_else(thread_not_found)?;
        validate_reviewer_provisioning(thread, &provisioning)?;
        thread.provisioning_token = None;
        thread.updated_at = unix_time_millis();
        Ok(thread_view(thread))
    }

    pub fn revise_handoff(
        &self,
        thread_id: Uuid,
        turn_id: Uuid,
        expected_revision: u32,
        handoff: String,
    ) -> std::result::Result<PeerThread, PeerError> {
        let handoff = validate_artifact(handoff)?;
        let mut state = lock(&self.inner.state);
        let thread = state
            .threads
            .get_mut(&thread_id)
            .ok_or_else(thread_not_found)?;
        ensure_thread_accepts_operations(thread)?;
        let turn = current_turn_mut(thread, turn_id)?;
        if turn.status != PeerStatus::AwaitingPreview {
            return Err(invalid_state("the handoff is not awaiting preview"));
        }
        if turn.handoff_revision != expected_revision {
            return Err(revision_conflict());
        }
        if turn.handoff.as_deref() != Some(handoff.as_str()) {
            turn.handoff = Some(handoff);
            turn.handoff_revision = turn.handoff_revision.saturating_add(1);
        }
        thread.updated_at = unix_time_millis();
        Ok(thread_view(thread))
    }

    pub fn dispatch_turn(
        &self,
        thread_id: Uuid,
        turn_id: Uuid,
        expected_revision: u32,
        handoff: String,
    ) -> std::result::Result<PeerThread, PeerError> {
        let handoff = validate_artifact(handoff)?;
        let mut state = lock(&self.inner.state);
        let (source_terminal_id, reviewer_terminal_id) = {
            let thread = state.threads.get(&thread_id).ok_or_else(thread_not_found)?;
            ensure_thread_accepts_operations(thread)?;
            (
                thread.source_terminal_id,
                thread.reviewer_terminal_id.ok_or_else(reviewer_not_bound)?,
            )
        };
        let reviewer_session_id =
            active_peer_session(&state, reviewer_terminal_id, thread_id, source_terminal_id)
                .ok_or_else(|| {
                    PeerError::new(
                        PeerErrorKind::SessionInactive,
                        "the linked peer terminal is no longer active",
                    )
                })?;

        let thread = state
            .threads
            .get_mut(&thread_id)
            .ok_or_else(thread_not_found)?;
        let turn = current_turn_mut(thread, turn_id)?;
        if turn.status != PeerStatus::AwaitingPreview {
            return Err(invalid_state("the handoff is not awaiting dispatch"));
        }
        if turn.handoff_revision != expected_revision {
            return Err(revision_conflict());
        }
        if turn.handoff.as_deref() != Some(handoff.as_str()) {
            turn.handoff = Some(handoff);
            turn.handoff_revision = turn.handoff_revision.saturating_add(1);
        }
        turn.reviewer_session_id = Some(reviewer_session_id);
        turn.status = PeerStatus::Reviewing;
        thread.status = PeerStatus::Reviewing;
        thread.updated_at = unix_time_millis();
        Ok(thread_view(thread))
    }

    pub fn return_response(
        &self,
        thread_id: Uuid,
        turn_id: Uuid,
    ) -> std::result::Result<PeerThread, PeerError> {
        let delivery = self.begin_return_delivery(thread_id, turn_id)?;
        self.complete_return_delivery(delivery)
    }

    pub fn begin_return_delivery(
        &self,
        thread_id: Uuid,
        turn_id: Uuid,
    ) -> std::result::Result<PeerReturnDelivery, PeerError> {
        let mut state = lock(&self.inner.state);
        let thread = state
            .threads
            .get_mut(&thread_id)
            .ok_or_else(thread_not_found)?;
        ensure_thread_accepts_operations(thread)?;
        let turn = current_turn_mut(thread, turn_id)?;
        if turn.status != PeerStatus::ResponseReady || turn.response.is_none() {
            return Err(invalid_state("the peer response is not ready to return"));
        }
        let token = Uuid::new_v4();
        thread.return_delivery_token = Some(token);
        thread.updated_at = unix_time_millis();
        Ok(PeerReturnDelivery {
            thread: thread_view(thread),
            token,
            inner: self.inner.clone(),
        })
    }

    pub fn complete_return_delivery(
        &self,
        delivery: PeerReturnDelivery,
    ) -> std::result::Result<PeerThread, PeerError> {
        let mut state = lock(&self.inner.state);
        let thread = state
            .threads
            .get_mut(&delivery.thread.id)
            .ok_or_else(thread_not_found)?;
        validate_return_delivery(thread, &delivery)?;
        let turn = current_turn_mut(thread, delivery.thread.current_turn.id)?;
        turn.status = PeerStatus::Returned;
        thread.status = PeerStatus::Returned;
        thread.return_delivery_token = None;
        thread.updated_at = unix_time_millis();
        Ok(thread_view(thread))
    }

    pub fn abort_return_delivery(
        &self,
        delivery: PeerReturnDelivery,
    ) -> std::result::Result<PeerThread, PeerError> {
        let mut state = lock(&self.inner.state);
        let thread = state
            .threads
            .get_mut(&delivery.thread.id)
            .ok_or_else(thread_not_found)?;
        validate_return_delivery(thread, &delivery)?;
        thread.return_delivery_token = None;
        thread.updated_at = unix_time_millis();
        Ok(thread_view(thread))
    }

    pub fn fail_turn(
        &self,
        thread_id: Uuid,
        turn_id: Uuid,
        message: String,
    ) -> std::result::Result<PeerThread, PeerError> {
        let message = validate_error(message)?;
        let mut state = lock(&self.inner.state);
        let thread = state
            .threads
            .get_mut(&thread_id)
            .ok_or_else(thread_not_found)?;
        ensure_thread_accepts_operations(thread)?;
        let turn = current_turn_mut(thread, turn_id)?;
        if matches!(turn.status, PeerStatus::Returned | PeerStatus::Closed) {
            return Err(invalid_state("the completed peer turn cannot be failed"));
        }
        turn.status = PeerStatus::Failed;
        turn.error = Some(message);
        thread.status = PeerStatus::Failed;
        thread.updated_at = unix_time_millis();
        Ok(thread_view(thread))
    }

    pub fn close_thread(&self, thread_id: Uuid) -> std::result::Result<PeerThread, PeerError> {
        let closing = self.begin_close(thread_id)?;
        self.finalize_close(closing)
    }

    pub fn begin_close(&self, thread_id: Uuid) -> std::result::Result<PeerClose, PeerError> {
        let mut state = lock(&self.inner.state);
        let thread = state
            .threads
            .get_mut(&thread_id)
            .ok_or_else(thread_not_found)?;
        ensure_thread_accepts_operations(thread)?;
        let token = Uuid::new_v4();
        thread.close_token = Some(token);
        thread.updated_at = unix_time_millis();
        Ok(PeerClose {
            thread: thread_view(thread),
            token,
            inner: self.inner.clone(),
        })
    }

    pub fn finalize_close(&self, closing: PeerClose) -> std::result::Result<PeerThread, PeerError> {
        let mut state = lock(&self.inner.state);
        let thread = state
            .threads
            .get(&closing.thread.id)
            .ok_or_else(thread_not_found)?;
        validate_close(thread, &closing)?;
        let mut thread = state
            .threads
            .remove(&closing.thread.id)
            .ok_or_else(thread_not_found)?;
        thread.status = PeerStatus::Closed;
        if let Some(turn) = thread.turns.last_mut() {
            turn.status = PeerStatus::Closed;
        }
        thread.updated_at = unix_time_millis();
        Ok(thread_view(&thread))
    }

    pub fn abort_close(&self, closing: PeerClose) -> std::result::Result<PeerThread, PeerError> {
        let mut state = lock(&self.inner.state);
        let thread = state
            .threads
            .get_mut(&closing.thread.id)
            .ok_or_else(thread_not_found)?;
        validate_close(thread, &closing)?;
        thread.close_token = None;
        thread.updated_at = unix_time_millis();
        Ok(thread_view(thread))
    }

    pub fn submit(
        &self,
        capability: &str,
        turn_id: Uuid,
        content: String,
    ) -> std::result::Result<PeerThread, PeerError> {
        let subject = self.authenticate_capability(capability)?;
        match subject.purpose {
            SessionPurpose::Interactive => self.submit_source_handoff(capability, turn_id, content),
            SessionPurpose::Peer { .. } => self.submit_peer_response(capability, turn_id, content),
        }
    }

    pub fn submit_source_handoff(
        &self,
        capability: &str,
        turn_id: Uuid,
        handoff: String,
    ) -> std::result::Result<PeerThread, PeerError> {
        let handoff = validate_artifact(handoff)?;
        let mut state = lock(&self.inner.state);
        let subject = capability_subject_in(&state, capability)?;
        if subject.purpose != SessionPurpose::Interactive {
            return Err(unauthorized());
        }
        let thread_id = thread_id_for_turn(&state, turn_id).ok_or_else(thread_not_found)?;
        let thread = state
            .threads
            .get_mut(&thread_id)
            .ok_or_else(thread_not_found)?;
        ensure_thread_accepts_operations(thread)?;
        if thread.source_terminal_id != subject.terminal_id {
            return Err(unauthorized());
        }
        let turn = current_turn_mut(thread, turn_id)?;
        if turn.source_session_id != subject.session_id {
            return Err(unauthorized());
        }
        if turn.status == PeerStatus::AwaitingPreview
            && turn.handoff.as_deref() == Some(handoff.as_str())
        {
            return Ok(thread_view(thread));
        }
        if turn.status != PeerStatus::PreparingHandoff {
            return Err(invalid_state("the peer turn is not accepting a handoff"));
        }
        turn.handoff = Some(handoff);
        turn.handoff_revision = 1;
        turn.status = PeerStatus::AwaitingPreview;
        thread.status = PeerStatus::AwaitingPreview;
        thread.updated_at = unix_time_millis();
        Ok(thread_view(thread))
    }

    pub fn submit_peer_response(
        &self,
        capability: &str,
        turn_id: Uuid,
        response: String,
    ) -> std::result::Result<PeerThread, PeerError> {
        let response = validate_artifact(response)?;
        let mut state = lock(&self.inner.state);
        let subject = capability_subject_in(&state, capability)?;
        let (purpose_thread_id, parent_terminal_id) = match subject.purpose {
            SessionPurpose::Peer {
                thread_id,
                parent_terminal_id,
            } => (thread_id, parent_terminal_id),
            SessionPurpose::Interactive => return Err(unauthorized()),
        };
        let thread_id = thread_id_for_turn(&state, turn_id).ok_or_else(thread_not_found)?;
        if thread_id != purpose_thread_id {
            return Err(unauthorized());
        }
        let thread = state
            .threads
            .get_mut(&thread_id)
            .ok_or_else(thread_not_found)?;
        ensure_thread_accepts_operations(thread)?;
        if thread.source_terminal_id != parent_terminal_id
            || thread.reviewer_terminal_id != Some(subject.terminal_id)
        {
            return Err(unauthorized());
        }
        let turn = current_turn_mut(thread, turn_id)?;
        if turn.reviewer_session_id != Some(subject.session_id) {
            return Err(unauthorized());
        }
        if turn.status == PeerStatus::ResponseReady
            && turn.response.as_deref() == Some(response.as_str())
        {
            return Ok(thread_view(thread));
        }
        if turn.status != PeerStatus::Reviewing {
            return Err(invalid_state(
                "the peer turn is not accepting a reviewer response",
            ));
        }
        turn.response = Some(response);
        turn.status = PeerStatus::ResponseReady;
        thread.status = PeerStatus::ResponseReady;
        thread.updated_at = unix_time_millis();
        Ok(thread_view(thread))
    }

    pub fn receive(
        &self,
        capability: &str,
        turn_id: Uuid,
    ) -> std::result::Result<PeerArtifact, PeerError> {
        let subject = self.authenticate_capability(capability)?;
        match subject.purpose {
            SessionPurpose::Interactive => self.receive_for_source(capability, turn_id),
            SessionPurpose::Peer { .. } => self.receive_for_peer(capability, turn_id),
        }
    }

    pub fn receive_for_peer(
        &self,
        capability: &str,
        turn_id: Uuid,
    ) -> std::result::Result<PeerArtifact, PeerError> {
        let state = lock(&self.inner.state);
        let subject = capability_subject_in(&state, capability)?;
        let (purpose_thread_id, parent_terminal_id) = match subject.purpose {
            SessionPurpose::Peer {
                thread_id,
                parent_terminal_id,
            } => (thread_id, parent_terminal_id),
            SessionPurpose::Interactive => return Err(unauthorized()),
        };
        let thread_id = thread_id_for_turn(&state, turn_id).ok_or_else(thread_not_found)?;
        let thread = state.threads.get(&thread_id).ok_or_else(thread_not_found)?;
        ensure_thread_accepts_operations(thread)?;
        let turn = current_turn(thread, turn_id)?;
        if purpose_thread_id != thread_id
            || parent_terminal_id != thread.source_terminal_id
            || thread.reviewer_terminal_id != Some(subject.terminal_id)
            || turn.reviewer_session_id != Some(subject.session_id)
            || turn.status != PeerStatus::Reviewing
        {
            return Err(unauthorized());
        }
        Ok(PeerArtifact {
            turn_id,
            kind: PeerArtifactKind::Handoff,
            content: turn
                .handoff
                .clone()
                .ok_or_else(|| invalid_state("the dispatched peer handoff is not available"))?,
        })
    }

    pub fn receive_for_source(
        &self,
        capability: &str,
        turn_id: Uuid,
    ) -> std::result::Result<PeerArtifact, PeerError> {
        let state = lock(&self.inner.state);
        let subject = capability_subject_in(&state, capability)?;
        if subject.purpose != SessionPurpose::Interactive {
            return Err(unauthorized());
        }
        let thread_id = thread_id_for_turn(&state, turn_id).ok_or_else(thread_not_found)?;
        let thread = state.threads.get(&thread_id).ok_or_else(thread_not_found)?;
        if thread.close_token.is_some() {
            return Err(invalid_state("the peer thread is closing"));
        }
        let turn = current_turn(thread, turn_id)?;
        let response_is_returned = turn.status == PeerStatus::Returned
            || (turn.status == PeerStatus::ResponseReady && thread.return_delivery_token.is_some());
        if thread.source_terminal_id != subject.terminal_id
            || turn.source_session_id != subject.session_id
            || !response_is_returned
        {
            return Err(unauthorized());
        }
        Ok(PeerArtifact {
            turn_id,
            kind: PeerArtifactKind::Response,
            content: turn
                .response
                .clone()
                .ok_or_else(|| invalid_state("the returned peer response is not available"))?,
        })
    }
}

fn validate_instruction(value: String) -> std::result::Result<String, PeerError> {
    validate_text(
        value,
        MAX_PEER_INSTRUCTION_BYTES,
        "the peer instruction must not be empty",
        "the peer instruction is too large",
    )
}

fn validate_artifact(value: String) -> std::result::Result<String, PeerError> {
    let value = validate_text(
        value,
        MAX_PEER_ARTIFACT_BYTES,
        "the peer artifact must not be empty",
        "the peer artifact is too large",
    )?;
    if peer_artifact_has_unsafe_control(&value) {
        return Err(PeerError::new(
            PeerErrorKind::InvalidInput,
            "the peer artifact contains an unsafe control character",
        ));
    }
    Ok(normalize_peer_artifact_line_endings(value))
}

fn validate_error(value: String) -> std::result::Result<String, PeerError> {
    validate_text(
        value,
        MAX_PEER_ERROR_BYTES,
        "the peer error must not be empty",
        "the peer error is too large",
    )
}

fn validate_text(
    value: String,
    maximum_bytes: usize,
    empty_message: &'static str,
    large_message: &'static str,
) -> std::result::Result<String, PeerError> {
    if value.len() > maximum_bytes {
        return Err(PeerError::new(
            PeerErrorKind::PayloadTooLarge,
            large_message,
        ));
    }
    if value.trim().is_empty() || value.contains('\0') {
        return Err(PeerError::new(PeerErrorKind::InvalidInput, empty_message));
    }
    Ok(value)
}

pub(crate) fn peer_artifact_has_unsafe_control(value: &str) -> bool {
    value
        .chars()
        .any(|character| character.is_control() && !matches!(character, '\n' | '\r' | '\t'))
}

fn normalize_peer_artifact_line_endings(value: String) -> String {
    if value.contains('\r') {
        value.replace("\r\n", "\n").replace('\r', "\n")
    } else {
        value
    }
}

fn active_interactive_session(state: &BrokerState, terminal_id: Uuid) -> Option<Uuid> {
    state
        .capabilities
        .iter()
        .find_map(|((active_terminal_id, session_id), record)| {
            (*active_terminal_id == terminal_id && record.purpose == SessionPurpose::Interactive)
                .then_some(*session_id)
        })
}

fn fail_current_turn_for_ended_session(thread: &mut ThreadRecord, message: &str) {
    let turn = thread
        .turns
        .last_mut()
        .expect("a peer thread always contains at least one turn");
    turn.status = PeerStatus::Failed;
    turn.error = Some(message.to_owned());
    thread.status = PeerStatus::Failed;
    thread.updated_at = unix_time_millis();
}

fn active_peer_session(
    state: &BrokerState,
    terminal_id: Uuid,
    thread_id: Uuid,
    parent_terminal_id: Uuid,
) -> Option<Uuid> {
    state
        .capabilities
        .iter()
        .find_map(|((active_terminal_id, session_id), record)| {
            (*active_terminal_id == terminal_id
                && record.purpose
                    == SessionPurpose::Peer {
                        thread_id,
                        parent_terminal_id,
                    })
            .then_some(*session_id)
        })
}

fn capability_subject_in(
    state: &BrokerState,
    candidate: &str,
) -> std::result::Result<CapabilitySubject, PeerError> {
    if candidate.is_empty() || candidate.len() > 512 {
        return Err(unauthorized());
    }
    state
        .capabilities
        .iter()
        .find_map(|((terminal_id, session_id), record)| {
            constant_time_secret_match(&record.secret, candidate).then(|| CapabilitySubject {
                terminal_id: *terminal_id,
                session_id: *session_id,
                purpose: record.purpose.clone(),
            })
        })
        .ok_or_else(unauthorized)
}

fn thread_id_for_turn(state: &BrokerState, turn_id: Uuid) -> Option<Uuid> {
    state
        .threads
        .values()
        .find(|thread| thread.turns.iter().any(|turn| turn.id == turn_id))
        .map(|thread| thread.id)
}

fn current_turn(
    thread: &ThreadRecord,
    turn_id: Uuid,
) -> std::result::Result<&TurnRecord, PeerError> {
    thread
        .turns
        .last()
        .filter(|turn| turn.id == turn_id)
        .ok_or_else(|| {
            PeerError::new(
                PeerErrorKind::Conflict,
                "the requested peer turn is not current",
            )
        })
}

fn current_turn_mut(
    thread: &mut ThreadRecord,
    turn_id: Uuid,
) -> std::result::Result<&mut TurnRecord, PeerError> {
    thread
        .turns
        .last_mut()
        .filter(|turn| turn.id == turn_id)
        .ok_or_else(|| {
            PeerError::new(
                PeerErrorKind::Conflict,
                "the requested peer turn is not current",
            )
        })
}

fn ensure_thread_accepts_operations(thread: &ThreadRecord) -> std::result::Result<(), PeerError> {
    if thread.provisioning_token.is_some() {
        return Err(invalid_state(
            "the dedicated reviewer is still being provisioned",
        ));
    }
    if thread.close_token.is_some() {
        return Err(invalid_state("the peer thread is closing"));
    }
    if thread.return_delivery_token.is_some() {
        return Err(invalid_state(
            "peer response delivery is already in progress",
        ));
    }
    Ok(())
}

fn validate_reviewer_provisioning(
    thread: &ThreadRecord,
    provisioning: &PeerProvisioning,
) -> std::result::Result<(), PeerError> {
    if thread.provisioning_token != Some(provisioning.token)
        || thread.reviewer_terminal_id != provisioning.thread.reviewer_terminal_id
    {
        return Err(PeerError::new(
            PeerErrorKind::Conflict,
            "the dedicated reviewer provisioning operation is no longer active",
        ));
    }
    Ok(())
}

fn validate_return_delivery(
    thread: &ThreadRecord,
    delivery: &PeerReturnDelivery,
) -> std::result::Result<(), PeerError> {
    if thread.close_token.is_some() {
        return Err(invalid_state("the peer thread is closing"));
    }
    if thread.return_delivery_token != Some(delivery.token) {
        return Err(PeerError::new(
            PeerErrorKind::Conflict,
            "the peer response delivery is no longer active",
        ));
    }
    let turn = current_turn(thread, delivery.thread.current_turn.id)?;
    if thread.status != PeerStatus::ResponseReady
        || turn.status != PeerStatus::ResponseReady
        || turn.response.is_none()
    {
        return Err(invalid_state("the peer response is not ready to return"));
    }
    Ok(())
}

fn validate_close(
    thread: &ThreadRecord,
    closing: &PeerClose,
) -> std::result::Result<(), PeerError> {
    if thread.close_token != Some(closing.token) {
        return Err(PeerError::new(
            PeerErrorKind::Conflict,
            "the peer thread close operation is no longer active",
        ));
    }
    Ok(())
}

fn thread_view(thread: &ThreadRecord) -> PeerThread {
    let turn = thread
        .turns
        .last()
        .expect("a peer thread always contains at least one turn");
    PeerThread {
        id: thread.id,
        source_terminal_id: thread.source_terminal_id,
        reviewer_terminal_id: thread.reviewer_terminal_id,
        target_agent: thread.target_agent,
        status: thread.status,
        current_turn: PeerTurn {
            id: turn.id,
            sequence: turn.sequence,
            action: turn.action,
            instruction: turn.instruction.clone(),
            status: turn.status,
            handoff: turn.handoff.clone(),
            handoff_revision: turn.handoff_revision,
            response: turn.response.clone(),
            error: turn.error.clone(),
        },
        created_at: thread.created_at,
        updated_at: thread.updated_at,
    }
}

fn thread_not_found() -> PeerError {
    PeerError::new(PeerErrorKind::NotFound, "the peer thread was not found")
}

fn reviewer_not_bound() -> PeerError {
    PeerError::new(
        PeerErrorKind::ReviewerNotBound,
        "the peer thread has no linked reviewer terminal",
    )
}

fn invalid_state(message: &'static str) -> PeerError {
    PeerError::new(PeerErrorKind::InvalidState, message)
}

fn revision_conflict() -> PeerError {
    PeerError::new(
        PeerErrorKind::Conflict,
        "the peer handoff preview was changed by another request",
    )
}

fn generate_capability() -> Result<String> {
    let mut bytes = [0_u8; CAPABILITY_BYTES];
    getrandom::fill(&mut bytes)
        .map_err(|error| anyhow::anyhow!(error.to_string()))
        .context("failed to generate a peer session capability")?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

fn constant_time_secret_match(expected: &str, candidate: &str) -> bool {
    expected.len() == candidate.len() && bool::from(expected.as_bytes().ct_eq(candidate.as_bytes()))
}

fn unauthorized() -> PeerError {
    PeerError::new(
        PeerErrorKind::Unauthorized,
        "the peer capability is invalid or expired",
    )
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
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
    use std::ffi::OsStr;

    use super::*;

    fn broker() -> PeerBroker {
        PeerBroker::new(
            "127.0.0.1:43123".parse().expect("loopback endpoint"),
            PathBuf::from("codex-web"),
        )
    }

    fn capability(activation: &SessionActivation) -> String {
        environment_value(activation, CWT_PEER_CAPABILITY_ENV)
            .to_str()
            .map(str::to_owned)
            .expect("capability environment")
    }

    fn environment_value<'a>(activation: &'a SessionActivation, name: &str) -> &'a OsStr {
        activation
            .environment()
            .iter()
            .find(|(candidate, _)| candidate == name)
            .map(|(_, value)| value.as_os_str())
            .expect("activation environment value")
    }

    fn response_ready_review() -> (PeerBroker, String, PeerThread) {
        let broker = broker();
        let source_terminal_id = Uuid::new_v4();
        let source_activation = broker
            .activate_session(
                source_terminal_id,
                Uuid::new_v4(),
                &SessionPurpose::Interactive,
            )
            .expect("source activation");
        let source_capability = capability(&source_activation);
        let created = broker
            .create_thread(
                source_terminal_id,
                AgentKind::Claude,
                PeerAction::Review,
                "Review the implementation.".to_owned(),
            )
            .expect("create thread");
        let preview = broker
            .submit_source_handoff(
                &source_capability,
                created.current_turn.id,
                "Structured handoff".to_owned(),
            )
            .expect("submit handoff");
        let reviewer_terminal_id = Uuid::new_v4();
        let peer_activation = broker
            .activate_session(
                reviewer_terminal_id,
                Uuid::new_v4(),
                &SessionPurpose::Peer {
                    thread_id: created.id,
                    parent_terminal_id: source_terminal_id,
                },
            )
            .expect("peer activation");
        let peer_capability = capability(&peer_activation);
        broker
            .bind_reviewer(created.id, reviewer_terminal_id)
            .expect("bind reviewer");
        broker
            .dispatch_turn(
                created.id,
                created.current_turn.id,
                preview.current_turn.handoff_revision,
                "Structured handoff".to_owned(),
            )
            .expect("dispatch");
        let ready = broker
            .submit_peer_response(
                &peer_capability,
                created.current_turn.id,
                "Review findings".to_owned(),
            )
            .expect("submit response");

        (broker, source_capability, ready)
    }

    #[test]
    fn session_purpose_has_stable_tagged_json() {
        assert_eq!(
            serde_json::to_value(SessionPurpose::Interactive).expect("serialize"),
            serde_json::json!({"kind": "interactive"})
        );

        let thread_id = Uuid::new_v4();
        let parent_terminal_id = Uuid::new_v4();
        assert_eq!(
            serde_json::to_value(SessionPurpose::Peer {
                thread_id,
                parent_terminal_id,
            })
            .expect("serialize"),
            serde_json::json!({
                "kind": "peer",
                "threadId": thread_id,
                "parentTerminalId": parent_terminal_id,
            })
        );
    }

    #[cfg(unix)]
    #[test]
    fn activation_preserves_a_non_utf8_unix_helper_path() {
        use std::os::unix::ffi::{OsStrExt, OsStringExt};

        let raw_path = b"/tmp/codex-web-\xff".to_vec();
        let broker = PeerBroker::new(
            "127.0.0.1:43123".parse().expect("loopback endpoint"),
            PathBuf::from(OsString::from_vec(raw_path.clone())),
        );

        let activation = broker
            .activate_session(Uuid::new_v4(), Uuid::new_v4(), &SessionPurpose::Interactive)
            .expect("activate with a non-UTF-8 helper path");

        assert_eq!(
            environment_value(&activation, CWT_PEER_HELPER_ENV).as_bytes(),
            raw_path
        );
    }

    #[cfg(windows)]
    #[test]
    fn activation_preserves_an_unpaired_windows_helper_path() {
        use std::os::windows::ffi::{OsStrExt, OsStringExt};

        let raw_path = [
            b'C' as u16,
            b':' as u16,
            b'\\' as u16,
            b'c' as u16,
            b'w' as u16,
            b't' as u16,
            b'-' as u16,
            0xd800,
            b'.' as u16,
            b'e' as u16,
            b'x' as u16,
            b'e' as u16,
        ];
        let broker = PeerBroker::new(
            "127.0.0.1:43123".parse().expect("loopback endpoint"),
            PathBuf::from(OsString::from_wide(&raw_path)),
        );

        let activation = broker
            .activate_session(Uuid::new_v4(), Uuid::new_v4(), &SessionPurpose::Interactive)
            .expect("activate with an unpaired Windows helper path");

        assert_eq!(
            environment_value(&activation, CWT_PEER_HELPER_ENV)
                .encode_wide()
                .collect::<Vec<_>>(),
            raw_path
        );
    }

    #[test]
    fn activation_uses_five_scoped_environment_markers_and_revokes() {
        let broker = broker();
        let terminal_id = Uuid::new_v4();
        let session_id = Uuid::new_v4();
        let activation = broker
            .activate_session(terminal_id, session_id, &SessionPurpose::Interactive)
            .expect("activate");
        assert_eq!(activation.environment().len(), 5);
        let token = capability(&activation);
        let subject = broker
            .authenticate_capability(&token)
            .expect("authenticate");
        assert_eq!(subject.terminal_id(), terminal_id);
        assert_eq!(subject.session_id(), session_id);

        broker.revoke_session(terminal_id, session_id);
        broker.revoke_session(terminal_id, session_id);
        assert_eq!(
            broker.authenticate_capability(&token).expect_err("revoked"),
            unauthorized()
        );
    }

    #[test]
    fn exact_source_revocation_fails_preparation_without_stale_generation_cross_talk() {
        let broker = broker();
        let source_terminal_id = Uuid::new_v4();
        let stale_session_id = Uuid::new_v4();
        broker
            .activate_session(
                source_terminal_id,
                stale_session_id,
                &SessionPurpose::Interactive,
            )
            .expect("stale source activation");
        let source_session_id = Uuid::new_v4();
        broker
            .activate_session(
                source_terminal_id,
                source_session_id,
                &SessionPurpose::Interactive,
            )
            .expect("current source activation");
        let created = broker
            .create_thread(
                source_terminal_id,
                AgentKind::Claude,
                PeerAction::Review,
                "Review the implementation.".to_owned(),
            )
            .expect("create thread");

        broker.revoke_session(source_terminal_id, stale_session_id);
        let unchanged = broker.get_thread(created.id).expect("thread remains");
        assert_eq!(unchanged.status, PeerStatus::PreparingHandoff);
        assert_eq!(unchanged.current_turn.error, None);

        broker.revoke_session(source_terminal_id, source_session_id);
        let failed = broker
            .get_thread(created.id)
            .expect("failed thread remains");
        assert_eq!(failed.status, PeerStatus::Failed);
        assert_eq!(failed.current_turn.status, PeerStatus::Failed);
        assert_eq!(
            failed.current_turn.error.as_deref(),
            Some(SOURCE_SESSION_ENDED_ERROR)
        );
    }

    #[test]
    fn exact_reviewer_revocation_fails_review_without_stale_generation_cross_talk() {
        let broker = broker();
        let source_terminal_id = Uuid::new_v4();
        let source_activation = broker
            .activate_session(
                source_terminal_id,
                Uuid::new_v4(),
                &SessionPurpose::Interactive,
            )
            .expect("source activation");
        let source_capability = capability(&source_activation);
        let created = broker
            .create_thread(
                source_terminal_id,
                AgentKind::Claude,
                PeerAction::Review,
                "Review the implementation.".to_owned(),
            )
            .expect("create thread");
        let preview = broker
            .submit_source_handoff(
                &source_capability,
                created.current_turn.id,
                "Structured handoff".to_owned(),
            )
            .expect("submit handoff");
        let reviewer_terminal_id = Uuid::new_v4();
        let stale_session_id = Uuid::new_v4();
        broker
            .activate_session(
                reviewer_terminal_id,
                stale_session_id,
                &SessionPurpose::Peer {
                    thread_id: created.id,
                    parent_terminal_id: source_terminal_id,
                },
            )
            .expect("stale reviewer activation");
        let reviewer_session_id = Uuid::new_v4();
        broker
            .activate_session(
                reviewer_terminal_id,
                reviewer_session_id,
                &SessionPurpose::Peer {
                    thread_id: created.id,
                    parent_terminal_id: source_terminal_id,
                },
            )
            .expect("current reviewer activation");
        broker
            .bind_reviewer(created.id, reviewer_terminal_id)
            .expect("bind reviewer");
        broker
            .dispatch_turn(
                created.id,
                created.current_turn.id,
                preview.current_turn.handoff_revision,
                "Structured handoff".to_owned(),
            )
            .expect("dispatch");

        broker.revoke_session(reviewer_terminal_id, stale_session_id);
        let unchanged = broker.get_thread(created.id).expect("thread remains");
        assert_eq!(unchanged.status, PeerStatus::Reviewing);
        assert_eq!(unchanged.current_turn.error, None);

        broker.revoke_session(reviewer_terminal_id, reviewer_session_id);
        let failed = broker
            .get_thread(created.id)
            .expect("failed thread remains");
        assert_eq!(failed.status, PeerStatus::Failed);
        assert_eq!(failed.current_turn.status, PeerStatus::Failed);
        assert_eq!(
            failed.current_turn.error.as_deref(),
            Some(REVIEWER_SESSION_ENDED_ERROR)
        );
    }

    #[test]
    fn reviewer_revocation_before_dispatch_fails_the_pending_preview() {
        let broker = broker();
        let source_terminal_id = Uuid::new_v4();
        let source_activation = broker
            .activate_session(
                source_terminal_id,
                Uuid::new_v4(),
                &SessionPurpose::Interactive,
            )
            .expect("source activation");
        let source_capability = capability(&source_activation);
        let created = broker
            .create_thread(
                source_terminal_id,
                AgentKind::Claude,
                PeerAction::Review,
                "Review the implementation.".to_owned(),
            )
            .expect("create thread");
        let provisioning = broker
            .begin_reviewer_provisioning(created.id)
            .expect("begin reviewer provisioning");
        let reviewer_terminal_id = provisioning
            .thread()
            .reviewer_terminal_id
            .expect("reserved reviewer");
        let reviewer_session_id = Uuid::new_v4();
        broker
            .activate_session(
                reviewer_terminal_id,
                reviewer_session_id,
                &SessionPurpose::Peer {
                    thread_id: created.id,
                    parent_terminal_id: source_terminal_id,
                },
            )
            .expect("reviewer activation");
        broker
            .complete_reviewer_provisioning(provisioning)
            .expect("complete reviewer provisioning");
        broker
            .submit_source_handoff(
                &source_capability,
                created.current_turn.id,
                "Structured handoff".to_owned(),
            )
            .expect("submit handoff");

        broker.revoke_session(reviewer_terminal_id, reviewer_session_id);
        let failed = broker
            .get_thread(created.id)
            .expect("failed thread remains");
        assert_eq!(failed.status, PeerStatus::Failed);
        assert_eq!(
            failed.current_turn.error.as_deref(),
            Some(REVIEWER_SESSION_ENDED_ERROR)
        );
    }

    #[test]
    fn reviewer_revocation_preserves_a_ready_response_for_source_return() {
        let broker = broker();
        let source_terminal_id = Uuid::new_v4();
        let source_activation = broker
            .activate_session(
                source_terminal_id,
                Uuid::new_v4(),
                &SessionPurpose::Interactive,
            )
            .expect("source activation");
        let source_capability = capability(&source_activation);
        let created = broker
            .create_thread(
                source_terminal_id,
                AgentKind::Claude,
                PeerAction::Review,
                "Review the implementation.".to_owned(),
            )
            .expect("create thread");
        let preview = broker
            .submit_source_handoff(
                &source_capability,
                created.current_turn.id,
                "Structured handoff".to_owned(),
            )
            .expect("submit handoff");
        let reviewer_terminal_id = Uuid::new_v4();
        let reviewer_session_id = Uuid::new_v4();
        let reviewer_activation = broker
            .activate_session(
                reviewer_terminal_id,
                reviewer_session_id,
                &SessionPurpose::Peer {
                    thread_id: created.id,
                    parent_terminal_id: source_terminal_id,
                },
            )
            .expect("reviewer activation");
        broker
            .bind_reviewer(created.id, reviewer_terminal_id)
            .expect("bind reviewer");
        broker
            .dispatch_turn(
                created.id,
                created.current_turn.id,
                preview.current_turn.handoff_revision,
                "Structured handoff".to_owned(),
            )
            .expect("dispatch");
        broker
            .submit_peer_response(
                &capability(&reviewer_activation),
                created.current_turn.id,
                "Review findings".to_owned(),
            )
            .expect("submit response");

        broker.revoke_session(reviewer_terminal_id, reviewer_session_id);
        let ready = broker.get_thread(created.id).expect("ready thread remains");
        assert_eq!(ready.status, PeerStatus::ResponseReady);
        assert_eq!(ready.current_turn.error, None);
        assert_eq!(
            ready.current_turn.response.as_deref(),
            Some("Review findings")
        );

        let returned = broker
            .return_response(created.id, created.current_turn.id)
            .expect("return stored response");
        assert_eq!(returned.status, PeerStatus::Returned);
        assert_eq!(
            broker
                .receive_for_source(&source_capability, created.current_turn.id)
                .expect("source receives stored response")
                .content,
            "Review findings"
        );
    }

    #[test]
    fn revocation_preserves_provisioning_and_close_leases() {
        let provisioning_broker = broker();
        let source_terminal_id = Uuid::new_v4();
        provisioning_broker
            .activate_session(
                source_terminal_id,
                Uuid::new_v4(),
                &SessionPurpose::Interactive,
            )
            .expect("source activation");
        let created = provisioning_broker
            .create_thread(
                source_terminal_id,
                AgentKind::Claude,
                PeerAction::Review,
                "Review the implementation.".to_owned(),
            )
            .expect("create thread");
        let provisioning = provisioning_broker
            .begin_reviewer_provisioning(created.id)
            .expect("begin reviewer provisioning");
        let reviewer_terminal_id = provisioning
            .thread()
            .reviewer_terminal_id
            .expect("reserved reviewer");
        let reviewer_session_id = Uuid::new_v4();
        provisioning_broker
            .activate_session(
                reviewer_terminal_id,
                reviewer_session_id,
                &SessionPurpose::Peer {
                    thread_id: created.id,
                    parent_terminal_id: source_terminal_id,
                },
            )
            .expect("reviewer activation");

        provisioning_broker.revoke_session(reviewer_terminal_id, reviewer_session_id);
        let aborted = provisioning_broker
            .abort_reviewer_provisioning(provisioning)
            .expect("provisioning lease remains valid");
        assert_eq!(aborted.status, PeerStatus::Failed);

        let closing_broker = broker();
        let source_terminal_id = Uuid::new_v4();
        let source_session_id = Uuid::new_v4();
        closing_broker
            .activate_session(
                source_terminal_id,
                source_session_id,
                &SessionPurpose::Interactive,
            )
            .expect("source activation");
        let created = closing_broker
            .create_thread(
                source_terminal_id,
                AgentKind::Claude,
                PeerAction::Review,
                "Review the implementation.".to_owned(),
            )
            .expect("create thread");
        let closing = closing_broker.begin_close(created.id).expect("begin close");

        closing_broker.revoke_session(source_terminal_id, source_session_id);
        let closed = closing_broker
            .finalize_close(closing)
            .expect("close lease remains valid");
        assert_eq!(closed.status, PeerStatus::Closed);
    }

    #[test]
    fn shutdown_revokes_every_active_capability() {
        let broker = broker();
        let first = broker
            .activate_session(Uuid::new_v4(), Uuid::new_v4(), &SessionPurpose::Interactive)
            .expect("first activation");
        let second = broker
            .activate_session(Uuid::new_v4(), Uuid::new_v4(), &SessionPurpose::Interactive)
            .expect("second activation");
        let first_capability = capability(&first);
        let second_capability = capability(&second);

        broker.begin_shutdown();
        broker.begin_shutdown();

        assert_eq!(
            broker
                .authenticate_capability(&first_capability)
                .expect_err("first capability revoked"),
            unauthorized()
        );
        assert_eq!(
            broker
                .authenticate_capability(&second_capability)
                .expect_err("second capability revoked"),
            unauthorized()
        );
        assert!(
            broker
                .activate_session(Uuid::new_v4(), Uuid::new_v4(), &SessionPurpose::Interactive,)
                .is_err()
        );
    }

    #[test]
    fn full_review_and_recheck_use_the_same_peer_terminal() {
        let broker = broker();
        let source_terminal_id = Uuid::new_v4();
        let source_session_id = Uuid::new_v4();
        let source_activation = broker
            .activate_session(
                source_terminal_id,
                source_session_id,
                &SessionPurpose::Interactive,
            )
            .expect("source activation");
        let source_capability = capability(&source_activation);

        let created = broker
            .create_thread(
                source_terminal_id,
                AgentKind::Claude,
                PeerAction::Review,
                "Review the current design.".to_owned(),
            )
            .expect("create thread");
        let first_turn_id = created.current_turn.id;
        let awaiting_preview = broker
            .submit_source_handoff(
                &source_capability,
                first_turn_id,
                "Structured handoff".to_owned(),
            )
            .expect("submit handoff");
        assert_eq!(awaiting_preview.status, PeerStatus::AwaitingPreview);

        let reviewer_terminal_id = Uuid::new_v4();
        let reviewer_session_id = Uuid::new_v4();
        let peer_activation = broker
            .activate_session(
                reviewer_terminal_id,
                reviewer_session_id,
                &SessionPurpose::Peer {
                    thread_id: created.id,
                    parent_terminal_id: source_terminal_id,
                },
            )
            .expect("peer activation");
        let peer_capability = capability(&peer_activation);
        broker
            .bind_reviewer(created.id, reviewer_terminal_id)
            .expect("bind reviewer");
        broker
            .dispatch_turn(
                created.id,
                first_turn_id,
                awaiting_preview.current_turn.handoff_revision,
                "Structured handoff".to_owned(),
            )
            .expect("dispatch");
        assert_eq!(
            broker
                .receive_for_peer(&peer_capability, first_turn_id)
                .expect("peer receive")
                .content,
            "Structured handoff"
        );
        let ready = broker
            .submit_peer_response(
                &peer_capability,
                first_turn_id,
                "Review findings".to_owned(),
            )
            .expect("peer response");
        assert_eq!(ready.status, PeerStatus::ResponseReady);
        broker
            .return_response(created.id, first_turn_id)
            .expect("return response");
        assert_eq!(
            broker
                .receive_for_source(&source_capability, first_turn_id)
                .expect("source receive")
                .content,
            "Review findings"
        );

        let recheck = broker
            .create_turn(
                created.id,
                PeerAction::Recheck,
                "Recheck the revised design.".to_owned(),
            )
            .expect("create recheck");
        assert_eq!(recheck.reviewer_terminal_id, Some(reviewer_terminal_id));
        assert_eq!(recheck.current_turn.sequence, 2);
        assert_eq!(
            broker
                .get_thread(created.id)
                .expect("recheck remains registered")
                .current_turn
                .id,
            recheck.current_turn.id
        );

        let second_preview = broker
            .submit_source_handoff(
                &source_capability,
                recheck.current_turn.id,
                "Updated structured handoff".to_owned(),
            )
            .expect("submit recheck handoff");
        broker
            .dispatch_turn(
                created.id,
                recheck.current_turn.id,
                second_preview.current_turn.handoff_revision,
                "Updated structured handoff".to_owned(),
            )
            .expect("dispatch recheck");
        assert_eq!(
            broker
                .receive_for_peer(&peer_capability, recheck.current_turn.id)
                .expect("peer receives recheck")
                .content,
            "Updated structured handoff"
        );
        broker
            .submit_peer_response(
                &peer_capability,
                recheck.current_turn.id,
                "Updated review findings".to_owned(),
            )
            .expect("submit recheck response");
        broker
            .return_response(created.id, recheck.current_turn.id)
            .expect("return recheck response");
        assert_eq!(
            broker
                .receive_for_source(&source_capability, recheck.current_turn.id)
                .expect("source receives recheck")
                .content,
            "Updated review findings"
        );

        let closed = broker.close_thread(created.id).expect("close thread");
        assert_eq!(closed.status, PeerStatus::Closed);
        assert_eq!(
            broker
                .get_thread(created.id)
                .expect_err("thread is purged")
                .kind(),
            PeerErrorKind::NotFound
        );
        assert!(broker.list_threads().is_empty());
    }

    #[test]
    fn active_thread_and_per_thread_turn_limits_are_enforced() {
        let broker = broker();
        let source_terminal_id = Uuid::new_v4();
        let _source_activation = broker
            .activate_session(
                source_terminal_id,
                Uuid::new_v4(),
                &SessionPurpose::Interactive,
            )
            .expect("source activation");

        let first = broker
            .create_thread(
                source_terminal_id,
                AgentKind::Claude,
                PeerAction::Review,
                "Initial review.".to_owned(),
            )
            .expect("first thread");
        for index in 1..MAX_PEER_THREADS {
            broker
                .create_thread(
                    source_terminal_id,
                    AgentKind::Claude,
                    PeerAction::Review,
                    format!("Review thread {index}."),
                )
                .expect("thread within global limit");
        }
        let thread_error = broker
            .create_thread(
                source_terminal_id,
                AgentKind::Claude,
                PeerAction::Review,
                "One thread too many.".to_owned(),
            )
            .expect_err("global thread limit");
        assert_eq!(thread_error.kind(), PeerErrorKind::LimitReached);

        let reviewer_terminal_id = Uuid::new_v4();
        let _reviewer_activation = broker
            .activate_session(
                reviewer_terminal_id,
                Uuid::new_v4(),
                &SessionPurpose::Peer {
                    thread_id: first.id,
                    parent_terminal_id: source_terminal_id,
                },
            )
            .expect("reviewer activation");
        broker
            .bind_reviewer(first.id, reviewer_terminal_id)
            .expect("bind reviewer");

        {
            let mut state = lock(&broker.inner.state);
            let thread = state.threads.get_mut(&first.id).expect("first thread");
            thread.status = PeerStatus::Returned;
            thread.turns.last_mut().expect("first turn").status = PeerStatus::Returned;
        }
        for sequence in 2..=MAX_PEER_TURNS_PER_THREAD {
            broker
                .create_turn(
                    first.id,
                    PeerAction::Recheck,
                    format!("Recheck turn {sequence}."),
                )
                .expect("turn within per-thread limit");
            let mut state = lock(&broker.inner.state);
            let thread = state.threads.get_mut(&first.id).expect("first thread");
            thread.status = PeerStatus::Returned;
            thread.turns.last_mut().expect("current turn").status = PeerStatus::Returned;
        }
        let turn_error = broker
            .create_turn(
                first.id,
                PeerAction::Recheck,
                "One turn too many.".to_owned(),
            )
            .expect_err("per-thread turn limit");
        assert_eq!(turn_error.kind(), PeerErrorKind::LimitReached);
    }

    #[test]
    fn failed_return_delivery_rolls_back_and_can_be_retried() {
        let (broker, source_capability, ready) = response_ready_review();
        let turn_id = ready.current_turn.id;

        let delivery = broker
            .begin_return_delivery(ready.id, turn_id)
            .expect("begin return delivery");
        assert_eq!(delivery.thread().status, PeerStatus::ResponseReady);
        assert_eq!(
            broker
                .receive_for_source(&source_capability, turn_id)
                .expect("source can retrieve the response after notification is queued")
                .content,
            "Review findings"
        );
        assert_eq!(
            broker
                .begin_close(ready.id)
                .err()
                .expect("a return delivery blocks close")
                .kind(),
            PeerErrorKind::InvalidState
        );

        let rolled_back = broker
            .abort_return_delivery(delivery)
            .expect("roll back failed notification");
        assert_eq!(rolled_back.status, PeerStatus::ResponseReady);
        assert_eq!(
            broker
                .receive_for_source(&source_capability, turn_id)
                .expect_err("an undelivered response remains private")
                .kind(),
            PeerErrorKind::Unauthorized
        );

        let abandoned = broker
            .begin_return_delivery(ready.id, turn_id)
            .expect("begin abandoned delivery");
        drop(abandoned);
        let retry = broker
            .begin_return_delivery(ready.id, turn_id)
            .expect("retry return delivery after cancellation");
        let returned = broker
            .complete_return_delivery(retry)
            .expect("complete retry");
        assert_eq!(returned.status, PeerStatus::Returned);
        assert_eq!(
            broker
                .receive_for_source(&source_capability, turn_id)
                .expect("source receives returned response")
                .content,
            "Review findings"
        );
    }

    #[test]
    fn close_lease_rejects_operations_and_abort_restores_the_thread() {
        let broker = broker();
        let source_terminal_id = Uuid::new_v4();
        let source_activation = broker
            .activate_session(
                source_terminal_id,
                Uuid::new_v4(),
                &SessionPurpose::Interactive,
            )
            .expect("source activation");
        let source_capability = capability(&source_activation);
        let created = broker
            .create_thread(
                source_terminal_id,
                AgentKind::Claude,
                PeerAction::Review,
                "Review the implementation.".to_owned(),
            )
            .expect("create thread");

        let closing = broker.begin_close(created.id).expect("begin close");
        assert_eq!(closing.thread().status, PeerStatus::PreparingHandoff);
        assert_eq!(
            broker
                .begin_close(created.id)
                .err()
                .expect("a concurrent close is rejected")
                .kind(),
            PeerErrorKind::InvalidState
        );
        assert_eq!(
            broker
                .submit_source_handoff(
                    &source_capability,
                    created.current_turn.id,
                    "Blocked handoff".to_owned(),
                )
                .expect_err("a closing thread rejects mutations")
                .kind(),
            PeerErrorKind::InvalidState
        );

        let restored = broker.abort_close(closing).expect("abort close");
        assert_eq!(restored.status, PeerStatus::PreparingHandoff);
        broker
            .submit_source_handoff(
                &source_capability,
                created.current_turn.id,
                "Accepted handoff".to_owned(),
            )
            .expect("operations resume after close rollback");

        let abandoned = broker
            .begin_close(created.id)
            .expect("begin abandoned close");
        drop(abandoned);
        let closing = broker
            .begin_close(created.id)
            .expect("retry close after cancellation");
        let closed = broker.finalize_close(closing).expect("finalize close");
        assert_eq!(closed.status, PeerStatus::Closed);
        assert_eq!(
            broker
                .close_thread(created.id)
                .expect_err("a completed close is idempotently absent")
                .kind(),
            PeerErrorKind::NotFound
        );
    }

    #[test]
    fn reviewer_provisioning_owns_identity_and_blocks_close_until_confirmed() {
        let broker = broker();
        let source_terminal_id = Uuid::new_v4();
        broker
            .activate_session(
                source_terminal_id,
                Uuid::new_v4(),
                &SessionPurpose::Interactive,
            )
            .expect("source activation");
        let created = broker
            .create_thread(
                source_terminal_id,
                AgentKind::Claude,
                PeerAction::Review,
                "Review the implementation.".to_owned(),
            )
            .expect("create thread");

        let provisioning = broker
            .begin_reviewer_provisioning(created.id)
            .expect("begin reviewer provisioning");
        let reviewer_terminal_id = provisioning
            .thread()
            .reviewer_terminal_id
            .expect("provisioning owns a reviewer identity");
        assert_eq!(
            broker
                .begin_close(created.id)
                .err()
                .expect("close is blocked during provisioning")
                .kind(),
            PeerErrorKind::InvalidState
        );

        broker
            .activate_session(
                reviewer_terminal_id,
                Uuid::new_v4(),
                &SessionPurpose::Peer {
                    thread_id: created.id,
                    parent_terminal_id: source_terminal_id,
                },
            )
            .expect("activate reserved reviewer");
        let provisioned = broker
            .complete_reviewer_provisioning(provisioning)
            .expect("complete reviewer provisioning");
        assert_eq!(provisioned.reviewer_terminal_id, Some(reviewer_terminal_id));

        let closing = broker
            .begin_close(created.id)
            .expect("close is allowed after provisioning");
        broker.finalize_close(closing).expect("finalize close");
    }

    #[test]
    fn cross_thread_peer_capability_is_rejected() {
        let broker = broker();
        let source_terminal_id = Uuid::new_v4();
        let source_activation = broker
            .activate_session(
                source_terminal_id,
                Uuid::new_v4(),
                &SessionPurpose::Interactive,
            )
            .expect("source activation");
        let source_capability = capability(&source_activation);
        let first = broker
            .create_thread(
                source_terminal_id,
                AgentKind::Claude,
                PeerAction::Review,
                "First".to_owned(),
            )
            .expect("first thread");
        let second = broker
            .create_thread(
                source_terminal_id,
                AgentKind::Agy,
                PeerAction::Verify,
                "Second".to_owned(),
            )
            .expect("second thread");
        broker
            .submit_source_handoff(
                &source_capability,
                second.current_turn.id,
                "Second handoff".to_owned(),
            )
            .expect("handoff");
        let peer_terminal_id = Uuid::new_v4();
        let peer_activation = broker
            .activate_session(
                peer_terminal_id,
                Uuid::new_v4(),
                &SessionPurpose::Peer {
                    thread_id: first.id,
                    parent_terminal_id: source_terminal_id,
                },
            )
            .expect("peer activation");
        assert_eq!(
            broker
                .receive_for_peer(&capability(&peer_activation), second.current_turn.id)
                .expect_err("cross-thread read"),
            unauthorized()
        );
    }

    #[test]
    fn artifact_and_instruction_limits_are_byte_based() {
        assert_eq!(
            validate_instruction("x".repeat(MAX_PEER_INSTRUCTION_BYTES + 1))
                .expect_err("oversized instruction")
                .kind(),
            PeerErrorKind::PayloadTooLarge
        );
        assert_eq!(
            validate_artifact("x".repeat(MAX_PEER_ARTIFACT_BYTES + 1))
                .expect_err("oversized artifact")
                .kind(),
            PeerErrorKind::PayloadTooLarge
        );
    }

    #[test]
    fn artifacts_preserve_unicode_markdown_and_normalize_line_endings() {
        let artifact = "Резюме\r\n\r\n- проверка\r- `код`\t✓";

        assert_eq!(
            validate_artifact(artifact.to_owned()).expect("valid Markdown artifact"),
            "Резюме\n\n- проверка\n- `код`\t✓"
        );
    }

    #[test]
    fn artifacts_reject_terminal_control_sequences() {
        for artifact in [
            "bell\u{0007}",
            "escape\u{001b}[2J",
            "delete\u{007f}",
            "c1-csi\u{009b}2J",
        ] {
            assert_eq!(
                validate_artifact(artifact.to_owned())
                    .expect_err("terminal control must be rejected")
                    .kind(),
                PeerErrorKind::InvalidInput
            );
        }
    }
}
