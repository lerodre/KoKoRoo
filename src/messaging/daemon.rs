use std::collections::{HashMap, VecDeque};
use std::net::{SocketAddr, UdpSocket};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use crate::identity::{Identity, Settings};
use crate::filetransfer::sender::SenderState;
use crate::filetransfer::receiver::ReceiverState;

use super::outbox::Outbox;
use super::pending_invites::PendingInviteStore;
use super::protocol;
use super::session::PeerSession;
use super::{MsgCommand, MsgEvent};

/// State for an outgoing avatar send to one peer.
pub struct AvatarSendState {
    pub avatar_data: Vec<u8>,
    pub sha256: [u8; 32],
    pub sent: bool,
    pub sent_at: Instant,
    pub retries: u8,
    /// Whether the offer has been sent (waiting for ACK/NACK).
    pub offer_sent: bool,
    /// Set to true when NACK received — peer wants the data.
    pub needs_send: bool,
}

/// State for an incoming avatar receive from one peer.
pub struct AvatarRecvState {
    pub sha256: [u8; 32],
    pub total_size: u32,
    pub total_chunks: u16,
    pub chunks: HashMap<u16, Vec<u8>>,
    pub started_at: Instant,
    pub contact_id: String,
}

/// State for an outgoing group avatar send to one peer for one group.
pub struct GroupAvatarSendState {
    pub avatar_data: Vec<u8>,
    pub sha256: [u8; 32],
    pub sent: bool,
    pub sent_at: Instant,
    pub retries: u8,
}

/// Outgoing group chat sync: pre-chunked messages waiting to be sent.
pub struct GroupSyncOut {
    pub chunks: Vec<Vec<u8>>,  // pre-serialized JSON chunks
    pub next_chunk: u16,
    pub total_chunks: u16,
    pub last_sent_at: Instant,
    pub retries: u32,
}

/// Result of async SHA-256 hashing (pre-send or post-receive verification).
pub struct HashResult {
    pub contact_id: String,
    pub transfer_id: u32,
    pub file_path: String,
    pub file_size: u64,
    pub filename: String,
    pub peer_addr: std::net::SocketAddr,
    pub peer_pubkey: [u8; 32],
    pub sha256: Option<[u8; 32]>,
}

/// Result of async post-receive verification.
pub struct VerifyResult {
    pub contact_id: String,
    pub transfer_id: u32,
    pub success: bool,
    pub saved_path: Option<String>,
}

pub(super) const AVATAR_CHUNK_SIZE: usize = 1200;
pub(super) const AVATAR_RECV_TIMEOUT: Duration = Duration::from_secs(30);
pub(super) const AVATAR_SEND_RETRY_INTERVAL: Duration = Duration::from_secs(5);
pub(super) const AVATAR_MAX_RETRIES: u8 = 3;

pub(super) const RECV_TIMEOUT: Duration = Duration::from_millis(50);
pub(super) const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(60);
pub(super) const PEER_TIMEOUT: Duration = Duration::from_secs(300);
pub(super) const HELLO_RETRY_INTERVAL: Duration = Duration::from_secs(3);
pub(super) const HELLO_MAX_RETRIES: u32 = 20;

// IP relay constants
pub(super) const IP_CHECK_INTERVAL: Duration = Duration::from_secs(30 * 60); // 30 minutes
pub(super) const PEER_QUERY_COOLDOWN: Duration = Duration::from_secs(5 * 60); // 5 min per target pubkey
pub(super) const QUERY_RATE_WINDOW: Duration = Duration::from_secs(60);       // 1 minute window
pub(super) const QUERY_RATE_MAX: u32 = 6;                                     // max 6 queries per window per peer
pub(super) const ANNOUNCE_MAX_AGE: Duration = Duration::from_secs(2 * 60 * 60); // 2 hours
pub(super) const BEACON_INTERVAL: Duration = Duration::from_secs(10 * 60);     // 10 minutes — reconnect disconnected contacts
pub(super) const FAILED_CONTACT_COOLDOWN: Duration = Duration::from_secs(60 * 60); // 1 hour — don't retry contacts that failed recently

/// Retry backoff tiers: 10s, 30s, 1m, 5m, 15m cap
pub(super) const RETRY_BACKOFFS: &[Duration] = &[
    Duration::from_secs(10),
    Duration::from_secs(30),
    Duration::from_secs(60),
    Duration::from_secs(300),
    Duration::from_secs(900),
];

