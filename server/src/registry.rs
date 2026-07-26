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
    session::{SessionManager, SessionSnapshot},
    terminal::TerminalConfig,
};

pub const MAX_SESSIONS: usize = 4;

#[derive(Debug)]
pub enum RegistryError {
    LimitReached,
    NotFound,
    PrimaryCannotBeDeleted,
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
    new_session_config: TerminalConfig,
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
        let primary_id = Uuid::new_v4();
        let primary =
            SessionManager::new_managed(primary_config, primary_id, "Terminal 1".to_owned(), true);
        let mut sessions = HashMap::new();
        sessions.insert(primary_id, primary);

        Self {
            inner: Arc::new(RegistryInner {
                new_session_config,
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

    pub async fn create(&self) -> Result<SessionSnapshot, RegistryError> {
        let session = self.reserve_session()?;
        session
            .start()
            .await
            .map_err(RegistryError::OperationFailed)?;
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

    fn reserve_session(&self) -> Result<SessionManager, RegistryError> {
        let mut state = lock(&self.inner.state);
        if self.inner.shutting_down.load(Ordering::SeqCst) {
            return Err(RegistryError::ShuttingDown);
        }
        if state.sessions.len() >= MAX_SESSIONS {
            return Err(RegistryError::LimitReached);
        }

        let terminal_id = Uuid::new_v4();
        let name = format!("Terminal {}", state.next_terminal_number);
        state.next_terminal_number = state.next_terminal_number.saturating_add(1);
        let session = SessionManager::new_managed(
            self.inner.new_session_config.clone(),
            terminal_id,
            name,
            false,
        );
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
            shell: ShellKind::Powershell,
        }
    }

    #[test]
    fn creates_a_stable_primary_terminal() {
        let registry = SessionRegistry::new(terminal_config());
        let first = registry.primary().snapshot();
        let second = registry.primary().snapshot();

        assert_eq!(first.terminal_id, second.terminal_id);
        assert_eq!(first.name, "Terminal 1");
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
        let created = registry.reserve_session().expect("reserved session");

        assert_eq!(primary.configured_command(), "resume-current");
        assert_eq!(created.configured_command(), "codex");
    }

    #[test]
    fn reserves_unique_terminals_up_to_the_limit() {
        let registry = SessionRegistry::new(terminal_config());

        for _ in 1..MAX_SESSIONS {
            registry.reserve_session().expect("session within limit");
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
            registry.reserve_session(),
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
        let session = registry.reserve_session().expect("reserved session");
        let terminal_id = session.snapshot().terminal_id;
        let shutdown_signal = session.shutdown_signal();

        registry
            .delete(terminal_id)
            .await
            .expect("terminal deleted");

        assert!(shutdown_signal.is_cancelled());
        assert!(registry.get(terminal_id).is_none());
    }
}
