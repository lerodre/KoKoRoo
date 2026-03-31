use std::net::SocketAddr;
use std::time::{Duration, Instant};

use crate::crypto::{
    self, PKT_HELLO,
    PKT_MSG_ACK, PKT_MSG_BYE, PKT_MSG_CHAT, PKT_MSG_CONFIRM, PKT_MSG_HELLO, PKT_MSG_IDENTITY,
    PKT_MSG_REQUEST, PKT_MSG_REQUEST_ACK,
    PKT_MSG_IP_ANNOUNCE, PKT_MSG_PEER_QUERY, PKT_MSG_PEER_RESPONSE,
    PKT_MSG_PRESENCE,
    PKT_MSG_FILE_OFFER, PKT_MSG_FILE_ACCEPT, PKT_MSG_FILE_REJECT,
    PKT_MSG_FILE_CHUNK, PKT_MSG_FILE_ACK, PKT_MSG_FILE_COMPLETE, PKT_MSG_FILE_CANCEL,
    PKT_MSG_FILE_NACK,
    PKT_MSG_AVATAR_OFFER, PKT_MSG_AVATAR_DATA, PKT_MSG_AVATAR_ACK, PKT_MSG_AVATAR_NACK,
    PKT_GRP_INVITE, PKT_GRP_MSG_CHAT,
    PKT_GRP_INVITE_ACK, PKT_GRP_INVITE_NACK,
    PKT_GRP_UPDATE, PKT_GRP_AVATAR_OFFER, PKT_GRP_AVATAR_DATA, PKT_GRP_AVATAR_ACK,
    PKT_GRP_MEMBER_SYNC,
    PKT_GRP_CALL_SIGNAL,
    PKT_MSG_DELETE_CONTACT, PKT_MSG_DELETE_ACK,
};
use crate::identity::{self};
use crate::filetransfer;
use crate::filetransfer::sender::SenderState;

use super::protocol;
use super::MsgEvent;
use super::daemon::{MsgDaemon, FileTransfer, QUERY_RATE_WINDOW, QUERY_RATE_MAX, ANNOUNCE_MAX_AGE};

impl MsgDaemon {
    pub(crate) fn receive_packets(&mut self) {
        let socket = match &self.socket {
            Some(s) => s,
            None => return,
        };

        let mut buf = [0u8; 8192];
        let mut packets_processed = 0u32;
        let mut deferred_delete: Option<(String, [u8; 32], String)> = None;
        while let Ok((len, from)) = socket.recv_from(&mut buf) {
            if len == 0 {
                continue;
            }
            let data = &buf[..len];
            let pkt_type = data[0];

            // Skip verbose log for keepalives and file chunks (too noisy)
            if pkt_type != PKT_MSG_ACK && pkt_type != PKT_MSG_FILE_CHUNK {
                log_fmt!("[daemon] recv pkt type=0x{:02x} len={} from={}", pkt_type, len, from);
            }

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
                        let (session, ephemeral_shared) = crate::crypto::complete_handshake(our_secret, &peer_ephemeral);
                        peer.session = Some(session);
                        peer.ephemeral_shared = Some(ephemeral_shared);
                        peer.upgraded_session = None;
                        peer.identity_confirmed = false;
                        peer.state = super::session::PeerState::AwaitingIdentity;
                        peer.touch();
                        // Send REQUEST (not IDENTITY)
                        protocol::send_request(peer, socket, &self.identity, &self.nickname);
                    } else if let Some(peer) = self.peers.get_mut(&from) {
                        if matches!(peer.state, super::session::PeerState::AwaitingHello { .. }) {
                            // Pending session (AwaitingHello) — this is a hello response
                            log_fmt!("[daemon] MSG_HELLO response from {} — completing handshake", from);
                            let ok = protocol::handle_hello_response(
                                data, peer, socket, &self.identity, &self.nickname,
                            );
                            log_fmt!("[daemon]   handshake result: {} (state now: {})",
                                ok,
                                match &peer.state {
                                    super::session::PeerState::AwaitingHello { .. } => "AwaitingHello",
                                    super::session::PeerState::AwaitingIdentity => "AwaitingIdentity",
                                    super::session::PeerState::Connected => "Connected",
                                });
                        } else if peer.is_connected() {
                            // Connected — peer restarted. Reset and accept fresh.
                            let old_cid = peer.contact_id.clone();
                            log_fmt!("[daemon] MSG_HELLO from {} (state=Connected) — resetting, accepting as incoming", from);
                            self.event_tx.send(MsgEvent::PeerStatus {
                                contact_id: old_cid.clone(),
                                online: false,
                            }).ok();
                            self.peers.remove(&from);
                            if !old_cid.is_empty() {
                                self.contact_addrs.remove(&old_cid);
                            }
                            self.hello_retries.remove(&from);
                            // Handle as new incoming connection
                            let ip_str = from.ip().to_string();
                            if !self.settings.is_ip_banned(&ip_str) {
                                if let Some(session) = protocol::handle_incoming_hello(
                                    data, from, socket, &self.identity, &self.nickname,
                                ) {
                                    log_fmt!("[daemon]   re-handshake OK");
                                    self.peers.insert(from, session);
                                }
                            }
                        } else {
                            // AwaitingIdentity — we already have a valid session from a
                            // completed handshake. Ignore duplicate/late HELLO to avoid
                            // resetting our session key and causing an infinite
                            // handshake-identity decrypt loop.
                            log_fmt!("[daemon] MSG_HELLO from {} (state=AwaitingIdentity) — ignoring (session already established)", from);
                        }
                    } else {
                        // Check if this is a response from a different SLAAC address
                        // of the same peer (IPv6 privacy extensions). Match by /64
                        // prefix so only devices on the same network segment qualify.
                        let matching_req = if let std::net::IpAddr::V6(from_v6) = from.ip() {
                            let from_prefix = u128::from(from_v6) >> 64;
                            self.outgoing_requests.keys()
                                .find(|addr| {
                                    if let std::net::IpAddr::V6(req_v6) = addr.ip() {
                                        let req_prefix = u128::from(req_v6) >> 64;
                                        req_prefix == from_prefix
                                            && matches!(
                                                self.outgoing_requests.get(addr).map(|p| &p.state),
                                                Some(super::session::PeerState::AwaitingHello { .. })
                                            )
                                    } else {
                                        false
                                    }
                                })
                                .copied()
                        } else {
                            None
                        };

                        if let Some(orig_addr) = matching_req {
                            log_fmt!("[daemon] MSG_HELLO from {} — matched outgoing_request {} (same /64 prefix)", from, orig_addr);
                            let mut peer = self.outgoing_requests.remove(&orig_addr).unwrap();
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
                            let (session, ephemeral_shared) = crate::crypto::complete_handshake(our_secret, &peer_ephemeral);
                            peer.session = Some(session);
                            peer.ephemeral_shared = Some(ephemeral_shared);
                            peer.upgraded_session = None;
                            peer.identity_confirmed = false;
                            peer.state = super::session::PeerState::AwaitingIdentity;
                            peer.peer_addr = from;
                            peer.touch();
                            protocol::send_request(&mut peer, socket, &self.identity, &self.nickname);
                            self.outgoing_requests.insert(from, peer);
                        } else {
                            // Unknown incoming connection
                            let ip_str = from.ip().to_string();
                            if self.settings.is_ip_banned(&ip_str) {
                                log_fmt!("[daemon] MSG_HELLO from {} — BLOCKED (IP banned)", from);
                                continue;
                            }
                            log_fmt!("[daemon] MSG_HELLO from unknown {} — accepting incoming connection", from);
                            if let Some(session) = protocol::handle_incoming_hello(
                                data, from, socket, &self.identity, &self.nickname,
                            ) {
                                log_fmt!("[daemon]   incoming session created (AwaitingIdentity)");
                                self.peers.insert(from, session);
                            } else {
                                log_fmt!("[daemon]   incoming session FAILED to create");
                            }
                        }
                    }
                }

