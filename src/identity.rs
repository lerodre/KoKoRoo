use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::PathBuf;

use crate::crypto;

/// Where hostelD stores its data.
fn data_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join(".hostelD")
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
            fs::create_dir_all(&dir).expect("Failed to create ~/.hostelD");
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
    hasher.update(b"hostelD-contact-id");
    let hash = hasher.finalize();
    format!("{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        hash[0], hash[1], hash[2], hash[3],
        hash[4], hash[5], hash[6], hash[7])
}

/// Save a contact to disk.
pub fn save_contact(contact: &Contact) {
    let dir = data_dir().join("contacts");
    fs::create_dir_all(&dir).expect("Failed to create contacts dir");

    let path = dir.join(format!("{}.json", contact.fingerprint));
    let json = serde_json::to_string_pretty(contact).expect("Failed to serialize contact");
    fs::write(path, json).expect("Failed to write contact");
}

/// Load a contact by fingerprint. Returns None if not found.
pub fn load_contact(fingerprint: &str) -> Option<Contact> {
    let path = data_dir().join("contacts").join(format!("{fingerprint}.json"));
    if !path.exists() {
        return None;
    }
    let json = fs::read_to_string(path).ok()?;
    serde_json::from_str(&json).ok()
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
