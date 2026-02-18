use eframe::egui;
use super::{HostelApp, get_best_ipv6, censor_ip};

impl HostelApp {
    pub(crate) fn draw_profile_tab(&mut self, ui: &mut egui::Ui) {
        ui.add_space(10.0);
        ui.heading("Profile");
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
            let resp = ui.add(
                egui::TextEdit::singleline(&mut self.settings.nickname)
                    .desired_width(200.0)
                    .hint_text("optional")
            );
            if resp.changed() {
                self.settings.save();
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
}
