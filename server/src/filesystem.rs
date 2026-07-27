use std::{
    ffi::OsString,
    fmt,
    io::ErrorKind,
    path::{Path, PathBuf},
};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::Serialize;

#[cfg(unix)]
use std::os::unix::ffi::{OsStrExt, OsStringExt};
#[cfg(windows)]
use std::os::windows::ffi::{OsStrExt, OsStringExt};
#[cfg(windows)]
use windows_sys::Win32::Storage::FileSystem::GetLogicalDrives;

const DIRECTORY_ID_VERSION: &str = "1";
pub const MAX_DIRECTORY_ENTRIES: usize = 10_000;

#[derive(Debug)]
pub enum DirectoryError {
    InvalidId,
    InvalidPath,
    NotFound,
    NotDirectory,
    Inaccessible,
    Io(std::io::Error),
    Join(tokio::task::JoinError),
}

impl fmt::Display for DirectoryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidId => write!(formatter, "the directory identifier is invalid"),
            Self::InvalidPath => write!(formatter, "the directory path is invalid"),
            Self::NotFound => write!(formatter, "the directory does not exist"),
            Self::NotDirectory => write!(formatter, "the selected path is not a directory"),
            Self::Inaccessible => write!(formatter, "the directory cannot be read"),
            Self::Io(error) => write!(formatter, "filesystem operation failed: {error}"),
            Self::Join(error) => write!(formatter, "filesystem task failed: {error}"),
        }
    }
}

