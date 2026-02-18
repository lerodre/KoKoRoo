use eframe::egui;
use super::{HostelApp, SidebarTab};

impl HostelApp {
    pub(crate) fn draw_sidebar(&mut self, ui: &mut egui::Ui, in_call: bool) {
        ui.add_space(8.0);
        ui.vertical_centered(|ui| {
            let total_unread: u32 = self.msg_unread.values().sum();

            let tabs: Vec<(SidebarTab, String)> = vec![
                (SidebarTab::Profile, "Profile".to_string()),
                (SidebarTab::Contacts, "Contacts".to_string()),
                (SidebarTab::Messages, if total_unread > 0 {
                    format!("Messages ({})", total_unread)
                } else {
                    "Messages".to_string()
                }),
                (SidebarTab::Call, "Call".to_string()),
                (SidebarTab::Settings, "Settings".to_string()),
            ];
            for (tab, label) in &tabs {
                let tab = *tab;
                let is_selected = self.active_tab == tab;
                let enabled = !in_call || tab == SidebarTab::Call;

                let text = egui::RichText::new(label.as_str()).size(11.0);
                let text = if !enabled {
                    text.color(egui::Color32::from_gray(100))
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
