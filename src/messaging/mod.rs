pub mod daemon;
pub mod session;
pub mod protocol;
pub mod outbox;

pub use daemon::MsgDaemon;

use std::net::SocketAddr;

/// Presence status for a contact.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PresenceStatus {
    Online,
    Away,
    Offline,
}

/// Commands sent from GUI to the messaging daemon.
pub enum MsgCommand {
    /// Send a text message to a contact. Daemon handles connection if needed.
    SendMessage { contact_id: String, peer_addr: SocketAddr, peer_pubkey: [u8; 32], text: String },
    /// Initiate a connection to a peer (e.g. when opening a conversation).
    Connect { contact_id: String, peer_addr: SocketAddr, peer_pubkey: [u8; 32] },
    /// Send a contact request to a peer by address.
    SendRequest { peer_addr: SocketAddr },
    /// Accept an incoming contact request.
    AcceptRequest { request_id: String },
    /// Reject an incoming contact request (silently discard).
    RejectRequest { request_id: String },
    /// Block an incoming contact request (ban IP + block pubkey).
    BlockRequest { request_id: String, ip: String },
    /// User dismissed an incoming call notification — clear cooldown so re-calls work.
    /// If reject=true, daemon will complete voice handshake + send HANGUP to cut the caller.
    DismissIncomingCall { ip: String, reject: bool },
    /// Connect to all contacts at startup (staggered to avoid burst).
    ConnectAll { contacts: Vec<(String, SocketAddr, [u8; 32])> },
    /// Update our local presence status (Online/Away) — daemon broadcasts to peers.
    UpdatePresence { status: PresenceStatus },
    /// Ask connected peers for a contact's current address (IP relay).
    QueryPeer { target_pubkey: [u8; 32] },
    /// Send a file offer to a contact.
    SendFileOffer { contact_id: String, peer_addr: SocketAddr, peer_pubkey: [u8; 32], file_path: String },
    /// Accept an incoming file transfer.
    AcceptFileTransfer { contact_id: String, transfer_id: u32 },
    /// Reject an incoming file transfer.
    RejectFileTransfer { contact_id: String, transfer_id: u32 },
    /// Cancel an active file transfer.
    CancelFileTransfer { contact_id: String, transfer_id: u32 },
    /// Voice call starting — daemon must release the UDP socket.
    YieldSocket,
    /// Voice call ended — daemon can reclaim the UDP socket.
    ReclaimSocket,
}

/// Events sent from the messaging daemon to the GUI.
pub enum MsgEvent {
    /// A message arrived from a peer.
    IncomingMessage { contact_id: String, text: String },
    /// A previously sent message was acknowledged by the peer.
    MessageDelivered,
    /// A peer's online status changed.
    PeerStatus { contact_id: String, online: bool },
    /// An incoming contact request from a stranger.
    IncomingRequest { request_id: String, nickname: String, ip: String, fingerprint: String },
    /// Our outgoing contact request was accepted; contact saved.
    RequestAccepted { contact_id: String },
    /// Our outgoing contact request failed (peer offline, timeout, etc.).
    RequestFailed { peer_addr: String, reason: String },
    /// Incoming voice call detected from a known contact.
    IncomingCall { nickname: String, fingerprint: String, ip: String, port: String },
    /// A contact's address was updated via IP relay (announce or peer response).
    PeerAddressUpdate { contact_id: String, ip: String, port: String },
    /// A peer's presence status changed (Online/Away).
    PresenceUpdate { contact_id: String, status: PresenceStatus },
    /// An incoming file offer from a peer.
    IncomingFileOffer { contact_id: String, transfer_id: u32, filename: String, file_size: u64 },
    /// Progress update for an active file transfer.
    FileTransferProgress { contact_id: String, transfer_id: u32, bytes_transferred: u64, total_bytes: u64 },
    /// A file transfer completed successfully.
    FileTransferComplete { contact_id: String, transfer_id: u32, saved_path: String },
    /// A file transfer failed or was cancelled.
    FileTransferFailed { contact_id: String, transfer_id: u32, reason: String },
}
