use std::net::{SocketAddr, UdpSocket};
use std::time::Instant;

use crate::crypto::{
    self, PKT_MSG_ACK, PKT_MSG_BYE, PKT_MSG_CHAT, PKT_MSG_CONFIRM, PKT_MSG_IDENTITY,
    PKT_MSG_REQUEST, PKT_MSG_REQUEST_ACK,
    PKT_MSG_IP_ANNOUNCE, PKT_MSG_PEER_QUERY, PKT_MSG_PEER_RESPONSE,
    PKT_MSG_PRESENCE,
    PKT_MSG_AVATAR_OFFER, PKT_MSG_AVATAR_DATA, PKT_MSG_AVATAR_ACK, PKT_MSG_AVATAR_NACK,
    PKT_GRP_INVITE, PKT_GRP_MSG_CHAT,
    PKT_GRP_INVITE_ACK, PKT_GRP_INVITE_NACK,
    PKT_GRP_UPDATE, PKT_GRP_AVATAR_OFFER, PKT_GRP_AVATAR_DATA, PKT_GRP_AVATAR_ACK,
    PKT_GRP_MEMBER_SYNC,
    PKT_GRP_CALL_SIGNAL,
    PKT_MSG_DELETE_CONTACT, PKT_MSG_DELETE_ACK,
};
use crate::identity::Identity;

use super::session::{PeerSession, PeerState};

/// Handle an incoming MSG_HELLO from an unknown address.
/// Completes our side of the handshake and sends our MSG_HELLO + identity back.
pub fn handle_incoming_hello(
    data: &[u8],
    from: SocketAddr,
    socket: &UdpSocket,
    identity: &Identity,
    nickname: &str,
) -> Option<PeerSession> {
    let peer_ephemeral = crypto::parse_msg_hello(data)?;

    // Generate our ephemeral keypair and reply
    let (our_secret, our_pubkey) = crypto::generate_keypair();
    let hello = crypto::build_msg_hello(&our_pubkey);
    socket.send_to(&hello, from).ok()?;

    // Complete handshake
    let (session, ephemeral_shared) = crypto::complete_handshake(our_secret, &peer_ephemeral);

    // Send our identity (encrypted)
    let mut id_payload = Vec::with_capacity(32 + nickname.len());
    id_payload.extend_from_slice(&identity.pubkey);
    id_payload.extend_from_slice(nickname.as_bytes());
    let pkt = session.encrypt_packet(PKT_MSG_IDENTITY, &id_payload);
    socket.send_to(&pkt, from).ok()?;

    Some(PeerSession {
        contact_id: String::new(), // filled after identity exchange
        peer_pubkey: [0u8; 32],    // filled after identity exchange
        peer_nickname: String::new(),
        peer_addr: from,
        session: Some(session),
        last_activity: Instant::now(),
        state: PeerState::AwaitingIdentity,
        seen_seqs: std::collections::HashSet::new(),
        ephemeral_shared: Some(ephemeral_shared),
        upgraded_session: None,
        identity_confirmed: false,
    })
}

/// Handle a MSG_HELLO response to our outgoing hello (we initiated).
/// Completes handshake and sends our identity.
pub fn handle_hello_response(
    data: &[u8],
    peer: &mut PeerSession,
    socket: &UdpSocket,
    identity: &Identity,
    nickname: &str,
) -> bool {
    let peer_ephemeral = match crypto::parse_msg_hello(data) {
        Some(pk) => pk,
        None => return false,
    };

    let our_secret = match &mut peer.state {
        PeerState::AwaitingHello { our_secret, .. } => our_secret.take(),
        _ => return false,
    };

    let our_secret = match our_secret {
        Some(s) => s,
        None => return false,
    };

    let (session, ephemeral_shared) = crypto::complete_handshake(our_secret, &peer_ephemeral);

    // Send our identity
    let mut id_payload = Vec::with_capacity(32 + nickname.len());
    id_payload.extend_from_slice(&identity.pubkey);
    id_payload.extend_from_slice(nickname.as_bytes());
    let pkt = session.encrypt_packet(PKT_MSG_IDENTITY, &id_payload);
    socket.send_to(&pkt, peer.peer_addr).ok();

    peer.session = Some(session);
    peer.ephemeral_shared = Some(ephemeral_shared);
    peer.upgraded_session = None;
    peer.identity_confirmed = false;
    peer.state = PeerState::AwaitingIdentity;
    peer.touch();
    true
}

