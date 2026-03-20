mod create;
mod detail;
mod voice;
mod settings;
mod sidebar;
mod helpers;

use eframe::egui;

use super::HostelApp;

#[derive(Clone, Copy, PartialEq)]
pub(crate) enum GroupView {
    List,
    Create,
    Detail,
    Settings,
    Connecting,
    InCall,
}

impl HostelApp {
    pub(crate) fn draw_groups_tab(&mut self, ui: &mut egui::Ui) {
        // Auto-clear unread for the currently viewed group+channel
        if let Some(idx) = self.group_detail_idx {
            if let Some(grp) = self.groups.get(idx) {
                self.group_unread.remove(&grp.group_id);
                self.group_channel_unread.remove(&(grp.group_id.clone(), self.group_selected_channel.clone()));
            }
        }

        match self.group_view {
            GroupView::List | GroupView::Detail | GroupView::Settings | GroupView::InCall | GroupView::Connecting => {
                // Always 3-column: icon strip (48px) + channels (140px) + detail/placeholder
                let available = ui.available_rect_before_wrap();
                let clip = ui.clip_rect();
                let sep_w = 1.0;
                let line_stroke = egui::Stroke::new(sep_w, self.settings.theme.text_muted());

                let mut open_idx: Option<usize> = None;
                let mut go_create = false;

                let icon_w = 48.0;
                let chan_w = 140.0;
                let detail_w = (available.width() - icon_w - chan_w - sep_w * 2.0 - 4.0).max(100.0);

                // Icon strip background
                let icon_bg = egui::Rect::from_min_max(
                    egui::pos2(clip.min.x, clip.min.y),
                    egui::pos2(clip.min.x + icon_w + (available.min.x - clip.min.x), clip.max.y),
                );
                ui.painter().rect_filled(icon_bg, 0.0, self.settings.theme.sidebar_bg());

                // Channels background
                let chan_x = available.min.x + icon_w + sep_w;
                let chan_bg = egui::Rect::from_min_max(
                    egui::pos2(chan_x, clip.min.y),
                    egui::pos2(chan_x + chan_w, clip.max.y),
                );
                ui.painter().rect_filled(chan_bg, 0.0, self.settings.theme.panel_bg());

                // Vertical separators
                ui.painter().vline(clip.min.x, clip.y_range(), line_stroke);
                let sep1_x = available.min.x + icon_w;
                ui.painter().vline(sep1_x, clip.y_range(), line_stroke);
                let sep2_x = chan_x + chan_w;
                ui.painter().vline(sep2_x, clip.y_range(), line_stroke);

                let icon_visual_w = icon_w + (available.min.x - clip.min.x);
                let icon_rect = egui::Rect::from_min_size(
                    egui::pos2(clip.min.x, available.min.y),
                    egui::vec2(icon_visual_w, available.height()),
                );
                let chan_rect = egui::Rect::from_min_size(
                    egui::pos2(chan_x, available.min.y),
                    egui::vec2(chan_w, available.height()),
                );
                let detail_rect = egui::Rect::from_min_size(
                    egui::pos2(sep2_x + sep_w + 2.0, available.min.y),
                    egui::vec2(detail_w, available.height()),
                );

                // Icon strip
                ui.allocate_new_ui(egui::UiBuilder::new().max_rect(icon_rect), |ui| {
                    self.draw_group_icon_strip(ui, &mut open_idx, &mut go_create);
                });

                // Channels sidebar
                let grp_name = self.group_detail_idx
                    .and_then(|i| self.groups.get(i))
                    .map(|g| g.name.clone())
                    .unwrap_or_default();
                ui.allocate_new_ui(egui::UiBuilder::new().max_rect(chan_rect), |ui| {
                    self.draw_channels_sidebar(ui, &grp_name);
                });

                // Detail panel
                ui.allocate_new_ui(egui::UiBuilder::new().max_rect(detail_rect), |ui| {
                    if self.group_settings_idx.is_some() && self.group_view == GroupView::Settings {
                        self.draw_group_settings(ui);
                    } else if self.group_detail_idx.is_some() && self.group_view == GroupView::Connecting {
                        self.draw_group_connecting(ui);
                    } else if self.group_detail_idx.is_some()
                        && (self.group_view == GroupView::Detail || self.group_view == GroupView::Settings || self.group_view == GroupView::InCall)
                    {
                        self.draw_group_detail(ui);
                    } else {
                        ui.add_space(40.0);
                        ui.vertical_centered(|ui| {
                            ui.colored_label(
                                self.settings.theme.text_muted(),
                                "Select a group to start chatting",
                            );
                        });
                    }
                });

                // Deferred actions
                if let Some(idx) = open_idx {
                    self.group_detail_idx = Some(idx);
                    self.group_settings_idx = None;
                    self.group_selected_channel = "general".to_string();
                    self.group_channel_creating = false;
                    self.group_channel_create_name.clear();
                    self.group_view = GroupView::Detail;
                    // Clear unread badges for this group
                    if let Some(grp) = self.groups.get(idx) {
                        self.group_unread.remove(&grp.group_id);
                        // Clear the "general" channel unread since we auto-select it
                        self.group_channel_unread.remove(&(grp.group_id.clone(), "general".to_string()));
                    }
                }
                if go_create {
                    self.group_view = GroupView::Create;
                    self.group_create_name.clear();
                    self.group_selected_members = vec![false; self.contacts.len()];
                }
            }
            GroupView::Create => self.draw_group_create(ui),
        }
    }
}
