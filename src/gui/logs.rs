use eframe::egui;

use super::HostelApp;

impl HostelApp {
    pub(crate) fn draw_logs_tab(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.heading("Logs");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("Clear").clicked() {
                    if let Ok(mut buf) = crate::logger::get_log_buffer().lock() {
                        buf.clear();
                    }
                }
                if ui.button("Copy All").clicked() {
                    let all = crate::logger::get_log_lines().join("\n");
                    ui.ctx().copy_text(all);
                }
            });
        });
        ui.separator();

        let lines = crate::logger::get_log_lines();
        let text_style = egui::TextStyle::Monospace;
        let row_height = ui.text_style_height(&text_style);

        egui::ScrollArea::vertical()
            .id_salt("logs_scroll")
            .stick_to_bottom(true)
            .auto_shrink([false, false])
            .show_rows(ui, row_height, lines.len(), |ui, row_range| {
                for i in row_range {
                    if let Some(line) = lines.get(i) {
                        ui.label(egui::RichText::new(line).monospace().size(11.0));
                    }
                }
            });
    }
}
