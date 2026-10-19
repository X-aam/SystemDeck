# SystemDeck Server

The team server: it stores shared vaults, users and teams, syncs them to the
desktop clients, and serves the web admin dashboard.

## Start

- `start-server.bat`: runs in a console window. **Closing that window stops the server.**
- `start-server-background.vbs`: runs without a window. Output goes to `data\server.log`.
- `stop-server.bat`: stops it.

The server listens on port **8740** and keeps everything in `.\data`.

On first start the console prints a generated password for the `admin` account.

- Admin dashboard: `http://<this-pc>:8740/`
- Desktop clients connect to the same address. Staff sign in to the same page to download
  the app (`downloads\SystemDeck-Client.zip`, created by the build).

Other options:

```
systemdeck-server.exe --data-dir .\data --bind 0.0.0.0:8740
systemdeck-server.exe --data-dir .\data --reset-password admin
```

## Back up

Copy the whole `data` folder: `systemdeck.redb` (database), `server.key` (encryption key)
and `recordings\` (encrypted SSH/RDP/VNC session recordings). Without the key, neither entries
nor recordings can be read.

## Security

The server speaks plain HTTP. Put it behind a reverse proxy with HTTPS (IIS, Caddy,
nginx) or only reach it over a VPN such as Tailscale. Allow port 8740 in Windows
Firewall only from trusted networks.
