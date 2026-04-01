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
    PathBuf::from(home).join(".kokoroo").join("chats")
}

impl ChatHistory {
    /// Create or load chat history for a contact.
    /// The storage_cipher is derived from our identity secret key.
    pub fn load(contact_id: &str, identity_secret: &[u8; 32]) -> Self {
        let storage_cipher = crypto::derive_storage_key(identity_secret);
        let dir = chats_dir();
        fs::create_dir_all(&dir).ok();

        let path = dir.join(format!("{contact_id}.enc"));
        let mut messages: Vec<ChatMessage> = if path.exists() {
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

        // Clean up stale file transfers (Offered/Accepted that never completed)
        let mut cleaned = false;
        for msg in &mut messages {
            if let Some(ref mut ft) = msg.file_transfer {
                if matches!(ft.status, FileTransferStatus::Offered | FileTransferStatus::Accepted) {
                    ft.status = FileTransferStatus::Failed("Interrupted".to_string());
                    cleaned = true;
                }
            }
        }

        let history = ChatHistory {
            contact_id: contact_id.to_string(),
            messages,
            storage_cipher,
        };
        if cleaned {
            history.save();
        }
        history
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
        let status_name = match &status {
            FileTransferStatus::Offered => "Offered",
            FileTransferStatus::Accepted => "Accepted",
            FileTransferStatus::Completed => "Completed",
            FileTransferStatus::Rejected => "Rejected",
            FileTransferStatus::Cancelled => "Cancelled",
            FileTransferStatus::Failed(_) => "Failed",
        };
        let mut idx = None;
        for i in (0..self.messages.len()).rev() {
            if let Some(ref ft) = self.messages[i].file_transfer {
                // Only match active transfers (not terminal states)
                if ft.transfer_id == transfer_id
                    && !matches!(ft.status, FileTransferStatus::Completed
                        | FileTransferStatus::Failed(_)
                        | FileTransferStatus::Cancelled
                        | FileTransferStatus::Rejected)
                {
                    idx = Some(i);
                    break;
                }
            }
        }
        if idx.is_none() {
            // Iterate forward so the oldest placeholder (first file sent) gets
            // assigned the first real transfer_id from the daemon.
            // Match Offered OR Accepted (progress event may have arrived before ID assignment).
            for i in 0..self.messages.len() {
                let msg = &self.messages[i];
                if let Some(ref ft) = msg.file_transfer {
                    if ft.transfer_id == 0 && msg.from_me
                        && matches!(ft.status, FileTransferStatus::Offered | FileTransferStatus::Accepted)
                    {
                        idx = Some(i);
                        break;
                    }
                }
            }
        }
        if let Some(i) = idx {
            if let Some(ref mut ft) = self.messages[i].file_transfer {
                log_fmt!("[chat] update_file_status: tid={} old_status={:?} -> {} (msg idx={})",
                    transfer_id, ft.transfer_id, status_name, i);
                ft.transfer_id = transfer_id;
                ft.status = status;
                if saved_path.is_some() {
                    ft.saved_path = saved_path;
                }
            }
            self.save();
        } else {
            // Debug: dump all file messages to find why we couldn't match
            let file_msgs: Vec<String> = self.messages.iter().enumerate()
                .filter_map(|(i, m)| m.file_transfer.as_ref().map(|ft| {
                    format!("  [{}] tid={} from_me={} status={:?}", i, ft.transfer_id, m.from_me,
                        match &ft.status {
                            FileTransferStatus::Offered => "Offered",
                            FileTransferStatus::Accepted => "Accepted",
                            FileTransferStatus::Completed => "Completed",
                            _ => "Other",
                        })
                }))
                .collect();
            log_fmt!("[chat] update_file_status FAILED: tid={} new_status={} — {} file msgs in history:\n{}",
                transfer_id, status_name, file_msgs.len(), file_msgs.join("\n"));
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
    PathBuf::from(home).join(".kokoroo").join("groups").join("chats")
}

/// A single group chat message for persistent storage.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct GroupChatMessage {
    pub sender_fingerprint: String,
    pub sender_nickname: String,
    pub text: String,
    pub timestamp: u64,
    /// Deterministic content hash for deduplication during sync.
    /// hex(sha256(sender_fingerprint || timestamp_le || text))[..32]
    #[serde(default)]
    pub msg_id: String,
}

/// Compute a deterministic message ID from content.
pub fn compute_msg_id(sender_fingerprint: &str, timestamp: u64, text: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(sender_fingerprint.as_bytes());
    hasher.update(timestamp.to_le_bytes());
    hasher.update(text.as_bytes());
    let hash = hasher.finalize();
    hash.iter().take(16).map(|b| format!("{:02x}", b)).collect()
}

/// Persistent group chat history, encrypted at rest with local identity key.
pub struct GroupChatHistory {
    pub group_id: String,
    pub channel_id: String,
    pub messages: Vec<GroupChatMessage>,
    storage_cipher: ChaCha20Poly1305,
}

impl GroupChatHistory {
    /// Load group chat history from disk (or create empty).
    /// Uses per-channel files: `{group_id}_{channel_id}.enc`.
    /// If `channel_id == "general"` and only the old `{group_id}.enc` exists, migrates it.
    pub fn load(group_id: &str, channel_id: &str, identity_secret: &[u8; 32]) -> Self {
        let storage_cipher = crypto::derive_storage_key(identity_secret);
        let dir = group_chats_dir();
        fs::create_dir_all(&dir).ok();

        let new_path = dir.join(format!("{}_{}.enc", group_id, channel_id));
        let old_path = dir.join(format!("{}.enc", group_id));

        // Migration: if loading "general" and old file exists but new doesn't, rename.
        if channel_id == "general" && !new_path.exists() && old_path.exists() {
            fs::rename(&old_path, &new_path).ok();
        }

        let mut messages: Vec<GroupChatMessage> = if new_path.exists() {
            match fs::read(&new_path) {
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

        // Backfill msg_id for legacy messages that don't have one
        let mut needs_save = false;
        for msg in &mut messages {
            if msg.msg_id.is_empty() {
                msg.msg_id = compute_msg_id(&msg.sender_fingerprint, msg.timestamp, &msg.text);
                needs_save = true;
            }
        }

        let mut hist = GroupChatHistory {
            group_id: group_id.to_string(),
            channel_id: channel_id.to_string(),
            messages,
            storage_cipher,
        };

        if needs_save {
            hist.save();
        }

        hist
    }

    /// Add a message and save to disk.
    pub fn add_message(&mut self, sender_fingerprint: String, sender_nickname: String, text: String) {
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();

        let msg_id = compute_msg_id(&sender_fingerprint, timestamp, &text);
        self.messages.push(GroupChatMessage {
            sender_fingerprint,
            sender_nickname,
            text,
            timestamp,
            msg_id,
        });

        self.save();
    }

    /// Get the latest message timestamp, or 0 if empty.
    pub fn latest_timestamp(&self) -> u64 {
        self.messages.iter().map(|m| m.timestamp).max().unwrap_or(0)
    }

    /// Return messages after the given timestamp.
    pub fn messages_after(&self, timestamp: u64) -> Vec<&GroupChatMessage> {
        self.messages.iter().filter(|m| m.timestamp > timestamp).collect()
    }

    /// Merge incoming messages, deduplicating by msg_id. Returns count of new messages added.
    pub fn merge_messages(&mut self, incoming: Vec<GroupChatMessage>) -> usize {
        let existing_ids: std::collections::HashSet<String> = self.messages.iter()
            .filter(|m| !m.msg_id.is_empty())
            .map(|m| m.msg_id.clone())
            .collect();

        let mut added = 0;
        for msg in incoming {
            let id = if msg.msg_id.is_empty() {
                compute_msg_id(&msg.sender_fingerprint, msg.timestamp, &msg.text)
            } else {
                msg.msg_id.clone()
            };
            if !existing_ids.contains(&id) {
                self.messages.push(GroupChatMessage {
                    msg_id: id,
                    ..msg
                });
                added += 1;
            }
        }

        if added > 0 {
            self.messages.sort_by_key(|m| m.timestamp);
            self.save();
        }
        added
    }

    /// Save all messages to encrypted file.
    pub fn save(&self) {
        let dir = group_chats_dir();
        fs::create_dir_all(&dir).ok();

        let json = serde_json::to_vec(&self.messages).expect("Failed to serialize group chat");
        let encrypted = crypto::encrypt_local(&self.storage_cipher, &json);

        let path = dir.join(format!("{}_{}.enc", self.group_id, self.channel_id));
        fs::write(path, encrypted).expect("Failed to write group chat history");
    }
}

/// Migrate messages from a deleted channel into the fallback channel.
/// Each message is prefixed with `[from #{channel_name}]`.
pub fn migrate_messages_to_fallback(
    group_id: &str,
    source_channel_id: &str,
    source_channel_name: &str,
    identity_secret: &[u8; 32],
) {
    let source = GroupChatHistory::load(group_id, source_channel_id, identity_secret);
    if source.messages.is_empty() {
        return;
    }
    let mut fallback = GroupChatHistory::load(group_id, "fallback", identity_secret);
    for msg in &source.messages {
        let text = format!("[from #{}] {}", source_channel_name, msg.text);
        let msg_id = compute_msg_id(&msg.sender_fingerprint, msg.timestamp, &text);
        fallback.messages.push(GroupChatMessage {
            sender_fingerprint: msg.sender_fingerprint.clone(),
            sender_nickname: msg.sender_nickname.clone(),
            text,
            timestamp: msg.timestamp,
            msg_id,
        });
    }
    fallback.save();
    // Delete source file
    let dir = group_chats_dir();
    let path = dir.join(format!("{}_{}.enc", group_id, source_channel_id));
    fs::remove_file(path).ok();
}
