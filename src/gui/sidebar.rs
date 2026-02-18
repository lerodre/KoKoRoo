use eframe::egui;
use super::{HostelApp, SidebarTab};

impl HostelApp {
    pub(crate) fn draw_sidebar(&mut self, ui: &mut egui::Ui, in_call: bool) {
        ui.add_space(8.0);

        // Logo: load texture once (cropped to remove transparent padding)
        let texture = self.logo_texture.get_or_insert_with(|| {
            let (rgba, w, h) = super::load_logo_cropped();
            let size = [w as usize, h as usize];
            let pixels = egui::ColorImage::from_rgba_unmultiplied(size, &rgba);
            ui.ctx().load_texture("app-logo", pixels, egui::TextureOptions::LINEAR)
        });
        let available_w = ui.available_width();
        let aspect = texture.size()[1] as f32 / texture.size()[0] as f32;
        let logo_size = egui::vec2(available_w, available_w * aspect);
        ui.vertical_centered(|ui| {
            ui.image(egui::load::SizedTexture::new(texture.id(), logo_size));
        });

        ui.add_space(4.0);
        ui.vertical_centered(|ui| {
            let total_unread: u32 = self.msg_unread.values().sum();

            let incoming_count = self.req_incoming.len();

            let tabs: Vec<(SidebarTab, String)> = vec![
                (SidebarTab::Profile, "Profile".to_string()),
                (SidebarTab::Contacts, "Contacts".to_string()),
                (SidebarTab::Requests, if incoming_count > 0 {
                    format!("Requests ({})", incoming_count)
                } else {
                    "Requests".to_string()
                }),
                (SidebarTab::Messages, if total_unread > 0 {
                    format!("Messages ({})", total_unread)
                } else {
                    "Messages".to_string()
                }),
                (SidebarTab::Call, "Call".to_string()),
                (SidebarTab::Settings, "Settings".to_string()),
                (SidebarTab::Appearance, "Colors".to_string()),
            ];
            for (tab, label) in &tabs {
                let tab = *tab;
                let is_selected = self.active_tab == tab;
                let enabled = !in_call || tab == SidebarTab::Call || tab == SidebarTab::Appearance;

                let text = egui::RichText::new(label.as_str()).size(11.0);
                let text = if !enabled {
                    text.color(self.settings.theme.text_muted())
                } else if is_selected {
                    text.strong()
                } else {
                    text
                };

                let btn = egui::Button::new(text)
                    .min_size(egui::vec2(76.0, 32.0))
                    .frame(is_selected);

                let resp = ui.add_enabled(enabled, btn);
                if resp.clicked() {
                    self.active_tab = tab;
                }
            }
        });
    }
}
