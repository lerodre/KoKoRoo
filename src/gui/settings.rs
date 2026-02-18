use eframe::egui;
use crate::identity;
use super::{HostelApp, list_audio_devices, get_adapter_names, get_best_ipv6};

impl HostelApp {
    pub(crate) fn draw_settings_tab(&mut self, ui: &mut egui::Ui) {
        ui.add_space(10.0);
        ui.heading("Settings");
        ui.add_space(10.0);

        // Network mode
        ui.label("Network mode:");
        let prev_mode = self.settings.network_mode;
        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.settings.network_mode, 0, "LAN");
            ui.selectable_value(&mut self.settings.network_mode, 1, "Internet");
        });
        if self.settings.network_mode != prev_mode {
            self.settings.save();
        }

        ui.add_space(8.0);

        // Network adapter
        ui.label("Network adapter:");
        let adapters = get_adapter_names();
        let prev_adapter = self.settings.network_adapter.clone();
        let selected_text = if self.settings.network_adapter.is_empty() {
            "Auto".to_string()
        } else {
            self.settings.network_adapter.clone()
        };
        egui::ComboBox::from_id_salt("settings_adapter").width(300.0)
            .selected_text(&selected_text)
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut self.settings.network_adapter, String::new(), "Auto");
                for name in &adapters {
                    ui.selectable_value(&mut self.settings.network_adapter, name.clone(), name.as_str());
                }
            });
        if self.settings.network_adapter != prev_adapter {
            self.settings.save();
            self.best_ipv6 = get_best_ipv6(&self.settings.network_adapter);
        }

        ui.add_space(8.0);
        ui.separator();
        ui.add_space(4.0);

        // Microphone
        ui.label("Microphone:");
        let prev_input = self.selected_input;
        egui::ComboBox::from_id_salt("settings_mic").width(300.0)
            .selected_text(self.devices.input_names.get(self.selected_input).map(|s| s.as_str()).unwrap_or("none"))
            .show_ui(ui, |ui| {
                for (i, name) in self.devices.input_names.iter().enumerate() {
                    ui.selectable_value(&mut self.selected_input, i, name.as_str());
                }
            });
        if self.selected_input != prev_input {
            self.settings.mic = self.devices.input_names.get(self.selected_input)
                .cloned().unwrap_or_default();
            self.settings.save();
        }

        // Speakers
        ui.label("Speakers:");
        let prev_output = self.selected_output;
        egui::ComboBox::from_id_salt("settings_spk").width(300.0)
            .selected_text(self.devices.output_names.get(self.selected_output).map(|s| s.as_str()).unwrap_or("none"))
            .show_ui(ui, |ui| {
                for (i, name) in self.devices.output_names.iter().enumerate() {
                    ui.selectable_value(&mut self.selected_output, i, name.as_str());
                }
            });
        if self.selected_output != prev_output {
            self.settings.speakers = self.devices.output_names.get(self.selected_output)
                .cloned().unwrap_or_default();
            self.settings.save();
        }

        ui.add_space(8.0);
        ui.separator();
        ui.add_space(4.0);

        // Local port
        ui.label("Local port:");
        let resp = ui.add(egui::TextEdit::singleline(&mut self.local_port).desired_width(120.0));
        if resp.lost_focus() {
            self.settings.local_port = self.local_port.clone();
            self.settings.save();
        }

        ui.add_space(10.0);
        if ui.button("Refresh Audio Devices").clicked() {
            self.devices = list_audio_devices();
            // Re-match saved names
            self.selected_input = if !self.settings.mic.is_empty() {
                self.devices.input_names.iter().position(|n| n == &self.settings.mic).unwrap_or(0)
            } else {
                0
            };
            self.selected_output = if !self.settings.speakers.is_empty() {
                self.devices.output_names.iter().position(|n| n == &self.settings.speakers).unwrap_or(0)
            } else {
                0
            };
        }

        // ── Blocked Contacts ──
        ui.add_space(8.0);
        ui.separator();
        ui.add_space(4.0);
        ui.label(egui::RichText::new("Blocked Contacts").strong());

        if self.settings.blocked.is_empty() {
            ui.colored_label(egui::Color32::GRAY, "No blocked contacts.");
        } else {
            let contacts = identity::load_all_contacts();
            let mut unblock_hex: Option<String> = None;
            let mut unblock_ip: Option<String> = None;

            for hex in &self.settings.blocked {
                let (display, ip) = if let Some(c) = contacts.iter().find(|c| identity::pubkey_hex(&c.pubkey) == *hex) {
                    let name = if c.nickname.is_empty() {
                        c.fingerprint.clone()
                    } else {
                        format!("{} #{}", c.nickname, c.fingerprint)
                    };
                    (name, c.last_address.clone())
                } else {
                    // Contact file deleted but hex still in blocked list
                    let short = if hex.len() > 16 { &hex[..16] } else { hex.as_str() };
                    (format!("{}...", short), String::new())
                };

                ui.horizontal(|ui| {
                    ui.label(&display);
                    if ui.small_button("Unblock").clicked() {
                        unblock_hex = Some(hex.clone());
                        if !ip.is_empty() {
                            unblock_ip = Some(ip.clone());
                        }
                    }
                });
            }

            if let Some(hex) = unblock_hex {
                self.settings.unblock_contact(&hex);
                if let Some(ip) = unblock_ip {
                    self.settings.unban_ip(&ip);
                }
            }
        }

        // ── Banned IPs ──
        ui.add_space(8.0);
        ui.separator();
        ui.add_space(4.0);
        ui.label(egui::RichText::new("Banned IPs").strong());

        // Merge persisted bans with runtime auto-bans
        let mut all_ips = self.settings.banned_ips.clone();
        if let Some(ref auto_ips) = self.auto_banned_ips {
            if let Ok(runtime) = auto_ips.lock() {
                for ip in runtime.iter() {
                    if !all_ips.contains(ip) {
                        // Persist newly auto-banned IPs
                        self.settings.ban_ip(ip);
                        all_ips.push(ip.clone());
                    }
                }
            }
        }

        if all_ips.is_empty() {
            ui.colored_label(egui::Color32::GRAY, "No banned IPs.");
        } else {
            let mut unban_ip: Option<String> = None;
            for ip in &all_ips {
                ui.horizontal(|ui| {
                    ui.monospace(ip);
                    if ui.small_button("Unban").clicked() {
                        unban_ip = Some(ip.clone());
                    }
                });
            }
            if let Some(ip) = unban_ip {
                self.settings.unban_ip(&ip);
            }
        }
    }
}
