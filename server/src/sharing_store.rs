//! Private, bounded SQLite storage. The PTY reader only uses nonblocking capture.
use std::{
    collections::HashMap,
    fs::{File, OpenOptions},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc::{self, SyncSender},
    },
    time::Duration,
};

use bytes::Bytes;
use fs2::FileExt;
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use tokio::sync::oneshot;
use uuid::Uuid;

pub const ARCHIVE_GENERATION_LIMIT: u64 = 256 * 1024 * 1024;
const ARCHIVE_TOTAL_LIMIT: i64 = 2 * 1024 * 1024 * 1024;
const CAPTURE_QUEUE: usize = 16_384;
const CAPTURE_BYTES: usize = 16 * 1024 * 1024;
const SCHEMA_VERSION: i64 = 2;

#[derive(Debug, Clone, Copy)]
pub struct Error(pub &'static str);
pub type Result<T> = std::result::Result<T, Error>;

impl From<rusqlite::Error> for Error {
    fn from(_: rusqlite::Error) -> Self {
        Self("storage_unavailable")
    }
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}
impl std::error::Error for Error {}

pub fn lock<T>(value: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    value
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}
pub fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as i64
}

type Job = Box<dyn FnOnce(&mut Connection) + Send>;

#[derive(Clone)]
pub struct Store {
    commands: SyncSender<Job>,
    captures: SyncSender<CaptureMessage>,
    healthy: Arc<AtomicBool>,
    gaps: Arc<Mutex<HashMap<Uuid, Arc<AtomicBool>>>>,
    queued_bytes: Arc<AtomicUsize>,
    wake: SyncSender<()>,
    pub(crate) directory: Arc<PathBuf>,
    pub(crate) admission: Arc<Mutex<StorageAdmission>>,
    protection: Arc<Mutex<Option<crate::sharing_storage::ProtectionProbe>>>,
}

#[derive(Default)]
pub(crate) struct StorageAdmission {
    pub recordings: usize,
    pub maintenance: bool,
}

pub struct RecordingPermit(Arc<Mutex<StorageAdmission>>);
impl Drop for RecordingPermit {
    fn drop(&mut self) {
        lock(&self.0).recordings -= 1;
    }
}

struct MaintenancePermit(Arc<Mutex<StorageAdmission>>);
impl Drop for MaintenancePermit {
    fn drop(&mut self) {
        lock(&self.0).maintenance = false;
    }
}

impl Store {
    pub async fn open(directory: PathBuf) -> Result<Self> {
        tokio::task::spawn_blocking(move || Self::open_blocking(&directory))
            .await
            .map_err(|_| Error("storage_unavailable"))?
    }

