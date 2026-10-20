//! HTTP API request/response bodies.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::model::{Entry, Grant, UserInfo};

#[derive(Serialize, Deserialize, Debug)]
pub struct ErrorBody {
    pub error: String,
}

#[derive(Serialize, Deserialize)]
pub struct LoginRequest {
    pub username: String,
    pub password: String,
}

#[derive(Serialize, Deserialize)]
pub struct LoginResponse {
    pub token: String,
    pub user: UserInfo,
}

#[derive(Serialize, Deserialize)]
pub struct ChangePasswordRequest {
    pub old_password: String,
    pub new_password: String,
}

#[derive(Serialize, Deserialize, Default)]
pub struct ChangesQuery {
    #[serde(default)]
    pub since: u64,
}

/// Entries in a vault whose revision is greater than `since`, plus the
/// cursor to pass as `since` next time.
#[derive(Serialize, Deserialize)]
pub struct ChangesResponse {
    pub entries: Vec<Entry>,
    pub cursor: u64,
}

#[derive(Serialize, Deserialize)]
pub struct PushChange {
    pub entry: Entry,
    /// Revision the client's edit was based on (0 for a new entry). The
    /// server rejects the write as a conflict if its copy has moved on.
    pub base_revision: u64,
}

#[derive(Serialize, Deserialize)]
pub struct PushRequest {
    pub changes: Vec<PushChange>,
}

#[derive(Serialize, Deserialize, Debug)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum PushResult {
    Accepted { entry: Entry },
    Conflict { server: Entry },
    Rejected { id: Uuid, reason: String },
}

#[derive(Serialize, Deserialize)]
pub struct PushResponse {
    pub results: Vec<PushResult>,
}

#[derive(Serialize, Deserialize, Default)]
pub struct CreateUserRequest {
    pub username: String,
    pub display_name: String,
    pub password: String,
    pub is_admin: bool,
}

#[derive(Serialize, Deserialize, Default)]
pub struct UpdateUserRequest {
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub password: Option<String>,
    #[serde(default)]
    pub is_admin: Option<bool>,
    #[serde(default)]
    pub disabled: Option<bool>,
}

#[derive(Serialize, Deserialize, Default)]
pub struct TeamRequest {
    pub name: String,
    pub members: Vec<String>,
}

