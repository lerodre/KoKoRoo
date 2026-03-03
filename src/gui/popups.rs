use std::sync::atomic::Ordering;

use eframe::egui;

use super::HostelApp;
use super::network::{peer_display_job, censor_ip};
use crate::group::{self, Group};
use crate::messaging::MsgCommand;

impl HostelApp {
    pub(crate) fn draw_incoming_call_popup(&mut self, ctx: &egui::Context) {
        let call_info = match &self.incoming_call {
            Some(info) => info,
            None => return,
        };
        let caller_job = peer_display_job(&call_info.nickname, &call_info.fingerprint, 16.0, self.settings.theme.text_primary(), self.settings.theme.text_dim());
        let ip_display = if self.show_ips {
            format!("[{}]:{}", call_info.ip, call_info.port)
        } else {
            format!("[{}]:{}", censor_ip(&call_info.ip), call_info.port)
        };

        let mut accept = false;
        let mut reject = false;

        egui::Window::new("Incoming Call")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.add_space(8.0);
                ui.vertical_centered(|ui| {
                    ui.label(
                        egui::RichText::new("Incoming Call")
                            .size(20.0)
                            .strong()
                            .color(self.settings.theme.accent()),
                    );
                    ui.add_space(8.0);
                    ui.label(caller_job);
                    ui.colored_label(self.settings.theme.text_muted(), &ip_display);
                    ui.add_space(12.0);
                });
                ui.horizontal(|ui| {
                    let accept_btn = egui::Button::new(
                        egui::RichText::new("Accept").size(16.0).color(egui::Color32::WHITE),
                    )
                    .min_size(egui::vec2(120.0, 38.0))
                    .fill(self.settings.theme.btn_positive());
                    if ui.add(accept_btn).clicked() {
                        accept = true;
                    }

                    let reject_btn = egui::Button::new(
                        egui::RichText::new("Reject").size(16.0).color(egui::Color32::WHITE),
                    )
                    .min_size(egui::vec2(120.0, 38.0))
                    .fill(self.settings.theme.btn_negative());
                    if ui.add(reject_btn).clicked() {
                        reject = true;
                    }
                });
                ui.add_space(4.0);
            });

        if accept {
            if let Some(info) = self.incoming_call.take() {
                // Stop ringtone
                if let Some(stop) = self.ringtone_stop.take() {
                    stop.store(true, Ordering::Relaxed);
                }
                // Clear cooldown (no reject — we're accepting)
                if let Some(tx) = &self.msg_cmd_tx {
                    tx.send(MsgCommand::DismissIncomingCall { ip: info.ip.clone(), reject: false }).ok();
                }
                self.peer_ip = info.ip;
                self.peer_port = info.port;
                self.active_tab = super::SidebarTab::Call;
                self.start_call();
            }
        }
        if reject {
            if let Some(info) = self.incoming_call.take() {
                // Stop ringtone
                if let Some(stop) = self.ringtone_stop.take() {
                    stop.store(true, Ordering::Relaxed);
                }
                // Reject: daemon will complete handshake + send HANGUP to cut caller
                if let Some(tx) = &self.msg_cmd_tx {
                    tx.send(MsgCommand::DismissIncomingCall { ip: info.ip, reject: true }).ok();
                }
            }
        }

        ctx.request_repaint_after(std::time::Duration::from_millis(200));
    }

    pub(crate) fn draw_firewall_prompt(&mut self, ctx: &egui::Context) {
        let is_port_change = !self.firewall_old_port.is_empty();
        let port = self.local_port.clone();

        let mut action = false;
        let mut skip = false;

        egui::Window::new("Firewall")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.add_space(8.0);
                ui.vertical_centered(|ui| {
                    if is_port_change {
                        ui.label(
                            egui::RichText::new(format!("Port changed to {}.", port))
                                .size(15.0),
                        );
                        ui.label(
                            egui::RichText::new("The old firewall rule will be replaced.")
                                .size(14.0),
                        );
                    } else {
                        ui.label(
                            egui::RichText::new(format!(
                                "hostelD needs to open UDP port {} in the firewall", port
                            ))
                            .size(15.0),
                        );
                        ui.label(
                            egui::RichText::new("for calls and messages to work.")
                                .size(14.0),
                        );
                    }
                    ui.add_space(2.0);
                    ui.colored_label(
                        self.settings.theme.text_muted(),
                        "This requires admin permission.",
                    );
                    ui.add_space(12.0);
                });
                ui.horizontal(|ui| {
                    let action_label = if is_port_change { "Update" } else { "Open Port" };
                    let action_btn = egui::Button::new(
                        egui::RichText::new(action_label).size(15.0).color(egui::Color32::WHITE),
                    )
                    .min_size(egui::vec2(120.0, 36.0))
                    .fill(self.settings.theme.btn_positive());
                    if ui.add(action_btn).clicked() {
                        action = true;
                    }

                    let skip_btn = egui::Button::new(
                        egui::RichText::new("Skip").size(15.0),
                    )
                    .min_size(egui::vec2(120.0, 36.0));
                    if ui.add(skip_btn).clicked() {
                        skip = true;
                    }
                });
                ui.add_space(4.0);
            });

        if action {
            // Remove old rule if port changed
            if is_port_change {
                if let Ok(old_port) = self.firewall_old_port.parse::<u16>() {
                    match crate::platform::remove_udp_port_rule(old_port) {
                        Ok(true) => log_fmt!("[firewall] Removed old rule for UDP port {}", old_port),
                        Ok(false) => log_fmt!("[firewall] No old rule found for UDP port {}", old_port),
                        Err(e) => log_fmt!("[firewall] WARNING removing old rule: {}", e),
                    }
                }
            }
            // Add new rule
            if let Ok(port_num) = port.parse::<u16>() {
                match crate::platform::ensure_udp_port_open(port_num) {
                    Ok(true) => log_fmt!("[firewall] Added rule for UDP port {}", port_num),
                    Ok(false) => log_fmt!("[firewall] Rule already exists for UDP port {}", port_num),
                    Err(e) => log_fmt!("[firewall] WARNING: {}", e),
                }
            }
            // Update tracked port
            self.settings.firewall_port = self.local_port.clone();
            self.settings.save();
            self.firewall_old_port.clear();
            self.show_firewall_prompt = false;
        }
        if skip {
            self.firewall_old_port.clear();
            self.show_firewall_prompt = false;
        }
    }

    pub(crate) fn draw_incoming_group_invite_popup(&mut self, ctx: &egui::Context) {
        let info = match &self.incoming_group_invite {
            Some(i) => i,
            None => return,
        };

        let from = info.from_nickname.clone();
        let name = info.group_name.clone();
        let count = info.member_count;

        let mut accept = false;
        let mut reject = false;

        egui::Window::new("Group Invitation")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.add_space(8.0);
                ui.vertical_centered(|ui| {
                    ui.label(
                        egui::RichText::new("Group Invitation")
                            .size(20.0)
                            .strong()
                            .color(self.settings.theme.accent()),
                    );
                    ui.add_space(10.0);
                    ui.label(
                        egui::RichText::new(format!("{} invited you to", from))
                            .size(14.0),
                    );
                    ui.label(
                        egui::RichText::new(&name)
                            .size(16.0)
                            .strong(),
                    );
                    ui.add_space(4.0);
                    ui.label(
                        egui::RichText::new(format!("{} members", count))
                            .size(12.0)
                            .color(self.settings.theme.text_muted()),
                    );
                    ui.add_space(12.0);
                });
                ui.horizontal(|ui| {
                    let accept_btn = egui::Button::new(
                        egui::RichText::new("Accept").size(16.0).color(egui::Color32::WHITE),
                    )
                    .min_size(egui::vec2(120.0, 38.0))
                    .fill(self.settings.theme.btn_positive());
                    if ui.add(accept_btn).clicked() {
                        accept = true;
                    }

                    let reject_btn = egui::Button::new(
                        egui::RichText::new("Reject").size(16.0).color(egui::Color32::WHITE),
                    )
                    .min_size(egui::vec2(120.0, 38.0))
                    .fill(self.settings.theme.btn_negative());
                    if ui.add(reject_btn).clicked() {
                        reject = true;
                    }
                });
                ui.add_space(4.0);
            });

        if accept {
            if let Some(info) = self.incoming_group_invite.take() {
                if let Ok(grp) = serde_json::from_slice::<Group>(&info.group_json) {
                    if !self.groups.iter().any(|g| g.group_id == grp.group_id) {
                        group::save_group(&grp);
                        // Send ACK to inviter via daemon
                        if let Some(tx) = &self.msg_cmd_tx {
                            if let Some(contact) = self.contacts.iter().find(|c| c.pubkey == grp.created_by) {
                                tx.send(MsgCommand::AcceptGroupInvite {
                                    contact_id: contact.contact_id.clone(),
                                    group_id: grp.group_id.clone(),
                                }).ok();
                            }
                        }
                        self.groups.push(grp);
                    }
                }
            }
        }
        if reject {
            if let Some(info) = self.incoming_group_invite.take() {
                if let Ok(grp) = serde_json::from_slice::<Group>(&info.group_json) {
                    // Send NACK to inviter via daemon
                    if let Some(tx) = &self.msg_cmd_tx {
                        if let Some(contact) = self.contacts.iter().find(|c| c.pubkey == grp.created_by) {
                            tx.send(MsgCommand::RejectGroupInvite {
                                contact_id: contact.contact_id.clone(),
                                group_id: grp.group_id.clone(),
                            }).ok();
                        }
                    }
                }
            }
        }
    }
}

