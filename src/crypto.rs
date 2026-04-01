use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{ChaCha20Poly1305, Nonce};
use sha2::{Digest, Sha256};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use x25519_dalek::{EphemeralSecret, PublicKey, SharedSecret, StaticSecret};

/// Packet type markers (first byte of every UDP packet).
pub const PKT_HELLO: u8 = 0x01;    // key exchange: [0x01][32-byte ephemeral pubkey]
pub const PKT_VOICE: u8 = 0x02;    // encrypted voice: [0x02][4-byte counter][ciphertext+tag]
pub const PKT_IDENTITY: u8 = 0x03; // identity exchange: [0x03][4-byte counter][encrypted identity pubkey]
pub const PKT_CHAT: u8 = 0x04;     // chat message: [0x04][4-byte counter][encrypted text+timestamp]
pub const PKT_HANGUP: u8 = 0x05;   // hangup signal: [0x05][4-byte counter][encrypted empty]
pub const PKT_SCREEN: u8 = 0x06;       // screen share: [0x06][4-byte counter][encrypted VP8 chunk]
pub const PKT_SCREEN_STOP: u8 = 0x07;  // screen share ended: [0x07][4-byte counter][encrypted empty]
pub const PKT_SCREEN_OFFER: u8 = 0x08; // screen offer beacon: [0x08][4-byte counter][encrypted empty]
pub const PKT_SCREEN_JOIN: u8 = 0x09;  // screen join/leave: [0x09][4-byte counter][encrypted 0x01=join/0x00=leave]

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
pub const PKT_MSG_FILE_ACK: u8      = 0x1F; // file ack (all received): encrypted [4B transfer_id]
pub const PKT_MSG_FILE_COMPLETE: u8 = 0x20; // file complete (sender done): encrypted [4B transfer_id][32B sha256]
pub const PKT_MSG_FILE_CANCEL: u8   = 0x21; // file cancel: encrypted [4B transfer_id][1B reason]
pub const PKT_MSG_FILE_NACK: u8     = 0x22; // file nack (missing chunks): encrypted [4B transfer_id][4B missing_count LE][4B idx LE ...]
pub const PKT_MSG_CONFIRM: u8       = 0x23; // identity-bound key upgrade confirmation
pub const PKT_MSG_AVATAR_OFFER: u8  = 0x24; // avatar offer: encrypted [32B sha256][4B total_size LE]
pub const PKT_MSG_AVATAR_DATA: u8   = 0x25; // avatar chunk: encrypted [2B chunk_index LE][up to 1200B data]
pub const PKT_MSG_AVATAR_ACK: u8    = 0x26; // avatar received: encrypted [32B sha256]
pub const PKT_MSG_AVATAR_NACK: u8   = 0x27; // avatar needed: encrypted [32B sha256]
pub const PKT_MSG_DELETE_CONTACT: u8 = 0x28; // contact deletion signal: encrypted empty payload
pub const PKT_MSG_DELETE_ACK: u8     = 0x29; // contact deletion acknowledgement: encrypted empty payload

// Group call packet types (shared-key encryption with sender_index nonces)
pub const PKT_GRP_HELLO: u8        = 0x30; // join request: [0x30][32B ephemeral pubkey][16B group_id]
pub const PKT_GRP_VOICE: u8        = 0x31; // voice: [0x31][2B sender_idx][4B counter][ciphertext+tag]
pub const PKT_GRP_CHAT: u8         = 0x32; // chat: [0x32][2B sender_idx][4B counter][encrypted text]
pub const PKT_GRP_HANGUP: u8       = 0x33; // member leaving
pub const PKT_GRP_ROSTER: u8       = 0x34; // leader → all: encrypted JSON roster
pub const PKT_GRP_PING: u8         = 0x35; // leader probes member: encrypted 8B timestamp
pub const PKT_GRP_PONG: u8         = 0x36; // member responds: encrypted 8B echo
pub const PKT_GRP_LEADER: u8       = 0x37; // new leader announcement: encrypted 32B pubkey
pub const PKT_GRP_INVITE: u8       = 0x38; // invite via messaging daemon
pub const PKT_GRP_ALIVE: u8        = 0x39; // failover: "I'm alive" discovery
pub const PKT_GRP_SPEED_DATA: u8   = 0x3A; // failover: speed test burst payload
pub const PKT_GRP_SPEED_RESULT: u8 = 0x3B; // failover: speed test result
pub const PKT_GRP_MSG_CHAT: u8    = 0x3C; // group chat via messaging daemon (offline)
pub const PKT_GRP_INVITE_ACK: u8  = 0x3D; // peer accepted group invite
pub const PKT_GRP_INVITE_NACK: u8 = 0x3E; // peer rejected group invite
pub const PKT_GRP_UPDATE: u8       = 0x3F; // group metadata update (full JSON)
pub const PKT_GRP_AVATAR_OFFER: u8 = 0x40; // group avatar: group_id\n + sha256 + size
pub const PKT_GRP_AVATAR_DATA: u8  = 0x41; // group avatar chunk: group_id\n + chunk_idx + data
pub const PKT_GRP_SCREEN: u8       = 0x42; // group screen share VP8 chunk data
pub const PKT_GRP_SCREEN_OFFER: u8 = 0x43; // group screen share beacon
pub const PKT_GRP_SCREEN_STOP: u8  = 0x44; // group screen share stopped
pub const PKT_GRP_MEMBER_SYNC: u8  = 0x45; // per-member sync after invite accept
pub const PKT_GRP_AVATAR_ACK: u8   = 0x46; // group avatar received: group_id\n + sha256
pub const PKT_GRP_CALL_SIGNAL: u8  = 0x47; // group call presence signal via daemon
pub const PKT_GRP_SYNC_REQUEST: u8 = 0x48; // group chat sync request: group_id\nchannel_id\n[8B ts][4B count]
pub const PKT_GRP_SYNC_DATA: u8    = 0x49; // group chat sync data chunk: group_id\nchannel_id\n[2B idx][2B total]\nJSON
pub const PKT_GRP_SYNC_ACK: u8     = 0x4A; // group chat sync chunk ack: group_id\nchannel_id\n[2B idx]

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

