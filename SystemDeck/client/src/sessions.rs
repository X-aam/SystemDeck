//! Embedded RDP (IronRDP) and VNC (vnc-rs) sessions rendered inside egui.
//!
//! Each session runs on a shared Tokio runtime and publishes its framebuffer
//! into `Shared`; the UI thread uploads it to a texture when it changes and
//! forwards mouse/keyboard input back through a channel.

use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex, OnceLock,
    },
    time::{Duration, Instant},
};

use eframe::egui::{self, Color32, Event, Key, Modifiers, PointerButton, Pos2, Rect, RichText, Sense, Vec2};
use ironrdp_client::{
    config::{ConfigBuilder, Destination},
    rdp::{RdpClient, RdpInputEvent, RdpOutputEvent},
};
use ironrdp_input::{Database, MouseButton, MousePosition, Operation, Scancode, WheelRotations};
use tokio::sync::mpsc;
use uuid::Uuid;
use systemdeck_core::{ColorDepth, Entry, EntryKind, Quality, Resolution, Scaling};
use vnc::{PixelFormat, VncConnector, VncEncoding, VncEvent, X11Event};

use crate::ssh::{RecordPlan, Recorder};

pub(crate) fn runtime() -> &'static tokio::runtime::Runtime {
    static RT: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RT.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .thread_name("systemdeck-session")
            .build()
            .expect("starting session runtime")
    })
}

/// A live tab in the central area (RDP, VNC, SSH terminal, SFTP browser).
pub trait SessionView {
    fn id(&self) -> Uuid;
    fn entry_id(&self) -> Uuid;
    fn title(&self) -> String;
    fn badge(&self) -> &'static str;
    fn status(&self) -> Status;
    /// One-off information to show the user (e.g. a newly trusted host key).
    fn note(&self) -> Option<String> {
        None
    }
    fn is_files(&self) -> bool {
        false
    }
    /// Optional per-session controls shown above the session (scaling etc.).
    fn toolbar(&mut self, _ui: &mut egui::Ui) {}
    fn ui(&mut self, ui: &mut egui::Ui);
    /// Shown/hidden each frame (only the active tab is visible). Native overlay
    /// sessions (the embedded browser) use this to hide their child window.
    fn set_visible(&mut self, _visible: bool) {}
    /// Re-parent a native child window to `hwnd` (for detaching into another
    /// window). egui-rendered sessions ignore this.
    #[cfg(windows)]
    fn reparent(&mut self, _hwnd: isize) {}
    /// Recent text context for the in-session AI assistant (terminal buffer,
    /// current URL, ...). Empty when there is none to share.
    fn ai_context(&self) -> String {
        String::new()
    }
    /// Inserts assistant-suggested text into the session input without running
    /// it (no Enter). Returns false when the session can't accept typed input.
    fn insert_text(&mut self, _text: &str) -> bool {
        false
    }
    fn close(&mut self);
}

/// The SystemDeck window's Win32 handle, for parenting a WebView2 child.
#[cfg(windows)]
pub fn window_hwnd(frame: &eframe::Frame) -> Option<isize> {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    match frame.window_handle().ok()?.as_raw() {
        RawWindowHandle::Win32(h) => Some(h.hwnd.get()),
        _ => None,
    }
}

/// Finds a top-level window by exact title (for detached viewport windows,
/// whose native handle egui doesn't expose directly).
#[cfg(windows)]
pub fn find_window_by_title(title: &str) -> Option<isize> {
    use windows::core::HSTRING;
    use windows::Win32::UI::WindowsAndMessaging::FindWindowW;
    let wide = HSTRING::from(title);
    let hwnd = unsafe { FindWindowW(windows::core::PCWSTR::null(), &wide).ok()? };
    let raw = hwnd.0 as isize;
    (raw != 0).then_some(raw)
}

#[derive(Clone, PartialEq)]
pub enum Status {
    Connecting,
    Connected,
    Closed(String),
    Failed(String),
}

impl Status {
    pub fn is_live(&self) -> bool {
        matches!(self, Status::Connecting | Status::Connected)
    }
}

#[derive(Default)]
struct Frame {
    width: usize,
    height: usize,
    rgba: Vec<u8>,
    version: u64,
}

struct Shared {
    status: Mutex<Status>,
    frame: Mutex<Frame>,
}

impl Shared {
    fn set_status(&self, status: Status) {
        *self.status.lock().unwrap() = status;
    }
}

enum Input {
    Rdp(mpsc::UnboundedSender<RdpInputEvent>),
    Vnc(mpsc::UnboundedSender<X11Event>),
}

pub struct Session {
    pub id: Uuid,
    pub entry_id: Uuid,
    pub title: String,
    pub kind: EntryKind,
    shared: Arc<Shared>,
    input: Input,
    texture: Option<egui::TextureHandle>,
    seen_version: u64,
    // Input state
    rdp_db: Database,
    modifiers: Modifiers,
    vnc_buttons: u8,
    last_pointer: Option<(u16, u16)>,
    buttons_down: [bool; 3],
    // Dynamic resize (RDP)
    requested_size: (u16, u16),
    pending_size: Option<((u16, u16), Instant)>,
    // Display
    scaling: Scaling,
    smooth: bool,
    /// Fixed remote resolution: never follow the tab size.
    fixed_size: bool,
    /// Pan offset (points) for the actual-size view.
    pan: Vec2,
    /// "1920 x 1080 · 32-bit · Balanced" for the toolbar.
    describe: String,
    rec: Option<ScreenRec>,
}

/// RDP/VNC recording: a sampler thread stores the screen as JPEG (at most
/// once a second, only when it changed); the UI adds typed text and clicks.
struct ScreenRec {
    recorder: Arc<Mutex<Option<Recorder>>>,
    status: Arc<Mutex<String>>,
    stop: Arc<AtomicBool>,
}

impl ScreenRec {
    const INTERVAL: Duration = Duration::from_millis(1000);
    const MAX_WIDTH: u32 = 1280;
    const JPEG_QUALITY: u8 = 55;

    fn start(shared: Arc<Shared>, plan: RecordPlan, title: String) -> Self {
        let recorder: Arc<Mutex<Option<Recorder>>> = Arc::new(Mutex::new(None));
        let status = Arc::new(Mutex::new("REC - waiting for the screen".to_string()));
        let stop = Arc::new(AtomicBool::new(false));
        let (slot, rec_status, stopped) = (recorder.clone(), status.clone(), stop.clone());
        std::thread::Builder::new()
            .name("systemdeck-screen-rec".into())
            .spawn(move || {
                let mut plan = Some(plan);
                let mut seen = 0u64;
                let mut last_flush = Instant::now();
                loop {
                    let ended = stopped.load(Ordering::Relaxed) || !shared.status.lock().unwrap().is_live();
                    let snapshot = {
                        let frame = shared.frame.lock().unwrap();
                        (frame.version != seen && frame.width > 0).then(|| {
                            seen = frame.version;
                            (frame.width, frame.height, frame.rgba.clone())
                        })
                    };
                    if let Some((w, h, rgba)) = snapshot {
                        // Recording starts with the first frame (after sign-in).
                        if let Some(plan) = plan.take() {
                            *slot.lock().unwrap() = Some(Recorder::screen(plan, w, h, &title, rec_status.clone()));
                        }
                        if let Some(jpeg) = encode_frame(w, h, rgba) {
                            if let Some(r) = slot.lock().unwrap().as_mut() {
                                r.event("f", jpeg.as_bytes());
                            }
                        }
                    }
                    if last_flush.elapsed() >= Duration::from_secs(3) {
                        if let Some(r) = slot.lock().unwrap().as_mut() {
                            r.flush();
                        }
                        last_flush = Instant::now();
                    }
                    if ended {
                        break;
                    }
                    std::thread::sleep(Self::INTERVAL);
                }
                if let Some(r) = slot.lock().unwrap().take() {
                    r.finish();
                }
            })
            .expect("starting recording thread");
        Self { recorder, status, stop }
    }