                PKT_MSG_IDENTITY => {
                    log_fmt!("[daemon] MSG_IDENTITY from {}", from);
                    // If peer is already Connected, treat as a nickname update
                    if let Some(peer) = self.peers.get_mut(&from) {
                        if peer.is_connected() && !peer.contact_id.is_empty() {
                            // Decrypt and extract nickname without full handshake
                            if let Some(session) = peer.session.as_ref() {
                                if let Some((pkt_type, plain)) = session.decrypt_packet(data) {
                                    if pkt_type == crate::crypto::PKT_MSG_IDENTITY && plain.len() >= 32 {
                                        let new_nick = String::from_utf8_lossy(&plain[32..]).to_string();
                                        if new_nick != peer.peer_nickname {
                                            log_fmt!("[daemon]   nickname update: '{}' -> '{}' for {}", peer.peer_nickname, new_nick, peer.contact_id);
                                            peer.peer_nickname = new_nick.clone();
                                            peer.touch();
                                            // Update on disk
                                            if let Some(mut contact) = identity::load_contact(&peer.peer_pubkey) {
                                                contact.nickname = new_nick.clone();
                                                identity::save_contact(&contact);
                                            }
                                            self.event_tx.send(MsgEvent::NicknameUpdated {
                                                contact_id: peer.contact_id.clone(),
                                                nickname: new_nick,
                                            }).ok();
                                        }
                                    }
                                }
                            }
                            continue;
                        }
                    }

                    let mut stale_addr_to_remove: Option<SocketAddr> = None;
                    if let Some(peer) = self.peers.get_mut(&from) {
                        match protocol::handle_identity(data, peer, &self.identity, socket) {
                            Ok(upgraded) => {
                            let contact_id = peer.contact_id.clone();
                            if upgraded {
                                log_fmt!("[daemon]   identity OK (upgraded session)! contact_id={} nick='{}' — peer is ONLINE", contact_id, peer.peer_nickname);
                            } else {
                                log_fmt!("[daemon]   identity OK! contact_id={} nick='{}' — peer is ONLINE", contact_id, peer.peer_nickname);
                            }

                            // Detect stale peer session if contact reconnected from a new address
                            if let Some(&old_addr) = self.contact_addrs.get(&contact_id) {
                                if old_addr != from {
                                    log_fmt!("[daemon]   contact {} migrated from {} to {}, will remove stale session", contact_id, old_addr, from);
                                    stale_addr_to_remove = Some(old_addr);
                                }
                            }
                            self.contact_addrs.insert(contact_id.clone(), from);

                            // Update last_address on disk so reconnect uses the current IP
                            if let Some(mut contact) = identity::load_contact(&peer.peer_pubkey) {
                                let new_ip = from.ip().to_string();
                                let new_port = from.port().to_string();
                                if contact.last_address != new_ip || contact.last_port != new_port {
                                    log_fmt!("[daemon]   updating last_address: {} -> {}", contact.last_address, new_ip);
                                    contact.last_address = new_ip;
                                    contact.last_port = new_port;
                                    contact.last_seen = identity::now_timestamp();
                                    identity::save_contact(&contact);
                                }
                            }

                            // Check if we have a pending delete for this contact
                            if self.pending_deletes.has_pending(&contact_id) {
                                log_fmt!("[daemon]   pending delete found for {} — sending DELETE", &contact_id[..8.min(contact_id.len())]);
                                protocol::send_delete_contact(peer, socket);
                                // Don't notify online or flush outbox — we're deleting
                                continue;
                            }

                            // Notify GUI peer is online
                            self.event_tx.send(MsgEvent::PeerStatus {
                                contact_id: contact_id.clone(),
                                online: true,
                            }).ok();

                            // Load outbox and flush pending messages
                            let outbox = self.outboxes.entry(contact_id.clone())
                                .or_insert_with(|| super::outbox::Outbox::load(&contact_id, &self.identity.secret));
                            for msg in &mut outbox.messages {
                                protocol::send_chat_message(peer, socket, msg.seq, &msg.text).ok();
                                msg.attempts += 1;
                            }
                            // Flush pending group invites (reactive — triggered on peer connect)
                            if let Some(store) = self.pending_invites.get_mut(&contact_id) {
                                for invite in &mut store.invites {
                                    protocol::send_group_invite(peer, socket, &invite.group_json).ok();
                                    invite.attempts += 1;
                                    // Load group from disk for member syncs on ACK
                                    if let Some(grp) = crate::group::load_group(&invite.group_id) {
                                        self.pending_member_syncs.insert(
                                            (contact_id.clone(), invite.group_id.clone()),
                                            grp.members,
                                        );
                                    }
                                }
                                store.save();
                            }
                            // Reset retry state
                            self.retry_state.insert(contact_id, (Instant::now(), 0));
                            // Send our presence to the newly connected peer
                            protocol::send_presence(peer, socket, self.our_presence);
                            }
                            Err(e) => {
                                log_fmt!("[daemon]   identity FAILED: {}", e);
                            }
                        }
                    } else if self.outgoing_requests.contains_key(&from) {
                        // IDENTITY arrived for a session in outgoing_requests (e.g. receiver
                        // sent HELLO+IDENTITY together). Move to peers so the normal flow
                        // processes it. The REQUEST will be sent after this, and the IDENTITY
                        // is just the receiver's auto-response — we can safely ignore it since
                        // we'll get their identity via REQUEST_ACK after they accept.
                        log_fmt!("[daemon]   IDENTITY from {} is in outgoing_requests — ignoring (will get identity via REQUEST_ACK)", from);
                    } else {
                        log_fmt!("[daemon]   no peer session for {} — ignoring IDENTITY", from);
                    }
                    // Clean up stale peer session after releasing the borrow
                    if let Some(old_addr) = stale_addr_to_remove {
                        self.peers.remove(&old_addr);
                    }
                }

