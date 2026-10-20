use std::{collections::HashSet, time::Duration};

use axum::{
    body::Bytes,
    extract::{DefaultBodyLimit, Path, Query, State},
    http::header,
    response::IntoResponse,
    routing::{get, post, put},
    Json, Router,
};
use chrono::Utc;
use serde_json::{json, Value};
use uuid::Uuid;
use systemdeck_core::{
    api::*,
    Principal, Role, Team, UserInfo, Vault, VaultSummary,
};

use crate::{
    auth::{self, AdminUser, AuthUser},
    error::{ApiError, ApiResult},
    store::{Db, StoredUser},
    AppState,
};

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/api/health", get(health))
        .route("/api/login", post(login))
        .route("/api/logout", post(logout))
        .route("/api/me", get(me))
        .route("/api/me/password", post(change_password))
        .route("/api/vaults", get(list_vaults))
        .route("/api/vaults/{id}/changes", get(changes))
        .route("/api/vaults/{id}/push", post(push))
        .route("/api/downloads", get(list_downloads))
        .route("/api/downloads/{name}", get(download_file))
        .route("/api/recordings", post(recording_start))
        // Recording chunks can be large (a full rrweb DOM snapshot or a batch
        // of screen frames), so lift the default 2 MB body limit for this route.
        .route("/api/recordings/{id}/chunk", post(recording_chunk).layer(DefaultBodyLimit::max(64 * 1024 * 1024)))
        .route("/api/recordings/{id}/finish", post(recording_finish))
        .route("/api/admin/recordings", get(admin_recordings))
        .route("/api/admin/recordings/{id}", get(admin_recording_data).delete(admin_delete_recording))
        .route("/api/admin/recordings/{id}/summarize", post(admin_summarize_recording))
        .route("/api/admin/users", get(admin_users).post(admin_create_user))
        .route("/api/admin/users/{username}", put(admin_update_user))
        .route("/api/admin/users/{username}/revoke", post(admin_revoke_sessions))
        .route("/api/admin/stats", get(admin_stats))
        .route("/api/admin/audit", get(admin_audit))
        .route("/api/admin/config", get(admin_get_config).put(admin_put_config))
        .route("/api/admin/config/test-ai", post(admin_test_ai))
        .route("/api/admin/vaults/{id}/entries", get(admin_vault_entries))
        .route("/api/admin/vaults/{id}/entries/{entry}/recording", put(admin_entry_recording))
        .route("/api/admin/teams", get(admin_teams).post(admin_create_team))
        .route("/api/admin/teams/{id}", put(admin_update_team).delete(admin_delete_team))
        .route("/api/admin/vaults", get(admin_vaults).post(admin_create_vault))
        .route("/api/admin/vaults/{id}", put(admin_update_vault).delete(admin_delete_vault))
        .merge(crate::web::router())
        .with_state(state)
}

fn ok() -> Json<Value> {
    Json(json!({ "ok": true }))
}

/// Strongest role the user holds on the vault, directly or via a team.
fn vault_role(db: &Db, user: &StoredUser, vault: &Vault) -> anyhow::Result<Option<Role>> {
    if user.is_admin {
        return Ok(Some(Role::Manage));
    }
    let my_teams: HashSet<Uuid> = db
        .teams()?
        .into_iter()
        .filter(|t| t.members.iter().any(|m| m.eq_ignore_ascii_case(&user.username)))
        .map(|t| t.id)
        .collect();
    Ok(vault
        .grants
        .iter()
        .filter(|g| match &g.principal {
            Principal::User(u) => u.eq_ignore_ascii_case(&user.username),
            Principal::Team(t) => my_teams.contains(t),
        })
        .map(|g| g.role)
        .max())
}

fn require_role(db: &Db, user: &StoredUser, vault_id: Uuid, needed: Role) -> ApiResult<Vault> {
    let vault = db.vault(vault_id)?.ok_or_else(|| ApiError::not_found("vault not found"))?;
    match vault_role(db, user, &vault)? {
        Some(role) if role >= needed => Ok(vault),
        Some(_) => Err(ApiError::forbidden("you do not have write access to this vault")),
        None => Err(ApiError::not_found("vault not found")),
    }
}

// ---- session ----

async fn health() -> Json<Value> {
    Json(json!({ "status": "ok", "version": env!("CARGO_PKG_VERSION") }))
}

