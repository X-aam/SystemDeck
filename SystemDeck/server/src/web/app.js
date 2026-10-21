// SystemDeck admin dashboard. Vanilla JS; every value from the server is
// inserted with textContent / DOM nodes, never as HTML.
"use strict";

const ROLES = { Read: "Read only", Write: "Read & write", Manage: "Manage" };
const KIND_COLORS = { Rdp: "#4084ee", Ssh: "#2eb078", Vnc: "#9662de", Telnet: "#e88e34", Serial: "#aa7650", Web: "#20a4be", RemoteSupport: "#dc4878", Credential: "#d6a824", Note: "#808894" };
const KIND_BADGE = { Rdp: "RDP", Ssh: "SSH", Vnc: "VNC", Telnet: "TEL", Serial: "COM", Web: "WEB", RemoteSupport: "SUP", Credential: "KEY", Note: "TXT" };
const KIND_LABELS = { Rdp: "Remote Desktop (RDP)", Ssh: "SSH", Vnc: "VNC", Telnet: "Telnet", Serial: "Serial console", Web: "Website", RemoteSupport: "Remote support (RustDesk)", Credential: "Credential", Note: "Secure note" };
const DISPLAY_LABELS = { Default: "Default", Embedded: "Embedded (tab)", External: "External program" };
const DEPTH_LABELS = { Bits16: "High colour (16-bit)", Bits24: "True colour (24-bit)", Bits32: "Highest quality (32-bit)" };
const QUALITY_LABELS = { Best: "Best quality (LAN)", Balanced: "Balanced", Fast: "Fast (slow connection)" };
const SCALING_LABELS = { Fit: "Shrink to fit", Stretch: "Stretch to fill", Actual: "Actual size (1:1)" };
const RES_PRESETS = ["1280x720", "1366x768", "1600x900", "1920x1080", "2560x1440", "3840x2160"];
const HAS_HOST = ["Rdp", "Ssh", "Vnc", "Telnet", "Serial"];
const CAN_EMBED = ["Rdp", "Ssh", "Vnc"];
const DEFAULT_PORTS = { Rdp: 3389, Ssh: 22, Vnc: 5900, Telnet: 23 };

const state = { token: null, me: null, view: "overview", users: [], teams: [], vaults: [], stats: null, recordings: [], downloads: [] };

// ---------- helpers ----------

function $(id) { return document.getElementById(id); }

/** h("div", {class: "x", onclick: fn}, child, "text", ...) */
function h(tag, attrs, ...children) {
  const el = document.createElement(tag);
  for (const [k, v] of Object.entries(attrs || {})) {
    if (v === undefined || v === null || v === false) continue;
    if (k.startsWith("on")) el.addEventListener(k.slice(2), v);
    else if (k === "class") el.className = v;
    else if (k === "style") {
      // CSSOM writes are allowed by the CSP (style-src 'self'); style="" attributes are not.
      for (const decl of String(v).split(";")) {
        const i = decl.indexOf(":");
        if (i > 0) el.style.setProperty(decl.slice(0, i).trim(), decl.slice(i + 1).trim());
      }
    }
    else if (v === true) el.setAttribute(k, "");
    else el.setAttribute(k, v);
  }
  for (const c of children.flat()) {
    if (c === null || c === undefined || c === false) continue;
    el.append(c instanceof Node ? c : document.createTextNode(String(c)));
  }
  return el;
}

// Reads check the tab first, then the browser. Writes clear both, then save to
// localStorage when `persist` ("Remember me") so it survives closing the browser.
function store(key, value, persist = false) {
  try {
    if (value === undefined) return sessionStorage.getItem(key) ?? localStorage.getItem(key);
    sessionStorage.removeItem(key);
    localStorage.removeItem(key);
    if (value !== null) (persist ? localStorage : sessionStorage).setItem(key, value);
  } catch (_) { return null; }
}

function prefs(key, value) {
  try {
    if (value === undefined) return localStorage.getItem(key);
    localStorage.setItem(key, value);
  } catch (_) { return null; }
}

function initials(name) {
  const parts = String(name || "").split(/[\s._-]+/).filter(Boolean);
  if (!parts.length) return "?";
  if (parts.length === 1) return parts[0].slice(0, 2).toUpperCase();
  return (parts[0][0] + parts[parts.length - 1][0]).toUpperCase();
}

function colorFor(name) {
  const palette = ["#38c2b2", "#4084ee", "#9662de", "#e88e34", "#2eb078", "#e8545c", "#20a4be"];
  let hash = 0;
  for (const ch of String(name)) hash = (hash * 31 + ch.charCodeAt(0)) | 0;
  return palette[Math.abs(hash) % palette.length];
}

function avatar(name) {
  return h("span", { class: "avatar", style: `background:${colorFor(name)}` }, initials(name));
}

function ago(iso) {
  if (!iso) return "never";
  const secs = Math.max(0, (Date.now() - new Date(iso).getTime()) / 1000);
  if (secs < 60) return "just now";
  if (secs < 3600) return `${Math.floor(secs / 60)} min ago`;
  if (secs < 86400) return `${Math.floor(secs / 3600)} h ago`;
  return new Date(iso).toLocaleDateString();
}

function presence(iso) {
  if (!iso) return "";
  const secs = (Date.now() - new Date(iso).getTime()) / 1000;
  return secs < 300 ? "on" : secs < 3600 ? "recent" : "";
}

function uptime(secs) {
  const d = Math.floor(secs / 86400), hrs = Math.floor((secs % 86400) / 3600), m = Math.floor((secs % 3600) / 60);
  return d ? `${d}d ${hrs}h` : hrs ? `${hrs}h ${m}m` : `${m}m`;
}

let toastTimer;
// Clipboard copy with a fallback for plain-http pages (navigator.clipboard
// only exists in secure contexts).
async function copyText(text, what) {
  try {
    if (navigator.clipboard && window.isSecureContext) {
      await navigator.clipboard.writeText(text);
    } else {
      const ta = h("textarea", { style: "position:fixed;opacity:0" }, text);
      document.body.appendChild(ta);
      ta.select();
      document.execCommand("copy");
      ta.remove();
    }
    toast(`${what} copied`);
  } catch (err) { toast(`Couldn't copy ${what.toLowerCase()}: ${err.message}`, true); }
}

// Login address of an entry: its URL, or https://host for Website entries without one.
function loginUrl(e) {
  if ((e.url || "").trim()) return e.url.trim();
  return e.kind === "Web" && (e.host || "").trim() ? `https://${e.host.trim()}` : "";
}

function toast(message, isError) {
  const t = $("toast");
  t.textContent = message;
  t.className = "toast" + (isError ? " error" : "");
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => t.classList.add("hidden"), isError ? 6000 : 3000);
}

// ---------- API ----------

async function api(method, path, body) {
  const headers = { "Content-Type": "application/json" };
  if (state.token) headers.Authorization = `Bearer ${state.token}`;
  const resp = await fetch(path, { method, headers, body: body === undefined ? undefined : JSON.stringify(body) });
  let data = null;
  try { data = await resp.json(); } catch (_) { /* empty body */ }
  if (resp.status === 401 && path !== "/api/login") {
    signOut(false);
    throw new Error("Your session expired. Please sign in again.");
  }
  if (!resp.ok) throw new Error((data && data.error) || `${resp.status} ${resp.statusText}`);
  return data;
}

// ---- roles & capabilities (mirrors the server) ----
const ROLE_CAPS = {
  admin: ["manage_users", "manage_config", "manage_vaults", "manage_teams", "view_recordings", "delete_recordings", "view_audit", "view_stats"],
  manager: ["manage_vaults", "manage_teams", "view_recordings", "delete_recordings", "view_audit", "view_stats"],
  auditor: ["view_recordings", "view_audit", "view_stats"],
  user: [],
};
const ROLE_LABELS = { admin: "Administrator", manager: "Manager", auditor: "Auditor", user: "User" };
const VIEW_CAP = { overview: "view_stats", users: "manage_users", teams: "manage_teams", vaults: "manage_vaults", recordings: "view_recordings", audit: "view_audit", settings: "manage_config", download: null };
function myRole() { return (state.me && state.me.role) || "user"; }
function can(cap) { return (ROLE_CAPS[myRole()] || []).includes(cap); }
function emptyStats() { return { version: "", uptime_secs: 0, users: 0, admins: 0, disabled_users: 0, teams: 0, vaults: 0, entries: 0, active_sessions: 0, vault_entries: {}, users_detail: [] }; }

async function loadAll() {
  const g = (cap, path, fallback) => (can(cap) ? api("GET", path).catch(() => fallback) : Promise.resolve(fallback));
  const [users, teams, vaults, stats, recordings] = await Promise.all([
    g("manage_users", "/api/admin/users", []),
    g("manage_teams", "/api/admin/teams", []),
    g("manage_vaults", "/api/admin/vaults", []),
    g("view_stats", "/api/admin/stats", emptyStats()),
    g("view_recordings", "/api/admin/recordings", []),
  ]);
  Object.assign(state, { users, teams, vaults, stats, recordings });
}

// ---------- modal ----------

function modal(title, body, okLabel, onOk, { danger = false, wide = false } = {}) {
  document.querySelector(".modal").classList.toggle("wide", wide);
  $("modal-title").textContent = title;
  const container = $("modal-body");
  container.replaceChildren(body);
  const err = $("modal-error");
  err.classList.add("hidden");
  const ok = $("modal-ok");
  ok.textContent = okLabel;
  ok.className = danger ? "primary danger-bg" : "primary";
  if (danger) ok.style.background = "var(--danger)"; else ok.style.background = "";
  $("modal-backdrop").classList.remove("hidden");
  const first = container.querySelector("input, select");
  if (first) setTimeout(() => first.focus(), 30);
  ok.onclick = async () => {
    ok.disabled = true;
    try {
      await onOk();
      closeModal();
    } catch (e) {
      err.textContent = e.message;
      err.classList.remove("hidden");
    } finally {
      ok.disabled = false;
    }
  };
}

function closeModal() { $("modal-backdrop").classList.add("hidden"); }

function confirmModal(title, text, okLabel, onOk) {
  modal(title, h("p", {}, text), okLabel, onOk, { danger: true });
}

// ---------- shell ----------

function setTheme(theme) {
  if (theme) document.documentElement.setAttribute("data-theme", theme);
  else document.documentElement.removeAttribute("data-theme");
  const dark = theme ? theme === "dark" : window.matchMedia("(prefers-color-scheme: dark)").matches;
  $("theme-toggle").textContent = dark ? "Light mode" : "Dark mode";
}

function showLogin(message) {
  $("app").classList.add("hidden");
  $("login").classList.remove("hidden");
  const err = $("login-error");
  if (message) { err.textContent = message; err.classList.remove("hidden"); } else err.classList.add("hidden");
  $("login-user").focus();
}

