//! CSV import. Understands Remote Desktop Manager's CSV export
//! (`Host,Port,Description,Display Name,Folder`) and the common column names
//! of other tools (Name, Username, Password, Domain, URL, Group, Notes, Type).

use std::{collections::HashSet, path::Path};

use anyhow::{bail, Context, Result};
use chrono::Utc;
use uuid::Uuid;
use systemdeck_core::{Entry, EntryKind};

use crate::store::{LocalEntry, Store, LOCAL_VAULT};

#[derive(Default, Debug)]
pub struct ImportReport {
    pub added: usize,
    pub duplicates: usize,
    pub skipped: Vec<String>,
}

impl ImportReport {
    pub fn summary(&self) -> String {
        let mut s = format!("Imported {} entr(ies)", self.added);
        if self.duplicates > 0 {
            s += &format!(", {} already present", self.duplicates);
        }
        if !self.skipped.is_empty() {
            s += &format!(", {} skipped ({})", self.skipped.len(), self.skipped.join("; "));
        }
        s
    }
}

/// Column lookup by any of several (case-insensitive) header names.
struct Columns(Vec<String>);

impl Columns {
    fn find(&self, names: &[&str]) -> Option<usize> {
        names.iter().find_map(|n| self.0.iter().position(|h| h == n))
    }
}

pub fn import_csv(store: &Store, path: &Path, vault: Uuid, who: &str) -> Result<ImportReport> {
    let mut reader = csv::ReaderBuilder::new()
        .flexible(true)
        .trim(csv::Trim::All)
        .from_path(path)
        .with_context(|| format!("opening {}", path.display()))?;
    let headers = Columns(
        reader
            .headers()?
            .iter()
            .map(|h| h.trim_start_matches('\u{feff}').trim().to_lowercase())
            .collect(),
    );

    let col_name = headers.find(&["display name", "name", "title", "entry name"]);
    let col_host = headers.find(&["host", "hostname", "host name", "address", "computer", "server"]);
    if col_name.is_none() && col_host.is_none() {
        bail!("the CSV needs at least a Name or Host column (found: {})", headers.0.join(", "));
    }
    let col_port = headers.find(&["port"]);
    let col_kind = headers.find(&["type", "kind", "connection type", "protocol"]);
    let col_group = headers.find(&["folder", "group", "path", "group/folder"]);
    let col_user = headers.find(&["username", "user name", "user", "login"]);
    let col_pass = headers.find(&["password", "pass"]);
    let col_domain = headers.find(&["domain"]);
    let col_url = headers.find(&["url", "website", "web address"]);
    let col_notes = headers.find(&["description", "notes", "note", "comment", "comments"]);
    let col_tags = headers.find(&["tags", "keywords"]);

    // Skip rows that already exist (same name, host and port), so importing
    // the same file twice is harmless.
    let key = |e: &Entry| (e.name.to_lowercase(), e.host.to_lowercase(), e.port);
    let mut existing: HashSet<_> = store.entries(vault)?.iter().filter(|e| !e.entry.deleted).map(|e| key(&e.entry)).collect();

    let mut report = ImportReport::default();
    for (line, record) in reader.records().enumerate() {
        let line = line + 2;
        let record = match record {
            Ok(r) => r,
            Err(e) => {
                report.skipped.push(format!("line {line}: {e}"));
                continue;
            }
        };
        let get = |col: Option<usize>| col.and_then(|i| record.get(i)).unwrap_or("").to_string();

        let host = get(col_host);
        let name = Some(get(col_name)).filter(|n| !n.is_empty()).unwrap_or_else(|| host.clone());
        if name.is_empty() {
            report.skipped.push(format!("line {line}: no name or host"));
            continue;
        }
        let port_text = get(col_port);
        let port = match port_text.as_str() {
            "" => None,
            p => match p.parse::<u16>() {
                Ok(p) => Some(p),
                Err(_) => {
                    report.skipped.push(format!("line {line}: bad port '{p}'"));
                    continue;
                }
            },
        };
        let url = get(col_url);
        let kind = parse_kind(&get(col_kind)).unwrap_or_else(|| infer_kind(&host, port, &url));
        let url = if url.is_empty() && kind == EntryKind::Web && !host.is_empty() { format!("https://{host}") } else { url };

        let entry = Entry {
            vault_id: vault,
            kind,
            name,
            group: get(col_group).replace('\\', "/").trim_matches('/').to_string(),
            host,
            port,
            username: get(col_user),
            password: get(col_pass),
            domain: get(col_domain),
            url,
            notes: get(col_notes),
            tags: get(col_tags).split([',', ';']).map(|t| t.trim().to_string()).filter(|t| !t.is_empty()).collect(),
            updated_at: Utc::now(),
            updated_by: who.to_string(),
            ..Entry::default()
        };
        if !existing.insert(key(&entry)) {
            report.duplicates += 1;
            continue;
        }
        store.put_entry(&LocalEntry { entry, dirty: vault != LOCAL_VAULT, base_revision: 0, conflict: None })?;
        report.added += 1;
    }
    Ok(report)
}

