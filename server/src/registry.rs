use std::{
    collections::HashMap,
    fmt,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

use uuid::Uuid;

use crate::{
    config::{AgentKind, DEFAULT_MAX_SESSIONS, MAX_CONFIGURED_SESSIONS},
    peer::{PeerBroker, SessionPurpose},
    session::{Lifecycle, SessionManager, SessionSnapshot},
    terminal::TerminalConfig,
};

#[derive(Debug)]
pub enum RegistryError {
    LimitReached,
    NotFound,
    PrimaryCannotBeDeleted,
    AgentUnavailable,
    PeerUnavailable,
    InvalidPeerParent,
    InvalidPeerSession,
    PeerSessionManaged,
    PeerSessionsActive,
    ShuttingDown,
    OperationFailed(anyhow::Error),
}

impl fmt::Display for RegistryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::LimitReached => write!(formatter, "the terminal session limit has been reached"),
            Self::NotFound => write!(formatter, "terminal session was not found"),
            Self::PrimaryCannotBeDeleted => {
                write!(formatter, "the primary terminal session cannot be deleted")
            }
            Self::AgentUnavailable => write!(formatter, "the requested agent is not configured"),
            Self::PeerUnavailable => write!(formatter, "peer communication is not configured"),
            Self::InvalidPeerParent => {
                write!(
                    formatter,
                    "the peer parent must be a running interactive session"
                )
            }
            Self::InvalidPeerSession => {
                write!(formatter, "the dedicated peer session no longer matches")
            }
            Self::PeerSessionManaged => {
                write!(
                    formatter,
                    "a dedicated peer terminal is managed by its peer thread"
                )
            }
            Self::PeerSessionsActive => {
                write!(
                    formatter,
                    "the terminal has an active dedicated peer session"
                )
            }
            Self::ShuttingDown => write!(formatter, "the server is shutting down"),
            Self::OperationFailed(error) => write!(formatter, "{error}"),
        }
    }
}

impl std::error::Error for RegistryError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::OperationFailed(error) => Some(error.as_ref()),
            _ => None,
        }
    }
}

#[derive(Clone)]
pub struct SessionRegistry {
    inner: Arc<RegistryInner>,
}

struct RegistryInner {
    agent_configs: HashMap<AgentKind, TerminalConfig>,
    default_new_agent: AgentKind,
    max_sessions: usize,
    peer_broker: Option<PeerBroker>,
    state: Mutex<RegistryState>,
    shutting_down: AtomicBool,
}

struct RegistryState {
    primary_id: Uuid,
    next_terminal_number: usize,
    sessions: HashMap<Uuid, SessionManager>,
    lifecycle_mutations: HashMap<Uuid, usize>,
}

struct LifecycleMutationGuard {
    inner: Arc<RegistryInner>,
    terminal_id: Uuid,
}

impl Drop for LifecycleMutationGuard {
    fn drop(&mut self) {
        let mut state = lock(&self.inner.state);
        let remove = match state.lifecycle_mutations.get_mut(&self.terminal_id) {
            Some(count) if *count > 1 => {
                *count -= 1;
                false
            }
            Some(_) => true,
            None => false,
        };
        if remove {
            state.lifecycle_mutations.remove(&self.terminal_id);
        }
    }
}

impl SessionRegistry {
    pub fn new(terminal_config: TerminalConfig) -> Self {
        Self::with_new_session_config(terminal_config.clone(), terminal_config)
    }

    pub fn with_new_session_config(
        primary_config: TerminalConfig,
        new_session_config: TerminalConfig,
    ) -> Self {
        Self::with_agent_configs(primary_config, new_session_config, Vec::new())
    }

    pub fn with_agent_configs(
        primary_config: TerminalConfig,
        new_session_config: TerminalConfig,
        additional_agent_configs: Vec<TerminalConfig>,
    ) -> Self {
        Self::with_optional_peer_broker(
            primary_config,
            new_session_config,
            additional_agent_configs,
            None,
            DEFAULT_MAX_SESSIONS,
        )
    }

    pub fn with_agent_configs_and_peer_broker(
        primary_config: TerminalConfig,
        new_session_config: TerminalConfig,
        additional_agent_configs: Vec<TerminalConfig>,
        peer_broker: PeerBroker,
    ) -> Self {
        Self::with_optional_peer_broker(
            primary_config,
            new_session_config,
            additional_agent_configs,
            Some(peer_broker),
            DEFAULT_MAX_SESSIONS,
        )
    }

    pub fn with_agent_configs_and_peer_broker_with_max_sessions(
        primary_config: TerminalConfig,
        new_session_config: TerminalConfig,
        additional_agent_configs: Vec<TerminalConfig>,
        peer_broker: PeerBroker,
        max_sessions: usize,
    ) -> Self {
        Self::with_optional_peer_broker(
            primary_config,
            new_session_config,
            additional_agent_configs,
            Some(peer_broker),
            max_sessions,
        )
    }