function signOut(callServer = true) {
  if (callServer && state.token) fetch("/api/logout", { method: "POST", headers: { Authorization: `Bearer ${state.token}` } }).catch(() => {});
  state.token = null;
  store("vd-token", null);
  showLogin();
}

async function start() {
  try {
    state.me = await api("GET", "/api/me");
    // Privileged roles get management views; plain users only the download page.
    const privileged = myRole() !== "user";
    if (privileged) { await loadAll(); state.view = "overview"; } else { state.view = "download"; }
    await loadDownloads();
  } catch (e) {
    showLogin(e.message);
    return;
  }
  $("login").classList.add("hidden");
  $("app").classList.remove("hidden");
  // Show each nav item only if the role has its capability (download is open).
  for (const b of document.querySelectorAll(".nav-item")) {
    const cap = VIEW_CAP[b.dataset.view];
    b.classList.toggle("hidden", cap ? !can(cap) : false);
  }
  const name = state.me.display_name || state.me.username;
  $("me").replaceChildren(avatar(name), h("div", {}, h("div", {}, name), h("div", { class: "muted" }, ROLE_LABELS[myRole()] || "User")));
  render();
}

async function loadDownloads() {
  try { state.downloads = await api("GET", "/api/downloads"); } catch (_) { state.downloads = []; }
}

async function go(view) {
  state.view = view;
  try {
    if (view === "download") await loadDownloads(); else await loadAll();
  } catch (e) { toast(e.message, true); }
  render();
}

function render() {
  for (const b of document.querySelectorAll(".nav-item")) b.classList.toggle("active", b.dataset.view === state.view);
  const views = { overview: renderOverview, users: renderUsers, teams: renderTeams, vaults: renderVaults, recordings: renderRecordings, audit: renderAudit, settings: renderSettings, download: renderDownload };
  $("actions").replaceChildren();
  views[state.view]();
}

function setHeader(title, subtitle, ...actions) {
  $("title").textContent = title;
  $("subtitle").textContent = subtitle || "";
  $("actions").replaceChildren(...actions);
}

async function act(promise, message) {
  try {
    await promise;
    if (message) toast(message);
    await loadAll();
    render();
  } catch (e) {
    toast(e.message, true);
  }
}

// ---------- overview ----------

function renderOverview() {
  const s = state.stats;
  setHeader("Overview", `SystemDeck server ${s.version} · up ${uptime(s.uptime_secs)}`, h("button", { onclick: () => go("overview") }, "Refresh"));
  const stat = (value, label) => h("div", { class: "card stat" }, h("div", { class: "value" }, value), h("div", { class: "label" }, label));
  const online = s.users_detail.filter((u) => presence(u.last_seen) === "on");
  const userList = h("ul", { class: "list" },
    s.users_detail.length ? s.users_detail.map((u) =>
      h("li", {},
        avatar(u.username),
        h("div", { class: "grow" }, h("div", {}, u.username), h("div", { class: "muted" }, `${u.sessions} active session(s)`)),
        h("span", { class: "muted" }, h("span", { class: `dot ${presence(u.last_seen)}` }), ago(u.last_seen))))
      : h("li", { class: "muted" }, "No users yet"));
  const vaultList = h("ul", { class: "list" },
    state.vaults.length ? state.vaults.map((v) =>
      h("li", {},
        h("div", { class: "grow" }, h("div", {}, v.name), h("div", { class: "muted" }, accessSummary(v))),
        h("span", { class: "chip" }, `${s.vault_entries[v.id] || 0} entries`)))
      : h("li", { class: "muted" }, "No shared vaults yet - create one under Vaults & access"));

  $("view").replaceChildren(
    h("div", { class: "cards" },
      stat(online.length, "online now"),
      stat(s.users, `users (${s.admins} admin${s.disabled_users ? `, ${s.disabled_users} disabled` : ""})`),
      stat(s.teams, "teams"),
      stat(s.vaults, "shared vaults"),
      stat(s.entries, "shared entries"),
      stat(s.active_sessions, "signed-in sessions")),
    h("div", { class: "grid2" },
      h("div", { class: "card" }, h("h3", {}, "Activity"), userList),
      h("div", { class: "card" }, h("h3", {}, "Vaults"), vaultList)));
}

function accessSummary(v) {
  if (!v.grants.length) return "Administrators only";
  return v.grants.map((g) => `${principalName(g.principal)} (${ROLES[g.role]})`).join(", ");
}

function principalName(p) {
  if (p.type === "Team") {
    const t = state.teams.find((x) => x.id === p.id);
    return t ? t.name : "(deleted team)";
  }
  return p.id;
}

// ---------- users ----------

// A role dropdown that PUTs the new role; disabled for your own account.
function roleSelect(u, self) {
  const sel = h("select", { disabled: self, title: self ? "You can't change your own role" : null },
    ...["admin", "manager", "auditor", "user"].map((r) => h("option", { value: r }, ROLE_LABELS[r])));
  sel.value = u.role || (u.is_admin ? "admin" : "user");
  sel.addEventListener("change", () => act(api("PUT", `/api/admin/users/${encodeURIComponent(u.username)}`, { role: sel.value }), `${u.username}: role set to ${ROLE_LABELS[sel.value]}`));
  return sel;
}

function renderUsers() {
  setHeader("Users", `${state.users.length} accounts`, h("button", { class: "primary", onclick: newUser }, "+ New user"));
  const activity = Object.fromEntries(state.stats.users_detail.map((u) => [u.username, u]));
  const rows = state.users.map((u) => {
    const a = activity[u.username] || {};
    const self = u.username === state.me.username;
    return h("tr", {},
      h("td", {}, h("div", { class: "who" }, avatar(u.display_name || u.username),
        h("div", {}, h("div", {}, u.display_name || u.username), h("div", { class: "sub" }, u.username)))),
      h("td", {}, roleSelect(u, self)),
      h("td", {}, u.disabled ? h("span", { class: "badge off" }, "Disabled") : "Active"),
      h("td", {}, h("span", { class: `dot ${presence(a.last_seen)}` }), ago(a.last_seen), a.sessions ? h("span", { class: "muted" }, ` · ${a.sessions} session(s)`) : null),
      h("td", {}, h("div", { class: "row-actions" },
        h("button", { class: "small", onclick: () => resetPassword(u) }, "Reset password"),
        h("button", { class: "small", disabled: self || !a.sessions,
          onclick: () => act(api("POST", `/api/admin/users/${encodeURIComponent(u.username)}/revoke`), `${u.username} signed out everywhere`) }, "Sign out"),
        h("button", { class: "small" + (u.disabled ? "" : " danger"), disabled: self,
          onclick: () => act(api("PUT", `/api/admin/users/${encodeURIComponent(u.username)}`, { disabled: !u.disabled }), u.disabled ? "Account enabled" : "Account disabled") },
          u.disabled ? "Enable" : "Disable"))));
  });
  $("view").replaceChildren(h("div", { class: "table-wrap" }, h("table", {},
    h("thead", {}, h("tr", {}, h("th", {}, "User"), h("th", {}, "Role"), h("th", {}, "Status"), h("th", {}, "Last seen"), h("th", {}))),
    h("tbody", {}, rows))));
}

function newUser() {
  const f = {
    username: h("input", { placeholder: "e.g. jdoe", autocomplete: "off" }),
    display: h("input", { placeholder: "Jane Doe" }),
    password: h("input", { type: "password", autocomplete: "new-password", placeholder: "at least 8 characters" }),
    role: h("select", {}, ...["user", "auditor", "manager", "admin"].map((r) => h("option", { value: r }, ROLE_LABELS[r]))),
  };
  f.role.value = "user";
  modal("New user", h("div", {},
    h("label", {}, "Username", f.username),
    h("label", {}, "Display name", f.display),
    h("label", {}, "Initial password", f.password),
    h("label", {}, "Role", f.role)), "Create", async () => {
    await api("POST", "/api/admin/users", { username: f.username.value, display_name: f.display.value, password: f.password.value, role: f.role.value });
    toast("User created");
    await loadAll();
    render();
  });
}

function resetPassword(u) {
  const pw = h("input", { type: "password", autocomplete: "new-password", placeholder: "at least 8 characters" });
  const pw2 = h("input", { type: "password", autocomplete: "new-password" });
  modal(`Reset password for ${u.username}`, h("div", {},
    h("label", {}, "New password", pw), h("label", {}, "Repeat", pw2),
    h("p", { class: "muted" }, "Their other sessions are signed out.")), "Set password", async () => {
    if (pw.value !== pw2.value) throw new Error("Passwords do not match");
    await api("PUT", `/api/admin/users/${encodeURIComponent(u.username)}`, { password: pw.value });
    toast("Password changed");
  });
}

// ---------- teams ----------

function renderTeams() {
  setHeader("Teams", "Group staff, then grant vault access once per team", h("button", { class: "primary", onclick: () => editTeam(null) }, "+ New team"));
  if (!state.teams.length) {
    $("view").replaceChildren(h("div", { class: "empty" }, "No teams yet. Create one, e.g. Helpdesk or Infrastructure."));
    return;
  }
  $("view").replaceChildren(h("div", { class: "grid2" }, state.teams.map((t) => {
    const vaults = state.vaults.filter((v) => v.grants.some((g) => g.principal.type === "Team" && g.principal.id === t.id));
    return h("div", { class: "card" },
      h("div", { class: "who", style: "margin-bottom:12px" }, avatar(t.name),
        h("div", { class: "grow" }, h("div", { style: "font-weight:600" }, t.name), h("div", { class: "sub" }, `${t.members.length} member(s)`)),
        h("button", { class: "small", onclick: () => editTeam(t) }, "Edit"),
        h("button", { class: "small danger", onclick: () => confirmModal("Delete team", `Delete "${t.name}"? Its vault access is removed too.`, "Delete",
          async () => { await api("DELETE", `/api/admin/teams/${t.id}`); toast("Team deleted"); await loadAll(); render(); }) }, "Delete")),
      h("h3", {}, "Members"),
      h("div", {}, t.members.length ? t.members.map((m) => h("span", { class: "chip" }, m)) : h("span", { class: "muted" }, "No members")),
      h("h3", { style: "margin-top:14px" }, "Vault access"),
      h("div", {}, vaults.length ? vaults.map((v) => {
        const g = v.grants.find((x) => x.principal.type === "Team" && x.principal.id === t.id);
        return h("span", { class: "chip" }, `${v.name} · ${ROLES[g.role]}`);
      }) : h("span", { class: "muted" }, "None")));
  })));
}

