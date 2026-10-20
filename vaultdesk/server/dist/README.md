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

### Retention and AI (optional)

Set these as environment variables (or `--flags`):

```
SYSTEMDECK_RECORDING_RETENTION_DAYS=30   delete finished recordings older than N days
SYSTEMDECK_AUDIT_RETENTION_DAYS=365      delete audit entries older than N days
SYSTEMDECK_AI_URL=http://localhost:11434/v1   local OpenAI-compatible LLM (Ollama/llama.cpp/vLLM)
SYSTEMDECK_AI_MODEL=llama3.1              model name for the AI endpoint
```

With `SYSTEMDECK_AI_URL` set, the dashboard's **Session recordings** can summarise a
session and flag sensitive actions, all on your own network. Leave it unset to disable AI.

## Back up

Copy the whole `data` folder: `systemdeck.redb` (database), `server.key` (encryption key)
and `recordings\` (encrypted SSH/RDP/VNC session recordings). Without the key, neither entries
nor recordings can be read.

## Security

The server speaks plain HTTP. Put it behind a reverse proxy with HTTPS (IIS, Caddy,
nginx) or only reach it over a VPN such as Tailscale. Allow port 8740 in Windows
Firewall only from trusted networks.
