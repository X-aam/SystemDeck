mod view;

use std::{
    collections::{HashMap, HashSet},
    sync::{mpsc, Arc},
    thread,
    time::{Duration, Instant},
};

use chrono::{DateTime, Local, Utc};
use eframe::egui::{self, Color32, RichText};
use rand::{seq::SliceRandom, Rng};
use uuid::Uuid;
use systemdeck_core::{ColorDepth, DisplayMode, DisplaySettings, Entry, EntryKind, Quality, Resolution, Scaling};
use systemdeck_core::api::{AiChatRequest, AiChatTurn, AiModel};

use crate::{
    admin::AdminPanel,
    api::{normalize_url, Api, ApiError},
    launch,
    sessions::{Session, SessionView},
    ssh::{FileSession, RecordPlan, TermSession},
    store::{CachedVault, LocalEntry, Settings, Store, LOCAL_VAULT},
    sync::{self, SyncReport},
};
#[cfg(windows)]
use crate::sessions::BrowserSession;

const AUTO_SYNC_EVERY: Duration = Duration::from_secs(60);
/// Vault-picker choice that lists every vault (Local + server) together.
pub(crate) const ALL_VAULTS: Uuid = Uuid::max();
/// `ToggleGroup` folder key for a vault's own heading in the all-vaults list.
pub(crate) const VAULT_HEADER: &str = "\u{1}vault";
const WARN: Color32 = crate::theme::WARN;
const ERROR: Color32 = crate::theme::DANGER;

#[derive(PartialEq)]
enum Conn {
    SignedOut,
    Online,
    Offline,
}

enum Action {
    SelectVault(Uuid),
    Select(Uuid),
    New,
    Edit,
    Save,
    Cancel,
    Delete(Uuid),
    Launch(Uuid),
    CopyUser(Uuid),
    CopyPassword(Uuid),
    CopyTo { id: Uuid, target: Uuid, remove_source: bool },
    KeepMine(Uuid),
    UseServer(Uuid),
    SyncNow,
    ShowLogin,
    Logout,
    OpenAdmin,
    ShowPasswordChange,
    ShowImport,
    LaunchEmbedded(Uuid),
    LaunchExternal(Uuid),
    ActivateTab(Option<Uuid>),
    CloseSession(Uuid),
    Reconnect(Uuid),
    OpenFiles(Uuid),
    /// (vault, folder path); the vault heading itself uses `VAULT_HEADER`.
    ToggleGroup(Uuid, String),
    /// true = open sessions in tabs by default.
    SetEmbed(bool),
    AskDelete(Uuid),
    SetDark(bool),
    /// Record all sessions of the Local vault.
    SetRecordLocal(bool),
    /// Entry-level recording switch (details card).
    SetEntryRecording(Uuid, bool),
    OpenRecordingsFolder,
    ToggleFullScreen,
    /// Enter/leave selection mode.
    SetSelectMode(bool),
    TogglePick(Uuid),
    PickAll(Vec<Uuid>),
    /// Push the picked entries to a vault (copy, or move = delete the originals).
    BulkPush { target: Uuid, remove_source: bool },
    /// Open another session of this entry even if one is already open.
    OpenAnother(Uuid),
    /// Show/hide the connection list panel.
    ToggleNav,
    /// Pop a session out into its own window (or re-attach it).
    ToggleDetach(Uuid),
    /// Show/hide the in-session AI assistant panel.
    ToggleAssistant,
    /// Send the typed question to the assistant for the active session.
    AssistantSend,
    /// Insert an assistant-suggested command into a session (no Enter).
    InsertToSession(Uuid, String),
}

/// One line in an assistant conversation.
#[derive(Clone)]
struct ChatMsg {
    role: ChatRole,
    text: String,
}

#[derive(Clone, Copy, PartialEq)]
enum ChatRole {
    User,
    Assistant,
    Error,
}

/// Entry being created or edited, with text fields for the non-string parts.
struct Draft {
    entry: Entry,
    port: String,
    tags: String,
    is_new: bool,
    show_password: bool,
    /// Why the recording switch can't be changed here (None = free to change).
    record_locked: Option<&'static str>,
}

impl Draft {
    fn new(entry: Entry, is_new: bool) -> Self {
        Self {
            port: entry.port.map(|p| p.to_string()).unwrap_or_default(),
            tags: entry.tags.join(", "),
            entry,
            is_new,
            show_password: false,
            record_locked: None,
        }
    }

    fn build(&self) -> Result<Entry, String> {
        let mut e = self.entry.clone();
        e.name = e.name.trim().to_string();
        if e.name.is_empty() {
            return Err("Name is required".into());
        }
        e.port = match self.port.trim() {
            "" => None,
            p => Some(p.parse().map_err(|_| "Port must be a number between 1 and 65535".to_string())?),
        };
        e.tags = self.tags.split(',').map(|t| t.trim().to_string()).filter(|t| !t.is_empty()).collect();
        e.group = e.group.trim().trim_matches('/').to_string();
        e.host = e.host.trim().to_string();
        e.username = e.username.trim().to_string();
        Ok(e)
    }
}

struct LoginForm {
    url: String,
    username: String,
    password: String,
    error: Option<String>,
}

#[derive(Default)]
struct PasswordForm {
    old: String,
    new: String,
    confirm: String,
    error: Option<String>,
}

pub struct App {
    store: Arc<Store>,
    settings: Settings,
    api: Api,
    conn: Conn,

    vaults: Vec<CachedVault>,
    current_vault: Uuid,
    entries: Vec<LocalEntry>,
    selected: Option<Uuid>,
    draft: Option<Draft>,
    search: String,
    reveal_password: bool,
    copy_target: Option<Uuid>,
    confirm_delete: Option<Uuid>,

    status: String,
    status_is_error: bool,
    sync_rx: Option<mpsc::Receiver<Result<SyncReport, ApiError>>>,
    last_sync_attempt: Option<Instant>,
    last_sync_ok: Option<DateTime<Local>>,

    login: Option<LoginForm>,
    password_form: Option<PasswordForm>,
    /// Path typed into the CSV import window.
    import_path: Option<String>,
    admin: Option<AdminPanel>,

    /// Embedded RDP/VNC sessions, shown as tabs next to "Details".
    sessions: Vec<Box<dyn SessionView>>,
    /// `None` = Details tab.
    active_tab: Option<Uuid>,
    /// Size of the session area in pixels (initial RDP desktop size).
    session_px: (u16, u16),

    /// Per vault: (entries, entries needing attention: unsynced or conflicting).
    vault_counts: HashMap<Uuid, (usize, usize)>,
    /// Collapsed folders, per vault: (vault id, folder path).
    collapsed_groups: HashSet<(Uuid, String)>,
    /// Session fills the window (lists and header hidden, OS full screen).
    focus_mode: bool,
    /// Multi-select mode in the list (bulk push / copy).
    select_mode: bool,
    picked: HashSet<Uuid>,
    push_target: Option<Uuid>,
    push_move: bool,
    /// SystemDeck window handle, captured each frame, for embedded browsers.
    window_handle: Option<isize>,
    /// Connection list hidden to give sessions more room.
    nav_collapsed: bool,
    /// Sessions popped out into their own window.
    detached: HashSet<Uuid>,
    /// Session tab currently being dragged to reorder.
    tab_drag: Option<Uuid>,

