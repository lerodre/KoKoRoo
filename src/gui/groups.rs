use eframe::egui;
use std::sync::atomic::Ordering;

use super::{HostelApp, load_avatar_texture};
use crate::avatar;
use crate::chat::{ChatHistory, GroupChatHistory};
use crate::group::{self, Group, GroupMember};
use crate::group_voice::{GroupChatMsg, GroupRole};
use crate::identity;

#[derive(Clone, Copy, PartialEq)]
pub(crate) enum GroupView {
    List,
    Create,
    Detail,
    Settings,
    Connecting,
    InCall,
}

impl HostelApp {
    pub(crate) fn draw_groups_tab(&mut self, ui: &mut egui::Ui) {
        match self.group_view {
            GroupView::List | GroupView::Detail | GroupView::Settings => {
                // 2-column layout: sidebar (group list) + panel (detail or placeholder)
                let available = ui.available_rect_before_wrap();
                let clip = ui.clip_rect();
                let sep_w = 1.0;
                let list_w = 165.0_f32.min(available.width() * 0.28);
                let detail_w = (available.width() - list_w - sep_w - 4.0).max(100.0);

                // Full-height sidebar background
                let bg_rect = egui::Rect::from_min_max(
                    egui::pos2(clip.min.x, clip.min.y),
                    egui::pos2(clip.min.x + list_w + (available.min.x - clip.min.x), clip.max.y),
                );
                ui.painter().rect_filled(bg_rect, 0.0, self.settings.theme.sidebar_bg());

                let line_stroke = egui::Stroke::new(sep_w, self.settings.theme.text_muted());
                ui.painter().vline(clip.min.x, clip.y_range(), line_stroke);
                let sep_x = available.min.x + list_w + 1.0;
                ui.painter().vline(sep_x, clip.y_range(), line_stroke);

                let list_rect = egui::Rect::from_min_size(
                    available.min,
                    egui::vec2(list_w, available.height()),
                );
                let detail_rect = egui::Rect::from_min_size(
                    egui::pos2(available.min.x + list_w + sep_w + 4.0, available.min.y),
                    egui::vec2(detail_w, available.height()),
                );

                // Left panel: group sidebar
                let mut open_idx: Option<usize> = None;
                let mut delete_idx: Option<usize> = None;
                let mut settings_idx: Option<usize> = None;
                let mut go_create = false;
                ui.allocate_new_ui(egui::UiBuilder::new().max_rect(list_rect), |ui| {
                    self.draw_group_sidebar(ui, &mut open_idx, &mut delete_idx, &mut settings_idx, &mut go_create);
                });

                // Right panel: detail, settings, or placeholder
                ui.allocate_new_ui(egui::UiBuilder::new().max_rect(detail_rect), |ui| {
                    if self.group_settings_idx.is_some() && self.group_view == GroupView::Settings {
                        self.draw_group_settings(ui);
                    } else if self.group_detail_idx.is_some() && self.group_view == GroupView::Detail {
                        self.draw_group_detail(ui);
                    } else {
                        ui.add_space(40.0);
                        ui.vertical_centered(|ui| {
                            ui.colored_label(
                                self.settings.theme.text_muted(),
                                "Select a group to start chatting",
                            );
                        });
                    }
                });

                // Deferred sidebar actions
                if let Some(idx) = delete_idx {
                    if idx < self.groups.len() {
                        let group_id = self.groups[idx].group_id.clone();
                        group::delete_group(&group_id);
                        self.groups.remove(idx);
                        // If the deleted group was active, clear selection
                        if self.group_detail_idx == Some(idx) {
                            self.group_detail_idx = None;
                            self.group_view = GroupView::List;
                        } else if let Some(active) = self.group_detail_idx {
                            if active > idx {
                                self.group_detail_idx = Some(active - 1);
                            }
                        }
                    }
                }
                if let Some(idx) = settings_idx {
                    self.group_settings_idx = Some(idx);
                    self.group_detail_idx = Some(idx);
                    if idx < self.groups.len() {
                        self.group_rename_input = self.groups[idx].name.clone();
                    }
                    self.group_settings_invite_mode = false;
                    self.group_settings_selected_members = Vec::new();
                    self.group_view = GroupView::Settings;
                }
                if let Some(idx) = open_idx {
                    self.group_detail_idx = Some(idx);
                    self.group_view = GroupView::Detail;
                }
                if go_create {
                    self.group_view = GroupView::Create;
                    self.group_create_name.clear();
                    self.group_selected_members = vec![false; self.contacts.len()];
                }
            }
            GroupView::Create => self.draw_group_create(ui),
            GroupView::Connecting => self.draw_group_connecting(ui),
            GroupView::InCall => self.draw_group_call(ui),
        }
    }

