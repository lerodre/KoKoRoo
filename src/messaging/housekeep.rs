use std::net::SocketAddr;
use std::time::{Duration, Instant};

use crate::identity;
use crate::filetransfer;

use super::protocol;
use super::MsgEvent;
use super::daemon::{
    MsgDaemon, FileTransfer,
    KEEPALIVE_INTERVAL, PEER_TIMEOUT, HELLO_RETRY_INTERVAL, HELLO_MAX_RETRIES,
    IP_CHECK_INTERVAL, PEER_QUERY_COOLDOWN, QUERY_RATE_WINDOW,
    BEACON_INTERVAL, RETRY_BACKOFFS, FAILED_CONTACT_COOLDOWN,
    AVATAR_RECV_TIMEOUT, AVATAR_SEND_RETRY_INTERVAL, AVATAR_MAX_RETRIES,
};

impl MsgDaemon {
    pub(crate) fn housekeep(&mut self) {
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
                            super::outbox::Outbox::load(&contact_id, &self.identity.secret),
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
            // Clean up expired cooldowns
            self.failed_contacts.retain(|_, t| t.elapsed() < FAILED_CONTACT_COOLDOWN);

            let mut queued = 0;
            let mut skipped = 0;
            for (cid, addr, pk) in &self.all_contacts {
                if !self.contact_addrs.contains_key(cid) {
                    if self.failed_contacts.contains_key(cid) {
                        skipped += 1;
                        continue;
                    }
                    self.connect_queue.push_back((cid.clone(), *addr, *pk));
                    queued += 1;
                }
            }
            if queued > 0 || skipped > 0 {
                log_fmt!("[daemon] beacon: re-queued {} disconnected contacts, {} skipped (cooldown)", queued, skipped);
            }
        }