async fn login(State(st): State<AppState>, Json(req): Json<LoginRequest>) -> ApiResult<Json<LoginResponse>> {
    let username = req.username.trim().to_lowercase();
    let user = st.db.user(&username)?;
    let hash = user.as_ref().map_or(auth::DUMMY_HASH.as_str(), |u| u.password_hash.as_str());
    let valid = auth::verify_password(&req.password, hash);
    match user {
        Some(u) if valid && !u.disabled => {
            let token = st.db.create_session(&u.username)?;
            if let Ok(mut seen) = st.last_seen.lock() {
                seen.insert(u.username.clone(), Utc::now());
            }
            tracing::info!(user = %u.username, "login");
            st.db.audit(&u.username, "login", "", "");
            Ok(Json(LoginResponse { token, user: u.info() }))
        }
        _ => {
            tracing::warn!(user = %username, "failed login");
            st.db.audit(&username, "login_failed", "", "");
            tokio::time::sleep(Duration::from_millis(500)).await;
            Err(ApiError::unauthorized("invalid username or password"))
        }
    }
}

async fn logout(State(st): State<AppState>, auth: AuthUser) -> ApiResult<Json<Value>> {
    st.db.delete_session(&auth.token_hash)?;
    st.db.audit(&auth.user.username, "logout", "", "");
    Ok(ok())
}

async fn me(auth: AuthUser) -> Json<UserInfo> {
    Json(auth.user.info())
}

async fn change_password(
    State(st): State<AppState>,
    auth: AuthUser,
    Json(req): Json<ChangePasswordRequest>,
) -> ApiResult<Json<Value>> {
    if !auth::verify_password(&req.old_password, &auth.user.password_hash) {
        return Err(ApiError::bad_request("current password is incorrect"));
    }
    auth::validate_password(&req.new_password)?;
    let mut user = auth.user;
    user.password_hash = auth::hash_password(&req.new_password)?;
    st.db.put_user(&user)?;
    st.db.audit(&user.username, "password_changed", "", "own password");
    Ok(ok())
}

// ---- vaults & sync ----

async fn list_vaults(State(st): State<AppState>, auth: AuthUser) -> ApiResult<Json<Vec<VaultSummary>>> {
    let mut out = Vec::new();
    for vault in st.db.vaults()? {
        if let Some(role) = vault_role(&st.db, &auth.user, &vault)? {
            out.push(VaultSummary { id: vault.id, name: vault.name, description: vault.description, role, record_ssh: vault.record_ssh });
        }
    }
    out.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    Ok(Json(out))
}

async fn changes(
    State(st): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
    Query(q): Query<ChangesQuery>,
) -> ApiResult<Json<ChangesResponse>> {
    require_role(&st.db, &auth.user, id, Role::Read)?;
    let (entries, cursor) = st.db.entries_since(id, q.since)?;
    Ok(Json(ChangesResponse { entries, cursor }))
}

async fn push(
    State(st): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
    Json(req): Json<PushRequest>,
) -> ApiResult<Json<PushResponse>> {
    require_role(&st.db, &auth.user, id, Role::Write)?;
    let mut results = Vec::with_capacity(req.changes.len());
    for change in req.changes {
        if change.entry.name.trim().is_empty() && !change.entry.deleted {
            results.push(PushResult::Rejected { id: change.entry.id, reason: "entry name is empty".into() });
            continue;
        }
        let entry_id = change.entry.id;
        let result = st.db.apply_change(id, change.entry, change.base_revision, &auth.user.username, !auth.user.is_admin)?;
        match &result {
            PushResult::Accepted { entry } => {
                tracing::info!(user = %auth.user.username, vault = %id, entry = %entry_id, rev = entry.revision, deleted = entry.deleted, "entry written");
                let action = if entry.deleted { "entry_delete" } else { "entry_write" };
                st.db.audit(&auth.user.username, action, &entry.name, &format!("vault {id}"));
            }
            PushResult::Conflict { .. } => {
                tracing::info!(user = %auth.user.username, vault = %id, entry = %entry_id, "push conflict")
            }
            PushResult::Rejected { .. } => {}
        }
        results.push(result);
    }
    Ok(Json(PushResponse { results }))
}

// ---- administration ----

async fn admin_users(State(st): State<AppState>, _a: AdminUser) -> ApiResult<Json<Vec<UserInfo>>> {
    let mut users: Vec<UserInfo> = st.db.users()?.iter().map(StoredUser::info).collect();
    users.sort_by(|a, b| a.username.cmp(&b.username));
    Ok(Json(users))
}

