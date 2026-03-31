use chacha20poly1305::ChaCha20Poly1305;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

use crate::crypto;

#[derive(Serialize, Deserialize, Clone)]
pub struct PendingDelete {
    pub contact_id: String,
    pub peer_pubkey_hex: String,
    pub peer_addr: String,
    pub timestamp: u64,
    pub attempts: u32,
}

pub struct PendingDeleteStore {
    pub deletes: Vec<PendingDelete>,
    storage_cipher: ChaCha20Poly1305,
}

fn pending_deletes_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join(".kokoroo").join("pending_deletes.enc")
}

impl PendingDeleteStore {
    /// Load or create the pending delete store.
    pub fn load(identity_secret: &[u8; 32]) -> Self {
        let storage_cipher = crypto::derive_storage_key(identity_secret);
        let path = pending_deletes_path();

        let deletes: Vec<PendingDelete> = if path.exists() {
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

        PendingDeleteStore {
            deletes,
            storage_cipher,
        }
    }

    /// Enqueue a pending delete. Deduplicates by contact_id, auto-saves.
    pub fn enqueue(&mut self, contact_id: String, peer_pubkey_hex: String, peer_addr: String) {
        self.deletes.retain(|d| d.contact_id != contact_id);

        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();

        self.deletes.push(PendingDelete {
            contact_id,
            peer_pubkey_hex,
            peer_addr,
            timestamp,
            attempts: 0,
        });
        self.save();
    }

    /// Remove a pending delete by contact_id (e.g. after ACK). Auto-saves.
    pub fn remove(&mut self, contact_id: &str) {
        self.deletes.retain(|d| d.contact_id != contact_id);
        self.save();
    }

    /// Check if a contact_id has a pending delete.
    pub fn has_pending(&self, contact_id: &str) -> bool {
        self.deletes.iter().any(|d| d.contact_id == contact_id)
    }

    /// Persist to encrypted file.
    pub fn save(&self) {
        let path = pending_deletes_path();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).ok();
        }
        let json = serde_json::to_vec(&self.deletes).unwrap_or_default();
        let encrypted = crypto::encrypt_local(&self.storage_cipher, &json);
        fs::write(path, encrypted).ok();
    }
}
