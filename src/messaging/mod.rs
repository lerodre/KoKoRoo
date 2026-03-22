pub mod daemon;
pub mod session;
pub mod protocol;
pub mod outbox;
pub mod pending_invites;
mod commands;
mod packets;
mod housekeep;

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
#[allow(dead_code)]
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
    /// Broadcast our avatar to all connected + identity-confirmed peers.
    BroadcastAvatar { avatar_data: Vec<u8>, sha256: [u8; 32] },
    /// Send our avatar to a specific contact (e.g. on new contact add).
    SendAvatarTo { contact_id: String, avatar_data: Vec<u8>, sha256: [u8; 32] },
    /// Send a group invite to a contact (lite invite JSON + members for sync on ACK).
    SendGroupInvite { contact_id: String, peer_addr: SocketAddr, peer_pubkey: [u8; 32], invite_json: Vec<u8>, members: Vec<crate::group::GroupMember> },
    /// Send a group chat message to a specific member via pairwise session.
    SendGroupChat { contact_id: String, peer_addr: SocketAddr, peer_pubkey: [u8; 32], group_id: String, channel_id: String, text: String },
    /// Accept an incoming group invite (send ACK to the inviter).
    AcceptGroupInvite { contact_id: String, group_id: String },
    /// Reject an incoming group invite (send NACK to the inviter).
    RejectGroupInvite { contact_id: String, group_id: String },
    /// Broadcast a group metadata update to all members of the group.
    SendGroupUpdate { group_id: String, group_json: Vec<u8>, member_contacts: Vec<(String, std::net::SocketAddr, [u8; 32])> },
    /// Broadcast a group avatar to all members of the group.
    SendGroupAvatar { group_id: String, avatar_data: Vec<u8>, sha256: [u8; 32], member_contacts: Vec<(String, std::net::SocketAddr, [u8; 32])> },
    /// Broadcast group call presence signal to all group members.
    SendCallSignal { group_id: String, channel_id: String, active: bool, call_mode: u8, member_contacts: Vec<(String, std::net::SocketAddr, [u8; 32])> },
    /// Broadcast updated nickname to all connected peers.
    UpdateNickname { nickname: String },
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
    /// A contact's nickname was updated.
    NicknameUpdated { contact_id: String, nickname: String },
    /// A contact's avatar was received and saved.
    AvatarReceived { contact_id: String },
    /// Incoming group invite from a contact (lite invite JSON).
    IncomingGroupInvite { from_nickname: String, from_contact_id: String, invite_json: Vec<u8> },
    /// Incoming group chat message via messaging daemon.
    /// `sender_fingerprint` is derived from the peer's verified pubkey (never from peer-supplied data).
    IncomingGroupChat { group_id: String, channel_id: String, sender_fingerprint: String, sender_nickname: String, text: String },
    /// A peer rejected our group invite — remove them from the group.
    GroupInviteRejected { contact_id: String, group_id: String },
    /// A group metadata update was received from an admin.
    GroupUpdated { group_json: Vec<u8> },
    /// A group member was synced after invite accept.
    GroupMemberSynced { group_id: String, member: crate::group::GroupMember },
    /// A group avatar was fully received and saved to disk.
    GroupAvatarReceived { group_id: String },
    /// A group call presence signal from a peer.
    GroupCallSignal { contact_id: String, group_id: String, channel_id: String, active: bool, call_mode: u8 },
}