/// Handle an incoming MSG_IDENTITY packet (encrypted).
/// Extracts peer's identity pubkey and nickname, derives contact_id.
/// For known contacts, computes an upgraded session key binding identity DH.
/// Returns Ok(true) if an upgraded session was computed (CONFIRM should be sent).
pub fn handle_identity(
    data: &[u8],
    peer: &mut PeerSession,
    identity: &Identity,
    socket: &UdpSocket,
) -> Result<bool, String> {
    let session = peer.session.as_ref().ok_or("no session")?;
    let (pkt_type, plain) = session.decrypt_packet(data).ok_or("decrypt failed")?;
    if pkt_type != PKT_MSG_IDENTITY {
        return Err("not identity packet".into());
    }
    if plain.len() < 32 {
        return Err("identity too short".into());
    }

    let mut peer_id_pubkey = [0u8; 32];
    peer_id_pubkey.copy_from_slice(&plain[..32]);
    let peer_nick = String::from_utf8_lossy(&plain[32..]).to_string();

    peer.peer_pubkey = peer_id_pubkey;
    peer.peer_nickname = peer_nick;
    peer.contact_id = crate::identity::derive_contact_id(&identity.pubkey, &peer_id_pubkey);
    peer.state = PeerState::Connected;
    peer.touch();

    // Phase 2: For known contacts, compute upgraded session key binding identity DH.
    let mut upgraded = false;
    if let Some(ephemeral_shared) = peer.ephemeral_shared.take() {
        if crate::identity::load_contact(&peer_id_pubkey).is_some() {
            let upgraded_session = crypto::upgrade_session_with_identity(
                &ephemeral_shared,
                &identity.secret,
                &peer_id_pubkey,
            );
            // Send CONFIRM encrypted with upgraded key
            let pkt = upgraded_session.encrypt_packet(PKT_MSG_CONFIRM, &[]);
            socket.send_to(&pkt, peer.peer_addr).ok();
            peer.upgraded_session = Some(upgraded_session);
            upgraded = true;
        }
    }

    Ok(upgraded)
}

/// Handle an incoming PKT_MSG_CONFIRM packet.
/// Tries to decrypt with the upgraded session. On success, promotes it.
/// Returns true if confirmation succeeded.
pub fn handle_confirm(data: &[u8], peer: &mut PeerSession) -> bool {
    if let Some(ref upgraded) = peer.upgraded_session {
        if let Some((pkt_type, _)) = upgraded.decrypt_packet(data) {
            if pkt_type == PKT_MSG_CONFIRM {
                // Promote upgraded session to primary
                peer.identity_confirmed = true;
                peer.session = peer.upgraded_session.take();
                return true;
            }
        }
    }
    false
}

/// Send a chat message through an established session.
pub fn send_chat_message(
    peer: &PeerSession,
    socket: &UdpSocket,
    seq: u32,
    text: &str,
) -> Result<(), String> {
    let mut payload = Vec::with_capacity(4 + text.len());
    payload.extend_from_slice(&seq.to_le_bytes());
    payload.extend_from_slice(text.as_bytes());
    let pkt = peer.encrypt_packet(PKT_MSG_CHAT, &payload).ok_or("no session")?;
    socket.send_to(&pkt, peer.peer_addr).map_err(|e| e.to_string())?;
    Ok(())
}

/// Handle an incoming MSG_CHAT packet. Returns (seq, text).
pub fn handle_chat(data: &[u8], peer: &mut PeerSession) -> Option<(u32, String)> {
    let (pkt_type, plain) = peer.decrypt_packet(data)?;
    if pkt_type != PKT_MSG_CHAT || plain.len() < 4 {
        return None;
    }
    let mut seq_bytes = [0u8; 4];
    seq_bytes.copy_from_slice(&plain[..4]);
    let seq = u32::from_le_bytes(seq_bytes);
    let text = String::from_utf8_lossy(&plain[4..]).to_string();
    Some((seq, text))
}

/// Send an ACK for a received message.
pub fn send_ack(peer: &PeerSession, socket: &UdpSocket, seq: u32) {
    if let Some(pkt) = peer.encrypt_packet(PKT_MSG_ACK, &seq.to_le_bytes()) {
        socket.send_to(&pkt, peer.peer_addr).ok();
    }
}

/// Handle an incoming ACK. Returns the acked sequence number.
pub fn handle_ack(data: &[u8], peer: &mut PeerSession) -> Option<u32> {
    let (pkt_type, plain) = peer.decrypt_packet(data)?;
    if pkt_type != PKT_MSG_ACK || plain.len() < 4 {
        return None;
    }
    let mut seq_bytes = [0u8; 4];
    seq_bytes.copy_from_slice(&plain[..4]);
    Some(u32::from_le_bytes(seq_bytes))
}

/// Send a BYE (disconnect) to a peer.
pub fn send_bye(peer: &PeerSession, socket: &UdpSocket) {
    if let Some(pkt) = peer.encrypt_packet(PKT_MSG_BYE, &[]) {
        socket.send_to(&pkt, peer.peer_addr).ok();
    }
}

/// Send a keepalive (ACK for seq 0, used to maintain NAT mappings).
pub fn send_keepalive(peer: &PeerSession, socket: &UdpSocket) {
    if let Some(pkt) = peer.encrypt_packet(PKT_MSG_ACK, &0u32.to_le_bytes()) {
        let _ = socket.send_to(&pkt, peer.peer_addr);
    }
}

