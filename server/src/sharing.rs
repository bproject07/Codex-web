//! Scoped human sharing. Durable decisions are serialized by the store worker.
use crate::{
    peer::SessionPurpose,
    registry::SessionRegistry,
    session::{Lifecycle, SessionManager, WriteReceipt},
    sharing_store::{self, Error, Recorder, Result, Store, lock, now},
};
use base64::Engine;
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Instant,
};
use subtle::ConstantTimeEq;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

const GUESTS_PER_SESSION: i64 = 8;
const GUESTS_PER_SERVER: i64 = 32;
const TEXT_LIMIT: usize = 16 * 1024;
const LEDGER_LIMIT: i64 = 16_384;
const INVITATION_MS: i64 = 15 * 60 * 1000;
const GRANT_MS: i64 = 12 * 60 * 60 * 1000;
const PROPOSAL_MS: i64 = 15 * 60 * 1000;
type InvitationRow = (String, String, Vec<u8>, i64, String, i64, Option<String>);
type ReconciliationCache = HashMap<Uuid, (Option<Uuid>, Lifecycle, Instant)>;

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Approval {
    pub request_id: Uuid,
    pub session_id: Uuid,
    pub revision: i64,
    pub text: String,
    pub ready: bool,
}

#[derive(Clone)]
pub struct Sharing {
    store: Option<Store>,
    sessions: SessionRegistry,
    runtime: Arc<Runtime>,
}
struct Runtime {
    guests: Arc<Semaphore>,
    connections: Mutex<HashMap<Uuid, Presence>>,
    closed: CancellationToken,
    failures: Mutex<HashMap<std::net::IpAddr, (Instant, u8)>>,
    receipts: Arc<Semaphore>,
    reconciled: Mutex<ReconciliationCache>,
}
#[derive(Clone)]
struct Presence {
    grant: Uuid,
    terminal: Uuid,
    visible: bool,
    seen: Instant,
    cancel: CancellationToken,
}
pub struct GuestConnection {
    id: Uuid,
    runtime: Arc<Runtime>,
    pub cancel: CancellationToken,
    _permit: OwnedSemaphorePermit,
}
impl GuestConnection {
    pub fn touch(&self, visible: Option<bool>) {
        if let Some(presence) = lock(&self.runtime.connections).get_mut(&self.id) {
            presence.seen = Instant::now();
            if let Some(visible) = visible {
                presence.visible = visible;
            }
        }
    }
}
impl Drop for GuestConnection {
    fn drop(&mut self) {
        lock(&self.runtime.connections).remove(&self.id);
    }
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Guest {
    pub grant_id: Uuid,
    pub guest_id: Uuid,
    #[serde(skip)]
    pub terminal_id: Uuid,
    pub session_id: Uuid,
    pub name: String,
    pub mode: String,
    pub expires_at: i64,
    pub admitted: bool,
}

impl Sharing {
    pub fn new(store: Option<Store>, sessions: SessionRegistry) -> Self {
        Self {
            store,
            sessions,
            runtime: Arc::new(Runtime {
                guests: Arc::new(Semaphore::new(64)),
                connections: Mutex::new(HashMap::new()),
                closed: CancellationToken::new(),
                failures: Mutex::new(HashMap::new()),
                receipts: Arc::new(Semaphore::new(64)),
                reconciled: Mutex::new(HashMap::new()),
            }),
        }
    }
    pub fn store(&self) -> Result<Store> {
        if self.runtime.closed.is_cancelled() {
            return Err(Error("sharing_closed"));
        }
        self.store.clone().ok_or(Error("storage_unavailable"))
    }
    pub fn check_attempt(&self, address: std::net::IpAddr) -> Result<()> {
        let mut failures = lock(&self.runtime.failures);
        failures.retain(|_, (at, _)| at.elapsed().as_secs() < 60);
        if failures.get(&address).is_some_and(|(_, count)| *count >= 5)
            || (failures.len() >= 4096 && !failures.contains_key(&address))
        {
            return Err(Error("rate_limited"));
        }
        Ok(())
    }
    pub fn failed_attempt(&self, address: std::net::IpAddr) {
        let mut failures = lock(&self.runtime.failures);
        if failures.len() >= 4096 && !failures.contains_key(&address) {
            return;
        }
        let entry = failures.entry(address).or_insert((Instant::now(), 0));
        entry.1 = entry.1.saturating_add(1);
    }
    pub fn close(&self) {
        self.runtime.closed.cancel();
        for presence in lock(&self.runtime.connections).values() {
            presence.cancel.cancel();
        }
    }
    pub async fn drain_receipts(&self) {
        // PTYs have stopped, so every accepted write now has a receipt or a
        // closed sender. A disk failure remains durable intent for recovery.
        if tokio::time::timeout(
            std::time::Duration::from_secs(5),
            self.runtime.receipts.acquire_many(64),
        )
        .await
        .is_err()
        {
            tracing::warn!("sharing receipt persistence incomplete during shutdown");
        }
    }
    fn session(&self, terminal: Uuid) -> Result<SessionManager> {
        let session = self.sessions.get(terminal).ok_or(Error("not_found"))?;
        if !matches!(session.purpose(), SessionPurpose::Interactive) {
            return Err(Error("ordinary_sessions_only"));
        }
        Ok(session)
    }
    pub fn record(&self, terminal: Uuid) -> Result<()> {
        let session = self.session(terminal)?;
        let snapshot = session.snapshot();
        session.enable_recording(Recorder::new(
            self.store()?,
            terminal,
            snapshot.name,
            snapshot.agent.label().to_owned(),
        ));
        Ok(())
    }
    pub fn stop_recording(&self, terminal: Uuid) -> Result<()> {
        self.session(terminal)?.pause_recording();
        Ok(())
    }