    fn event(&self, kind: &str, data: &str) {
        if let Some(r) = self.recorder.lock().unwrap().as_mut() {
            r.event(kind, data.as_bytes());
        }
    }
}

/// RGBA frame -> downscaled JPEG, base64.
fn encode_frame(w: usize, h: usize, rgba: Vec<u8>) -> Option<String> {
    use base64::Engine as _;
    let rgb: Vec<u8> = rgba.chunks_exact(4).flat_map(|p| [p[0], p[1], p[2]]).collect();
    let mut img = image::RgbImage::from_raw(w as u32, h as u32, rgb)?;
    if img.width() > ScreenRec::MAX_WIDTH {
        let nh = (u64::from(img.height()) * u64::from(ScreenRec::MAX_WIDTH) / u64::from(img.width())).max(1) as u32;
        img = image::imageops::resize(&img, ScreenRec::MAX_WIDTH, nh, image::imageops::FilterType::Triangle);
    }
    let mut out = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, ScreenRec::JPEG_QUALITY).encode_image(&img).ok()?;
    Some(base64::engine::general_purpose::STANDARD.encode(out))
}

/// What a key press adds to the "typed" log: control characters the
/// dashboard understands (Enter, Backspace, Tab) or a named combination.
fn recorded_key(key: Key, modifiers: Modifiers) -> Option<(&'static str, String)> {
    if modifiers.ctrl || modifiers.alt {
        let name = key.name();
        if matches!(key, Key::Enter | Key::Tab | Key::Escape | Key::Delete | Key::Backspace) || name.len() == 1 || name.starts_with('F') {
            let mut parts = Vec::new();
            if modifiers.ctrl {
                parts.push("Ctrl");
            }
            if modifiers.alt {
                parts.push("Alt");
            }
            if modifiers.shift {
                parts.push("Shift");
            }
            return Some(("k", format!("{}+{name}", parts.join("+"))));
        }
        return None;
    }
    match key {
        Key::Enter => Some(("i", "\r".into())),
        Key::Backspace => Some(("i", "\u{7f}".into())),
        Key::Tab => Some(("i", "\t".into())),
        Key::Escape => Some(("k", "Esc".into())),
        Key::Delete => Some(("k", "Del".into())),
        _ => None,
    }
}

impl Session {
    pub fn status(&self) -> Status {
        self.shared.status.lock().unwrap().clone()
    }

    /// Connects to an RDP server with IronRDP. `size` is the initial desktop size in pixels.
    pub fn connect_rdp(entry: &Entry, size: (u16, u16), ctx: &egui::Context, record: Option<RecordPlan>) -> Result<Self, String> {
        let host = entry.host.trim();
        if host.is_empty() {
            return Err("No host set for this entry".into());
        }
        if entry.username.trim().is_empty() || entry.password.is_empty() {
            return Err("Embedded RDP needs a username and password on the entry (Network Level Authentication). \
                        Add them, or use \"Open externally\" to sign in with mstsc."
                .into());
        }
        let screen = entry.screen;
        let size = match screen.resolution {
            Resolution::Fixed(w, h) => (w, h),
            Resolution::Fit => even_size(size),
        };
        // IronRDP decodes 16 or 32 bpp (24-bit is sent as 32). "Fast" trades colour for bandwidth.
        let depth = if screen.color_depth == ColorDepth::Bits16 || screen.quality == Quality::Fast { 16 } else { 32 };
        let destination = Destination::new(format!("{host}:{}", entry.port.unwrap_or(3389))).map_err(|e| format!("Bad address: {e:#}"))?;
        let mut builder = ConfigBuilder::new()
            .with_destination(destination)
            .with_username(entry.username.trim())
            .with_password(entry.password.clone())
            .with_client_build(0)
            .with_client_dir("C:\\Windows\\System32\\mstscax.dll")
            .with_client_name(hostname())
            .with_platform(ironrdp_pdu::rdp::capability_sets::MajorPlatformType::WINDOWS)
            .with_desktop_width(size.0)
            .with_desktop_height(size.1)
            .with_color_depth(depth)
            .with_compression(screen.quality != Quality::Best);
        if !entry.domain.trim().is_empty() {
            builder = builder.with_domain(entry.domain.trim());
        }
        let config = builder.build().map_err(|e| format!("{e:#}"))?;

        let shared = Arc::new(Shared { status: Mutex::new(Status::Connecting), frame: Mutex::new(Frame::default()) });
        let (out_tx, mut out_rx) = mpsc::channel::<RdpOutputEvent>(8);
        let client = RdpClient::new(config, out_tx);
        let input = client.input_sender();
        // The connection future is not Send, so it gets its own single-threaded runtime.
        std::thread::Builder::new()
            .name("systemdeck-rdp".into())
            .spawn(move || match tokio::runtime::Builder::new_current_thread().enable_all().build() {
                Ok(rt) => rt.block_on(client.run()),
                Err(e) => eprintln!("cannot start RDP runtime: {e}"),
            })
            .map_err(|e| format!("Cannot start RDP thread: {e}"))?;
        let (state, ctx) = (shared.clone(), ctx.clone());
        runtime().spawn(async move {
            while let Some(event) = out_rx.recv().await {
                match event {
                    RdpOutputEvent::Image { buffer, width, height } => {
                        let (w, h) = (usize::from(width.get()), usize::from(height.get()));
                        {
                            let mut frame = state.frame.lock().unwrap();
                            frame.width = w;
                            frame.height = h;
                            frame.rgba.clear();
                            frame.rgba.reserve(w * h * 4);
                            for px in buffer.iter().take(w * h) {
                                let [_, r, g, b] = px.to_be_bytes();
                                frame.rgba.extend_from_slice(&[r, g, b, 255]);
                            }
                            frame.version += 1;
                        }
                        state.set_status(Status::Connected);
                        ctx.request_repaint();
                    }
                    RdpOutputEvent::ConnectionFailure(e) => {
                        state.set_status(Status::Failed(rdp_error_text(&format!("{e}"))));
                        ctx.request_repaint();
                    }
                    RdpOutputEvent::Terminated(result) => {
                        let msg = match result {
                            Ok(reason) => format!("Session ended: {reason}"),
                            Err(e) => rdp_error_text(&format!("{e}")),
                        };
                        let current = state.status.lock().unwrap().clone();
                        if !matches!(current, Status::Failed(_)) {
                            state.set_status(Status::Closed(msg));
                        }
                        ctx.request_repaint();
                        break;
                    }
                    _ => {}
                }
            }
        });

        let mut session = Self::new(entry, shared, Input::Rdp(input), size, record);
        session.describe = format!("{} x {}  ·  {depth}-bit  ·  {}", size.0, size.1, screen.quality.label());
        Ok(session)
    }

