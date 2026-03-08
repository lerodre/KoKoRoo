use eframe::egui;

use super::GroupView;
use super::helpers::paint_initial_avatar;
use crate::avatar;
use crate::group::{self, GroupMember};
use crate::gui::{HostelApp, load_avatar_texture};
use crate::identity;

/// Deferred actions from group settings UI.
enum GroupSettingsAction {
    Rename,
    Kick(usize),
    Promote(usize),
    Demote(usize),
    PickAvatar,
    InviteSelected,
    Delete,
}

impl HostelApp {
    pub(super) fn draw_group_settings(&mut self, ui: &mut egui::Ui) {
        let idx = match self.group_settings_idx {
            Some(i) if i < self.groups.len() => i,
            _ => {
                self.group_view = GroupView::List;
                return;
            }
        };

        let grp_id = self.groups[idx].group_id.clone();
        let my_pubkey = self.identity.pubkey;
        let is_admin = self.groups[idx].members.iter().any(|m| m.pubkey == my_pubkey && m.is_admin);

        // Non-admins cannot access settings — redirect to chat
        if !is_admin {
            self.group_view = GroupView::Detail;
            self.group_settings_idx = None;
            return;
        }
        let member_count = self.groups[idx].members.len();

        let mut actions: Vec<GroupSettingsAction> = Vec::new();

        egui::ScrollArea::vertical()
            .id_salt("group_settings_scroll")
            .show(ui, |ui| {
                ui.add_space(6.0);

                // Back to chat button
                if ui.button("<- Back to Chat").clicked() {
                    self.group_view = GroupView::Detail;
                    self.group_settings_idx = None;
                }

                ui.add_space(12.0);

                // ── Group avatar (96px circle) ──
                let avatar_size = 96.0;
                ui.vertical_centered(|ui| {
                    let (rect, response) = ui.allocate_exact_size(
                        egui::vec2(avatar_size, avatar_size),
                        if is_admin { egui::Sense::click() } else { egui::Sense::hover() },
                    );
                    let center = rect.center();
                    let radius = avatar_size / 2.0;

                    // Load group avatar texture lazily
                    if !self.group_avatar_textures.contains_key(&grp_id) {
                        if let Some(bytes) = avatar::load_group_avatar(&grp_id) {
                            if let Some(tex) = load_avatar_texture(
                                ui.ctx(),
                                &format!("grp_avatar_{}", &grp_id[..8.min(grp_id.len())]),
                                &bytes,
                                96,
                            ) {
                                self.group_avatar_textures.insert(grp_id.clone(), tex);
                            }
                        }
                    }

                    if let Some(tex) = self.group_avatar_textures.get(&grp_id) {
                        let uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
                        ui.painter().image(tex.id(), rect, uv, egui::Color32::WHITE);
                    } else {
                        // Placeholder circle with group initial
                        paint_initial_avatar(ui.painter(), rect, &self.groups[idx].name, &self.settings.theme);
                    }

                    // Hover effect (admin only)
                    if is_admin && response.hovered() {
                        ui.painter().circle_filled(center, radius, egui::Color32::from_black_alpha(40));
                        ui.painter().text(
                            center,
                            egui::Align2::CENTER_CENTER,
                            "Change",
                            egui::FontId::proportional(14.0),
                            egui::Color32::WHITE,
                        );
                    }

                    if is_admin && response.clicked() {
                        actions.push(GroupSettingsAction::PickAvatar);
                    }

                    if is_admin {
                        response.on_hover_cursor(egui::CursorIcon::PointingHand);
                        ui.label(
                            egui::RichText::new("Click to change group photo")
                                .size(11.0)
                                .color(self.settings.theme.text_muted()),
                        );
                    }
                });

                ui.add_space(12.0);

                // ── Group name (editable if admin) ──
                ui.horizontal(|ui| {
                    ui.label("Group name:");
                    if is_admin {
                        let te = egui::TextEdit::singleline(&mut self.group_rename_input)
                            .desired_width(180.0)
                            .hint_text("Group name…");
                        ui.add(te);
                        let name_changed = self.group_rename_input.trim() != self.groups[idx].name
                            && !self.group_rename_input.trim().is_empty();
                        if name_changed {
                            if ui.button("Save").clicked() {
                                actions.push(GroupSettingsAction::Rename);
                            }
                        }
                    } else {
                        ui.strong(&self.groups[idx].name);
                    }
                });

                ui.add_space(12.0);
                ui.separator();
                ui.add_space(4.0);

                // ── Members section ──
                ui.label(egui::RichText::new(format!("Members ({})", member_count)).strong().size(13.0));
                ui.add_space(4.0);

                let members: Vec<GroupMember> = self.groups[idx].members.clone();
                let color_even = self.settings.theme.panel_bg();
                let color_odd = self.settings.theme.sidebar_bg();

                for (i, member) in members.iter().enumerate() {
                    let bg = if i % 2 == 0 { color_even } else { color_odd };
                    let is_me = member.pubkey == my_pubkey;

                    let frame = egui::Frame::none()
                        .fill(bg)
                        .inner_margin(egui::Margin::symmetric(8.0, 4.0));
                    frame.show(ui, |ui| {
                        ui.horizontal(|ui| {
                            // Member avatar (small)
                            let av_size = 24.0;
                            let (av_rect, _) = ui.allocate_exact_size(
                                egui::vec2(av_size, av_size),
                                egui::Sense::hover(),
                            );
                            let mut drew = false;
                            if is_me {
                                if let Some(tex) = &self.own_avatar_texture {
                                    let uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
                                    ui.painter().image(tex.id(), av_rect, uv, egui::Color32::WHITE);
                                    drew = true;
                                }
                            } else {
                                let cid = identity::derive_contact_id(&my_pubkey, &member.pubkey);
                                if !self.contact_avatar_textures.contains_key(&cid) {
                                    if let Some(bytes) = avatar::load_contact_avatar(&cid) {
                                        if let Some(tex) = load_avatar_texture(
                                            ui.ctx(),
                                            &format!("gs_av_{}", &cid[..8.min(cid.len())]),
                                            &bytes,
                                            32,
                                        ) {
                                            self.contact_avatar_textures.insert(cid.clone(), tex);
                                        }
                                    }
                                }
                                if let Some(tex) = self.contact_avatar_textures.get(&cid) {
                                    let uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
                                    ui.painter().image(tex.id(), av_rect, uv, egui::Color32::WHITE);
                                    drew = true;
                                }
                            }
                            if !drew {
                                paint_initial_avatar(ui.painter(), av_rect, &member.nickname, &self.settings.theme);
                            }

                            // Nickname
                            ui.label(egui::RichText::new(&member.nickname).strong());

                            // Role badge
                            let role_text = if member.is_admin { "admin" } else { "member" };
                            let role_color = if member.is_admin {
                                self.settings.theme.btn_primary()
                            } else {
                                self.settings.theme.text_muted()
                            };
                            ui.label(egui::RichText::new(role_text).size(11.0).color(role_color));

                            if is_me {
                                ui.label(egui::RichText::new("(you)").size(11.0).color(self.settings.theme.text_muted()));
                            }

                            // Admin actions (only for other members, only if we are admin)
                            if is_admin && !is_me {
                                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                    // Kick button
                                    let kick_btn = egui::Button::new(
                                        egui::RichText::new("X").size(12.0).color(self.settings.theme.btn_negative()),
                                    ).min_size(egui::vec2(24.0, 20.0));
                                    if ui.add(kick_btn).on_hover_text("Remove from group").clicked() {
                                        actions.push(GroupSettingsAction::Kick(i));
                                    }

                                    // Promote/Demote button
                                    if member.is_admin {
                                        if ui.small_button("Demote").clicked() {
                                            actions.push(GroupSettingsAction::Demote(i));
                                        }
                                    } else {
                                        if ui.small_button("Promote").clicked() {
                                            actions.push(GroupSettingsAction::Promote(i));
                                        }
                                    }
                                });
                            }
                        });
                    });
                }

                ui.add_space(8.0);

                // ── Invite Members ──
                if is_admin {
                    if ui.button("+ Invite Members").clicked() {
                        self.group_settings_invite_mode = !self.group_settings_invite_mode;
                        if self.group_settings_invite_mode {
                            self.group_settings_selected_members = vec![false; self.contacts.len()];
                        }
                    }

                    if self.group_settings_invite_mode {
                        ui.add_space(4.0);
                        ui.label(egui::RichText::new("Select contacts to invite:").size(12.0));

                        // Ensure vec matches
                        if self.group_settings_selected_members.len() != self.contacts.len() {
                            self.group_settings_selected_members = vec![false; self.contacts.len()];
                        }

                        let existing_pubkeys: Vec<[u8; 32]> = self.groups[idx].members.iter().map(|m| m.pubkey).collect();

                        let mut invite_count = 0;
                        egui::ScrollArea::vertical().max_height(150.0).id_salt("invite_contacts").show(ui, |ui| {
                            for (ci, contact) in self.contacts.iter().enumerate() {
                                // Skip contacts already in the group
                                if existing_pubkeys.contains(&contact.pubkey) {
                                    continue;
                                }
                                ui.horizontal(|ui| {
                                    ui.checkbox(&mut self.group_settings_selected_members[ci], "");
                                    ui.label(&contact.nickname);
                                    ui.label(
                                        egui::RichText::new(&contact.fingerprint)
                                            .size(11.0)
                                            .color(self.settings.theme.text_muted()),
                                    );
                                });
                                if self.group_settings_selected_members[ci] {
                                    invite_count += 1;
                                }
                            }
                        });

                        if invite_count > 0 {
                            if ui.button(format!("Send {} Invite(s)", invite_count)).clicked() {
                                actions.push(GroupSettingsAction::InviteSelected);
                            }
                        }
                    }
                }

                ui.add_space(16.0);
                ui.separator();
                ui.add_space(8.0);

                // ── Delete Group ──
                let delete_btn = egui::Button::new(
                    egui::RichText::new("Delete Group")
                        .color(self.settings.theme.btn_negative()),
                );
                if ui.add(delete_btn).clicked() {
                    actions.push(GroupSettingsAction::Delete);
                }
            });

        // Process deferred actions
        for action in actions {
            match action {
                GroupSettingsAction::Rename => {
                    let new_name = self.group_rename_input.trim().to_string();
                    self.groups[idx].name = new_name;
                    group::save_group(&self.groups[idx]);
                    self.broadcast_group_update(idx);
                }
                GroupSettingsAction::Kick(member_idx) => {
                    if member_idx < self.groups[idx].members.len() {
                        let kicked_nick = self.groups[idx].members[member_idx].nickname.clone();
                        let kicked_pubkey = self.groups[idx].members[member_idx].pubkey;
                        log_fmt!("[gui] kicking '{}' from group '{}' — rotating group key",
                            kicked_nick, self.groups[idx].name);
                        group::remove_member(&mut self.groups[idx], &kicked_pubkey);
                        log_fmt!("[gui] group key rotated to v{}, broadcasting update to {} members",
                            self.groups[idx].key_version, self.groups[idx].members.len());
                        self.broadcast_group_update(idx);
                    }
                }
                GroupSettingsAction::Promote(member_idx) => {
                    if member_idx < self.groups[idx].members.len() {
                        self.groups[idx].members[member_idx].is_admin = true;
                        group::save_group(&self.groups[idx]);
                        self.broadcast_group_update(idx);
                    }
                }
                GroupSettingsAction::Demote(member_idx) => {
                    if member_idx < self.groups[idx].members.len() {
                        self.groups[idx].members[member_idx].is_admin = false;
                        group::save_group(&self.groups[idx]);
                        self.broadcast_group_update(idx);
                    }
                }
                GroupSettingsAction::PickAvatar => {
                    self.group_avatar_crop_group_id = Some(grp_id.clone());
                    self.open_avatar_picker();
                }
                GroupSettingsAction::InviteSelected => {
                    self.invite_members_to_group(idx);
                    self.group_settings_invite_mode = false;
                }
                GroupSettingsAction::Delete => {
                    let gid = self.groups[idx].group_id.clone();
                    group::delete_group(&gid);
                    self.groups.remove(idx);
                    self.group_settings_idx = None;
                    self.group_detail_idx = None;
                    self.group_view = GroupView::List;
                }
            }
        }
    }
}
