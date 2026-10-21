use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// What an entry represents and how the client launches it.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum EntryKind {
    #[default]
    Rdp,
    Ssh,
    Vnc,
    Telnet,
    Serial,
    Web,
    /// Attended remote support via a self-hosted RustDesk. `host` holds the
    /// RustDesk ID; `password` is an optional unattended password.
    RemoteSupport,
    Credential,
    Note,
}

impl EntryKind {
    pub const ALL: [EntryKind; 9] = [
        EntryKind::Rdp,
        EntryKind::Ssh,
        EntryKind::Vnc,
        EntryKind::Telnet,
        EntryKind::Serial,
        EntryKind::Web,
        EntryKind::RemoteSupport,
        EntryKind::Credential,
        EntryKind::Note,
    ];

    pub fn label(self) -> &'static str {
        match self {
            EntryKind::Rdp => "Remote Desktop (RDP)",
            EntryKind::Ssh => "SSH",
            EntryKind::Vnc => "VNC",
            EntryKind::Telnet => "Telnet",
            EntryKind::Serial => "Serial console",
            EntryKind::Web => "Website",
            EntryKind::RemoteSupport => "Remote support (RustDesk)",
            EntryKind::Credential => "Credential",
            EntryKind::Note => "Secure note",
        }
    }

    /// Short text badge (renders with any font, unlike emoji).
    pub fn badge(self) -> &'static str {
        match self {
            EntryKind::Rdp => "RDP",
            EntryKind::Ssh => "SSH",
            EntryKind::Vnc => "VNC",
            EntryKind::Telnet => "TEL",
            EntryKind::Serial => "COM",
            EntryKind::Web => "WEB",
            EntryKind::RemoteSupport => "SUP",
            EntryKind::Credential => "KEY",
            EntryKind::Note => "TXT",
        }
    }

    /// Can open inside SystemDeck instead of an external program.
    pub fn can_embed(self) -> bool {
        matches!(self, EntryKind::Rdp | EntryKind::Vnc | EntryKind::Ssh)
    }

    pub fn default_port(self) -> Option<u16> {
        match self {
            EntryKind::Rdp => Some(3389),
            EntryKind::Ssh => Some(22),
            EntryKind::Vnc => Some(5900),
            EntryKind::Telnet => Some(23),
            _ => None,
        }
    }

    pub fn has_host(self) -> bool {
        matches!(self, EntryKind::Rdp | EntryKind::Ssh | EntryKind::Vnc | EntryKind::Telnet | EntryKind::Serial)
    }

    pub fn has_credentials(self) -> bool {
        !matches!(self, EntryKind::Note)
    }
}

/// Where a connection opens (RDM's `ConnectionDisplayMode`).
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum DisplayMode {
    /// Follow the client's global setting.
    #[default]
    Default,
    Embedded,
    External,
}

impl DisplayMode {
    pub const ALL: [DisplayMode; 3] = [DisplayMode::Default, DisplayMode::Embedded, DisplayMode::External];

    pub fn label(self) -> &'static str {
        match self {
            DisplayMode::Default => "Default",
            DisplayMode::Embedded => "Embedded (tab)",
            DisplayMode::External => "External program",
        }
    }
}

/// Remote desktop size.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Resolution {
    /// Match the tab (embedded) or the monitor (external).
    #[default]
    Fit,
    Fixed(u16, u16),
}

impl Resolution {
    pub const PRESETS: [(u16, u16); 6] = [(1280, 720), (1366, 768), (1600, 900), (1920, 1080), (2560, 1440), (3840, 2160)];

    pub fn label(self) -> String {
        match self {
            Resolution::Fit => "Fit to window".into(),
            Resolution::Fixed(w, h) => format!("{w} x {h}"),
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ColorDepth {
    Bits16,
    Bits24,
    #[default]
    Bits32,
}

impl ColorDepth {
    pub const ALL: [ColorDepth; 3] = [ColorDepth::Bits16, ColorDepth::Bits24, ColorDepth::Bits32];

    pub fn bits(self) -> u32 {
        match self {
            ColorDepth::Bits16 => 16,
            ColorDepth::Bits24 => 24,
            ColorDepth::Bits32 => 32,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            ColorDepth::Bits16 => "High colour (16-bit)",
            ColorDepth::Bits24 => "True colour (24-bit)",
            ColorDepth::Bits32 => "Highest quality (32-bit)",
        }
    }
}

/// Experience preset: visual effects vs. bandwidth.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Quality {
    /// Everything on: wallpaper, themes, font smoothing, composition (LAN).
    Best,
    #[default]
    Balanced,
    /// Minimal effects for slow or high-latency links.
    Fast,
}

impl Quality {
    pub const ALL: [Quality; 3] = [Quality::Best, Quality::Balanced, Quality::Fast];

    pub fn label(self) -> &'static str {
        match self {
            Quality::Best => "Best quality (LAN)",
            Quality::Balanced => "Balanced",
            Quality::Fast => "Fast (slow connection)",
        }
    }
}

/// How the remote screen is fitted into the tab.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Scaling {
    /// Shrink to fit, never enlarge (sharpest).
    #[default]
    Fit,
    /// Fill the tab, keeping the aspect ratio.
    Stretch,
    /// 1:1 pixels with scroll bars.
    Actual,
}