/// Active file transfer (either sending or receiving).
#[allow(dead_code)]
pub enum FileTransfer {
    Sending(SenderState), // legacy fallback, kept for compatibility
    Receiving(ReceiverState),
    /// Waiting for the peer to accept/reject our offer.
    OfferedWaiting {
        file_path: String,
        file_size: u64,
        sha256: [u8; 32],
        offered_at: Instant,
    },
    /// Waiting for our user to accept/reject an incoming offer.
    IncomingWaiting {
        transfer_id: u32,
        contact_id: String,
        filename: String,
        file_size: u64,
        sha256: [u8; 32],
    },
    /// SHA-256 being computed in background thread before sending offer.
    Hashing {
        file_path: String,
        file_size: u64,
        filename: String,
        peer_addr: std::net::SocketAddr,
        peer_pubkey: [u8; 32],
    },
    /// File being sent by a dedicated thread.
    SendingThreaded {
        cmd_tx: mpsc::Sender<crate::filetransfer::sender::SenderThreadCmd>,
        sha256: [u8; 32],
        file_size: u64,
        complete_sent: bool,
        complete_sent_at: Instant,
    },
}

pub struct MsgDaemon {
    pub(super) socket: Option<UdpSocket>,
    pub(super) local_port: String,
    pub(super) identity: Identity,
    pub(super) nickname: String,
    /// Active peer sessions keyed by socket address.
    pub(super) peers: HashMap<SocketAddr, PeerSession>,
    /// Reverse lookup: contact_id -> peer socket address.
    pub(super) contact_addrs: HashMap<String, SocketAddr>,
    pub(super) command_rx: mpsc::Receiver<MsgCommand>,
    pub(super) event_tx: mpsc::Sender<MsgEvent>,
    pub(super) outboxes: HashMap<String, Outbox>,
    /// Pending group invites per contact (queued when peer is offline).
    pub(super) pending_invites: HashMap<String, PendingInviteStore>,
    /// Last outbox retry time per contact_id, plus current backoff tier index.
    pub(super) retry_state: HashMap<String, (Instant, usize)>,
    pub(super) last_keepalive: Instant,
    /// Counts of HELLO retries per address.
    pub(super) hello_retries: HashMap<SocketAddr, u32>,
    /// Saved peer info for reconnection after voice call ends.
    pub(super) saved_peers: Vec<(String, SocketAddr, [u8; 32])>,
    /// Pending incoming requests: request_id -> (peer_addr, identity_pubkey, nickname, fingerprint)
    pub(super) pending_requests: HashMap<String, (SocketAddr, [u8; 32], String, String)>,
    /// Outgoing request sessions (ephemeral handshakes before REQUEST is sent).
    pub(super) outgoing_requests: HashMap<SocketAddr, PeerSession>,
    /// Settings snapshot for checking banned IPs / blocked contacts.
    pub(super) settings: Settings,
    /// IPs we already notified about an incoming voice call (avoid re-notifying on HELLO retries).
    /// Value: (timestamp, peer_addr, peer_ephemeral_pubkey) — stored for reject-with-hangup.
    pub(super) notified_calls: HashMap<String, (Instant, SocketAddr, [u8; 32])>,
    /// Recently rejected IPs — suppress extra HELLOs for a few seconds after reject.
    pub(super) rejected_ips: HashMap<String, Instant>,
    // ── IP relay state ──
    /// Cached IP announces from peers: identity pubkey → (ip, port, timestamp).
    pub(super) ip_announces: HashMap<[u8; 32], (String, String, u64)>,
    /// Last time we checked our own IP for changes.
    pub(super) last_ip_check: Instant,
    /// Our last announced IP (to detect changes).
    pub(super) last_announced_ip: String,
    /// Cooldown per target pubkey for outgoing PEER_QUERY commands.
    pub(super) peer_query_cooldowns: HashMap<[u8; 32], Instant>,
    /// Rate limiting for incoming PEER_QUERY: peer addr → (window_start, count).
    pub(super) query_counts: HashMap<SocketAddr, (Instant, u32)>,
    /// Staggered connect queue for ConnectAll.
    pub(super) connect_queue: VecDeque<(String, SocketAddr, [u8; 32])>,
    /// Last time a contact was popped from the connect queue.
    pub(super) last_queue_pop: Instant,
    /// Our current presence status.
    pub(super) our_presence: super::PresenceStatus,
    /// Last time we ran the periodic reconnect beacon.
    pub(super) last_beacon: Instant,
    /// All known contacts (for periodic reconnect). Populated by ConnectAll.
    pub(super) all_contacts: Vec<(String, SocketAddr, [u8; 32])>,
    /// Active file transfers keyed by (contact_id, transfer_id).
    pub(super) file_transfers: HashMap<(String, u32), FileTransfer>,
    /// Pending outgoing avatar sends: contact_id -> state.
    pub(super) avatar_sends: HashMap<String, AvatarSendState>,
    /// Pending incoming avatar receives: peer socket addr -> state.
    pub(super) avatar_recvs: HashMap<SocketAddr, AvatarRecvState>,
    /// Counter for generating unique transfer IDs.
    pub(super) next_transfer_id: u32,
    /// Last time progress events were emitted.
    pub(super) last_progress_emit: Instant,
    /// Pending incoming group avatar receives: (peer_addr, group_id) -> state.
    pub(super) group_avatar_recvs: HashMap<(SocketAddr, String), AvatarRecvState>,
    /// Pending outgoing group avatar sends: (contact_id, group_id) -> state.
    pub(super) group_avatar_sends: HashMap<(String, String), GroupAvatarSendState>,
    /// Members to sync when a group invite ACK is received: (contact_id, group_id) -> members.
    pub(super) pending_member_syncs: HashMap<(String, String), Vec<crate::group::GroupMember>>,
    /// Contacts that failed to connect (max HELLO retries). Cooldown before retrying.
    pub(super) failed_contacts: HashMap<String, Instant>,
    /// Pending contact deletions (queued when peer is offline).
    pub(super) pending_deletes: super::pending_deletes::PendingDeleteStore,
    /// Active group chat sync sessions: (peer_addr, group_id, channel_id) -> outgoing chunks.
    pub(super) group_sync_out: HashMap<(SocketAddr, String, String), GroupSyncOut>,
    pub(super) hash_results_tx: mpsc::Sender<HashResult>,
    pub(super) hash_results_rx: mpsc::Receiver<HashResult>,
    pub(super) verify_results_tx: mpsc::Sender<VerifyResult>,
    pub(super) verify_results_rx: mpsc::Receiver<VerifyResult>,
    pub(super) sender_events_tx: mpsc::Sender<crate::filetransfer::sender::SenderThreadEvent>,
    pub(super) sender_events_rx: mpsc::Receiver<crate::filetransfer::sender::SenderThreadEvent>,
}