    /// Connects to a VNC server (ZRLE / CopyRect / Raw, VNC password auth).
    pub fn connect_vnc(entry: &Entry, ctx: &egui::Context, record: Option<RecordPlan>) -> Result<Self, String> {
        let host = entry.host.trim().to_string();
        if host.is_empty() {
            return Err("No host set for this entry".into());
        }
        let addr = format!("{host}:{}", entry.port.unwrap_or(5900));
        let password = entry.password.clone();
        let shared = Arc::new(Shared { status: Mutex::new(Status::Connecting), frame: Mutex::new(Frame::default()) });
        let (in_tx, mut in_rx) = mpsc::unbounded_channel::<X11Event>();
        let (state, ctx) = (shared.clone(), ctx.clone());

        runtime().spawn(async move {
            let result: Result<String, String> = async {
                let tcp = tokio::time::timeout(Duration::from_secs(10), tokio::net::TcpStream::connect(&addr))
                    .await
                    .map_err(|_| format!("Timed out connecting to {addr}"))?
                    .map_err(|e| format!("Cannot connect to {addr}: {e}"))?;
                let vnc = VncConnector::new(tcp)
                    .set_auth_method(async move { Ok(password) })
                    .add_encoding(VncEncoding::Zrle)
                    .add_encoding(VncEncoding::CopyRect)
                    .add_encoding(VncEncoding::Raw)
                    .add_encoding(VncEncoding::DesktopSizePseudo)
                    .allow_shared(true)
                    .set_pixel_format(PixelFormat::rgba())
                    .build()
                    .map_err(|e| format!("{e}"))?
                    .try_start()
                    .await
                    .map_err(|e| format!("VNC handshake failed: {e}"))?
                    .finish()
                    .map_err(|e| format!("{e}"))?;
                state.set_status(Status::Connected);
                ctx.request_repaint();

                let mut refresh = tokio::time::interval(Duration::from_millis(40));
                loop {
                    tokio::select! {
                        event = vnc.recv_event() => {
                            let event = event.map_err(|e| format!("{e}"))?;
                            if let VncEvent::Error(e) = &event {
                                return Err(e.clone());
                            }
                            apply_vnc_event(&state, event);
                            ctx.request_repaint();
                        }
                        input = in_rx.recv() => match input {
                            Some(ev) => { let _ = vnc.input(ev).await; }
                            None => { let _ = vnc.close().await; return Ok("Disconnected".to_string()); }
                        },
                        _ = refresh.tick() => { let _ = vnc.input(X11Event::Refresh).await; }
                    }
                }
            }
            .await;
            state.set_status(match result {
                Ok(msg) => Status::Closed(msg),
                Err(msg) => Status::Failed(msg),
            });
            ctx.request_repaint();
        });

        Ok(Self::new(entry, shared, Input::Vnc(in_tx), (0, 0), record))
    }

    fn new(entry: &Entry, shared: Arc<Shared>, input: Input, size: (u16, u16), record: Option<RecordPlan>) -> Self {
        let login = if entry.username.trim().is_empty() { String::new() } else { format!("{}@", entry.username.trim()) };
        let rec = record.map(|plan| ScreenRec::start(shared.clone(), plan, format!("{login}{} - {}", entry.host.trim(), entry.name)));
        Self {
            rec,
            id: Uuid::new_v4(),
            entry_id: entry.id,
            title: entry.name.clone(),
            kind: entry.kind,
            shared,
            input,
            texture: None,
            seen_version: 0,
            rdp_db: Database::new(),
            modifiers: Modifiers::default(),
            vnc_buttons: 0,
            last_pointer: None,
            buttons_down: [false; 3],
            requested_size: size,
            pending_size: None,
            scaling: entry.screen.scaling,
            smooth: entry.screen.smooth,
            fixed_size: matches!(entry.screen.resolution, Resolution::Fixed(..)) && entry.kind == EntryKind::Rdp,
            pan: Vec2::ZERO,
            describe: String::new(),
        }
    }

    pub fn close(&mut self) {
        if let Some(rec) = &self.rec {
            rec.stop.store(true, Ordering::Relaxed);
        }
        match &self.input {
            Input::Rdp(tx) => {
                let _ = tx.send(RdpInputEvent::Close);
            }
            Input::Vnc(_) => {
                // Dropping the sender ends the VNC task.
                let (tx, _) = mpsc::unbounded_channel();
                self.input = Input::Vnc(tx);
            }
        }
    }

    /// Draws the remote screen into the remaining space of `ui` and handles input.
    pub fn ui(&mut self, ui: &mut egui::Ui) {
        let available = ui.available_rect_before_wrap();
        let ppp = ui.ctx().pixels_per_point();
        self.maybe_resize(available, ppp);

        // Upload the newest frame.
        {
            let frame = self.shared.frame.lock().unwrap();
            if frame.version != self.seen_version && frame.width > 0 {
                let image = egui::ColorImage::from_rgba_unmultiplied([frame.width, frame.height], &frame.rgba);
                let options = if self.smooth { egui::TextureOptions::LINEAR } else { egui::TextureOptions::NEAREST };
                match &mut self.texture {
                    Some(t) => t.set(image, options),
                    None => self.texture = Some(ui.ctx().load_texture(format!("session-{}", self.id), image, options)),
                }
                self.seen_version = frame.version;
            }
        }

        let response = ui.allocate_rect(available, Sense::click_and_drag());
        ui.painter().rect_filled(available, 0.0, Color32::from_gray(12));

        let Some(texture) = &self.texture else {
            let text = match self.status() {
                Status::Connecting => "Connecting...".to_string(),
                Status::Failed(e) | Status::Closed(e) => e,
                Status::Connected => "Waiting for the first frame...".into(),
            };
            ui.painter().text(available.center(), egui::Align2::CENTER_CENTER, text, egui::FontId::proportional(16.0), Color32::LIGHT_GRAY);
            return;
        };

        let [w, h] = texture.size();
        let native = Vec2::new(w as f32 / ppp, h as f32 / ppp);
        let texture_id = texture.id();
        let image_rect = match self.scaling {
            Scaling::Fit => {
                let scale = (available.width() / native.x).min(available.height() / native.y).min(1.0);
                Rect::from_center_size(available.center(), native * scale)
            }
            Scaling::Stretch => {
                let scale = (available.width() / native.x).min(available.height() / native.y);
                Rect::from_center_size(available.center(), native * scale)
            }
            Scaling::Actual => {
                let overflow = (native - available.size()).max(Vec2::ZERO);
                self.pan = self.pan.clamp(Vec2::ZERO, overflow);
                let min = egui::pos2(
                    if overflow.x > 0.0 { available.min.x - self.pan.x } else { available.center().x - native.x / 2.0 },
                    if overflow.y > 0.0 { available.min.y - self.pan.y } else { available.center().y - native.y / 2.0 },
                );
                Rect::from_min_size(min, native)
            }
        };
        ui.painter().with_clip_rect(available).image(texture_id, image_rect, Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), Color32::WHITE);
        if self.scaling == Scaling::Actual {
            self.scrollbars(ui, available, native);
        }