        // ── File transfer ticking (ACK-on-Error) ──
        if let Some(ref socket) = self.socket {
            // Tick senders: blast chunks + send COMPLETE when done
            let mut chunks_to_send: Vec<(String, u32, Vec<(u32, Vec<u8>)>)> = Vec::new();
            let mut completes_to_send: Vec<(String, u32, [u8; 32])> = Vec::new();

            for ((cid, _tid), ft) in &mut self.file_transfers {
                if let FileTransfer::Sending(ref mut sender) = ft {
                    if sender.done {
                        continue;
                    }
                    if !sender.all_sent {
                        // Blast up to CHUNKS_PER_TICK chunks
                        let batch = sender.next_chunks(filetransfer::CHUNKS_PER_TICK);
                        if !batch.is_empty() {
                            chunks_to_send.push((cid.clone(), sender.transfer_id, batch));
                        }
                    }
                    if sender.all_sent && !sender.complete_sent {
                        // All chunks sent — send FILE_COMPLETE
                        completes_to_send.push((cid.clone(), sender.transfer_id, sender.sha256));
                        sender.mark_complete_sent();
                    } else if sender.should_resend_complete() {
                        // Resend COMPLETE if no response
                        completes_to_send.push((cid.clone(), sender.transfer_id, sender.sha256));
                        sender.mark_complete_sent();
                    }
                }
            }

            // Send queued chunks
            for (cid, transfer_id, batch) in chunks_to_send {
                if let Some(addr) = self.contact_addrs.get(&cid) {
                    if let Some(peer) = self.peers.get(addr) {
                        for (chunk_idx, chunk_data) in batch {
                            filetransfer::protocol::send_file_chunk(
                                peer, socket, transfer_id, chunk_idx, &chunk_data,
                            );
                        }
                    }
                }
            }

            // Send FILE_COMPLETE packets
            for (cid, transfer_id, sha256) in completes_to_send {
                if let Some(addr) = self.contact_addrs.get(&cid) {
                    if let Some(peer) = self.peers.get(addr) {
                        filetransfer::protocol::send_file_complete(
                            peer, socket, transfer_id, &sha256,
                        );
                    }
                }
            }

            // Resend FILE_COMPLETE for SendingThreaded transfers
            {
                let mut resend_completes: Vec<(String, u32, [u8; 32])> = Vec::new();
                for ((cid, tid), ft) in &mut self.file_transfers {
                    if let FileTransfer::SendingThreaded { sha256, complete_sent, complete_sent_at, .. } = ft {
                        if *complete_sent && complete_sent_at.elapsed() >= std::time::Duration::from_secs(2) {
                            resend_completes.push((cid.clone(), *tid, *sha256));
                            *complete_sent_at = Instant::now();
                        }
                    }
                }
                for (cid, tid, sha256) in resend_completes {
                    if let Some(addr) = self.contact_addrs.get(&cid) {
                        if let Some(peer) = self.peers.get(addr) {
                            filetransfer::protocol::send_file_complete(
                                peer, socket, tid, &sha256,
                            );
                        }
                    }
                }
            }

            // Emit progress events periodically
            if self.last_progress_emit.elapsed().as_millis() >= filetransfer::PROGRESS_INTERVAL_MS as u128 {
                self.last_progress_emit = now;
                for ((cid, tid), ft) in &self.file_transfers {
                    match ft {
                        FileTransfer::Sending(sender) => {
                            self.event_tx.send(MsgEvent::FileTransferProgress {
                                contact_id: cid.clone(),
                                transfer_id: *tid,
                                bytes_transferred: sender.progress_bytes(),
                                total_bytes: sender.file_size,
                            }).ok();
                        }
                        FileTransfer::Receiving(recv) => {
                            self.event_tx.send(MsgEvent::FileTransferProgress {
                                contact_id: cid.clone(),
                                transfer_id: *tid,
                                bytes_transferred: recv.bytes_received,
                                total_bytes: recv.file_size,
                            }).ok();
                        }
                        // SendingThreaded progress is reported via sender_events_rx
                        _ => {}
                    }
                }
            }

            // Timeout offers that weren't responded to
            let mut timed_out_offers = Vec::new();
            for ((cid, tid), ft) in &self.file_transfers {
                if let FileTransfer::OfferedWaiting { offered_at, .. } = ft {
                    if offered_at.elapsed().as_secs() >= filetransfer::OFFER_TIMEOUT_SECS {
                        timed_out_offers.push((cid.clone(), *tid));
                    }
                }
            }
            for (cid, tid) in timed_out_offers {
                self.file_transfers.remove(&(cid.clone(), tid));
                self.event_tx.send(MsgEvent::FileTransferFailed {
                    contact_id: cid,
                    transfer_id: tid,
                    reason: "Offer timed out".into(),
                }).ok();
            }

            // Timeout stale transfers (no progress for 30 seconds)
            let mut stale_transfers = Vec::new();
            for ((cid, tid), ft) in &self.file_transfers {
                let is_stale = match ft {
                    FileTransfer::Sending(sender) => {
                        sender.complete_sent && sender.complete_sent_at.elapsed().as_secs() >= filetransfer::STALE_TIMEOUT_SECS
                            && !sender.done
                    }
                    FileTransfer::Receiving(recv) => {
                        recv.last_chunk_time.elapsed().as_secs() >= filetransfer::STALE_TIMEOUT_SECS && !recv.is_complete()
                    }
                    FileTransfer::SendingThreaded { complete_sent, complete_sent_at, .. } => {
                        *complete_sent && complete_sent_at.elapsed().as_secs() >= filetransfer::STALE_TIMEOUT_SECS
                    }
                    FileTransfer::IncomingWaiting { .. } => false,
                    FileTransfer::Hashing { .. } => false,
                    _ => false,
                };
                if is_stale {
                    stale_transfers.push((cid.clone(), *tid));
                }
            }
            for (cid, tid) in stale_transfers {
                if let Some(mut ft) = self.file_transfers.remove(&(cid.clone(), tid)) {
                    match ft {
                        FileTransfer::Receiving(ref mut recv) => {
                            recv.cleanup();
                        }
                        FileTransfer::SendingThreaded { ref cmd_tx, .. } => {
                            cmd_tx.send(crate::filetransfer::sender::SenderThreadCmd::Cancel).ok();
                        }
                        _ => {}
                    }
                    self.event_tx.send(MsgEvent::FileTransferFailed {
                        contact_id: cid,
                        transfer_id: tid,
                        reason: "Transfer timed out".into(),
                    }).ok();
                }
            }
        }

