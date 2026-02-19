use eframe::egui;
use std::net::SocketAddr;

use crate::chat::{ChatHistory, FileTransferStatus};
use crate::identity;
use crate::messaging::MsgCommand;

use super::HostelApp;

impl HostelApp {
    pub(crate) fn draw_messages_tab(&mut self, ui: &mut egui::Ui) {
        let available = ui.available_rect_before_wrap();
        let list_w = 165.0_f32.min(available.width() * 0.28);
        let chat_w = (available.width() - list_w - 4.0).max(100.0);

        let list_rect = egui::Rect::from_min_size(
            available.min,
            egui::vec2(list_w, available.height()),
        );
        let chat_rect = egui::Rect::from_min_size(
            egui::pos2(available.min.x + list_w + 4.0, available.min.y),
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
    }

    fn draw_message_list(&mut self, ui: &mut egui::Ui, open_chat: &mut Option<String>) {
        ui.add_space(6.0);
        ui.heading("Messages");
        ui.add_space(4.0);

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
                for (contact_id, name, preview, online, unread) in &conversations {
                    let is_active = active_cid.as_deref() == Some(contact_id.as_str());

                    // Highlight active chat
                    let frame = if is_active {
                        egui::Frame::none()
                            .fill(self.settings.theme.sidebar_bg())
                            .inner_margin(4.0)
                    } else {
                        egui::Frame::none().inner_margin(4.0)
                    };

                    frame.show(ui, |ui| {
                        ui.horizontal(|ui| {
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
                            let (rect, _) = ui.allocate_exact_size(egui::vec2(8.0, 8.0), egui::Sense::hover());
                            ui.painter().circle_filled(rect.center(), 4.0, color);

                            // Name + preview (clickable)
                            let text = if *unread > 0 {
                                egui::RichText::new(format!("{name} ({unread})")).strong().size(12.0)
                            } else {
                                egui::RichText::new(name.as_str()).size(12.0)
                            };

                            ui.vertical(|ui| {
                                if ui.add(egui::Button::new(text).frame(false)).clicked() {
                                    *open_chat = Some(contact_id.clone());
                                }
                                if !preview.is_empty() {
                                    ui.colored_label(self.settings.theme.text_muted(),
                                        egui::RichText::new(preview.as_str()).small());
                                }
                            });
                        });
                    });
                    ui.separator();
                }
            });
    }

    fn draw_message_conversation(&mut self, ui: &mut egui::Ui) {
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
        let available = ui.available_height() - 46.0;
        let scroll_height = available.max(80.0);

        // Collect file transfer actions to process after the borrow ends
        let mut file_actions: Vec<FileAction> = Vec::new();

        egui::ScrollArea::vertical()
            .max_height(scroll_height)
            .stick_to_bottom(true)
            .id_salt("msg_conversation_scroll")
            .show(ui, |ui| {
                if let Some(history) = self.msg_chat_histories.get(&contact_id) {
                    if history.messages.is_empty() {
                        ui.colored_label(self.settings.theme.text_muted(), "No messages yet.");
                    }
                    for msg in &history.messages {
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
            });

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

        // Input bar
        ui.separator();
        ui.add_space(2.0);
        let mut send = false;
        let mut pick_file = false;
        ui.horizontal(|ui| {
            // File attach button
            let attach_btn = egui::Button::new(
                egui::RichText::new("+").size(16.0).strong(),
            )
            .min_size(egui::vec2(28.0, 28.0));
            let attach_resp = ui.add(attach_btn);
            if attach_resp.clicked() {
                pick_file = true;
            }
            attach_resp.on_hover_text("Send file");

            let resp = ui.add(
                egui::TextEdit::singleline(&mut self.msg_chat_input)
                    .hint_text("Type a message...")
                    .desired_width(ui.available_width() - 70.0),
            );
            if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                send = true;
            }
            if ui.button("Send").clicked() {
                send = true;
            }
            if send {
                resp.request_focus();
            }
        });
        ui.add_space(4.0);

        // Open file picker dialog (runs after the ui borrow ends)
        // rfd on Linux uses xdg-desktop-portal via zbus which needs a Tokio runtime.
        if pick_file {
            if let Some(contact) = self.contacts.iter().find(|c| c.contact_id == contact_id).cloned() {
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

                if let Some(handles) = picked {
                    for handle in handles {
                        let path: std::path::PathBuf = handle.path().to_path_buf();
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

    fn resolve_peer_addr(&self, contact: &identity::Contact) -> Option<SocketAddr> {
        if contact.last_address.is_empty() || contact.last_port.is_empty() {
            return None;
        }
        let addr_str = format!("[{}]:{}", contact.last_address, contact.last_port);
        addr_str.parse().ok()
    }
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