        if !self.status().is_live() {
            return;
        }
        if response.clicked() || response.drag_started() {
            response.request_focus();
        }
        if response.has_focus() {
            ui.memory_mut(|m| {
                m.set_focus_lock_filter(
                    response.id,
                    egui::EventFilter { tab: true, horizontal_arrows: true, vertical_arrows: true, escape: true },
                )
            });
            ui.painter().rect_stroke(image_rect, 0.0, egui::Stroke::new(1.0_f32, Color32::from_rgb(70, 130, 220)), egui::StrokeKind::Outside);
        }

        let to_remote = |p: Pos2| -> (u16, u16) {
            let x = ((p.x - image_rect.min.x) / image_rect.width() * w as f32).clamp(0.0, w as f32 - 1.0);
            let y = ((p.y - image_rect.min.y) / image_rect.height() * h as f32).clamp(0.0, h as f32 - 1.0);
            (x as u16, y as u16)
        };

        let (events, pointer, modifiers, buttons) = ui.input(|i| {
            (
                i.events.clone(),
                i.pointer.latest_pos(),
                i.modifiers,
                [
                    i.pointer.button_down(PointerButton::Primary),
                    i.pointer.button_down(PointerButton::Middle),
                    i.pointer.button_down(PointerButton::Secondary),
                ],
            )
        });

        // Mouse: only while hovering the image, or while a drag that started on it continues.
        let pointer_active = response.hovered() || response.dragged() || self.buttons_down.iter().any(|b| *b);
        if pointer_active {
            if let Some(pos) = pointer {
                let remote = to_remote(pos);
                if self.last_pointer != Some(remote) {
                    self.last_pointer = Some(remote);
                    self.pointer_move(remote, buttons);
                }
            }
            for (idx, down) in buttons.iter().enumerate() {
                if *down != self.buttons_down[idx] && (response.hovered() || !*down) {
                    self.buttons_down[idx] = *down;
                    self.mouse_button(idx, *down);
                }
            }
            for event in &events {
                if let Event::MouseWheel { delta, unit, .. } = event {
                    let notches = match unit {
                        egui::MouseWheelUnit::Line => delta.y,
                        egui::MouseWheelUnit::Page => delta.y * 3.0,
                        egui::MouseWheelUnit::Point => delta.y / 50.0,
                    };
                    self.wheel(notches);
                }
            }
        }