async fn admin_create_user(
    State(st): State<AppState>,
    AdminUser(admin): AdminUser,
    Json(req): Json<CreateUserRequest>,
) -> ApiResult<Json<UserInfo>> {
    let username = auth::validate_username(&req.username)?;
    auth::validate_password(&req.password)?;
    if st.db.user(&username)?.is_some() {
        return Err(ApiError::bad_request("a user with that name already exists"));
    }
    let display_name = if req.display_name.trim().is_empty() { username.clone() } else { req.display_name.trim().to_string() };
    let user = StoredUser {
        username,
        display_name,
        password_hash: auth::hash_password(&req.password)?,
        is_admin: req.is_admin,
        disabled: false,
        created_at: Utc::now(),
    };
    st.db.put_user(&user)?;
    st.db.audit(&admin.username, "user_create", &user.username, if user.is_admin { "administrator" } else { "" });
    Ok(Json(user.info()))
}

async fn admin_update_user(
    State(st): State<AppState>,
    AdminUser(admin): AdminUser,
    Path(username): Path<String>,
    Json(req): Json<UpdateUserRequest>,
) -> ApiResult<Json<UserInfo>> {
    let mut user = st.db.user(&username)?.ok_or_else(|| ApiError::not_found("user not found"))?;
    let is_self = user.username == admin.username;
    if is_self && (req.disabled == Some(true) || req.is_admin == Some(false)) {
        return Err(ApiError::bad_request("you cannot disable or demote your own account"));
    }
    let mut revoke_sessions = false;
    if let Some(name) = req.display_name {
        user.display_name = name.trim().to_string();
    }
    if let Some(password) = req.password {
        auth::validate_password(&password)?;
        user.password_hash = auth::hash_password(&password)?;
        revoke_sessions = !is_self;
    }
    if let Some(is_admin) = req.is_admin {
        user.is_admin = is_admin;
    }
    if let Some(disabled) = req.disabled {
        user.disabled = disabled;
        revoke_sessions |= disabled;
    }
    st.db.put_user(&user)?;
    if revoke_sessions {
        st.db.delete_sessions_for(&user.username)?;
    }
    st.db.audit(&admin.username, "user_update", &user.username, if user.disabled { "disabled" } else { "updated" });
    Ok(Json(user.info()))
}

async fn admin_revoke_sessions(
    State(st): State<AppState>,
    AdminUser(admin): AdminUser,
    Path(username): Path<String>,
) -> ApiResult<Json<Value>> {
    let user = st.db.user(&username)?.ok_or_else(|| ApiError::not_found("user not found"))?;
    if user.username == admin.username {
        return Err(ApiError::bad_request("use Sign out to end your own session"));
    }
    st.db.delete_sessions_for(&user.username)?;
    if let Ok(mut seen) = st.last_seen.lock() {
        seen.remove(&user.username);
    }
    tracing::info!(admin = %admin.username, user = %user.username, "sessions revoked");
    st.db.audit(&admin.username, "sessions_revoked", &user.username, "signed the user out everywhere");
    Ok(ok())
}

#[derive(serde::Deserialize, Default)]
struct AuditQuery {
    #[serde(default)]
    limit: Option<usize>,
    #[serde(default)]
    user: Option<String>,
    #[serde(default)]
    action: Option<String>,
}

async fn admin_get_config(State(st): State<AppState>, _a: AdminUser) -> ApiResult<Json<ServerConfig>> {
    let c = st.db.config()?;
    let providers = c.providers();
    let default = c.default_provider().map(|p| p.id).unwrap_or_default();
    Ok(Json(ServerConfig {
        ai_enabled: !providers.is_empty(),
        ai_default: default,
        ai_providers: providers
            .into_iter()
            .map(|p| AiProviderInfo { id: p.id, label: p.label, url: p.url, model: p.model, key_set: !p.key.is_empty() })
            .collect(),
        recording_retention_days: c.recording_retention_days,
        audit_retention_days: c.audit_retention_days,
    }))
}

