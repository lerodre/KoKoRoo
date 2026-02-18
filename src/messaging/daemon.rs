use std::collections::HashMap;
use std::net::{SocketAddr, UdpSocket};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use crate::crypto::{PKT_MSG_ACK, PKT_MSG_BYE, PKT_MSG_CHAT, PKT_MSG_HELLO, PKT_MSG_IDENTITY};
use crate::identity::Identity;

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

                MsgCommand::YieldSocket => {
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
                    // Reconnect peers with pending outbox messages
                    // Peers with pending messages will be reconnected when the GUI
                    // re-issues Connect commands (the GUI holds contact info).
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
                    if let Some(peer) = self.peers.get_mut(&from) {
                        // We have a pending session — this is a hello response
                        protocol::handle_hello_response(
                            data, peer, socket, &self.identity, &self.nickname,
                        );
                    } else {
                        // Incoming connection from unknown peer
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
                }

                _ => {} // Ignore voice packets or unknown types
            }

            // Only process one packet per frame to keep latency low
            break;
        }
    }

    fn housekeep(&mut self) {
        let now = Instant::now();

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
