//! Main window layout (three columns, after the reference design):
//! connection list (with account menu + vault picker) · content (details or live session).

use std::collections::{BTreeMap, HashSet};

use chrono::Local;
use eframe::egui::{self, Color32, FontId, Frame, Margin, RichText, Sense, Stroke};
use uuid::Uuid;
use systemdeck_core::EntryKind;

use super::{editor_ui, Action, App, Conn, ALL_VAULTS, VAULT_HEADER};
use crate::{
    sessions::Status,
    store::{CachedVault, LocalEntry, LOCAL_VAULT},
    theme::{self, avatar, initials, kind_color, pill, round_button, status_color, status_text, Icon},
};

impl App {
    // ------------------------------------------------------------------
    // Connection list
    // ------------------------------------------------------------------

    pub(super) fn list_panel(&mut self, ctx: &egui::Context, actions: &mut Vec<Action>) {
        let writable = self.can_add();
        egui::SidePanel::left("list")
            .default_width(320.0)
            .width_range(260.0..=460.0)
            .resizable(true)
            .frame(Frame::new().fill(theme::panel()).stroke(Stroke::new(1.0_f32, theme::border())))
            .show(ctx, |ui| {
                Frame::new().inner_margin(Margin { left: 14, right: 12, top: 14, bottom: 10 }).show(ui, |ui| {
                    ui.horizontal(|ui| {
                        self.account_button(ui, actions);
                        ui.add_space(2.0);
                        ui.vertical(|ui| {
                            ui.spacing_mut().item_spacing.y = 0.0;
                            self.vault_picker(ui, actions);
                            let (dot, state) = match self.conn {
                                Conn::Online => (theme::OK, "online"),
                                Conn::Offline => (theme::WARN, "offline"),
                                Conn::SignedOut => (theme::text_weak(), "local only"),
                            };
                            ui.horizontal(|ui| {
                                ui.label(RichText::new(format!("{} connections", self.entries.len())).color(theme::text_weak()).size(11.5));
                                ui.label(RichText::new(format!("• {state}")).color(dot).size(11.5));
                            });
                        });
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            let enabled = writable && self.draft.is_none();
                            if ui.add_enabled_ui(enabled, |ui| round_button(ui, Icon::Import, false, 30.0, "Import CSV")).inner.clicked() {
                                actions.push(Action::ShowImport);
                            }
                            if ui.add_enabled_ui(enabled, |ui| round_button(ui, Icon::Plus, true, 30.0, "New entry")).inner.clicked() {
                                actions.push(Action::New);
                            }
                        });
                    });
                    ui.add_space(10.0);
                    ui.horizontal(|ui| {
                        let buttons = if self.signed_in() { 2.0 } else { 1.0 };
                        // Buttons are 32 px + item spacing; leave slack so the panel never grows to fit.
                        let spacing = ui.spacing().item_spacing.x;
                        let width = (ui.available_width() - buttons * (32.0 + spacing) - 2.0).max(120.0);
                        ui.allocate_ui(egui::vec2(width, 34.0), |ui| search_box(ui, &mut self.search));
                        if round_button(ui, Icon::Check, self.select_mode, 32.0, "Select several entries to push to a server vault").clicked() {
                            actions.push(Action::SetSelectMode(!self.select_mode));
                        }
                        if self.sync_rx.is_some() {
                            ui.add_sized(egui::vec2(32.0, 32.0), egui::Spinner::new().size(16.0));
                        } else if self.signed_in() && round_button(ui, Icon::Sync, false, 32.0, "Sync now").clicked() {
                            actions.push(Action::SyncNow);
                        }
                    });
                    if self.select_mode {
                        ui.add_space(8.0);
                        self.push_bar(ui, actions);
                    }
                });
                let (top, w) = (ui.cursor().min.y, ui.max_rect().width());
                ui.painter().hline(ui.max_rect().x_range(), top, Stroke::new(1.0_f32, theme::border()));
                let _ = w;

                // Sections: one per vault when showing all vaults, else just the picked vault.
                let all_vaults = self.current_vault == ALL_VAULTS;
                let mut sections: Vec<(Option<&CachedVault>, BTreeMap<String, Vec<&LocalEntry>>)> = Vec::new();
                for le in self.entries.iter().filter(|e| e.entry.matches(&self.search)) {
                    let vault = all_vaults.then(|| self.vaults.iter().find(|v| v.id == le.entry.vault_id)).flatten();
                    if sections.last().is_none_or(|(v, _)| v.map(|v| v.id) != vault.map(|v| v.id)) {
                        sections.push((vault, BTreeMap::new()));
                    }
                    let g = le.entry.group.split('/').map(str::trim).filter(|p| !p.is_empty()).collect::<Vec<_>>().join(" / ");
                    sections.last_mut().unwrap().1.entry(g).or_default().push(le);
                }
                let live: HashSet<Uuid> = self.sessions.iter().filter(|s| s.status().is_live()).map(|s| s.entry_id()).collect();
                let searching = !self.search.trim().is_empty();

                egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 0.0;
                    if sections.is_empty() {
                        ui.add_space(30.0);
                        ui.vertical_centered(|ui| {
                            ui.label(RichText::new(if searching { "No matches" } else { "No connections yet" }).color(theme::text_weak()));
                            if !searching && writable {
                                ui.label(RichText::new("Use + to add one, or import a CSV.").color(theme::text_weak()).size(11.5));
                            }
                        });
                    }
                    for (vault, groups) in sections {
                        let vault_id = vault.map_or(self.current_vault, |v| v.id);
                        if let Some(v) = vault {
                            let count = groups.values().map(Vec::len).sum();
                            let collapsed = !searching && self.collapsed_groups.contains(&(v.id, VAULT_HEADER.to_string()));
                            if vault_header(ui, v, count, collapsed).clicked() {
                                actions.push(Action::ToggleGroup(v.id, VAULT_HEADER.to_string()));
                            }
                            if collapsed {
                                continue;
                            }
                        }
                        let multiple_groups = groups.len() > 1 || groups.keys().any(|k| !k.is_empty());
                        for (group, entries) in groups {
                            // Only collapsible when its header is shown, and per vault.
                            let collapsed = multiple_groups && !searching && self.collapsed_groups.contains(&(vault_id, group.clone()));
                            if multiple_groups {
                                let label = if group.is_empty() { "UNGROUPED".to_string() } else { group.to_uppercase() };
                                if group_header(ui, &label, entries.len(), collapsed).clicked() {
                                    actions.push(Action::ToggleGroup(vault_id, group.clone()));
                                }
                            }
                            if collapsed {
                                continue;
                            }
                            for le in entries {
                                let picked = self.select_mode.then(|| self.picked.contains(&le.entry.id));
                                entry_row(ui, le, self.selected == Some(le.entry.id), live.contains(&le.entry.id), picked, actions);
                            }
                        }
                    }
                });
            });
    }

    /// Selection-mode bar: counts, All/None, target vault, Push.
    fn push_bar(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        let visible: Vec<Uuid> = self.entries.iter().filter(|e| e.entry.matches(&self.search)).map(|e| e.entry.id).collect();
        let targets: Vec<(Uuid, String)> = self
            .vaults
            .iter()
            .filter(|v| v.id != self.current_vault && v.writable())
            .map(|v| (v.id, if v.is_local() { format!("{} (private)", v.name) } else { v.name.clone() }))
            .collect();
        if self.push_target.is_some_and(|t| !targets.iter().any(|(id, _)| *id == t)) {
            self.push_target = None;
        }
        if self.push_target.is_none() {
            // Prefer the first server vault.
            self.push_target = targets.iter().map(|(id, _)| *id).find(|id| *id != LOCAL_VAULT).or(targets.first().map(|(id, _)| *id));
        }
        let from_local = self.entries.iter().filter(|e| self.picked.contains(&e.entry.id)).all(|e| e.entry.vault_id == LOCAL_VAULT);

        Frame::new()
            .fill(theme::row_selected())
            .stroke(Stroke::new(1.0_f32, theme::ACCENT))
            .corner_radius(10)
            .inner_margin(Margin::symmetric(10, 8))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.label(RichText::new(format!("{} selected", self.picked.len())).color(theme::text()).strong());
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.small_button("Done").clicked() {
                            actions.push(Action::SetSelectMode(false));
                        }
                        if ui.small_button("None").clicked() {
                            actions.push(Action::PickAll(Vec::new()));
                        }
                        if ui.small_button(format!("All ({})", visible.len())).on_hover_text("Select every entry shown (respects the search)").clicked() {
                            actions.push(Action::PickAll(visible.clone()));
                        }
                    });
                });
                if targets.is_empty() || (from_local && targets.iter().all(|(id, _)| *id == LOCAL_VAULT)) {
                    let hint = if self.signed_in() {
                        "No server vault you can write to yet. Ask an admin to grant your team access."
                    } else {
                        "Connect to a server (avatar menu) to push entries to team vaults."
                    };
                    ui.label(RichText::new(hint).color(theme::text_weak()).size(11.5));
                    return;
                }
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.label(RichText::new(if from_local { "Push to" } else { "Copy to" }).color(theme::text_weak()));
                    let current = self.push_target.and_then(|t| targets.iter().find(|(id, _)| *id == t)).map_or("choose...".to_string(), |(_, n)| n.clone());
                    egui::ComboBox::from_id_salt("push_target").selected_text(current).width(140.0).show_ui(ui, |ui| {
                        for (id, name) in &targets {
                            ui.selectable_value(&mut self.push_target, Some(*id), name);
                        }
                    });
                });
                ui.add_space(2.0);
                ui.horizontal(|ui| {
                    let n = self.picked.len();
                    let ready = n > 0 && self.push_target.is_some();
                    let copy = egui::Button::new(RichText::new(format!("Copy {n}")).color(Color32::WHITE).strong()).fill(theme::ACCENT);
                    if ui
                        .add_enabled(ready, copy)
                        .on_hover_text("Copy to the vault and keep the originals here")
                        .clicked()
                    {
                        if let Some(target) = self.push_target {
                            self.push_move = false;
                            actions.push(Action::BulkPush { target, remove_source: false });
                        }
                    }
                    // Move = copy, then delete the originals; needs a second click.
                    if self.push_move {
                        let confirm = egui::Button::new(RichText::new(format!("Confirm move {n}")).color(Color32::WHITE).strong()).fill(theme::DANGER);
                        if ui.add_enabled(ready, confirm).on_hover_text("The originals here will be deleted").clicked() {
                            if let Some(target) = self.push_target {
                                self.push_move = false;
                                actions.push(Action::BulkPush { target, remove_source: true });
                            }
                        }
                        if ui.small_button("Cancel").clicked() {
                            self.push_move = false;
                        }
                    } else if ui
                        .add_enabled(ready, egui::Button::new(format!("Move {n}")))
                        .on_hover_text("Copy to the vault, then remove the originals here")
                        .clicked()
                    {
                        self.push_move = true;
                    }
                });
            });
    }

    /// Avatar with a status dot; click opens the account menu.
    fn account_button(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        let name = if !self.settings.display_name.is_empty() {
            self.settings.display_name.clone()
        } else if !self.settings.username.is_empty() {
            self.settings.username.clone()
        } else {
            "Local user".to_string()
        };
        let (rect, resp) = ui.allocate_exact_size(egui::vec2(38.0, 38.0), Sense::click());
        avatar(ui.painter(), rect.center(), 19.0, theme::ACCENT, &initials(&name));
        let dot = match self.conn {
            Conn::Online => theme::OK,
            Conn::Offline => theme::WARN,
            Conn::SignedOut => theme::text_weak(),
        };
        let dot_at = rect.right_bottom() - egui::vec2(5.0, 5.0);
        ui.painter().circle_filled(dot_at, 6.0, theme::panel());
        ui.painter().circle_filled(dot_at, 4.0, dot);
        let resp = resp.on_hover_text("Account & settings");
        let popup_id = ui.make_persistent_id("account_menu");
        if resp.clicked() {
            ui.memory_mut(|m| m.toggle_popup(popup_id));
        }
        egui::popup::popup_below_widget(ui, popup_id, &resp, egui::PopupCloseBehavior::CloseOnClick, |ui| {
            ui.set_min_width(220.0);
            ui.label(RichText::new(&name).color(theme::text()).strong());
            let line = match self.conn {
                Conn::Online => short_url(&self.settings.server_url),
                Conn::Offline => format!("{} (offline)", short_url(&self.settings.server_url)),
                Conn::SignedOut => "Not connected to a server".to_string(),
            };
            ui.label(RichText::new(line).color(theme::text_weak()).size(11.5));
            if let Some(t) = self.last_sync_ok {
                ui.label(RichText::new(format!("Last sync {}", t.format("%H:%M"))).color(theme::text_weak()).size(11.0));
            }
            ui.separator();
            let item = |ui: &mut egui::Ui, text: &str| ui.add(egui::Button::new(text).frame(false)).clicked();
            if self.signed_in() {
                if item(ui, "Sync now") {
                    actions.push(Action::SyncNow);
                }
                if self.settings.is_admin && item(ui, "Administration") {
                    actions.push(Action::OpenAdmin);
                }
                if item(ui, "Change password") {
                    actions.push(Action::ShowPasswordChange);
                }
            } else if item(ui, "Connect to server...") {
                actions.push(Action::ShowLogin);
            }
            ui.separator();
            let dark = theme::is_dark();
            if item(ui, if dark { "Light mode" } else { "Dark mode" }) {
                actions.push(Action::SetDark(!dark));
            }
            let embed = !self.settings.external_by_default;
            if item(ui, if embed { "Open sessions externally" } else { "Open sessions in tabs" }) {
                actions.push(Action::SetEmbed(!embed));
            }
            ui.separator();
            let rec = self.settings.record_local;
            let mut on = rec;
            if ui
                .checkbox(&mut on, "Record all Local sessions")
                .on_hover_text("Record every SSH/RDP/VNC/web session of the Local vault, saved on this PC")
                .changed()
            {
                actions.push(Action::SetRecordLocal(on));
            }
            if item(ui, "Open recordings folder") {
                actions.push(Action::OpenRecordingsFolder);
            }
            if self.signed_in() {
                ui.separator();
                if ui.add(egui::Button::new(RichText::new("Sign out").color(theme::DANGER)).frame(false)).clicked() {
                    actions.push(Action::Logout);
                }
            }
        });
    }

    /// Vault title doubling as a dropdown.
    fn vault_picker(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        let current = self.current().map_or_else(|| "All vaults".to_string(), |v| v.name.clone());
        let resp = ui.add(
            egui::Button::new(RichText::new(format!("{current}  v")).color(theme::text()).strong().size(15.5))
                .frame(false),
        );
        let popup_id = ui.make_persistent_id("vault_menu");
        if resp.clicked() {
            ui.memory_mut(|m| m.toggle_popup(popup_id));
        }
        egui::popup::popup_below_widget(ui, popup_id, &resp, egui::PopupCloseBehavior::CloseOnClick, |ui| {
            ui.set_min_width(240.0);
            ui.label(RichText::new("VAULTS").color(theme::text_weak()).size(10.5).strong());
            // Everything at once: Local and every server vault in one list.
            {
                let total: usize = self.vault_counts.values().map(|(n, _)| n).sum();
                let (rect, r) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 30.0), Sense::click());
                let painter = ui.painter();
                if self.current_vault == ALL_VAULTS {
                    painter.rect_filled(rect, 6.0, theme::row_selected());
                } else if r.hovered() {
                    painter.rect_filled(rect, 6.0, theme::row_hover());
                }
                painter.circle_filled(rect.left_center() + egui::vec2(12.0, 0.0), 4.0, theme::text_weak());
                painter.text(rect.left_center() + egui::vec2(24.0, 0.0), egui::Align2::LEFT_CENTER, "All vaults", FontId::proportional(13.0), theme::text());
                pill(painter, rect.right_center() - egui::vec2(8.0, 0.0), &total.to_string(), theme::ACCENT, Color32::WHITE);
                if r.on_hover_text("Local and server vaults together in one list").clicked() {
                    actions.push(Action::SelectVault(ALL_VAULTS));
                }
                ui.separator();
            }
            for v in &self.vaults {
                let (count, attention) = self.vault_counts.get(&v.id).copied().unwrap_or((0, 0));
                let (rect, r) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 30.0), Sense::click());
                let painter = ui.painter();
                if v.id == self.current_vault {
                    painter.rect_filled(rect, 6.0, theme::row_selected());
                } else if r.hovered() {
                    painter.rect_filled(rect, 6.0, theme::row_hover());
                }
                let dot = if v.is_local() {
                    Color32::from_rgb(110, 160, 240)
                } else if v.writable() {
                    theme::ACCENT
                } else {
                    Color32::from_rgb(180, 130, 230)
                };
                painter.circle_filled(rect.left_center() + egui::vec2(12.0, 0.0), 4.0, dot);
                painter.text(rect.left_center() + egui::vec2(24.0, 0.0), egui::Align2::LEFT_CENTER, &v.name, FontId::proportional(13.0), theme::text());
                let (n, fill) = if attention > 0 { (attention, theme::DANGER) } else { (count, theme::ACCENT) };
                pill(painter, rect.right_center() - egui::vec2(8.0, 0.0), &n.to_string(), fill, Color32::WHITE);
                let r = if v.is_local() { r.on_hover_text("Private - stored only on this PC") } else { r.on_hover_text(format!("Your access: {}", v.role.label())) };
                if r.clicked() {
                    actions.push(Action::SelectVault(v.id));
                }
            }
            if self.vaults.len() <= 1 {
                ui.label(RichText::new("Connect to a server to see team vaults.").color(theme::text_weak()).size(11.5));
            }
        });
    }

    // ------------------------------------------------------------------
    // Status line (bottom of the content column)
    // ------------------------------------------------------------------

    pub(super) fn status_line(&mut self, ctx: &egui::Context) {
        if self.status.is_empty() {
            return;
        }
        egui::TopBottomPanel::bottom("status")
            .frame(Frame::new().fill(theme::panel()).stroke(Stroke::new(1.0_f32, theme::border())).inner_margin(Margin::symmetric(16, 6)))
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    let color = if self.status_is_error { theme::DANGER } else { theme::text_weak() };
                    ui.label(RichText::new(&self.status).color(color).size(12.0));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.small_button("Dismiss").clicked() {
                            self.status.clear();
                        }
                    });
                });
            });
    }

    // ------------------------------------------------------------------
    // Content: header, session pills, details or live session
    // ------------------------------------------------------------------

    pub(super) fn content_panel(&mut self, ctx: &egui::Context, actions: &mut Vec<Action>) {
        if let Some(id) = self.active_tab {
            if !self.sessions.iter().any(|s| s.id() == id) {
                self.active_tab = None;
            }
        }
        // Native overlay sessions (embedded browser): only the active tab is shown.
        let active = self.active_tab;
        for s in &mut self.sessions {
            s.set_visible(active == Some(s.id()));
        }
        egui::CentralPanel::default().frame(Frame::new().fill(theme::panel())).show(ctx, |ui| {
            // The large entry header is only useful on the Details tab; a live
            // session has its own tab strip and toolbar, so hide it there to
            // give the session (browser, RDP, SSH) the full height.
            if !self.focus_mode && self.active_tab.is_none() {
                self.content_header(ui, actions);
            }

            if !self.sessions.is_empty() && !self.focus_mode {
                Frame::new().inner_margin(Margin { left: 16, right: 16, top: 5, bottom: 5 }).fill(theme::panel()).show(ui, |ui| {
                    ui.horizontal_wrapped(|ui| {
                        if tab_pill(ui, None, "Details", self.active_tab.is_none(), false).0.clicked() {
                            actions.push(Action::ActivateTab(None));
                        }
                        for s in &self.sessions {
                            let (resp, close) = tab_pill(ui, Some(status_color(&s.status())), &format!("{}  {}", s.badge(), s.title()), self.active_tab == Some(s.id()), true);
                            if close {
                                actions.push(Action::CloseSession(s.id()));
                            } else if resp.clicked() {
                                actions.push(Action::ActivateTab(Some(s.id())));
                            }
                        }
                    });
                });
                let y = ui.cursor().min.y;
                ui.painter().hline(ui.max_rect().x_range(), y, Stroke::new(1.0_f32, theme::border()));
            }

            let ppp = ui.ctx().pixels_per_point();
            let size = ui.available_size() * ppp;
            self.session_px = (size.x as u16, size.y as u16);

            match self.active_tab.and_then(|id| self.sessions.iter_mut().find(|s| s.id() == id)) {
                Some(session) => {
                    let status = session.status();
                    if let Status::Failed(_) | Status::Closed(_) = status {
                        Frame::new().fill(theme::danger_bg()).inner_margin(Margin::symmetric(16, 8)).show(ui, |ui| {
                            ui.horizontal(|ui| {
                                ui.label(RichText::new(status_text(&status)).color(theme::DANGER));
                                if ui.button("Reconnect").clicked() {
                                    actions.push(Action::Reconnect(session.id()));
                                }
                            });
                        });
                    }
                    // Session toolbar: display controls + full screen.
                    let focus_mode = self.focus_mode;
                    Frame::new().fill(theme::subtle()).inner_margin(Margin::symmetric(14, 5)).show(ui, |ui| {
                        ui.horizontal(|ui| {
                            if focus_mode {
                                ui.label(RichText::new(format!("{}  {}", session.badge(), session.title())).color(theme::text()).strong());
                                ui.separator();
                            }
                            session.toolbar(ui);
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                let label = if focus_mode { "Exit full screen" } else { "Full screen" };
                                if ui.button(label).on_hover_text("Hide the lists and fill the screen with this session").clicked() {
                                    actions.push(Action::ToggleFullScreen);
                                }
                            });
                        });
                    });
                    session.ui(ui);
                }
                None => {
                    Frame::new().fill(theme::app_bg()).show(ui, |ui| {
                        ui.set_min_size(ui.available_size());
                        // Explicit column width: the scroll area otherwise remembers wider content.
                        let column = (ui.available_width() - 56.0 - 14.0).clamp(280.0, 780.0);
                        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                            Frame::new().inner_margin(Margin::symmetric(28, 22)).show(ui, |ui| {
                                ui.set_width(column);
                                self.details_ui(ui, actions);
                            });
                        });
                    });
                }
            }
        });
    }

    fn content_header(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        let session = self.active_tab.and_then(|id| self.sessions.iter().find(|s| s.id() == id));
        let entry = match session {
            Some(s) => self.entries.iter().find(|e| e.entry.id == s.entry_id()),
            None => self.selected.and_then(|id| self.entries.iter().find(|e| e.entry.id == id)),
        };
        let live = entry.is_some_and(|le| self.sessions.iter().any(|s| s.entry_id() == le.entry.id && s.status() == Status::Connected));

        Frame::new().inner_margin(Margin { left: 22, right: 18, top: 14, bottom: 14 }).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.set_min_height(44.0);
                match entry {
                    Some(le) => {
                        let e = &le.entry;
                        let (r, _) = ui.allocate_exact_size(egui::vec2(42.0, 42.0), Sense::hover());
                        avatar(ui.painter(), r.center(), 21.0, kind_color(e.kind), e.kind.badge());
                        ui.add_space(4.0);
                        ui.vertical(|ui| {
                            ui.add_space(2.0);
                            ui.label(RichText::new(&e.name).color(theme::text()).strong().size(16.0));
                            ui.horizontal(|ui| {
                                let addr = match (e.host.is_empty(), e.port) {
                                    (true, _) => e.kind.label().to_string(),
                                    (false, Some(p)) => format!("{}  ·  {}:{}", e.kind.label(), e.host, p),
                                    (false, None) => format!("{}  ·  {}", e.kind.label(), e.host),
                                };
                                ui.label(RichText::new(addr).color(theme::text_weak()).size(12.0));
                                if let Some(s) = session {
                                    let st = s.status();
                                    ui.label(RichText::new(format!("•  {}", short_status(&st))).color(status_color(&st)).size(12.0));
                                    if let Some(note) = s.note() {
                                        ui.label(RichText::new(note).color(theme::text_weak()).size(11.0)).on_hover_text("SSH host key fingerprint saved on first connect");
                                    }
                                } else if live {
                                    ui.label(RichText::new("•  Connected").color(theme::OK).size(12.0));
                                }
                            });
                        });
                        let id = e.id;
                        let kind = e.kind;
                        let writable = self.entry_writable(id);
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            // "More" menu
                            let more = round_button(ui, Icon::More, false, 40.0, "More actions");
                            let popup_id = ui.make_persistent_id(("more", id));
                            if more.clicked() {
                                ui.memory_mut(|m| m.toggle_popup(popup_id));
                            }
                            egui::popup::popup_below_widget(ui, popup_id, &more, egui::PopupCloseBehavior::CloseOnClick, |ui| {
                                ui.set_min_width(180.0);
                                if ui.add_enabled(writable, egui::Button::new("Edit entry").frame(false)).clicked() {
                                    actions.push(Action::Edit);
                                }
                                if kind.can_embed() && ui.add(egui::Button::new("Open in a tab").frame(false)).clicked() {
                                    actions.push(Action::LaunchEmbedded(id));
                                }
                                if kind.can_embed() && ui.add(egui::Button::new("Open in external program").frame(false)).clicked() {
                                    actions.push(Action::LaunchExternal(id));
                                }
                                if ui.add(egui::Button::new("Copy username").frame(false)).clicked() {
                                    actions.push(Action::CopyUser(id));
                                }
                                if ui.add(egui::Button::new("Copy password").frame(false)).clicked() {
                                    actions.push(Action::CopyPassword(id));
                                }
                                ui.separator();
                                if ui.add_enabled(writable, egui::Button::new(RichText::new("Delete...").color(theme::DANGER)).frame(false)).clicked() {
                                    actions.push(Action::AskDelete(id));
                                }
                            });
                            ui.add_space(4.0);
                            if writable && round_button(ui, Icon::Pencil, false, 40.0, "Edit").clicked() {
                                actions.push(Action::Edit);
                            }
                            if kind == EntryKind::Ssh {
                                ui.add_space(4.0);
                                if round_button(ui, Icon::Folder, false, 40.0, "Browse files (SFTP)").clicked() {
                                    actions.push(Action::OpenFiles(id));
                                }
                            }
                            if !matches!(kind, EntryKind::Credential | EntryKind::Note) {
                                ui.add_space(4.0);
                                if round_button(ui, Icon::Play, true, 40.0, "Connect").clicked() {
                                    actions.push(Action::Launch(id));
                                }
                            }
                        });
                    }
                    None => {
                        ui.vertical(|ui| {
                            ui.label(RichText::new("SystemDeck").color(theme::text()).strong().size(16.0));
                            ui.label(RichText::new("Select a connection from the list").color(theme::text_weak()).size(12.0));
                        });
                    }
                }
            });
        });
        let y = ui.cursor().min.y;
        ui.painter().hline(ui.max_rect().x_range(), y, Stroke::new(1.0_f32, theme::border()));
    }

    // ------------------------------------------------------------------
    // Details (cards)
    // ------------------------------------------------------------------

    fn details_ui(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        let writable = self.selected.is_some_and(|id| self.entry_writable(id));
        if let Some(draft) = &mut self.draft {
            card(ui, |ui| editor_ui(ui, draft, actions));
            return;
        }
        let Some(id) = self.selected else {
            ui.add_space(30.0);
            ui.label(RichText::new("Welcome to SystemDeck").color(theme::text()).strong().size(24.0));
            ui.add_space(4.0);
            ui.label(RichText::new("Pick a connection on the left. Double-click opens it - RDP, VNC and SSH open right here in a tab.").color(theme::text_weak()));
            ui.add_space(18.0);
            ui.horizontal(|ui| {
                stat_card(ui, &self.entries.len().to_string(), if self.current_vault == ALL_VAULTS { "connections" } else { "connections in this vault" });
                stat_card(ui, &self.vaults.len().to_string(), "vaults");
                stat_card(ui, &self.sessions.len().to_string(), "open sessions");
            });
            return;
        };
        let Some(le) = self.entries.iter().find(|e| e.entry.id == id) else { return };
        let e = &le.entry;

        if let Some(server) = &le.conflict {
            Frame::new().fill(theme::danger_bg()).stroke(Stroke::new(1.0_f32, theme::DANGER)).corner_radius(10).inner_margin(14).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.label(RichText::new("Sync conflict").color(theme::DANGER).strong());
                ui.label(format!(
                    "{} changed this entry on the server at {} while you had unsynced edits{}.",
                    server.updated_by,
                    server.updated_at.with_timezone(&Local).format("%Y-%m-%d %H:%M"),
                    if server.deleted { " (they deleted it)" } else { "" }
                ));
                ui.horizontal(|ui| {
                    if ui.button("Keep mine").clicked() {
                        actions.push(Action::KeepMine(id));
                    }
                    if ui.button("Use server version").clicked() {
                        actions.push(Action::UseServer(id));
                    }
                });
            });
            ui.add_space(12.0);
        } else if le.dirty {
            ui.label(RichText::new("•  Changes not yet pushed to the server").color(theme::WARN));
            ui.add_space(8.0);
        }

        card(ui, |ui| {
            section_title(ui, "Connection");
            egui::Grid::new("conn").num_columns(2).spacing([24.0, 10.0]).min_col_width(90.0).show(ui, |ui| {
                field(ui, "Type", e.kind.label());
                field(ui, "Host", &e.host);
                field(ui, "Port", &e.port.map(|p| p.to_string()).unwrap_or_else(|| e.kind.default_port().map(|p| format!("{p} (default)")).unwrap_or_default()));
                field(ui, "URL", &e.url);
                field(ui, "Folder", &e.group);
                if e.kind.can_embed() || e.kind == EntryKind::Web {
                    field(ui, "Opens", e.display.label());
                }
                if e.kind.can_embed() || e.kind == EntryKind::Web {
                    let local = e.vault_id == LOCAL_VAULT;
                    let lock = self.recording_lock(e);
                    let on = lock.is_some() || e.record_session;
                    let place = if local { "saved on this PC" } else { "saved to the server" };
                    ui.label(RichText::new("Recording").color(theme::text_weak()));
                    ui.horizontal(|ui| {
                        if let Some(why) = lock {
                            // Locked on: vault policy, the Local setting, or switched on at the server.
                            ui.label(RichText::new(format!("On, {place}")).color(theme::DANGER).monospace());
                            ui.label(RichText::new(format!("- {why}")).color(theme::text_weak()).size(11.5));
                        } else {
                            let label = if on { format!("On, {place}") } else { "Off".to_string() };
                            ui.label(RichText::new(label).color(if on { theme::DANGER } else { theme::text() }).monospace());
                            let toggle = egui::Button::new(if on { "Turn off" } else { "Turn on" }).small();
                            if ui
                                .add_enabled(writable, toggle)
                                .on_hover_text("Record this entry's sessions (SSH: keystrokes + output; RDP/VNC: screen, typed text, clicks)")
                                .clicked()
                            {
                                actions.push(Action::SetEntryRecording(e.id, !on));
                            }
                        }
                    });
                    ui.end_row();
                }
                if matches!(e.kind, EntryKind::Rdp | EntryKind::Vnc) {
                    let d = e.screen;
                    let summary = if e.kind == EntryKind::Rdp {
                        format!("{}  ·  {}-bit  ·  {}  ·  {}", d.resolution.label(), d.color_depth.bits(), d.quality.label(), d.scaling.label())
                    } else {
                        d.scaling.label().to_string()
                    };
                    field(ui, "Display", &summary);
                }
                field(ui, "Tags", &e.tags.join(", "));
            });
        });
        ui.add_space(12.0);

        if e.kind.has_credentials() {
            card(ui, |ui| {
                section_title(ui, "Credentials");
                egui::Grid::new("creds").num_columns(2).spacing([24.0, 10.0]).min_col_width(90.0).show(ui, |ui| {
                    field(ui, "Domain", &e.domain);
                    ui.label(RichText::new("Username").color(theme::text_weak()));
                    ui.horizontal(|ui| {
                        if e.username.is_empty() {
                            ui.label(RichText::new("not set").color(theme::text_weak()).italics());
                        } else {
                            ui.label(RichText::new(&e.username).color(theme::text()).monospace());
                            if ui.small_button("Copy").clicked() {
                                actions.push(Action::CopyUser(id));
                            }
                        }
                    });
                    ui.end_row();
                    ui.label(RichText::new("Password").color(theme::text_weak()));
                    ui.horizontal(|ui| {
                        if e.password.is_empty() {
                            ui.label(RichText::new("not set").color(theme::text_weak()).italics());
                        } else {
                            let shown = if self.reveal_password { e.password.clone() } else { "••••••••••".into() };
                            ui.label(RichText::new(shown).color(theme::text()).monospace());
                            if ui.small_button(if self.reveal_password { "Hide" } else { "Show" }).clicked() {
                                self.reveal_password = !self.reveal_password;
                            }
                            if ui.small_button("Copy").clicked() {
                                actions.push(Action::CopyPassword(id));
                            }
                        }
                    });
                    ui.end_row();
                });
                if matches!(e.kind, EntryKind::Rdp | EntryKind::Ssh) && (e.username.is_empty() || e.password.is_empty()) {
                    ui.add_space(6.0);
                    ui.label(RichText::new("Add a username and password (Edit) to sign in automatically.").color(theme::WARN).size(12.0));
                }
            });
            ui.add_space(12.0);
        }

        if !e.notes.is_empty() {
            card(ui, |ui| {
                section_title(ui, "Notes");
                ui.label(RichText::new(&e.notes).color(theme::text()));
            });
            ui.add_space(12.0);
        }

        let targets: Vec<&CachedVault> = self.vaults.iter().filter(|v| v.id != e.vault_id && v.writable()).collect();
        if !targets.is_empty() || writable {
            card(ui, |ui| {
                if !targets.is_empty() {
                    section_title(ui, if e.vault_id == LOCAL_VAULT { "Share with a team" } else { "Copy to another vault" });
                    ui.horizontal(|ui| {
                        let selected_name = self
                            .copy_target
                            .and_then(|t| targets.iter().find(|v| v.id == t))
                            .map_or("Choose vault...".to_string(), |v| v.name.clone());
                        egui::ComboBox::from_id_salt("copy_target").selected_text(selected_name).show_ui(ui, |ui| {
                            for v in &targets {
                                ui.selectable_value(&mut self.copy_target, Some(v.id), &v.name);
                            }
                        });
                        if let Some(target) = self.copy_target {
                            if ui.button("Copy").clicked() {
                                actions.push(Action::CopyTo { id, target, remove_source: false });
                            }
                            if writable && ui.button("Move").clicked() {
                                actions.push(Action::CopyTo { id, target, remove_source: true });
                            }
                        }
                    });
                    ui.add_space(10.0);
                }
                if writable {
                    if self.confirm_delete == Some(id) {
                        ui.horizontal(|ui| {
                            ui.label(RichText::new("Delete this entry?").color(theme::DANGER));
                            if ui.button(RichText::new("Yes, delete").color(theme::DANGER)).clicked() {
                                actions.push(Action::Delete(id));
                            }
                            if ui.button("No").clicked() {
                                self.confirm_delete = None;
                            }
                        });
                    } else if ui.button(RichText::new("Delete entry").color(theme::DANGER)).clicked() {
                        self.confirm_delete = Some(id);
                    }
                }
            });
            ui.add_space(12.0);
        }

        ui.label(
            RichText::new(format!(
                "Last changed by {} on {}{}",
                if e.updated_by.is_empty() { "-" } else { &e.updated_by },
                e.updated_at.with_timezone(&Local).format("%Y-%m-%d %H:%M"),
                if e.revision > 0 { format!("  ·  revision {}", e.revision) } else { String::new() }
            ))
            .color(theme::text_weak())
            .size(11.0),
        );
    }
}

