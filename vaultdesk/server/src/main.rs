//! SystemDeck team server: stores shared vaults and syncs them to desktop clients.

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

    let downloads = args.downloads_dir.unwrap_or_else(|| {
        std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.join("downloads"))).unwrap_or_else(|| PathBuf::from("downloads"))
    });
    let state = AppState { db: Arc::new(db), downloads, started_at: Instant::now(), last_seen: Arc::new(Mutex::new(HashMap::new())) };
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