    pub async fn indicator(&self, terminal: Uuid) -> Result<Value> {
        let snapshot = self.session(terminal)?.snapshot();
        let Some(store) = self.store.clone() else {
            return Ok(json!({"available":false}));
        };
        store.run(move |db| {
            let generation = snapshot.session_id.map(|id| id.to_string());
            let guests: i64 = db.query_row("SELECT COUNT(*) FROM grants WHERE terminal=?1 AND generation=?2 AND revoked=0 AND expires>?3",
                params![terminal.to_string(),generation,now()], |r|r.get(0))?;
            let pending: i64 = if snapshot.status == Lifecycle::Running {
                db.query_row("SELECT COUNT(*) FROM proposals WHERE terminal=?1 AND generation=?2 AND state='pending' AND created>?3",
                    params![terminal.to_string(),generation,now()-PROPOSAL_MS], |r|r.get(0))?
            } else { 0 };
            Ok(json!({"available":true,"guests":guests,"pending":pending}))
        }).await
    }

    /// Only retired terminal identities are removable. Their credentials and
    /// proposal IDs can never target a live registry entry, even on delayed retry.
    pub async fn prune_retired(&self) -> Result<Value> {
        let sessions = self.sessions.clone();
        self.store()?.run(move |db| {
            let live: Vec<String> = sessions.list().iter().map(|s|s.terminal_id.to_string()).collect();
            let mut query = db.prepare("SELECT terminal FROM invitations UNION SELECT terminal FROM guests UNION SELECT terminal FROM proposals")?;
            let retired = query.query_map([], |r|r.get::<_,String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?
                .into_iter().filter(|terminal|!live.contains(terminal)).collect::<Vec<_>>();
            drop(query);
            let tx = db.transaction()?;
            for terminal in &retired {
                // An owned receipt may still be completing after deletion.
                let active: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM proposals WHERE terminal=?1 AND state IN ('approving','queued'))", [terminal], |r|r.get(0))?;
                if active { continue; }
                tx.execute("DELETE FROM approval_attempts WHERE proposal IN (SELECT id FROM proposals WHERE terminal=?1)", [terminal])?;
                tx.execute("DELETE FROM proposals WHERE terminal=?1", [terminal])?;
                tx.execute("DELETE FROM grants WHERE terminal=?1", [terminal])?;
                tx.execute("DELETE FROM invitations WHERE terminal=?1", [terminal])?;
                tx.execute("DELETE FROM guests WHERE terminal=?1", [terminal])?;
            }
            tx.commit()?;
            ledger_usage(db)
        }).await
    }
    pub async fn owner_view(&self, terminal: Uuid) -> Result<Value> {
        self.session(terminal)?;
        let sessions = self.sessions.clone();
        let runtime = self.runtime.clone();
        self.store()?.run(move |db| {
            let session = sessions.get(terminal).ok_or(Error("not_found"))?.snapshot();
            let reconcile = {
                let mut cache = lock(&runtime.reconciled);
                let due = cache.get(&terminal).is_none_or(|(generation,status,at)|
                    *generation != session.session_id || *status != session.status || at.elapsed().as_secs() >= 10);
                if due {
                    if cache.len() >= 256 { cache.clear(); }
                    cache.insert(terminal, (session.session_id,session.status,Instant::now()));
                }
                due
            };
            if reconcile {
            expire(db)?;
            let generation=session.session_id.map(|id|id.to_string());
            db.execute("UPDATE grants SET revoked=1 WHERE terminal=?1 AND (?2 IS NULL OR generation<>?2)",
                params![terminal.to_string(),generation])?;
            db.execute("UPDATE invitations SET revoked=1 WHERE terminal=?1 AND (?2 IS NULL OR generation<>?2)",
                params![terminal.to_string(),generation])?;
            db.execute("UPDATE proposals SET state='invalidated',reason='generation_ended',updated=?3
                WHERE terminal=?1 AND state='pending' AND (?2 IS NULL OR generation<>?2 OR ?4)",
                params![terminal.to_string(),generation,now(),session.status!=Lifecycle::Running])?;
            }
            let guests = guest_rows(db, terminal, &runtime)?;
            let proposals = proposal_rows(db, terminal, None, None)?;
            Ok(json!({"sessionId":session.session_id,"status":session.status,
                "recording":session.recording,"archiveIncomplete":session.archive_incomplete,
                "guests":guests,"proposals":proposals,"usage":ledger_usage(db)?}))
        }).await
    }
    pub async fn invite(&self, terminal: Uuid, mode: String) -> Result<Value> {
        if !matches!(mode.as_str(), "view" | "propose") {
            return Err(Error("invalid_mode"));
        }
        self.record(terminal)?;
        let sessions = self.sessions.clone();
        self.store()?.run(move |db| {
            expire(db)?;
            reconcile_generations(db, &sessions)?;
            let snapshot = sessions.get(terminal).ok_or(Error("not_found"))?.snapshot();
            let generation = snapshot.session_id.ok_or(Error("not_running"))?;
            let invitations: i64 = db.query_row(
                "SELECT COUNT(*) FROM invitations WHERE terminal=?1 AND revoked=0 AND guest IS NULL AND expires>?2",
                params![terminal.to_string(),now()], |r|r.get(0))?;
            let ledger: i64 = db.query_row("SELECT COUNT(*) FROM invitations", [], |r|r.get(0))?;
            // Invitations do not reserve a grant slot. Redemption enforces
            // capacity, allowing credential rotation even at the guest limit.
            if invitations >= 4 || ledger >= LEDGER_LIMIT {
                return Err(Error("sharing_capacity"));
            }
            let id = Uuid::new_v4();
            let secret = crate::auth::generate_token().map_err(|_|Error("random_unavailable"))?;
            let expiry = now()+INVITATION_MS;
            db.execute("INSERT INTO invitations(id,terminal,generation,secret,expires,mode) VALUES(?1,?2,?3,?4,?5,?6)",
                params![id.to_string(),terminal.to_string(),generation.to_string(),digest(&secret),expiry,mode])?;
            Ok(json!({"invitation":format!("{id}.{secret}"),"expiresAt":expiry}))
        }).await
    }
    /// A browser-generated credential makes redemption retryable without storing
    /// a recoverable bearer secret in SQLite or returning it in a response.
    pub async fn redeem(
        &self,
        invitation: String,
        credential: String,
        name: String,
        previous_credential: Option<String>,
    ) -> Result<Guest> {
        let (invitation_id, invitation_secret) = credential_parts(&invitation)?;
        let (grant_id, grant_secret) = credential_parts(&credential)?;
        let name = validate_name(&name)?;
        let sessions = self.sessions.clone();
        let runtime = self.runtime.clone();
        self.store()?.run(move |db| {
            expire(db)?;
            reconcile_generations(db, &sessions)?;
            let row: Option<InvitationRow> = db.query_row(
                "SELECT terminal,generation,secret,expires,mode,revoked,guest FROM invitations WHERE id=?1",
                [invitation_id.to_string()], |r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?))
            ).optional()?;
            let (terminal,generation,hash,expires,mode,revoked,redeemed) = row.ok_or(Error("invalid_invitation"))?;
            if !matches_secret(&hash,&invitation_secret) || revoked!=0 || expires<=now() {
                return Err(Error("invalid_invitation"));
            }
            if let Some(previous) = redeemed {
                if previous != grant_id.to_string() { return Err(Error("invitation_used")); }
                let guest = authenticate_db(db,grant_id,&grant_secret,&sessions)?;
                if guest.name != name { return Err(Error("name_locked")); }
                return Ok(guest);
            }
            let terminal_id = parse_id(&terminal)?;
            let session_id = parse_id(&generation)?;
            let snapshot = sessions.get(terminal_id).ok_or(Error("invalid_invitation"))?.snapshot();
            if snapshot.session_id != Some(session_id) { return Err(Error("generation_changed")); }
            let count: i64 = db.query_row(
                "SELECT COUNT(*) FROM grants WHERE revoked=0 AND expires>?1 AND terminal=?2",
                params![now(),terminal], |r|r.get(0))?;
            let total: i64 = db.query_row(
                "SELECT COUNT(*) FROM grants WHERE revoked=0 AND expires>?1", [now()], |r|r.get(0))?;
            let key = name.to_lowercase();
            // A new invitation can renew the same identity without releasing its
            // reserved name. Possession of both secrets and fresh admission are
            // required; an old revoked credential alone never restores access.
            let previous = previous_credential.as_deref().map(credential_parts).transpose()?;
            let existing = if let Some((id, ref secret)) = previous {
                let row: Option<(String, String, Vec<u8>)> = db.query_row(
                    "SELECT g.guest,p.name,g.secret FROM grants g JOIN guests p ON p.id=g.guest
                     WHERE g.id=?1 AND g.terminal=?2",
                    params![id.to_string(),terminal], |r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?;
                match row {
                    Some((guest, fixed_name, hash)) if matches_secret(&hash, secret) => {
                        if fixed_name != name { return Err(Error("name_locked")); }
                        Some(parse_id(&guest)?)
                    }
                    Some(_) => return Err(Error("invalid_credential")),
                    None => None,
                }
            } else { None };
            let guest_id = if let Some(id) = existing { id } else {
                let used: bool = db.query_row("SELECT EXISTS(SELECT 1 FROM guests WHERE terminal=?1 AND name_key=?2)",
                    params![terminal,key], |r|r.get(0))?;
                if used { return Err(Error("name_taken")); }
                Uuid::new_v4()
            };
            let rotating = if existing.is_some() { previous.map(|(id,_)| id) } else { None };
            let replacing: i64 = if let Some(old) = rotating {
                db.query_row("SELECT COUNT(*) FROM grants WHERE id=?1 AND revoked=0 AND expires>?2",
                    params![old.to_string(),now()], |r|r.get(0))?
            } else { 0 };
            if count-replacing>=GUESTS_PER_SESSION || total-replacing>=GUESTS_PER_SERVER { return Err(Error("sharing_capacity")); }
            let tx = db.transaction()?;
            if let Some(old) = rotating {
                tx.execute("UPDATE grants SET revoked=1 WHERE id=?1", [old.to_string()])?;
                tx.execute("UPDATE proposals SET state='invalidated',reason='access_renewed',updated=?2 WHERE grant_id=?1 AND state='pending'",
                    params![old.to_string(),now()])?;
            }
            tx.execute("INSERT OR IGNORE INTO guests(id,terminal,name,name_key) VALUES(?1,?2,?3,?4)",
                params![guest_id.to_string(),terminal,name,key])?;
            tx.execute("INSERT INTO grants(id,guest,terminal,generation,secret,mode,expires,created) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
                params![grant_id.to_string(),guest_id.to_string(),terminal,generation,digest(&grant_secret),mode,now()+GRANT_MS,now()])?;
            tx.execute("UPDATE invitations SET guest=?2 WHERE id=?1",params![invitation_id.to_string(),grant_id.to_string()])?;
            tx.commit()?;
            if let Some(old) = rotating { cancel_connections(&runtime, terminal_id, Some(old)); }
            authenticate_db(db,grant_id,&grant_secret,&sessions)
        }).await
    }
    pub async fn authenticate(&self, credential: String) -> Result<Guest> {
        let (id, secret) = credential_parts(&credential)?;
        let sessions = self.sessions.clone();
        self.store()?
            .run(move |db| authenticate_db(db, id, &secret, &sessions))
            .await
    }
    pub async fn guest_view(&self, guest: Guest) -> Result<Value> {
        let sessions = self.sessions.clone();
        self.store()?.run(move |db| {
            expire(db)?;
            let current = validate_grant(db,&guest,&sessions,false)?;
            let snapshot = sessions.get(current.terminal_id).ok_or(Error("access_ended"))?.snapshot();
            if snapshot.status != Lifecycle::Running {
                db.execute("UPDATE proposals SET state='invalidated',reason='generation_ended'
                    WHERE grant_id=?1 AND state='pending'", [current.grant_id.to_string()])?;
            }
            let proposals = if current.admitted { proposal_rows(db,current.terminal_id,Some(current.guest_id),Some(current.session_id))? } else { vec![] };
            Ok(json!({"guest":current,"status":snapshot.status,"agent":snapshot.agent.label(),
                "cols":snapshot.cols,"rows":snapshot.rows,"archiveIncomplete":snapshot.archive_incomplete,
                "proposals":proposals}))
        }).await
    }
    pub async fn admit(&self, terminal: Uuid, grant: Uuid) -> Result<()> {
        let sessions = self.sessions.clone();
        self.store()?.run(move |db| {
            let generation: Option<String> = db.query_row(
                "SELECT generation FROM grants WHERE id=?1 AND terminal=?2 AND revoked=0 AND expires>?3",
                params![grant.to_string(),terminal.to_string(),now()], |r|r.get(0)).optional()?;
            if generation.is_none() { return Err(Error("not_found")); }
            let current = sessions.get(terminal).ok_or(Error("not_found"))?.snapshot();
            if generation.as_deref() != current.session_id.map(|id|id.to_string()).as_deref() {
                return Err(Error("generation_changed"));
            }
            db.execute("UPDATE grants SET admitted=1 WHERE id=?1",[grant.to_string()])?;
            Ok(())
        }).await
    }
    pub async fn revoke(&self, terminal: Uuid, grant: Option<Uuid>) -> Result<()> {
        let runtime = self.runtime.clone();
        self.store()?
            .run(move |db| {
                let tx = db.transaction()?;
                tx.execute(
                    "UPDATE grants SET revoked=1 WHERE terminal=?1 AND (?2 IS NULL OR id=?2)",
                    params![terminal.to_string(), grant.map(|id| id.to_string())],
                )?;
                tx.execute(
                    "UPDATE proposals SET state='invalidated',reason='revoked',updated=?3
                WHERE terminal=?1 AND (?2 IS NULL OR grant_id=?2) AND state='pending'",
                    params![terminal.to_string(), grant.map(|id| id.to_string()), now()],
                )?;
                if grant.is_none() {
                    tx.execute(
                        "UPDATE invitations SET revoked=1 WHERE terminal=?1",
                        [terminal.to_string()],
                    )?;
                }
                tx.commit()?;
                cancel_connections(&runtime, terminal, grant);
                Ok(())
            })
            .await
    }
    pub async fn propose(&self, guest: Guest, request: Uuid, text: String) -> Result<Value> {
        validate_text(&text)?;
        let sessions = self.sessions.clone();
        self.store()?.run(move |db| {
            expire(db)?;
            let current = validate_grant(db,&guest,&sessions,true)?;
            if current.mode!="propose" { return Err(Error("view_only")); }
            let previous: Option<(String,Option<String>,String)> = db.query_row(
                "SELECT id,original,generation FROM proposals WHERE guest=?1 AND request=?2",
                params![guest.guest_id.to_string(),request.to_string()], |r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?;
            if let Some((id,original,generation))=previous {
                if original.as_deref()!=Some(&text) || generation != current.session_id.to_string() {
                    return Err(Error("request_conflict"));
                }
                return guest_proposal(db,&id);
            }
            let snapshot = sessions.get(guest.terminal_id).ok_or(Error("access_ended"))?.snapshot();
            if snapshot.status!=Lifecycle::Running { return Err(Error("not_running")); }
            let pending: bool=db.query_row("SELECT EXISTS(SELECT 1 FROM proposals WHERE guest=?1 AND state='pending')",
                [guest.guest_id.to_string()], |r|r.get(0))?;
            if pending { return Err(Error("proposal_pending")); }
            let recent:i64=db.query_row("SELECT COUNT(*) FROM proposals WHERE guest=?1 AND created>?2",
                params![guest.guest_id.to_string(),now()-60_000], |r|r.get(0))?;
            let count:i64=db.query_row("SELECT COUNT(*) FROM proposals",[],|r|r.get(0))?;
            if recent>=6 { return Err(Error("rate_limited")); }
            if count>=LEDGER_LIMIT { return Err(Error("sharing_capacity")); }
            let id=Uuid::new_v4().to_string();
            db.execute("INSERT INTO proposals(id,guest,grant_id,terminal,generation,request,original,current,created,updated)
                VALUES(?1,?2,?3,?4,?5,?6,?7,?7,?8,?8)",
                params![id,guest.guest_id.to_string(),guest.grant_id.to_string(),guest.terminal_id.to_string(),
                    guest.session_id.to_string(),request.to_string(),text,now()])?;
            guest_proposal(db,&id)
        }).await
    }
    pub async fn reject(&self, terminal: Uuid, id: Uuid, revision: i64) -> Result<Value> {
        self.store()?
            .run(move |db| {
                let changed = db.execute(
                    "UPDATE proposals SET state='rejected',updated=?4
                WHERE id=?1 AND terminal=?2 AND revision=?3 AND state='pending'",
                    params![id.to_string(), terminal.to_string(), revision, now()],
                )?;
                if changed == 0 {
                    return Err(Error("proposal_changed"));
                }
                proposal(db, &id.to_string())
            })
            .await
    }
    pub async fn approve(&self, terminal: Uuid, id: Uuid, approval: Approval) -> Result<Value> {
        let Approval {
            request_id: request,
            session_id: generation,
            revision,
            text,
            ready,
        } = approval;
        if !ready {
            return Err(Error("confirm_empty_prompt"));
        }
        validate_text(&text)?;
        if text.chars().any(char::is_control) {
            return Err(Error("single_line_only"));
        }
        let sessions = self.sessions.clone();
        let store = self.store()?;
        let receipts = store.clone();
        let handle = tokio::runtime::Handle::current();
        let runtime = self.runtime.clone();
        store.run(move |db| {
            expire(db)?;
            let prior:Option<(String,String,String,i64)> = db.query_row(
                "SELECT proposal,text,generation,revision FROM approval_attempts WHERE request=?1",
                [request.to_string()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional()?;
            if let Some((proposal_id,approved,old_generation,old_revision))=prior {
                let same_terminal: bool = db.query_row(
                    "SELECT EXISTS(SELECT 1 FROM proposals WHERE id=?1 AND terminal=?2)",
                    params![proposal_id,terminal.to_string()], |r| r.get(0))?;
                if !same_terminal || proposal_id!=id.to_string() || approved!=text || old_generation!=generation.to_string() || old_revision!=revision {
                    return Err(Error("request_conflict"));
                }
                return proposal(db,&id.to_string());
            }
            let row:Option<(String,String,String,i64,String)> = db.query_row(
                "SELECT grant_id,generation,state,revision,current FROM proposals WHERE id=?1 AND terminal=?2",
                params![id.to_string(),terminal.to_string()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).optional()?;
            let (grant,generation_id,state,current_revision,_) = row.ok_or(Error("not_found"))?;
            if state!="pending" || current_revision!=revision { return Err(Error("proposal_changed")); }
            if generation_id!=generation.to_string() { return Err(Error("generation_changed")); }
            let active:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM grants WHERE id=?1 AND revoked=0 AND admitted=1 AND expires>?2 AND mode='propose')",
                params![grant,now()],|r|r.get(0))?;
            if !active { return Err(Error("access_ended")); }
            let attempts:i64=db.query_row("SELECT COUNT(*) FROM approval_attempts",[],|r|r.get(0))?;
            if attempts>=LEDGER_LIMIT { return Err(Error("sharing_capacity")); }
            let receipt_permit = runtime.receipts.clone().try_acquire_owned()
                .map_err(|_|Error("storage_busy"))?;
            // FULL-synchronous commit precedes PTY admission. No locks on PTYs or
            // registry are held while SQLite performs I/O.
            let tx=db.transaction()?;
            tx.execute("INSERT INTO approval_attempts(request,proposal,text,generation,revision,outcome)
                VALUES(?1,?2,?3,?4,?5,'intent')",params![request.to_string(),id.to_string(),text,generation.to_string(),revision])?;
            tx.execute("UPDATE proposals SET state='approving',revision=revision+1,current=?2,approved_text=NULL,
                approval_request=?3,updated=?4 WHERE id=?1",params![id.to_string(),text,request.to_string(),now()])?;
            tx.commit()?;
            // All grants/decisions share this actor, so revocation cannot interleave
            // between this check and synchronous registry admission.
            let expires:i64=db.query_row("SELECT expires FROM grants WHERE id=?1",[grant],|r|r.get(0))?;
            let (sender,receiver)=tokio::sync::oneshot::channel();
            let admission=if expires<=now() { Err(Error("access_ended")) } else {
                sessions.enqueue_shared_prompt(terminal,generation,&text,sender)
            };
            if let Err(error)=admission {
                let tx = db.transaction()?;
                tx.execute("UPDATE approval_attempts SET outcome='not_queued' WHERE request=?1",[request.to_string()])?;
                tx.execute("UPDATE proposals SET state='pending',approved_text=NULL,reason=?2,updated=?3 WHERE id=?1",
                    params![id.to_string(),error.0,now()])?;
                tx.commit()?;
                return proposal(db,&id.to_string());
            }
            // Spawn the receipt owner before another fallible persistence operation.
            handle.spawn(async move {
                let _permit = receipt_permit;
                let outcome=match receiver.await {
                    Ok(WriteReceipt::Written)=>"written",
                    Ok(WriteReceipt::NotWritten)=>"not_written",
                    Ok(WriteReceipt::Uncertain)|Err(_)=>"uncertain",
                };
                loop {
                let result=receipts.run_critical(move |db| {
                    let tx=db.transaction()?;
                    tx.execute("UPDATE approval_attempts SET outcome=?2 WHERE request=?1",params![request.to_string(),outcome])?;
                    tx.execute("UPDATE proposals SET state=?2,approved_text=current,reason=NULL,updated=?3 WHERE id=?1 AND approval_request=?4",
                        params![id.to_string(),outcome,now(),request.to_string()])?;
                    tx.commit()?;
                    Ok(())
                }).await;
                if result.is_ok() { break; }
                receipts.receipt_failed();
                // Retry only persistence, never PTY admission or input bytes.
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                }
            });
            db.execute("UPDATE proposals SET state='queued',approved_text=current,reason=NULL,updated=?2 WHERE id=?1",params![id.to_string(),now()])?;
            proposal(db,&id.to_string())
        }).await
    }
    pub async fn archives(&self, guest: Option<Guest>) -> Result<Value> {
        let sessions = self.sessions.clone();
        self.store()?.run(move |db| {
            if let Some(guest)=guest {
                validate_grant(db,&guest,&sessions,true)?;
                let generations=sharing_store::generations(db,Some(guest.terminal_id.to_string()))?;
                let safe:Vec<_>=generations.into_iter().filter(|g|g.id==guest.session_id.to_string())
                    .map(|g|json!({"id":g.id,"agent":g.agent,"createdAt":g.created_at,"bytes":g.bytes,
                        "incomplete":g.incomplete,"ended":g.ended,"cols":g.cols,"rows":g.rows})).collect();
                Ok(json!({"generations":safe}))
            } else {
                Ok(json!({"generations":sharing_store::generations(db,None)?}))
            }
        }).await
    }
    pub async fn archive(
        &self,
        guest: Option<Guest>,
        generation: Uuid,
        after: i64,
    ) -> Result<Value> {
        if after < 0 {
            return Err(Error("invalid_cursor"));
        }
        let sessions = self.sessions.clone();
        self.store()?
            .run(move |db| {
                if let Some(guest) = guest {
                    validate_grant(db, &guest, &sessions, true)?;
                    if generation != guest.session_id {
                        return Err(Error("access_denied"));
                    }
                }
                sharing_store::archive_events(db, &generation.to_string(), after)
            })
            .await
    }
    pub async fn delete_archive(&self, generation: Uuid) -> Result<()> {
        let sessions = self.sessions.clone();
        self.store()?
            .run(move |db| {
                // A live recorder must not recreate an archive after deletion.
                if sessions.list().iter().any(|session| {
                    session.session_id == Some(generation)
                        && matches!(
                            session.status,
                            Lifecycle::Starting | Lifecycle::Running | Lifecycle::Terminating
                        )
                }) {
                    return Err(Error("recording_active"));
                }
                let ended: Option<bool> = db
                    .query_row(
                        "SELECT ended FROM generations WHERE id=?1",
                        [generation.to_string()],
                        |r| r.get(0),
                    )
                    .optional()?;
                if ended != Some(true) {
                    return Err(Error("recording_active"));
                }
                db.execute(
                    "DELETE FROM generations WHERE id=?1",
                    [generation.to_string()],
                )?;
                // SQLite reuses freed pages; the physical file need not shrink.
                Ok(())
            })
            .await
    }
    pub async fn archive_proposals(&self, generation: Uuid) -> Result<Value> {
        self.store()?
            .run(move |db| {
                let mut query = db.prepare(
                    "SELECT id FROM proposals WHERE generation=?1 ORDER BY created DESC LIMIT 128",
                )?;
                let ids = query
                    .query_map([generation.to_string()], |r| r.get::<_, String>(0))?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                let proposals = ids
                    .iter()
                    .map(|id| proposal(db, id))
                    .collect::<Result<Vec<_>>>()?;
                Ok(json!({"proposals": proposals}))
            })
            .await
    }
    pub async fn connect(&self, guest: Guest) -> Result<GuestConnection> {
        let sessions = self.sessions.clone();
        let sharing = self.clone();
        self.store()?
            .run(move |db| {
                let guest = validate_grant(db, &guest, &sessions, true)?;
                sharing.connect_validated(&guest)
            })
            .await
    }
    fn connect_validated(&self, guest: &Guest) -> Result<GuestConnection> {
        if !guest.admitted || guest.expires_at <= now() || self.runtime.closed.is_cancelled() {
            return Err(Error("access_denied"));
        }
        // A permit belongs to the connection, not the managed PTY.
        let mut connections = lock(&self.runtime.connections);
        if connections
            .values()
            .filter(|p| p.grant == guest.grant_id)
            .count()
            >= 2
        {
            return Err(Error("connection_limit"));
        }
        let permit = self
            .runtime
            .guests
            .clone()
            .try_acquire_owned()
            .map_err(|_| Error("connection_limit"))?;
        let id = Uuid::new_v4();
        let cancel = self.runtime.closed.child_token();
        connections.insert(
            id,
            Presence {
                grant: guest.grant_id,
                terminal: guest.terminal_id,
                visible: false,
                seen: Instant::now(),
                cancel: cancel.clone(),
            },
        );
        Ok(GuestConnection {
            id,
            runtime: self.runtime.clone(),
            cancel,
            _permit: permit,
        })
    }
}
fn credential_parts(credential: &str) -> Result<(Uuid, String)> {
    let (id, secret) = credential
        .split_once('.')
        .ok_or(Error("invalid_credential"))?;
    let id = Uuid::parse_str(id).map_err(|_| Error("invalid_credential"))?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(secret)
        .map_err(|_| Error("invalid_credential"))?;
    if bytes.len() != 32 {
        return Err(Error("invalid_credential"));
    }
    Ok((id, secret.to_owned()))
}
fn digest(secret: &str) -> Vec<u8> {
    Sha256::digest(secret.as_bytes()).to_vec()
}
fn matches_secret(hash: &[u8], secret: &str) -> bool {
    let candidate = digest(secret);
    hash.len() == candidate.len() && bool::from(hash.ct_eq(&candidate))
}
fn parse_id(id: &str) -> Result<Uuid> {
    Uuid::parse_str(id).map_err(|_| Error("storage_unavailable"))
}
fn validate_name(name: &str) -> Result<String> {
    let name = name.trim();
    if name.is_empty()
        || name.chars().count() > 32
        || !name
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, ' ' | '-' | '_'))
        || matches!(
            name.to_lowercase().as_str(),
            "owner" | "system" | "admin" | "собственик"
        )
    {
        return Err(Error("invalid_name"));
    }
    Ok(name.to_owned())
}
fn validate_text(text: &str) -> Result<()> {
    if text.trim().is_empty()
        || text.len() > TEXT_LIMIT
        || text
            .chars()
            .any(|c| c.is_control() && !matches!(c, '\n' | '\t'))
    {
        return Err(Error("invalid_proposal"));
    }
    Ok(())
}
fn authenticate_db(
    db: &Connection,
    id: Uuid,
    secret: &str,
    sessions: &SessionRegistry,
) -> Result<Guest> {
    let stored: Option<Vec<u8>> = db
        .query_row(
            "SELECT secret FROM grants WHERE id=?1",
            [id.to_string()],
            |r| r.get(0),
        )
        .optional()?;
    if !stored.is_some_and(|hash| matches_secret(&hash, secret)) {
        return Err(Error("invalid_credential"));
    }
    let guest = read_guest(db, id)?;
    validate_grant(db, &guest, sessions, false)
}
fn read_guest(db: &Connection, id: Uuid) -> Result<Guest> {
    let row: Option<(String, String, String, String, String, i64, bool)> = db
        .query_row(
            "SELECT g.guest,g.terminal,g.generation,p.name,g.mode,g.expires,g.admitted
         FROM grants g JOIN guests p ON p.id=g.guest WHERE g.id=?1 AND g.revoked=0",
            [id.to_string()],
            |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                ))
            },
        )
        .optional()?;
    let (guest, terminal, generation, name, mode, expires, admitted) =
        row.ok_or(Error("access_ended"))?;
    Ok(Guest {
        grant_id: id,
        guest_id: parse_id(&guest)?,
        terminal_id: parse_id(&terminal)?,
        session_id: parse_id(&generation)?,
        name,
        mode,
        expires_at: expires,
        admitted,
    })
}
fn validate_grant(
    db: &Connection,
    guest: &Guest,
    sessions: &SessionRegistry,
    admitted: bool,
) -> Result<Guest> {
    let current = read_guest(db, guest.grant_id)?;
    if current.expires_at <= now() {
        return Err(Error("access_ended"));
    }
    if admitted && !current.admitted {
        return Err(Error("awaiting_admission"));
    }
    let snapshot = sessions
        .get(current.terminal_id)
        .ok_or(Error("access_ended"))?
        .snapshot();
    if snapshot.session_id != Some(current.session_id) {
        return Err(Error("generation_changed"));
    }
    Ok(current)
}
fn expire(db: &Connection) -> Result<()> {
    db.execute(
        "UPDATE proposals SET state='expired',reason='expired',updated=?1
        WHERE state='pending' AND created<?2",
        params![now(), now() - PROPOSAL_MS],
    )?;
    Ok(())
}
fn guest_rows(db: &Connection, terminal: Uuid, runtime: &Runtime) -> Result<Vec<Value>> {
    let mut query = db.prepare(
        "SELECT g.id,p.id,p.name,g.admitted,g.revoked,g.expires,g.mode
        FROM grants g JOIN guests p ON p.id=g.guest WHERE g.terminal=?1
        ORDER BY (g.revoked=0 AND g.expires>?2) DESC,g.created DESC LIMIT 64",
    )?;
    let mut rows = query.query(params![terminal.to_string(), now()])?;
    let connections = lock(&runtime.connections).clone();
    let mut result = vec![];
    while let Some(row) = rows.next()? {
        let id: String = row.get(0)?;
        let active: Vec<_> = connections
            .values()
            .filter(|p| p.grant.to_string() == id)
            .collect();
        let presence = if active.is_empty() {
            "disconnected"
        } else if active.iter().all(|p| p.seen.elapsed().as_secs() > 45) {
            "stale"
        } else if active
            .iter()
            .any(|p| p.visible && p.seen.elapsed().as_secs() <= 45)
        {
            "visible"
        } else {
            "background"
        };
        result.push(json!({"grantId":id,"guestId":row.get::<_,String>(1)?,"name":row.get::<_,String>(2)?,
            "admitted":row.get::<_,bool>(3)?,"revoked":row.get::<_,bool>(4)?,"expiresAt":row.get::<_,i64>(5)?,
            "mode":row.get::<_,String>(6)?,"presence":presence}));
    }
    Ok(result)
}
fn proposal(db: &Connection, id: &str) -> Result<Value> {
    db.query_row("SELECT p.id,g.name,p.original,p.current,p.approved_text,p.revision,p.state,p.reason,p.created,p.updated,p.generation
        FROM proposals p JOIN guests g ON g.id=p.guest WHERE p.id=?1",[id], |r|Ok(json!({
            "id":r.get::<_,String>(0)?,"author":r.get::<_,String>(1)?,"original":r.get::<_,Option<String>>(2)?,
            "text":r.get::<_,Option<String>>(3)?,"approvedText":r.get::<_,Option<String>>(4)?,"revision":r.get::<_,i64>(5)?,
            "state":r.get::<_,String>(6)?,"reason":r.get::<_,Option<String>>(7)?,"createdAt":r.get::<_,i64>(8)?,
            "updatedAt":r.get::<_,i64>(9)?,"sessionId":r.get::<_,String>(10)?
        }))).optional()?.ok_or(Error("not_found"))
}
fn proposal_rows(
    db: &Connection,
    terminal: Uuid,
    guest: Option<Uuid>,
    generation: Option<Uuid>,
) -> Result<Vec<Value>> {
    let mut query = db.prepare(
        "SELECT id FROM proposals WHERE terminal=?1 AND (?2 IS NULL OR guest=?2) AND (?3 IS NULL OR generation=?3)
        ORDER BY (state='pending') DESC,created DESC LIMIT 128",
    )?;
    let ids = query
        .query_map(
            params![
                terminal.to_string(),
                guest.map(|g| g.to_string()),
                generation.map(|g| g.to_string())
            ],
            |r| r.get::<_, String>(0),
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    ids.iter()
        .map(|id| {
            if guest.is_some() {
                guest_proposal(db, id)
            } else {
                proposal(db, id)
            }
        })
        .collect()
}

fn guest_proposal(db: &Connection, id: &str) -> Result<Value> {
    let mut value = proposal(db, id)?;
    // The owner can retain an edited draft after failed queue admission.
    // Only accepted delivery text is shared; pending edits stay owner-only.
    if matches!(
        value["state"].as_str(),
        Some("approving" | "pending" | "rejected" | "expired" | "invalidated")
    ) {
        value["approvedText"] = Value::Null;
    }
    value["text"] = value["approvedText"]
        .as_str()
        .map(|s| json!(s))
        .unwrap_or_else(|| value["original"].clone());
    Ok(value)
}

fn cancel_connections(runtime: &Runtime, terminal: Uuid, grant: Option<Uuid>) {
    let cancellations: Vec<_> = lock(&runtime.connections)
        .values()
        .filter(|p| p.terminal == terminal && grant.is_none_or(|id| id == p.grant))
        .map(|p| p.cancel.clone())
        .collect();
    for cancel in cancellations {
        cancel.cancel();
    }
}

fn ledger_usage(db: &Connection) -> Result<Value> {
    let invitations: i64 = db.query_row("SELECT COUNT(*) FROM invitations", [], |r| r.get(0))?;
    let proposals: i64 = db.query_row("SELECT COUNT(*) FROM proposals", [], |r| r.get(0))?;
    let approvals: i64 =
        db.query_row("SELECT COUNT(*) FROM approval_attempts", [], |r| r.get(0))?;
    Ok(
        json!({"invitations":invitations,"proposals":proposals,"approvals":approvals,"limit":LEDGER_LIMIT}),
    )
}

fn reconcile_generations(db: &Connection, sessions: &SessionRegistry) -> Result<()> {
    let mut query = db.prepare("SELECT terminal,generation FROM grants WHERE revoked=0 AND expires>?1
        UNION SELECT terminal,generation FROM invitations WHERE revoked=0 AND guest IS NULL AND expires>?1")?;
    let participants = query
        .query_map([now()], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (terminal, generation) in participants {
        let current = sessions
            .get(parse_id(&terminal)?)
            .and_then(|session| session.snapshot().session_id);
        if current.map(|id| id.to_string()).as_deref() != Some(generation.as_str()) {
            db.execute(
                "UPDATE grants SET revoked=1 WHERE terminal=?1 AND generation=?2",
                params![terminal, generation],
            )?;
            db.execute(
                "UPDATE invitations SET revoked=1 WHERE terminal=?1 AND generation=?2",
                params![terminal, generation],
            )?;
            db.execute(
                "UPDATE proposals SET state='invalidated',reason='generation_ended'
                WHERE terminal=?1 AND generation=?2 AND state='pending'",
                params![terminal, generation],
            )?;
        }
    }
    Ok(())
}