/// Initiate a handshake: generate keypair, send MSG_HELLO, return a PeerSession in AwaitingHello.
pub fn initiate_handshake(
    socket: &UdpSocket,
    contact_id: &str,
    peer_addr: SocketAddr,
    peer_pubkey: [u8; 32],
) -> Option<PeerSession> {
    let (our_secret, our_pubkey) = crypto::generate_keypair();
    let hello = crypto::build_msg_hello(&our_pubkey);
    socket.send_to(&hello, peer_addr).ok()?;

    Some(PeerSession {
        contact_id: contact_id.to_string(),
        peer_pubkey,
        peer_nickname: String::new(),
        peer_addr,
        session: None,
        last_activity: Instant::now(),
        state: PeerState::AwaitingHello {
            our_secret: Some(our_secret),
            our_pubkey,
            sent_at: Instant::now(),
        },
        seen_seqs: std::collections::HashSet::new(),
        ephemeral_shared: None,
        upgraded_session: None,
        identity_confirmed: false,
    })
}

/// Send a contact request (PKT_MSG_REQUEST) containing our identity pubkey + nickname.
pub fn send_request(
    peer: &PeerSession,
    socket: &UdpSocket,
    identity: &Identity,
    nickname: &str,
) {
    let mut payload = Vec::with_capacity(32 + nickname.len());
    payload.extend_from_slice(&identity.pubkey);
    payload.extend_from_slice(nickname.as_bytes());
    if let Some(pkt) = peer.encrypt_packet(PKT_MSG_REQUEST, &payload) {
        socket.send_to(&pkt, peer.peer_addr).ok();
    }
}

/// Handle an incoming PKT_MSG_REQUEST. Returns (identity_pubkey, nickname).
pub fn handle_request(data: &[u8], peer: &mut PeerSession) -> Option<([u8; 32], String)> {
    let (pkt_type, plain) = peer.decrypt_packet(data)?;
    if pkt_type != PKT_MSG_REQUEST || plain.len() < 32 {
        return None;
    }
    let mut pubkey = [0u8; 32];
    pubkey.copy_from_slice(&plain[..32]);
    let nickname = String::from_utf8_lossy(&plain[32..]).to_string();
    Some((pubkey, nickname))
}

/// Send a contact request acceptance (PKT_MSG_REQUEST_ACK) with our identity pubkey + nickname.
pub fn send_request_accept(
    peer: &PeerSession,
    socket: &UdpSocket,
    identity: &Identity,
    nickname: &str,
) {
    let mut payload = Vec::with_capacity(32 + nickname.len());
    payload.extend_from_slice(&identity.pubkey);
    payload.extend_from_slice(nickname.as_bytes());
    if let Some(pkt) = peer.encrypt_packet(PKT_MSG_REQUEST_ACK, &payload) {
        socket.send_to(&pkt, peer.peer_addr).ok();
    }
}

/// Handle an incoming PKT_MSG_REQUEST_ACK. Returns (identity_pubkey, nickname).
pub fn handle_request_accept(data: &[u8], peer: &mut PeerSession) -> Option<([u8; 32], String)> {
    let (pkt_type, plain) = peer.decrypt_packet(data)?;
    if pkt_type != PKT_MSG_REQUEST_ACK || plain.len() < 32 {
        return None;
    }
    let mut pubkey = [0u8; 32];
    pubkey.copy_from_slice(&plain[..32]);
    let nickname = String::from_utf8_lossy(&plain[32..]).to_string();
    Some((pubkey, nickname))
}

// ── IP relay protocol ──

/// Send an IP_ANNOUNCE to a peer: our current IP + port + timestamp.
/// Payload: [ip_bytes][0x00][port_bytes][0x00][8-byte timestamp LE]
pub fn send_ip_announce(
    peer: &PeerSession,
    socket: &UdpSocket,
    ip: &str,
    port: &str,
    timestamp: u64,
) {
    let mut payload = Vec::new();
    payload.extend_from_slice(ip.as_bytes());
    payload.push(0x00);
    payload.extend_from_slice(port.as_bytes());
    payload.push(0x00);
    payload.extend_from_slice(&timestamp.to_le_bytes());
    if let Some(pkt) = peer.encrypt_packet(PKT_MSG_IP_ANNOUNCE, &payload) {
        socket.send_to(&pkt, peer.peer_addr).ok();
    }
}