    fn with_optional_peer_broker(
        primary_config: TerminalConfig,
        new_session_config: TerminalConfig,
        additional_agent_configs: Vec<TerminalConfig>,
        peer_broker: Option<PeerBroker>,
        max_sessions: usize,
    ) -> Self {
        assert!(
            (1..=MAX_CONFIGURED_SESSIONS).contains(&max_sessions),
            "the session limit must be between 1 and {MAX_CONFIGURED_SESSIONS}"
        );
        let primary_id = Uuid::new_v4();
        let primary_name = format!("{} 1", primary_config.agent.label());
        let primary = SessionManager::new_managed_with_peer(
            primary_config,
            primary_id,
            primary_name,
            true,
            SessionPurpose::Interactive,
            peer_broker.clone(),
        );
        let default_new_agent = new_session_config.agent;
        let mut agent_configs = HashMap::new();
        agent_configs.insert(default_new_agent, new_session_config);
        for config in additional_agent_configs {
            agent_configs.insert(config.agent, config);
        }
        let mut sessions = HashMap::new();
        sessions.insert(primary_id, primary);

        Self {
            inner: Arc::new(RegistryInner {
                agent_configs,
                default_new_agent,
                max_sessions,
                peer_broker,
                state: Mutex::new(RegistryState {
                    primary_id,
                    next_terminal_number: 2,
                    sessions,
                    lifecycle_mutations: HashMap::new(),
                }),
                shutting_down: AtomicBool::new(false),
            }),
        }
    }

    pub async fn start_primary(&self) -> anyhow::Result<()> {
        self.primary().start().await
    }

    pub fn primary(&self) -> SessionManager {
        let state = lock(&self.inner.state);
        state
            .sessions
            .get(&state.primary_id)
            .expect("the primary terminal session is always registered")
            .clone()
    }

    pub fn get(&self, terminal_id: Uuid) -> Option<SessionManager> {
        lock(&self.inner.state).sessions.get(&terminal_id).cloned()
    }

    pub fn list(&self) -> Vec<SessionSnapshot> {
        let sessions = self.managers();
        let mut snapshots: Vec<_> = sessions.iter().map(SessionManager::snapshot).collect();
        snapshots.sort_by(|left, right| {
            right
                .is_primary
                .cmp(&left.is_primary)
                .then_with(|| left.created_at.cmp(&right.created_at))
                .then_with(|| left.name.cmp(&right.name))
                .then_with(|| {
                    left.terminal_id
                        .as_bytes()
                        .cmp(right.terminal_id.as_bytes())
                })
        });
        snapshots
    }

    pub async fn create(
        &self,
        requested_agent: Option<AgentKind>,
    ) -> Result<SessionSnapshot, RegistryError> {
        self.create_in(requested_agent, None).await
    }

    pub async fn create_in(
        &self,
        requested_agent: Option<AgentKind>,
        project_dir: Option<PathBuf>,
    ) -> Result<SessionSnapshot, RegistryError> {
        let session = self.reserve_session_in(requested_agent, project_dir)?;
        self.start_reserved_session(session).await
    }

    pub async fn create_peer_in(
        &self,
        requested_agent: AgentKind,
        project_dir: PathBuf,
        thread_id: Uuid,
        parent_terminal_id: Uuid,
        parent_session_id: Uuid,
        reviewer_terminal_id: Uuid,
    ) -> Result<SessionSnapshot, RegistryError> {
        let session = self.reserve_peer_session_in(
            requested_agent,
            project_dir,
            thread_id,
            parent_terminal_id,
            parent_session_id,
            reviewer_terminal_id,
        )?;
        self.start_reserved_session(session).await
    }

    async fn start_reserved_session(
        &self,
        session: SessionManager,
    ) -> Result<SessionSnapshot, RegistryError> {
        let inner = self.inner.clone();
        tokio::spawn(async move {
            let terminal_id = session.snapshot().terminal_id;
            if let Err(error) = session.start().await {
                if let Err(cleanup_error) = session.shutdown_checked().await {
                    return Err(RegistryError::OperationFailed(
                        cleanup_error.context("failed to roll back a session that did not start"),
                    ));
                }
                lock(&inner.state).sessions.remove(&terminal_id);
                return Err(RegistryError::OperationFailed(error));
            }
            Ok(session.snapshot())
        })
        .await
        .map_err(|error| RegistryError::OperationFailed(anyhow::Error::new(error)))?
    }

    pub async fn restart(&self, terminal_id: Uuid) -> Result<(), RegistryError> {
        let (session, mutation) = self.begin_interactive_lifecycle_mutation(terminal_id)?;
        tokio::spawn(async move {
            let _mutation = mutation;
            session.restart().await
        })
        .await
        .map_err(|error| RegistryError::OperationFailed(anyhow::Error::new(error)))?
        .map_err(RegistryError::OperationFailed)
    }

    pub async fn terminate(&self, terminal_id: Uuid) -> Result<(), RegistryError> {
        let (session, mutation) = self.begin_interactive_lifecycle_mutation(terminal_id)?;
        tokio::spawn(async move {
            let _mutation = mutation;
            session.terminate().await
        })
        .await
        .map_err(|error| RegistryError::OperationFailed(anyhow::Error::new(error)))?
        .map_err(RegistryError::OperationFailed)
    }

    pub async fn delete(&self, terminal_id: Uuid) -> Result<(), RegistryError> {
        {
            let state = lock(&self.inner.state);
            if terminal_id == state.primary_id {
                return Err(RegistryError::PrimaryCannotBeDeleted);
            }
        }
        let (session, mutation) = self.begin_interactive_lifecycle_mutation(terminal_id)?;
        self.delete_with_mutation(terminal_id, session, mutation)
            .await
    }

    pub(crate) async fn delete_peer(
        &self,
        terminal_id: Uuid,
        thread_id: Uuid,
        parent_terminal_id: Uuid,
        expected_session_id: Option<Uuid>,
    ) -> Result<(), RegistryError> {
        let (session, mutation) = self.begin_peer_deletion(
            terminal_id,
            thread_id,
            parent_terminal_id,
            expected_session_id,
        )?;
        self.delete_with_mutation(terminal_id, session, mutation)
            .await
    }