    fn draw_group_sidebar(
        &mut self,
        ui: &mut egui::Ui,
        open_idx: &mut Option<usize>,
        delete_idx: &mut Option<usize>,
        settings_idx: &mut Option<usize>,
        go_create: &mut bool,
    ) {
        ui.add_space(6.0);

        // "+ Create Group" button at full width
        let btn_w = ui.available_width() - 8.0;
        ui.horizontal(|ui| {
            ui.add_space(4.0);
            if ui.add_sized(
                egui::vec2(btn_w, 28.0),
                egui::Button::new(egui::RichText::new("+ Create Group").strong().size(12.0)),
            ).clicked() {
                *go_create = true;
            }
        });

        ui.add_space(4.0);

        if self.groups.is_empty() {
            ui.add_space(20.0);
            ui.vertical_centered(|ui| {
                ui.label(
                    egui::RichText::new("No groups yet")
                        .size(12.0)
                        .color(self.settings.theme.text_muted()),
                );
            });
            return;
        }

        let active_idx = self.group_detail_idx;

        let max_height = ui.available_height().max(80.0);
        egui::ScrollArea::vertical()
            .max_height(max_height)
            .id_salt("groups_sidebar_scroll")
            .show(ui, |ui| {
                for (idx, grp) in self.groups.iter().enumerate() {
                    let is_active = active_idx == Some(idx) && self.group_view == GroupView::Detail;

                    // Full-width highlight between separators
                    let row_width = ui.available_width();
                    let (row_rect, row_resp) = ui.allocate_exact_size(
                        egui::vec2(row_width, 28.0),
                        egui::Sense::click(),
                    );

                    if is_active {
                        ui.painter().rect_filled(row_rect, 0.0, self.settings.theme.widget_bg());
                    } else if row_resp.hovered() {
                        ui.painter().rect_filled(
                            row_rect, 0.0,
                            self.settings.theme.widget_bg().gamma_multiply(0.5),
                        );
                    }

                    // Draw group name centered vertically in the row (leave room for ⋮ button)
                    let dots_w = 20.0;
                    let text_max_w = row_rect.width() - 8.0 - dots_w - 4.0;
                    let text_pos = egui::pos2(row_rect.min.x + 8.0, row_rect.center().y - 7.0);
                    let name_galley = ui.painter().layout(
                        grp.name.clone(),
                        egui::FontId::proportional(12.0),
                        self.settings.theme.text_primary(),
                        text_max_w,
                    );
                    ui.painter().galley(
                        text_pos,
                        name_galley,
                        self.settings.theme.text_primary(),
                    );

                    // ⋮ button (3 vertical dots)
                    let dots_rect = egui::Rect::from_min_size(
                        egui::pos2(row_rect.max.x - dots_w - 2.0, row_rect.min.y),
                        egui::vec2(dots_w, row_rect.height()),
                    );
                    let dots_resp = ui.allocate_rect(dots_rect, egui::Sense::click());
                    let dots_center = dots_rect.center();
                    let dot_color = if dots_resp.hovered() {
                        self.settings.theme.text_primary()
                    } else {
                        self.settings.theme.text_muted()
                    };
                    for dy in [-4.0_f32, 0.0, 4.0] {
                        ui.painter().circle_filled(
                            egui::pos2(dots_center.x, dots_center.y + dy),
                            2.0,
                            dot_color,
                        );
                    }
                    if dots_resp.clicked() {
                        *settings_idx = Some(idx);
                    }

                    // Click on rest of row → open Detail (chat)
                    if row_resp.clicked() {
                        *open_idx = Some(idx);
                    }

                    // Right-click context menu
                    row_resp.context_menu(|ui| {
                        if ui.button("Settings").clicked() {
                            *settings_idx = Some(idx);
                            ui.close_menu();
                        }
                        if ui.button("Delete").clicked() {
                            *delete_idx = Some(idx);
                            ui.close_menu();
                        }
                    });

                    ui.separator();
                }
            });
    }

