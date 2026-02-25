use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

/// Where hostelD stores its data.
fn data_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join(".hostelD")
}

/// Groups directory: ~/.hostelD/groups/
fn groups_dir() -> PathBuf {
    data_dir().join("groups")
}

/// Group chat history directory: ~/.hostelD/groups/chats/
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

/// Save a group to disk at ~/.hostelD/groups/{group_id}.json
pub fn save_group(group: &Group) {
    let dir = groups_dir();
    fs::create_dir_all(&dir).ok();
    let path = dir.join(format!("{}.json", group.group_id));
    if let Ok(json) = serde_json::to_string_pretty(group) {
        fs::write(path, json).ok();
    }
}

/// Load a group from disk by its ID.
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
    // Also remove chat history
    let chat_path = group_chats_dir().join(format!("{}.enc", group_id));
    fs::remove_file(chat_path).ok();
}

/// Find a member in the group by pubkey.
pub fn find_member_by_pubkey<'a>(group: &'a Group, pubkey: &[u8; 32]) -> Option<&'a GroupMember> {
    group.members.iter().find(|m| &m.pubkey == pubkey)
}

/// Find a member in the group by sender_index.
pub fn find_member_by_index(group: &Group, sender_index: u16) -> Option<&GroupMember> {
    group.members.iter().find(|m| m.sender_index == sender_index)
}
