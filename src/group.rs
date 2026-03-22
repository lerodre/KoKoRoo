use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

/// Where KoKoRoo stores its data.
fn data_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join(".kokoroo")
}

/// Groups directory: ~/.kokoroo/groups/
fn groups_dir() -> PathBuf {
    data_dir().join("groups")
}

/// Group chat history directory: ~/.kokoroo/groups/chats/
fn group_chats_dir() -> PathBuf {
    groups_dir().join("chats")
}

/// Unique group identifier (16 random bytes, hex-encoded = 32 chars).
pub type GroupId = String;

/// A member in the group roster.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct GroupMember {
    pub pubkey: [u8; 32],
    pub nickname: String,
    pub fingerprint: String,
    pub sender_index: u16,
    pub address: String,
    pub port: String,
    pub is_admin: bool,
}

/// A text channel inside a group.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct TextChannel {
    pub channel_id: String,
    pub name: String,
    pub created_at: u64,
    pub created_by: [u8; 32],
    pub deleted: bool,
    pub deleted_at: Option<u64>,
}

/// A voice channel inside a group.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct VoiceChannel {
    pub channel_id: String,
    pub name: String,
    pub created_at: u64,
    pub created_by: [u8; 32],
    pub deleted: bool,
    pub deleted_at: Option<u64>,
}

/// Call mode for group voice channels.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Default)]
pub enum CallMode {
    #[default]
    Relay,
    P2P,
}

/// Lightweight invite sent over the wire (small enough for a single UDP packet).
/// Uses hex-encoded strings for byte fields to avoid serde_json's verbose [u8; 32] arrays.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct GroupInviteLite {
    pub group_id: String,
    pub name: String,
    pub group_key_hex: String,
    pub created_by_hex: String,
    pub created_at: String,
    pub call_mode: CallMode,
    pub member_count: u16,
    pub your_sender_index: u16,
    pub next_sender_index: u16,
}

/// Wire-friendly group member (hex-encoded pubkey instead of [u8; 32]).
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct GroupMemberWire {
    pub pubkey_hex: String,
    pub nickname: String,
    pub fingerprint: String,
    pub sender_index: u16,
    pub address: String,
    pub port: String,
    pub is_admin: bool,
}

/// Hex-encode a 32-byte array to a 64-char string.
fn bytes_to_hex(bytes: &[u8; 32]) -> String {
    bytes.iter().map(|b| format!("{:02x}", b)).collect()
}