function editTeam(team) {
  const name = h("input", { value: team ? team.name : "", placeholder: "e.g. Helpdesk" });
  const boxes = state.users.map((u) => {
    const cb = h("input", { type: "checkbox", value: u.username });
    cb.checked = !!(team && team.members.includes(u.username));
    return h("label", { class: "check" }, cb, `${u.display_name || u.username} (${u.username})`);
  });
  modal(team ? `Edit ${team.name}` : "New team", h("div", {},
    h("label", {}, "Team name", name),
    h("label", {}, "Members"),
    h("div", { class: "checklist" }, boxes.length ? boxes : h("p", { class: "muted" }, "Create users first"))),
  team ? "Save" : "Create", async () => {
    const members = boxes.map((b) => b.querySelector("input")).filter((c) => c.checked).map((c) => c.value);
    const body = { name: name.value, members };
    if (team) await api("PUT", `/api/admin/teams/${team.id}`, body); else await api("POST", "/api/admin/teams", body);
    toast(team ? "Team saved" : "Team created");
    await loadAll();
    render();
  });
}

// ---------- vaults ----------

function renderVaults() {
  setHeader("Vaults & access", "Shared vaults sync to every desktop client that has access", h("button", { class: "primary", onclick: () => editVault(null) }, "+ New vault"));
  if (!state.vaults.length) {
    $("view").replaceChildren(h("div", { class: "empty" }, "No shared vaults yet."));
    return;
  }
  const rows = state.vaults.map((v) => h("tr", {},
    h("td", {}, h("div", { style: "font-weight:600" }, v.name, v.record_ssh ? h("span", { class: "badge off", style: "margin-left:8px" }, "All sessions recorded") : null),
      v.description ? h("div", { class: "muted" }, v.description) : null),
    h("td", {}, state.stats.vault_entries[v.id] || 0),
    h("td", {}, v.grants.length ? v.grants.map((g) => h("span", { class: "chip" }, `${g.principal.type === "Team" ? "Team" : "User"}: ${principalName(g.principal)} · ${ROLES[g.role]}`))
      : h("span", { class: "muted" }, "Administrators only")),
    h("td", {}, h("div", { class: "row-actions" },
      h("button", { class: "small", onclick: () => showEntries(v) }, "Entries"),
      h("button", { class: "small", onclick: () => editAccess(v) }, "Access"),
      h("button", { class: "small", onclick: () => editVault(v) }, "Settings"),
      h("button", { class: "small danger", onclick: () => confirmModal("Delete vault", `Delete "${v.name}" and ALL ${state.stats.vault_entries[v.id] || 0} entries in it? Clients keep unsynced edits in their Local vault.`, "Delete vault",
        async () => { await api("DELETE", `/api/admin/vaults/${v.id}`); toast("Vault deleted"); await loadAll(); render(); }) }, "Delete")))));
  $("view").replaceChildren(h("div", { class: "table-wrap" }, h("table", {},
    h("thead", {}, h("tr", {}, h("th", {}, "Vault"), h("th", {}, "Entries"), h("th", {}, "Access"), h("th", {}))),
    h("tbody", {}, rows))));
}

function editVault(v) {
  const name = h("input", { value: v ? v.name : "", placeholder: "e.g. Infrastructure" });
  const desc = h("input", { value: v ? v.description : "", placeholder: "What belongs in this vault" });
  const rec = h("input", { type: "checkbox" });
  rec.checked = !!(v && v.record_ssh);
  modal(v ? `${v.name} settings` : "New vault", h("div", {}, h("label", {}, "Name", name), h("label", {}, "Description", desc),
    h("label", { class: "check" }, rec, "Record all remote sessions (SSH, RDP, VNC), stored on this server"),
    h("p", { class: "muted" }, "When on, the desktop app records every session to entries in this vault (SSH: keystrokes + output; RDP/VNC: screen, typed text, clicks), always opens them in a SystemDeck tab and refuses to connect while signed out. To record only some entries, leave this off and use Entries → Record.")),
    v ? "Save" : "Create", async () => {
      const body = { name: name.value, description: desc.value, grants: v ? v.grants : [], record_ssh: rec.checked };
      if (v) await api("PUT", `/api/admin/vaults/${v.id}`, body); else await api("POST", "/api/admin/vaults", body);
      toast(v ? "Vault saved" : "Vault created - now grant access to a team");
      await loadAll();
      render();
    });
}

function editAccess(v) {
  let grants = v.grants.map((g) => ({ principal: { ...g.principal }, role: g.role }));
  const list = h("div", {});
  const roleSelect = (value) => h("select", {}, Object.entries(ROLES).map(([k, label]) => {
    const o = h("option", { value: k }, label);
    if (k === value) o.selected = true;
    return o;
  }));

  const draw = () => {
    list.replaceChildren(grants.length ? h("ul", { class: "list" }, grants.map((g, i) => {
      const sel = roleSelect(g.role);
      sel.style.width = "auto";
      sel.addEventListener("change", () => { g.role = sel.value; });
      return h("li", {},
        avatar(principalName(g.principal)),
        h("div", { class: "grow" }, h("div", {}, principalName(g.principal)), h("div", { class: "muted" }, g.principal.type)),
        sel,
        h("button", { class: "small danger", onclick: () => { grants.splice(i, 1); draw(); } }, "Remove"));
    })) : h("p", { class: "muted" }, "Only administrators can see this vault."));
  };

  const kind = h("select", {}, h("option", { value: "Team" }, "Team"), h("option", { value: "User" }, "User"));
  const who = h("select", {});
  const fillWho = () => {
    const opts = kind.value === "Team"
      ? state.teams.map((t) => h("option", { value: t.id }, t.name))
      : state.users.map((u) => h("option", { value: u.username }, u.username));
    who.replaceChildren(...(opts.length ? opts : [h("option", { value: "" }, kind.value === "Team" ? "(no teams)" : "(no users)")]));
  };
  kind.addEventListener("change", fillWho);
  fillWho();
  const role = roleSelect("Read");
  const add = h("button", { onclick: () => {
    if (!who.value) return;
    const principal = { type: kind.value, id: who.value };
    grants = grants.filter((g) => !(g.principal.type === principal.type && g.principal.id === principal.id));
    grants.push({ principal, role: role.value });
    draw();
  } }, "Add");
  draw();

  modal(`Access to ${v.name}`, h("div", {},
    list,
    h("div", { class: "grant-add" },
      h("label", {}, "Type", kind), h("label", {}, "Who", who), h("label", {}, "Role", role), h("label", {}, " ", add)),
    h("p", { class: "muted" }, "Read only: view and connect. Read & write: add, edit and delete entries.")),
  "Save access", async () => {
    await api("PUT", `/api/admin/vaults/${v.id}`, { name: v.name, description: v.description, grants });
    toast("Access saved - clients pick it up on their next sync");
    await loadAll();
    render();
  });
}

// Inline entries list for a vault (no modal).
async function showEntries(v) {
  let entries;
  try { entries = await api("GET", `/api/admin/vaults/${v.id}/entries`); } catch (e) { toast(e.message, true); return; }
  setHeader(v.name, `${entries.length} ${entries.length === 1 ? "entry" : "entries"}`,
    h("button", { class: "small primary", onclick: () => newEntry(v) }, "+ New entry"),
    h("button", { class: "small", onclick: () => importEntries(v) }, "Import CSV"),
    h("button", { class: "small", onclick: renderVaults }, "← Back to vaults"));
  const recordable = (e) => CAN_EMBED.includes(e.kind);
  // Entry-level recording switch; the vault policy overrides it.
  const recSwitch = (e) => {
    if (!recordable(e)) return h("span", {});
    const box = h("input", { type: "checkbox" });
    box.checked = v.record_ssh || e.record_session;
    box.disabled = v.record_ssh;
    box.addEventListener("change", async () => {
      box.disabled = true;
      try {
        await api("PUT", `/api/admin/vaults/${v.id}/entries/${e.id}/recording`, { record: box.checked });
        e.record_session = box.checked;
        toast(box.checked ? `${e.name}: sessions will be recorded (clients pick it up on their next sync)` : `${e.name}: recording off`);
      } catch (err) { box.checked = !box.checked; toast(err.message, true); }
      box.disabled = false;
    });
    return h("label", { class: "check rec-switch", title: v.record_ssh ? "Recorded by the vault setting" : "Record this entry's sessions" }, box, v.record_ssh ? "Recorded (vault)" : "Record");
  };
  const body = entries.length ? h("ul", { class: "list" }, entries.map((e) =>
    h("li", {},
      h("span", { class: "badge kind", style: `background:${KIND_COLORS[e.kind] || "#888"}` }, KIND_BADGE[e.kind] || e.kind),
      h("div", { class: "grow" },
        h("div", {}, e.name),
        h("div", { class: "muted" }, [e.group, e.username && e.host ? `${e.username}@${e.host}` : e.host].filter(Boolean).join(" · "))),
      recSwitch(e),
      e.username ? h("button", { class: "small", title: e.username, onclick: () => copyText(e.username, "Username") }, "Copy user") : null,
      loginUrl(e) ? h("button", { class: "small", title: loginUrl(e), onclick: () => copyText(loginUrl(e), "Login URL") }, "Copy URL") : null,
      h("button", { class: "small", onclick: () => editEntry(v, e) }, "Edit"),
      h("button", { class: "small", onclick: () => dupEntry(v, e) }, "Duplicate"),
      h("button", { class: "small", onclick: () => delEntry(v, e) }, "Delete"),
      h("span", { class: "muted", title: new Date(e.updated_at).toLocaleString() }, `${e.updated_by || "-"} · ${ago(e.updated_at)}`))))
    : h("p", { class: "muted" }, "This vault is empty. Users add entries from the desktop client.");
  const hint = v.record_ssh
    ? "Every SSH/RDP/VNC session in this vault is recorded (vault Settings)."
    : "Record: record that entry's SSH/RDP/VNC sessions. Or record the whole vault in Settings.";
  $("view").replaceChildren(h("div", { class: "card" }, body, h("p", { class: "muted", style: "margin-top:12px" }, `${hint} Edit opens the full entry editor, including its password.`)));
}

async function dupEntry(v, e) {
  try {
    await api("POST", `/api/admin/vaults/${v.id}/entries/${e.id}/duplicate`);
    toast(`Duplicated "${e.name}"`);
    await loadAll();
    showEntries(v);
  } catch (err) { toast(err.message, true); }
}

// ---- Bulk import (CSV parsed in the browser, posted as entry objects) ----

