pub mod daemon;
pub mod session;
pub mod protocol;
pub mod outbox;

pub use daemon::MsgDaemon;

use std::net::SocketAddr;

/// Commands sent from GUI to the messaging daemon.
pub enum MsgCommand {
    /// Send a text message to a contact. Daemon handles connection if needed.
    SendMessage { contact_id: String, peer_addr: SocketAddr, peer_pubkey: [u8; 32], text: String },
    /// Initiate a connection to a peer (e.g. when opening a conversation).
    Connect { contact_id: String, peer_addr: SocketAddr, peer_pubkey: [u8; 32] },
    /// Voice call starting — daemon must release the UDP socket.
    YieldSocket,
    /// Voice call ended — daemon can reclaim the UDP socket.
    ReclaimSocket,
    /// App shutting down.
    Shutdown,
}

/// Events sent from the messaging daemon to the GUI.
pub enum MsgEvent {
    /// A message arrived from a peer.
    IncomingMessage { contact_id: String, text: String, timestamp: u64 },
    /// A previously sent message was acknowledged by the peer.
    MessageDelivered { contact_id: String, seq: u32 },
    /// A peer's online status changed.
    PeerStatus { contact_id: String, online: bool },
}
