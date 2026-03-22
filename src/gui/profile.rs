use eframe::egui;
use super::{HostelApp, get_best_ipv6, censor_ip, load_avatar_texture};
use crate::avatar;
use crate::messaging::MsgCommand;

const DEFAULT_AVATAR_PNG: &[u8] = include_bytes!("../../assets/default_avatar.png");

impl HostelApp {
    pub(crate) fn draw_profile_tab(&mut self, ui: &mut egui::Ui) {
        ui.add_space(10.0);
        ui.heading("Profile");
        ui.add_space(10.0);

        // ── Avatar circle (96px) ──
        let avatar_size = 96.0;

        // Ensure default avatar texture is loaded
        if self.default_avatar_texture.is_none() {
            self.default_avatar_texture = load_avatar_texture(
                ui.ctx(), "default_avatar", DEFAULT_AVATAR_PNG, 96,
            );
        }

        // Load own avatar texture lazily
        if self.own_avatar_texture.is_none() {
            if let Some(bytes) = avatar::load_own_avatar() {
                self.own_avatar_texture = load_avatar_texture(
                    ui.ctx(), "own_avatar", &bytes, 96,
                );
            }
        }

        let accent = self.settings.theme.accent();

        ui.horizontal(|ui| {
            // Reserve space for the avatar circle
            let (rect, response) = ui.allocate_exact_size(
                egui::vec2(avatar_size, avatar_size),
                egui::Sense::click(),
            );
            let center = rect.center();
            let radius = avatar_size / 2.0;

            // Draw avatar image or placeholder
            if let Some(tex) = &self.own_avatar_texture {
                let uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
                ui.painter().image(tex.id(), rect, uv, egui::Color32::WHITE);
            } else if let Some(tex) = &self.default_avatar_texture {
                // Placeholder with semi-transparent overlay
                let uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
                ui.painter().image(tex.id(), rect, uv, egui::Color32::from_white_alpha(120));
                // Draw "+" in center
                ui.painter().text(
                    center,
                    egui::Align2::CENTER_CENTER,
                    "+",
                    egui::FontId::proportional(36.0),
                    accent,
                );
            } else {
                // Fallback: just draw "+" inside circle
                ui.painter().circle_filled(center, radius - 2.0, self.settings.theme.widget_bg());
                ui.painter().text(
                    center,
                    egui::Align2::CENTER_CENTER,
                    "+",
                    egui::FontId::proportional(36.0),
                    accent,
                );
            }

            // Hover effect
            if response.hovered() {
                ui.painter().circle_filled(center, radius, egui::Color32::from_black_alpha(40));
                ui.painter().text(
                    center,
                    egui::Align2::CENTER_CENTER,
                    if self.own_avatar_texture.is_some() { "Change" } else { "+" },
                    egui::FontId::proportional(if self.own_avatar_texture.is_some() { 14.0 } else { 36.0 }),
                    egui::Color32::WHITE,
                );
            }

            // Click → open file picker
            if response.clicked() {
                self.open_avatar_picker();
            }

            response.on_hover_cursor(egui::CursorIcon::PointingHand);
        });

        ui.add_space(10.0);

        ui.horizontal(|ui| {
            ui.label("Your ID:");
            ui.strong(&self.identity.fingerprint);
            if ui.small_button("Copy").clicked() {
                ui.ctx().copy_text(self.identity.fingerprint.clone());
            }
        });

        ui.add_space(6.0);

        ui.horizontal(|ui| {
            ui.label("Nickname:");
            let nick_edit = egui::TextEdit::singleline(&mut self.settings.nickname)
                .desired_width(200.0)
                .hint_text("optional");
            let resp = egui::Frame::none()
                .stroke(egui::Stroke::new(1.0, self.settings.theme.separator()))
                .inner_margin(0.0)
                .show(ui, |ui| ui.add(nick_edit)).inner;
            if resp.changed() {
                self.settings.save();
                // Broadcast nickname update to all connected peers
                if let Some(tx) = &self.msg_cmd_tx {
                    tx.send(crate::messaging::MsgCommand::UpdateNickname {
                        nickname: self.settings.nickname.clone(),
                    }).ok();
                }
            }
        });

        ui.add_space(10.0);
        ui.separator();
        ui.add_space(6.0);

        ui.horizontal(|ui| {
            ui.label("Your IPv6:");
            let display_ip = if self.show_ips { self.best_ipv6.clone() } else { censor_ip(&self.best_ipv6) };
            ui.monospace(&display_ip);
            let eye_label = if self.show_ips { "Hide" } else { "Show" };
            if ui.small_button(eye_label).clicked() {
                self.show_ips = !self.show_ips;
            }
            if ui.small_button("Copy").clicked() {
                ui.ctx().copy_text(self.best_ipv6.clone());
            }
            if ui.small_button("Refresh").clicked() {
                self.best_ipv6 = get_best_ipv6(&self.settings.network_adapter);
            }
        });

        if self.best_ipv6 == "::1" {
            ui.add_space(4.0);
            ui.colored_label(
                self.settings.theme.warning(),
                "Only loopback detected. Check your network connection.",
            );
        }
    }

