use eframe::egui;
use crate::chat::ChatHistory;
use crate::identity;
use super::{HostelApp, SidebarTab, format_peer_display};

impl HostelApp {
    pub(crate) fn draw_contacts_tab(&mut self, ui: &mut egui::Ui) {
        // Detail view
        if self.viewing_contact.is_some() {
            self.draw_contact_detail(ui);
            return;
        }

        // List view
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            ui.heading("Contacts");
            ui.add(
                egui::TextEdit::singleline(&mut self.contact_search)
                    .hint_text("Search...")
                    .desired_width(150.0)
            );
        });
        ui.add_space(6.0);

        if self.contacts.is_empty() {
            ui.colored_label(egui::Color32::GRAY, "No contacts yet. Make a call to add one.");
            return;
        }

        let search = self.contact_search.to_lowercase();

        // Collect actions during iteration to avoid borrow checker issues
        let mut click_contact: Option<usize> = None;
        let mut block_contact: Option<usize> = None;
        let mut delete_contact: Option<usize> = None;

        let max_height = ui.available_height().max(80.0);
        egui::ScrollArea::vertical()
            .max_height(max_height)
            .id_salt("contacts_list_scroll")
            .show(ui, |ui| {
                for (i, contact) in self.contacts.iter().enumerate() {
                    if !search.is_empty()
                        && !contact.nickname.to_lowercase().contains(&search)
                        && !contact.fingerprint.to_lowercase().contains(&search)
                    {
                        continue;
                    }

                    let hex = identity::pubkey_hex(&contact.pubkey);
                    let is_blocked = self.settings.is_blocked(&hex);
                    let display = format_peer_display(&contact.nickname, &contact.fingerprint);

                    ui.horizontal(|ui| {
                        // Contact name (clickable)
                        let text = if is_blocked {
                            egui::RichText::new(&display).strikethrough().color(egui::Color32::GRAY)
                        } else {
                            egui::RichText::new(&display)
                        };
                        if ui.add(egui::Button::new(text).frame(false)).clicked() {
                            click_contact = Some(i);
                        }

                        // Push buttons to the right
                        let remaining = ui.available_width() - 110.0;
                        if remaining > 0.0 {
                            ui.add_space(remaining);
                        }

                        // Block/Unblock
                        let block_label = if is_blocked { "Unblock" } else { "Block" };
                        if ui.small_button(block_label).clicked() {
                            block_contact = Some(i);
                        }

                        // Delete
                        if ui.small_button("X").clicked() {
                            delete_contact = Some(i);
                        }
                    });

                    ui.separator();
                }
            });

        // Apply actions after iteration
        if let Some(i) = click_contact {
            let contact = self.contacts[i].clone();
            // Load chat history for viewing
            let chat = ChatHistory::load(&contact.contact_id, &self.identity.secret);
            self.viewing_chat = Some(chat);
            self.viewing_contact = Some(contact);
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
            let contact = &self.contacts[i];
            crate::chat::delete_chat_history(&contact.contact_id);
            identity::delete_contact(&contact.pubkey);
            self.contacts = identity::load_all_contacts();
        }
    }

    pub(crate) fn draw_contact_detail(&mut self, ui: &mut egui::Ui) {
        let contact = self.viewing_contact.clone().unwrap();

        ui.add_space(10.0);
        if ui.button("<< Back").clicked() {
            self.viewing_contact = None;
            self.viewing_chat = None;
            self.contacts = identity::load_all_contacts();
            return;
        }

        ui.add_space(6.0);
        let display = format_peer_display(&contact.nickname, &contact.fingerprint);
        ui.heading(&display);
        ui.add_space(6.0);

        ui.horizontal(|ui| {
            ui.label("Fingerprint:");
            ui.monospace(&contact.fingerprint);
        });
        if !contact.last_address.is_empty() {
            ui.horizontal(|ui| {
                ui.label("Last address:");
                ui.monospace(format!("[{}]:{}", contact.last_address, contact.last_port));
            });
        }
        ui.horizontal(|ui| {
            ui.label("Calls:");
            ui.label(format!("{}", contact.call_count));
        });

        ui.add_space(8.0);
        let call_btn = egui::Button::new(
            egui::RichText::new("Call").size(16.0).color(egui::Color32::WHITE)
        ).min_size(egui::vec2(120.0, 34.0)).fill(egui::Color32::from_rgb(40, 140, 60));
        if ui.add(call_btn).clicked() {
            if !contact.last_address.is_empty() {
                self.peer_ip = contact.last_address.clone();
            }
            if !contact.last_port.is_empty() {
                self.peer_port = contact.last_port.clone();
            }
            self.viewing_contact = None;
            self.viewing_chat = None;
            self.active_tab = SidebarTab::Call;
        }

        ui.add_space(10.0);
        ui.separator();
        ui.label(egui::RichText::new("Chat History").strong());

        // Read-only chat history
        let available_height = ui.available_height().max(80.0);
        let peer_label = if contact.nickname.is_empty() {
            "Peer:".to_string()
        } else {
            format!("{}:", contact.nickname)
        };
        egui::ScrollArea::vertical()
            .max_height(available_height)
            .stick_to_bottom(true)
            .id_salt("contact_chat_scroll")
            .show(ui, |ui| {
                if let Some(history) = &self.viewing_chat {
                    if history.messages.is_empty() {
                        ui.colored_label(egui::Color32::GRAY, "No messages.");
                    }
                    for msg in &history.messages {
                        let time = ChatHistory::format_time(msg.timestamp);
                        if msg.from_me {
                            ui.horizontal_wrapped(|ui| {
                                ui.colored_label(egui::Color32::GRAY, &time);
                                ui.colored_label(egui::Color32::from_rgb(100, 180, 255), "You:");
                                ui.label(&msg.text);
                            });
                        } else {
                            ui.horizontal_wrapped(|ui| {
                                ui.colored_label(egui::Color32::GRAY, &time);
                                ui.colored_label(egui::Color32::from_rgb(180, 255, 100), &peer_label);
                                ui.label(&msg.text);
                            });
                        }
                    }
                }
            });
    }
}
