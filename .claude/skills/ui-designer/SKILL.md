---
name: ui-designer
description: Design and polish desktop UI for SystemDeck (egui/eframe). Use when changing layout, styling, icons, colors, panels, tabs, forms, or when the user asks to "improve the UI", "make it look better", "redesign", or reports glyphs rendering as boxes.
---

# UI designer (SystemDeck / egui)

The look follows the user's reference: a clean messaging-app layout (list column + content pane
with round teal action buttons), in light or dark mode.

## Where things live
- `client/src/theme.rs`: palette constants, `apply()` (light visuals, radii, spacing, text sizes), `kind_color`, `avatar`, `pill`, painted `Icon`s, `round_button`, `initials`, light/dark palette functions.
- `client/src/app/view.rs`: the layout (`list_panel`, `status_line`, `content_panel` → `content_header` + session pills + `details_ui` cards, `account_button`, `vault_picker`) and the small widgets (`entry_row`, `group_header`, `tab_pill`, `card`, `field`, `stat_card`, `search_box`).
- `client/src/app.rs`: state, `Action` enum + `handle()`, `editor_ui`, modal windows (login, import, admin, password).

## Layout contract (two columns: the user rejected a separate nav rail)
1. **List column** (~320 px, resizable): the header row has the **account avatar** (initials, status dot; click opens the menu with Connect/Sync/Admin/Change password/Dark mode/session mode/Sign out), the **vault picker** (title as a dropdown with counters, red when items need attention), the "N connections • online/offline/local only" line, and round Sync / `+` (primary) / Import buttons. Below that sit the pill search, collapsible grey group headers, and 60 px entry rows (kind avatar circle, bold name, grey "user · host", time, `live`/`conflict` pill or amber unsynced dot; selected = teal-tinted row + 3 px accent bar).
2. **Content**: header with entry avatar, name, "Type · host:port" + status, and round buttons Connect (primary) / Files (SSH) / Edit / More. Below it, session **pills** (Details + one per open session). The body is the cards column (max 780 px, explicit width) on `app_bg()`, or the live session.
3. Status line: slim bar at the bottom of the content column, only while a message exists.
4. Don't add a second navigation column. Put new global actions in the account menu.

## Visual rules
- **Light and dark mode**: never hard-code surface or text colours. Use `theme::panel()`, `app_bg()`, `border()`, `text()`, `text_weak()`, `row_selected()`, `row_hover()`, `subtle()`, `danger_bg()`, which follow `theme::is_dark()`. Fixed colours are only `ACCENT`, `ACCENT_DARK`, `OK`, `WARN`, `DANGER`, kind colours, and white text on coloured fills. `theme::apply(ctx, dark)` switches the mode; it's saved as `Settings::dark_mode`.
- Kind colours (avatars): RDP blue, SSH green, VNC purple, TEL orange, COM brown, WEB cyan, KEY gold, TXT grey (`theme::kind_color`).
- **No emoji or icon glyphs.** The bundled fonts render many as boxes. Draw icons with `theme::paint_icon`, or add a new `Icon` variant drawn with painter primitives. `•` is safe.
- One primary (teal, filled) action per area. Everything else is an outline round button or a plain button. Destructive actions are red text and need confirmation.
- Text: field labels in `TEXT_WEAK`, values in `TEXT` monospace, section titles in small uppercase `TEXT_WEAK`. Missing values show an italic "not set".
- Give the details column an explicit width (`ui.set_width`), because ScrollArea remembers wider content and cards overflow.
- Type float literals in `Stroke::new` (`1.0_f32`).

## Workflow
1. Read `theme.rs` and `app/view.rs` before editing. Put new colours and widgets in `theme.rs`.
2. Inside panel closures, push `Action`s and handle them after rendering. Never mutate state the closure is borrowing.
3. Stop `systemdeck.exe`, then build with the WinLibs toolchain on PATH (`vaultdesk/build.ps1`).
4. **Verify visually**: launch and capture the window with `CopyFromScreen` (PowerShell + System.Drawing). `PrintWindow` is white for OpenGL. If `logonui` is running, the screen is locked, so report that rather than guess. To check interactions, use `SetCursorPos` + `mouse_event` on window-relative coordinates, then capture again.
5. Look for glyph boxes, clipped text, card overflow on the right edge, and rows that crowd the list column.
