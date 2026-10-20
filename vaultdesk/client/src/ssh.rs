//! Embedded SSH terminal (russh + vt100) and SFTP file browser (russh-sftp).

use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};

use eframe::egui::{self, text::LayoutJob, Color32, Event, FontId, Key, Modifiers, RichText, Sense, Stroke, TextFormat};
use russh::{
    client::{self, KeyboardInteractiveAuthResponse},
    keys::{HashAlg, PublicKey},
    ChannelMsg, Disconnect,
};
use russh_sftp::client::SftpSession;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::mpsc,
};
use uuid::Uuid;
use systemdeck_core::Entry;

use crate::{
    api::Api,
    sessions::{runtime, SessionView, Status},
    store::Store,
};
use systemdeck_core::api::RecordingStart;

// ---------------------------------------------------------------------------
// Connection + authentication
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct Target {
    host: String,
    port: u16,
    username: String,
    password: String,
}

impl Target {
    fn from_entry(entry: &Entry) -> Result<Self, String> {
        let host = entry.host.trim().to_string();
        if host.is_empty() {
            return Err("No host set for this entry".into());
        }
        if entry.username.trim().is_empty() {
            return Err("Embedded SSH needs a username on the entry (Edit -> Username).".into());
        }
        Ok(Self { host, port: entry.port.unwrap_or(22), username: entry.username.trim().to_string(), password: entry.password.clone() })
    }

    fn id(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }
}

/// Trust-on-first-use host key check against the local known-hosts list.
struct HostKeyCheck {
    store: Arc<Store>,
    host_id: String,
    mismatch: Arc<Mutex<Option<String>>>,
    new_key: Arc<Mutex<Option<String>>>,
}

impl client::Handler for HostKeyCheck {
    type Error = russh::Error;

    async fn check_server_key(&mut self, key: &PublicKey) -> Result<bool, Self::Error> {
        let fingerprint = key.fingerprint(HashAlg::Sha256).to_string();
        match self.store.known_host(&self.host_id) {
            Ok(Some(known)) if known == fingerprint => Ok(true),
            Ok(Some(known)) => {
                *self.mismatch.lock().unwrap() = Some(format!(
                    "HOST KEY CHANGED for {}! Expected {known}, got {fingerprint}. This can mean a reinstalled server \
                     or a man-in-the-middle attack. Connection refused.",
                    self.host_id
                ));
                Ok(false)
            }
            _ => {
                let _ = self.store.set_known_host(&self.host_id, &fingerprint);
                *self.new_key.lock().unwrap() = Some(fingerprint);
                Ok(true)
            }
        }
    }
}

/// Connects and authenticates. Returns the handle and an optional note
/// (e.g. "new host key trusted").
async fn connect(target: &Target, store: Arc<Store>) -> Result<(client::Handle<HostKeyCheck>, Option<String>), String> {
    let config = Arc::new(client::Config {
        inactivity_timeout: None,
        keepalive_interval: Some(Duration::from_secs(30)),
        ..Default::default()
    });
    let mismatch = Arc::new(Mutex::new(None));
    let new_key = Arc::new(Mutex::new(None));
    let handler = HostKeyCheck { store, host_id: target.id(), mismatch: mismatch.clone(), new_key: new_key.clone() };

    let mut handle = match tokio::time::timeout(Duration::from_secs(15), client::connect(config, (target.host.as_str(), target.port), handler)).await {
        Err(_) => return Err(format!("Timed out connecting to {}", target.id())),
        Ok(Err(e)) => {
            if let Some(msg) = mismatch.lock().unwrap().take() {
                return Err(msg);
            }
            return Err(format!("Cannot connect to {}: {e}", target.id()));
        }
        Ok(Ok(handle)) => handle,
    };

    let mut ok = handle
        .authenticate_password(target.username.clone(), target.password.clone())
        .await
        .map_err(|e| format!("Authentication error: {e}"))?
        .success();

    // Many network devices only offer keyboard-interactive: answer every prompt with the password.
    if !ok && !target.password.is_empty() {
        if let Ok(mut response) = handle.authenticate_keyboard_interactive_start(target.username.clone(), None::<String>).await {
            for _ in 0..3 {
                match response {
                    KeyboardInteractiveAuthResponse::Success => {
                        ok = true;
                        break;
                    }
                    KeyboardInteractiveAuthResponse::Failure { .. } => break,
                    KeyboardInteractiveAuthResponse::InfoRequest { ref prompts, .. } => {
                        let answers = prompts.iter().map(|_| target.password.clone()).collect();
                        match handle.authenticate_keyboard_interactive_respond(answers).await {
                            Ok(next) => response = next,
                            Err(_) => break,
                        }
                    }
                }
            }
        }
    }
    if !ok {
        return Err(format!("Authentication failed for {}@{} - check the username and password", target.username, target.host));
    }
    let note = new_key.lock().unwrap().take().map(|fp| format!("New host key trusted: {fp}"));
    Ok((handle, note))
}

// ---------------------------------------------------------------------------
// Session recording (asciicast v2: header line + [time, "o"|"i"|"r", data] lines)
// ---------------------------------------------------------------------------

/// How a session is recorded. `upload` = server vault + signed in.
pub struct RecordPlan {
    pub upload: Option<(Api, RecordingStart)>,
    pub local_dir: PathBuf,
    pub file_stem: String,
}

enum RecMsg {
    Chunk(Vec<u8>),
    Finish,
}

/// Collects events on the session task; a thread uploads them (blocking HTTP).
pub(crate) struct Recorder {
    start: std::time::Instant,
    buf: String,
    tx: std::sync::mpsc::Sender<RecMsg>,
}

impl Recorder {
    const FLUSH_BYTES: usize = 32 * 1024;

    /// Terminal recording (asciicast v2).
    pub(crate) fn new(plan: RecordPlan, cols: u16, rows: u16, title: &str, status: Arc<Mutex<String>>) -> Self {
        let header = serde_json::json!({
            "version": 2,
            "width": cols,
            "height": rows,
            "timestamp": chrono::Utc::now().timestamp(),
            "title": title,
            "env": { "TERM": "xterm-256color" },
        });
        Self::with_header(plan, header, "cast", status)
    }

    /// RDP/VNC recording: same line layout, events "f" (JPEG, base64), "i", "k", "m".
    pub(crate) fn screen(plan: RecordPlan, width: usize, height: usize, title: &str, status: Arc<Mutex<String>>) -> Self {
        let header = serde_json::json!({
            "version": 2,
            "kind": "screen",
            "width": width,
            "height": height,
            "timestamp": chrono::Utc::now().timestamp(),
            "title": title,
        });
        Self::with_header(plan, header, "sdrec", status)
    }

