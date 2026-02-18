use eframe::egui;
use crate::identity;
use crate::theme::Theme;
use super::{HostelApp, list_audio_devices, get_adapter_names, get_best_ipv6, censor_ip, peer_display_job};

impl HostelApp {
    pub(crate) fn draw_settings_tab(&mut self, ui: &mut egui::Ui) {
        let settings_start = std::time::Instant::now();

        ui.add_space(10.0);
        ui.heading("Settings");
        ui.add_space(10.0);

        // Network adapter — use cached list, only refresh on demand
        ui.label("Network adapter:");
        if self.adapter_names.is_empty() {
            let t = std::time::Instant::now();
            self.adapter_names = get_adapter_names();
            log_fmt!("[settings] get_adapter_names() took {}ms ({} adapters)",
                t.elapsed().as_millis(), self.adapter_names.len());
        }
        let adapters = &self.adapter_names;
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
                for name in adapters {
                    ui.selectable_value(&mut self.settings.network_adapter, name.clone(), name.as_str());
                }
            });
        if self.settings.network_adapter != prev_adapter {
            self.settings.save();
            let t = std::time::Instant::now();
            self.best_ipv6 = get_best_ipv6(&self.settings.network_adapter);
            log_fmt!("[settings] get_best_ipv6() took {}ms", t.elapsed().as_millis());
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
        ui.label("Local port (UDP):");
        ui.horizontal(|ui| {
            let port_edit = egui::TextEdit::singleline(&mut self.local_port).desired_width(120.0);
            let resp = egui::Frame::none()
                .stroke(egui::Stroke::new(1.0, self.settings.theme.separator()))
                .inner_margin(0.0)
                .show(ui, |ui| ui.add(port_edit)).inner;

            let save_clicked = ui.button("Save Port").clicked();

            if save_clicked || resp.lost_focus() {
                let changed = self.settings.local_port != self.local_port;
                self.settings.local_port = self.local_port.clone();
                self.settings.save();
                self.port_saved_at = Some(std::time::Instant::now());
                // Trigger firewall prompt if port changed from what the rule covers
                if changed && self.local_port != self.settings.firewall_port {
                    self.firewall_old_port = self.settings.firewall_port.clone();
                    self.show_firewall_prompt = true;
                }
            }

            // Show "Saved" feedback for 3 seconds
            if let Some(saved_at) = self.port_saved_at {
                if saved_at.elapsed().as_secs_f32() < 3.0 {
                    ui.colored_label(self.settings.theme.accent(), "Saved");
                    ui.ctx().request_repaint_after(std::time::Duration::from_secs(1));
                } else {
                    self.port_saved_at = None;
                }
            }
        });

        ui.add_space(10.0);
        if ui.button("Refresh Audio Devices").clicked() {
            let t = std::time::Instant::now();
            self.devices = list_audio_devices();
            log_fmt!("[settings] list_audio_devices() took {}ms", t.elapsed().as_millis());
            let t2 = std::time::Instant::now();
            self.adapter_names = get_adapter_names();
            log_fmt!("[settings] get_adapter_names() refresh took {}ms", t2.elapsed().as_millis());
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
            ui.colored_label(self.settings.theme.text_muted(), "No blocked contacts.");
        } else {
            let contacts = identity::load_all_contacts();
            let mut unblock_hex: Option<String> = None;
            let mut unblock_ip: Option<String> = None;

            for hex in &self.settings.blocked {
                let (nick, fp, ip) = if let Some(c) = contacts.iter().find(|c| identity::pubkey_hex(&c.pubkey) == *hex) {
                    (c.nickname.clone(), c.fingerprint.clone(), c.last_address.clone())
                } else {
                    let short = if hex.len() > 16 { &hex[..16] } else { hex.as_str() };
                    (String::new(), format!("{}...", short), String::new())
                };

                ui.horizontal(|ui| {
                    ui.label(peer_display_job(&nick, &fp, 13.0, self.settings.theme.text_primary(), self.settings.theme.text_dim()));
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
            ui.colored_label(self.settings.theme.text_muted(), "No banned IPs.");
        } else {
            let mut unban_ip: Option<String> = None;
            for ip in &all_ips {
                ui.horizontal(|ui| {
                    let display_ip = if self.show_ips { ip.clone() } else { censor_ip(ip) };
                    ui.monospace(&display_ip);
                    if ui.small_button("Unban").clicked() {
                        unban_ip = Some(ip.clone());
                    }
                });
            }
            if let Some(ip) = unban_ip {
                self.settings.unban_ip(&ip);
            }
        }

        let total_ms = settings_start.elapsed().as_millis();
        if total_ms > 16 {
            log_fmt!("[settings] draw_settings_tab took {}ms (slow!)", total_ms);
        }
    }

    pub(crate) fn draw_appearance_tab(&mut self, ui: &mut egui::Ui) {
        ui.add_space(10.0);
        ui.heading("Colors");
        ui.add_space(10.0);

        // Initialize hex inputs if needed
        if self.color_hex_inputs.is_empty() {
            for (name, _, rgb) in self.settings.theme.all_entries() {
                self.color_hex_inputs.insert(name.to_string(), Theme::to_hex(rgb));
            }
        }

        let mut updates: Vec<(&'static str, [u8; 3])> = Vec::new();
        let mut randomize = false;
        let mut reset = false;

        ui.horizontal(|ui| {
            if ui.button("Randomize").clicked() {
                randomize = true;
            }
            if ui.button("Reset to Defaults").clicked() {
                reset = true;
            }
        });

        ui.add_space(8.0);

        let entries = self.settings.theme.all_entries();
        let max_height = ui.available_height().max(80.0);
        egui::ScrollArea::vertical()
            .max_height(max_height)
            .id_salt("appearance_colors_scroll")
            .show(ui, |ui| {
                for (name, label, rgb) in &entries {
                    ui.horizontal(|ui| {
                        // Lock/Unlock button
                        let locked = self.color_locks.contains(*name);
                        if ui.small_button(if locked { "Unlock" } else { "Lock  " }).clicked() {
                            if locked {
                                self.color_locks.remove(*name);
                            } else {
                                self.color_locks.insert(name.to_string());
                            }
                        }

                        // Color picker (click to open color wheel)
                        let mut c = egui::Color32::from_rgb(rgb[0], rgb[1], rgb[2]);
                        let prev = c;
                        egui::color_picker::color_edit_button_srgba(ui, &mut c, egui::color_picker::Alpha::Opaque);
                        if c != prev {
                            updates.push((*name, [c.r(), c.g(), c.b()]));
                        }

                        // Label
                        ui.label(*label);

                        // Hex input
                        let hex = self.color_hex_inputs.entry(name.to_string())
                            .or_insert_with(|| Theme::to_hex(*rgb));
                        let resp = ui.add(
                            egui::TextEdit::singleline(hex)
                                .desired_width(80.0)
                                .char_limit(7),
                        );
                        if resp.lost_focus() {
                            if let Some(new_rgb) = Theme::from_hex(hex) {
                                updates.push((*name, new_rgb));
                            } else {
                                *hex = Theme::to_hex(*rgb);
                            }
                        }
                    });
                    ui.add_space(2.0);
                }
            });

        // Apply smart randomize
        if randomize {
            self.settings.theme.smart_randomize(&self.color_locks);
            for (name, _, rgb) in self.settings.theme.all_entries() {
                self.color_hex_inputs.insert(name.to_string(), Theme::to_hex(rgb));
            }
        }

        // Apply deferred updates
        for (name, rgb) in &updates {
            self.settings.theme.set_by_name(name, *rgb);
            self.color_hex_inputs.insert(name.to_string(), Theme::to_hex(*rgb));
        }

        // Apply reset (respects locks)
        if reset {
            let default_theme = Theme::default();
            for (name, _, rgb) in default_theme.all_entries() {
                if !self.color_locks.contains(name) {
                    self.settings.theme.set_by_name(name, rgb);
                    self.color_hex_inputs.insert(name.to_string(), Theme::to_hex(rgb));
                }
            }
        }

        if !updates.is_empty() || reset || randomize {
            self.settings.save();
        }
    }
}
