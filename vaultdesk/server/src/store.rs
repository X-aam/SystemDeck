//! Embedded storage (redb). Entries are encrypted at rest with the server key;
//! session tokens are stored only as SHA-256 hashes.

use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, Duration, Utc};
use redb::{Database, ReadableTable, TableDefinition};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;
use systemdeck_core::{
    api::{AuditEvent, PushResult, RecordingMeta},
    crypto::{self, Key},
    Entry, Principal, Team, UserInfo, Vault,
};

type Tbl = TableDefinition<'static, &'static str, &'static [u8]>;

const USERS: Tbl = TableDefinition::new("users");
const SESSIONS: Tbl = TableDefinition::new("sessions");
const TEAMS: Tbl = TableDefinition::new("teams");
const VAULTS: Tbl = TableDefinition::new("vaults");
/// Key: `{vault_id}/{entry_id}`, value: sealed JSON `Entry`.
const ENTRIES: Tbl = TableDefinition::new("entries");
/// `seq`: last revision number handed out (u64 little endian).
const META: Tbl = TableDefinition::new("meta");
/// Recording metadata; the session data lives in `recordings/<id>.rec`.
const RECORDINGS: Tbl = TableDefinition::new("recordings");
/// Audit log. Key: `{timestamp_millis:013}-{uuid}` so it sorts oldest-first.
const AUDIT: Tbl = TableDefinition::new("audit");

const SESSION_TTL_DAYS: i64 = 7;

#[derive(Serialize, Deserialize, Clone)]
pub struct StoredUser {
    pub username: String,
    pub display_name: String,
    pub password_hash: String,
    pub is_admin: bool,
    pub disabled: bool,
    pub created_at: DateTime<Utc>,
}

impl StoredUser {
    pub fn info(&self) -> UserInfo {
        UserInfo {
            username: self.username.clone(),
            display_name: self.display_name.clone(),
            is_admin: self.is_admin,
            disabled: self.disabled,
        }
    }
}

#[derive(Serialize, Deserialize)]
pub struct Session {
    pub username: String,
    pub expires_at: DateTime<Utc>,
}

pub fn token_hash(token: &str) -> String {
    hex::encode(Sha256::digest(token.as_bytes()))
}

fn entry_key(vault: Uuid, id: Uuid) -> String {
    format!("{vault}/{id}")
}

/// Key range covering every entry of a vault (`'0'` sorts right after `'/'`).
fn vault_range(vault: Uuid) -> (String, String) {
    (format!("{vault}/"), format!("{vault}0"))
}

fn read_seq(meta: &impl ReadableTable<&'static str, &'static [u8]>) -> Result<u64> {
    let seq = match meta.get("seq")? {
        Some(v) => u64::from_le_bytes(v.value().try_into().map_err(|_| anyhow!("corrupt sequence counter"))?),
        None => 0,
    };
    Ok(seq)
}

pub struct Db {
    db: Database,
    key: Key,
    rec_dir: PathBuf,
}

impl Db {
    pub fn open(path: &Path, key: Key) -> Result<Self> {
        let db = Database::create(path).with_context(|| format!("opening {}", path.display()))?;
        let tx = db.begin_write()?;
        for table in [USERS, SESSIONS, TEAMS, VAULTS, ENTRIES, META, RECORDINGS, AUDIT] {
            tx.open_table(table)?;
        }
        tx.commit()?;
        let rec_dir = path.parent().unwrap_or(Path::new(".")).join("recordings");
        fs::create_dir_all(&rec_dir).with_context(|| format!("creating {}", rec_dir.display()))?;
        Ok(Self { db, key, rec_dir })
    }

    // ---- generic JSON helpers ----

    fn get<T: DeserializeOwned>(&self, table: Tbl, key: &str) -> Result<Option<T>> {
        let tx = self.db.begin_read()?;
        let t = tx.open_table(table)?;
        let raw = t.get(key)?.map(|v| v.value().to_vec());
        Ok(raw.map(|b| serde_json::from_slice(&b)).transpose()?)
    }

    fn put<T: Serialize>(&self, table: Tbl, key: &str, value: &T) -> Result<()> {
        let bytes = serde_json::to_vec(value)?;
        let tx = self.db.begin_write()?;
        {
            let mut t = tx.open_table(table)?;
            t.insert(key, bytes.as_slice())?;
        }
        tx.commit()?;
        Ok(())
    }