/// Decode a 64-char hex string back to [u8; 32].
fn hex_to_bytes32(hex: &str) -> Option<[u8; 32]> {
    if hex.len() != 64 { return None; }
    let mut out = [0u8; 32];
    for i in 0..32 {
        out[i] = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some(out)
}

impl GroupInviteLite {
    /// Create a lite invite from a full Group.
    pub fn from_group(group: &Group) -> Self {
        GroupInviteLite {
            group_id: group.group_id.clone(),
            name: group.name.clone(),
            group_key_hex: bytes_to_hex(&group.group_key),
            created_by_hex: bytes_to_hex(&group.created_by),
            created_at: group.created_at.clone(),
            call_mode: group.call_mode,
            member_count: group.members.len() as u16,
            your_sender_index: 0, // set by caller before sending
            next_sender_index: group.next_sender_index,
        }
    }

    /// Decode the hex group_key back to [u8; 32].
    pub fn group_key(&self) -> Option<[u8; 32]> {
        hex_to_bytes32(&self.group_key_hex)
    }

    /// Decode the hex created_by back to [u8; 32].
    pub fn created_by(&self) -> Option<[u8; 32]> {
        hex_to_bytes32(&self.created_by_hex)
    }

    /// Build a skeleton Group from the lite invite (no members yet — those come via member syncs).
    pub fn to_skeleton_group(&self, my_pubkey: &[u8; 32], my_nickname: &str, my_fingerprint: &str, my_address: &str, my_port: &str) -> Option<Group> {
        let group_key = self.group_key()?;
        let created_by = self.created_by()?;
        let me = GroupMember {
            pubkey: *my_pubkey,
            nickname: my_nickname.to_string(),
            fingerprint: my_fingerprint.to_string(),
            sender_index: self.your_sender_index,
            address: my_address.to_string(),
            port: my_port.to_string(),
            is_admin: false,
        };
        Some(Group {
            group_id: self.group_id.clone(),
            name: self.name.clone(),
            created_by,
            created_at: self.created_at.clone(),
            members: vec![me],
            group_key,
            next_sender_index: self.next_sender_index,
            avatar_sha256: None,
            text_channels: Vec::new(),
            voice_channels: Vec::new(),
            call_mode: self.call_mode,
            key_version: 0,
            previous_key: None,
        })
    }
}

impl GroupMemberWire {
    /// Convert a GroupMember to wire format.
    pub fn from_member(m: &GroupMember) -> Self {
        GroupMemberWire {
            pubkey_hex: bytes_to_hex(&m.pubkey),
            nickname: m.nickname.clone(),
            fingerprint: m.fingerprint.clone(),
            sender_index: m.sender_index,
            address: m.address.clone(),
            port: m.port.clone(),
            is_admin: m.is_admin,
        }
    }

    /// Convert wire format back to GroupMember.
    pub fn to_member(&self) -> Option<GroupMember> {
        let pubkey = hex_to_bytes32(&self.pubkey_hex)?;
        Some(GroupMember {
            pubkey,
            nickname: self.nickname.clone(),
            fingerprint: self.fingerprint.clone(),
            sender_index: self.sender_index,
            address: self.address.clone(),
            port: self.port.clone(),
            is_admin: self.is_admin,
        })
    }
}

/// Persisted group definition.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Group {
    pub group_id: GroupId,
    pub name: String,
    pub created_by: [u8; 32],
    pub created_at: String,
    pub members: Vec<GroupMember>,
    pub group_key: [u8; 32],
    pub next_sender_index: u16,
    #[serde(default)]
    pub avatar_sha256: Option<[u8; 32]>,
    #[serde(default)]
    pub text_channels: Vec<TextChannel>,
    #[serde(default)]
    pub voice_channels: Vec<VoiceChannel>,
    #[serde(default)]
    pub call_mode: CallMode,
    /// Incremented on each key rotation (member kicked).
    #[serde(default)]
    pub key_version: u32,
    /// Previous group key for decrypting messages from peers that haven't received the rotation yet.
    #[serde(default)]
    pub previous_key: Option<[u8; 32]>,
}

/// Generate a random 16-byte group ID (hex-encoded = 32 chars).
pub fn generate_group_id() -> GroupId {
    use rand::RngCore;
    let mut bytes = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut bytes);
    bytes.iter().map(|b| format!("{:02x}", b)).collect()
}

/// Generate a random 256-bit symmetric key for the group.
pub fn generate_group_key() -> [u8; 32] {
    use rand::RngCore;
    let mut key = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut key);
    key
}

/// Save a group to disk at ~/.kokoroo/groups/{group_id}.json
pub fn save_group(group: &Group) {
    let dir = groups_dir();
    fs::create_dir_all(&dir).ok();
    let path = dir.join(format!("{}.json", group.group_id));
    if let Ok(json) = serde_json::to_string_pretty(group) {
        fs::write(path, json).ok();
    }
}

/// Load a group from disk by its ID.
#[allow(dead_code)]
pub fn load_group(group_id: &str) -> Option<Group> {
    let path = groups_dir().join(format!("{}.json", group_id));
    let data = fs::read_to_string(path).ok()?;
    serde_json::from_str(&data).ok()
}

/// Load all saved groups, sorted by creation date (newest first).
pub fn load_all_groups() -> Vec<Group> {
    let dir = groups_dir();
    let mut groups = Vec::new();
    if let Ok(entries) = fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().map_or(false, |e| e == "json") {
                if let Ok(data) = fs::read_to_string(&path) {
                    if let Ok(group) = serde_json::from_str::<Group>(&data) {
                        groups.push(group);
                    }
                }
            }
        }
    }
    groups.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    groups
}

