use eframe::egui;

use super::helpers::paint_initial_avatar;
use crate::group;
use crate::groupcall::GroupRole;
use crate::screen::ScreenCommand;
use crate::gui::{HostelApp, load_icon_texture_sized};

impl HostelApp {
    /// Voice channel detail when NOT in a call — shows join screen with mode selector.
    pub(super) fn draw_group_voice_idle(&mut self, ui: &mut egui::Ui) {
        let idx = match self.group_detail_idx {
            Some(i) if i < self.groups.len() => i,
            _ => return,
        };
        let selected_channel_id = self.group_selected_channel.clone();
        let group_id = self.groups[idx].group_id.clone();
        let channel_name = self.groups[idx].voice_channels.iter()
            .find(|ch| ch.channel_id == selected_channel_id)
            .map(|ch| ch.name.clone())
            .unwrap_or_else(|| "Voice".to_string());

        // Check presence signals for this channel (clone to avoid borrow conflict)
        let presence: Option<Vec<(String, u8)>> = self.group_call_presence
            .get(&group_id)
            .and_then(|ch| ch.get(&selected_channel_id))
            .cloned();
        let has_ongoing = presence.as_ref().map(|p| !p.is_empty()).unwrap_or(false);

        ui.add_space(60.0);
        ui.vertical_centered(|ui| {
            ui.label(
                egui::RichText::new(format!("\u{00BB} {}", channel_name))
                    .size(20.0)
                    .strong(),
            );
            ui.add_space(16.0);

            if has_ongoing {
                let count = presence.as_ref().map(|p| p.len()).unwrap_or(0);
                ui.label(
                    egui::RichText::new(format!("A call is in progress ({} member{})", count, if count == 1 { "" } else { "s" }))
                        .size(14.0)
                        .color(self.settings.theme.btn_primary()),
                );
                // Show nicknames of members in call
                if let Some(ref members_in_call) = presence {
                    let nicknames: Vec<String> = members_in_call.iter().filter_map(|(cid, _)| {
                        self.contacts.iter()
                            .find(|c| c.contact_id == *cid)
                            .map(|c| c.nickname.clone())
                    }).collect();
                    if !nicknames.is_empty() {
                        ui.label(
                            egui::RichText::new(nicknames.join(", "))
                                .size(12.0)
                                .color(self.settings.theme.text_muted()),
                        );
                    }
                }
            } else {
                ui.label(
                    egui::RichText::new("No active call — you will start a new session")
                        .size(14.0)
                        .color(self.settings.theme.text_muted()),
                );
            }

            // Mode selector: locked when there's an ongoing call
            ui.add_space(20.0);
            if has_ongoing {
                // Determine locked mode from first member's signal
                let locked_mode = presence.as_ref().and_then(|p| p.first()).map(|(_, m)| *m).unwrap_or(0);
                let mode_label = if locked_mode == 1 { "P2P" } else { "Relay" };
                ui.horizontal(|ui| {
                    ui.label("Mode:");
                    ui.label(
                        egui::RichText::new(mode_label)
                            .strong()
                            .color(self.settings.theme.btn_primary()),
                    );
                    ui.label(
                        egui::RichText::new("(locked — call in progress)")
                            .size(11.0)
                            .color(self.settings.theme.text_muted()),
                    );
                });
                // Sync the group's call_mode to match the ongoing call
                if let Some(grp) = self.groups.get_mut(idx) {
                    let target_mode = if locked_mode == 1 { group::CallMode::P2P } else { group::CallMode::Relay };
                    if grp.call_mode != target_mode {
                        grp.call_mode = target_mode;
                        group::save_group(grp);
                    }
                }
            } else if let Some(grp) = self.groups.get(idx) {
                let mode = grp.call_mode;
                ui.horizontal(|ui| {
                    ui.label("Mode:");
                    if ui.selectable_label(mode == group::CallMode::Relay, "Relay").clicked() {
                        if let Some(grp) = self.groups.get_mut(idx) {
                            grp.call_mode = group::CallMode::Relay;
                            group::save_group(grp);
                            self.broadcast_group_update(idx);
                        }
                    }
                    if ui.selectable_label(mode == group::CallMode::P2P, "P2P").clicked() {
                        if let Some(grp) = self.groups.get_mut(idx) {
                            grp.call_mode = group::CallMode::P2P;
                            group::save_group(grp);
                            self.broadcast_group_update(idx);
                        }
                    }
                });
            }

            ui.add_space(20.0);
            let btn_text = if has_ongoing { "Join Call" } else { "Start Voice Channel" };
            let join_btn = egui::Button::new(
                egui::RichText::new(btn_text)
                    .strong()
                    .color(egui::Color32::WHITE),
            ).fill(self.settings.theme.btn_positive());
            if ui.add(join_btn).clicked() {
                self.start_group_call(&selected_channel_id);
            }
        });
    }