    /// Embedded browser recording: a header line, then raw rrweb event JSON per line.
    pub(crate) fn dom(plan: RecordPlan, title: &str, status: Arc<Mutex<String>>) -> Self {
        let header = serde_json::json!({
            "version": 2,
            "kind": "dom",
            "timestamp": chrono::Utc::now().timestamp(),
            "title": title,
        });
        Self::with_header(plan, header, "sdweb", status)
    }

    /// Appends one already-formatted JSON line (used by the browser recorder).
    pub(crate) fn push_json(&mut self, line: &str) {
        self.buf.push_str(line);
        self.buf.push('\n');
        if self.buf.len() >= Self::FLUSH_BYTES {
            self.flush();
        }
    }

    fn with_header(plan: RecordPlan, header: serde_json::Value, ext: &'static str, status: Arc<Mutex<String>>) -> Self {
        let (tx, rx) = std::sync::mpsc::channel::<RecMsg>();
        std::thread::Builder::new()
            .name("systemdeck-recording".into())
            .spawn(move || upload_loop(plan, ext, rx, status))
            .expect("starting recording thread");
        let _ = tx.send(RecMsg::Chunk(format!("{header}\n").into_bytes()));
        Self { start: std::time::Instant::now(), buf: String::new(), tx }
    }

    pub(crate) fn event(&mut self, kind: &str, data: &[u8]) {
        // Fixed-point seconds, as asciinema writes them.
        let t = self.start.elapsed().as_secs_f64();
        let line = format!("[{t:.6},{},{}]", serde_json::Value::from(kind), serde_json::Value::from(String::from_utf8_lossy(data)));
        self.buf.push_str(&line);
        self.buf.push('\n');
        if self.buf.len() >= Self::FLUSH_BYTES {
            self.flush();
        }
    }

    pub(crate) fn flush(&mut self) {
        if !self.buf.is_empty() {
            let _ = self.tx.send(RecMsg::Chunk(std::mem::take(&mut self.buf).into_bytes()));
        }
    }

    pub(crate) fn finish(mut self) {
        self.flush();
        let _ = self.tx.send(RecMsg::Finish);
    }
}

/// Runs on its own thread: sends chunks to the server, falling back to a
/// local `.cast` file if the server cannot be reached.
fn upload_loop(plan: RecordPlan, ext: &str, rx: std::sync::mpsc::Receiver<RecMsg>, status: Arc<Mutex<String>>) {
    use std::io::Write as _;
    let set = |s: String| *status.lock().unwrap() = s;
    let mut header: Option<Vec<u8>> = None;
    let mut local: Option<std::fs::File> = None;
    let local_path = plan.local_dir.join(format!("{}.{ext}", plan.file_stem));
    let open_local = |header: &Option<Vec<u8>>| -> Option<std::fs::File> {
        std::fs::create_dir_all(&plan.local_dir).ok()?;
        let mut f = std::fs::OpenOptions::new().create(true).append(true).open(&local_path).ok()?;
        if let Some(h) = header {
            let _ = f.write_all(h);
        }
        Some(f)
    };

    let mut server: Option<(Api, Uuid)> = match plan.upload {
        Some((api, start)) => match api.recording_start(&start) {
            Ok(r) => {
                set("REC - saving to server".into());
                Some((api, r.id))
            }
            Err(e) => {
                set(format!("REC - server refused ({e}); saving on this PC"));
                None
            }
        },
        None => {
            set(format!("REC - saving to {}", local_path.display()));
            None
        }
    };

    for msg in rx {
        match msg {
            RecMsg::Chunk(bytes) => {
                let first = header.is_none();
                if first {
                    header = Some(bytes.clone());
                }
                if let Some((api, id)) = &server {
                    match api.recording_chunk(*id, bytes.clone()) {
                        Ok(()) => continue,
                        Err(e) => {
                            set(format!("REC - upload failed ({e}); continuing on this PC"));
                            server = None;
                        }
                    }
                }
                if local.is_none() {
                    // The header was already written by open_local when this is the first chunk.
                    local = open_local(&header);
                    if first {
                        continue;
                    }
                }
                if let Some(f) = &mut local {
                    let _ = f.write_all(&bytes);
                }
            }
            RecMsg::Finish => break,
        }
    }
    if let Some((api, id)) = server {
        let _ = api.recording_finish(id);
    }
}

fn file_stem(name: &str) -> String {
    let safe: String = name.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).collect();
    format!("{}_{}", chrono::Local::now().format("%Y%m%d-%H%M%S"), safe.trim_matches('_'))
}

impl RecordPlan {
    pub fn new(upload: Option<(Api, RecordingStart)>, entry_name: &str) -> Self {
        Self { upload, local_dir: crate::store::recordings_dir(), file_stem: file_stem(entry_name) }
    }
}

// ---------------------------------------------------------------------------
// Terminal
// ---------------------------------------------------------------------------

enum TermInput {
    Data(Vec<u8>),
    Resize(u16, u16),
}

struct TermShared {
    status: Mutex<Status>,
    parser: Mutex<vt100::Parser>,
    note: Mutex<Option<String>>,
}

pub struct TermSession {
    /// Recording state shown in the toolbar (None = not recording).
    rec_status: Option<Arc<Mutex<String>>>,
    id: Uuid,
    entry_id: Uuid,
    title: String,
    shared: Arc<TermShared>,
    tx: mpsc::UnboundedSender<TermInput>,
    size: (u16, u16),
    scroll: usize,
    focus_requested: bool,
}

const SCROLLBACK: usize = 5000;
const TERM_BG: Color32 = Color32::from_rgb(16, 18, 22);
const TERM_FG: Color32 = Color32::from_rgb(220, 220, 220);