    /// In-session AI assistant: panel open, draft question, chosen model.
    assistant_open: bool,
    assistant_input: String,
    assistant_model: Option<String>,
    /// Conversation per session id, so switching tabs keeps each chat.
    assistant_chats: HashMap<Uuid, Vec<ChatMsg>>,
    /// In-flight reply: (session id, receiver of the reply or an error).
    assistant_pending: Option<(Uuid, mpsc::Receiver<Result<String, String>>)>,
    /// Models offered by the server (fetched once when the panel first opens).
    ai_models: Vec<AiModel>,
    ai_models_rx: Option<mpsc::Receiver<Vec<AiModel>>>,
    ai_loaded: bool,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, store: Store) -> Self {
        cc.egui_ctx.set_pixels_per_point(cc.egui_ctx.pixels_per_point().max(1.0));
        let settings = store.settings().unwrap_or_default();
        crate::theme::apply(&cc.egui_ctx, settings.dark_mode);
        let api = Api::new(&settings.server_url, settings.token.clone());
        let conn = if settings.token.is_some() { Conn::Offline } else { Conn::SignedOut };
        let mut app = Self {
            store: Arc::new(store),
            settings,
            api,
            conn,
            vaults: Vec::new(),
            current_vault: ALL_VAULTS,
            entries: Vec::new(),
            selected: None,
            draft: None,
            search: String::new(),
            reveal_password: false,
            copy_target: None,
            confirm_delete: None,
            status: String::new(),
            status_is_error: false,
            sync_rx: None,
            last_sync_attempt: None,
            last_sync_ok: None,
            login: None,
            password_form: None,
            import_path: None,
            admin: None,
            sessions: Vec::new(),
            active_tab: None,
            session_px: (1280, 720),
            vault_counts: HashMap::new(),
            collapsed_groups: HashSet::new(),
            focus_mode: false,
            select_mode: false,
            picked: HashSet::new(),
            push_target: None,
            push_move: false,
            window_handle: None,
            nav_collapsed: false,
            detached: HashSet::new(),
            tab_drag: None,
            assistant_open: false,
            assistant_input: String::new(),
            assistant_model: None,
            assistant_chats: HashMap::new(),
            assistant_pending: None,
            ai_models: Vec::new(),
            ai_models_rx: None,
            ai_loaded: false,
        };
        app.reload();
        app
    }

    // ---- state helpers ----

    fn set_status(&mut self, msg: impl Into<String>) {
        self.status = msg.into();
        self.status_is_error = false;
    }

    fn set_error(&mut self, msg: impl Into<String>) {
        self.status = msg.into();
        self.status_is_error = true;
    }

    fn check<T>(&mut self, result: anyhow::Result<T>) -> Option<T> {
        match result {
            Ok(v) => Some(v),
            Err(e) => {
                self.set_error(format!("{e:#}"));
                None
            }
        }
    }

    fn reload(&mut self) {
        if let Some(vaults) = self.check(self.store.vaults()) {
            self.vaults = vaults;
        }
        if self.current_vault != ALL_VAULTS && !self.vaults.iter().any(|v| v.id == self.current_vault) {
            self.current_vault = ALL_VAULTS;
            self.selected = None;
            self.draft = None;
        }
        // "All vaults": every vault's entries in one list (Local first, then server vaults).
        let shown: Vec<Uuid> =
            if self.current_vault == ALL_VAULTS { self.vaults.iter().map(|v| v.id).collect() } else { vec![self.current_vault] };
        let mut all = Vec::new();
        for (rank, id) in shown.iter().enumerate() {
            if let Some(entries) = self.check(self.store.entries(*id)) {
                all.extend(entries.into_iter().filter(|e| !e.entry.deleted).map(|e| (rank, e)));
            }
        }
        all.sort_by_key(|(rank, e)| (*rank, e.entry.group.to_lowercase(), e.entry.name.to_lowercase()));
        self.entries = all.into_iter().map(|(_, e)| e).collect();
        if self.selected.is_some_and(|id| self.entry(id).is_none()) {
            self.selected = None;
        }
        let mut counts = HashMap::new();
        for v in &self.vaults {
            if let Ok(entries) = self.store.entries(v.id) {
                let live: Vec<_> = entries.iter().filter(|e| !e.entry.deleted).collect();
                let attention = entries.iter().filter(|e| e.conflict.is_some() || (e.dirty && v.id != LOCAL_VAULT)).count();
                counts.insert(v.id, (live.len(), attention));
            }
        }
        self.vault_counts = counts;
    }

    /// The picked vault; `None` while showing all vaults.
    fn current(&self) -> Option<&CachedVault> {
        self.vaults.iter().find(|v| v.id == self.current_vault)
    }

    /// Where "+ New" and CSV import put entries: the picked vault, or Local when showing all.
    fn add_target(&self) -> Uuid {
        if self.current_vault == ALL_VAULTS { LOCAL_VAULT } else { self.current_vault }
    }

    fn can_add(&self) -> bool {
        let target = self.add_target();
        self.vaults.iter().any(|v| v.id == target && v.writable())
    }

    fn vault_of(&self, id: Uuid) -> Uuid {
        self.entry(id).map_or(self.current_vault, |e| e.entry.vault_id)
    }

    /// Whether the entry's own vault may be edited by this user.
    fn entry_writable(&self, id: Uuid) -> bool {
        let vault = self.vault_of(id);
        self.vaults.iter().any(|v| v.id == vault && v.writable())
    }

    fn entry(&self, id: Uuid) -> Option<&LocalEntry> {
        self.entries.iter().find(|e| e.entry.id == id)
    }

    fn who(&self) -> String {
        if self.settings.username.is_empty() { "local".into() } else { self.settings.username.clone() }
    }

    fn signed_in(&self) -> bool {
        self.settings.token.is_some()
    }

    // ---- sync ----

    fn start_sync(&mut self, ctx: &egui::Context) {
        if self.sync_rx.is_some() || !self.signed_in() {
            return;
        }
        self.last_sync_attempt = Some(Instant::now());
        let (tx, rx) = mpsc::channel();
        let store = self.store.clone();
        let api = self.api.clone();
        let ctx = ctx.clone();
        thread::spawn(move || {
            let _ = tx.send(sync::sync(&store, &api));
            ctx.request_repaint();
        });
        self.sync_rx = Some(rx);
    }

    fn poll_sync(&mut self) {
        let Some(received) = self.sync_rx.as_ref().map(|rx| rx.try_recv()) else { return };
        let result = match received {
            Ok(result) => result,
            Err(mpsc::TryRecvError::Empty) => return,
            Err(mpsc::TryRecvError::Disconnected) => Err(ApiError::Local("sync thread stopped".into())),
        };
        self.sync_rx = None;
        match result {
            Ok(report) => {
                self.conn = Conn::Online;
                self.last_sync_ok = Some(Local::now());
                let summary = report.summary();
                if report.conflicts > 0 || !report.rejected.is_empty() {
                    self.set_error(summary);
                } else {
                    self.set_status(summary);
                }
            }
            Err(ApiError::Unauthorized(_)) => {
                self.settings.token = None;
                self.save_settings();
                self.conn = Conn::SignedOut;
                self.set_error("Your session has expired. Sign in again to sync - cached data is still available.");
            }
            Err(ApiError::Network(e)) => {
                self.conn = Conn::Offline;
                self.set_error(format!("Server unreachable - working offline from cache ({e})"));
            }
            Err(e) => self.set_error(e.to_string()),
        }
        self.reload();
    }

    fn save_settings(&mut self) {
        let settings = self.settings.clone();
        let saved = self.store.save_settings(&settings);
        self.check(saved);
        self.api = Api::new(&self.settings.server_url, self.settings.token.clone());
    }

    // ---- in-session AI assistant ----

    /// Fetches the configured models in the background (for the model picker).
    fn load_ai_models(&mut self, ctx: &egui::Context) {
        self.ai_loaded = true;
        if !self.signed_in() {
            return;
        }
        let api = self.api.clone();
        let ctx = ctx.clone();
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            if let Ok(info) = api.ai_models() {
                let _ = tx.send(info.models);
                ctx.request_repaint();
            }
        });
        self.ai_models_rx = Some(rx);
    }

    fn poll_ai_models(&mut self) {
        if let Some(rx) = &self.ai_models_rx {
            match rx.try_recv() {
                Ok(models) => {
                    self.ai_models = models;
                    self.ai_models_rx = None;
                }
                Err(mpsc::TryRecvError::Disconnected) => self.ai_models_rx = None,
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
    }

    /// Sends the typed question to the assistant for the active session,
    /// gathering that session's context on the way out.
    fn assistant_send(&mut self, ctx: &egui::Context) {
        let Some(sid) = self.active_tab else { return };
        let question = self.assistant_input.trim().to_string();
        if question.is_empty() || self.assistant_pending.is_some() {
            return;
        }
        let Some(session) = self.sessions.iter().find(|s| s.id() == sid) else { return };
        let protocol = session.badge().to_string();
        let context = session.ai_context();
        let entry_id = session.entry_id();
        let host = self
            .entries
            .iter()
            .find(|e| e.entry.id == entry_id)
            .map(|e| if e.entry.host.is_empty() { e.entry.name.clone() } else { e.entry.host.clone() })
            .unwrap_or_default();
        let chat = self.assistant_chats.entry(sid).or_default();
        let history: Vec<AiChatTurn> = chat
            .iter()
            .filter_map(|m| match m.role {
                ChatRole::User => Some(AiChatTurn { role: "user".into(), content: m.text.clone() }),
                ChatRole::Assistant => Some(AiChatTurn { role: "assistant".into(), content: m.text.clone() }),
                ChatRole::Error => None,
            })
            .collect();
        chat.push(ChatMsg { role: ChatRole::User, text: question.clone() });
        self.assistant_input.clear();

        let req = AiChatRequest { model: self.assistant_model.clone(), protocol, host, context, history, question };
        let api = self.api.clone();
        let ctx = ctx.clone();
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let result = api.ai_chat(&req).map(|r| r.reply).map_err(|e| e.to_string());
            let _ = tx.send(result);
            ctx.request_repaint();
        });
        self.assistant_pending = Some((sid, rx));
    }

    fn poll_assistant(&mut self) {
        let Some((sid, rx)) = &self.assistant_pending else { return };
        let sid = *sid;
        match rx.try_recv() {
            Ok(result) => {
                self.assistant_pending = None;
                let chat = self.assistant_chats.entry(sid).or_default();
                match result {
                    Ok(reply) => chat.push(ChatMsg { role: ChatRole::Assistant, text: reply }),
                    Err(e) => chat.push(ChatMsg { role: ChatRole::Error, text: e }),
                }
            }
            Err(mpsc::TryRecvError::Disconnected) => self.assistant_pending = None,
            Err(mpsc::TryRecvError::Empty) => {}
        }
    }

    // ---- actions ----

    fn handle(&mut self, ctx: &egui::Context, action: Action) {
        match action {
            Action::SelectVault(id) => {
                self.select_mode = false;
                self.picked.clear();
                self.current_vault = id;
                self.selected = None;
                self.draft = None;
                self.copy_target = None;
                self.confirm_delete = None;
                self.reload();
            }
            Action::Select(id) => {
                if self.draft.is_none() {
                    self.active_tab = None;
                }
                if self.draft.is_none() && self.selected != Some(id) {
                    self.selected = Some(id);
                    self.reveal_password = false;
                    self.confirm_delete = None;
                    self.copy_target = None;
                }
            }
            Action::New => {
                let entry = Entry { vault_id: self.add_target(), ..Entry::default() };
                let mut draft = Draft::new(entry, true);
                draft.record_locked = self.recording_lock(&draft.entry);
                self.draft = Some(draft);
                self.active_tab = None;
            }
            Action::Edit => {
                if let Some(le) = self.selected.and_then(|id| self.entry(id)) {
                    let mut draft = Draft::new(le.entry.clone(), false);
                    draft.record_locked = self.recording_lock(&draft.entry);
                    self.draft = Some(draft);
                    self.active_tab = None;
                }
            }
            Action::Cancel => self.draft = None,
            Action::Save => self.save_draft(ctx),
            Action::Delete(id) => self.delete_entry(ctx, id),
            Action::Launch(id) => self.open_entry(ctx, id, None),
            Action::LaunchEmbedded(id) => self.open_entry(ctx, id, Some(true)),
            Action::LaunchExternal(id) => self.open_entry(ctx, id, Some(false)),
            Action::ActivateTab(tab) => self.active_tab = tab,
            Action::CloseSession(sid) => {
                if let Some(pos) = self.sessions.iter().position(|s| s.id() == sid) {
                    let session = self.sessions.remove(pos);
                    self.set_status(format!("Closed {}", session.title()));
                }
                if self.active_tab == Some(sid) {
                    self.active_tab = self.sessions.last().map(|s| s.id());
                }
            }
            Action::Reconnect(sid) => {
                if let Some(pos) = self.sessions.iter().position(|s| s.id() == sid) {
                    let old = self.sessions.remove(pos);
                    if old.is_files() {
                        self.open_files(ctx, old.entry_id());
                    } else {
                        self.open_entry(ctx, old.entry_id(), Some(true));
                    }
                }
            }
            Action::OpenFiles(id) => self.open_files(ctx, id),
            Action::ToggleGroup(vault, group) => {
                let key = (vault, group);
                if !self.collapsed_groups.remove(&key) {
                    self.collapsed_groups.insert(key);
                }
            }
            Action::SetEmbed(embed) => {
                self.settings.external_by_default = !embed;
                self.save_settings();
                self.set_status(if embed { "RDP, VNC and SSH now open in tabs" } else { "RDP, VNC and SSH now open in external programs" });
            }
            Action::SetSelectMode(on) => {
                self.select_mode = on;
                self.picked.clear();
            }
            Action::TogglePick(id) => {
                self.select_mode = true;
                if !self.picked.remove(&id) {
                    self.picked.insert(id);
                }
            }
            Action::PickAll(ids) => {
                self.select_mode = true;
                self.picked = ids.into_iter().collect();
            }
            Action::BulkPush { target, remove_source } => self.bulk_push(ctx, target, remove_source),
            Action::OpenAnother(id) => self.open_entry_new(ctx, id),
            Action::ToggleNav => self.nav_collapsed = !self.nav_collapsed,
            Action::ToggleDetach(sid) => {
                if !self.detached.remove(&sid) {
                    self.detached.insert(sid);
                    // A detached session is shown in its own window, not the main tabs.
                    if self.active_tab == Some(sid) {
                        self.active_tab = self.sessions.iter().find(|s| !self.detached.contains(&s.id())).map(|s| s.id());
                    }
                } else if self.sessions.iter().any(|s| s.id() == sid) {
                    self.active_tab = Some(sid);
                }
            }
            Action::ToggleAssistant => {
                self.assistant_open = !self.assistant_open;
                if self.assistant_open && !self.ai_loaded {
                    self.load_ai_models(ctx);
                }
            }
            Action::AssistantSend => self.assistant_send(ctx),
            Action::InsertToSession(sid, text) => {
                if let Some(s) = self.sessions.iter_mut().find(|s| s.id() == sid) {
                    if s.insert_text(&text) {
                        self.set_status("Inserted into the session — review it, then press Enter to run");
                    } else {
                        self.set_error("This session can't accept inserted text right now");
                    }
                }
            }
            Action::ToggleFullScreen => {
                self.focus_mode = !self.focus_mode;
                ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(self.focus_mode));
            }
            Action::SetRecordLocal(on) => {
                self.settings.record_local = on;
                self.save_settings();
                self.set_status(if on {
                    "Recording every SSH/RDP/VNC/web session of the Local vault (saved under %APPDATA%\\SystemDeck\\recordings)"
                } else {
                    "Local vault: only entries with recording switched on are recorded"
                });
            }
            Action::SetEntryRecording(id, on) => self.set_entry_recording(ctx, id, on),
            Action::OpenRecordingsFolder => {
                let dir = crate::store::recordings_dir();
                let _ = std::fs::create_dir_all(&dir);
                if let Err(e) = open::that(&dir) {
                    self.set_error(format!("Cannot open {}: {e}", dir.display()));
                }
            }
            Action::SetDark(dark) => {
                self.settings.dark_mode = dark;
                self.save_settings();
                crate::theme::apply(ctx, dark);
            }
            Action::AskDelete(id) => {
                self.selected = Some(id);
                self.active_tab = None;
                self.draft = None;
                self.confirm_delete = Some(id);
            }
            Action::CopyUser(id) => {
                if let Some(le) = self.entry(id) {
                    let user = if le.entry.domain.is_empty() {
                        le.entry.username.clone()
                    } else {
                        format!("{}\\{}", le.entry.domain, le.entry.username)
                    };
                    ctx.copy_text(user);
                    self.set_status("Username copied");
                }
            }
            Action::CopyPassword(id) => {
                if let Some(le) = self.entry(id) {
                    ctx.copy_text(le.entry.password.clone());
                    self.set_status("Password copied to clipboard");
                }
            }
            Action::CopyTo { id, target, remove_source } => self.copy_to(ctx, id, target, remove_source),
            Action::KeepMine(id) => {
                if let Some(Some(mut le)) = self.check(self.store.entry(self.vault_of(id), id)) {
                    if let Some(server) = le.conflict.take() {
                        le.base_revision = server.revision;
                        le.dirty = true;
                    }
                    let saved = self.store.put_entry(&le);
                    self.check(saved);
                    self.set_status("Keeping your version - it will overwrite the server copy on the next sync");
                    self.reload();
                    self.start_sync(ctx);
                }
            }
            Action::UseServer(id) => {
                if let Some(Some(mut le)) = self.check(self.store.entry(self.vault_of(id), id)) {
                    if let Some(server) = le.conflict.take() {
                        let result = if server.deleted {
                            self.store.remove_entry(server.vault_id, server.id)
                        } else {
                            self.store.put_entry(&LocalEntry::clean(server))
                        };
                        self.check(result);
                    }
                    self.set_status("Replaced your local edit with the server version");
                    self.reload();
                }
            }
            Action::SyncNow => {
                if self.signed_in() {
                    self.set_status("Syncing...");
                    self.start_sync(ctx);
                } else {
                    self.handle(ctx, Action::ShowLogin);
                }
            }
            Action::ShowLogin => {
                self.login = Some(LoginForm {
                    url: if self.settings.server_url.is_empty() { "http://localhost:8740".into() } else { self.settings.server_url.clone() },
                    username: self.settings.username.clone(),
                    password: String::new(),
                    error: None,
                });
            }
            Action::Logout => {
                let api = self.api.clone();
                thread::spawn(move || {
                    let _ = api.logout();
                });
                self.settings.token = None;
                self.settings.is_admin = false;
                self.save_settings();
                self.conn = Conn::SignedOut;
                self.admin = None;
                self.set_status("Signed out. Cached shared vaults stay available offline (read-only until you sign in).");
            }
            Action::OpenAdmin => self.admin = Some(AdminPanel::new(&self.api)),
            Action::ShowPasswordChange => self.password_form = Some(PasswordForm::default()),
            Action::ShowImport => self.import_path = Some(String::new()),
        }
    }

    /// Opens a connection. `embedded`: force tab (true) / external program
    /// (false); `None` follows the entry's display mode and the global setting.
    fn open_entry(&mut self, ctx: &egui::Context, id: Uuid, embedded: Option<bool>) {
        self.open_entry_ex(ctx, id, embedded, false);
    }

    /// Opens a new session of `id` even if one is already open.
    fn open_entry_new(&mut self, ctx: &egui::Context, id: Uuid) {
        self.open_entry_ex(ctx, id, Some(true), true);
    }

    fn open_entry_ex(&mut self, ctx: &egui::Context, id: Uuid, embedded: Option<bool>, force_new: bool) {
        let Some(le) = self.entry(id) else { return };
        let entry = le.entry.clone();
        let recorded = entry.kind.can_embed() && (entry.record_session || self.vault_requires_recording(entry.vault_id));
        if recorded && embedded == Some(false) {
            return self.set_error("This connection is recorded, so it can only open in a SystemDeck tab (external programs can't be recorded).");
        }
        // Websites can open in a tab (WebView2) or the default browser.
        if entry.kind == EntryKind::Web {
            let in_tab = embedded.unwrap_or(match entry.display {
                DisplayMode::Embedded => true,
                DisplayMode::External => false,
                DisplayMode::Default => !self.settings.external_by_default,
            });
            if in_tab {
                return self.open_browser(ctx, id, force_new);
            }
            match launch::launch(&entry) {
                Ok(msg) => self.set_status(msg),
                Err(e) => self.set_error(e),
            }
            return;
        }

        // Recorded sessions always open in a tab: an external program can't be recorded.
        let embedded = entry.kind.can_embed()
            && (recorded
                || embedded.unwrap_or(match entry.display {
                    DisplayMode::Embedded => true,
                    DisplayMode::External => false,
                    DisplayMode::Default => !self.settings.external_by_default,
                }));

        if embedded {
            if !force_new {
                if let Some(existing) = self.sessions.iter().find(|s| s.entry_id() == id && s.status().is_live() && !s.is_files()) {
                    self.active_tab = Some(existing.id());
                    return;
                }
            }
            let plan = match self.recording_plan(&entry) {
                Ok(plan) => plan,
                Err(msg) => return self.set_error(msg),
            };
            let session: Result<Box<dyn SessionView>, String> = match entry.kind {
                EntryKind::Rdp => Session::connect_rdp(&entry, self.session_px, ctx, plan).map(|s| Box::new(s) as _),
                EntryKind::Ssh => TermSession::connect(&entry, self.store.clone(), ctx, plan).map(|s| Box::new(s) as _),
                _ => Session::connect_vnc(&entry, ctx, plan).map(|s| Box::new(s) as _),
            };
            match session {
                Ok(session) => {
                    self.active_tab = Some(session.id());
                    self.sessions.push(session);
                    self.set_status(format!("Connecting to {}...", entry.name));
                }
                Err(e) => self.set_error(e),
            }
            return;
        }

        if entry.kind == EntryKind::Ssh && !entry.password.is_empty() {
            ctx.copy_text(entry.password.clone());
        }
        match launch::launch(&entry) {
            Ok(msg) if entry.kind == EntryKind::Ssh && !entry.password.is_empty() => {
                self.set_status(format!("{msg} - password copied to clipboard"))
            }
            Ok(msg) => self.set_status(msg),
            Err(e) => self.set_error(e),
        }
    }

    /// Opens a Website entry in an embedded WebView2 tab.
    fn open_browser(&mut self, ctx: &egui::Context, id: Uuid, force_new: bool) {
        if !force_new {
            if let Some(existing) = self.sessions.iter().find(|s| s.entry_id() == id && !s.is_files()) {
                self.active_tab = Some(existing.id());
                return;
            }
        }
        let Some(le) = self.entry(id) else { return };
        let entry = le.entry.clone();
        #[cfg(windows)]
        {
            let Some(hwnd) = self.window_handle else {
                return self.set_error("The window is still starting - try again in a moment.");
            };
            let plan = match self.recording_plan(&entry) {
                Ok(plan) => plan,
                Err(msg) => return self.set_error(msg),
            };
            match BrowserSession::connect(&entry, hwnd, ctx, plan) {
                Ok(session) => {
                    self.active_tab = Some(session.id());
                    self.sessions.push(Box::new(session));
                    self.set_status(format!("Opening {}...", entry.name));
                }
                Err(e) => self.set_error(e),
            }
        }
        #[cfg(not(windows))]
        {
            let _ = ctx;
            match launch::launch(&entry) {
                Ok(msg) => self.set_status(msg),
                Err(e) => self.set_error(e),
            }
        }
    }

    /// The vault policy: server vaults from the dashboard ("Record all remote
    /// sessions"), the Local vault from this PC's setting.
    fn vault_requires_recording(&self, vault_id: Uuid) -> bool {
        if vault_id == LOCAL_VAULT {
            return self.settings.record_local;
        }
        self.vaults.iter().any(|v| v.id == vault_id && v.record_ssh)
    }

    /// Why this entry's recording can't be switched off by this user, if so.
    /// Mirrors the server: once on in a server vault, only an administrator
    /// may turn it off; a vault-wide policy can't be overridden per entry.
    pub(crate) fn recording_lock(&self, entry: &Entry) -> Option<&'static str> {
        if !(entry.kind.can_embed() || entry.kind == EntryKind::Web) {
            return None;
        }
        if entry.vault_id == LOCAL_VAULT {
            return self.settings.record_local.then_some("all Local sessions are recorded (account menu)");
        }
        if self.vault_requires_recording(entry.vault_id) {
            return Some("required by this vault - set by an administrator");
        }
        (entry.record_session && !self.settings.is_admin).then_some("turned on on the server - only an administrator can turn it off")
    }

    /// Flips one entry's "Record sessions" without opening the editor.
    fn set_entry_recording(&mut self, ctx: &egui::Context, id: Uuid, on: bool) {
        let Some(Some(mut le)) = self.check(self.store.entry(self.vault_of(id), id)) else { return };
        if !on {
            if let Some(why) = self.recording_lock(&le.entry) {
                return self.set_error(format!("Recording can't be turned off: {why}."));
            }
        }
        le.entry.record_session = on;
        le.entry.updated_at = Utc::now();
        le.entry.updated_by = self.who();
        let server = le.entry.vault_id != LOCAL_VAULT;
        le.dirty = server;
        let name = le.entry.name.clone();
        let saved = self.store.put_entry(&le);
        if self.check(saved).is_none() {
            return;
        }
        self.reload();
        self.set_status(format!("{name}: recording {}", if on { "on" } else { "off" }));
        if server {
            self.start_sync(ctx);
        }
    }

    /// Whether (and where) a session of `entry` is recorded: on when the
    /// entry asks for it or its vault requires it. Server-vault sessions upload
    /// to the server; a required recording refuses to start while signed out.
    fn recording_plan(&self, entry: &Entry) -> Result<Option<RecordPlan>, String> {
        let required = self.vault_requires_recording(entry.vault_id);
        let recordable = entry.kind.can_embed() || entry.kind == EntryKind::Web;
        if !recordable || !(entry.record_session || required) {
            return Ok(None);
        }
        let (kind, protocol) = match entry.kind {
            EntryKind::Ssh => (systemdeck_core::api::RecordingKind::Ssh, "SSH"),
            EntryKind::Rdp => (systemdeck_core::api::RecordingKind::Screen, "RDP"),
            EntryKind::Web => (systemdeck_core::api::RecordingKind::Dom, "WEB"),
            _ => (systemdeck_core::api::RecordingKind::Screen, "VNC"),
        };
        let upload = (entry.vault_id != LOCAL_VAULT && self.signed_in()).then(|| {
            (
                self.api.clone(),
                systemdeck_core::api::RecordingStart {
                    vault_id: entry.vault_id,
                    entry_id: entry.id,
                    entry_name: entry.name.clone(),
                    host: entry.host.clone(),
                    login: entry.username.clone(),
                    cols: 80,
                    rows: 24,
                    kind,
                    protocol: protocol.into(),
                },
            )
        });
        if required && upload.is_none() && entry.vault_id != LOCAL_VAULT {
            return Err("This vault requires sessions to be recorded on the server. Sign in to the server first.".into());
        }
        Ok(Some(RecordPlan::new(upload, &entry.name)))
    }

    fn open_files(&mut self, ctx: &egui::Context, id: Uuid) {
        let Some(le) = self.entry(id) else { return };
        let entry = le.entry.clone();
        match FileSession::connect(&entry, self.store.clone(), ctx) {
            Ok(session) => {
                self.active_tab = Some(session.id());
                self.sessions.push(Box::new(session));
                self.set_status(format!("Opening files on {}...", entry.name));
            }
            Err(e) => self.set_error(e),
        }
    }

    fn save_draft(&mut self, ctx: &egui::Context) {
        let Some(draft) = &self.draft else { return };
        let mut entry = match draft.build() {
            Ok(e) => e,
            Err(msg) => return self.set_error(msg),
        };
        let vault = entry.vault_id;
        let existing = self.check(self.store.entry(vault, entry.id)).flatten();
        entry.updated_at = Utc::now();
        entry.updated_by = self.who();
        let id = entry.id;
        let local_entry = LocalEntry {
            entry,
            dirty: vault != LOCAL_VAULT,
            base_revision: existing.as_ref().map_or(0, |e| e.base_revision),
            conflict: existing.and_then(|e| e.conflict),
        };
        let saved = self.store.put_entry(&local_entry);
        if self.check(saved).is_none() {
            return;
        }
        self.draft = None;
        self.selected = Some(id);
        self.reload();
        if vault == LOCAL_VAULT {
            self.set_status("Saved to the local vault");
        } else if self.signed_in() {
            self.set_status("Saved - pushing to server...");
            self.start_sync(ctx);
        } else {
            self.set_status("Saved offline - it will be pushed when you sign in");
        }
    }

    fn delete_entry(&mut self, ctx: &egui::Context, id: Uuid) {
        let Some(Some(mut le)) = self.check(self.store.entry(self.vault_of(id), id)) else { return };
        let result = if le.entry.vault_id == LOCAL_VAULT || le.base_revision == 0 {
            self.store.remove_entry(le.entry.vault_id, id)
        } else {
            // Tombstone, so the deletion reaches the server and other clients.
            le.entry.deleted = true;
            le.entry.updated_at = Utc::now();
            le.entry.updated_by = self.who();
            le.dirty = true;
            self.store.put_entry(&le)
        };
        if self.check(result).is_some() {
            self.selected = None;
            self.confirm_delete = None;
            self.set_status(format!("Deleted '{}'", le.entry.name));
            self.reload();
            self.start_sync(ctx);
        }
    }

    /// Copies (or moves) an entry into another vault. Copying a local entry
    /// into a server vault is how private entries get shared with a team.
    /// Copies (or moves) every picked entry into `target` and syncs once.
    /// Entries that already exist there (same name, host and port) are skipped.
    fn bulk_push(&mut self, ctx: &egui::Context, target: Uuid, remove_source: bool) {
        let target_name = self.vaults.iter().find(|v| v.id == target).map(|v| v.name.clone()).unwrap_or_default();
        let key = |e: &Entry| (e.name.trim().to_lowercase(), e.host.trim().to_lowercase(), e.port);
        let Some(existing) = self.check(self.store.entries(target)) else { return };
        let mut present: HashSet<_> = existing.iter().filter(|e| !e.entry.deleted).map(|e| key(&e.entry)).collect();
        let sources: Vec<LocalEntry> = self.entries.iter().filter(|e| self.picked.contains(&e.entry.id)).cloned().collect();
        let (mut pushed, mut skipped) = (0, 0);
        for source in &sources {
            if !present.insert(key(&source.entry)) {
                skipped += 1;
                continue;
            }
            let entry = Entry {
                id: Uuid::new_v4(),
                vault_id: target,
                revision: 0,
                updated_at: Utc::now(),
                updated_by: self.who(),
                ..source.entry.clone()
            };
            let saved = self.store.put_entry(&LocalEntry { entry, dirty: target != LOCAL_VAULT, base_revision: 0, conflict: None });
            if self.check(saved).is_none() {
                return;
            }
            pushed += 1;
        }
        if remove_source {
            for source in &sources {
                let id = source.entry.id;
                let result = if source.entry.vault_id == LOCAL_VAULT || source.base_revision == 0 {
                    self.store.remove_entry(source.entry.vault_id, id)
                } else {
                    let mut tomb = source.clone();
                    tomb.entry.deleted = true;
                    tomb.entry.updated_at = Utc::now();
                    tomb.dirty = true;
                    self.store.put_entry(&tomb)
                };
                self.check(result);
            }
            self.selected = None;
        }
        self.select_mode = false;
        self.picked.clear();
        self.reload();
        let verb = if remove_source { "Moved" } else { "Pushed" };
        let mut msg = format!("{verb} {pushed} entr{} to {target_name}", if pushed == 1 { "y" } else { "ies" });
        if skipped > 0 {
            msg += &format!(" ({skipped} already there, skipped)");
        }
        if target != LOCAL_VAULT && self.signed_in() {
            self.set_status(format!("{msg} - syncing..."));
            self.start_sync(ctx);
        } else if target != LOCAL_VAULT {
            self.set_status(format!("{msg} - will upload when you sign in"));
        } else {
            self.set_status(msg);
        }
    }

    fn copy_to(&mut self, ctx: &egui::Context, id: Uuid, target: Uuid, remove_source: bool) {
        let Some(source) = self.entry(id).cloned() else { return };
        let target_name = self.vaults.iter().find(|v| v.id == target).map(|v| v.name.clone()).unwrap_or_default();
        let entry = Entry {
            id: Uuid::new_v4(),
            vault_id: target,
            revision: 0,
            updated_at: Utc::now(),
            updated_by: self.who(),
            ..source.entry.clone()
        };
        let copy = LocalEntry { entry, dirty: target != LOCAL_VAULT, base_revision: 0, conflict: None };
        let saved = self.store.put_entry(&copy);
        if self.check(saved).is_none() {
            return;
        }
        if remove_source {
            self.delete_entry(ctx, id);
        }
        self.copy_target = None;
        self.reload();
        let verb = if remove_source { "Moved" } else { "Copied" };
        if target != LOCAL_VAULT && self.signed_in() {
            self.set_status(format!("{verb} '{}' to {target_name} - pushing to server...", source.entry.name));
            self.start_sync(ctx);
        } else if target != LOCAL_VAULT {
            self.set_status(format!("{verb} '{}' to {target_name} - it will be pushed when you sign in", source.entry.name));
        } else {
            self.set_status(format!("{verb} '{}' to {target_name}", source.entry.name));
        }
    }

    // ---- UI ----

    fn login_window(&mut self, ctx: &egui::Context) {
        let Some(form) = &mut self.login else { return };
        let mut open = true;
        let mut submit = false;
        egui::Window::new("Connect to SystemDeck server")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .open(&mut open)
            .show(ctx, |ui| {
                egui::Grid::new("login").num_columns(2).spacing([10.0, 8.0]).show(ui, |ui| {
                    ui.label("Server");
                    ui.add(egui::TextEdit::singleline(&mut form.url).hint_text("http://server:8740").desired_width(260.0));
                    ui.end_row();
                    ui.label("Username");
                    ui.add(egui::TextEdit::singleline(&mut form.username).desired_width(260.0));
                    ui.end_row();
                    ui.label("Password");
                    let resp = ui.add(egui::TextEdit::singleline(&mut form.password).password(true).desired_width(260.0));
                    if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                        submit = true;
                    }
                    ui.end_row();
                });
                if let Some(err) = &form.error {
                    ui.colored_label(ERROR, err);
                }
                if form.url.trim_start().starts_with("http://")
                    && !form.url.contains("localhost")
                    && !form.url.contains("127.0.0.1")
                {
                    ui.label(
                        RichText::new("⚠ Plain http:// sends passwords unencrypted. Use https:// (reverse proxy) or a VPN such as Tailscale.")
                            .small()
                            .color(WARN),
                    );
                }
                ui.add_space(4.0);
                if ui.button("Sign in").clicked() {
                    submit = true;
                }
            });
        if !open {
            self.login = None;
            return;
        }
        if !submit {
            return;
        }

        let (url, username, password) = (normalize_url(&form.url), form.username.trim().to_lowercase(), form.password.clone());
        match Api::new(&url, None).login(&username, &password) {
            Ok(resp) => {
                if !self.settings.server_url.is_empty() && self.settings.server_url != url {
                    // Different server: drop the old server's cached vaults.
                    for v in self.vaults.clone().into_iter().filter(|v| !v.is_local()) {
                        let removed = self.store.remove_vault(v.id);
                        self.check(removed);
                    }
                }
                self.settings.server_url = url;
                self.settings.username = resp.user.username;
                self.settings.display_name = resp.user.display_name;
                self.settings.is_admin = resp.user.is_admin;
                self.settings.token = Some(resp.token);
                self.save_settings();
                self.login = None;
                self.conn = Conn::Online;
                self.set_status("Signed in - syncing...");
                self.reload();
                self.start_sync(ctx);
            }
            Err(e) => {
                if let Some(form) = &mut self.login {
                    form.error = Some(e.to_string());
                    form.password.clear();
                }
            }
        }
    }

    fn password_window(&mut self, ctx: &egui::Context) {
        let Some(form) = &mut self.password_form else { return };
        let mut open = true;
        let mut submit = false;
        egui::Window::new("Change password")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .open(&mut open)
            .show(ctx, |ui| {
                egui::Grid::new("pw").num_columns(2).spacing([10.0, 8.0]).show(ui, |ui| {
                    ui.label("Current password");
                    ui.add(egui::TextEdit::singleline(&mut form.old).password(true));
                    ui.end_row();
                    ui.label("New password");
                    ui.add(egui::TextEdit::singleline(&mut form.new).password(true));
                    ui.end_row();
                    ui.label("Confirm");
                    ui.add(egui::TextEdit::singleline(&mut form.confirm).password(true));
                    ui.end_row();
                });
                if let Some(err) = &form.error {
                    ui.colored_label(ERROR, err);
                }
                if ui.button("Change password").clicked() {
                    submit = true;
                }
            });
        if !open {
            self.password_form = None;
            return;
        }
        if !submit {
            return;
        }
        if form.new != form.confirm {
            form.error = Some("New passwords do not match".into());
            return;
        }
        match self.api.change_password(&form.old, &form.new) {
            Ok(_) => {
                self.password_form = None;
                self.set_status("Password changed");
            }
            Err(e) => {
                if let Some(form) = &mut self.password_form {
                    form.error = Some(e.to_string());
                }
            }
        }
    }

    fn import_window(&mut self, ctx: &egui::Context) {
        let target = self.add_target();
        let Some(path) = &mut self.import_path else { return };
        let vault_name = self.vaults.iter().find(|v| v.id == target).map_or(String::new(), |v| v.name.clone());
        let mut open = true;
        let mut submit = false;
        egui::Window::new("Import CSV")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .open(&mut open)
            .show(ctx, |ui| {
                ui.label(format!("Entries are added to the vault \"{vault_name}\"."));
                ui.label(
                    RichText::new("Recognised columns: Display Name/Name, Host, Port, Folder/Group, Username, Password, Domain, URL, Description/Notes, Type, Tags. Rows already in the vault are skipped.")
                        .small()
                        .weak(),
                );
                ui.horizontal(|ui| {
                    ui.label("File");
                    ui.add(egui::TextEdit::singleline(path).hint_text(r"C:\path\to\export.csv").desired_width(380.0));
                });
                ui.label(RichText::new("Tip: in Explorer, Shift + right-click the file → Copy as path.").small().weak());
                if ui.button("Import").clicked() {
                    submit = true;
                }
            });
        if !open {
            self.import_path = None;
            return;
        }
        if !submit {
            return;
        }
        let file = std::path::PathBuf::from(path.trim().trim_matches('"'));
        match crate::import::import_csv(&self.store, &file, target, &self.who()) {
            Ok(report) => {
                self.import_path = None;
                self.reload();
                if target != LOCAL_VAULT && report.added > 0 {
                    self.start_sync(ctx);
                }
                self.set_status(report.summary());
            }
            Err(e) => self.set_error(format!("Import failed: {e:#}")),
        }
    }

    fn admin_window(&mut self, ctx: &egui::Context) {
        let Some(panel) = &mut self.admin else { return };
        let (open, changed) = panel.show(ctx, &self.api);
        if !open {
            self.admin = None;
        }
        if changed {
            self.start_sync(ctx);
        }
    }
}

