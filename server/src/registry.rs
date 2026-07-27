use std::{
    collections::HashMap,
    fmt,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

use uuid::Uuid;

use crate::{
    config::AgentKind,
    session::{SessionManager, SessionSnapshot},
    terminal::TerminalConfig,
};

pub const MAX_SESSIONS: usize = 4;

#[derive(Debug)]
pub enum RegistryError {
    LimitReached,
    NotFound,
    PrimaryCannotBeDeleted,
    AgentUnavailable,
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
    state: Mutex<RegistryState>,
    shutting_down: AtomicBool,
}

struct RegistryState {
    primary_id: Uuid,
    next_terminal_number: usize,
    sessions: HashMap<Uuid, SessionManager>,
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
        let primary_id = Uuid::new_v4();
        let primary_name = format!("{} 1", primary_config.agent.label());
        let primary = SessionManager::new_managed(primary_config, primary_id, primary_name, true);
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
                state: Mutex::new(RegistryState {
                    primary_id,
                    next_terminal_number: 2,
                    sessions,
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
        let session = self.reserve_session(requested_agent)?;
        let terminal_id = session.snapshot().terminal_id;
        if let Err(error) = session.start().await {
            session.shutdown().await;
            lock(&self.inner.state).sessions.remove(&terminal_id);
            return Err(RegistryError::OperationFailed(error));
        }
        Ok(session.snapshot())
    }

    pub async fn restart(&self, terminal_id: Uuid) -> Result<(), RegistryError> {
        let session = self.get(terminal_id).ok_or(RegistryError::NotFound)?;
        session
            .restart()
            .await
            .map_err(RegistryError::OperationFailed)
    }

    pub async fn terminate(&self, terminal_id: Uuid) -> Result<(), RegistryError> {
        let session = self.get(terminal_id).ok_or(RegistryError::NotFound)?;
        session
            .terminate()
            .await
            .map_err(RegistryError::OperationFailed)
    }

    pub async fn delete(&self, terminal_id: Uuid) -> Result<(), RegistryError> {
        let session = {
            let state = lock(&self.inner.state);
            if terminal_id == state.primary_id {
                return Err(RegistryError::PrimaryCannotBeDeleted);
            }
            state
                .sessions
                .get(&terminal_id)
                .cloned()
                .ok_or(RegistryError::NotFound)?
        };

        session.shutdown().await;

        let mut state = lock(&self.inner.state);
        if state.sessions.remove(&terminal_id).is_none() {
            return Err(RegistryError::NotFound);
        }
        Ok(())
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

    pub fn available_agents(&self) -> Vec<AgentKind> {
        AgentKind::ALL
            .into_iter()
            .filter(|agent| self.inner.agent_configs.contains_key(agent))
            .collect()
    }

    fn reserve_session(
        &self,
        requested_agent: Option<AgentKind>,
    ) -> Result<SessionManager, RegistryError> {
        let mut state = lock(&self.inner.state);
        if self.inner.shutting_down.load(Ordering::SeqCst) {
            return Err(RegistryError::ShuttingDown);
        }
        if state.sessions.len() >= MAX_SESSIONS {
            return Err(RegistryError::LimitReached);
        }

        let terminal_id = Uuid::new_v4();
        let agent = requested_agent.unwrap_or(self.inner.default_new_agent);
        let terminal_config = self
            .inner
            .agent_configs
            .get(&agent)
            .cloned()
            .ok_or(RegistryError::AgentUnavailable)?;
        let name = format!("{} {}", agent.label(), state.next_terminal_number);
        state.next_terminal_number = state.next_terminal_number.saturating_add(1);
        let session = SessionManager::new_managed(terminal_config, terminal_id, name, false);
        state.sessions.insert(terminal_id, session.clone());
        Ok(session)
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
    use std::path::PathBuf;

    use super::*;
    use crate::config::ShellKind;

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
        assert_eq!(first.session_id, None);
        assert_eq!(registry.list().len(), 1);
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
        let registry = SessionRegistry::new(terminal_config());

        for _ in 1..MAX_SESSIONS {
            registry
                .reserve_session(None)
                .expect("session within limit");
        }

        let snapshots = registry.list();
        assert_eq!(snapshots.len(), MAX_SESSIONS);
        assert_eq!(
            snapshots
                .iter()
                .map(|snapshot| snapshot.terminal_id)
                .collect::<std::collections::HashSet<_>>()
                .len(),
            MAX_SESSIONS
        );
        assert!(matches!(
            registry.reserve_session(None),
            Err(RegistryError::LimitReached)
        ));
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
}
