use eframe::egui;
use super::{HostelApp, Screen};

impl HostelApp {
    pub(crate) fn draw_error(&mut self, ui: &mut egui::Ui, message: &str) {
        ui.vertical_centered(|ui| {
            ui.add_space(60.0);
            ui.colored_label(self.settings.theme.error(),
                egui::RichText::new("Connection Failed").size(24.0));
            ui.add_space(15.0);
            ui.label(message);
            ui.add_space(25.0);
            if ui.add(egui::Button::new("Back").min_size(egui::vec2(140.0, 40.0))).clicked() {
                self.screen = Screen::Setup;
            }
        });
    }
}