    fn open_blocking(directory: &Path) -> Result<Self> {
        crate::workspaces::prepare_state_directory_sync(directory)
            .map_err(|_| Error("unsafe_state_directory"))?;
        #[cfg(windows)]
        windows_privacy::check(directory).map_err(|_| Error("unsafe_state_directory"))?;
        let database = directory.join("sharing.sqlite3");
        let lease = private_file(&directory.join("sharing.sqlite3.lock"))?;
        lease
            .try_lock_exclusive()
            .map_err(|_| Error("sharing_store_in_use"))?;
        remove_compaction_copy(directory)?;
        drop(private_file(&database)?);
        for suffix in ["sharing.sqlite3-wal", "sharing.sqlite3-shm"] {
            let path = directory.join(suffix);
            match std::fs::symlink_metadata(&path) {
                Ok(_) => check_file(&path)?,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(_) => return Err(Error("unsafe_state_file")),
            }
        }
        let mut connection = Connection::open(&database)?;
        connection.busy_timeout(Duration::from_secs(2))?;
        let version: i64 = connection.pragma_query_value(None, "user_version", |r| r.get(0))?;
        if version > SCHEMA_VERSION {
            return Err(Error("sharing_schema_too_new"));
        }
        if version == 0 {
            connection.pragma_update(None, "auto_vacuum", 2)?;
        }
        connection.execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; PRAGMA foreign_keys=ON;
             PRAGMA journal_size_limit=67108864; PRAGMA wal_autocheckpoint=1000;
             PRAGMA temp_store=MEMORY;",
        )?;
        let page_size: i64 = connection.pragma_query_value(None, "page_size", |r| r.get(0))?;
        connection.pragma_update(
            None,
            "max_page_count",
            3_i64 * 1024 * 1024 * 1024 / page_size,
        )?;
        if version == 0 {
            let tx = connection.transaction()?;
            tx.execute_batch(SCHEMA)?;
            tx.pragma_update(None, "user_version", 1)?;
            tx.commit()?;
        }
        if version < 2 {
            let tx = connection.transaction()?;
            tx.execute_batch(crate::sharing_storage::MIGRATION)?;
            tx.execute("UPDATE generations SET ended_at=?1 WHERE ended=1", [now()])?;
            tx.pragma_update(None, "user_version", SCHEMA_VERSION)?;
            tx.commit()?;
        }
        // Never recover delivery by resending. Historical identities remain private.
        connection.execute_batch(
            "UPDATE grants SET revoked=1;
             UPDATE invitations SET revoked=1;
             UPDATE proposals SET state='invalidated', reason='server_restart' WHERE state='pending';
             UPDATE proposals SET state='uncertain', reason='server_restart' WHERE state IN ('approving','queued');
             UPDATE approval_attempts SET outcome='uncertain' WHERE outcome='intent';
             UPDATE generations SET ended=1,incomplete=1,ended_at=CAST(strftime('%s','now') AS INTEGER)*1000 WHERE ended=0;"
        )?;
        let (commands, command_rx) = mpsc::sync_channel::<Job>(128);
        connection
            .execute_batch("CREATE TEMP TABLE live_recording_generations(id TEXT PRIMARY KEY);")?;
        let protection = Arc::new(Mutex::new(None));
        let worker_protection = protection.clone();
        let (captures, capture_rx) = mpsc::sync_channel::<CaptureMessage>(CAPTURE_QUEUE);
        let (wake, wake_rx) = mpsc::sync_channel::<()>(1);
        let healthy = Arc::new(AtomicBool::new(true));
        let health = healthy.clone();
        let gaps = Arc::new(Mutex::new(HashMap::<Uuid, Arc<AtomicBool>>::new()));
        let known_gaps = gaps.clone();
        let queued_bytes = Arc::new(AtomicUsize::new(0));
        let queued = queued_bytes.clone();
        std::thread::Builder::new()
            .name("sharing-db".into())
            .spawn(move || {
                let _lease = lease;
                let mut last_retention = std::time::Instant::now() - Duration::from_secs(60);
                loop {
                    let had_job = match command_rx.try_recv() {
                        Ok(job) => {
                            job(&mut connection);
                            true
                        }
                        Err(mpsc::TryRecvError::Disconnected) => break,
                        Err(mpsc::TryRecvError::Empty) => false,
                    };
                    let protected = crate::sharing_storage::refresh_protection(
                        &mut connection,
                        lock(&worker_protection).clone(),
                    )
                    .is_ok();
                    if !protected {
                        health.store(false, Ordering::Release);
                    }
                    if protected && last_retention.elapsed() >= Duration::from_secs(60) {
                        if crate::sharing_storage::retention(&mut connection).is_err() {
                            health.store(false, Ordering::Release);
                        }
                        last_retention = std::time::Instant::now();
                    }
                    let cleanup = if protected {
                        crate::sharing_storage::cleanup_step(&mut connection)
                    } else {
                        Err(Error("storage_unavailable"))
                    };
                    if cleanup.is_err() {
                        health.store(false, Ordering::Release);
                    }
                    let compact = crate::sharing_storage::compact_step(&connection);
                    if compact.is_err() {
                        health.store(false, Ordering::Release);
                    }
                    // A dropped final capture has no later event to carry its gap flag.
                    // Keep bounded out-of-band flags until SQLite acknowledges them.
                    let pending_gaps: Vec<_> = lock(&known_gaps)
                        .iter()
                        .filter(|(_, gap)| gap.load(Ordering::Acquire))
                        .map(|(id, _)| *id)
                        .collect();
                    for generation in pending_gaps {
                        if connection
                            .execute(
                                "UPDATE generations SET incomplete=1 WHERE id=?1",
                                [generation.to_string()],
                            )
                            .is_ok_and(|rows| rows > 0)
                        {
                            lock(&known_gaps).remove(&generation);
                        }
                    }
                    let batch: Vec<_> = capture_rx.try_iter().take(128).collect();
                    for message in &batch {
                        if let CaptureMessage::Event(capture) = message {
                            queued.fetch_sub(capture.data.len(), Ordering::Relaxed);
                        }
                    }
                    if batch.is_empty() {
                        // Producers coalesce wakeups. Recheck both queues after
                        // consuming a wake, so a send racing this wait is safe.
                        if !had_job && !cleanup.unwrap_or(false) {
                            match wake_rx.recv_timeout(Duration::from_secs(1)) {
                                Err(mpsc::RecvTimeoutError::Disconnected) => break,
                                Ok(()) | Err(mpsc::RecvTimeoutError::Timeout) => {}
                            }
                        }
                        continue;
                    }
                    let written = (|| -> rusqlite::Result<()> {
                        let tx = connection.transaction()?;
                        let mut total: i64 = tx.query_row(
                            "SELECT COALESCE(SUM(bytes),0) FROM generations",
                            [],
                            |r| r.get(0),
                        )?;
                        for message in &batch {
                            if let CaptureMessage::Event(capture) = message {
                                capture.persist(&tx, &mut total)?;
                            }
                        }
                        tx.commit()
                    })();
                    if written.is_err() {
                        health.store(false, Ordering::Release);
                    } else {
                        health.store(true, Ordering::Release);
                    }
                    for message in batch {
                        match message {
                            CaptureMessage::Event(capture) if written.is_err() => {
                                capture.incomplete.store(true, Ordering::Release);
                                // A failed end must never turn a partial recording into a complete one.
                                let _ = connection.execute(
                                    "UPDATE generations SET incomplete=1 WHERE id=?1",
                                    [capture.generation.to_string()],
                                );
                            }
                            CaptureMessage::Barrier(sender) => {
                                let result = if written.is_err() {
                                    Err(Error("storage_unavailable"))
                                } else {
                                    connection
                                        .execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
                                        .map_err(Error::from)
                                };
                                let _ = sender.send(result);
                            }
                            _ => {}
                        }
                    }
                }
                let _ = connection.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);");
            })
            .map_err(|_| Error("storage_unavailable"))?;
        Ok(Self {
            commands,
            captures,
            healthy,
            gaps,
            queued_bytes,
            wake,
            directory: Arc::new(directory.to_path_buf()),
            admission: Arc::new(Mutex::new(StorageAdmission::default())),
            protection,
        })
    }

    /// The worker owns accepted operations, even if the HTTP future disconnects.
    pub async fn run<T: Send + 'static>(
        &self,
        operation: impl FnOnce(&mut Connection) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let (sender, receiver) = oneshot::channel();
        let protection = self.protection.clone();
        {
            let admission = lock(&self.admission);
            if admission.maintenance {
                return Err(Error("storage_maintenance"));
            }
            self.commands
                .try_send(Box::new(move |db| {
                    let result =
                        crate::sharing_storage::refresh_protection(db, lock(&protection).clone())
                            .and_then(|_| operation(db));
                    let _ = sender.send(result);
                }))
                .map_err(|_| Error("storage_busy"))?;
        }
        self.notify();
        receiver.await.map_err(|_| Error("storage_unavailable"))?
    }

    /// Receipts must survive metadata backpressure. The blocking sender owns
    /// admission even when its caller disappears; it never runs on a PTY thread.
    pub async fn run_critical<T: Send + 'static>(
        &self,
        operation: impl FnOnce(&mut Connection) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let (sender, receiver) = oneshot::channel();
        let commands = self.commands.clone();
        let wake = self.wake.clone();
        let protection = self.protection.clone();
        tokio::task::spawn_blocking(move || -> Result<()> {
            commands
                .send(Box::new(move |db| {
                    let result =
                        crate::sharing_storage::refresh_protection(db, lock(&protection).clone())
                            .and_then(|_| operation(db));
                    let _ = sender.send(result);
                }))
                .map_err(|_| Error("storage_unavailable"))?;
            let _ = wake.try_send(());
            Ok(())
        })
        .await
        .map_err(|_| Error("storage_unavailable"))??;
        receiver.await.map_err(|_| Error("storage_unavailable"))?
    }

    pub fn notify(&self) {
        let _ = self.wake.try_send(());
    }

    pub fn receipt_failed(&self) {
        self.healthy.store(false, Ordering::Release);
    }

    pub fn healthy(&self) -> bool {
        self.healthy.load(Ordering::Acquire)
    }

    pub fn recording_permit(&self) -> Result<RecordingPermit> {
        let mut admission = lock(&self.admission);
        if admission.maintenance {
            return Err(Error("storage_maintenance"));
        }
        admission.recordings += 1;
        Ok(RecordingPermit(self.admission.clone()))
    }

    pub fn bind_registry(&self, registry: &crate::registry::SessionRegistry) {
        *lock(&self.protection) = Some(registry.sharing_generation_probe());
    }

    pub async fn storage_usage(&self) -> Result<serde_json::Value> {
        let directory = self.directory.clone();
        let (recordings, maintenance) = {
            let admission = lock(&self.admission);
            (admission.recordings, admission.maintenance)
        };
        self.run_critical(move |db| {
            let mut value = serde_json::to_value(crate::sharing_storage::usage(db, &directory)?)
                .map_err(|_| Error("storage_unavailable"))?;
            value["recordingEnabledCount"] = recordings.into();
            value["maintenance"] = maintenance.into();
            Ok(value)
        })
        .await
    }

    pub async fn compact(&self) -> Result<serde_json::Value> {
        let permit = {
            let mut admission = lock(&self.admission);
            if admission.maintenance {
                return Err(Error("storage_maintenance"));
            }
            if admission.recordings != 0 {
                return Err(Error("recording_enabled"));
            }
            admission.maintenance = true;
            MaintenancePermit(self.admission.clone())
        };
        let store = self.clone();
        // Maintenance is cancellation-owned. Browser disconnects cannot release
        // admission while a database operation is still running.
        tokio::spawn(async move {
            store.flush().await?;
            let directory = store.directory.clone();
            store.run_critical(move |db| {
                let _permit = permit;
                let pending: bool = db.query_row("SELECT EXISTS(SELECT 1 FROM proposals WHERE state IN ('approving','queued'))",[],|r|r.get(0))?;
                if pending { return Err(Error("delivery_pending")); }
                let queued: bool = db.query_row("SELECT EXISTS(SELECT 1 FROM archive_cleanup)",[],|r|r.get(0))?;
                if queued { return Err(Error("cleanup_pending")); }
                let before = crate::sharing_storage::usage(db, &directory)?;
                let started = std::time::Instant::now();
                if !before.incremental_compaction {
                    let required = before.database_bytes.saturating_mul(2).saturating_add(64 * 1024 * 1024);
                    if before.free_disk_bytes < required { return Err(Error("insufficient_disk_space")); }
                    compact_legacy(db, &directory)?;
                } else {
                    db.execute_batch("PRAGMA incremental_vacuum;")?;
                }
                let (busy,_,_):(i64,i64,i64) = db.query_row("PRAGMA wal_checkpoint(TRUNCATE)",[],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
                if busy != 0 { return Err(Error("storage_busy")); }
                let after = crate::sharing_storage::usage(db, &directory)?;
                Ok(serde_json::json!({"beforeBytes":before.disk_bytes,"afterBytes":after.disk_bytes,
                    "reclaimedBytes":before.disk_bytes.saturating_sub(after.disk_bytes),"elapsedMs":started.elapsed().as_millis()}))
            }).await
        }).await.map_err(|_|Error("storage_unavailable"))?
    }

    pub async fn flush(&self) -> Result<()> {
        // Ordered with capture itself, not the independent metadata queue.
        let (sender, receiver) = oneshot::channel();
        let captures = self.captures.clone();
        let wake = self.wake.clone();
        tokio::task::spawn_blocking(move || -> Result<()> {
            captures
                .send(CaptureMessage::Barrier(sender))
                .map_err(|_| Error("storage_unavailable"))?;
            let _ = wake.try_send(());
            Ok(())
        })
        .await
        .map_err(|_| Error("storage_unavailable"))??;
        receiver.await.map_err(|_| Error("storage_unavailable"))?
    }
}

