# SystemDeck

A remote-connection and credential manager for teams, written in Rust and inspired by
Remote Desktop Manager. Staff keep connections (RDP, SSH, VNC, web, credentials,
secure notes) in a desktop app that works offline, and share them with their team
through a central server.

```
 ┌────────────── Desktop client (systemdeck.exe) ──────────────┐        ┌──── Server (systemdeck-server.exe) ────┐
 │  Local vault ── private, never leaves the PC               │        │  users · teams · vaults · access list │
 │  Cached team vaults ── usable offline                      │ HTTPS  │  entries (encrypted at rest)          │
 │  Unsynced edits queue ── pushed when connected  ───────────┼───────►│  revision counter → conflict checks   │
 │  Launches mstsc / ssh / vncviewer / browser                │◄───────┤  /changes?since=cursor                │
 └────────────────────────────────────────────────────────────┘        └───────────────────────────────────────┘
```

## Layout

| Folder | What it is |
|---|---|
| `server/` | **Server app**: axum HTTP API, embedded redb database, web admin dashboard (`server/src/web/`) |
| `client/` | **Desktop app**: egui UI, encrypted local store, sync engine, embedded RDP/VNC/SSH/SFTP |
| `shared/` | Library compiled into both (not a separate program): data model, API messages, encryption |
| `release/` | Output of `build.ps1`: `SystemDeck-Server\` and `SystemDeck-Client\`, ready to copy to their machines |

## Build

The project uses the `x86_64-pc-windows-gnu` toolchain and needs MinGW binutils on PATH
(already installed on this machine via winget):

```powershell
.\build.ps1            # release build
.\build.ps1 -Debug     # debug build
```

Output: two separate folders, `release\SystemDeck-Server\` (exe + `start-server.bat` + README;
its `data\` folder is never touched by builds) and `release\SystemDeck-Client\` (exe + README).
Build just one with `.\build.ps1 -Only server` or `-Only client`.

## Run the server

```powershell
.\target\release\systemdeck-server.exe --bind 0.0.0.0:8740 --data-dir D:\SystemDeck
```

On first start it prints a generated password for the `admin` account. Options can also
come from `SYSTEMDECK_BIND` / `SYSTEMDECK_DATA`.

Forgot the admin password:

```powershell
.\systemdeck-server.exe --data-dir D:\SystemDeck --reset-password admin
```

### Web admin dashboard

Open `http://<server>:8740/` in a browser and sign in with an administrator account. The
dashboard is compiled into the server binary and needs no internet access.

- **Overview**: who's online, signed-in sessions, users, teams, vaults, entry counts, uptime.
- **Users**: create users, reset passwords, make or remove admins, disable or enable accounts,
  and sign a user out everywhere.
- **Teams**: create teams and pick members.
- **Vaults & access**: create, rename or delete vaults, grant Read / Read & write / Manage to
  teams or users, and list a vault's entries. Passwords are never sent to the dashboard.

- **Session recordings**: replay SSH/RDP/VNC/web sessions (see *Session recording* below).
  With an AI endpoint configured, each recording can be **summarised** and its sensitive
  actions (sudo, user/password changes, deletes, firewall/service changes) flagged for review.
- **Audit log**: every server action — logins (and failures), logouts, password changes, entry
  and vault/team/user changes, recording views/deletes, downloads — filterable by user and action.
- **Download app**: every signed-in user (staff too, who see only this page) can download
  `SystemDeck-Client.zip`. `build.ps1` puts it in the server's `downloads\` folder
  (`--downloads-dir` to use another folder).

### AI (optional, self-hosted)

Point the server at a local OpenAI-compatible LLM and nothing leaves your network:

```powershell
$env:SYSTEMDECK_AI_URL = "http://localhost:11434/v1"   # Ollama / llama.cpp / vLLM / LM Studio
$env:SYSTEMDECK_AI_MODEL = "llama3.1"
```

Enables **recording summaries + sensitive-action flagging** on the dashboard. Leave `SYSTEMDECK_AI_URL`
unset to disable AI (the feature then returns a clear "not configured" message).

### Retention (optional)

`SYSTEMDECK_RECORDING_RETENTION_DAYS` and `SYSTEMDECK_AUDIT_RETENTION_DAYS` (0 = keep forever)
prune old recordings/audit entries on startup and once a day.

It uses the same JSON API as the desktop client and is served with a strict
Content-Security-Policy (own scripts only, no framing). Use HTTPS (reverse proxy) when you
reach it over the network.

**Back up both files in the data directory**: `systemdeck.redb` (the database) and
`server.key` (the key that encrypts every entry). Without the key the entries can't be read.

## Use the client

1. Start `systemdeck.exe`. It works immediately with a private **Local (this PC)** vault.
2. **Connect to server** → server URL, username, password.
3. As admin, open **⚙ Admin**:
   - **Users**: create staff accounts.
   - **Teams**: e.g. `Helpdesk: alice, bob`.
   - **Vaults & access**: create `Infrastructure`, give the *Helpdesk* team *Read & write*.
4. Everyone's client syncs every 60 seconds (or press **⟳ Sync now**).

### Connections

- Layout: a navy rail (profile, vaults with counters, open sessions, account), a connection
  list (search, collapsible folders from each entry's group), and a content pane (details
  cards or live sessions). Double-click an entry to connect.
- **SSH opens as an embedded terminal** (xterm-256color, scrollback, Ctrl+C/Ctrl+V, resize).
  It tries password auth and falls back to keyboard-interactive, which many switches use. Host
  keys are trusted on first use and pinned. A changed key refuses the connection.
