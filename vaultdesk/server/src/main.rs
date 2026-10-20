//! SystemDeck team server: stores shared vaults and syncs them to desktop clients.

mod ai;
mod auth;
mod error;
mod routes;
mod store;
mod web;

use std::{
    collections::HashMap,
    fs,
    net::SocketAddr,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Instant,
};

use anyhow::{bail, Context, Result};
use chrono::{DateTime, Utc};
use clap::Parser;
use tracing_subscriber::EnvFilter;
use systemdeck_core::crypto::{self, Key, KEY_LEN};

use crate::store::{Db, StoredUser};

#[derive(Parser)]
#[command(name = "systemdeck-server", about = "SystemDeck team server")]
struct Args {
    /// Address to listen on.
    #[arg(long, default_value = "0.0.0.0:8740", env = "SYSTEMDECK_BIND")]
    bind: SocketAddr,

    /// Directory holding the database and the server encryption key.
    #[arg(long, default_value = "./systemdeck-data", env = "SYSTEMDECK_DATA")]
    data_dir: PathBuf,

    /// Set a new random password for USER (creating it as an admin if it
    /// does not exist), print it, and exit.
    #[arg(long, value_name = "USER")]
    reset_password: Option<String>,

    /// Files signed-in users can download from the dashboard (the desktop
    /// client). Default: `downloads` next to the server executable.
    #[arg(long, env = "SYSTEMDECK_DOWNLOADS")]
    downloads_dir: Option<PathBuf>,

    /// Delete finished recordings older than this many days (0 = keep forever).
    /// Pruned on start and once a day.
    #[arg(long, default_value_t = 0, env = "SYSTEMDECK_RECORDING_RETENTION_DAYS")]
    recording_retention_days: i64,

    /// Delete audit-log entries older than this many days (0 = keep forever).
    #[arg(long, default_value_t = 0, env = "SYSTEMDECK_AUDIT_RETENTION_DAYS")]
    audit_retention_days: i64,

    /// OpenAI-compatible LLM endpoint for AI features, ending in /v1
    /// (e.g. http://localhost:11434/v1 for Ollama). Empty = AI disabled.
    #[arg(long, env = "SYSTEMDECK_AI_URL")]
    ai_url: Option<String>,

    /// Model name for the AI endpoint (e.g. llama3.1, qwen2.5).
    #[arg(long, env = "SYSTEMDECK_AI_MODEL")]
    ai_model: Option<String>,

    /// Optional bearer key for the AI endpoint.
    #[arg(long, env = "SYSTEMDECK_AI_KEY")]
    ai_key: Option<String>,
}

#[derive(Clone)]
pub struct AppState {
    pub db: Arc<Db>,
    pub downloads: PathBuf,
    pub started_at: Instant,
    /// Last authenticated request per user (in memory; drives "online now").
    pub last_seen: Arc<Mutex<HashMap<String, DateTime<Utc>>>>,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .init();
    let args = Args::parse();

    fs::create_dir_all(&args.data_dir)
        .with_context(|| format!("creating {}", args.data_dir.display()))?;
    let key = load_or_create_key(&args.data_dir.join("server.key"))?;
    // Database of the former product name (VaultDesk): rename it once.
    let db_path = args.data_dir.join("systemdeck.redb");
    let legacy_db = args.data_dir.join("vaultdesk.redb");
    if !db_path.exists() && legacy_db.exists() {
        fs::rename(&legacy_db, &db_path).with_context(|| format!("renaming {} to {}", legacy_db.display(), db_path.display()))?;
        tracing::info!("renamed {} to {}", legacy_db.display(), db_path.display());
    }
    let db = Db::open(&db_path, key)?;

    if let Some(username) = args.reset_password {
        let password = auth::random_password();
        set_password(&db, &username.to_lowercase(), &password)?;
        println!("Password for '{}' set to: {password}", username.to_lowercase());
        return Ok(());
    }

    if db.users()?.is_empty() {
        let password = auth::random_password();
        set_password(&db, "admin", &password)?;
        println!("============================================================");
        println!(" First start: created administrator account");
        println!("   username: admin");
        println!("   password: {password}");
        println!(" Sign in from the client and change it (Admin > Users).");
        println!("============================================================");
    }