// Column header aliases, matching the desktop client's CSV importer.
const CSV_COLS = {
  name: ["display name", "name", "title", "entry name"],
  host: ["host", "hostname", "host name", "address", "computer", "server"],
  port: ["port"],
  kind: ["type", "kind", "connection type", "protocol"],
  group: ["folder", "group", "path", "group/folder"],
  user: ["username", "user name", "user", "login"],
  pass: ["password", "pass"],
  domain: ["domain"],
  url: ["url", "website", "web address"],
  notes: ["description", "notes", "note", "comment", "comments"],
  tags: ["tags", "keywords"],
};

// Minimal RFC-4180 CSV parser: handles quoted fields, "" escapes and CRLF.
function parseCsv(text) {
  const rows = [];
  let row = [], field = "", inQuotes = false;
  text = String(text).replace(/\r\n/g, "\n").replace(/\r/g, "\n");
  for (let i = 0; i < text.length; i++) {
    const c = text[i];
    if (inQuotes) {
      if (c === '"') { if (text[i + 1] === '"') { field += '"'; i++; } else inQuotes = false; }
      else field += c;
    } else if (c === '"') inQuotes = true;
    else if (c === ",") { row.push(field); field = ""; }
    else if (c === "\n") { row.push(field); rows.push(row); row = []; field = ""; }
    else field += c;
  }
  if (field.length || row.length) { row.push(field); rows.push(row); }
  return rows;
}

function csvParseKind(s) {
  s = (s || "").toLowerCase();
  if (!s) return null;
  if (s.includes("rdp") || s.includes("remote desktop")) return "Rdp";
  if (s.includes("ssh") || s.includes("putty")) return "Ssh";
  if (s.includes("vnc")) return "Vnc";
  if (s.includes("telnet")) return "Telnet";
  if (s.includes("serial") || s.includes("com port")) return "Serial";
  if (s.includes("web") || s.includes("http")) return "Web";
  if (s.includes("credential") || s.includes("password") || s.includes("login")) return "Credential";
  if (s.includes("note")) return "Note";
  return null;
}

function csvInferKind(host, port, url) {
  const up = (host || "").toUpperCase();
  if (up.startsWith("COM") && up.length > 3 && /^\d+$/.test(up.slice(3))) return "Serial";
  if (port != null) {
    if (port >= 3389 && port <= 3399) return "Rdp";
    if (port === 22) return "Ssh";
    if (port === 23) return "Telnet";
    if (port >= 5900 && port <= 5910) return "Vnc";
    if ([80, 443, 8080, 8443].includes(port)) return "Web";
    return "Ssh";
  }
  if (url) return "Web";
  if ((host || "").includes(".")) return "Web";
  return "Credential";
}

function csvToEntries(text) {
  const rows = parseCsv(text).filter((r) => r.some((c) => c.trim() !== ""));
  if (!rows.length) throw new Error("no rows found");
  const headers = rows[0].map((hd) => hd.replace(/^﻿/, "").trim().toLowerCase());
  const find = (names) => { for (const n of names) { const i = headers.indexOf(n); if (i >= 0) return i; } return -1; };
  const cols = {};
  for (const k in CSV_COLS) cols[k] = find(CSV_COLS[k]);
  if (cols.name < 0 && cols.host < 0) throw new Error(`the CSV needs at least a Name or Host column (found: ${headers.join(", ")})`);
  const entries = [], skipped = [];
  for (let r = 1; r < rows.length; r++) {
    const rec = rows[r];
    const get = (i) => (i >= 0 && rec[i] != null ? rec[i].trim() : "");
    const host = get(cols.host);
    const name = get(cols.name) || host;
    if (!name) { skipped.push(`line ${r + 1}: no name or host`); continue; }
    const portText = get(cols.port);
    let port = null;
    if (portText) {
      const p = Number(portText);
      if (!Number.isInteger(p) || p < 0 || p > 65535) { skipped.push(`line ${r + 1}: bad port '${portText}'`); continue; }
      port = p;
    }
    let url = get(cols.url);
    const kind = csvParseKind(get(cols.kind)) || csvInferKind(host, port, url);
    if (!url && kind === "Web" && host) url = `https://${host}`;
    entries.push({
      kind, name,
      group: get(cols.group).replace(/\\/g, "/").replace(/^\/+|\/+$/g, ""),
      host, port,
      username: get(cols.user), password: get(cols.pass), domain: get(cols.domain), url,
      notes: get(cols.notes),
      tags: get(cols.tags).split(/[,;]/).map((t) => t.trim()).filter(Boolean),
    });
  }
  return { entries, skipped };
}

// Sample CSV showing every recognised column; one row per entry type.
const CSV_TEMPLATE = [
  "Name,Type,Host,Port,Folder,Username,Password,Domain,URL,Description,Tags",
  "DC01,RDP,10.0.0.10,3389,Servers/Core,administrator,,CORP,,Domain controller,windows;core",
  "web01,SSH,10.0.0.20,22,Servers/Linux,root,,,,Web server,linux",
  "lab-vnc,VNC,10.0.0.30,5900,Lab,,,,,,",
  "switch01,Telnet,10.0.0.1,23,Network,admin,,,,Core switch,network",
  "console,Serial,COM3,,Network,,,,,Console cable,",
  "Router UI,Website,,,Network,admin,,,https://192.168.1.1,,network",
  "Backup account,Credential,,,Accounts,svc-backup,,CORP,,Service account,",
  "Runbook,Note,,,Docs,,,,,Restart order: DB then app,",
].join("\r\n") + "\r\n";

function downloadCsvTemplate() {
  const blob = new Blob(["﻿" + CSV_TEMPLATE], { type: "text/csv;charset=utf-8" });
  const a = h("a", { href: URL.createObjectURL(blob), download: "systemdeck-import-template.csv" });
  document.body.appendChild(a);
  a.click();
  a.remove();
  setTimeout(() => URL.revokeObjectURL(a.href), 1000);
}

function importEntries(v) {
  const ta = h("textarea", { rows: "10", placeholder: "Paste CSV here — the first row is the column headers.", style: "width:100%;box-sizing:border-box;font-family:monospace;font-size:12px" });
  const file = h("input", { type: "file", accept: ".csv,text/csv,text/plain" });
  file.addEventListener("change", () => {
    const f = file.files && file.files[0];
    if (!f) return;
    const reader = new FileReader();
    reader.onload = () => { ta.value = reader.result; };
    reader.readAsText(f);
  });
  const body = h("div", {},
    h("p", { class: "muted" }, "Columns are matched by name: Name/Display Name, Host/Address, Port, Type, Folder/Group, Username, Password, Domain, URL, Description/Notes, Tags. A Name or Host column is required; Remote Desktop Manager exports work as-is. Exact duplicates (same name, host and port) are skipped."),
    h("p", {}, h("button", { type: "button", class: "small", onclick: downloadCsvTemplate }, "Download template"),
      h("span", { class: "muted" }, " — a sample CSV with every column and one row per entry type.")),
    h("label", {}, "CSV file", file),
    h("label", {}, "or paste CSV", ta));
  modal(`Import into ${v.name}`, body, "Import", async () => {
    const { entries, skipped } = csvToEntries(ta.value);
    if (!entries.length) throw new Error(skipped.length ? `nothing to import — ${skipped.join("; ")}` : "no data rows found");
    const report = await api("POST", `/api/admin/vaults/${v.id}/entries/import`, entries);
    const serverSkip = (report.skipped || []).length;
    const totalSkip = serverSkip + skipped.length;
    let msg = `Imported ${report.added} entr${report.added === 1 ? "y" : "ies"}`;
    if (report.duplicates) msg += `, ${report.duplicates} already present`;
    if (totalSkip) msg += `, ${totalSkip} skipped`;
    toast(msg, totalSkip > 0);
    await loadAll();
    showEntries(v);
  }, { wide: true });
}

function delEntry(v, e) {
  confirmModal("Delete entry", `Delete "${e.name}"? Clients remove it on their next sync.`, "Delete", async () => {
    await api("DELETE", `/api/admin/vaults/${v.id}/entries/${e.id}`);
    toast(`Deleted "${e.name}"`);
    await loadAll();
    showEntries(v);
  });
}

async function editEntry(v, summary) {
  let e;
  try { e = await api("GET", `/api/admin/vaults/${v.id}/entries/${summary.id}`); } catch (err) { toast(err.message, true); return; }
  renderEntryEditor(v, e);
}

// A blank entry for manual creation. `_new` tells the editor to POST (create)
// rather than PUT (update). Defaults mirror the desktop client's new entry.
function newEntry(v) {
  renderEntryEditor(v, {
    _new: true, kind: "Rdp", name: "", group: "", host: "", port: null,
    username: "", password: "", domain: "", url: "", notes: "", tags: [],
    display: "Default", record_session: false, screen: {},
  });
}