        // ── Avatar send ticking (offer-wait protocol) ──
        if let Some(ref socket) = self.socket {
            let mut avatar_done: Vec<String> = Vec::new();
            for (contact_id, state) in &mut self.avatar_sends {
                if state.sent {
                    // Already sent chunks + got ACK or waiting for ACK
                    if state.sent_at.elapsed() >= AVATAR_SEND_RETRY_INTERVAL {
                        if state.retries >= AVATAR_MAX_RETRIES {
                            avatar_done.push(contact_id.clone());
                            continue;
                        }
                        // Retry: resend offer (not chunks)
                        state.offer_sent = false;
                        state.needs_send = false;
                        state.sent = false;
                        state.retries += 1;
                    }
                    continue;
                }
                if let Some(addr) = self.contact_addrs.get(contact_id) {
                    if let Some(peer) = self.peers.get(addr) {
                        if peer.is_connected() {
                            if !state.offer_sent {
                                // Step 1: send only the offer, wait for NACK
                                protocol::send_avatar_offer(
                                    peer, socket, &state.sha256, state.avatar_data.len() as u32,
                                );
                                state.offer_sent = true;
                                state.sent_at = Instant::now();
                                log_fmt!("[daemon] avatar offer sent to {} ({}B, waiting for NACK)", contact_id, state.avatar_data.len());
                            } else if state.needs_send {
                                // Step 2: NACK received — peer wants the data, send chunks now
                                protocol::send_avatar_chunks(peer, socket, &state.avatar_data);
                                state.sent = true;
                                state.sent_at = Instant::now();
                                log_fmt!("[daemon] avatar chunks sent to {} ({} bytes)", contact_id, state.avatar_data.len());
                            } else if state.sent_at.elapsed() >= AVATAR_SEND_RETRY_INTERVAL {
                                // No ACK/NACK response — retry offer
                                if state.retries >= AVATAR_MAX_RETRIES {
                                    avatar_done.push(contact_id.clone());
                                    continue;
                                }
                                state.offer_sent = false;
                                state.retries += 1;
                                log_fmt!("[daemon] avatar offer retry {} for {}", state.retries, contact_id);
                            }
                        }
                    }
                }
            }
            for cid in avatar_done {
                self.avatar_sends.remove(&cid);
            }
        }

        // Timeout stale avatar receives
        self.avatar_recvs.retain(|_, recv| recv.started_at.elapsed() < AVATAR_RECV_TIMEOUT);

        // ── Group avatar send ticking ──
        if let Some(ref socket) = self.socket {
            let mut grp_avatar_done: Vec<(String, String)> = Vec::new();
            for ((contact_id, group_id), state) in &mut self.group_avatar_sends {
                if state.sent {
                    if state.sent_at.elapsed() >= AVATAR_SEND_RETRY_INTERVAL {
                        if state.retries >= AVATAR_MAX_RETRIES {
                            grp_avatar_done.push((contact_id.clone(), group_id.clone()));
                            continue;
                        }
                        state.sent = false;
                        state.retries += 1;
                    }
                    continue;
                }
                if let Some(addr) = self.contact_addrs.get(contact_id) {
                    if let Some(peer) = self.peers.get(addr) {
                        if peer.is_connected() {
                            protocol::send_group_avatar_offer(
                                peer, socket, group_id, &state.sha256, state.avatar_data.len() as u32,
                            );
                            protocol::send_group_avatar_chunks(peer, socket, group_id, &state.avatar_data);
                            state.sent = true;
                            state.sent_at = Instant::now();
                        }
                    }
                }
            }
            for key in grp_avatar_done {
                self.group_avatar_sends.remove(&key);
            }
        }

