//! Encrypted local store (redb). Holds the private local vault, an offline
//! cache of every server vault the user can access, and unsynced edits.

use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{anyhow, Context, Result};
use redb::{Database, ReadableTable, TableDefinition};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use uuid::Uuid;
use systemdeck_core::{
    crypto::{self, Key, KEY_LEN},
    Entry, Role,
};

type Tbl = TableDefinition<'static, &'static str, &'static [u8]>;

const SETTINGS: Tbl = TableDefinition::new("settings");
const VAULTS: Tbl = TableDefinition::new("vaults");
/// Key: `{vault_id}/{entry_id}`.
const ENTRIES: Tbl = TableDefinition::new("entries");

/// The private vault that only exists on this computer and is never synced.
pub const LOCAL_VAULT: Uuid = Uuid::nil();

const KEYRING_SERVICE: &str = "SystemDeck";
const KEYRING_USER: &str = "local-store-key";

fn default_true() -> bool {
    true
}

#[derive(Serialize, Deserialize, Clone, Default)]
#[serde(default)]
pub struct Settings {
    pub server_url: String,
    pub username: String,
    pub display_name: String,
    pub token: Option<String>,
    /// Keep the sign-in saved across restarts. Defaults on, so an existing
    /// saved token keeps working after upgrade.
    #[serde(default = "default_true")]
    pub remember_me: bool,
    pub is_admin: bool,
    /// Open RDP/VNC in an external program instead of a tab.
    pub external_by_default: bool,
    pub dark_mode: bool,
    /// Record every SSH/RDP/VNC session of the Local vault (saved on this PC).
    pub record_local: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct CachedVault {
    pub id: Uuid,
    pub name: String,
    pub description: String,
    pub role: Role,
    /// Server sequence number this cache is up to date with.
    pub cursor: u64,
    /// Vault policy: SSH sessions must be recorded to the server.
    #[serde(default)]
    pub record_ssh: bool,
}

impl CachedVault {
    pub fn is_local(&self) -> bool {
        self.id == LOCAL_VAULT
    }