        if response.has_focus() {
            self.sync_modifiers(modifiers);
            if let Some(rec) = &self.rec {
                for event in &events {
                    match event {
                        Event::Text(text) => rec.event("i", text),
                        Event::Paste(text) => rec.event("i", text),
                        Event::Key { key, pressed: true, modifiers, .. } => {
                            if let Some((kind, data)) = recorded_key(*key, *modifiers) {
                                rec.event(kind, &data);
                            }
                        }
                        Event::Copy => rec.event("k", "Ctrl+C"),
                        Event::Cut => rec.event("k", "Ctrl+X"),
                        _ => {}
                    }
                }
            }
            for event in events {
                match event {
                    Event::Key { key, physical_key, pressed, .. } => self.key(physical_key.unwrap_or(key), pressed),
                    Event::Text(text) if matches!(self.input, Input::Vnc(_)) => self.vnc_text(&text),
                    Event::Copy => self.tap(Key::C),
                    Event::Cut => self.tap(Key::X),
                    // Paste types the local clipboard into the remote session.
                    Event::Paste(text) => self.type_text(&text),
                    _ => {}
                }
            }
        } else if self.modifiers.any() {
            self.sync_modifiers(Modifiers::default());
        }
    }

    /// Thin draggable scroll bars for the actual-size view (the mouse wheel
    /// keeps scrolling inside the remote session).
    fn scrollbars(&mut self, ui: &mut egui::Ui, area: Rect, native: Vec2) {
        let overflow = (native - area.size()).max(Vec2::ZERO);
        let bar = 10.0;
        let track = Color32::from_rgba_unmultiplied(255, 255, 255, 18);
        let thumb_color = Color32::from_rgba_unmultiplied(255, 255, 255, 110);
        if overflow.x > 0.0 {
            let rect = Rect::from_min_max(egui::pos2(area.min.x, area.max.y - bar), area.max);
            let resp = ui.interact(rect, ui.id().with((self.id, "hbar")), Sense::drag());
            self.pan.x = (self.pan.x + resp.drag_delta().x * native.x / area.width()).clamp(0.0, overflow.x);
            let thumb_w = area.width() * area.width() / native.x;
            let x = area.min.x + self.pan.x / overflow.x * (area.width() - thumb_w);
            ui.painter().rect_filled(rect, 0.0, track);
            ui.painter().rect_filled(Rect::from_min_size(egui::pos2(x, rect.min.y + 2.0), egui::vec2(thumb_w, bar - 4.0)), 3.0, thumb_color);
        }
        if overflow.y > 0.0 {
            let rect = Rect::from_min_max(egui::pos2(area.max.x - bar, area.min.y), area.max);
            let resp = ui.interact(rect, ui.id().with((self.id, "vbar")), Sense::drag());
            self.pan.y = (self.pan.y + resp.drag_delta().y * native.y / area.height()).clamp(0.0, overflow.y);
            let thumb_h = area.height() * area.height() / native.y;
            let y = area.min.y + self.pan.y / overflow.y * (area.height() - thumb_h);
            ui.painter().rect_filled(rect, 0.0, track);
            ui.painter().rect_filled(Rect::from_min_size(egui::pos2(rect.min.x + 2.0, y), egui::vec2(bar - 4.0, thumb_h)), 3.0, thumb_color);
        }
    }

    fn display_toolbar(&mut self, ui: &mut egui::Ui) {
        if let Some(rec) = &self.rec {
            let text = rec.status.lock().unwrap().clone();
            let blink = (ui.input(|i| i.time) * 1.5).fract() < 0.6;
            let (r, _) = ui.allocate_exact_size(egui::vec2(12.0, 12.0), Sense::hover());
            if blink {
                ui.painter().circle_filled(r.center(), 5.0, Color32::from_rgb(232, 64, 72));
            }
            ui.label(RichText::new(text).color(Color32::from_rgb(232, 84, 92)).strong())
                .on_hover_text("The screen, typed text and mouse clicks of this session are being recorded");
            ui.ctx().request_repaint_after(Duration::from_millis(300));
            ui.separator();
        }
        ui.label(RichText::new("Scaling").weak());
        for mode in Scaling::ALL {
            let short = match mode {
                Scaling::Fit => "Fit",
                Scaling::Stretch => "Stretch",
                Scaling::Actual => "1:1",
            };
            if ui.selectable_label(self.scaling == mode, short).on_hover_text(mode.label()).clicked() {
                self.scaling = mode;
                self.pan = Vec2::ZERO;
            }
        }
        ui.separator();
        if ui.checkbox(&mut self.smooth, "Smooth").on_hover_text("Bilinear filtering when scaled; off = crisp pixels").changed() {
            self.seen_version = 0; // re-upload with the new filter
        }
        if matches!(self.input, Input::Rdp(_)) {
            ui.separator();
            let mut follow = !self.fixed_size;
            if ui.checkbox(&mut follow, "Resize with window").on_hover_text("Change the remote resolution to match this tab").changed() {
                self.fixed_size = !follow;
                self.pending_size = None;
            }
        }
        if !self.describe.is_empty() {
            ui.separator();
            ui.label(RichText::new(&self.describe).weak().small());
        } else if let Some(t) = &self.texture {
            ui.separator();
            ui.label(RichText::new(format!("{} x {}", t.size()[0], t.size()[1])).weak().small());
        }
    }

    // ---- resize ----

    fn maybe_resize(&mut self, area: Rect, ppp: f32) {
        let Input::Rdp(tx) = &self.input else { return };
        if self.fixed_size || self.status() != Status::Connected {
            return;
        }
        let wanted = even_size(((area.width() * ppp) as u16, (area.height() * ppp) as u16));
        if wanted == self.requested_size {
            self.pending_size = None;
            return;
        }
        match self.pending_size {
            Some((size, since)) if size == wanted && since.elapsed() > Duration::from_millis(600) => {
                let _ = tx.send(RdpInputEvent::Resize { width: wanted.0, height: wanted.1, scale_factor: 100, physical_size: None });
                self.requested_size = wanted;
                self.pending_size = None;
            }
            Some((size, _)) if size == wanted => {}
            _ => self.pending_size = Some((wanted, Instant::now())),
        }
    }

    // ---- input helpers ----

    fn rdp(&mut self, ops: Vec<Operation>) {
        if let Input::Rdp(tx) = &self.input {
            let events = self.rdp_db.apply(ops);
            if !events.is_empty() {
                let _ = tx.send(RdpInputEvent::FastPath(events));
            }
        }
    }

    fn vnc(&self, event: X11Event) {
        if let Input::Vnc(tx) = &self.input {
            let _ = tx.send(event);
        }
    }

    fn pointer_move(&mut self, (x, y): (u16, u16), _buttons: [bool; 3]) {
        match self.input {
            Input::Rdp(_) => self.rdp(vec![Operation::MouseMove(MousePosition { x, y })]),
            Input::Vnc(_) => self.vnc(X11Event::PointerEvent((x, y, self.vnc_buttons).into())),
        }
    }

    fn mouse_button(&mut self, idx: usize, down: bool) {
        if let (Some(rec), true) = (&self.rec, down) {
            let (x, y) = self.last_pointer.unwrap_or((0, 0));
            let (w, h) = self.texture.as_ref().map_or((0, 0), |t| (t.size()[0], t.size()[1]));
            rec.event("m", &format!("{} {x},{y} {w}x{h}", ["left", "middle", "right"][idx]));
        }
        match self.input {
            Input::Rdp(_) => {
                let button = [MouseButton::Left, MouseButton::Middle, MouseButton::Right][idx];
                self.rdp(vec![if down { Operation::MouseButtonPressed(button) } else { Operation::MouseButtonReleased(button) }]);
            }
            Input::Vnc(_) => {
                let bit = [1u8, 2, 4][idx];
                if down {
                    self.vnc_buttons |= bit;
                } else {
                    self.vnc_buttons &= !bit;
                }
                let (x, y) = self.last_pointer.unwrap_or((0, 0));
                self.vnc(X11Event::PointerEvent((x, y, self.vnc_buttons).into()));
            }
        }
    }

    fn wheel(&mut self, notches: f32) {
        if notches.abs() < 0.01 {
            return;
        }
        match self.input {
            Input::Rdp(_) => {
                let units = (notches * 120.0).round().clamp(-480.0, 480.0) as i16;
                self.rdp(vec![Operation::WheelRotations(WheelRotations { is_vertical: true, rotation_units: units })]);
            }
            Input::Vnc(_) => {
                let (x, y) = self.last_pointer.unwrap_or((0, 0));
                let bit = if notches > 0.0 { 8 } else { 16 };
                for _ in 0..(notches.abs().round().max(1.0) as usize).min(5) {
                    self.vnc(X11Event::PointerEvent((x, y, self.vnc_buttons | bit).into()));
                    self.vnc(X11Event::PointerEvent((x, y, self.vnc_buttons).into()));
                }
            }
        }
    }

    fn sync_modifiers(&mut self, now: Modifiers) {
        let pairs = [
            (self.modifiers.shift, now.shift, 0x2A_u16, 0xFFE1_u32),
            (self.modifiers.ctrl, now.ctrl, 0x1D, 0xFFE3),
            (self.modifiers.alt, now.alt, 0x38, 0xFFE9),
        ];
        for (was, is, scancode, keysym) in pairs {
            if was != is {
                match self.input {
                    Input::Rdp(_) => {
                        let sc = Scancode::from_u16(scancode);
                        self.rdp(vec![if is { Operation::KeyPressed(sc) } else { Operation::KeyReleased(sc) }]);
                    }
                    Input::Vnc(_) => self.vnc(X11Event::KeyEvent((keysym, is).into())),
                }
            }
        }
        self.modifiers = now;
    }

    fn key(&mut self, key: Key, pressed: bool) {
        match self.input {
            Input::Rdp(_) => {
                if let Some(code) = scancode(key) {
                    let sc = Scancode::from_u16(code);
                    self.rdp(vec![if pressed { Operation::KeyPressed(sc) } else { Operation::KeyReleased(sc) }]);
                }
            }
            Input::Vnc(_) => {
                // Printable keys arrive as Text events, except with Ctrl/Alt held.
                let keysym = special_keysym(key).or_else(|| {
                    (self.modifiers.ctrl || self.modifiers.alt).then(|| key.symbol_or_name().chars().next()).flatten().filter(|c| c.is_ascii_alphanumeric()).map(|c| c.to_ascii_lowercase() as u32)
                });
                if let Some(keysym) = keysym {
                    self.vnc(X11Event::KeyEvent((keysym, pressed).into()));
                }
            }
        }
    }

    fn tap(&mut self, key: Key) {
        self.key(key, true);
        self.key(key, false);
    }

    fn vnc_text(&self, text: &str) {
        for c in text.chars() {
            let keysym = vnc_char_keysym(c);
            self.vnc(X11Event::KeyEvent((keysym, true).into()));
            self.vnc(X11Event::KeyEvent((keysym, false).into()));
        }
    }

    fn type_text(&mut self, text: &str) {
        // Release Ctrl first so the remote side sees plain typing, not shortcuts.
        let held = self.modifiers;
        self.sync_modifiers(Modifiers::default());
        match self.input {
            Input::Rdp(_) => {
                for c in text.chars().filter(|c| *c != '\r') {
                    if c == '\n' {
                        self.tap(Key::Enter);
                    } else {
                        self.rdp(vec![Operation::UnicodeKeyPressed(c), Operation::UnicodeKeyReleased(c)]);
                    }
                }
            }
            Input::Vnc(_) => self.vnc_text(&text.replace('\r', "")),
        }
        self.sync_modifiers(held);
    }
}

impl SessionView for Session {
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
        self.kind.badge()
    }
    fn status(&self) -> Status {
        Session::status(self)
    }
    fn toolbar(&mut self, ui: &mut egui::Ui) {
        self.display_toolbar(ui)
    }
    fn ui(&mut self, ui: &mut egui::Ui) {
        Session::ui(self, ui)
    }
    fn insert_text(&mut self, text: &str) -> bool {
        if !self.status().is_live() {
            return false;
        }
        // Types into whatever field has focus on the remote screen (no Enter).
        self.type_text(text);
        true
    }
    fn close(&mut self) {
        Session::close(self)
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.close();
    }
}