impl TermSession {
    pub fn connect(entry: &Entry, store: Arc<Store>, ctx: &egui::Context, record: Option<RecordPlan>) -> Result<Self, String> {
        let target = Target::from_entry(entry)?;
        let rec_status = record.as_ref().map(|_| Arc::new(Mutex::new("REC - starting".to_string())));
        let rec_status_task = rec_status.clone();
        let title = format!("{}@{} - {}", target.username, target.host, entry.name);
        let shared = Arc::new(TermShared {
            status: Mutex::new(Status::Connecting),
            parser: Mutex::new(vt100::Parser::new(24, 80, SCROLLBACK)),
            note: Mutex::new(None),
        });
        let (tx, mut rx) = mpsc::unbounded_channel::<TermInput>();
        let (state, ctx) = (shared.clone(), ctx.clone());

        runtime().spawn(async move {
            let mut recorder: Option<Recorder> = None;
            let result: Result<String, String> = async {
                let (handle, note) = connect(&target, store).await?;
                *state.note.lock().unwrap() = note;
                let mut channel = handle.channel_open_session().await.map_err(|e| format!("Cannot open session: {e}"))?;
                let (rows, cols) = state.parser.lock().unwrap().screen().size();
                channel
                    .request_pty(false, "xterm-256color", u32::from(cols), u32::from(rows), 0, 0, &[])
                    .await
                    .map_err(|e| format!("PTY request failed: {e}"))?;
                channel.request_shell(false).await.map_err(|e| format!("Shell request failed: {e}"))?;
                *state.status.lock().unwrap() = Status::Connected;
                ctx.request_repaint();

                // Recording starts once the shell is up.
                if let (Some(plan), Some(status)) = (record, rec_status_task) {
                    recorder = Some(Recorder::new(plan, cols, rows, &title, status));
                }
                let mut flush_tick = tokio::time::interval(Duration::from_secs(3));

                loop {
                    tokio::select! {
                        msg = channel.wait() => match msg {
                            Some(ChannelMsg::Data { data }) | Some(ChannelMsg::ExtendedData { data, .. }) => {
                                state.parser.lock().unwrap().process(&data);
                                if let Some(r) = &mut recorder {
                                    r.event("o", &data);
                                }
                                ctx.request_repaint();
                            }
                            Some(ChannelMsg::Eof) | Some(ChannelMsg::Close) | None => {
                                return Ok("Session closed by the server".to_string());
                            }
                            _ => {}
                        },
                        input = rx.recv() => match input {
                            Some(TermInput::Data(bytes)) => {
                                if let Some(r) = &mut recorder {
                                    r.event("i", &bytes);
                                }
                                channel.data(&bytes[..]).await.map_err(|e| format!("Send failed: {e}"))?;
                            }
                            Some(TermInput::Resize(rows, cols)) => {
                                if let Some(r) = &mut recorder {
                                    r.event("r", format!("{cols}x{rows}").as_bytes());
                                }
                                let _ = channel.window_change(u32::from(cols), u32::from(rows), 0, 0).await;
                            }
                            None => {
                                let _ = channel.eof().await;
                                let _ = handle.disconnect(Disconnect::ByApplication, "", "en").await;
                                return Ok("Disconnected".to_string());
                            }
                        },
                        _ = flush_tick.tick() => {
                            if let Some(r) = &mut recorder {
                                r.flush();
                            }
                        }
                    }
                }
            }
            .await;
            if let Some(r) = recorder.take() {
                r.finish();
            }
            *state.status.lock().unwrap() = match result {
                Ok(msg) => Status::Closed(msg),
                Err(msg) => Status::Failed(msg),
            };
            ctx.request_repaint();
        });

        Ok(Self {
            rec_status,
            id: Uuid::new_v4(),
            entry_id: entry.id,
            title: entry.name.clone(),
            shared,
            tx,
            size: (24, 80),
            scroll: 0,
            focus_requested: false,
        })
    }

    fn send(&mut self, bytes: Vec<u8>) {
        if !bytes.is_empty() {
            self.scroll = 0;
            let _ = self.tx.send(TermInput::Data(bytes));
        }
    }
}

