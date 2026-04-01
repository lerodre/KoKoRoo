use eframe::egui;

use super::GroupView;
use crate::group::{self, Group, GroupMember};
use crate::gui::HostelApp;
use crate::identity;

impl HostelApp {
    pub(super) fn draw_group_create(&mut self, ui: &mut egui::Ui) {
        ui.add_space(8.0);

        ui.horizontal(|ui| {
            if ui.button("<- Back").clicked() {
                self.group_view = GroupView::List;
            }
            ui.heading("Create Group");
        });

        ui.separator();
        ui.add_space(8.0);

        ui.horizontal(|ui| {
            ui.label("Group name:");
            let te = egui::TextEdit::singleline(&mut self.group_create_name)
                .hint_text("Enter group name…")
                .frame(true);
            ui.add(te);
        });

        ui.add_space(12.0);
        ui.label(egui::RichText::new("Select members:").strong());
        ui.add_space(4.0);

        // Ensure selected_members vec matches contacts length
        if self.group_selected_members.len() != self.contacts.len() {
            self.group_selected_members = vec![false; self.contacts.len()];
        }

        egui::ScrollArea::vertical().max_height(300.0).show(ui, |ui| {
            for (i, contact) in self.contacts.iter().enumerate() {
                ui.horizontal(|ui| {
                    ui.checkbox(&mut self.group_selected_members[i], "");
                    ui.label(&contact.nickname);
                    ui.label(
                        egui::RichText::new(&contact.fingerprint)
                            .size(11.0)
                            .color(self.settings.theme.text_muted()),
                    );
                });
            }
        });

        ui.add_space(12.0);

        let selected_count = self.group_selected_members.iter().filter(|&&s| s).count();
        let name_valid = !self.group_create_name.trim().is_empty();
        let can_create = name_valid && selected_count >= 1;

        ui.horizontal(|ui| {
            let create_btn = egui::Button::new(
                egui::RichText::new(format!("Create ({} members)", selected_count + 1))
                    .strong(),
            );
            if ui.add_enabled(can_create, create_btn).clicked() {
                self.create_group();
            }
            if !name_valid {
                ui.label(
                    egui::RichText::new("Enter a group name")
                        .color(self.settings.theme.text_muted()),
                );
            }
        });
    }

    fn create_group(&mut self) {
        let group_key = group::generate_group_key();
        let group_id = group::generate_group_id();
        let now = identity::now_timestamp();

        let now_ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();

        // Add ourselves as member 0 (admin, founding member)
        let mut members = vec![GroupMember {
            pubkey: self.identity.pubkey,
            nickname: self.settings.nickname.clone(),
            fingerprint: self.identity.fingerprint.clone(),
            sender_index: 0,
            address: self.best_ipv6.clone(),
            port: self.local_port.clone(),
            is_admin: true,
            joined_at: 0, // founding member
        }];

        // Add selected contacts
        let mut next_index: u16 = 1;
        for (i, contact) in self.contacts.iter().enumerate() {
            if self.group_selected_members.get(i).copied().unwrap_or(false) {
                members.push(GroupMember {
                    pubkey: contact.pubkey,
                    nickname: contact.nickname.clone(),
                    fingerprint: contact.fingerprint.clone(),
                    sender_index: next_index,
                    address: contact.last_address.clone(),
                    port: contact.last_port.clone(),
                    is_admin: false,
                    joined_at: now_ts,
                });
                next_index += 1;
            }
        }

        let mut grp = Group {
            group_id,
            name: self.group_create_name.trim().to_string(),
            created_by: self.identity.pubkey,
            created_at: now,
            members,
            group_key,
            next_sender_index: next_index,
            avatar_sha256: None,
            text_channels: Vec::new(),
            voice_channels: Vec::new(),
            call_mode: group::CallMode::default(),
            key_version: 0,
            previous_key: None,
        };
        group::ensure_general_channel(&mut grp);
        group::ensure_fallback_channel(&mut grp);
        group::ensure_general_voice_channel(&mut grp);

        group::save_group(&grp);
        log_fmt!("[gui] group created: '{}' (id={})", grp.name, grp.group_id);

        // Send lite invite to each member via messaging daemon
        {
            let mut lite = crate::group::GroupInviteLite::from_group(&grp);
            log_fmt!("[gui] sending group lite invite '{}' ({} members)", grp.name, grp.members.len());
            if let Some(tx) = &self.msg_cmd_tx {
                for member in &grp.members {
                    if member.pubkey == self.identity.pubkey {
                        continue;
                    }
                    lite.your_sender_index = member.sender_index;
                    if let Ok(invite_json) = serde_json::to_vec(&lite) {
                        if let Some(contact) = self.contacts.iter().find(|c| c.pubkey == member.pubkey) {
                            if !contact.last_address.is_empty() && !contact.last_port.is_empty() {
                                let addr_str = format!("[{}]:{}", contact.last_address, contact.last_port);
                                if let Ok(addr) = addr_str.parse() {
                                    log_fmt!("[gui]   invite -> {} ({}) at {} ({} bytes)", member.nickname, contact.contact_id, addr_str, invite_json.len());
                                    tx.send(crate::messaging::MsgCommand::SendGroupInvite {
                                        contact_id: contact.contact_id.clone(),
                                        peer_addr: addr,
                                        peer_pubkey: contact.pubkey,
                                        invite_json: invite_json.clone(),
                                        members: grp.members.clone(),
                                    }).ok();
                                } else {
                                    log_fmt!("[gui]   invite SKIP {} - bad address: {}", member.nickname, addr_str);
                                }
                            } else {
                                log_fmt!("[gui]   invite SKIP {} - no address (addr='{}' port='{}')",
                                    member.nickname, contact.last_address, contact.last_port);
                            }
                        } else {
                            log_fmt!("[gui]   invite SKIP {} - contact not found in local contacts", member.nickname);
                        }
                    }
                }
            } else {
                log_fmt!("[gui]   invite SKIP - no msg_cmd_tx (daemon not running?)");
            }
        }

        self.groups.push(grp);
        self.group_view = GroupView::List;
        self.group_create_name.clear();
    }
}