/// Complete the key exchange: our secret + peer's public key → (Session, ephemeral_shared_bytes).
///
/// Derives a 256-bit symmetric key from the X25519 shared secret using SHA-256.
/// Also generates a human-readable verification code for anti-MITM.
/// Returns the raw ephemeral shared secret bytes for Phase 2 identity-bound upgrade.
pub fn complete_handshake(our_secret: EphemeralSecret, peer_pubkey: &[u8; 32]) -> (Session, [u8; 32]) {
    let peer_public = PublicKey::from(*peer_pubkey);
    let shared: SharedSecret = our_secret.diffie_hellman(&peer_public);
    let ephemeral_shared = *shared.as_bytes();

    // Derive encryption key: SHA-256(shared_secret || "kokoroo-voice-key")
    let mut hasher = Sha256::new();
    hasher.update(shared.as_bytes());
    hasher.update(b"kokoroo-voice-key");
    let key_bytes = hasher.finalize();

    let cipher = ChaCha20Poly1305::new_from_slice(&key_bytes)
        .expect("key size mismatch");

    // Verification code: SHA-256(shared_secret || "kokoroo-verify")
    // Display as XXXX-XXXX so users can compare verbally.
    let mut verify_hasher = Sha256::new();
    verify_hasher.update(shared.as_bytes());
    verify_hasher.update(b"kokoroo-verify");
    let verify_hash = verify_hasher.finalize();
    let code = format!(
        "{:02X}{:02X}-{:02X}{:02X}",
        verify_hash[0], verify_hash[1], verify_hash[2], verify_hash[3]
    );

    (Session {
        cipher,
        send_counter: Arc::new(AtomicU32::new(0)),
        verification_code: code,
    }, ephemeral_shared)
}