async fn admin_put_config(State(st): State<AppState>, AdminUser(admin): AdminUser, Json(req): Json<ServerConfigUpdate>) -> ApiResult<Json<ServerConfig>> {
    let mut c = st.db.config()?;
    if let Some(list) = req.ai_providers {
        // Keys are write-only: a blank key keeps the existing one for that id.
        let existing = c.providers();
        let mut out = Vec::new();
        for p in list {
            let id = p.id.filter(|s| !s.is_empty()).unwrap_or_else(|| Uuid::new_v4().simple().to_string());
            let key = match p.key {
                Some(k) if !k.is_empty() => k,
                _ => existing.iter().find(|e| e.id == id).map(|e| e.key.clone()).unwrap_or_default(),
            };
            let label = if p.label.trim().is_empty() { p.model.clone() } else { p.label.trim().to_string() };
            out.push(crate::store::AiProvider {
                id,
                label,
                url: p.url.trim().trim_end_matches('/').to_string(),
                model: p.model.trim().to_string(),
                key,
            });
        }
        // Drop entries with no URL, and migrate off the legacy single fields.
        c.ai_providers = out.into_iter().filter(|p| !p.url.is_empty()).collect();
        c.ai_url = String::new();
        c.ai_model = String::new();
        c.ai_key = String::new();
        if !c.ai_providers.iter().any(|p| p.id == c.ai_default) {
            c.ai_default = c.ai_providers.first().map(|p| p.id.clone()).unwrap_or_default();
        }
    }
    if let Some(d) = req.ai_default {
        if c.ai_providers.iter().any(|p| p.id == d) {
            c.ai_default = d;
        }
    }
    if let Some(v) = req.recording_retention_days {
        c.recording_retention_days = v.max(0);
    }
    if let Some(v) = req.audit_retention_days {
        c.audit_retention_days = v.max(0);
    }
    st.db.put_config(&c)?;
    st.db.audit(&admin.username, "config_update", "server settings", &format!("{} model(s)", c.providers().len()));
    admin_get_config(State(st), AdminUser(admin)).await
}

#[derive(serde::Deserialize)]
struct TestAi {
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    key: Option<String>,
    /// Test an already-saved model by id (used when the key is hidden).
    #[serde(default)]
    id: Option<String>,
}

/// Tests an AI model: the one in the body, or a saved one by id, or the default.
async fn admin_test_ai(State(st): State<AppState>, _a: AdminUser, Json(req): Json<TestAi>) -> ApiResult<Json<Value>> {
    let saved = st.db.config()?;
    let base = req.id.as_deref().and_then(|id| saved.provider(id)).or_else(|| saved.default_provider()).unwrap_or_default();
    let url = req.url.filter(|s| !s.trim().is_empty()).unwrap_or(base.url);
    let model = req.model.filter(|s| !s.trim().is_empty()).unwrap_or(base.model);
    let key = match req.key {
        Some(k) if !k.is_empty() => k,
        _ => base.key,
    };
    let ai = crate::ai::Ai::new(Some(url), Some(model), Some(key)).ok_or_else(|| ApiError::bad_request("no endpoint URL to test"))?;
    match ai.ping().await {
        Ok(reply) => Ok(Json(json!({ "ok": true, "reply": reply.chars().take(120).collect::<String>() }))),
        Err(e) => Ok(Json(json!({ "ok": false, "error": format!("{e:#}") }))),
    }
}

async fn admin_audit(State(st): State<AppState>, _a: AdminUser, Query(q): Query<AuditQuery>) -> ApiResult<Json<Vec<AuditEvent>>> {
    let limit = q.limit.unwrap_or(500).clamp(1, 5000);
    let user = q.user.as_deref().filter(|s| !s.is_empty());
    let action = q.action.as_deref().filter(|s| !s.is_empty());
    Ok(Json(st.db.audit_log(limit, user, action)?))
}

async fn admin_stats(State(st): State<AppState>, _a: AdminUser) -> ApiResult<Json<ServerStats>> {
    let users = st.db.users()?;
    let vaults = st.db.vaults()?;
    let sessions = st.db.active_sessions()?;
    let mut vault_entries = std::collections::HashMap::new();
    for v in &vaults {
        let (entries, _) = st.db.entries_since(v.id, 0)?;
        vault_entries.insert(v.id, entries.iter().filter(|e| !e.deleted).count());
    }
    let seen = st.last_seen.lock().map(|m| m.clone()).unwrap_or_default();
    let mut users_detail: Vec<UserActivity> = users
        .iter()
        .map(|u| UserActivity {
            username: u.username.clone(),
            last_seen: seen.get(&u.username).copied(),
            sessions: sessions.get(&u.username).copied().unwrap_or(0),
        })
        .collect();
    users_detail.sort_by(|a, b| b.last_seen.cmp(&a.last_seen).then_with(|| a.username.cmp(&b.username)));
    Ok(Json(ServerStats {
        version: env!("CARGO_PKG_VERSION").to_string(),
        uptime_secs: st.started_at.elapsed().as_secs(),
        users: users.len(),
        admins: users.iter().filter(|u| u.is_admin).count(),
        disabled_users: users.iter().filter(|u| u.disabled).count(),
        teams: st.db.teams()?.len(),
        vaults: vaults.len(),
        entries: vault_entries.values().sum(),
        active_sessions: sessions.values().sum(),
        vault_entries,
        users_detail,
    }))
}

