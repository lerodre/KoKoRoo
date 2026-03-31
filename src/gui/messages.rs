use eframe::egui;
use std::net::SocketAddr;

use crate::avatar;
use crate::chat::{ChatHistory, FileTransferStatus};
use crate::identity;
use crate::messaging::MsgCommand;

use super::{HostelApp, FriendsSubTab, load_avatar_texture, peer_display_job, censor_ip};

const DEFAULT_AVATAR_PNG: &[u8] = include_bytes!("../../assets/default_avatar.png");

/// Number of messages to show initially and per "Load more" page.
const MSG_PAGE_SIZE: usize = 100;

impl HostelApp {
    pub(crate) fn draw_messages_tab(&mut self, ui: &mut egui::Ui) {
        let available = ui.available_rect_before_wrap();
        let clip = ui.clip_rect();
        let sep_w = 1.0;
        let list_w = 165.0_f32.min(available.width() * 0.28);
        let chat_w = (available.width() - list_w - sep_w - 4.0).max(100.0);

        // Full-height background for the contact list column (edge to edge)
        let bg_rect = egui::Rect::from_min_max(
            egui::pos2(clip.min.x, clip.min.y),
            egui::pos2(clip.min.x + list_w + (available.min.x - clip.min.x), clip.max.y),
        );
        ui.painter().rect_filled(bg_rect, 0.0, self.settings.theme.sidebar_bg());

        let line_stroke = egui::Stroke::new(sep_w, self.settings.theme.text_muted());

        // Separator between main sidebar and contact list (left edge)
        ui.painter().vline(clip.min.x, clip.y_range(), line_stroke);

        // Separator between contact list and chat (right edge)
        let sep_x = available.min.x + list_w + 1.0;
        ui.painter().vline(sep_x, clip.y_range(), line_stroke);

        let list_rect = egui::Rect::from_min_size(
            available.min,
            egui::vec2(list_w, available.height()),
        );
        let chat_rect = egui::Rect::from_min_size(
            egui::pos2(available.min.x + list_w + sep_w + 4.0, available.min.y),
            egui::vec2(chat_w, available.height()),
        );

        // Left panel: contact list
        let mut open_chat: Option<String> = None;
        ui.allocate_new_ui(egui::UiBuilder::new().max_rect(list_rect), |ui| {
            self.draw_message_list(ui, &mut open_chat);
        });

        // Right panel: conversation
        ui.allocate_new_ui(egui::UiBuilder::new().max_rect(chat_rect), |ui| {
            self.draw_message_conversation(ui);
        });

        if let Some(cid) = open_chat {
            self.open_msg_chat(&cid);
        }

        // Delete contact confirmation dialog
        let mut confirm_action = None;
        if let Some(idx) = self.msg_confirm_delete_contact {
            if idx < self.contacts.len() {
                let contact = &self.contacts[idx];
                let name = if contact.nickname.is_empty() {
                    &contact.fingerprint
                } else {
                    &contact.nickname
                };
                egui::Window::new("Delete contact")
                    .collapsible(false)
                    .resizable(false)
                    .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                    .show(ui.ctx(), |ui| {
                        ui.label(format!("Delete {} and all messages?", name));
                        ui.label("This will notify the peer and remove you from each other's contacts.");
                        ui.add_space(8.0);
                        ui.horizontal(|ui| {
                            if ui.button("Delete").clicked() {
                                confirm_action = Some(true);
                            }
                            if ui.button("Cancel").clicked() {
                                confirm_action = Some(false);
                            }
                        });
                    });
            } else {
                self.msg_confirm_delete_contact = None;
            }
        }
        if let Some(confirmed) = confirm_action {
            if confirmed {
                if let Some(idx) = self.msg_confirm_delete_contact {
                    if idx < self.contacts.len() {
                        let contact = &self.contacts[idx];
                        let contact_id = contact.contact_id.clone();
                        let peer_pubkey = contact.pubkey;

                        // Close chat if viewing this contact
                        if self.msg_active_chat.as_deref() == Some(&contact_id) {
                            self.msg_active_chat = None;
                        }

                        // Send delete command to daemon
                        if let Some(ref tx) = self.msg_cmd_tx {
                            tx.send(crate::messaging::MsgCommand::DeleteContact {
                                contact_id: contact_id.clone(),
                                peer_pubkey,
                            }).ok();
                        }

                        // Local GUI cleanup
                        self.msg_chat_histories.remove(&contact_id);
                        self.msg_peer_online.remove(&contact_id);
                        self.msg_peer_presence.remove(&contact_id);
                        self.msg_unread.remove(&contact_id);
                        self.contact_avatar_textures.remove(&contact_id);
                        self.contacts = identity::load_all_contacts();
                    }
                }
            }
            self.msg_confirm_delete_contact = None;
        }
    }