    async fn delete_with_mutation(
        &self,
        terminal_id: Uuid,
        session: SessionManager,
        mutation: LifecycleMutationGuard,
    ) -> Result<(), RegistryError> {
        let inner = self.inner.clone();
        tokio::spawn(async move {
            let _mutation = mutation;
            session
                .shutdown_checked()
                .await
                .map_err(RegistryError::OperationFailed)?;

            let mut state = lock(&inner.state);
            if state.sessions.remove(&terminal_id).is_none() {
                return Err(RegistryError::NotFound);
            }
            Ok(())
        })
        .await
        .map_err(|error| RegistryError::OperationFailed(anyhow::Error::new(error)))?
    }

    pub async fn shutdown(&self) {
        if self.inner.shutting_down.swap(true, Ordering::SeqCst) {
            return;
        }

        let sessions = self.managers();
        for session in sessions {
            session.shutdown().await;
        }
    }

    pub fn codex_installed(&self) -> bool {
        self.managers().iter().any(SessionManager::codex_installed)
    }

    pub fn running_sessions(&self) -> usize {
        self.managers()
            .iter()
            .filter(|session| session.is_running())
            .count()
    }

    pub fn connected_clients(&self) -> usize {
        self.managers().iter().fold(0_usize, |total, session| {
            total.saturating_add(session.connected_clients())
        })
    }

    pub fn session_count(&self) -> usize {
        lock(&self.inner.state).sessions.len()
    }

    pub fn max_sessions(&self) -> usize {
        self.inner.max_sessions
    }

    pub fn available_agents(&self) -> Vec<AgentKind> {
        AgentKind::ALL
            .into_iter()
            .filter(|agent| self.inner.agent_configs.contains_key(agent))
            .collect()
    }

    #[cfg(test)]
    fn reserve_session(
        &self,
        requested_agent: Option<AgentKind>,
    ) -> Result<SessionManager, RegistryError> {
        self.reserve_session_in(requested_agent, None)
    }

    fn reserve_session_in(
        &self,
        requested_agent: Option<AgentKind>,
        project_dir: Option<PathBuf>,
    ) -> Result<SessionManager, RegistryError> {
        self.reserve_session_with_purpose(
            requested_agent.unwrap_or(self.inner.default_new_agent),
            project_dir,
            SessionPurpose::Interactive,
            None,
            None,
        )
    }

    fn reserve_peer_session_in(
        &self,
        requested_agent: AgentKind,
        project_dir: PathBuf,
        thread_id: Uuid,
        parent_terminal_id: Uuid,
        parent_session_id: Uuid,
        reviewer_terminal_id: Uuid,
    ) -> Result<SessionManager, RegistryError> {
        if self.inner.peer_broker.is_none() {
            return Err(RegistryError::PeerUnavailable);
        }

        self.reserve_session_with_purpose(
            requested_agent,
            Some(project_dir),
            SessionPurpose::Peer {
                thread_id,
                parent_terminal_id,
            },
            Some((parent_terminal_id, parent_session_id)),
            Some(reviewer_terminal_id),
        )
    }

    fn reserve_session_with_purpose(
        &self,
        agent: AgentKind,
        project_dir: Option<PathBuf>,
        purpose: SessionPurpose,
        peer_parent: Option<(Uuid, Uuid)>,
        terminal_id: Option<Uuid>,
    ) -> Result<SessionManager, RegistryError> {
        let mut state = lock(&self.inner.state);
        if self.inner.shutting_down.load(Ordering::SeqCst) {
            return Err(RegistryError::ShuttingDown);
        }
        if state.sessions.len() >= self.inner.max_sessions {
            return Err(RegistryError::LimitReached);
        }

        if let Some((parent_terminal_id, parent_session_id)) = peer_parent {
            if state.lifecycle_mutations.contains_key(&parent_terminal_id) {
                return Err(RegistryError::InvalidPeerParent);
            }
            let parent = state
                .sessions
                .get(&parent_terminal_id)
                .ok_or(RegistryError::InvalidPeerParent)?;
            let parent_snapshot = parent.snapshot();
            if !matches!(parent.purpose(), SessionPurpose::Interactive)
                || parent_snapshot.status != Lifecycle::Running
                || parent_snapshot.session_id != Some(parent_session_id)
            {
                return Err(RegistryError::InvalidPeerParent);
            }
        }

        let terminal_id = terminal_id.unwrap_or_else(Uuid::new_v4);
        if state.sessions.contains_key(&terminal_id) {
            return Err(RegistryError::InvalidPeerSession);
        }
        let mut terminal_config = self
            .inner
            .agent_configs
            .get(&agent)
            .cloned()
            .ok_or(RegistryError::AgentUnavailable)?;
        if let Some(project_dir) = project_dir {
            terminal_config.project_dir = project_dir;
        }
        let name = match &purpose {
            SessionPurpose::Interactive => {
                format!("{} {}", agent.label(), state.next_terminal_number)
            }
            SessionPurpose::Peer { .. } => {
                format!("↳ {} Review {}", agent.label(), state.next_terminal_number)
            }
        };
        state.next_terminal_number = state.next_terminal_number.saturating_add(1);
        let session = SessionManager::new_managed_with_peer(
            terminal_config,
            terminal_id,
            name,
            false,
            purpose,
            self.inner.peer_broker.clone(),
        );
        state.sessions.insert(terminal_id, session.clone());
        Ok(session)
    }