impl Scaling {
    pub const ALL: [Scaling; 3] = [Scaling::Fit, Scaling::Stretch, Scaling::Actual];

    pub fn label(self) -> &'static str {
        match self {
            Scaling::Fit => "Shrink to fit",
            Scaling::Stretch => "Stretch to fill",
            Scaling::Actual => "Actual size (1:1)",
        }
    }
}

/// Display options for RDP entries (RDM's "Display" tab).
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(default)]
pub struct DisplaySettings {
    pub resolution: Resolution,
    pub color_depth: ColorDepth,
    pub quality: Quality,
    pub scaling: Scaling,
    /// Smooth (bilinear) scaling; off = crisp nearest-neighbour pixels.
    pub smooth: bool,
    /// External mstsc: start full screen.
    pub full_screen: bool,
}

impl Default for DisplaySettings {
    fn default() -> Self {
        Self {
            resolution: Resolution::Fit,
            color_depth: ColorDepth::Bits32,
            quality: Quality::Balanced,
            scaling: Scaling::Fit,
            smooth: true,
            full_screen: false,
        }
    }
}

/// A connection or credential record. Entries live in exactly one vault.
///
/// `revision` is assigned by the server on every accepted write (a global,
/// monotonically increasing sequence number) and is what sync uses for
/// change detection and optimistic concurrency.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct Entry {
    pub id: Uuid,
    pub vault_id: Uuid,
    pub kind: EntryKind,
    pub name: String,
    /// Folder path, e.g. `Servers/Production`.
    pub group: String,
    pub host: String,
    pub port: Option<u16>,
    pub username: String,
    pub domain: String,
    pub password: String,
    pub url: String,
    pub notes: String,
    pub tags: Vec<String>,
    pub display: DisplayMode,
    /// Resolution / colour / quality for RDP (VNC uses scaling + smoothing).
    pub screen: DisplaySettings,
    /// Record SSH sessions of this entry (the vault can also require it).
    pub record_session: bool,
    pub revision: u64,
    pub deleted: bool,
    pub updated_at: DateTime<Utc>,
    pub updated_by: String,
}

impl Default for Entry {
    fn default() -> Self {
        Self {
            id: Uuid::new_v4(),
            vault_id: Uuid::nil(),
            kind: EntryKind::default(),
            name: String::new(),
            group: String::new(),
            host: String::new(),
            port: None,
            username: String::new(),
            domain: String::new(),
            password: String::new(),
            url: String::new(),
            notes: String::new(),
            tags: Vec::new(),
            display: DisplayMode::Default,
            screen: DisplaySettings::default(),
            record_session: false,
            revision: 0,
            deleted: false,
            updated_at: Utc::now(),
            updated_by: String::new(),
        }
    }
}

impl Entry {
    /// Case-insensitive match against the searchable fields.
    pub fn matches(&self, query: &str) -> bool {
        let q = query.trim().to_lowercase();
        if q.is_empty() {
            return true;
        }
        [&self.name, &self.host, &self.username, &self.group, &self.url]
            .iter()
            .any(|f| f.to_lowercase().contains(&q))
            || self.tags.iter().any(|t| t.to_lowercase().contains(&q))
    }
}

/// Access level on a vault. Ordered so `max()` yields the strongest grant.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub enum Role {
    #[default]
    Read,
    Write,
    Manage,
}

impl Role {
    pub const ALL: [Role; 3] = [Role::Read, Role::Write, Role::Manage];

    pub fn label(self) -> &'static str {
        match self {
            Role::Read => "Read only",
            Role::Write => "Read & write",
            Role::Manage => "Manage",
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Hash)]
#[serde(tag = "type", content = "id")]
pub enum Principal {
    User(String),
    Team(Uuid),
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Grant {
    pub principal: Principal,
    pub role: Role,
}

/// A shared container of entries on the server, with its access list.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Vault {
    pub id: Uuid,
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub grants: Vec<Grant>,
    /// Every SSH session to entries in this vault is recorded and uploaded.
    #[serde(default)]
    pub record_ssh: bool,
    pub created_at: DateTime<Utc>,
}

/// What a regular user sees about a vault they can access.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct VaultSummary {
    pub id: Uuid,
    pub name: String,
    pub description: String,
    pub role: Role,
    #[serde(default)]
    pub record_ssh: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Team {
    pub id: Uuid,
    pub name: String,
    pub members: Vec<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct UserInfo {
    pub username: String,
    pub display_name: String,
    pub is_admin: bool,
    pub disabled: bool,
}