// Full, inline entry editor (every field). Re-renders on type change so only
// the relevant fields show; edits write straight into the working copy `e`.
function renderEntryEditor(v, e) {
  if (!e.screen) e.screen = {};
  const k = e.kind;
  setHeader(e._new ? "New entry" : "Edit entry", `${v.name} › ${e.name || "entry"}`, h("button", { class: "small", onclick: () => showEntries(v) }, "← Back to entries"));
  const rows = [];
  const add = (label, control) => rows.push(h("label", {}, label, control));
  const txt = (key, ph) => { const el = h("input", { value: e[key] != null ? e[key] : "", placeholder: ph || "" }); el.addEventListener("input", () => (e[key] = el.value)); return el; };
  // A text field with a Copy button that copies its current value.
  const withCopy = (input, what) => h("div", { class: "row-actions" }, input,
    h("button", { class: "small", type: "button", onclick: () => input.value ? copyText(input.value, what) : toast(`No ${what.toLowerCase()} to copy`, true) }, "Copy"));
  const pick = (key, labels) => { const el = h("select", {}, ...Object.entries(labels).map(([val, lab]) => h("option", { value: val }, lab))); el.value = e[key]; el.addEventListener("change", () => { e[key] = el.value; if (key === "kind") renderEntryEditor(v, e); }); return el; };
  const topCheck = (key, label) => { const b = h("input", { type: "checkbox" }); b.checked = !!e[key]; b.addEventListener("change", () => (e[key] = b.checked)); return h("label", { class: "check" }, b, label); };

  add("Type", pick("kind", KIND_LABELS));
  add("Name", txt("name", "required"));
  add("Group", txt("group", "e.g. Servers/Production"));
  if (k === "RemoteSupport") add("RustDesk ID", txt("host", "9-digit ID"));
  else if (HAS_HOST.includes(k) || k === "Web") add("Host", txt("host", "hostname or IP"));
  if (HAS_HOST.includes(k)) {
    const el = h("input", { type: "number", value: e.port != null ? String(e.port) : "", placeholder: DEFAULT_PORTS[k] ? `default ${DEFAULT_PORTS[k]}` : "" });
    el.addEventListener("input", () => (e.port = el.value ? Number(el.value) : null));
    add("Port", el);
  }
  if (k === "Web") add("URL", withCopy(txt("url", "https://..."), "Login URL"));
  if (k !== "Note") {
    if (k === "Rdp" || k === "Credential") add("Domain", txt("domain"));
    if (k !== "RemoteSupport") add("Username", withCopy(txt("username"), "Username"));
    const pwd = h("input", { type: "password", value: e.password || "" });
    pwd.addEventListener("input", () => (e.password = pwd.value));
    const reveal = h("button", { class: "small", type: "button" }, "Show");
    reveal.addEventListener("click", () => { const hid = pwd.type === "password"; pwd.type = hid ? "text" : "password"; reveal.textContent = hid ? "Hide" : "Show"; });
    add(k === "RemoteSupport" ? "Password (unattended)" : "Password", h("div", { class: "row-actions" }, pwd, reveal));
  }
  if (CAN_EMBED.includes(k) || k === "Web") add("Open in", pick("display", DISPLAY_LABELS));
  if (k === "Rdp" || k === "Vnc") {
    if (k === "Rdp") {
      add("Resolution", resolutionControl(v, e));
      add("Colours", screenPick(e, "color_depth", DEPTH_LABELS));
      add("Quality", screenPick(e, "quality", QUALITY_LABELS));
    }
    add("Scaling", screenPick(e, "scaling", SCALING_LABELS));
    const extras = [screenCheck(e, "smooth", "Smooth scaling")];
    if (k === "Rdp") extras.push(screenCheck(e, "full_screen", "Full screen (external mstsc)"));
    add("Display", h("div", { class: "row-actions" }, ...extras));
  }
  if (CAN_EMBED.includes(k) || k === "Web") add("Recording", topCheck("record_session", "Record this entry's sessions"));
  const tags = h("input", { value: (e.tags || []).join(", "), placeholder: "comma separated" });
  tags.addEventListener("input", () => (e.tags = tags.value.split(",").map((t) => t.trim()).filter(Boolean)));
  add("Tags", tags);
  const notes = h("textarea", { rows: "4" }, e.notes || "");
  notes.addEventListener("input", () => (e.notes = notes.value));
  add("Notes", notes);

  const save = h("button", { class: "primary", onclick: async () => {
    if (!(e.name || "").trim()) { toast("Name is required", true); return; }
    try {
      if (e._new) await api("POST", `/api/admin/vaults/${v.id}/entries`, e);
      else await api("PUT", `/api/admin/vaults/${v.id}/entries/${e.id}`, e);
      toast(e._new ? "Entry created" : "Entry saved");
      await loadAll();
      showEntries(v);
    } catch (err) { toast(err.message, true); }
  } }, e._new ? "Create entry" : "Save entry");
  const cancel = h("button", { class: "small", onclick: () => showEntries(v) }, "Cancel");
  $("view").replaceChildren(h("div", { class: "card", style: "max-width:720px" }, ...rows, h("div", { style: "margin-top:16px; display:flex; gap:10px" }, save, cancel)));
}

function screenPick(e, key, labels) {
  const el = h("select", {}, ...Object.entries(labels).map(([val, lab]) => h("option", { value: val }, lab)));
  el.value = e.screen[key];
  el.addEventListener("change", () => (e.screen[key] = el.value));
  return el;
}
function screenCheck(e, key, label) {
  const b = h("input", { type: "checkbox" });
  b.checked = !!e.screen[key];
  b.addEventListener("change", () => (e.screen[key] = b.checked));
  return h("label", { class: "check" }, b, label);
}
function resolutionControl(v, e) {
  const fixed = e.screen.resolution && typeof e.screen.resolution === "object" && Array.isArray(e.screen.resolution.Fixed) ? e.screen.resolution.Fixed : null;
  const presetStr = fixed ? `${fixed[0]}x${fixed[1]}` : null;
  const sel = h("select", {}, h("option", { value: "fit" }, "Fit to window"),
    ...RES_PRESETS.map((p) => h("option", { value: p }, p.replace("x", " × "))), h("option", { value: "custom" }, "Custom"));
  sel.value = !fixed ? "fit" : (RES_PRESETS.includes(presetStr) ? presetStr : "custom");
  const w = h("input", { type: "number", value: fixed ? fixed[0] : 1024 }); w.style.width = "90px";
  const hgt = h("input", { type: "number", value: fixed ? fixed[1] : 768 }); hgt.style.width = "90px";
  const applyCustom = () => (e.screen.resolution = { Fixed: [Number(w.value) || 1024, Number(hgt.value) || 768] });
  w.addEventListener("input", applyCustom);
  hgt.addEventListener("input", applyCustom);
  sel.addEventListener("change", () => {
    if (sel.value === "fit") e.screen.resolution = "Fit";
    else if (sel.value === "custom") applyCustom();
    else { const [a, b] = sel.value.split("x").map(Number); e.screen.resolution = { Fixed: [a, b] }; }
    renderEntryEditor(v, e);
  });
  const wrap = h("div", { class: "row-actions" }, sel);
  if (sel.value === "custom") wrap.append(w, hgt);
  return wrap;
}

// ---------- recordings ----------

function fmtDuration(ms) {
  const s = Math.max(0, Math.round(ms / 1000));
  const h = Math.floor(s / 3600), m = Math.floor((s % 3600) / 60), sec = s % 60;
  return h ? `${h}h ${m}m` : m ? `${m}m ${sec}s` : `${sec}s`;
}

function fmtBytes(n) {
  return n < 1024 ? `${n} B` : n < 1048576 ? `${(n / 1024).toFixed(1)} KB` : `${(n / 1048576).toFixed(1)} MB`;
}

function renderRecordings() {
  const recs = state.recordings;
  setHeader("Session recordings", `${recs.length} recorded session(s) - SSH, RDP, VNC and web`, h("button", { onclick: () => go("recordings") }, "Refresh"));
  if (!recs.length) {
    $("view").replaceChildren(h("div", { class: "empty" },
      "No recordings yet. Turn on \"Record all remote sessions\" in a vault's Settings, switch on Record for single entries (Vaults & access → Entries), or tick \"Record sessions\" on an entry in the desktop app."));
    return;
  }
  const rows = recs.map((r) => {
    const live = !r.finished && r.ended_at && (Date.now() - new Date(r.ended_at).getTime()) < 120000;
    const end = r.ended_at ? new Date(r.ended_at) : new Date(r.started_at);
    return h("tr", {},
      h("td", {}, h("div", {}, new Date(r.started_at).toLocaleString()), h("div", { class: "muted" }, fmtDuration(end - new Date(r.started_at)))),
      h("td", {}, h("div", { class: "who" }, avatar(r.username), r.username)),
      h("td", {}, (() => {
        const p = r.protocol || "SSH";
        const kind = { SSH: "Ssh", RDP: "Rdp", VNC: "Vnc", WEB: "Web" }[p] || "Ssh";
        return h("span", { class: "badge kind", style: `background:${KIND_COLORS[kind]}` }, p);
      })()),
      h("td", {}, h("div", {}, r.entry_name, r.flags && r.flags.length ? h("span", { class: "badge danger-badge", style: "margin-left:8px", title: r.flags.join("\n") }, `⚑ ${r.flags.length}`) : null),
        h("div", { class: "muted" }, `${r.login ? r.login + "@" : ""}${r.host} · ${r.vault_name}`)),
      h("td", {}, fmtBytes(r.bytes)),
      h("td", {}, live ? h("span", { class: "rec" }, "● live") : r.finished ? "Finished" : h("span", { class: "muted" }, "Ended (no clean close)")),
      h("td", {}, h("div", { class: "row-actions" },
        h("button", { class: "small primary", onclick: () => viewRecording(r) }, "View"),
        h("button", { class: "small", onclick: () => downloadRecording(r) }, "Download"),
        h("button", { class: "small danger", onclick: () => confirmModal("Delete recording", `Delete the recording of ${r.username} on ${r.host} from ${new Date(r.started_at).toLocaleString()}?`, "Delete",
          async () => { await api("DELETE", `/api/admin/recordings/${r.id}`); toast("Recording deleted"); await loadAll(); render(); }) }, "Delete"))));
  });
  $("view").replaceChildren(h("div", { class: "table-wrap" }, h("table", {},
    h("thead", {}, h("tr", {}, h("th", {}, "Started"), h("th", {}, "User"), h("th", {}, "Type"), h("th", {}, "Target"), h("th", {}, "Size"), h("th", {}, "Status"), h("th", {}))),
    h("tbody", {}, rows))));
}

// AI summary panel shown at the top of a recording view.
function aiBar(r) {
  const box = h("div", { class: "ai-summary" });
  const draw = () => {
    box.replaceChildren();
    if (r.summary) {
      box.append(h("div", { class: "ai-text" }, h("b", {}, "AI summary: "), r.summary));
      if (r.flags && r.flags.length) {
        box.append(h("ul", { class: "ai-flags" }, r.flags.map((f) => h("li", {}, f))));
      }
    } else {
      box.append(h("div", { class: "muted" }, "No AI summary yet."));
    }
    const btn = h("button", { class: "small" }, r.summary ? "Re-summarise" : "Summarise with AI");
    btn.addEventListener("click", async () => {
      btn.disabled = true;
      btn.textContent = "Summarising…";
      try {
        const updated = await api("POST", `/api/admin/recordings/${r.id}/summarize`);
        r.summary = updated.summary;
        r.flags = updated.flags || [];
        const row = state.recordings.find((x) => x.id === r.id);
        if (row) { row.summary = r.summary; row.flags = r.flags; }
        toast("Summary ready");
        draw();
      } catch (e) {
        toast(e.message, true);
        btn.disabled = false;
        btn.textContent = r.summary ? "Re-summarise" : "Summarise with AI";
      }
    });
    box.append(btn);
  };
  draw();
  return box;
}

async function fetchCast(r) {
  const resp = await fetch(`/api/admin/recordings/${r.id}`, { headers: { Authorization: `Bearer ${state.token}` } });
  if (!resp.ok) throw new Error(`Download failed: ${resp.status}`);
  return resp.text();
}

async function downloadRecording(r) {
  try {
    const text = await fetchCast(r);
    const ext = r.kind === "screen" ? "sdrec" : r.kind === "dom" ? "sdweb" : "cast";
    const mime = r.kind === "dom" || r.kind === "screen" ? "application/x-ndjson" : "application/x-asciicast";
    const url = URL.createObjectURL(new Blob([text], { type: mime }));
    const a = h("a", { href: url, download: `${r.started_at.slice(0, 19).replace(/[:T]/g, "-")}_${r.username}_${r.host}.${ext}` });
    document.body.append(a);
    a.click();
    a.remove();
    setTimeout(() => URL.revokeObjectURL(url), 5000);
  } catch (e) { toast(e.message, true); }
}