/// Delete a group from disk.
pub fn delete_group(group_id: &str) {
    let path = groups_dir().join(format!("{}.json", group_id));
    fs::remove_file(path).ok();
    // Also remove chat history (old format + per-channel files)
    let chat_path = group_chats_dir().join(format!("{}.enc", group_id));
    fs::remove_file(chat_path).ok();
    let chats_dir = group_chats_dir();
    if let Ok(entries) = fs::read_dir(&chats_dir) {
        let prefix = format!("{}_", group_id);
        for entry in entries.flatten() {
            if let Some(name) = entry.file_name().to_str() {
                if name.starts_with(&prefix) && name.ends_with(".enc") {
                    fs::remove_file(entry.path()).ok();
                }
            }
        }
    }
    // Also remove group avatar
    crate::avatar::delete_group_avatar(group_id);
}

/// Remove a member from a group by pubkey.
/// Rotates the group key so the removed member can no longer decrypt future traffic.
/// Returns true if a member was actually removed.
pub fn remove_member(group: &mut Group, pubkey: &[u8; 32]) -> bool {
    let before = group.members.len();
    let kicked_nick = group.members.iter()
        .find(|m| &m.pubkey == pubkey)
        .map(|m| m.nickname.clone())
        .unwrap_or_default();
    group.members.retain(|m| &m.pubkey != pubkey);
    if group.members.len() != before {
        // Rotate the group key
        group.previous_key = Some(group.group_key);
        group.group_key = generate_group_key();
        group.key_version += 1;
        log_fmt!("[group] key rotated to v{} after removing '{}' from '{}'",
            group.key_version, kicked_nick, group.name);
        save_group(group);
        true
    } else {
        false
    }
}

/// Find a member in the group by pubkey.
#[allow(dead_code)]
pub fn find_member_by_pubkey<'a>(group: &'a Group, pubkey: &[u8; 32]) -> Option<&'a GroupMember> {
    group.members.iter().find(|m| &m.pubkey == pubkey)
}

/// Find a member in the group by sender_index.
#[allow(dead_code)]
pub fn find_member_by_index(group: &Group, sender_index: u16) -> Option<&GroupMember> {
    group.members.iter().find(|m| m.sender_index == sender_index)
}

/// Generate a random channel ID (16 random bytes, hex-encoded = 32 chars).
pub fn generate_channel_id() -> String {
    use rand::RngCore;
    let mut bytes = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut bytes);
    bytes.iter().map(|b| format!("{:02x}", b)).collect()
}

/// Ensure the group has a "general" channel. Inserts one at index 0 if missing.
pub fn ensure_general_channel(group: &mut Group) {
    if !group.text_channels.iter().any(|ch| ch.channel_id == "general") {
        group.text_channels.insert(0, TextChannel {
            channel_id: "general".to_string(),
            name: "general".to_string(),
            created_at: 0,
            created_by: group.created_by,
            deleted: false,
            deleted_at: None,
        });
    }
}

/// Ensure the group has a "voice_general" voice channel. Inserts one at index 0 if missing.
pub fn ensure_general_voice_channel(group: &mut Group) {
    if !group.voice_channels.iter().any(|ch| ch.channel_id == "voice_general") {
        group.voice_channels.insert(0, VoiceChannel {
            channel_id: "voice_general".to_string(),
            name: "general".to_string(),
            created_at: 0,
            created_by: group.created_by,
            deleted: false,
            deleted_at: None,
        });
    }
}

/// Ensure the group has a "fallback" channel (for orphaned messages from deleted channels).
/// Only creates it if missing — the UI controls visibility based on whether it has messages.
pub fn ensure_fallback_channel(group: &mut Group) {
    if !group.text_channels.iter().any(|ch| ch.channel_id == "fallback") {
        group.text_channels.push(TextChannel {
            channel_id: "fallback".to_string(),
            name: "fallback".to_string(),
            created_at: 0,
            created_by: group.created_by,
            deleted: false,
            deleted_at: None,
        });
    }
}
