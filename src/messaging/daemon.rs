use std::collections::{HashMap, VecDeque};
use std::net::{SocketAddr, UdpSocket};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use crate::crypto::{
    self, PKT_HELLO, PKT_HANGUP,
    PKT_MSG_ACK, PKT_MSG_BYE, PKT_MSG_CHAT, PKT_MSG_HELLO, PKT_MSG_IDENTITY,
    PKT_MSG_REQUEST, PKT_MSG_REQUEST_ACK,
    PKT_MSG_IP_ANNOUNCE, PKT_MSG_PEER_QUERY, PKT_MSG_PEER_RESPONSE,
    PKT_MSG_PRESENCE,
};
use crate::identity::{self, Identity, Settings};

use super::outbox::Outbox;
use super::protocol;
use super::session::PeerSession;
use super::{MsgCommand, MsgEvent};

const RECV_TIMEOUT: Duration = Duration::from_millis(50);
const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(60);
const PEER_TIMEOUT: Duration = Duration::from_secs(300);
const HELLO_RETRY_INTERVAL: Duration = Duration::from_secs(3);
const HELLO_MAX_RETRIES: u32 = 20;

// IP relay constants
const IP_CHECK_INTERVAL: Duration = Duration::from_secs(30 * 60); // 30 minutes
const PEER_QUERY_COOLDOWN: Duration = Duration::from_secs(5 * 60); // 5 min per target pubkey
const QUERY_RATE_WINDOW: Duration = Duration::from_secs(60);       // 1 minute window
const QUERY_RATE_MAX: u32 = 6;                                     // max 6 queries per window per peer
const ANNOUNCE_MAX_AGE: Duration = Duration::from_secs(2 * 60 * 60); // 2 hours
const BEACON_INTERVAL: Duration = Duration::from_secs(10 * 60);     // 10 minutes — reconnect disconnected contacts

/// Retry backoff tiers: 10s, 30s, 1m, 5m, 15m cap
const RETRY_BACKOFFS: &[Duration] = &[
    Duration::from_secs(10),
    Duration::from_secs(30),
    Duration::from_secs(60),
    Duration::from_secs(300),
    Duration::from_secs(900),
];

