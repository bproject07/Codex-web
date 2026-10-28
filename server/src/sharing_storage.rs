//! Bounded archive cleanup, opt-in retention and physical disk accounting.
//! All database operations run on the sharing actor, never under a PTY lock.
use crate::sharing_store::{Error, Result, now};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::Path;

pub(crate) type ProtectionProbe = std::sync::Arc<dyn Fn(&str, &str) -> bool + Send + Sync>;

pub(crate) fn refresh_protection(
    db: &mut Connection,
    probe: Option<ProtectionProbe>,
) -> Result<()> {
    // Inspect only terminals whose archive has drained. An unrelated terminal
    // may hold its lifecycle lock during a native process operation.
    let mut query = db.prepare("SELECT id,terminal FROM generations WHERE ended=1")?;
    let ended = query
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(query);
    let protected: Vec<_> = ended
        .into_iter()
        .filter(|(id, terminal)| probe.as_ref().is_some_and(|p| p(terminal, id)))
        .collect();
    let tx = db.transaction()?;
    tx.execute("DELETE FROM live_recording_generations", [])?;
    for (id, _) in protected {
        tx.execute(
            "INSERT INTO live_recording_generations(id) VALUES(?1)",
            [id],
        )?;
    }
    tx.commit()?;
    Ok(())
}

fn unsigned(row: &rusqlite::Row<'_>, index: usize) -> rusqlite::Result<u64> {
    u64::try_from(row.get::<_, i64>(index)?).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            index,
            rusqlite::types::Type::Integer,
            Box::new(error),
        )
    })
}

pub const MIGRATION: &str = "
ALTER TABLE generations ADD COLUMN ended_at INTEGER;
CREATE TABLE archive_cleanup(generation TEXT PRIMARY KEY REFERENCES generations(id) ON DELETE CASCADE);
CREATE TABLE storage_policy(id INTEGER PRIMARY KEY CHECK(id=1),enabled INTEGER NOT NULL DEFAULT 0,
 retention_days INTEGER NOT NULL DEFAULT 30,max_bytes INTEGER NOT NULL DEFAULT 536870912);
INSERT INTO storage_policy(id) VALUES(1);";

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Policy {
    pub enabled: bool,
    pub retention_days: u32,
    pub max_bytes: u64,
}

pub fn policy(db: &Connection) -> Result<Policy> {
    Ok(db.query_row(
        "SELECT enabled,retention_days,max_bytes FROM storage_policy WHERE id=1",
        [],
        |r| {
            Ok(Policy {
                enabled: r.get(0)?,
                retention_days: r.get(1)?,
                max_bytes: unsigned(r, 2)?,
            })
        },
    )?)
}

