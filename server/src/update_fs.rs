use std::{
    fs::{self, File, Metadata, OpenOptions},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use fs2::FileExt;

#[cfg(windows)]
use std::os::windows::fs::MetadataExt;

#[cfg(windows)]
const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;

pub struct UpdateFileLock {
    file: File,
}

impl UpdateFileLock {
    pub fn acquire(updates_root: &Path) -> Result<Self> {
        validate_regular_directory(updates_root)?;
        let path = updates_root.join("update.lock");
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&path)
            .with_context(|| format!("failed to open update lock {}", path.display()))?;
        validate_regular_file(&path, Some(1024))?;
        file.try_lock_exclusive()
            .context("another Codex Web update is already running")?;
        Ok(Self { file })
    }
}

impl Drop for UpdateFileLock {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.file);
    }
}

pub fn is_link_or_reparse(metadata: &Metadata) -> bool {
    if metadata.file_type().is_symlink() {
        return true;
    }

    #[cfg(windows)]
    {
        metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    }

    #[cfg(not(windows))]
    {
        false
    }
}

pub fn ensure_private_directory(path: &Path) -> Result<()> {
    if path.exists() {
        validate_regular_directory(path)?;
    } else {
        fs::create_dir(path)
            .with_context(|| format!("failed to create update directory {}", path.display()))?;
        validate_regular_directory(path)?;
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        fs::set_permissions(path, fs::Permissions::from_mode(0o700))
            .with_context(|| format!("failed to protect update directory {}", path.display()))?;
    }
    Ok(())
}

pub fn validate_regular_directory(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("failed to inspect update directory {}", path.display()))?;
    if !metadata.is_dir() || is_link_or_reparse(&metadata) {
        bail!("update path is not a regular directory: {}", path.display());
    }
    Ok(())
}

pub fn validate_regular_file(path: &Path, maximum_size: Option<u64>) -> Result<Metadata> {
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("failed to inspect update file {}", path.display()))?;
    if !metadata.is_file()
        || is_link_or_reparse(&metadata)
        || maximum_size.is_some_and(|limit| metadata.len() > limit)
    {
        bail!("update path is not a safe regular file: {}", path.display());
    }
    Ok(metadata)
}

pub fn safe_remove_tree(path: &Path, allowed_root: &Path) -> Result<()> {
    if !path.exists() {
        return Ok(());
    }

    validate_regular_directory(allowed_root)?;
    validate_regular_directory(path)?;
    let allowed = dunce::canonicalize(allowed_root)
        .with_context(|| format!("failed to resolve {}", allowed_root.display()))?;
    let resolved = dunce::canonicalize(path)
        .with_context(|| format!("failed to resolve {}", path.display()))?;
    if resolved == allowed || !resolved.starts_with(&allowed) {
        bail!("refusing to remove an update path outside the update directory");
    }
    validate_removal_tree(&resolved)?;
    fs::remove_dir_all(&resolved)
        .with_context(|| format!("failed to remove {}", resolved.display()))
}

fn validate_removal_tree(root: &Path) -> Result<()> {
    let mut directories = vec![root.to_path_buf()];
    while let Some(directory) = directories.pop() {
        validate_regular_directory(&directory)?;
        for entry in fs::read_dir(&directory)
            .with_context(|| format!("failed to inspect {}", directory.display()))?
        {
            let entry =
                entry.with_context(|| format!("failed to inspect {}", directory.display()))?;
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path)
                .with_context(|| format!("failed to inspect {}", path.display()))?;
            if is_link_or_reparse(&metadata) {
                bail!(
                    "refusing to remove an update tree containing a link or reparse point: {}",
                    path.display()
                );
            }
            if metadata.is_dir() {
                directories.push(path);
            } else if !metadata.is_file() {
                bail!(
                    "refusing to remove an update tree containing a special file: {}",
                    path.display()
                );
            }
        }
    }
    Ok(())
}

pub fn exact_child(parent: &Path, child: &Path) -> Result<PathBuf> {
    validate_regular_directory(parent)?;
    let resolved_parent = dunce::canonicalize(parent)
        .with_context(|| format!("failed to resolve {}", parent.display()))?;
    let resolved_child = dunce::canonicalize(child)
        .with_context(|| format!("failed to resolve {}", child.display()))?;
    if resolved_child.parent() != Some(resolved_parent.as_path()) {
        bail!("update path is not an immediate child of its expected directory");
    }
    Ok(resolved_child)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recursive_removal_is_confined_to_an_exact_child() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let allowed = temporary.path().join("updates");
        let child = allowed.join("staging");
        ensure_private_directory(&allowed).expect("allowed root");
        ensure_private_directory(&child).expect("child");
        fs::write(child.join("payload"), b"test").expect("fixture");

        safe_remove_tree(&child, &allowed).expect("safe child removal");
        assert!(!child.exists());
        assert!(allowed.exists());
        assert!(safe_remove_tree(&allowed, &allowed).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn symbolic_link_directories_are_rejected_before_removal() {
        use std::os::unix::fs::symlink;

        let temporary = tempfile::tempdir().expect("temporary directory");
        let allowed = temporary.path().join("updates");
        let outside = temporary.path().join("outside");
        ensure_private_directory(&allowed).expect("allowed root");
        ensure_private_directory(&outside).expect("outside root");
        let link = allowed.join("v9.9.9");
        symlink(&outside, &link).expect("symlink fixture");

        assert!(safe_remove_tree(&link, &allowed).is_err());
        assert!(outside.exists());
    }

    #[cfg(unix)]
    #[test]
    fn nested_symbolic_links_are_rejected_before_removal() {
        use std::os::unix::fs::symlink;

        let temporary = tempfile::tempdir().expect("temporary directory");
        let allowed = temporary.path().join("updates");
        let child = allowed.join("staging");
        let outside = temporary.path().join("outside");
        ensure_private_directory(&allowed).expect("allowed root");
        ensure_private_directory(&child).expect("child");
        ensure_private_directory(&outside).expect("outside");
        symlink(&outside, child.join("nested-link")).expect("nested symlink fixture");

        assert!(safe_remove_tree(&child, &allowed).is_err());
        assert!(child.exists());
        assert!(outside.exists());
    }
}
