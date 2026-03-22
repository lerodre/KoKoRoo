use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::PathBuf;

use crate::crypto;
use crate::theme::Theme;

/// Where KoKoRoo stores its data.
fn data_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join(".kokoroo")
}

/// Our persistent identity: a static X25519 keypair stored on disk.
pub struct Identity {
    pub secret: [u8; 32],
    pub pubkey: [u8; 32],
    pub fingerprint: String,
}

impl Identity {
    /// Load identity from disk, or generate a new one on first launch.
    pub fn load_or_create() -> Self {
        let dir = data_dir();
        let key_path = dir.join("identity.key");

        if key_path.exists() {
            // Load existing
            let data = fs::read(&key_path).expect("Failed to read identity.key");
            if data.len() != 64 {
                panic!("Corrupt identity.key (expected 64 bytes, got {})", data.len());
            }
            let mut secret = [0u8; 32];
            let mut pubkey = [0u8; 32];
            secret.copy_from_slice(&data[..32]);
            pubkey.copy_from_slice(&data[32..64]);
            let fingerprint = crypto::fingerprint(&pubkey);

            Identity { secret, pubkey, fingerprint }
        } else {
            // Generate new identity
            use rand::RngCore;
            let mut secret = [0u8; 32];
            rand::thread_rng().fill_bytes(&mut secret);

            // Derive public key: X25519 clamp + basepoint multiply
            let static_secret = x25519_dalek::StaticSecret::from(secret);
            let public = x25519_dalek::PublicKey::from(&static_secret);
            let pubkey = public.to_bytes();
            let fingerprint = crypto::fingerprint(&pubkey);

            // Save to disk
            fs::create_dir_all(&dir).expect("Failed to create ~/.kokoroo");
            let mut data = Vec::with_capacity(64);
            data.extend_from_slice(&secret);
            data.extend_from_slice(&pubkey);
            fs::write(&key_path, &data).expect("Failed to write identity.key");

            // Restrict permissions (owner-only)
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&key_path, fs::Permissions::from_mode(0o600)).ok();
            }

            println!("Generated new identity: {fingerprint}");
            Identity { secret, pubkey, fingerprint }
        }
    }
}

/// A saved contact (peer we've connected to before).
#[derive(Serialize, Deserialize, Clone)]
pub struct Contact {
    pub fingerprint: String,
    pub pubkey: [u8; 32],
    pub nickname: String,
    pub contact_id: String,  // shared between both peers
    pub first_seen: String,
    pub last_seen: String,
    #[serde(default)] pub last_address: String,
    #[serde(default)] pub last_port: String,
    #[serde(default)] pub call_count: u64,
}

/// Full hex representation of a public key (64 hex chars). Used as storage key.
pub fn pubkey_hex(pubkey: &[u8; 32]) -> String {
    pubkey.iter().map(|b| format!("{:02x}", b)).collect()
}

/// Derive a contact ID from two public keys.
/// The result is the same regardless of which peer computes it.
pub fn derive_contact_id(pubkey_a: &[u8; 32], pubkey_b: &[u8; 32]) -> String {
    // Sort keys so both peers get the same hash
    let (first, second) = if pubkey_a < pubkey_b {
        (pubkey_a, pubkey_b)
    } else {
        (pubkey_b, pubkey_a)
    };

    let mut hasher = Sha256::new();
    hasher.update(first);
    hasher.update(second);
    hasher.update(b"kokoroo-contact-id");
    let hash = hasher.finalize();
    format!("{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        hash[0], hash[1], hash[2], hash[3],
        hash[4], hash[5], hash[6], hash[7])
}

/// Save a contact to disk. Uses full pubkey hex as filename (collision-proof).
pub fn save_contact(contact: &Contact) {
    let dir = data_dir().join("contacts");
    fs::create_dir_all(&dir).expect("Failed to create contacts dir");

    let hex = pubkey_hex(&contact.pubkey);
    let path = dir.join(format!("{hex}.json"));

    // Remove old fingerprint-based file if it exists (migration)
    let old_path = dir.join(format!("{}.json", contact.fingerprint));
    if old_path.exists() && old_path != path {
        fs::remove_file(&old_path).ok();
    }

    let json = serde_json::to_string_pretty(contact).expect("Failed to serialize contact");
    fs::write(path, json).expect("Failed to write contact");
}

/// Delete a contact by their public key. Also removes old fingerprint-based file if present.
pub fn delete_contact(pubkey: &[u8; 32]) {
    let hex = pubkey_hex(pubkey);
    let dir = data_dir().join("contacts");
    let path = dir.join(format!("{hex}.json"));
    if path.exists() {
        std::fs::remove_file(&path).ok();
    }
    // Also remove old fingerprint-based file
    let fp = crate::crypto::fingerprint(pubkey);
    let old_path = dir.join(format!("{fp}.json"));
    if old_path.exists() {
        std::fs::remove_file(&old_path).ok();
    }
}