fn even_size((w, h): (u16, u16)) -> (u16, u16) {
    (w.clamp(640, 8192) & !1, h.clamp(480, 8192) & !1)
}

fn hostname() -> String {
    std::env::var("COMPUTERNAME").unwrap_or_else(|_| "SYSTEMDECK".into())
}

/// Turns IronRDP's error chain into something an admin can act on.
fn rdp_error_text(raw: &str) -> String {
    let lower = raw.to_lowercase();
    let hint = if lower.contains("logon") || lower.contains("credssp") || lower.contains("authentication") || lower.contains("0xc000006d") {
        " - check the username, password and domain"
    } else if lower.contains("refused") || lower.contains("timed out") || lower.contains("unreachable") {
        " - is the host reachable and RDP enabled?"
    } else {
        ""
    };
    format!("RDP connection failed: {raw}{hint}")
}

fn apply_vnc_event(state: &Shared, event: VncEvent) {
    let mut f = state.frame.lock().unwrap();
    match event {
        VncEvent::SetResolution(screen) => {
            f.width = usize::from(screen.width);
            f.height = usize::from(screen.height);
            f.rgba = vec![0; f.width * f.height * 4];
            f.rgba.chunks_exact_mut(4).for_each(|p| p[3] = 255);
        }
        VncEvent::RawImage(rect, data) => {
            let (fw, fh) = (f.width, f.height);
            let (rx, ry, rw, rh) = (usize::from(rect.x), usize::from(rect.y), usize::from(rect.width), usize::from(rect.height));
            for row in 0..rh {
                let y = ry + row;
                if y >= fh || rx >= fw {
                    break;
                }
                let n = rw.min(fw - rx);
                let src = &data[row * rw * 4..][..n * 4];
                let dst = &mut f.rgba[(y * fw + rx) * 4..][..n * 4];
                for (d, s) in dst.chunks_exact_mut(4).zip(src.chunks_exact(4)) {
                    d.copy_from_slice(&[s[0], s[1], s[2], 255]);
                }
            }
        }
        VncEvent::Copy(dst, src) => {
            let fw = f.width;
            let (w, h) = (usize::from(dst.width), usize::from(dst.height));
            let copy: Vec<Vec<u8>> = (0..h)
                .filter_map(|row| {
                    let start = ((usize::from(src.y) + row) * fw + usize::from(src.x)) * 4;
                    f.rgba.get(start..start + w * 4).map(<[u8]>::to_vec)
                })
                .collect();
            for (row, line) in copy.into_iter().enumerate() {
                let start = ((usize::from(dst.y) + row) * fw + usize::from(dst.x)) * 4;
                if let Some(d) = f.rgba.get_mut(start..start + line.len()) {
                    d.copy_from_slice(&line);
                }
            }
        }
        _ => return,
    }
    f.version += 1;
}

/// PC/AT set-1 scancodes (0xE0xx = extended) by physical key position.
fn scancode(key: Key) -> Option<u16> {
    use Key::*;
    Some(match key {
        Escape => 0x01,
        Num1 => 0x02, Num2 => 0x03, Num3 => 0x04, Num4 => 0x05, Num5 => 0x06,
        Num6 => 0x07, Num7 => 0x08, Num8 => 0x09, Num9 => 0x0A, Num0 => 0x0B,
        Minus => 0x0C, Equals | Plus => 0x0D, Backspace => 0x0E, Tab => 0x0F,
        Q => 0x10, W => 0x11, E => 0x12, R => 0x13, T => 0x14, Y => 0x15, U => 0x16, I => 0x17, O => 0x18, P => 0x19,
        OpenBracket => 0x1A, CloseBracket => 0x1B, Enter => 0x1C,
        A => 0x1E, S => 0x1F, D => 0x20, F => 0x21, G => 0x22, H => 0x23, J => 0x24, K => 0x25, L => 0x26,
        Semicolon => 0x27, Quote => 0x28, Backtick => 0x29, Backslash => 0x2B,
        Z => 0x2C, X => 0x2D, C => 0x2E, V => 0x2F, B => 0x30, N => 0x31, M => 0x32,
        Comma => 0x33, Period => 0x34, Slash => 0x35, Space => 0x39,
        F1 => 0x3B, F2 => 0x3C, F3 => 0x3D, F4 => 0x3E, F5 => 0x3F, F6 => 0x40, F7 => 0x41, F8 => 0x42, F9 => 0x43, F10 => 0x44,
        F11 => 0x57, F12 => 0x58,
        Insert => 0xE052, Delete => 0xE053, Home => 0xE047, End => 0xE04F, PageUp => 0xE049, PageDown => 0xE051,
        ArrowUp => 0xE048, ArrowDown => 0xE050, ArrowLeft => 0xE04B, ArrowRight => 0xE04D,
        _ => return None,
    })
}

/// X11 keysyms for non-printing keys.
fn special_keysym(key: Key) -> Option<u32> {
    use Key::*;
    Some(match key {
        Backspace => 0xFF08, Tab => 0xFF09, Enter => 0xFF0D, Escape => 0xFF1B, Delete => 0xFFFF,
        Home => 0xFF50, ArrowLeft => 0xFF51, ArrowUp => 0xFF52, ArrowRight => 0xFF53, ArrowDown => 0xFF54,
        PageUp => 0xFF55, PageDown => 0xFF56, End => 0xFF57, Insert => 0xFF63,
        F1 => 0xFFBE, F2 => 0xFFBF, F3 => 0xFFC0, F4 => 0xFFC1, F5 => 0xFFC2, F6 => 0xFFC3,
        F7 => 0xFFC4, F8 => 0xFFC5, F9 => 0xFFC6, F10 => 0xFFC7, F11 => 0xFFC8, F12 => 0xFFC9,
        _ => return None,
    })
}

fn vnc_char_keysym(c: char) -> u32 {
    match c {
        '\n' => 0xFF0D,
        '\t' => 0xFF09,
        c if (c as u32) < 0x100 => c as u32,
        c => 0x0100_0000 + c as u32,
    }
}

// ---------------------------------------------------------------------------
// Embedded browser (WebView2 child window, Windows only)
// ---------------------------------------------------------------------------

/// A Website entry opened inside SystemDeck. A WebView2 control (the Edge
/// engine built into Windows) is parented to the SystemDeck window and
/// positioned over the content pane each frame.
#[cfg(windows)]
pub struct BrowserSession {
    id: Uuid,
    entry_id: Uuid,
    title: String,
    url: String,
    username: String,
    password: String,
    webview: wry::WebView,
    visible: bool,
    /// Current parent window HWND (main window, or a detached one).
    parent: isize,
    rec: Option<BrowserRec>,
}

/// Records a browser session: a thread screenshots the WebView2 window about
/// once a second (JPEG frames) and logs each page the user visits.
#[cfg(windows)]
struct BrowserRec {
    status: Arc<Mutex<String>>,
    stop: Arc<std::sync::atomic::AtomicBool>,
}

/// Lets wry parent a child webview to our existing window by its HWND.
#[cfg(windows)]
struct ParentWindow(isize);

