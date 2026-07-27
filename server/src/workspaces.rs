use std::{
    collections::HashSet,
    env, fmt,
    fs::OpenOptions,
    io::{ErrorKind, Read, Write},
    path::{Component, Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use uuid::Uuid;

#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt as _, MetadataExt as _, OpenOptionsExt};
#[cfg(windows)]
use std::os::windows::fs::{MetadataExt as _, OpenOptionsExt as _};
#[cfg(windows)]
use windows_sys::Win32::Storage::FileSystem::{
    FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_OPEN_REPARSE_POINT,
};

use crate::{
    config::AgentKind,
    filesystem::{DirectoryEntry, decode_directory_id},
};

pub const WORKSPACE_STATE_VERSION: u32 = 1;
pub const MAX_FAVORITES: usize = 100;
pub const MAX_RECENT_WORKSPACES: usize = 30;
pub const MAX_STATE_FILE_BYTES: u64 = 32 * 1024 * 1024;
const MAX_JAVASCRIPT_SAFE_INTEGER: u64 = 9_007_199_254_740_991;
const MAX_FAVORITE_LABEL_CHARS: usize = 120;
const STATE_FILE_NAME: &str = "workspaces.json";

#[derive(Debug)]
pub enum WorkspaceError {
    InvalidLabel,
    StateTooLarge,
    FavoriteLimitReached,
    FavoriteNotFound,
    UnsafeStateLocation(&'static str),
    UnsupportedVersion(u32),
    InvalidState(&'static str),
    Io(std::io::Error),
    Json(serde_json::Error),
    Join(tokio::task::JoinError),
}

impl fmt::Display for WorkspaceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidLabel => write!(formatter, "the favorite label is invalid"),
            Self::StateTooLarge => write!(
                formatter,
                "workspace state exceeds the {MAX_STATE_FILE_BYTES} byte limit"
            ),
            Self::FavoriteLimitReached => write!(formatter, "the favorite limit has been reached"),
            Self::FavoriteNotFound => write!(formatter, "the favorite was not found"),
            Self::UnsafeStateLocation(reason) => {
                write!(
                    formatter,
                    "the workspace state location is unsafe: {reason}"
                )
            }
            Self::UnsupportedVersion(version) => {
                write!(formatter, "unsupported workspace state version {version}")
            }
            Self::InvalidState(reason) => write!(formatter, "invalid workspace state: {reason}"),
            Self::Io(error) => write!(formatter, "workspace state I/O failed: {error}"),
            Self::Json(error) => write!(formatter, "workspace state JSON is invalid: {error}"),
            Self::Join(error) => write!(formatter, "workspace state task failed: {error}"),
        }
    }
}

impl std::error::Error for WorkspaceError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Json(error) => Some(error),
            Self::Join(error) => Some(error),
            _ => None,
        }
    }
}

impl From<std::io::Error> for WorkspaceError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<serde_json::Error> for WorkspaceError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[serde(rename_all = "camelCase")]
pub struct FavoriteWorkspace {
    pub id: Uuid,
    pub directory_id: String,
    pub name: String,
    pub path: String,
    pub label: Option<String>,
    pub preferred_agent: Option<AgentKind>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[serde(rename_all = "camelCase")]
pub struct RecentWorkspace {
    pub directory_id: String,
    pub name: String,
    pub path: String,
    pub last_agent: AgentKind,
    pub last_opened_at: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceLibrary {
    pub version: u32,
    pub favorites: Vec<FavoriteWorkspace>,
    pub recent: Vec<RecentWorkspace>,
}

impl Default for WorkspaceLibrary {
    fn default() -> Self {
        Self {
            version: WORKSPACE_STATE_VERSION,
            favorites: Vec::new(),
            recent: Vec::new(),
        }
    }
}

#[derive(Clone)]
pub struct WorkspaceStore {
    state_file: PathBuf,
    state: std::sync::Arc<Mutex<WorkspaceLibrary>>,
}

enum StateFileRead {
    Missing,
    Oversized,
    Bytes(Vec<u8>),
}

impl WorkspaceStore {
    pub async fn open(state_directory: PathBuf) -> Result<Self, WorkspaceError> {
        prepare_state_directory(&state_directory).await?;

        let state_file = state_directory.join(STATE_FILE_NAME);
        let state = match read_state_file(state_file.clone()).await? {
            StateFileRead::Missing => WorkspaceLibrary::default(),
            StateFileRead::Oversized => {
                quarantine_invalid_state(&state_file).await?;
                tracing::warn!(
                    maximum_bytes = MAX_STATE_FILE_BYTES,
                    "workspace state exceeded its size limit and has been quarantined"
                );
                WorkspaceLibrary::default()
            }
            StateFileRead::Bytes(bytes) => match decode_state(&bytes) {
                Ok(state) => state,
                Err(error) => {
                    quarantine_invalid_state(&state_file).await?;
                    tracing::warn!(
                        %error,
                        "workspace state was invalid and has been quarantined"
                    );
                    WorkspaceLibrary::default()
                }
            },
        };

        Ok(Self {
            state_file,
            state: std::sync::Arc::new(Mutex::new(state)),
        })
    }

