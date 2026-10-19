# SystemDeck (desktop client)

Run `systemdeck.exe`. No installation is needed.

- Works offline with a private **Local (this PC)** vault.
- Click your avatar → **Connect to server...** and enter the server address
  (e.g. `http://systemdeck-server:8740`) to use your team's shared vaults.
- RDP, VNC and SSH open in tabs inside the app; SSH entries can also transfer files.
- Import connections with the import button (Remote Desktop Manager / mRemoteNG CSV),
  or from a terminal: `systemdeck.exe --import connections.csv`

Your data is stored encrypted in `%APPDATA%\SystemDeck`. The encryption key is kept in
Windows Credential Manager (`SystemDeck / local-store-key`).
