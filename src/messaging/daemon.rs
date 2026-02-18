use std::collections::HashMap;
use std::net::{SocketAddr, UdpSocket};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use crate::crypto::{
    self, PKT_HELLO, PKT_HANGUP,
    PKT_MSG_ACK, PKT_MSG_BYE, PKT_MSG_CHAT, PKT_MSG_HELLO, PKT_MSG_IDENTITY,
    PKT_MSG_REQUEST, PKT_MSG_REQUEST_ACK,
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
        match UdpSocket::bind(&addr) {
            Ok(s) => {
                s.set_read_timeout(Some(RECV_TIMEOUT)).ok();
                s.set_nonblocking(false).ok();
                self.socket = Some(s);
            }
            Err(e) => {
                eprintln!("[msg-daemon] Failed to bind {}: {}", addr, e);
            }
        }
    }

    /// Process all pending commands. Returns false on Shutdown.
    fn process_commands(&mut self) -> bool {
        while let Ok(cmd) = self.command_rx.try_recv() {
            match cmd {
                MsgCommand::Shutdown => return false,

                MsgCommand::DismissIncomingCall { ip, reject } => {
                    if reject {
                        // Complete voice handshake and send HANGUP to cut the caller's attempt
                        if let Some((_, peer_addr, peer_ephemeral)) = self.notified_calls.remove(&ip) {
                            if let Some(ref socket) = self.socket {
                                let (our_secret, our_pubkey) = crypto::generate_keypair();
                                let hello_reply = crypto::build_hello(&our_pubkey);
                                socket.send_to(&hello_reply, peer_addr).ok();
                                let session = crypto::complete_handshake(our_secret, &peer_ephemeral);
                                let hangup = session.encrypt_packet(PKT_HANGUP, &[]);
                                socket.send_to(&hangup, peer_addr).ok();
                            }
                        }
                        // Suppress extra HELLOs the caller sends after completing handshake
                        // (voice.rs sends ~10 extra HELLOs). Short TTL so new calls still work.
                        self.rejected_ips.insert(ip, Instant::now());
                    } else {
                        self.notified_calls.remove(&ip);
                    }
                }

                MsgCommand::YieldSocket => {
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
                    self.bind_socket();
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
        true
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
                        peer.state = super::session::PeerState::AwaitingIdentity {
                            sent_at: Instant::now(),
                        };
                        peer.touch();
                        // Send REQUEST (not IDENTITY)
                        protocol::send_request(peer, socket, &self.identity, &self.nickname);
                    } else if let Some(peer) = self.peers.get_mut(&from) {
                        // We have a pending session — this is a hello response
                        protocol::handle_hello_response(
                            data, peer, socket, &self.identity, &self.nickname,
                        );
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
                        }
                    }
                }

                PKT_MSG_CHAT => {
                    if let Some(peer) = self.peers.get_mut(&from) {
                        if let Some((seq, text)) = protocol::handle_chat(data, peer) {
                            peer.touch();
                            // Send ACK
                            protocol::send_ack(peer, socket, seq);

                            let timestamp = std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .unwrap()
                                .as_secs();

                            self.event_tx.send(MsgEvent::IncomingMessage {
                                contact_id: peer.contact_id.clone(),
                                text,
                                timestamp,
                            }).ok();
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
                            self.event_tx.send(MsgEvent::MessageDelivered {
                                contact_id: cid,
                                seq,
                            }).ok();
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
                                // Already a contact, skip
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

                PKT_HELLO => {
                    // Voice HELLO (0x01) — detect incoming calls from known contacts
                    let ip_str = from.ip().to_string();
                    if self.settings.is_ip_banned(&ip_str) {
                        continue;
                    }
                    // Suppress extra HELLOs after a recent reject (short window)
                    if let Some(t) = self.rejected_ips.get(&ip_str) {
                        if t.elapsed() < Duration::from_secs(5) {
                            continue;
                        }
                    }
                    // Only notify once per caller (expires after 60s to allow re-calls)
                    if let Some((t, ..)) = self.notified_calls.get(&ip_str) {
                        if t.elapsed() < Duration::from_secs(60) {
                            continue;
                        }
                    }
                    // Parse ephemeral pubkey from HELLO
                    let peer_ephemeral = match crypto::parse_hello(data) {
                        Some(pk) => pk,
                        None => continue,
                    };

                    // Try to identify caller:
                    // 1. Check active messaging peers (most reliable)
                    let mut found_contact: Option<identity::Contact> = None;
                    if let Some(peer) = self.peers.get(&from) {
                        if peer.is_connected() && !peer.contact_id.is_empty() {
                            found_contact = identity::load_contact(&peer.peer_pubkey);
                        }
                    }
                    // 2. Check contacts by exact last_address
                    if found_contact.is_none() {
                        let contacts = identity::load_all_contacts();
                        found_contact = contacts.into_iter()
                            .find(|c| c.last_address == ip_str);
                    }
                    // 3. Check contacts by /64 prefix match (IPv6 privacy addresses)
                    if found_contact.is_none() {
                        let contacts = identity::load_all_contacts();
                        found_contact = contacts.into_iter()
                            .find(|c| ipv6_prefix_match(&c.last_address, &ip_str));
                    }

                    if let Some(contact) = found_contact {
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

        // Clean up expired incoming call notifications
        self.notified_calls.retain(|_, (t, ..)| t.elapsed() < Duration::from_secs(60));
        self.rejected_ips.retain(|_, t| t.elapsed() < Duration::from_secs(5));

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