    fn draw_group_create(&mut self, ui: &mut egui::Ui) {
        ui.add_space(8.0);

        ui.horizontal(|ui| {
            if ui.button("<- Back").clicked() {
                self.group_view = GroupView::List;
            }
            ui.heading("Create Group");
        });

        ui.separator();
        ui.add_space(8.0);

        ui.horizontal(|ui| {
            ui.label("Group name:");
            let te = egui::TextEdit::singleline(&mut self.group_create_name)
                .hint_text("Enter group name…")
                .frame(true);
            ui.add(te);
        });

        ui.add_space(12.0);
        ui.label(egui::RichText::new("Select members:").strong());
        ui.add_space(4.0);

        // Ensure selected_members vec matches contacts length
        if self.group_selected_members.len() != self.contacts.len() {
            self.group_selected_members = vec![false; self.contacts.len()];
        }

        egui::ScrollArea::vertical().max_height(300.0).show(ui, |ui| {
            for (i, contact) in self.contacts.iter().enumerate() {
                ui.horizontal(|ui| {
                    ui.checkbox(&mut self.group_selected_members[i], "");
                    ui.label(&contact.nickname);
                    ui.label(
                        egui::RichText::new(&contact.fingerprint)
                            .size(11.0)
                            .color(self.settings.theme.text_muted()),
                    );
                });
            }
        });

        ui.add_space(12.0);

        let selected_count = self.group_selected_members.iter().filter(|&&s| s).count();
        let name_valid = !self.group_create_name.trim().is_empty();
        let can_create = name_valid && selected_count >= 1;

        ui.horizontal(|ui| {
            let create_btn = egui::Button::new(
                egui::RichText::new(format!("Create ({} members)", selected_count + 1))
                    .strong(),
            );
            if ui.add_enabled(can_create, create_btn).clicked() {
                self.create_group();
            }
            if !name_valid {
                ui.label(
                    egui::RichText::new("Enter a group name")
                        .color(self.settings.theme.text_muted()),
                );
            }
        });
    }

