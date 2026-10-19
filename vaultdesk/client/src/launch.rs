//! Opens connections with the tools already on the machine (mstsc, ssh,
//! vncviewer, the default browser). Programs are spawned directly, never via
//! a shell, and host/user values are validated before use.

use std::process::Command;
#[cfg(windows)]
use std::{thread, time::Duration};

use systemdeck_core::{DisplaySettings, Entry, EntryKind, Quality, Resolution, Scaling};

pub fn launch(entry: &Entry) -> Result<String, String> {
    match entry.kind {
        EntryKind::Rdp => rdp(entry),
        EntryKind::Ssh => ssh(entry),
        EntryKind::Vnc => vnc(entry),
        EntryKind::Telnet => telnet(entry),
        EntryKind::Serial => serial(entry),
        EntryKind::Web => web(entry),
        EntryKind::RemoteSupport => remote_support(entry),
        EntryKind::Credential | EntryKind::Note => Err("Nothing to launch for this entry type - use the copy buttons".into()),
    }
}

/// Starts an attended remote-support session through RustDesk, pointed at the
/// stored ID. The person at the remote PC accepts the prompt (or it connects
/// with the stored unattended password). RustDesk talks only to the server
/// baked into its own config, so support traffic stays on your network.
fn remote_support(entry: &Entry) -> Result<String, String> {
    let id: String = entry.host.chars().filter(|c| !c.is_whitespace()).collect();
    if id.is_empty() {
        return Err("No RustDesk ID set for this entry".into());
    }
    if !id.chars().all(|c| c.is_ascii_digit()) {
        return Err("RustDesk ID should be digits only".into());
    }
    let exe = rustdesk_exe().ok_or(
        "RustDesk is not installed. Install your organisation's RustDesk client (it is preconfigured \
         to use your own server), then try again.",
    )?;
    let mut cmd = Command::new(exe);
    cmd.arg("--connect").arg(&id);
    if !entry.password.is_empty() {
        cmd.arg("--password").arg(&entry.password);
    }
    #[cfg(windows)]
    {
        use platform::*;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd.spawn().map_err(|e| format!("Could not start RustDesk: {e}"))?;
    Ok(format!("Starting remote support to {id} - the remote user is asked to accept"))
}

/// Finds the RustDesk client: PATH first, then the usual install locations.
fn rustdesk_exe() -> Option<std::path::PathBuf> {
    #[cfg(windows)]
    {
        let mut candidates = vec![std::path::PathBuf::from("rustdesk.exe")];
        for var in ["ProgramFiles", "ProgramFiles(x86)", "LOCALAPPDATA"] {
            if let Some(base) = std::env::var_os(var) {
                candidates.push(std::path::Path::new(&base).join("RustDesk").join("rustdesk.exe"));
            }
        }
        candidates.into_iter().find(|p| p.components().count() == 1 || p.exists())
    }
    #[cfg(not(windows))]
    {
        Some(std::path::PathBuf::from("rustdesk"))
    }
}

fn check(value: &str, what: &str) -> Result<(), String> {
    if value.starts_with('-') || value.chars().any(|c| c.is_control() || c.is_whitespace() || c == '"') {
        return Err(format!("{what} contains characters that are not allowed"));
    }
    Ok(())
}

fn host(entry: &Entry) -> Result<&str, String> {
    let host = entry.host.trim();
    if host.is_empty() {
        return Err("No host set for this entry".into());
    }
    check(host, "Host")?;
    check(&entry.username, "Username")?;
    check(&entry.domain, "Domain")?;
    Ok(host)
}

fn web(entry: &Entry) -> Result<String, String> {
    let url = if entry.url.trim().is_empty() { format!("https://{}", entry.host.trim()) } else { entry.url.trim().to_string() };
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        return Err("Only http:// and https:// links can be opened".into());
    }
    open::that_detached(&url).map_err(|e| format!("Could not open browser: {e}"))?;
    Ok(format!("Opened {url}"))
}

#[cfg(windows)]
mod platform {
    pub use std::os::windows::process::CommandExt;
    pub const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    pub const CREATE_NEW_CONSOLE: u32 = 0x0000_0010;
}