/// Admin dashboard overview.
#[derive(Serialize, Deserialize, Default)]
pub struct ServerStats {
    pub version: String,
    pub uptime_secs: u64,
    pub users: usize,
    pub admins: usize,
    pub disabled_users: usize,
    pub teams: usize,
    pub vaults: usize,
    pub entries: usize,
    pub active_sessions: usize,
    /// Entry count per vault id.
    pub vault_entries: std::collections::HashMap<Uuid, usize>,
    pub users_detail: Vec<UserActivity>,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct UserActivity {
    pub username: String,
    pub last_seen: Option<chrono::DateTime<chrono::Utc>>,
    pub sessions: usize,
}

/// Entry listing for administrators: never includes secrets.
#[derive(Serialize, Deserialize, Clone)]
pub struct EntrySummary {
    pub id: Uuid,
    pub name: String,
    pub kind: crate::model::EntryKind,
    pub group: String,
    pub host: String,
    pub username: String,
    pub updated_by: String,
    pub updated_at: chrono::DateTime<chrono::Utc>,
    #[serde(default)]
    pub record_session: bool,
}

#[derive(Serialize, Deserialize, Default)]
pub struct VaultRequest {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub grants: Vec<Grant>,
    /// None = leave unchanged (on update).
    #[serde(default)]
    pub record_ssh: Option<bool>,
}

/// One AI model as shown in Settings (the key is never returned).
#[derive(Serialize, Deserialize, Clone, Default)]
pub struct AiProviderInfo {
    pub id: String,
    pub label: String,
    pub url: String,
    pub model: String,
    pub key_set: bool,
}

/// Server configuration shown in the dashboard Settings page.
#[derive(Serialize, Deserialize, Clone, Default)]
pub struct ServerConfig {
    pub ai_providers: Vec<AiProviderInfo>,
    pub ai_default: String,
    pub ai_enabled: bool,
    pub recording_retention_days: i64,
    pub audit_retention_days: i64,
}

/// One model in a config update. An absent/empty `key` keeps the existing key
/// for that `id`; `id` absent means a new model.
#[derive(Serialize, Deserialize, Default)]
pub struct AiProviderInput {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub key: Option<String>,
}

/// Config changes from the dashboard. `None` leaves that part unchanged.
#[derive(Serialize, Deserialize, Default)]
pub struct ServerConfigUpdate {
    /// Full replacement of the model list when present.
    #[serde(default)]
    pub ai_providers: Option<Vec<AiProviderInput>>,
    #[serde(default)]
    pub ai_default: Option<String>,
    #[serde(default)]
    pub recording_retention_days: Option<i64>,
    #[serde(default)]
    pub audit_retention_days: Option<i64>,
}

/// One AI model offered to the client (no key or URL exposed).
#[derive(Serialize, Deserialize, Clone, Default)]
pub struct AiModel {
    pub id: String,
    pub label: String,
}

/// AI availability for signed-in clients (drives the assistant's model picker).
#[derive(Serialize, Deserialize, Clone, Default)]
pub struct AiModelsInfo {
    pub enabled: bool,
    pub default: String,
    pub models: Vec<AiModel>,
}

/// One earlier turn of an assistant conversation.
#[derive(Serialize, Deserialize, Clone, Default)]
pub struct AiChatTurn {
    /// "user" or "assistant".
    pub role: String,
    pub content: String,
}

/// In-session assistant request: the live session's context plus a question.
#[derive(Serialize, Deserialize, Default)]
pub struct AiChatRequest {
    /// Model id to use; None = the configured default model.
    #[serde(default)]
    pub model: Option<String>,
    /// "SSH", "RDP", "VNC", "WEB" or "SFTP".
    #[serde(default)]
    pub protocol: String,
    #[serde(default)]
    pub host: String,
    /// Recent session text (terminal buffer, current URL, ...). Server truncates.
    #[serde(default)]
    pub context: String,
    /// Earlier turns of this conversation, oldest first.
    #[serde(default)]
    pub history: Vec<AiChatTurn>,
    pub question: String,
}

#[derive(Serialize, Deserialize)]
pub struct AiChatResponse {
    pub reply: String,
}

/// One recorded action in the server audit log.
#[derive(Serialize, Deserialize, Clone)]
pub struct AuditEvent {
    pub at: chrono::DateTime<chrono::Utc>,
    /// Who did it (or attempted it).
    pub username: String,
    /// Short machine action, e.g. "login", "entry_write", "vault_delete".
    pub action: String,
    /// What it acted on (vault/user/entry name), may be empty.
    pub target: String,
    /// Extra context for the reader.
    pub detail: String,
}

/// What a recording contains.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum RecordingKind {
    /// asciicast v2: terminal output, keystrokes, resizes.
    #[default]
    Ssh,
    /// RDP/VNC: same JSON-lines layout; "f" = JPEG frame (base64),
    /// "i" = typed text, "k" = key combination, "m" = mouse click.
    Screen,
    /// Embedded browser: a header line, then one rrweb event (JSON) per line.
    /// Replays the page's DOM over time; far smaller than screen frames.
    Dom,
}

/// Client announces a new session recording.
#[derive(Serialize, Deserialize, Clone)]
pub struct RecordingStart {
    pub vault_id: Uuid,
    pub entry_id: Uuid,
    pub entry_name: String,
    pub host: String,
    /// Login used for the session.
    pub login: String,
    pub cols: u16,
    pub rows: u16,
    #[serde(default)]
    pub kind: RecordingKind,
    /// "SSH", "RDP" or "VNC".
    #[serde(default)]
    pub protocol: String,
}

#[derive(Serialize, Deserialize)]
pub struct RecordingStarted {
    pub id: Uuid,
}

/// Recording metadata (the asciicast data is stored separately, encrypted).
#[derive(Serialize, Deserialize, Clone)]
pub struct RecordingMeta {
    pub id: Uuid,
    pub username: String,
    pub vault_id: Uuid,
    pub vault_name: String,
    pub entry_id: Uuid,
    pub entry_name: String,
    pub host: String,
    pub login: String,
    pub started_at: chrono::DateTime<chrono::Utc>,
    pub ended_at: Option<chrono::DateTime<chrono::Utc>>,
    pub bytes: u64,
    pub finished: bool,
    #[serde(default)]
    pub kind: RecordingKind,
    #[serde(default)]
    pub protocol: String,
    /// AI-generated summary of the session, once requested (None = not yet).
    #[serde(default)]
    pub summary: Option<String>,
    /// AI-flagged sensitive actions in the session.
    #[serde(default)]
    pub flags: Vec<String>,
}
