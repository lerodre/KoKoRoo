use eframe::egui;
use std::net::SocketAddr;

use crate::chat::ChatHistory;
use crate::identity;
use crate::messaging::MsgCommand;
use super::{HostelApp, SidebarTab, FriendsSubTab, format_peer_display, peer_display_job, censor_ip};

impl HostelApp {
    pub(crate) fn draw_friends_tab(&mut self, ui: &mut egui::Ui) {
        // Detail view (contact info)
        if self.viewing_contact.is_some() {
            self.draw_contact_detail(ui);
            return;
        }

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
            self.send_contact_request();
        }

        ui.add_space(8.0);
        ui.separator();
        ui.add_space(6.0);

        // ── Sub-tabs: Friends | Requests ──
        let incoming_count = self.req_incoming.len();

        ui.horizontal(|ui| {
            let is_list = self.friends_sub_tab == FriendsSubTab::List;
            let is_reqs = self.friends_sub_tab == FriendsSubTab::Requests;

            // Friends sub-tab button
            let list_text = egui::RichText::new("Friends").size(14.0);
            let list_text = if is_list { list_text.strong() } else { list_text };
            let list_btn = egui::Button::new(list_text)
                .fill(if is_list { self.settings.theme.widget_bg() } else { egui::Color32::TRANSPARENT })
                .rounding(6.0)
                .min_size(egui::vec2(80.0, 28.0));
            if ui.add(list_btn).clicked() {
                self.friends_sub_tab = FriendsSubTab::List;
            }

            // Requests sub-tab button
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
            FriendsSubTab::List => self.draw_friends_list(ui),
            FriendsSubTab::Requests => self.draw_friends_requests(ui),
        }
    }

    fn draw_friends_list(&mut self, ui: &mut egui::Ui) {
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

        // Sort: self first, then rest
        let mut sorted_indices: Vec<usize> = (0..self.contacts.len()).collect();
        sorted_indices.sort_by_key(|&i| if self.contacts[i].pubkey == self.identity.pubkey { 0 } else { 1 });

        let max_height = ui.available_height().max(80.0);
        egui::ScrollArea::vertical()
            .max_height(max_height)
            .id_salt("friends_list_scroll")
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
                            let text = format_peer_display(&contact.nickname, &contact.fingerprint);
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

    fn draw_friends_requests(&mut self, ui: &mut egui::Ui) {
        if self.req_incoming.is_empty() {
            ui.colored_label(self.settings.theme.text_muted(), "No pending requests.");
            return;
        }

        let mut action: Option<(String, RequestAction)> = None;

        let max_height = ui.available_height().max(80.0);
        egui::ScrollArea::vertical()
            .max_height(max_height)
            .id_salt("friends_requests_scroll")
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
                                action = Some((request_id.clone(), RequestAction::Accept));
                            }
                            if ui.button("Reject").clicked() {
                                action = Some((request_id.clone(), RequestAction::Reject));
                            }
                            if ui.add(egui::Button::new(
                                egui::RichText::new("Block").color(self.settings.theme.error()),
                            )).clicked() {
                                action = Some((request_id.clone(), RequestAction::Block));
                            }
                        });
                    });
                    ui.add_space(2.0);
                }
            });

        if let Some((request_id, act)) = action {
            self.handle_request_action(&request_id, act);
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
        let job = peer_display_job(&contact.nickname, &contact.fingerprint, 18.0, self.settings.theme.text_primary(), self.settings.theme.text_dim());
        ui.label(job);
        ui.add_space(6.0);

        ui.horizontal(|ui| {
            ui.label("Fingerprint:");
            ui.monospace(&contact.fingerprint);
        });
        if !contact.last_address.is_empty() {
            ui.horizontal(|ui| {
                ui.label("Last address:");
                let addr_display = if self.show_ips {
                    format!("[{}]:{}", contact.last_address, contact.last_port)
                } else {
                    format!("[{}]:{}", censor_ip(&contact.last_address), contact.last_port)
                };
                ui.monospace(&addr_display);
                let eye = if self.show_ips { "Hide" } else { "Show" };
                if ui.small_button(eye).clicked() {
                    self.show_ips = !self.show_ips;
                }
            });
        }
        ui.horizontal(|ui| {
            ui.label("Calls:");
            ui.label(format!("{}", contact.call_count));
        });

        ui.add_space(8.0);
        ui.horizontal(|ui| {
            let call_btn = egui::Button::new(
                egui::RichText::new("Call").size(16.0).color(egui::Color32::WHITE)
            ).min_size(egui::vec2(120.0, 34.0)).fill(self.settings.theme.btn_positive());
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

            let msg_btn = egui::Button::new(
                egui::RichText::new("Message").size(16.0).color(egui::Color32::WHITE)
            ).min_size(egui::vec2(120.0, 34.0)).fill(self.settings.theme.btn_primary());
            if ui.add(msg_btn).clicked() {
                let cid = contact.contact_id.clone();
                self.viewing_contact = None;
                self.viewing_chat = None;
                self.active_tab = SidebarTab::Messages;
                self.open_msg_chat(&cid);
            }

            let online = self.msg_peer_online.get(&contact.contact_id).copied().unwrap_or(false);
            if !online {
                let find_btn = egui::Button::new(
                    egui::RichText::new("Find Peer").size(14.0)
                ).min_size(egui::vec2(100.0, 34.0));
                if ui.add(find_btn).clicked() {
                    if let Some(tx) = &self.msg_cmd_tx {
                        tx.send(MsgCommand::QueryPeer {
                            target_pubkey: contact.pubkey,
                        }).ok();
                    }
                }
            }
        });

        ui.add_space(10.0);
        ui.separator();
        ui.label(egui::RichText::new("Chat History").strong());

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
                        ui.colored_label(self.settings.theme.text_muted(), "No messages.");
                    }
                    for msg in &history.messages {
                        let time = ChatHistory::format_time(msg.timestamp);
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
                                ui.colored_label(self.settings.theme.chat_peer(), &peer_label);
                                ui.label(&msg.text);
                            });
                        }
                    }
                }
            });
    }

    fn send_contact_request(&mut self) {
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

    fn handle_request_action(&mut self, request_id: &str, action: RequestAction) {
        let ip = self.req_incoming.iter()
            .find(|(rid, ..)| rid == request_id)
            .map(|(_, _, ip, _)| ip.clone())
            .unwrap_or_default();

        match action {
            RequestAction::Accept => {
                if let Some(tx) = &self.msg_cmd_tx {
                    tx.send(MsgCommand::AcceptRequest {
                        request_id: request_id.to_string(),
                    }).ok();
                }
            }
            RequestAction::Reject => {
                if let Some(tx) = &self.msg_cmd_tx {
                    tx.send(MsgCommand::RejectRequest {
                        request_id: request_id.to_string(),
                    }).ok();
                }
            }
            RequestAction::Block => {
                if let Some(tx) = &self.msg_cmd_tx {
                    tx.send(MsgCommand::BlockRequest {
                        request_id: request_id.to_string(),
                        ip,
                    }).ok();
                }
            }
        }

        self.req_incoming.retain(|(rid, ..)| rid != request_id);
    }
}

enum RequestAction {
    Accept,
    Reject,
    Block,
}