/// Writes a temporary .rdp file and, if a password is stored, hands it to
/// mstsc through a short-lived Windows credential (`TERMSRV/<host>`) that is
/// removed again after 30 seconds.
#[cfg(windows)]
fn rdp(entry: &Entry) -> Result<String, String> {
    use platform::*;

    let host = host(entry)?;
    let address = match entry.port {
        Some(p) if p != 3389 => format!("{host}:{p}"),
        _ => host.to_string(),
    };
    let user = match (entry.domain.trim(), entry.username.trim()) {
        (_, "") => String::new(),
        ("", u) => u.to_string(),
        (d, u) => format!("{d}\\{u}"),
    };

    let mut rdp = format!("full address:s:{address}\r\nauthentication level:i:2\r\nprompt for credentials:i:0\r\n");
    rdp += &rdp_display_settings(&entry.screen);
    if !user.is_empty() {
        rdp += &format!("username:s:{user}\r\n");
    }
    let rdp_file = std::env::temp_dir().join(format!("systemdeck-{}.rdp", entry.id));
    std::fs::write(&rdp_file, rdp).map_err(|e| format!("Could not write .rdp file: {e}"))?;

    // mstsc looks the credential up as TERMSRV/<host>; with a custom port some
    // versions use TERMSRV/<host:port>, so store both.
    let mut targets = vec![format!("TERMSRV/{host}")];
    if address != host {
        targets.push(format!("TERMSRV/{address}"));
    }
    let has_credentials = !entry.password.is_empty() && !user.is_empty();
    // Only remove credentials we created, never ones the user saved in Windows.
    let mut created = Vec::new();
    if has_credentials {
        for target in &targets {
            let existing = Command::new("cmdkey")
                .arg(format!("/list:{target}"))
                .creation_flags(CREATE_NO_WINDOW)
                .output()
                .map(|o| String::from_utf8_lossy(&o.stdout).contains(target.as_str()))
                .unwrap_or(false);
            let status = Command::new("cmdkey")
                .arg(format!("/generic:{target}"))
                .arg(format!("/user:{user}"))
                .arg(format!("/pass:{}", entry.password))
                .creation_flags(CREATE_NO_WINDOW)
                .status()
                .map_err(|e| format!("cmdkey failed: {e}"))?;
            if !status.success() {
                return Err(format!("cmdkey could not store the credential for {target}"));
            }
            if !existing {
                created.push(target.clone());
            }
        }
    }

    let mut mstsc = Command::new("mstsc").arg(&rdp_file).spawn().map_err(|e| format!("Could not start mstsc: {e}"))?;

    // Clean up when mstsc exits, not on a timer: the certificate prompt can
    // keep mstsc waiting before it reads the credential.
    thread::spawn(move || {
        let _ = mstsc.wait();
        // mstsc may hand off to an existing instance and exit at once; give it time to read.
        thread::sleep(Duration::from_secs(5));
        for target in created {
            let _ = Command::new("cmdkey").arg(format!("/delete:{target}")).creation_flags(CREATE_NO_WINDOW).status();
        }
        let _ = std::fs::remove_file(rdp_file);
    });
    if has_credentials {
        Ok(format!("Connecting to {address} with Remote Desktop (signing in as {user})"))
    } else {
        Ok(format!("Connecting to {address} - no username/password stored, mstsc will ask"))
    }
}

/// `.rdp` lines for resolution, colour depth, scaling and the experience preset.
fn rdp_display_settings(d: &DisplaySettings) -> String {
    let mut out = String::new();
    let mut line = |key: &str, value: String| {
        out += &format!("{key}:{value}\r\n");
    };
    line("screen mode id", format!("i:{}", if d.full_screen { 2 } else { 1 }));
    match d.resolution {
        Resolution::Fixed(w, h) => {
            line("desktopwidth", format!("i:{w}"));
            line("desktopheight", format!("i:{h}"));
            line("dynamic resolution", "i:0".into());
        }
        Resolution::Fit => line("dynamic resolution", "i:1".into()),
    }
    line("smart sizing", format!("i:{}", u8::from(d.scaling != Scaling::Actual)));
    let bpp = if d.quality == Quality::Fast { 16 } else { d.color_depth.bits() };
    line("session bpp", format!("i:{bpp}"));
    // Experience flags (1 = disable, except font smoothing / composition).
    let (connection_type, wallpaper, themes, drag, menus, smoothing, composition) = match d.quality {
        Quality::Best => (6, 0, 0, 0, 0, 1, 1),
        Quality::Balanced => (4, 1, 0, 1, 1, 1, 1),
        Quality::Fast => (2, 1, 1, 1, 1, 0, 0),
    };
    line("connection type", format!("i:{connection_type}"));
    line("networkautodetect", "i:0".into());
    line("bandwidthautodetect", "i:1".into());
    line("disable wallpaper", format!("i:{wallpaper}"));
    line("disable themes", format!("i:{themes}"));
    line("disable full window drag", format!("i:{drag}"));
    line("disable menu anims", format!("i:{menus}"));
    line("allow font smoothing", format!("i:{smoothing}"));
    line("allow desktop composition", format!("i:{composition}"));
    line("bitmapcachepersistenable", "i:1".into());
    line("compression", "i:1".into());
    out
}

#[cfg(windows)]
fn ssh(entry: &Entry) -> Result<String, String> {
    use platform::*;

    let host = host(entry)?;
    let mut cmd = Command::new("ssh");
    if let Some(port) = entry.port {
        cmd.arg("-p").arg(port.to_string());
    }
    let dest = if entry.username.trim().is_empty() { host.to_string() } else { format!("{}@{host}", entry.username.trim()) };
    cmd.arg(&dest).creation_flags(CREATE_NEW_CONSOLE);
    cmd.spawn().map_err(|e| format!("Could not start ssh (is OpenSSH installed?): {e}"))?;
    Ok(format!("Opened SSH session to {dest}"))
}

