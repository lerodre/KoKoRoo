use eframe::egui;
use super::{HostelApp, list_audio_devices};

impl HostelApp {
    pub(crate) fn draw_settings_tab(&mut self, ui: &mut egui::Ui) {
        ui.add_space(10.0);
        ui.heading("Settings");
        ui.add_space(10.0);

        // Network mode
        ui.label("Network mode:");
        let prev_mode = self.settings.network_mode;
        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.settings.network_mode, 0, "LAN");
            ui.selectable_value(&mut self.settings.network_mode, 1, "Internet");
        });
        if self.settings.network_mode != prev_mode {
            self.settings.save();
        }

        ui.add_space(8.0);
        ui.separator();
        ui.add_space(4.0);

        // Microphone
        ui.label("Microphone:");
        let prev_input = self.selected_input;
        egui::ComboBox::from_id_salt("settings_mic").width(300.0)
            .selected_text(self.devices.input_names.get(self.selected_input).map(|s| s.as_str()).unwrap_or("none"))
            .show_ui(ui, |ui| {
                for (i, name) in self.devices.input_names.iter().enumerate() {
                    ui.selectable_value(&mut self.selected_input, i, name.as_str());
                }
            });
        if self.selected_input != prev_input {
            self.settings.mic = self.devices.input_names.get(self.selected_input)
                .cloned().unwrap_or_default();
            self.settings.save();
        }

        // Speakers
        ui.label("Speakers:");
        let prev_output = self.selected_output;
        egui::ComboBox::from_id_salt("settings_spk").width(300.0)
            .selected_text(self.devices.output_names.get(self.selected_output).map(|s| s.as_str()).unwrap_or("none"))
            .show_ui(ui, |ui| {
                for (i, name) in self.devices.output_names.iter().enumerate() {
                    ui.selectable_value(&mut self.selected_output, i, name.as_str());
                }
            });
        if self.selected_output != prev_output {
            self.settings.speakers = self.devices.output_names.get(self.selected_output)
                .cloned().unwrap_or_default();
            self.settings.save();
        }

        ui.add_space(8.0);
        ui.separator();
        ui.add_space(4.0);

        // Local port
        ui.label("Local port:");
        let resp = ui.add(egui::TextEdit::singleline(&mut self.local_port).desired_width(120.0));
        if resp.lost_focus() {
            self.settings.local_port = self.local_port.clone();
            self.settings.save();
        }

        ui.add_space(10.0);
        if ui.button("Refresh Audio Devices").clicked() {
            self.devices = list_audio_devices();
            // Re-match saved names
            self.selected_input = if !self.settings.mic.is_empty() {
                self.devices.input_names.iter().position(|n| n == &self.settings.mic).unwrap_or(0)
            } else {
                0
            };
            self.selected_output = if !self.settings.speakers.is_empty() {
                self.devices.output_names.iter().position(|n| n == &self.settings.speakers).unwrap_or(0)
            } else {
                0
            };
        }
    }
}
