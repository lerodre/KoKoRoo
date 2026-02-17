use chacha20poly1305::ChaCha20Poly1305;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

use crate::crypto;

/// A single chat message.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ChatMessage {
    pub from_me: bool,
    pub text: String,
    pub timestamp: u64,
}

/// Chat history for one contact.
pub struct ChatHistory {
    pub contact_id: String,
    pub messages: Vec<ChatMessage>,
    storage_cipher: ChaCha20Poly1305,
}

fn chats_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join(".hostelD").join("chats")
}

impl ChatHistory {
    /// Create or load chat history for a contact.
    /// The storage_cipher is derived from our identity secret key.
    pub fn load(contact_id: &str, identity_secret: &[u8; 32]) -> Self {
        let storage_cipher = crypto::derive_storage_key(identity_secret);
        let dir = chats_dir();
        fs::create_dir_all(&dir).ok();

        let path = dir.join(format!("{contact_id}.enc"));
        let messages = if path.exists() {
            match fs::read(&path) {
                Ok(encrypted) => {
                    match crypto::decrypt_local(&storage_cipher, &encrypted) {
                        Some(plaintext) => {
                            serde_json::from_slice(&plaintext).unwrap_or_default()
                        }
                        None => {
                            eprintln!("Warning: could not decrypt chat history (key changed?)");
                            Vec::new()
                        }
                    }
                }
                Err(_) => Vec::new(),
            }
        } else {
            Vec::new()
        };

        ChatHistory {
            contact_id: contact_id.to_string(),
            messages,
            storage_cipher,
        }
    }

    /// Add a message and save to disk.
    pub fn add_message(&mut self, from_me: bool, text: String) {
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();

        self.messages.push(ChatMessage {
            from_me,
            text,
            timestamp,
        });

        self.save();
    }

    /// Save all messages to encrypted file.
    fn save(&self) {
        let dir = chats_dir();
        fs::create_dir_all(&dir).ok();

        let json = serde_json::to_vec(&self.messages).expect("Failed to serialize chat");
        let encrypted = crypto::encrypt_local(&self.storage_cipher, &json);

        let path = dir.join(format!("{}.enc", self.contact_id));
        fs::write(path, encrypted).expect("Failed to write chat history");
    }

    /// Format a timestamp as HH:MM.
    pub fn format_time(timestamp: u64) -> String {
        // Simple: seconds since epoch → hours:minutes of day (UTC)
        let secs_in_day = timestamp % 86400;
        let hours = secs_in_day / 3600;
        let minutes = (secs_in_day % 3600) / 60;
        format!("{hours:02}:{minutes:02}")
    }
}

/// Encode a chat message for sending over UDP.
/// The text is serialized as UTF-8 bytes, then encrypted by the Session.
pub fn encode_chat_text(text: &str) -> Vec<u8> {
    text.as_bytes().to_vec()
}

/// Decode received chat bytes back to text.
pub fn decode_chat_text(data: &[u8]) -> Option<String> {
    String::from_utf8(data.to_vec()).ok()
}