#[cfg(windows)]
impl raw_window_handle::HasWindowHandle for ParentWindow {
    fn window_handle(&self) -> Result<raw_window_handle::WindowHandle<'_>, raw_window_handle::HandleError> {
        use raw_window_handle::{HandleError, RawWindowHandle, Win32WindowHandle, WindowHandle};
        let hwnd = std::num::NonZeroIsize::new(self.0).ok_or(HandleError::Unavailable)?;
        let handle = Win32WindowHandle::new(hwnd);
        // Safe: the HWND outlives the webview (it is the app's own window).
        Ok(unsafe { WindowHandle::borrow_raw(RawWindowHandle::Win32(handle)) })
    }
}

#[cfg(windows)]
impl BrowserSession {
    pub fn connect(entry: &Entry, parent_hwnd: isize, ctx: &egui::Context, record: Option<RecordPlan>) -> Result<Self, String> {
        let url = if entry.url.trim().is_empty() {
            format!("https://{}", entry.host.trim())
        } else {
            entry.url.trim().to_string()
        };
        if !(url.starts_with("http://") || url.starts_with("https://")) {
            return Err("Only http:// and https:// addresses can open in a tab".into());
        }
        if parent_hwnd == 0 {
            return Err("The window is not ready yet - try again in a moment".into());
        }
        let ppp = ctx.pixels_per_point();
        let parent = ParentWindow(parent_hwnd);
        // When recording, inject rrweb and receive its DOM events over the IPC bridge.
        let (ev_tx, ev_rx) = std::sync::mpsc::channel::<String>();
        let mut builder = wry::WebViewBuilder::new()
            .with_url(&url)
            .with_bounds(wry::Rect {
                position: wry::dpi::PhysicalPosition::new((320.0 * ppp) as i32, (120.0 * ppp) as i32).into(),
                size: wry::dpi::PhysicalSize::new(800, 600).into(),
            })
            .with_focused(true);
        if record.is_some() {
            builder = builder
                .with_initialization_script(RRWEB_RECORDER)
                .with_ipc_handler(move |req: wry::http::Request<String>| {
                    let _ = ev_tx.send(req.into_body());
                });
        }
        let webview = builder
            .build_as_child(&parent)
            .map_err(|e| format!("Could not open the embedded browser (is the WebView2 runtime installed?): {e}"))?;
        let rec = record.map(|plan| BrowserRec::start(plan, format!("{} - {}", url, entry.name), ev_rx));
        Ok(Self {
            id: Uuid::new_v4(),
            entry_id: entry.id,
            title: entry.name.clone(),
            url,
            username: entry.username.clone(),
            password: entry.password.clone(),
            webview,
            visible: true,
            parent: parent_hwnd,
            rec,
        })
    }

    /// JS that fills the page's password field and the username field just
    /// before it, firing input/change events so the site's form notices.
    fn fill_script(&self) -> String {
        let u = serde_json::to_string(&self.username).unwrap_or_else(|_| "\"\"".into());
        let p = serde_json::to_string(&self.password).unwrap_or_else(|_| "\"\"".into());
        format!(
            r#"(function(){{
  function setv(el,v){{try{{var d=Object.getOwnPropertyDescriptor(Object.getPrototypeOf(el),'value');d&&d.set?d.set.call(el,v):el.value=v;}}catch(e){{el.value=v;}}el.dispatchEvent(new Event('input',{{bubbles:true}}));el.dispatchEvent(new Event('change',{{bubbles:true}}));}}
  var pw=document.querySelector('input[type=password]');
  if(!pw){{return;}}
  setv(pw,{p});
  var ins=[].slice.call(document.querySelectorAll('input'));
  var i=ins.indexOf(pw),user=null;
  for(var j=i-1;j>=0;j--){{var t=(ins[j].type||'text').toLowerCase();if(t==='text'||t==='email'||t==='tel'){{user=ins[j];break;}}}}
  if(user&&{u}){{setv(user,{u});}}
  pw.focus();
}})();"#
        )
    }

    /// JS that types a value into whatever field the user last clicked.
    fn type_script(value: &str) -> String {
        let v = serde_json::to_string(value).unwrap_or_else(|_| "\"\"".into());
        format!(
            r#"(function(){{var el=document.activeElement;if(!el||(el.tagName!=='INPUT'&&el.tagName!=='TEXTAREA')){{return;}}try{{var d=Object.getOwnPropertyDescriptor(Object.getPrototypeOf(el),'value');d&&d.set?d.set.call(el,{v}):el.value={v};}}catch(e){{el.value={v};}}el.dispatchEvent(new Event('input',{{bubbles:true}}));el.dispatchEvent(new Event('change',{{bubbles:true}}));}})();"#
        )
    }

    fn place(&self, rect: Rect, ppp: f32) {
        let _ = self.webview.set_bounds(wry::Rect {
            position: wry::dpi::PhysicalPosition::new((rect.min.x * ppp) as i32, (rect.min.y * ppp) as i32).into(),
            size: wry::dpi::PhysicalSize::new((rect.width() * ppp).max(1.0) as u32, (rect.height() * ppp).max(1.0) as u32).into(),
        });
    }
}

#[cfg(windows)]
impl SessionView for BrowserSession {
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
        "WEB"
    }
    fn status(&self) -> Status {
        Status::Connected
    }
    fn toolbar(&mut self, ui: &mut egui::Ui) {
        if let Some(rec) = &self.rec {
            let text = rec.status.lock().unwrap().clone();
            let blink = (ui.input(|i| i.time) * 1.5).fract() < 0.6;
            let (r, _) = ui.allocate_exact_size(egui::vec2(12.0, 12.0), Sense::hover());
            if blink {
                ui.painter().circle_filled(r.center(), 5.0, Color32::from_rgb(232, 64, 72));
            }
            ui.label(RichText::new(text).color(Color32::from_rgb(232, 84, 92)).strong())
                .on_hover_text("This browser session (pages visited and the screen) is being recorded");
            ui.ctx().request_repaint_after(Duration::from_millis(300));
            ui.separator();
        }
        if ui.button("Reload").clicked() {
            let _ = self.webview.load_url(&self.url);
        }
        if !self.password.is_empty() {
            ui.separator();
            if ui
                .button("Fill login")
                .on_hover_text("Fill this page's username and password fields with the stored credentials")
                .clicked()
            {
                let _ = self.webview.evaluate_script(&self.fill_script());
            }
            if ui
                .button("Type password")
                .on_hover_text("Click the password box on the page first, then this")
                .clicked()
            {
                let _ = self.webview.evaluate_script(&Self::type_script(&self.password));
            }
            if !self.username.is_empty()
                && ui
                    .button("Type username")
                    .on_hover_text("Click the username box on the page first, then this")
                    .clicked()
            {
                let _ = self.webview.evaluate_script(&Self::type_script(&self.username));
            }
            if ui.button("Copy password").clicked() {
                ui.ctx().copy_text(self.password.clone());
            }
        }
        ui.separator();
        if ui.button("Open in browser").on_hover_text("Open this page in your default browser").clicked() {
            let _ = open::that_detached(&self.url);
        }
        ui.separator();
        ui.label(RichText::new(&self.url).weak().small());
    }
    fn set_visible(&mut self, visible: bool) {
        if visible != self.visible {
            let _ = self.webview.set_visible(visible);
            self.visible = visible;
        }
    }
    fn reparent(&mut self, hwnd: isize) {
        if hwnd != 0 && hwnd != self.parent {
            use wry::WebViewExtWindows;
            if self.webview.reparent(hwnd).is_ok() {
                self.parent = hwnd;
            }
        }
    }
    fn ui(&mut self, ui: &mut egui::Ui) {
        let rect = ui.available_rect_before_wrap();
        ui.allocate_rect(rect, Sense::hover());
        self.set_visible(true);
        self.place(rect, ui.ctx().pixels_per_point());
    }
    fn ai_context(&self) -> String {
        format!("Current page URL: {}", self.url)
    }
    fn insert_text(&mut self, text: &str) -> bool {
        // Types into whatever field the user last clicked on the page.
        let _ = self.webview.evaluate_script(&Self::type_script(text));
        true
    }
    fn close(&mut self) {
        if let Some(rec) = &self.rec {
            rec.stop.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        let _ = self.webview.set_visible(false);
    }
}