impl std::error::Error for DirectoryError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Join(error) => Some(error),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DirectoryEntry {
    pub id: String,
    pub name: String,
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DirectoryListing {
    pub current: DirectoryEntry,
    pub parent_id: Option<String>,
    pub breadcrumbs: Vec<DirectoryEntry>,
    pub directories: Vec<DirectoryEntry>,
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FilesystemRoots {
    pub default_directory: DirectoryEntry,
    pub roots: Vec<DirectoryEntry>,
}

#[derive(Debug, Clone)]
pub struct DirectoryBrowser {
    default_directory: PathBuf,
}

impl DirectoryBrowser {
    pub fn new(default_directory: PathBuf) -> Self {
        debug_assert!(default_directory.is_absolute());
        Self { default_directory }
    }

    pub fn default_path(&self) -> &Path {
        &self.default_directory
    }

    pub fn roots(&self) -> FilesystemRoots {
        FilesystemRoots {
            default_directory: directory_entry(&self.default_directory),
            roots: platform_roots()
                .into_iter()
                .map(|path| directory_entry(&path))
                .collect(),
        }
    }

    pub async fn list_default(&self) -> Result<DirectoryListing, DirectoryError> {
        self.list_path(self.default_directory.clone()).await
    }

    pub async fn list_id(&self, directory_id: &str) -> Result<DirectoryListing, DirectoryError> {
        let path = decode_directory_id(directory_id)?;
        self.list_path(path).await
    }

    pub async fn resolve_display_path(
        &self,
        display_path: &str,
    ) -> Result<DirectoryListing, DirectoryError> {
        if display_path.is_empty() || display_path.contains('\0') {
            return Err(DirectoryError::InvalidPath);
        }
        self.list_path(PathBuf::from(display_path)).await
    }

    pub async fn resolve_id(&self, directory_id: &str) -> Result<PathBuf, DirectoryError> {
        let path = decode_directory_id(directory_id)?;
        resolve_readable_directory_async(path).await
    }

    pub fn describe(&self, path: &Path) -> DirectoryEntry {
        directory_entry(path)
    }

    async fn list_path(&self, path: PathBuf) -> Result<DirectoryListing, DirectoryError> {
        let current_path = resolve_readable_directory_async(path).await?;
        let current = directory_entry(&current_path);
        let parent_id = current_path
            .parent()
            .filter(|parent| *parent != current_path)
            .map(encode_directory_id);
        let breadcrumbs = current_path
            .ancestors()
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .map(directory_entry)
            .collect();
        let mut reader = tokio::fs::read_dir(&current_path)
            .await
            .map_err(classify_io_error)?;
        let mut directories = Vec::new();
        let mut truncated = false;

        while let Some(entry) = reader.next_entry().await.map_err(classify_io_error)? {
            let is_directory = match entry.file_type().await {
                Ok(file_type) if file_type.is_dir() => true,
                Ok(file_type) if file_type.is_symlink() => entry
                    .metadata()
                    .await
                    .map(|metadata| metadata.is_dir())
                    .unwrap_or(false),
                Ok(_) | Err(_) => false,
            };
            if is_directory {
                if directories.len() == MAX_DIRECTORY_ENTRIES {
                    truncated = true;
                    break;
                }
                directories.push(directory_entry(&entry.path()));
            }
        }

        directories.sort_by(|left, right| directory_name_cmp(&left.name, &right.name));
        Ok(DirectoryListing {
            current,
            parent_id,
            breadcrumbs,
            directories,
            truncated,
        })
    }
}

pub fn encode_directory_id(path: &Path) -> String {
    #[cfg(windows)]
    let (platform, bytes) = {
        let bytes = path
            .as_os_str()
            .encode_wide()
            .flat_map(u16::to_le_bytes)
            .collect::<Vec<_>>();
        ("w", bytes)
    };

    #[cfg(unix)]
    let (platform, bytes) = ("u", path.as_os_str().as_bytes().to_vec());

    #[cfg(not(any(unix, windows)))]
    let (platform, bytes) = ("s", path.to_string_lossy().as_bytes().to_vec());

    format!(
        "{platform}{DIRECTORY_ID_VERSION}.{}",
        URL_SAFE_NO_PAD.encode(bytes)
    )
}

pub fn decode_directory_id(directory_id: &str) -> Result<PathBuf, DirectoryError> {
    let expected_prefix = if cfg!(windows) {
        "w1."
    } else if cfg!(unix) {
        "u1."
    } else {
        "s1."
    };
    let encoded = directory_id
        .strip_prefix(expected_prefix)
        .ok_or(DirectoryError::InvalidId)?;
    if encoded.is_empty() {
        return Err(DirectoryError::InvalidId);
    }
    let bytes = URL_SAFE_NO_PAD
        .decode(encoded)
        .map_err(|_| DirectoryError::InvalidId)?;

    #[cfg(windows)]
    let path = {
        let chunks = bytes.chunks_exact(2);
        if !chunks.remainder().is_empty() {
            return Err(DirectoryError::InvalidId);
        }
        let wide = chunks
            .map(|chunk| u16::from_le_bytes([chunk[0], chunk[1]]))
            .collect::<Vec<_>>();
        PathBuf::from(OsString::from_wide(&wide))
    };

    #[cfg(unix)]
    let path = PathBuf::from(OsString::from_vec(bytes));

    #[cfg(not(any(unix, windows)))]
    let path = PathBuf::from(String::from_utf8(bytes).map_err(|_| DirectoryError::InvalidId)?);

    if !path.is_absolute() {
        return Err(DirectoryError::InvalidId);
    }
    Ok(path)
}

async fn resolve_readable_directory_async(path: PathBuf) -> Result<PathBuf, DirectoryError> {
    if !path.is_absolute() {
        return Err(DirectoryError::InvalidPath);
    }

    tokio::task::spawn_blocking(move || canonicalize_readable_directory(&path))
        .await
        .map_err(DirectoryError::Join)?
}

pub fn canonicalize_readable_directory(path: &Path) -> Result<PathBuf, DirectoryError> {
    let resolved = dunce::canonicalize(path).map_err(classify_io_error)?;
    let metadata = std::fs::metadata(&resolved).map_err(classify_io_error)?;
    if !metadata.is_dir() {
        return Err(DirectoryError::NotDirectory);
    }

    // Opening the iterator verifies access without reading or mutating file content.
    let _reader = std::fs::read_dir(&resolved).map_err(classify_io_error)?;
    Ok(resolved)
}

pub fn validate_canonical_readable_directory(path: &Path) -> Result<(), DirectoryError> {
    if !path.is_absolute() {
        return Err(DirectoryError::InvalidPath);
    }

    let resolved = canonicalize_readable_directory(path)?;
    if resolved != path {
        return Err(DirectoryError::InvalidPath);
    }
    Ok(())
}

fn classify_io_error(error: std::io::Error) -> DirectoryError {
    match error.kind() {
        ErrorKind::NotFound => DirectoryError::NotFound,
        ErrorKind::PermissionDenied => DirectoryError::Inaccessible,
        _ => DirectoryError::Io(error),
    }
}

fn directory_entry(path: &Path) -> DirectoryEntry {
    DirectoryEntry {
        id: encode_directory_id(path),
        name: directory_name(path),
        path: path.to_string_lossy().into_owned(),
    }
}

fn directory_name(path: &Path) -> String {
    path.file_name()
        .filter(|name| !name.is_empty())
        .map_or_else(
            || path.to_string_lossy().into_owned(),
            |name| name.to_string_lossy().into_owned(),
        )
}

#[cfg(windows)]
fn directory_name_cmp(left: &str, right: &str) -> std::cmp::Ordering {
    left.to_lowercase()
        .cmp(&right.to_lowercase())
        .then_with(|| left.cmp(right))
}

#[cfg(not(windows))]
fn directory_name_cmp(left: &str, right: &str) -> std::cmp::Ordering {
    left.cmp(right)
}

#[cfg(windows)]
fn platform_roots() -> Vec<PathBuf> {
    // SAFETY: GetLogicalDrives takes no pointers and has no preconditions.
    let mask = unsafe { GetLogicalDrives() };
    (0_u8..26)
        .filter(|index| mask & (1_u32 << index) != 0)
        .map(|index| PathBuf::from(format!("{}:\\", char::from(b'A' + index))))
        .collect()
}

#[cfg(not(windows))]
fn platform_roots() -> Vec<PathBuf> {
    vec![PathBuf::from("/")]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn directory_ids_round_trip_native_absolute_paths() {
        let path = if cfg!(windows) {
            PathBuf::from(r"C:\projects\пример")
        } else {
            PathBuf::from("/projects/пример")
        };

        let id = encode_directory_id(&path);

        assert_eq!(decode_directory_id(&id).expect("decode path"), path);
        assert!(!id.contains(['/', '+', '=']));
        assert!(decode_directory_id("not-a-directory-id").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn directory_ids_preserve_non_utf8_unix_paths() {
        let path = PathBuf::from(OsString::from_vec(vec![b'/', b't', b'm', b'p', b'/', 0xFF]));

        assert_eq!(
            decode_directory_id(&encode_directory_id(&path)).expect("decode native bytes"),
            path
        );
    }

    #[cfg(windows)]
    #[test]
    fn directory_ids_preserve_unpaired_windows_utf16() {
        let path = PathBuf::from(OsString::from_wide(&[
            b'C' as u16,
            b':' as u16,
            b'\\' as u16,
            0xD800,
        ]));

        assert_eq!(
            decode_directory_id(&encode_directory_id(&path)).expect("decode native UTF-16"),
            path
        );
    }

    #[tokio::test]
    async fn listing_returns_only_immediate_directories() {
        let fixture = tempfile::tempdir().expect("temporary directory");
        std::fs::create_dir(fixture.path().join("beta")).expect("create beta");
        std::fs::create_dir(fixture.path().join("alpha")).expect("create alpha");
        std::fs::create_dir(fixture.path().join("alpha").join("nested")).expect("create nested");
        std::fs::write(fixture.path().join("file.txt"), "not returned").expect("write file");
        let canonical = dunce::canonicalize(fixture.path()).expect("canonical fixture");
        let browser = DirectoryBrowser::new(canonical.clone());

        let listing = browser.list_default().await.expect("list fixture");

        assert_eq!(listing.current.id, encode_directory_id(&canonical));
        assert_eq!(
            listing
                .directories
                .iter()
                .map(|entry| entry.name.as_str())
                .collect::<Vec<_>>(),
            ["alpha", "beta"]
        );
        assert!(
            listing
                .directories
                .iter()
                .all(|entry| entry.name != "nested" && entry.name != "file.txt")
        );
        assert!(!listing.truncated);
        assert_eq!(
            listing.breadcrumbs.last().map(|entry| entry.id.as_str()),
            Some(listing.current.id.as_str())
        );
    }

    #[tokio::test]
    async fn rejects_relative_missing_and_file_paths() {
        let fixture = tempfile::tempdir().expect("temporary directory");
        let canonical = dunce::canonicalize(fixture.path()).expect("canonical fixture");
        let browser = DirectoryBrowser::new(canonical.clone());
        let file = canonical.join("file.txt");
        std::fs::write(&file, "file").expect("write file");

        assert!(matches!(
            browser.resolve_display_path("relative").await,
            Err(DirectoryError::InvalidPath)
        ));
        assert!(matches!(
            browser
                .resolve_display_path(&canonical.join("missing").to_string_lossy())
                .await,
            Err(DirectoryError::NotFound)
        ));
        assert!(matches!(
            browser.resolve_display_path(&file.to_string_lossy()).await,
            Err(DirectoryError::NotDirectory)
        ));
    }

    #[test]
    fn configured_directory_validator_accepts_only_the_same_canonical_target() {
        let fixture = tempfile::tempdir().expect("temporary directory");
        let canonical = dunce::canonicalize(fixture.path()).expect("canonical fixture");
        let child = canonical.join("child");
        std::fs::create_dir(&child).expect("create child");
        let noncanonical = child.join("..");

        validate_canonical_readable_directory(&canonical).expect("canonical readable directory");
        assert!(matches!(
            validate_canonical_readable_directory(&noncanonical),
            Err(DirectoryError::InvalidPath)
        ));
        assert!(matches!(
            validate_canonical_readable_directory(Path::new("relative")),
            Err(DirectoryError::InvalidPath)
        ));
    }

    #[test]
    fn configured_directory_validator_rejects_stale_and_non_directory_targets() {
        let fixture = tempfile::tempdir().expect("temporary directory");
        let missing = fixture.path().join("missing");
        let file = fixture.path().join("file.txt");
        std::fs::write(&file, "file").expect("write file");

        assert!(matches!(
            validate_canonical_readable_directory(&missing),
            Err(DirectoryError::NotFound)
        ));
        assert!(matches!(
            validate_canonical_readable_directory(&file),
            Err(DirectoryError::NotDirectory)
        ));
    }

    #[test]
    fn roots_include_the_default_directory() {
        let path = if cfg!(windows) {
            PathBuf::from(r"C:\projects")
        } else {
            PathBuf::from("/projects")
        };
        let browser = DirectoryBrowser::new(path.clone());

        let roots = browser.roots();

        assert_eq!(roots.default_directory.id, encode_directory_id(&path));
        assert!(!roots.roots.is_empty());
        assert!(roots.roots.iter().all(|root| {
            decode_directory_id(&root.id)
                .expect("root identifier")
                .is_absolute()
        }));
    }
}