#[cfg(windows)]
fn vnc(entry: &Entry) -> Result<String, String> {
    let host = host(entry)?;
    let target = format!("{host}::{}", entry.port.unwrap_or(5900));
    Command::new("vncviewer")
        .arg(&target)
        .spawn()
        .map_err(|e| format!("Could not start vncviewer (install TightVNC/RealVNC and add it to PATH): {e}"))?;
    Ok(format!("Opened VNC viewer for {target}"))
}

/// PuTTY first (handles telnet well and is common on admin machines), then
/// the Windows/OS telnet client.
fn telnet(entry: &Entry) -> Result<String, String> {
    let host = host(entry)?;
    let port = entry.port.unwrap_or(23).to_string();
    if Command::new("putty").args(["-telnet", host, "-P", &port]).spawn().is_ok() {
        return Ok(format!("Opened Telnet session to {host}:{port} in PuTTY"));
    }
    let mut cmd = Command::new("telnet");
    cmd.args([host, &port]);
    #[cfg(windows)]
    {
        use platform::*;
        cmd.creation_flags(CREATE_NEW_CONSOLE);
    }
    cmd.spawn().map_err(|e| format!("Could not start PuTTY or telnet (install PuTTY or enable the Telnet Client feature): {e}"))?;
    Ok(format!("Opened Telnet session to {host}:{port}"))
}

/// Host holds the line (COM6, /dev/ttyUSB0); the port field, if set, is the baud rate.
fn serial(entry: &Entry) -> Result<String, String> {
    let line = host(entry)?;
    let mut cmd = Command::new("putty");
    cmd.args(["-serial", line]);
    if let Some(baud) = entry.port {
        cmd.args(["-sercfg", &baud.to_string()]);
    }
    cmd.spawn().map_err(|e| format!("Could not start PuTTY for the serial console: {e}"))?;
    Ok(format!("Opened serial console on {line}"))
}

#[cfg(not(windows))]
fn rdp(entry: &Entry) -> Result<String, String> {
    let host = host(entry)?;
    let address = format!("{host}:{}", entry.port.unwrap_or(3389));
    let mut cmd = Command::new("xfreerdp");
    cmd.arg(format!("/v:{address}"));
    if !entry.username.is_empty() {
        cmd.arg(format!("/u:{}", entry.username));
    }
    if !entry.domain.is_empty() {
        cmd.arg(format!("/d:{}", entry.domain));
    }
    cmd.spawn().map_err(|e| format!("Could not start xfreerdp: {e}"))?;
    Ok(format!("Connecting to {address}"))
}

#[cfg(not(windows))]
fn ssh(entry: &Entry) -> Result<String, String> {
    let host = host(entry)?;
    let dest = if entry.username.is_empty() { host.to_string() } else { format!("{}@{host}", entry.username) };
    let mut cmd = Command::new("x-terminal-emulator");
    cmd.arg("-e").arg("ssh");
    if let Some(port) = entry.port {
        cmd.arg("-p").arg(port.to_string());
    }
    cmd.arg(&dest).spawn().map_err(|e| format!("Could not open terminal: {e}"))?;
    Ok(format!("Opened SSH session to {dest}"))
}

#[cfg(not(windows))]
fn vnc(entry: &Entry) -> Result<String, String> {
    let host = host(entry)?;
    let target = format!("{host}::{}", entry.port.unwrap_or(5900));
    Command::new("vncviewer").arg(&target).spawn().map_err(|e| format!("Could not start vncviewer: {e}"))?;
    Ok(format!("Opened VNC viewer for {target}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use systemdeck_core::ColorDepth;

    #[test]
    fn rdp_file_reflects_display_settings() {
        let fast = DisplaySettings {
            resolution: Resolution::Fixed(1600, 900),
            color_depth: ColorDepth::Bits32,
            quality: Quality::Fast,
            scaling: Scaling::Fit,
            smooth: true,
            full_screen: true,
        };
        let rdp = rdp_display_settings(&fast);
        for line in ["screen mode id:i:2", "desktopwidth:i:1600", "desktopheight:i:900", "session bpp:i:16", "disable wallpaper:i:1", "allow font smoothing:i:0", "smart sizing:i:1"] {
            assert!(rdp.contains(line), "missing {line} in\n{rdp}");
        }
        let best = DisplaySettings { quality: Quality::Best, resolution: Resolution::Fit, full_screen: false, scaling: Scaling::Actual, ..fast };
        let rdp = rdp_display_settings(&best);
        for line in ["screen mode id:i:1", "dynamic resolution:i:1", "session bpp:i:32", "disable wallpaper:i:0", "allow desktop composition:i:1", "smart sizing:i:0"] {
            assert!(rdp.contains(line), "missing {line} in\n{rdp}");
        }
    }
}