/// Handle an incoming IP_ANNOUNCE. Returns (ip, port, timestamp).
pub fn handle_ip_announce(data: &[u8], peer: &mut PeerSession) -> Option<(String, String, u64)> {
    let (pkt_type, plain) = peer.decrypt_packet(data)?;
    if pkt_type != PKT_MSG_IP_ANNOUNCE {
        return None;
    }
    // Need at least: 1 byte ip + 0x00 + 1 byte port + 0x00 + 8 bytes timestamp
    if plain.len() < 12 {
        return None;
    }
    // Find first null separator (end of IP)
    let ip_end = plain.iter().position(|&b| b == 0x00)?;
    let ip = String::from_utf8_lossy(&plain[..ip_end]).to_string();
    // Find second null separator (end of port)
    let rest = &plain[ip_end + 1..];
    let port_end = rest.iter().position(|&b| b == 0x00)?;
    let port = String::from_utf8_lossy(&rest[..port_end]).to_string();
    // Remaining should be 8 bytes for timestamp
    let ts_data = &rest[port_end + 1..];
    if ts_data.len() < 8 {
        return None;
    }
    let mut ts_bytes = [0u8; 8];
    ts_bytes.copy_from_slice(&ts_data[..8]);
    let timestamp = u64::from_le_bytes(ts_bytes);
    Some((ip, port, timestamp))
}

/// Send a PEER_QUERY to ask a peer about another contact's address.
/// Payload: [32-byte target pubkey]
pub fn send_peer_query(
    peer: &PeerSession,
    socket: &UdpSocket,
    target_pubkey: &[u8; 32],
) {
    if let Some(pkt) = peer.encrypt_packet(PKT_MSG_PEER_QUERY, target_pubkey) {
        socket.send_to(&pkt, peer.peer_addr).ok();
    }
}

/// Handle an incoming PEER_QUERY. Returns the target pubkey being searched.
pub fn handle_peer_query(data: &[u8], peer: &mut PeerSession) -> Option<[u8; 32]> {
    let (pkt_type, plain) = peer.decrypt_packet(data)?;
    if pkt_type != PKT_MSG_PEER_QUERY || plain.len() < 32 {
        return None;
    }
    let mut pubkey = [0u8; 32];
    pubkey.copy_from_slice(&plain[..32]);
    Some(pubkey)
}

/// Send a PEER_RESPONSE with the found peer's address info.
/// Payload: [32-byte pubkey][ip_bytes][0x00][port_bytes][0x00][8-byte timestamp LE]
pub fn send_peer_response(
    peer: &PeerSession,
    socket: &UdpSocket,
    target_pubkey: &[u8; 32],
    ip: &str,
    port: &str,
    timestamp: u64,
) {
    let mut payload = Vec::with_capacity(32 + ip.len() + port.len() + 10);
    payload.extend_from_slice(target_pubkey);
    payload.extend_from_slice(ip.as_bytes());
    payload.push(0x00);
    payload.extend_from_slice(port.as_bytes());
    payload.push(0x00);
    payload.extend_from_slice(&timestamp.to_le_bytes());
    if let Some(pkt) = peer.encrypt_packet(PKT_MSG_PEER_RESPONSE, &payload) {
        socket.send_to(&pkt, peer.peer_addr).ok();
    }
}

/// Handle an incoming PEER_RESPONSE. Returns (target_pubkey, ip, port, timestamp).
pub fn handle_peer_response(data: &[u8], peer: &mut PeerSession) -> Option<([u8; 32], String, String, u64)> {
    let (pkt_type, plain) = peer.decrypt_packet(data)?;
    if pkt_type != PKT_MSG_PEER_RESPONSE {
        return None;
    }
    // Need at least: 32 pubkey + 1 ip + 0x00 + 1 port + 0x00 + 8 timestamp
    if plain.len() < 44 {
        return None;
    }
    let mut pubkey = [0u8; 32];
    pubkey.copy_from_slice(&plain[..32]);
    let rest = &plain[32..];
    let ip_end = rest.iter().position(|&b| b == 0x00)?;
    let ip = String::from_utf8_lossy(&rest[..ip_end]).to_string();
    let rest2 = &rest[ip_end + 1..];
    let port_end = rest2.iter().position(|&b| b == 0x00)?;
    let port = String::from_utf8_lossy(&rest2[..port_end]).to_string();
    let ts_data = &rest2[port_end + 1..];
    if ts_data.len() < 8 {
        return None;
    }
    let mut ts_bytes = [0u8; 8];
    ts_bytes.copy_from_slice(&ts_data[..8]);
    let timestamp = u64::from_le_bytes(ts_bytes);
    Some((pubkey, ip, port, timestamp))
}

// ── Presence protocol ──

/// Send a PRESENCE packet to a peer. Payload: 1 byte (0x01=Online, 0x02=Away).
pub fn send_presence(
    peer: &PeerSession,
    socket: &UdpSocket,
    status: super::PresenceStatus,
) {
    let byte = match status {
        super::PresenceStatus::Online => 0x01,
        super::PresenceStatus::Away => 0x02,
        super::PresenceStatus::Offline => return, // never sent, inferred from disconnect
    };
    if let Some(pkt) = peer.encrypt_packet(PKT_MSG_PRESENCE, &[byte]) {
        socket.send_to(&pkt, peer.peer_addr).ok();
    }
}

