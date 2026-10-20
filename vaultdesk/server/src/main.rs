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
    pub ai: Option<crate::ai::Ai>,
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

    let downloads = args.downloads_dir.unwrap_or_else(|| {
        std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.join("downloads"))).unwrap_or_else(|| PathBuf::from("downloads"))
    });
    let db = Arc::new(db);

    // Retention: prune on start, then once a day in the background.
    let (rec_days, audit_days) = (args.recording_retention_days, args.audit_retention_days);
    if rec_days > 0 || audit_days > 0 {
        prune(&db, rec_days, audit_days);
        let bg = db.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(std::time::Duration::from_secs(24 * 60 * 60));
            tick.tick().await; // the immediate first tick; we already pruned above
            loop {
                tick.tick().await;
                prune(&bg, rec_days, audit_days);
            }
        });
    }

    let ai = ai::Ai::new(args.ai_url, args.ai_model, args.ai_key);
    if let Some(ai) = &ai {
        tracing::info!("AI features enabled (model {})", ai.model());
    }
    let state = AppState { db, ai, downloads, started_at: Instant::now(), last_seen: Arc::new(Mutex::new(HashMap::new())) };
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
