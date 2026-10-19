//! Administration window: users, teams, and who can access which vault.

use std::collections::HashMap;

use eframe::egui::{self, Color32, RichText};
use uuid::Uuid;
use systemdeck_core::{
    api::{CreateUserRequest, TeamRequest, UpdateUserRequest, VaultRequest},
    Grant, Principal, Role, Team, UserInfo, Vault,
};

use crate::api::{Api, ApiResult};

#[derive(PartialEq, Clone, Copy)]
enum Tab {
    Vaults,
    Teams,
    Users,
}

/// "Add access" row of one vault.
struct GrantDraft {
    to_team: bool,
    principal: String,
    role: Role,
}

impl Default for GrantDraft {
    fn default() -> Self {
        Self { to_team: true, principal: String::new(), role: Role::Read }
    }
}

enum Op {
    UpdateUser(String, UpdateUserRequest),
    CreateUser,
    SaveTeam(Uuid),
    DeleteTeam(Uuid),
    CreateTeam,
    SaveVault(Uuid),
    DeleteVault(Uuid),
    CreateVault,
}

pub struct AdminPanel {
    tab: Tab,
    users: Vec<UserInfo>,
    teams: Vec<Team>,
    vaults: Vec<Vault>,
    error: Option<String>,
    info: Option<String>,
    changed: bool,

    new_user: CreateUserRequest,
    reset_password: HashMap<String, String>,

    new_team: String,
    new_team_members: String,
    team_members: HashMap<Uuid, String>,

    new_vault: String,
    new_vault_desc: String,
    grant_drafts: HashMap<Uuid, GrantDraft>,
    confirm_delete: Option<Uuid>,
}

impl AdminPanel {
    pub fn new(api: &Api) -> Self {
        let mut panel = Self {
            tab: Tab::Vaults,
            users: Vec::new(),
            teams: Vec::new(),
            vaults: Vec::new(),
            error: None,
            info: None,
            changed: false,
            new_user: CreateUserRequest::default(),
            reset_password: HashMap::new(),
            new_team: String::new(),
            new_team_members: String::new(),
            team_members: HashMap::new(),
            new_vault: String::new(),
            new_vault_desc: String::new(),
            grant_drafts: HashMap::new(),
            confirm_delete: None,
        };
        panel.refresh(api);
        panel
    }

    fn refresh(&mut self, api: &Api) {
        let loaded = (|| -> ApiResult<_> { Ok((api.admin_users()?, api.admin_teams()?, api.admin_vaults()?)) })();
        match loaded {
            Ok((users, teams, vaults)) => {
                self.team_members = teams.iter().map(|t| (t.id, t.members.join(", "))).collect();
                self.users = users;
                self.teams = teams;
                self.vaults = vaults;
            }
            Err(e) => self.error = Some(e.to_string()),
        }
    }

    fn run<T>(&mut self, result: ApiResult<T>, ok: &str) -> Option<T> {
        match result {
            Ok(v) => {
                self.error = None;
                self.info = Some(ok.to_string());
                Some(v)
            }
            Err(e) => {
                self.error = Some(e.to_string());
                self.info = None;
                None
            }
        }
    }

    fn team_name(&self, id: Uuid) -> String {
        self.teams.iter().find(|t| t.id == id).map_or_else(|| "(deleted team)".into(), |t| t.name.clone())
    }