/// Handle an incoming PRESENCE packet. Returns the presence status.
pub fn handle_presence(data: &[u8], peer: &mut PeerSession) -> Option<super::PresenceStatus> {
    let (pkt_type, plain) = peer.decrypt_packet(data)?;
    if pkt_type != PKT_MSG_PRESENCE || plain.is_empty() {
        return None;
    }
    match plain[0] {
        0x01 => Some(super::PresenceStatus::Online),
        0x02 => Some(super::PresenceStatus::Away),
        _ => None,
    }
}

// ── Avatar protocol ──

/// Send an AVATAR_OFFER to a peer. Payload: [32B sha256][4B total_size LE].
pub fn send_avatar_offer(
    peer: &PeerSession,
    socket: &UdpSocket,
    sha256: &[u8; 32],
    total_size: u32,
) {
    let mut payload = Vec::with_capacity(36);
    payload.extend_from_slice(sha256);
    payload.extend_from_slice(&total_size.to_le_bytes());
    if let Some(pkt) = peer.encrypt_packet(PKT_MSG_AVATAR_OFFER, &payload) {
        socket.send_to(&pkt, peer.peer_addr).ok();
    }
}

/// Handle an incoming AVATAR_OFFER. Returns (sha256, total_size).
pub fn handle_avatar_offer(data: &[u8], peer: &mut PeerSession) -> Option<([u8; 32], u32)> {
    let (pkt_type, plain) = peer.decrypt_packet(data)?;
    if pkt_type != PKT_MSG_AVATAR_OFFER || plain.len() < 36 {
        return None;
    }
    let mut sha256 = [0u8; 32];
    sha256.copy_from_slice(&plain[..32]);
    let mut size_bytes = [0u8; 4];
    size_bytes.copy_from_slice(&plain[32..36]);
    let total_size = u32::from_le_bytes(size_bytes);
    Some((sha256, total_size))
}

/// Send all avatar data chunks to a peer.
/// Payload per chunk: [2B chunk_index LE][up to 1200B data].
pub fn send_avatar_chunks(
    peer: &PeerSession,
    socket: &UdpSocket,
    avatar_data: &[u8],
) {
    let chunk_size = super::daemon::AVATAR_CHUNK_SIZE;
    let total_chunks = (avatar_data.len() + chunk_size - 1) / chunk_size;
    for i in 0..total_chunks {
        let start = i * chunk_size;
        let end = (start + chunk_size).min(avatar_data.len());
        let chunk = &avatar_data[start..end];
        let mut payload = Vec::with_capacity(2 + chunk.len());
        payload.extend_from_slice(&(i as u16).to_le_bytes());
        payload.extend_from_slice(chunk);
        if let Some(pkt) = peer.encrypt_packet(PKT_MSG_AVATAR_DATA, &payload) {
            socket.send_to(&pkt, peer.peer_addr).ok();
        }
    }
}

/// Handle an incoming AVATAR_DATA chunk. Returns (chunk_index, data).
pub fn handle_avatar_data(data: &[u8], peer: &mut PeerSession) -> Option<(u16, Vec<u8>)> {
    let (pkt_type, plain) = peer.decrypt_packet(data)?;
    if pkt_type != PKT_MSG_AVATAR_DATA || plain.len() < 3 {
        return None;
    }
    let mut idx_bytes = [0u8; 2];
    idx_bytes.copy_from_slice(&plain[..2]);
    let chunk_index = u16::from_le_bytes(idx_bytes);
    let chunk_data = plain[2..].to_vec();
    Some((chunk_index, chunk_data))
}

/// Send an AVATAR_ACK to confirm receipt. Payload: [32B sha256].
pub fn send_avatar_ack(
    peer: &PeerSession,
    socket: &UdpSocket,
    sha256: &[u8; 32],
) {
    if let Some(pkt) = peer.encrypt_packet(PKT_MSG_AVATAR_ACK, sha256) {
        socket.send_to(&pkt, peer.peer_addr).ok();
    }
}

/// Handle an incoming AVATAR_ACK. Returns the sha256 from the ACK.
pub fn handle_avatar_ack(data: &[u8], peer: &mut PeerSession) -> Option<[u8; 32]> {
    let (pkt_type, plain) = peer.decrypt_packet(data)?;
    if pkt_type != PKT_MSG_AVATAR_ACK || plain.len() < 32 {
        return None;
    }
    let mut sha256 = [0u8; 32];
    sha256.copy_from_slice(&plain[..32]);
    Some(sha256)
}

/// Send an AVATAR_NACK to request avatar data. Payload: [32B sha256].
pub fn send_avatar_nack(
    peer: &PeerSession,
    socket: &UdpSocket,
    sha256: &[u8; 32],
) {
    if let Some(pkt) = peer.encrypt_packet(PKT_MSG_AVATAR_NACK, sha256) {
        socket.send_to(&pkt, peer.peer_addr).ok();
    }
}