/// Compute an upgraded session key that binds both ephemeral and identity DH.
/// Used for known contacts after IDENTITY exchange to prove private key ownership.
pub fn upgrade_session_with_identity(
    ephemeral_shared: &[u8; 32],
    our_identity_secret: &[u8; 32],
    peer_identity_pubkey: &[u8; 32],
) -> Session {
    let identity_secret = StaticSecret::from(*our_identity_secret);
    let peer_identity_public = PublicKey::from(*peer_identity_pubkey);
    let identity_dh = identity_secret.diffie_hellman(&peer_identity_public);

    // Derive upgraded key: SHA-256(ephemeral_shared || identity_DH || "kokoroo-msg-key")
    let mut hasher = Sha256::new();
    hasher.update(ephemeral_shared);
    hasher.update(identity_dh.as_bytes());
    hasher.update(b"kokoroo-msg-key");
    let key_bytes = hasher.finalize();

    let cipher = ChaCha20Poly1305::new_from_slice(&key_bytes)
        .expect("key size mismatch");

    // Upgraded verification: SHA-256(ephemeral_shared || identity_DH || "kokoroo-verify")
    let mut verify_hasher = Sha256::new();
    verify_hasher.update(ephemeral_shared);
    verify_hasher.update(identity_dh.as_bytes());
    verify_hasher.update(b"kokoroo-verify");
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
    hasher.update(b"kokoroo-local-storage");
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

/// Derive a short fingerprint from a public key: "KR-XXXXXXXX"
pub fn fingerprint(pubkey: &[u8; 32]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(pubkey);
    hasher.update(b"kokoroo-fingerprint");
    let hash = hasher.finalize();
    format!("KR-{:02X}{:02X}{:02X}{:02X}", hash[0], hash[1], hash[2], hash[3])
}

// ── Group encryption (shared key with sender_index-based nonces) ──

/// Group HELLO packet: [0x30][32B ephemeral pubkey][16B group_id_bytes]
pub const GRP_HELLO_SIZE: usize = 1 + PUBKEY_SIZE + 16;

/// Build a group HELLO packet.
pub fn build_grp_hello(pubkey: &[u8; 32], group_id: &[u8; 16]) -> [u8; GRP_HELLO_SIZE] {
    let mut pkt = [0u8; GRP_HELLO_SIZE];
    pkt[0] = PKT_GRP_HELLO;
    pkt[1..33].copy_from_slice(pubkey);
    pkt[33..49].copy_from_slice(group_id);
    pkt
}

/// Parse a group HELLO packet. Returns (ephemeral_pubkey, group_id_bytes).
pub fn parse_grp_hello(data: &[u8]) -> Option<([u8; 32], [u8; 16])> {
    if data.len() < GRP_HELLO_SIZE || data[0] != PKT_GRP_HELLO {
        return None;
    }
    let mut pubkey = [0u8; 32];
    pubkey.copy_from_slice(&data[1..33]);
    let mut group_id = [0u8; 16];
    group_id.copy_from_slice(&data[33..49]);
    Some((pubkey, group_id))
}

/// Convert a hex group_id string (32 chars) to 16 raw bytes.
pub fn group_id_to_bytes(group_id: &str) -> Option<[u8; 16]> {
    if group_id.len() != 32 {
        return None;
    }
    let mut bytes = [0u8; 16];
    for i in 0..16 {
        bytes[i] = u8::from_str_radix(&group_id[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some(bytes)
}

/// Convert 16 raw bytes to a hex group_id string (32 chars).
pub fn group_id_from_bytes(bytes: &[u8; 16]) -> String {
    bytes.iter().map(|b| format!("{:02x}", b)).collect()
}

/// Encrypt a group packet using the shared group key.
///
/// Nonce = [sender_index LE u16][counter LE u32][0x00; 6] = 12 bytes.
/// Packet = [1B type][2B sender_index LE][4B counter LE][ciphertext + 16B tag]
pub fn grp_encrypt(
    cipher: &ChaCha20Poly1305,
    sender_index: u16,
    counter: u32,
    pkt_type: u8,
    plaintext: &[u8],
) -> Vec<u8> {
    let mut nonce_bytes = [0u8; NONCE_SIZE];
    nonce_bytes[..2].copy_from_slice(&sender_index.to_le_bytes());
    nonce_bytes[2..6].copy_from_slice(&counter.to_le_bytes());
    let nonce = Nonce::from(nonce_bytes);

    let ciphertext = cipher.encrypt(&nonce, plaintext)
        .expect("group encryption failed");

    let mut packet = Vec::with_capacity(1 + 2 + 4 + ciphertext.len());
    packet.push(pkt_type);
    packet.extend_from_slice(&sender_index.to_le_bytes());
    packet.extend_from_slice(&counter.to_le_bytes());
    packet.extend_from_slice(&ciphertext);
    packet
}

/// Decrypt a group packet using the shared group key.
///
/// Returns (pkt_type, sender_index, plaintext) or None if decryption fails.
/// Minimum size: 1 (type) + 2 (sender_index) + 4 (counter) + 16 (tag) = 23 bytes.
pub fn grp_decrypt(
    cipher: &ChaCha20Poly1305,
    packet: &[u8],
) -> Option<(u8, u16, Vec<u8>)> {
    if packet.len() < 1 + 2 + 4 + TAG_SIZE {
        return None;
    }
    let pkt_type = packet[0];
    let sender_index = u16::from_le_bytes([packet[1], packet[2]]);
    let counter = u32::from_le_bytes([packet[3], packet[4], packet[5], packet[6]]);

    let mut nonce_bytes = [0u8; NONCE_SIZE];
    nonce_bytes[..2].copy_from_slice(&sender_index.to_le_bytes());
    nonce_bytes[2..6].copy_from_slice(&counter.to_le_bytes());
    let nonce = Nonce::from(nonce_bytes);

    let ciphertext = &packet[7..];
    cipher.decrypt(&nonce, ciphertext)
        .ok()
        .map(|plain| (pkt_type, sender_index, plain))
}

/// Create a ChaCha20Poly1305 cipher from a raw 32-byte group key.
pub fn grp_cipher_from_key(key: &[u8; 32]) -> ChaCha20Poly1305 {
    ChaCha20Poly1305::new_from_slice(key).expect("key size mismatch")
}

/// Extract sender_index from a group packet header without decrypting.
/// Used by the relay leader to identify the sender and forward.
pub fn grp_read_header(packet: &[u8]) -> Option<(u8, u16)> {
    if packet.len() < 3 {
        return None;
    }
    let pkt_type = packet[0];
    let sender_index = u16::from_le_bytes([packet[1], packet[2]]);
    Some((pkt_type, sender_index))
}
