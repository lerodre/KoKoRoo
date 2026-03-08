use eframe::egui;
use super::{HostelApp, SidebarTab, load_icon_texture_sized};

impl HostelApp {
    pub(crate) fn draw_sidebar(&mut self, ui: &mut egui::Ui, in_call: bool, _in_group_call: bool) {
        ui.add_space(8.0);

        // Logo: load texture once (cropped + downscaled with Lanczos3 for crisp display)
        let texture = self.logo_texture.get_or_insert_with(|| {
            load_icon_texture_sized(ui.ctx(), "app-logo", include_bytes!("../../assets/logo.png"), 128)
        });
        let available_w = ui.available_width();
        let aspect = texture.size()[1] as f32 / texture.size()[0] as f32;
        let logo_size = egui::vec2(available_w, available_w * aspect);
        ui.vertical_centered(|ui| {
            ui.image(egui::load::SizedTexture::new(texture.id(), logo_size));
        });

        ui.add_space(4.0);

        // Preload icon textures for Call and Settings
        let call_tex = self.call_icon_texture.get_or_insert_with(|| {
            load_icon_texture_sized(ui.ctx(), "icon-call", include_bytes!("../../assets/call.png"), 48)
        }).clone();
        let settings_tex = self.settings_icon_texture.get_or_insert_with(|| {
            load_icon_texture_sized(ui.ctx(), "icon-settings", include_bytes!("../../assets/settings.png"), 48)
        }).clone();

        let badge_color = self.settings.theme.btn_negative();

        ui.vertical_centered(|ui| {
            let total_unread: u32 = self.msg_unread.values().sum();
            let incoming_count = self.req_incoming.len();

            let icon_h = 30.0; // icon height in sidebar buttons

            let friends_badge = incoming_count as u32;
            let tabs: Vec<(SidebarTab, String, u32)> = vec![
                (SidebarTab::Profile, "Profile".to_string(), 0),
                (SidebarTab::Friends, "Friends".to_string(), friends_badge),
                (SidebarTab::Messages, "Messages".to_string(), total_unread),
                (SidebarTab::Groups, "Groups".to_string(), self.group_unread.values().sum()),
                (SidebarTab::Call, "Call".to_string(), 0),
                (SidebarTab::Settings, "Settings".to_string(), 0),
                (SidebarTab::Appearance, "Colors".to_string(), 0),
                (SidebarTab::Logs, "Logs".to_string(), 0),
            ];

            // Collect badge positions to draw on top after all buttons
            let mut badges: Vec<(egui::Rect, u32)> = Vec::new();

            for (tab, label, badge_count) in &tabs {
                let tab = *tab;
                let badge_count = *badge_count;
                let is_selected = self.active_tab == tab;
                let enabled = if in_call {
                    tab == SidebarTab::Call || tab == SidebarTab::Appearance || tab == SidebarTab::Logs
                } else {
                    true
                };

                let text = egui::RichText::new(label.as_str()).size(13.0);
                let text = if !enabled {
                    text.color(self.settings.theme.text_muted())
                } else if is_selected {
                    text.strong()
                } else {
                    text
                };

                // For Call and Settings tabs, show icon + text
                let icon_tex = match tab {
                    SidebarTab::Call => Some(&call_tex),
                    SidebarTab::Settings => Some(&settings_tex),
                    _ => None,
                };

                let resp = if let Some(tex) = icon_tex {
                    let icon_aspect = tex.size()[0] as f32 / tex.size()[1] as f32;
                    let icon_w = icon_h * icon_aspect;
                    let icon_sized = egui::load::SizedTexture::new(tex.id(), egui::vec2(icon_w, icon_h));
                    let btn = egui::Button::image_and_text(icon_sized, text)
                        .min_size(egui::vec2(116.0, 38.0));
                    ui.add_enabled(enabled, btn)
                } else {
                    let btn = egui::Button::new(text)
                        .min_size(egui::vec2(116.0, 38.0));
                    ui.add_enabled(enabled, btn)
                };

                if badge_count > 0 {
                    badges.push((resp.rect, badge_count));
                }

                if resp.clicked() {
                    self.active_tab = tab;
                }
            }

            // Draw badge bubbles on top of all buttons
            let painter = ui.painter();
            for (rect, count) in badges {
                let badge_text = format!("{}", count);
                let font = egui::FontId::proportional(10.0);
                let text_galley = painter.layout_no_wrap(badge_text, font, egui::Color32::WHITE);
                let text_w = text_galley.size().x;
                let text_h = text_galley.size().y;
                let radius = (text_w / 2.0 + 4.0).max(8.0);

                let badge_center = egui::pos2(
                    rect.right() - 12.0,
                    rect.top() + 12.0,
                );

                painter.circle_filled(badge_center, radius, badge_color);
                painter.galley(
                    egui::pos2(badge_center.x - text_w / 2.0, badge_center.y - text_h / 2.0),
                    text_galley,
                    egui::Color32::WHITE,
                );
            }
        });
    }
}
