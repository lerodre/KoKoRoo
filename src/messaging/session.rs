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
    AwaitingIdentity { sent_at: Instant },
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
}