const COMPACTION_COPY: &str = "sharing.compact.sqlite3";

fn remove_compaction_copy(directory: &Path) -> Result<()> {
    // Only our fixed scratch file, while holding the exclusive store lease.
    // Never follow a link or remove an operator-owned/unprotected file.
    let path = directory.join(COMPACTION_COPY);
    match std::fs::symlink_metadata(&path) {
        Ok(_) => {
            check_file(&path)?;
            std::fs::remove_file(path).map_err(|_| Error("storage_unavailable"))?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => return Err(Error("unsafe_state_file")),
    }
    Ok(())
}

fn compact_legacy(db: &mut Connection, directory: &Path) -> Result<()> {
    // Plain VACUUM can spill a complete recording into the operating system's
    // temporary directory. Keep the copy private and let SQLite's backup
    // transaction replace the original without closing it or renaming WAL files.
    remove_compaction_copy(directory)?;
    let path = directory.join(COMPACTION_COPY);
    drop(private_file(&path)?);
    struct Scratch<'a>(&'a Path);
    impl Drop for Scratch<'_> {
        fn drop(&mut self) {
            let _ = remove_compaction_copy(self.0);
        }
    }
    let _scratch = Scratch(directory);
    db.pragma_update(None, "auto_vacuum", 2)?;
    db.execute(
        "VACUUM main INTO ?1",
        [path.to_str().ok_or(Error("unsafe_state_file"))?],
    )?;
    check_file(&path)?;
    let copy = Connection::open_with_flags(
        &path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    let backup = rusqlite::backup::Backup::new(&copy, db)?;
    // The source is our private, immutable copy. A busy destination fails rather
    // than retrying forever. Dropping an unfinished backup rolls it back.
    loop {
        match backup.step(256)? {
            rusqlite::backup::StepResult::Done => break,
            rusqlite::backup::StepResult::More => {}
            _ => return Err(Error("storage_busy")),
        }
    }
    Ok(())
}

pub fn repair_storage_permissions(directory: &Path) -> Result<()> {
    crate::workspaces::prepare_state_directory_sync(directory)
        .map_err(|_| Error("unsafe_state_directory"))?;
    #[cfg(windows)]
    {
        crate::windows_private_storage::repair(directory)
    }
    #[cfg(not(windows))]
    {
        Err(Error("repair_unavailable"))
    }
}