impl SessionView for TermSession {
    fn id(&self) -> Uuid {
        self.id
    }
    fn entry_id(&self) -> Uuid {
        self.entry_id
    }
    fn title(&self) -> String {
        self.title.clone()
    }
    fn badge(&self) -> &'static str {
        "SSH"
    }
    fn status(&self) -> Status {
        self.shared.status.lock().unwrap().clone()
    }
    fn note(&self) -> Option<String> {
        self.shared.note.lock().unwrap().clone()
    }
    fn ai_context(&self) -> String {
        // The visible terminal screen, as plain text, for the assistant.
        self.shared.parser.lock().unwrap().screen().contents()
    }
    fn insert_text(&mut self, text: &str) -> bool {
        if !self.status().is_live() {
            return false;
        }
        // Type the suggestion at the prompt without sending a newline, so the
        // operator can review it and press Enter themselves.
        let clean: String = text.replace(['\r', '\n'], " ");
        self.send(clean.into_bytes());
        true
    }

    fn toolbar(&mut self, ui: &mut egui::Ui) {
        match &self.rec_status {
            Some(status) => {
                let text = status.lock().unwrap().clone();
                let blink = (ui.input(|i| i.time) * 1.5).fract() < 0.6;
                let (r, _) = ui.allocate_exact_size(egui::vec2(12.0, 12.0), Sense::hover());
                if blink {
                    ui.painter().circle_filled(r.center(), 5.0, Color32::from_rgb(232, 64, 72));
                }
                ui.label(RichText::new(text).color(Color32::from_rgb(232, 84, 92)).strong())
                    .on_hover_text("Keystrokes and screen output of this session are being recorded");
                ui.ctx().request_repaint_after(Duration::from_millis(300));
            }
            None => {
                ui.label(RichText::new("Not recorded").weak());
            }
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui) {
        let rect = ui.available_rect_before_wrap();
        let response = ui.allocate_rect(rect, Sense::click_and_drag());
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 0.0, TERM_BG);

        let font = FontId::monospace(13.0);
        let (cell_w, cell_h) = ui.fonts(|f| (f.glyph_width(&font, 'M'), f.row_height(&font)));
        let inner = rect.shrink(6.0);
        let cols = ((inner.width() / cell_w) as u16).clamp(20, 500);
        let rows = ((inner.height() / cell_h) as u16).clamp(5, 300);
        if (rows, cols) != self.size {
            self.size = (rows, cols);
            self.shared.parser.lock().unwrap().screen_mut().set_size(rows, cols);
            let _ = self.tx.send(TermInput::Resize(rows, cols));
        }

        let live = self.status().is_live();
        if !self.focus_requested || response.clicked() || response.drag_started() {
            response.request_focus();
            self.focus_requested = true;
        }
        let focused = response.has_focus();
        if focused {
            ui.memory_mut(|m| {
                m.set_focus_lock_filter(
                    response.id,
                    egui::EventFilter { tab: true, horizontal_arrows: true, vertical_arrows: true, escape: true },
                )
            });
        }

        // Input
        let (events, modifiers, scroll_delta) = ui.input(|i| (i.events.clone(), i.modifiers, i.raw_scroll_delta.y));
        if response.hovered() && scroll_delta != 0.0 {
            let lines = (scroll_delta / cell_h).round() as i64;
            self.scroll = (self.scroll as i64 + lines).clamp(0, SCROLLBACK as i64) as usize;
        }
        if focused && live {
            let (app_cursor, bracketed) = {
                let parser = self.shared.parser.lock().unwrap();
                (parser.screen().application_cursor(), parser.screen().bracketed_paste())
            };
            let mut out = Vec::new();
            for event in events {
                match event {
                    Event::Text(text) => {
                        if modifiers.alt {
                            out.push(0x1b);
                        }
                        out.extend_from_slice(text.as_bytes());
                    }
                    Event::Key { key, pressed: true, modifiers, .. } => {
                        if let Some(seq) = key_bytes(key, modifiers, app_cursor) {
                            out.extend(seq);
                        }
                    }
                    Event::Copy => out.push(0x03),
                    Event::Cut => out.push(0x18),
                    Event::Paste(text) => {
                        let text = text.replace("\r\n", "\r").replace('\n', "\r");
                        if bracketed {
                            out.extend_from_slice(b"\x1b[200~");
                            out.extend_from_slice(text.as_bytes());
                            out.extend_from_slice(b"\x1b[201~");
                        } else {
                            out.extend_from_slice(text.as_bytes());
                        }
                    }
                    _ => {}
                }
            }
            self.send(out);
        }

        // Render
        let mut parser = self.shared.parser.lock().unwrap();
        parser.screen_mut().set_scrollback(self.scroll);
        self.scroll = parser.screen().scrollback();
        let screen = parser.screen();
        let (screen_rows, screen_cols) = screen.size();
        for row in 0..screen_rows {
            let y = inner.min.y + f32::from(row) * cell_h;
            let mut job = LayoutJob::default();
            for col in 0..screen_cols {
                let Some(cell) = screen.cell(row, col) else { continue };
                if cell.is_wide_continuation() {
                    continue;
                }
                let mut fg = term_color(cell.fgcolor(), TERM_FG, cell.bold());
                let mut bg = term_color(cell.bgcolor(), TERM_BG, false);
                if cell.inverse() {
                    std::mem::swap(&mut fg, &mut bg);
                }
                if bg != TERM_BG {
                    let width = if cell.is_wide() { 2.0 } else { 1.0 } * cell_w;
                    let x = inner.min.x + f32::from(col) * cell_w;
                    painter.rect_filled(egui::Rect::from_min_size(egui::pos2(x, y), egui::vec2(width, cell_h)), 0.0, bg);
                }
                let text = if cell.has_contents() { cell.contents() } else { " " };
                let underline = if cell.underline() { Stroke::new(1.0_f32, fg) } else { Stroke::NONE };
                job.append(text, 0.0, TextFormat { font_id: font.clone(), color: fg, underline, ..Default::default() });
            }
            let galley = ui.fonts(|f| f.layout_job(job));
            painter.galley(egui::pos2(inner.min.x, y), galley, TERM_FG);
        }

        if live && self.scroll == 0 && !screen.hide_cursor() {
            let (crow, ccol) = screen.cursor_position();
            let cursor = egui::Rect::from_min_size(
                egui::pos2(inner.min.x + f32::from(ccol) * cell_w, inner.min.y + f32::from(crow) * cell_h),
                egui::vec2(cell_w, cell_h),
            );
            if focused {
                painter.rect_filled(cursor, 0.0, Color32::from_rgba_unmultiplied(220, 220, 220, 140));
            } else {
                painter.rect_stroke(cursor, 0.0, Stroke::new(1.0_f32, TERM_FG), egui::StrokeKind::Inside);
            }
        }
        if self.scroll > 0 {
            painter.text(
                rect.right_top() + egui::vec2(-10.0, 8.0),
                egui::Align2::RIGHT_TOP,
                format!("scrollback -{} lines", self.scroll),
                FontId::proportional(12.0),
                Color32::from_rgb(230, 160, 40),
            );
        }
        let contents = screen.contents();
        drop(parser);

        response.context_menu(|ui| {
            if ui.button("Copy screen text").clicked() {
                ui.ctx().copy_text(contents.clone());
                ui.close_menu();
            }
            if ui.button("Clear scrollback").clicked() {
                self.shared.parser.lock().unwrap().screen_mut().set_scrollback(0);
                self.scroll = 0;
                ui.close_menu();
            }
        });
    }

    fn close(&mut self) {
        let (tx, _) = mpsc::unbounded_channel();
        self.tx = tx; // dropping the old sender ends the session task
    }
}

impl Drop for TermSession {
    fn drop(&mut self) {
        self.close();
    }
}

/// Bytes a terminal sends for a key press (non-printing keys and Ctrl/Alt combos).
fn key_bytes(key: Key, mods: Modifiers, app_cursor: bool) -> Option<Vec<u8>> {
    let csi = |s: &str| Some(format!("\x1b[{s}").into_bytes());
    let cursor = |c: char| Some(if app_cursor { format!("\x1bO{c}") } else { format!("\x1b[{c}") }.into_bytes());
    match key {
        Key::Enter => Some(b"\r".to_vec()),
        Key::Backspace => Some(vec![0x7f]),
        Key::Tab if mods.shift => csi("Z"),
        Key::Tab => Some(b"\t".to_vec()),
        Key::Escape => Some(vec![0x1b]),
        Key::ArrowUp => cursor('A'),
        Key::ArrowDown => cursor('B'),
        Key::ArrowRight => cursor('C'),
        Key::ArrowLeft => cursor('D'),
        Key::Home => csi("H"),
        Key::End => csi("F"),
        Key::Insert => csi("2~"),
        Key::Delete => csi("3~"),
        Key::PageUp => csi("5~"),
        Key::PageDown => csi("6~"),
        Key::F1 => Some(b"\x1bOP".to_vec()),
        Key::F2 => Some(b"\x1bOQ".to_vec()),
        Key::F3 => Some(b"\x1bOR".to_vec()),
        Key::F4 => Some(b"\x1bOS".to_vec()),
        Key::F5 => csi("15~"),
        Key::F6 => csi("17~"),
        Key::F7 => csi("18~"),
        Key::F8 => csi("19~"),
        Key::F9 => csi("20~"),
        Key::F10 => csi("21~"),
        Key::F11 => csi("23~"),
        Key::F12 => csi("24~"),
        _ if mods.ctrl && !mods.alt => {
            let name = key.name();
            let c = name.chars().next()?;
            match (name.len(), c) {
                (1, 'A'..='Z') => Some(vec![(c as u8) & 0x1f]),
                _ => match key {
                    Key::Space => Some(vec![0]),
                    Key::OpenBracket => Some(vec![0x1b]),
                    Key::Backslash => Some(vec![0x1c]),
                    Key::CloseBracket => Some(vec![0x1d]),
                    _ => None,
                },
            }
        }
        _ if mods.alt && !mods.ctrl => {
            // Alt+letter arrives without a Text event on Windows.
            let name = key.name();
            let c = name.chars().next()?;
            (name.len() == 1 && c.is_ascii_alphanumeric()).then(|| vec![0x1b, c.to_ascii_lowercase() as u8])
        }
        _ => None,
    }
}

