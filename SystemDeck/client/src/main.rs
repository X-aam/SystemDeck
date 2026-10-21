#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

//! SystemDeck desktop client.
//!
//! `systemdeck.exe --import <file.csv>` imports a CSV into the Local vault
//! without opening the window.

mod admin;
mod api;
mod app;
mod import;
mod launch;
mod sessions;
mod ssh;
mod store;
mod sync;
mod theme;

use eframe::egui;

/// IronRDP and the session code log through `tracing`. Keep warnings (plus clipboard
/// details) in `%LOCALAPPDATA%\SystemDeck\client.log`, recreated on each start, so
/// connection and copy/paste problems can be diagnosed. `SYSTEMDECK_LOG` overrides the filter.
fn init_logging() {
    let base = std::env::var_os("SYSTEMDECK_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| dirs::data_local_dir().map(|d| d.join("SystemDeck")));
    let Some(dir) = base else { return };
    let _ = std::fs::create_dir_all(&dir);
    let Ok(file) = std::fs::File::create(dir.join("client.log")) else { return };
    let filter = tracing_subscriber::EnvFilter::try_from_env("SYSTEMDECK_LOG")
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn,ironrdp_cliprdr=debug,ironrdp_cliprdr_native=debug"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_ansi(false)
        .with_writer(std::sync::Mutex::new(file))
        .try_init();
}

fn main() -> eframe::Result<()> {
    init_logging();

    // WebView2 keeps its browser profile next to the exe by default, which is
    // read-only once installed under Program Files. Keep it per user instead.
    if std::env::var_os("WEBVIEW2_USER_DATA_FOLDER").is_none() {
        let base = std::env::var_os("SYSTEMDECK_HOME")
            .map(std::path::PathBuf::from)
            .or_else(|| dirs::data_local_dir().map(|d| d.join("SystemDeck")));
        if let Some(base) = base {
            std::env::set_var("WEBVIEW2_USER_DATA_FOLDER", base.join("WebView2"));
        }
    }

    let store = match store::Store::open_default() {
        Ok(store) => store,
        Err(err) => {
            eprintln!("SystemDeck: cannot open local store: {err:#}");
            std::process::exit(1);
        }
    };

    let args: Vec<String> = std::env::args().skip(1).collect();
    if let [flag, path] = args.as_slice() {
        if flag == "--import" {
            let who = store.settings().map(|s| s.username).unwrap_or_default();
            let who = if who.is_empty() { "local".to_string() } else { who };
            match import::import_csv(&store, std::path::Path::new(path), store::LOCAL_VAULT, &who) {
                Ok(report) => {
                    println!("{}", report.summary());
                    std::process::exit(0);
                }
                Err(err) => {
                    eprintln!("Import failed: {err:#}");
                    std::process::exit(1);
                }
            }
        }
    }

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("SystemDeck")
            .with_inner_size([1200.0, 720.0])
            .with_min_inner_size([820.0, 480.0]),
        ..Default::default()
    };
    eframe::run_native("SystemDeck", options, Box::new(move |cc| Ok(Box::new(app::App::new(cc, store)))))
}
