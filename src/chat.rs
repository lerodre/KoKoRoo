use chacha20poly1305::ChaCha20Poly1305;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

use crate::crypto;

/// Status of a file transfer attached to a chat message.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub enum FileTransferStatus {
    Offered,
    Accepted,
    Rejected,
    Completed,
    Cancelled,
    Failed(String),
}

/// Metadata for a file transfer attached to a chat message.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct FileTransferInfo {
    pub filename: String,
    pub file_size: u64,
    pub transfer_id: u32,
    pub status: FileTransferStatus,
    /// Final saved path (set on completion).
    #[serde(default)]
    pub saved_path: Option<String>,
}

/// A single chat message.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ChatMessage {
    pub from_me: bool,
    pub text: String,
    pub timestamp: u64,
    #[serde(default)]
    pub file_transfer: Option<FileTransferInfo>,
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
            file_transfer: None,
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

    /// Add a file transfer message and save to disk.
    pub fn add_file_message(&mut self, from_me: bool, info: FileTransferInfo) {
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();

        let text = if from_me {
            format!("Sent file: {} ({})", info.filename, crate::filetransfer::format_size(info.file_size))
        } else {
            format!("File: {} ({})", info.filename, crate::filetransfer::format_size(info.file_size))
        };

        self.messages.push(ChatMessage {
            from_me,
            text,
            timestamp,
            file_transfer: Some(info),
        });

        self.save();
    }

    /// Update the status of a file transfer message by transfer_id.
    /// If no exact match is found, falls back to matching the most recent
    /// outgoing Offered message with transfer_id=0 (placeholder assigned by GUI
    /// before daemon generates the real ID).
    pub fn update_file_status(&mut self, transfer_id: u32, status: FileTransferStatus, saved_path: Option<String>) {
        // Find index: exact transfer_id match first, then fallback to
        // outgoing Offered with transfer_id=0 (GUI placeholder before daemon assigns real ID).
        let mut idx = None;
        for i in (0..self.messages.len()).rev() {
            if let Some(ref ft) = self.messages[i].file_transfer {
                if ft.transfer_id == transfer_id {
                    idx = Some(i);
                    break;
                }
            }
        }
        if idx.is_none() {
            // Iterate forward so the oldest placeholder (first file sent) gets
            // assigned the first real transfer_id from the daemon.
            for i in 0..self.messages.len() {
                let msg = &self.messages[i];
                if let Some(ref ft) = msg.file_transfer {
                    if ft.transfer_id == 0 && msg.from_me && matches!(ft.status, FileTransferStatus::Offered) {
                        idx = Some(i);
                        break;
                    }
                }
            }
        }
        if let Some(i) = idx {
            if let Some(ref mut ft) = self.messages[i].file_transfer {
                ft.transfer_id = transfer_id;
                ft.status = status;
                if saved_path.is_some() {
                    ft.saved_path = saved_path;
                }
            }
            self.save();
        }
    }

    /// Format a timestamp as HH:MM.
    pub fn format_time(timestamp: u64) -> String {
        // Simple: seconds since epoch → hours:minutes of day (UTC)
        let secs_in_day = timestamp % 86400;
        let hours = secs_in_day / 3600;
        let minutes = (secs_in_day % 3600) / 60;
        format!("{hours:02}:{minutes:02} UTC")
    }
}

/// Delete chat history for a contact.
pub fn delete_chat_history(contact_id: &str) {
    let path = chats_dir().join(format!("{contact_id}.enc"));
    if path.exists() {
        std::fs::remove_file(&path).ok();
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

// ── Group chat persistence ──

fn group_chats_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join(".hostelD").join("groups").join("chats")
}

/// A single group chat message for persistent storage.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct GroupChatMessage {
    pub sender_fingerprint: String,
    pub sender_nickname: String,
    pub text: String,
    pub timestamp: u64,
}

/// Persistent group chat history, encrypted at rest with local identity key.
pub struct GroupChatHistory {
    pub group_id: String,
    pub messages: Vec<GroupChatMessage>,
    storage_cipher: ChaCha20Poly1305,
}

impl GroupChatHistory {
    /// Load group chat history from disk (or create empty).
    pub fn load(group_id: &str, identity_secret: &[u8; 32]) -> Self {
        let storage_cipher = crypto::derive_storage_key(identity_secret);
        let dir = group_chats_dir();
        fs::create_dir_all(&dir).ok();

        let path = dir.join(format!("{group_id}.enc"));
        let messages = if path.exists() {
            match fs::read(&path) {
                Ok(encrypted) => {
                    match crypto::decrypt_local(&storage_cipher, &encrypted) {
                        Some(plaintext) => {
                            serde_json::from_slice(&plaintext).unwrap_or_default()
                        }
                        None => Vec::new(),
                    }
                }
                Err(_) => Vec::new(),
            }
        } else {
            Vec::new()
        };

        GroupChatHistory { group_id: group_id.to_string(), messages, storage_cipher }
    }

    /// Add a message and save to disk.
    pub fn add_message(&mut self, sender_fingerprint: String, sender_nickname: String, text: String) {
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();

        self.messages.push(GroupChatMessage {
            sender_fingerprint,
            sender_nickname,
            text,
            timestamp,
        });

        self.save();
    }

    /// Save all messages to encrypted file.
    pub fn save(&self) {
        let dir = group_chats_dir();
        fs::create_dir_all(&dir).ok();

        let json = serde_json::to_vec(&self.messages).expect("Failed to serialize group chat");
        let encrypted = crypto::encrypt_local(&self.storage_cipher, &json);

        let path = dir.join(format!("{}.enc", self.group_id));
        fs::write(path, encrypted).expect("Failed to write group chat history");
    }
}
