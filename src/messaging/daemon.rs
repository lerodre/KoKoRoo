use std::collections::{HashMap, VecDeque};
use std::net::{SocketAddr, UdpSocket};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use crate::identity::{Identity, Settings};
use crate::filetransfer::sender::SenderState;
use crate::filetransfer::receiver::ReceiverState;

use super::outbox::Outbox;
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

/// Retry backoff tiers: 10s, 30s, 1m, 5m, 15m cap
pub(super) const RETRY_BACKOFFS: &[Duration] = &[
    Duration::from_secs(10),
    Duration::from_secs(30),
    Duration::from_secs(60),
    Duration::from_secs(300),
    Duration::from_secs(900),
];

/// Active file transfer (either sending or receiving).
pub enum FileTransfer {
    Sending(SenderState),
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
}

impl MsgDaemon {
    pub fn new(
        local_port: String,
        identity: Identity,
        nickname: String,
        command_rx: mpsc::Receiver<MsgCommand>,
        event_tx: mpsc::Sender<MsgEvent>,
    ) -> Self {
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
            retry_state: HashMap::new(),
            last_keepalive: Instant::now(),
            hello_retries: HashMap::new(),
            saved_peers: Vec::new(),
            pending_requests: HashMap::new(),
            outgoing_requests: HashMap::new(),
            settings: Settings::load(),
            notified_calls: HashMap::new(),
            rejected_ips: HashMap::new(),
            ip_announces: HashMap::new(),
            last_ip_check: Instant::now() - IP_CHECK_INTERVAL, // trigger check on first housekeep
            last_announced_ip: String::new(),
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
            self.housekeep();
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