    #[cfg(test)]
    fn begin_lifecycle_mutation(
        &self,
        terminal_id: Uuid,
    ) -> Result<(SessionManager, LifecycleMutationGuard), RegistryError> {
        self.begin_lifecycle_mutation_with_policy(terminal_id, true)
    }

    fn begin_interactive_lifecycle_mutation(
        &self,
        terminal_id: Uuid,
    ) -> Result<(SessionManager, LifecycleMutationGuard), RegistryError> {
        self.begin_lifecycle_mutation_with_policy(terminal_id, false)
    }

    fn begin_peer_deletion(
        &self,
        terminal_id: Uuid,
        thread_id: Uuid,
        parent_terminal_id: Uuid,
        expected_session_id: Option<Uuid>,
    ) -> Result<(SessionManager, LifecycleMutationGuard), RegistryError> {
        let mut state = lock(&self.inner.state);
        let session = state
            .sessions
            .get(&terminal_id)
            .cloned()
            .ok_or(RegistryError::NotFound)?;
        if session.purpose()
            != &(SessionPurpose::Peer {
                thread_id,
                parent_terminal_id,
            })
            || session.snapshot().session_id != expected_session_id
        {
            return Err(RegistryError::InvalidPeerSession);
        }
        if state.lifecycle_mutations.contains_key(&terminal_id) {
            return Err(RegistryError::InvalidPeerSession);
        }
        state.lifecycle_mutations.insert(terminal_id, 1);
        Ok((
            session,
            LifecycleMutationGuard {
                inner: self.inner.clone(),
                terminal_id,
            },
        ))
    }

    fn begin_lifecycle_mutation_with_policy(
        &self,
        terminal_id: Uuid,
        allow_peer_session: bool,
    ) -> Result<(SessionManager, LifecycleMutationGuard), RegistryError> {
        let mut state = lock(&self.inner.state);
        let session = state
            .sessions
            .get(&terminal_id)
            .cloned()
            .ok_or(RegistryError::NotFound)?;
        if !allow_peer_session && matches!(session.purpose(), SessionPurpose::Peer { .. }) {
            return Err(RegistryError::PeerSessionManaged);
        }
        if state.sessions.values().any(|candidate| {
            matches!(
                candidate.purpose(),
                SessionPurpose::Peer {
                    parent_terminal_id,
                    ..
                } if *parent_terminal_id == terminal_id
            )
        }) {
            return Err(RegistryError::PeerSessionsActive);
        }
        let count = state.lifecycle_mutations.entry(terminal_id).or_default();
        *count = count.saturating_add(1);
        Ok((
            session,
            LifecycleMutationGuard {
                inner: self.inner.clone(),
                terminal_id,
            },
        ))
    }