pub fn save_policy(db: &Connection, value: &Policy) -> Result<()> {
    if !(1..=3650).contains(&value.retention_days)
        || !(16 * 1024 * 1024..=2 * 1024 * 1024 * 1024).contains(&value.max_bytes)
    {
        return Err(Error("invalid_storage_policy"));
    }
    db.execute(
        "UPDATE storage_policy SET enabled=?1,retention_days=?2,max_bytes=?3 WHERE id=1",
        params![value.enabled, value.retention_days, value.max_bytes as i64],
    )?;
    Ok(())
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CleanupFilter {
    /// Omitted means all eligible finished recordings.
    pub older_than_days: Option<u32>,
    /// Omitted means all recordings matching the age filter.
    pub generations: Option<Vec<uuid::Uuid>>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CleanupPreview {
    pub count: usize,
    pub bytes: u64,
    pub fingerprint: String,
}

fn candidates(db: &Connection, filter: &CleanupFilter) -> Result<Vec<(String, u64)>> {
    if filter
        .older_than_days
        .is_some_and(|d| !(1..=3650).contains(&d))
        || filter.generations.as_ref().is_some_and(|v| v.len() > 256)
    {
        return Err(Error("invalid_body"));
    }
    let cutoff = filter
        .older_than_days
        .map(|d| now() - i64::from(d) * 86_400_000);
    let mut query = db.prepare("SELECT id,bytes FROM generations g WHERE ended=1
        AND (?1 IS NULL OR ended_at<?1) AND NOT EXISTS(SELECT 1 FROM archive_cleanup c WHERE c.generation=g.id)
        AND id NOT IN (SELECT id FROM live_recording_generations)
        AND NOT EXISTS(SELECT 1 FROM proposals p WHERE p.generation=g.id AND p.state IN ('approving','queued'))
        ORDER BY ended_at,id LIMIT 4096")?;
    let rows = query
        .query_map([cutoff], |r| Ok((r.get::<_, String>(0)?, unsigned(r, 1)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows
        .into_iter()
        .filter(|(id, _)| {
            filter
                .generations
                .as_ref()
                .is_none_or(|ids| ids.iter().any(|v| v.to_string() == *id))
        })
        .collect())
}

fn preview_rows(rows: &[(String, u64)]) -> CleanupPreview {
    let mut hash = Sha256::new();
    for (id, bytes) in rows {
        hash.update(id.as_bytes());
        hash.update(bytes.to_le_bytes());
    }
    CleanupPreview {
        count: rows.len(),
        bytes: rows.iter().map(|r| r.1).sum(),
        fingerprint: format!("{:x}", hash.finalize()),
    }
}

pub fn preview(db: &Connection, filter: &CleanupFilter) -> Result<CleanupPreview> {
    Ok(preview_rows(&candidates(db, filter)?))
}

pub fn enqueue_cleanup(
    db: &mut Connection,
    filter: &CleanupFilter,
    expected: &str,
) -> Result<CleanupPreview> {
    let tx = db.transaction()?;
    let rows = candidates(&tx, filter)?;
    let result = preview_rows(&rows);
    if result.fingerprint != expected {
        return Err(Error("cleanup_changed"));
    }
    for (id, _) in rows {
        tx.execute(
            "INSERT OR IGNORE INTO archive_cleanup(generation) VALUES(?1)",
            [id],
        )?;
    }
    tx.commit()?;
    Ok(result)
}

/// At most 256 events (4 MiB of terminal bytes) per actor iteration.
/// Durable requests survive HTTP cancellation and server restarts.
pub fn cleanup_step(db: &mut Connection) -> Result<bool> {
    let id: Option<String> = db
        .query_row(
        "SELECT generation FROM archive_cleanup WHERE generation NOT IN (SELECT id FROM live_recording_generations) ORDER BY generation LIMIT 1",
            [],
            |r| r.get(0),
        )
        .optional()?;
    let Some(id) = id else {
        return Ok(false);
    };
    let tx = db.transaction()?;
    tx.execute(
        "DELETE FROM archive_events WHERE generation=?1 AND sequence IN
        (SELECT sequence FROM archive_events WHERE generation=?1 ORDER BY sequence LIMIT 256)",
        [&id],
    )?;
    let remains: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM archive_events WHERE generation=?1)",
        [&id],
        |r| r.get(0),
    )?;
    if !remains {
        tx.execute("DELETE FROM generations WHERE id=?1 AND ended=1", [&id])?;
    }
    tx.commit()?;
    Ok(true)
}

pub fn retention(db: &mut Connection) -> Result<()> {
    let setting = policy(db)?;
    if !setting.enabled {
        return Ok(());
    }
    let cutoff = now() - i64::from(setting.retention_days) * 86_400_000;
    let mut total: u64 = db.query_row("SELECT COALESCE(SUM(bytes),0) FROM generations WHERE id NOT IN (SELECT generation FROM archive_cleanup)",[],|r|unsigned(r,0))?;
    let rows = candidates(db, &CleanupFilter::default())?;
    let tx = db.transaction()?;
    for (id, bytes) in rows {
        let old: bool = tx.query_row(
            "SELECT COALESCE(ended_at<?2,0) FROM generations WHERE id=?1",
            params![id, cutoff],
            |r| r.get(0),
        )?;
        if old || total > setting.max_bytes {
            tx.execute(
                "INSERT OR IGNORE INTO archive_cleanup(generation) VALUES(?1)",
                [id],
            )?;
            total = total.saturating_sub(bytes);
        }
    }
    tx.commit()?;
    Ok(())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Usage {
    pub database_bytes: u64,
    pub wal_bytes: u64,
    pub shared_memory_bytes: u64,
    pub disk_bytes: u64,
    pub reusable_bytes: u64,
    pub archive_bytes: u64,
    pub recordings: u64,
    pub cleanup_pending: u64,
    pub free_disk_bytes: u64,
    pub incremental_compaction: bool,
    pub policy: Policy,
}

pub fn usage(db: &Connection, directory: &Path) -> Result<Usage> {
    let size = |name: &str| -> Result<u64> {
        match std::fs::metadata(directory.join(name)) {
            Ok(m) => Ok(m.len()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(0),
            Err(_) => Err(Error("storage_unavailable")),
        }
    };
    let database_bytes = size("sharing.sqlite3")?;
    let wal_bytes = size("sharing.sqlite3-wal")?;
    let shared_memory_bytes = size("sharing.sqlite3-shm")?;
    let page_size: u64 = db.pragma_query_value(None, "page_size", |r| unsigned(r, 0))?;
    let free_pages: u64 = db.pragma_query_value(None, "freelist_count", |r| unsigned(r, 0))?;
    let mode: u32 = db.pragma_query_value(None, "auto_vacuum", |r| r.get(0))?;
    let (recordings, archive_bytes) = db.query_row(
        "SELECT COUNT(*),COALESCE(SUM(bytes),0) FROM generations",
        [],
        |r| Ok((unsigned(r, 0)?, unsigned(r, 1)?)),
    )?;
    Ok(Usage {
        database_bytes,
        wal_bytes,
        shared_memory_bytes,
        disk_bytes: database_bytes + wal_bytes + shared_memory_bytes,
        reusable_bytes: free_pages * page_size,
        archive_bytes,
        recordings,
        cleanup_pending: db.query_row("SELECT COUNT(*) FROM archive_cleanup", [], |r| {
            unsigned(r, 0)
        })?,
        free_disk_bytes: fs2::available_space(directory)
            .map_err(|_| Error("storage_unavailable"))?,
        incremental_compaction: mode == 2,
        policy: policy(db)?,
    })
}

/// A small step during normal operation. Legacy databases require an explicit
/// maintenance conversion; changing the pragma alone would not convert them.
pub fn compact_step(db: &Connection) -> Result<()> {
    let mode: u32 = db.pragma_query_value(None, "auto_vacuum", |r| r.get(0))?;
    if mode == 2 {
        db.execute_batch("PRAGMA incremental_vacuum(64);")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sharing_store::{Recorder, Store};
    use uuid::Uuid;

    fn seed(db: &Connection, id: &str, ended: bool, age: i64, events: usize) -> Result<()> {
        db.execute("INSERT INTO generations(id,terminal,label,agent,created,bytes,ended,ended_at,cols,rows)
            VALUES(?1,'retired','Fixture','Codex',?2,?3,?4,?2,80,24)",params![id,now()-age,events as i64*16384,ended])?;
        for sequence in 1..=events {
            db.execute(
                "INSERT INTO archive_events(generation,sequence,kind,data,cols,rows,at)
            VALUES(?1,?2,'output',?3,0,0,?4)",
                params![id, sequence as i64, vec![b'x'; 16384], now()],
            )?;
        }
        Ok(())
    }

    #[tokio::test]
    async fn cleanup_is_bounded_snapshot_checked_and_preserves_live_history_and_identities() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(root.path().join("state")).await.unwrap();
        store.run(|db| {
            seed(db,"finished",true,0,600)?;
            seed(db,"active",false,0,2)?;
            db.execute("INSERT INTO guests(id,terminal,name,name_key) VALUES('guest','retired','Reserved','reserved')",[])?;
            let filter=CleanupFilter::default();
            let before=preview(db,&filter)?;
            assert_eq!((before.count,before.bytes),(1,600*16384));
            seed(db,"changed",true,0,1)?;
            assert!(matches!(enqueue_cleanup(db,&filter,&before.fingerprint),Err(Error("cleanup_changed"))));
            assert_eq!(db.query_row("SELECT COUNT(*) FROM archive_cleanup",[],|r|r.get::<_,i64>(0))?,0);
            let reviewed=preview(db,&filter)?;
            enqueue_cleanup(db,&filter,&reviewed.fingerprint)?;
            assert!(cleanup_step(db)?);
            // The lexicographic first recording is small; next is bounded at 256 events.
            assert!(cleanup_step(db)?);
            let remaining:i64=db.query_row("SELECT COUNT(*) FROM archive_events WHERE generation='finished'",[],|r|r.get(0))?;
            assert_eq!(remaining,344);
            while cleanup_step(db)? {}
            assert_eq!(db.query_row("SELECT COUNT(*) FROM generations",[],|r|r.get::<_,i64>(0))?,1);
            assert_eq!(db.query_row("SELECT COUNT(*) FROM archive_events WHERE generation='active'",[],|r|r.get::<_,i64>(0))?,2);
            assert_eq!(db.query_row("SELECT name FROM guests",[],|r|r.get::<_,String>(0))?,"Reserved");
            Ok(())
        }).await.unwrap();
    }

    #[tokio::test]
    async fn retention_is_opt_in_preserves_active_and_uses_completion_age() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(root.path().join("state")).await.unwrap();
        store
            .run(|db| {
                seed(db, "old", true, 40 * 86_400_000, 1)?;
                seed(db, "recent", true, 0, 1)?;
                seed(db, "live", false, 60 * 86_400_000, 1)?;
                retention(db)?;
                assert_eq!(
                    db.query_row("SELECT COUNT(*) FROM archive_cleanup", [], |r| r
                        .get::<_, i64>(0))?,
                    0
                );
                save_policy(
                    db,
                    &Policy {
                        enabled: true,
                        retention_days: 30,
                        max_bytes: 16 * 1024 * 1024,
                    },
                )?;
                retention(db)?;
                assert_eq!(
                    db.query_row("SELECT generation FROM archive_cleanup", [], |r| r
                        .get::<_, String>(0))?,
                    "old"
                );
                assert!(matches!(
                    save_policy(
                        db,
                        &Policy {
                            enabled: true,
                            retention_days: 0,
                            max_bytes: 0
                        }
                    ),
                    Err(Error("invalid_storage_policy"))
                ));
                Ok(())
            })
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn reader_completion_does_not_make_a_live_generation_eligible() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(root.path().join("state")).await.unwrap();
        store
            .run(|db| {
                seed(db, "drained-but-running", true, 40 * 86_400_000, 1)?;
                refresh_protection(
                    db,
                    Some(std::sync::Arc::new(|_, generation| {
                        generation == "drained-but-running"
                    })),
                )?;
                assert_eq!(preview(db, &CleanupFilter::default())?.count, 0);
                save_policy(
                    db,
                    &Policy {
                        enabled: true,
                        retention_days: 30,
                        max_bytes: 16 * 1024 * 1024,
                    },
                )?;
                retention(db)?;
                assert_eq!(
                    db.query_row("SELECT COUNT(*) FROM archive_cleanup", [], |r| r
                        .get::<_, i64>(0))?,
                    0
                );
                Ok(())
            })
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn retention_budget_accounts_for_active_recordings_without_deleting_them() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(root.path().join("state")).await.unwrap();
        store
            .run(|db| {
                seed(db, "old", true, 1000, 1)?;
                seed(db, "recent", true, 0, 1)?;
                seed(db, "live", false, 0, 1)?;
                db.execute("UPDATE generations SET bytes=16777216", [])?;
                save_policy(
                    db,
                    &Policy {
                        enabled: true,
                        retention_days: 30,
                        max_bytes: 16 * 1024 * 1024,
                    },
                )?;
                retention(db)?;
                assert_eq!(
                    db.query_row("SELECT COUNT(*) FROM archive_cleanup", [], |r| r
                        .get::<_, i64>(0))?,
                    2
                );
                assert_eq!(
                    db.query_row(
                        "SELECT COUNT(*) FROM archive_cleanup WHERE generation='live'",
                        [],
                        |r| r.get::<_, i64>(0)
                    )?,
                    0
                );
                Ok(())
            })
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn compaction_requires_paused_recorders_and_releases_admission_after_failure() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(root.path().join("state")).await.unwrap();
        let recorder = Recorder::new(
            store.clone(),
            Uuid::new_v4(),
            "Fixture".into(),
            "Codex".into(),
        );
        assert!(matches!(
            store.compact().await,
            Err(Error("recording_enabled"))
        ));
        recorder.pause();
        assert!(store.compact().await.is_ok());
        recorder.resume().unwrap();
        assert!(matches!(
            store.compact().await,
            Err(Error("recording_enabled"))
        ));
        drop(recorder);
        assert!(store.compact().await.is_ok());
    }

    #[tokio::test]
    async fn failed_maintenance_and_pending_receipts_preserve_admission_and_data() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(root.path().join("state")).await.unwrap();
        store.run(|db| {
            seed(db,"pending",true,0,1)?;
            db.execute("INSERT INTO guests(id,terminal,name,name_key) VALUES('guest','retired','Fixture','fixture')",[])?;
            db.execute("INSERT INTO grants(id,guest,terminal,generation,secret,mode,expires,created) VALUES('grant','guest','retired','pending',zeroblob(32),'propose',0,0)",[])?;
            db.execute("INSERT INTO proposals(id,guest,grant_id,terminal,generation,request,state,created,updated) VALUES('proposal','guest','grant','retired','pending','request','queued',0,0)",[])?;
            assert_eq!(preview(db,&CleanupFilter::default())?.count,0);
            Ok(())
        }).await.unwrap();
        assert!(matches!(
            store.compact().await,
            Err(Error("delivery_pending"))
        ));
        assert!(store.recording_permit().is_ok());
        store
            .run(|db| {
                assert_eq!(
                    db.query_row("SELECT COUNT(*) FROM archive_events", [], |r| r
                        .get::<_, i64>(0))?,
                    1
                );
                Ok(())
            })
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn cancelling_the_http_owner_does_not_release_maintenance_early() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(root.path().join("state")).await.unwrap();
        let (entered, ready) = tokio::sync::oneshot::channel();
        let (release, blocked) = std::sync::mpsc::channel();
        let worker = store.clone();
        let blocker = tokio::spawn(async move {
            worker
                .run(move |_| {
                    let _ = entered.send(());
                    blocked.recv().unwrap();
                    Ok(())
                })
                .await
        });
        ready.await.unwrap();
        let worker = store.clone();
        let request = tokio::spawn(async move { worker.compact().await });
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                match store.recording_permit() {
                    Err(Error("storage_maintenance")) => break,
                    Ok(permit) => drop(permit),
                    Err(error) => panic!("unexpected admission error: {error}"),
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        request.abort();
        let _ = request.await;
        assert!(matches!(
            store.recording_permit(),
            Err(Error("storage_maintenance"))
        ));
        release.send(()).unwrap();
        blocker.await.unwrap().unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                if let Ok(permit) = store.recording_permit() {
                    drop(permit);
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        store
            .run(|db| {
                assert_eq!(
                    db.query_row("PRAGMA integrity_check", [], |r| r.get::<_, String>(0))?,
                    "ok"
                );
                Ok(())
            })
            .await
            .unwrap();
    }
}