/// Load a PNG from bytes, crop transparent padding, return (rgba, width, height).
pub(crate) fn load_png_cropped(png_bytes: &[u8]) -> (Vec<u8>, u32, u32) {
    let img = image::load_from_memory(png_bytes).expect("failed to decode PNG");
    let rgba = img.to_rgba8();
    let (w, h) = (rgba.width(), rgba.height());

    // Find bounding box of non-transparent pixels
    let mut min_x = w;
    let mut min_y = h;
    let mut max_x = 0u32;
    let mut max_y = 0u32;
    for y in 0..h {
        for x in 0..w {
            if rgba.get_pixel(x, y)[3] > 0 {
                min_x = min_x.min(x);
                min_y = min_y.min(y);
                max_x = max_x.max(x);
                max_y = max_y.max(y);
            }
        }
    }

    if max_x < min_x || max_y < min_y {
        return (rgba.into_raw(), w, h);
    }

    let crop_w = max_x - min_x + 1;
    let crop_h = max_y - min_y + 1;
    let cropped = image::imageops::crop_imm(&rgba, min_x, min_y, crop_w, crop_h).to_image();
    (cropped.into_raw(), crop_w, crop_h)
}

/// Load PNG bytes → resize with Lanczos3 → apply circular alpha mask → create TextureHandle.
/// `display_size` is the expected display size in logical pixels; the texture is created at 2×
/// for HiDPI crispness. Circle mask is applied *after* downscale so the antialiased edge
/// stays sharp at any display size.
pub(crate) fn load_avatar_texture(ctx: &egui::Context, name: &str, png_bytes: &[u8], display_size: u32) -> Option<egui::TextureHandle> {
    let img = image::load_from_memory(png_bytes).ok()?;
    // 2× the display size for HiDPI; cap at native resolution
    let tex_size = (display_size * 2).min(img.width()).min(img.height()).max(1);
    let mut rgba_img = image::imageops::resize(
        &img.to_rgba8(), tex_size, tex_size,
        image::imageops::FilterType::Lanczos3,
    );
    crate::avatar::apply_circle_mask(&mut rgba_img);
    let pixels = egui::ColorImage::from_rgba_unmultiplied(
        [tex_size as usize, tex_size as usize],
        &rgba_img,
    );
    Some(ctx.load_texture(name, pixels, egui::TextureOptions::LINEAR))
}

/// Load a PNG, crop transparent padding, downscale with Lanczos3, and create an egui texture.
/// max_size caps the largest dimension (0 = no downscale).
pub(crate) fn load_icon_texture_sized(ctx: &egui::Context, name: &str, png_bytes: &[u8], max_size: u32) -> egui::TextureHandle {
    let (rgba, w, h) = load_png_cropped(png_bytes);
    let img = image::RgbaImage::from_raw(w, h, rgba).unwrap();
    let img = if max_size > 0 && (w > max_size || h > max_size) {
        // Preserve aspect ratio: scale so largest dimension = max_size
        let scale = max_size as f32 / w.max(h) as f32;
        let nw = ((w as f32 * scale).round() as u32).max(1);
        let nh = ((h as f32 * scale).round() as u32).max(1);
        image::imageops::resize(&img, nw, nh, image::imageops::FilterType::Lanczos3)
    } else {
        img
    };
    let (fw, fh) = (img.width(), img.height());
    let pixels = egui::ColorImage::from_rgba_unmultiplied([fw as usize, fh as usize], &img);
    ctx.load_texture(name, pixels, egui::TextureOptions::LINEAR)
}
