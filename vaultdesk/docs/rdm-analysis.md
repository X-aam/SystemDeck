# Remote Desktop Manager 2026.3 – static analysis & SystemDeck gap list

Static analysis only: nothing was executed. Findings come from the installer's PE
structure and from the .NET metadata (type and enum names) of the installed
`C:\Program Files\Devolutions\Remote Desktop Manager`. No Devolutions code is copied
into SystemDeck; this is a feature inventory used to plan SystemDeck's roadmap.

## Installer

| | |
|---|---|
| File | `Setup.RemoteDesktopManager.win-x64.2026.3.15.0.exe`, 245 MB, Authenticode-signed |
| SHA-256 | `67ac000eea4816d42f5de0190008a5577e311888d70a2bdfb60818159ecca800` |
| Packaging | Advanced Installer bootstrapper (32-bit stub) + 241 MB compressed overlay |
| Payload | `RemoteDesktopManager.exe`, `Launcher.exe`, `Devolutions.Updater.exe`, .NET Desktop Runtime 10.0.1 (x64 + ARM64) |
| App stack | .NET 10 WPF, 464 files / 1.1 GB installed |

Notable bundled engines: `Devolutions.IronRdp` / `IronVnc` (Devolutions' own Rust RDP/VNC
stacks), `Devolutions.MsRdpEx` (mstsc extensions), `Devolutions.Gateway.Client`,
`Devolutions.Hub.*`, `Devolutions.Server.ApiWrapper`, `VaultSharp` (HashiCorp Vault),
`Azure.Security.KeyVault.Secrets`, `AWSSDK.*`, `Azure.AI.OpenAI` + Semantic Kernel (AI assistant),
`Devolutions.KeePassPlugin.Interface`.

## Data model highlights (from `RemoteDesktopManager.Business.dll`)

- **`ConnectionType`: 160 values**, e.g. RDPConfigured, SSHShell, SSHTunnel, VNC, NoVNC, Telnet,
  Serial, TN3270, IBM5250, WebBrowser, PowerShell, PowerShellRemoteConsole, WindowsTerminal,
  Sftp, Scp, FtpNative, SMB, FileExplorer, HyperV, VMWare, ProxmoxConsole, NutanixConsole,
  Idrac, Ilo, IntelAMT, AzureBastion, AppleRemoteDesktop, RustDesk, TeamViewer, Splashtop,
  Credential, Document, Contact, Customer, Host, HostList, PortForward, ProxyTunnel,
  DevolutionsGateway, CyberArk*, BeyondTrust*, PAM, SmartFolder, TemplateGroup, AIAssistant.
- **`DataSourceType`: 25 back-ends**: SQLite, Xml, SQLServer, MySQL, MariaDB, Access, S3,
  Ftp/Sftp/WebDav, Dropbox, Google Drive, iCloud, Devolutions Server (RDMS/DOD), Hub, CyberArk.
- **`SecurityRoleRight`: 38 rights**: View, ViewPassword, Add, Edit, Delete, Move, Export,
  EditSecurity, PasswordHistory, ConnectionHistory, ViewLogs, Attachment, Handbook,
  Execute, ViewSessionRecording, ViewSensitiveInformation, ResetPassword, Checkout,
  ApproveCheckoutRequest, ForceCheckin, …
- **Check-out / PAM**: `CheckOutMode` (Manual, Automatic, Optional…), approval, reason and
  ticket-number modes (`None/Optional/Mandatory/MandatoryOnElevationOnly`), duration,
  status lifecycle (Pending → Granted → Active → Ended/Expired/Revoked).
- **`OTPType`**: TOTP, HOTP, SSO. **Password complexity** policies (warn / required, per folder).
- **`ConnectionDisplayMode`**: Embedded (tabs), External, Undocked.

## Gap list for SystemDeck (prioritised)

| # | Feature (RDM evidence) | SystemDeck today | Effort |
|---|---|---|---|
| 1 | **Audit log**: who opened / viewed password / edited what (`ConnectionHistory`, `ViewLogs`) | server logs to console only | M |
| 2 | **Password history** (`PasswordHistory`) | overwritten on edit | S |
| 3 | **Finer roles**: connect without revealing password (`View` vs `ViewPassword`), export rights | Read / Write / Manage | M |
| 4 | **TOTP field** with live code (`OTPType.Totp`) | none | S |
| 5 | **More launchers**: PowerShell remoting, SFTP/SCP (WinSCP), Windows Terminal, iDRAC/iLO web, RustDesk | RDP, SSH, VNC, Telnet, Serial, Web | S |
| 6 | **Credential reuse**: entries link to a shared credential entry | credentials stored per entry | M |
| 7 | **Folder-level permissions & inheritance** (`Inherited` modes everywhere) | per-vault only | M |
| 8 | **Check-out / approval workflow** (PAM enums) | none | L |
| 9 | **Password complexity policy** per vault | generator only | S |
| 10 | **Attachments & documents** (`Attachment`, `Document`) | none | M |
| 11 | **Embedded sessions in tabs** (`Embedded` display mode, IronRDP) | launches external tools | L |
| 12 | **Session recording** | none | L |
| 13 | **Gateway / jump host** (`SSHTunnel`, `DevolutionsGateway`, `PortForward`) | none | L |
| 14 | **Secret back-ends** (HashiCorp Vault, Azure Key Vault) | none | M |