async fn admin_vault_entries(State(st): State<AppState>, _a: AdminUser, Path(id): Path<Uuid>) -> ApiResult<Json<Vec<EntrySummary>>> {
    st.db.vault(id)?.ok_or_else(|| ApiError::not_found("vault not found"))?;
    let (entries, _) = st.db.entries_since(id, 0)?;
    let mut out: Vec<EntrySummary> = entries
        .into_iter()
        .filter(|e| !e.deleted)
        .map(|e| EntrySummary {
            id: e.id,
            name: e.name,
            kind: e.kind,
            group: e.group,
            host: e.host,
            username: e.username,
            updated_by: e.updated_by,
            updated_at: e.updated_at,
            record_session: e.record_session,
        })
        .collect();
    out.sort_by(|a, b| (a.group.to_lowercase(), a.name.to_lowercase()).cmp(&(b.group.to_lowercase(), b.name.to_lowercase())));
    Ok(Json(out))
}

/// Entry-level recording switch from the dashboard. Written as a normal
/// revision, so every client picks it up on its next sync.
async fn admin_entry_recording(
    State(st): State<AppState>,
    AdminUser(admin): AdminUser,
    Path((vault_id, entry_id)): Path<(Uuid, Uuid)>,
    Json(req): Json<Value>,
) -> ApiResult<Json<Value>> {
    let record = req["record"].as_bool().ok_or_else(|| ApiError::bad_request("expected {\"record\": true|false}"))?;
    st.db.vault(vault_id)?.ok_or_else(|| ApiError::not_found("vault not found"))?;
    let (entries, _) = st.db.entries_since(vault_id, 0)?;
    let mut entry = entries.into_iter().find(|e| e.id == entry_id && !e.deleted).ok_or_else(|| ApiError::not_found("entry not found"))?;
    if !entry.kind.can_embed() {
        return Err(ApiError::bad_request("only SSH, RDP and VNC sessions can be recorded"));
    }
    entry.record_session = record;
    let base = entry.revision;
    match st.db.apply_change(vault_id, entry, base, &admin.username, false)? {
        PushResult::Accepted { entry } => {
            tracing::info!(admin = %admin.username, vault = %vault_id, entry = %entry_id, record, rev = entry.revision, "entry recording changed");
            st.db.audit(&admin.username, "entry_recording", &entry.name, if record { "recording on" } else { "recording off" });
            Ok(ok())
        }
        _ => Err(ApiError::bad_request("the entry changed meanwhile - reload and try again")),
    }
}

// ---- downloads (desktop client) ----

/// Files in the downloads folder (not recursive), for any signed-in user.
async fn list_downloads(State(st): State<AppState>, _auth: AuthUser) -> ApiResult<Json<Vec<Value>>> {
    let mut out = Vec::new();
    if let Ok(dir) = std::fs::read_dir(&st.downloads) {
        for item in dir.flatten() {
            let Ok(meta) = item.metadata() else { continue };
            if !meta.is_file() {
                continue;
            }
            let modified = meta.modified().ok().map(chrono::DateTime::<Utc>::from);
            out.push(json!({ "name": item.file_name().to_string_lossy(), "bytes": meta.len(), "modified": modified }));
        }
    }
    out.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
    Ok(Json(out))
}

async fn download_file(State(st): State<AppState>, auth: AuthUser, Path(name): Path<String>) -> ApiResult<impl IntoResponse> {
    // Plain file names only: no separators, no "..".
    let valid = !name.is_empty() && !name.starts_with('.') && name.chars().all(|c| c.is_ascii_alphanumeric() || "-_. ".contains(c));
    let path = st.downloads.join(&name);
    if !valid || !path.is_file() {
        return Err(ApiError::not_found("file not found"));
    }
    let data = tokio::task::spawn_blocking(move || std::fs::read(path))
        .await
        .map_err(|e| ApiError::from(anyhow::anyhow!("reading {name}: {e}")))?
        .map_err(|e| ApiError::from(anyhow::anyhow!("reading {name}: {e}")))?;
    tracing::info!(user = %auth.user.username, file = %name, "download");
    st.db.audit(&auth.user.username, "download", &name, "");
    Ok((
        [
            (header::CONTENT_TYPE, "application/octet-stream".to_string()),
            (header::CONTENT_DISPOSITION, format!("attachment; filename=\"{name}\"")),
        ],
        data,
    ))
}