    pub async fn snapshot(&self) -> WorkspaceLibrary {
        self.state.lock().await.clone()
    }

    pub async fn upsert_favorite(
        &self,
        directory: DirectoryEntry,
        label: Option<String>,
        preferred_agent: Option<AgentKind>,
    ) -> Result<FavoriteWorkspace, WorkspaceError> {
        let label = normalize_label(label)?;
        let mut guard = self.state.lock().await;
        let mut next = guard.clone();

        let favorite = if let Some(existing) = next
            .favorites
            .iter_mut()
            .find(|favorite| favorite.directory_id == directory.id)
        {
            existing.name = directory.name;
            existing.path = directory.path;
            existing.label = label;
            existing.preferred_agent = preferred_agent;
            existing.clone()
        } else {
            if next.favorites.len() >= MAX_FAVORITES {
                return Err(WorkspaceError::FavoriteLimitReached);
            }
            let favorite = FavoriteWorkspace {
                id: Uuid::new_v4(),
                directory_id: directory.id,
                name: directory.name,
                path: directory.path,
                label,
                preferred_agent,
            };
            next.favorites.push(favorite.clone());
            favorite
        };

        self.persist(&next).await?;
        *guard = next;
        Ok(favorite)
    }

    pub async fn delete_favorite(&self, favorite_id: Uuid) -> Result<(), WorkspaceError> {
        let mut guard = self.state.lock().await;
        let mut next = guard.clone();
        let original_len = next.favorites.len();
        next.favorites.retain(|favorite| favorite.id != favorite_id);
        if next.favorites.len() == original_len {
            return Err(WorkspaceError::FavoriteNotFound);
        }

        self.persist(&next).await?;
        *guard = next;
        Ok(())
    }

    pub async fn record_recent(
        &self,
        directory: DirectoryEntry,
        agent: AgentKind,
    ) -> Result<(), WorkspaceError> {
        let mut guard = self.state.lock().await;
        let mut next = guard.clone();

        for favorite in &mut next.favorites {
            if favorite.directory_id == directory.id {
                favorite.name.clone_from(&directory.name);
                favorite.path.clone_from(&directory.path);
                favorite.preferred_agent = Some(agent);
            }
        }

        next.recent
            .retain(|recent| recent.directory_id != directory.id);
        next.recent.insert(
            0,
            RecentWorkspace {
                directory_id: directory.id,
                name: directory.name,
                path: directory.path,
                last_agent: agent,
                last_opened_at: unix_time_millis(),
            },
        );
        next.recent.truncate(MAX_RECENT_WORKSPACES);

        self.persist(&next).await?;
        *guard = next;
        Ok(())
    }

    async fn persist(&self, state: &WorkspaceLibrary) -> Result<(), WorkspaceError> {
        validate_state(state)?;
        let mut bytes = serde_json::to_vec_pretty(state)?;
        bytes.push(b'\n');
        if bytes.len() as u64 > MAX_STATE_FILE_BYTES {
            return Err(WorkspaceError::StateTooLarge);
        }
        let state_file = self.state_file.clone();
        tokio::task::spawn_blocking(move || atomic_write(&state_file, &bytes))
            .await
            .map_err(WorkspaceError::Join)??;
        Ok(())
    }

    #[cfg(test)]
    fn state_file(&self) -> &Path {
        &self.state_file
    }
}

async fn read_state_file(state_file: PathBuf) -> Result<StateFileRead, WorkspaceError> {
    tokio::task::spawn_blocking(move || read_state_file_blocking(&state_file))
        .await
        .map_err(WorkspaceError::Join)?
}

fn read_state_file_blocking(state_file: &Path) -> Result<StateFileRead, WorkspaceError> {
    let path_metadata = match std::fs::symlink_metadata(state_file) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == ErrorKind::NotFound => {
            return Ok(StateFileRead::Missing);
        }
        Err(error) => return Err(WorkspaceError::Io(error)),
    };
    validate_existing_state_file(&path_metadata)?;

    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    options.custom_flags(libc::O_NOFOLLOW);
    #[cfg(windows)]
    options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);

