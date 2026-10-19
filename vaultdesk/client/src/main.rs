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

fn main() -> eframe::Result<()> {
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
