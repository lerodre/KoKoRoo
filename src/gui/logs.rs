use eframe::egui;

use super::{HostelApp, LogFilter};

fn matches_log_filter(line: &str, filter: LogFilter) -> bool {
    match filter {
        LogFilter::All => true,
        LogFilter::Daemon => line.contains("] [daemon]"),
        LogFilter::Groups => {
            line.contains("] [group]")
                || line.contains("] [group-chat]")
                || line.contains("] [groupcall]")
                || line.contains("] [p2p]")
                || line.contains("] [failover]")
                || line.contains("] [grp-screen]")
        }
        LogFilter::Voice => {
            line.contains("] [voice]")
                || line.contains("] [sysaudio]")
                || line.contains("] [sck_audio]")
                || line.contains("] [screen]")
                || line.contains("] [wayland]")
        }
        LogFilter::Gui => line.contains("] [gui]"),
        LogFilter::Network => line.contains("] [firewall]") || line.contains("] [settings]"),
    }
}

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
                    let all = crate::logger::get_log_lines();
                    let filtered: Vec<&str> = all
                        .iter()
                        .filter(|l| matches_log_filter(l, self.log_filter))
                        .map(|s| s.as_str())
                        .collect();
                    ui.ctx().copy_text(filtered.join("\n"));
                }
            });
        });

        // Sub-tab buttons
        ui.horizontal(|ui| {
            let tabs = [
                (LogFilter::All, "All"),
                (LogFilter::Daemon, "Daemon"),
                (LogFilter::Groups, "Groups"),
                (LogFilter::Voice, "Voice"),
                (LogFilter::Gui, "GUI"),
                (LogFilter::Network, "Network"),
            ];
            for (filter, label) in tabs {
                let is_active = self.log_filter == filter;
                if ui.selectable_label(is_active, label).clicked() {
                    self.log_filter = filter;
                }
            }
        });

        ui.separator();

        let all_lines = crate::logger::get_log_lines();
        let filter = self.log_filter;
        let lines: Vec<&str> = all_lines
            .iter()
            .filter(|l| matches_log_filter(l, filter))
            .map(|s| s.as_str())
            .collect();

        let text_style = egui::TextStyle::Monospace;
        let row_height = ui.text_style_height(&text_style);

        egui::ScrollArea::vertical()
            .id_salt("logs_scroll")
            .stick_to_bottom(true)
            .auto_shrink([false, false])
            .show_rows(ui, row_height, lines.len(), |ui, row_range| {
                for i in row_range {
                    if let Some(line) = lines.get(i) {
                        ui.label(egui::RichText::new(*line).monospace().size(11.0));
                    }
                }
            });
    }
}
