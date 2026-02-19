use std::collections::HashSet;
use std::net::SocketAddr;
use std::time::{Duration, Instant};
use x25519_dalek::EphemeralSecret;

use crate::crypto::Session;

pub enum PeerState {
    /// We sent MSG_HELLO, waiting for their MSG_HELLO back.
    AwaitingHello {
        our_secret: Option<EphemeralSecret>,
        our_pubkey: [u8; 32],
        sent_at: Instant,
    },
    /// Handshake done, waiting for encrypted identity exchange.
    AwaitingIdentity,
    /// Fully connected, can exchange messages.
    Connected,
}

pub struct PeerSession {
    pub contact_id: String,
    pub peer_pubkey: [u8; 32],
    pub peer_nickname: String,
    pub peer_addr: SocketAddr,
    pub session: Option<Session>,
    pub last_activity: Instant,
    pub state: PeerState,
    /// Sequence numbers already received (dedup retries).
    pub seen_seqs: HashSet<u32>,
    /// Raw ephemeral DH shared secret, kept for Phase 2 identity-bound upgrade.
    pub ephemeral_shared: Option<[u8; 32]>,
    /// Upgraded session key binding both ephemeral and identity DH (known contacts only).
    pub upgraded_session: Option<Session>,
    /// True once a PKT_MSG_CONFIRM has been successfully exchanged.
    pub identity_confirmed: bool,
}

impl PeerSession {
    pub fn is_connected(&self) -> bool {
        matches!(self.state, PeerState::Connected)
    }

    pub fn is_timed_out(&self, timeout: Duration) -> bool {
        self.last_activity.elapsed() > timeout
    }

    pub fn touch(&mut self) {
        self.last_activity = Instant::now();
    }

    /// Decrypt trying upgraded session first, then base session.
    /// On first successful upgraded decrypt, promotes it to the primary session.
    pub fn decrypt_packet(&mut self, data: &[u8]) -> Option<(u8, Vec<u8>)> {
        // Try upgraded session first (if available)
        if let Some(ref upgraded) = self.upgraded_session {
            if let Some(result) = upgraded.decrypt_packet(data) {
                // Auto-promote: upgraded key works, make it primary
                if !self.identity_confirmed {
                    self.identity_confirmed = true;
                    self.session = self.upgraded_session.take();
                }
                return Some(result);
            }
        }
        // Fall back to base session
        let session = self.session.as_ref()?;
        session.decrypt_packet(data)
    }

    /// Encrypt using upgraded session if confirmed, else base session.
    pub fn encrypt_packet(&self, pkt_type: u8, plaintext: &[u8]) -> Option<Vec<u8>> {
        if self.identity_confirmed {
            // After confirmation, upgraded session has been promoted to self.session
            self.session.as_ref().map(|s| s.encrypt_packet(pkt_type, plaintext))
        } else if let Some(ref upgraded) = self.upgraded_session {
            Some(upgraded.encrypt_packet(pkt_type, plaintext))
        } else {
            self.session.as_ref().map(|s| s.encrypt_packet(pkt_type, plaintext))
        }
    }
}