/// Handle an incoming AVATAR_NACK. Returns the sha256 from the NACK.
pub fn handle_avatar_nack(data: &[u8], peer: &mut PeerSession) -> Option<[u8; 32]> {
    let (pkt_type, plain) = peer.decrypt_packet(data)?;
    if pkt_type != PKT_MSG_AVATAR_NACK || plain.len() < 32 {
        return None;
    }
    let mut sha256 = [0u8; 32];
    sha256.copy_from_slice(&plain[..32]);
    Some(sha256)
}

/// Send a group invite (lite invite JSON) via existing pairwise session.
pub fn send_group_invite(
    peer: &PeerSession,
    socket: &UdpSocket,
    invite_json: &[u8],
) -> Result<(), String> {
    let pkt = peer.encrypt_packet(PKT_GRP_INVITE, invite_json).ok_or("no session")?;
    socket.send_to(&pkt, peer.peer_addr).map_err(|e| e.to_string())?;
    Ok(())
}

/// Handle an incoming group invite. Returns the raw lite invite JSON bytes.
pub fn handle_group_invite(data: &[u8], peer: &mut PeerSession) -> Option<Vec<u8>> {
    let (pkt_type, plain) = peer.decrypt_packet(data)?;
    if pkt_type != PKT_GRP_INVITE || plain.is_empty() {
        return None;
    }
    Some(plain)
}

/// Send a single group member sync packet. Payload: group_id + '\n' + member wire JSON.
pub fn send_group_member_sync(
    peer: &PeerSession,
    socket: &UdpSocket,
    group_id: &str,
    member_wire_json: &[u8],
) -> Result<(), String> {
    let mut payload = Vec::with_capacity(group_id.len() + 1 + member_wire_json.len());
    payload.extend_from_slice(group_id.as_bytes());
    payload.push(b'\n');
    payload.extend_from_slice(member_wire_json);
    let pkt = peer.encrypt_packet(PKT_GRP_MEMBER_SYNC, &payload).ok_or("no session")?;
    socket.send_to(&pkt, peer.peer_addr).map_err(|e| e.to_string())?;
    Ok(())
}

/// Handle an incoming member sync. Returns (group_id, member_wire_json_bytes).
pub fn handle_group_member_sync(data: &[u8], peer: &mut PeerSession) -> Option<(String, Vec<u8>)> {
    let (pkt_type, plain) = peer.decrypt_packet(data)?;
    if pkt_type != PKT_GRP_MEMBER_SYNC || plain.is_empty() {
        return None;
    }
    let nl_pos = plain.iter().position(|&b| b == b'\n')?;
    let group_id = String::from_utf8_lossy(&plain[..nl_pos]).to_string();
    let member_json = plain[nl_pos + 1..].to_vec();
    Some((group_id, member_json))
}

/// Send a group chat message to a peer via the messaging daemon.
/// Payload: group_id + '\n' + channel_id + '\n' + text
pub fn send_group_chat(
    peer: &PeerSession,
    socket: &UdpSocket,
    group_id: &str,
    channel_id: &str,
    text: &str,
) -> Result<(), String> {
    let payload = format!("{}\n{}\n{}", group_id, channel_id, text);
    let pkt = peer.encrypt_packet(PKT_GRP_MSG_CHAT, payload.as_bytes()).ok_or("no session")?;
    socket.send_to(&pkt, peer.peer_addr).map_err(|e| e.to_string())?;
    Ok(())
}

/// Handle an incoming group chat message. Returns (group_id, channel_id, text).
/// Backward compatible: if only one '\n' (old format), channel_id defaults to "general".
pub fn handle_group_chat(data: &[u8], peer: &mut PeerSession) -> Option<(String, String, String)> {
    let (pkt_type, plain) = peer.decrypt_packet(data)?;
    if pkt_type != PKT_GRP_MSG_CHAT || plain.is_empty() {
        return None;
    }
    let s = String::from_utf8_lossy(&plain);
    // Try 3-part split first (new format)
    let parts: Vec<&str> = s.splitn(3, '\n').collect();
    match parts.len() {
        3 => Some((parts[0].to_string(), parts[1].to_string(), parts[2].to_string())),
        2 => Some((parts[0].to_string(), "general".to_string(), parts[1].to_string())),
        _ => None,
    }
}

/// Send a group invite ACK (peer accepted). Payload: group_id UTF-8 string.
pub fn send_group_invite_ack(
    peer: &PeerSession,
    socket: &UdpSocket,
    group_id: &str,
) -> Result<(), String> {
    let pkt = peer.encrypt_packet(PKT_GRP_INVITE_ACK, group_id.as_bytes()).ok_or("no session")?;
    socket.send_to(&pkt, peer.peer_addr).map_err(|e| e.to_string())?;
    Ok(())
}