// ----------------------------------------------------------------------
// Widgets
// ----------------------------------------------------------------------

fn short_url(url: &str) -> String {
    url.trim_start_matches("https://").trim_start_matches("http://").to_string()
}

fn short_status(status: &Status) -> String {
    match status {
        Status::Connected => "Connected".into(),
        Status::Connecting => "Connecting...".into(),
        Status::Closed(_) => "Closed".into(),
        Status::Failed(_) => "Failed".into(),
    }
}

fn search_box(ui: &mut egui::Ui, text: &mut String) {
    Frame::new()
        .fill(theme::panel())
        .stroke(Stroke::new(1.0_f32, theme::border()))
        .corner_radius(16)
        .inner_margin(Margin { left: 10, right: 10, top: 4, bottom: 4 })
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                let (r, _) = ui.allocate_exact_size(egui::vec2(14.0, 14.0), Sense::hover());
                theme::paint_icon(ui.painter(), r, Icon::Search, theme::text_weak());
                ui.add(egui::TextEdit::singleline(text).hint_text("Search").frame(false).desired_width(f32::INFINITY));
            });
        });
}

/// Heading of one vault in the all-vaults list (click to fold).
fn vault_header(ui: &mut egui::Ui, v: &CachedVault, count: usize, collapsed: bool) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 34.0), Sense::click());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0.0, if resp.hovered() { theme::row_hover() } else { theme::panel() });
    painter.hline(rect.x_range(), rect.bottom() - 0.5, Stroke::new(1.0_f32, theme::border()));
    let color = if v.is_local() {
        Color32::from_rgb(110, 160, 240)
    } else if v.writable() {
        theme::ACCENT
    } else {
        Color32::from_rgb(180, 130, 230)
    };
    let arrow = if collapsed { ">" } else { "v" };
    painter.text(rect.left_center() + egui::vec2(12.0, 0.0), egui::Align2::LEFT_CENTER, arrow, FontId::monospace(11.0), theme::text_weak());
    painter.circle_filled(rect.left_center() + egui::vec2(28.0, 0.0), 4.5, color);
    painter.text(rect.left_center() + egui::vec2(40.0, 0.0), egui::Align2::LEFT_CENTER, &v.name, FontId::proportional(13.5), theme::text());
    let kind = if v.is_local() { "this PC" } else { v.role.label() };
    painter.text(rect.right_center() - egui::vec2(40.0, 0.0), egui::Align2::RIGHT_CENTER, kind, FontId::proportional(11.0), theme::text_weak());
    pill(&painter, rect.right_center() - egui::vec2(10.0, 0.0), &count.to_string(), color, Color32::WHITE);
    if v.is_local() { resp.on_hover_text("Private - stored only on this PC") } else { resp.on_hover_text("Shared server vault") }
}