    let downloads = args.downloads_dir.clone().unwrap_or_else(|| {
        std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.join("downloads"))).unwrap_or_else(|| PathBuf::from("downloads"))
    });
    let db = Arc::new(db);

    // The dashboard-editable config in the DB is the source of truth; seed it
    // once from CLI/env for an existing deployment, then it is managed from the UI.
    seed_config_from_args(&db, &args)?;
    if db.config()?.ai_url.trim().is_empty() {
        tracing::info!("AI features disabled (set an endpoint in the dashboard Settings)");
    } else {
        tracing::info!("AI features enabled");
    }

    // Retention runs daily, always reading the current config.
    {
        let bg = db.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(std::time::Duration::from_secs(24 * 60 * 60));
            loop {
                tick.tick().await;
                if let Ok(c) = bg.config() {
                    prune(&bg, c.recording_retention_days, c.audit_retention_days);
                }
            }
        });
    }

    let state = AppState { db, downloads, started_at: Instant::now(), last_seen: Arc::new(Mutex::new(HashMap::new())) };
    let app = routes::router(state);
    let listener = tokio::net::TcpListener::bind(args.bind)
        .await
        .with_context(|| format!("binding {}", args.bind))?;
    tracing::info!("SystemDeck server listening on http://{} (admin dashboard at /)", args.bind);
    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    Ok(())
}

impl AppState {
    /// Builds an AI client from the current (dashboard-editable) config,
    /// or None when no endpoint is configured.
    pub fn ai(&self) -> Option<crate::ai::Ai> {
        let c = self.db.config().ok()?;
        crate::ai::Ai::new(Some(c.ai_url), Some(c.ai_model), Some(c.ai_key))
    }
}

/// Seeds the DB config from CLI/env the first time each field is empty, so an
/// existing env-based deployment keeps working after the move to UI config.
fn seed_config_from_args(db: &Db, args: &Args) -> Result<()> {
    let mut c = db.config()?;
    let mut changed = false;
    let take = |opt: &Option<String>| opt.as_deref().map(str::trim).filter(|s| !s.is_empty()).map(str::to_string);
    if c.ai_url.is_empty() {
        if let Some(v) = take(&args.ai_url) {
            c.ai_url = v;
            changed = true;
        }
    }
    if c.ai_model.is_empty() {
        if let Some(v) = take(&args.ai_model) {
            c.ai_model = v;
            changed = true;
        }
    }
    if c.ai_key.is_empty() {
        if let Some(v) = take(&args.ai_key) {
            c.ai_key = v;
            changed = true;
        }
    }
    if c.recording_retention_days == 0 && args.recording_retention_days > 0 {
        c.recording_retention_days = args.recording_retention_days;
        changed = true;
    }
    if c.audit_retention_days == 0 && args.audit_retention_days > 0 {
        c.audit_retention_days = args.audit_retention_days;
        changed = true;
    }
    if changed {
        db.put_config(&c)?;
    }
    Ok(())
}

/// Applies the retention policy, logging what it removed.
fn prune(db: &Db, rec_days: i64, audit_days: i64) {
    match db.prune_recordings(rec_days) {
        Ok(n) if n > 0 => tracing::info!("retention: deleted {n} recording(s) older than {rec_days} days"),
        Err(e) => tracing::warn!("recording retention failed: {e:#}"),
        _ => {}
    }
    match db.prune_audit(audit_days) {
        Ok(n) if n > 0 => tracing::info!("retention: deleted {n} audit entr(ies) older than {audit_days} days"),
        Err(e) => tracing::warn!("audit retention failed: {e:#}"),
        _ => {}
    }
}

fn set_password(db: &Db, username: &str, password: &str) -> Result<()> {
    let hash = auth::hash_password(password)?;
    let user = match db.user(username)? {
        Some(mut u) => {
            u.password_hash = hash;
            u.disabled = false;
            u
        }
        None => StoredUser {
            username: username.to_string(),
            display_name: username.to_string(),
            password_hash: hash,
            is_admin: true,
            disabled: false,
            created_at: Utc::now(),
        },
    };
    db.put_user(&user)?;
    db.delete_sessions_for(username)?;
    Ok(())
}

/// The server key encrypts every entry at rest. Losing it means losing the
/// data, so it is created once and never rotated automatically.
fn load_or_create_key(path: &PathBuf) -> Result<Key> {
    if path.exists() {
        let bytes = fs::read(path).with_context(|| format!("reading {}", path.display()))?;
        if bytes.len() != KEY_LEN {
            bail!("{} is corrupt (expected {KEY_LEN} bytes)", path.display());
        }
        let mut key = [0u8; KEY_LEN];
        key.copy_from_slice(&bytes);
        return Ok(key);
    }
    let key = crypto::random_key();
    fs::write(path, key).with_context(|| format!("writing {}", path.display()))?;
    tracing::warn!("created new encryption key at {} - back it up with the database", path.display());
    Ok(key)
}
