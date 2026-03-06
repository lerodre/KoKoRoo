use eframe::egui;

use crate::group::{self, GroupMember};
use crate::gui::HostelApp;
use crate::identity;

impl HostelApp {
    /// Broadcast a group metadata update to all members of a group.
    pub(crate) fn broadcast_group_update(&self, group_idx: usize) {
        if group_idx >= self.groups.len() {
            return;
        }
        let grp = &self.groups[group_idx];
        let group_json = match serde_json::to_vec(grp) {
            Ok(j) => j,
            Err(_) => return,
        };
        let member_contacts = self.group_member_contacts(group_idx);
        if let Some(tx) = &self.msg_cmd_tx {
            tx.send(crate::messaging::MsgCommand::SendGroupUpdate {
                group_id: grp.group_id.clone(),
                group_json,
                member_contacts,
            }).ok();
        }
    }

    /// Broadcast a group avatar to all members of a group.
    pub(crate) fn broadcast_group_avatar(&self, group_idx: usize, avatar_data: Vec<u8>, sha256: [u8; 32]) {
        if group_idx >= self.groups.len() {
            return;
        }
        let grp = &self.groups[group_idx];
        let member_contacts = self.group_member_contacts(group_idx);
        if let Some(tx) = &self.msg_cmd_tx {
            tx.send(crate::messaging::MsgCommand::SendGroupAvatar {
                group_id: grp.group_id.clone(),
                avatar_data,
                sha256,
                member_contacts,
            }).ok();
        }
    }

    /// Build the (contact_id, addr, pubkey) list for all members of a group, excluding ourselves.
    fn group_member_contacts(&self, group_idx: usize) -> Vec<(String, std::net::SocketAddr, [u8; 32])> {
        let grp = &self.groups[group_idx];
        let my_pubkey = self.identity.pubkey;
        let mut result = Vec::new();
        for member in &grp.members {
            if member.pubkey == my_pubkey {
                continue;
            }
            if member.address.is_empty() || member.port.is_empty() {
                continue;
            }
            let addr_str = format!("[{}]:{}", member.address, member.port);
            if let Ok(addr) = addr_str.parse() {
                let contact_id = identity::derive_contact_id(&my_pubkey, &member.pubkey);
                result.push((contact_id, addr, member.pubkey));
            }
        }
        result
    }

    /// Invite selected contacts to an existing group.
    pub(super) fn invite_members_to_group(&mut self, group_idx: usize) {
        if group_idx >= self.groups.len() {
            return;
        }
        let my_pubkey = self.identity.pubkey;
        let existing_pubkeys: Vec<[u8; 32]> = self.groups[group_idx].members.iter().map(|m| m.pubkey).collect();

        // Collect contacts to invite
        let mut new_members = Vec::new();
        for (ci, contact) in self.contacts.iter().enumerate() {
            if self.group_settings_selected_members.get(ci).copied().unwrap_or(false)
                && !existing_pubkeys.contains(&contact.pubkey)
            {
                let next_idx = self.groups[group_idx].next_sender_index;
                new_members.push((ci, GroupMember {
                    pubkey: contact.pubkey,
                    nickname: contact.nickname.clone(),
                    fingerprint: contact.fingerprint.clone(),
                    sender_index: next_idx,
                    address: contact.last_address.clone(),
                    port: contact.last_port.clone(),
                    is_admin: false,
                }));
                self.groups[group_idx].next_sender_index += 1;
            }
        }

        // Add new members to the group
        for (_, member) in &new_members {
            log_fmt!("[gui] member added to group {}: {}", self.groups[group_idx].group_id, member.nickname);
            self.groups[group_idx].members.push(member.clone());
        }
        group::save_group(&self.groups[group_idx]);

        // Send lite invites to new members
        {
            let grp = &self.groups[group_idx];
            let mut lite = crate::group::GroupInviteLite::from_group(grp);
            if let Some(tx) = &self.msg_cmd_tx {
                for (_, member) in &new_members {
                    if member.address.is_empty() || member.port.is_empty() {
                        continue;
                    }
                    lite.your_sender_index = member.sender_index;
                    if let Ok(invite_json) = serde_json::to_vec(&lite) {
                        let addr_str = format!("[{}]:{}", member.address, member.port);
                        if let Ok(addr) = addr_str.parse() {
                            let contact_id = identity::derive_contact_id(&my_pubkey, &member.pubkey);
                            tx.send(crate::messaging::MsgCommand::SendGroupInvite {
                                contact_id,
                                peer_addr: addr,
                                peer_pubkey: member.pubkey,
                                invite_json,
                                members: grp.members.clone(),
                            }).ok();
                        }
                    }
                }
            }
        }

        // Broadcast update to existing members
        self.broadcast_group_update(group_idx);
    }
}

/// Paint a fallback avatar: colored circle with the first letter of the nickname.
pub(crate) fn paint_initial_avatar(
    painter: &egui::Painter,
    rect: egui::Rect,
    nickname: &str,
    _theme: &crate::theme::Theme,
) {
    // Deterministic color from nickname hash
    let hash = {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        nickname.hash(&mut hasher);
        hasher.finish()
    };
    let hue = (hash % 360) as f32;
    let r = ((hue * std::f32::consts::PI / 180.0).cos() * 40.0 + 110.0) as u8;
    let g = (((hue + 120.0) * std::f32::consts::PI / 180.0).cos() * 40.0 + 110.0) as u8;
    let b = (((hue + 240.0) * std::f32::consts::PI / 180.0).cos() * 40.0 + 110.0) as u8;
    let bg_color = egui::Color32::from_rgb(r, g, b);

    let center = rect.center();
    let radius = rect.width().min(rect.height()) / 2.0;
    painter.circle_filled(center, radius, bg_color);

    // Draw initial letter
    let initial = nickname
        .chars()
        .next()
        .map(|c| c.to_uppercase().to_string())
        .unwrap_or_else(|| "?".to_string());
    painter.text(
        center,
        egui::Align2::CENTER_CENTER,
        initial,
        egui::FontId::proportional(radius * 1.1),
        egui::Color32::WHITE,
    );
}