fn group_header(ui: &mut egui::Ui, label: &str, count: usize, collapsed: bool) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 30.0), Sense::click());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0.0, if resp.hovered() { theme::row_hover() } else { theme::subtle() });
    let arrow = if collapsed { ">" } else { "v" };
    painter.text(rect.left_center() + egui::vec2(18.0, 0.0), egui::Align2::LEFT_CENTER, arrow, FontId::monospace(10.0), theme::text_weak());
    painter.text(rect.left_center() + egui::vec2(32.0, 0.0), egui::Align2::LEFT_CENTER, label, FontId::proportional(10.5), theme::text_weak());
    painter.text(rect.right_center() - egui::vec2(16.0, 0.0), egui::Align2::RIGHT_CENTER, count.to_string(), FontId::proportional(10.5), theme::text_weak());
    painter.hline(rect.x_range(), rect.max.y - 0.5, Stroke::new(1.0_f32, theme::border()));
    resp
}

/// `picked`: Some(..) in selection mode (row shows a check circle instead of the avatar).
fn entry_row(ui: &mut egui::Ui, le: &LocalEntry, selected: bool, live: bool, picked: Option<bool>, actions: &mut Vec<Action>) {
    let e = &le.entry;
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 60.0), Sense::click());
    let painter = ui.painter_at(rect);
    if picked == Some(true) || (picked.is_none() && selected) {
        painter.rect_filled(rect, 0.0, theme::row_selected());
        painter.rect_filled(egui::Rect::from_min_size(rect.min, egui::vec2(3.0, rect.height())), 0.0, theme::ACCENT);
    } else if resp.hovered() {
        painter.rect_filled(rect, 0.0, theme::row_hover());
    }
    let cy = rect.center().y;
    let avatar_center = egui::pos2(rect.min.x + 36.0, cy);
    match picked {
        Some(true) => {
            painter.circle_filled(avatar_center, 18.0, theme::ACCENT);
            theme::paint_icon(&painter, egui::Rect::from_center_size(avatar_center, egui::vec2(18.0, 18.0)), Icon::Check, Color32::WHITE);
        }
        Some(false) => {
            avatar(&painter, avatar_center, 18.0, kind_color(e.kind).gamma_multiply(0.35), e.kind.badge());
            painter.circle_stroke(avatar_center, 18.0, Stroke::new(1.5_f32, theme::text_weak()));
        }
        None => avatar(&painter, avatar_center, 18.0, kind_color(e.kind), e.kind.badge()),
    }

    // Right column: time + status pill
    let right = rect.max.x - 16.0;
    let local = e.updated_at.with_timezone(&Local);
    let when = if local.date_naive() == Local::now().date_naive() { local.format("%H:%M").to_string() } else { local.format("%d %b").to_string() };
    painter.text(egui::pos2(right, cy - 10.0), egui::Align2::RIGHT_CENTER, when, FontId::proportional(10.5), theme::text_weak());
    if le.conflict.is_some() {
        pill(&painter, egui::pos2(right, cy + 10.0), "conflict", theme::DANGER, Color32::WHITE);
    } else if live {
        pill(&painter, egui::pos2(right, cy + 10.0), "live", theme::ACCENT, Color32::WHITE);
    } else if le.dirty {
        painter.circle_filled(egui::pos2(right - 4.0, cy + 10.0), 4.0, theme::WARN);
    }

    // Name + subtitle, clipped before the right column
    let text_left = rect.min.x + 64.0;
    let clip = egui::Rect::from_min_max(egui::pos2(text_left, rect.min.y), egui::pos2(right - 52.0, rect.max.y));
    let p = painter.with_clip_rect(clip);
    p.text(egui::pos2(text_left, cy - 9.0), egui::Align2::LEFT_CENTER, &e.name, FontId::proportional(13.5), theme::text());
    let mut sub = e.host.clone();
    if let Some(port) = e.port {
        if !sub.is_empty() && Some(port) != e.kind.default_port() {
            sub += &format!(":{port}");
        }
    }
    if !e.username.is_empty() {
        sub = if sub.is_empty() { e.username.clone() } else { format!("{} · {sub}", e.username) };
    }
    if sub.is_empty() {
        sub = e.kind.label().to_string();
    }
    p.text(egui::pos2(text_left, cy + 10.0), egui::Align2::LEFT_CENTER, sub, FontId::proportional(12.0), theme::text_weak());
    painter.hline(egui::Rangef::new(rect.min.x + 64.0, rect.max.x), rect.max.y - 0.5, Stroke::new(1.0_f32, theme::border()));

    let ctrl = ui.input(|i| i.modifiers.command);
    if resp.clicked() {
        if picked.is_some() || ctrl {
            actions.push(Action::TogglePick(e.id));
        } else {
            actions.push(Action::Select(e.id));
        }
    }
    if resp.double_clicked() && picked.is_none() {
        actions.push(Action::Launch(e.id));
    }
}