fn parse_kind(s: &str) -> Option<EntryKind> {
    let s = s.to_lowercase();
    Some(match s.as_str() {
        "" => return None,
        _ if s.contains("rdp") || s.contains("remote desktop") => EntryKind::Rdp,
        _ if s.contains("ssh") || s.contains("putty") => EntryKind::Ssh,
        _ if s.contains("vnc") => EntryKind::Vnc,
        _ if s.contains("telnet") => EntryKind::Telnet,
        _ if s.contains("serial") || s.contains("com port") => EntryKind::Serial,
        _ if s.contains("web") || s.contains("http") => EntryKind::Web,
        _ if s.contains("credential") || s.contains("password") || s.contains("login") => EntryKind::Credential,
        _ if s.contains("note") => EntryKind::Note,
        _ => return None,
    })
}

/// Best guess when the export has no type column (RDM's CSV export doesn't).
fn infer_kind(host: &str, port: Option<u16>, url: &str) -> EntryKind {
    let upper = host.to_uppercase();
    if upper.starts_with("COM") && upper[3..].chars().all(|c| c.is_ascii_digit()) && upper.len() > 3 {
        return EntryKind::Serial;
    }
    match port {
        Some(3389..=3399) => EntryKind::Rdp,
        Some(22) => EntryKind::Ssh,
        Some(23) => EntryKind::Telnet,
        Some(5900..=5910) => EntryKind::Vnc,
        Some(80 | 443 | 8080 | 8443) => EntryKind::Web,
        Some(_) => EntryKind::Ssh,
        None if !url.is_empty() => EntryKind::Web,
        // An address without a port, e.g. a vCenter or appliance UI.
        None if host.contains('.') => EntryKind::Web,
        None => EntryKind::Credential,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds_from_rdm_export() {
        assert_eq!(infer_kind("10.0.0.1", Some(3389), ""), EntryKind::Rdp);
        assert_eq!(infer_kind("host", Some(3390), ""), EntryKind::Rdp);
        assert_eq!(infer_kind("10.0.0.1", Some(22), ""), EntryKind::Ssh);
        assert_eq!(infer_kind("10.0.0.1", Some(23), ""), EntryKind::Telnet);
        assert_eq!(infer_kind("COM6", None, ""), EntryKind::Serial);
        assert_eq!(infer_kind("192.168.201.219", None, ""), EntryKind::Web);
        assert_eq!(infer_kind("210244096", None, ""), EntryKind::Credential);
    }

    #[test]
    fn imports_and_dedupes() {
        let dir = std::env::temp_dir().join(format!("systemdeck-import-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("local.key"), systemdeck_core::crypto::random_key()).unwrap();
        let store = Store::open(&dir).unwrap();
        let csv = dir.join("in.csv");
        std::fs::write(
            &csv,
            "\u{feff}Host,Port,Description,Display Name,Folder\n10.0.0.5,3389,dc,DC01,Servers\\Core\nCOM6,,,serial,\n,,,,\n10.0.0.9,abc,,Bad,\n",
        )
        .unwrap();
        let r = import_csv(&store, &csv, LOCAL_VAULT, "me").unwrap();
        assert_eq!(r.added, 2, "{r:?}");
        assert_eq!(r.skipped.len(), 2, "{r:?}");
        let entries = store.entries(LOCAL_VAULT).unwrap();
        let dc = entries.iter().find(|e| e.entry.name == "DC01").unwrap();
        assert_eq!((dc.entry.kind, dc.entry.group.as_str(), dc.entry.notes.as_str()), (EntryKind::Rdp, "Servers/Core", "dc"));
        assert!(!dc.dirty, "local vault entries are never queued for push");
        assert_eq!(import_csv(&store, &csv, LOCAL_VAULT, "me").unwrap().duplicates, 2);
        drop(store);
        let _ = std::fs::remove_dir_all(dir);
    }
}
