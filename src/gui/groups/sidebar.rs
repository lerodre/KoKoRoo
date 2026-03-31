use eframe::egui;

use super::GroupView;
use super::helpers::paint_initial_avatar;
use crate::avatar;
use crate::group;
use crate::gui::{HostelApp, load_avatar_texture};

impl HostelApp {
    pub(super) fn draw_group_icon_strip(
        &mut self,
        ui: &mut egui::Ui,
        open_idx: &mut Option<usize>,
        go_create: &mut bool,
    ) {
        // "+" create group button
        {
            let strip_w = ui.available_width();
            let btn_sz = 32.0;
            let row_h = 38.0;
            let (row_rect, row_resp) = ui.allocate_exact_size(
                egui::vec2(strip_w, row_h),
                egui::Sense::click(),
            );
            if row_resp.hovered() {
                ui.painter().rect_filled(
                    row_rect, 4.0,
                    self.settings.theme.widget_bg().gamma_multiply(0.5),
                );
            }
            let circle_center = row_rect.center();
            let radius = btn_sz / 2.0;
            ui.painter().circle_stroke(
                circle_center,
                radius,
                egui::Stroke::new(1.5, self.settings.theme.text_muted()),
            );
            ui.painter().text(
                circle_center,
                egui::Align2::CENTER_CENTER,
                "+",
                egui::FontId::proportional(18.0),
                self.settings.theme.text_muted(),
            );
            row_resp.clone().on_hover_text("Create Group");
            if row_resp.clicked() {
                *go_create = true;
            }

            // Separator line below the + button
            let sep_y = row_rect.max.y + 2.0;
            ui.painter().hline(
                row_rect.x_range(),
                sep_y,
                egui::Stroke::new(1.0, self.settings.theme.text_muted().gamma_multiply(0.4)),
            );
            ui.add_space(4.0);
        }

        let active_idx = self.group_detail_idx;
        let mut leave_group_idx: Option<usize> = None;

        let max_height = ui.available_height().max(80.0);
        egui::ScrollArea::vertical()
            .max_height(max_height)
            .id_salt("groups_icon_strip")
            .show(ui, |ui| {
                let strip_w = ui.available_width();
                for (idx, grp) in self.groups.iter().enumerate() {
                    let is_active = active_idx == Some(idx)
                        && (self.group_view == GroupView::Detail || self.group_view == GroupView::Settings || self.group_view == GroupView::InCall || self.group_view == GroupView::Connecting);

                    let av_sz = 32.0;
                    let row_h = 42.0;
                    let (row_rect, row_resp) = ui.allocate_exact_size(
                        egui::vec2(strip_w, row_h),
                        egui::Sense::click(),
                    );

                    // Hover background
                    if row_resp.hovered() && !is_active {
                        ui.painter().rect_filled(
                            row_rect, 4.0,
                            self.settings.theme.widget_bg().gamma_multiply(0.5),
                        );
                    }

                    // Active indicator: 3px pill on left edge
                    if is_active {
                        let pill_rect = egui::Rect::from_min_size(
                            egui::pos2(row_rect.min.x, row_rect.center().y - 10.0),
                            egui::vec2(3.0, 20.0),
                        );
                        ui.painter().rect_filled(pill_rect, 2.0, self.settings.theme.text_primary());
                    }

                    // Avatar centered
                    let av_rect = egui::Rect::from_center_size(
                        row_rect.center(),
                        egui::vec2(av_sz, av_sz),
                    );
                    let grp_id = &grp.group_id;
                    if let Some(tex) = self.group_avatar_textures.get(grp_id) {
                        let uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
                        ui.painter().image(tex.id(), av_rect, uv, egui::Color32::WHITE);
                    } else {
                        // Try loading
                        if !self.group_avatar_textures.contains_key(grp_id) {
                            if let Some(bytes) = avatar::load_group_avatar(grp_id) {
                                if let Some(tex) = load_avatar_texture(
                                    ui.ctx(),
                                    &format!("gis_{}", &grp_id[..8.min(grp_id.len())]),
                                    &bytes,
                                    96,
                                ) {
                                    self.group_avatar_textures.insert(grp_id.clone(), tex);
                                }
                            }
                        }
                        if let Some(tex) = self.group_avatar_textures.get(grp_id) {
                            let uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
                            ui.painter().image(tex.id(), av_rect, uv, egui::Color32::WHITE);
                        } else {
                            paint_initial_avatar(ui.painter(), av_rect, &grp.name, &self.settings.theme);
                        }
                    }

                    // Unread badge (red circle at top-right of avatar)
                    let grp_id_ref = &grp.group_id;
                    if let Some(&count) = self.group_unread.get(grp_id_ref) {
                        if count > 0 {
                            let badge_color = self.settings.theme.btn_negative();
                            let badge_text = format!("{}", count);
                            let font = egui::FontId::proportional(9.0);
                            let text_galley = ui.painter().layout_no_wrap(badge_text, font, egui::Color32::WHITE);
                            let text_w = text_galley.size().x;
                            let text_h = text_galley.size().y;
                            let radius = (text_w / 2.0 + 3.0).max(7.0);
                            let badge_center = egui::pos2(av_rect.right() + 2.0, av_rect.top() - 2.0);
                            ui.painter().circle_filled(badge_center, radius, badge_color);
                            ui.painter().galley(
                                egui::pos2(badge_center.x - text_w / 2.0, badge_center.y - text_h / 2.0),
                                text_galley,
                                egui::Color32::WHITE,
                            );
                        }
                    }

                    // Tooltip with group name
                    row_resp.clone().on_hover_text(&grp.name);

                    // Right-click: Leave Group + Mute/Unmute
                    let is_muted = self.settings.muted_groups.contains(&grp.group_id);
                    let grp_id_ctx = grp.group_id.clone();
                    row_resp.context_menu(|ui| {
                        let mute_label = if is_muted { "Unmute" } else { "Mute" };
                        if ui.button(mute_label).clicked() {
                            if is_muted {
                                self.settings.muted_groups.retain(|g| g != &grp_id_ctx);
                            } else {
                                self.settings.muted_groups.push(grp_id_ctx.clone());
                                // Clear existing unread when muting
                                self.group_unread.remove(&grp_id_ctx);
                                self.group_channel_unread.retain(|(gid, _), _| gid != &grp_id_ctx);
                            }
                            self.settings.save();
                            ui.close_menu();
                        }
                        if ui.button("Leave Group").clicked() {
                            leave_group_idx = Some(idx);
                            ui.close_menu();
                        }
                    });

                    if row_resp.clicked() {
                        *open_idx = Some(idx);
                    }
                }
            });

        // Handle deferred leave group
        if let Some(idx) = leave_group_idx {
            if idx < self.groups.len() {
                // If we're in a call on this group, cleanup first
                if self.group_detail_idx == Some(idx) && self.group_call_channel_id.is_some() {
                    self.cleanup_group_call();
                }
                let gid = self.groups[idx].group_id.clone();
                log_fmt!("[gui] leaving group: {}", self.groups[idx].name);
                group::delete_group(&gid);
                self.groups.remove(idx);
                self.group_avatar_textures.remove(&gid);
                if self.group_detail_idx == Some(idx) {
                    self.group_detail_idx = None;
                    self.group_settings_idx = None;
                    self.group_view = GroupView::List;
                } else if let Some(ref mut di) = self.group_detail_idx {
                    if *di > idx { *di -= 1; }
                }
            }
        }
    }

