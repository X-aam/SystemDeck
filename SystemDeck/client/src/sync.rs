//! Two-way sync between the local store and the server.
//!
//! For each server vault: push local edits first (with the revision they
//! were based on, so the server can detect conflicts), then pull everything
//! that changed since the last cursor.

use std::collections::HashSet;

use uuid::Uuid;
use systemdeck_core::api::{PushChange, PushRequest, PushResult};

use crate::{
    api::{Api, ApiResult},
    store::{CachedVault, LocalEntry, Store, LOCAL_VAULT},
};

#[derive(Default, Debug)]
pub struct SyncReport {
    pub vaults: usize,
    pub pushed: usize,
    pub received: usize,
    pub conflicts: usize,
    /// Unpushed edits moved to the local vault because access was revoked.
    pub rescued: usize,
    pub rejected: Vec<String>,
}

impl SyncReport {
    pub fn summary(&self) -> String {
        let mut s = format!(
            "Synced {} vault(s): {} pushed, {} received",
            self.vaults, self.pushed, self.received
        );
        if self.conflicts > 0 {
            s += &format!(", {} conflict(s) need attention", self.conflicts);
        }
        if self.rescued > 0 {
            s += &format!(", {} unsynced entr(ies) moved to Local after losing vault access", self.rescued);
        }
        if !self.rejected.is_empty() {
            s += &format!(", rejected: {}", self.rejected.join("; "));
        }
        s
    }
}

pub fn sync(store: &Store, api: &Api) -> ApiResult<SyncReport> {
    let mut report = SyncReport::default();
    let remote = api.vaults()?;
    let remote_ids: HashSet<Uuid> = remote.iter().map(|v| v.id).collect();

    // Vaults that disappeared (deleted, or access revoked).
    for cached in store.vaults()? {
        if cached.is_local() || remote_ids.contains(&cached.id) {
            continue;
        }
        for le in store.entries(cached.id)? {
            if le.dirty && !le.entry.deleted {
                let mut entry = le.entry;
                entry.id = Uuid::new_v4();
                entry.vault_id = LOCAL_VAULT;
                entry.revision = 0;
                entry.name = format!("{} (from {})", entry.name, cached.name);
                store.put_entry(&LocalEntry::clean(entry))?;
                report.rescued += 1;
            }
        }
        store.remove_vault(cached.id)?;
    }

    for summary in remote {
        report.vaults += 1;
        let mut vault = match store.vault(summary.id)? {
            Some(mut v) => {
                v.name = summary.name;
                v.description = summary.description;
                v.role = summary.role;
                v.record_ssh = summary.record_ssh;
                v
            }
            None => CachedVault {
                id: summary.id,
                name: summary.name,
                description: summary.description,
                role: summary.role,
                cursor: 0,
                record_ssh: summary.record_ssh,
            },
        };
        store.put_vault(&vault)?;

        // Push. Entries already flagged as conflicting wait for the user.
        let dirty: Vec<LocalEntry> = store.dirty_entries(vault.id)?.into_iter().filter(|e| e.conflict.is_none()).collect();
        if !dirty.is_empty() && vault.writable() {
            let req = PushRequest {
                changes: dirty
                    .iter()
                    .map(|le| PushChange { entry: le.entry.clone(), base_revision: le.base_revision })
                    .collect(),
            };
            for result in api.push(vault.id, &req)?.results {
                match result {
                    PushResult::Accepted { entry } => {
                        report.pushed += 1;
                        if entry.deleted {
                            store.remove_entry(vault.id, entry.id)?;
                        } else {
                            store.put_entry(&LocalEntry::clean(entry))?;
                        }
                    }
                    PushResult::Conflict { server } => {
                        report.conflicts += 1;
                        if let Some(mut le) = store.entry(vault.id, server.id)? {
                            le.conflict = Some(server);
                            store.put_entry(&le)?;
                        }
                    }
                    PushResult::Rejected { id, reason } => report.rejected.push(format!("{id}: {reason}")),
                }
            }
        }

        // Pull.
        let changes = api.changes(vault.id, vault.cursor)?;
        for entry in changes.entries {
            match store.entry(vault.id, entry.id)? {
                Some(mut le) if le.dirty => {
                    if entry.revision != le.base_revision {
                        // Already counted if the push above just hit it.
                        if le.conflict.as_ref().map(|c| c.revision) != Some(entry.revision) {
                            report.conflicts += 1;
                        }
                        le.conflict = Some(entry);
                        store.put_entry(&le)?;
                    }
                }
                existing => {
                    let unchanged = existing.as_ref().is_some_and(|le| le.entry.revision == entry.revision);
                    if !unchanged {
                        report.received += 1;
                    }
                    if entry.deleted {
                        store.remove_entry(vault.id, entry.id)?;
                    } else {
                        store.put_entry(&LocalEntry::clean(entry))?;
                    }
                }
            }
        }
        vault.cursor = changes.cursor;
        store.put_vault(&vault)?;
    }
    Ok(report)
}