/// Session/Details pill in the tab strip. Returns (response, close clicked).
fn tab_pill(ui: &mut egui::Ui, dot: Option<Color32>, text: &str, active: bool, closable: bool) -> (egui::Response, bool) {
    let font = FontId::proportional(12.5);
    let text_w = ui.fonts(|f| f.layout_no_wrap(text.to_string(), font.clone(), theme::text()).size().x);
    let width = 24.0 + text_w + if dot.is_some() { 14.0 } else { 0.0 } + if closable { 20.0 } else { 0.0 };
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(width, 28.0), Sense::click());
    let painter = ui.painter();
    let (fill, fg, stroke) = if active {
        (theme::ACCENT, Color32::WHITE, Stroke::NONE)
    } else if resp.hovered() {
        (theme::app_bg(), theme::text(), Stroke::new(1.0_f32, theme::border()))
    } else {
        (theme::panel(), theme::text(), Stroke::new(1.0_f32, theme::border()))
    };
    painter.rect(rect, 14.0, fill, stroke, egui::StrokeKind::Inside);
    let mut x = rect.min.x + 12.0;
    if let Some(c) = dot {
        painter.circle_filled(egui::pos2(x + 3.0, rect.center().y), 3.5, if active { Color32::WHITE } else { c });
        x += 14.0;
    }
    painter.text(egui::pos2(x, rect.center().y), egui::Align2::LEFT_CENTER, text, font, fg);
    let mut close = false;
    if closable {
        let close_rect = egui::Rect::from_center_size(egui::pos2(rect.max.x - 15.0, rect.center().y), egui::vec2(14.0, 14.0));
        let hovered = ui.rect_contains_pointer(close_rect);
        theme::paint_icon(painter, close_rect.shrink(2.0), Icon::Close, if hovered { theme::DANGER } else { fg });
        close = resp.clicked() && hovered;
    }
    (resp, close)
}