pub struct MsgDaemon {
    socket: Option<UdpSocket>,
    local_port: String,
    identity: Identity,
    nickname: String,
    /// Active peer sessions keyed by socket address.
    peers: HashMap<SocketAddr, PeerSession>,
    /// Reverse lookup: contact_id -> peer socket address.
    contact_addrs: HashMap<String, SocketAddr>,
    command_rx: mpsc::Receiver<MsgCommand>,
    event_tx: mpsc::Sender<MsgEvent>,
    outboxes: HashMap<String, Outbox>,
    /// Last outbox retry time per contact_id, plus current backoff tier index.
    retry_state: HashMap<String, (Instant, usize)>,
    last_keepalive: Instant,
    /// Counts of HELLO retries per address.
    hello_retries: HashMap<SocketAddr, u32>,
    /// Saved peer info for reconnection after voice call ends.
    saved_peers: Vec<(String, SocketAddr, [u8; 32])>,
    /// Pending incoming requests: request_id -> (peer_addr, identity_pubkey, nickname, fingerprint)
    pending_requests: HashMap<String, (SocketAddr, [u8; 32], String, String)>,
    /// Outgoing request sessions (ephemeral handshakes before REQUEST is sent).
    outgoing_requests: HashMap<SocketAddr, PeerSession>,
    /// Settings snapshot for checking banned IPs / blocked contacts.
    settings: Settings,
    /// IPs we already notified about an incoming voice call (avoid re-notifying on HELLO retries).
    /// Value: (timestamp, peer_addr, peer_ephemeral_pubkey) — stored for reject-with-hangup.
    notified_calls: HashMap<String, (Instant, SocketAddr, [u8; 32])>,
    /// Recently rejected IPs — suppress extra HELLOs for a few seconds after reject.
    rejected_ips: HashMap<String, Instant>,
    // ── IP relay state ──
    /// Cached IP announces from peers: identity pubkey → (ip, port, timestamp).
    ip_announces: HashMap<[u8; 32], (String, String, u64)>,
    /// Last time we checked our own IP for changes.
    last_ip_check: Instant,
    /// Our last announced IP (to detect changes).
    last_announced_ip: String,
    /// Cooldown per target pubkey for outgoing PEER_QUERY commands.
    peer_query_cooldowns: HashMap<[u8; 32], Instant>,
    /// Rate limiting for incoming PEER_QUERY: peer addr → (window_start, count).
    query_counts: HashMap<SocketAddr, (Instant, u32)>,
    /// Staggered connect queue for ConnectAll.
    connect_queue: VecDeque<(String, SocketAddr, [u8; 32])>,
    /// Last time a contact was popped from the connect queue.
    last_queue_pop: Instant,
    /// Our current presence status.
    our_presence: super::PresenceStatus,
    /// Last time we ran the periodic reconnect beacon.
    last_beacon: Instant,
    /// All known contacts (for periodic reconnect). Populated by ConnectAll.
    all_contacts: Vec<(String, SocketAddr, [u8; 32])>,
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
        }
    }

    /// Main daemon loop. Runs until Shutdown command.
    pub fn run(&mut self) {
        self.bind_socket();

        loop {
            if !self.process_commands() {
                break; // Shutdown
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

    fn bind_socket(&mut self) {
        let addr = format!("[::]:{}", self.local_port);
        // Retry up to 20 times (4 seconds) — voice engine may still hold the socket
        for attempt in 0..20 {
            match UdpSocket::bind(&addr) {
                Ok(s) => {
                    s.set_read_timeout(Some(RECV_TIMEOUT)).ok();
                    s.set_nonblocking(false).ok();
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

    /// Process all pending commands. Returns false when the channel is closed (app exit).
    fn process_commands(&mut self) -> bool {
        use std::sync::mpsc::TryRecvError;
        loop {
            let cmd = match self.command_rx.try_recv() {
                Ok(cmd) => cmd,
                Err(TryRecvError::Empty) => return true,
                Err(TryRecvError::Disconnected) => return false,
            };
            match cmd {
                MsgCommand::DismissIncomingCall { ip, reject } => {
                    log_fmt!("[daemon] DismissIncomingCall ip={} reject={}", ip, reject);
                    log_fmt!("[daemon]   notified_calls keys: {:?}", self.notified_calls.keys().collect::<Vec<_>>());
                    if reject {
                        // Complete voice handshake and send HANGUP to cut the caller's attempt
                        if let Some((_, peer_addr, peer_ephemeral)) = self.notified_calls.remove(&ip) {
                            log_fmt!("[daemon]   found entry, sending HELLO+HANGUP to {}", peer_addr);
                            if let Some(ref socket) = self.socket {
                                let (our_secret, our_pubkey) = crypto::generate_keypair();
                                let hello_reply = crypto::build_hello(&our_pubkey);
                                socket.send_to(&hello_reply, peer_addr).ok();
                                let session = crypto::complete_handshake(our_secret, &peer_ephemeral);
                                let hangup = session.encrypt_packet(PKT_HANGUP, &[]);
                                socket.send_to(&hangup, peer_addr).ok();
                            }
                        } else {
                            log_fmt!("[daemon]   NO entry found for ip={}", ip);
                        }
                        // Suppress extra HELLOs the caller sends after completing handshake
                        self.rejected_ips.insert(ip, Instant::now());
                    } else {
                        self.notified_calls.remove(&ip);
                    }
                    log_fmt!("[daemon]   after: notified_calls={} rejected_ips={}", self.notified_calls.len(), self.rejected_ips.len());
                }

                MsgCommand::YieldSocket => {
                    log_fmt!("[daemon] YieldSocket — releasing socket for voice call");
                    // Save connected peer info for reconnection after call
                    self.saved_peers.clear();
                    for peer in self.peers.values() {
                        if !peer.contact_id.is_empty() {
                            self.saved_peers.push((
                                peer.contact_id.clone(),
                                peer.peer_addr,
                                peer.peer_pubkey,
                            ));
                        }
                    }
                    // Disconnect all peers and drop socket for voice call
                    if let Some(ref socket) = self.socket {
                        for peer in self.peers.values() {
                            protocol::send_bye(peer, socket);
                        }
                    }
                    // Notify GUI that all peers went offline
                    for (_, peer) in &self.peers {
                        if peer.is_connected() {
                            self.event_tx.send(MsgEvent::PeerStatus {
                                contact_id: peer.contact_id.clone(),
                                online: false,
                            }).ok();
                        }
                    }
                    self.peers.clear();
                    self.contact_addrs.clear();
                    self.hello_retries.clear();
                    self.socket = None;
                }

                MsgCommand::ReclaimSocket => {
                    log_fmt!("[daemon] ReclaimSocket — rebinding socket after voice call");
                    self.bind_socket();
                    // Fresh slate after a call — allow new incoming call notifications
                    self.notified_calls.clear();
                    self.rejected_ips.clear();
                    // Reconnect all peers that were active before the call
                    let to_reconnect = std::mem::take(&mut self.saved_peers);
                    if let Some(ref socket) = self.socket {
                        for (contact_id, peer_addr, peer_pubkey) in to_reconnect {
                            if self.contact_addrs.contains_key(&contact_id) {
                                continue;
                            }
                            if let Some(session) = protocol::initiate_handshake(
                                socket, &contact_id, peer_addr, peer_pubkey,
                            ) {
                                self.contact_addrs.insert(contact_id, peer_addr);
                                self.hello_retries.insert(peer_addr, 0);
                                self.peers.insert(peer_addr, session);
                            }
                        }
                    }
                }

                MsgCommand::Connect { contact_id, peer_addr, peer_pubkey } => {
                    if self.contact_addrs.contains_key(&contact_id) {
                        continue; // Already connected or connecting
                    }
                    if let Some(ref socket) = self.socket {
                        if let Some(session) = protocol::initiate_handshake(
                            socket, &contact_id, peer_addr, peer_pubkey,
                        ) {
                            self.contact_addrs.insert(contact_id.clone(), peer_addr);
                            self.hello_retries.insert(peer_addr, 0);
                            self.peers.insert(peer_addr, session);
                        }
                    }
                    // Ensure outbox is loaded
                    if !self.outboxes.contains_key(&contact_id) {
                        self.outboxes.insert(
                            contact_id.clone(),
                            Outbox::load(&contact_id, &self.identity.secret),
                        );
                    }
                }

                MsgCommand::SendRequest { peer_addr } => {
                    // Initiate ephemeral handshake for a contact request
                    if self.outgoing_requests.contains_key(&peer_addr) {
                        continue; // Already have a pending request to this address
                    }
                    if let Some(ref socket) = self.socket {
                        let (our_secret, our_pubkey) = crate::crypto::generate_keypair();
                        let hello = crate::crypto::build_msg_hello(&our_pubkey);
                        if socket.send_to(&hello, peer_addr).is_ok() {
                            let session = PeerSession {
                                contact_id: String::new(),
                                peer_pubkey: [0u8; 32],
                                peer_nickname: String::new(),
                                peer_addr,
                                session: None,
                                last_activity: Instant::now(),
                                state: super::session::PeerState::AwaitingHello {
                                    our_secret: Some(our_secret),
                                    our_pubkey,
                                    sent_at: Instant::now(),
                                },
                                seen_seqs: std::collections::HashSet::new(),
                            };
                            self.outgoing_requests.insert(peer_addr, session);
                        } else {
                            self.event_tx.send(MsgEvent::RequestFailed {
                                peer_addr: peer_addr.to_string(),
                                reason: "Failed to send".into(),
                            }).ok();
                        }
                    } else {
                        self.event_tx.send(MsgEvent::RequestFailed {
                            peer_addr: peer_addr.to_string(),
                            reason: "Socket not available".into(),
                        }).ok();
                    }
                }

                MsgCommand::AcceptRequest { request_id } => {
                    if let Some((peer_addr, peer_id_pubkey, peer_nick, _fp)) =
                        self.pending_requests.remove(&request_id)
                    {
                        // Send REQUEST_ACK to the peer
                        // The peer session should still be in self.peers from the incoming hello
                        if let Some(peer) = self.peers.get(&peer_addr) {
                            if let Some(ref socket) = self.socket {
                                protocol::send_request_accept(
                                    peer, socket, &self.identity, &self.nickname,
                                );
                            }
                        }

                        // Save the contact locally
                        let contact_id = identity::derive_contact_id(
                            &self.identity.pubkey, &peer_id_pubkey,
                        );
                        let fingerprint = crate::crypto::fingerprint(&peer_id_pubkey);
                        let contact = identity::Contact {
                            fingerprint,
                            pubkey: peer_id_pubkey,
                            nickname: peer_nick,
                            contact_id: contact_id.clone(),
                            first_seen: identity::now_timestamp(),
                            last_seen: identity::now_timestamp(),
                            last_address: peer_addr.ip().to_string(),
                            last_port: peer_addr.port().to_string(),
                            call_count: 0,
                        };
                        identity::save_contact(&contact);

                        // Update the peer session to Connected state with identity info
                        if let Some(peer) = self.peers.get_mut(&peer_addr) {
                            peer.contact_id = contact_id.clone();
                            peer.peer_pubkey = peer_id_pubkey;
                            peer.peer_nickname = contact.nickname.clone();
                            peer.state = super::session::PeerState::Connected;
                            peer.touch();
                        }
                        self.contact_addrs.insert(contact_id.clone(), peer_addr);

                        self.event_tx.send(MsgEvent::RequestAccepted {
                            contact_id,
                        }).ok();
                    }
                }

                MsgCommand::RejectRequest { request_id } => {
                    if let Some((peer_addr, ..)) = self.pending_requests.remove(&request_id) {
                        // Send BYE and clean up session
                        if let Some(peer) = self.peers.get(&peer_addr) {
                            if let Some(ref socket) = self.socket {
                                protocol::send_bye(peer, socket);
                            }
                        }
                        self.peers.remove(&peer_addr);
                    }
                }

                MsgCommand::BlockRequest { request_id, ip } => {
                    if let Some((peer_addr, peer_id_pubkey, ..)) =
                        self.pending_requests.remove(&request_id)
                    {
                        // Ban IP and block pubkey
                        self.settings.ban_ip(&ip);
                        let hex = identity::pubkey_hex(&peer_id_pubkey);
                        self.settings.block_contact(&hex);

                        // Clean up session
                        if let Some(peer) = self.peers.get(&peer_addr) {
                            if let Some(ref socket) = self.socket {
                                protocol::send_bye(peer, socket);
                            }
                        }
                        self.peers.remove(&peer_addr);
                    }
                }

                MsgCommand::ConnectAll { contacts } => {
                    log_fmt!("[daemon] ConnectAll: queuing {} contacts", contacts.len());
                    self.all_contacts = contacts.clone();
                    for entry in contacts {
                        self.connect_queue.push_back(entry);
                    }
                }

                MsgCommand::UpdatePresence { status } => {
                    if status != self.our_presence {
                        self.our_presence = status;
                        // Broadcast presence to all connected peers
                        if let Some(ref socket) = self.socket {
                            for peer in self.peers.values() {
                                if peer.is_connected() {
                                    protocol::send_presence(peer, socket, status);
                                }
                            }
                        }
                    }
                }

                MsgCommand::QueryPeer { target_pubkey } => {
                    // Ask all connected peers for the target's current address
                    // Respect cooldown per target pubkey
                    if let Some(last) = self.peer_query_cooldowns.get(&target_pubkey) {
                        if last.elapsed() < PEER_QUERY_COOLDOWN {
                            continue;
                        }
                    }
                    self.peer_query_cooldowns.insert(target_pubkey, Instant::now());
                    if let Some(ref socket) = self.socket {
                        for peer in self.peers.values() {
                            if peer.is_connected() {
                                protocol::send_peer_query(peer, socket, &target_pubkey);
                            }
                        }
                    }
                }

                MsgCommand::SendMessage { contact_id, peer_addr, peer_pubkey, text } => {
                    // Ensure outbox exists
                    let outbox = self.outboxes.entry(contact_id.clone())
                        .or_insert_with(|| Outbox::load(&contact_id, &self.identity.secret));
                    let seq = outbox.enqueue(text.clone());

                    // If peer is connected, try to send immediately
                    if let Some(addr) = self.contact_addrs.get(&contact_id) {
                        if let Some(peer) = self.peers.get(addr) {
                            if peer.is_connected() {
                                if let Some(ref socket) = self.socket {
                                    protocol::send_chat_message(peer, socket, seq, &text).ok();
                                }
                            }
                        }
                    } else {
                        // Not connected — initiate connection
                        if let Some(ref socket) = self.socket {
                            if let Some(session) = protocol::initiate_handshake(
                                socket, &contact_id, peer_addr, peer_pubkey,
                            ) {
                                self.contact_addrs.insert(contact_id.clone(), peer_addr);
                                self.hello_retries.insert(peer_addr, 0);
                                self.peers.insert(peer_addr, session);
                            }
                        }
                    }
                }
            }
        }
    }

    fn receive_packets(&mut self) {
        let socket = match &self.socket {
            Some(s) => s,
            None => return,
        };

        let mut buf = [0u8; 1500];
        while let Ok((len, from)) = socket.recv_from(&mut buf) {
            if len == 0 {
                continue;
            }
            let data = &buf[..len];
            let pkt_type = data[0];

            match pkt_type {
                PKT_MSG_HELLO => {
                    if let Some(peer) = self.outgoing_requests.get_mut(&from) {
                        // HELLO response for an outgoing contact request
                        // Complete handshake but send REQUEST instead of IDENTITY
                        let peer_ephemeral = match crate::crypto::parse_msg_hello(data) {
                            Some(pk) => pk,
                            None => continue,
                        };
                        let our_secret = match &mut peer.state {
                            super::session::PeerState::AwaitingHello { our_secret, .. } => {
                                our_secret.take()
                            }
                            _ => continue,
                        };
                        let our_secret = match our_secret {
                            Some(s) => s,
                            None => continue,
                        };
                        let session = crate::crypto::complete_handshake(our_secret, &peer_ephemeral);
                        peer.session = Some(session);
                        peer.state = super::session::PeerState::AwaitingIdentity;
                        peer.touch();
                        // Send REQUEST (not IDENTITY)
                        protocol::send_request(peer, socket, &self.identity, &self.nickname);
                    } else if let Some(peer) = self.peers.get_mut(&from) {
                        if peer.is_connected() {
                            // Peer restarted — they sent a fresh HELLO but we still
                            // have a stale Connected session. Tear it down and accept
                            // the new handshake as an incoming connection.
                            log_fmt!("[daemon] HELLO from already-connected {} — peer restarted, resetting session", from);
                            let old_cid = peer.contact_id.clone();
                            self.event_tx.send(MsgEvent::PeerStatus {
                                contact_id: old_cid.clone(),
                                online: false,
                            }).ok();
                            self.peers.remove(&from);
                            self.contact_addrs.remove(&old_cid);
                            self.hello_retries.remove(&from);
                            // Handle as new incoming connection
                            let ip_str = from.ip().to_string();
                            if !self.settings.is_ip_banned(&ip_str) {
                                if let Some(session) = protocol::handle_incoming_hello(
                                    data, from, socket, &self.identity, &self.nickname,
                                ) {
                                    self.peers.insert(from, session);
                                }
                            }
                        } else {
                            // Pending session (AwaitingHello) — this is a hello response
                            protocol::handle_hello_response(
                                data, peer, socket, &self.identity, &self.nickname,
                            );
                        }
                    } else {
                        // Incoming connection from unknown peer
                        // Check if IP is banned before accepting
                        let ip_str = from.ip().to_string();
                        if self.settings.is_ip_banned(&ip_str) {
                            continue;
                        }
                        if let Some(session) = protocol::handle_incoming_hello(
                            data, from, socket, &self.identity, &self.nickname,
                        ) {
                            self.peers.insert(from, session);
                        }
                    }
                }

                PKT_MSG_IDENTITY => {
                    if let Some(peer) = self.peers.get_mut(&from) {
                        if protocol::handle_identity(data, peer, &self.identity).is_ok() {
                            let contact_id = peer.contact_id.clone();
                            self.contact_addrs.insert(contact_id.clone(), from);

                            // Notify GUI peer is online
                            self.event_tx.send(MsgEvent::PeerStatus {
                                contact_id: contact_id.clone(),
                                online: true,
                            }).ok();

                            // Load outbox and flush pending messages
                            let outbox = self.outboxes.entry(contact_id.clone())
                                .or_insert_with(|| Outbox::load(&contact_id, &self.identity.secret));
                            for msg in &mut outbox.messages {
                                protocol::send_chat_message(peer, socket, msg.seq, &msg.text).ok();
                                msg.attempts += 1;
                            }
                            // Reset retry state
                            self.retry_state.insert(contact_id, (Instant::now(), 0));
                            // Send our presence to the newly connected peer
                            protocol::send_presence(peer, socket, self.our_presence);
                        }
                    }
                }

                PKT_MSG_CHAT => {
                    if let Some(peer) = self.peers.get_mut(&from) {
                        if let Some((seq, text)) = protocol::handle_chat(data, peer) {
                            peer.touch();
                            // Always ACK (so sender stops retrying)
                            protocol::send_ack(peer, socket, seq);

                            // Only deliver to GUI if not a duplicate
                            if peer.seen_seqs.insert(seq) {
                                self.event_tx.send(MsgEvent::IncomingMessage {
                                    contact_id: peer.contact_id.clone(),
                                    text,
                                }).ok();
                            }
                        }
                    }
                }

                PKT_MSG_ACK => {
                    if let Some(peer) = self.peers.get_mut(&from) {
                        if let Some(seq) = protocol::handle_ack(data, peer) {
                            peer.touch();
                            let cid = peer.contact_id.clone();
                            if let Some(outbox) = self.outboxes.get_mut(&cid) {
                                outbox.remove_acked(seq);
                            }
                            self.event_tx.send(MsgEvent::MessageDelivered).ok();
                        }
                    }
                }

                PKT_MSG_BYE => {
                    if let Some(peer) = self.peers.remove(&from) {
                        if peer.is_connected() {
                            self.event_tx.send(MsgEvent::PeerStatus {
                                contact_id: peer.contact_id.clone(),
                                online: false,
                            }).ok();
                        }
                        self.contact_addrs.retain(|_, addr| *addr != from);
                        self.hello_retries.remove(&from);
                    }
                    // Also clean up outgoing requests
                    self.outgoing_requests.remove(&from);
                }

                PKT_MSG_REQUEST => {
                    // Incoming contact request from a peer we already have a session with
                    if let Some(peer) = self.peers.get_mut(&from) {
                        if let Some((pubkey, nickname)) = protocol::handle_request(data, peer) {
                            let ip_str = from.ip().to_string();
                            let hex = identity::pubkey_hex(&pubkey);

                            // Check if blocked/banned
                            if self.settings.is_ip_banned(&ip_str) || self.settings.is_blocked(&hex) {
                                // Silently discard
                                continue;
                            }

                            let fingerprint = crate::crypto::fingerprint(&pubkey);
                            let request_id = from.to_string();

                            // Check if we already have this contact
                            if identity::load_contact(&pubkey).is_some() {
                                // Already a contact — auto-accept so the other side completes too
                                if let Some(ref socket) = self.socket {
                                    protocol::send_request_accept(
                                        peer, socket, &self.identity, &self.nickname,
                                    );
                                }
                                continue;
                            }

                            // Store as pending
                            self.pending_requests.insert(
                                request_id.clone(),
                                (from, pubkey, nickname.clone(), fingerprint.clone()),
                            );

                            self.event_tx.send(MsgEvent::IncomingRequest {
                                request_id,
                                nickname,
                                ip: ip_str,
                                fingerprint,
                            }).ok();
                        }
                    }
                }

                PKT_MSG_REQUEST_ACK => {
                    // Our contact request was accepted
                    if let Some(peer) = self.outgoing_requests.get(&from) {
                        if let Some((pubkey, nickname)) = protocol::handle_request_accept(data, peer) {
                            let contact_id = identity::derive_contact_id(
                                &self.identity.pubkey, &pubkey,
                            );
                            let fingerprint = crate::crypto::fingerprint(&pubkey);

                            // Save the new contact
                            let contact = identity::Contact {
                                fingerprint,
                                pubkey,
                                nickname,
                                contact_id: contact_id.clone(),
                                first_seen: identity::now_timestamp(),
                                last_seen: identity::now_timestamp(),
                                last_address: from.ip().to_string(),
                                last_port: from.port().to_string(),
                                call_count: 0,
                            };
                            identity::save_contact(&contact);

                            // Move session from outgoing_requests to regular peers
                            if let Some(mut peer_session) = self.outgoing_requests.remove(&from) {
                                peer_session.contact_id = contact_id.clone();
                                peer_session.peer_pubkey = pubkey;
                                peer_session.peer_nickname = contact.nickname.clone();
                                peer_session.state = super::session::PeerState::Connected;
                                peer_session.touch();
                                self.contact_addrs.insert(contact_id.clone(), from);
                                self.peers.insert(from, peer_session);
                            }

                            self.event_tx.send(MsgEvent::RequestAccepted {
                                contact_id,
                            }).ok();
                        }
                    }
                }

                PKT_MSG_IP_ANNOUNCE => {
                    if let Some(peer) = self.peers.get_mut(&from) {
                        if let Some((ip, port, timestamp)) = protocol::handle_ip_announce(data, peer) {
                            peer.touch();
                            let peer_pubkey = peer.peer_pubkey;
                            let contact_id = peer.contact_id.clone();
                            // Only accept if timestamp is newer than what we have
                            let dominated = self.ip_announces.get(&peer_pubkey)
                                .map(|(_, _, old_ts)| timestamp <= *old_ts)
                                .unwrap_or(false);
                            if !dominated {
                                log_fmt!("[daemon] IP_ANNOUNCE from {} => ip={} port={} ts={}", from, ip, port, timestamp);
                                self.ip_announces.insert(peer_pubkey, (ip.clone(), port.clone(), timestamp));
                                // Update contact on disk
                                if let Some(mut contact) = identity::load_contact(&peer_pubkey) {
                                    contact.last_address = ip.clone();
                                    contact.last_port = port.clone();
                                    contact.last_seen = identity::now_timestamp();
                                    identity::save_contact(&contact);
                                }
                                if !contact_id.is_empty() {
                                    self.event_tx.send(MsgEvent::PeerAddressUpdate {
                                        contact_id,
                                        ip,
                                        port,
                                    }).ok();
                                }
                            }
                        }
                    }
                }

                PKT_MSG_PEER_QUERY => {
                    if let Some(peer) = self.peers.get_mut(&from) {
                        if let Some(target_pubkey) = protocol::handle_peer_query(data, peer) {
                            peer.touch();
                            // Rate limit: max QUERY_RATE_MAX queries per QUERY_RATE_WINDOW per peer
                            let (window_start, count) = self.query_counts.entry(from)
                                .or_insert((Instant::now(), 0));
                            if window_start.elapsed() > QUERY_RATE_WINDOW {
                                *window_start = Instant::now();
                                *count = 0;
                            }
                            *count += 1;
                            if *count > QUERY_RATE_MAX {
                                continue; // Rate limited
                            }
                            // Only respond if target is our contact AND we have a recent announce
                            if identity::load_contact(&target_pubkey).is_some() {
                                if let Some((ip, port, ts)) = self.ip_announces.get(&target_pubkey) {
                                    let age = std::time::SystemTime::now()
                                        .duration_since(std::time::UNIX_EPOCH)
                                        .unwrap()
                                        .as_secs()
                                        .saturating_sub(*ts);
                                    if age < ANNOUNCE_MAX_AGE.as_secs() {
                                        if let Some(ref socket) = self.socket {
                                            log_fmt!("[daemon] PEER_QUERY from {} for target => responding with ip={} port={}", from, ip, port);
                                            protocol::send_peer_response(
                                                peer, socket, &target_pubkey,
                                                ip, port, *ts,
                                            );
                                        }
                                    }
                                }
                            }
                        }
                    }
                }

                PKT_MSG_PEER_RESPONSE => {
                    if let Some(peer) = self.peers.get_mut(&from) {
                        if let Some((target_pubkey, ip, port, timestamp)) = protocol::handle_peer_response(data, peer) {
                            peer.touch();
                            log_fmt!("[daemon] PEER_RESPONSE from {} => target ip={} port={} ts={}", from, ip, port, timestamp);
                            // Update the target contact's address
                            if let Some(mut contact) = identity::load_contact(&target_pubkey) {
                                contact.last_address = ip.clone();
                                contact.last_port = port.clone();
                                contact.last_seen = identity::now_timestamp();
                                identity::save_contact(&contact);
                                // Also cache the announce
                                self.ip_announces.insert(target_pubkey, (ip.clone(), port.clone(), timestamp));
                                self.event_tx.send(MsgEvent::PeerAddressUpdate {
                                    contact_id: contact.contact_id,
                                    ip,
                                    port,
                                }).ok();
                            }
                        }
                    }
                }

                PKT_MSG_PRESENCE => {
                    if let Some(peer) = self.peers.get_mut(&from) {
                        if let Some(status) = protocol::handle_presence(data, peer) {
                            peer.touch();
                            if !peer.contact_id.is_empty() {
                                self.event_tx.send(MsgEvent::PresenceUpdate {
                                    contact_id: peer.contact_id.clone(),
                                    status,
                                }).ok();
                            }
                        }
                    }
                }

                PKT_HELLO => {
                    // Voice HELLO (0x01) — detect incoming calls from known contacts
                    let ip_str = from.ip().to_string();
                    log_fmt!("[daemon] PKT_HELLO from {} (ip={})", from, ip_str);
                    if self.settings.is_ip_banned(&ip_str) {
                        log_fmt!("[daemon]   SKIP: ip banned");
                        continue;
                    }
                    // Suppress extra HELLOs after a recent reject (short window)
                    if let Some(t) = self.rejected_ips.get(&ip_str) {
                        if t.elapsed() < Duration::from_secs(5) {
                            log_fmt!("[daemon]   SKIP: rejected_ips cooldown ({}ms ago)", t.elapsed().as_millis());
                            continue;
                        }
                    }
                    // Parse ephemeral pubkey from HELLO
                    let peer_ephemeral = match crypto::parse_hello(data) {
                        Some(pk) => pk,
                        None => {
                            log_fmt!("[daemon]   SKIP: parse_hello failed (len={}, type=0x{:02x})", data.len(), data[0]);
                            continue;
                        }
                    };
                    let pk_short = format!("{:02x}{:02x}{:02x}{:02x}", peer_ephemeral[0], peer_ephemeral[1], peer_ephemeral[2], peer_ephemeral[3]);
                    log_fmt!("[daemon]   ephemeral_pk={}...", pk_short);
                    // Suppress retries from the SAME call (same ephemeral key).
                    // A NEW call uses a different key, so it always gets through.
                    if let Some((t, _, stored_pk)) = self.notified_calls.get(&ip_str) {
                        let stored_short = format!("{:02x}{:02x}{:02x}{:02x}", stored_pk[0], stored_pk[1], stored_pk[2], stored_pk[3]);
                        let same_key = *stored_pk == peer_ephemeral;
                        log_fmt!("[daemon]   notified_calls entry: stored_pk={}... same_key={} age={}ms",
                            stored_short, same_key, t.elapsed().as_millis());
                        if same_key && t.elapsed() < Duration::from_secs(60) {
                            log_fmt!("[daemon]   SKIP: same call retry");
                            continue;
                        }
                        log_fmt!("[daemon]   PASS: different key or expired");
                    } else {
                        log_fmt!("[daemon]   no notified_calls entry for this IP");
                    }

                    // Try to identify caller:
                    // 1. Check active messaging peers (most reliable)
                    let mut found_contact: Option<identity::Contact> = None;
                    if let Some(peer) = self.peers.get(&from) {
                        if peer.is_connected() && !peer.contact_id.is_empty() {
                            found_contact = identity::load_contact(&peer.peer_pubkey);
                            if found_contact.is_some() {
                                log_fmt!("[daemon]   matched via active messaging peer");
                            }
                        }
                    }
                    // 2. Check contacts by exact last_address
                    if found_contact.is_none() {
                        let contacts = identity::load_all_contacts();
                        found_contact = contacts.into_iter()
                            .find(|c| c.last_address == ip_str);
                        if found_contact.is_some() {
                            log_fmt!("[daemon]   matched via exact last_address");
                        }
                    }
                    // 3. Check contacts by /64 prefix match (IPv6 privacy addresses)
                    if found_contact.is_none() {
                        let contacts = identity::load_all_contacts();
                        found_contact = contacts.into_iter()
                            .find(|c| ipv6_prefix_match(&c.last_address, &ip_str));
                        if found_contact.is_some() {
                            log_fmt!("[daemon]   matched via /64 prefix");
                        }
                    }

                    if let Some(contact) = found_contact {
                        log_fmt!("[daemon]   => EMITTING IncomingCall for '{}' ({})", contact.nickname, contact.fingerprint);
                        self.notified_calls.insert(
                            ip_str.clone(),
                            (Instant::now(), from, peer_ephemeral),
                        );
                        self.event_tx.send(MsgEvent::IncomingCall {
                            nickname: contact.nickname.clone(),
                            fingerprint: contact.fingerprint.clone(),
                            ip: ip_str,
                            port: from.port().to_string(),
                        }).ok();
                    } else {
                        log_fmt!("[daemon]   NO contact match found (checked {} contacts)", identity::load_all_contacts().len());
                    }
                }

                _ => {} // Ignore unknown packet types
            }

            // Only process one packet per frame to keep latency low
            break;
        }
    }

    fn housekeep(&mut self) {
        let now = Instant::now();

        // Process staggered connect queue (1 contact every 100ms)
        if !self.connect_queue.is_empty()
            && now.duration_since(self.last_queue_pop) >= Duration::from_millis(100)
        {
            if let Some((contact_id, peer_addr, peer_pubkey)) = self.connect_queue.pop_front() {
                self.last_queue_pop = now;
                if !self.contact_addrs.contains_key(&contact_id) {
                    if let Some(ref socket) = self.socket {
                        if let Some(session) = protocol::initiate_handshake(
                            socket, &contact_id, peer_addr, peer_pubkey,
                        ) {
                            log_fmt!("[daemon] auto-connect: {} -> {}", contact_id, peer_addr);
                            self.contact_addrs.insert(contact_id.clone(), peer_addr);
                            self.hello_retries.insert(peer_addr, 0);
                            self.peers.insert(peer_addr, session);
                        }
                    }
                    // Ensure outbox is loaded
                    if !self.outboxes.contains_key(&contact_id) {
                        self.outboxes.insert(
                            contact_id.clone(),
                            Outbox::load(&contact_id, &self.identity.secret),
                        );
                    }
                }
            }
        }

        // Clean up expired incoming call notifications
        self.notified_calls.retain(|_, (t, ..)| t.elapsed() < Duration::from_secs(60));
        self.rejected_ips.retain(|_, t| t.elapsed() < Duration::from_secs(5));
        // Clean up expired query rate counters
        self.query_counts.retain(|_, (t, _)| t.elapsed() < QUERY_RATE_WINDOW * 2);
        // Clean up expired query cooldowns
        self.peer_query_cooldowns.retain(|_, t| t.elapsed() < PEER_QUERY_COOLDOWN);

        // IP change detection and announce (every IP_CHECK_INTERVAL)
        if self.last_ip_check.elapsed() >= IP_CHECK_INTERVAL {
            self.last_ip_check = now;
            let current_ip = crate::gui::get_best_ipv6(&self.settings.network_adapter);
            if current_ip != "::1" && current_ip != self.last_announced_ip {
                log_fmt!("[daemon] IP changed: '{}' -> '{}'", self.last_announced_ip, current_ip);
                self.last_announced_ip = current_ip.clone();
                let timestamp = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_secs();
                // Broadcast IP_ANNOUNCE to all connected peers
                if let Some(ref socket) = self.socket {
                    for peer in self.peers.values() {
                        if peer.is_connected() {
                            protocol::send_ip_announce(
                                peer, socket,
                                &self.last_announced_ip, &self.local_port,
                                timestamp,
                            );
                        }
                    }
                }
            }
        }

        // Periodic beacon: re-queue disconnected contacts every BEACON_INTERVAL
        // Reload contacts from disk to pick up IP changes from IP_ANNOUNCE / PEER_RESPONSE
        if now.duration_since(self.last_beacon) >= BEACON_INTERVAL {
            self.last_beacon = now;
            let fresh_contacts = identity::load_all_contacts();
            self.all_contacts = fresh_contacts.iter()
                .filter_map(|c| {
                    if c.last_address.is_empty() || c.last_port.is_empty() {
                        return None;
                    }
                    let addr_str = format!("[{}]:{}", c.last_address, c.last_port);
                    let addr: std::net::SocketAddr = addr_str.parse().ok()?;
                    Some((c.contact_id.clone(), addr, c.pubkey))
                })
                .collect();
            let mut queued = 0;
            for (cid, addr, pk) in &self.all_contacts {
                if !self.contact_addrs.contains_key(cid) {
                    self.connect_queue.push_back((cid.clone(), *addr, *pk));
                    queued += 1;
                }
            }
            if queued > 0 {
                log_fmt!("[daemon] beacon: re-queued {} disconnected contacts (refreshed from disk)", queued);
            }
        }

        // Keepalives
        if now.duration_since(self.last_keepalive) > KEEPALIVE_INTERVAL {
            self.last_keepalive = now;
            if let Some(ref socket) = self.socket {
                for peer in self.peers.values() {
                    if peer.is_connected() {
                        protocol::send_keepalive(peer, socket);
                    }
                }
            }
        }

        // Timeout disconnected peers
        let timed_out: Vec<SocketAddr> = self.peers.iter()
            .filter(|(_, p)| p.is_timed_out(PEER_TIMEOUT))
            .map(|(addr, _)| *addr)
            .collect();

        for addr in timed_out {
            if let Some(peer) = self.peers.remove(&addr) {
                if peer.is_connected() {
                    self.event_tx.send(MsgEvent::PeerStatus {
                        contact_id: peer.contact_id.clone(),
                        online: false,
                    }).ok();
                }
                self.contact_addrs.retain(|_, a| *a != addr);
                self.hello_retries.remove(&addr);
            }
        }

        // Retry HELLO for peers in AwaitingHello state
        if let Some(ref socket) = self.socket {
            let mut to_remove = Vec::new();
            for (addr, peer) in &self.peers {
                if let super::session::PeerState::AwaitingHello { our_pubkey, sent_at, .. } = &peer.state {
                    if sent_at.elapsed() > HELLO_RETRY_INTERVAL {
                        let retries = self.hello_retries.get(addr).copied().unwrap_or(0);
                        if retries >= HELLO_MAX_RETRIES {
                            to_remove.push(*addr);
                        } else {
                            let hello = crate::crypto::build_msg_hello(our_pubkey);
                            socket.send_to(&hello, addr).ok();
                            // We can't mutate peer here, but we'll update retry count
                        }
                    }
                }
            }
            // Update retry counts
            for (addr, peer) in &mut self.peers {
                if let super::session::PeerState::AwaitingHello { sent_at, .. } = &mut peer.state {
                    if sent_at.elapsed() > HELLO_RETRY_INTERVAL {
                        *sent_at = Instant::now();
                        *self.hello_retries.entry(*addr).or_insert(0) += 1;
                    }
                }
            }
            // Remove peers that exceeded retries
            for addr in to_remove {
                self.peers.remove(&addr);
                self.contact_addrs.retain(|_, a| *a != addr);
                self.hello_retries.remove(&addr);
            }
        }

        // Timeout outgoing requests (reuse PEER_TIMEOUT)
        let timed_out_reqs: Vec<SocketAddr> = self.outgoing_requests.iter()
            .filter(|(_, p)| p.is_timed_out(PEER_TIMEOUT))
            .map(|(addr, _)| *addr)
            .collect();
        for addr in timed_out_reqs {
            self.outgoing_requests.remove(&addr);
            self.event_tx.send(MsgEvent::RequestFailed {
                peer_addr: addr.to_string(),
                reason: "Timed out".into(),
            }).ok();
        }

        // Retry HELLO for outgoing requests in AwaitingHello state
        if let Some(ref socket) = self.socket {
            let mut req_remove = Vec::new();
            for (addr, peer) in &mut self.outgoing_requests {
                if let super::session::PeerState::AwaitingHello { our_pubkey, sent_at, .. } = &mut peer.state {
                    if sent_at.elapsed() > HELLO_RETRY_INTERVAL {
                        let retries = self.hello_retries.get(addr).copied().unwrap_or(0);
                        if retries >= HELLO_MAX_RETRIES {
                            req_remove.push(*addr);
                        } else {
                            let hello = crate::crypto::build_msg_hello(our_pubkey);
                            socket.send_to(&hello, addr).ok();
                            *sent_at = Instant::now();
                            *self.hello_retries.entry(*addr).or_insert(0) += 1;
                        }
                    }
                }
            }
            for addr in req_remove {
                self.outgoing_requests.remove(&addr);
                self.hello_retries.remove(&addr);
                self.event_tx.send(MsgEvent::RequestFailed {
                    peer_addr: addr.to_string(),
                    reason: "Peer unreachable".into(),
                }).ok();
            }
        }

        // Retry outbox messages for connected peers with backoff
        if let Some(ref socket) = self.socket {
            for (contact_id, outbox) in &mut self.outboxes {
                if !outbox.has_pending() {
                    continue;
                }
                let (last_retry, tier) = self.retry_state.get(contact_id)
                    .cloned()
                    .unwrap_or((Instant::now() - Duration::from_secs(999), 0));

                let backoff = RETRY_BACKOFFS.get(tier).copied()
                    .unwrap_or(*RETRY_BACKOFFS.last().unwrap());

                if now.duration_since(last_retry) < backoff {
                    continue;
                }

                if let Some(addr) = self.contact_addrs.get(contact_id) {
                    if let Some(peer) = self.peers.get(addr) {
                        if peer.is_connected() {
                            for msg in &mut outbox.messages {
                                protocol::send_chat_message(peer, socket, msg.seq, &msg.text).ok();
                                msg.attempts += 1;
                            }
                            let next_tier = (tier + 1).min(RETRY_BACKOFFS.len() - 1);
                            self.retry_state.insert(contact_id.clone(), (now, next_tier));
                        }
                    }
                }
            }
        }
    }
}

/// Check if two IPv6 addresses share the same /64 prefix.
/// This handles IPv6 privacy extensions where the host part changes
/// but the network prefix stays the same.
fn ipv6_prefix_match(a: &str, b: &str) -> bool {
    use std::net::Ipv6Addr;
    let a_addr: Ipv6Addr = match a.parse() {
        Ok(addr) => addr,
        Err(_) => return false,
    };
    let b_addr: Ipv6Addr = match b.parse() {
        Ok(addr) => addr,
        Err(_) => return false,
    };
    let a_segs = a_addr.segments();
    let b_segs = b_addr.segments();
    // Compare first 4 segments (64 bits = /64 prefix)
    a_segs[0] == b_segs[0] && a_segs[1] == b_segs[1]
        && a_segs[2] == b_segs[2] && a_segs[3] == b_segs[3]
}
