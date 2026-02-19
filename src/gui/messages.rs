use eframe::egui;
use std::net::SocketAddr;

use crate::chat::ChatHistory;
use crate::identity;
use crate::messaging::MsgCommand;

use super::HostelApp;

impl HostelApp {
    pub(crate) fn draw_messages_tab(&mut self, ui: &mut egui::Ui) {
        if self.msg_active_chat.is_some() {
            self.draw_message_conversation(ui);
            return;
        }
        self.draw_message_list(ui);
    }

    fn draw_message_list(&mut self, ui: &mut egui::Ui) {
        ui.add_space(10.0);
        ui.heading("Messages");
        ui.add_space(6.0);

        // Build conversation list from ALL contacts (except blocked)
        let mut conversations: Vec<(String, String, String, bool, u32)> = Vec::new(); // (contact_id, nickname, preview, online, unread)

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
                    let text = if m.text.len() > 30 { &m.text[..30] } else { &m.text };
                    format!("{prefix}{text}")
                })
                .unwrap_or_default();

            let name = if contact.nickname.is_empty() {
                contact.fingerprint.clone()
            } else {
                contact.nickname.clone()
            };

            conversations.push((contact.contact_id.clone(), name, preview, online, unread));
        }

        if conversations.is_empty() {
            ui.colored_label(self.settings.theme.text_muted(), "No contacts yet. Make a call to add one.");
            return;
        }

        // Sort: online first, then by most recent message, then rest
        conversations.sort_by(|a, b| {
            // Online first
            let online_ord = b.3.cmp(&a.3);
            if online_ord != std::cmp::Ordering::Equal { return online_ord; }
            // Then by most recent message
            let ts_a = self.msg_chat_histories.get(&a.0)
                .and_then(|h| h.messages.last().map(|m| m.timestamp))
                .unwrap_or(0);
            let ts_b = self.msg_chat_histories.get(&b.0)
                .and_then(|h| h.messages.last().map(|m| m.timestamp))
                .unwrap_or(0);
            ts_b.cmp(&ts_a)
        });

        let mut open_chat: Option<String> = None;

        let max_height = ui.available_height().max(80.0);
        egui::ScrollArea::vertical()
            .max_height(max_height)
            .id_salt("messages_list_scroll")
            .show(ui, |ui| {
                for (contact_id, name, preview, online, unread) in &conversations {
                    ui.horizontal(|ui| {
                        // Presence indicator: green=online, yellow=away, grey=offline
                        let presence = self.msg_peer_presence.get(contact_id)
                            .copied()
                            .unwrap_or(if *online {
                                crate::messaging::PresenceStatus::Online
                            } else {
                                crate::messaging::PresenceStatus::Offline
                            });
                        let color = match presence {
                            crate::messaging::PresenceStatus::Online => egui::Color32::from_rgb(0x4C, 0xAF, 0x50), // green
                            crate::messaging::PresenceStatus::Away => egui::Color32::from_rgb(0xFF, 0xC1, 0x07),   // yellow/amber
                            crate::messaging::PresenceStatus::Offline => self.settings.theme.text_muted(),
                        };
                        let (rect, _) = ui.allocate_exact_size(egui::vec2(8.0, 8.0), egui::Sense::hover());
                        ui.painter().circle_filled(rect.center(), 4.0, color);

                        // Name + preview (clickable)
                        let text = if *unread > 0 {
                            egui::RichText::new(format!("{name}  ({unread})")).strong()
                        } else {
                            egui::RichText::new(name.as_str()).into()
                        };

                        ui.vertical(|ui| {
                            if ui.add(egui::Button::new(text).frame(false)).clicked() {
                                open_chat = Some(contact_id.clone());
                            }
                            if !preview.is_empty() {
                                ui.colored_label(self.settings.theme.text_muted(),
                                    egui::RichText::new(preview.as_str()).small());
                            }
                        });
                    });
                    ui.separator();
                }
            });

        if let Some(cid) = open_chat {
            self.open_msg_chat(&cid);
        }
    }

    fn draw_message_conversation(&mut self, ui: &mut egui::Ui) {
        let contact_id = match &self.msg_active_chat {
            Some(cid) => cid.clone(),
            None => return,
        };

        // Find contact info
        let contact = self.contacts.iter().find(|c| c.contact_id == contact_id).cloned();
        let peer_name = contact.as_ref()
            .map(|c| if c.nickname.is_empty() { c.fingerprint.clone() } else { c.nickname.clone() })
            .unwrap_or_else(|| "Unknown".to_string());
        let online = self.msg_peer_online.get(&contact_id).copied().unwrap_or(false);

        ui.add_space(10.0);

        // Header
        let mut go_back = false;
        ui.horizontal(|ui| {
            if ui.button("<< Back").clicked() {
                go_back = true;
            }
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
        });

        if go_back {
            self.msg_active_chat = None;
            return;
        }

        ui.add_space(4.0);
        ui.separator();

        // Chat history
        let available = ui.available_height() - 40.0;
        let scroll_height = available.max(80.0);

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
                        if msg.from_me {
                            ui.horizontal_wrapped(|ui| {
                                ui.colored_label(self.settings.theme.text_muted(), &time);
                                ui.colored_label(self.settings.theme.chat_self(), "You:");
                                ui.label(&msg.text);
                            });
                        } else {
                            ui.horizontal_wrapped(|ui| {
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

        // Input bar
        ui.separator();
        let mut send = false;
        ui.horizontal(|ui| {
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

    fn resolve_peer_addr(&self, contact: &identity::Contact) -> Option<SocketAddr> {
        if contact.last_address.is_empty() || contact.last_port.is_empty() {
            return None;
        }
        let addr_str = format!("[{}]:{}", contact.last_address, contact.last_port);
        addr_str.parse().ok()
    }
}
