use chacha20poly1305::ChaCha20Poly1305;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

use crate::crypto;

#[derive(Serialize, Deserialize, Clone)]
pub struct PendingInvite {
    pub group_id: String,
    pub group_json: Vec<u8>,
    pub timestamp: u64,
    pub attempts: u32,
}

pub struct PendingInviteStore {
    contact_id: String,
    pub invites: Vec<PendingInvite>,
    storage_cipher: ChaCha20Poly1305,
}

fn pending_invites_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join(".hostelD").join("pending_invites")
}

impl PendingInviteStore {
    /// Load or create a pending invite store for a contact.
    pub fn load(contact_id: &str, identity_secret: &[u8; 32]) -> Self {
        let storage_cipher = crypto::derive_storage_key(identity_secret);
        let dir = pending_invites_dir();
        fs::create_dir_all(&dir).ok();

        let path = dir.join(format!("{contact_id}.enc"));
        let invites: Vec<PendingInvite> = if path.exists() {
            match fs::read(&path) {
                Ok(encrypted) => {
                    crypto::decrypt_local(&storage_cipher, &encrypted)
                        .and_then(|plain| serde_json::from_slice(&plain).ok())
                        .unwrap_or_default()
                }
                Err(_) => Vec::new(),
            }
        } else {
            Vec::new()
        };

        PendingInviteStore {
            contact_id: contact_id.to_string(),
            invites,
            storage_cipher,
        }
    }

    /// Enqueue a new group invite. Deduplicates by group_id, auto-saves.
    pub fn enqueue(&mut self, group_id: String, group_json: Vec<u8>) {
        // Dedup: replace existing invite for same group
        self.invites.retain(|i| i.group_id != group_id);

        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();

        self.invites.push(PendingInvite {
            group_id,
            group_json,
            timestamp,
            attempts: 0,
        });
        self.save();
    }

    /// Remove an invite by group_id (e.g. after ACK/NACK). Auto-saves.
    pub fn remove(&mut self, group_id: &str) {
        self.invites.retain(|i| i.group_id != group_id);
        self.save();
    }

    #[allow(dead_code)]
    pub fn has_pending(&self) -> bool {
        !self.invites.is_empty()
    }

    /// Persist to encrypted file.
    pub fn save(&self) {
        let dir = pending_invites_dir();
        fs::create_dir_all(&dir).ok();
        let json = serde_json::to_vec(&self.invites).unwrap_or_default();
        let encrypted = crypto::encrypt_local(&self.storage_cipher, &json);
        let path = dir.join(format!("{}.enc", self.contact_id));
        fs::write(path, encrypted).ok();
    }
}