fn generate_password() -> String {
    const SETS: [&[u8]; 4] = [b"ABCDEFGHJKLMNPQRSTUVWXYZ", b"abcdefghijkmnopqrstuvwxyz", b"23456789", b"!@#$%^&*-_=+?"];
    let mut rng = rand::thread_rng();
    let all: Vec<u8> = SETS.concat();
    // At least one character from every set, the rest from all of them.
    let mut chars: Vec<u8> = SETS.iter().map(|s| *s.choose(&mut rng).unwrap()).collect();
    while chars.len() < 20 {
        chars.push(all[rng.gen_range(0..all.len())]);
    }
    chars.shuffle(&mut rng);
    String::from_utf8(chars).expect("ASCII")
}

fn editor_ui(ui: &mut egui::Ui, d: &mut Draft, actions: &mut Vec<Action>) {
    ui.heading(if d.is_new { "New entry" } else { "Edit entry" });
    ui.add_space(6.0);
    let width = ui.available_width() - 90.0;
    egui::Grid::new("editor").num_columns(2).spacing([10.0, 6.0]).show(ui, |ui| {
        ui.label("Type");
        egui::ComboBox::from_id_salt("kind").selected_text(d.entry.kind.label()).show_ui(ui, |ui| {
            for kind in EntryKind::ALL {
                ui.selectable_value(&mut d.entry.kind, kind, format!("{}  {}", kind.badge(), kind.label()));
            }
        });
        ui.end_row();

        ui.label("Name");
        ui.add(egui::TextEdit::singleline(&mut d.entry.name).desired_width(width));
        ui.end_row();

        ui.label("Group");
        ui.add(egui::TextEdit::singleline(&mut d.entry.group).hint_text("e.g. Servers/Production").desired_width(width));
        ui.end_row();

        let kind = d.entry.kind;
        if kind == EntryKind::RemoteSupport {
            ui.label("RustDesk ID");
            ui.add(egui::TextEdit::singleline(&mut d.entry.host).hint_text("9-digit ID of the remote PC").desired_width(width));
            ui.end_row();
        } else if kind.has_host() || kind == EntryKind::Web {
            ui.label("Host");
            ui.add(egui::TextEdit::singleline(&mut d.entry.host).hint_text("hostname or IP").desired_width(width));
            ui.end_row();
        }
        if kind.has_host() {
            ui.label("Port");
            let hint = match kind {
                EntryKind::Serial => "baud, e.g. 9600".to_string(),
                _ => kind.default_port().map(|p| format!("default {p}")).unwrap_or_default(),
            };
            ui.add(egui::TextEdit::singleline(&mut d.port).hint_text(hint).desired_width(80.0));
            ui.end_row();
        }
        if kind == EntryKind::Web {
            ui.label("URL");
            ui.add(egui::TextEdit::singleline(&mut d.entry.url).hint_text("https://...").desired_width(width));
            ui.end_row();
        }
        if kind.has_credentials() {
            if kind == EntryKind::Rdp || kind == EntryKind::Credential {
                ui.label("Domain");
                ui.add(egui::TextEdit::singleline(&mut d.entry.domain).desired_width(width));
                ui.end_row();
            }
            if kind != EntryKind::RemoteSupport {
                ui.label("Username");
                ui.add(egui::TextEdit::singleline(&mut d.entry.username).desired_width(width));
                ui.end_row();
            }

            ui.label(if kind == EntryKind::RemoteSupport { "Password (unattended)" } else { "Password" });
            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(&mut d.entry.password).password(!d.show_password).desired_width(width - 60.0));
                if ui.small_button("👁").on_hover_text("Show / hide").clicked() {
                    d.show_password = !d.show_password;
                }
                if ui.small_button("🎲").on_hover_text("Generate a strong password").clicked() {
                    d.entry.password = generate_password();
                    d.show_password = true;
                }
            });
            ui.end_row();
        }

        if kind.can_embed() || kind == EntryKind::Web {
            ui.label("Open in");
            egui::ComboBox::from_id_salt("display").selected_text(d.entry.display.label()).show_ui(ui, |ui| {
                for mode in DisplayMode::ALL {
                    ui.selectable_value(&mut d.entry.display, mode, mode.label());
                }
            });
            ui.end_row();
        }

        if matches!(kind, EntryKind::Rdp | EntryKind::Vnc) {
            display_settings_ui(ui, &mut d.entry.screen, kind);
        }
        if kind.can_embed() || kind == EntryKind::Web {
            ui.label("Recording");
            let what = match kind {
                EntryKind::Ssh => "Record sessions (keystrokes + output)",
                EntryKind::Web => "Record sessions (screen + pages visited)",
                _ => "Record sessions (screen, typed text, clicks)",
            };
            match d.record_locked {
                Some(why) => {
                    ui.horizontal(|ui| {
                        let mut on = true;
                        ui.add_enabled(false, egui::Checkbox::new(&mut on, what));
                        ui.label(RichText::new(format!("({why})")).small().weak());
                    });
                }
                None => {
                    ui.checkbox(&mut d.entry.record_session, what).on_hover_text(
                        "Recorded sessions always open in a SystemDeck tab. Server vaults: uploaded to the server. \
                         Local vault: saved under %APPDATA%\\SystemDeck\\recordings.",
                    );
                }
            }
            ui.end_row();
        }

        ui.label("Tags");
        ui.add(egui::TextEdit::singleline(&mut d.tags).hint_text("comma separated").desired_width(width));
        ui.end_row();
    });
    ui.label("Notes");
    ui.add(egui::TextEdit::multiline(&mut d.entry.notes).desired_rows(5).desired_width(f32::INFINITY));
    ui.add_space(8.0);
    ui.horizontal(|ui| {
        if ui.button("💾 Save").clicked() {
            actions.push(Action::Save);
        }
        if ui.button("Cancel").clicked() {
            actions.push(Action::Cancel);
        }
    });
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        #[cfg(windows)]
        if self.window_handle.is_none() {
            self.window_handle = crate::sessions::window_hwnd(frame);
        }
        let _ = &frame;
        self.poll_sync();
        self.poll_ai_models();
        self.poll_assistant();
        let due = self.last_sync_attempt.is_none_or(|t| t.elapsed() >= AUTO_SYNC_EVERY);
        if self.signed_in() && self.sync_rx.is_none() && due {
            self.start_sync(ctx);
        }
        ctx.request_repaint_after(Duration::from_secs(5));

        let mut actions = Vec::new();
        // Leave full-screen mode when its session goes away.
        if self.focus_mode && self.active_tab.is_none() {
            self.focus_mode = false;
            ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(false));
        }
        if !self.focus_mode {
            if !self.nav_collapsed {
                self.list_panel(ctx, &mut actions);
            }
            self.status_line(ctx);
        }
        self.assistant_panel(ctx, &mut actions);
        self.content_panel(ctx, &mut actions);
        self.detached_windows(ctx, &mut actions);
        self.login_window(ctx);
        self.password_window(ctx);
        self.import_window(ctx);
        self.admin_window(ctx);

        for action in actions {
            self.handle(ctx, action);
        }
    }
}

