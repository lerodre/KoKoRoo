use eframe::egui;
use std::sync::atomic::Ordering;
use std::time::Instant;
use crate::chat::ChatHistory;
use crate::identity;
use crate::screen::{ScreenCommand, ScreenQuality};
use super::{HostelApp, Screen, format_peer_display, peer_display_job, censor_ip, load_icon_texture_sized};

impl HostelApp {
    pub(crate) fn draw_call_tab(&mut self, ui: &mut egui::Ui) {
        ui.add_space(10.0);
        ui.heading("Call");
        ui.add_space(10.0);

        ui.label("Peer IPv6:");
        ui.horizontal(|ui| {
            let ip_edit = egui::TextEdit::singleline(&mut self.peer_ip)
                .desired_width(300.0)
                .password(!self.show_ips);
            let ip_frame = egui::Frame::none()
                .stroke(egui::Stroke::new(1.0, self.settings.theme.separator()))
                .inner_margin(0.0);
            ip_frame.show(ui, |ui| { ui.add(ip_edit); });
            let eye = if self.show_ips { "Hide" } else { "Show" };
            if ui.small_button(eye).clicked() {
                self.show_ips = !self.show_ips;
            }
        });
        ui.label("Peer port:");
        let port_edit = egui::TextEdit::singleline(&mut self.peer_port).desired_width(120.0);
        egui::Frame::none()
            .stroke(egui::Stroke::new(1.0, self.settings.theme.separator()))
            .inner_margin(0.0)
            .show(ui, |ui| { ui.add(port_edit); });

        ui.add_space(12.0);
        let call_tex = self.call_icon_texture.get_or_insert_with(|| {
            load_icon_texture_sized(ui.ctx(), "icon-call", include_bytes!("../../assets/call.png"), 48)
        }).clone();
        let icon_h = 28.0;
        let icon_aspect = call_tex.size()[0] as f32 / call_tex.size()[1] as f32;
        let icon_sized = egui::load::SizedTexture::new(call_tex.id(), egui::vec2(icon_h * icon_aspect, icon_h));
        let btn = egui::Button::image_and_text(
            icon_sized,
            egui::RichText::new("Call").size(20.0).color(egui::Color32::WHITE),
        )
        .min_size(egui::vec2(150.0, 42.0))
        .fill(self.settings.theme.btn_positive());
        if ui.add(btn).clicked() {
            self.start_call();
        }

        // ── Contacts quick-dial ──
        ui.add_space(10.0);
        ui.separator();
        ui.add_space(6.0);

        ui.horizontal(|ui| {
            ui.heading("Contacts");
            let search_edit = egui::TextEdit::singleline(&mut self.contact_search)
                .hint_text("Search...")
                .desired_width(150.0);
            egui::Frame::none()
                .stroke(egui::Stroke::new(1.0, self.settings.theme.separator()))
                .inner_margin(2.0)
                .show(ui, |ui| { ui.add(search_edit); });
        });
        ui.add_space(4.0);

        if self.contacts.is_empty() {
            ui.colored_label(self.settings.theme.text_muted(), "No contacts yet. Make a call to add one.");
            return;
        }

        let search = self.contact_search.to_lowercase();
        let mut selected_contact: Option<usize> = None;

        // Sort: self first, then rest
        let mut sorted_indices: Vec<usize> = (0..self.contacts.len()).collect();
        sorted_indices.sort_by_key(|&i| if self.contacts[i].pubkey == self.identity.pubkey { 0 } else { 1 });

        let max_height = ui.available_height().max(80.0);
        egui::ScrollArea::vertical()
            .max_height(max_height)
            .id_salt("call_contacts_scroll")
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

                    let has_addr = !contact.last_address.is_empty();

                    ui.horizontal(|ui| {
                        // Presence indicator
                        let presence = self.msg_peer_presence.get(&contact.contact_id)
                            .copied()
                            .unwrap_or_else(|| {
                                if self.msg_peer_online.get(&contact.contact_id).copied().unwrap_or(false) {
                                    crate::messaging::PresenceStatus::Online
                                } else {
                                    crate::messaging::PresenceStatus::Offline
                                }
                            });
                        let dot_color = match presence {
                            crate::messaging::PresenceStatus::Online => egui::Color32::from_rgb(0x4C, 0xAF, 0x50),
                            crate::messaging::PresenceStatus::Away => egui::Color32::from_rgb(0xFF, 0xC1, 0x07),
                            crate::messaging::PresenceStatus::Offline => self.settings.theme.text_muted(),
                        };
                        let (rect, _) = ui.allocate_exact_size(egui::vec2(8.0, 8.0), egui::Sense::hover());
                        ui.painter().circle_filled(rect.center(), 4.0, dot_color);

                        let is_self = contact.pubkey == self.identity.pubkey;
                        let display_nick = if is_self { "YO" } else { &contact.nickname };

                        let btn_widget = if is_self {
                            egui::Button::new(egui::RichText::new("YO (you)").color(self.settings.theme.text_muted()).italics()).frame(false)
                        } else if has_addr {
                            egui::Button::new(peer_display_job(display_nick, &contact.fingerprint, 13.0, self.settings.theme.text_primary(), self.settings.theme.text_dim())).frame(false)
                        } else {
                            let text = format_peer_display(display_nick, &contact.fingerprint);
                            egui::Button::new(egui::RichText::new(&text).color(self.settings.theme.text_muted())).frame(false)
                        };
                        if ui.add(btn_widget).clicked() && has_addr && !is_self {
                            selected_contact = Some(i);
                        }
                    });
                    ui.separator();
                }
            });

        if let Some(i) = selected_contact {
            let contact = &self.contacts[i];
            self.peer_ip = contact.last_address.clone();
            if !contact.last_port.is_empty() {
                self.peer_port = contact.last_port.clone();
            }
        }
    }

    pub(crate) fn draw_connecting(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        ui.vertical_centered(|ui| {
            ui.add_space(60.0);
            ui.heading("Connecting...");
            ui.add_space(15.0);
            ui.spinner();
            ui.add_space(15.0);
            ui.label("Key exchange + identity verification");
            let conn_ip = if self.show_ips { self.peer_ip.clone() } else { censor_ip(&self.peer_ip) };
            ui.label(format!("Peer: [{}]:{}", conn_ip, self.peer_port));
            ui.add_space(20.0);
            let btn = egui::Button::new(egui::RichText::new("Cancel").size(16.0))
                .min_size(egui::vec2(120.0, 34.0))
                .fill(self.settings.theme.btn_negative());
            if ui.add(btn).clicked() {
                self.running.store(false, Ordering::Relaxed);
                self.cleanup_call();
            }
        });
        ctx.request_repaint_after(std::time::Duration::from_millis(200));
    }

    pub(crate) fn draw_key_warning(&mut self, ui: &mut egui::Ui) {
        let warning_text = self.key_change_warning.clone().unwrap_or_default();

        ui.vertical_centered(|ui| {
            ui.add_space(30.0);
            ui.colored_label(
                self.settings.theme.error(),
                egui::RichText::new("SECURITY WARNING").size(28.0).strong(),
            );
            ui.add_space(15.0);
        });

        ui.add_space(5.0);
        ui.colored_label(
            self.settings.theme.error(),
            egui::RichText::new(&warning_text).size(15.0).strong(),
        );

        ui.add_space(15.0);
        ui.separator();
        ui.add_space(10.0);

        ui.horizontal(|ui| {
            ui.label("Peer:");
            ui.label(peer_display_job(&self.peer_nickname, &self.peer_fingerprint, 14.0, self.settings.theme.text_primary(), self.settings.theme.text_dim()));
        });
        ui.horizontal(|ui| {
            ui.label("Verify code:");
            ui.colored_label(
                self.settings.theme.warning(),
                egui::RichText::new(&self.verification_code).size(18.0).strong(),
            );
        });

        ui.add_space(15.0);
        ui.label("Possible reasons:");
        ui.label("  - The peer reinstalled the app or changed devices");
        ui.label("  - Someone is impersonating the peer (MITM attack)");
        ui.add_space(5.0);
        ui.colored_label(
            self.settings.theme.warning(),
            "Verify the code above with your peer through a trusted channel.",
        );

        ui.add_space(25.0);
        ui.vertical_centered(|ui| {
            ui.horizontal(|ui| {
                let proceed_btn = egui::Button::new(
                    egui::RichText::new("Proceed (Trust)").size(18.0).color(egui::Color32::WHITE),
                )
                .min_size(egui::vec2(180.0, 44.0))
                .fill(self.settings.theme.btn_positive());

                if ui.add(proceed_btn).clicked() {
                    if let Some(contact) = self.pending_contact.take() {
                        identity::save_contact(&contact);
                    }
                    self.key_change_warning = None;
                    self.screen = Screen::InCall;
                }

                ui.add_space(20.0);

                let reject_btn = egui::Button::new(
                    egui::RichText::new("Reject (Hang Up)").size(18.0).color(egui::Color32::WHITE),
                )
                .min_size(egui::vec2(180.0, 44.0))
                .fill(self.settings.theme.btn_negative());

                if ui.add(reject_btn).clicked() {
                    self.pending_contact = None;
                    self.hang_up();
                }
            });
        });
    }

    pub(crate) fn draw_call(&mut self, ui: &mut egui::Ui) {
        let mic_on = self.mic_active.load(Ordering::Relaxed);
        let has_video = self.screen_texture.is_some();

        // ── Top bar: status ──
        ui.horizontal(|ui| {
            ui.heading("hostelD");
            ui.separator();
            ui.colored_label(self.settings.theme.accent(), "ENCRYPTED");
            ui.separator();
            ui.label("Peer:");
            ui.label(peer_display_job(&self.peer_nickname, &self.peer_fingerprint, 13.0, self.settings.theme.text_primary(), self.settings.theme.text_dim()));
        });

        ui.separator();

        // ── Info row ──
        ui.horizontal(|ui| {
            ui.label("Verify:");
            ui.colored_label(
                self.settings.theme.warning(),
                egui::RichText::new(&self.verification_code).size(18.0).strong(),
            );
            ui.separator();
            ui.label("Opus 64kbps");
            ui.separator();
        });

        ui.add_space(4.0);

        // ── Controls row ──
        ui.horizontal(|ui| {
            let (btn_text, btn_color) = if mic_on {
                ("Mic: ON", self.settings.theme.btn_positive())
            } else {
                ("Mic: MUTED", self.settings.theme.btn_negative())
            };
            let mic_btn = egui::Button::new(
                egui::RichText::new(btn_text).size(16.0).color(egui::Color32::WHITE)
            ).min_size(egui::vec2(120.0, 35.0)).fill(btn_color);
            if ui.add(mic_btn).clicked() {
                self.mic_active.store(!mic_on, Ordering::Relaxed);
            }

            let (scr_text, scr_color) = if self.screen_sharing {
                ("Screen: ON", self.settings.theme.btn_primary())
            } else {
                ("Screen: OFF", self.settings.theme.btn_neutral())
            };
            let scr_btn = egui::Button::new(
                egui::RichText::new(scr_text).size(16.0).color(egui::Color32::WHITE)
            ).min_size(egui::vec2(130.0, 35.0)).fill(scr_color);
            if ui.add(scr_btn).clicked() {
                if self.screen_sharing {
                    self.screen_sharing = false;
                    if let Some(tx) = &self.screen_cmd_tx {
                        let _ = tx.send(ScreenCommand::Stop);
                    }
                } else {
                    // Mutual exclusion: stop webcam if active
                    if self.webcam_sharing {
                        self.webcam_sharing = false;
                        if let Some(tx) = &self.screen_cmd_tx {
                            let _ = tx.send(ScreenCommand::Stop);
                        }
                    }
                    if self.loopback_devices.is_empty() {
                        self.loopback_devices = crate::audio::list_loopback_devices();
                    }
                    self.display_names = crate::screen::list_displays();
                    self.show_screen_popup = true;
                }
            }

            let (cam_text, cam_color) = if self.webcam_sharing {
                ("Cam: ON", self.settings.theme.btn_positive())
            } else {
                ("Cam: OFF", self.settings.theme.btn_neutral())
            };
            let cam_tex = self.enablecam_icon_texture.get_or_insert_with(|| {
                load_icon_texture_sized(ui.ctx(), "icon-enablecam", include_bytes!("../../assets/enablecam.png"), 48)
            }).clone();
            let cam_icon_h = 20.0;
            let cam_icon_aspect = cam_tex.size()[0] as f32 / cam_tex.size()[1] as f32;
            let cam_icon_sized = egui::load::SizedTexture::new(cam_tex.id(), egui::vec2(cam_icon_h * cam_icon_aspect, cam_icon_h));
            let cam_btn = egui::Button::image_and_text(
                cam_icon_sized,
                egui::RichText::new(cam_text).size(16.0).color(egui::Color32::WHITE),
            ).min_size(egui::vec2(120.0, 35.0)).fill(cam_color);
            if ui.add(cam_btn).clicked() {
                if self.webcam_sharing {
                    self.webcam_sharing = false;
                    if let Some(tx) = &self.screen_cmd_tx {
                        let _ = tx.send(ScreenCommand::Stop);
                    }
                } else {
                    // Mutual exclusion: stop screen share if active
                    if self.screen_sharing {
                        self.screen_sharing = false;
                        if let Some(tx) = &self.screen_cmd_tx {
                            let _ = tx.send(ScreenCommand::Stop);
                        }
                    }
                    self.camera_names = crate::screen::list_cameras();
                    self.show_webcam_popup = true;
                }
            }

            let hangup_btn = egui::Button::new(
                egui::RichText::new("Hang Up").size(16.0).color(egui::Color32::WHITE)
            ).min_size(egui::vec2(100.0, 35.0)).fill(self.settings.theme.btn_negative());
            if ui.add(hangup_btn).clicked() {
                self.show_hangup_confirm = true;
            }

            if has_video {
                let remaining = ui.available_width() - 250.0;
                if remaining > 0.0 {
                    ui.add_space(remaining);
                }
                let end_btn = egui::Button::new(
                    egui::RichText::new("End Viewing").size(14.0).color(egui::Color32::WHITE)
                ).min_size(egui::vec2(110.0, 35.0)).fill(egui::Color32::from_rgb(140, 80, 40));
                if ui.add(end_btn).clicked() {
                    self.screen_texture = None;
                    self.last_frame_time = None;
                }
                let fs_btn = egui::Button::new(
                    egui::RichText::new("Fullscreen").size(14.0).color(egui::Color32::WHITE)
                ).min_size(egui::vec2(110.0, 35.0)).fill(egui::Color32::from_rgb(80, 80, 120));
                if ui.add(fs_btn).clicked() {
                    self.video_fullscreen = true;
                    self.is_fullscreen = true;
                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::Fullscreen(true));
                }
            }
        });

        // ── Screen share config popup ──
        if self.show_screen_popup {
            let mut open = true;
            egui::Window::new("Screen Share")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .open(&mut open)
                .show(ui.ctx(), |ui| {
                    ui.label("Display:");
                    let display_label = self.display_names.get(self.selected_display)
                        .cloned().unwrap_or_else(|| "Display 1".to_string());
                    egui::ComboBox::from_id_salt("popup_display")
                        .width(200.0)
                        .selected_text(&display_label)
                        .show_ui(ui, |ui| {
                            for (i, name) in self.display_names.iter().enumerate() {
                                ui.selectable_value(&mut self.selected_display, i, name.as_str());
                            }
                        });

                    #[cfg(target_os = "linux")]
                    if crate::screen::wayland::is_wayland() {
                        ui.colored_label(
                            egui::Color32::from_rgb(180, 180, 255),
                            "Display will be selected via system dialog",
                        );
                    }

                    ui.add_space(4.0);
                    ui.label("Quality:");
                    let current_label = ScreenQuality::ALL[self.selected_screen_quality].label();
                    egui::ComboBox::from_id_salt("popup_quality")
                        .width(160.0)
                        .selected_text(current_label)
                        .show_ui(ui, |ui| {
                            for (i, q) in ScreenQuality::ALL.iter().enumerate() {
                                ui.selectable_value(&mut self.selected_screen_quality, i, q.label());
                            }
                        });

                    ui.add_space(4.0);
                    ui.label("System Audio:");
                    let audio_label = match self.selected_audio_device {
                        0 => "None".to_string(),
                        1 => "Default".to_string(),
                        n => self.loopback_devices.get(n - 2)
                            .cloned().unwrap_or_else(|| "Unknown".to_string()),
                    };
                    egui::ComboBox::from_id_salt("popup_audio")
                        .width(240.0)
                        .selected_text(&audio_label)
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut self.selected_audio_device, 0, "None");
                            ui.selectable_value(&mut self.selected_audio_device, 1, "Default");
                            for (i, name) in self.loopback_devices.iter().enumerate() {
                                ui.selectable_value(&mut self.selected_audio_device, i + 2, name.as_str());
                            }
                        });

                    ui.add_space(8.0);
                    let share_btn = egui::Button::new(
                        egui::RichText::new("Share Screen").size(16.0).color(egui::Color32::WHITE)
                    ).min_size(egui::vec2(160.0, 35.0)).fill(self.settings.theme.btn_primary());
                    if ui.add(share_btn).clicked() {
                        let quality = ScreenQuality::ALL[self.selected_screen_quality];
                        let audio_device = match self.selected_audio_device {
                            0 => None,
                            1 => Some(String::new()),
                            n => self.loopback_devices.get(n - 2).cloned(),
                        };
                        if let Some(tx) = &self.screen_cmd_tx {
                            let _ = tx.send(ScreenCommand::StartScreen { quality, audio_device, display_index: self.selected_display });
                        }
                        self.screen_sharing = true;
                        self.show_screen_popup = false;
                    }
                });
            if !open {
                self.show_screen_popup = false;
            }
        }

        // ── Webcam config popup ──
        if self.show_webcam_popup {
            let mut open = true;
            egui::Window::new("Webcam")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .open(&mut open)
                .show(ui.ctx(), |ui| {
                    ui.label("Camera:");
                    if self.camera_names.is_empty() {
                        ui.colored_label(self.settings.theme.error(), "No cameras found");
                    } else {
                        let cam_label = self.camera_names.get(self.selected_camera)
                            .cloned().unwrap_or_else(|| "Camera 0".to_string());
                        egui::ComboBox::from_id_salt("popup_camera")
                            .width(240.0)
                            .selected_text(&cam_label)
                            .show_ui(ui, |ui| {
                                for (i, name) in self.camera_names.iter().enumerate() {
                                    ui.selectable_value(&mut self.selected_camera, i, name.as_str());
                                }
                            });
                    }

                    ui.add_space(4.0);
                    ui.label("Quality:");
                    let current_label = ScreenQuality::ALL[self.selected_screen_quality].label();
                    egui::ComboBox::from_id_salt("popup_cam_quality")
                        .width(160.0)
                        .selected_text(current_label)
                        .show_ui(ui, |ui| {
                            for (i, q) in ScreenQuality::ALL.iter().enumerate() {
                                ui.selectable_value(&mut self.selected_screen_quality, i, q.label());
                            }
                        });

                    ui.add_space(8.0);
                    let can_start = !self.camera_names.is_empty();
                    let start_btn = egui::Button::new(
                        egui::RichText::new("Start Camera").size(16.0).color(egui::Color32::WHITE)
                    ).min_size(egui::vec2(160.0, 35.0)).fill(
                        if can_start { self.settings.theme.btn_positive() }
                        else { self.settings.theme.btn_neutral() }
                    );
                    if ui.add_enabled(can_start, start_btn).clicked() {
                        let quality = ScreenQuality::ALL[self.selected_screen_quality];
                        if let Some(tx) = &self.screen_cmd_tx {
                            let _ = tx.send(ScreenCommand::StartWebcam { quality, device_index: self.selected_camera });
                        }
                        self.webcam_sharing = true;
                        self.show_webcam_popup = false;
                    }
                });
            if !open {
                self.show_webcam_popup = false;
            }
        }

        // ── Hang up confirmation ──
        if self.show_hangup_confirm {
            let mut open = true;
            egui::Window::new("Hang Up")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .open(&mut open)
                .show(ui.ctx(), |ui| {
                    ui.add_space(4.0);
                    ui.label("Are you sure you want to disconnect?");
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        let yes_btn = egui::Button::new(
                            egui::RichText::new("Yes").size(15.0).color(egui::Color32::WHITE)
                        ).min_size(egui::vec2(80.0, 32.0)).fill(self.settings.theme.btn_negative());
                        if ui.add(yes_btn).clicked() {
                            self.show_hangup_confirm = false;
                            if self.is_fullscreen || self.video_fullscreen {
                                ui.ctx().send_viewport_cmd(egui::ViewportCommand::Fullscreen(false));
                                self.is_fullscreen = false;
                                self.video_fullscreen = false;
                            }
                            self.hang_up();
                        }
                        let no_btn = egui::Button::new(
                            egui::RichText::new("Cancel").size(15.0)
                        ).min_size(egui::vec2(80.0, 32.0));
                        if ui.add(no_btn).clicked() {
                            self.show_hangup_confirm = false;
                        }
                    });
                });
            if !open {
                self.show_hangup_confirm = false;
            }
        }

        // ── Screen viewer ──
        let mut video_w: u32 = 1280;
        let mut video_h: u32 = 720;
        if let Some(viewer) = &self.screen_viewer {
            if let Ok(mut v) = viewer.lock() {
                video_w = v.frame_width;
                video_h = v.frame_height;
                if let Some(rgba_frame) = v.take_frame() {
                    self.last_frame_time = Some(Instant::now());
                    let image = egui::ColorImage::from_rgba_unmultiplied(
                        [video_w as usize, video_h as usize],
                        &rgba_frame,
                    );
                    self.screen_texture = Some(
                        ui.ctx().load_texture("screen_share", image, Default::default())
                    );
                } else if v.stopped && self.screen_texture.is_some() {
                    // Peer stopped sharing — clear stale texture
                    self.screen_texture = None;
                    self.last_frame_time = None;
                }
            }
        }
        // ── Video + Chat layout ──
        if has_video {
            ui.separator();
            let chat_w = 280.0_f32;
            let available = ui.available_rect_before_wrap();
            let actual_chat_w = chat_w.min(available.width() * 0.4);
            let video_area_w = (available.width() - actual_chat_w - 4.0).max(100.0);

            let video_rect = egui::Rect::from_min_size(
                available.min,
                egui::vec2(video_area_w, available.height()),
            );
            let chat_rect = egui::Rect::from_min_size(
                egui::pos2(available.min.x + video_area_w + 4.0, available.min.y),
                egui::vec2(actual_chat_w, available.height()),
            );

            // Video on the left
            ui.allocate_new_ui(egui::UiBuilder::new().max_rect(video_rect), |ui| {
                if let Some(tex) = &self.screen_texture {
                    let avail_w = ui.available_width();
                    let avail_h = ui.available_height();
                    let aspect = video_w as f32 / video_h as f32;
                    let (dw, dh) = {
                        let h_from_w = avail_w / aspect;
                        if h_from_w <= avail_h {
                            (avail_w, h_from_w)
                        } else {
                            (avail_h * aspect, avail_h)
                        }
                    };
                    let pad = (avail_w - dw).max(0.0) / 2.0;
                    ui.horizontal(|ui| {
                        ui.add_space(pad);
                        ui.image(egui::load::SizedTexture::new(
                            tex.id(),
                            egui::vec2(dw, dh),
                        ));
                    });
                }
            });

            // Chat on the right
            ui.allocate_new_ui(egui::UiBuilder::new().max_rect(chat_rect), |ui| {
                self.draw_chat(ui);
            });
        } else {
            // No video: chat below
            ui.separator();
            self.draw_chat(ui);
        }
    }

    pub(crate) fn draw_fullscreen_video(&mut self, ui: &mut egui::Ui) {
        let mut video_w: u32 = 1280;
        let mut video_h: u32 = 720;
        if let Some(viewer) = &self.screen_viewer {
            if let Ok(mut v) = viewer.lock() {
                video_w = v.frame_width;
                video_h = v.frame_height;
                if let Some(rgba_frame) = v.take_frame() {
                    self.last_frame_time = Some(Instant::now());
                    let image = egui::ColorImage::from_rgba_unmultiplied(
                        [video_w as usize, video_h as usize],
                        &rgba_frame,
                    );
                    self.screen_texture = Some(
                        ui.ctx().load_texture("screen_share", image, Default::default())
                    );
                }
            }
        }

        if let Some(tex) = &self.screen_texture {
            let available_width = ui.available_width();
            let available_height = ui.available_height();
            let aspect = video_w as f32 / video_h as f32;
            let (display_w, display_h) = {
                let w_from_width = available_width;
                let h_from_width = available_width / aspect;
                let h_from_height = available_height;
                let w_from_height = available_height * aspect;
                if h_from_width <= available_height {
                    (w_from_width, h_from_width)
                } else {
                    (w_from_height, h_from_height)
                }
            };
            let pad_x = (available_width - display_w).max(0.0) / 2.0;
            let pad_y = (available_height - display_h).max(0.0) / 2.0;
            ui.add_space(pad_y);
            ui.horizontal(|ui| {
                ui.add_space(pad_x);
                ui.image(egui::load::SizedTexture::new(
                    tex.id(),
                    egui::vec2(display_w, display_h),
                ));
            });
        } else {
            self.video_fullscreen = false;
            self.is_fullscreen = false;
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Fullscreen(false));
            return;
        }

        let show_overlay = self.last_mouse_move.elapsed().as_secs_f32() < 3.0;
        if show_overlay {
            egui::Area::new(egui::Id::new("fs_overlay"))
                .fixed_pos(egui::pos2(0.0, 0.0))
                .order(egui::Order::Foreground)
                .show(ui.ctx(), |ui| {
                    let screen_width = ui.ctx().screen_rect().width();
                    let frame = egui::Frame::none()
                        .fill(egui::Color32::from_rgba_premultiplied(0, 0, 0, 160))
                        .inner_margin(egui::Margin::same(8.0));
                    frame.show(ui, |ui: &mut egui::Ui| {
                        ui.set_min_width(screen_width);
                        ui.horizontal(|ui: &mut egui::Ui| {
                            ui.colored_label(
                                egui::Color32::WHITE,
                                egui::RichText::new("hostelD").size(16.0),
                            );
                            ui.separator();
                            let exit_btn = egui::Button::new(
                                egui::RichText::new("Exit Fullscreen").size(14.0).color(egui::Color32::WHITE)
                            ).fill(egui::Color32::from_rgb(80, 80, 120));
                            if ui.add(exit_btn).clicked() {
                                self.video_fullscreen = false;
                                self.is_fullscreen = false;
                                ui.ctx().send_viewport_cmd(egui::ViewportCommand::Fullscreen(false));
                            }
                        });
                    });
                });
        }
    }

    pub(crate) fn draw_chat(&mut self, ui: &mut egui::Ui) {
        ui.label(egui::RichText::new("Chat").strong());

        let available_height = ui.available_height() - 35.0;
        let peer_label = if self.peer_nickname.is_empty() {
            "Peer:".to_string()
        } else {
            format!("{}:", self.peer_nickname)
        };
        egui::ScrollArea::vertical()
            .max_height(available_height)
            .stick_to_bottom(true)
            .id_salt("chat_scroll")
            .show(ui, |ui| {
                if let Some(history) = &self.chat_history {
                    if history.messages.is_empty() {
                        ui.colored_label(self.settings.theme.text_muted(), "No messages yet. Type below.");
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
                                ui.colored_label(self.settings.theme.chat_peer(), &peer_label);
                                ui.label(&msg.text);
                            });
                        }
                    }
                }
            });

        ui.horizontal(|ui| {
            let response = ui.add(
                egui::TextEdit::singleline(&mut self.chat_input)
                    .desired_width(ui.available_width() - 70.0)
                    .hint_text("Type a message...")
            );

            let send_clicked = ui.add(
                egui::Button::new("Send").min_size(egui::vec2(55.0, 30.0))
            ).clicked();

            let enter_pressed = response.lost_focus()
                && ui.input(|i| i.key_pressed(egui::Key::Enter));

            if (send_clicked || enter_pressed) && !self.chat_input.trim().is_empty() {
                let text = self.chat_input.trim().to_string();
                if let Some(tx) = &self.chat_tx {
                    let _ = tx.send(text.clone());
                }
                if let Some(history) = &mut self.chat_history {
                    history.add_message(true, text);
                }
                self.chat_input.clear();
                response.request_focus();
            }
        });
    }
}