    fn del(&self, table: Tbl, key: &str) -> Result<()> {
        let tx = self.db.begin_write()?;
        {
            let mut t = tx.open_table(table)?;
            t.remove(key)?;
        }
        tx.commit()?;
        Ok(())
    }

    fn list<T: DeserializeOwned>(&self, table: Tbl) -> Result<Vec<T>> {
        let tx = self.db.begin_read()?;
        let t = tx.open_table(table)?;
        let mut out = Vec::new();
        for item in t.iter()? {
            let (_, v) = item?;
            out.push(serde_json::from_slice(v.value())?);
        }
        Ok(out)
    }

    // ---- users & sessions ----

    pub fn user(&self, username: &str) -> Result<Option<StoredUser>> {
        self.get(USERS, &username.to_lowercase())
    }

    pub fn users(&self) -> Result<Vec<StoredUser>> {
        self.list(USERS)
    }

    pub fn put_user(&self, user: &StoredUser) -> Result<()> {
        self.put(USERS, &user.username, user)
    }

    /// Returns the bearer token; only its hash is persisted.
    pub fn create_session(&self, username: &str) -> Result<String> {
        let token = crypto::random_token();
        let session = Session {
            username: username.to_string(),
            expires_at: Utc::now() + Duration::days(SESSION_TTL_DAYS),
        };
        self.put(SESSIONS, &token_hash(&token), &session)?;
        Ok(token)
    }

    pub fn session(&self, token: &str) -> Result<Option<Session>> {
        let hash = token_hash(token);
        match self.get::<Session>(SESSIONS, &hash)? {
            Some(s) if s.expires_at > Utc::now() => Ok(Some(s)),
            Some(_) => {
                self.del(SESSIONS, &hash)?;
                Ok(None)
            }
            None => Ok(None),
        }
    }

    /// Number of unexpired sessions per user.
    pub fn active_sessions(&self) -> Result<std::collections::HashMap<String, usize>> {
        let now = Utc::now();
        let mut out = std::collections::HashMap::new();
        for s in self.list::<Session>(SESSIONS)? {
            if s.expires_at > now {
                *out.entry(s.username).or_insert(0) += 1;
            }
        }
        Ok(out)
    }

    pub fn delete_session(&self, hash: &str) -> Result<()> {
        self.del(SESSIONS, hash)
    }