    /// Returns (window still open, vault access changed → resync).
    pub fn show(&mut self, ctx: &egui::Context, api: &Api) -> (bool, bool) {
        let mut open = true;
        let mut ops = Vec::new();
        egui::Window::new("⚙ Administration")
            .open(&mut open)
            .default_size([720.0, 520.0])
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.selectable_value(&mut self.tab, Tab::Vaults, "🗄 Vaults & access");
                    ui.selectable_value(&mut self.tab, Tab::Teams, "👥 Teams");
                    ui.selectable_value(&mut self.tab, Tab::Users, "👤 Users");
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("⟳ Refresh").clicked() {
                            self.refresh(api);
                        }
                    });
                });
                if let Some(err) = &self.error {
                    ui.colored_label(Color32::from_rgb(220, 80, 80), err);
                } else if let Some(info) = &self.info {
                    ui.colored_label(Color32::from_rgb(80, 180, 100), info);
                }
                ui.separator();
                egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| match self.tab {
                    Tab::Vaults => self.vaults_tab(ui, &mut ops),
                    Tab::Teams => self.teams_tab(ui, &mut ops),
                    Tab::Users => self.users_tab(ui, &mut ops),
                });
            });

        for op in ops {
            self.apply(api, op);
        }
        (open, std::mem::take(&mut self.changed))
    }

    fn apply(&mut self, api: &Api, op: Op) {
        let done = match op {
            Op::UpdateUser(name, req) => {
                let r = api.admin_update_user(&name, &req);
                let ok = self.run(r, &format!("Updated {name}")).is_some();
                if ok {
                    self.reset_password.remove(&name);
                }
                ok
            }
            Op::CreateUser => {
                let r = api.admin_create_user(&self.new_user);
                let ok = self.run(r, "User created").is_some();
                if ok {
                    self.new_user = CreateUserRequest::default();
                }
                ok
            }
            Op::CreateTeam => {
                let req = TeamRequest { name: self.new_team.clone(), members: split_members(&self.new_team_members) };
                let ok = self.run(api.admin_create_team(&req), "Team created").is_some();
                if ok {
                    self.new_team.clear();
                    self.new_team_members.clear();
                    self.changed = true;
                }
                ok
            }
            Op::SaveTeam(id) => {
                let name = self.team_name(id);
                let members = split_members(self.team_members.get(&id).map_or("", |s| s.as_str()));
                let r = api.admin_update_team(id, &TeamRequest { name, members });
                self.changed = true;
                self.run(r, "Team saved").is_some()
            }
            Op::DeleteTeam(id) => {
                self.changed = true;
                self.run(api.admin_delete_team(id), "Team deleted").is_some()
            }
            Op::CreateVault => {
                let req = VaultRequest { name: self.new_vault.clone(), description: self.new_vault_desc.clone(), grants: Vec::new(), record_ssh: None };
                let ok = self.run(api.admin_create_vault(&req), "Vault created - now give teams access to it").is_some();
                if ok {
                    self.new_vault.clear();
                    self.new_vault_desc.clear();
                    self.changed = true;
                }
                ok
            }
            Op::SaveVault(id) => {
                let Some(v) = self.vaults.iter().find(|v| v.id == id) else { return };
                let req = VaultRequest { name: v.name.clone(), description: v.description.clone(), grants: v.grants.clone(), record_ssh: Some(v.record_ssh) };
                self.changed = true;
                self.run(api.admin_update_vault(id, &req), "Access saved").is_some()
            }
            Op::DeleteVault(id) => {
                self.confirm_delete = None;
                self.changed = true;
                self.run(api.admin_delete_vault(id), "Vault deleted").is_some()
            }
        };
        if done {
            let (err, info) = (self.error.take(), self.info.take());
            self.refresh(api);
            self.error = self.error.take().or(err);
            self.info = info;
        }
    }

    fn vaults_tab(&mut self, ui: &mut egui::Ui, ops: &mut Vec<Op>) {
        ui.label("Shared vaults are synced to every client that has access. Grant access to teams (preferred) or individual users.");
        ui.add_space(6.0);
        for v in &mut self.vaults {
            egui::CollapsingHeader::new(RichText::new(format!("🗄 {}", v.name)).strong())
                .id_salt(v.id)
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label("Description");
                        ui.text_edit_singleline(&mut v.description);
                    });
                    ui.add_space(4.0);
                    let mut remove = None;
                    egui::Grid::new(("grants", v.id)).num_columns(3).striped(true).show(ui, |ui| {
                        for (i, g) in v.grants.iter_mut().enumerate() {
                            match &g.principal {
                                Principal::Team(t) => ui.label(format!(
                                    "👥 {}",
                                    self.teams.iter().find(|x| x.id == *t).map_or("(deleted team)", |x| x.name.as_str())
                                )),
                                Principal::User(u) => ui.label(format!("👤 {u}")),
                            };
                            egui::ComboBox::from_id_salt(("role", v.id, i)).selected_text(g.role.label()).show_ui(ui, |ui| {
                                for r in Role::ALL {
                                    ui.selectable_value(&mut g.role, r, r.label());
                                }
                            });
                            if ui.small_button("✖").on_hover_text("Remove access").clicked() {
                                remove = Some(i);
                            }
                            ui.end_row();
                        }
                    });
                    if let Some(i) = remove {
                        v.grants.remove(i);
                    }
                    if v.grants.is_empty() {
                        ui.label(RichText::new("No one but administrators can see this vault yet.").weak());
                    }

                    let draft = self.grant_drafts.entry(v.id).or_default();
                    ui.horizontal(|ui| {
                        ui.label("Add access:");
                        ui.selectable_value(&mut draft.to_team, true, "Team");
                        ui.selectable_value(&mut draft.to_team, false, "User");
                        let current = if draft.to_team {
                            Uuid::parse_str(&draft.principal)
                                .ok()
                                .and_then(|id| self.teams.iter().find(|t| t.id == id))
                                .map_or("choose...".to_string(), |t| t.name.clone())
                        } else if draft.principal.is_empty() {
                            "choose...".to_string()
                        } else {
                            draft.principal.clone()
                        };
                        egui::ComboBox::from_id_salt(("principal", v.id)).selected_text(current).show_ui(ui, |ui| {
                            if draft.to_team {
                                for t in &self.teams {
                                    ui.selectable_value(&mut draft.principal, t.id.to_string(), &t.name);
                                }
                            } else {
                                for u in &self.users {
                                    ui.selectable_value(&mut draft.principal, u.username.clone(), &u.username);
                                }
                            }
                        });
                        egui::ComboBox::from_id_salt(("newrole", v.id)).selected_text(draft.role.label()).show_ui(ui, |ui| {
                            for r in Role::ALL {
                                ui.selectable_value(&mut draft.role, r, r.label());
                            }
                        });
                        if ui.button("Add").clicked() && !draft.principal.is_empty() {
                            let principal = if draft.to_team {
                                Uuid::parse_str(&draft.principal).ok().map(Principal::Team)
                            } else {
                                Some(Principal::User(draft.principal.clone()))
                            };
                            if let Some(principal) = principal {
                                v.grants.retain(|g| g.principal != principal);
                                v.grants.push(Grant { principal, role: draft.role });
                                draft.principal.clear();
                            }
                        }
                    });

                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        if ui.button("💾 Save access").clicked() {
                            ops.push(Op::SaveVault(v.id));
                        }
                        if self.confirm_delete == Some(v.id) {
                            ui.colored_label(Color32::from_rgb(220, 80, 80), "Delete vault and ALL its entries?");
                            if ui.button("Yes, delete").clicked() {
                                ops.push(Op::DeleteVault(v.id));
                            }
                            if ui.button("No").clicked() {
                                self.confirm_delete = None;
                            }
                        } else if ui.button("🗑 Delete vault").clicked() {
                            self.confirm_delete = Some(v.id);
                        }
                    });
                });
        }

        ui.separator();
        ui.label(RichText::new("New vault").strong());
        ui.horizontal(|ui| {
            ui.add(egui::TextEdit::singleline(&mut self.new_vault).hint_text("Name, e.g. Infrastructure"));
            ui.add(egui::TextEdit::singleline(&mut self.new_vault_desc).hint_text("Description"));
            if ui.button("Create").clicked() && !self.new_vault.trim().is_empty() {
                ops.push(Op::CreateVault);
            }
        });
    }

    fn teams_tab(&mut self, ui: &mut egui::Ui, ops: &mut Vec<Op>) {
        ui.label("Teams group staff so vault access can be granted once per team. Members are usernames, comma separated.");
        ui.add_space(6.0);
        egui::Grid::new("teams").num_columns(3).striped(true).spacing([10.0, 6.0]).show(ui, |ui| {
            for t in &self.teams {
                ui.label(RichText::new(format!("👥 {}", t.name)).strong());
                let members = self.team_members.entry(t.id).or_default();
                ui.add(egui::TextEdit::singleline(members).desired_width(320.0));
                ui.horizontal(|ui| {
                    if ui.button("Save").clicked() {
                        ops.push(Op::SaveTeam(t.id));
                    }
                    if ui.button("Delete").clicked() {
                        ops.push(Op::DeleteTeam(t.id));
                    }
                });
                ui.end_row();
            }
        });
        ui.separator();
        ui.label(RichText::new("New team").strong());
        ui.horizontal(|ui| {
            ui.add(egui::TextEdit::singleline(&mut self.new_team).hint_text("Name, e.g. Helpdesk"));
            ui.add(egui::TextEdit::singleline(&mut self.new_team_members).hint_text("alice, bob"));
            if ui.button("Create").clicked() && !self.new_team.trim().is_empty() {
                ops.push(Op::CreateTeam);
            }
        });
        let usernames: Vec<&str> = self.users.iter().map(|u| u.username.as_str()).collect();
        ui.label(RichText::new(format!("Users: {}", usernames.join(", "))).small().weak());
    }

    fn users_tab(&mut self, ui: &mut egui::Ui, ops: &mut Vec<Op>) {
        egui::Grid::new("users").num_columns(5).striped(true).spacing([10.0, 6.0]).show(ui, |ui| {
            for h in ["Username", "Name", "Admin", "Disabled", "Set password"] {
                ui.label(RichText::new(h).strong());
            }
            ui.end_row();
            for u in &self.users {
                ui.label(&u.username);
                ui.label(&u.display_name);
                let mut admin = u.is_admin;
                if ui.checkbox(&mut admin, "").changed() {
                    ops.push(Op::UpdateUser(u.username.clone(), UpdateUserRequest { is_admin: Some(admin), ..Default::default() }));
                }
                let mut disabled = u.disabled;
                if ui.checkbox(&mut disabled, "").changed() {
                    ops.push(Op::UpdateUser(u.username.clone(), UpdateUserRequest { disabled: Some(disabled), ..Default::default() }));
                }
                let pw = self.reset_password.entry(u.username.clone()).or_default();
                ui.horizontal(|ui| {
                    ui.add(egui::TextEdit::singleline(pw).password(true).desired_width(140.0));
                    if ui.button("Set").clicked() && !pw.is_empty() {
                        ops.push(Op::UpdateUser(u.username.clone(), UpdateUserRequest { password: Some(pw.clone()), ..Default::default() }));
                    }
                });
                ui.end_row();
            }
        });
        ui.separator();
        ui.label(RichText::new("New user").strong());
        egui::Grid::new("newuser").num_columns(2).spacing([10.0, 6.0]).show(ui, |ui| {
            ui.label("Username");
            ui.text_edit_singleline(&mut self.new_user.username);
            ui.end_row();
            ui.label("Display name");
            ui.text_edit_singleline(&mut self.new_user.display_name);
            ui.end_row();
            ui.label("Initial password");
            ui.add(egui::TextEdit::singleline(&mut self.new_user.password).password(true));
            ui.end_row();
            ui.label("Administrator");
            ui.checkbox(&mut self.new_user.is_admin, "");
            ui.end_row();
        });
        if ui.button("Create user").clicked() {
            ops.push(Op::CreateUser);
        }
    }
}

fn split_members(s: &str) -> Vec<String> {
    s.split([',', ';', ' ']).map(|m| m.trim().to_lowercase()).filter(|m| !m.is_empty()).collect()
}