- **Session recording (SSH, RDP, VNC)**:
  - SSH: keystrokes (`i`), terminal output (`o`) and resizes (`r`) in asciicast v2 format.
  - RDP/VNC: the screen as JPEG (at most once a second, only when it changed, scaled to
    ≤1280 px wide), typed text (`i`), key combinations (`k`) and mouse clicks (`m`) in the same
    JSON-lines layout (`.sdrec`).
  - Switch it on for **one entry** (desktop editor **Record sessions**, or dashboard **Vaults &
    access → Entries → Record**) or for **a whole vault** (dashboard vault **Settings → Record all
    remote sessions**). Recorded entries always open in a SystemDeck tab. A vault that requires
    recording refuses to connect while signed out.
  - Server-vault sessions stream to the server every few seconds. They're stored encrypted in
    `data\recordings\` and only visible to admins (**Session recordings**: replay screen, typed
    commands / activity with click-to-seek, download).
  - If the server can't be reached mid-session, recording continues to
    `%APPDATA%\SystemDeck\recordings\`. Local-vault entries always record there.
  - The session toolbar shows a blinking **REC** while recording. Typed passwords (e.g. `sudo`,
    a Windows sign-in screen) are part of the log, so treat recordings as sensitive.
  - Recording happens in the client: it shows what a SystemDeck user did. A user with a
    modified client, or another tool, isn't covered. Enforce that with a jump host or gateway.
- **SFTP file transfer**: the folder button on an SSH entry opens a two-pane browser
  (This PC ↔ remote). It can upload, download (with progress), create folders and delete.
  Single files only; folder transfer isn't supported yet.
- **RDP and VNC open in a tab inside SystemDeck.** RDP uses IronRDP with NLA/CredSSP, so the
  stored username and password are always used. VNC supports ZRLE/CopyRect/Raw and VNC
  password auth. Click the session to give it keyboard focus. Ctrl+V types your local
  clipboard into the remote session. The RDP desktop resizes to fit the tab.
- Per entry: **Open in** = Default / Embedded / External. The top-bar checkbox
  *Open RDP/VNC in tabs* sets the default. "Open externally" uses mstsc / vncviewer.
- External mstsc gets the password through a temporary `TERMSRV/<host>` Windows credential.
  The credential is removed after mstsc exits, and only if SystemDeck created it.
- Embedded-session limitations (first version): no clipboard sync or drive/audio redirection.
  No RD Gateway. Server certificates are not validated. Local cursor shape only.

### Offline-first sync

- Every entry you can access is cached locally (encrypted) and is usable without the server.
- Edits made while offline are queued (shown as "• not yet pushed") and pushed on the next sync.
- **Pushing many local entries at once**: click the ✓ button next to the search box (or
  Ctrl+click entries) to enter selection mode. Tick entries, or use **All** (respects the
  search). Pick a server vault, choose whether to remove the local copies, and press **Push**.
  It syncs once. Entries already in the target vault (same name + host + port) are skipped.
- **Sharing a private entry**: select it in *Local* → *Share to a team vault* → choose the
  vault → **Copy** (keeps your private copy) or **Move**.
- **Conflicts**: if someone else changed an entry while you had unsynced edits, the entry turns
  red and you choose **Keep mine** or **Use server version**. Nothing is silently overwritten.
- Deletions sync as tombstones. If your access to a vault is removed, its cache is dropped.
  Any unsynced edits from it are kept in your Local vault.

### Roles

| Role | Can |
|---|---|
| Read only | View, connect, copy passwords |
| Read & write | Create / edit / delete entries |
| Manage | Reserved for vault-level management |
| Administrator (user flag) | Everything, including users/teams/vaults |

## Security notes

- **Use TLS in production.** The server speaks plain HTTP. Put it behind a reverse proxy
  (Caddy, IIS, nginx) with HTTPS, or only expose it over a VPN such as Tailscale. The
  client warns when a non-localhost `http://` URL is used.
- Server: entries are encrypted at rest with `server.key`. Account passwords are Argon2id
  hashes. Session tokens are random 256-bit values stored only as SHA-256 hashes and expire
  after 7 days. Failed logins are delayed and don't reveal whether the user exists.
- Client: the local database is encrypted with a random key kept in Windows Credential
  Manager (`SystemDeck / local-store-key`). If that isn't available, the key goes in
  `%APPDATA%\SystemDeck\local.key`.
- RDP launches store the password as a temporary `TERMSRV/<host>` Windows credential so
  mstsc can use it, and delete it 30 seconds later. SSH launches copy the password to the
  clipboard.
- This is **not end-to-end encrypted**: the server can decrypt what it stores. That's what
  lets team sharing work without per-user key management.

## Testing

```powershell
cargo test --workspace                       # unit tests
# end-to-end sync test against a fresh server:
.\target\debug\systemdeck-server.exe --bind 127.0.0.1:8741 --data-dir $env:TEMP\vd-test
$env:SYSTEMDECK_TEST_URL="http://127.0.0.1:8741"; $env:SYSTEMDECK_TEST_ADMIN_PW="<printed password>"
cargo test -p systemdeck-client -- --ignored   # client tests live in client/
```

The end-to-end test covers sharing from the Local vault, a second user receiving the entry,
an edit conflict and its resolution, delete propagation, access revocation (with rescue
of unsynced work), and read-only enforcement.

Set `SYSTEMDECK_HOME` to run several clients side by side with separate local stores.

## Roadmap ideas

- Built-in TLS (rustls) and server-side audit log viewer
- End-to-end encryption with per-vault keys wrapped per user
- Embedded RDP/SSH sessions in tabs instead of launching external tools
- Import from RDM (`.rdm` export) / mRemoteNG / CSV
- Password history, entry attachments, TOTP fields
- Active Directory / Entra ID sign-in