    pub(crate) fn open_avatar_picker(&mut self) {
        // macOS/Windows: use sync dialog on main thread (AppKit requires it).
        // Linux: needs tokio runtime for xdg-desktop-portal via zbus.
        #[cfg(target_os = "linux")]
        let picked = std::thread::spawn(|| {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .ok()?;
            rt.block_on(async {
                rfd::AsyncFileDialog::new()
                    .set_title("Select avatar image")
                    .add_filter("Images", &["png", "jpg", "jpeg"])
                    .pick_file()
                    .await
            })
        }).join().ok().flatten();

        #[cfg(not(target_os = "linux"))]
        let picked = rfd::FileDialog::new()
            .set_title("Select avatar image")
            .add_filter("Images", &["png", "jpg", "jpeg"])
            .pick_file();

        #[cfg(target_os = "linux")]
        let path = picked.map(|h| h.path().to_path_buf());
        #[cfg(not(target_os = "linux"))]
        let path = picked;

        if let Some(path) = path {
            if let Ok(bytes) = std::fs::read(&path) {
                // Try to decode to get dimensions
                if let Ok(img) = image::load_from_memory(&bytes) {
                    let (w, h) = image::GenericImageView::dimensions(&img);
                    self.crop_source_bytes = Some(bytes.clone());
                    self.crop_source_dims = (w, h);

                    // Default crop: centered square covering max area
                    let min_dim = w.min(h) as f32;
                    self.crop_size = min_dim;
                    self.crop_offset = (
                        (w as f32 - min_dim) / 2.0,
                        (h as f32 - min_dim) / 2.0,
                    );
                    self.crop_dragging = false;

                    // Texture will be created by draw_crop_editor with the correct context
                    self.crop_source_texture = None;
                    self.show_crop_editor = true;
                }
            }
        }
    }