#[cfg(windows)]
impl Drop for BrowserSession {
    fn drop(&mut self) {
        if let Some(rec) = &self.rec {
            rec.stop.store(true, std::sync::atomic::Ordering::Relaxed);
        }
    }
}

// ---- Browser DOM recording (rrweb, Windows) ----

/// rrweb recorder injected at document start: captures the DOM and forwards
/// each event over the WebView2 IPC bridge. Runs even on CSP-strict sites
/// because WebView2 executes it as a host script. Passwords are masked.
#[cfg(windows)]
const RRWEB_RECORDER: &str = concat!(
    include_str!("../assets/rrweb.min.js"),
    "\n;(function(){if(window.__sdrec)return;window.__sdrec=1;function s(){try{window.__sdstop=rrweb.record({emit:function(ev){try{window.ipc.postMessage(JSON.stringify(ev));}catch(e){}},maskInputOptions:{password:true},inlineStylesheet:true,collectFonts:false});try{rrweb.record.addCustomEvent('url',location.href);}catch(e){}}catch(e){}}if(document.readyState==='loading'){document.addEventListener('DOMContentLoaded',s);}else{s();}})();"
);

#[cfg(windows)]
impl BrowserRec {
    fn start(plan: RecordPlan, title: String, rx: std::sync::mpsc::Receiver<String>) -> Self {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::mpsc::RecvTimeoutError;
        let status = Arc::new(Mutex::new("REC - starting".to_string()));
        let stop = Arc::new(AtomicBool::new(false));
        let (st, stopped) = (status.clone(), stop.clone());
        std::thread::Builder::new()
            .name("systemdeck-browser-rec".into())
            .spawn(move || {
                let mut rec = Recorder::dom(plan, &title, st);
                let mut last_flush = Instant::now();
                loop {
                    match rx.recv_timeout(Duration::from_millis(500)) {
                        Ok(line) => rec.push_json(&line),
                        Err(RecvTimeoutError::Timeout) => {}
                        Err(RecvTimeoutError::Disconnected) => break,
                    }
                    if last_flush.elapsed() >= Duration::from_secs(2) {
                        rec.flush();
                        last_flush = Instant::now();
                    }
                    if stopped.load(Ordering::Relaxed) {
                        // Drain anything buffered before finishing.
                        while let Ok(line) = rx.try_recv() {
                            rec.push_json(&line);
                        }
                        break;
                    }
                }
                rec.finish();
            })
            .expect("starting browser recording thread");
        Self { status, stop }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scancodes_cover_typing_keys() {
        assert_eq!(scancode(Key::A), Some(0x1E));
        assert_eq!(scancode(Key::Enter), Some(0x1C));
        assert_eq!(Scancode::from_u16(scancode(Key::ArrowLeft).unwrap()), Scancode::from_u8(true, 0x4B));
    }

    #[test]
    fn vnc_keysyms() {
        assert_eq!(vnc_char_keysym('a'), 0x61);
        assert_eq!(vnc_char_keysym('é'), 0xE9);
        assert_eq!(vnc_char_keysym('€'), 0x0100_20AC);
    }

    #[test]
    fn resize_is_even_and_bounded() {
        assert_eq!(even_size((1001, 101)), (1000, 480));
    }

    #[test]
    fn frames_are_downscaled_jpeg() {
        use base64::Engine as _;
        let (w, h) = (1920usize, 1080usize);
        let rgba: Vec<u8> = (0..w * h).flat_map(|i| [(i % 256) as u8, 80, 160, 255]).collect();
        let b64 = encode_frame(w, h, rgba).expect("encodes");
        let jpeg = base64::engine::general_purpose::STANDARD.decode(b64).unwrap();
        assert_eq!(&jpeg[..2], &[0xFF, 0xD8], "JPEG magic");
        let img = image::load_from_memory_with_format(&jpeg, image::ImageFormat::Jpeg).unwrap();
        assert_eq!((img.width(), img.height()), (1280, 720));
    }

    #[test]
    fn typed_keys_for_the_activity_log() {
        assert_eq!(recorded_key(Key::Enter, Modifiers::NONE), Some(("i", "\r".into())));
        assert_eq!(recorded_key(Key::Backspace, Modifiers::NONE), Some(("i", "\u{7f}".into())));
        assert_eq!(recorded_key(Key::C, Modifiers::CTRL), Some(("k", "Ctrl+C".into())));
        assert_eq!(recorded_key(Key::Delete, Modifiers::CTRL | Modifiers::ALT), Some(("k", "Ctrl+Alt+Delete".into())));
        assert_eq!(recorded_key(Key::A, Modifiers::NONE), None, "letters come as Text events");
    }

    #[test]
    fn screen_recording_saved_locally() {
        let dir = std::env::temp_dir().join(format!("vd-screen-{}", Uuid::new_v4()));
        let plan = RecordPlan { upload: None, local_dir: dir.clone(), file_stem: "rdp".into() };
        let shared = Arc::new(Shared { status: Mutex::new(Status::Connected), frame: Mutex::new(Frame::default()) });
        {
            let mut f = shared.frame.lock().unwrap();
            *f = Frame { width: 64, height: 48, rgba: vec![200; 64 * 48 * 4], version: 1 };
        }
        let rec = ScreenRec::start(shared.clone(), plan, "admin@10.0.0.5 - DC".into());
        std::thread::sleep(Duration::from_millis(300));
        rec.event("i", "ipconfig\r");
        rec.event("m", "left 10,20 64x48");
        rec.stop.store(true, Ordering::Relaxed);
        let path = dir.join("rdp.sdrec");
        let mut text = String::new();
        for _ in 0..60 {
            text = std::fs::read_to_string(&path).unwrap_or_default();
            if text.contains("left 10,20") {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        let lines: Vec<&str> = text.lines().collect();
        let header: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!((header["kind"].as_str(), header["width"].as_i64()), (Some("screen"), Some(64)));
        assert!(lines.iter().any(|l| l.contains(r#","f",""#)), "a frame: {text:.300}");
        assert!(text.contains(r#""i","ipconfig\r""#), "{text:.300}");
        let _ = std::fs::remove_dir_all(dir);
    }
}