function parseCast(text) {
  const lines = text.split("\n").filter(Boolean);
  let header = {};
  const events = [];
  for (const [i, line] of lines.entries()) {
    try {
      const v = JSON.parse(line);
      if (i === 0 && !Array.isArray(v)) header = v;
      else if (Array.isArray(v)) events.push(v);
    } catch (_) { /* skip a torn line */ }
  }
  return { header, events };
}

// Minimal screen model: strips escape sequences, honours \r, \n and backspace.
const ESC = /\x1b\[[0-9;?]*[ -\/]*[@-~]|\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)|\x1b[()][A-Za-z0-9]|\x1b[=>78DEHMc]/g;
function screen() {
  const out = [""];
  let col = 0;
  return {
    write(data) {
      for (const ch of data.replace(ESC, "")) {
        let line = out[out.length - 1];
        if (ch === "\n") { out.push(""); col = 0; }
        else if (ch === "\r") col = 0;
        else if (ch === "\b") col = Math.max(0, col - 1);
        else if (ch === "\x07" || ch < " ") continue;
        else {
          line = line.padEnd(col, " ");
          out[out.length - 1] = line.slice(0, col) + ch + line.slice(col + 1);
          col++;
        }
      }
      if (out.length > 5000) out.splice(0, out.length - 5000);
    },
    text() { return out.join("\n"); },
  };
}

// Keystrokes grouped into the lines the user submitted.
function keystrokes(events) {
  const lines = [];
  let cur = "", start = null;
  const named = { "\x7f": "", "\x1b[A": "<Up>", "\x1b[B": "<Down>", "\x1b[C": "<Right>", "\x1b[D": "<Left>", "\t": "<Tab>", "\x1b": "<Esc>" };
  for (const [t, kind, data] of events) {
    if (kind !== "i") continue;
    if (start === null) start = t;
    if (data in named) {
      if (data === "\x7f") cur = cur.slice(0, -1); else cur += named[data];
      continue;
    }
    for (const ch of data) {
      if (ch === "\r" || ch === "\n") { lines.push([start, cur]); cur = ""; start = null; }
      else if (ch === "\x7f") cur = cur.slice(0, -1);
      else if (ch === "\x03" || ch === "\x04") {
        // Ctrl+C / Ctrl+D end what was being typed.
        lines.push([start ?? t, `${cur}^${ch === "\x03" ? "C" : "D"}`]);
        cur = ""; start = null;
      }
      else if (ch < " ") cur += `^${String.fromCharCode(ch.charCodeAt(0) + 64)}`;
      else cur += ch;
    }
  }
  if (cur) lines.push([start ?? 0, cur]);
  return lines;
}

// Typed text, key combinations and clicks of an RDP/VNC recording, in order.
function screenActivity(events) {
  const typed = keystrokes(events).map(([t, line]) => [t, "typed", line]);
  const other = events.filter((e) => e[1] === "k" || e[1] === "m")
    .map(([t, kind, data]) => [t, kind === "k" ? "key" : "click", kind === "k" ? data : data.split(" ").slice(0, 2).join(" ")]);
  return typed.concat(other).sort((a, b) => a[0] - b[0]);
}

async function viewScreenRecording(r, cast) {
  const frames = cast.events.filter((e) => e[1] === "f");
  const clicks = cast.events.filter((e) => e[1] === "m").map(([t, , data]) => {
    const [, xy, wh] = data.split(" ");
    const [x, y] = (xy || "0,0").split(",").map(Number);
    const [w, hh] = (wh || "0x0").split("x").map(Number);
    return { t, x: w ? x / w : 0, y: hh ? y / hh : 0 };
  });
  const total = cast.events.length ? cast.events[cast.events.length - 1][0] : 0;
  const img = h("img", { class: "frame", alt: "Recorded screen" });
  const marker = h("div", { class: "click-marker hidden" });
  // The marker is positioned in % of the image, so both share a box sized to the image.
  const stage = h("div", { class: "stage" }, frames.length ? h("div", { class: "shot" }, img, marker) : h("p", { class: "muted" }, "No screen frames recorded."));

  let timer = null, clock = 0, speed = 4, shown = -1;
  const pos = h("input", { type: "range", min: "0", max: String(Math.max(1, Math.ceil(total))), value: "0", class: "progress" });
  const timeLabel = h("span", { class: "muted" }, `0s / ${total.toFixed(0)}s`);
  const playBtn = h("button", {}, "Play");
  const draw = () => {
    // Newest frame at or before the clock.
    let i = shown;
    if (i < 0 || i >= frames.length || frames[i][0] > clock) i = -1;
    while (i + 1 < frames.length && frames[i + 1][0] <= clock) i++;
    if (i < 0 && frames.length) i = 0; // before the first capture: show it anyway
    if (i >= 0 && i !== shown) { img.src = `data:image/jpeg;base64,${frames[i][2]}`; shown = i; }
    const click = clicks.filter((c) => c.t <= clock && clock - c.t < 1.2).pop();
    marker.classList.toggle("hidden", !click);
    if (click) { marker.style.left = `${click.x * 100}%`; marker.style.top = `${click.y * 100}%`; }
    pos.value = String(Math.floor(clock));
    timeLabel.textContent = `${clock.toFixed(0)}s / ${total.toFixed(0)}s`;
  };
  const stop = () => { clearInterval(timer); timer = null; playBtn.textContent = "Play"; };
  const play = () => {
    if (clock >= total) { clock = 0; shown = -1; }
    playBtn.textContent = "Pause";
    timer = setInterval(() => {
      clock = Math.min(total, clock + 0.1 * speed);
      if (clock >= total) stop();
      draw();
    }, 100);
  };
  playBtn.addEventListener("click", () => (timer ? stop() : play()));
  pos.addEventListener("input", () => { stop(); clock = Number(pos.value); shown = -1; draw(); });
  const speedSel = h("select", { style: "width:auto" }, ["1", "2", "4", "8", "16"].map((s) => { const o = h("option", { value: s }, `${s}x`); if (s === "4") o.selected = true; return o; }));
  speedSel.addEventListener("change", () => { speed = Number(speedSel.value); });

  const activity = screenActivity(cast.events);
  const jump = (t) => { stop(); clock = t; shown = -1; draw(); showTab(true); };
  const keysBox = h("div", { class: "keys" }, activity.length
    ? activity.map(([t, kind, text]) => h("div", { class: "jump", onclick: () => jump(t), title: "Show the screen at this moment" },
      h("span", { class: "t" }, `${t.toFixed(1)}s`), h("span", { class: `act ${kind}` }, kind), text || h("span", { class: "muted" }, "(Enter)")))
    : [h("p", { class: "muted" }, "No typing or clicks recorded.")]);

  const replayPane = h("div", {}, h("div", { class: "tabs" }, playBtn, speedSel, pos, timeLabel), stage);
  const keysPane = h("div", { class: "hidden" }, h("p", { class: "muted" }, "Typed text (Enter-separated), key combinations and clicks. Click a line to see the screen at that moment. Typed passwords appear here too."), keysBox);
  const tabReplay = h("button", { class: "on" }, "Replay screen");
  const tabKeys = h("button", {}, `Activity (${activity.length})`);
  const showTab = (replay) => {
    tabReplay.classList.toggle("on", replay); tabKeys.classList.toggle("on", !replay);
    replayPane.classList.toggle("hidden", !replay); keysPane.classList.toggle("hidden", replay);
  };
  tabReplay.addEventListener("click", () => showTab(true));
  tabKeys.addEventListener("click", () => showTab(false));

  draw();
  modal(`${r.username} → ${r.protocol || "RDP"} ${r.login ? r.login + "@" : ""}${r.host}  ·  ${new Date(r.started_at).toLocaleString()}`,
    h("div", {}, aiBar(r), h("div", { class: "tabs" }, tabReplay, tabKeys, h("button", { class: "right", onclick: () => downloadRecording(r) }, "Download")), replayPane, keysPane),
    "Close", async () => { stop(); }, { wide: true });
  $("modal-cancel").classList.add("hidden");
  const observer = new MutationObserver(() => { if ($("modal-backdrop").classList.contains("hidden")) { stop(); observer.disconnect(); } });
  observer.observe($("modal-backdrop"), { attributes: true, attributeFilter: ["class"] });
}

// Browser (rrweb) recording: play it in the sandboxed /replay iframe. We fetch
// the events here (authenticated) and post them into the iframe.
async function viewDomRecording(r) {
  let text;
  try { text = await fetchCast(r); } catch (e) { toast(e.message, true); return; }
  const events = [];
  for (const line of text.split("\n")) {
    if (!line) continue;
    try {
      const o = JSON.parse(line);
      if (o && typeof o.type === "number" && typeof o.timestamp === "number") events.push(o);
    } catch (_) { /* skip header / torn line */ }
  }
  if (events.length < 2) { toast("This recording has no replayable page data yet.", true); return; }

  const frame = h("iframe", { class: "replay-frame", src: "/replay" });
  const onMsg = (e) => {
    if (e.source === frame.contentWindow && e.data && e.data.type === "replay-ready") {
      frame.contentWindow.postMessage({ type: "events", events, width: 1000, height: 640 }, location.origin);
    }
  };
  window.addEventListener("message", onMsg);
  modal(`${r.username} → web ${r.host}  ·  ${new Date(r.started_at).toLocaleString()}`,
    h("div", {}, aiBar(r), h("div", { class: "tabs" }, h("span", { class: "muted" }, `${events.length} events`), h("button", { class: "right", onclick: () => downloadRecording(r) }, "Download")), frame),
    "Close", async () => { window.removeEventListener("message", onMsg); }, { wide: true });
  $("modal-cancel").classList.add("hidden");
  const observer = new MutationObserver(() => { if ($("modal-backdrop").classList.contains("hidden")) { window.removeEventListener("message", onMsg); observer.disconnect(); } });
  observer.observe($("modal-backdrop"), { attributes: true, attributeFilter: ["class"] });
}