/// Handle an incoming group invite ACK. Returns the group_id.
pub fn handle_group_invite_ack(data: &[u8], peer: &mut PeerSession) -> Option<String> {
    let (pkt_type, plain) = peer.decrypt_packet(data)?;
    if pkt_type != PKT_GRP_INVITE_ACK || plain.is_empty() {
        return None;
    }
    Some(String::from_utf8_lossy(&plain).to_string())
}

/// Send a group invite NACK (peer rejected). Payload: group_id UTF-8 string.
pub fn send_group_invite_nack(
    peer: &PeerSession,
    socket: &UdpSocket,
    group_id: &str,
) -> Result<(), String> {
    let pkt = peer.encrypt_packet(PKT_GRP_INVITE_NACK, group_id.as_bytes()).ok_or("no session")?;
    socket.send_to(&pkt, peer.peer_addr).map_err(|e| e.to_string())?;
    Ok(())
}

/// Handle an incoming group invite NACK. Returns the group_id.
pub fn handle_group_invite_nack(data: &[u8], peer: &mut PeerSession) -> Option<String> {
    let (pkt_type, plain) = peer.decrypt_packet(data)?;
    if pkt_type != PKT_GRP_INVITE_NACK || plain.is_empty() {
        return None;
    }
    Some(String::from_utf8_lossy(&plain).to_string())
}

// ── Group update protocol ──

/// Send a group metadata update (full Group JSON) via existing pairwise session.
pub fn send_group_update(
    peer: &PeerSession,
    socket: &UdpSocket,
    group_json: &[u8],
) -> Result<(), String> {
    let pkt = peer.encrypt_packet(PKT_GRP_UPDATE, group_json).ok_or("no session")?;
    socket.send_to(&pkt, peer.peer_addr).map_err(|e| e.to_string())?;
    Ok(())
}

/// Handle an incoming group update. Returns the raw Group JSON bytes.
pub fn handle_group_update(data: &[u8], peer: &mut PeerSession) -> Option<Vec<u8>> {
    let (pkt_type, plain) = peer.decrypt_packet(data)?;
    if pkt_type != PKT_GRP_UPDATE || plain.is_empty() {
        return None;
    }
    Some(plain)
}

// ── Group avatar protocol ──

/// Send a group AVATAR_OFFER. Payload: group_id + '\n' + [32B sha256][4B total_size LE].
pub fn send_group_avatar_offer(
    peer: &PeerSession,
    socket: &UdpSocket,
    group_id: &str,
    sha256: &[u8; 32],
    total_size: u32,
) {
    let mut payload = Vec::with_capacity(group_id.len() + 1 + 36);
    payload.extend_from_slice(group_id.as_bytes());
    payload.push(b'\n');
    payload.extend_from_slice(sha256);
    payload.extend_from_slice(&total_size.to_le_bytes());
    if let Some(pkt) = peer.encrypt_packet(PKT_GRP_AVATAR_OFFER, &payload) {
        socket.send_to(&pkt, peer.peer_addr).ok();
    }
}

/// Handle an incoming group AVATAR_OFFER. Returns (group_id, sha256, total_size).
pub fn handle_group_avatar_offer(data: &[u8], peer: &mut PeerSession) -> Option<(String, [u8; 32], u32)> {
    let (pkt_type, plain) = peer.decrypt_packet(data)?;
    if pkt_type != PKT_GRP_AVATAR_OFFER {
        return None;
    }
    // Find newline separator between group_id and sha256+size
    let nl_pos = plain.iter().position(|&b| b == b'\n')?;
    let group_id = String::from_utf8_lossy(&plain[..nl_pos]).to_string();
    let rest = &plain[nl_pos + 1..];
    if rest.len() < 36 {
        return None;
    }
    let mut sha256 = [0u8; 32];
    sha256.copy_from_slice(&rest[..32]);
    let mut size_bytes = [0u8; 4];
    size_bytes.copy_from_slice(&rest[32..36]);
    let total_size = u32::from_le_bytes(size_bytes);
    Some((group_id, sha256, total_size))
}

/// Send all group avatar data chunks to a peer.
/// Payload per chunk: group_id + '\n' + [2B chunk_index LE][up to 1200B data].
pub fn send_group_avatar_chunks(
    peer: &PeerSession,
    socket: &UdpSocket,
    group_id: &str,
    avatar_data: &[u8],
) {
    let chunk_size = super::daemon::AVATAR_CHUNK_SIZE;
    let total_chunks = (avatar_data.len() + chunk_size - 1) / chunk_size;
    for i in 0..total_chunks {
        let start = i * chunk_size;
        let end = (start + chunk_size).min(avatar_data.len());
        let chunk = &avatar_data[start..end];
        let mut payload = Vec::with_capacity(group_id.len() + 1 + 2 + chunk.len());
        payload.extend_from_slice(group_id.as_bytes());
        payload.push(b'\n');
        payload.extend_from_slice(&(i as u16).to_le_bytes());
        payload.extend_from_slice(chunk);
        if let Some(pkt) = peer.encrypt_packet(PKT_GRP_AVATAR_DATA, &payload) {
            socket.send_to(&pkt, peer.peer_addr).ok();
        }
    }
}