impl MsgDaemon {
    pub fn new(
        local_port: String,
        identity: Identity,
        nickname: String,
        command_rx: mpsc::Receiver<MsgCommand>,
        event_tx: mpsc::Sender<MsgEvent>,
    ) -> Self {
        let settings = Settings::load();
        let initial_ip = crate::gui::get_best_ipv6(&settings.network_adapter);
        let pending_deletes = super::pending_deletes::PendingDeleteStore::load(&identity.secret);
        let (hash_results_tx, hash_results_rx) = mpsc::channel();
        let (verify_results_tx, verify_results_rx) = mpsc::channel();
        let (sender_events_tx, sender_events_rx) = mpsc::channel();
        MsgDaemon {
            socket: None,
            local_port,
            identity,
            nickname,
            peers: HashMap::new(),
            contact_addrs: HashMap::new(),
            command_rx,
            event_tx,
            outboxes: HashMap::new(),
            pending_invites: HashMap::new(),
            retry_state: HashMap::new(),
            last_keepalive: Instant::now(),
            hello_retries: HashMap::new(),
            saved_peers: Vec::new(),
            pending_requests: HashMap::new(),
            outgoing_requests: HashMap::new(),
            settings,
            notified_calls: HashMap::new(),
            rejected_ips: HashMap::new(),
            ip_announces: HashMap::new(),
            last_ip_check: Instant::now().checked_sub(IP_CHECK_INTERVAL).unwrap_or_else(Instant::now), // trigger check on first housekeep
            last_announced_ip: initial_ip,
            peer_query_cooldowns: HashMap::new(),
            query_counts: HashMap::new(),
            connect_queue: VecDeque::new(),
            last_queue_pop: Instant::now(),
            our_presence: super::PresenceStatus::Online,
            last_beacon: Instant::now(),
            all_contacts: Vec::new(),
            file_transfers: HashMap::new(),
            next_transfer_id: 1,
            last_progress_emit: Instant::now(),
            avatar_sends: HashMap::new(),
            avatar_recvs: HashMap::new(),
            group_avatar_recvs: HashMap::new(),
            group_avatar_sends: HashMap::new(),
            pending_member_syncs: HashMap::new(),
            failed_contacts: HashMap::new(),
            pending_deletes,
            group_sync_out: HashMap::new(),
            hash_results_tx,
            hash_results_rx,
            verify_results_tx,
            verify_results_rx,
            sender_events_tx,
            sender_events_rx,
        }
    }