/// "Display" rows of the entry editor (inside its two-column grid).
fn display_settings_ui(ui: &mut egui::Ui, s: &mut DisplaySettings, kind: EntryKind) {
    ui.label(RichText::new("DISPLAY").small().weak().strong());
    ui.end_row();

    if kind == EntryKind::Rdp {
        ui.label("Resolution");
        ui.horizontal(|ui| {
            let is_preset = |r: Resolution| matches!(r, Resolution::Fixed(w, h) if Resolution::PRESETS.contains(&(w, h)));
            let current = if s.resolution == Resolution::Fit || is_preset(s.resolution) { s.resolution.label() } else { "Custom".to_string() };
            egui::ComboBox::from_id_salt("resolution").selected_text(current).show_ui(ui, |ui| {
                ui.selectable_value(&mut s.resolution, Resolution::Fit, Resolution::Fit.label());
                for (w, h) in Resolution::PRESETS {
                    ui.selectable_value(&mut s.resolution, Resolution::Fixed(w, h), Resolution::Fixed(w, h).label());
                }
                let custom = matches!(s.resolution, Resolution::Fixed(..)) && !is_preset(s.resolution);
                if ui.selectable_label(custom, "Custom...").clicked() && !custom {
                    s.resolution = Resolution::Fixed(1024, 768);
                }
            });
            if let Resolution::Fixed(w, h) = &mut s.resolution {
                if !Resolution::PRESETS.contains(&(*w, *h)) {
                    ui.add(egui::DragValue::new(w).range(640..=8192).suffix(" w"));
                    ui.add(egui::DragValue::new(h).range(480..=8192).suffix(" h"));
                }
            }
        });
        ui.end_row();

        ui.label("Colours");
        egui::ComboBox::from_id_salt("colour_depth").selected_text(s.color_depth.label()).show_ui(ui, |ui| {
            for depth in ColorDepth::ALL {
                ui.selectable_value(&mut s.color_depth, depth, depth.label());
            }
        });
        ui.end_row();

        ui.label("Quality");
        egui::ComboBox::from_id_salt("quality").selected_text(s.quality.label()).show_ui(ui, |ui| {
            for q in Quality::ALL {
                ui.selectable_value(&mut s.quality, q, q.label());
            }
        })
        .response
        .on_hover_text(
            "Best: wallpaper, themes, font smoothing, no compression (LAN).\n\
             Balanced: no wallpaper or animations.\n\
             Fast: 16-bit colour and all effects off (VPN / slow links).\n\
             Visual-effect flags apply to external mstsc; embedded sessions use colour depth and compression.",
        );
        ui.end_row();
    }

    ui.label("Scaling");
    egui::ComboBox::from_id_salt("scaling").selected_text(s.scaling.label()).show_ui(ui, |ui| {
        for mode in Scaling::ALL {
            ui.selectable_value(&mut s.scaling, mode, mode.label());
        }
    });
    ui.end_row();

    ui.label("");
    ui.horizontal(|ui| {
        ui.checkbox(&mut s.smooth, "Smooth scaling");
        if kind == EntryKind::Rdp {
            ui.checkbox(&mut s.full_screen, "Full screen (external mstsc)");
        }
    });
    ui.end_row();
}