// ---- session recordings (SSH, RDP, VNC) ----

async fn recording_start(State(st): State<AppState>, auth: AuthUser, Json(req): Json<RecordingStart>) -> ApiResult<Json<RecordingStarted>> {
    let vault = require_role(&st.db, &auth.user, req.vault_id, Role::Read)?;
    let meta = RecordingMeta {
        id: Uuid::new_v4(),
        username: auth.user.username.clone(),
        vault_id: vault.id,
        vault_name: vault.name,
        entry_id: req.entry_id,
        entry_name: req.entry_name,
        host: req.host,
        login: req.login,
        started_at: Utc::now(),
        ended_at: None,
        bytes: 0,
        finished: false,
        kind: req.kind,
        protocol: if req.protocol.is_empty() { "SSH".into() } else { req.protocol },
        summary: None,
        flags: Vec::new(),
    };
    st.db.put_recording(&meta)?;
    tracing::info!(user = %meta.username, host = %meta.host, protocol = %meta.protocol, recording = %meta.id, "session recording started");
    Ok(Json(RecordingStarted { id: meta.id }))
}

/// Only the user who started a recording may append to it, and only until it is finished.
fn own_open_recording(st: &AppState, user: &str, id: Uuid) -> ApiResult<RecordingMeta> {
    let meta = st.db.recording(id)?.ok_or_else(|| ApiError::not_found("recording not found"))?;
    if meta.username != user {
        return Err(ApiError::forbidden("not your recording"));
    }
    if meta.finished {
        return Err(ApiError::bad_request("recording already finished"));
    }
    Ok(meta)
}

async fn recording_chunk(State(st): State<AppState>, auth: AuthUser, Path(id): Path<Uuid>, body: Bytes) -> ApiResult<Json<Value>> {
    let mut meta = own_open_recording(&st, &auth.user.username, id)?;
    if !body.is_empty() {
        st.db.append_recording(id, &body)?;
        meta.bytes += body.len() as u64;
        meta.ended_at = Some(Utc::now());
        st.db.put_recording(&meta)?;
    }
    Ok(ok())
}

async fn recording_finish(State(st): State<AppState>, auth: AuthUser, Path(id): Path<Uuid>) -> ApiResult<Json<Value>> {
    let mut meta = own_open_recording(&st, &auth.user.username, id)?;
    meta.finished = true;
    meta.ended_at = Some(Utc::now());
    st.db.put_recording(&meta)?;
    tracing::info!(user = %meta.username, recording = %id, bytes = meta.bytes, "session recording finished");
    Ok(ok())
}

async fn admin_recordings(State(st): State<AppState>, _a: AdminUser) -> ApiResult<Json<Vec<RecordingMeta>>> {
    Ok(Json(st.db.recordings()?))
}

async fn admin_recording_data(State(st): State<AppState>, AdminUser(admin): AdminUser, Path(id): Path<Uuid>) -> ApiResult<impl IntoResponse> {
    let meta = st.db.recording(id)?.ok_or_else(|| ApiError::not_found("recording not found"))?;
    let data = st.db.read_recording(id)?;
    tracing::info!(admin = %admin.username, recording = %id, "recording viewed");
    st.db.audit(&admin.username, "recording_view", &format!("{} on {}", meta.username, meta.host), &meta.entry_name);
    let (ext, mime) = match meta.kind {
        RecordingKind::Ssh => ("cast", "application/x-asciicast"),
        RecordingKind::Screen => ("sdrec", "application/x-ndjson"),
        RecordingKind::Dom => ("sdweb", "application/x-ndjson"),
    };
    let file = format!("attachment; filename=\"{}-{}.{ext}\"", meta.started_at.format("%Y%m%d-%H%M%S"), meta.username);
    Ok((
        [(header::CONTENT_TYPE, mime.to_string()), (header::CONTENT_DISPOSITION, file)],
        data,
    ))
}