/// Handle an incoming group AVATAR_DATA chunk. Returns (group_id, chunk_index, data).
pub fn handle_group_avatar_data(data: &[u8], peer: &mut PeerSession) -> Option<(String, u16, Vec<u8>)> {
    let (pkt_type, plain) = peer.decrypt_packet(data)?;
    if pkt_type != PKT_GRP_AVATAR_DATA {
        return None;
    }
    let nl_pos = plain.iter().position(|&b| b == b'\n')?;
    let group_id = String::from_utf8_lossy(&plain[..nl_pos]).to_string();
    let rest = &plain[nl_pos + 1..];
    if rest.len() < 3 {
        return None;
    }
    let mut idx_bytes = [0u8; 2];
    idx_bytes.copy_from_slice(&rest[..2]);
    let chunk_index = u16::from_le_bytes(idx_bytes);
    let chunk_data = rest[2..].to_vec();
    Some((group_id, chunk_index, chunk_data))
}

/// Send a GROUP_AVATAR_ACK. Payload: group_id + '\n' + [32B sha256].
pub fn send_group_avatar_ack(
    peer: &PeerSession,
    socket: &UdpSocket,
    group_id: &str,
    sha256: &[u8; 32],
) {
    let mut payload = Vec::with_capacity(group_id.len() + 1 + 32);
    payload.extend_from_slice(group_id.as_bytes());
    payload.push(b'\n');
    payload.extend_from_slice(sha256);
    if let Some(pkt) = peer.encrypt_packet(PKT_GRP_AVATAR_ACK, &payload) {
        socket.send_to(&pkt, peer.peer_addr).ok();
    }
}

/// Handle an incoming GROUP_AVATAR_ACK. Returns (group_id, sha256).
pub fn handle_group_avatar_ack(data: &[u8], peer: &mut PeerSession) -> Option<(String, [u8; 32])> {
    let (pkt_type, plain) = peer.decrypt_packet(data)?;
    if pkt_type != PKT_GRP_AVATAR_ACK {
        return None;
    }
    let nl_pos = plain.iter().position(|&b| b == b'\n')?;
    let group_id = String::from_utf8_lossy(&plain[..nl_pos]).to_string();
    let rest = &plain[nl_pos + 1..];
    if rest.len() < 32 {
        return None;
    }
    let mut sha256 = [0u8; 32];
    sha256.copy_from_slice(&rest[..32]);
    Some((group_id, sha256))
}

/// Send a group call presence signal. Payload: group_id + '\n' + channel_id + '\n' + active_byte + call_mode_byte
pub fn send_call_signal(
    peer: &PeerSession,
    socket: &UdpSocket,
    group_id: &str,
    channel_id: &str,
    active: bool,
    call_mode: u8,
) -> Result<(), String> {
    let mut payload = format!("{}\n{}\n", group_id, channel_id).into_bytes();
    payload.push(if active { 1 } else { 0 });
    payload.push(call_mode);
    let pkt = peer.encrypt_packet(PKT_GRP_CALL_SIGNAL, &payload).ok_or("no session")?;
    socket.send_to(&pkt, peer.peer_addr).map_err(|e| e.to_string())?;
    Ok(())
}

/// Handle an incoming group call signal. Returns (group_id, channel_id, active, call_mode).
pub fn handle_call_signal(data: &[u8], peer: &mut PeerSession) -> Option<(String, String, bool, u8)> {
    let (pkt_type, plain) = peer.decrypt_packet(data)?;
    if pkt_type != PKT_GRP_CALL_SIGNAL || plain.len() < 4 {
        return None;
    }
    let s = String::from_utf8_lossy(&plain[..plain.len() - 2]);
    let parts: Vec<&str> = s.splitn(3, '\n').collect();
    if parts.len() < 2 { return None; }
    let active = plain[plain.len() - 2] == 1;
    let call_mode = plain[plain.len() - 1];
    Some((parts[0].to_string(), parts[1].to_string(), active, call_mode))
}

/// Send a contact deletion signal to a peer.
pub fn send_delete_contact(peer: &PeerSession, socket: &UdpSocket) {
    if let Some(pkt) = peer.encrypt_packet(PKT_MSG_DELETE_CONTACT, &[]) {
        socket.send_to(&pkt, peer.peer_addr).ok();
    }
}

/// Send a deletion acknowledgement to a peer.
pub fn send_delete_ack(peer: &PeerSession, socket: &UdpSocket) {
    if let Some(pkt) = peer.encrypt_packet(PKT_MSG_DELETE_ACK, &[]) {
        socket.send_to(&pkt, peer.peer_addr).ok();
    }
}