fn term_color(color: vt100::Color, default: Color32, bold: bool) -> Color32 {
    const BASE: [(u8, u8, u8); 16] = [
        (0, 0, 0),
        (205, 49, 49),
        (13, 188, 121),
        (229, 229, 16),
        (36, 114, 200),
        (188, 63, 188),
        (17, 168, 205),
        (229, 229, 229),
        (102, 102, 102),
        (241, 76, 76),
        (35, 209, 139),
        (245, 245, 67),
        (59, 142, 234),
        (214, 112, 214),
        (41, 184, 219),
        (255, 255, 255),
    ];
    match color {
        vt100::Color::Default => default,
        vt100::Color::Rgb(r, g, b) => Color32::from_rgb(r, g, b),
        vt100::Color::Idx(i) => {
            let i = if bold && i < 8 { i + 8 } else { i };
            match i {
                0..=15 => {
                    let (r, g, b) = BASE[usize::from(i)];
                    Color32::from_rgb(r, g, b)
                }
                16..=231 => {
                    let i = i - 16;
                    let level = |v: u8| if v == 0 { 0 } else { 55 + v * 40 };
                    Color32::from_rgb(level(i / 36), level((i / 6) % 6), level(i % 6))
                }
                _ => {
                    let v = 8 + (i - 232) * 10;
                    Color32::from_rgb(v, v, v)
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// SFTP file browser
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct FileItem {
    name: String,
    is_dir: bool,
    size: u64,
}

enum FsCmd {
    List(String),
    Download { remote: String, local: PathBuf },
    Upload { local: PathBuf, remote: String },
    Mkdir(String),
    Delete { path: String, is_dir: bool },
}

struct FsShared {
    status: Mutex<Status>,
    cwd: Mutex<String>,
    items: Mutex<Vec<FileItem>>,
    busy: Mutex<Option<String>>,
    message: Mutex<Option<(bool, String)>>,
    local_dirty: Mutex<bool>,
    note: Mutex<Option<String>>,
}

pub struct FileSession {
    id: Uuid,
    entry_id: Uuid,
    title: String,
    host: String,
    shared: Arc<FsShared>,
    tx: mpsc::UnboundedSender<FsCmd>,
    remote_path_edit: String,
    remote_selected: Option<String>,
    local_dir: PathBuf,
    local_path_edit: String,
    local_items: Vec<FileItem>,
    local_selected: Option<String>,
    new_folder: String,
    confirm_delete: bool,
}

fn remote_join(dir: &str, name: &str) -> String {
    if dir.ends_with('/') { format!("{dir}{name}") } else { format!("{dir}/{name}") }
}

fn remote_parent(dir: &str) -> String {
    match dir.trim_end_matches('/').rsplit_once('/') {
        Some(("", _)) | None => "/".into(),
        Some((parent, _)) => parent.into(),
    }
}

fn human_size(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = n as f64;
    let mut unit = 0;
    while v >= 1024.0 && unit < UNITS.len() - 1 {
        v /= 1024.0;
        unit += 1;
    }
    if unit == 0 { format!("{n} B") } else { format!("{v:.1} {}", UNITS[unit]) }
}

impl FileSession {
    pub fn connect(entry: &Entry, store: Arc<Store>, ctx: &egui::Context) -> Result<Self, String> {
        let target = Target::from_entry(entry)?;
        let shared = Arc::new(FsShared {
            status: Mutex::new(Status::Connecting),
            cwd: Mutex::new(String::new()),
            items: Mutex::new(Vec::new()),
            busy: Mutex::new(Some("Connecting...".into())),
            message: Mutex::new(None),
            local_dirty: Mutex::new(false),
            note: Mutex::new(None),
        });
        let host = target.host.clone();
        let (tx, mut rx) = mpsc::unbounded_channel::<FsCmd>();
        let (state, ctx2) = (shared.clone(), ctx.clone());

        runtime().spawn(async move {
            let result: Result<String, String> = async {
                let (handle, note) = connect(&target, store).await?;
                *state.note.lock().unwrap() = note;
                let channel = handle.channel_open_session().await.map_err(|e| format!("Cannot open session: {e}"))?;
                channel.request_subsystem(true, "sftp").await.map_err(|e| format!("SFTP not available: {e}"))?;
                let sftp = SftpSession::new(channel.into_stream()).await.map_err(|e| format!("SFTP start failed: {e}"))?;
                let home = sftp.canonicalize(".").await.unwrap_or_else(|_| "/".into());
                *state.status.lock().unwrap() = Status::Connected;
                list(&sftp, &state, &home).await;
                *state.busy.lock().unwrap() = None;
                ctx2.request_repaint();

                while let Some(cmd) = rx.recv().await {
                    let outcome = run_cmd(&sftp, &state, &ctx2, cmd).await;
                    *state.busy.lock().unwrap() = None;
                    if let Err(e) = outcome {
                        *state.message.lock().unwrap() = Some((true, e));
                    }
                    let cwd = state.cwd.lock().unwrap().clone();
                    list(&sftp, &state, &cwd).await;
                    ctx2.request_repaint();
                }
                let _ = sftp.close().await;
                let _ = handle.disconnect(Disconnect::ByApplication, "", "en").await;
                Ok("Disconnected".to_string())
            }
            .await;
            *state.busy.lock().unwrap() = None;
            *state.status.lock().unwrap() = match result {
                Ok(msg) => Status::Closed(msg),
                Err(msg) => Status::Failed(msg),
            };
            ctx2.request_repaint();
        });

        let local_dir = dirs::download_dir().or_else(dirs::home_dir).unwrap_or_else(|| PathBuf::from("C:\\"));
        let mut session = Self {
            id: Uuid::new_v4(),
            entry_id: entry.id,
            title: entry.name.clone(),
            host,
            shared,
            tx,
            remote_path_edit: String::new(),
            remote_selected: None,
            local_path_edit: local_dir.display().to_string(),
            local_dir,
            local_items: Vec::new(),
            local_selected: None,
            new_folder: String::new(),
            confirm_delete: false,
        };
        session.reload_local();
        Ok(session)
    }

    fn reload_local(&mut self) {
        let mut items = Vec::new();
        match std::fs::read_dir(&self.local_dir) {
            Ok(rd) => {
                for e in rd.flatten() {
                    let meta = e.metadata().ok();
                    items.push(FileItem {
                        name: e.file_name().to_string_lossy().into_owned(),
                        is_dir: meta.as_ref().is_some_and(|m| m.is_dir()),
                        size: meta.map_or(0, |m| m.len()),
                    });
                }
            }
            Err(e) => *self.shared.message.lock().unwrap() = Some((true, format!("{}: {e}", self.local_dir.display()))),
        }
        items.sort_by_key(|i| (!i.is_dir, i.name.to_lowercase()));
        self.local_items = items;
        self.local_path_edit = self.local_dir.display().to_string();
        self.local_selected = None;
    }

    fn set_local_dir(&mut self, dir: PathBuf) {
        if dir.is_dir() {
            self.local_dir = dir;
        }
        self.reload_local();
    }

    fn cmd(&self, cmd: FsCmd, busy: String) {
        *self.shared.busy.lock().unwrap() = Some(busy);
        *self.shared.message.lock().unwrap() = None;
        let _ = self.tx.send(cmd);
    }
}

async fn list(sftp: &SftpSession, state: &FsShared, dir: &str) {
    match sftp.read_dir(dir).await {
        Ok(rd) => {
            let mut items: Vec<FileItem> = rd
                .filter(|e| e.file_name() != "." && e.file_name() != "..")
                .map(|e| {
                    let meta = e.metadata();
                    FileItem { name: e.file_name(), is_dir: meta.is_dir(), size: meta.len() }
                })
                .collect();
            items.sort_by_key(|i| (!i.is_dir, i.name.to_lowercase()));
            *state.items.lock().unwrap() = items;
            *state.cwd.lock().unwrap() = dir.to_string();
        }
        Err(e) => *state.message.lock().unwrap() = Some((true, format!("Cannot list {dir}: {e}"))),
    }
}

async fn run_cmd(sftp: &SftpSession, state: &FsShared, ctx: &egui::Context, cmd: FsCmd) -> Result<(), String> {
    const CHUNK: usize = 256 * 1024;
    match cmd {
        FsCmd::List(dir) => {
            *state.cwd.lock().unwrap() = dir;
        }
        FsCmd::Mkdir(path) => {
            sftp.create_dir(path.clone()).await.map_err(|e| format!("Cannot create {path}: {e}"))?;
            *state.message.lock().unwrap() = Some((false, format!("Created {path}")));
        }
        FsCmd::Delete { path, is_dir } => {
            let r = if is_dir { sftp.remove_dir(path.clone()).await } else { sftp.remove_file(path.clone()).await };
            r.map_err(|e| format!("Cannot delete {path}: {e}{}", if is_dir { " (folders must be empty)" } else { "" }))?;
            *state.message.lock().unwrap() = Some((false, format!("Deleted {path}")));
        }
        FsCmd::Download { remote, local } => {
            let total = sftp.metadata(remote.clone()).await.map(|m| m.len()).unwrap_or(0);
            let mut src = sftp.open(remote.clone()).await.map_err(|e| format!("Cannot open {remote}: {e}"))?;
            let mut dst = tokio::fs::File::create(&local).await.map_err(|e| format!("Cannot create {}: {e}", local.display()))?;
            let name = local.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let mut buf = vec![0u8; CHUNK];
            let mut done = 0u64;
            loop {
                let n = src.read(&mut buf).await.map_err(|e| format!("Read failed: {e}"))?;
                if n == 0 {
                    break;
                }
                dst.write_all(&buf[..n]).await.map_err(|e| format!("Write failed: {e}"))?;
                done += n as u64;
                *state.busy.lock().unwrap() = Some(progress("Downloading", &name, done, total));
                ctx.request_repaint();
            }
            dst.flush().await.map_err(|e| format!("Write failed: {e}"))?;
            *state.local_dirty.lock().unwrap() = true;
            *state.message.lock().unwrap() = Some((false, format!("Downloaded {name} ({}) to {}", human_size(done), local.display())));
        }
        FsCmd::Upload { local, remote } => {
            let mut src = tokio::fs::File::open(&local).await.map_err(|e| format!("Cannot open {}: {e}", local.display()))?;
            let total = src.metadata().await.map(|m| m.len()).unwrap_or(0);
            let mut dst = sftp.create(remote.clone()).await.map_err(|e| format!("Cannot create {remote}: {e}"))?;
            let name = local.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let mut buf = vec![0u8; CHUNK];
            let mut done = 0u64;
            loop {
                let n = src.read(&mut buf).await.map_err(|e| format!("Read failed: {e}"))?;
                if n == 0 {
                    break;
                }
                dst.write_all(&buf[..n]).await.map_err(|e| format!("Upload failed: {e}"))?;
                done += n as u64;
                *state.busy.lock().unwrap() = Some(progress("Uploading", &name, done, total));
                ctx.request_repaint();
            }
            dst.shutdown().await.map_err(|e| format!("Upload failed: {e}"))?;
            *state.message.lock().unwrap() = Some((false, format!("Uploaded {name} ({}) to {remote}", human_size(done))));
        }
    }
    Ok(())
}

fn progress(verb: &str, name: &str, done: u64, total: u64) -> String {
    if total > 0 {
        format!("{verb} {name}: {} / {} ({}%)", human_size(done), human_size(total), done * 100 / total)
    } else {
        format!("{verb} {name}: {}", human_size(done))
    }
}

/// One pane of the file browser. Returns (clicked item, double-clicked item).
fn file_list(ui: &mut egui::Ui, id: &str, items: &[FileItem], selected: &Option<String>) -> (Option<String>, Option<String>) {
    let (mut clicked, mut opened) = (None, None);
    egui::ScrollArea::vertical().id_salt(id).auto_shrink([false, false]).show(ui, |ui| {
        for item in items {
            let (rect, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 20.0), Sense::click());
            let visuals = ui.visuals();
            if selected.as_deref() == Some(item.name.as_str()) {
                ui.painter().rect_filled(rect, 3.0, visuals.selection.bg_fill);
            } else if resp.hovered() {
                ui.painter().rect_filled(rect, 3.0, visuals.widgets.hovered.weak_bg_fill);
            }
            let (tag, color) = if item.is_dir { ("DIR", Color32::from_rgb(214, 160, 40)) } else { ("", visuals.weak_text_color()) };
            ui.painter().text(rect.left_center() + egui::vec2(4.0, 0.0), egui::Align2::LEFT_CENTER, tag, FontId::monospace(10.0), color);
            ui.painter().text(rect.left_center() + egui::vec2(36.0, 0.0), egui::Align2::LEFT_CENTER, &item.name, FontId::proportional(13.0), visuals.text_color());
            if !item.is_dir {
                ui.painter().text(rect.right_center() - egui::vec2(6.0, 0.0), egui::Align2::RIGHT_CENTER, human_size(item.size), FontId::proportional(11.5), visuals.weak_text_color());
            }
            if resp.clicked() {
                clicked = Some(item.name.clone());
            }
            if resp.double_clicked() {
                opened = Some(item.name.clone());
            }
        }
    });
    (clicked, opened)
}

impl SessionView for FileSession {
    fn id(&self) -> Uuid {
        self.id
    }
    fn entry_id(&self) -> Uuid {
        self.entry_id
    }
    fn title(&self) -> String {
        format!("{} (files)", self.title)
    }
    fn badge(&self) -> &'static str {
        "SFTP"
    }
    fn status(&self) -> Status {
        self.shared.status.lock().unwrap().clone()
    }
    fn note(&self) -> Option<String> {
        self.shared.note.lock().unwrap().clone()
    }
    fn is_files(&self) -> bool {
        true
    }

    fn ui(&mut self, ui: &mut egui::Ui) {
        if std::mem::take(&mut *self.shared.local_dirty.lock().unwrap()) {
            self.reload_local();
        }
        let connected = self.status() == Status::Connected;
        let busy = self.shared.busy.lock().unwrap().clone();
        let idle = connected && busy.is_none();
        let cwd = self.shared.cwd.lock().unwrap().clone();
        if self.remote_path_edit.is_empty() || (!ui.memory(|m| m.has_focus(egui::Id::new(("remote_path", self.id)))) && self.remote_path_edit != cwd) {
            self.remote_path_edit = cwd.clone();
        }
        let remote_items = self.shared.items.lock().unwrap().clone();

        egui::Frame::new().inner_margin(egui::Margin::symmetric(10, 6)).show(ui, |ui| {
            ui.horizontal(|ui| {
                if let Some(b) = &busy {
                    ui.add(egui::Spinner::new());
                    ui.label(b);
                } else if let Some((is_err, msg)) = self.shared.message.lock().unwrap().clone() {
                    ui.colored_label(if is_err { Color32::from_rgb(220, 80, 80) } else { Color32::from_rgb(80, 180, 100) }, msg);
                } else if connected {
                    ui.label(RichText::new(format!("Connected to {} over SFTP", self.host)).weak());
                }
            });
        });
        ui.separator();

        let mut actions: Vec<Box<dyn FnOnce(&mut Self)>> = Vec::new();
        ui.columns(2, |cols| {
            // ---- local ----
            let ui = &mut cols[0];
            ui.horizontal(|ui| {
                ui.label(RichText::new("This PC").strong());
                if ui.small_button("Up").clicked() {
                    if let Some(parent) = self.local_dir.parent() {
                        let p = parent.to_path_buf();
                        actions.push(Box::new(move |s| s.set_local_dir(p)));
                    }
                }
                if ui.small_button("Refresh").clicked() {
                    actions.push(Box::new(|s| s.reload_local()));
                }
            });
            let resp = ui.add(egui::TextEdit::singleline(&mut self.local_path_edit).desired_width(f32::INFINITY));
            if resp.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                let p = PathBuf::from(self.local_path_edit.trim());
                actions.push(Box::new(move |s| s.set_local_dir(p)));
            }
            let upload_ok = idle && self.local_selected.as_ref().is_some_and(|n| self.local_items.iter().any(|i| &i.name == n && !i.is_dir));
            if ui.add_enabled(upload_ok, egui::Button::new("Upload selected  >>")).clicked() {
                if let Some(name) = &self.local_selected {
                    let (local, remote) = (self.local_dir.join(name), remote_join(&cwd, name));
                    actions.push(Box::new(move |s| s.cmd(FsCmd::Upload { local, remote }, "Uploading...".into())));
                }
            }
            let (clicked, opened) = file_list(ui, "local_list", &self.local_items, &self.local_selected);
            if let Some(name) = clicked {
                self.local_selected = Some(name);
            }
            if let Some(name) = opened {
                let p = self.local_dir.join(&name);
                if p.is_dir() {
                    actions.push(Box::new(move |s| s.set_local_dir(p)));
                }
            }

            // ---- remote ----
            let ui = &mut cols[1];
            ui.horizontal(|ui| {
                ui.label(RichText::new(format!("Remote: {}", self.host)).strong());
                if ui.add_enabled(idle, egui::Button::new("Up").small()).clicked() {
                    let p = remote_parent(&cwd);
                    actions.push(Box::new(move |s| s.cmd(FsCmd::List(p), "Loading...".into())));
                }
                if ui.add_enabled(idle, egui::Button::new("Refresh").small()).clicked() {
                    let p = cwd.clone();
                    actions.push(Box::new(move |s| s.cmd(FsCmd::List(p), "Loading...".into())));
                }
            });
            let resp = ui.add(egui::TextEdit::singleline(&mut self.remote_path_edit).id(egui::Id::new(("remote_path", self.id))).desired_width(f32::INFINITY));
            if resp.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) && idle {
                let p = self.remote_path_edit.trim().to_string();
                actions.push(Box::new(move |s| s.cmd(FsCmd::List(p), "Loading...".into())));
            }
            ui.horizontal(|ui| {
                let selected_file = self.remote_selected.as_ref().and_then(|n| remote_items.iter().find(|i| &i.name == n)).cloned();
                if ui.add_enabled(idle && selected_file.as_ref().is_some_and(|i| !i.is_dir), egui::Button::new("<<  Download")).clicked() {
                    if let Some(item) = &selected_file {
                        let (remote, local) = (remote_join(&cwd, &item.name), self.local_dir.join(&item.name));
                        actions.push(Box::new(move |s| s.cmd(FsCmd::Download { remote, local }, "Downloading...".into())));
                    }
                }
                if self.confirm_delete {
                    if ui.button(RichText::new("Confirm delete").color(Color32::from_rgb(220, 80, 80))).clicked() {
                        if let Some(item) = &selected_file {
                            let (path, is_dir) = (remote_join(&cwd, &item.name), item.is_dir);
                            actions.push(Box::new(move |s| s.cmd(FsCmd::Delete { path, is_dir }, "Deleting...".into())));
                        }
                        self.confirm_delete = false;
                    }
                    if ui.small_button("Cancel").clicked() {
                        self.confirm_delete = false;
                    }
                } else if ui.add_enabled(idle && selected_file.is_some(), egui::Button::new("Delete")).clicked() {
                    self.confirm_delete = true;
                }
            });
            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(&mut self.new_folder).hint_text("new folder name").desired_width(140.0));
                if ui.add_enabled(idle && !self.new_folder.trim().is_empty(), egui::Button::new("Create folder")).clicked() {
                    let path = remote_join(&cwd, self.new_folder.trim());
                    self.new_folder.clear();
                    actions.push(Box::new(move |s| s.cmd(FsCmd::Mkdir(path), "Creating folder...".into())));
                }
            });
            let (clicked, opened) = file_list(ui, "remote_list", &remote_items, &self.remote_selected);
            if let Some(name) = clicked {
                self.remote_selected = Some(name);
                self.confirm_delete = false;
            }
            if let Some(name) = opened {
                if idle && remote_items.iter().any(|i| i.name == name && i.is_dir) {
                    let p = remote_join(&cwd, &name);
                    self.remote_selected = None;
                    actions.push(Box::new(move |s| s.cmd(FsCmd::List(p), "Loading...".into())));
                }
            }
        });
        for action in actions {
            action(self);
        }
    }

    fn close(&mut self) {
        let (tx, _) = mpsc::unbounded_channel();
        self.tx = tx;
    }
}

