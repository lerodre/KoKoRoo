use eframe::egui;

use super::GroupView;
use super::helpers::paint_initial_avatar;
use crate::avatar;
use crate::chat::{ChatHistory, GroupChatHistory};
use crate::group::GroupMember;
use crate::gui::{HostelApp, load_avatar_texture};
use crate::identity;

impl HostelApp {
    pub(super) fn draw_group_detail(&mut self, ui: &mut egui::Ui) {
        let idx = match self.group_detail_idx {
            Some(i) if i < self.groups.len() => i,
            _ => {
                self.group_view = GroupView::List;
                return;
            }
        };

        // Check if selected channel is a voice channel
        let selected_channel_id = self.group_selected_channel.clone();
        let is_voice_channel = self.groups[idx].voice_channels.iter()
            .any(|ch| ch.channel_id == selected_channel_id && !ch.deleted);

        if is_voice_channel {
            if self.group_call_channel_id.as_deref() == Some(&selected_channel_id) {
                self.draw_group_voice_active(ui);
            } else {
                self.draw_group_voice_idle(ui);
            }
            return;
        }

        let grp_name = self.groups[idx].name.clone();
        let grp_id = self.groups[idx].group_id.clone();
        let member_count = self.groups[idx].members.len();
        let members: Vec<GroupMember> = self.groups[idx].members.clone();
        let my_pubkey = self.identity.pubkey;
        let is_admin = members.iter().any(|m| m.pubkey == my_pubkey && m.is_admin);
        let identity_secret = self.identity.secret;

        let mut open_settings = false;

        // Pre-compute column widths so the header can be constrained to chat area
        let avail_for_split = ui.available_rect_before_wrap();
        let sep_w = 1.0;
        let members_w = 180.0_f32.max(avail_for_split.width() * 0.22).min(240.0);
        let chat_w = (avail_for_split.width() - members_w - sep_w - 4.0).max(100.0);

        // ── Top bar: Group avatar + name + Settings ──
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.set_max_width(chat_w);

            // Group avatar in header (28px)
            let hdr_av = 28.0;
            let (av_rect, _) = ui.allocate_exact_size(
                egui::vec2(hdr_av, hdr_av),
                egui::Sense::hover(),
            );
            if !self.group_avatar_textures.contains_key(&grp_id) {
                if let Some(bytes) = avatar::load_group_avatar(&grp_id) {
                    if let Some(tex) = load_avatar_texture(
                        ui.ctx(),
                        &format!("gav_{}", &grp_id[..8.min(grp_id.len())]),
                        &bytes,
                        96,
                    ) {
                        self.group_avatar_textures.insert(grp_id.clone(), tex);
                    }
                }
            }
            if let Some(tex) = self.group_avatar_textures.get(&grp_id) {
                let uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
                ui.painter().image(tex.id(), av_rect, uv, egui::Color32::WHITE);
            } else {
                paint_initial_avatar(ui.painter(), av_rect, &grp_name, &self.settings.theme);
            }

            ui.heading(&grp_name);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if is_admin {
                    let settings_btn = egui::Button::new(
                        egui::RichText::new("Settings").size(12.0),
                    );
                    if ui.add(settings_btn).clicked() {
                        open_settings = true;
                    }
                }
            });
        });

        // Horizontal separator constrained to chat area width
        let sep_y = ui.cursor().top();
        ui.painter().hline(
            avail_for_split.min.x..=avail_for_split.min.x + chat_w,
            sep_y,
            egui::Stroke::new(1.0, self.settings.theme.text_muted()),
        );
        ui.add_space(2.0);

        let available = ui.available_rect_before_wrap();
        let clip = ui.clip_rect();

        // Background for right sidebar
        let bg_rect = egui::Rect::from_min_max(
            egui::pos2(available.min.x + chat_w + sep_w + 4.0, clip.min.y),
            egui::pos2(clip.max.x, clip.max.y),
        );
        ui.painter().rect_filled(bg_rect, 0.0, self.settings.theme.sidebar_bg());

        // Vertical separator between chat and members
        let sep_x = available.min.x + chat_w + 2.0;
        ui.painter().vline(sep_x, clip.y_range(), egui::Stroke::new(sep_w, self.settings.theme.text_muted()));

        let chat_rect = egui::Rect::from_min_size(
            available.min,
            egui::vec2(chat_w, available.height()),
        );
        let members_rect = egui::Rect::from_min_size(
            egui::pos2(available.min.x + chat_w + sep_w + 4.0, available.min.y),
            egui::vec2(members_w - 4.0, available.height()),
        );

        // ── Left panel: Chat + input ──
        let mut send_detail_chat = false;
        let selected_channel_id = self.group_selected_channel.clone();
        let is_fallback = selected_channel_id == "fallback";
        let channel_display_name = if let Some(grp) = self.groups.get(idx) {
            grp.text_channels.iter()
                .find(|ch| ch.channel_id == selected_channel_id)
                .map(|ch| ch.name.clone())
                .unwrap_or_else(|| selected_channel_id.clone())
        } else {
            selected_channel_id.clone()
        };
        ui.allocate_new_ui(egui::UiBuilder::new().max_rect(chat_rect), |ui| {
            ui.add_space(6.0);
            ui.label(egui::RichText::new(format!("Chat — # {}", channel_display_name)).strong().size(13.0));
            ui.add_space(4.0);

            let history = GroupChatHistory::load(&grp_id, &selected_channel_id, &identity_secret);
            let input_h = 54.0;
            let chat_h = (ui.available_height() - input_h - 8.0).max(40.0);

            // Build fingerprint → contact_id map for avatar lookups
            let fp_to_cid: std::collections::HashMap<&str, String> = members
                .iter()
                .map(|m| {
                    (
                        m.fingerprint.as_str(),
                        identity::derive_contact_id(&my_pubkey, &m.pubkey),
                    )
                })
                .collect();

            egui::ScrollArea::vertical()
                .max_height(chat_h)
                .auto_shrink(false)
                .stick_to_bottom(true)
                .id_salt("grp_detail_chat")
                .show(ui, |ui| {
                    ui.set_min_width(ui.available_width());

                    if history.messages.is_empty() {
                        ui.add_space(40.0);
                        ui.vertical_centered(|ui| {
                            ui.label(
                                egui::RichText::new("No messages yet")
                                    .color(self.settings.theme.text_muted()),
                            );
                        });
                    }

                    let avatar_size = 28.0;
                    let spacing = ui.spacing().item_spacing.x;
                    let mut prev_sender: Option<&str> = None;

                    for msg in &history.messages {
                        let is_own = msg.sender_fingerprint.is_empty()
                            || msg.sender_fingerprint == self.identity.fingerprint;
                        let same_sender = prev_sender == Some(msg.sender_nickname.as_str());
                        prev_sender = Some(msg.sender_nickname.as_str());

                        if same_sender {
                            ui.horizontal_wrapped(|ui| {
                                ui.add_space(avatar_size + spacing);
                                ui.add(egui::Label::new(&msg.text).wrap());
                            });
                            continue;
                        }

                        ui.add_space(3.0);

                        ui.horizontal(|ui| {
                            let (av_rect, _) = ui.allocate_exact_size(
                                egui::vec2(avatar_size, avatar_size),
                                egui::Sense::hover(),
                            );

                            let mut drew_avatar = false;
                            if is_own {
                                if self.own_avatar_texture.is_none() {
                                    if let Some(bytes) = avatar::load_own_avatar() {
                                        self.own_avatar_texture = load_avatar_texture(
                                            ui.ctx(), "own_avatar", &bytes, 96,
                                        );
                                    }
                                }
                                if let Some(tex) = &self.own_avatar_texture {
                                    let uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
                                    ui.painter().image(tex.id(), av_rect, uv, egui::Color32::WHITE);
                                    drew_avatar = true;
                                }
                            } else if let Some(contact_id) = fp_to_cid.get(msg.sender_fingerprint.as_str()) {
                                if !self.contact_avatar_textures.contains_key(contact_id) {
                                    if let Some(bytes) = avatar::load_contact_avatar(contact_id) {
                                        if let Some(tex) = load_avatar_texture(
                                            ui.ctx(),
                                            &format!("grp_av_{}", &contact_id[..8.min(contact_id.len())]),
                                            &bytes, 32,
                                        ) {
                                            self.contact_avatar_textures.insert(contact_id.clone(), tex);
                                        }
                                    }
                                }
                                if let Some(tex) = self.contact_avatar_textures.get(contact_id) {
                                    let uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
                                    ui.painter().image(tex.id(), av_rect, uv, egui::Color32::WHITE);
                                    drew_avatar = true;
                                }
                            }

                            if !drew_avatar {
                                paint_initial_avatar(ui.painter(), av_rect, &msg.sender_nickname, &self.settings.theme);
                            }

                            ui.label(
                                egui::RichText::new(&msg.sender_nickname)
                                    .strong()
                                    .color(self.settings.theme.btn_primary()),
                            );
                            ui.label(
                                egui::RichText::new(ChatHistory::format_time(msg.timestamp))
                                    .size(11.0)
                                    .color(self.settings.theme.text_muted()),
                            );
                        });

                        ui.horizontal_wrapped(|ui| {
                            ui.add_space(avatar_size + spacing);
                            ui.add(egui::Label::new(&msg.text).wrap());
                        });

                        ui.add_space(2.0);
                    }
                });

            // Chat input bar
            if is_fallback {
                ui.add_space(4.0);
                ui.label(
                    egui::RichText::new("This channel is read-only (sync conflict archive)")
                        .size(11.0)
                        .color(self.settings.theme.text_muted()),
                );
            } else {
                let bar_h = 38.0;
                let bar_frame = egui::Frame::none()
                    .fill(self.settings.theme.sidebar_bg())
                    .inner_margin(egui::Margin::symmetric(6.0, 6.0))
                    .rounding(4.0);
                bar_frame.show(ui, |ui| {
                    ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                        let attach_btn = egui::Button::new(
                            egui::RichText::new("+").size(18.0).strong(),
                        ).min_size(egui::vec2(bar_h, bar_h));
                        ui.add(attach_btn).on_hover_text("Send file");

                        let outline = self.settings.theme.text_muted();
                        ui.visuals_mut().widgets.inactive.bg_stroke = egui::Stroke::new(1.0, outline);
                        ui.visuals_mut().widgets.inactive.bg_fill = self.settings.theme.panel_bg();

                        let resp = ui.add_sized(
                            egui::vec2(ui.available_width() - 75.0, bar_h),
                            egui::TextEdit::singleline(&mut self.group_detail_chat_input)
                                .hint_text("Type a message...")
                                .margin(egui::vec2(8.0, 10.0)),
                        );
                        let enter = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                        if ui.add(egui::Button::new("Send").min_size(egui::vec2(60.0, bar_h))).clicked() || enter {
                            send_detail_chat = true;
                            resp.request_focus();
                        }
                    });
                });
            }
        });

        // ── Right sidebar: Members with online/offline status ──
        let color_even = self.settings.theme.panel_bg();
        let color_odd = self.settings.theme.sidebar_bg();

        // Collect members with online status, sorted: online first
        let mut member_entries: Vec<(&GroupMember, bool)> = members.iter().map(|m| {
            let is_me = m.pubkey == my_pubkey;
            let online = if is_me {
                true
            } else {
                let cid = identity::derive_contact_id(&my_pubkey, &m.pubkey);
                self.msg_peer_presence.get(&cid)
                    .map(|p| *p != crate::messaging::PresenceStatus::Offline)
                    .unwrap_or_else(|| {
                        self.msg_peer_online.get(&cid).copied().unwrap_or(false)
                    })
            };
            (m, online)
        }).collect();
        member_entries.sort_by_key(|(_, online)| if *online { 0 } else { 1 });

        let online_count = member_entries.iter().filter(|(_, o)| *o).count();

        ui.allocate_new_ui(egui::UiBuilder::new().max_rect(members_rect), |ui| {
            let header_w = ui.available_width();
            let header_rect = ui.allocate_space(egui::vec2(header_w, 28.0)).1;
            ui.painter().rect_filled(header_rect, 0.0, self.settings.theme.sidebar_bg());
            ui.painter().text(
                egui::pos2(header_rect.min.x + 8.0, header_rect.center().y),
                egui::Align2::LEFT_CENTER,
                format!("Members ({}) — {} online", member_count, online_count),
                egui::FontId::proportional(13.0),
                self.settings.theme.text_primary(),
            );
            let hline_stroke = egui::Stroke::new(1.0, self.settings.theme.text_muted());
            ui.painter().hline(header_rect.x_range(), header_rect.max.y, hline_stroke);

            if member_count > 8 {
                ui.colored_label(
                    self.settings.theme.btn_negative(),
                    ">8 — quality may degrade",
                );
            }

            egui::ScrollArea::vertical()
                .id_salt("detail_members")
                .show(ui, |ui| {
                    let row_w = ui.available_width();
                    for (i, (member, online)) in member_entries.iter().enumerate() {
                        let bg = if i % 2 == 0 { color_even } else { color_odd };
                        let row_rect = ui.allocate_space(egui::vec2(row_w, 32.0)).1;
                        ui.painter().rect_filled(row_rect, 0.0, bg);

                        let mut x = row_rect.min.x + 8.0;
                        let cy = row_rect.center().y;

                        // Presence dot
                        let dot_color = if *online {
                            egui::Color32::from_rgb(0x4C, 0xAF, 0x50)
                        } else {
                            self.settings.theme.text_muted()
                        };
                        ui.painter().circle_filled(egui::pos2(x + 4.0, cy), 3.5, dot_color);
                        x += 14.0;

                        let av_size = 22.0;
                        let av_rect = egui::Rect::from_center_size(
                            egui::pos2(x + av_size / 2.0, cy),
                            egui::vec2(av_size, av_size),
                        );
                        let mut drew_av = false;
                        if member.pubkey == my_pubkey {
                            if let Some(tex) = &self.own_avatar_texture {
                                let uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
                                ui.painter().image(tex.id(), av_rect, uv, egui::Color32::WHITE);
                                drew_av = true;
                            }
                        } else {
                            let cid = identity::derive_contact_id(&my_pubkey, &member.pubkey);
                            if !self.contact_avatar_textures.contains_key(&cid) {
                                if let Some(bytes) = avatar::load_contact_avatar(&cid) {
                                    if let Some(tex) = load_avatar_texture(
                                        ui.ctx(),
                                        &format!("dm_av_{}", &cid[..8.min(cid.len())]),
                                        &bytes, 32,
                                    ) {
                                        self.contact_avatar_textures.insert(cid.clone(), tex);
                                    }
                                }
                            }
                            if let Some(tex) = self.contact_avatar_textures.get(&cid) {
                                let uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
                                ui.painter().image(tex.id(), av_rect, uv, egui::Color32::WHITE);
                                drew_av = true;
                            }
                        }
                        if !drew_av {
                            paint_initial_avatar(ui.painter(), av_rect, &member.nickname, &self.settings.theme);
                        }
                        x += av_size + 6.0;

                        // Nickname (dimmed if offline)
                        let nick_color = if *online {
                            self.settings.theme.text_primary()
                        } else {
                            self.settings.theme.text_muted()
                        };
                        let nick_galley = ui.painter().layout_no_wrap(
                            member.nickname.clone(),
                            egui::FontId::proportional(13.0),
                            nick_color,
                        );
                        ui.painter().galley(
                            egui::pos2(x, cy - nick_galley.size().y / 2.0),
                            nick_galley.clone(),
                            nick_color,
                        );
                        x += nick_galley.size().x + 6.0;

                        let role_text = if member.is_admin { "admin" } else { "member" };
                        let role_color = if member.is_admin {
                            self.settings.theme.btn_primary()
                        } else {
                            self.settings.theme.text_muted()
                        };
                        let role_galley = ui.painter().layout_no_wrap(
                            role_text.to_string(),
                            egui::FontId::proportional(11.0),
                            role_color,
                        );
                        ui.painter().galley(
                            egui::pos2(x, cy - role_galley.size().y / 2.0),
                            role_galley.clone(),
                            role_color,
                        );
                        x += role_galley.size().x + 5.0;

                        if member.pubkey == my_pubkey {
                            let you_galley = ui.painter().layout_no_wrap(
                                "(you)".to_string(),
                                egui::FontId::proportional(11.0),
                                self.settings.theme.text_muted(),
                            );
                            ui.painter().galley(
                                egui::pos2(x, cy - you_galley.size().y / 2.0),
                                you_galley,
                                self.settings.theme.text_muted(),
                            );
                        }
                    }
                });
        });

        // Deferred actions
        if open_settings {
            self.group_settings_idx = Some(idx);
            self.group_rename_input = grp_name.clone();
            self.group_settings_invite_mode = false;
            self.group_settings_selected_members = Vec::new();
            self.group_view = GroupView::Settings;
        }
        if send_detail_chat && !is_fallback {
            let text = self.group_detail_chat_input.trim().to_string();
            if !text.is_empty() {
                let my_nickname = self.settings.nickname.clone();
                let my_fingerprint = self.identity.fingerprint.clone();
                {
                    let mut history = GroupChatHistory::load(&grp_id, &selected_channel_id, &self.identity.secret);
                    history.add_message(my_fingerprint, my_nickname.clone(), text.clone());
                }
                if let Some(tx) = &self.msg_cmd_tx {
                    for member in &members {
                        if member.pubkey == my_pubkey {
                            continue;
                        }
                        let contact_id = identity::derive_contact_id(&my_pubkey, &member.pubkey);
                        // Use live contact address (updated by messaging daemon) instead of
                        // potentially stale group member address
                        let (addr_ip, addr_port) = self.contacts.iter()
                            .find(|c| c.contact_id == contact_id)
                            .map(|c| (c.last_address.clone(), c.last_port.clone()))
                            .unwrap_or_else(|| (member.address.clone(), member.port.clone()));
                        if addr_ip.is_empty() || addr_port.is_empty() {
                            continue;
                        }
                        let addr_str = format!("[{}]:{}", addr_ip, addr_port);
                        if let Ok(addr) = addr_str.parse() {
                            tx.send(crate::messaging::MsgCommand::SendGroupChat {
                                contact_id,
                                peer_addr: addr,
                                peer_pubkey: member.pubkey,
                                group_id: grp_id.clone(),
                                channel_id: selected_channel_id.clone(),
                                text: text.clone(),
                            }).ok();
                        }
                    }
                }
            }
            self.group_detail_chat_input.clear();
        }
    }
}