    fn managers(&self) -> Vec<SessionManager> {
        lock(&self.inner.state).sessions.values().cloned().collect()
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use std::{
        net::SocketAddr,
        path::{Path, PathBuf},
        time::{Duration, Instant},
    };

    use super::*;
    use crate::{config::ShellKind, peer::PeerAction};

    fn terminal_config() -> TerminalConfig {
        TerminalConfig {
            project_dir: PathBuf::from("."),
            command: "codex".to_owned(),
            arguments: Vec::new(),
            agent: AgentKind::Codex,
            shell: ShellKind::Powershell,
        }
    }

    #[test]
    fn creates_a_stable_primary_terminal() {
        let registry = SessionRegistry::new(terminal_config());
        let first = registry.primary().snapshot();
        let second = registry.primary().snapshot();

        assert_eq!(first.terminal_id, second.terminal_id);
        assert_eq!(first.name, "Codex 1");
        assert_eq!(first.agent, AgentKind::Codex);
        assert!(first.is_primary);
        assert_eq!(first.purpose, SessionPurpose::Interactive);
        assert_eq!(first.session_id, None);
        assert_eq!(registry.list().len(), 1);
    }

    #[test]
    fn session_purpose_has_a_stable_frontend_shape() {
        let thread_id = Uuid::new_v4();
        let parent_terminal_id = Uuid::new_v4();

        assert_eq!(
            serde_json::to_value(SessionPurpose::Interactive).expect("interactive purpose"),
            serde_json::json!({ "kind": "interactive" })
        );
        assert_eq!(
            serde_json::to_value(SessionPurpose::Peer {
                thread_id,
                parent_terminal_id,
            })
            .expect("peer purpose"),
            serde_json::json!({
                "kind": "peer",
                "threadId": thread_id,
                "parentTerminalId": parent_terminal_id,
            })
        );
    }

    #[test]
    fn new_terminals_can_use_a_different_command_from_the_primary() {
        let mut primary_config = terminal_config();
        primary_config.command = "resume-current".to_owned();
        let new_session_config = terminal_config();
        let registry = SessionRegistry::with_new_session_config(primary_config, new_session_config);

        let primary = registry.primary();
        let created = registry.reserve_session(None).expect("reserved session");

        assert_eq!(primary.configured_command(), "resume-current");
        assert_eq!(created.configured_command(), "codex");
        assert_eq!(created.configured_agent(), AgentKind::Codex);
    }

    #[test]
    fn selected_project_directory_applies_only_to_the_reserved_terminal() {
        let registry = SessionRegistry::new(terminal_config());
        let selected = if cfg!(windows) {
            PathBuf::from(r"C:\projects\selected")
        } else {
            PathBuf::from("/projects/selected")
        };

        let created = registry
            .reserve_session_in(None, Some(selected.clone()))
            .expect("reserved session");

        assert_eq!(created.snapshot().project, selected.to_string_lossy());
        assert_eq!(
            crate::filesystem::decode_directory_id(&created.snapshot().directory_id)
                .expect("session directory ID"),
            selected
        );
        assert_eq!(registry.primary().snapshot().project, ".");
    }

    #[test]
    fn reserves_only_configured_agent_profiles() {
        let primary_config = terminal_config();
        let new_session_config = terminal_config();
        let mut claude_config = terminal_config();
        claude_config.command = "claude".to_owned();
        claude_config.arguments = vec!["--dangerously-skip-permissions".to_owned()];
        claude_config.agent = AgentKind::Claude;
        let registry = SessionRegistry::with_agent_configs(
            primary_config,
            new_session_config,
            vec![claude_config],
        );

        assert_eq!(
            registry.available_agents(),
            vec![AgentKind::Codex, AgentKind::Claude]
        );
        let claude = registry
            .reserve_session(Some(AgentKind::Claude))
            .expect("configured Claude session");
        assert_eq!(claude.configured_command(), "claude");
        assert_eq!(
            claude.configured_arguments(),
            ["--dangerously-skip-permissions"]
        );
        assert_eq!(claude.configured_agent(), AgentKind::Claude);
        assert_eq!(claude.snapshot().name, "Claude 2");
        assert!(matches!(
            registry.reserve_session(Some(AgentKind::Agy)),
            Err(RegistryError::AgentUnavailable)
        ));
    }

    #[test]
    fn reserves_unique_terminals_up_to_the_limit() {
        const TEST_MAX_SESSIONS: usize = 3;
        let config = terminal_config();
        let registry = SessionRegistry::with_optional_peer_broker(
            config.clone(),
            config,
            Vec::new(),
            None,
            TEST_MAX_SESSIONS,
        );

        for _ in 1..TEST_MAX_SESSIONS {
            registry
                .reserve_session(None)
                .expect("session within limit");
        }

        let snapshots = registry.list();
        assert_eq!(registry.max_sessions(), TEST_MAX_SESSIONS);
        assert_eq!(snapshots.len(), TEST_MAX_SESSIONS);
        assert_eq!(
            snapshots
                .iter()
                .map(|snapshot| snapshot.terminal_id)
                .collect::<std::collections::HashSet<_>>()
                .len(),
            TEST_MAX_SESSIONS
        );
        assert!(matches!(
            registry.reserve_session(None),
            Err(RegistryError::LimitReached)
        ));
    }

    #[test]
    fn concurrent_reservations_never_exceed_the_configured_limit() {
        const TEST_MAX_SESSIONS: usize = 8;
        const ATTEMPTS: usize = 64;
        let config = terminal_config();
        let registry = SessionRegistry::with_optional_peer_broker(
            config.clone(),
            config,
            Vec::new(),
            None,
            TEST_MAX_SESSIONS,
        );
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(ATTEMPTS));

        let successes = std::thread::scope(|scope| {
            let handles = (0..ATTEMPTS)
                .map(|_| {
                    let registry = registry.clone();
                    let barrier = barrier.clone();
                    scope.spawn(move || {
                        barrier.wait();
                        registry.reserve_session(None).is_ok()
                    })
                })
                .collect::<Vec<_>>();
            handles
                .into_iter()
                .map(|handle| handle.join().expect("reservation thread"))
                .filter(|reserved| *reserved)
                .count()
        });

        assert_eq!(successes, TEST_MAX_SESSIONS - 1);
        assert_eq!(registry.session_count(), TEST_MAX_SESSIONS);
    }

    #[test]
    #[should_panic(expected = "the session limit must be between 1 and 256")]
    fn custom_registry_capacity_rejects_values_above_the_supported_range() {
        let config = terminal_config();
        let _registry = SessionRegistry::with_optional_peer_broker(
            config.clone(),
            config,
            Vec::new(),
            None,
            MAX_CONFIGURED_SESSIONS + 1,
        );
    }

    #[test]
    fn peer_sessions_require_an_internal_broker() {
        let registry = SessionRegistry::new(terminal_config());

        assert!(matches!(
            registry.reserve_peer_session_in(
                AgentKind::Codex,
                PathBuf::from("."),
                Uuid::new_v4(),
                registry.primary().snapshot().terminal_id,
                Uuid::new_v4(),
                Uuid::new_v4(),
            ),
            Err(RegistryError::PeerUnavailable)
        ));
    }