        // Timeout stale group avatar receives
        self.group_avatar_recvs.retain(|_, recv| recv.started_at.elapsed() < AVATAR_RECV_TIMEOUT);

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

        for addr in &timed_out {
            if let Some(peer) = self.peers.get(addr) {
                let state_name = match &peer.state {
                    super::session::PeerState::AwaitingHello { .. } => "AwaitingHello",
                    super::session::PeerState::AwaitingIdentity => "AwaitingIdentity",
                    super::session::PeerState::Connected => "Connected",
                };
                log_fmt!("[daemon] peer timeout: {} (state={}, cid={})", addr, state_name, peer.contact_id);
            }
        }

        for addr in timed_out {
            if let Some(peer) = self.peers.remove(&addr) {
                if peer.is_connected() {
                    // Cancel any active SendingThreaded transfers for this contact
                    let cid = peer.contact_id.clone();
                    if !cid.is_empty() {
                        let mut to_cancel = Vec::new();
                        for ((c, tid), ft) in &self.file_transfers {
                            if c == &cid {
                                if let FileTransfer::SendingThreaded { .. } = ft {
                                    to_cancel.push((c.clone(), *tid));
                                }
                            }
                        }
                        for key in to_cancel {
                            if let Some(ft) = self.file_transfers.remove(&key) {
                                if let FileTransfer::SendingThreaded { cmd_tx, .. } = ft {
                                    cmd_tx.send(crate::filetransfer::sender::SenderThreadCmd::Cancel).ok();
                                }
                                self.event_tx.send(MsgEvent::FileTransferFailed {
                                    contact_id: key.0,
                                    transfer_id: key.1,
                                    reason: "Peer disconnected".into(),
                                }).ok();
                            }
                        }
                    }
                    self.event_tx.send(MsgEvent::PeerStatus {
                        contact_id: cid,
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
                            log_fmt!("[daemon] HELLO max retries reached for {} (cid={}) — giving up (1h cooldown)", addr, peer.contact_id);
                            self.failed_contacts.insert(peer.contact_id.clone(), Instant::now());
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
                    .unwrap_or((Instant::now().checked_sub(Duration::from_secs(999)).unwrap_or_else(Instant::now), 0));

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

        // Clean up stale group sync sessions (no ACK received within 30s)
        self.group_sync_out.retain(|key, sync| {
            if sync.last_sent_at.elapsed() > Duration::from_secs(30) {
                log_fmt!("[sync] timeout: giving up sync for grp={} ch={}", &key.1[..8.min(key.1.len())], key.2);
                false
            } else {
                true
            }
        });

        // Retry unacked sync chunks after 5 seconds
        if let Some(ref socket) = self.socket {
            let keys: Vec<_> = self.group_sync_out.keys().cloned().collect();
            for key in keys {
                let should_retry = self.group_sync_out.get(&key)
                    .map(|s| s.last_sent_at.elapsed() > Duration::from_secs(5) && s.retries < 3)
                    .unwrap_or(false);
                if should_retry {
                    let (peer_addr, ref group_id, ref channel_id) = key;
                    if let Some(peer) = self.peers.get(&peer_addr) {
                        if let Some(sync) = self.group_sync_out.get_mut(&key) {
                            if let Some(chunk_data) = sync.chunks.get(sync.next_chunk as usize) {
                                let chunk_data = chunk_data.clone();
                                super::protocol::send_group_sync_data(
                                    peer, socket, group_id, channel_id,
                                    sync.next_chunk, sync.total_chunks, &chunk_data,
                                );
                                sync.last_sent_at = Instant::now();
                                sync.retries += 1;
                            }
                        }
                    }
                }
            }
        }
    }
}