                PKT_MSG_CONFIRM => {
                    if let Some(peer) = self.peers.get_mut(&from) {
                        if protocol::handle_confirm(data, peer) {
                            let cid = peer.contact_id.clone();
                            log_fmt!("[daemon] CONFIRM: identity upgrade confirmed for {}", cid);
                            // Auto-send our avatar if we have one
                            if !self.avatar_sends.contains_key(&cid) {
                                if let Some(avatar_data) = crate::avatar::load_own_avatar() {
                                    let sha256 = crate::avatar::avatar_sha256(&avatar_data);
                                    self.avatar_sends.insert(cid, super::daemon::AvatarSendState {
                                        avatar_data,
                                        sha256,
                                        sent: false,
                                        sent_at: Instant::now(),
                                        retries: 0,
                                        offer_sent: false,
                                        needs_send: false,
                                    });
                                }
                            }
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
                            if seq == 0 {
                                // Keepalive — just log briefly
                                let nick = if peer.peer_nickname.is_empty() { "?" } else { &peer.peer_nickname };
                                log_fmt!("[daemon] keepalive from {} ({})", nick, from);
                            } else {
                                let cid = peer.contact_id.clone();
                                if let Some(outbox) = self.outboxes.get_mut(&cid) {
                                    outbox.remove_acked(seq);
                                }
                                self.event_tx.send(MsgEvent::MessageDelivered).ok();
                            }
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
                    if let Some(peer) = self.outgoing_requests.get_mut(&from) {
                        if let Some((pubkey, nickname)) = protocol::handle_request_accept(data, peer) {
                            let contact_id = identity::derive_contact_id(
                                &self.identity.pubkey, &pubkey,
                            );

                            // Clear any stale pending delete for this contact (re-added after delete)
                            if self.pending_deletes.has_pending(&contact_id) {
                                log_fmt!("[daemon]   clearing stale pending delete for {}", &contact_id[..8.min(contact_id.len())]);
                                self.pending_deletes.remove(&contact_id);
                            }

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

                                // Notify GUI peer is online and send our presence
                                self.event_tx.send(MsgEvent::PeerStatus {
                                    contact_id: contact_id.clone(),
                                    online: true,
                                }).ok();
                                if let Some(ref socket) = self.socket {
                                    protocol::send_presence(&mut peer_session, socket, self.our_presence);
                                }

                                self.peers.insert(from, peer_session);

                                // Queue avatar send for new contact
                                if !self.avatar_sends.contains_key(&contact_id) {
                                    if let Some(avatar_data) = crate::avatar::load_own_avatar() {
                                        let sha256 = crate::avatar::avatar_sha256(&avatar_data);
                                        self.avatar_sends.insert(contact_id.clone(), super::daemon::AvatarSendState {
                                            avatar_data,
                                            sha256,
                                            sent: false,
                                            sent_at: Instant::now(),
                                            retries: 0,
                                            offer_sent: false,
                                            needs_send: false,
                                        });
                                    }
                                }
                            }
                            self.retry_state.insert(contact_id.clone(), (Instant::now(), 0));

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
                                    // Clear failed cooldown — new IP means worth retrying
                                    self.failed_contacts.remove(&contact_id);
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
                                // Clear failed cooldown — new IP means worth retrying
                                self.failed_contacts.remove(&contact.contact_id);
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

                PKT_MSG_FILE_OFFER => {
                    if let Some(peer) = self.peers.get_mut(&from) {
                        if let Some((transfer_id, file_size, sha256, filename)) =
                            filetransfer::protocol::handle_file_offer(data, peer)
                        {
                            peer.touch();
                            let contact_id = peer.contact_id.clone();
                            if contact_id.is_empty() { continue; }

                            // Store as incoming waiting
                            self.file_transfers.insert(
                                (contact_id.clone(), transfer_id),
                                FileTransfer::IncomingWaiting {
                                    transfer_id,
                                    contact_id: contact_id.clone(),
                                    filename: filename.clone(),
                                    file_size,
                                    sha256,
                                },
                            );

                            self.event_tx.send(MsgEvent::IncomingFileOffer {
                                contact_id,
                                transfer_id,
                                filename,
                                file_size,
                            }).ok();
                        }
                    }
                }

                PKT_MSG_FILE_ACCEPT => {
                    if let Some(peer) = self.peers.get_mut(&from) {
                        if let Some(transfer_id) = filetransfer::protocol::handle_file_accept(data, peer) {
                            peer.touch();
                            let contact_id = peer.contact_id.clone();
                            let key = (contact_id.clone(), transfer_id);
                            // Transition OfferedWaiting → SendingThreaded
                            if let Some(ft) = self.file_transfers.remove(&key) {
                                if let FileTransfer::OfferedWaiting { file_path, file_size, sha256, .. } = ft {
                                    let sender = SenderState::new(transfer_id, file_path, file_size, sha256);
                                    // Clone socket and session for the sender thread
                                    let sock_clone = self.socket.as_ref().unwrap().try_clone().unwrap();
                                    let session_clone = peer.session.as_ref().unwrap().clone_for_sending();
                                    let peer_addr = peer.peer_addr;

                                    // Create channels
                                    let (cmd_tx, cmd_rx) = std::sync::mpsc::channel();
                                    let event_tx = self.sender_events_tx.clone();
                                    let cid = contact_id.clone();

                                    // Spawn sender thread
                                    std::thread::spawn(move || {
                                        crate::filetransfer::sender::sender_thread_run(
                                            sender, sock_clone, session_clone,
                                            peer_addr, cmd_rx, event_tx, cid,
                                        );
                                    });

                                    self.file_transfers.insert(key, FileTransfer::SendingThreaded {
                                        cmd_tx,
                                        sha256,
                                        file_size,
                                        complete_sent: false,
                                        complete_sent_at: Instant::now(),
                                    });

                                    // Notify GUI that transfer started
                                    self.event_tx.send(MsgEvent::FileTransferProgress {
                                        contact_id,
                                        transfer_id,
                                        bytes_transferred: 0,
                                        total_bytes: file_size,
                                    }).ok();
                                }
                            }
                        }
                    }
                }

                PKT_MSG_FILE_REJECT => {
                    if let Some(peer) = self.peers.get_mut(&from) {
                        if let Some(transfer_id) = filetransfer::protocol::handle_file_reject(data, peer) {
                            peer.touch();
                            let contact_id = peer.contact_id.clone();
                            let key = (contact_id.clone(), transfer_id);
                            self.file_transfers.remove(&key);
                            self.event_tx.send(MsgEvent::FileTransferFailed {
                                contact_id,
                                transfer_id,
                                reason: "Rejected by peer".into(),
                            }).ok();
                        }
                    }
                }

                PKT_MSG_FILE_CHUNK => {
                    // ACK-on-Error: just store the chunk, no ACK needed per-chunk.
                    if let Some(peer) = self.peers.get_mut(&from) {
                        if let Some((transfer_id, chunk_index, chunk_data)) =
                            filetransfer::protocol::handle_file_chunk(data, peer)
                        {
                            peer.touch();
                            let contact_id = peer.contact_id.clone();
                            let key = (contact_id, transfer_id);
                            if let Some(FileTransfer::Receiving(ref mut recv)) = self.file_transfers.get_mut(&key) {
                                recv.on_chunk(chunk_index, &chunk_data);
                            }
                        }
                    }
                }

                PKT_MSG_FILE_ACK => {
                    // Final ACK from receiver: all chunks received, transfer done.
                    if let Some(peer) = self.peers.get_mut(&from) {
                        if let Some(transfer_id) =
                            filetransfer::protocol::handle_file_ack(data, peer)
                        {
                            peer.touch();
                            let contact_id = peer.contact_id.clone();
                            let key = (contact_id.clone(), transfer_id);
                            let had_transfer = self.file_transfers.contains_key(&key);
                            if let Some(ft) = self.file_transfers.get_mut(&key) {
                                match ft {
                                    FileTransfer::SendingThreaded { cmd_tx, .. } => {
                                        cmd_tx.send(crate::filetransfer::sender::SenderThreadCmd::Ack).ok();
                                    }
                                    FileTransfer::Sending(ref mut sender) => {
                                        sender.on_ack();
                                    }
                                    _ => {}
                                }
                            }
                            self.file_transfers.remove(&key);
                            log_fmt!("[daemon] file transfer complete (sender ACK): tid={} cid={} had_state={}",
                                transfer_id, &contact_id[..8.min(contact_id.len())], had_transfer);
                            self.event_tx.send(MsgEvent::FileTransferComplete {
                                contact_id,
                                transfer_id,
                                saved_path: String::new(),
                            }).ok();
                        }
                    }
                }

                PKT_MSG_FILE_NACK => {
                    // Receiver reports missing chunks — retransmit only those.
                    if let Some(peer) = self.peers.get_mut(&from) {
                        if let Some((transfer_id, missing)) =
                            filetransfer::protocol::handle_file_nack(data, peer)
                        {
                            peer.touch();
                            let contact_id = peer.contact_id.clone();
                            let key = (contact_id, transfer_id);
                            if let Some(ft) = self.file_transfers.get_mut(&key) {
                                match ft {
                                    FileTransfer::SendingThreaded { cmd_tx, complete_sent, .. } => {
                                        cmd_tx.send(crate::filetransfer::sender::SenderThreadCmd::Nack(missing)).ok();
                                        *complete_sent = false;
                                    }
                                    FileTransfer::Sending(ref mut sender) => {
                                        sender.on_nack(missing);
                                    }
                                    _ => {}
                                }
                            }
                        }
                    }
                }

                PKT_MSG_FILE_COMPLETE => {
                    // Sender says all chunks sent. Check for missing, respond with ACK or NACK.
                    if let Some(peer) = self.peers.get_mut(&from) {
                        if let Some((transfer_id, _sha256)) =
                            filetransfer::protocol::handle_file_complete(data, peer)
                        {
                            peer.touch();
                            let contact_id = peer.contact_id.clone();
                            let key = (contact_id.clone(), transfer_id);

                            // Check if receiver has all chunks
                            let action = if let Some(FileTransfer::Receiving(ref recv)) = self.file_transfers.get(&key) {
                                let missing = recv.missing_chunks();
                                if missing.is_empty() {
                                    Some(("complete".to_string(), missing))
                                } else {
                                    Some(("nack".to_string(), missing))
                                }
                            } else {
                                None
                            };

                            if let Some((action_type, missing)) = action {
                                if action_type == "nack" {
                                    // Send NACK with missing chunk list
                                    if let Some(ref socket) = self.socket {
                                        filetransfer::protocol::send_file_nack(
                                            peer, socket, transfer_id, &missing,
                                        );
                                    }
                                } else {
                                    // All received — send ACK immediately to stop sender retrying
                                    if let Some(ref socket) = self.socket {
                                        filetransfer::protocol::send_file_ack(peer, socket, transfer_id);
                                    }
                                    // Take the ReceiverState and verify async
                                    if let Some(ft) = self.file_transfers.remove(&key) {
                                        if let FileTransfer::Receiving(mut recv) = ft {
                                            let verify_tx = self.verify_results_tx.clone();
                                            let cid = contact_id.clone();
                                            std::thread::spawn(move || {
                                                recv.flush();
                                                let success = recv.verify_hash();
                                                let saved_path = if success {
                                                    recv.finalize()
                                                } else {
                                                    recv.cleanup();
                                                    None
                                                };
                                                verify_tx.send(super::daemon::VerifyResult {
                                                    contact_id: cid,
                                                    transfer_id,
                                                    success: success && saved_path.is_some(),
                                                    saved_path,
                                                }).ok();
                                            });
                                        }
                                    }
                                }
                            }
                        }
                    }
                }

                PKT_MSG_FILE_CANCEL => {
                    if let Some(peer) = self.peers.get_mut(&from) {
                        if let Some((transfer_id, _reason)) =
                            filetransfer::protocol::handle_file_cancel(data, peer)
                        {
                            peer.touch();
                            let contact_id = peer.contact_id.clone();
                            let key = (contact_id.clone(), transfer_id);
                            if let Some(mut ft) = self.file_transfers.remove(&key) {
                                match ft {
                                    FileTransfer::Receiving(ref mut recv) => {
                                        recv.cleanup();
                                    }
                                    FileTransfer::SendingThreaded { ref cmd_tx, .. } => {
                                        cmd_tx.send(crate::filetransfer::sender::SenderThreadCmd::Cancel).ok();
                                    }
                                    _ => {}
                                }
                                log_fmt!("[daemon] file transfer failed: id={}", transfer_id);
                                self.event_tx.send(MsgEvent::FileTransferFailed {
                                    contact_id,
                                    transfer_id,
                                    reason: "Cancelled by peer".into(),
                                }).ok();
                            }
                        }
                    }
                }

                PKT_MSG_AVATAR_OFFER => {
                    if let Some(peer) = self.peers.get_mut(&from) {
                        if let Some((sha256, total_size)) = protocol::handle_avatar_offer(data, peer) {
                            peer.touch();
                            let contact_id = peer.contact_id.clone();
                            if contact_id.is_empty() { continue; }
                            // Validate size cap
                            if total_size as usize > crate::avatar::MAX_AVATAR_BYTES {
                                continue;
                            }
                            // Skip if we already have this exact avatar (same SHA-256)
                            if let Some(existing) = crate::avatar::load_contact_avatar(&contact_id) {
                                if crate::avatar::avatar_sha256(&existing) == sha256 {
                                    // Already have it — ACK so sender stops retrying
                                    if let Some(ref socket) = self.socket {
                                        protocol::send_avatar_ack(peer, socket, &sha256);
                                    }
                                    log_fmt!("[daemon] avatar ACK (already have it) -> {}", from);
                                    continue;
                                }
                            }
                            let chunk_size = super::daemon::AVATAR_CHUNK_SIZE;
                            let total_chunks = ((total_size as usize + chunk_size - 1) / chunk_size) as u16;
                            log_fmt!("[daemon] AVATAR_OFFER from {} size={} chunks={} — sending NACK (need data)", from, total_size, total_chunks);
                            self.avatar_recvs.insert(from, super::daemon::AvatarRecvState {
                                sha256,
                                total_size,
                                total_chunks,
                                chunks: std::collections::HashMap::new(),
                                started_at: Instant::now(),
                                contact_id,
                            });
                            // Send NACK to request avatar data
                            if let Some(ref socket) = self.socket {
                                protocol::send_avatar_nack(peer, socket, &sha256);
                            }
                        }
                    }
                }

                PKT_MSG_AVATAR_DATA => {
                    if let Some(peer) = self.peers.get_mut(&from) {
                        if let Some((chunk_index, chunk_data)) = protocol::handle_avatar_data(data, peer) {
                            peer.touch();
                            if let Some(recv) = self.avatar_recvs.get_mut(&from) {
                                if chunk_index < recv.total_chunks {
                                    recv.chunks.insert(chunk_index, chunk_data);
                                }
                                // Check if all chunks received
                                if recv.chunks.len() as u16 >= recv.total_chunks {
                                    // Reassemble
                                    let mut assembled = Vec::with_capacity(recv.total_size as usize);
                                    let mut ok = true;
                                    for i in 0..recv.total_chunks {
                                        if let Some(chunk) = recv.chunks.get(&i) {
                                            assembled.extend_from_slice(chunk);
                                        } else {
                                            ok = false;
                                            break;
                                        }
                                    }
                                    if ok {
                                        // Validate
                                        let hash = crate::avatar::avatar_sha256(&assembled);
                                        if hash == recv.sha256 && crate::avatar::validate_received_avatar(&assembled) {
                                            let cid = recv.contact_id.clone();
                                            log_fmt!("[daemon] avatar received OK for contact {}", cid);
                                            crate::avatar::save_contact_avatar(&cid, &assembled).ok();
                                            self.event_tx.send(MsgEvent::AvatarReceived {
                                                contact_id: cid,
                                            }).ok();
                                            // ACK so sender stops retrying
                                            if let Some(ref socket) = self.socket {
                                                protocol::send_avatar_ack(peer, socket, &hash);
                                            }
                                        } else {
                                            log_fmt!("[daemon] avatar validation FAILED from {}", from);
                                        }
                                    }
                                    self.avatar_recvs.remove(&from);
                                }
                            }
                        }
                    }
                }

                PKT_MSG_AVATAR_ACK => {
                    if let Some(peer) = self.peers.get_mut(&from) {
                        if let Some(sha256) = protocol::handle_avatar_ack(data, peer) {
                            peer.touch();
                            let cid = peer.contact_id.clone();
                            // Remove from retry queue if SHA-256 matches
                            if let Some(state) = self.avatar_sends.get(&cid) {
                                if state.sha256 == sha256 {
                                    self.avatar_sends.remove(&cid);
                                    log_fmt!("[daemon] avatar ACK from {}, send complete", cid);
                                }
                            }
                        }
                    }
                }

                PKT_MSG_AVATAR_NACK => {
                    if let Some(peer) = self.peers.get_mut(&from) {
                        if let Some(sha256) = protocol::handle_avatar_nack(data, peer) {
                            peer.touch();
                            let cid = peer.contact_id.clone();
                            if let Some(state) = self.avatar_sends.get_mut(&cid) {
                                if state.sha256 == sha256 {
                                    state.needs_send = true;
                                    log_fmt!("[daemon] avatar NACK from {} — will send chunks", cid);
                                }
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

                PKT_GRP_INVITE => {
                    if let Some(peer) = self.peers.get_mut(&from) {
                        if let Some(invite_json) = protocol::handle_group_invite(data, peer) {
                            peer.touch();
                            let from_nickname = peer.peer_nickname.clone();
                            let from_contact_id = peer.contact_id.clone();
                            log_fmt!("[daemon] received group invite from {} ({} bytes)", from_nickname, invite_json.len());
                            self.event_tx.send(MsgEvent::IncomingGroupInvite {
                                from_nickname,
                                from_contact_id,
                                invite_json,
                            }).ok();
                        }
                    }
                }

                PKT_GRP_MSG_CHAT => {
                    if let Some(peer) = self.peers.get_mut(&from) {
                        if let Some((group_id, channel_id, text)) = protocol::handle_group_chat(data, peer) {
                            peer.touch();
                            // Derive fingerprint from cryptographically verified pubkey (never trust peer-supplied data)
                            let sender_fingerprint = crypto::fingerprint(&peer.peer_pubkey);
                            let sender_nickname = peer.peer_nickname.clone();
                            self.event_tx.send(MsgEvent::IncomingGroupChat {
                                group_id,
                                channel_id,
                                sender_fingerprint,
                                sender_nickname,
                                text,
                            }).ok();
                        }
                    }
                }

                PKT_GRP_INVITE_ACK => {
                    if let Some(peer) = self.peers.get_mut(&from) {
                        if let Some(group_id) = protocol::handle_group_invite_ack(data, peer) {
                            peer.touch();
                            let cid = peer.contact_id.clone();
                            log_fmt!("[daemon] group invite ACK from {} for group={}", cid, group_id);
                            if let Some(store) = self.pending_invites.get_mut(&cid) {
                                store.remove(&group_id);
                            }
                            // Send member syncs to the acceptor
                            let key = (cid.clone(), group_id.clone());
                            if let Some(members) = self.pending_member_syncs.remove(&key) {
                                if let Some(ref socket) = self.socket {
                                    if let Some(peer) = self.peers.get(&from) {
                                        log_fmt!("[daemon]   sending {} member syncs for group={}", members.len(), group_id);
                                        for member in &members {
                                            let wire = crate::group::GroupMemberWire::from_member(member);
                                            if let Ok(wire_json) = serde_json::to_vec(&wire) {
                                                protocol::send_group_member_sync(peer, socket, &group_id, &wire_json).ok();
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }

                PKT_GRP_INVITE_NACK => {
                    if let Some(peer) = self.peers.get_mut(&from) {
                        if let Some(group_id) = protocol::handle_group_invite_nack(data, peer) {
                            peer.touch();
                            let cid = peer.contact_id.clone();
                            log_fmt!("[daemon] group invite NACK from {} for group={}", cid, group_id);
                            if let Some(store) = self.pending_invites.get_mut(&cid) {
                                store.remove(&group_id);
                            }
                            self.event_tx.send(MsgEvent::GroupInviteRejected {
                                contact_id: cid,
                                group_id,
                            }).ok();
                        }
                    }
                }

                PKT_GRP_UPDATE => {
                    if let Some(peer) = self.peers.get_mut(&from) {
                        if let Some(group_json) = protocol::handle_group_update(data, peer) {
                            peer.touch();
                            // Security: verify sender is admin in our LOCAL copy
                            let sender_pubkey = peer.peer_pubkey;
                            if let Ok(received_group) = serde_json::from_slice::<crate::group::Group>(&group_json) {
                                if let Some(local_group) = crate::group::load_group(&received_group.group_id) {
                                    let is_admin = local_group.members.iter().any(|m| m.pubkey == sender_pubkey && m.is_admin);
                                    if is_admin {
                                        log_fmt!("[daemon] group update from admin for group={}", received_group.group_id);
                                        self.event_tx.send(MsgEvent::GroupUpdated { group_json }).ok();
                                    } else {
                                        log_fmt!("[daemon] WARNING: group update from non-admin, discarding");
                                    }
                                } else {
                                    log_fmt!("[daemon] group update for unknown group={}, discarding", received_group.group_id);
                                }
                            }
                        }
                    }
                }

                PKT_GRP_AVATAR_OFFER => {
                    if let Some(peer) = self.peers.get_mut(&from) {
                        if let Some((group_id, sha256, total_size)) = protocol::handle_group_avatar_offer(data, peer) {
                            peer.touch();
                            // Security: verify sender is admin in our LOCAL copy
                            let sender_pubkey = peer.peer_pubkey;
                            if let Some(local_group) = crate::group::load_group(&group_id) {
                                let is_admin = local_group.members.iter().any(|m| m.pubkey == sender_pubkey && m.is_admin);
                                if is_admin {
                                    // Skip if we already have this exact group avatar (same SHA-256)
                                    if let Some(existing) = crate::avatar::load_group_avatar(&group_id) {
                                        if crate::avatar::avatar_sha256(&existing) == sha256 {
                                            if let Some(ref socket) = self.socket {
                                                protocol::send_group_avatar_ack(peer, socket, &group_id, &sha256);
                                            }
                                            continue;
                                        }
                                    }
                                    let chunk_size = super::daemon::AVATAR_CHUNK_SIZE as u32;
                                    let total_chunks = ((total_size + chunk_size - 1) / chunk_size) as u16;
                                    log_fmt!("[daemon] group avatar offer for group={} size={} chunks={}", group_id, total_size, total_chunks);
                                    self.group_avatar_recvs.insert(
                                        (from, group_id.clone()),
                                        super::daemon::AvatarRecvState {
                                            sha256,
                                            total_size,
                                            total_chunks,
                                            chunks: std::collections::HashMap::new(),
                                            started_at: Instant::now(),
                                            contact_id: group_id,
                                        },
                                    );
                                } else {
                                    log_fmt!("[daemon] WARNING: group avatar offer from non-admin, discarding");
                                }
                            }
                        }
                    }
                }

                PKT_GRP_AVATAR_DATA => {
                    if let Some(peer) = self.peers.get_mut(&from) {
                        if let Some((group_id, chunk_index, chunk_data)) = protocol::handle_group_avatar_data(data, peer) {
                            peer.touch();
                            let key = (from, group_id.clone());
                            if let Some(recv) = self.group_avatar_recvs.get_mut(&key) {
                                recv.chunks.insert(chunk_index, chunk_data);
                                // Check if all chunks received
                                if recv.chunks.len() as u16 >= recv.total_chunks {
                                    // Reassemble
                                    let mut full_data = Vec::with_capacity(recv.total_size as usize);
                                    for i in 0..recv.total_chunks {
                                        if let Some(chunk) = recv.chunks.get(&i) {
                                            full_data.extend_from_slice(chunk);
                                        }
                                    }
                                    // Verify SHA-256
                                    let hash = crate::avatar::avatar_sha256(&full_data);
                                    if hash == recv.sha256 && crate::avatar::validate_received_avatar(&full_data) {
                                        crate::avatar::save_group_avatar(&group_id, &full_data).ok();
                                        log_fmt!("[daemon] group avatar saved for group={}", group_id);
                                        self.event_tx.send(MsgEvent::GroupAvatarReceived { group_id: group_id.clone() }).ok();
                                        // ACK so sender stops retrying
                                        if let Some(ref socket) = self.socket {
                                            protocol::send_group_avatar_ack(peer, socket, &group_id, &hash);
                                        }
                                    } else {
                                        log_fmt!("[daemon] group avatar hash mismatch or invalid");
                                    }
                                    self.group_avatar_recvs.remove(&key);
                                }
                            }
                        }
                    }
                }

                PKT_GRP_AVATAR_ACK => {
                    if let Some(peer) = self.peers.get_mut(&from) {
                        if let Some((group_id, sha256)) = protocol::handle_group_avatar_ack(data, peer) {
                            peer.touch();
                            let cid = peer.contact_id.clone();
                            let key = (cid.clone(), group_id.clone());
                            if let Some(state) = self.group_avatar_sends.get(&key) {
                                if state.sha256 == sha256 {
                                    self.group_avatar_sends.remove(&key);
                                    log_fmt!("[daemon] group avatar ACK from {} for group={}", cid, group_id);
                                }
                            }
                        }
                    }
                }

                PKT_GRP_MEMBER_SYNC => {
                    if let Some(peer) = self.peers.get_mut(&from) {
                        if let Some((group_id, member_json)) = protocol::handle_group_member_sync(data, peer) {
                            peer.touch();
                            if let Ok(wire) = serde_json::from_slice::<crate::group::GroupMemberWire>(&member_json) {
                                if let Some(member) = wire.to_member() {
                                    log_fmt!("[daemon] member sync for group={}: {} (idx={})",
                                        group_id, member.nickname, member.sender_index);
                                    self.event_tx.send(MsgEvent::GroupMemberSynced {
                                        group_id,
                                        member,
                                    }).ok();
                                }
                            }
                        }
                    }
                }

                PKT_GRP_CALL_SIGNAL => {
                    if let Some(peer) = self.peers.get_mut(&from) {
                        if let Some((group_id, channel_id, active, call_mode)) = protocol::handle_call_signal(data, peer) {
                            peer.touch();
                            let contact_id = peer.contact_id.clone();
                            log_fmt!("[probe] SIGNAL IN from={} active={} group={} channel={}", &contact_id[..8.min(contact_id.len())], active, group_id, channel_id);
                            self.event_tx.send(MsgEvent::GroupCallSignal {
                                contact_id,
                                group_id,
                                channel_id,
                                active,
                                call_mode,
                            }).ok();
                        }
                    }
                }

                PKT_MSG_DELETE_CONTACT => {
                    if let Some(peer) = self.peers.get_mut(&from) {
                        if peer.is_connected() {
                            if peer.decrypt_packet(data).is_some() {
                                let contact_id = peer.contact_id.clone();
                                let peer_pubkey = peer.peer_pubkey;
                                let nickname = peer.peer_nickname.clone();
                                log_fmt!("[daemon] PKT_MSG_DELETE_CONTACT from {} (contact={})", from, &contact_id[..8.min(contact_id.len())]);
                                // Send ACK while we still have the session
                                protocol::send_delete_ack(peer, socket);
                                // Defer cleanup to after the recv loop
                                deferred_delete = Some((contact_id, peer_pubkey, nickname));
                                break;
                            }
                        }
                    }
                }

                PKT_MSG_DELETE_ACK => {
                    let ack_info = if let Some(peer) = self.peers.get_mut(&from) {
                        if peer.is_connected() {
                            if peer.decrypt_packet(data).is_some() {
                                Some(peer.contact_id.clone())
                            } else { None }
                        } else { None }
                    } else { None };

                    if let Some(contact_id) = ack_info {
                        log_fmt!("[daemon] PKT_MSG_DELETE_ACK from {} (contact={})", from, &contact_id[..8.min(contact_id.len())]);
                        self.pending_deletes.remove(&contact_id);
                        self.peers.remove(&from);
                        self.contact_addrs.remove(&contact_id);
                    }
                }

                _ => {} // Ignore unknown packet types
            }

            // Process up to 5000 packets per call to keep socket buffer drained,
            // then yield to housekeep/commands. Without this, file transfers
            // overflow the socket buffer because only 1 packet was processed per loop.
            packets_processed += 1;
            if packets_processed >= 5000 {
                break;
            }
        }

        // Process deferred delete outside the recv loop (needs &mut self)
        if let Some((contact_id, peer_pubkey, nickname)) = deferred_delete {
            self.cleanup_deleted_contact(&contact_id, &peer_pubkey);
            self.event_tx.send(MsgEvent::ContactDeletedByPeer {
                contact_id,
                nickname,
            }).ok();
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