    pub(crate) fn draw_crop_editor(&mut self, ctx: &egui::Context) {
        let mut close = false;
        let mut save = false;

        let accent = self.settings.theme.accent();
        let (src_w, src_h) = self.crop_source_dims;
        if src_w == 0 || src_h == 0 {
            self.show_crop_editor = false;
            return;
        }

        // Create texture with the real app context if not yet loaded
        if self.crop_source_texture.is_none() {
            if let Some(ref bytes) = self.crop_source_bytes {
                if let Ok(img) = image::load_from_memory(bytes) {
                    let rgba = img.to_rgba8();
                    let pixels = egui::ColorImage::from_rgba_unmultiplied(
                        [src_w as usize, src_h as usize],
                        &rgba,
                    );
                    self.crop_source_texture = Some(
                        ctx.load_texture("crop_source", pixels, egui::TextureOptions::LINEAR)
                    );
                }
            }
        }

        egui::Window::new("Crop Avatar")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .fixed_size([420.0, 480.0])
            .show(ctx, |ui| {
                ui.add_space(4.0);

                // Calculate display size: fit image within 380×380
                let max_display = 380.0_f32;
                let scale = (max_display / src_w as f32).min(max_display / src_h as f32).min(1.0);
                let disp_w = src_w as f32 * scale;
                let disp_h = src_h as f32 * scale;

                // Center the image
                let available_w = ui.available_width();
                let offset_x = ((available_w - disp_w) / 2.0).max(0.0);

                ui.add_space(2.0);
                ui.horizontal(|ui| {
                    ui.add_space(offset_x);
                    let (img_rect, response) = ui.allocate_exact_size(
                        egui::vec2(disp_w, disp_h),
                        egui::Sense::click_and_drag(),
                    );

                    // Draw the source image
                    if let Some(tex) = &self.crop_source_texture {
                        let uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
                        ui.painter().image(tex.id(), img_rect, uv, egui::Color32::WHITE);
                    }

                    // Crop rect in display coordinates
                    let crop_x_disp = self.crop_offset.0 * scale;
                    let crop_y_disp = self.crop_offset.1 * scale;
                    let crop_size_disp = self.crop_size * scale;

                    let crop_rect = egui::Rect::from_min_size(
                        egui::pos2(img_rect.min.x + crop_x_disp, img_rect.min.y + crop_y_disp),
                        egui::vec2(crop_size_disp, crop_size_disp),
                    );

                    // Dark overlay outside crop area
                    let dark = egui::Color32::from_black_alpha(140);
                    // Top
                    if crop_rect.min.y > img_rect.min.y {
                        ui.painter().rect_filled(
                            egui::Rect::from_min_max(img_rect.min, egui::pos2(img_rect.max.x, crop_rect.min.y)),
                            0.0, dark,
                        );
                    }
                    // Bottom
                    if crop_rect.max.y < img_rect.max.y {
                        ui.painter().rect_filled(
                            egui::Rect::from_min_max(egui::pos2(img_rect.min.x, crop_rect.max.y), img_rect.max),
                            0.0, dark,
                        );
                    }
                    // Left
                    ui.painter().rect_filled(
                        egui::Rect::from_min_max(
                            egui::pos2(img_rect.min.x, crop_rect.min.y),
                            egui::pos2(crop_rect.min.x, crop_rect.max.y),
                        ),
                        0.0, dark,
                    );
                    // Right
                    ui.painter().rect_filled(
                        egui::Rect::from_min_max(
                            egui::pos2(crop_rect.max.x, crop_rect.min.y),
                            egui::pos2(img_rect.max.x, crop_rect.max.y),
                        ),
                        0.0, dark,
                    );

                    // Crop border
                    ui.painter().rect_stroke(crop_rect, 0.0, egui::Stroke::new(2.0, accent));

                    // Grid lines (thirds)
                    let third = crop_size_disp / 3.0;
                    let grid_color = egui::Color32::from_white_alpha(60);
                    for i in 1..3 {
                        let x = crop_rect.min.x + third * i as f32;
                        ui.painter().line_segment(
                            [egui::pos2(x, crop_rect.min.y), egui::pos2(x, crop_rect.max.y)],
                            egui::Stroke::new(1.0, grid_color),
                        );
                        let y = crop_rect.min.y + third * i as f32;
                        ui.painter().line_segment(
                            [egui::pos2(crop_rect.min.x, y), egui::pos2(crop_rect.max.x, y)],
                            egui::Stroke::new(1.0, grid_color),
                        );
                    }

                    // Handle dragging the crop rect
                    if response.drag_started() {
                        self.crop_dragging = true;
                        if let Some(pos) = response.interact_pointer_pos() {
                            self.crop_drag_start = (
                                pos.x - (img_rect.min.x + crop_x_disp),
                                pos.y - (img_rect.min.y + crop_y_disp),
                            );
                        }
                    }
                    if response.dragged() && self.crop_dragging {
                        if let Some(pos) = ctx.input(|i| i.pointer.latest_pos()) {
                            let new_x = (pos.x - img_rect.min.x - self.crop_drag_start.0) / scale;
                            let new_y = (pos.y - img_rect.min.y - self.crop_drag_start.1) / scale;
                            // Clamp to image bounds
                            self.crop_offset.0 = new_x.max(0.0).min(src_w as f32 - self.crop_size);
                            self.crop_offset.1 = new_y.max(0.0).min(src_h as f32 - self.crop_size);
                        }
                    }
                    if response.drag_stopped() {
                        self.crop_dragging = false;
                    }

                    // Scroll to resize crop
                    let scroll = ui.input(|i| i.smooth_scroll_delta.y);
                    if scroll != 0.0 && response.hovered() {
                        let min_dim = src_w.min(src_h) as f32;
                        let delta = scroll * 0.5;
                        let new_size = (self.crop_size + delta).max(32.0).min(min_dim);
                        // Resize centered
                        let size_diff = new_size - self.crop_size;
                        self.crop_offset.0 = (self.crop_offset.0 - size_diff / 2.0).max(0.0);
                        self.crop_offset.1 = (self.crop_offset.1 - size_diff / 2.0).max(0.0);
                        self.crop_size = new_size;
                        // Re-clamp
                        self.crop_offset.0 = self.crop_offset.0.min(src_w as f32 - self.crop_size);
                        self.crop_offset.1 = self.crop_offset.1.min(src_h as f32 - self.crop_size);
                    }
                });

                ui.add_space(8.0);
                ui.colored_label(
                    self.settings.theme.text_muted(),
                    "Drag to move. Scroll to resize.",
                );
                ui.add_space(4.0);

                ui.horizontal(|ui| {
                    let save_btn = egui::Button::new(
                        egui::RichText::new("Save").size(15.0).color(egui::Color32::WHITE),
                    )
                    .min_size(egui::vec2(100.0, 34.0))
                    .fill(self.settings.theme.btn_positive());
                    if ui.add(save_btn).clicked() {
                        save = true;
                    }

                    let cancel_btn = egui::Button::new(
                        egui::RichText::new("Cancel").size(15.0),
                    )
                    .min_size(egui::vec2(100.0, 34.0));
                    if ui.add(cancel_btn).clicked() {
                        close = true;
                    }
                });
            });

        if save {
            if let Some(ref bytes) = self.crop_source_bytes.clone() {
                let cx = self.crop_offset.0 as u32;
                let cy = self.crop_offset.1 as u32;
                let cs = self.crop_size as u32;
                if let Ok(png_data) = avatar::process_avatar(bytes, cx, cy, cs) {
                    if let Some(ref group_id) = self.group_avatar_crop_group_id.clone() {
                        // Save as group avatar
                        if avatar::save_group_avatar(group_id, &png_data).is_ok() {
                            // Update group: avatar_sha256
                            let sha256 = avatar::avatar_sha256(&png_data);
                            if let Some(grp) = self.groups.iter_mut().find(|g| &g.group_id == group_id) {
                                grp.avatar_sha256 = Some(sha256);
                                crate::group::save_group(grp);
                            }
                            // Invalidate texture cache
                            self.group_avatar_textures.remove(group_id);
                            // Find group index for broadcasting
                            if let Some(gidx) = self.groups.iter().position(|g| &g.group_id == group_id) {
                                self.broadcast_group_update(gidx);
                                self.broadcast_group_avatar(gidx, png_data, sha256);
                            }
                        }
                        self.group_avatar_crop_group_id = None;
                    } else {
                        // Personal avatar
                        if avatar::save_own_avatar(&png_data).is_ok() {
                            // Force texture reload
                            self.own_avatar_texture = None;
                            // Broadcast to connected peers
                            let sha256 = avatar::avatar_sha256(&png_data);
                            if let Some(tx) = &self.msg_cmd_tx {
                                tx.send(MsgCommand::BroadcastAvatar {
                                    avatar_data: png_data,
                                    sha256,
                                }).ok();
                            }
                        }
                    }
                }
            }
            close = true;
        }

        if close {
            self.show_crop_editor = false;
            self.crop_source_bytes = None;
            self.crop_source_texture = None;
            self.crop_source_dims = (0, 0);
            self.group_avatar_crop_group_id = None;
        }
    }
}
