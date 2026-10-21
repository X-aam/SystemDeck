# SystemDeck (desktop client)

Run `SystemDeck-Setup.exe` to install it (no admin rights needed; Start menu shortcut,
uninstall from Settings > Apps), or run `systemdeck.exe` from this folder without installing.

- Works offline with a private **Local (this PC)** vault.
- Click your avatar → **Connect to server...** and enter the server address
  (e.g. `http://systemdeck-server:8740`) to use your team's shared vaults.
- RDP, VNC and SSH open in tabs inside the app; SSH entries can also transfer files.
- Import connections with the import button (Remote Desktop Manager / mRemoteNG CSV),
  or from a terminal: `systemdeck.exe --import connections.csv`

Your data is stored encrypted in `%APPDATA%\SystemDeck`. The encryption key is kept in
Windows Credential Manager (`SystemDeck / local-store-key`).