async function viewRecording(r) {
  if (r.kind === "dom") return viewDomRecording(r);
  let cast;
  try { cast = parseCast(await fetchCast(r)); } catch (e) { toast(e.message, true); return; }
  if (r.kind === "screen") return viewScreenRecording(r, cast);
  const outputs = cast.events.filter((e) => e[1] === "o");
  const total = cast.events.length ? cast.events[cast.events.length - 1][0] : 0;
  const pre = h("pre", { class: "term" });
  const keysBox = h("div", { class: "keys" });
  const typed = keystrokes(cast.events);
  keysBox.replaceChildren(...(typed.length ? typed.map(([t, line]) => h("div", {}, h("span", { class: "t" }, `${t.toFixed(1)}s`), line || h("span", { class: "muted" }, "(Enter)")))
    : [h("p", { class: "muted" }, "No keystrokes recorded.")]));

  // Player
  let timer = null, idx = 0, scr = screen(), clock = 0, speed = 4;
  const pos = h("input", { type: "range", min: "0", max: String(Math.max(1, Math.ceil(total))), value: "0", class: "progress" });
  const timeLabel = h("span", { class: "muted" }, `0s / ${total.toFixed(0)}s`);
  const playBtn = h("button", {}, "Play");
  const draw = () => { pre.textContent = scr.text(); pre.scrollTop = pre.scrollHeight; pos.value = String(Math.floor(clock)); timeLabel.textContent = `${clock.toFixed(0)}s / ${total.toFixed(0)}s`; };
  const seek = (t) => { scr = screen(); idx = 0; clock = t; while (idx < outputs.length && outputs[idx][0] <= t) scr.write(outputs[idx++][2]); draw(); };
  const stop = () => { clearInterval(timer); timer = null; playBtn.textContent = "Play"; };
  const play = () => {
    if (idx >= outputs.length) seek(0);
    playBtn.textContent = "Pause";
    timer = setInterval(() => {
      clock += 0.05 * speed;
      while (idx < outputs.length && outputs[idx][0] <= clock) scr.write(outputs[idx++][2]);
      if (idx >= outputs.length) { clock = total; stop(); }
      draw();
    }, 50);
  };
  playBtn.addEventListener("click", () => (timer ? stop() : play()));
  pos.addEventListener("input", () => { stop(); seek(Number(pos.value)); });
  const speedSel = h("select", { style: "width:auto" }, ["1", "2", "4", "8", "16"].map((s) => { const o = h("option", { value: s }, `${s}x`); if (s === "4") o.selected = true; return o; }));
  speedSel.addEventListener("change", () => { speed = Number(speedSel.value); });
  const endBtn = h("button", { onclick: () => { stop(); seek(total); } }, "Show all");

  const replayPane = h("div", {}, h("div", { class: "tabs" }, playBtn, speedSel, pos, timeLabel, endBtn), pre);
  const keysPane = h("div", { class: "hidden" }, h("p", { class: "muted" }, "Lines typed by the user (Enter-separated). Typed passwords appear here too."), keysBox);
  const tabReplay = h("button", { class: "on" }, "Replay screen");
  const tabKeys = h("button", {}, `Typed commands (${typed.length})`);
  tabReplay.addEventListener("click", () => { tabReplay.classList.add("on"); tabKeys.classList.remove("on"); replayPane.classList.remove("hidden"); keysPane.classList.add("hidden"); });
  tabKeys.addEventListener("click", () => { tabKeys.classList.add("on"); tabReplay.classList.remove("on"); keysPane.classList.remove("hidden"); replayPane.classList.add("hidden"); });

  seek(total);
  modal(`${r.username} → ${r.login ? r.login + "@" : ""}${r.host}  ·  ${new Date(r.started_at).toLocaleString()}`,
    h("div", {}, aiBar(r), h("div", { class: "tabs" }, tabReplay, tabKeys, h("button", { class: "right", onclick: () => downloadRecording(r) }, "Download .cast")), replayPane, keysPane),
    "Close", async () => { stop(); }, { wide: true });
  $("modal-cancel").classList.add("hidden");
  const observer = new MutationObserver(() => { if ($("modal-backdrop").classList.contains("hidden")) { stop(); observer.disconnect(); } });
  observer.observe($("modal-backdrop"), { attributes: true, attributeFilter: ["class"] });
}

// ---------- audit log ----------

const AUDIT_ACTIONS = [
  "login", "login_failed", "logout", "password_changed",
  "entry_write", "entry_delete", "entry_recording",
  "vault_create", "vault_update", "vault_delete",
  "team_create", "team_update", "team_delete",
  "user_create", "user_update", "sessions_revoked",
  "recording_view", "recording_delete", "download",
];

async function renderAudit() {
  setHeader("Audit log", "Every recorded action on the server, newest first", h("button", { onclick: () => go("audit") }, "Refresh"));

  const userIn = h("input", { placeholder: "filter by user", value: state.auditUser || "" });
  userIn.style.width = "160px";
  const actionSel = h("select", {}, h("option", { value: "" }, "all actions"),
    ...AUDIT_ACTIONS.map((a) => { const o = h("option", { value: a }, a); if (a === state.auditAction) o.selected = true; return o; }));
  actionSel.style.width = "auto";
  const run = async () => { state.auditUser = userIn.value.trim(); state.auditAction = actionSel.value; await renderAudit(); };
  userIn.addEventListener("keydown", (e) => { if (e.key === "Enter") run(); });
  actionSel.addEventListener("change", run);

  let events = [];
  try {
    const q = new URLSearchParams({ limit: "1000" });
    if (state.auditUser) q.set("user", state.auditUser);
    if (state.auditAction) q.set("action", state.auditAction);
    events = await api("GET", "/api/admin/audit?" + q.toString());
  } catch (e) { toast(e.message, true); }

  const badge = (a) => {
    const danger = a === "login_failed" || a.endsWith("_delete");
    const warn = a === "sessions_revoked" || a === "recording_view" || a === "entry_recording";
    return h("span", { class: "badge " + (danger ? "danger-badge" : warn ? "warn-badge" : "neutral") }, a);
  };
  const rows = events.map((ev) => h("tr", {},
    h("td", { class: "muted", title: new Date(ev.at).toLocaleString() }, new Date(ev.at).toLocaleString()),
    h("td", {}, h("div", { class: "who" }, avatar(ev.username), ev.username)),
    h("td", {}, badge(ev.action)),
    h("td", {}, ev.target || h("span", { class: "muted" }, "-")),
    h("td", { class: "muted" }, ev.detail || "")));

  const bar = h("div", { class: "grant-add" }, h("label", {}, "User", userIn), h("label", {}, "Action", actionSel),
    h("label", {}, " ", h("button", { onclick: run }, "Apply")),
    h("label", {}, " ", h("span", { class: "muted" }, `${events.length} event(s)`)));
  $("view").replaceChildren(h("div", {}, bar,
    events.length
      ? h("div", { class: "table-wrap" }, h("table", {},
          h("thead", {}, h("tr", {}, h("th", {}, "When"), h("th", {}, "User"), h("th", {}, "Action"), h("th", {}, "Target"), h("th", {}, "Detail"))),
          h("tbody", {}, rows)))
      : h("div", { class: "empty" }, "No audit events match.")));
}

// ---------- settings ----------

function uid() {
  try { return crypto.randomUUID(); } catch (e) { return "id" + Date.now() + Math.random().toString(16).slice(2); }
}

