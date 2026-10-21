# SystemDeck — release notes

## Unreleased

### Desktop client
- **Website tabs now have full navigation**: Back, Forward, Reload (reloads the
  current page) and Home (returns to the entry's start page). Navigation stays in
  the tab, so recording keeps running across it.
- **Warn before leaving a recorded web session**: opening a recorded website entry
  in the external browser now asks first, because the external browser can't be
  recorded. Covers both the entry list and the in-session "Open in browser" button.
  Vaults that require recording block the external open entirely.
- **Portable build** (`SystemDeck-Portable.exe`): a single, no-install executable
  that carries the client and WebView2 loader, unpacks them per version to
  `%LOCALAPPDATA%\SystemDeck\portable\`, and runs with no admin rights, registry
  entries or shortcuts. Settings still live in `%APPDATA%\SystemDeck`.
- **Windows installer** (`SystemDeck-Setup-<version>.exe`, built with Inno Setup):
  per-user by default (no admin prompt), optional all-users install, Start-menu and
  desktop shortcuts, clean uninstall that keeps your vault data. Warns if the Edge
  WebView2 Runtime is missing.
- **WebView2 cache moved** to `%LOCALAPPDATA%\SystemDeck\WebView2` so the browser
  works when the client is installed under Program Files (read-only). You may need
  to sign in to website entries once more.
- **"Remember me"** on the desktop sign-in.

### Server dashboard
- **Manual entries**: add a single entry with the full field editor (**+ New entry**),
  and **Delete** entries from the list. Previously the dashboard could only edit or
  bulk-import.
- **Bulk CSV import** (**Import CSV**): paste or upload a CSV, matched by column name
  (Remote Desktop Manager exports work as-is); exact duplicates are skipped. Includes
  a **Download template** button with a sample file covering every column and type.
- **"Remember me"** on the dashboard sign-in.
- Entry list now shows the URL of Website entries.

### Deployment
- **Docker**: `Dockerfile` (multi-stage, runs as a non-root user) plus
  `deploy/docker-compose.yml` and `deploy/deploy.py` to build and start the server on
  a remote Docker host over SSH. Server data in the host's `data/` is never touched.
- **HTTPS via Caddy** using its internal CA (works for a bare IP, no public domain).
  Clients trust the CA root once: `certutil -user -addstore Root SystemDeck-CA.crt`.

### API / internals
- New admin routes: `POST /api/admin/vaults/{id}/entries` (create),
  `DELETE …/{entry}` (soft delete), `POST …/entries/import` (bulk), all gated on the
  Manage-vaults capability and written as normal revisions so clients sync them.