fn card<R>(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    Frame::new()
        .fill(theme::panel())
        .stroke(Stroke::new(1.0_f32, theme::border()))
        .corner_radius(10)
        .inner_margin(Margin::symmetric(18, 16))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui)
        })
        .inner
}

fn section_title(ui: &mut egui::Ui, text: &str) {
    ui.label(RichText::new(text.to_uppercase()).color(theme::text_weak()).size(10.5).strong());
    ui.add_space(6.0);
}

fn field(ui: &mut egui::Ui, label: &str, value: &str) {
    if value.is_empty() {
        return;
    }
    ui.label(RichText::new(label).color(theme::text_weak()));
    ui.label(RichText::new(value).color(theme::text()).monospace());
    ui.end_row();
}

fn stat_card(ui: &mut egui::Ui, value: &str, label: &str) {
    Frame::new()
        .fill(theme::panel())
        .stroke(Stroke::new(1.0_f32, theme::border()))
        .corner_radius(10)
        .inner_margin(Margin::symmetric(18, 14))
        .show(ui, |ui| {
            ui.set_min_width(150.0);
            ui.label(RichText::new(value).color(theme::ACCENT_DARK).strong().size(26.0));
            ui.label(RichText::new(label).color(theme::text_weak()).size(12.0));
        });
}