impl Drop for FileSession {
    fn drop(&mut self) {
        self.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_paths() {
        assert_eq!(remote_join("/home/a", "x"), "/home/a/x");
        assert_eq!(remote_join("/", "x"), "/x");
        assert_eq!(remote_parent("/home/a"), "/home");
        assert_eq!(remote_parent("/home"), "/");
        assert_eq!(remote_parent("/"), "/");
    }

    #[test]
    fn terminal_keys() {
        assert_eq!(key_bytes(Key::C, Modifiers::CTRL, false), Some(vec![3]));
        assert_eq!(key_bytes(Key::ArrowUp, Modifiers::NONE, false), Some(b"\x1b[A".to_vec()));
        assert_eq!(key_bytes(Key::ArrowUp, Modifiers::NONE, true), Some(b"\x1bOA".to_vec()));
        assert_eq!(key_bytes(Key::A, Modifiers::NONE, false), None, "printable keys come via Text events");
    }

    fn wait_for(path: &std::path::Path, needle: &str) -> String {
        for _ in 0..100 {
            if let Ok(text) = std::fs::read_to_string(path) {
                if text.contains(needle) {
                    return text;
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        std::fs::read_to_string(path).unwrap_or_default()
    }

    #[test]
    fn recording_saved_locally_without_server() {
        let dir = std::env::temp_dir().join(format!("vd-rec-{}", Uuid::new_v4()));
        let plan = RecordPlan { upload: None, local_dir: dir.clone(), file_stem: "test".into() };
        let status = Arc::new(Mutex::new(String::new()));
        let mut rec = Recorder::new(plan, 120, 40, "root@host - test", status.clone());
        rec.event("i", b"ls -la\r");
        rec.event("o", b"total 0\r\n\x1b[32mok\x1b[0m\r\n");
        rec.event("r", b"100x30");
        rec.finish();
        let text = wait_for(&dir.join("test.cast"), "100x30");
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 4, "header + 3 events, header once:\n{text}");
        let header: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!((header["version"].as_i64(), header["width"].as_i64()), (Some(2), Some(120)));
        let input: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
        assert_eq!((input[1].as_str(), input[2].as_str()), (Some("i"), Some("ls -la\r")));
        assert!(lines[2].contains("\"o\"") && lines[2].contains("total 0"));
        assert!(status.lock().unwrap().contains("saving to"));
        let _ = std::fs::remove_dir_all(dir);
    }

    /// `SYSTEMDECK_TEST_URL` + `SYSTEMDECK_TEST_ADMIN_PW` against a fresh server.
    #[test]
    #[ignore = "needs a running server"]
    fn recording_uploaded_to_server() {
        use systemdeck_core::api::{RecordingMeta, VaultRequest};
        let url = std::env::var("SYSTEMDECK_TEST_URL").unwrap();
        let pw = std::env::var("SYSTEMDECK_TEST_ADMIN_PW").unwrap();
        let token = Api::new(&url, None).login("admin", &pw).unwrap().token;
        let api = Api::new(&url, Some(token.clone()));
        let vault = api.admin_create_vault(&VaultRequest { name: "Rec test".into(), record_ssh: Some(true), ..Default::default() }).unwrap();
        let start = RecordingStart { vault_id: vault.id, entry_id: Uuid::new_v4(), entry_name: "Docker".into(), host: "10.0.0.9".into(), login: "root".into(), cols: 80, rows: 24, kind: Default::default(), protocol: "SSH".into() };
        let dir = std::env::temp_dir().join(format!("vd-rec-{}", Uuid::new_v4()));
        let status = Arc::new(Mutex::new(String::new()));
        let mut rec = Recorder::new(RecordPlan { upload: Some((api.clone(), start)), local_dir: dir.clone(), file_stem: "x".into() }, 80, 24, "root@10.0.0.9", status.clone());
        rec.event("i", b"whoami\r");
        rec.event("o", b"root\r\n");
        rec.finish();

        let http = reqwest::blocking::Client::new();
        let mut meta: Option<RecordingMeta> = None;
        for _ in 0..100 {
            let list: Vec<RecordingMeta> = http.get(format!("{url}/api/admin/recordings")).bearer_auth(&token).send().unwrap().json().unwrap();
            if let Some(m) = list.into_iter().find(|m| m.vault_id == vault.id && m.finished) {
                meta = Some(m);
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        let meta = meta.expect("recording finished on the server");
        assert_eq!((meta.username.as_str(), meta.login.as_str(), meta.host.as_str()), ("admin", "root", "10.0.0.9"));
        let cast = http.get(format!("{url}/api/admin/recordings/{}", meta.id)).bearer_auth(&token).send().unwrap().text().unwrap();
        assert!(cast.lines().next().unwrap().contains("\"version\":2"));
        assert!(cast.contains("whoami") && cast.contains(r#""root\r\n""#), "{cast}");
        assert!(!dir.exists(), "nothing saved locally when the upload works");
        assert!(status.lock().unwrap().contains("server"));
        // Only the owner may append; finished recordings are closed.
        assert!(api.recording_chunk(meta.id, b"x".to_vec()).is_err());
    }

    #[test]
    fn sizes_and_colors() {
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(1536), "1.5 KB");
        assert_eq!(term_color(vt100::Color::Idx(1), TERM_FG, true), Color32::from_rgb(241, 76, 76));
        assert_eq!(term_color(vt100::Color::Idx(232), TERM_FG, false), Color32::from_rgb(8, 8, 8));
    }
}
