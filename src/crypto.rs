use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{ChaCha20Poly1305, Nonce};
use sha2::{Digest, Sha256};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use x25519_dalek::{EphemeralSecret, PublicKey, SharedSecret};

/// Packet type markers (first byte of every UDP packet).
pub const PKT_HELLO: u8 = 0x01;    // key exchange: [0x01][32-byte ephemeral pubkey]
pub const PKT_VOICE: u8 = 0x02;    // encrypted voice: [0x02][4-byte counter][ciphertext+tag]
pub const PKT_IDENTITY: u8 = 0x03; // identity exchange: [0x03][4-byte counter][encrypted identity pubkey]
pub const PKT_CHAT: u8 = 0x04;     // chat message: [0x04][4-byte counter][encrypted text+timestamp]
pub const PKT_HANGUP: u8 = 0x05;   // hangup signal: [0x05][4-byte counter][encrypted empty]
pub const PKT_SCREEN: u8 = 0x06;       // screen share: [0x06][4-byte counter][encrypted VP8 chunk]
pub const PKT_SCREEN_STOP: u8 = 0x07;  // screen share ended: [0x07][4-byte counter][encrypted empty]

// Messaging daemon packet types (independent of voice calls)
pub const PKT_MSG_HELLO: u8    = 0x10; // msg handshake: [0x10][32-byte ephemeral pubkey]
pub const PKT_MSG_IDENTITY: u8 = 0x11; // msg identity exchange (encrypted)
pub const PKT_MSG_CHAT: u8     = 0x12; // message: [4-byte seq][text]
pub const PKT_MSG_ACK: u8      = 0x13; // delivery confirmation: [4-byte seq]
pub const PKT_MSG_BYE: u8      = 0x14; // disconnect signal
pub const PKT_MSG_REQUEST: u8  = 0x15; // contact request: encrypted [32-byte identity pubkey][nickname]
pub const PKT_MSG_REQUEST_ACK: u8 = 0x16; // contact request accepted: encrypted [32-byte identity pubkey][nickname]
pub const PKT_MSG_IP_ANNOUNCE: u8  = 0x17; // IP relay: encrypted [ip_str][0x00][port_str][0x00][8-byte timestamp LE]
pub const PKT_MSG_PEER_QUERY: u8   = 0x18; // peer lookup: encrypted [32-byte target pubkey]
pub const PKT_MSG_PEER_RESPONSE: u8 = 0x19; // peer lookup reply: encrypted [32-byte pubkey][ip_str][0x00][port_str][0x00][8-byte timestamp LE]
pub const PKT_MSG_PRESENCE: u8 = 0x1A;      // presence status: encrypted [1 byte: 0x01=Online, 0x02=Away]

// File transfer packet types
pub const PKT_MSG_FILE_OFFER: u8    = 0x1B; // file offer: encrypted [4B transfer_id][8B file_size LE][32B sha256][filename UTF-8]
pub const PKT_MSG_FILE_ACCEPT: u8   = 0x1C; // file accept: encrypted [4B transfer_id]
pub const PKT_MSG_FILE_REJECT: u8   = 0x1D; // file reject: encrypted [4B transfer_id]
pub const PKT_MSG_FILE_CHUNK: u8    = 0x1E; // file chunk: encrypted [4B transfer_id][4B chunk_index LE][data: up to 1200 bytes]
pub const PKT_MSG_FILE_ACK: u8      = 0x1F; // file ack: encrypted [4B transfer_id][4B ack_through LE]
pub const PKT_MSG_FILE_COMPLETE: u8 = 0x20; // file complete: encrypted [4B transfer_id][32B sha256]
pub const PKT_MSG_FILE_CANCEL: u8   = 0x21; // file cancel: encrypted [4B transfer_id][1B reason]

/// Size of an X25519 public key.
pub const PUBKEY_SIZE: usize = 32;

/// HELLO packet size: 1 type byte + 32 pubkey bytes.
pub const HELLO_SIZE: usize = 1 + PUBKEY_SIZE;