    pub fn writable(&self) -> bool {
        self.role >= Role::Write
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct LocalEntry {
    pub entry: Entry,
    /// Changed locally and not yet accepted by the server.
    pub dirty: bool,
    /// Server revision the local copy was based on (0 = never pushed).
    pub base_revision: u64,
    /// Newer server version that clashes with an unpushed local edit.
    pub conflict: Option<Entry>,
}

impl LocalEntry {
    pub fn clean(entry: Entry) -> Self {
        Self { base_revision: entry.revision, entry, dirty: false, conflict: None }
    }
}

pub struct Store {
    db: Database,
    key: Key,
}

impl Store {
    /// `%APPDATA%\SystemDeck`, or `SYSTEMDECK_HOME` if set (handy for running
    /// several clients on one machine).
    pub fn open_default() -> Result<Self> {
        Self::open(&home_dir()?)
    }

    pub fn open(dir: &Path) -> Result<Self> {
        fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        let key = load_key(dir)?;
        let path = dir.join("local.redb");
        let db = Database::create(&path).with_context(|| format!("opening {}", path.display()))?;
        let tx = db.begin_write()?;
        for table in [SETTINGS, VAULTS, ENTRIES] {
            tx.open_table(table)?;
        }
        tx.commit()?;
        let store = Self { db, key };
        if store.vault(LOCAL_VAULT)?.is_none() {
            store.put_vault(&CachedVault {
                id: LOCAL_VAULT,
                name: "Local (this PC)".into(),
                description: "Private entries stored only on this computer".into(),
                role: Role::Manage,
                cursor: 0,
                record_ssh: false,
            })?;
        }
        Ok(store)
    }

    // ---- sealed JSON helpers ----

    fn decode<T: DeserializeOwned>(&self, sealed: &[u8]) -> Result<T> {
        let plain = crypto::open(&self.key, sealed).context("decrypting local data (local key changed?)")?;
        Ok(serde_json::from_slice(&plain)?)
    }

    fn get<T: DeserializeOwned>(&self, table: Tbl, key: &str) -> Result<Option<T>> {
        let tx = self.db.begin_read()?;
        let t = tx.open_table(table)?;
        let raw = t.get(key)?.map(|v| v.value().to_vec());
        raw.map(|b| self.decode(&b)).transpose()
    }

    fn put<T: Serialize>(&self, table: Tbl, key: &str, value: &T) -> Result<()> {
        let sealed = crypto::seal(&self.key, &serde_json::to_vec(value)?);
        let tx = self.db.begin_write()?;
        {
            let mut t = tx.open_table(table)?;
            t.insert(key, sealed.as_slice())?;
        }
        tx.commit()?;
        Ok(())
    }

    fn remove_range(&self, table: Tbl, lo: &str, hi: &str) -> Result<()> {
        let tx = self.db.begin_write()?;
        {
            let mut t = tx.open_table(table)?;
            let mut keys = Vec::new();
            for item in t.range(lo..hi)? {
                let (k, _) = item?;
                keys.push(k.value().to_string());
            }
            for k in keys {
                t.remove(k.as_str())?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    // ---- settings ----

    pub fn settings(&self) -> Result<Settings> {
        Ok(self.get(SETTINGS, "settings")?.unwrap_or_default())
    }

    pub fn save_settings(&self, settings: &Settings) -> Result<()> {
        self.put(SETTINGS, "settings", settings)
    }

    // ---- SSH known hosts (trust on first use) ----

    /// SHA-256 fingerprint previously accepted for `host:port`.
    pub fn known_host(&self, host_port: &str) -> Result<Option<String>> {
        let hosts: std::collections::HashMap<String, String> = self.get(SETTINGS, "known_hosts")?.unwrap_or_default();
        Ok(hosts.get(&host_port.to_lowercase()).cloned())
    }

    pub fn set_known_host(&self, host_port: &str, fingerprint: &str) -> Result<()> {
        let mut hosts: std::collections::HashMap<String, String> = self.get(SETTINGS, "known_hosts")?.unwrap_or_default();
        hosts.insert(host_port.to_lowercase(), fingerprint.to_string());
        self.put(SETTINGS, "known_hosts", &hosts)
    }

    // ---- vaults ----

    /// Local vault first, then server vaults by name.
    pub fn vaults(&self) -> Result<Vec<CachedVault>> {
        let tx = self.db.begin_read()?;
        let t = tx.open_table(VAULTS)?;
        let mut out: Vec<CachedVault> = Vec::new();
        for item in t.iter()? {
            let (_, v) = item?;
            out.push(self.decode(v.value())?);
        }
        out.sort_by_key(|v| (!v.is_local(), v.name.to_lowercase()));
        Ok(out)
    }

    pub fn vault(&self, id: Uuid) -> Result<Option<CachedVault>> {
        self.get(VAULTS, &id.to_string())
    }

    pub fn put_vault(&self, vault: &CachedVault) -> Result<()> {
        self.put(VAULTS, &vault.id.to_string(), vault)
    }

    /// Drops a cached vault and all of its entries.
    pub fn remove_vault(&self, id: Uuid) -> Result<()> {
        self.remove_range(ENTRIES, &format!("{id}/"), &format!("{id}0"))?;
        let tx = self.db.begin_write()?;
        {
            let mut t = tx.open_table(VAULTS)?;
            t.remove(id.to_string().as_str())?;
        }
        tx.commit()?;
        Ok(())
    }

    // ---- entries ----

    pub fn entries(&self, vault: Uuid) -> Result<Vec<LocalEntry>> {
        let (lo, hi) = (format!("{vault}/"), format!("{vault}0"));
        let tx = self.db.begin_read()?;
        let t = tx.open_table(ENTRIES)?;
        let mut out = Vec::new();
        for item in t.range(lo.as_str()..hi.as_str())? {
            let (_, v) = item?;
            out.push(self.decode(v.value())?);
        }
        Ok(out)
    }

    pub fn dirty_entries(&self, vault: Uuid) -> Result<Vec<LocalEntry>> {
        Ok(self.entries(vault)?.into_iter().filter(|e| e.dirty).collect())
    }

    pub fn entry(&self, vault: Uuid, id: Uuid) -> Result<Option<LocalEntry>> {
        self.get(ENTRIES, &format!("{vault}/{id}"))
    }

    pub fn put_entry(&self, entry: &LocalEntry) -> Result<()> {
        self.put(ENTRIES, &format!("{}/{}", entry.entry.vault_id, entry.entry.id), entry)
    }

    pub fn remove_entry(&self, vault: Uuid, id: Uuid) -> Result<()> {
        let tx = self.db.begin_write()?;
        {
            let mut t = tx.open_table(ENTRIES)?;
            t.remove(format!("{vault}/{id}").as_str())?;
        }
        tx.commit()?;
        Ok(())
    }
}

/// `%APPDATA%\\SystemDeck`, or `SYSTEMDECK_HOME` if set.
pub fn home_dir() -> Result<PathBuf> {
    Ok(match std::env::var_os("SYSTEMDECK_HOME") {
        Some(dir) => PathBuf::from(dir),
        None => {
            let base = dirs::data_dir().ok_or_else(|| anyhow!("no application data directory"))?;
            let dir = base.join("SystemDeck");
            // The app used to be called VaultDesk: move its data over once.
            let old = base.join(LEGACY_NAME);
            if !dir.exists() && old.is_dir() {
                let _ = fs::rename(&old, &dir);
            }
            dir
        }
    })
}

/// Former product name (data folder and credential-store service).
const LEGACY_NAME: &str = "VaultDesk";

/// Where SSH recordings of Local-vault entries (and upload fallbacks) are saved.
pub fn recordings_dir() -> PathBuf {
    home_dir().unwrap_or_else(|_| std::env::temp_dir()).join("recordings")
}

/// The local database key lives in the Windows Credential Manager (DPAPI
/// protected, tied to the Windows account). If the credential store is not
/// available, it falls back to a `local.key` file next to the database.
fn load_key(dir: &Path) -> Result<Key> {
    let key_file = dir.join("local.key");
    if key_file.exists() {
        let bytes = fs::read(&key_file)?;
        return bytes.try_into().map_err(|_| anyhow!("{} is corrupt", key_file.display()));
    }

    if let Ok(cred) = keyring::Entry::new(KEYRING_SERVICE, KEYRING_USER) {
        match cred.get_password() {
            Ok(stored) => {
                let bytes = hex::decode(stored.trim()).context("decoding local key from credential store")?;
                return bytes.try_into().map_err(|_| anyhow!("local key in credential store has wrong length"));
            }
            Err(keyring::Error::NoEntry) => {
                // Key saved under the old product name: copy it to the new one.
                let legacy = keyring::Entry::new(LEGACY_NAME, KEYRING_USER).and_then(|c| c.get_password());
                if let Ok(stored) = legacy {
                    let bytes = hex::decode(stored.trim()).context("decoding local key from credential store")?;
                    let key: Key = bytes.try_into().map_err(|_| anyhow!("local key in credential store has wrong length"))?;
                    let _ = cred.set_password(&hex::encode(key));
                    return Ok(key);
                }
                let key = crypto::random_key();
                if cred.set_password(&hex::encode(key)).is_ok() {
                    return Ok(key);
                }
            }
            Err(_) => {}
        }
    }

    let key = crypto::random_key();
    fs::write(&key_file, key)?;
    debug_assert_eq!(key.len(), KEY_LEN);
    Ok(key)
}