    fn draw_message_list(&mut self, ui: &mut egui::Ui, open_chat: &mut Option<String>) {
        ui.add_space(6.0);
        ui.heading("Messages");
        ui.add_space(4.0);

        // "Add Friend" row — first item
        {
            let row_width = ui.available_width();
            let row_height = 36.0;
            let is_active = self.show_add_friend;
            let incoming_count = self.req_incoming.len();

            let (row_rect, row_resp) = ui.allocate_exact_size(
                egui::vec2(row_width, row_height),
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

            let center_y = row_rect.center().y;
            let font = egui::FontId::proportional(16.0);
            let plus_galley = ui.painter().layout_no_wrap(
                "+".to_string(), font, self.settings.theme.accent(),
            );
            ui.painter().galley(
                egui::pos2(row_rect.min.x + 10.0, center_y - plus_galley.size().y / 2.0),
                plus_galley,
                self.settings.theme.accent(),
            );

            let label_font = egui::FontId::proportional(12.0);
            let label_galley = ui.painter().layout_no_wrap(
                "Add Friend".to_string(), label_font, self.settings.theme.text_primary(),
            );
            ui.painter().galley(
                egui::pos2(row_rect.min.x + 28.0, center_y - label_galley.size().y / 2.0),
                label_galley,
                self.settings.theme.text_primary(),
            );

            // Badge for incoming requests
            if incoming_count > 0 {
                let badge_color = self.settings.theme.btn_negative();
                let badge_text = format!("{}", incoming_count);
                let badge_font = egui::FontId::proportional(9.0);
                let badge_galley = ui.painter().layout_no_wrap(badge_text, badge_font, egui::Color32::WHITE);
                let bw = badge_galley.size().x;
                let bh = badge_galley.size().y;
                let radius = (bw / 2.0 + 4.0).max(8.0);
                let badge_center = egui::pos2(row_rect.max.x - radius - 6.0, center_y);
                ui.painter().circle_filled(badge_center, radius, badge_color);
                ui.painter().galley(
                    egui::pos2(badge_center.x - bw / 2.0, badge_center.y - bh / 2.0),
                    badge_galley,
                    egui::Color32::WHITE,
                );
            }

            if row_resp.clicked() {
                self.show_add_friend = !self.show_add_friend;
                self.msg_active_chat = None;
            }
            ui.separator();
        }

        // Build conversation list from ALL contacts (except blocked)
        let mut conversations: Vec<(String, String, String, bool, u32)> = Vec::new();

        for contact in &self.contacts {
            let hex = identity::pubkey_hex(&contact.pubkey);
            if self.settings.is_blocked(&hex) {
                continue;
            }

            let online = self.msg_peer_online.get(&contact.contact_id).copied().unwrap_or(false);
            let unread = self.msg_unread.get(&contact.contact_id).copied().unwrap_or(0);

            let preview = self.msg_chat_histories.get(&contact.contact_id)
                .and_then(|h| h.messages.last())
                .map(|m| {
                    let prefix = if m.from_me { "You: " } else { "" };
                    let text = if m.text.len() > 25 { &m.text[..25] } else { &m.text };
                    format!("{prefix}{text}")
                })
                .unwrap_or_default();

            let is_self = contact.pubkey == self.identity.pubkey;
            let name = if is_self {
                "YO (you)".to_string()
            } else if contact.nickname.is_empty() {
                contact.fingerprint.clone()
            } else {
                contact.nickname.clone()
            };

            conversations.push((contact.contact_id.clone(), name, preview, online, unread));
        }

        if conversations.is_empty() {
            ui.colored_label(self.settings.theme.text_muted(), "No contacts yet.");
            return;
        }

        // Sort: self first, then online, then by most recent message
        conversations.sort_by(|a, b| {
            let self_a = a.1 == "YO (you)";
            let self_b = b.1 == "YO (you)";
            if self_a != self_b { return self_b.cmp(&self_a); }
            let online_ord = b.3.cmp(&a.3);
            if online_ord != std::cmp::Ordering::Equal { return online_ord; }
            let ts_a = self.msg_chat_histories.get(&a.0)
                .and_then(|h| h.messages.last().map(|m| m.timestamp))
                .unwrap_or(0);
            let ts_b = self.msg_chat_histories.get(&b.0)
                .and_then(|h| h.messages.last().map(|m| m.timestamp))
                .unwrap_or(0);
            ts_b.cmp(&ts_a)
        });

        let active_cid = self.msg_active_chat.clone();

        let max_height = ui.available_height().max(80.0);
        egui::ScrollArea::vertical()
            .max_height(max_height)
            .id_salt("messages_list_scroll")
            .show(ui, |ui| {
                for (contact_id, name, _preview, online, unread) in &conversations {
                    let is_active = active_cid.as_deref() == Some(contact_id.as_str());

                    // Full-width highlight between separators
                    let row_width = ui.available_width();
                    let row_height = 40.0;
                    let (row_rect, row_resp) = ui.allocate_exact_size(
                        egui::vec2(row_width, row_height),
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

                    // Paint content on top of the highlight
                    let mut cursor_x = row_rect.min.x + 4.0;
                    let center_y = row_rect.center().y;

                    // Presence indicator
                    let presence = self.msg_peer_presence.get(contact_id)
                        .copied()
                        .unwrap_or(if *online {
                            crate::messaging::PresenceStatus::Online
                        } else {
                            crate::messaging::PresenceStatus::Offline
                        });
                    let color = match presence {
                        crate::messaging::PresenceStatus::Online => egui::Color32::from_rgb(0x4C, 0xAF, 0x50),
                        crate::messaging::PresenceStatus::Away => egui::Color32::from_rgb(0xFF, 0xC1, 0x07),
                        crate::messaging::PresenceStatus::Offline => self.settings.theme.text_muted(),
                    };
                    ui.painter().circle_filled(egui::pos2(cursor_x + 4.0, center_y), 4.0, color);
                    cursor_x += 12.0;

                    // Avatar (32px circle)
                    let avatar_size = 32.0;
                    let is_self = name == "YO (you)";
                    let avatar_tex = if is_self {
                        if self.own_avatar_texture.is_none() {
                            if let Some(bytes) = avatar::load_own_avatar() {
                                self.own_avatar_texture = load_avatar_texture(
                                    ui.ctx(), "own_avatar", &bytes, 96,
                                );
                            }
                        }
                        self.own_avatar_texture.as_ref()
                    } else {
                        if !self.contact_avatar_textures.contains_key(contact_id) {
                            if let Some(bytes) = avatar::load_contact_avatar(contact_id) {
                                if let Some(tex) = load_avatar_texture(
                                    ui.ctx(),
                                    &format!("contact_avatar_{}", &contact_id[..8.min(contact_id.len())]),
                                    &bytes, 32,
                                ) {
                                    self.contact_avatar_textures.insert(contact_id.clone(), tex);
                                }
                            }
                        }
                        self.contact_avatar_textures.get(contact_id)
                    };

                    let av_rect = egui::Rect::from_center_size(
                        egui::pos2(cursor_x + avatar_size / 2.0, center_y),
                        egui::vec2(avatar_size, avatar_size),
                    );
                    if let Some(tex) = avatar_tex {
                        let uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
                        ui.painter().image(tex.id(), av_rect, uv, egui::Color32::WHITE);
                    } else {
                        if self.default_avatar_texture.is_none() {
                            self.default_avatar_texture = load_avatar_texture(
                                ui.ctx(), "default_avatar", DEFAULT_AVATAR_PNG, 96,
                            );
                        }
                        if let Some(tex) = &self.default_avatar_texture {
                            let uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
                            ui.painter().image(tex.id(), av_rect, uv, egui::Color32::from_white_alpha(160));
                        }
                    }
                    cursor_x += avatar_size + 6.0;

                    // Name text
                    let text_color = self.settings.theme.text_primary();
                    let font = if *unread > 0 {
                        egui::FontId::proportional(12.0)
                    } else {
                        egui::FontId::proportional(12.0)
                    };
                    let name_galley = ui.painter().layout_no_wrap(
                        name.clone(), font, text_color,
                    );
                    let name_pos = egui::pos2(cursor_x, center_y - name_galley.size().y / 2.0);
                    ui.painter().galley(name_pos, name_galley, text_color);

                    // Unread badge
                    if *unread > 0 {
                        let badge_color = self.settings.theme.btn_negative();
                        let badge_text = format!("{}", unread);
                        let badge_font = egui::FontId::proportional(9.0);
                        let badge_galley = ui.painter().layout_no_wrap(badge_text, badge_font, egui::Color32::WHITE);
                        let bw = badge_galley.size().x;
                        let bh = badge_galley.size().y;
                        let radius = (bw / 2.0 + 4.0).max(8.0);
                        let badge_center = egui::pos2(row_rect.max.x - radius - 6.0, center_y);
                        ui.painter().circle_filled(badge_center, radius, badge_color);
                        ui.painter().galley(
                            egui::pos2(badge_center.x - bw / 2.0, badge_center.y - bh / 2.0),
                            badge_galley,
                            egui::Color32::WHITE,
                        );
                    }

                    if row_resp.clicked() {
                        *open_chat = Some(contact_id.clone());
                        self.show_add_friend = false;
                    }

                    // Right-click context menu
                    if !is_self {
                        row_resp.context_menu(|ui| {
                            if ui.button("Delete contact").clicked() {
                                // Find index in self.contacts by contact_id
                                if let Some(idx) = self.contacts.iter().position(|c| c.contact_id == *contact_id) {
                                    self.msg_confirm_delete_contact = Some(idx);
                                }
                                ui.close_menu();
                            }
                        });
                    }

                    ui.separator();
                }
            });
    }

    fn draw_message_conversation(&mut self, ui: &mut egui::Ui) {
        // Show "Add Friend" panel when active
        if self.show_add_friend {
            self.draw_add_friend_panel(ui);
            return;
        }

        let contact_id = match &self.msg_active_chat {
            Some(cid) => cid.clone(),
            None => {
                ui.add_space(40.0);
                ui.vertical_centered(|ui| {
                    ui.colored_label(
                        self.settings.theme.text_muted(),
                        "Select a contact to start chatting",
                    );
                });
                return;
            }
        };

        // ── Drag & drop file handling ──
        let dropped_files: Vec<egui::DroppedFile> = ui.ctx().input(|i| i.raw.dropped_files.clone());
        let has_hovered_files = ui.ctx().input(|i| !i.raw.hovered_files.is_empty());

        if !dropped_files.is_empty() {
            if let Some(contact) = self.contacts.iter().find(|c| c.contact_id == contact_id).cloned() {
                for file in &dropped_files {
                    if let Some(path) = &file.path {
                        if path.is_file() {
                            self.send_file_to_contact(&contact_id, &contact, path);
                        }
                    }
                }
            }
        }

        // Find contact info
        let contact = self.contacts.iter().find(|c| c.contact_id == contact_id).cloned();
        let peer_name = contact.as_ref()
            .map(|c| if c.nickname.is_empty() { c.fingerprint.clone() } else { c.nickname.clone() })
            .unwrap_or_else(|| "Unknown".to_string());
        let online = self.msg_peer_online.get(&contact_id).copied().unwrap_or(false);

        // Header
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.heading(&peer_name);
            let presence = self.msg_peer_presence.get(&contact_id)
                .copied()
                .unwrap_or(if online {
                    crate::messaging::PresenceStatus::Online
                } else {
                    crate::messaging::PresenceStatus::Offline
                });
            let (status_color, status_text) = match presence {
                crate::messaging::PresenceStatus::Online => (egui::Color32::from_rgb(0x4C, 0xAF, 0x50), "online"),
                crate::messaging::PresenceStatus::Away => (egui::Color32::from_rgb(0xFF, 0xC1, 0x07), "away"),
                crate::messaging::PresenceStatus::Offline => (self.settings.theme.text_muted(), "offline"),
            };
            ui.colored_label(status_color, status_text);

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.small_button("Clear chat").clicked() {
                    self.msg_confirm_delete_chat = Some(contact_id.clone());
                }
            });
        });

        // Confirmation dialog for deleting chat
        if self.msg_confirm_delete_chat.as_deref() == Some(contact_id.as_str()) {
            ui.horizontal(|ui| {
                ui.colored_label(egui::Color32::from_rgb(0xFF, 0x80, 0x80), "Delete all messages?");
                if ui.small_button("Yes, delete").clicked() {
                    crate::chat::delete_chat_history(&contact_id);
                    if let Some(history) = self.msg_chat_histories.get_mut(&contact_id) {
                        history.messages.clear();
                    }
                    self.msg_confirm_delete_chat = None;
                }
                if ui.small_button("Cancel").clicked() {
                    self.msg_confirm_delete_chat = None;
                }
            });
        }

        ui.separator();

        // Drop zone indicator
        if has_hovered_files {
            let drop_frame = egui::Frame::none()
                .fill(self.settings.theme.widget_bg())
                .stroke(egui::Stroke::new(2.0, self.settings.theme.accent()))
                .inner_margin(16.0)
                .rounding(8.0);
            drop_frame.show(ui, |ui| {
                ui.vertical_centered(|ui| {
                    ui.label(
                        egui::RichText::new("Drop file here to send")
                            .size(16.0)
                            .color(self.settings.theme.accent()),
                    );
                });
            });
        }

        // Chat history
        let input_bar_h = 62.0; // taller input bar (1.5×)
        let available = ui.available_height() - input_bar_h;
        let scroll_height = available.max(80.0);

        // Collect file transfer actions to process after the borrow ends
        let mut file_actions: Vec<FileAction> = Vec::new();

        let visible_count = self.msg_visible_count.get(&contact_id).copied().unwrap_or(MSG_PAGE_SIZE);

        // If we loaded more messages last frame, set the initial scroll offset to compensate
        let pending_compensate = self.msg_scroll_compensate.take();
        let mut scroll_area = egui::ScrollArea::vertical()
            .max_height(scroll_height)
            .auto_shrink(false)
            .id_salt("msg_conversation_scroll");

        // Only stick to bottom if we're NOT compensating (normal flow)
        if pending_compensate.is_none() {
            scroll_area = scroll_area.stick_to_bottom(true);
        }

        let scroll_output = scroll_area.show(ui, |ui| {
                if let Some(history) = self.msg_chat_histories.get(&contact_id) {
                    if history.messages.is_empty() {
                        ui.colored_label(self.settings.theme.text_muted(), "No messages yet.");
                    } else {
                        let total = history.messages.len();
                        let skip = total.saturating_sub(visible_count);

                        for msg in history.messages.iter().skip(skip) {
                            let time = ChatHistory::format_time(msg.timestamp);

                            // File transfer message
                            if let Some(ref ft) = msg.file_transfer {
                                self.draw_file_message(ui, &contact_id, msg, ft, &time, &peer_name, &mut file_actions);
                                continue;
                            }

                            // Regular text message
                            if msg.from_me {
                                ui.horizontal_wrapped(|ui| {
                                    ui.add_space(2.0);
                                    ui.colored_label(self.settings.theme.text_muted(), &time);
                                    ui.colored_label(self.settings.theme.chat_self(), "You:");
                                    ui.label(&msg.text);
                                });
                            } else {
                                ui.horizontal_wrapped(|ui| {
                                    ui.add_space(2.0);
                                    ui.colored_label(self.settings.theme.text_muted(), &time);
                                    ui.colored_label(
                                        self.settings.theme.chat_peer(),
                                        format!("{}:", peer_name),
                                    );
                                    ui.label(&msg.text);
                                });
                            }
                        }
                    }
                }
            });

        // Compensate scroll position after loading older messages
        if let Some(old_height) = pending_compensate {
            let new_height = scroll_output.content_size.y;
            let height_diff = new_height - old_height;
            if height_diff > 0.0 {
                let mut state = scroll_output.state.clone();
                state.offset.y += height_diff;
                let scroll_id = egui::Id::new("msg_conversation_scroll");
                state.store(ui.ctx(), scroll_id);
                ui.ctx().request_repaint();
            }
        }

        // Auto-load older messages when scrolled near the top
        let has_older = self.msg_chat_histories.get(&contact_id)
            .map_or(false, |h| h.messages.len() > visible_count);
        if has_older && scroll_output.state.offset.y < 50.0 && pending_compensate.is_none() {
            // Save current content height so next frame we can compensate
            self.msg_scroll_compensate = Some(scroll_output.content_size.y);
            let new_count = visible_count + MSG_PAGE_SIZE;
            self.msg_visible_count.insert(contact_id.clone(), new_count);
        }

        // Process deferred file actions
        for action in file_actions {
            match action {
                FileAction::Accept(cid, tid) => {
                    if let Some(history) = self.msg_chat_histories.get_mut(&cid) {
                        history.update_file_status(tid, FileTransferStatus::Accepted, None);
                    }
                    if let Some(tx) = &self.msg_cmd_tx {
                        tx.send(MsgCommand::AcceptFileTransfer {
                            contact_id: cid,
                            transfer_id: tid,
                        }).ok();
                    }
                }
                FileAction::Reject(cid, tid) => {
                    if let Some(history) = self.msg_chat_histories.get_mut(&cid) {
                        history.update_file_status(tid, FileTransferStatus::Rejected, None);
                    }
                    if let Some(tx) = &self.msg_cmd_tx {
                        tx.send(MsgCommand::RejectFileTransfer {
                            contact_id: cid,
                            transfer_id: tid,
                        }).ok();
                    }
                }
                FileAction::Cancel(cid, tid) => {
                    if let Some(history) = self.msg_chat_histories.get_mut(&cid) {
                        history.update_file_status(tid, FileTransferStatus::Cancelled, None);
                    }
                    self.file_transfer_progress.remove(&(cid.clone(), tid));
                    if let Some(tx) = &self.msg_cmd_tx {
                        tx.send(MsgCommand::CancelFileTransfer {
                            contact_id: cid,
                            transfer_id: tid,
                        }).ok();
                    }
                }
                FileAction::OpenFolder(path) => {
                    open_folder_in_explorer(&path);
                }
            }
        }

        // Input bar (1.5× height) with distinct background
        let mut send = false;
        let mut pick_file = false;
        let bar_h = 38.0;
        let bar_frame = egui::Frame::none()
            .fill(self.settings.theme.sidebar_bg())
            .inner_margin(egui::Margin::symmetric(6.0, 6.0))
            .rounding(4.0);
        bar_frame.show(ui, |ui| {
            ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                // File attach button
                let attach_btn = egui::Button::new(
                    egui::RichText::new("+").size(18.0).strong(),
                )
                .min_size(egui::vec2(bar_h, bar_h));
                let attach_resp = ui.add(attach_btn);
                if attach_resp.clicked() {
                    pick_file = true;
                }
                attach_resp.on_hover_text("Send file");

                // TextEdit with always-visible outline and distinct bg
                let outline = self.settings.theme.text_muted();
                ui.visuals_mut().widgets.inactive.bg_stroke = egui::Stroke::new(1.0, outline);
                ui.visuals_mut().widgets.inactive.bg_fill = self.settings.theme.panel_bg();

                let resp = ui.add_sized(
                    egui::vec2(ui.available_width() - 75.0, bar_h),
                    egui::TextEdit::singleline(&mut self.msg_chat_input)
                        .hint_text("Type a message...")
                        .margin(egui::vec2(8.0, 10.0)),
                );
                if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    send = true;
                }
                if ui.add(egui::Button::new("Send").min_size(egui::vec2(60.0, bar_h))).clicked() {
                    send = true;
                }
                if send {
                    resp.request_focus();
                }
            });
        });

        // Open file picker dialog (runs after the ui borrow ends)
        // rfd on Linux uses xdg-desktop-portal via zbus which needs a Tokio runtime.
        if pick_file {
            if let Some(contact) = self.contacts.iter().find(|c| c.contact_id == contact_id).cloned() {
                // macOS/Windows: sync dialog on main thread. Linux: needs tokio for xdg-portal.
                #[cfg(target_os = "linux")]
                let picked = std::thread::spawn(|| {
                    let rt = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .ok()?;
                    rt.block_on(async {
                        rfd::AsyncFileDialog::new()
                            .set_title("Send file")
                            .pick_files()
                            .await
                    })
                }).join().ok().flatten();

                #[cfg(not(target_os = "linux"))]
                let picked = rfd::FileDialog::new()
                    .set_title("Send file")
                    .pick_files();

                if let Some(handles) = picked {
                    for handle in handles {
                        #[cfg(target_os = "linux")]
                        let path: std::path::PathBuf = handle.path().to_path_buf();
                        #[cfg(not(target_os = "linux"))]
                        let path: std::path::PathBuf = handle;
                        if path.is_file() {
                            self.send_file_to_contact(&contact_id, &contact, &path);
                        }
                    }
                }
            }
        }

        if send && !self.msg_chat_input.trim().is_empty() {
            let text = self.msg_chat_input.trim().to_string();
            self.msg_chat_input.clear();

            // Save to local chat history
            if let Some(history) = self.msg_chat_histories.get_mut(&contact_id) {
                history.add_message(true, text.clone());
            }

            // Send via daemon
            if let Some(contact) = &contact {
                if let (Some(tx), Some(addr)) = (&self.msg_cmd_tx, self.resolve_peer_addr(contact)) {
                    tx.send(MsgCommand::SendMessage {
                        contact_id: contact_id.clone(),
                        peer_addr: addr,
                        peer_pubkey: contact.pubkey,
                        text,
                    }).ok();
                }
            }
        }
    }

    fn draw_file_message(
        &self,
        ui: &mut egui::Ui,
        contact_id: &str,
        msg: &crate::chat::ChatMessage,
        ft: &crate::chat::FileTransferInfo,
        time: &str,
        peer_name: &str,
        actions: &mut Vec<FileAction>,
    ) {
        let size_str = crate::filetransfer::format_size(ft.file_size);
        let sender_label = if msg.from_me { "You" } else { peer_name };

        let file_frame = egui::Frame::none()
            .fill(self.settings.theme.widget_bg())
            .inner_margin(8.0)
            .rounding(6.0)
            .outer_margin(egui::Margin::symmetric(0.0, 2.0));

        file_frame.show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.colored_label(self.settings.theme.text_muted(), time);
                let sender_color = if msg.from_me {
                    self.settings.theme.chat_self()
                } else {
                    self.settings.theme.chat_peer()
                };
                ui.colored_label(sender_color, format!("{sender_label}:"));
            });

            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new(&ft.filename)
                        .strong()
                        .size(13.0),
                );
                ui.colored_label(
                    self.settings.theme.text_muted(),
                    format!("({size_str})"),
                );
            });

            match &ft.status {
                FileTransferStatus::Offered => {
                    if !msg.from_me {
                        // Incoming offer: show Accept/Reject buttons
                        ui.horizontal(|ui| {
                            let accept_btn = egui::Button::new(
                                egui::RichText::new("Accept").color(egui::Color32::WHITE),
                            )
                            .fill(self.settings.theme.btn_positive())
                            .min_size(egui::vec2(70.0, 24.0));
                            if ui.add(accept_btn).clicked() {
                                actions.push(FileAction::Accept(contact_id.to_string(), ft.transfer_id));
                            }

                            let reject_btn = egui::Button::new(
                                egui::RichText::new("Reject").color(egui::Color32::WHITE),
                            )
                            .fill(self.settings.theme.btn_negative())
                            .min_size(egui::vec2(70.0, 24.0));
                            if ui.add(reject_btn).clicked() {
                                actions.push(FileAction::Reject(contact_id.to_string(), ft.transfer_id));
                            }
                        });
                    } else {
                        ui.colored_label(self.settings.theme.text_muted(), "Waiting for response...");
                    }
                }
                FileTransferStatus::Accepted => {
                    // Show progress bar if we have progress data
                    let progress = self.file_transfer_progress
                        .get(&(contact_id.to_string(), ft.transfer_id));
                    if let Some((transferred, total)) = progress {
                        let fraction = if *total > 0 {
                            *transferred as f32 / *total as f32
                        } else {
                            0.0
                        };
                        let progress_text = format!(
                            "{} / {}",
                            crate::filetransfer::format_size(*transferred),
                            crate::filetransfer::format_size(*total),
                        );
                        ui.add(
                            egui::ProgressBar::new(fraction)
                                .text(progress_text)
                                .desired_width(ui.available_width().min(300.0)),
                        );
                    } else {
                        ui.colored_label(self.settings.theme.text_muted(), "Transferring...");
                    }

                    // Cancel button
                    if ui.small_button("Cancel").clicked() {
                        actions.push(FileAction::Cancel(contact_id.to_string(), ft.transfer_id));
                    }
                }
                FileTransferStatus::Completed => {
                    ui.colored_label(
                        egui::Color32::from_rgb(0x4C, 0xAF, 0x50),
                        "Completed",
                    );
                    if let Some(ref saved_path) = ft.saved_path {
                        if !saved_path.is_empty() {
                            if ui.small_button("Open folder").clicked() {
                                actions.push(FileAction::OpenFolder(saved_path.clone()));
                            }
                        }
                    }
                }
                FileTransferStatus::Rejected => {
                    ui.colored_label(self.settings.theme.text_muted(), "Rejected");
                }
                FileTransferStatus::Cancelled => {
                    ui.colored_label(self.settings.theme.text_muted(), "Cancelled");
                }
                FileTransferStatus::Failed(reason) => {
                    ui.colored_label(
                        self.settings.theme.btn_negative(),
                        format!("Failed: {reason}"),
                    );
                }
            }
        });
    }

    /// Open a conversation for a contact_id. Loads history and clears unread.
    pub(crate) fn open_msg_chat(&mut self, contact_id: &str) {
        // Load chat history if not already loaded
        if !self.msg_chat_histories.contains_key(contact_id) {
            let history = ChatHistory::load(contact_id, &self.identity.secret);
            self.msg_chat_histories.insert(contact_id.to_string(), history);
        }
        self.msg_active_chat = Some(contact_id.to_string());
        self.msg_unread.remove(contact_id);
        // Start showing only the last PAGE_SIZE messages
        self.msg_visible_count.insert(contact_id.to_string(), MSG_PAGE_SIZE);

        // Try to connect to peer via daemon
        if let Some(contact) = self.contacts.iter().find(|c| c.contact_id == contact_id) {
            if let (Some(tx), Some(addr)) = (&self.msg_cmd_tx, self.resolve_peer_addr(contact)) {
                tx.send(MsgCommand::Connect {
                    contact_id: contact_id.to_string(),
                    peer_addr: addr,
                    peer_pubkey: contact.pubkey,
                }).ok();
            }
        }
    }

    /// Send a file to a contact: add to chat history and dispatch to daemon.
    fn send_file_to_contact(
        &mut self,
        contact_id: &str,
        contact: &identity::Contact,
        path: &std::path::Path,
    ) {
        let path_str = path.to_string_lossy().to_string();
        let filename = path.file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "file".to_string());
        let file_size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);

        // Add to local chat history as "offered"
        if let Some(history) = self.msg_chat_histories.get_mut(contact_id) {
            history.add_file_message(true, crate::chat::FileTransferInfo {
                filename,
                file_size,
                transfer_id: 0, // assigned by daemon
                status: crate::chat::FileTransferStatus::Offered,
                saved_path: None,
            });
        }

        // Send offer to daemon
        if let (Some(tx), Some(addr)) = (&self.msg_cmd_tx, self.resolve_peer_addr(contact)) {
            tx.send(MsgCommand::SendFileOffer {
                contact_id: contact_id.to_string(),
                peer_addr: addr,
                peer_pubkey: contact.pubkey,
                file_path: path_str,
            }).ok();
        }
    }

    /// Draw the "Add Friend" panel (friend request form + requests list) inside Messages.
    fn draw_add_friend_panel(&mut self, ui: &mut egui::Ui) {
        ui.add_space(10.0);

        // ── Send a Friend Request ──
        ui.label(egui::RichText::new("Send a Friend Request").strong().size(15.0));
        ui.add_space(4.0);

        ui.horizontal(|ui| {
            ui.label("IP:");
            let ip_edit = egui::TextEdit::singleline(&mut self.req_ip_input)
                .hint_text("e.g. ::1 or 2001:db8::1")
                .desired_width(200.0)
                .password(!self.show_ips);
            egui::Frame::none()
                .stroke(egui::Stroke::new(1.0, self.settings.theme.separator()))
                .inner_margin(0.0)
                .show(ui, |ui| { ui.add(ip_edit); });
            let eye = if self.show_ips { "Hide" } else { "Show" };
            if ui.small_button(eye).clicked() {
                self.show_ips = !self.show_ips;
            }
            ui.label("Port:");
            let port_edit = egui::TextEdit::singleline(&mut self.req_port_input)
                .hint_text("9000")
                .desired_width(80.0);
            egui::Frame::none()
                .stroke(egui::Stroke::new(1.0, self.settings.theme.separator()))
                .inner_margin(0.0)
                .show(ui, |ui| { ui.add(port_edit); });
        });

        ui.add_space(4.0);

        // Status message
        if !self.req_status.is_empty() {
            let color = if self.req_status.starts_with("Error") || self.req_status.starts_with("Failed") {
                self.settings.theme.error()
            } else {
                self.settings.theme.accent()
            };
            ui.colored_label(color, &self.req_status);
        }

        if ui.button("Send Request").clicked() {
            self.send_add_friend_request();
        }

        ui.add_space(8.0);
        ui.separator();
        ui.add_space(6.0);

        // ── Sub-tabs: Friends | Requests ──
        let incoming_count = self.req_incoming.len();

        ui.horizontal(|ui| {
            let is_list = self.friends_sub_tab == FriendsSubTab::List;
            let is_reqs = self.friends_sub_tab == FriendsSubTab::Requests;

            let list_text = egui::RichText::new("Friends").size(14.0);
            let list_text = if is_list { list_text.strong() } else { list_text };
            let list_btn = egui::Button::new(list_text)
                .fill(if is_list { self.settings.theme.widget_bg() } else { egui::Color32::TRANSPARENT })
                .rounding(6.0)
                .min_size(egui::vec2(80.0, 28.0));
            if ui.add(list_btn).clicked() {
                self.friends_sub_tab = FriendsSubTab::List;
            }

            let req_label = if incoming_count > 0 {
                format!("Requests ({})", incoming_count)
            } else {
                "Requests".to_string()
            };
            let req_text = egui::RichText::new(&req_label).size(14.0);
            let req_text = if is_reqs { req_text.strong() } else { req_text };
            let req_btn = egui::Button::new(req_text)
                .fill(if is_reqs { self.settings.theme.widget_bg() } else { egui::Color32::TRANSPARENT })
                .rounding(6.0)
                .min_size(egui::vec2(80.0, 28.0));
            if ui.add(req_btn).clicked() {
                self.friends_sub_tab = FriendsSubTab::Requests;
            }
        });

        ui.add_space(6.0);

        match self.friends_sub_tab {
            FriendsSubTab::List => self.draw_add_friend_list(ui),
            FriendsSubTab::Requests => self.draw_add_friend_requests(ui),
        }
    }

    fn draw_add_friend_list(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            let search_edit = egui::TextEdit::singleline(&mut self.contact_search)
                .hint_text("Search...")
                .desired_width(200.0);
            egui::Frame::none()
                .stroke(egui::Stroke::new(1.0, self.settings.theme.separator()))
                .inner_margin(2.0)
                .show(ui, |ui| { ui.add(search_edit); });
        });
        ui.add_space(6.0);

        if self.contacts.is_empty() {
            ui.colored_label(self.settings.theme.text_muted(), "No contacts yet. Send a friend request to add one.");
            return;
        }

        let search = self.contact_search.to_lowercase();

        let mut click_contact: Option<usize> = None;
        let mut block_contact: Option<usize> = None;
        let mut delete_contact: Option<usize> = None;

        let mut sorted_indices: Vec<usize> = (0..self.contacts.len()).collect();
        sorted_indices.sort_by_key(|&i| if self.contacts[i].pubkey == self.identity.pubkey { 0 } else { 1 });

        let max_height = ui.available_height().max(80.0);
        egui::ScrollArea::vertical()
            .max_height(max_height)
            .id_salt("add_friend_list_scroll")
            .show(ui, |ui| {
                for i in &sorted_indices {
                    let i = *i;
                    let contact = &self.contacts[i];
                    if !search.is_empty()
                        && !contact.nickname.to_lowercase().contains(&search)
                        && !contact.fingerprint.to_lowercase().contains(&search)
                    {
                        continue;
                    }

                    let hex = identity::pubkey_hex(&contact.pubkey);
                    let is_blocked = self.settings.is_blocked(&hex);
                    let is_self = contact.pubkey == self.identity.pubkey;
                    ui.horizontal(|ui| {
                        if is_self {
                            if ui.add(egui::Button::new(
                                egui::RichText::new("YO (you)").italics().color(self.settings.theme.text_muted())
                            ).frame(false)).clicked() {
                                click_contact = Some(i);
                            }
                        } else if is_blocked {
                            let text = format!("{} ({})", contact.nickname, contact.fingerprint);
                            if ui.add(egui::Button::new(
                                egui::RichText::new(&text).strikethrough().color(self.settings.theme.text_muted())
                            ).frame(false)).clicked() {
                                click_contact = Some(i);
                            }
                        } else {
                            let job = peer_display_job(&contact.nickname, &contact.fingerprint, 13.0, self.settings.theme.text_primary(), self.settings.theme.text_dim());
                            if ui.add(egui::Button::new(job).frame(false)).clicked() {
                                click_contact = Some(i);
                            }
                        }

                        let remaining = ui.available_width() - 110.0;
                        if remaining > 0.0 {
                            ui.add_space(remaining);
                        }

                        let block_label = if is_blocked { "Unblock" } else { "Block" };
                        if ui.small_button(block_label).clicked() {
                            block_contact = Some(i);
                        }

                        if ui.small_button("X").clicked() {
                            delete_contact = Some(i);
                        }
                    });

                    ui.separator();
                }
            });

        if let Some(i) = click_contact {
            let contact = self.contacts[i].clone();
            let cid = contact.contact_id.clone();
            self.show_add_friend = false;
            self.open_msg_chat(&cid);
        }
        if let Some(i) = block_contact {
            let contact = &self.contacts[i];
            let hex = identity::pubkey_hex(&contact.pubkey);
            if self.settings.is_blocked(&hex) {
                self.settings.unblock_contact(&hex);
                if !contact.last_address.is_empty() {
                    self.settings.unban_ip(&contact.last_address);
                }
            } else {
                self.settings.block_contact(&hex);
                if !contact.last_address.is_empty() {
                    self.settings.ban_ip(&contact.last_address);
                }
            }
        }
        if let Some(i) = delete_contact {
            self.msg_confirm_delete_contact = Some(i);
        }
    }

    fn draw_add_friend_requests(&mut self, ui: &mut egui::Ui) {
        if self.req_incoming.is_empty() {
            ui.colored_label(self.settings.theme.text_muted(), "No pending requests.");
            return;
        }

        let mut action: Option<(String, AddFriendAction)> = None;

        let max_height = ui.available_height().max(80.0);
        egui::ScrollArea::vertical()
            .max_height(max_height)
            .id_salt("add_friend_requests_scroll")
            .show(ui, |ui| {
                for (request_id, nickname, ip, fingerprint) in &self.req_incoming {
                    ui.group(|ui| {
                        ui.horizontal(|ui| {
                            ui.vertical(|ui| {
                                ui.label(peer_display_job(nickname, fingerprint, 14.0, self.settings.theme.text_primary(), self.settings.theme.text_dim()));
                                let display_ip = if self.show_ips { ip.clone() } else { censor_ip(ip) };
                                ui.colored_label(
                                    self.settings.theme.text_muted(),
                                    format!("IP: {display_ip}"),
                                );
                            });
                        });
                        ui.add_space(2.0);
                        ui.horizontal(|ui| {
                            if ui.add(egui::Button::new(
                                egui::RichText::new("Accept").color(self.settings.theme.accent()),
                            )).clicked() {
                                action = Some((request_id.clone(), AddFriendAction::Accept));
                            }
                            if ui.button("Reject").clicked() {
                                action = Some((request_id.clone(), AddFriendAction::Reject));
                            }
                            if ui.add(egui::Button::new(
                                egui::RichText::new("Block").color(self.settings.theme.error()),
                            )).clicked() {
                                action = Some((request_id.clone(), AddFriendAction::Block));
                            }
                        });
                    });
                    ui.add_space(2.0);
                }
            });

        if let Some((request_id, act)) = action {
            let cmd = match act {
                AddFriendAction::Accept => {
                    log_fmt!("[gui] accepted contact request");
                    Some(MsgCommand::AcceptRequest { request_id: request_id.clone() })
                }
                AddFriendAction::Reject => {
                    log_fmt!("[gui] rejected contact request");
                    Some(MsgCommand::RejectRequest { request_id: request_id.clone() })
                }
                AddFriendAction::Block => {
                    let ip = self.req_incoming.iter()
                        .find(|(rid, ..)| rid == &request_id)
                        .map(|(_, _, ip, _)| ip.clone())
                        .unwrap_or_default();
                    Some(MsgCommand::BlockRequest { request_id: request_id.clone(), ip })
                }
            };
            if let Some(cmd) = cmd {
                if let Some(tx) = &self.msg_cmd_tx {
                    tx.send(cmd).ok();
                }
            }
            self.req_incoming.retain(|(rid, ..)| rid != &request_id);
        }
    }

    fn send_add_friend_request(&mut self) {
        let ip = self.req_ip_input.trim().to_string();
        let port = self.req_port_input.trim().to_string();

        if ip.is_empty() {
            self.req_status = "Error: IP address is required".to_string();
            return;
        }
        if port.is_empty() {
            self.req_status = "Error: Port is required".to_string();
            return;
        }

        let addr_str = format!("[{ip}]:{port}");
        let peer_addr: SocketAddr = match addr_str.parse() {
            Ok(a) => a,
            Err(_) => {
                let addr_str2 = format!("{ip}:{port}");
                match addr_str2.parse() {
                    Ok(a) => a,
                    Err(_) => {
                        self.req_status = "Error: Invalid IP or port".to_string();
                        return;
                    }
                }
            }
        };

        if let Some(tx) = &self.msg_cmd_tx {
            tx.send(MsgCommand::SendRequest { peer_addr }).ok();
            self.req_status = format!("Request sent to {}", peer_addr);
            self.req_ip_input.clear();
            self.req_port_input.clear();
        }
    }

    fn resolve_peer_addr(&self, contact: &identity::Contact) -> Option<SocketAddr> {
        if contact.last_address.is_empty() || contact.last_port.is_empty() {
            return None;
        }
        let addr_str = format!("[{}]:{}", contact.last_address, contact.last_port);
        addr_str.parse().ok()
    }
}

enum AddFriendAction {
    Accept,
    Reject,
    Block,
}

/// Deferred actions from file message buttons (avoids borrow conflicts).
enum FileAction {
    Accept(String, u32),
    Reject(String, u32),
    Cancel(String, u32),
    OpenFolder(String),
}

/// Open the folder containing a file in the system file manager.
fn open_folder_in_explorer(file_path: &str) {
    let path = std::path::Path::new(file_path);
    let folder = path.parent().unwrap_or(path);

    #[cfg(target_os = "linux")]
    {
        std::process::Command::new("xdg-open")
            .arg(folder)
            .spawn()
            .ok();
    }
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg(folder)
            .spawn()
            .ok();
    }
    #[cfg(target_os = "windows")]
    {
        std::process::Command::new("explorer")
            .arg(folder)
            .spawn()
            .ok();
    }
}
