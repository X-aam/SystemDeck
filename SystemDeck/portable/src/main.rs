//! SystemDeck-Portable.exe: runs the desktop client without installing it.
//!
//! systemdeck.exe imports WebView2Loader.dll at load time, so the two files must
//! sit side by side. This launcher carries both, unpacks them once per version to
//! %LOCALAPPDATA%\SystemDeck\portable\<version>\ and starts the client from there.
//! No admin rights, registry entries or shortcuts; settings live in %APPDATA% as
//! with the installed client.

#![windows_subsystem = "windows"]

use std::path::{Path, PathBuf};
use std::process::Command;

// Paths are set by build.ps1 (SYSTEMDECK_CLIENT_DIR = release\SystemDeck-Client).
const CLIENT_EXE: &[u8] = include_bytes!(concat!(env!("SYSTEMDECK_CLIENT_DIR"), "/systemdeck.exe"));
const LOADER_DLL: &[u8] = include_bytes!(concat!(env!("SYSTEMDECK_CLIENT_DIR"), "/WebView2Loader.dll"));

/// FNV-1a, so each client build unpacks into its own folder.
fn fingerprint(data: &[u8]) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in data {
        h ^= b as u64;
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{h:016x}")
}

/// Writes `data` to `path` unless an identical-size file is already there.
fn place(path: &Path, data: &[u8]) -> std::io::Result<()> {
    if std::fs::metadata(path).map(|m| m.len() == data.len() as u64).unwrap_or(false) {
        return Ok(());
    }
    let tmp = path.with_extension("part");
    std::fs::write(&tmp, data)?;
    std::fs::rename(&tmp, path)
}

fn run() -> Result<(), String> {
    let base = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .ok_or("LOCALAPPDATA is not set")?
        .join("SystemDeck")
        .join("portable");
    let version = fingerprint(CLIENT_EXE);
    let dir = base.join(&version);
    std::fs::create_dir_all(&dir).map_err(|e| format!("Can't create {}: {e}", dir.display()))?;

    let exe = dir.join("systemdeck.exe");
    place(&dir.join("WebView2Loader.dll"), LOADER_DLL).map_err(|e| format!("Can't unpack WebView2Loader.dll: {e}"))?;
    place(&exe, CLIENT_EXE).map_err(|e| format!("Can't unpack systemdeck.exe: {e}"))?;

    // Tidy up older unpacked versions (skipped silently if one is still running).
    if let Ok(entries) = std::fs::read_dir(&base) {
        for e in entries.flatten() {
            if e.file_name() != version.as_str() && e.path().is_dir() {
                let _ = std::fs::remove_dir_all(e.path());
            }
        }
    }

    Command::new(&exe)
        .args(std::env::args_os().skip(1))
        .current_dir(&dir)
        .spawn()
        .map_err(|e| format!("Can't start SystemDeck: {e}"))?;
    Ok(())
}

#[link(name = "user32")]
extern "system" {
    fn MessageBoxW(hwnd: isize, text: *const u16, caption: *const u16, kind: u32) -> i32;
}

fn main() {
    if let Err(msg) = run() {
        let wide = |s: &str| s.encode_utf16().chain(Some(0)).collect::<Vec<u16>>();
        // MB_ICONERROR
        unsafe { MessageBoxW(0, wide(&msg).as_ptr(), wide("SystemDeck Portable").as_ptr(), 0x10) };
        std::process::exit(1);
    }
}