    let mut file = match options.open(state_file) {
        Ok(file) => file,
        Err(error) if error.kind() == ErrorKind::NotFound => {
            return Ok(StateFileRead::Missing);
        }
        Err(error) => return Err(WorkspaceError::Io(error)),
    };
    let opened_metadata = file.metadata()?;
    validate_existing_state_file(&opened_metadata)?;
    validate_state_file_identity(&path_metadata, &opened_metadata)?;

    if opened_metadata.len() > MAX_STATE_FILE_BYTES {
        return Ok(StateFileRead::Oversized);
    }

    match read_bounded(&mut file, opened_metadata.len(), MAX_STATE_FILE_BYTES)? {
        Some(bytes) => Ok(StateFileRead::Bytes(bytes)),
        None => Ok(StateFileRead::Oversized),
    }
}

fn read_bounded(
    reader: &mut impl Read,
    length_hint: u64,
    maximum_bytes: u64,
) -> Result<Option<Vec<u8>>, WorkspaceError> {
    let capacity = usize::try_from(length_hint.min(maximum_bytes)).unwrap_or(0);
    let mut bytes = Vec::with_capacity(capacity);
    reader
        .take(maximum_bytes.saturating_add(1))
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > maximum_bytes {
        Ok(None)
    } else {
        Ok(Some(bytes))
    }
}

#[cfg(unix)]
fn validate_state_file_identity(
    path_metadata: &std::fs::Metadata,
    opened_metadata: &std::fs::Metadata,
) -> Result<(), WorkspaceError> {
    if path_metadata.dev() != opened_metadata.dev() || path_metadata.ino() != opened_metadata.ino()
    {
        return Err(WorkspaceError::UnsafeStateLocation(
            "the state file changed while it was being opened",
        ));
    }
    Ok(())
}

#[cfg(not(unix))]
fn validate_state_file_identity(
    _path_metadata: &std::fs::Metadata,
    _opened_metadata: &std::fs::Metadata,
) -> Result<(), WorkspaceError> {
    Ok(())
}

fn decode_state(bytes: &[u8]) -> Result<WorkspaceLibrary, WorkspaceError> {
    let state: WorkspaceLibrary = serde_json::from_slice(bytes)?;
    validate_state(&state)?;
    Ok(state)
}

async fn quarantine_invalid_state(state_file: &Path) -> Result<(), WorkspaceError> {
    let parent = state_file
        .parent()
        .ok_or(WorkspaceError::InvalidState("state file has no parent"))?;
    let quarantine = parent.join(format!("workspaces.corrupt.{}.json", Uuid::new_v4()));
    tokio::fs::rename(state_file, quarantine).await?;
    Ok(())
}

async fn prepare_state_directory(path: &Path) -> Result<(), WorkspaceError> {
    validate_state_directory_path(path)?;

    match tokio::fs::symlink_metadata(path).await {
        Ok(metadata) => validate_existing_state_directory(path, &metadata),
        Err(error) if error.kind() == ErrorKind::NotFound => {
            let parent = path.parent().ok_or(WorkspaceError::UnsafeStateLocation(
                "the state directory must have a parent",
            ))?;
            tokio::fs::create_dir_all(parent).await?;
            let create_path = path.to_path_buf();
            match tokio::task::spawn_blocking(move || create_private_directory(&create_path)).await
            {
                Ok(Ok(())) => {}
                Ok(Err(error)) if error.kind() == ErrorKind::AlreadyExists => {}
                Ok(Err(error)) => return Err(WorkspaceError::Io(error)),
                Err(error) => return Err(WorkspaceError::Join(error)),
            }
            let metadata = tokio::fs::symlink_metadata(path).await?;
            validate_existing_state_directory(path, &metadata)
        }
        Err(error) => Err(WorkspaceError::Io(error)),
    }
}

fn validate_state_directory_path(path: &Path) -> Result<(), WorkspaceError> {
    if !path.is_absolute() {
        return Err(WorkspaceError::UnsafeStateLocation(
            "the state directory must be absolute",
        ));
    }
    if path.parent().is_none()
        || !matches!(path.components().next_back(), Some(Component::Normal(_)))
    {
        return Err(WorkspaceError::UnsafeStateLocation(
            "filesystem roots and non-dedicated paths are not allowed",
        ));
    }
    Ok(())
}