    pub(super) fn draw_channels_sidebar(&mut self, ui: &mut egui::Ui, _group_name: &str) {
        ui.add_space(8.0);

        // Determine if user is admin for this group
        let (is_admin, _group_id, text_channels) = if let Some(idx) = self.group_detail_idx {
            if let Some(grp) = self.groups.get(idx) {
                let admin = grp.members.iter().any(|m| m.pubkey == self.identity.pubkey && m.is_admin);
                (admin, grp.group_id.clone(), grp.text_channels.clone())
            } else {
                (false, String::new(), Vec::new())
            }
        } else {
            (false, String::new(), Vec::new())
        };

        // TEXT GROUPS section header
        ui.horizontal(|ui| {
            ui.add_space(8.0);
            ui.label(
                egui::RichText::new("TEXT GROUPS")
                    .size(10.0)
                    .color(self.settings.theme.text_muted()),
            );
            if is_admin {
                let plus_btn = ui.small_button(
                    egui::RichText::new("+").size(10.0).color(self.settings.theme.text_muted()),
                );
                if plus_btn.clicked() {
                    self.group_channel_creating = !self.group_channel_creating;
                    self.group_channel_create_name.clear();
                }
            }
        });
        ui.add_space(2.0);

        // Inline channel creation UI
        if self.group_channel_creating && is_admin {
            ui.horizontal(|ui| {
                ui.add_space(12.0);
                let resp = ui.add_sized(
                    egui::vec2(ui.available_width() - 70.0, 22.0),
                    egui::TextEdit::singleline(&mut self.group_channel_create_name)
                        .hint_text("channel name")
                        .desired_width(80.0),
                );
                let enter = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                if (ui.small_button("OK").clicked() || enter) && !self.group_channel_create_name.trim().is_empty() {
                    let ch_name = self.group_channel_create_name.trim().to_lowercase().replace(' ', "-");
                    let now = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs();
                    let new_ch = group::TextChannel {
                        channel_id: group::generate_channel_id(),
                        name: ch_name,
                        created_at: now,
                        created_by: self.identity.pubkey,
                        deleted: false,
                        deleted_at: None,
                    };
                    if let Some(idx) = self.group_detail_idx {
                        if let Some(grp) = self.groups.get_mut(idx) {
                            log_fmt!("[gui] channel created: '{}' in group {}", new_ch.name, grp.group_id);
                            grp.text_channels.push(new_ch);
                            group::save_group(grp);
                            // Broadcast group update to all members
                            if let Some(tx) = &self.msg_cmd_tx {
                                let group_json = serde_json::to_vec(grp).unwrap_or_default();
                                let my_pubkey = self.identity.pubkey;
                                let member_contacts: Vec<_> = grp.members.iter()
                                    .filter(|m| m.pubkey != my_pubkey && !m.address.is_empty() && !m.port.is_empty())
                                    .filter_map(|m| {
                                        let addr_str = format!("[{}]:{}", m.address, m.port);
                                        addr_str.parse().ok().map(|addr| {
                                            let cid = crate::identity::derive_contact_id(&my_pubkey, &m.pubkey);
                                            (cid, addr, m.pubkey)
                                        })
                                    })
                                    .collect();
                                tx.send(crate::messaging::MsgCommand::SendGroupUpdate {
                                    group_id: grp.group_id.clone(),
                                    group_json,
                                    member_contacts,
                                }).ok();
                            }
                        }
                    }
                    self.group_channel_creating = false;
                    self.group_channel_create_name.clear();
                }
            });
            ui.add_space(2.0);
        }

        // List all non-deleted text channels (including fallback, always visible)
        let mut channel_to_delete: Option<String> = None;
        for ch in &text_channels {
            if ch.deleted {
                continue;
            }

            let row_w = ui.available_width();
            let (row_rect, row_resp) = ui.allocate_exact_size(
                egui::vec2(row_w, 28.0),
                egui::Sense::click(),
            );
            let is_sel = self.group_selected_channel == ch.channel_id;
            if is_sel {
                ui.painter().rect_filled(row_rect, 4.0, self.settings.theme.widget_bg());
            } else if row_resp.hovered() {
                ui.painter().rect_filled(
                    row_rect, 4.0,
                    self.settings.theme.widget_bg().gamma_multiply(0.5),
                );
            }
            let text_color = if is_sel {
                self.settings.theme.text_primary()
            } else {
                self.settings.theme.text_muted()
            };
            ui.painter().text(
                egui::pos2(row_rect.min.x + 12.0, row_rect.center().y),
                egui::Align2::LEFT_CENTER,
                format!("# {}", ch.name),
                egui::FontId::proportional(12.0),
                text_color,
            );
            // Per-channel unread badge
            let ch_unread = self.group_channel_unread
                .get(&(_group_id.clone(), ch.channel_id.clone()))
                .copied()
                .unwrap_or(0);
            if ch_unread > 0 {
                let badge_color = self.settings.theme.btn_negative();
                let badge_text = format!("{}", ch_unread);
                let badge_font = egui::FontId::proportional(9.0);
                let badge_galley = ui.painter().layout_no_wrap(badge_text, badge_font, egui::Color32::WHITE);
                let bw = badge_galley.size().x;
                let bh = badge_galley.size().y;
                let radius = (bw / 2.0 + 3.0).max(7.0);
                let badge_center = egui::pos2(row_rect.max.x - radius - 4.0, row_rect.center().y);
                ui.painter().circle_filled(badge_center, radius, badge_color);
                ui.painter().galley(
                    egui::pos2(badge_center.x - bw / 2.0, badge_center.y - bh / 2.0),
                    badge_galley,
                    egui::Color32::WHITE,
                );
            }
            if row_resp.clicked() {
                self.group_selected_channel = ch.channel_id.clone();
                // Clear per-channel unread
                self.group_channel_unread.remove(&(_group_id.clone(), ch.channel_id.clone()));
            }
            // Right-click context menu for delete (admin only, not general/fallback)
            if is_admin && ch.channel_id != "general" && ch.channel_id != "fallback" {
                row_resp.context_menu(|ui| {
                    if ui.button("Delete channel").clicked() {
                        channel_to_delete = Some(ch.channel_id.clone());
                        ui.close_menu();
                    }
                });
            }
        }

        // Handle deferred channel deletion
        if let Some(del_id) = channel_to_delete {
            if let Some(idx) = self.group_detail_idx {
                if let Some(grp) = self.groups.get_mut(idx) {
                    let now = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs();
                    // Find channel name for migration prefix
                    let ch_name = grp.text_channels.iter()
                        .find(|ch| ch.channel_id == del_id)
                        .map(|ch| ch.name.clone())
                        .unwrap_or_default();
                    log_fmt!("[gui] channel deleted: '{}' in group {}", ch_name, grp.group_id);
                    // Mark as deleted
                    if let Some(ch) = grp.text_channels.iter_mut().find(|ch| ch.channel_id == del_id) {
                        ch.deleted = true;
                        ch.deleted_at = Some(now);
                    }
                    // Migrate messages to fallback
                    crate::chat::migrate_messages_to_fallback(
                        &grp.group_id, &del_id, &ch_name, &self.identity.secret,
                    );
                    group::save_group(grp);
                    // Switch to general
                    self.group_selected_channel = "general".to_string();
                    // Broadcast group update
                    if let Some(tx) = &self.msg_cmd_tx {
                        let group_json = serde_json::to_vec(grp).unwrap_or_default();
                        let my_pubkey = self.identity.pubkey;
                        let member_contacts: Vec<_> = grp.members.iter()
                            .filter(|m| m.pubkey != my_pubkey && !m.address.is_empty() && !m.port.is_empty())
                            .filter_map(|m| {
                                let addr_str = format!("[{}]:{}", m.address, m.port);
                                addr_str.parse().ok().map(|addr| {
                                    let cid = crate::identity::derive_contact_id(&my_pubkey, &m.pubkey);
                                    (cid, addr, m.pubkey)
                                })
                            })
                            .collect();
                        tx.send(crate::messaging::MsgCommand::SendGroupUpdate {
                            group_id: grp.group_id.clone(),
                            group_json,
                            member_contacts,
                        }).ok();
                    }
                }
            }
        }

        ui.add_space(10.0);

        // VOICE GROUPS section header
        ui.horizontal(|ui| {
            ui.add_space(8.0);
            ui.label(
                egui::RichText::new("VOICE GROUPS")
                    .size(10.0)
                    .color(self.settings.theme.text_muted()),
            );
            if is_admin {
                let plus_btn = ui.small_button(
                    egui::RichText::new("+").size(10.0).color(self.settings.theme.text_muted()),
                );
                if plus_btn.clicked() {
                    self.voice_channel_creating = !self.voice_channel_creating;
                    self.voice_channel_create_name.clear();
                }
            }
        });
        ui.add_space(2.0);

        // Inline voice channel creation UI
        if self.voice_channel_creating && is_admin {
            ui.horizontal(|ui| {
                ui.add_space(12.0);
                let resp = ui.add_sized(
                    egui::vec2(ui.available_width() - 70.0, 22.0),
                    egui::TextEdit::singleline(&mut self.voice_channel_create_name)
                        .hint_text("channel name")
                        .desired_width(80.0),
                );
                let enter = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                if (ui.small_button("OK").clicked() || enter) && !self.voice_channel_create_name.trim().is_empty() {
                    let ch_name = self.voice_channel_create_name.trim().to_lowercase().replace(' ', "-");
                    let now = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs();
                    let new_ch = group::VoiceChannel {
                        channel_id: group::generate_channel_id(),
                        name: ch_name,
                        created_at: now,
                        created_by: self.identity.pubkey,
                        deleted: false,
                        deleted_at: None,
                    };
                    if let Some(idx) = self.group_detail_idx {
                        if let Some(grp) = self.groups.get_mut(idx) {
                            grp.voice_channels.push(new_ch);
                            group::save_group(grp);
                            self.broadcast_group_update(idx);
                        }
                    }
                    self.voice_channel_creating = false;
                    self.voice_channel_create_name.clear();
                }
            });
            ui.add_space(2.0);
        }

        // Dynamic voice channels list
        let voice_channels = if let Some(idx) = self.group_detail_idx {
            if let Some(grp) = self.groups.get(idx) {
                grp.voice_channels.clone()
            } else {
                Vec::new()
            }
        } else {
            Vec::new()
        };

        let mut voice_channel_to_delete: Option<String> = None;
        let active_voice_channel = self.group_call_channel_id.clone();

        for ch in &voice_channels {
            if ch.deleted {
                continue;
            }

            let row_w = ui.available_width();
            let (row_rect, row_resp) = ui.allocate_exact_size(
                egui::vec2(row_w, 28.0),
                egui::Sense::click(),
            );
            let is_sel = self.group_selected_channel == ch.channel_id;
            let is_active = active_voice_channel.as_deref() == Some(&ch.channel_id);
            if is_sel || is_active {
                ui.painter().rect_filled(row_rect, 4.0, self.settings.theme.widget_bg());
            } else if row_resp.hovered() {
                ui.painter().rect_filled(
                    row_rect, 4.0,
                    self.settings.theme.widget_bg().gamma_multiply(0.5),
                );
            }
            let text_color = if is_sel || is_active {
                self.settings.theme.text_primary()
            } else {
                self.settings.theme.text_muted()
            };
            ui.painter().text(
                egui::pos2(row_rect.min.x + 12.0, row_rect.center().y),
                egui::Align2::LEFT_CENTER,
                format!("\u{00BB} {}", ch.name),
                egui::FontId::proportional(12.0),
                text_color,
            );
            if row_resp.clicked() {
                self.group_selected_channel = ch.channel_id.clone();
                // Just select the channel — the detail panel will show the join screen
            }

            // Right-click context menu for delete (admin only, not voice_general)
            if is_admin && ch.channel_id != "voice_general" {
                row_resp.context_menu(|ui| {
                    if ui.button("Delete voice channel").clicked() {
                        voice_channel_to_delete = Some(ch.channel_id.clone());
                        ui.close_menu();
                    }
                });
            }

            // Show connected members indented under voice channel
            // When in the call: show roster (group_call_members)
            // When not in the call: show presence signals (group_call_presence)
            if is_active {
                // We're in this call — show roster members with speaking indicator
                let my_pubkey = self.identity.pubkey;
                let voice_levels_snapshot: std::collections::HashMap<u16, f32> = self.group_voice_levels
                    .as_ref()
                    .and_then(|vl| vl.lock().ok().map(|m| m.clone()))
                    .unwrap_or_default();
                let speaking_threshold = 0.01;

                for member in &self.group_call_members {
                    let member_row_w = ui.available_width();
                    let (member_rect, _) = ui.allocate_exact_size(
                        egui::vec2(member_row_w, 24.0),
                        egui::Sense::hover(),
                    );
                    let mut x = member_rect.min.x + 20.0;
                    let cy = member_rect.center().y;
                    let av_size = 20.0;
                    let av_rect = egui::Rect::from_center_size(
                        egui::pos2(x + av_size / 2.0, cy),
                        egui::vec2(av_size, av_size),
                    );

                    // Speaking indicator (green border)
                    let is_speaking = voice_levels_snapshot.get(&member.sender_index)
                        .map(|&level| level > speaking_threshold)
                        .unwrap_or(false);
                    if is_speaking {
                        let border_rect = av_rect.expand(2.0);
                        ui.painter().rect_stroke(
                            border_rect,
                            egui::Rounding::same(3.0),
                            egui::Stroke::new(2.0, egui::Color32::from_rgb(67, 181, 129)),
                        );
                    }

                    let mut drew_av = false;
                    if member.pubkey == my_pubkey {
                        if let Some(tex) = &self.own_avatar_texture {
                            let uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
                            ui.painter().image(tex.id(), av_rect, uv, egui::Color32::WHITE);
                            drew_av = true;
                        }
                    } else {
                        let cid = crate::identity::derive_contact_id(&my_pubkey, &member.pubkey);
                        if let Some(tex) = self.contact_avatar_textures.get(&cid) {
                            let uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
                            ui.painter().image(tex.id(), av_rect, uv, egui::Color32::WHITE);
                            drew_av = true;
                        }
                    }
                    if !drew_av {
                        paint_initial_avatar(ui.painter(), av_rect, &member.nickname, &self.settings.theme);
                    }
                    x += av_size + 4.0;
                    ui.painter().text(
                        egui::pos2(x, cy),
                        egui::Align2::LEFT_CENTER,
                        &member.nickname,
                        egui::FontId::proportional(11.0),
                        self.settings.theme.text_muted(),
                    );
                }
            } else {
                // Not in call — show presence from call signals
                let presence = self.group_call_presence
                    .get(&_group_id)
                    .and_then(|channels| channels.get(&ch.channel_id));
                if let Some(members_in_call) = presence {
                    for (contact_id, _mode) in members_in_call {
                        let nickname = self.contacts.iter()
                            .find(|c| c.contact_id == *contact_id)
                            .map(|c| c.nickname.as_str())
                            .unwrap_or("?");
                        let member_row_w = ui.available_width();
                        let (member_rect, _) = ui.allocate_exact_size(
                            egui::vec2(member_row_w, 24.0),
                            egui::Sense::hover(),
                        );
                        let mut x = member_rect.min.x + 20.0;
                        let cy = member_rect.center().y;
                        let av_size = 20.0;
                        let av_rect = egui::Rect::from_center_size(
                            egui::pos2(x + av_size / 2.0, cy),
                            egui::vec2(av_size, av_size),
                        );
                        let mut drew_av = false;
                        let cid = contact_id.clone();
                        if let Some(tex) = self.contact_avatar_textures.get(&cid) {
                            let uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
                            ui.painter().image(tex.id(), av_rect, uv, egui::Color32::WHITE);
                            drew_av = true;
                        }
                        if !drew_av {
                            paint_initial_avatar(ui.painter(), av_rect, nickname, &self.settings.theme);
                        }
                        x += av_size + 4.0;
                        ui.painter().text(
                            egui::pos2(x, cy),
                            egui::Align2::LEFT_CENTER,
                            nickname,
                            egui::FontId::proportional(11.0),
                            self.settings.theme.text_muted(),
                        );
                    }
                }
            }
        }

        // Handle deferred voice channel deletion
        if let Some(del_id) = voice_channel_to_delete {
            if let Some(idx) = self.group_detail_idx {
                if let Some(grp) = self.groups.get_mut(idx) {
                    let now = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs();
                    if let Some(ch) = grp.voice_channels.iter_mut().find(|ch| ch.channel_id == del_id) {
                        ch.deleted = true;
                        ch.deleted_at = Some(now);
                    }
                    group::save_group(grp);
                    // If someone is in this deleted channel, disconnect them
                    if self.group_call_channel_id.as_deref() == Some(&del_id) {
                        self.cleanup_group_call();
                    }
                    self.broadcast_group_update(idx);
                }
            }
        }
    }
}