    /// Main daemon loop. Runs until Shutdown command.
    pub fn run(&mut self) {
        self.bind_socket();

        loop {
            if !self.process_commands() {
                break; // Shutdown
            }
            if self.socket.is_none() {
                // No socket (yielded for voice call) — sleep to avoid busy-wait
                std::thread::sleep(Duration::from_millis(50));
                continue;
            }
            self.receive_packets();
            self.drain_background_results();
            self.housekeep();
        }

        // Cancel all active sender threads
        for (_, ft) in &self.file_transfers {
            if let FileTransfer::SendingThreaded { cmd_tx, .. } = ft {
                cmd_tx.send(crate::filetransfer::sender::SenderThreadCmd::Cancel).ok();
            }
        }
        // Disconnect all peers
        if let Some(ref socket) = self.socket {
            for peer in self.peers.values() {
                protocol::send_bye(peer, socket);
            }
        }
        // Save all outboxes
        for outbox in self.outboxes.values() {
            outbox.save();
        }
        // Save all pending invite stores
        for store in self.pending_invites.values() {
            store.save();
        }
    }

    pub(super) fn bind_socket(&mut self) {
        let addr = format!("[::]:{}", self.local_port);
        // Retry up to 20 times (4 seconds) — voice engine may still hold the socket
        for attempt in 0..20 {
            match UdpSocket::bind(&addr) {
                Ok(s) => {
                    s.set_read_timeout(Some(RECV_TIMEOUT)).ok();
                    s.set_nonblocking(false).ok();
                    // Large buffers for file transfer bursts (16 MB each)
                    set_socket_buffers(&s, 16 * 1024 * 1024);
                    self.socket = Some(s);
                    log_fmt!("[daemon] bind_socket OK on attempt {}", attempt + 1);
                    return;
                }
                Err(e) => {
                    if attempt < 19 {
                        std::thread::sleep(Duration::from_millis(200));
                    } else {
                        log_fmt!("[daemon] bind_socket FAILED after 20 attempts: {}", e);
                    }
                }
            }
        }
    }