fn validate_existing_state_directory(
    path: &Path,
    metadata: &std::fs::Metadata,
) -> Result<(), WorkspaceError> {
    if is_link_or_reparse(metadata) {
        return Err(WorkspaceError::UnsafeStateLocation(
            "the state directory must not be a symlink or reparse point",
        ));
    }
    if !metadata.file_type().is_dir() {
        return Err(WorkspaceError::UnsafeStateLocation(
            "the state path is not a directory",
        ));
    }
    if is_known_broad_directory(path) {
        return Err(WorkspaceError::UnsafeStateLocation(
            "a dedicated application state directory is required",
        ));
    }
    validate_private_owner_permissions(metadata, true)
}

fn validate_existing_state_file(metadata: &std::fs::Metadata) -> Result<(), WorkspaceError> {
    if is_link_or_reparse(metadata) {
        return Err(WorkspaceError::UnsafeStateLocation(
            "the state file must not be a symlink or reparse point",
        ));
    }
    if !metadata.file_type().is_file() {
        return Err(WorkspaceError::UnsafeStateLocation(
            "the state file is not a regular file",
        ));
    }
    validate_private_owner_permissions(metadata, false)
}

#[cfg(unix)]
fn validate_private_owner_permissions(
    metadata: &std::fs::Metadata,
    is_directory: bool,
) -> Result<(), WorkspaceError> {
    // SAFETY: geteuid takes no pointers and has no preconditions.
    if metadata.uid() != unsafe { libc::geteuid() } {
        return Err(WorkspaceError::UnsafeStateLocation(
            "the existing state target is not owned by the current user",
        ));
    }
    if metadata.mode() & 0o077 != 0 {
        return Err(WorkspaceError::UnsafeStateLocation(if is_directory {
            "the existing state directory grants group or other permissions"
        } else {
            "the existing state file grants group or other permissions"
        }));
    }
    Ok(())
}

#[cfg(not(unix))]
fn validate_private_owner_permissions(
    _metadata: &std::fs::Metadata,
    _is_directory: bool,
) -> Result<(), WorkspaceError> {
    Ok(())
}

#[cfg(unix)]
fn create_private_directory(path: &Path) -> std::io::Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    builder.mode(0o700);
    builder.create(path)
}

#[cfg(not(unix))]
fn create_private_directory(path: &Path) -> std::io::Result<()> {
    std::fs::create_dir(path)
}