/// Nonce size for ChaCha20-Poly1305 (12 bytes).
const NONCE_SIZE: usize = 12;

/// Poly1305 authentication tag size (16 bytes).
const TAG_SIZE: usize = 16;

/// Holds the cryptographic state for one session.
pub struct Session {
    cipher: ChaCha20Poly1305,
    send_counter: Arc<AtomicU32>,
    pub verification_code: String,
}

/// Generate an ephemeral X25519 keypair.
/// Returns (secret, public_key_bytes).
pub fn generate_keypair() -> (EphemeralSecret, [u8; 32]) {
    let secret = EphemeralSecret::random_from_rng(rand::thread_rng());
    let public = PublicKey::from(&secret);
    (secret, public.to_bytes())
}

/// Build a HELLO packet containing our public key.
pub fn build_hello(pubkey: &[u8; 32]) -> [u8; HELLO_SIZE] {
    let mut pkt = [0u8; HELLO_SIZE];
    pkt[0] = PKT_HELLO;
    pkt[1..HELLO_SIZE].copy_from_slice(pubkey);
    pkt
}

/// Parse a received HELLO packet. Returns the peer's public key bytes.
pub fn parse_hello(data: &[u8]) -> Option<[u8; 32]> {
    if data.len() < HELLO_SIZE || data[0] != PKT_HELLO {
        return None;
    }
    let mut pubkey = [0u8; 32];
    pubkey.copy_from_slice(&data[1..HELLO_SIZE]);
    Some(pubkey)
}

/// Build a MSG_HELLO packet containing our public key.
pub fn build_msg_hello(pubkey: &[u8; 32]) -> [u8; HELLO_SIZE] {
    let mut pkt = [0u8; HELLO_SIZE];
    pkt[0] = PKT_MSG_HELLO;
    pkt[1..HELLO_SIZE].copy_from_slice(pubkey);
    pkt
}

/// Parse a received MSG_HELLO packet. Returns the peer's public key bytes.
pub fn parse_msg_hello(data: &[u8]) -> Option<[u8; 32]> {
    if data.len() < HELLO_SIZE || data[0] != PKT_MSG_HELLO {
        return None;
    }
    let mut pubkey = [0u8; 32];
    pubkey.copy_from_slice(&data[1..HELLO_SIZE]);
    Some(pubkey)
}

/// Complete the key exchange: our secret + peer's public key → Session.
///
/// Derives a 256-bit symmetric key from the X25519 shared secret using SHA-256.
/// Also generates a human-readable verification code for anti-MITM.
pub fn complete_handshake(our_secret: EphemeralSecret, peer_pubkey: &[u8; 32]) -> Session {
    let peer_public = PublicKey::from(*peer_pubkey);
    let shared: SharedSecret = our_secret.diffie_hellman(&peer_public);

    // Derive encryption key: SHA-256(shared_secret || "hostelD-voice-key")
    let mut hasher = Sha256::new();
    hasher.update(shared.as_bytes());
    hasher.update(b"hostelD-voice-key");
    let key_bytes = hasher.finalize();

    let cipher = ChaCha20Poly1305::new_from_slice(&key_bytes)
        .expect("key size mismatch");

    // Verification code: SHA-256(shared_secret || "hostelD-verify")
    // Display as XXXX-XXXX so users can compare verbally.
    let mut verify_hasher = Sha256::new();
    verify_hasher.update(shared.as_bytes());
    verify_hasher.update(b"hostelD-verify");
    let verify_hash = verify_hasher.finalize();
    let code = format!(
        "{:02X}{:02X}-{:02X}{:02X}",
        verify_hash[0], verify_hash[1], verify_hash[2], verify_hash[3]
    );

    Session {
        cipher,
        send_counter: Arc::new(AtomicU32::new(0)),
        verification_code: code,
    }
}