/// End-to-end test against a running server. Start one with a fresh data
/// directory, then run:
/// `SYSTEMDECK_TEST_URL=http://127.0.0.1:8741 SYSTEMDECK_TEST_ADMIN_PW=... cargo test -p systemdeck-client -- --ignored`
#[cfg(test)]
mod e2e {
    use super::*;
    use crate::store::Store;
    use systemdeck_core::{
        api::{CreateUserRequest, TeamRequest, VaultRequest},
        Entry, EntryKind, Grant, Principal, Role,
    };

    fn client_store(name: &str) -> Store {
        let dir = std::env::temp_dir().join(format!("systemdeck-e2e-{name}-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        // A key file keeps the test away from the real Windows credential store.
        std::fs::write(dir.join("local.key"), systemdeck_core::crypto::random_key()).unwrap();
        Store::open(&dir).unwrap()
    }

    fn login(url: &str, user: &str, pw: &str) -> Api {
        let token = Api::new(url, None).login(user, pw).expect("login").token;
        Api::new(url, Some(token))
    }

    #[test]
    #[ignore = "needs a running server"]
    fn share_sync_conflict_and_revoke() {
        let url = std::env::var("SYSTEMDECK_TEST_URL").expect("SYSTEMDECK_TEST_URL");
        let admin_pw = std::env::var("SYSTEMDECK_TEST_ADMIN_PW").expect("SYSTEMDECK_TEST_ADMIN_PW");
        let admin = login(&url, "admin", &admin_pw);

        let suffix = &Uuid::new_v4().to_string()[..8];
        let (alice, bob) = (format!("alice{suffix}"), format!("bob{suffix}"));
        for u in [&alice, &bob] {
            admin
                .admin_create_user(&CreateUserRequest { username: u.clone(), password: "correct-horse".into(), ..Default::default() })
                .unwrap();
        }
        let team = admin.admin_create_team(&TeamRequest { name: format!("Ops {suffix}"), members: vec![alice.clone(), bob.clone()] }).unwrap();
        let grants = vec![Grant { principal: Principal::Team(team.id), role: Role::Write }];
        let vault = admin
            .admin_create_vault(&VaultRequest { name: format!("Infra {suffix}"), description: String::new(), grants: grants.clone(), record_ssh: None })
            .unwrap();

        // Alice creates an entry offline in her local vault, then shares it.
        let (a_store, a_api) = (client_store("alice"), login(&url, &alice, "correct-horse"));
        let local = Entry {
            vault_id: LOCAL_VAULT,
            kind: EntryKind::Rdp,
            name: "DC01".into(),
            host: "dc01.corp.local".into(),
            username: "administrator".into(),
            password: "S3cret!".into(),
            ..Entry::default()
        };
        a_store.put_entry(&LocalEntry::clean(local.clone())).unwrap();
        let shared = Entry { id: Uuid::new_v4(), vault_id: vault.id, ..local };
        a_store.put_entry(&LocalEntry { entry: shared.clone(), dirty: true, base_revision: 0, conflict: None }).unwrap();
        let r = sync(&a_store, &a_api).unwrap();
        assert_eq!(r.pushed, 1, "{r:?}");
        assert_eq!(a_store.entries(LOCAL_VAULT).unwrap().len(), 1, "local entry stays private");

        // Bob receives it.
        let (b_store, b_api) = (client_store("bob"), login(&url, &bob, "correct-horse"));
        let r = sync(&b_store, &b_api).unwrap();
        assert_eq!(r.received, 1, "{r:?}");
        let b_entry = b_store.entry(vault.id, shared.id).unwrap().expect("bob has the shared entry");
        assert_eq!(b_entry.entry.password, "S3cret!");
        assert_eq!(b_entry.entry.updated_by, alice);
        assert!(b_store.entries(LOCAL_VAULT).unwrap().is_empty(), "alice's local vault never leaves her PC");

        // Bob edits and syncs; Alice edits the old version -> conflict.
        let mut b_edit = b_entry.clone();
        b_edit.entry.host = "dc01-new.corp.local".into();
        b_edit.dirty = true;
        b_store.put_entry(&b_edit).unwrap();
        assert_eq!(sync(&b_store, &b_api).unwrap().pushed, 1);

        let mut a_edit = a_store.entry(vault.id, shared.id).unwrap().unwrap();
        a_edit.entry.notes = "alice's note".into();
        a_edit.dirty = true;
        a_store.put_entry(&a_edit).unwrap();
        let r = sync(&a_store, &a_api).unwrap();
        assert_eq!(r.conflicts, 1, "{r:?}");
        let conflicted = a_store.entry(vault.id, shared.id).unwrap().unwrap();
        assert_eq!(conflicted.conflict.as_ref().unwrap().host, "dc01-new.corp.local");

        // "Keep mine": rebase onto the server revision and push again.
        let mut resolved = conflicted;
        resolved.base_revision = resolved.conflict.take().unwrap().revision;
        a_store.put_entry(&resolved).unwrap();
        assert_eq!(sync(&a_store, &a_api).unwrap().pushed, 1);
        sync(&b_store, &b_api).unwrap();
        assert_eq!(b_store.entry(vault.id, shared.id).unwrap().unwrap().entry.notes, "alice's note");

        // Deleting propagates.
        let mut tomb = b_store.entry(vault.id, shared.id).unwrap().unwrap();
        tomb.entry.deleted = true;
        tomb.dirty = true;
        b_store.put_entry(&tomb).unwrap();
        sync(&b_store, &b_api).unwrap();
        sync(&a_store, &a_api).unwrap();
        assert!(a_store.entry(vault.id, shared.id).unwrap().is_none());

        // Revoking access removes the cache but rescues unsynced work.
        let pending = Entry { vault_id: vault.id, name: "Unsynced".into(), ..Entry::default() };
        a_store.put_entry(&LocalEntry { entry: pending, dirty: true, base_revision: 0, conflict: None }).unwrap();
        admin.admin_update_vault(vault.id, &VaultRequest { name: vault.name.clone(), description: String::new(), grants: vec![], record_ssh: None }).unwrap();
        let r = sync(&a_store, &a_api).unwrap();
        assert_eq!(r.rescued, 1, "{r:?}");
        assert!(a_store.vault(vault.id).unwrap().is_none());
        assert!(a_store.entries(LOCAL_VAULT).unwrap().iter().any(|e| e.entry.name.starts_with("Unsynced")));

        // Read-only users cannot push.
        let ro = vec![Grant { principal: Principal::User(bob.clone()), role: Role::Read }];
        admin.admin_update_vault(vault.id, &VaultRequest { name: vault.name.clone(), description: String::new(), grants: ro, record_ssh: None }).unwrap();
        let req = PushRequest { changes: vec![PushChange { entry: Entry { vault_id: vault.id, name: "x".into(), ..Entry::default() }, base_revision: 0 }] };
        assert!(matches!(b_api.push(vault.id, &req), Err(crate::api::ApiError::Forbidden(_))));
    }
}