fn private_file(path: &Path) -> Result<File> {
    let mut options = OpenOptions::new();
    options.read(true).write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    match options.open(path) {
        Ok(file) => {
            check_file(path)?;
            Ok(file)
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            check_file(path)?;
            OpenOptions::new()
                .read(true)
                .write(true)
                .open(path)
                .map_err(|_| Error("unsafe_state_file"))
        }
        Err(_) => Err(Error("unsafe_state_file")),
    }
}
fn check_file(path: &Path) -> Result<()> {
    let metadata = std::fs::symlink_metadata(path).map_err(|_| Error("unsafe_state_file"))?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(Error("unsafe_state_file"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.mode() & 0o077 != 0 || metadata.uid() != unsafe { libc::geteuid() } {
            return Err(Error("unsafe_state_file"));
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes() & 0x400 != 0 {
            return Err(Error("unsafe_state_file"));
        }
        windows_privacy::check(path)?;
    }
    Ok(())
}

#[cfg(windows)]
mod windows_privacy {
    use super::*;
    use std::{
        ffi::c_void,
        os::windows::ffi::OsStrExt,
        ptr::{addr_of_mut, null_mut},
    };
    use windows_sys::Win32::{
        Foundation::{CloseHandle, LocalFree},
        Security::{
            ACCESS_ALLOWED_ACE, ACE_HEADER, ACL,
            Authorization::{GetNamedSecurityInfoW, SE_FILE_OBJECT},
            DACL_SECURITY_INFORMATION, EqualSid, GetAce, GetTokenInformation, IsValidAcl,
            IsValidSid, IsWellKnownSid, OWNER_SECURITY_INFORMATION, TOKEN_QUERY, TOKEN_USER,
            TokenUser, WinBuiltinAdministratorsSid, WinLocalSystemSid,
        },
        System::Threading::{GetCurrentProcess, OpenProcessToken},
    };

    /// Fail closed on broadly readable/writable state. Never modify an existing
    /// operator ACL. Administrators and SYSTEM already control this process.
    pub(super) fn check(path: &Path) -> Result<()> {
        let name: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        let mut owner = null_mut();
        let mut dacl: *mut ACL = null_mut();
        let mut descriptor = null_mut();
        // SAFETY: all outputs point to initialized storage; the descriptor owns
        // the returned SID/ACL pointers until LocalFree below.
        if unsafe {
            GetNamedSecurityInfoW(
                name.as_ptr(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION | OWNER_SECURITY_INFORMATION,
                &mut owner,
                null_mut(),
                &mut dacl,
                null_mut(),
                &mut descriptor,
            )
        } != 0
        {
            return Err(Error("unsafe_state_file"));
        }
        let result = (|| {
            let mut token = null_mut();
            // SAFETY: current process pseudo-handle is valid; output is owned.
            if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
                return Err(Error("unsafe_state_file"));
            }
            let mut size = 0;
            unsafe {
                GetTokenInformation(token, TokenUser, null_mut(), 0, &mut size);
            }
            if (size as usize) < std::mem::size_of::<TOKEN_USER>() || size > 65536 {
                unsafe {
                    CloseHandle(token);
                }
                return Err(Error("unsafe_state_file"));
            }
            // u64 alignment is sufficient for TOKEN_USER and native SID pointers.
            let mut storage = vec![0_u64; (size as usize).div_ceil(8)];
            let ok = unsafe {
                GetTokenInformation(
                    token,
                    TokenUser,
                    storage.as_mut_ptr().cast(),
                    size,
                    &mut size,
                )
            };
            unsafe {
                CloseHandle(token);
            }
            if ok == 0 {
                return Err(Error("unsafe_state_file"));
            }
            let user = unsafe { (*(storage.as_ptr().cast::<TOKEN_USER>())).User.Sid };
            let trusted = |sid: *mut c_void| {
                !sid.is_null()
                    && unsafe {
                        IsValidSid(sid) != 0
                            && (EqualSid(sid, user) != 0
                                || IsWellKnownSid(sid, WinBuiltinAdministratorsSid) != 0
                                || IsWellKnownSid(sid, WinLocalSystemSid) != 0)
                    }
            };
            if !trusted(owner) || dacl.is_null() || unsafe { IsValidAcl(dacl) } == 0 {
                return Err(Error("unsafe_state_file"));
            }
            for index in 0..unsafe { (*dacl).AceCount } {
                let mut ace = null_mut();
                if unsafe { GetAce(dacl, index.into(), &mut ace) } == 0 {
                    return Err(Error("unsafe_state_file"));
                }
                let header = unsafe { &*(ace.cast::<ACE_HEADER>()) };
                // Deny ACEs do not grant access. Inherit-only grants still need
                // checking because SQLite creates WAL/SHM children here.
                if header.AceType == 1 {
                    continue;
                }
                if header.AceType != 0
                    || usize::from(header.AceSize) < std::mem::size_of::<ACCESS_ALLOWED_ACE>()
                {
                    return Err(Error("unsafe_state_file"));
                }
                let allowed = ace.cast::<ACCESS_ALLOWED_ACE>();
                let sid = unsafe { addr_of_mut!((*allowed).SidStart).cast() };
                // CREATOR OWNER inherits as the file creator, who is this user.
                let creator = unsafe {
                    IsWellKnownSid(sid, windows_sys::Win32::Security::WinCreatorOwnerSid)
                } != 0;
                // OWNER RIGHTS (S-1-3-4), used by Python's private directories,
                // grants only the already-validated owner access to this object.
                let owner_rights = unsafe {
                    IsWellKnownSid(sid, windows_sys::Win32::Security::WinCreatorOwnerRightsSid)
                } != 0;
                if !(trusted(sid) || owner_rights || creator && header.AceFlags & 8 != 0) {
                    return Err(Error("unsafe_state_file"));
                }
            }
            Ok(())
        })();
        unsafe {
            LocalFree(descriptor);
        }
        result
    }
}

#[derive(Clone)]
pub struct Recorder {
    inner: Arc<RecorderInner>,
}
struct RecorderInner {
    store: Store,
    terminal: Uuid,
    label: String,
    agent: String,
    generations: Mutex<HashMap<Uuid, CaptureState>>,
    capacity_exceeded: AtomicBool,
    enabled: AtomicBool,
    permit: Mutex<Option<RecordingPermit>>,
}
struct CaptureState {
    sequence: i64,
    incomplete: Arc<AtomicBool>,
    bytes: u64,
    ended: bool,
}
enum CaptureMessage {
    Event(Capture),
    Barrier(oneshot::Sender<Result<()>>),
}
struct Capture {
    terminal: Uuid,
    generation: Uuid,
    label: String,
    agent: String,
    sequence: i64,
    at: i64,
    kind: &'static str,
    data: Bytes,
    cols: u16,
    rows: u16,
    incomplete: Arc<AtomicBool>,
}
impl Recorder {
    #[cfg(test)]
    pub fn new(store: Store, terminal: Uuid, label: String, agent: String) -> Self {
        Self::try_new(store, terminal, label, agent).expect("recording admitted")
    }
    pub fn try_new(store: Store, terminal: Uuid, label: String, agent: String) -> Result<Self> {
        let permit = store.recording_permit()?;
        Ok(Self {
            inner: Arc::new(RecorderInner {
                store,
                terminal,
                label,
                agent,
                generations: Mutex::new(HashMap::new()),
                capacity_exceeded: AtomicBool::new(false),
                enabled: AtomicBool::new(true),
                permit: Mutex::new(Some(permit)),
            }),
        })
    }
    pub fn begin(&self, generation: Uuid, cols: u16, rows: u16, partial: bool) {
        if !self.enabled() {
            return;
        }
        let mut states = lock(&self.inner.generations);
        if states.contains_key(&generation) {
            return;
        }
        let mut gaps = lock(&self.inner.store.gaps);
        if states.len() >= 256 || gaps.len() >= 4096 {
            self.inner.capacity_exceeded.store(true, Ordering::Release);
            return;
        }
        let incomplete = Arc::new(AtomicBool::new(partial));
        gaps.insert(generation, incomplete.clone());
        drop(gaps);
        states.insert(
            generation,
            CaptureState {
                sequence: 0,
                incomplete,
                bytes: 0,
                ended: false,
            },
        );
        self.capture_locked(&mut states, generation, "start", Bytes::new(), cols, rows);
    }
    pub fn output(&self, generation: Uuid, data: &[u8]) {
        let mut states = lock(&self.inner.generations);
        if !self.enabled() {
            return;
        }
        for part in data.chunks(16 * 1024) {
            self.capture_locked(
                &mut states,
                generation,
                "output",
                Bytes::copy_from_slice(part),
                0,
                0,
            );
        }
    }
    pub fn geometry(&self, generation: Uuid, cols: u16, rows: u16) {
        let mut states = lock(&self.inner.generations);
        if !self.enabled() {
            return;
        }
        self.capture_locked(&mut states, generation, "resize", Bytes::new(), cols, rows);
    }
    pub fn end(&self, generation: Uuid) {
        self.capture_locked(
            &mut lock(&self.inner.generations),
            generation,
            "end",
            Bytes::new(),
            0,
            0,
        );
    }
    pub fn mark_incomplete(&self, generation: Uuid) {
        if let Some(state) = lock(&self.inner.generations).get(&generation) {
            state.incomplete.store(true, Ordering::Release);
        }
        self.inner.store.notify();
    }
    pub fn enabled(&self) -> bool {
        self.inner.enabled.load(Ordering::Acquire)
    }
    pub fn pause(&self) {
        let states = lock(&self.inner.generations);
        self.inner.enabled.store(false, Ordering::Release);
        for state in states.values().filter(|state| !state.ended) {
            state.incomplete.store(true, Ordering::Release);
        }
        lock(&self.inner.permit).take();
        self.inner.store.notify();
    }
    pub fn resume(&self) -> Result<()> {
        let mut permit = lock(&self.inner.permit);
        if permit.is_none() {
            *permit = Some(self.inner.store.recording_permit()?);
        }
        self.inner.enabled.store(true, Ordering::Release);
        Ok(())
    }
    pub fn incomplete(&self, generation: Option<Uuid>) -> bool {
        self.inner.capacity_exceeded.load(Ordering::Acquire)
            || !self.inner.store.healthy()
            || generation.is_some_and(|generation| {
                lock(&self.inner.generations)
                    .get(&generation)
                    .is_some_and(|state| state.incomplete.load(Ordering::Acquire))
            })
    }
    fn capture_locked(
        &self,
        states: &mut HashMap<Uuid, CaptureState>,
        generation: Uuid,
        kind: &'static str,
        data: Bytes,
        cols: u16,
        rows: u16,
    ) {
        let Some(state) = states.get_mut(&generation) else {
            return;
        };
        if state.ended {
            return;
        }
        state.ended = kind == "end";
        state.sequence += 1;
        state.bytes += data.len() as u64;
        if state.bytes > ARCHIVE_GENERATION_LIMIT && kind == "output" {
            state.incomplete.store(true, Ordering::Release);
            self.inner.store.notify();
            return;
        }
        let capture = Capture {
            terminal: self.inner.terminal,
            generation,
            label: self.inner.label.clone(),
            agent: self.inner.agent.clone(),
            sequence: state.sequence,
            at: now(),
            kind,
            data,
            cols,
            rows,
            incomplete: state.incomplete.clone(),
        };
        let bytes = capture.data.len();
        let budget = &self.inner.store.queued_bytes;
        if budget
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |queued| {
                (queued + bytes <= CAPTURE_BYTES).then_some(queued + bytes)
            })
            .is_err()
        {
            state.incomplete.store(true, Ordering::Release);
            self.inner.store.notify();
            return;
        }
        if self
            .inner
            .store
            .captures
            .try_send(CaptureMessage::Event(capture))
            .is_err()
        {
            budget.fetch_sub(bytes, Ordering::Relaxed);
            state.incomplete.store(true, Ordering::Release);
        }
        self.inner.store.notify();
    }
}
impl Capture {
    fn persist(&self, db: &Connection, total: &mut i64) -> rusqlite::Result<()> {
        let generation = self.generation.to_string();
        let existing: bool = db.query_row(
            "SELECT EXISTS(SELECT 1 FROM generations WHERE id=?1)",
            [&generation],
            |r| r.get(0),
        )?;
        if !existing {
            let count: i64 = db.query_row("SELECT COUNT(*) FROM generations", [], |r| r.get(0))?;
            if count >= 4096 {
                self.incomplete.store(true, Ordering::Release);
                return Ok(());
            }
        }
        db.execute(
            "INSERT OR IGNORE INTO generations(id,terminal,label,agent,created,incomplete,cols,rows)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
            params![generation,self.terminal.to_string(),self.label,self.agent,self.at,
                self.kind != "start" || self.incomplete.load(Ordering::Acquire),self.cols,self.rows],
        )?;
        let previous: i64 = db.query_row(
            "SELECT last_event FROM generations WHERE id=?1",
            [&generation],
            |r| r.get(0),
        )?;
        let gap = self.sequence != previous + 1 || self.incomplete.load(Ordering::Acquire);
        if *total + self.data.len() as i64 > ARCHIVE_TOTAL_LIMIT && self.kind == "output" {
            self.incomplete.store(true, Ordering::Release);
            db.execute(
                "UPDATE generations SET incomplete=1 WHERE id=?1",
                [&generation],
            )?;
            return Ok(());
        }
        db.execute(
            "INSERT OR IGNORE INTO archive_events(generation,sequence,kind,data,cols,rows,at)
             VALUES(?1,?2,?3,?4,?5,?6,?7)",
            params![
                generation,
                self.sequence,
                self.kind,
                self.data.as_ref(),
                self.cols,
                self.rows,
                self.at
            ],
        )?;
        db.execute(
            "UPDATE generations SET last_event=?2,bytes=bytes+?3,incomplete=MAX(incomplete,?4),
             ended=MAX(ended,?5),ended_at=CASE WHEN ?5 THEN ?6 ELSE ended_at END WHERE id=?1",
            params![
                generation,
                self.sequence,
                self.data.len() as i64,
                gap,
                self.kind == "end",
                self.at
            ],
        )?;
        *total += self.data.len() as i64;
        Ok(())
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ArchiveGeneration {
    pub id: String,
    pub terminal_id: String,
    pub label: String,
    pub agent: String,
    pub created_at: i64,
    pub bytes: i64,
    pub incomplete: bool,
    pub ended: bool,
    pub cols: u16,
    pub rows: u16,
}
pub fn generations(db: &Connection, terminal: Option<String>) -> Result<Vec<ArchiveGeneration>> {
    let mut query = db.prepare(
        "SELECT id,terminal,label,agent,created,bytes,incomplete,ended,cols,rows FROM generations
         WHERE (?1 IS NULL OR terminal=?1) AND id NOT IN (SELECT generation FROM archive_cleanup) ORDER BY created DESC LIMIT 256",
    )?;
    Ok(query
        .query_map([terminal], |r| {
            Ok(ArchiveGeneration {
                id: r.get(0)?,
                terminal_id: r.get(1)?,
                label: r.get(2)?,
                agent: r.get(3)?,
                created_at: r.get(4)?,
                bytes: r.get(5)?,
                incomplete: r.get(6)?,
                ended: r.get(7)?,
                cols: r.get(8)?,
                rows: r.get(9)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?)
}
pub fn archive_events(db: &Connection, generation: &str, after: i64) -> Result<serde_json::Value> {
    use base64::Engine;
    let exists: Option<i64> = db
        .query_row("SELECT 1 FROM generations WHERE id=?1 AND id NOT IN (SELECT generation FROM archive_cleanup)", [generation], |r| {
            r.get(0)
        })
        .optional()?;
    if exists.is_none() {
        return Err(Error("not_found"));
    }
    let mut query = db.prepare(
        "SELECT sequence,kind,data,cols,rows FROM archive_events
         WHERE generation=?1 AND sequence>?2 ORDER BY sequence LIMIT 1024",
    )?;
    let mut rows = query.query(params![generation, after])?;
    let mut events = Vec::new();
    let mut bytes = 0;
    while let Some(row) = rows.next()? {
        let data: Vec<u8> = row.get(2)?;
        if bytes + data.len() > 512 * 1024 {
            break;
        }
        bytes += data.len();
        events.push(
            serde_json::json!({"sequence":row.get::<_,i64>(0)?,"kind":row.get::<_,String>(1)?,
            "data":base64::engine::general_purpose::STANDARD.encode(data),
            "cols":row.get::<_,u16>(3)?,"rows":row.get::<_,u16>(4)?}),
        );
    }
    Ok(serde_json::json!({"events":events}))
}

const SCHEMA: &str = "
CREATE TABLE generations(
 id TEXT PRIMARY KEY, terminal TEXT NOT NULL,label TEXT NOT NULL,agent TEXT NOT NULL,
 created INTEGER NOT NULL,bytes INTEGER NOT NULL DEFAULT 0,last_event INTEGER NOT NULL DEFAULT 0,
 incomplete INTEGER NOT NULL DEFAULT 0,ended INTEGER NOT NULL DEFAULT 0,cols INTEGER NOT NULL,rows INTEGER NOT NULL);
CREATE TABLE archive_events(
 generation TEXT NOT NULL REFERENCES generations(id) ON DELETE CASCADE,sequence INTEGER NOT NULL,
 kind TEXT NOT NULL,data BLOB NOT NULL,cols INTEGER NOT NULL,rows INTEGER NOT NULL,at INTEGER NOT NULL,
 PRIMARY KEY(generation,sequence));
CREATE TABLE invitations(
 id TEXT PRIMARY KEY,terminal TEXT NOT NULL,generation TEXT NOT NULL,secret BLOB NOT NULL,
 expires INTEGER NOT NULL,mode TEXT NOT NULL,revoked INTEGER NOT NULL DEFAULT 0,
 guest TEXT,request TEXT UNIQUE);
CREATE TABLE guests(
 id TEXT PRIMARY KEY,terminal TEXT NOT NULL,name TEXT NOT NULL,name_key TEXT NOT NULL,
 UNIQUE(terminal,name_key));
CREATE TABLE grants(
 id TEXT PRIMARY KEY,guest TEXT NOT NULL REFERENCES guests(id),terminal TEXT NOT NULL,
 generation TEXT NOT NULL,secret BLOB NOT NULL,mode TEXT NOT NULL,expires INTEGER NOT NULL,
 admitted INTEGER NOT NULL DEFAULT 0,revoked INTEGER NOT NULL DEFAULT 0,created INTEGER NOT NULL);
CREATE TABLE proposals(
 id TEXT PRIMARY KEY,guest TEXT NOT NULL REFERENCES guests(id),grant_id TEXT NOT NULL REFERENCES grants(id),
 terminal TEXT NOT NULL,generation TEXT NOT NULL,request TEXT NOT NULL,
 original TEXT,current TEXT,revision INTEGER NOT NULL DEFAULT 1,state TEXT NOT NULL DEFAULT 'pending',
 reason TEXT,approval_request TEXT UNIQUE,approved_text TEXT,created INTEGER NOT NULL,updated INTEGER NOT NULL,
 UNIQUE(guest,request));
CREATE INDEX proposal_terminal ON proposals(terminal,created);
CREATE INDEX grant_terminal ON grants(terminal);
CREATE TABLE approval_attempts(
 request TEXT PRIMARY KEY,proposal TEXT NOT NULL REFERENCES proposals(id),text TEXT NOT NULL,
 generation TEXT NOT NULL,revision INTEGER NOT NULL,outcome TEXT NOT NULL);
";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_compaction_preserves_data_and_rejects_an_unsafe_scratch_path() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("state");
        crate::workspaces::prepare_state_directory_sync(&directory).unwrap();
        let path = directory.join("sharing.sqlite3");
        drop(private_file(&path).unwrap());
        let mut db = Connection::open(&path).unwrap();
        db.execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; PRAGMA temp_store=MEMORY;
            CREATE TABLE preserved(id INTEGER PRIMARY KEY, data BLOB);
            INSERT INTO preserved VALUES(7,x'001b5b33316dff'); PRAGMA user_version=2;",
        )
        .unwrap();
        let scratch = directory.join(COMPACTION_COPY);
        std::fs::create_dir(&scratch).unwrap();
        assert!(matches!(
            compact_legacy(&mut db, &directory),
            Err(Error("unsafe_state_file"))
        ));
        assert_eq!(
            db.query_row("SELECT hex(data) FROM preserved WHERE id=7", [], |r| r
                .get::<_, String>(
                0
            ))
            .unwrap(),
            "001B5B33316DFF"
        );
        std::fs::remove_dir(&scratch).unwrap();
        compact_legacy(&mut db, &directory).unwrap();
        assert!(!scratch.exists());
        assert_eq!(
            db.pragma_query_value(None, "auto_vacuum", |r| r.get::<_, i64>(0))
                .unwrap(),
            2
        );
        assert_eq!(
            db.pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))
                .unwrap(),
            2
        );
        assert_eq!(
            db.query_row("SELECT hex(data) FROM preserved WHERE id=7", [], |r| r
                .get::<_, String>(
                0
            ))
            .unwrap(),
            "001B5B33316DFF"
        );
        assert_eq!(
            db.query_row("PRAGMA integrity_check", [], |r| r.get::<_, String>(0))
                .unwrap(),
            "ok"
        );
    }

    #[tokio::test]
    async fn startup_discards_only_the_private_interrupted_compaction_copy() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("state");
        crate::workspaces::prepare_state_directory_sync(&directory).unwrap();
        let scratch = directory.join(COMPACTION_COPY);
        private_file(&scratch).unwrap().set_len(4096).unwrap();
        let store = Store::open(directory).await.unwrap();
        assert!(!scratch.exists());
        assert!(store.storage_usage().await.is_ok());
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn windows_new_state_is_private_under_a_broad_parent_and_repair_preserves_files() {
        use std::os::windows::process::CommandExt;
        let root = tempfile::tempdir().unwrap();
        let changed = std::process::Command::new("icacls.exe")
            .arg(root.path())
            .args(["/grant", "*S-1-1-0:(OI)(CI)R"])
            .creation_flags(0x08000000)
            .output()
            .unwrap();
        assert!(changed.status.success());
        let directory = root.path().join("new-state");
        let store = Store::open(directory.clone()).await.unwrap();
        store.flush().await.unwrap();
        windows_privacy::check(&directory).unwrap();
        let legacy = root.path().join("legacy-state");
        std::fs::create_dir(&legacy).unwrap();
        std::fs::write(legacy.join("workspaces.json"), b"preserved workspace data").unwrap();
        std::fs::write(legacy.join("sharing.sqlite3"), b"preserved database bytes").unwrap();
        assert!(matches!(
            Store::open(legacy.clone()).await,
            Err(Error("unsafe_state_directory"))
        ));
        repair_storage_permissions(&legacy).unwrap();
        windows_privacy::check(&legacy).unwrap();
        check_file(&legacy.join("sharing.sqlite3")).unwrap();
        assert_eq!(
            std::fs::read(legacy.join("workspaces.json")).unwrap(),
            b"preserved workspace data"
        );
        assert_eq!(
            std::fs::read(legacy.join("sharing.sqlite3")).unwrap(),
            b"preserved database bytes"
        );
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn windows_repair_refuses_a_store_owned_by_another_running_server() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("state");
        let _store = Store::open(directory.clone()).await.unwrap();
        assert!(matches!(
            repair_storage_permissions(&directory),
            Err(Error("sharing_store_in_use"))
        ));
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn windows_accepts_owner_rights_for_a_trusted_owner() {
        use std::os::windows::process::CommandExt;
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("state");
        crate::workspaces::prepare_state_directory_sync(&directory).unwrap();
        let changed = std::process::Command::new("icacls.exe")
            .arg(&directory)
            .args(["/grant", "*S-1-3-4:(OI)(CI)F"])
            .creation_flags(0x08000000)
            .output()
            .unwrap();
        assert!(changed.status.success());
        let store = Store::open(directory).await.unwrap();
        store.flush().await.unwrap();
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn windows_rejects_a_database_readable_by_everyone() {
        use std::os::windows::process::CommandExt;
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("state");
        crate::workspaces::prepare_state_directory_sync(&directory).unwrap();
        let database = directory.join("sharing.sqlite3");
        drop(private_file(&database).unwrap());
        let changed = std::process::Command::new("icacls.exe")
            .arg(&database)
            .args(["/grant", "*S-1-1-0:(R)"])
            .creation_flags(0x08000000)
            .output()
            .unwrap();
        assert!(changed.status.success());
        assert!(matches!(
            Store::open(directory).await,
            Err(Error("unsafe_state_file"))
        ));
    }

    #[tokio::test]
    async fn critical_receipt_waits_for_full_metadata_queue_and_survives_cancellation() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(root.path().join("state")).await.unwrap();
        let (entered, ready) = oneshot::channel();
        let (release, wait) = mpsc::channel();
        let worker = store.clone();
        let blocked = tokio::spawn(async move {
            worker
                .run(move |_| {
                    let _ = entered.send(());
                    wait.recv().unwrap();
                    Ok(())
                })
                .await
        });
        ready.await.unwrap();
        for _ in 0..128 {
            store.commands.try_send(Box::new(|_| {})).unwrap();
        }
        assert!(matches!(
            store.run(|_| Ok(())).await,
            Err(Error("storage_busy"))
        ));
        let worker = store.clone();
        let (started, waiting) = oneshot::channel();
        let receipt = tokio::spawn(async move {
            let _ = started.send(());
            worker.run_critical(|db| {
                db.execute("INSERT INTO guests(id,terminal,name,name_key) VALUES('receipt','t','Alice','alice')", [])?;
                Ok(())
            }).await
        });
        waiting.await.unwrap();
        tokio::task::yield_now().await;
        assert!(!receipt.is_finished());
        // The critical task owns the blocking send before awaiting its result.
        receipt.abort();
        release.send(()).unwrap();
        blocked.await.unwrap().unwrap();
        let persisted = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if store
                    .run_critical(|db| {
                        Ok(db.query_row(
                            "SELECT EXISTS(SELECT 1 FROM guests WHERE id='receipt')",
                            [],
                            |r| r.get::<_, bool>(0),
                        )?)
                    })
                    .await
                    .unwrap()
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await;
        assert!(persisted.is_ok());
    }

    #[tokio::test]
    async fn paused_capture_preserves_sequences_and_never_backfills_private_output() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(root.path().join("state")).await.unwrap();
        let recorder = Recorder::new(
            store.clone(),
            Uuid::new_v4(),
            "Fixture".into(),
            "Codex".into(),
        );
        let generation = Uuid::new_v4();
        recorder.begin(generation, 80, 24, false);
        recorder.output(generation, b"public");
        recorder.pause();
        recorder.output(generation, b"private");
        recorder.resume().unwrap();
        recorder.begin(generation, 80, 24, true);
        recorder.output(generation, b"again");
        recorder.end(generation);
        recorder.end(generation);
        store.flush().await.unwrap();
        store
            .run(move |db| {
                let row = &generations(db, None)?[0];
                assert_eq!(row.bytes, 11);
                assert!(row.ended && row.incomplete);
                let events = archive_events(db, &generation.to_string(), 0)?;
                assert_eq!(events["events"].as_array().unwrap().len(), 4);
                Ok(())
            })
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn archive_pages_bound_bytes_and_batch_small_events() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(root.path().join("state")).await.unwrap();
        let recorder = Recorder::new(
            store.clone(),
            Uuid::new_v4(),
            "Fixture".into(),
            "Codex".into(),
        );
        let generation = Uuid::new_v4();
        recorder.begin(generation, 80, 24, false);
        for _ in 0..100 {
            recorder.output(generation, b"tiny");
        }
        for _ in 0..40 {
            recorder.output(generation, &vec![b'x'; 16 * 1024]);
        }
        store.flush().await.unwrap();
        store
            .run(move |db| {
                use base64::Engine;
                let first = archive_events(db, &generation.to_string(), 0)?;
                let events = first["events"].as_array().unwrap();
                assert!(events.len() > 100);
                let bytes: usize = events
                    .iter()
                    .map(|e| {
                        base64::engine::general_purpose::STANDARD
                            .decode(e["data"].as_str().unwrap())
                            .unwrap()
                            .len()
                    })
                    .sum();
                assert!(bytes <= 512 * 1024);
                let next = archive_events(
                    db,
                    &generation.to_string(),
                    events.last().unwrap()["sequence"].as_i64().unwrap(),
                )?;
                assert!(!next["events"].as_array().unwrap().is_empty());
                Ok(())
            })
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn capture_barrier_preserves_bytes_geometry_and_final_drain() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(root.path().join("state")).await.unwrap();
        let terminal = Uuid::new_v4();
        let generation = Uuid::new_v4();
        let recorder = Recorder::new(store.clone(), terminal, "Fixture".into(), "Codex".into());
        recorder.begin(generation, 80, 24, false);
        recorder.output(generation, b"\x1b[31mfirst\xff");
        recorder.geometry(generation, 120, 35);
        recorder.output(generation, b"final\n");
        recorder.end(generation);
        store.flush().await.unwrap();
        store
            .run(move |db| {
                let events = archive_events(db, &generation.to_string(), 0)?;
                let events = events["events"].as_array().unwrap();
                assert_eq!(events.len(), 5);
                assert_eq!(events[2]["kind"], "resize");
                assert_eq!(events[2]["cols"], 120);
                use base64::Engine;
                let raw = base64::engine::general_purpose::STANDARD
                    .decode(events[1]["data"].as_str().unwrap())
                    .unwrap();
                assert_eq!(raw, b"\x1b[31mfirst\xff");
                let rows = generations(db, None)?;
                assert!(rows[0].ended);
                assert!(!rows[0].incomplete);
                assert_eq!(rows[0].bytes, 17);
                Ok(())
            })
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn a_dropped_last_capture_persists_its_gap_without_future_output() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(root.path().join("state")).await.unwrap();
        let generation = Uuid::new_v4();
        let recorder = Recorder::new(
            store.clone(),
            Uuid::new_v4(),
            "Fixture".into(),
            "Codex".into(),
        );
        recorder.begin(generation, 80, 24, false);
        store.flush().await.unwrap();
        recorder.mark_incomplete(generation);
        store.flush().await.unwrap();
        store
            .run(|db| {
                assert!(generations(db, None)?[0].incomplete);
                Ok(())
            })
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn accepted_metadata_is_owned_after_request_cancellation_and_store_is_exclusive() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("state");
        let store = Store::open(path.clone()).await.unwrap();
        assert!(matches!(
            Store::open(path).await,
            Err(Error("sharing_store_in_use"))
        ));
        let (entered, ready) = oneshot::channel();
        let (release, wait) = mpsc::channel();
        let worker = store.clone();
        let request = tokio::spawn(async move {
            worker.run(move |db| {
                let _ = entered.send(());
                wait.recv().unwrap();
                db.execute("INSERT INTO guests(id,terminal,name,name_key) VALUES('g','t','Alice','alice')", [])?;
                Ok(())
            }).await
        });
        ready.await.unwrap();
        request.abort();
        release.send(()).unwrap();
        store
            .run(|db| {
                assert_eq!(
                    db.query_row("SELECT name FROM guests WHERE id='g'", [], |r| r
                        .get::<_, String>(0))?,
                    "Alice"
                );
                Ok(())
            })
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn full_capture_budget_never_blocks_the_pty_and_records_the_gap() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(root.path().join("state")).await.unwrap();
        let recorder = Recorder::new(
            store.clone(),
            Uuid::new_v4(),
            "Fixture".into(),
            "Codex".into(),
        );
        let generation = Uuid::new_v4();
        recorder.begin(generation, 80, 24, false);
        store.flush().await.unwrap();
        let (entered, ready) = oneshot::channel();
        let (release, wait) = mpsc::channel();
        let worker = store.clone();
        let blocked = tokio::spawn(async move {
            worker
                .run(move |_| {
                    let _ = entered.send(());
                    wait.recv().unwrap();
                    Ok(())
                })
                .await
        });
        ready.await.unwrap();
        let chunk = vec![b'x'; 16 * 1024];
        for _ in 0..=CAPTURE_BYTES / chunk.len() {
            recorder.output(generation, &chunk);
        }
        recorder.end(generation);
        assert!(recorder.incomplete(Some(generation)));
        assert!(store.queued_bytes.load(Ordering::Relaxed) <= CAPTURE_BYTES);
        release.send(()).unwrap();
        blocked.await.unwrap().unwrap();
        store.flush().await.unwrap();
        store
            .run(|db| {
                let archive = generations(db, None)?;
                assert!(archive[0].incomplete && archive[0].ended);
                assert_eq!(archive[0].bytes, CAPTURE_BYTES as i64);
                Ok(())
            })
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn reopening_revokes_access_and_preserves_uncertain_intent_without_resending() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("state");
        let store = Store::open(directory.clone()).await.unwrap();
        store.run(|db| {
            db.execute_batch("INSERT INTO guests(id,terminal,name,name_key) VALUES('g','t','Alice','alice');
                INSERT INTO grants(id,guest,terminal,generation,secret,mode,expires,created,admitted)
                VALUES('grant','g','t','s',X'00','propose',9223372036854775807,1,1);
                INSERT INTO proposals(id,guest,grant_id,terminal,generation,request,original,current,state,created,updated)
                VALUES('p','g','grant','t','s','r','original','reviewed','queued',1,1);
                INSERT INTO approval_attempts(request,proposal,text,generation,revision,outcome)
                VALUES('approval','p','reviewed','s',1,'intent');")?;
            Ok(())
        }).await.unwrap();
        store.flush().await.unwrap();
        drop(store);
        let reopened = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                match Store::open(directory.clone()).await {
                    Ok(store) => break store,
                    Err(Error("sharing_store_in_use")) => {
                        tokio::time::sleep(Duration::from_millis(10)).await
                    }
                    Err(error) => panic!("{error}"),
                }
            }
        })
        .await
        .expect("store worker releases its lease");
        reopened
            .run(|db| {
                assert!(
                    db.query_row("SELECT revoked FROM grants WHERE id='grant'", [], |r| r
                        .get::<_, bool>(0))?
                );
                assert_eq!(
                    db.query_row("SELECT state FROM proposals WHERE id='p'", [], |r| r
                        .get::<_, String>(0))?,
                    "uncertain"
                );
                assert_eq!(
                    db.query_row(
                        "SELECT outcome FROM approval_attempts WHERE request='approval'",
                        [],
                        |r| r.get::<_, String>(0)
                    )?,
                    "uncertain"
                );
                assert_eq!(
                    db.query_row("SELECT original FROM proposals WHERE id='p'", [], |r| {
                        r.get::<_, String>(0)
                    })?,
                    "original"
                );
                Ok(())
            })
            .await
            .unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn database_permissions_are_private_and_public_sidecars_are_rejected() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("state");
        let store = Store::open(path.clone()).await.unwrap();
        assert_eq!(
            std::fs::metadata(path.join("sharing.sqlite3"))
                .unwrap()
                .mode()
                & 0o777,
            0o600
        );
        store.flush().await.unwrap();
        let unsafe_path = root.path().join("unsafe");
        std::fs::create_dir(&unsafe_path).unwrap();
        std::fs::set_permissions(&unsafe_path, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::write(unsafe_path.join("sharing.sqlite3-wal"), b"").unwrap();
        std::fs::set_permissions(
            unsafe_path.join("sharing.sqlite3-wal"),
            std::fs::Permissions::from_mode(0o644),
        )
        .unwrap();
        assert!(matches!(
            Store::open(unsafe_path).await,
            Err(Error("unsafe_state_file"))
        ));
        let linked_path = root.path().join("linked");
        std::fs::create_dir(&linked_path).unwrap();
        std::fs::set_permissions(&linked_path, std::fs::Permissions::from_mode(0o700)).unwrap();
        let outside = root.path().join("must-not-create");
        std::os::unix::fs::symlink(&outside, linked_path.join("sharing.sqlite3-wal")).unwrap();
        assert!(matches!(
            Store::open(linked_path).await,
            Err(Error("unsafe_state_file"))
        ));
        assert!(!outside.exists());
    }
}