    /// Drain results from background threads (hash, verify, sender).
    pub(crate) fn drain_background_results(&mut self) {
        use crate::filetransfer;

        // Drain hash results
        while let Ok(result) = self.hash_results_rx.try_recv() {
            let key = (result.contact_id.clone(), result.transfer_id);
            if let Some(sha256) = result.sha256 {
                // Hash succeeded — send FILE_OFFER and transition to OfferedWaiting
                if let Some(FileTransfer::Hashing { .. }) = self.file_transfers.get(&key) {
                    // Ensure connected and send offer
                    if let Some(addr) = self.contact_addrs.get(&result.contact_id) {
                        if let Some(peer) = self.peers.get(addr) {
                            if peer.is_connected() {
                                if let Some(ref socket) = self.socket {
                                    filetransfer::protocol::send_file_offer(
                                        peer, socket, result.transfer_id,
                                        result.file_size, &sha256, &result.filename,
                                    );
                                }
                            }
                        }
                    } else {
                        // Try to connect
                        if let Some(ref socket) = self.socket {
                            if let Some(session) = super::protocol::initiate_handshake(
                                socket, &result.contact_id, result.peer_addr, result.peer_pubkey,
                            ) {
                                self.contact_addrs.insert(result.contact_id.clone(), result.peer_addr);
                                self.hello_retries.insert(result.peer_addr, 0);
                                self.peers.insert(result.peer_addr, session);
                            }
                        }
                    }
                    self.file_transfers.insert(key, FileTransfer::OfferedWaiting {
                        file_path: result.file_path,
                        file_size: result.file_size,
                        sha256,
                        offered_at: Instant::now(),
                    });
                }
            } else {
                // Hash failed
                self.file_transfers.remove(&key);
                self.event_tx.send(super::MsgEvent::FileTransferFailed {
                    contact_id: result.contact_id,
                    transfer_id: result.transfer_id,
                    reason: "Failed to hash file".into(),
                }).ok();
            }
        }

        // Drain verify results
        while let Ok(result) = self.verify_results_rx.try_recv() {
            if result.success {
                self.event_tx.send(super::MsgEvent::FileTransferComplete {
                    contact_id: result.contact_id,
                    transfer_id: result.transfer_id,
                    saved_path: result.saved_path.unwrap_or_default(),
                }).ok();
            } else {
                self.event_tx.send(super::MsgEvent::FileTransferFailed {
                    contact_id: result.contact_id,
                    transfer_id: result.transfer_id,
                    reason: "Hash verification failed".into(),
                }).ok();
            }
        }

        // Drain sender thread events
        while let Ok(event) = self.sender_events_rx.try_recv() {
            let key = (event.contact_id.clone(), event.transfer_id);
            match event.kind {
                crate::filetransfer::sender::SenderEventKind::Progress { bytes_sent, total } => {
                    self.event_tx.send(super::MsgEvent::FileTransferProgress {
                        contact_id: event.contact_id,
                        transfer_id: event.transfer_id,
                        bytes_transferred: bytes_sent,
                        total_bytes: total,
                    }).ok();
                }
                crate::filetransfer::sender::SenderEventKind::AllChunksSent => {
                    // Send FILE_COMPLETE via the peer session
                    if let Some(FileTransfer::SendingThreaded { sha256, complete_sent, complete_sent_at, .. }) = self.file_transfers.get_mut(&key) {
                        if !*complete_sent {
                            if let Some(addr) = self.contact_addrs.get(&event.contact_id) {
                                if let Some(peer) = self.peers.get(addr) {
                                    if let Some(ref socket) = self.socket {
                                        filetransfer::protocol::send_file_complete(
                                            peer, socket, event.transfer_id, sha256,
                                        );
                                    }
                                }
                            }
                            *complete_sent = true;
                            *complete_sent_at = Instant::now();
                        }
                    }
                }
                crate::filetransfer::sender::SenderEventKind::Done => {
                    self.file_transfers.remove(&key);
                    self.event_tx.send(super::MsgEvent::FileTransferComplete {
                        contact_id: event.contact_id,
                        transfer_id: event.transfer_id,
                        saved_path: String::new(),
                    }).ok();
                }
                crate::filetransfer::sender::SenderEventKind::Error(reason) => {
                    self.file_transfers.remove(&key);
                    self.event_tx.send(super::MsgEvent::FileTransferFailed {
                        contact_id: event.contact_id,
                        transfer_id: event.transfer_id,
                        reason,
                    }).ok();
                }
            }
        }
    }
}

/// Set large socket send/receive buffers for file transfer throughput.
fn set_socket_buffers(socket: &UdpSocket, size: usize) {
    let size_i = size as i32;
    let size_ptr = &size_i as *const i32 as *const u8;
    let size_len = std::mem::size_of::<i32>() as u32;

    #[cfg(target_os = "windows")]
    {
        use std::os::windows::io::AsRawSocket;
        let raw = socket.as_raw_socket() as usize;
        extern "system" {
            fn setsockopt(s: usize, level: i32, optname: i32, optval: *const u8, optlen: i32) -> i32;
        }
        unsafe {
            setsockopt(raw, 0xFFFF, 0x1002, size_ptr, size_len as i32); // SO_RCVBUF
            setsockopt(raw, 0xFFFF, 0x1001, size_ptr, size_len as i32); // SO_SNDBUF
        }
    }

    #[cfg(unix)]
    {
        use std::os::unix::io::AsRawFd;
        extern "C" {
            fn setsockopt(sockfd: i32, level: i32, optname: i32, optval: *const u8, optlen: u32) -> i32;
        }
        let raw = socket.as_raw_fd();
        // Linux: SOL_SOCKET=1, SO_RCVBUF=8, SO_SNDBUF=7
        unsafe {
            setsockopt(raw, 1, 8, size_ptr, size_len as u32);
            setsockopt(raw, 1, 7, size_ptr, size_len as u32);
        }
    }

    let _ = (socket, size); // suppress unused warnings on other platforms
}