    fn draw_group_detail(&mut self, ui: &mut egui::Ui) {
        let idx = match self.group_detail_idx {
            Some(i) if i < self.groups.len() => i,
            _ => {
                self.group_view = GroupView::List;
                return;
            }
        };

        let grp_name = self.groups[idx].name.clone();
        let grp_id = self.groups[idx].group_id.clone();
        let member_count = self.groups[idx].members.len();
        let members: Vec<GroupMember> = self.groups[idx].members.clone();
        let my_pubkey = self.identity.pubkey;
        let is_admin = members.iter().any(|m| m.pubkey == my_pubkey && m.is_admin);
        let identity_secret = self.identity.secret;

        let mut start_call = false;

        // ── Top bar: Group name + Call button ──
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.heading(&grp_name);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let label = if is_admin { "Start Call (Leader)" } else { "Join Call" };
                let call_btn = egui::Button::new(
                    egui::RichText::new(label).strong().color(egui::Color32::WHITE),
                ).fill(self.settings.theme.btn_positive());
                if ui.add(call_btn).clicked() {
                    start_call = true;
                }
            });
        });
        ui.separator();

        // ── 2-column layout: Chat (left) | Members sidebar (right) ──
        let available = ui.available_rect_before_wrap();
        let clip = ui.clip_rect();
        let sep_w = 1.0;
        let members_w = 180.0_f32.max(available.width() * 0.22).min(240.0);
        let chat_w = (available.width() - members_w - sep_w - 4.0).max(100.0);

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
        ui.allocate_new_ui(egui::UiBuilder::new().max_rect(chat_rect), |ui| {
            ui.add_space(6.0);
            ui.label(egui::RichText::new("Chat").strong().size(13.0));
            ui.add_space(4.0);

            let history = GroupChatHistory::load(&grp_id, &identity_secret);
            let input_h = 54.0; // bar_h(38) + frame margins(12) + spacing
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
                            // Continuation — just the text, indented
                            ui.horizontal_wrapped(|ui| {
                                ui.add_space(avatar_size + spacing);
                                ui.add(egui::Label::new(&msg.text).wrap());
                            });
                            continue;
                        }

                        ui.add_space(3.0);

                        // Row 1: [avatar] Name   HH:MM
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
                                            ui.ctx(),
                                            "own_avatar",
                                            &bytes,
                                            96,
                                        );
                                    }
                                }
                                if let Some(tex) = &self.own_avatar_texture {
                                    let uv = egui::Rect::from_min_max(
                                        egui::pos2(0.0, 0.0),
                                        egui::pos2(1.0, 1.0),
                                    );
                                    ui.painter().image(
                                        tex.id(),
                                        av_rect,
                                        uv,
                                        egui::Color32::WHITE,
                                    );
                                    drew_avatar = true;
                                }
                            } else if let Some(contact_id) =
                                fp_to_cid.get(msg.sender_fingerprint.as_str())
                            {
                                if !self.contact_avatar_textures.contains_key(contact_id) {
                                    if let Some(bytes) =
                                        avatar::load_contact_avatar(contact_id)
                                    {
                                        if let Some(tex) = load_avatar_texture(
                                            ui.ctx(),
                                            &format!(
                                                "grp_av_{}",
                                                &contact_id[..8.min(contact_id.len())]
                                            ),
                                            &bytes,
                                            32,
                                        ) {
                                            self.contact_avatar_textures
                                                .insert(contact_id.clone(), tex);
                                        }
                                    }
                                }
                                if let Some(tex) =
                                    self.contact_avatar_textures.get(contact_id)
                                {
                                    let uv = egui::Rect::from_min_max(
                                        egui::pos2(0.0, 0.0),
                                        egui::pos2(1.0, 1.0),
                                    );
                                    ui.painter().image(
                                        tex.id(),
                                        av_rect,
                                        uv,
                                        egui::Color32::WHITE,
                                    );
                                    drew_avatar = true;
                                }
                            }

                            if !drew_avatar {
                                paint_initial_avatar(
                                    ui.painter(),
                                    av_rect,
                                    &msg.sender_nickname,
                                    &self.settings.theme,
                                );
                            }

                            ui.label(
                                egui::RichText::new(&msg.sender_nickname)
                                    .strong()
                                    .color(self.settings.theme.btn_primary()),
                            );
                            ui.label(
                                egui::RichText::new(ChatHistory::format_time(
                                    msg.timestamp,
                                ))
                                .size(11.0)
                                .color(self.settings.theme.text_muted()),
                            );
                        });

                        // Row 2: indented message text (wraps long lines)
                        ui.horizontal_wrapped(|ui| {
                            ui.add_space(avatar_size + spacing);
                            ui.add(egui::Label::new(&msg.text).wrap());
                        });

                        ui.add_space(2.0);
                    }
                });

            // Chat input bar at bottom — matching messages.rs style
            let bar_h = 38.0;
            let bar_frame = egui::Frame::none()
                .fill(self.settings.theme.sidebar_bg())
                .inner_margin(egui::Margin::symmetric(6.0, 6.0))
                .rounding(4.0);
            bar_frame.show(ui, |ui| {
                ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                    // Attach file button
                    let attach_btn = egui::Button::new(
                        egui::RichText::new("+").size(18.0).strong(),
                    ).min_size(egui::vec2(bar_h, bar_h));
                    ui.add(attach_btn).on_hover_text("Send file");

                    // TextEdit with always-visible outline and distinct bg
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
        });

        // ── Right sidebar: Members ──
        let color_even = self.settings.theme.panel_bg();
        let color_odd = self.settings.theme.sidebar_bg();

        ui.allocate_new_ui(egui::UiBuilder::new().max_rect(members_rect), |ui| {
            // Header bar
            let header_w = ui.available_width();
            let header_rect = ui.allocate_space(egui::vec2(header_w, 28.0)).1;
            ui.painter().rect_filled(header_rect, 0.0, self.settings.theme.sidebar_bg());
            ui.painter().text(
                egui::pos2(header_rect.min.x + 8.0, header_rect.center().y),
                egui::Align2::LEFT_CENTER,
                format!("Members ({})", member_count),
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

            // Member rows — alternating colors, Nickname + Role only
            egui::ScrollArea::vertical()
                .id_salt("detail_members")
                .show(ui, |ui| {
                    let row_w = ui.available_width();
                    for (i, member) in members.iter().enumerate() {
                        let bg = if i % 2 == 0 { color_even } else { color_odd };
                        let row_rect = ui.allocate_space(egui::vec2(row_w, 26.0)).1;
                        ui.painter().rect_filled(row_rect, 0.0, bg);

                        let mut x = row_rect.min.x + 8.0;
                        let cy = row_rect.center().y;

                        // Nickname
                        let nick_galley = ui.painter().layout_no_wrap(
                            member.nickname.clone(),
                            egui::FontId::proportional(13.0),
                            self.settings.theme.text_primary(),
                        );
                        ui.painter().galley(
                            egui::pos2(x, cy - nick_galley.size().y / 2.0),
                            nick_galley.clone(),
                            self.settings.theme.text_primary(),
                        );
                        x += nick_galley.size().x + 6.0;

                        // Role badge
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

                        // (you)
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
        if start_call {
            self.start_group_call(is_admin);
        }
        if send_detail_chat {
            let text = self.group_detail_chat_input.trim().to_string();
            if !text.is_empty() {
                // Save to local history
                let my_nickname = self.settings.nickname.clone();
                let my_fingerprint = self.identity.fingerprint.clone();
                {
                    let mut history = GroupChatHistory::load(&grp_id, &self.identity.secret);
                    history.add_message(my_fingerprint, my_nickname.clone(), text.clone());
                }
                // Send to all other group members via messaging daemon
                if let Some(tx) = &self.msg_cmd_tx {
                    for member in &members {
                        if member.pubkey == my_pubkey {
                            continue;
                        }
                        if member.address.is_empty() || member.port.is_empty() {
                            continue;
                        }
                        let addr_str = format!("[{}]:{}", member.address, member.port);
                        if let Ok(addr) = addr_str.parse() {
                            let contact_id = identity::derive_contact_id(&my_pubkey, &member.pubkey);
                            tx.send(crate::messaging::MsgCommand::SendGroupChat {
                                contact_id,
                                peer_addr: addr,
                                peer_pubkey: member.pubkey,
                                group_id: grp_id.clone(),
                                text: text.clone(),
                            }).ok();
                        }
                    }
                }
            }
            self.group_detail_chat_input.clear();
        }
    }

    fn draw_group_connecting(&mut self, ui: &mut egui::Ui) {
        ui.add_space(60.0);
        ui.vertical_centered(|ui| {
            ui.spinner();
            ui.add_space(12.0);
            ui.label(
                egui::RichText::new("Connecting to group call...")
                    .size(16.0),
            );
            ui.add_space(20.0);
            let cancel_btn = egui::Button::new(
                egui::RichText::new("Cancel")
                    .color(self.settings.theme.btn_negative()),
            );
            if ui.add(cancel_btn).clicked() {
                self.cleanup_group_call();
            }
        });
    }

    fn draw_group_call(&mut self, ui: &mut egui::Ui) {
        let group_name = self.group_call_group.as_ref()
            .map(|g| g.name.clone())
            .unwrap_or_else(|| "Group".to_string());
        let role = self.group_call_role.unwrap_or(GroupRole::Member);
        let member_count = self.group_call_members.len();

        // Top bar
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.heading(&group_name);
            ui.add_space(8.0);
            ui.label(
                egui::RichText::new(format!("{} members", member_count))
                    .size(12.0)
                    .color(self.settings.theme.text_muted()),
            );
            ui.label(
                egui::RichText::new("ENCRYPTED")
                    .size(10.0)
                    .strong()
                    .color(self.settings.theme.btn_positive()),
            );
            ui.label(
                egui::RichText::new(if role == GroupRole::Leader { "LEADER" } else { "MEMBER" })
                    .size(10.0)
                    .color(self.settings.theme.btn_primary()),
            );
        });

        ui.separator();

        // Members panel
        ui.add_space(4.0);
        ui.label(egui::RichText::new("Members").strong().size(13.0));

        let my_pubkey = self.identity.pubkey;
        egui::ScrollArea::vertical()
            .max_height(120.0)
            .id_salt("grp_call_members")
            .show(ui, |ui| {
                for member in &self.group_call_members {
                    let frame = egui::Frame::none()
                        .fill(self.settings.theme.panel_bg())
                        .rounding(egui::Rounding::same(4.0))
                        .inner_margin(egui::Margin::same(6.0))
                        .outer_margin(egui::Margin::symmetric(0.0, 1.0));

                    frame.show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(egui::RichText::new(&member.nickname).strong());
                            ui.label(
                                egui::RichText::new(&member.fingerprint)
                                    .size(11.0)
                                    .color(self.settings.theme.text_muted()),
                            );
                            if member.is_admin {
                                ui.label(
                                    egui::RichText::new("admin")
                                        .size(10.0)
                                        .color(self.settings.theme.btn_primary()),
                                );
                            }
                            if member.pubkey == my_pubkey {
                                ui.label(
                                    egui::RichText::new("(you)")
                                        .size(10.0)
                                        .color(self.settings.theme.text_muted()),
                                );
                            }
                        });
                    });
                }
            });

        ui.separator();

        // Chat area
        ui.add_space(4.0);
        ui.label(egui::RichText::new("Chat").strong().size(13.0));

        // Build nickname → contact_id map for avatar lookups
        let nick_to_cid: std::collections::HashMap<&str, String> = self
            .group_call_members
            .iter()
            .map(|m| {
                (
                    m.nickname.as_str(),
                    identity::derive_contact_id(&my_pubkey, &m.pubkey),
                )
            })
            .collect();

        let avail = ui.available_height() - 70.0;
        egui::ScrollArea::vertical()
            .max_height(avail.max(80.0))
            .stick_to_bottom(true)
            .id_salt("grp_call_chat")
            .show(ui, |ui| {
                if self.group_call_messages.is_empty() {
                    ui.label(
                        egui::RichText::new("No messages yet")
                            .color(self.settings.theme.text_muted()),
                    );
                }

                let avatar_size = 28.0;
                let spacing = ui.spacing().item_spacing.x;
                let mut prev_call_sender: Option<&str> = None;

                for msg in &self.group_call_messages {
                    let is_own = msg.sender_nickname == self.settings.nickname;
                    let same_sender = prev_call_sender == Some(msg.sender_nickname.as_str());
                    prev_call_sender = Some(msg.sender_nickname.as_str());

                    if same_sender {
                        // Continuation — just the text, indented
                        ui.horizontal_wrapped(|ui| {
                            ui.add_space(avatar_size + spacing);
                            ui.add(egui::Label::new(&msg.text).wrap());
                        });
                        continue;
                    }

                    ui.add_space(3.0);

                    // Row 1: [avatar] Name
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
                                        ui.ctx(),
                                        "own_avatar",
                                        &bytes,
                                        96,
                                    );
                                }
                            }
                            if let Some(tex) = &self.own_avatar_texture {
                                let uv = egui::Rect::from_min_max(
                                    egui::pos2(0.0, 0.0),
                                    egui::pos2(1.0, 1.0),
                                );
                                ui.painter().image(
                                    tex.id(),
                                    av_rect,
                                    uv,
                                    egui::Color32::WHITE,
                                );
                                drew_avatar = true;
                            }
                        } else if let Some(contact_id) =
                            nick_to_cid.get(msg.sender_nickname.as_str())
                        {
                            if !self.contact_avatar_textures.contains_key(contact_id) {
                                if let Some(bytes) =
                                    avatar::load_contact_avatar(contact_id)
                                {
                                    if let Some(tex) = load_avatar_texture(
                                        ui.ctx(),
                                        &format!(
                                            "grp_call_av_{}",
                                            &contact_id[..8.min(contact_id.len())]
                                        ),
                                        &bytes,
                                        32,
                                    ) {
                                        self.contact_avatar_textures
                                            .insert(contact_id.clone(), tex);
                                    }
                                }
                            }
                            if let Some(tex) =
                                self.contact_avatar_textures.get(contact_id)
                            {
                                let uv = egui::Rect::from_min_max(
                                    egui::pos2(0.0, 0.0),
                                    egui::pos2(1.0, 1.0),
                                );
                                ui.painter().image(
                                    tex.id(),
                                    av_rect,
                                    uv,
                                    egui::Color32::WHITE,
                                );
                                drew_avatar = true;
                            }
                        }

                        if !drew_avatar {
                            paint_initial_avatar(
                                ui.painter(),
                                av_rect,
                                &msg.sender_nickname,
                                &self.settings.theme,
                            );
                        }

                        ui.label(
                            egui::RichText::new(&msg.sender_nickname)
                                .strong()
                                .color(self.settings.theme.btn_primary()),
                        );
                    });

                    // Row 2: indented message text (wraps long lines)
                    ui.horizontal_wrapped(|ui| {
                        ui.add_space(avatar_size + spacing);
                        ui.add(egui::Label::new(&msg.text).wrap());
                    });

                    ui.add_space(2.0);
                }
            });

        // Chat input + send — matching messages.rs style
        let mut send_msg = false;
        let bar_h = 38.0;
        let bar_frame = egui::Frame::none()
            .fill(self.settings.theme.sidebar_bg())
            .inner_margin(egui::Margin::symmetric(6.0, 6.0))
            .rounding(4.0);
        bar_frame.show(ui, |ui| {
            ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                // Attach file button
                let attach_btn = egui::Button::new(
                    egui::RichText::new("+").size(18.0).strong(),
                ).min_size(egui::vec2(bar_h, bar_h));
                ui.add(attach_btn).on_hover_text("Send file");

                // TextEdit with always-visible outline and distinct bg
                let outline = self.settings.theme.text_muted();
                ui.visuals_mut().widgets.inactive.bg_stroke = egui::Stroke::new(1.0, outline);
                ui.visuals_mut().widgets.inactive.bg_fill = self.settings.theme.panel_bg();

                let resp = ui.add_sized(
                    egui::vec2(ui.available_width() - 75.0, bar_h),
                    egui::TextEdit::singleline(&mut self.group_call_chat_input)
                        .hint_text("Type a message...")
                        .margin(egui::vec2(8.0, 10.0)),
                );
                let enter = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                if ui.add(egui::Button::new("Send").min_size(egui::vec2(60.0, bar_h))).clicked() || enter {
                    send_msg = true;
                    resp.request_focus();
                }
            });
        });

        if send_msg {
            let text = self.group_call_chat_input.trim().to_string();
            if !text.is_empty() {
                if let Some(tx) = &self.group_call_chat_tx {
                    tx.send(text.clone()).ok();
                }
                let my_nickname = self.settings.nickname.clone();
                let my_fingerprint = self.identity.fingerprint.clone();
                // Persist to group chat history
                if let Some(ref mut hist) = self.group_chat_history {
                    hist.add_message(
                        my_fingerprint,
                        my_nickname.clone(),
                        text.clone(),
                    );
                }
                self.group_call_messages.push(GroupChatMsg {
                    sender_index: 0,
                    sender_nickname: my_nickname,
                    text,
                });
            }
            self.group_call_chat_input.clear();
        }

        ui.add_space(4.0);

        // Controls bar
        ui.horizontal(|ui| {
            let mic_on = self.group_call_mic.load(Ordering::Relaxed);
            let mic_text = if mic_on { "Mute" } else { "Unmute" };
            if ui.button(mic_text).clicked() {
                self.group_call_mic.store(!mic_on, Ordering::Relaxed);
            }

            ui.add_space(12.0);

            let hangup_btn = egui::Button::new(
                egui::RichText::new("Hang Up")
                    .strong()
                    .color(self.settings.theme.btn_negative()),
            );
            if ui.add(hangup_btn).clicked() {
                self.cleanup_group_call();
            }
        });
    }

    fn create_group(&mut self) {
        let group_key = group::generate_group_key();
        let group_id = group::generate_group_id();
        let now = identity::now_timestamp();

        // Add ourselves as member 0 (admin)
        let mut members = vec![GroupMember {
            pubkey: self.identity.pubkey,
            nickname: self.settings.nickname.clone(),
            fingerprint: self.identity.fingerprint.clone(),
            sender_index: 0,
            address: self.best_ipv6.clone(),
            port: self.local_port.clone(),
            is_admin: true,
        }];

        // Add selected contacts
        let mut next_index: u16 = 1;
        for (i, contact) in self.contacts.iter().enumerate() {
            if self.group_selected_members.get(i).copied().unwrap_or(false) {
                members.push(GroupMember {
                    pubkey: contact.pubkey,
                    nickname: contact.nickname.clone(),
                    fingerprint: contact.fingerprint.clone(),
                    sender_index: next_index,
                    address: contact.last_address.clone(),
                    port: contact.last_port.clone(),
                    is_admin: false,
                });
                next_index += 1;
            }
        }

        let grp = Group {
            group_id,
            name: self.group_create_name.trim().to_string(),
            created_by: self.identity.pubkey,
            created_at: now,
            members,
            group_key,
            next_sender_index: next_index,
            avatar_sha256: None,
        };

        group::save_group(&grp);

        // Send invite to each member via messaging daemon
        if let Ok(group_json) = serde_json::to_vec(&grp) {
            if let Some(tx) = &self.msg_cmd_tx {
                for member in &grp.members {
                    // Skip ourselves
                    if member.pubkey == self.identity.pubkey {
                        continue;
                    }
                    // Find contact to get address info
                    if let Some(contact) = self.contacts.iter().find(|c| c.pubkey == member.pubkey) {
                        if !contact.last_address.is_empty() && !contact.last_port.is_empty() {
                            let addr_str = format!("[{}]:{}", contact.last_address, contact.last_port);
                            if let Ok(addr) = addr_str.parse() {
                                tx.send(crate::messaging::MsgCommand::SendGroupInvite {
                                    contact_id: contact.contact_id.clone(),
                                    peer_addr: addr,
                                    peer_pubkey: contact.pubkey,
                                    group_json: group_json.clone(),
                                }).ok();
                            }
                        }
                    }
                }
            }
        }

        self.groups.push(grp);
        self.group_view = GroupView::List;
        self.group_create_name.clear();
    }

    fn draw_group_settings(&mut self, ui: &mut egui::Ui) {
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
                        let kicked_pubkey = self.groups[idx].members[member_idx].pubkey;
                        group::remove_member(&mut self.groups[idx], &kicked_pubkey);
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

    /// Broadcast a group metadata update to all members of a group.
    pub(crate) fn broadcast_group_update(&self, group_idx: usize) {
        if group_idx >= self.groups.len() {
            return;
        }
        let grp = &self.groups[group_idx];
        let group_json = match serde_json::to_vec(grp) {
            Ok(j) => j,
            Err(_) => return,
        };
        let member_contacts = self.group_member_contacts(group_idx);
        if let Some(tx) = &self.msg_cmd_tx {
            tx.send(crate::messaging::MsgCommand::SendGroupUpdate {
                group_id: grp.group_id.clone(),
                group_json,
                member_contacts,
            }).ok();
        }
    }

    /// Broadcast a group avatar to all members of a group.
    pub(crate) fn broadcast_group_avatar(&self, group_idx: usize, avatar_data: Vec<u8>, sha256: [u8; 32]) {
        if group_idx >= self.groups.len() {
            return;
        }
        let grp = &self.groups[group_idx];
        let member_contacts = self.group_member_contacts(group_idx);
        if let Some(tx) = &self.msg_cmd_tx {
            tx.send(crate::messaging::MsgCommand::SendGroupAvatar {
                group_id: grp.group_id.clone(),
                avatar_data,
                sha256,
                member_contacts,
            }).ok();
        }
    }

    /// Build the (contact_id, addr, pubkey) list for all members of a group, excluding ourselves.
    fn group_member_contacts(&self, group_idx: usize) -> Vec<(String, std::net::SocketAddr, [u8; 32])> {
        let grp = &self.groups[group_idx];
        let my_pubkey = self.identity.pubkey;
        let mut result = Vec::new();
        for member in &grp.members {
            if member.pubkey == my_pubkey {
                continue;
            }
            if member.address.is_empty() || member.port.is_empty() {
                continue;
            }
            let addr_str = format!("[{}]:{}", member.address, member.port);
            if let Ok(addr) = addr_str.parse() {
                let contact_id = identity::derive_contact_id(&my_pubkey, &member.pubkey);
                result.push((contact_id, addr, member.pubkey));
            }
        }
        result
    }

    /// Invite selected contacts to an existing group.
    fn invite_members_to_group(&mut self, group_idx: usize) {
        if group_idx >= self.groups.len() {
            return;
        }
        let my_pubkey = self.identity.pubkey;
        let existing_pubkeys: Vec<[u8; 32]> = self.groups[group_idx].members.iter().map(|m| m.pubkey).collect();

        // Collect contacts to invite
        let mut new_members = Vec::new();
        for (ci, contact) in self.contacts.iter().enumerate() {
            if self.group_settings_selected_members.get(ci).copied().unwrap_or(false)
                && !existing_pubkeys.contains(&contact.pubkey)
            {
                let next_idx = self.groups[group_idx].next_sender_index;
                new_members.push((ci, GroupMember {
                    pubkey: contact.pubkey,
                    nickname: contact.nickname.clone(),
                    fingerprint: contact.fingerprint.clone(),
                    sender_index: next_idx,
                    address: contact.last_address.clone(),
                    port: contact.last_port.clone(),
                    is_admin: false,
                }));
                self.groups[group_idx].next_sender_index += 1;
            }
        }

        // Add new members to the group
        for (_, member) in &new_members {
            self.groups[group_idx].members.push(member.clone());
        }
        group::save_group(&self.groups[group_idx]);

        // Send invites to new members
        if let Ok(group_json) = serde_json::to_vec(&self.groups[group_idx]) {
            if let Some(tx) = &self.msg_cmd_tx {
                for (_, member) in &new_members {
                    if member.address.is_empty() || member.port.is_empty() {
                        continue;
                    }
                    let addr_str = format!("[{}]:{}", member.address, member.port);
                    if let Ok(addr) = addr_str.parse() {
                        let contact_id = identity::derive_contact_id(&my_pubkey, &member.pubkey);
                        tx.send(crate::messaging::MsgCommand::SendGroupInvite {
                            contact_id,
                            peer_addr: addr,
                            peer_pubkey: member.pubkey,
                            group_json: group_json.clone(),
                        }).ok();
                    }
                }
            }
        }

        // Broadcast update to existing members
        self.broadcast_group_update(group_idx);
    }
}

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