#[cfg(windows)]
fn is_link_or_reparse(metadata: &std::fs::Metadata) -> bool {
    metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

#[cfg(not(windows))]
fn is_link_or_reparse(metadata: &std::fs::Metadata) -> bool {
    metadata.file_type().is_symlink()
}

fn is_known_broad_directory(path: &Path) -> bool {
    let candidate = dunce::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let mut known = vec![env::temp_dir()];
    if let Ok(current) = env::current_dir() {
        known.push(current);
    }
    for variable in [
        "HOME",
        "XDG_STATE_HOME",
        "USERPROFILE",
        "LOCALAPPDATA",
        "APPDATA",
        "PROGRAMDATA",
        "SYSTEMROOT",
    ] {
        if let Some(value) = env::var_os(variable) {
            known.push(PathBuf::from(value));
        }
    }

    known.into_iter().any(|known| {
        let known = dunce::canonicalize(&known).unwrap_or(known);
        paths_equal(&candidate, &known)
    })
}

#[cfg(windows)]
fn paths_equal(left: &Path, right: &Path) -> bool {
    left.to_string_lossy()
        .eq_ignore_ascii_case(&right.to_string_lossy())
}

#[cfg(not(windows))]
fn paths_equal(left: &Path, right: &Path) -> bool {
    left == right
}

fn validate_state(state: &WorkspaceLibrary) -> Result<(), WorkspaceError> {
    if state.version != WORKSPACE_STATE_VERSION {
        return Err(WorkspaceError::UnsupportedVersion(state.version));
    }
    if state.favorites.len() > MAX_FAVORITES {
        return Err(WorkspaceError::InvalidState("too many favorites"));
    }
    if state.recent.len() > MAX_RECENT_WORKSPACES {
        return Err(WorkspaceError::InvalidState("too many recent workspaces"));
    }

    let mut favorite_ids = HashSet::new();
    let mut favorite_directories = HashSet::new();
    for favorite in &state.favorites {
        if !favorite_ids.insert(favorite.id) {
            return Err(WorkspaceError::InvalidState("duplicate favorite ID"));
        }
        if !favorite_directories.insert(favorite.directory_id.as_str()) {
            return Err(WorkspaceError::InvalidState("duplicate favorite directory"));
        }
        decode_directory_id(&favorite.directory_id)
            .map_err(|_| WorkspaceError::InvalidState("invalid favorite directory ID"))?;
        // Native paths may need lossy display conversion (notably non-UTF-8
        // Unix names), so match the frontend contract without trimming or
        // attempting to reconstruct a native path from these display fields.
        if favorite.name.is_empty() {
            return Err(WorkspaceError::InvalidState("empty favorite name"));
        }
        if favorite.path.is_empty() {
            return Err(WorkspaceError::InvalidState("empty favorite path"));
        }
        normalize_label(favorite.label.clone())?;
    }

    let mut recent_directories = HashSet::new();
    for recent in &state.recent {
        if !recent_directories.insert(recent.directory_id.as_str()) {
            return Err(WorkspaceError::InvalidState("duplicate recent directory"));
        }
        decode_directory_id(&recent.directory_id)
            .map_err(|_| WorkspaceError::InvalidState("invalid recent directory ID"))?;
        if recent.name.is_empty() {
            return Err(WorkspaceError::InvalidState("empty recent name"));
        }
        if recent.path.is_empty() {
            return Err(WorkspaceError::InvalidState("empty recent path"));
        }
        if recent.last_opened_at > MAX_JAVASCRIPT_SAFE_INTEGER {
            return Err(WorkspaceError::InvalidState(
                "recent timestamp exceeds the JavaScript safe-integer range",
            ));
        }
    }
    Ok(())
}

fn normalize_label(label: Option<String>) -> Result<Option<String>, WorkspaceError> {
    let Some(label) = label else {
        return Ok(None);
    };
    let label = label.trim();
    if label.is_empty() {
        return Ok(None);
    }
    if label.chars().count() > MAX_FAVORITE_LABEL_CHARS || label.contains(['\r', '\n', '\0']) {
        return Err(WorkspaceError::InvalidLabel);
    }
    Ok(Some(label.to_owned()))
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), WorkspaceError> {
    let parent = path
        .parent()
        .ok_or(WorkspaceError::InvalidState("state file has no parent"))?;
    let file_name = path
        .file_name()
        .ok_or(WorkspaceError::InvalidState("state file has no name"))?
        .to_string_lossy();
    let temporary = parent.join(format!(".{file_name}.{}.tmp", Uuid::new_v4()));

    let result = (|| -> Result<(), WorkspaceError> {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        options.mode(0o600);
        let mut file = options.open(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        replace_file(&temporary, path)?;
        sync_parent_directory(parent)?;
        Ok(())
    })();

    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

fn replace_file(source: &Path, destination: &Path) -> Result<(), WorkspaceError> {
    // `std::fs::rename` preserves native path handling (including Windows
    // verbatim/long paths) and replaces an existing destination on supported
    // same-filesystem platforms. The temporary file is always created beside
    // the destination, so the operation remains an atomic same-volume replace.
    std::fs::rename(source, destination)?;
    Ok(())
}

#[cfg(unix)]
fn sync_parent_directory(parent: &Path) -> Result<(), WorkspaceError> {
    std::fs::File::open(parent)?.sync_all()?;
    Ok(())
}

#[cfg(not(unix))]
fn sync_parent_directory(_parent: &Path) -> Result<(), WorkspaceError> {
    Ok(())
}

fn unix_time_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(MAX_JAVASCRIPT_SAFE_INTEGER)
        .min(MAX_JAVASCRIPT_SAFE_INTEGER)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::filesystem::encode_directory_id;
    use serde_json::json;

    fn directory(path: &Path) -> DirectoryEntry {
        DirectoryEntry {
            id: encode_directory_id(path),
            name: path
                .file_name()
                .unwrap_or(path.as_os_str())
                .to_string_lossy()
                .into_owned(),
            path: path.to_string_lossy().into_owned(),
        }
    }

    #[tokio::test]
    async fn favorites_persist_replace_existing_state_and_reopen() {
        let fixture = tempfile::tempdir().expect("temporary state");
        let project = dunce::canonicalize(fixture.path()).expect("canonical project");
        let state_dir = fixture.path().join("state");
        let store = WorkspaceStore::open(state_dir.clone())
            .await
            .expect("open store");

        let first = store
            .upsert_favorite(
                directory(&project),
                Some("  Main project  ".to_owned()),
                None,
            )
            .await
            .expect("create favorite");
        let first_serialized =
            std::fs::read(store.state_file()).expect("read the first persisted state");
        let updated = store
            .upsert_favorite(
                directory(&project),
                Some("Updated".to_owned()),
                Some(AgentKind::Claude),
            )
            .await
            .expect("update favorite");
        let updated_serialized =
            std::fs::read(store.state_file()).expect("read the replaced state");

        assert_eq!(updated.id, first.id);
        assert_eq!(updated.label.as_deref(), Some("Updated"));
        assert_eq!(updated.preferred_agent, Some(AgentKind::Claude));
        assert_ne!(updated_serialized, first_serialized);
        assert!(store.state_file().is_file());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            assert_eq!(
                std::fs::metadata(&state_dir)
                    .expect("state directory metadata")
                    .permissions()
                    .mode()
                    & 0o777,
                0o700
            );
            assert_eq!(
                std::fs::metadata(store.state_file())
                    .expect("state file metadata")
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
        assert!(
            std::fs::read_dir(&state_dir)
                .expect("read state directory")
                .all(|entry| !entry
                    .expect("state entry")
                    .file_name()
                    .to_string_lossy()
                    .ends_with(".tmp"))
        );

        let reopened = WorkspaceStore::open(state_dir)
            .await
            .expect("reopen store")
            .snapshot()
            .await;
        assert_eq!(reopened.favorites, vec![updated]);
    }

    #[tokio::test]
    async fn recents_are_deduplicated_bounded_and_update_favorite_agent() {
        let fixture = tempfile::tempdir().expect("temporary state");
        let store = WorkspaceStore::open(fixture.path().join("state"))
            .await
            .expect("open store");
        let favorite_path = absolute_test_path("favorite");
        store
            .upsert_favorite(directory(&favorite_path), None, None)
            .await
            .expect("create favorite");

        store
            .record_recent(directory(&favorite_path), AgentKind::Agy)
            .await
            .expect("record favorite");
        store
            .record_recent(directory(&favorite_path), AgentKind::Claude)
            .await
            .expect("update favorite");
        for index in 0..MAX_RECENT_WORKSPACES + 5 {
            let path = absolute_test_path(&format!("project-{index}"));
            store
                .record_recent(directory(&path), AgentKind::Codex)
                .await
                .expect("record recent");
        }

        let state = store.snapshot().await;
        assert_eq!(state.recent.len(), MAX_RECENT_WORKSPACES);
        assert_eq!(
            state.recent[0].name,
            format!("project-{}", MAX_RECENT_WORKSPACES + 4)
        );
        assert_eq!(state.favorites[0].preferred_agent, Some(AgentKind::Claude));
        assert_eq!(
            state
                .recent
                .iter()
                .map(|recent| recent.directory_id.as_str())
                .collect::<HashSet<_>>()
                .len(),
            state.recent.len()
        );
    }

    #[tokio::test]
    async fn favorite_delete_is_persisted_and_unknown_ids_are_rejected() {
        let fixture = tempfile::tempdir().expect("temporary state");
        let state_dir = fixture.path().join("state");
        let store = WorkspaceStore::open(state_dir.clone())
            .await
            .expect("open store");
        let favorite = store
            .upsert_favorite(directory(&absolute_test_path("favorite")), None, None)
            .await
            .expect("create favorite");

        assert!(matches!(
            store.delete_favorite(Uuid::new_v4()).await,
            Err(WorkspaceError::FavoriteNotFound)
        ));
        store
            .delete_favorite(favorite.id)
            .await
            .expect("delete favorite");

        let reopened = WorkspaceStore::open(state_dir)
            .await
            .expect("reopen store")
            .snapshot()
            .await;
        assert!(reopened.favorites.is_empty());
    }

    #[tokio::test]
    async fn unsupported_state_versions_are_quarantined_without_data_loss() {
        let fixture = tempfile::tempdir().expect("temporary state");
        let state_dir = fixture.path().join("state");
        create_private_test_directory(&state_dir);
        let state_file = state_dir.join(STATE_FILE_NAME);
        write_private_test_file(&state_file, r#"{"version":2,"favorites":[],"recent":[]}"#);

        let store = WorkspaceStore::open(state_dir.clone())
            .await
            .expect("future state is quarantined");

        assert_eq!(store.snapshot().await, WorkspaceLibrary::default());
        assert!(!state_file.exists());
        let quarantined = std::fs::read_dir(state_dir)
            .expect("read state directory")
            .map(|entry| entry.expect("state entry").path())
            .find(|path| {
                path.file_name()
                    .is_some_and(|name| name.to_string_lossy().starts_with("workspaces.corrupt."))
            })
            .expect("quarantined state");
        assert_eq!(
            std::fs::read_to_string(quarantined).expect("quarantine remains"),
            r#"{"version":2,"favorites":[],"recent":[]}"#
        );
    }

    #[tokio::test]
    async fn oversized_state_is_quarantined_before_json_parsing() {
        let fixture = tempfile::tempdir().expect("temporary state");
        let state_dir = fixture.path().join("state");
        create_private_test_directory(&state_dir);
        let state_file = state_dir.join(STATE_FILE_NAME);
        write_private_test_file(&state_file, vec![b' '; MAX_STATE_FILE_BYTES as usize + 1]);

        let store = WorkspaceStore::open(state_dir.clone())
            .await
            .expect("oversized state is quarantined");

        assert_eq!(store.snapshot().await, WorkspaceLibrary::default());
        assert!(!state_file.exists());
        assert!(
            std::fs::read_dir(state_dir)
                .expect("read state directory")
                .map(|entry| entry.expect("state entry").file_name())
                .any(|name| name.to_string_lossy().starts_with("workspaces.corrupt."))
        );
    }

    #[test]
    fn bounded_reader_stops_at_the_limit_plus_one_byte() {
        let mut exact = std::io::Cursor::new(vec![b'a'; 16]);
        let exact_bytes = read_bounded(&mut exact, 16, 16)
            .expect("bounded read")
            .expect("exact limit is accepted");
        assert_eq!(exact_bytes.len(), 16);
        assert_eq!(exact.position(), 16);

        let mut oversized = std::io::Cursor::new(vec![b'b'; 64]);
        assert!(
            read_bounded(&mut oversized, 0, 16)
                .expect("bounded read")
                .is_none()
        );
        assert_eq!(oversized.position(), 17);
    }

    #[tokio::test]
    async fn frontend_incompatible_workspace_dtos_are_quarantined() {
        let fixture = tempfile::tempdir().expect("temporary state");
        let favorite_path = absolute_test_path("favorite");
        let recent_path = absolute_test_path("recent");
        let valid = json!({
            "version": WORKSPACE_STATE_VERSION,
            "favorites": [{
                "id": Uuid::new_v4(),
                "directoryId": encode_directory_id(&favorite_path),
                "name": "favorite",
                "path": favorite_path.to_string_lossy(),
                "label": null,
                "preferredAgent": null
            }],
            "recent": [{
                "directoryId": encode_directory_id(&recent_path),
                "name": "recent",
                "path": recent_path.to_string_lossy(),
                "lastAgent": "codex",
                "lastOpenedAt": MAX_JAVASCRIPT_SAFE_INTEGER
            }]
        });
        decode_state(&serde_json::to_vec(&valid).expect("serialize valid state"))
            .expect("the JavaScript safe-integer boundary is accepted");

        let mut empty_favorite_name = valid.clone();
        empty_favorite_name["favorites"][0]["name"] = json!("");
        let mut empty_favorite_path = valid.clone();
        empty_favorite_path["favorites"][0]["path"] = json!("");
        let mut empty_recent_name = valid.clone();
        empty_recent_name["recent"][0]["name"] = json!("");
        let mut empty_recent_path = valid.clone();
        empty_recent_path["recent"][0]["path"] = json!("");
        let mut unsafe_recent_timestamp = valid;
        unsafe_recent_timestamp["recent"][0]["lastOpenedAt"] =
            json!(MAX_JAVASCRIPT_SAFE_INTEGER + 1);

        for (case_name, invalid_state) in [
            ("empty-favorite-name", empty_favorite_name),
            ("empty-favorite-path", empty_favorite_path),
            ("empty-recent-name", empty_recent_name),
            ("empty-recent-path", empty_recent_path),
            ("unsafe-recent-timestamp", unsafe_recent_timestamp),
        ] {
            let state_dir = fixture.path().join(case_name);
            create_private_test_directory(&state_dir);
            let state_file = state_dir.join(STATE_FILE_NAME);
            write_private_test_file(
                &state_file,
                serde_json::to_vec(&invalid_state).expect("serialize invalid state"),
            );

            let store = WorkspaceStore::open(state_dir.clone())
                .await
                .expect("invalid DTO is quarantined");

            assert_eq!(store.snapshot().await, WorkspaceLibrary::default());
            assert!(!state_file.exists());
            assert_eq!(
                std::fs::read_dir(state_dir)
                    .expect("read state directory")
                    .map(|entry| entry.expect("state entry").file_name())
                    .filter(|name| name.to_string_lossy().starts_with("workspaces.corrupt."))
                    .count(),
                1,
                "{case_name}"
            );
        }
    }

    #[tokio::test]
    async fn serialized_state_over_the_read_cap_is_rejected_before_replace() {
        let fixture = tempfile::tempdir().expect("temporary state");
        let store = WorkspaceStore::open(fixture.path().join("state"))
            .await
            .expect("open store");
        let mut state = WorkspaceLibrary::default();
        state.recent.push(RecentWorkspace {
            directory_id: encode_directory_id(&absolute_test_path("large")),
            name: "large".to_owned(),
            path: "x".repeat(MAX_STATE_FILE_BYTES as usize),
            last_agent: AgentKind::Codex,
            last_opened_at: 1,
        });

        assert!(matches!(
            store.persist(&state).await,
            Err(WorkspaceError::StateTooLarge)
        ));
        assert!(!store.state_file().exists());
        assert_eq!(store.snapshot().await, WorkspaceLibrary::default());
    }

    #[test]
    fn filesystem_roots_are_rejected_as_state_directories() {
        let root = if cfg!(windows) {
            PathBuf::from(r"C:\")
        } else {
            PathBuf::from("/")
        };

        assert!(matches!(
            validate_state_directory_path(&root),
            Err(WorkspaceError::UnsafeStateLocation(_))
        ));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn preexisting_public_directory_is_rejected_without_chmod() {
        use std::os::unix::fs::PermissionsExt;

        let fixture = tempfile::tempdir().expect("temporary state");
        let state_dir = fixture.path().join("public-state");
        std::fs::create_dir(&state_dir).expect("create public state directory");
        std::fs::set_permissions(&state_dir, std::fs::Permissions::from_mode(0o777))
            .expect("make public state directory");

        assert!(matches!(
            WorkspaceStore::open(state_dir.clone()).await,
            Err(WorkspaceError::UnsafeStateLocation(_))
        ));
        assert_eq!(
            std::fs::symlink_metadata(state_dir)
                .expect("state metadata")
                .permissions()
                .mode()
                & 0o777,
            0o777
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn state_directory_symlinks_are_rejected_without_touching_the_target() {
        use std::os::unix::fs::{PermissionsExt, symlink};

        let fixture = tempfile::tempdir().expect("temporary state");
        let target = fixture.path().join("target");
        create_private_test_directory(&target);
        let state_dir = fixture.path().join("state-link");
        symlink(&target, &state_dir).expect("create state directory symlink");

        assert!(matches!(
            WorkspaceStore::open(state_dir).await,
            Err(WorkspaceError::UnsafeStateLocation(_))
        ));
        assert_eq!(
            std::fs::metadata(target)
                .expect("target metadata")
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn state_file_symlinks_are_rejected_without_reading_the_target() {
        use std::os::unix::fs::symlink;

        let fixture = tempfile::tempdir().expect("temporary state");
        let state_dir = fixture.path().join("state");
        create_private_test_directory(&state_dir);
        let target = fixture.path().join("outside.json");
        write_private_test_file(&target, r#"{"version":1,"favorites":[],"recent":[]}"#);
        symlink(&target, state_dir.join(STATE_FILE_NAME)).expect("create state file symlink");

        assert!(matches!(
            WorkspaceStore::open(state_dir).await,
            Err(WorkspaceError::UnsafeStateLocation(_))
        ));
        assert_eq!(
            std::fs::read_to_string(target).expect("target remains readable"),
            r#"{"version":1,"favorites":[],"recent":[]}"#
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn preexisting_public_state_file_is_rejected_without_chmod() {
        use std::os::unix::fs::PermissionsExt;

        let fixture = tempfile::tempdir().expect("temporary state");
        let state_dir = fixture.path().join("state");
        create_private_test_directory(&state_dir);
        let state_file = state_dir.join(STATE_FILE_NAME);
        std::fs::write(&state_file, r#"{"version":1,"favorites":[],"recent":[]}"#)
            .expect("write state file");
        std::fs::set_permissions(&state_file, std::fs::Permissions::from_mode(0o644))
            .expect("make state file public");

        assert!(matches!(
            WorkspaceStore::open(state_dir).await,
            Err(WorkspaceError::UnsafeStateLocation(_))
        ));
        assert_eq!(
            std::fs::symlink_metadata(state_file)
                .expect("state file metadata")
                .permissions()
                .mode()
                & 0o777,
            0o644
        );
    }

    fn create_private_test_directory(path: &Path) {
        std::fs::create_dir(path).expect("create state directory");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
                .expect("secure state directory");
        }
    }

    fn write_private_test_file(path: &Path, bytes: impl AsRef<[u8]>) {
        std::fs::write(path, bytes).expect("write state file");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
                .expect("secure state file");
        }
    }

    fn absolute_test_path(name: &str) -> PathBuf {
        if cfg!(windows) {
            PathBuf::from(format!(r"C:\projects\{name}"))
        } else {
            PathBuf::from(format!("/projects/{name}"))
        }
    }
}