    #[tokio::test]
    async fn creates_a_fresh_dedicated_peer_linked_to_its_running_parent() {
        let fixture = tempfile::tempdir().expect("temporary project");
        let command = write_long_running_fixture(fixture.path());
        let project_dir = dunce::canonicalize(fixture.path()).expect("canonical temporary project");
        let mut config = terminal_config();
        config.project_dir = project_dir.clone();
        config.command = command.to_string_lossy().into_owned();
        let broker = PeerBroker::new(
            "127.0.0.1:43123"
                .parse::<SocketAddr>()
                .expect("loopback peer endpoint"),
            PathBuf::from("codex-web"),
        );
        let registry = SessionRegistry::with_agent_configs_and_peer_broker(
            config.clone(),
            config,
            Vec::new(),
            broker.clone(),
        );

        registry.start_primary().await.expect("start parent");
        let parent = registry.primary();
        wait_for_output(&parent, "WORKSPACE-READY").await;
        let parent_snapshot = parent.snapshot();
        let ordinary = registry
            .reserve_session(Some(AgentKind::Codex))
            .expect("reserve ordinary session");
        let thread = broker
            .create_thread(
                parent_snapshot.terminal_id,
                AgentKind::Codex,
                PeerAction::Review,
                "Review the completed work.".to_owned(),
            )
            .expect("create peer thread");
        let provisioning = broker
            .begin_reviewer_provisioning(thread.id)
            .expect("reserve peer reviewer identity");
        let reviewer_terminal_id = provisioning
            .thread()
            .reviewer_terminal_id
            .expect("reserved reviewer identity");

        let peer = registry
            .create_peer_in(
                AgentKind::Codex,
                project_dir,
                thread.id,
                parent_snapshot.terminal_id,
                parent_snapshot
                    .session_id
                    .expect("active parent generation"),
                reviewer_terminal_id,
            )
            .await
            .expect("start dedicated peer");
        broker
            .complete_reviewer_provisioning(provisioning)
            .expect("complete peer reviewer provisioning");

        assert_ne!(peer.terminal_id, ordinary.snapshot().terminal_id);
        assert_ne!(peer.terminal_id, parent_snapshot.terminal_id);
        assert!(peer.name.starts_with("↳ Codex Review "));
        assert_eq!(
            peer.purpose,
            SessionPurpose::Peer {
                thread_id: thread.id,
                parent_terminal_id: parent_snapshot.terminal_id,
            }
        );
        assert_eq!(peer.status, crate::session::Lifecycle::Running);
        assert!(peer.session_id.is_some());
        assert_eq!(registry.session_count(), 3);
        assert!(matches!(
            registry.restart(peer.terminal_id).await,
            Err(RegistryError::PeerSessionManaged)
        ));
        assert!(matches!(
            registry.terminate(peer.terminal_id).await,
            Err(RegistryError::PeerSessionManaged)
        ));
        assert!(matches!(
            registry.delete(peer.terminal_id).await,
            Err(RegistryError::PeerSessionManaged)
        ));
        assert!(matches!(
            registry.restart(parent_snapshot.terminal_id).await,
            Err(RegistryError::PeerSessionsActive)
        ));

        registry.shutdown().await;
    }

    #[tokio::test]
    async fn peer_reservation_and_source_lifecycle_mutation_are_transactional() {
        let fixture = tempfile::tempdir().expect("temporary project");
        let command = write_long_running_fixture(fixture.path());
        let project_dir = dunce::canonicalize(fixture.path()).expect("canonical temporary project");
        let mut config = terminal_config();
        config.project_dir = project_dir.clone();
        config.command = command.to_string_lossy().into_owned();
        let broker = PeerBroker::new(
            "127.0.0.1:43123"
                .parse::<SocketAddr>()
                .expect("loopback peer endpoint"),
            PathBuf::from("codex-web"),
        );
        let registry = SessionRegistry::with_agent_configs_and_peer_broker(
            config.clone(),
            config,
            Vec::new(),
            broker,
        );

        registry.start_primary().await.expect("start source");
        let parent = registry.primary().snapshot();
        let parent_session_id = parent.session_id.expect("running source generation");
        let occupied = registry
            .reserve_session(Some(AgentKind::Codex))
            .expect("reserve an occupied ordinary terminal identity");
        let occupied_snapshot = occupied.snapshot();
        assert!(matches!(
            registry.reserve_peer_session_in(
                AgentKind::Codex,
                project_dir.clone(),
                Uuid::new_v4(),
                parent.terminal_id,
                parent_session_id,
                occupied_snapshot.terminal_id,
            ),
            Err(RegistryError::InvalidPeerSession)
        ));
        assert_eq!(
            registry
                .get(occupied_snapshot.terminal_id)
                .expect("ordinary terminal was not replaced")
                .purpose(),
            &SessionPurpose::Interactive
        );
        registry
            .delete(occupied_snapshot.terminal_id)
            .await
            .expect("delete occupied ordinary terminal");

        assert!(matches!(
            registry.reserve_peer_session_in(
                AgentKind::Codex,
                project_dir.clone(),
                Uuid::new_v4(),
                parent.terminal_id,
                Uuid::new_v4(),
                Uuid::new_v4(),
            ),
            Err(RegistryError::InvalidPeerParent)
        ));

        let (_, mutation) = registry
            .begin_lifecycle_mutation(parent.terminal_id)
            .expect("begin deterministic lifecycle window");
        assert!(matches!(
            registry.reserve_peer_session_in(
                AgentKind::Codex,
                project_dir.clone(),
                Uuid::new_v4(),
                parent.terminal_id,
                parent_session_id,
                Uuid::new_v4(),
            ),
            Err(RegistryError::InvalidPeerParent)
        ));
        drop(mutation);

        let peer_thread_id = Uuid::new_v4();
        let peer = registry
            .reserve_peer_session_in(
                AgentKind::Codex,
                project_dir,
                peer_thread_id,
                parent.terminal_id,
                parent_session_id,
                Uuid::new_v4(),
            )
            .expect("reserve peer after lifecycle window");
        assert!(matches!(
            registry.begin_lifecycle_mutation(parent.terminal_id),
            Err(RegistryError::PeerSessionsActive)
        ));

        let peer_snapshot = peer.snapshot();
        registry
            .delete_peer(
                peer_snapshot.terminal_id,
                peer_thread_id,
                parent.terminal_id,
                peer_snapshot.session_id,
            )
            .await
            .expect("delete reserved peer");
        let (_, mutation) = registry
            .begin_lifecycle_mutation(parent.terminal_id)
            .expect("source lifecycle unblocked after peer deletion");
        drop(mutation);
        registry.shutdown().await;
    }