async function renderSettings() {
  setHeader("Settings", "Server configuration — AI endpoints, models, access and retention");
  let cfg;
  try { cfg = await api("GET", "/api/admin/config"); } catch (e) { toast(e.message, true); return; }

  // Working copies; endpoint keys stay blank (blank = keep the stored key).
  let endpoints = (cfg.ai_endpoints || []).map((e) => ({ id: e.id, label: e.label, url: e.url, key_set: e.key_set, key: "" }));
  let models = (cfg.ai_models || []).map((m) => ({ id: m.id, label: m.label, endpoint: m.endpoint, model: m.model }));
  let def = cfg.ai_default || (models[0] && models[0].id) || "";
  const users = cfg.users || [];
  // access: username -> Set(model ids)
  const access = {};
  (cfg.ai_access || []).forEach((a) => { access[a.username] = new Set(a.models || []); });

  const epList = h("div", {});
  const modelList = h("div", {});
  const accessList = h("div", {});

  const endpointName = (e) => (e && (e.label || e.url)) || "(unnamed endpoint)";

  // ---- endpoints ----
  const drawEndpoints = () => {
    if (!endpoints.length) { epList.replaceChildren(h("p", { class: "muted" }, "No endpoints yet. Add one (e.g. your Ollama server), then fetch its models.")); return; }
    epList.replaceChildren(...endpoints.map((e, i) => {
      const label = h("input", { value: e.label || "", placeholder: "name, e.g. Local Ollama" });
      const url = h("input", { value: e.url || "", placeholder: "http://localhost:11434/v1" });
      const key = h("input", { type: "password", placeholder: e.key_set ? "•••• set — blank keeps it" : "(none)" });
      label.addEventListener("input", () => (e.label = label.value));
      url.addEventListener("input", () => (e.url = url.value));
      key.addEventListener("input", () => (e.key = key.value));
      const out = h("span", { class: "muted" });
      const chips = h("div", { class: "model-chips" });
      const fetchBtn = h("button", { class: "small" }, "Fetch models");
      fetchBtn.addEventListener("click", async () => {
        out.className = "muted"; out.textContent = "";
        chips.replaceChildren(h("span", { class: "muted" }, "Loading…"));
        try {
          const r = await api("POST", "/api/admin/config/list-models", { url: e.url, key: e.key, endpoint: e.id });
          if (!r.ok) { chips.replaceChildren(h("span", { class: "error" }, "Failed: " + r.error)); return; }
          if (!r.models || !r.models.length) { chips.replaceChildren(h("span", { class: "muted" }, "This endpoint reported no models")); return; }
          chips.replaceChildren(
            h("span", { class: "muted" }, "Add a model:"),
            ...r.models.map((name) => {
              const chip = h("button", { class: "small chip" }, name);
              chip.addEventListener("click", () => {
                if (!models.some((m) => m.endpoint === e.id && m.model === name)) {
                  models.push({ id: uid(), label: name, endpoint: e.id, model: name });
                  if (!def) def = models[0].id;
                  drawModels(); drawAccess();
                }
              });
              return chip;
            })
          );
        } catch (ex) { chips.replaceChildren(h("span", { class: "error" }, "Failed: " + ex.message)); }
      });
      const rm = h("button", { class: "small danger" }, "Remove");
      rm.addEventListener("click", () => {
        endpoints.splice(i, 1);
        models = models.filter((m) => m.endpoint !== e.id); // models need an endpoint
        drawEndpoints(); drawModels(); drawAccess();
      });
      return h("div", { class: "model-row" },
        h("div", { class: "model-grid" },
          h("label", {}, "Name", label), h("label", {}, "Endpoint URL (ends in /v1)", url), h("label", {}, "API key (optional)", key)),
        h("div", { class: "model-actions" }, fetchBtn, rm, h("span", { class: "grow" }), out),
        chips);
    }));
  };

  // ---- models ----
  const drawModels = () => {
    if (!endpoints.length) { modelList.replaceChildren(h("p", { class: "muted" }, "Add an endpoint first.")); return; }
    if (!models.length) { modelList.replaceChildren(h("p", { class: "muted" }, "No models yet. Use an endpoint's “Fetch models”, or add one below.")); return; }
    modelList.replaceChildren(...models.map((m, i) => {
      const label = h("input", { value: m.label || "", placeholder: "name shown to users" });
      const name = h("input", { value: m.model || "", placeholder: "llama3.1" });
      label.addEventListener("input", () => (m.label = label.value));
      name.addEventListener("input", () => (m.model = name.value));
      const sel = h("select", {}, ...endpoints.map((e) => h("option", { value: e.id }, endpointName(e))));
      sel.value = m.endpoint;
      sel.addEventListener("change", () => (m.endpoint = sel.value));
      const defRadio = h("input", { type: "radio", name: "defmodel" });
      defRadio.checked = def === m.id;
      defRadio.addEventListener("change", () => { def = m.id; });
      const out = h("span", { class: "muted" });
      const test = h("button", { class: "small" }, "Test");
      test.addEventListener("click", async () => {
        const e = endpoints.find((x) => x.id === m.endpoint) || {};
        out.className = "muted"; out.textContent = "Testing…";
        try {
          const r = await api("POST", "/api/admin/config/test-ai", { url: e.url, key: e.key, endpoint: m.endpoint, model: m.model });
          out.className = r.ok ? "ok-text" : "error";
          out.textContent = r.ok ? "OK — " + (r.reply || "reachable") : "Failed: " + r.error;
        } catch (ex) { out.className = "error"; out.textContent = "Failed: " + ex.message; }
      });
      const rm = h("button", { class: "small danger" }, "Remove");
      rm.addEventListener("click", () => {
        const id = m.id;
        models.splice(i, 1);
        Object.values(access).forEach((s) => s.delete(id));
        if (def === id) def = models[0] ? models[0].id : "";
        drawModels(); drawAccess();
      });
      return h("div", { class: "model-row" },
        h("div", { class: "model-grid" },
          h("label", {}, "Name", label), h("label", {}, "Model", name), h("label", {}, "Endpoint", sel)),
        h("div", { class: "model-actions" }, h("label", { class: "check" }, defRadio, "Default"), test, out, h("span", { class: "grow" }), rm));
    }));
  };

  // ---- per-user access ----
  const drawAccess = () => {
    if (!models.length) { accessList.replaceChildren(h("p", { class: "muted" }, "Add a model first, then grant users access to it.")); return; }
    if (!users.length) { accessList.replaceChildren(h("p", { class: "muted" }, "No non-admin users yet. Administrators can use every model.")); return; }
    accessList.replaceChildren(...users.map((u) => {
      const set = access[u] || (access[u] = new Set());
      const boxes = models.map((m) => {
        const cb = h("input", { type: "checkbox" });
        cb.checked = set.has(m.id);
        cb.addEventListener("change", () => { cb.checked ? set.add(m.id) : set.delete(m.id); });
        return h("label", { class: "check" }, cb, m.label || m.model);
      });
      return h("div", { class: "access-row" },
        h("div", { class: "access-user" }, u),
        h("div", { class: "access-models" }, ...boxes));
    }));
  };

  drawEndpoints(); drawModels(); drawAccess();

  const addEp = h("button", { class: "small" }, "+ Add endpoint");
  addEp.addEventListener("click", () => { endpoints.push({ id: uid(), label: "", url: "", key_set: false, key: "" }); drawEndpoints(); drawModels(); });
  const addModel = h("button", { class: "small" }, "+ Add model");
  addModel.addEventListener("click", () => { models.push({ id: uid(), label: "", endpoint: endpoints[0] ? endpoints[0].id : "", model: "" }); if (!def) def = models[0].id; drawModels(); drawAccess(); });

  const recDays = h("input", { type: "number", min: "0", value: String(cfg.recording_retention_days || 0) });
  recDays.style.width = "100px";
  const audDays = h("input", { type: "number", min: "0", value: String(cfg.audit_retention_days || 0) });
  audDays.style.width = "100px";

  const save = h("button", { class: "primary" }, "Save settings");
  save.addEventListener("click", async () => {
    const body = {
      ai_endpoints: endpoints.map((e) => { const o = { id: e.id, label: e.label, url: e.url }; if (e.key) o.key = e.key; return o; }),
      ai_models: models.map((m) => ({ id: m.id, label: m.label, endpoint: m.endpoint, model: m.model })),
      ai_default: def,
      ai_access: Object.entries(access).map(([username, set]) => ({ username, models: [...set] })).filter((a) => a.models.length),
      recording_retention_days: Number(recDays.value) || 0,
      audit_retention_days: Number(audDays.value) || 0,
    };
    try { await api("PUT", "/api/admin/config", body); toast("Settings saved"); await renderSettings(); }
    catch (e) { toast(e.message, true); }
  });

  $("view").replaceChildren(h("div", { class: "card", style: "max-width:820px" },
    h("h3", {}, "AI endpoints (APIs)"),
    h("p", { class: "muted" }, "Each endpoint is one OpenAI-compatible API (Ollama / llama.cpp / vLLM). One endpoint can serve many models — use “Fetch models” to add them. Nothing leaves your network."),
    epList, h("div", { style: "margin-top:10px" }, addEp),
    h("h3", { style: "margin-top:22px" }, "Models"),
    h("p", { class: "muted" }, "Each model is a model name served by one endpoint. The Default model is used for session summaries and for users who don't pick one."),
    modelList, h("div", { style: "margin-top:10px" }, addModel),
    h("h3", { style: "margin-top:22px" }, "User access"),
    h("p", { class: "muted" }, "Pick which models each user may use in the in-session assistant. Users with no models ticked get no AI. Administrators can always use every model."),
    accessList,
    h("h3", { style: "margin-top:22px" }, "Retention"),
    h("div", { class: "grant-add" },
      h("label", {}, "Recordings older than (days)", recDays),
      h("label", {}, "Audit entries older than (days)", audDays)),
    h("p", { class: "muted" }, "0 = keep forever. Pruned on start and daily."),
    h("div", { style: "margin-top:16px" }, save)));
}

// ---------- download ----------

function renderDownload() {
  setHeader("Download the desktop app", "SystemDeck for Windows - connect to this server and use your team's vaults");
  const files = state.downloads || [];
  const download = async (f) => {
    try {
      toast(`Downloading ${f.name}...`);
      const resp = await fetch(`/api/downloads/${encodeURIComponent(f.name)}`, { headers: { Authorization: `Bearer ${state.token}` } });
      if (!resp.ok) throw new Error(`Download failed: ${resp.status}`);
      const url = URL.createObjectURL(await resp.blob());
      const a = h("a", { href: url, download: f.name });
      document.body.append(a);
      a.click();
      a.remove();
      setTimeout(() => URL.revokeObjectURL(url), 10000);
    } catch (e) { toast(e.message, true); }
  };
  // What each published file is, so users pick the right one.
  const describe = (name) => {
    const n = name.toLowerCase();
    if (n.includes("portable")) return "Portable - no install, no admin rights: just run it.";
    if (n.includes("setup")) return "Installer - adds SystemDeck to the Start menu.";
    if (n.endsWith(".zip")) return "Zip - unzip anywhere and run systemdeck.exe.";
    if (n.endsWith(".crt")) return "Server certificate - install it to trust this server's HTTPS.";
    return "";
  };
  const rank = (name) => ["portable", "setup", ".zip"].findIndex((k) => name.toLowerCase().includes(k)) >>> 0;
  const ordered = [...files].sort((a, b) => rank(a.name) - rank(b.name));
  const list = files.length
    ? h("ul", { class: "list" }, ordered.map((f) => h("li", {},
      h("span", { class: "badge kind", style: "background:#38c2b2" }, f.name.endsWith(".zip") ? "ZIP" : f.name.endsWith(".crt") ? "CRT" : "EXE"),
      h("div", { class: "grow" }, h("div", {}, f.name),
        describe(f.name) ? h("div", {}, describe(f.name)) : null,
        h("div", { class: "muted" }, `${fmtBytes(f.bytes)}${f.modified ? " · built " + new Date(f.modified).toLocaleString() : ""}`)),
      h("button", { class: "primary", onclick: () => download(f) }, "Download"))))
    : h("p", { class: "muted" }, "No download has been published on this server yet. Ask your administrator (it is created by build.ps1 in the server's downloads folder).");
  $("view").replaceChildren(h("div", { class: "card" },
    list,
    h("h3", {}, "Setup"),
    h("ol", { class: "steps" },
      h("li", {}, "Download SystemDeck-Portable.exe and run it (nothing to install). Or use the installer / zip."),
      h("li", {}, "Click the account button → Connect to server."),
      h("li", {}, "Server: ", h("code", {}, location.origin), " · your SystemDeck username and password."),
      h("li", {}, "Your team's vaults appear in the vault picker and stay available offline."))));
}

// ---------- wiring ----------

document.addEventListener("DOMContentLoaded", () => {
  setTheme(prefs("vd-theme"));
  $("theme-toggle").addEventListener("click", () => {
    const current = document.documentElement.getAttribute("data-theme")
      || (window.matchMedia("(prefers-color-scheme: dark)").matches ? "dark" : "light");
    const next = current === "dark" ? "light" : "dark";
    prefs("vd-theme", next);
    setTheme(next);
  });
  $("logout").addEventListener("click", () => signOut(true));
  for (const b of document.querySelectorAll(".nav-item")) b.addEventListener("click", () => go(b.dataset.view));
  $("modal-cancel").addEventListener("click", () => { closeModal(); $("modal-cancel").classList.remove("hidden"); });
  $("modal-backdrop").addEventListener("click", (e) => { if (e.target.id === "modal-backdrop") closeModal(); });
  document.addEventListener("keydown", (e) => { if (e.key === "Escape") closeModal(); });
  new MutationObserver(() => { if ($("modal-backdrop").classList.contains("hidden")) $("modal-cancel").classList.remove("hidden"); })
    .observe($("modal-backdrop"), { attributes: true, attributeFilter: ["class"] });

  $("login-form").addEventListener("submit", async (e) => {
    e.preventDefault();
    const btn = e.submitter;
    if (btn) btn.disabled = true;
    try {
      const resp = await api("POST", "/api/login", { username: $("login-user").value, password: $("login-pass").value });
      const remember = $("login-remember").checked;
      prefs("vd-remember", remember ? "1" : "0");
      state.token = resp.token;
      store("vd-token", resp.token, remember);
      $("login-pass").value = "";
      await start();
    } catch (err) {
      showLogin(err.message);
    } finally {
      if (btn) btn.disabled = false;
    }
  });

  $("login-remember").checked = prefs("vd-remember") !== "0";
  state.token = store("vd-token");
  if (state.token) start(); else showLogin();
});
