use std::net::{SocketAddr, UdpSocket};
use std::time::Instant;

use crate::crypto::{
    self, PKT_MSG_ACK, PKT_MSG_BYE, PKT_MSG_CHAT, PKT_MSG_IDENTITY,
    PKT_MSG_REQUEST, PKT_MSG_REQUEST_ACK,
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
    let session = crypto::complete_handshake(our_secret, &peer_ephemeral);

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
        state: PeerState::AwaitingIdentity { sent_at: Instant::now() },
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

    let (our_secret, sent_at) = match &mut peer.state {
        PeerState::AwaitingHello { our_secret, sent_at, .. } => {
            (our_secret.take(), *sent_at)
        }
        _ => return false,
    };

    let our_secret = match our_secret {
        Some(s) => s,
        None => return false,
    };

    let session = crypto::complete_handshake(our_secret, &peer_ephemeral);

    // Send our identity
    let mut id_payload = Vec::with_capacity(32 + nickname.len());
    id_payload.extend_from_slice(&identity.pubkey);
    id_payload.extend_from_slice(nickname.as_bytes());
    let pkt = session.encrypt_packet(PKT_MSG_IDENTITY, &id_payload);
    socket.send_to(&pkt, peer.peer_addr).ok();

    peer.session = Some(session);
    peer.state = PeerState::AwaitingIdentity { sent_at };
    peer.touch();
    true
}

/// Handle an incoming MSG_IDENTITY packet (encrypted).
/// Extracts peer's identity pubkey and nickname, derives contact_id.
pub fn handle_identity(
    data: &[u8],
    peer: &mut PeerSession,
    identity: &Identity,
) -> Result<(), String> {
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
    Ok(())
}

/// Send a chat message through an established session.
pub fn send_chat_message(
    peer: &PeerSession,
    socket: &UdpSocket,
    seq: u32,
    text: &str,
) -> Result<(), String> {
    let session = peer.session.as_ref().ok_or("no session")?;
    let mut payload = Vec::with_capacity(4 + text.len());
    payload.extend_from_slice(&seq.to_le_bytes());
    payload.extend_from_slice(text.as_bytes());
    let pkt = session.encrypt_packet(PKT_MSG_CHAT, &payload);
    socket.send_to(&pkt, peer.peer_addr).map_err(|e| e.to_string())?;
    Ok(())
}

/// Handle an incoming MSG_CHAT packet. Returns (seq, text).
pub fn handle_chat(data: &[u8], peer: &PeerSession) -> Option<(u32, String)> {
    let session = peer.session.as_ref()?;
    let (pkt_type, plain) = session.decrypt_packet(data)?;
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
    if let Some(session) = &peer.session {
        let pkt = session.encrypt_packet(PKT_MSG_ACK, &seq.to_le_bytes());
        socket.send_to(&pkt, peer.peer_addr).ok();
    }
}

/// Handle an incoming ACK. Returns the acked sequence number.
pub fn handle_ack(data: &[u8], peer: &PeerSession) -> Option<u32> {
    let session = peer.session.as_ref()?;
    let (pkt_type, plain) = session.decrypt_packet(data)?;
    if pkt_type != PKT_MSG_ACK || plain.len() < 4 {
        return None;
    }
    let mut seq_bytes = [0u8; 4];
    seq_bytes.copy_from_slice(&plain[..4]);
    Some(u32::from_le_bytes(seq_bytes))
}

/// Send a BYE (disconnect) to a peer.
pub fn send_bye(peer: &PeerSession, socket: &UdpSocket) {
    if let Some(session) = &peer.session {
        let pkt = session.encrypt_packet(PKT_MSG_BYE, &[]);
        socket.send_to(&pkt, peer.peer_addr).ok();
    }
}

/// Send a keepalive (ACK for seq 0, used to maintain NAT mappings).
pub fn send_keepalive(peer: &PeerSession, socket: &UdpSocket) {
    if let Some(session) = &peer.session {
        let pkt = session.encrypt_packet(PKT_MSG_ACK, &0u32.to_le_bytes());
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
    })
}

/// Send a contact request (PKT_MSG_REQUEST) containing our identity pubkey + nickname.
pub fn send_request(
    peer: &PeerSession,
    socket: &UdpSocket,
    identity: &Identity,
    nickname: &str,
) {
    if let Some(session) = &peer.session {
        let mut payload = Vec::with_capacity(32 + nickname.len());
        payload.extend_from_slice(&identity.pubkey);
        payload.extend_from_slice(nickname.as_bytes());
        let pkt = session.encrypt_packet(PKT_MSG_REQUEST, &payload);
        socket.send_to(&pkt, peer.peer_addr).ok();
    }
}

/// Handle an incoming PKT_MSG_REQUEST. Returns (identity_pubkey, nickname).
pub fn handle_request(data: &[u8], peer: &PeerSession) -> Option<([u8; 32], String)> {
    let session = peer.session.as_ref()?;
    let (pkt_type, plain) = session.decrypt_packet(data)?;
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
    if let Some(session) = &peer.session {
        let mut payload = Vec::with_capacity(32 + nickname.len());
        payload.extend_from_slice(&identity.pubkey);
        payload.extend_from_slice(nickname.as_bytes());
        let pkt = session.encrypt_packet(PKT_MSG_REQUEST_ACK, &payload);
        socket.send_to(&pkt, peer.peer_addr).ok();
    }
}

/// Handle an incoming PKT_MSG_REQUEST_ACK. Returns (identity_pubkey, nickname).
pub fn handle_request_accept(data: &[u8], peer: &PeerSession) -> Option<([u8; 32], String)> {
    let session = peer.session.as_ref()?;
    let (pkt_type, plain) = session.decrypt_packet(data)?;
    if pkt_type != PKT_MSG_REQUEST_ACK || plain.len() < 32 {
        return None;
    }
    let mut pubkey = [0u8; 32];
    pubkey.copy_from_slice(&plain[..32]);
    let nickname = String::from_utf8_lossy(&plain[32..]).to_string();
    Some((pubkey, nickname))
}