    /// Signs a user out everywhere (password reset, account disabled) and
    /// drops expired sessions while at it.
    pub fn delete_sessions_for(&self, username: &str) -> Result<()> {
        let now = Utc::now();
        let tx = self.db.begin_write()?;
        {
            let mut t = tx.open_table(SESSIONS)?;
            let mut stale = Vec::new();
            for item in t.iter()? {
                let (k, v) = item?;
                let s: Session = serde_json::from_slice(v.value())?;
                if s.username == username || s.expires_at <= now {
                    stale.push(k.value().to_string());
                }
            }
            for k in stale {
                t.remove(k.as_str())?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    // ---- teams ----

    pub fn teams(&self) -> Result<Vec<Team>> {
        self.list(TEAMS)
    }

    pub fn team(&self, id: Uuid) -> Result<Option<Team>> {
        self.get(TEAMS, &id.to_string())
    }

    pub fn put_team(&self, team: &Team) -> Result<()> {
        self.put(TEAMS, &team.id.to_string(), team)
    }

    /// Deletes the team and strips its grants from every vault.
    pub fn delete_team(&self, id: Uuid) -> Result<()> {
        self.del(TEAMS, &id.to_string())?;
        for mut vault in self.vaults()? {
            let before = vault.grants.len();
            vault.grants.retain(|g| g.principal != Principal::Team(id));
            if vault.grants.len() != before {
                self.put_vault(&vault)?;
            }
        }
        Ok(())
    }

    // ---- vaults ----

    pub fn vaults(&self) -> Result<Vec<Vault>> {
        self.list(VAULTS)
    }

    pub fn vault(&self, id: Uuid) -> Result<Option<Vault>> {
        self.get(VAULTS, &id.to_string())
    }

    pub fn put_vault(&self, vault: &Vault) -> Result<()> {
        self.put(VAULTS, &vault.id.to_string(), vault)
    }

    pub fn delete_vault(&self, id: Uuid) -> Result<()> {
        let (lo, hi) = vault_range(id);
        let tx = self.db.begin_write()?;
        {
            let mut entries = tx.open_table(ENTRIES)?;
            let mut keys = Vec::new();
            for item in entries.range(lo.as_str()..hi.as_str())? {
                let (k, _) = item?;
                keys.push(k.value().to_string());
            }
            for k in keys {
                entries.remove(k.as_str())?;
            }
            let mut vaults = tx.open_table(VAULTS)?;
            vaults.remove(id.to_string().as_str())?;
        }
        tx.commit()?;
        Ok(())
    }

    // ---- session recordings ----

    pub fn recording(&self, id: Uuid) -> Result<Option<RecordingMeta>> {
        self.get(RECORDINGS, &id.to_string())
    }

    pub fn put_recording(&self, meta: &RecordingMeta) -> Result<()> {
        self.put(RECORDINGS, &meta.id.to_string(), meta)
    }

    /// Newest first.
    pub fn recordings(&self) -> Result<Vec<RecordingMeta>> {
        let mut all: Vec<RecordingMeta> = self.list(RECORDINGS)?;
        all.sort_by(|a, b| b.started_at.cmp(&a.started_at));
        Ok(all)
    }

    fn recording_path(&self, id: Uuid) -> PathBuf {
        self.rec_dir.join(format!("{id}.rec"))
    }

    /// Appends one encrypted block: `[u32 LE length][sealed bytes]`.
    pub fn append_recording(&self, id: Uuid, data: &[u8]) -> Result<()> {
        let sealed = crypto::seal(&self.key, data);
        let mut file = OpenOptions::new().create(true).append(true).open(self.recording_path(id))?;
        file.write_all(&(sealed.len() as u32).to_le_bytes())?;
        file.write_all(&sealed)?;
        Ok(())
    }

    /// Decrypts and concatenates all blocks (the asciicast text).
    pub fn read_recording(&self, id: Uuid) -> Result<Vec<u8>> {
        let raw = match fs::read(self.recording_path(id)) {
            Ok(raw) => raw,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e.into()),
        };
        let mut out = Vec::new();
        let mut pos = 0;
        while pos + 4 <= raw.len() {
            let len = u32::from_le_bytes(raw[pos..pos + 4].try_into()?) as usize;
            pos += 4;
            let block = raw.get(pos..pos + len).ok_or_else(|| anyhow!("recording {id} is truncated"))?;
            out.extend(crypto::open(&self.key, block).context("decrypting recording")?);
            pos += len;
        }
        Ok(out)
    }

    pub fn delete_recording(&self, id: Uuid) -> Result<()> {
        self.del(RECORDINGS, &id.to_string())?;
        let _ = fs::remove_file(self.recording_path(id));
        Ok(())
    }

    /// Deletes finished recordings older than `days` (retention). Returns how
    /// many were removed. `days == 0` disables pruning.
    pub fn prune_recordings(&self, days: i64) -> Result<usize> {
        if days <= 0 {
            return Ok(0);
        }
        let cutoff = Utc::now() - Duration::days(days);
        let old: Vec<Uuid> = self
            .recordings()?
            .into_iter()
            .filter(|r| r.finished && r.started_at < cutoff)
            .map(|r| r.id)
            .collect();
        for id in &old {
            self.delete_recording(*id)?;
        }
        Ok(old.len())
    }

    // ---- audit log ----

    /// Records one action. Best effort: auditing never fails the request.
    pub fn audit(&self, username: &str, action: &str, target: &str, detail: &str) {
        let at = Utc::now();
        let key = format!("{:013}-{}", at.timestamp_millis(), Uuid::new_v4().simple());
        let event = AuditEvent {
            at,
            username: username.to_string(),
            action: action.to_string(),
            target: target.to_string(),
            detail: detail.to_string(),
        };
        if let Err(e) = self.put(AUDIT, &key, &event) {
            tracing::warn!("audit write failed: {e}");
        }
    }

    /// Most recent audit events first, optionally filtered by user and action.
    pub fn audit_log(&self, limit: usize, user: Option<&str>, action: Option<&str>) -> Result<Vec<AuditEvent>> {
        let tx = self.db.begin_read()?;
        let t = tx.open_table(AUDIT)?;
        let mut out = Vec::new();
        for item in t.iter()?.rev() {
            let (_, v) = item?;
            let event: AuditEvent = serde_json::from_slice(v.value())?;
            if user.is_some_and(|u| !event.username.eq_ignore_ascii_case(u)) {
                continue;
            }
            if action.is_some_and(|a| event.action != a) {
                continue;
            }
            out.push(event);
            if out.len() >= limit {
                break;
            }
        }
        Ok(out)
    }

    /// Deletes audit events older than `days` (0 = keep forever).
    pub fn prune_audit(&self, days: i64) -> Result<usize> {
        if days <= 0 {
            return Ok(0);
        }
        let cutoff = (Utc::now() - Duration::days(days)).timestamp_millis();
        let tx = self.db.begin_write()?;
        let mut removed = 0;
        {
            let mut t = tx.open_table(AUDIT)?;
            let old: Vec<String> = t
                .iter()?
                .filter_map(|r| r.ok())
                .map(|(k, _)| k.value().to_string())
                .take_while(|k| k.split('-').next().and_then(|m| m.parse::<i64>().ok()).is_some_and(|m| m < cutoff))
                .collect();
            for k in old {
                t.remove(k.as_str())?;
                removed += 1;
            }
        }
        tx.commit()?;
        Ok(removed)
    }

    // ---- entries ----

    fn decode_entry(&self, sealed: &[u8]) -> Result<Entry> {
        let plain = crypto::open(&self.key, sealed).context("decrypting entry (wrong server.key?)")?;
        Ok(serde_json::from_slice(&plain)?)
    }

    /// Entries (including tombstones) changed after `since`, oldest first,
    /// and the current global sequence number to use as the next cursor.
    pub fn entries_since(&self, vault: Uuid, since: u64) -> Result<(Vec<Entry>, u64)> {
        let (lo, hi) = vault_range(vault);
        let tx = self.db.begin_read()?;
        let entries = tx.open_table(ENTRIES)?;
        let meta = tx.open_table(META)?;
        let cursor = read_seq(&meta)?;
        let mut out = Vec::new();
        for item in entries.range(lo.as_str()..hi.as_str())? {
            let (_, v) = item?;
            let entry = self.decode_entry(v.value())?;
            if entry.revision > since {
                out.push(entry);
            }
        }
        out.sort_by_key(|e| e.revision);
        Ok((out, cursor))
    }

    /// Writes one client change with optimistic concurrency: it is accepted
    /// only if the stored revision still equals the client's base revision.
    /// `keep_recording`: a recording switch that is on stays on (only
    /// administrators may turn an entry's recording off).
    pub fn apply_change(&self, vault: Uuid, mut entry: Entry, base_revision: u64, username: &str, keep_recording: bool) -> Result<PushResult> {
        let key = entry_key(vault, entry.id);
        let tx = self.db.begin_write()?;
        let result = {
            let mut entries = tx.open_table(ENTRIES)?;
            let mut meta = tx.open_table(META)?;
            let existing_raw = entries.get(key.as_str())?.map(|v| v.value().to_vec());
            let existing = existing_raw.map(|b| self.decode_entry(&b)).transpose()?;
            match existing {
                Some(current) if current.revision != base_revision => PushResult::Conflict { server: current },
                current => {
                    if keep_recording && current.is_some_and(|c| c.record_session) {
                        entry.record_session = true;
                    }
                    let seq = read_seq(&meta)? + 1;
                    meta.insert("seq", seq.to_le_bytes().as_slice())?;
                    entry.vault_id = vault;
                    entry.revision = seq;
                    entry.updated_at = Utc::now();
                    entry.updated_by = username.to_string();
                    let sealed = crypto::seal(&self.key, &serde_json::to_vec(&entry)?);
                    entries.insert(key.as_str(), sealed.as_slice())?;
                    PushResult::Accepted { entry }
                }
            }
        };
        if matches!(result, PushResult::Accepted { .. }) {
            tx.commit()?;
        } else {
            tx.abort()?;
        }
        Ok(result)
    }
}
