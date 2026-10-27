//! One-turn context transfer ("Continue in <agent>") between an ordinary
//! source session and an ordinary target session.
//!
//! A transfer is a separately typed transaction that shares the peer broker's
//! loopback helper, per-generation capabilities, and artifact validation, but
//! never a peer thread, a dedicated reviewer, or peer Close. Its target is a
//! user-owned ordinary session that is published at reservation. Delivery
//! happens only on an explicit, readiness-confirmed user action, and the
//! artifact text lives in memory only until a terminal state.
//!
//! Lock order: transfer coordinator -> session registry -> session record.
//! The peer broker lock is never nested inside the coordinator lock:
//! capability authentication completes and releases it before dispatch, and
//! revocation notifications arrive through an observer invoked outside it.

use std::{
    collections::HashMap,
    fmt,
    path::PathBuf,
    sync::{Arc, Mutex, MutexGuard},
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::sync::watch;
use uuid::Uuid;

use crate::{
    config::AgentKind,
    peer::{
        CapabilitySubject, PeerBroker, PeerError, PeerErrorKind, SessionPurpose, validate_artifact,
        validate_instruction,
    },
    peer_routes::compact_for_prompt,
    registry::{RegistryError, SessionRegistry, TransferClaim, TransferRole},
    session::{Lifecycle, SessionManager, SessionSnapshot},
};

/// Active transfers that may still hold artifact text.
pub const MAX_ACTIVE_TRANSFERS: usize = 256;
/// Ledger records (transfers plus their attempts) per server generation. The
/// ledger fails closed instead of evicting, so a delayed duplicate request can
/// never create a second target.
pub const MAX_TRANSFER_LEDGER_RECORDS: usize = 4096;
/// Target attempts per transfer.
pub const MAX_TRANSFER_ATTEMPTS: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransferMode {
    Prepare,
    Manual,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransferState {
    Preparing,
    Preview,
    Starting,
    CleanupPending,
    TargetBound,
    Delivering,
    AwaitingReceipt,
    Delivered,
    Failed,
    Abandoned,
}

impl TransferState {
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Delivered | Self::Failed | Self::Abandoned)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Preparing => "preparing",
            Self::Preview => "preview",
            Self::Starting => "starting",
            Self::CleanupPending => "cleanup_pending",
            Self::TargetBound => "target_bound",
            Self::Delivering => "delivering",
            Self::AwaitingReceipt => "awaiting_receipt",
            Self::Delivered => "delivered",
            Self::Failed => "failed",
            Self::Abandoned => "abandoned",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransferPreparation {
    Pending,
    Submitted,
    Overridden,
    SourceEnded,
    QueueRejected,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransferDelivery {
    Unconfirmed,
    TargetEnded,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TargetMode {
    New,
    Existing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttemptOutcome {
    Starting,
    Bound,
    StartFailed,
    CleanupPending,
    TargetEnded,
    Delivered,
    Abandoned,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransferErrorCode {
    TransferNotFound,
    TransferConflict,
    GenerationConflict,
    RevisionConflict,
    AttemptInvalidated,
    AttemptPending,
    AttemptLimitReached,
    ParticipantBusy,
    InvalidParticipant,
    TransferLimitReached,
    TransferHistoryFull,
    CapacityFull,
    InvalidDirectory,
    AgentUnavailable,
    SourceNotRunning,
    TargetNotEligible,
    TargetStartFailed,
    TargetEnded,
    SourceEnded,
    CleanupPending,
    CleanupFailed,
    QueueRejected,
    TransferDiscarded,
    AlreadyDelivered,
    InvalidState,
    InvalidInput,
    PayloadTooLarge,
    Unauthorized,
    ShuttingDown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransferError {
    code: TransferErrorCode,
    message: &'static str,
}

impl TransferError {
    const fn new(code: TransferErrorCode, message: &'static str) -> Self {
        Self { code, message }
    }

    /// An error carrying only its code with a generic message.
    pub fn from_code(code: TransferErrorCode) -> Self {
        let message = match code {
            TransferErrorCode::TransferNotFound => "transfer not found",
            TransferErrorCode::ShuttingDown => "the server is shutting down",
            _ => "the transfer request could not be completed",
        };
        Self::new(code, message)
    }

    pub fn code(&self) -> TransferErrorCode {
        self.code
    }

    pub fn message(&self) -> &'static str {
        self.message
    }
}

impl fmt::Display for TransferError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message)
    }
}

impl std::error::Error for TransferError {}

impl From<PeerError> for TransferError {
    fn from(error: PeerError) -> Self {
        let code = match error.kind() {
            PeerErrorKind::Unauthorized => TransferErrorCode::Unauthorized,
            PeerErrorKind::PayloadTooLarge => TransferErrorCode::PayloadTooLarge,
            PeerErrorKind::InvalidInput => TransferErrorCode::InvalidInput,
            PeerErrorKind::NotFound => TransferErrorCode::TransferNotFound,
            _ => TransferErrorCode::InvalidState,
        };
        Self::new(code, "the transfer artifact was rejected")
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TransferTargetSnapshot {
    pub mode: TargetMode,
    pub terminal_id: Uuid,
    pub session_id: Option<Uuid>,
    pub agent: AgentKind,
    pub name: String,
    pub project: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TransferAttemptSnapshot {
    pub target_request_id: Uuid,
    pub number: u32,
    pub target: TransferTargetSnapshot,
    pub outcome: AttemptOutcome,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TransferErrorSnapshot {
    pub code: TransferErrorCode,
}

/// Public view of a transfer. Never contains artifact text or the instruction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TransferSnapshot {
    pub transfer_id: Uuid,
    pub revision: u64,
    pub state: TransferState,
    pub mode: TransferMode,
    pub source_terminal_id: Uuid,
    pub source_session_id: Option<Uuid>,
    pub draft_revision: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub approved_revision: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preparation: Option<TransferPreparation>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attempt: Option<TransferAttemptSnapshot>,
    pub attempts_used: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delivery: Option<TransferDelivery>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<TransferErrorSnapshot>,
    pub created_at: u64,
    pub updated_at: u64,
}

/// What the browser asked for as a target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TargetRequest {
    New {
        agent: AgentKind,
        project_dir: PathBuf,
        directory_id: String,
    },
    Existing {
        terminal_id: Uuid,
        session_id: Uuid,
    },
}

/// Result of admitting a target request.
pub enum TargetAdmission {
    /// A fresh target was reserved; the caller must start it and report back
    /// with [`TransferCoordinator::complete_target_start`].
    Start {
        session: SessionManager,
        token: StartToken,
        snapshot: TransferSnapshot,
    },
    /// The same request is already in flight; wait for its completion.
    Wait(watch::Receiver<bool>),
    /// Nothing to start: the target is bound or the saved outcome is returned.
    Settled(TransferSnapshot),
}

impl fmt::Debug for TargetAdmission {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Start { token, .. } => formatter
                .debug_struct("Start")
                .field("token", token)
                .finish_non_exhaustive(),
            Self::Wait(_) => formatter.write_str("Wait"),
            Self::Settled(snapshot) => formatter.debug_tuple("Settled").field(snapshot).finish(),
        }
    }
}

/// Identifies one start attempt so a late completion can never be applied to
/// a different attempt or a different generation of the transfer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StartToken {
    transfer_id: Uuid,
    target_request_id: Uuid,
    terminal_id: Uuid,
}

impl StartToken {
    pub fn terminal_id(&self) -> Uuid {
        self.terminal_id
    }
}

/// Identifies one cleanup retry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CleanupTicket {
    transfer_id: Uuid,
    target_request_id: Uuid,
    pub terminal_id: Uuid,
    pub session_id: Option<Uuid>,
}

/// Artifact returned to the target through the helper.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransferArtifact {
    pub content: String,
    pub acknowledge: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct TransferFilter {
    pub id: Option<Uuid>,
    pub source: Option<Uuid>,
    pub target: Option<Uuid>,
}

#[derive(Clone)]
pub struct TransferCoordinator {
    inner: Arc<CoordinatorInner>,
}

struct CoordinatorInner {
    sessions: SessionRegistry,
    state: Mutex<CoordinatorState>,
}

#[derive(Default)]
struct CoordinatorState {
    transfers: HashMap<Uuid, TransferRecord>,
    ledger_records: usize,
    admission_disabled: bool,
}

struct TransferRecord {
    id: Uuid,
    revision: u64,
    state: TransferState,
    mode: TransferMode,
    source_terminal_id: Uuid,
    source_session_id: Option<Uuid>,
    source_claimed: bool,
    instruction: String,
    draft: Option<String>,
    draft_revision: u32,
    approved_revision: Option<u32>,
    fingerprint: Option<[u8; 32]>,
    preparation: Option<TransferPreparation>,
    preparation_attempt: Option<Uuid>,
    attempts: Vec<AttemptRecord>,
    delivery: Option<TransferDelivery>,
    error: Option<TransferErrorCode>,
    created_at: u64,
    updated_at: u64,
}

struct AttemptRecord {
    target_request_id: Uuid,
    number: u32,
    request: TargetRequest,
    target: TransferTargetSnapshot,
    outcome: AttemptOutcome,
    /// `true` once a start result (or bind) has been applied.
    completion: watch::Sender<bool>,
    abandon_after_start: bool,
}

impl AttemptRecord {
    fn is_settled(&self) -> bool {
        matches!(
            self.outcome,
            AttemptOutcome::StartFailed
                | AttemptOutcome::TargetEnded
                | AttemptOutcome::Delivered
                | AttemptOutcome::Abandoned
        )
    }
}

impl TransferRecord {
    fn snapshot(&self) -> TransferSnapshot {
        TransferSnapshot {
            transfer_id: self.id,
            revision: self.revision,
            state: self.state,
            mode: self.mode,
            source_terminal_id: self.source_terminal_id,
            source_session_id: self.source_session_id,
            draft_revision: self.draft_revision,
            approved_revision: self.approved_revision,
            preparation: self.preparation,
            attempt: self.attempts.last().map(|attempt| TransferAttemptSnapshot {
                target_request_id: attempt.target_request_id,
                number: attempt.number,
                target: attempt.target.clone(),
                outcome: attempt.outcome,
            }),
            attempts_used: self.attempts.len() as u32,
            delivery: self.delivery,
            error: self.error.map(|code| TransferErrorSnapshot { code }),
            created_at: self.created_at,
            updated_at: self.updated_at,
        }
    }

    fn touch(&mut self) {
        self.revision = self.revision.saturating_add(1);
        self.updated_at = unix_time_millis();
    }

    fn current_attempt(&self) -> Option<&AttemptRecord> {
        self.attempts.last().filter(|attempt| !attempt.is_settled())
    }

    fn current_attempt_mut(&mut self) -> Option<&mut AttemptRecord> {
        self.attempts
            .last_mut()
            .filter(|attempt| !attempt.is_settled())
    }

    fn bound_target(&self) -> Option<(Uuid, Option<Uuid>)> {
        self.current_attempt()
            .filter(|attempt| matches!(attempt.outcome, AttemptOutcome::Bound))
            .map(|attempt| (attempt.target.terminal_id, attempt.target.session_id))
    }

    fn enter_terminal(&mut self, state: TransferState, error: Option<TransferErrorCode>) {
        self.state = state;
        self.error = error;
        self.draft = None;
        self.fingerprint = None;
        self.touch();
    }
}

impl TransferCoordinator {
    pub fn new(peers: PeerBroker, sessions: SessionRegistry) -> Self {
        let coordinator = Self {
            inner: Arc::new(CoordinatorInner {
                sessions,
                state: Mutex::new(CoordinatorState::default()),
            }),
        };
        let observer = coordinator.clone();
        peers.set_revocation_observer(Arc::new(move |terminal_id, session_id| {
            observer.on_session_revoked(terminal_id, session_id);
        }));
        coordinator
    }

    /// Refuses new transfers and fails every non-terminal one. Must run after
    /// the peer broker disabled capability activation and before the loopback
    /// listener is released.
    pub fn begin_shutdown(&self) {
        let mut state = lock(&self.inner.state);
        state.admission_disabled = true;
        for transfer in state.transfers.values_mut() {
            if !transfer.state.is_terminal() {
                self.release_claims(transfer);
                transfer
                    .enter_terminal(TransferState::Failed, Some(TransferErrorCode::ShuttingDown));
            }
        }
    }

    // ----- browser API -----------------------------------------------------

    #[allow(clippy::too_many_arguments)]
    pub fn create(
        &self,
        transfer_id: Uuid,
        source_terminal_id: Uuid,
        source_session_id: Option<Uuid>,
        mode: TransferMode,
        source_ready: bool,
        instruction: String,
    ) -> Result<TransferSnapshot, TransferError> {
        let instruction = validate_instruction(instruction)?;
        let mut state = lock(&self.inner.state);
        if state.admission_disabled {
            return Err(TransferError::new(
                TransferErrorCode::ShuttingDown,
                "the server is shutting down",
            ));
        }
        if let Some(existing) = state.transfers.get(&transfer_id) {
            if existing.source_terminal_id == source_terminal_id
                && existing.mode == mode
                && existing.instruction == instruction
            {
                return Ok(existing.snapshot());
            }
            return Err(TransferError::new(
                TransferErrorCode::TransferConflict,
                "the transfer identifier was already used with different parameters",
            ));
        }
        if state
            .transfers
            .values()
            .filter(|transfer| !transfer.state.is_terminal())
            .count()
            >= MAX_ACTIVE_TRANSFERS
        {
            return Err(TransferError::new(
                TransferErrorCode::TransferLimitReached,
                "too many transfers are active; finish or abandon one first",
            ));
        }
        if state.ledger_records >= MAX_TRANSFER_LEDGER_RECORDS {
            return Err(TransferError::new(
                TransferErrorCode::TransferHistoryFull,
                "the transfer history is full for this server generation",
            ));
        }

        let source = self.inner.sessions.get(source_terminal_id).ok_or_else(|| {
            TransferError::new(
                TransferErrorCode::InvalidParticipant,
                "the source terminal was not found",
            )
        })?;
        if !matches!(source.purpose(), SessionPurpose::Interactive) {
            return Err(TransferError::new(
                TransferErrorCode::InvalidParticipant,
                "a dedicated peer reviewer cannot be a transfer source",
            ));
        }

        let now = unix_time_millis();
        let mut transfer = TransferRecord {
            id: transfer_id,
            revision: 1,
            state: TransferState::Preview,
            mode,
            source_terminal_id,
            source_session_id,
            source_claimed: false,
            instruction,
            draft: None,
            draft_revision: 0,
            approved_revision: None,
            fingerprint: None,
            preparation: None,
            preparation_attempt: None,
            attempts: Vec::new(),
            delivery: None,
            error: None,
            created_at: now,
            updated_at: now,
        };

        if mode == TransferMode::Prepare {
            if !source_ready {
                return Err(TransferError::new(
                    TransferErrorCode::InvalidInput,
                    "automated preparation requires confirming the source is at an empty prompt",
                ));
            }
            let Some(expected_session_id) = source_session_id else {
                return Err(TransferError::new(
                    TransferErrorCode::SourceNotRunning,
                    "automated preparation needs the running source generation",
                ));
            };
            self.inner
                .sessions
                .claim_transfer_participant(
                    source_terminal_id,
                    TransferClaim {
                        transfer_id,
                        role: TransferRole::Source,
                    },
                    Some(expected_session_id),
                )
                .map_err(claim_error_for_source)?;
            transfer.source_claimed = true;
            let attempt_id = Uuid::new_v4();
            let prompt = preparation_prompt(transfer_id, attempt_id, &transfer.instruction);
            match source.write_automation_prompt(expected_session_id, &prompt) {
                Ok(()) => {
                    transfer.state = TransferState::Preparing;
                    transfer.preparation = Some(TransferPreparation::Pending);
                    transfer.preparation_attempt = Some(attempt_id);
                }
                Err(_) => {
                    self.release_claims(&mut transfer);
                    transfer.state = TransferState::Preview;
                    transfer.preparation = Some(TransferPreparation::QueueRejected);
                    transfer.error = Some(TransferErrorCode::QueueRejected);
                }
            }
        }

        let snapshot = transfer.snapshot();
        state.ledger_records = state.ledger_records.saturating_add(1);
        state.transfers.insert(transfer_id, transfer);
        Ok(snapshot)
    }

    pub fn get(&self, transfer_id: Uuid) -> Result<TransferSnapshot, TransferError> {
        let state = lock(&self.inner.state);
        state
            .transfers
            .get(&transfer_id)
            .map(TransferRecord::snapshot)
            .ok_or_else(not_found)
    }

    pub fn list(&self, filter: &TransferFilter) -> Vec<TransferSnapshot> {
        let state = lock(&self.inner.state);
        let mut snapshots: Vec<_> = state
            .transfers
            .values()
            .filter(|transfer| filter.id.is_none_or(|id| transfer.id == id))
            .filter(|transfer| {
                filter
                    .source
                    .is_none_or(|source| transfer.source_terminal_id == source)
            })
            .filter(|transfer| {
                filter.target.is_none_or(|target| {
                    transfer
                        .attempts
                        .iter()
                        .any(|attempt| attempt.target.terminal_id == target)
                })
            })
            .map(TransferRecord::snapshot)
            .collect();
        snapshots.sort_by(|left, right| {
            left.created_at.cmp(&right.created_at).then_with(|| {
                left.transfer_id
                    .as_bytes()
                    .cmp(right.transfer_id.as_bytes())
            })
        });
        snapshots
    }

    /// Draft content, available only while the transfer is not terminal.
    pub fn draft(&self, transfer_id: Uuid) -> Result<(u32, Option<String>), TransferError> {
        let state = lock(&self.inner.state);
        let transfer = state.transfers.get(&transfer_id).ok_or_else(not_found)?;
        Ok((transfer.draft_revision, transfer.draft.clone()))
    }

    /// Manual entry or edit. Allowed in `Preparing` (which overrides the
    /// pending automated preparation), `Preview`, and `TargetBound`.
    pub fn edit_draft(
        &self,
        transfer_id: Uuid,
        expected_revision: u32,
        content: String,
    ) -> Result<TransferSnapshot, TransferError> {
        let content = validate_artifact(content)?;
        let mut state = lock(&self.inner.state);
        let transfer = state
            .transfers
            .get_mut(&transfer_id)
            .ok_or_else(not_found)?;
        match transfer.state {
            TransferState::Preparing | TransferState::Preview | TransferState::TargetBound => {}
            _ => return Err(invalid_state("the draft cannot be edited in this state")),
        }
        if transfer.draft_revision != expected_revision {
            return Err(TransferError::new(
                TransferErrorCode::RevisionConflict,
                "the draft changed since it was last read",
            ));
        }
        if transfer.state == TransferState::Preparing {
            // Manual override: withdraw acceptance of the pending attempt. The
            // queued prompt bytes cannot be recalled; a late submission for
            // that attempt is rejected.
            transfer.preparation_attempt = None;
            transfer.preparation = Some(TransferPreparation::Overridden);
            transfer.state = TransferState::Preview;
            self.release_claims(transfer);
        }
        transfer.fingerprint = Some(fingerprint(&content));
        transfer.draft = Some(content);
        transfer.draft_revision = transfer.draft_revision.saturating_add(1);
        transfer.error = None;
        transfer.touch();
        Ok(transfer.snapshot())
    }

    /// Admits a target request. For a new target the session is reserved and
    /// bound in the registry's critical section while this coordinator lock is
    /// held, so it is listed, user-owned, and lifecycle-protected before any
    /// other request can observe the attempt.
    pub fn begin_target(
        &self,
        transfer_id: Uuid,
        target_request_id: Uuid,
        request: TargetRequest,
    ) -> Result<TargetAdmission, TransferError> {
        let mut state = lock(&self.inner.state);
        if state.admission_disabled {
            return Err(TransferError::new(
                TransferErrorCode::ShuttingDown,
                "the server is shutting down",
            ));
        }
        let ledger_records = state.ledger_records;
        let transfer = state
            .transfers
            .get_mut(&transfer_id)
            .ok_or_else(not_found)?;

        // Idempotent repeat of an existing attempt.
        if let Some(attempt) = transfer
            .attempts
            .iter()
            .find(|attempt| attempt.target_request_id == target_request_id)
        {
            if attempt.request != request {
                return Err(TransferError::new(
                    TransferErrorCode::TransferConflict,
                    "the target request identifier was already used with different parameters",
                ));
            }
            if attempt.outcome == AttemptOutcome::Starting {
                return Ok(TargetAdmission::Wait(attempt.completion.subscribe()));
            }
            return Ok(TargetAdmission::Settled(transfer.snapshot()));
        }

        if transfer
            .current_attempt()
            .is_some_and(|attempt| attempt.outcome == AttemptOutcome::Starting)
        {
            return Err(TransferError::new(
                TransferErrorCode::AttemptPending,
                "the previous target attempt has not settled",
            ));
        }
        if transfer.state != TransferState::Preview {
            return Err(invalid_state(
                "a target can be chosen only while the handoff is in preview",
            ));
        }
        if transfer.draft.is_none() {
            return Err(TransferError::new(
                TransferErrorCode::InvalidState,
                "write or submit the handoff before choosing a target",
            ));
        }
        if transfer.current_attempt().is_some() {
            return Err(TransferError::new(
                TransferErrorCode::AttemptPending,
                "the previous target attempt has not settled",
            ));
        }
        if transfer.attempts.len() >= MAX_TRANSFER_ATTEMPTS {
            return Err(TransferError::new(
                TransferErrorCode::AttemptLimitReached,
                "this transfer has used all of its target attempts",
            ));
        }
        if ledger_records >= MAX_TRANSFER_LEDGER_RECORDS {
            return Err(TransferError::new(
                TransferErrorCode::TransferHistoryFull,
                "the transfer history is full for this server generation",
            ));
        }
        if transfer.source_terminal_id
            == match &request {
                TargetRequest::Existing { terminal_id, .. } => *terminal_id,
                TargetRequest::New { .. } => Uuid::nil(),
            }
        {
            return Err(TransferError::new(
                TransferErrorCode::TargetNotEligible,
                "the source cannot be its own transfer target",
            ));
        }

        let number = transfer.attempts.len() as u32 + 1;
        let (completion, _) = watch::channel(false);
        match request.clone() {
            TargetRequest::New {
                agent,
                project_dir,
                directory_id: _,
            } => {
                let terminal_id = Uuid::new_v4();
                let session = self
                    .inner
                    .sessions
                    .reserve_transfer_target(agent, project_dir, terminal_id, transfer_id)
                    .map_err(reserve_error)?;
                let snapshot = session.snapshot();
                transfer.attempts.push(AttemptRecord {
                    target_request_id,
                    number,
                    request,
                    target: target_snapshot(TargetMode::New, &snapshot),
                    outcome: AttemptOutcome::Starting,
                    completion,
                    abandon_after_start: false,
                });
                transfer.state = TransferState::Starting;
                transfer.error = None;
                transfer.touch();
                state.ledger_records = state.ledger_records.saturating_add(1);
                let snapshot = state.transfers[&transfer_id].snapshot();
                Ok(TargetAdmission::Start {
                    session,
                    token: StartToken {
                        transfer_id,
                        target_request_id,
                        terminal_id,
                    },
                    snapshot,
                })
            }
            TargetRequest::Existing {
                terminal_id,
                session_id,
            } => {
                let observed = self
                    .inner
                    .sessions
                    .claim_transfer_participant(
                        terminal_id,
                        TransferClaim {
                            transfer_id,
                            role: TransferRole::Target,
                        },
                        Some(session_id),
                    )
                    .map_err(claim_error_for_target)?;
                let _ = completion.send_replace(true);
                transfer.attempts.push(AttemptRecord {
                    target_request_id,
                    number,
                    request,
                    target: target_snapshot(TargetMode::Existing, &observed),
                    outcome: AttemptOutcome::Bound,
                    completion,
                    abandon_after_start: false,
                });
                transfer.state = TransferState::TargetBound;
                transfer.error = None;
                transfer.touch();
                state.ledger_records = state.ledger_records.saturating_add(1);
                Ok(TargetAdmission::Settled(
                    state.transfers[&transfer_id].snapshot(),
                ))
            }
        }
    }

    /// Applies the outcome of starting a reserved target. Verified against the
    /// exact attempt and terminal; stale or foreign completions are ignored.
    pub fn complete_target_start(
        &self,
        token: StartToken,
        result: Result<SessionSnapshot, RegistryError>,
    ) -> Result<TransferSnapshot, TransferError> {
        let mut state = lock(&self.inner.state);
        let transfer = state
            .transfers
            .get_mut(&token.transfer_id)
            .ok_or_else(not_found)?;
        let Some(attempt) = transfer.attempts.iter_mut().find(|attempt| {
            attempt.target_request_id == token.target_request_id
                && attempt.target.terminal_id == token.terminal_id
                && attempt.outcome == AttemptOutcome::Starting
        }) else {
            return Ok(transfer.snapshot());
        };
        let abandon = attempt.abandon_after_start;
        // Starting owns a registry claim, but the child may already have
        // exited while the caller was awaiting startup or workspace storage.
        // Read its current generation under the coordinator lock. Exit marks
        // the session record first, then notifies this coordinator, so an exit
        // after this read will settle the Bound attempt below.
        let result = result.map(|started| {
            self.inner
                .sessions
                .get(token.terminal_id)
                .map(|session| session.snapshot())
                .filter(|current| current.session_id == started.session_id)
        });
        match result {
            Ok(Some(snapshot)) if snapshot.status == Lifecycle::Running => {
                attempt.target.session_id = snapshot.session_id;
                attempt.target.name = snapshot.name;
                attempt.target.project = snapshot.project;
                if abandon {
                    attempt.outcome = AttemptOutcome::Abandoned;
                    let _ = attempt.completion.send_replace(true);
                    self.release_target_claim(token.terminal_id, token.transfer_id);
                    transfer.enter_terminal(TransferState::Abandoned, None);
                } else {
                    attempt.outcome = AttemptOutcome::Bound;
                    let _ = attempt.completion.send_replace(true);
                    transfer.state = TransferState::TargetBound;
                    transfer.touch();
                }
            }
            Ok(_) => {
                // Started but already gone: settle as a target that ended.
                attempt.outcome = AttemptOutcome::TargetEnded;
                let _ = attempt.completion.send_replace(true);
                self.release_target_claim(token.terminal_id, token.transfer_id);
                if abandon {
                    transfer.enter_terminal(TransferState::Abandoned, None);
                } else {
                    transfer.state = TransferState::Preview;
                    transfer.delivery = Some(TransferDelivery::TargetEnded);
                    transfer.error = Some(TransferErrorCode::TargetEnded);
                    transfer.touch();
                }
            }
            Err(RegistryError::StartRollbackFailed(_)) => {
                // Reservation has no generation yet. A failed startup has
                // assigned one; cleanup must name that exact retained child.
                if let Some(session) = self.inner.sessions.get(token.terminal_id) {
                    attempt.target = target_snapshot(TargetMode::New, &session.snapshot());
                }
                attempt.outcome = AttemptOutcome::CleanupPending;
                let _ = attempt.completion.send_replace(true);
                transfer.state = TransferState::CleanupPending;
                transfer.error = Some(TransferErrorCode::CleanupPending);
                transfer.touch();
            }
            Err(_) => {
                attempt.outcome = AttemptOutcome::StartFailed;
                let _ = attempt.completion.send_replace(true);
                // The registry removed the entry and its claim.
                if abandon {
                    transfer.enter_terminal(TransferState::Abandoned, None);
                } else {
                    transfer.state = TransferState::Preview;
                    transfer.error = Some(TransferErrorCode::TargetStartFailed);
                    transfer.touch();
                }
            }
        }
        Ok(transfer.snapshot())
    }

    /// Issues a cleanup retry for a target whose start rollback was not
    /// confirmed. The caller runs the registry cleanup and reports back.
    pub fn begin_cleanup(
        &self,
        transfer_id: Uuid,
        target_terminal_id: Uuid,
        target_session_id: Option<Uuid>,
    ) -> Result<CleanupTicket, TransferError> {
        let state = lock(&self.inner.state);
        let transfer = state.transfers.get(&transfer_id).ok_or_else(not_found)?;
        if transfer.state != TransferState::CleanupPending {
            return Err(invalid_state("no cleanup is pending for this transfer"));
        }
        let attempt = transfer
            .attempts
            .last()
            .filter(|attempt| attempt.outcome == AttemptOutcome::CleanupPending)
            .ok_or_else(|| invalid_state("no cleanup is pending for this transfer"))?;
        if attempt.target.terminal_id != target_terminal_id
            || attempt.target.session_id != target_session_id
        {
            return Err(TransferError::new(
                TransferErrorCode::GenerationConflict,
                "the cleanup request does not name the pending target exactly",
            ));
        }
        Ok(CleanupTicket {
            transfer_id,
            target_request_id: attempt.target_request_id,
            terminal_id: attempt.target.terminal_id,
            session_id: attempt.target.session_id,
        })
    }

    pub fn complete_cleanup(
        &self,
        ticket: CleanupTicket,
        result: Result<(), RegistryError>,
    ) -> Result<TransferSnapshot, TransferError> {
        let mut state = lock(&self.inner.state);
        let transfer = state
            .transfers
            .get_mut(&ticket.transfer_id)
            .ok_or_else(not_found)?;
        let Some(attempt) = transfer.attempts.iter_mut().find(|attempt| {
            attempt.target_request_id == ticket.target_request_id
                && attempt.outcome == AttemptOutcome::CleanupPending
        }) else {
            return Ok(transfer.snapshot());
        };
        match result {
            Ok(()) => {
                attempt.outcome = AttemptOutcome::StartFailed;
                transfer.state = TransferState::Preview;
                transfer.error = Some(TransferErrorCode::TargetStartFailed);
                transfer.touch();
                Ok(transfer.snapshot())
            }
            Err(_) => {
                transfer.error = Some(TransferErrorCode::CleanupFailed);
                transfer.touch();
                Err(TransferError::new(
                    TransferErrorCode::CleanupFailed,
                    "the target process could not be confirmed terminated; retry cleanup",
                ))
            }
        }
    }

    /// Explicit, readiness-confirmed delivery. Freezes the approved revision
    /// and enqueues the prompt synchronously inside this critical section.
    pub fn deliver(
        &self,
        transfer_id: Uuid,
        expected_revision: u32,
        target_session_id: Uuid,
        target_ready: bool,
    ) -> Result<TransferSnapshot, TransferError> {
        if !target_ready {
            return Err(TransferError::new(
                TransferErrorCode::InvalidInput,
                "delivery requires confirming the target is at an empty prompt",
            ));
        }
        let mut state = lock(&self.inner.state);
        let transfer = state
            .transfers
            .get_mut(&transfer_id)
            .ok_or_else(not_found)?;
        if transfer.state != TransferState::TargetBound {
            return Err(invalid_state("no target is bound and ready for delivery"));
        }
        if transfer.draft_revision != expected_revision || transfer.draft.is_none() {
            return Err(TransferError::new(
                TransferErrorCode::RevisionConflict,
                "the draft changed since it was last read",
            ));
        }
        let (target_terminal_id, bound_session_id) = transfer
            .bound_target()
            .ok_or_else(|| invalid_state("no target is bound"))?;
        if bound_session_id != Some(target_session_id) {
            return Err(TransferError::new(
                TransferErrorCode::GenerationConflict,
                "the target session changed since it was bound",
            ));
        }
        let target = self.inner.sessions.get(target_terminal_id).ok_or_else(|| {
            TransferError::new(
                TransferErrorCode::TargetEnded,
                "the target terminal no longer exists",
            )
        })?;
        let prompt = delivery_prompt(transfer_id);
        match target.write_automation_prompt(target_session_id, &prompt) {
            Ok(()) => {
                transfer.approved_revision = Some(transfer.draft_revision);
                transfer.state = TransferState::Delivering;
                transfer.delivery = Some(TransferDelivery::Unconfirmed);
                transfer.error = None;
                transfer.touch();
                Ok(transfer.snapshot())
            }
            Err(_) => {
                transfer.error = Some(TransferErrorCode::QueueRejected);
                transfer.touch();
                Err(TransferError::new(
                    TransferErrorCode::QueueRejected,
                    "the target did not accept the delivery prompt; it was not sent",
                ))
            }
        }
    }

    /// Explicit user abandonment. Deferred while a target start is in flight;
    /// from `Delivering`/`AwaitingReceipt` it is a discard of the queued but
    /// unrecallable delivery.
    pub fn abandon(&self, transfer_id: Uuid) -> Result<TransferSnapshot, TransferError> {
        let mut state = lock(&self.inner.state);
        let transfer = state
            .transfers
            .get_mut(&transfer_id)
            .ok_or_else(not_found)?;
        match transfer.state {
            TransferState::Abandoned => return Ok(transfer.snapshot()),
            TransferState::Delivered | TransferState::Failed => {
                return Err(invalid_state(
                    "the transfer already finished with another outcome",
                ));
            }
            TransferState::Starting => {
                if let Some(attempt) = transfer.current_attempt_mut() {
                    attempt.abandon_after_start = true;
                }
                transfer.touch();
                return Ok(transfer.snapshot());
            }
            TransferState::CleanupPending => {
                // The failed session stays visible and user-owned; ordinary
                // deletion applies once the claim is gone.
                if let Some(attempt) = transfer.current_attempt_mut() {
                    attempt.outcome = AttemptOutcome::Abandoned;
                    let terminal_id = attempt.target.terminal_id;
                    self.inner
                        .sessions
                        .release_transfer_participant(terminal_id, transfer_id);
                }
            }
            TransferState::Preparing
            | TransferState::Preview
            | TransferState::TargetBound
            | TransferState::Delivering
            | TransferState::AwaitingReceipt => {
                if let Some(attempt) = transfer.current_attempt_mut() {
                    attempt.outcome = AttemptOutcome::Abandoned;
                }
                transfer.preparation_attempt = None;
                self.release_claims(transfer);
            }
        }
        transfer.enter_terminal(TransferState::Abandoned, None);
        Ok(transfer.snapshot())
    }

    // ----- helper (loopback) API ------------------------------------------

    pub fn submit(
        &self,
        subject: &CapabilitySubject,
        transfer_id: Uuid,
        attempt_id: Uuid,
        content: String,
    ) -> Result<TransferSnapshot, TransferError> {
        let content = validate_artifact(content)?;
        let digest = fingerprint(&content);
        let mut state = lock(&self.inner.state);
        let transfer = state
            .transfers
            .get_mut(&transfer_id)
            .ok_or_else(unauthorized)?;
        if transfer.source_terminal_id != subject.terminal_id()
            || transfer.source_session_id != Some(subject.session_id())
            || subject.purpose() != &SessionPurpose::Interactive
        {
            return Err(unauthorized());
        }
        if transfer.state != TransferState::Preparing
            || transfer.preparation_attempt != Some(attempt_id)
        {
            if transfer.fingerprint == Some(digest)
                && transfer.preparation == Some(TransferPreparation::Submitted)
            {
                // Duplicate of the accepted submission: idempotent.
                return Ok(transfer.snapshot());
            }
            return Err(TransferError::new(
                TransferErrorCode::AttemptInvalidated,
                "this preparation attempt is no longer accepted",
            ));
        }
        transfer.draft = Some(content);
        transfer.fingerprint = Some(digest);
        transfer.draft_revision = transfer.draft_revision.saturating_add(1);
        transfer.preparation = Some(TransferPreparation::Submitted);
        transfer.state = TransferState::Preview;
        transfer.error = None;
        self.release_claims(transfer);
        transfer.touch();
        Ok(transfer.snapshot())
    }

    pub fn receive(
        &self,
        subject: &CapabilitySubject,
        transfer_id: Uuid,
    ) -> Result<TransferArtifact, TransferError> {
        let mut state = lock(&self.inner.state);
        let transfer = state
            .transfers
            .get_mut(&transfer_id)
            .ok_or_else(unauthorized)?;
        authorize_target(transfer, subject)?;
        match transfer.state {
            TransferState::Delivering => {
                transfer.state = TransferState::AwaitingReceipt;
                transfer.touch();
            }
            TransferState::AwaitingReceipt => {}
            TransferState::Delivered => {
                return Err(TransferError::new(
                    TransferErrorCode::AlreadyDelivered,
                    "the transferred context was already acknowledged",
                ));
            }
            TransferState::Abandoned => {
                return Err(TransferError::new(
                    TransferErrorCode::TransferDiscarded,
                    "the transfer was discarded",
                ));
            }
            _ => return Err(invalid_state("no delivery is pending for this target")),
        }
        let content = transfer
            .draft
            .clone()
            .ok_or_else(|| invalid_state("the transferred context is no longer available"))?;
        Ok(TransferArtifact {
            content,
            acknowledge: true,
        })
    }

    pub fn acknowledge(
        &self,
        subject: &CapabilitySubject,
        transfer_id: Uuid,
    ) -> Result<TransferSnapshot, TransferError> {
        let mut state = lock(&self.inner.state);
        let transfer = state
            .transfers
            .get_mut(&transfer_id)
            .ok_or_else(unauthorized)?;
        authorize_target(transfer, subject)?;
        match transfer.state {
            TransferState::AwaitingReceipt => {
                if let Some(attempt) = transfer.current_attempt_mut() {
                    attempt.outcome = AttemptOutcome::Delivered;
                }
                transfer.delivery = None;
                self.release_claims(transfer);
                transfer.enter_terminal(TransferState::Delivered, None);
                Ok(transfer.snapshot())
            }
            TransferState::Delivered => Ok(transfer.snapshot()),
            TransferState::Abandoned => Err(TransferError::new(
                TransferErrorCode::TransferDiscarded,
                "the transfer was discarded",
            )),
            _ => Err(invalid_state(
                "the transferred context has not been retrieved yet",
            )),
        }
    }

    pub fn status(
        &self,
        subject: &CapabilitySubject,
        transfer_id: Uuid,
    ) -> Result<TransferState, TransferError> {
        let state = lock(&self.inner.state);
        let transfer = state.transfers.get(&transfer_id).ok_or_else(unauthorized)?;
        let is_source = transfer.source_terminal_id == subject.terminal_id()
            && transfer.source_session_id == Some(subject.session_id());
        if !is_source && authorize_target(transfer, subject).is_err() {
            return Err(unauthorized());
        }
        Ok(transfer.state)
    }

    // ----- generation observer ---------------------------------------------

    /// Called after a PTY generation lost its capability. Settles the affected
    /// source preparation or target attempt without erasing the draft.
    pub fn on_session_revoked(&self, terminal_id: Uuid, session_id: Uuid) {
        let mut state = lock(&self.inner.state);
        for transfer in state.transfers.values_mut() {
            if transfer.state.is_terminal() {
                continue;
            }
            if transfer.state == TransferState::Preparing
                && transfer.source_terminal_id == terminal_id
                && transfer.source_session_id == Some(session_id)
            {
                transfer.preparation_attempt = None;
                transfer.preparation = Some(TransferPreparation::SourceEnded);
                transfer.state = TransferState::Preview;
                transfer.error = Some(TransferErrorCode::SourceEnded);
                self.release_claims(transfer);
                transfer.touch();
                continue;
            }
            let ended_target = transfer.current_attempt().is_some_and(|attempt| {
                attempt.target.terminal_id == terminal_id
                    && attempt.target.session_id == Some(session_id)
                    && matches!(attempt.outcome, AttemptOutcome::Bound)
            });
            if ended_target {
                let transfer_id = transfer.id;
                if let Some(attempt) = transfer.current_attempt_mut() {
                    attempt.outcome = AttemptOutcome::TargetEnded;
                }
                self.inner
                    .sessions
                    .release_transfer_participant(terminal_id, transfer_id);
                transfer.state = TransferState::Preview;
                transfer.approved_revision = None;
                transfer.delivery = Some(TransferDelivery::TargetEnded);
                transfer.error = Some(TransferErrorCode::TargetEnded);
                transfer.touch();
            }
        }
    }

    // ----- internals -------------------------------------------------------

    fn release_claims(&self, transfer: &mut TransferRecord) {
        if transfer.source_claimed {
            self.inner
                .sessions
                .release_transfer_participant(transfer.source_terminal_id, transfer.id);
            transfer.source_claimed = false;
        }
        if let Some(attempt) = transfer.attempts.last()
            && matches!(
                attempt.outcome,
                AttemptOutcome::Bound
                    | AttemptOutcome::Delivered
                    | AttemptOutcome::Abandoned
                    | AttemptOutcome::TargetEnded
            )
        {
            self.inner
                .sessions
                .release_transfer_participant(attempt.target.terminal_id, transfer.id);
        }
    }

    fn release_target_claim(&self, terminal_id: Uuid, transfer_id: Uuid) {
        self.inner
            .sessions
            .release_transfer_participant(terminal_id, transfer_id);
    }
}

fn authorize_target(
    transfer: &TransferRecord,
    subject: &CapabilitySubject,
) -> Result<(), TransferError> {
    let bound = transfer
        .current_attempt()
        .or_else(|| transfer.attempts.last())
        .filter(|attempt| {
            attempt.target.terminal_id == subject.terminal_id()
                && attempt.target.session_id == Some(subject.session_id())
        });
    if bound.is_none() || subject.purpose() != &SessionPurpose::Interactive {
        return Err(unauthorized());
    }
    Ok(())
}

fn target_snapshot(mode: TargetMode, snapshot: &SessionSnapshot) -> TransferTargetSnapshot {
    TransferTargetSnapshot {
        mode,
        terminal_id: snapshot.terminal_id,
        session_id: snapshot.session_id,
        agent: snapshot.agent,
        name: snapshot.name.clone(),
        project: snapshot.project.clone(),
    }
}

fn claim_error_for_source(error: RegistryError) -> TransferError {
    match error {
        RegistryError::NotFound => TransferError::new(
            TransferErrorCode::InvalidParticipant,
            "the source terminal was not found",
        ),
        RegistryError::PeerSessionManaged => TransferError::new(
            TransferErrorCode::InvalidParticipant,
            "a dedicated peer reviewer cannot be a transfer source",
        ),
        RegistryError::ParticipantBusy | RegistryError::PeerSessionsActive => TransferError::new(
            TransferErrorCode::ParticipantBusy,
            "the source is busy with a peer thread or another transfer",
        ),
        RegistryError::InvalidPeerParent => TransferError::new(
            TransferErrorCode::SourceNotRunning,
            "the source is not running with the stated session",
        ),
        RegistryError::ShuttingDown => TransferError::new(
            TransferErrorCode::ShuttingDown,
            "the server is shutting down",
        ),
        _ => TransferError::new(
            TransferErrorCode::InvalidState,
            "the source could not be claimed",
        ),
    }
}

fn claim_error_for_target(error: RegistryError) -> TransferError {
    match error {
        RegistryError::NotFound => TransferError::new(
            TransferErrorCode::TargetNotEligible,
            "the target terminal was not found",
        ),
        RegistryError::PeerSessionManaged => TransferError::new(
            TransferErrorCode::TargetNotEligible,
            "a dedicated peer reviewer cannot be a transfer target",
        ),
        RegistryError::ParticipantBusy | RegistryError::PeerSessionsActive => TransferError::new(
            TransferErrorCode::ParticipantBusy,
            "the target is busy with a peer thread or another transfer",
        ),
        RegistryError::TransferTargetNotEligible => TransferError::new(
            TransferErrorCode::GenerationConflict,
            "the target is not running with the stated session",
        ),
        RegistryError::ShuttingDown => TransferError::new(
            TransferErrorCode::ShuttingDown,
            "the server is shutting down",
        ),
        _ => TransferError::new(
            TransferErrorCode::TargetNotEligible,
            "the target could not be claimed",
        ),
    }
}

fn reserve_error(error: RegistryError) -> TransferError {
    match error {
        RegistryError::LimitReached => TransferError::new(
            TransferErrorCode::CapacityFull,
            "the terminal session limit has been reached",
        ),
        RegistryError::AgentUnavailable => TransferError::new(
            TransferErrorCode::AgentUnavailable,
            "the requested agent is not ready",
        ),
        RegistryError::ShuttingDown => TransferError::new(
            TransferErrorCode::ShuttingDown,
            "the server is shutting down",
        ),
        _ => TransferError::new(
            TransferErrorCode::TargetStartFailed,
            "the target could not be reserved",
        ),
    }
}

fn preparation_prompt(transfer_id: Uuid, attempt_id: Uuid, instruction: &str) -> String {
    format!(
        "[CWT transfer] Prepare a concise, self-contained Markdown handoff so another coding agent can continue this work in a fresh session. \
Use your current conversation context; include the user goal, decisions taken, relevant paths or files, completed work and tests, open items and risks, and this instruction: {}. \
Do not merely print the handoff. Submit it through the local CWT bridge by running the executable in CWT_PEER_HELPER with \
`__cwt-peer transfer-submit --transfer {transfer_id} --attempt {attempt_id} --stdin` and piping the Markdown to standard input, or replace `--stdin` with `--file PATH`. \
Never print CWT_PEER_CAPABILITY.",
        compact_for_prompt(instruction)
    )
}

fn delivery_prompt(transfer_id: Uuid) -> String {
    format!(
        "[CWT transfer] Context transferred from another agent session is ready. Retrieve it by running the executable in CWT_PEER_HELPER with \
`__cwt-peer transfer-receive --transfer {transfer_id}`. Treat it as transferred context, not verified fact: inspect the workspace before acting, \
then continue the work it describes and confirm with the user before any destructive action. Never print CWT_PEER_CAPABILITY."
    )
}

fn fingerprint(content: &str) -> [u8; 32] {
    Sha256::digest(content.as_bytes()).into()
}

fn not_found() -> TransferError {
    TransferError::new(TransferErrorCode::TransferNotFound, "transfer not found")
}

fn unauthorized() -> TransferError {
    TransferError::new(
        TransferErrorCode::Unauthorized,
        "this capability is not authorized for the transfer",
    )
}

fn invalid_state(message: &'static str) -> TransferError {
    TransferError::new(TransferErrorCode::InvalidState, message)
}

fn unix_time_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or_default()
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
mod tests {
    use std::{
        net::SocketAddr,
        path::{Path, PathBuf},
        time::{Duration, Instant},
    };

    use super::*;
    use crate::{config::ShellKind, terminal::TerminalConfig};

    fn broker() -> PeerBroker {
        PeerBroker::new(
            "127.0.0.1:43123"
                .parse::<SocketAddr>()
                .expect("loopback peer endpoint"),
            PathBuf::from("codex-web"),
        )
    }

    fn terminal_config(project_dir: &Path, command: &Path) -> TerminalConfig {
        TerminalConfig {
            project_dir: project_dir.to_path_buf(),
            command: command.to_string_lossy().into_owned(),
            arguments: Vec::new(),
            agent: AgentKind::Codex,
            shell: ShellKind::Powershell,
        }
    }

    struct Fixture {
        _directory: tempfile::TempDir,
        project_dir: PathBuf,
        peers: PeerBroker,
        sessions: SessionRegistry,
        transfers: TransferCoordinator,
    }

    async fn fixture() -> Fixture {
        let directory = tempfile::tempdir().expect("temporary project");
        let command = write_long_running_fixture(directory.path());
        let project_dir = dunce::canonicalize(directory.path()).expect("canonical project");
        let config = terminal_config(&project_dir, &command);
        let peers = broker();
        let sessions = SessionRegistry::with_agent_configs_and_peer_broker(
            config.clone(),
            config,
            Vec::new(),
            peers.clone(),
        );
        sessions.start_primary().await.expect("start primary");
        wait_for_output(&sessions.primary(), "WORKSPACE-READY").await;
        let transfers = TransferCoordinator::new(peers.clone(), sessions.clone());
        Fixture {
            _directory: directory,
            project_dir,
            peers,
            sessions,
            transfers,
        }
    }

    fn subject_for(peers: &PeerBroker, session: &SessionManager) -> CapabilitySubject {
        let snapshot = session.snapshot();
        let capability = peers
            .capability_for_test(
                snapshot.terminal_id,
                snapshot.session_id.expect("running generation"),
            )
            .expect("capability for running generation");
        peers
            .authenticate_capability(&capability)
            .expect("authenticate fixture capability")
    }

    #[tokio::test]
    async fn exit_before_start_completion_settles_the_attempt_and_preserves_the_draft() {
        let fixture = fixture().await;
        let source = fixture.sessions.primary().snapshot();
        let id = Uuid::new_v4();
        fixture
            .transfers
            .create(
                id,
                source.terminal_id,
                source.session_id,
                TransferMode::Manual,
                false,
                "Continue.".to_owned(),
            )
            .expect("create");
        fixture
            .transfers
            .edit_draft(id, 0, "# Keep this draft".to_owned())
            .expect("draft");
        let TargetAdmission::Start { session, token, .. } = fixture
            .transfers
            .begin_target(
                id,
                Uuid::new_v4(),
                TargetRequest::New {
                    agent: AgentKind::Codex,
                    project_dir: fixture.project_dir.clone(),
                    directory_id: "synthetic".to_owned(),
                },
            )
            .expect("reserve")
        else {
            panic!("new target")
        };
        let started = fixture
            .sessions
            .start_transfer_target(session.clone())
            .await
            .expect("start");
        assert_eq!(started.status, Lifecycle::Running);
        // Deterministically reproduce an exit while the route holds its old
        // Running snapshot, before the coordinator has bound the generation.
        session.terminate().await.expect("exit before commit");
        let after = fixture
            .transfers
            .complete_target_start(token, Ok(started))
            .expect("commit");
        assert_eq!(after.state, TransferState::Preview);
        assert_eq!(after.delivery, Some(TransferDelivery::TargetEnded));
        assert_eq!(
            after.attempt.expect("attempt").outcome,
            AttemptOutcome::TargetEnded
        );
        assert_eq!(
            fixture.transfers.draft(id).expect("draft").1.as_deref(),
            Some("# Keep this draft")
        );
        assert!(
            fixture
                .sessions
                .transfer_claim(token.terminal_id())
                .is_none()
        );
        fixture
            .sessions
            .delete(token.terminal_id())
            .await
            .expect("removable target");
        fixture.transfers.begin_shutdown();
        fixture.sessions.shutdown().await;
    }

    #[tokio::test]
    async fn cleanup_pending_carries_the_started_generation_and_retries_failed_termination() {
        let fixture = fixture().await;
        let source = fixture.sessions.primary().snapshot();
        let id = Uuid::new_v4();
        fixture
            .transfers
            .create(
                id,
                source.terminal_id,
                source.session_id,
                TransferMode::Manual,
                false,
                "Continue.".to_owned(),
            )
            .expect("create");
        fixture
            .transfers
            .edit_draft(id, 0, "# Retained draft".to_owned())
            .expect("draft");
        let TargetAdmission::Start { session, token, .. } = fixture
            .transfers
            .begin_target(
                id,
                Uuid::new_v4(),
                TargetRequest::New {
                    agent: AgentKind::Codex,
                    project_dir: fixture.project_dir.clone(),
                    directory_id: "synthetic".to_owned(),
                },
            )
            .expect("reserve")
        else {
            panic!("new target")
        };
        let started = fixture
            .sessions
            .start_transfer_target(session.clone())
            .await
            .expect("start");
        let pending = fixture
            .transfers
            .complete_target_start(
                token,
                Err(RegistryError::StartRollbackFailed(anyhow::anyhow!(
                    "synthetic rollback failure"
                ))),
            )
            .expect("pending cleanup");
        assert_eq!(
            pending.attempt.expect("attempt").target.session_id,
            started.session_id
        );
        assert!(
            fixture
                .transfers
                .begin_cleanup(id, token.terminal_id(), None)
                .is_err()
        );
        let ticket = fixture
            .transfers
            .begin_cleanup(id, token.terminal_id(), started.session_id)
            .expect("ticket");
        session.fail_next_termination_for_test();
        let result = fixture
            .sessions
            .cleanup_transfer_target(ticket.terminal_id, id, ticket.session_id)
            .await;
        assert!(fixture.transfers.complete_cleanup(ticket, result).is_err());
        assert!(fixture.sessions.get(ticket.terminal_id).is_some());
        let ticket = fixture
            .transfers
            .begin_cleanup(id, token.terminal_id(), started.session_id)
            .expect("retry ticket");
        let result = fixture
            .sessions
            .cleanup_transfer_target(ticket.terminal_id, id, ticket.session_id)
            .await;
        let after = fixture
            .transfers
            .complete_cleanup(ticket, result)
            .expect("cleanup retry");
        assert_eq!(after.state, TransferState::Preview);
        assert!(fixture.sessions.get(ticket.terminal_id).is_none());
        assert_eq!(
            fixture.transfers.draft(id).expect("draft").1.as_deref(),
            Some("# Retained draft")
        );
        fixture.transfers.begin_shutdown();
        fixture.sessions.shutdown().await;
    }

    #[tokio::test]
    async fn prepare_submit_bind_existing_deliver_receive_acknowledge() {
        let fixture = fixture().await;
        let source = fixture.sessions.primary();
        let source_snapshot = source.snapshot();
        let transfer_id = Uuid::new_v4();

        let created = fixture
            .transfers
            .create(
                transfer_id,
                source_snapshot.terminal_id,
                source_snapshot.session_id,
                TransferMode::Prepare,
                true,
                "Continue the refactor.".to_owned(),
            )
            .expect("create prepare transfer");
        assert_eq!(created.state, TransferState::Preparing);
        assert_eq!(created.preparation, Some(TransferPreparation::Pending));
        assert!(
            fixture
                .sessions
                .transfer_claim(source_snapshot.terminal_id)
                .is_some()
        );
        // The source cannot be restarted or become a peer source while preparing.
        assert!(matches!(
            fixture.sessions.restart(source_snapshot.terminal_id).await,
            Err(RegistryError::ParticipantBusy)
        ));

        // Duplicate create with the same parameters is idempotent; different
        // parameters conflict.
        assert_eq!(
            fixture
                .transfers
                .create(
                    transfer_id,
                    source_snapshot.terminal_id,
                    source_snapshot.session_id,
                    TransferMode::Prepare,
                    true,
                    "Continue the refactor.".to_owned(),
                )
                .expect("idempotent create")
                .state,
            TransferState::Preparing
        );
        assert_eq!(
            fixture
                .transfers
                .create(
                    transfer_id,
                    source_snapshot.terminal_id,
                    source_snapshot.session_id,
                    TransferMode::Manual,
                    false,
                    "Continue the refactor.".to_owned(),
                )
                .expect_err("changed parameters")
                .code(),
            TransferErrorCode::TransferConflict
        );

        // The source helper submits with the current attempt id.
        let source_subject = subject_for(&fixture.peers, &source);
        let attempt_id = {
            let state = lock(&fixture.transfers.inner.state);
            state.transfers[&transfer_id]
                .preparation_attempt
                .expect("pending attempt")
        };
        assert_eq!(
            fixture
                .transfers
                .submit(
                    &source_subject,
                    transfer_id,
                    Uuid::new_v4(),
                    "# Handoff".to_owned()
                )
                .expect_err("wrong attempt")
                .code(),
            TransferErrorCode::AttemptInvalidated
        );
        let submitted = fixture
            .transfers
            .submit(
                &source_subject,
                transfer_id,
                attempt_id,
                "# Handoff\r\nline".to_owned(),
            )
            .expect("submit handoff");
        assert_eq!(submitted.state, TransferState::Preview);
        assert_eq!(submitted.draft_revision, 1);
        assert_eq!(submitted.preparation, Some(TransferPreparation::Submitted));
        assert!(
            fixture
                .sessions
                .transfer_claim(source_snapshot.terminal_id)
                .is_none()
        );
        assert_eq!(
            fixture
                .transfers
                .draft(transfer_id)
                .expect("draft")
                .1
                .as_deref(),
            Some("# Handoff\nline")
        );
        // Same content again is idempotent; a different body is rejected.
        assert_eq!(
            fixture
                .transfers
                .submit(
                    &source_subject,
                    transfer_id,
                    attempt_id,
                    "# Handoff\nline".to_owned()
                )
                .expect("duplicate submit")
                .draft_revision,
            1
        );
        assert_eq!(
            fixture
                .transfers
                .submit(
                    &source_subject,
                    transfer_id,
                    attempt_id,
                    "# Other".to_owned()
                )
                .expect_err("late different submit")
                .code(),
            TransferErrorCode::AttemptInvalidated
        );

        // Bind an existing running ordinary tab; a wrong generation conflicts.
        let target = fixture
            .sessions
            .create_in(Some(AgentKind::Codex), Some(fixture.project_dir.clone()))
            .await
            .expect("create target tab");
        let target_request_id = Uuid::new_v4();
        assert_eq!(
            fixture
                .transfers
                .begin_target(
                    transfer_id,
                    Uuid::new_v4(),
                    TargetRequest::Existing {
                        terminal_id: target.terminal_id,
                        session_id: Uuid::new_v4(),
                    },
                )
                .expect_err("wrong generation")
                .code(),
            TransferErrorCode::GenerationConflict
        );
        let bound = match fixture
            .transfers
            .begin_target(
                transfer_id,
                target_request_id,
                TargetRequest::Existing {
                    terminal_id: target.terminal_id,
                    session_id: target.session_id.expect("running target"),
                },
            )
            .expect("bind existing target")
        {
            TargetAdmission::Settled(snapshot) => snapshot,
            _ => panic!("existing targets settle immediately"),
        };
        assert_eq!(bound.state, TransferState::TargetBound);
        let attempt = bound.attempt.expect("attempt recorded");
        assert_eq!(attempt.target.mode, TargetMode::Existing);
        assert_eq!(attempt.outcome, AttemptOutcome::Bound);
        assert!(matches!(
            fixture.sessions.delete(target.terminal_id).await,
            Err(RegistryError::ParticipantBusy)
        ));
        // Repeating the same request is idempotent; a new one is refused while bound.
        assert!(matches!(
            fixture
                .transfers
                .begin_target(
                    transfer_id,
                    target_request_id,
                    TargetRequest::Existing {
                        terminal_id: target.terminal_id,
                        session_id: target.session_id.expect("running target"),
                    },
                )
                .expect("idempotent target"),
            TargetAdmission::Settled(_)
        ));
        assert_eq!(
            fixture
                .transfers
                .begin_target(
                    transfer_id,
                    Uuid::new_v4(),
                    TargetRequest::Existing {
                        terminal_id: target.terminal_id,
                        session_id: target.session_id.expect("running target"),
                    },
                )
                .expect_err("second attempt while bound")
                .code(),
            TransferErrorCode::InvalidState
        );

        // Deliver requires readiness, the expected revision and generation.
        assert_eq!(
            fixture
                .transfers
                .deliver(transfer_id, 1, target.session_id.expect("gen"), false)
                .expect_err("not ready")
                .code(),
            TransferErrorCode::InvalidInput
        );
        assert_eq!(
            fixture
                .transfers
                .deliver(transfer_id, 0, target.session_id.expect("gen"), true)
                .expect_err("stale revision")
                .code(),
            TransferErrorCode::RevisionConflict
        );
        assert_eq!(
            fixture
                .transfers
                .deliver(transfer_id, 1, Uuid::new_v4(), true)
                .expect_err("wrong generation")
                .code(),
            TransferErrorCode::GenerationConflict
        );
        let delivering = fixture
            .transfers
            .deliver(transfer_id, 1, target.session_id.expect("gen"), true)
            .expect("deliver");
        assert_eq!(delivering.state, TransferState::Delivering);
        assert_eq!(delivering.approved_revision, Some(1));
        assert_eq!(delivering.delivery, Some(TransferDelivery::Unconfirmed));
        // Editing after approval is refused.
        assert_eq!(
            fixture
                .transfers
                .edit_draft(transfer_id, 1, "# Changed".to_owned())
                .expect_err("frozen")
                .code(),
            TransferErrorCode::InvalidState
        );

        // Only the target generation may receive; the source may not.
        let target_manager = fixture.sessions.get(target.terminal_id).expect("target");
        let target_subject = subject_for(&fixture.peers, &target_manager);
        assert_eq!(
            fixture
                .transfers
                .receive(&source_subject, transfer_id)
                .expect_err("source cannot receive")
                .code(),
            TransferErrorCode::Unauthorized
        );
        let artifact = fixture
            .transfers
            .receive(&target_subject, transfer_id)
            .expect("receive");
        assert_eq!(artifact.content, "# Handoff\nline");
        assert!(artifact.acknowledge);
        assert_eq!(
            fixture.transfers.get(transfer_id).expect("snapshot").state,
            TransferState::AwaitingReceipt
        );
        // Repeated receive before acknowledgement returns the same artifact.
        assert_eq!(
            fixture
                .transfers
                .receive(&target_subject, transfer_id)
                .expect("repeat receive")
                .content,
            "# Handoff\nline"
        );
        assert_eq!(
            fixture
                .transfers
                .status(&target_subject, transfer_id)
                .expect("status"),
            TransferState::AwaitingReceipt
        );

        let delivered = fixture
            .transfers
            .acknowledge(&target_subject, transfer_id)
            .expect("acknowledge");
        assert_eq!(delivered.state, TransferState::Delivered);
        assert_eq!(
            delivered.attempt.expect("attempt").outcome,
            AttemptOutcome::Delivered
        );
        assert!(
            fixture
                .sessions
                .transfer_claim(target.terminal_id)
                .is_none()
        );
        // Text is erased; acknowledgement is idempotent; receive is status-only.
        assert_eq!(fixture.transfers.draft(transfer_id).expect("draft").1, None);
        assert_eq!(
            fixture
                .transfers
                .acknowledge(&target_subject, transfer_id)
                .expect("idempotent ack")
                .state,
            TransferState::Delivered
        );
        assert_eq!(
            fixture
                .transfers
                .receive(&target_subject, transfer_id)
                .expect_err("already delivered")
                .code(),
            TransferErrorCode::AlreadyDelivered
        );
        // The target is user-owned again.
        fixture
            .sessions
            .delete(target.terminal_id)
            .await
            .expect("delete released target");

        fixture.transfers.begin_shutdown();
        fixture.sessions.shutdown().await;
    }

    #[tokio::test]
    async fn manual_draft_new_target_start_and_target_end_keep_the_draft() {
        let fixture = fixture().await;
        let source_snapshot = fixture.sessions.primary().snapshot();
        let transfer_id = Uuid::new_v4();

        let created = fixture
            .transfers
            .create(
                transfer_id,
                source_snapshot.terminal_id,
                source_snapshot.session_id,
                TransferMode::Manual,
                false,
                "Carry on.".to_owned(),
            )
            .expect("manual transfer");
        assert_eq!(created.state, TransferState::Preview);
        assert!(
            fixture
                .sessions
                .transfer_claim(source_snapshot.terminal_id)
                .is_none()
        );
        // No target before a draft exists.
        assert_eq!(
            fixture
                .transfers
                .begin_target(
                    transfer_id,
                    Uuid::new_v4(),
                    TargetRequest::New {
                        agent: AgentKind::Codex,
                        project_dir: fixture.project_dir.clone(),
                        directory_id: "dir".to_owned(),
                    },
                )
                .expect_err("no draft")
                .code(),
            TransferErrorCode::InvalidState
        );
        let drafted = fixture
            .transfers
            .edit_draft(transfer_id, 0, "# Manual handoff".to_owned())
            .expect("manual draft");
        assert_eq!(drafted.draft_revision, 1);
        assert_eq!(
            fixture
                .transfers
                .edit_draft(transfer_id, 0, "# Stale".to_owned())
                .expect_err("stale revision")
                .code(),
            TransferErrorCode::RevisionConflict
        );

        // A new target is published at reservation and started by the caller.
        let target_request_id = Uuid::new_v4();
        let request = TargetRequest::New {
            agent: AgentKind::Codex,
            project_dir: fixture.project_dir.clone(),
            directory_id: "dir".to_owned(),
        };
        let (session, token, starting) = match fixture
            .transfers
            .begin_target(transfer_id, target_request_id, request.clone())
            .expect("reserve new target")
        {
            TargetAdmission::Start {
                session,
                token,
                snapshot,
            } => (session, token, snapshot),
            _ => panic!("new targets must start"),
        };
        assert_eq!(starting.state, TransferState::Starting);
        assert_eq!(fixture.sessions.session_count(), 2);
        assert!(
            fixture
                .sessions
                .transfer_claim(token.terminal_id())
                .is_some()
        );
        // The same request while starting waits; a different one conflicts.
        assert!(matches!(
            fixture
                .transfers
                .begin_target(transfer_id, target_request_id, request.clone())
                .expect("in-flight repeat"),
            TargetAdmission::Wait(_)
        ));
        assert_eq!(
            fixture
                .transfers
                .begin_target(
                    transfer_id,
                    target_request_id,
                    TargetRequest::New {
                        agent: AgentKind::Codex,
                        project_dir: fixture.project_dir.join("other"),
                        directory_id: "other".to_owned(),
                    },
                )
                .expect_err("changed parameters")
                .code(),
            TransferErrorCode::TransferConflict
        );
        assert_eq!(
            fixture
                .transfers
                .begin_target(transfer_id, Uuid::new_v4(), request.clone())
                .expect_err("new id while unsettled")
                .code(),
            TransferErrorCode::AttemptPending
        );

        let result = fixture.sessions.start_transfer_target(session).await;
        let bound = fixture
            .transfers
            .complete_target_start(token, result)
            .expect("complete start");
        assert_eq!(bound.state, TransferState::TargetBound);
        let attempt = bound.attempt.expect("attempt");
        assert_eq!(attempt.target.mode, TargetMode::New);
        assert!(attempt.target.session_id.is_some());
        assert!(matches!(
            fixture
                .transfers
                .begin_target(transfer_id, target_request_id, request)
                .expect("settled repeat"),
            TargetAdmission::Settled(_)
        ));

        // The target ends before delivery: the attempt settles, the draft
        // stays, and a new target may be chosen explicitly.
        let target = fixture
            .sessions
            .get(token.terminal_id())
            .expect("target manager");
        target.terminate().await.expect("end target");
        let after = fixture.transfers.get(transfer_id).expect("snapshot");
        assert_eq!(after.state, TransferState::Preview);
        assert_eq!(after.delivery, Some(TransferDelivery::TargetEnded));
        assert_eq!(
            after.attempt.expect("attempt").outcome,
            AttemptOutcome::TargetEnded
        );
        assert_eq!(
            fixture
                .transfers
                .draft(transfer_id)
                .expect("draft")
                .1
                .as_deref(),
            Some("# Manual handoff")
        );
        assert!(
            fixture
                .sessions
                .transfer_claim(token.terminal_id())
                .is_none()
        );
        // The ended target is now an ordinary stopped tab the user owns.
        fixture
            .sessions
            .delete(token.terminal_id())
            .await
            .expect("delete ended target");

        let abandoned = fixture.transfers.abandon(transfer_id).expect("abandon");
        assert_eq!(abandoned.state, TransferState::Abandoned);
        assert_eq!(fixture.transfers.draft(transfer_id).expect("draft").1, None);
        assert_eq!(
            fixture
                .transfers
                .abandon(transfer_id)
                .expect("idempotent")
                .state,
            TransferState::Abandoned
        );

        fixture.transfers.begin_shutdown();
        fixture.sessions.shutdown().await;
    }

    #[tokio::test]
    async fn manual_override_and_source_end_release_the_source_without_losing_work() {
        let fixture = fixture().await;
        let source = fixture.sessions.primary();
        let source_snapshot = source.snapshot();

        // Override: editing during Preparing withdraws the attempt.
        let override_id = Uuid::new_v4();
        fixture
            .transfers
            .create(
                override_id,
                source_snapshot.terminal_id,
                source_snapshot.session_id,
                TransferMode::Prepare,
                true,
                "Summarize.".to_owned(),
            )
            .expect("prepare");
        let attempt_id = {
            let state = lock(&fixture.transfers.inner.state);
            state.transfers[&override_id]
                .preparation_attempt
                .expect("attempt")
        };
        let overridden = fixture
            .transfers
            .edit_draft(override_id, 0, "# Written by hand".to_owned())
            .expect("manual override");
        assert_eq!(overridden.state, TransferState::Preview);
        assert_eq!(
            overridden.preparation,
            Some(TransferPreparation::Overridden)
        );
        assert!(
            fixture
                .sessions
                .transfer_claim(source_snapshot.terminal_id)
                .is_none()
        );
        let subject = subject_for(&fixture.peers, &source);
        assert_eq!(
            fixture
                .transfers
                .submit(&subject, override_id, attempt_id, "# Late".to_owned())
                .expect_err("late submit after override")
                .code(),
            TransferErrorCode::AttemptInvalidated
        );
        assert_eq!(
            fixture
                .transfers
                .draft(override_id)
                .expect("draft")
                .1
                .as_deref(),
            Some("# Written by hand")
        );
        fixture.transfers.abandon(override_id).expect("abandon");

        // Source end during Preparing returns to Preview in manual mode.
        let ended_id = Uuid::new_v4();
        fixture
            .transfers
            .create(
                ended_id,
                source_snapshot.terminal_id,
                source_snapshot.session_id,
                TransferMode::Prepare,
                true,
                "Summarize.".to_owned(),
            )
            .expect("prepare again");
        source.terminate().await.expect("end source");
        let after = fixture.transfers.get(ended_id).expect("snapshot");
        assert_eq!(after.state, TransferState::Preview);
        assert_eq!(after.preparation, Some(TransferPreparation::SourceEnded));
        assert!(
            fixture
                .sessions
                .transfer_claim(source_snapshot.terminal_id)
                .is_none()
        );
        // Manual drafting from the stopped source still works.
        fixture
            .transfers
            .edit_draft(ended_id, 0, "# From a stopped source".to_owned())
            .expect("manual draft after source end");
        // Automated preparation from a stopped source is refused.
        assert_eq!(
            fixture
                .transfers
                .create(
                    Uuid::new_v4(),
                    source_snapshot.terminal_id,
                    source_snapshot.session_id,
                    TransferMode::Prepare,
                    true,
                    "Summarize.".to_owned(),
                )
                .expect_err("stopped source")
                .code(),
            TransferErrorCode::SourceNotRunning
        );

        fixture.transfers.begin_shutdown();
        assert_eq!(
            fixture.transfers.get(ended_id).expect("snapshot").state,
            TransferState::Failed
        );
        fixture.sessions.shutdown().await;
    }

    #[tokio::test]
    async fn failed_target_start_returns_to_preview_and_limits_are_enforced() {
        let fixture = fixture().await;
        let source_snapshot = fixture.sessions.primary().snapshot();
        let transfer_id = Uuid::new_v4();
        fixture
            .transfers
            .create(
                transfer_id,
                source_snapshot.terminal_id,
                source_snapshot.session_id,
                TransferMode::Manual,
                false,
                "Carry on.".to_owned(),
            )
            .expect("manual transfer");
        fixture
            .transfers
            .edit_draft(transfer_id, 0, "# Draft".to_owned())
            .expect("draft");

        // An agent that is not configured cannot be a new target.
        assert_eq!(
            fixture
                .transfers
                .begin_target(
                    transfer_id,
                    Uuid::new_v4(),
                    TargetRequest::New {
                        agent: AgentKind::Kilo,
                        project_dir: fixture.project_dir.clone(),
                        directory_id: "dir".to_owned(),
                    },
                )
                .expect_err("agent unavailable")
                .code(),
            TransferErrorCode::AgentUnavailable
        );

        // A start failure (simulated) settles the attempt and keeps the draft.
        let request = TargetRequest::New {
            agent: AgentKind::Codex,
            project_dir: fixture.project_dir.clone(),
            directory_id: "dir".to_owned(),
        };
        let (session, token, _) = match fixture
            .transfers
            .begin_target(transfer_id, Uuid::new_v4(), request.clone())
            .expect("reserve")
        {
            TargetAdmission::Start {
                session,
                token,
                snapshot,
            } => (session, token, snapshot),
            _ => panic!("new targets start"),
        };
        // Drop the reservation through the registry as a confirmed rollback.
        fixture
            .sessions
            .cleanup_transfer_target(token.terminal_id(), transfer_id, None)
            .await
            .expect("confirmed rollback of an unstarted reservation");
        drop(session);
        let after = fixture
            .transfers
            .complete_target_start(
                token,
                Err(RegistryError::OperationFailed(anyhow::anyhow!("synthetic"))),
            )
            .expect("apply failure");
        assert_eq!(after.state, TransferState::Preview);
        assert_eq!(
            after.error.map(|error| error.code),
            Some(TransferErrorCode::TargetStartFailed)
        );
        assert_eq!(
            fixture
                .transfers
                .draft(transfer_id)
                .expect("draft")
                .1
                .as_deref(),
            Some("# Draft")
        );

        // Unconfirmed rollback enters CleanupPending; cleanup needs exact identity.
        let (session, token, _) = match fixture
            .transfers
            .begin_target(transfer_id, Uuid::new_v4(), request.clone())
            .expect("reserve again")
        {
            TargetAdmission::Start {
                session,
                token,
                snapshot,
            } => (session, token, snapshot),
            _ => panic!("new targets start"),
        };
        let pending = fixture
            .transfers
            .complete_target_start(
                token,
                Err(RegistryError::StartRollbackFailed(anyhow::anyhow!(
                    "synthetic"
                ))),
            )
            .expect("apply unconfirmed rollback");
        assert_eq!(pending.state, TransferState::CleanupPending);
        assert_eq!(
            fixture
                .transfers
                .begin_target(transfer_id, Uuid::new_v4(), request.clone())
                .expect_err("no new attempt while cleanup pending")
                .code(),
            TransferErrorCode::InvalidState
        );
        assert_eq!(
            fixture
                .transfers
                .begin_cleanup(transfer_id, Uuid::new_v4(), None)
                .expect_err("wrong identity")
                .code(),
            TransferErrorCode::GenerationConflict
        );
        let ticket = fixture
            .transfers
            .begin_cleanup(transfer_id, token.terminal_id(), None)
            .expect("cleanup ticket");
        assert_eq!(
            fixture
                .transfers
                .complete_cleanup(
                    ticket,
                    Err(RegistryError::OperationFailed(anyhow::anyhow!(
                        "still running"
                    )))
                )
                .expect_err("cleanup failed again")
                .code(),
            TransferErrorCode::CleanupFailed
        );
        assert_eq!(
            fixture.transfers.get(transfer_id).expect("snapshot").state,
            TransferState::CleanupPending
        );
        let result = fixture
            .sessions
            .cleanup_transfer_target(ticket.terminal_id, transfer_id, ticket.session_id)
            .await;
        let settled = fixture
            .transfers
            .complete_cleanup(ticket, result)
            .expect("cleanup settled");
        assert_eq!(settled.state, TransferState::Preview);
        assert_eq!(settled.attempts_used, 2);
        drop(session);

        // Attempt limit.
        for _ in 2..MAX_TRANSFER_ATTEMPTS {
            let (_, token, _) = match fixture
                .transfers
                .begin_target(transfer_id, Uuid::new_v4(), request.clone())
                .expect("reserve")
            {
                TargetAdmission::Start {
                    session,
                    token,
                    snapshot,
                } => (session, token, snapshot),
                _ => panic!("new targets start"),
            };
            fixture
                .sessions
                .cleanup_transfer_target(token.terminal_id(), transfer_id, None)
                .await
                .expect("rollback");
            fixture
                .transfers
                .complete_target_start(
                    token,
                    Err(RegistryError::OperationFailed(anyhow::anyhow!("synthetic"))),
                )
                .expect("apply failure");
        }
        assert_eq!(
            fixture
                .transfers
                .begin_target(transfer_id, Uuid::new_v4(), request)
                .expect_err("attempt limit")
                .code(),
            TransferErrorCode::AttemptLimitReached
        );

        // Ledger and active limits fail closed.
        {
            let mut state = lock(&fixture.transfers.inner.state);
            state.ledger_records = MAX_TRANSFER_LEDGER_RECORDS;
        }
        assert_eq!(
            fixture
                .transfers
                .create(
                    Uuid::new_v4(),
                    source_snapshot.terminal_id,
                    source_snapshot.session_id,
                    TransferMode::Manual,
                    false,
                    "x".to_owned(),
                )
                .expect_err("ledger full")
                .code(),
            TransferErrorCode::TransferHistoryFull
        );

        fixture.transfers.begin_shutdown();
        fixture.sessions.shutdown().await;
    }

    #[tokio::test]
    async fn transfer_and_peer_claims_are_mutually_exclusive_per_source() {
        let fixture = fixture().await;
        let source_snapshot = fixture.sessions.primary().snapshot();

        let admission = fixture
            .sessions
            .begin_peer_admission(source_snapshot.terminal_id)
            .expect("peer admission");
        assert_eq!(
            fixture
                .transfers
                .create(
                    Uuid::new_v4(),
                    source_snapshot.terminal_id,
                    source_snapshot.session_id,
                    TransferMode::Prepare,
                    true,
                    "Summarize.".to_owned(),
                )
                .expect_err("peer provisioning in flight")
                .code(),
            TransferErrorCode::ParticipantBusy
        );
        drop(admission);

        let transfer_id = Uuid::new_v4();
        fixture
            .transfers
            .create(
                transfer_id,
                source_snapshot.terminal_id,
                source_snapshot.session_id,
                TransferMode::Prepare,
                true,
                "Summarize.".to_owned(),
            )
            .expect("prepare");
        assert!(matches!(
            fixture
                .sessions
                .begin_peer_admission(source_snapshot.terminal_id),
            Err(RegistryError::ParticipantBusy)
        ));
        fixture.transfers.abandon(transfer_id).expect("abandon");
        fixture
            .sessions
            .begin_peer_admission(source_snapshot.terminal_id)
            .expect("peer admission after abandon");

        fixture.transfers.begin_shutdown();
        fixture.sessions.shutdown().await;
    }

    #[test]
    fn snapshot_and_error_wire_shapes_are_stable() {
        let snapshot = TransferSnapshot {
            transfer_id: Uuid::nil(),
            revision: 3,
            state: TransferState::AwaitingReceipt,
            mode: TransferMode::Prepare,
            source_terminal_id: Uuid::nil(),
            source_session_id: None,
            draft_revision: 2,
            approved_revision: Some(2),
            preparation: Some(TransferPreparation::QueueRejected),
            attempt: None,
            attempts_used: 0,
            delivery: Some(TransferDelivery::Unconfirmed),
            error: Some(TransferErrorSnapshot {
                code: TransferErrorCode::AttemptLimitReached,
            }),
            created_at: 1,
            updated_at: 2,
        };
        let value = serde_json::to_value(snapshot).expect("serialize");
        assert_eq!(value["state"], "awaiting_receipt");
        assert_eq!(value["preparation"], "queue_rejected");
        assert_eq!(value["delivery"], "unconfirmed");
        assert_eq!(value["error"]["code"], "attempt_limit_reached");
        assert_eq!(value["sourceSessionId"], serde_json::Value::Null);
        assert!(value.get("attempt").is_none());
        assert_eq!(TransferState::CleanupPending.as_str(), "cleanup_pending");
        assert!(delivery_prompt(Uuid::nil()).contains("transfer-receive --transfer"));
        assert!(
            preparation_prompt(Uuid::nil(), Uuid::nil(), "line one\nline\ttwo")
                .contains("line one line two")
        );
    }

    async fn wait_for_output(session: &SessionManager, expected: &str) {
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut answered_cursor_query = false;
        loop {
            let output = session
                .output_snapshot()
                .chunks
                .into_iter()
                .flat_map(|chunk| chunk.data)
                .collect::<Vec<_>>();
            if cfg!(windows)
                && !answered_cursor_query
                && output.windows(4).any(|window| window == b"\x1b[6n")
            {
                session
                    .write_input(b"\x1b[1;1R")
                    .expect("answer ConPTY cursor query");
                answered_cursor_query = true;
            }
            if String::from_utf8_lossy(&output).contains(expected) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {expected}"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    #[cfg(windows)]
    fn write_long_running_fixture(directory: &Path) -> PathBuf {
        let command = directory.join("long-running-agent.cmd");
        std::fs::write(
            &command,
            "@echo off\r\nif \"%~1\"==\"--version\" (\r\n  echo codex 1.2.3\r\n  exit /b 0\r\n)\r\necho WORKSPACE-READY\r\n:loop\r\nping -n 2 127.0.0.1 >nul\r\ngoto loop\r\n",
        )
        .expect("write Windows fixture");
        command
    }

    #[cfg(unix)]
    fn write_long_running_fixture(directory: &Path) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;

        let command = directory.join("long-running-agent");
        std::fs::write(
            &command,
            "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then\n  echo 'codex 1.2.3'\n  exit 0\nfi\nprintf 'WORKSPACE-READY\\n'\nwhile :; do sleep 1; done\n",
        )
        .expect("write Unix fixture");
        let mut permissions = std::fs::metadata(&command)
            .expect("fixture metadata")
            .permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(&command, permissions).expect("make fixture executable");
        command
    }
}
