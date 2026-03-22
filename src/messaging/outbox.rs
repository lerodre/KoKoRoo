use chacha20poly1305::ChaCha20Poly1305;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

use crate::crypto;

#[derive(Serialize, Deserialize, Clone)]
pub struct OutboxMessage {
    pub seq: u32,
    pub text: String,
    pub timestamp: u64,
    pub attempts: u32,
}

pub struct Outbox {
    contact_id: String,
    pub messages: Vec<OutboxMessage>,
    storage_cipher: ChaCha20Poly1305,
    next_seq: u32,
}

fn outbox_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join(".kokoroo").join("outbox")
}

impl Outbox {
    /// Load or create an outbox for a contact.
    pub fn load(contact_id: &str, identity_secret: &[u8; 32]) -> Self {
        let storage_cipher = crypto::derive_storage_key(identity_secret);
        let dir = outbox_dir();
        fs::create_dir_all(&dir).ok();

        let path = dir.join(format!("{contact_id}.enc"));
        let messages: Vec<OutboxMessage> = if path.exists() {
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

        let next_seq = messages.iter().map(|m| m.seq).max().unwrap_or(0) + 1;

        Outbox {
            contact_id: contact_id.to_string(),
            messages,
            storage_cipher,
            next_seq,
        }
    }

    /// Enqueue a new message. Returns the assigned sequence number.
    pub fn enqueue(&mut self, text: String) -> u32 {
        let seq = self.next_seq;
        self.next_seq += 1;

        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();

        self.messages.push(OutboxMessage {
            seq,
            text,
            timestamp,
            attempts: 0,
        });
        self.save();
        seq
    }

    /// Remove a message that was acknowledged.
    pub fn remove_acked(&mut self, seq: u32) {
        self.messages.retain(|m| m.seq != seq);
        self.save();
    }

    /// Persist outbox to encrypted file.
    pub fn save(&self) {
        let dir = outbox_dir();
        fs::create_dir_all(&dir).ok();
        let json = serde_json::to_vec(&self.messages).unwrap_or_default();
        let encrypted = crypto::encrypt_local(&self.storage_cipher, &json);
        let path = dir.join(format!("{}.enc", self.contact_id));
        fs::write(path, encrypted).ok();
    }

    pub fn has_pending(&self) -> bool {
        !self.messages.is_empty()
    }
}
