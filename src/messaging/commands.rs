use std::collections::HashSet;
use std::time::Instant;

use crate::crypto::{self, PKT_HANGUP};
use crate::identity::{self};
use crate::filetransfer;
use crate::filetransfer::receiver::ReceiverState;

use super::outbox::Outbox;
use super::protocol;
use super::session::PeerSession;
use super::{MsgCommand, MsgEvent};
use super::daemon::{MsgDaemon, FileTransfer};

impl MsgDaemon {
    /// Process all pending commands. Returns false when the channel is closed (app exit).
    pub(crate) fn process_commands(&mut self) -> bool {
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
                                let (session, _) = crypto::complete_handshake(our_secret, &peer_ephemeral);
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

                MsgCommand::SendGroupInvite { contact_id, peer_addr, peer_pubkey, invite_json, members } => {
                    log_fmt!("[daemon] SendGroupInvite to contact={} addr={}", contact_id, peer_addr);
                    // Extract group_id from invite JSON for pending store
                    let group_id = serde_json::from_slice::<serde_json::Value>(&invite_json)
                        .ok()
                        .and_then(|v| v.get("group_id").and_then(|g| g.as_str().map(|s| s.to_string())))
                        .unwrap_or_default();
                    // Store members for sync when ACK arrives
                    if !group_id.is_empty() {
                        self.pending_member_syncs.insert(
                            (contact_id.clone(), group_id.clone()),
                            members,
                        );
                    }
                    let mut sent = false;
                    // Find connected peer session — send immediately if possible
                    if let Some(addr) = self.contact_addrs.get(&contact_id) {
                        if let Some(peer) = self.peers.get(addr) {
                            if peer.is_connected() {
                                if let Some(ref socket) = self.socket {
                                    protocol::send_group_invite(peer, socket, &invite_json).ok();
                                    log_fmt!("[daemon]   lite invite sent ({} bytes)", invite_json.len());
                                    sent = true;
                                }
                            }
                        }
                    }
                    if !sent {
                        log_fmt!("[daemon]   peer offline, enqueuing invite for group={}", group_id);
                        let store = self.pending_invites.entry(contact_id.clone())
                            .or_insert_with(|| super::pending_invites::PendingInviteStore::load(&contact_id, &self.identity.secret));
                        store.enqueue(group_id, invite_json);
                        // Initiate handshake so invite is sent reactively on connect
                        if !self.contact_addrs.contains_key(&contact_id) {
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

                MsgCommand::AcceptGroupInvite { contact_id, group_id } => {
                    log_fmt!("[daemon] AcceptGroupInvite contact={} group={}", contact_id, group_id);
                    if let Some(addr) = self.contact_addrs.get(&contact_id) {
                        if let Some(peer) = self.peers.get(addr) {
                            if peer.is_connected() {
                                if let Some(ref socket) = self.socket {
                                    protocol::send_group_invite_ack(peer, socket, &group_id).ok();
                                }
                            }
                        }
                    }
                }

                MsgCommand::RejectGroupInvite { contact_id, group_id } => {
                    log_fmt!("[daemon] RejectGroupInvite contact={} group={}", contact_id, group_id);
                    if let Some(addr) = self.contact_addrs.get(&contact_id) {
                        if let Some(peer) = self.peers.get(addr) {
                            if peer.is_connected() {
                                if let Some(ref socket) = self.socket {
                                    protocol::send_group_invite_nack(peer, socket, &group_id).ok();
                                }
                            }
                        }
                    }
                }

                MsgCommand::SendGroupChat { contact_id, peer_addr: _, peer_pubkey: _, group_id, channel_id, text } => {
                    if let Some(addr) = self.contact_addrs.get(&contact_id) {
                        if let Some(peer) = self.peers.get(addr) {
                            if peer.is_connected() {
                                if let Some(ref socket) = self.socket {
                                    protocol::send_group_chat(peer, socket, &group_id, &channel_id, &text).ok();
                                }
                            }
                        }
                    }
                }

                MsgCommand::YieldSocket => {
                    log_fmt!("[daemon] YieldSocket — releasing socket for voice call");
                    // Cancel all active file transfers
                    for ((cid, tid), mut ft) in self.file_transfers.drain() {
                        if let FileTransfer::Receiving(ref mut recv) = ft {
                            recv.cleanup();
                        }
                        self.event_tx.send(MsgEvent::FileTransferFailed {
                            contact_id: cid,
                            transfer_id: tid,
                            reason: "Voice call started".into(),
                        }).ok();
                    }
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
                                seen_seqs: HashSet::new(),
                                ephemeral_shared: None,
                                upgraded_session: None,
                                identity_confirmed: false,
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
                        if last.elapsed() < super::daemon::PEER_QUERY_COOLDOWN {
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

                MsgCommand::SendFileOffer { contact_id, peer_addr, peer_pubkey, file_path } => {
                    let filename_log = std::path::Path::new(&file_path)
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_else(|| "file".to_string());
                    // Compute SHA-256 and file size
                    let metadata = match std::fs::metadata(&file_path) {
                        Ok(m) => m,
                        Err(e) => {
                            self.event_tx.send(MsgEvent::FileTransferFailed {
                                contact_id: contact_id.clone(),
                                transfer_id: 0,
                                reason: format!("Cannot read file: {e}"),
                            }).ok();
                            continue;
                        }
                    };
                    let file_size = metadata.len();
                    log_fmt!("[daemon] SendFileOffer: file='{}' size={} to={}", filename_log, file_size, contact_id);
                    let filename = std::path::Path::new(&file_path)
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_else(|| "file".to_string());

                    // Compute SHA-256
                    let sha256 = match compute_file_sha256(&file_path) {
                        Some(h) => h,
                        None => {
                            self.event_tx.send(MsgEvent::FileTransferFailed {
                                contact_id: contact_id.clone(),
                                transfer_id: 0,
                                reason: "Failed to hash file".into(),
                            }).ok();
                            continue;
                        }
                    };

                    let transfer_id = self.next_transfer_id;
                    self.next_transfer_id += 1;

                    // Ensure connected
                    if let Some(addr) = self.contact_addrs.get(&contact_id) {
                        if let Some(peer) = self.peers.get(addr) {
                            if peer.is_connected() {
                                if let Some(ref socket) = self.socket {
                                    filetransfer::protocol::send_file_offer(
                                        peer, socket, transfer_id, file_size, &sha256, &filename,
                                    );
                                }
                            }
                        }
                    } else {
                        // Try to connect first
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

                    // Store as waiting for accept
                    self.file_transfers.insert(
                        (contact_id.clone(), transfer_id),
                        FileTransfer::OfferedWaiting {
                            file_path,
                            file_size,
                            sha256,
                            offered_at: Instant::now(),
                        },
                    );
                }

                MsgCommand::AcceptFileTransfer { contact_id, transfer_id } => {
                    log_fmt!("[daemon] AcceptFileTransfer: id={} from={}", transfer_id, contact_id);
                    let key = (contact_id.clone(), transfer_id);
                    if let Some(ft) = self.file_transfers.remove(&key) {
                        if let FileTransfer::IncomingWaiting { transfer_id, contact_id, filename, file_size, sha256 } = ft {
                            // Send FILE_ACCEPT to peer
                            if let Some(addr) = self.contact_addrs.get(&contact_id) {
                                if let Some(peer) = self.peers.get(addr) {
                                    if let Some(ref socket) = self.socket {
                                        filetransfer::protocol::send_file_accept(peer, socket, transfer_id);
                                    }
                                }
                            }
                            // Create receiver state
                            let receiver = ReceiverState::new(
                                transfer_id, filename, file_size, sha256, contact_id.clone(),
                            );
                            self.file_transfers.insert(
                                (contact_id, transfer_id),
                                FileTransfer::Receiving(receiver),
                            );
                        }
                    }
                }

                MsgCommand::RejectFileTransfer { contact_id, transfer_id } => {
                    log_fmt!("[daemon] RejectFileTransfer: id={} from={}", transfer_id, contact_id);
                    let key = (contact_id.clone(), transfer_id);
                    if let Some(ft) = self.file_transfers.remove(&key) {
                        if let FileTransfer::IncomingWaiting { transfer_id, contact_id, .. } = ft {
                            if let Some(addr) = self.contact_addrs.get(&contact_id) {
                                if let Some(peer) = self.peers.get(addr) {
                                    if let Some(ref socket) = self.socket {
                                        filetransfer::protocol::send_file_reject(peer, socket, transfer_id);
                                    }
                                }
                            }
                        }
                    }
                }

                MsgCommand::CancelFileTransfer { contact_id, transfer_id } => {
                    log_fmt!("[daemon] CancelFileTransfer: id={}", transfer_id);
                    let key = (contact_id.clone(), transfer_id);
                    if let Some(mut ft) = self.file_transfers.remove(&key) {
                        // Send cancel to peer
                        if let Some(addr) = self.contact_addrs.get(&contact_id) {
                            if let Some(peer) = self.peers.get(addr) {
                                if let Some(ref socket) = self.socket {
                                    filetransfer::protocol::send_file_cancel(
                                        peer, socket, transfer_id, filetransfer::CANCEL_USER,
                                    );
                                }
                            }
                        }
                        // Clean up receiver temp file if applicable
                        if let FileTransfer::Receiving(ref mut recv) = ft {
                            recv.cleanup();
                        }
                        self.event_tx.send(MsgEvent::FileTransferFailed {
                            contact_id,
                            transfer_id,
                            reason: "Cancelled".into(),
                        }).ok();
                    }
                }

                MsgCommand::BroadcastAvatar { avatar_data, sha256 } => {
                    // Queue avatar send for all connected + identity-confirmed peers
                    for peer in self.peers.values() {
                        if peer.is_connected() && peer.identity_confirmed && !peer.contact_id.is_empty() {
                            self.avatar_sends.insert(peer.contact_id.clone(), super::daemon::AvatarSendState {
                                avatar_data: avatar_data.clone(),
                                sha256,
                                sent: false,
                                sent_at: Instant::now(),
                                retries: 0,
                            });
                        }
                    }
                }

                MsgCommand::SendAvatarTo { contact_id, avatar_data, sha256 } => {
                    self.avatar_sends.insert(contact_id, super::daemon::AvatarSendState {
                        avatar_data,
                        sha256,
                        sent: false,
                        sent_at: Instant::now(),
                        retries: 0,
                    });
                }

                MsgCommand::SendGroupUpdate { group_id, group_json, member_contacts } => {
                    log_fmt!("[daemon] SendGroupUpdate group={}", group_id);
                    if let Some(ref socket) = self.socket {
                        for (contact_id, peer_addr, peer_pubkey) in &member_contacts {
                            let mut sent = false;
                            if let Some(addr) = self.contact_addrs.get(contact_id) {
                                if let Some(peer) = self.peers.get(addr) {
                                    if peer.is_connected() {
                                        protocol::send_group_update(peer, socket, &group_json).ok();
                                        sent = true;
                                    }
                                }
                            }
                            if !sent {
                                // Peer offline — initiate handshake so update is sent on connect
                                if !self.contact_addrs.contains_key(contact_id) {
                                    if let Some(session) = protocol::initiate_handshake(
                                        socket, contact_id, *peer_addr, *peer_pubkey,
                                    ) {
                                        self.contact_addrs.insert(contact_id.clone(), *peer_addr);
                                        self.hello_retries.insert(*peer_addr, 0);
                                        self.peers.insert(*peer_addr, session);
                                    }
                                }
                            }
                        }
                    }
                }

                MsgCommand::SendGroupAvatar { group_id, avatar_data, sha256, member_contacts } => {
                    log_fmt!("[daemon] SendGroupAvatar group={} size={}", group_id, avatar_data.len());
                    for (contact_id, _peer_addr, _peer_pubkey) in &member_contacts {
                        self.group_avatar_sends.insert(
                            (contact_id.clone(), group_id.clone()),
                            super::daemon::GroupAvatarSendState {
                                avatar_data: avatar_data.clone(),
                                sha256,
                                sent: false,
                                sent_at: Instant::now(),
                                retries: 0,
                            },
                        );
                    }
                }

                MsgCommand::SendCallSignal { group_id, channel_id, active, call_mode, member_contacts } => {
                    log_fmt!("[probe] SIGNAL OUT active={} group={} channel={} mode={}", active, group_id, channel_id, call_mode);
                    if let Some(ref socket) = self.socket {
                        for (contact_id, peer_addr, peer_pubkey) in &member_contacts {
                            let mut sent = false;
                            if let Some(addr) = self.contact_addrs.get(contact_id) {
                                if let Some(peer) = self.peers.get(addr) {
                                    if peer.is_connected() {
                                        protocol::send_call_signal(peer, socket, &group_id, &channel_id, active, call_mode).ok();
                                        sent = true;
                                    }
                                }
                            }
                            if !sent {
                                if !self.contact_addrs.contains_key(contact_id) {
                                    if let Some(session) = protocol::initiate_handshake(
                                        socket, contact_id, *peer_addr, *peer_pubkey,
                                    ) {
                                        self.contact_addrs.insert(contact_id.clone(), *peer_addr);
                                        self.hello_retries.insert(*peer_addr, 0);
                                        self.peers.insert(*peer_addr, session);
                                    }
                                }
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
}

/// Compute SHA-256 hash of a file.
pub(super) fn compute_file_sha256(path: &str) -> Option<[u8; 32]> {
    use sha2::{Digest, Sha256};
    let mut file = std::fs::File::open(path).ok()?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 8192];
    loop {
        match std::io::Read::read(&mut file, &mut buf) {
            Ok(0) => break,
            Ok(n) => hasher.update(&buf[..n]),
            Err(_) => return None,
        }
    }
    let hash = hasher.finalize();
    let mut result = [0u8; 32];
    result.copy_from_slice(&hash);
    Some(result)
}