async fn admin_delete_recording(State(st): State<AppState>, AdminUser(admin): AdminUser, Path(id): Path<Uuid>) -> ApiResult<Json<Value>> {
    st.db.delete_recording(id)?;
    tracing::info!(admin = %admin.username, recording = %id, "recording deleted");
    st.db.audit(&admin.username, "recording_delete", &id.to_string(), "");
    Ok(ok())
}

#[derive(serde::Deserialize, Default)]
struct SummarizeQuery {
    /// Optional model id; defaults to the configured default model.
    #[serde(default)]
    model: Option<String>,
}

/// AI: summarise a recording and flag sensitive actions, caching the result.
async fn admin_summarize_recording(State(st): State<AppState>, AdminUser(admin): AdminUser, Path(id): Path<Uuid>, Query(q): Query<SummarizeQuery>) -> ApiResult<Json<RecordingMeta>> {
    let ai = st.ai(q.model.as_deref()).ok_or_else(|| ApiError::new(axum::http::StatusCode::SERVICE_UNAVAILABLE, "AI is not configured (add a model in Settings)"))?;
    let mut meta = st.db.recording(id)?.ok_or_else(|| ApiError::not_found("recording not found"))?;
    let data = st.db.read_recording(id)?;
    let transcript = transcript_for_ai(meta.kind, &data);
    if transcript.trim().is_empty() {
        return Err(ApiError::bad_request("this recording has no readable transcript to summarise"));
    }
    let result = ai
        .summarise(&meta.protocol, &meta.username, &meta.host, &transcript)
        .await
        .map_err(|e| ApiError::new(axum::http::StatusCode::BAD_GATEWAY, format!("AI request failed: {e:#}")))?;
    meta.summary = Some(result.summary);
    meta.flags = result.flags;
    st.db.put_recording(&meta)?;
    tracing::info!(admin = %admin.username, recording = %id, flags = meta.flags.len(), "recording summarised");
    st.db.audit(&admin.username, "recording_summarise", &format!("{} on {}", meta.username, meta.host), &format!("{} flag(s)", meta.flags.len()));
    Ok(Json(meta))
}

/// Builds a plain-text transcript for the model from a recording's bytes.
/// SSH: the terminal output (ANSI stripped). Screen/DOM: typed text, key
/// combinations, clicks and pages visited. Capped so the prompt stays small.
fn transcript_for_ai(kind: RecordingKind, data: &[u8]) -> String {
    const MAX: usize = 16_000;
    let text = String::from_utf8_lossy(data);
    let mut out = String::new();
    for line in text.lines() {
        let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
        match kind {
            RecordingKind::Ssh => {
                // asciicast events: [t, "o"|"i", data]; collect terminal output.
                if v.get(1).and_then(Value::as_str) == Some("o") {
                    if let Some(s) = v.get(2).and_then(Value::as_str) {
                        out.push_str(&strip_ansi(s));
                    }
                }
            }
            RecordingKind::Screen => {
                let (kind_s, data_s) = (v.get(1).and_then(Value::as_str), v.get(2).and_then(Value::as_str));
                match (kind_s, data_s) {
                    (Some("i"), Some(d)) => out.push_str(&strip_ansi(d)),
                    (Some("k"), Some(d)) => out.push_str(&format!("[key {d}]\n")),
                    (Some("m"), Some(d)) => out.push_str(&format!("[click {d}]\n")),
                    _ => {}
                }
            }
            RecordingKind::Dom => {
                // rrweb custom "url" events mark each page visited.
                if v.get("type").and_then(Value::as_i64) == Some(5) {
                    if let Some(d) = v.get("data") {
                        if d.get("tag").and_then(Value::as_str) == Some("url") {
                            if let Some(u) = d.get("payload").and_then(Value::as_str) {
                                out.push_str(&format!("visited {u}\n"));
                            }
                        }
                    }
                }
            }
        }
        if out.len() > MAX * 2 {
            break;
        }
    }
    // Keep the most recent part if very long.
    if out.len() > MAX {
        out = out[out.len() - MAX..].to_string();
    }
    out
}