/// Load a contact by their public key. Returns None if not found.
pub fn load_contact(pubkey: &[u8; 32]) -> Option<Contact> {
    let hex = pubkey_hex(pubkey);
    let path = data_dir().join("contacts").join(format!("{hex}.json"));
    if path.exists() {
        let json = fs::read_to_string(&path).ok()?;
        return serde_json::from_str(&json).ok();
    }
    // Fallback: try old fingerprint-based filename
    let fp = crypto::fingerprint(pubkey);
    let old_path = data_dir().join("contacts").join(format!("{fp}.json"));
    if old_path.exists() {
        let json = fs::read_to_string(&old_path).ok()?;
        return serde_json::from_str(&json).ok();
    }
    None
}

/// Find all known contacts that use a given nickname (for TOFU checks).
pub fn find_contacts_by_nickname(nickname: &str) -> Vec<Contact> {
    if nickname.is_empty() {
        return Vec::new();
    }
    load_all_contacts().into_iter()
        .filter(|c| c.nickname == nickname)
        .collect()
}

/// Get current timestamp as a string.
pub fn now_timestamp() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    format!("{secs}")
}

/// Persisted user settings (mic, speakers, port, nickname).
#[derive(Serialize, Deserialize, Clone, Default)]
pub struct Settings {
    #[serde(default)] pub nickname: String,
    #[serde(default)] pub mic: String,
    #[serde(default)] pub speakers: String,
    #[serde(default)] pub local_port: String,
    #[serde(default)] pub network_adapter: String,
    #[serde(default)] pub blocked: Vec<String>,
    #[serde(default)] pub banned_ips: Vec<String>,
    #[serde(default)] pub theme: Theme,
    #[serde(default)] pub firewall_port: String,
    #[serde(default)] pub muted_groups: Vec<String>,
}

impl Settings {
    /// Load settings from `~/.kokoroo/settings.json`, returning defaults if missing.
    pub fn load() -> Self {
        let path = data_dir().join("settings.json");
        if path.exists() {
            if let Ok(json) = fs::read_to_string(&path) {
                if let Ok(s) = serde_json::from_str(&json) {
                    return s;
                }
            }
        }
        Settings::default()
    }

    /// Save settings to `~/.kokoroo/settings.json`.
    pub fn save(&self) {
        let dir = data_dir();
        fs::create_dir_all(&dir).ok();
        let path = dir.join("settings.json");
        if let Ok(json) = serde_json::to_string_pretty(self) {
            fs::write(path, json).ok();
        }
    }

    /// Check if a public key (hex) is blocked.
    pub fn is_blocked(&self, pubkey_hex: &str) -> bool {
        self.blocked.iter().any(|b| b == pubkey_hex)
    }

    /// Block a contact by hex pubkey and save.
    pub fn block_contact(&mut self, pubkey_hex: &str) {
        let hex = pubkey_hex.to_string();
        if !self.blocked.contains(&hex) {
            self.blocked.push(hex);
            self.save();
        }
    }

    /// Unblock a contact by hex pubkey and save.
    pub fn unblock_contact(&mut self, pubkey_hex: &str) {
        self.blocked.retain(|b| b != pubkey_hex);
        self.save();
    }

    /// Ban an IP address and save.
    pub fn ban_ip(&mut self, ip: &str) {
        let ip = ip.to_string();
        if !ip.is_empty() && !self.banned_ips.contains(&ip) {
            self.banned_ips.push(ip);
            self.save();
        }
    }

    /// Unban an IP address and save.
    pub fn unban_ip(&mut self, ip: &str) {
        self.banned_ips.retain(|b| b != ip);
        self.save();
    }

    /// Check if an IP is banned.
    pub fn is_ip_banned(&self, ip: &str) -> bool {
        self.banned_ips.iter().any(|b| b == ip)
    }
}

/// Load all contacts from `~/.kokoroo/contacts/`, sorted by `last_seen` descending.
pub fn load_all_contacts() -> Vec<Contact> {
    let dir = data_dir().join("contacts");
    if !dir.exists() {
        return Vec::new();
    }
    let mut contacts: Vec<Contact> = Vec::new();
    if let Ok(entries) = fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("json") {
                if let Ok(json) = fs::read_to_string(&path) {
                    if let Ok(c) = serde_json::from_str::<Contact>(&json) {
                        contacts.push(c);
                    }
                }
            }
        }
    }
    // Sort by last_seen descending (most recent first)
    contacts.sort_by(|a, b| b.last_seen.cmp(&a.last_seen));
    contacts
}