/// Paint a fallback avatar: colored circle with the first letter of the nickname.
fn paint_initial_avatar(
    painter: &egui::Painter,
    rect: egui::Rect,
    nickname: &str,
    _theme: &crate::theme::Theme,
) {
    // Deterministic color from nickname hash
    let hash = {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        nickname.hash(&mut hasher);
        hasher.finish()
    };
    let hue = (hash % 360) as f32;
    let r = ((hue * std::f32::consts::PI / 180.0).cos() * 40.0 + 110.0) as u8;
    let g = (((hue + 120.0) * std::f32::consts::PI / 180.0).cos() * 40.0 + 110.0) as u8;
    let b = (((hue + 240.0) * std::f32::consts::PI / 180.0).cos() * 40.0 + 110.0) as u8;
    let bg_color = egui::Color32::from_rgb(r, g, b);

    let center = rect.center();
    let radius = rect.width().min(rect.height()) / 2.0;
    painter.circle_filled(center, radius, bg_color);

    // Draw initial letter
    let initial = nickname
        .chars()
        .next()
        .map(|c| c.to_uppercase().to_string())
        .unwrap_or_else(|| "?".to_string());
    painter.text(
        center,
        egui::Align2::CENTER_CENTER,
        initial,
        egui::FontId::proportional(radius * 1.1),
        egui::Color32::WHITE,
    );
}