impl Session {
    /// Encrypt any payload with a given packet type marker.
    /// Returns: [type][4-byte counter][ciphertext + 16-byte auth tag]
    pub fn encrypt_packet(&self, pkt_type: u8, plaintext: &[u8]) -> Vec<u8> {
        let counter = self.send_counter.fetch_add(1, Ordering::Relaxed);

        let mut nonce_bytes = [0u8; NONCE_SIZE];
        nonce_bytes[..4].copy_from_slice(&counter.to_le_bytes());
        let nonce = Nonce::from(nonce_bytes);

        let ciphertext = self.cipher.encrypt(&nonce, plaintext)
            .expect("encryption failed");

        let mut packet = Vec::with_capacity(1 + 4 + ciphertext.len());
        packet.push(pkt_type);
        packet.extend_from_slice(&counter.to_le_bytes());
        packet.extend_from_slice(&ciphertext);
        packet
    }

    /// Decrypt any packet. Returns (packet_type, plaintext).
    /// Returns None if decryption fails.
    /// Minimum size: 1 (type) + 4 (counter) + 16 (auth tag) = 21 bytes (empty payload).
    pub fn decrypt_packet(&self, packet: &[u8]) -> Option<(u8, Vec<u8>)> {
        if packet.len() < 1 + 4 + TAG_SIZE {
            return None;
        }
        let pkt_type = packet[0];

        let mut counter_bytes = [0u8; 4];
        counter_bytes.copy_from_slice(&packet[1..5]);
        let mut nonce_bytes = [0u8; NONCE_SIZE];
        nonce_bytes[..4].copy_from_slice(&counter_bytes);
        let nonce = Nonce::from(nonce_bytes);

        let ciphertext = &packet[5..];
        self.cipher.decrypt(&nonce, ciphertext)
            .ok()
            .map(|plain| (pkt_type, plain))
    }

    /// Create a clone that shares the same cipher key and atomic send counter.
    /// Used so the screen capture thread can encrypt packets concurrently
    /// with unique nonces (no mutex needed for sending).
    pub fn clone_for_sending(&self) -> Session {
        Session {
            cipher: self.cipher.clone(),
            send_counter: self.send_counter.clone(),
            verification_code: self.verification_code.clone(),
        }
    }
}

/// Derive a storage encryption key from an identity secret key.
/// Used to encrypt local chat history files.
pub fn derive_storage_key(identity_secret: &[u8; 32]) -> ChaCha20Poly1305 {
    let mut hasher = Sha256::new();
    hasher.update(identity_secret);
    hasher.update(b"hostelD-local-storage");
    let key = hasher.finalize();
    ChaCha20Poly1305::new_from_slice(&key).expect("key size mismatch")
}

/// Encrypt data for local storage. Prepends a random 12-byte nonce.
pub fn encrypt_local(cipher: &ChaCha20Poly1305, plaintext: &[u8]) -> Vec<u8> {
    use rand::RngCore;
    let mut nonce_bytes = [0u8; NONCE_SIZE];
    rand::thread_rng().fill_bytes(&mut nonce_bytes);
    let nonce = Nonce::from(nonce_bytes);

    let ciphertext = cipher.encrypt(&nonce, plaintext).expect("encryption failed");

    let mut out = Vec::with_capacity(NONCE_SIZE + ciphertext.len());
    out.extend_from_slice(&nonce_bytes);
    out.extend_from_slice(&ciphertext);
    out
}

/// Decrypt locally stored data. Expects nonce prepended.
pub fn decrypt_local(cipher: &ChaCha20Poly1305, data: &[u8]) -> Option<Vec<u8>> {
    if data.len() < NONCE_SIZE + TAG_SIZE + 1 {
        return None;
    }
    let nonce = Nonce::from_slice(&data[..NONCE_SIZE]);
    let ciphertext = &data[NONCE_SIZE..];
    cipher.decrypt(nonce, ciphertext).ok()
}

/// Derive a short fingerprint from a public key: "hD-XXXXXXXX"
pub fn fingerprint(pubkey: &[u8; 32]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(pubkey);
    hasher.update(b"hostelD-fingerprint");
    let hash = hasher.finalize();
    format!("hD-{:02X}{:02X}{:02X}{:02X}", hash[0], hash[1], hash[2], hash[3])
}