    /// Voice channel detail when IN a call — shows controls + members.
    pub(super) fn draw_group_voice_active(&mut self, ui: &mut egui::Ui) {
        let idx = match self.group_detail_idx {
            Some(i) if i < self.groups.len() => i,
            _ => return,
        };
        let selected_channel_id = self.group_selected_channel.clone();
        let channel_name = self.groups[idx].voice_channels.iter()
            .find(|ch| ch.channel_id == selected_channel_id)
            .map(|ch| ch.name.clone())
            .unwrap_or_else(|| "Voice".to_string());
        let role = self.group_call_role.unwrap_or(GroupRole::Member);
        let member_count = self.group_call_members.len();
        let my_pubkey = self.identity.pubkey;
        let call_mode = self.groups[idx].call_mode;

        // Pre-compute column widths
        let avail_for_split = ui.available_rect_before_wrap();
        let sep_w = 1.0;
        let members_w = 180.0_f32.max(avail_for_split.width() * 0.22).min(240.0);
        let main_w = (avail_for_split.width() - members_w - sep_w - 4.0).max(100.0);

        // Top bar
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.set_max_width(main_w);
            ui.heading(format!("\u{00BB} {}", channel_name));
            ui.add_space(8.0);
            ui.label(
                egui::RichText::new("ENCRYPTED")
                    .size(10.0)
                    .strong()
                    .color(self.settings.theme.btn_positive()),
            );
            let role_label = match call_mode {
                group::CallMode::P2P => "P2P",
                group::CallMode::Relay => if role == GroupRole::Leader { "LEADER" } else { "MEMBER" },
            };
            ui.label(
                egui::RichText::new(role_label)
                    .size(10.0)
                    .color(self.settings.theme.btn_primary()),
            );
        });

        ui.separator();

        let available = ui.available_rect_before_wrap();
        let clip = ui.clip_rect();

        // Background for right sidebar
        let bg_rect = egui::Rect::from_min_max(
            egui::pos2(available.min.x + main_w + sep_w + 4.0, clip.min.y),
            egui::pos2(clip.max.x, clip.max.y),
        );
        ui.painter().rect_filled(bg_rect, 0.0, self.settings.theme.sidebar_bg());

        // Vertical separator
        let sep_x = available.min.x + main_w + 2.0;
        ui.painter().vline(sep_x, clip.y_range(), egui::Stroke::new(sep_w, self.settings.theme.text_muted()));

        let main_rect = egui::Rect::from_min_size(
            available.min,
            egui::vec2(main_w, available.height()),
        );
        let members_rect = egui::Rect::from_min_size(
            egui::pos2(available.min.x + main_w + sep_w + 4.0, available.min.y),
            egui::vec2(members_w - 4.0, available.height()),
        );

        // ── Left panel: Controls ──
        ui.allocate_new_ui(egui::UiBuilder::new().max_rect(main_rect), |ui| {
            ui.add_space(20.0);
            ui.vertical_centered(|ui| {
                ui.label(
                    egui::RichText::new(format!("{} connected", member_count))
                        .size(14.0)
                        .color(self.settings.theme.text_muted()),
                );
                ui.add_space(20.0);

                // Controls row (matching 1-on-1 call styling)
                ui.horizontal(|ui| {
                    // Mic toggle
                    let mic_on = self.group_call_mic.load(std::sync::atomic::Ordering::Relaxed);
                    let (btn_text, btn_color) = if mic_on {
                        ("Mic: ON", self.settings.theme.btn_positive())
                    } else {
                        ("Mic: MUTED", self.settings.theme.btn_negative())
                    };
                    let mic_btn = egui::Button::new(
                        egui::RichText::new(btn_text).size(16.0).color(egui::Color32::WHITE)
                    ).min_size(egui::vec2(120.0, 35.0)).fill(btn_color);
                    if ui.add(mic_btn).clicked() {
                        self.group_call_mic.store(!mic_on, std::sync::atomic::Ordering::Relaxed);
                    }

                    // Screen share toggle
                    let (scr_text, scr_color) = if self.group_screen_sharing {
                        ("Screen: ON", self.settings.theme.btn_primary())
                    } else {
                        ("Screen: OFF", self.settings.theme.btn_neutral())
                    };
                    let scr_btn = egui::Button::new(
                        egui::RichText::new(scr_text).size(16.0).color(egui::Color32::WHITE)
                    ).min_size(egui::vec2(130.0, 35.0)).fill(scr_color);
                    if ui.add(scr_btn).clicked() {
                        if self.group_screen_sharing {
                            self.group_screen_sharing = false;
                            if let Some(tx) = &self.screen_cmd_tx {
                                let _ = tx.send(ScreenCommand::Stop);
                            }
                        } else {
                            if self.group_webcam_sharing {
                                self.group_webcam_sharing = false;
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

                    // Webcam toggle
                    let (cam_text, cam_color) = if self.group_webcam_sharing {
                        ("Cam: ON", self.settings.theme.btn_positive())
                    } else {
                        ("Cam: OFF", self.settings.theme.btn_neutral())
                    };
                    let cam_tex = self.enablecam_icon_texture.get_or_insert_with(|| {
                        load_icon_texture_sized(ui.ctx(), "icon-enablecam", include_bytes!("../../../assets/enablecam.png"), 48)
                    }).clone();
                    let cam_icon_h = 20.0;
                    let cam_icon_aspect = cam_tex.size()[0] as f32 / cam_tex.size()[1] as f32;
                    let cam_icon_sized = egui::load::SizedTexture::new(cam_tex.id(), egui::vec2(cam_icon_h * cam_icon_aspect, cam_icon_h));
                    let cam_btn = egui::Button::image_and_text(
                        cam_icon_sized,
                        egui::RichText::new(cam_text).size(16.0).color(egui::Color32::WHITE),
                    ).min_size(egui::vec2(120.0, 35.0)).fill(cam_color);
                    if ui.add(cam_btn).clicked() {
                        if self.group_webcam_sharing {
                            self.group_webcam_sharing = false;
                            if let Some(tx) = &self.screen_cmd_tx {
                                let _ = tx.send(ScreenCommand::Stop);
                            }
                        } else {
                            if self.group_screen_sharing {
                                self.group_screen_sharing = false;
                                if let Some(tx) = &self.screen_cmd_tx {
                                    let _ = tx.send(ScreenCommand::Stop);
                                }
                            }
                            self.camera_names = crate::screen::list_cameras();
                            self.show_webcam_popup = true;
                        }
                    }

                    // Spacer to push Hang Up to far right
                    let remaining = ui.available_width() - 110.0;
                    if remaining > 0.0 {
                        ui.add_space(remaining);
                    }

                    // Hang up
                    let hangup_btn = egui::Button::new(
                        egui::RichText::new("Hang Up").size(16.0).color(egui::Color32::WHITE)
                    ).min_size(egui::vec2(100.0, 35.0)).fill(self.settings.theme.btn_negative());
                    if ui.add(hangup_btn).clicked() {
                        self.cleanup_group_call();
                    }
                });
            });
        });

        // ── Right panel: Members + mode selector ──
        let color_even = self.settings.theme.panel_bg();
        let color_odd = self.settings.theme.sidebar_bg();

        ui.allocate_new_ui(egui::UiBuilder::new().max_rect(members_rect), |ui| {
            // Mode selector header (disabled during active call)
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new("Mode:").size(11.0).color(self.settings.theme.text_muted()),
                );
                let relay_label = egui::SelectableLabel::new(
                    call_mode == group::CallMode::Relay, "Relay",
                );
                let p2p_label = egui::SelectableLabel::new(
                    call_mode == group::CallMode::P2P, "P2P",
                );
                // Disabled during call
                ui.add_enabled(false, relay_label);
                ui.add_enabled(false, p2p_label);
            });

            ui.separator();

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

            // Member rows with avatars
            egui::ScrollArea::vertical()
                .id_salt("voice_call_members")
                .show(ui, |ui| {
                    let row_w = ui.available_width();
                    for (i, member) in self.group_call_members.iter().enumerate() {
                        let bg = if i % 2 == 0 { color_even } else { color_odd };
                        let row_rect = ui.allocate_space(egui::vec2(row_w, 32.0)).1;
                        ui.painter().rect_filled(row_rect, 0.0, bg);

                        let mut x = row_rect.min.x + 8.0;
                        let cy = row_rect.center().y;

                        // Avatar (22px)
                        let av_size = 22.0;
                        let av_rect = egui::Rect::from_center_size(
                            egui::pos2(x + av_size / 2.0, cy),
                            egui::vec2(av_size, av_size),
                        );
                        let mut drew_av = false;
                        if member.pubkey == my_pubkey {
                            if let Some(tex) = &self.own_avatar_texture {
                                let uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
                                ui.painter().image(tex.id(), av_rect, uv, egui::Color32::WHITE);
                                drew_av = true;
                            }
                        } else {
                            let cid = crate::identity::derive_contact_id(&my_pubkey, &member.pubkey);
                            if let Some(tex) = self.contact_avatar_textures.get(&cid) {
                                let uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
                                ui.painter().image(tex.id(), av_rect, uv, egui::Color32::WHITE);
                                drew_av = true;
                            }
                        }
                        if !drew_av {
                            paint_initial_avatar(ui.painter(), av_rect, &member.nickname, &self.settings.theme);
                        }
                        x += av_size + 6.0;

                        // Nickname
                        let nick_galley = ui.painter().layout_no_wrap(
                            member.nickname.clone(),
                            egui::FontId::proportional(13.0),
                            self.settings.theme.text_primary(),
                        );
                        ui.painter().galley(
                            egui::pos2(x, cy - nick_galley.size().y / 2.0),
                            nick_galley,
                            self.settings.theme.text_primary(),
                        );

                        // (you)
                        if member.pubkey == my_pubkey {
                            x += 60.0;
                            ui.painter().text(
                                egui::pos2(x, cy),
                                egui::Align2::LEFT_CENTER,
                                "(you)",
                                egui::FontId::proportional(11.0),
                                self.settings.theme.text_muted(),
                            );
                        }
                    }
                });
        });
    }

    pub(super) fn draw_group_connecting(&mut self, ui: &mut egui::Ui) {
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
}