    #[tokio::test]
    async fn failed_peer_termination_preserves_registry_and_thread_for_retry() {
        let fixture = tempfile::tempdir().expect("temporary project");
        let command = write_long_running_fixture(fixture.path());
        let project_dir = dunce::canonicalize(fixture.path()).expect("canonical temporary project");
        let mut config = terminal_config();
        config.project_dir = project_dir.clone();
        config.command = command.to_string_lossy().into_owned();
        let broker = PeerBroker::new(
            "127.0.0.1:43123"
                .parse::<SocketAddr>()
                .expect("loopback peer endpoint"),
            PathBuf::from("codex-web"),
        );
        let registry = SessionRegistry::with_agent_configs_and_peer_broker(
            config.clone(),
            config,
            Vec::new(),
            broker.clone(),
        );

        registry.start_primary().await.expect("start source");
        let source = registry.primary().snapshot();
        let thread = broker
            .create_thread(
                source.terminal_id,
                AgentKind::Codex,
                PeerAction::Review,
                "Review the completed work.".to_owned(),
            )
            .expect("create peer thread");
        let provisioning = broker
            .begin_reviewer_provisioning(thread.id)
            .expect("begin reviewer provisioning");
        let reviewer_terminal_id = provisioning
            .thread()
            .reviewer_terminal_id
            .expect("reserved reviewer identity");
        let reviewer = registry
            .create_peer_in(
                AgentKind::Codex,
                project_dir,
                thread.id,
                source.terminal_id,
                source.session_id.expect("running source generation"),
                reviewer_terminal_id,
            )
            .await
            .expect("start reviewer");
        broker
            .complete_reviewer_provisioning(provisioning)
            .expect("complete reviewer provisioning");
        let reviewer_manager = registry
            .get(reviewer_terminal_id)
            .expect("registered reviewer");
        reviewer_manager.fail_next_termination_for_test();

        let closing = broker.begin_close(thread.id).expect("begin first close");
        assert!(matches!(
            registry
                .delete_peer(
                    reviewer_terminal_id,
                    thread.id,
                    source.terminal_id,
                    reviewer.session_id,
                )
                .await,
            Err(RegistryError::OperationFailed(_))
        ));
        broker.abort_close(closing).expect("abort failed close");

        assert!(registry.get(reviewer_terminal_id).is_some());
        assert!(reviewer_manager.is_running());
        assert!(!broker.has_active_session(
            reviewer_terminal_id,
            reviewer.session_id.expect("reviewer generation")
        ));
        assert!(broker.get_thread(thread.id).is_ok());

        let closing = broker.begin_close(thread.id).expect("begin retry close");
        registry
            .delete_peer(
                reviewer_terminal_id,
                thread.id,
                source.terminal_id,
                reviewer.session_id,
            )
            .await
            .expect("retry exact reviewer deletion");
        broker
            .finalize_close(closing)
            .expect("finalize retry close");

        assert!(registry.get(reviewer_terminal_id).is_none());
        assert!(broker.get_thread(thread.id).is_err());
        registry.shutdown().await;
    }

    #[tokio::test]
    async fn refuses_to_delete_the_primary_terminal() {
        let registry = SessionRegistry::new(terminal_config());
        let primary_id = registry.primary().snapshot().terminal_id;

        assert!(matches!(
            registry.delete(primary_id).await,
            Err(RegistryError::PrimaryCannotBeDeleted)
        ));
        assert_eq!(registry.session_count(), 1);
    }

    #[tokio::test]
    async fn deleting_a_terminal_cancels_its_connections() {
        let registry = SessionRegistry::new(terminal_config());
        let session = registry.reserve_session(None).expect("reserved session");
        let terminal_id = session.snapshot().terminal_id;
        let shutdown_signal = session.shutdown_signal();

        registry
            .delete(terminal_id)
            .await
            .expect("terminal deleted");

        assert!(shutdown_signal.is_cancelled());
        assert!(registry.get(terminal_id).is_none());
    }

    #[tokio::test]
    async fn failed_create_rolls_back_the_reserved_terminal() {
        let directory = tempfile::tempdir().expect("temporary project");
        let mut config = terminal_config();
        config.project_dir = directory.path().to_path_buf();
        config.command = directory
            .path()
            .join("missing-agent-command")
            .to_string_lossy()
            .into_owned();
        let registry = SessionRegistry::new(config);

        assert!(matches!(
            registry.create(None).await,
            Err(RegistryError::OperationFailed(_))
        ));
        assert_eq!(registry.session_count(), 1);
        assert_eq!(registry.list().len(), 1);
    }