/// Removes ANSI escape sequences (colours, cursor moves) from terminal text.
fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            if chars.peek() == Some(&'[') {
                chars.next();
                // CSI: consume parameters until a final byte (@ to ~).
                while let Some(&n) = chars.peek() {
                    chars.next();
                    if ('@'..='~').contains(&n) {
                        break;
                    }
                }
            } else {
                // OSC / other: skip to BEL or the next ESC.
                while let Some(&n) = chars.peek() {
                    if n == '\u{07}' || n == '\u{1b}' {
                        break;
                    }
                    chars.next();
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn clean_members(members: Vec<String>) -> Vec<String> {
    let mut out: Vec<String> = members
        .into_iter()
        .map(|m| m.trim().to_lowercase())
        .filter(|m| !m.is_empty())
        .collect();
    out.sort();
    out.dedup();
    out
}

async fn admin_teams(State(st): State<AppState>, _a: AdminUser) -> ApiResult<Json<Vec<Team>>> {
    let mut teams = st.db.teams()?;
    teams.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    Ok(Json(teams))
}

async fn admin_create_team(State(st): State<AppState>, AdminUser(admin): AdminUser, Json(req): Json<TeamRequest>) -> ApiResult<Json<Team>> {
    if req.name.trim().is_empty() {
        return Err(ApiError::bad_request("team name is required"));
    }
    let team = Team { id: Uuid::new_v4(), name: req.name.trim().to_string(), members: clean_members(req.members) };
    st.db.put_team(&team)?;
    st.db.audit(&admin.username, "team_create", &team.name, &format!("{} member(s)", team.members.len()));
    Ok(Json(team))
}

async fn admin_update_team(
    State(st): State<AppState>,
    AdminUser(admin): AdminUser,
    Path(id): Path<Uuid>,
    Json(req): Json<TeamRequest>,
) -> ApiResult<Json<Team>> {
    let mut team = st.db.team(id)?.ok_or_else(|| ApiError::not_found("team not found"))?;
    if !req.name.trim().is_empty() {
        team.name = req.name.trim().to_string();
    }
    team.members = clean_members(req.members);
    st.db.put_team(&team)?;
    st.db.audit(&admin.username, "team_update", &team.name, &format!("{} member(s)", team.members.len()));
    Ok(Json(team))
}

async fn admin_delete_team(State(st): State<AppState>, AdminUser(admin): AdminUser, Path(id): Path<Uuid>) -> ApiResult<Json<Value>> {
    let name = st.db.team(id)?.map(|t| t.name).unwrap_or_default();
    st.db.delete_team(id)?;
    st.db.audit(&admin.username, "team_delete", &name, "");
    Ok(ok())
}

async fn admin_vaults(State(st): State<AppState>, _a: AdminUser) -> ApiResult<Json<Vec<Vault>>> {
    let mut vaults = st.db.vaults()?;
    vaults.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    Ok(Json(vaults))
}

async fn admin_create_vault(State(st): State<AppState>, AdminUser(admin): AdminUser, Json(req): Json<VaultRequest>) -> ApiResult<Json<Vault>> {
    if req.name.trim().is_empty() {
        return Err(ApiError::bad_request("vault name is required"));
    }
    let vault = Vault {
        id: Uuid::new_v4(),
        name: req.name.trim().to_string(),
        description: req.description.trim().to_string(),
        grants: req.grants,
        record_ssh: req.record_ssh.unwrap_or(false),
        created_at: Utc::now(),
    };
    st.db.put_vault(&vault)?;
    st.db.audit(&admin.username, "vault_create", &vault.name, "");
    Ok(Json(vault))
}

async fn admin_update_vault(
    State(st): State<AppState>,
    AdminUser(admin): AdminUser,
    Path(id): Path<Uuid>,
    Json(req): Json<VaultRequest>,
) -> ApiResult<Json<Vault>> {
    let mut vault = st.db.vault(id)?.ok_or_else(|| ApiError::not_found("vault not found"))?;
    if !req.name.trim().is_empty() {
        vault.name = req.name.trim().to_string();
    }
    vault.description = req.description.trim().to_string();
    vault.grants = req.grants;
    if let Some(record) = req.record_ssh {
        vault.record_ssh = record;
    }
    st.db.put_vault(&vault)?;
    st.db.audit(&admin.username, "vault_update", &vault.name, &format!("{} grant(s), recording {}", vault.grants.len(), if vault.record_ssh { "on" } else { "off" }));
    Ok(Json(vault))
}

async fn admin_delete_vault(State(st): State<AppState>, AdminUser(admin): AdminUser, Path(id): Path<Uuid>) -> ApiResult<Json<Value>> {
    let name = st.db.vault(id)?.map(|v| v.name).unwrap_or_default();
    st.db.delete_vault(id)?;
    st.db.audit(&admin.username, "vault_delete", &name, "");
    Ok(ok())
}
