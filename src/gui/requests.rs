use eframe::egui;
use std::net::SocketAddr;

use crate::messaging::MsgCommand;

use super::{HostelApp, censor_ip};

impl HostelApp {
    pub(crate) fn draw_requests_tab(&mut self, ui: &mut egui::Ui) {
        ui.add_space(10.0);
        ui.heading("Contact Requests");
        ui.add_space(10.0);

        // ── Send Request section ──
        ui.label(egui::RichText::new("Send Request").strong());
        ui.add_space(4.0);

        ui.horizontal(|ui| {
            ui.label("IP:");
            ui.add(
                egui::TextEdit::singleline(&mut self.req_ip_input)
                    .hint_text("e.g. ::1 or 2001:db8::1")
                    .desired_width(200.0)
                    .password(!self.show_ips),
            );
            let eye = if self.show_ips { "Hide" } else { "Show" };
            if ui.small_button(eye).clicked() {
                self.show_ips = !self.show_ips;
            }
            ui.label("Port:");
            ui.add(
                egui::TextEdit::singleline(&mut self.req_port_input)
                    .hint_text("9000")
                    .desired_width(80.0),
            );
        });

        ui.add_space(4.0);

        // Status message
        if !self.req_status.is_empty() {
            let color = if self.req_status.starts_with("Error") || self.req_status.starts_with("Failed") {
                egui::Color32::from_rgb(255, 100, 100)
            } else {
                egui::Color32::from_rgb(100, 200, 100)
            };
            ui.colored_label(color, &self.req_status);
        }

        if ui.button("Send Request").clicked() {
            self.send_contact_request();
        }

        ui.add_space(10.0);
        ui.separator();
        ui.add_space(6.0);

        // ── Incoming Requests section ──
        ui.label(egui::RichText::new("Incoming Requests").strong());
        ui.add_space(4.0);

        if self.req_incoming.is_empty() {
            ui.colored_label(egui::Color32::GRAY, "No pending requests.");
            return;
        }

        let mut action: Option<(String, RequestAction)> = None;

        let max_height = ui.available_height().max(80.0);
        egui::ScrollArea::vertical()
            .max_height(max_height)
            .id_salt("requests_incoming_scroll")
            .show(ui, |ui| {
                for (request_id, nickname, ip, fingerprint) in &self.req_incoming {
                    ui.group(|ui| {
                        ui.horizontal(|ui| {
                            ui.vertical(|ui| {
                                let display = if nickname.is_empty() {
                                    format!("#{fingerprint}")
                                } else {
                                    format!("{nickname} #{fingerprint}")
                                };
                                ui.label(
                                    egui::RichText::new(&display).strong(),
                                );
                                let display_ip = if self.show_ips { ip.clone() } else { censor_ip(ip) };
                                ui.colored_label(
                                    egui::Color32::GRAY,
                                    format!("IP: {display_ip}"),
                                );
                            });
                        });
                        ui.add_space(2.0);
                        ui.horizontal(|ui| {
                            if ui.add(egui::Button::new(
                                egui::RichText::new("Accept").color(egui::Color32::from_rgb(80, 200, 80)),
                            )).clicked() {
                                action = Some((request_id.clone(), RequestAction::Accept));
                            }
                            if ui.button("Reject").clicked() {
                                action = Some((request_id.clone(), RequestAction::Reject));
                            }
                            if ui.add(egui::Button::new(
                                egui::RichText::new("Block").color(egui::Color32::from_rgb(255, 80, 80)),
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
                // Try without brackets for IPv4
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
        // Find the IP for blocking
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

        // Remove from local list
        self.req_incoming.retain(|(rid, ..)| rid != request_id);
    }
}

enum RequestAction {
    Accept,
    Reject,
    Block,
}