    #[tokio::test]
    async fn selected_project_directory_is_the_native_pty_working_directory() {
        let fixture = tempfile::tempdir().expect("temporary project");
        let selected = fixture.path().join("selected");
        std::fs::create_dir(&selected).expect("create selected directory");
        let selected = dunce::canonicalize(selected).expect("canonical selected directory");
        let command = write_working_directory_fixture(fixture.path());
        let mut config = terminal_config();
        config.project_dir = fixture.path().to_path_buf();
        config.command = command.to_string_lossy().into_owned();
        let registry = SessionRegistry::new(config);

        let snapshot = registry
            .create_in(None, Some(selected.clone()))
            .await
            .expect("start fixture in selected directory");
        let session = registry
            .get(snapshot.terminal_id)
            .expect("created session remains registered");
        let deadline = Instant::now() + Duration::from_secs(3);
        let mut answered_cursor_query = false;
        let output = loop {
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
            if String::from_utf8_lossy(&output).contains(&selected.to_string_lossy().into_owned())
                || Instant::now() >= deadline
            {
                break String::from_utf8_lossy(&output).into_owned();
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        };

        assert!(
            output.contains(&selected.to_string_lossy().into_owned()),
            "PTY output did not contain selected cwd; output={output:?}"
        );
        registry.shutdown().await;
    }

    #[tokio::test]
    async fn stale_workspace_restart_does_not_terminate_the_running_session() {
        let fixture = tempfile::tempdir().expect("temporary project");
        let selected = fixture.path().join("selected");
        std::fs::create_dir(&selected).expect("create selected directory");
        let selected = dunce::canonicalize(selected).expect("canonical selected directory");
        let command = write_long_running_fixture(fixture.path());
        let mut config = terminal_config();
        config.project_dir = selected.clone();
        config.command = command.to_string_lossy().into_owned();
        let registry = SessionRegistry::new(config);
        let session = registry.primary();

        registry.start_primary().await.expect("start fixture");
        wait_for_output(&session, "WORKSPACE-READY").await;
        let before = session.snapshot();
        std::fs::remove_dir(&selected).expect("remove stale selected directory");

        let error = registry
            .restart(before.terminal_id)
            .await
            .expect_err("stale workspace must reject restart");
        let after = session.snapshot();

        assert!(
            error
                .to_string()
                .contains("configured project directory is no longer")
        );
        assert_eq!(after.status, crate::session::Lifecycle::Running);
        assert_eq!(after.session_id, before.session_id);
        assert_eq!(after.pid, before.pid);
        registry.shutdown().await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn symlink_swapped_workspace_restart_preserves_the_running_session() {
        use std::os::unix::fs::symlink;

        let fixture = tempfile::tempdir().expect("temporary project");
        let selected = fixture.path().join("selected");
        let replacement = fixture.path().join("replacement");
        let original = fixture.path().join("selected-original");
        std::fs::create_dir(&selected).expect("create selected directory");
        std::fs::create_dir(&replacement).expect("create replacement directory");
        let selected = dunce::canonicalize(selected).expect("canonical selected directory");
        let replacement =
            dunce::canonicalize(replacement).expect("canonical replacement directory");
        let command = write_long_running_fixture(fixture.path());
        let mut config = terminal_config();
        config.project_dir = selected.clone();
        config.command = command.to_string_lossy().into_owned();
        let registry = SessionRegistry::new(config);
        let session = registry.primary();

        registry.start_primary().await.expect("start fixture");
        wait_for_output(&session, "WORKSPACE-READY").await;
        let before = session.snapshot();
        std::fs::rename(&selected, &original).expect("move original selected directory");
        symlink(&replacement, &selected).expect("replace selected directory with symlink");
        assert_eq!(
            dunce::canonicalize(&selected).expect("resolve swapped workspace"),
            replacement
        );

        registry
            .restart(before.terminal_id)
            .await
            .expect_err("swapped workspace must reject restart");
        let after = session.snapshot();

        assert_eq!(after.status, crate::session::Lifecycle::Running);
        assert_eq!(after.session_id, before.session_id);
        assert_eq!(after.pid, before.pid);
        registry.shutdown().await;
    }

    async fn wait_for_output(session: &SessionManager, expected: &str) {
        let deadline = Instant::now() + Duration::from_secs(3);
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
                "PTY output did not contain {expected:?}; output={:?}",
                String::from_utf8_lossy(&output)
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    #[cfg(windows)]
    fn write_long_running_fixture(directory: &Path) -> PathBuf {
        let command = directory.join("long-running-agent.cmd");
        std::fs::write(
            &command,
            "@echo off\r\nif \"%~1\"==\"--version\" (\r\n  echo codex 1.2.3\r\n  exit /b 0\r\n)\r\ncd /d \"%TEMP%\"\r\necho WORKSPACE-READY\r\n:loop\r\nping -n 2 127.0.0.1 >nul\r\ngoto loop\r\n",
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
            "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then\n  echo 'codex 1.2.3'\n  exit 0\nfi\ncd /\nprintf 'WORKSPACE-READY\\n'\nwhile :; do sleep 1; done\n",
        )
        .expect("write Unix fixture");
        let mut permissions = std::fs::metadata(&command)
            .expect("fixture metadata")
            .permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(&command, permissions).expect("make fixture executable");
        command
    }

    #[cfg(windows)]
    fn write_working_directory_fixture(directory: &Path) -> PathBuf {
        let command = directory.join("cwd-agent.cmd");
        std::fs::write(
            &command,
            "@echo off\r\nif \"%~1\"==\"--version\" (\r\n  echo codex 1.2.3\r\n  exit /b 0\r\n)\r\ncd\r\n",
        )
        .expect("write Windows fixture");
        command
    }

    #[cfg(unix)]
    fn write_working_directory_fixture(directory: &Path) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;

        let command = directory.join("cwd-agent");
        std::fs::write(
            &command,
            "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then\n  echo 'codex 1.2.3'\n  exit 0\nfi\npwd\n",
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
