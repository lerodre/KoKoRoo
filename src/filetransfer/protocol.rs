use std::net::UdpSocket;

use crate::crypto::{
    PKT_MSG_FILE_OFFER, PKT_MSG_FILE_ACCEPT, PKT_MSG_FILE_REJECT,
    PKT_MSG_FILE_CHUNK, PKT_MSG_FILE_ACK, PKT_MSG_FILE_COMPLETE, PKT_MSG_FILE_CANCEL,
};
use crate::messaging::session::PeerSession;

/// Send a FILE_OFFER: [4B transfer_id][8B file_size LE][32B sha256][filename UTF-8]
pub fn send_file_offer(
    peer: &PeerSession,
    socket: &UdpSocket,
    transfer_id: u32,
    file_size: u64,
    sha256: &[u8; 32],
    filename: &str,
) {
    if let Some(session) = &peer.session {
        let mut payload = Vec::with_capacity(4 + 8 + 32 + filename.len());
        payload.extend_from_slice(&transfer_id.to_le_bytes());
        payload.extend_from_slice(&file_size.to_le_bytes());
        payload.extend_from_slice(sha256);
        payload.extend_from_slice(filename.as_bytes());
        let pkt = session.encrypt_packet(PKT_MSG_FILE_OFFER, &payload);
        socket.send_to(&pkt, peer.peer_addr).ok();
    }
}

/// Handle an incoming FILE_OFFER. Returns (transfer_id, file_size, sha256, filename).
pub fn handle_file_offer(data: &[u8], peer: &PeerSession) -> Option<(u32, u64, [u8; 32], String)> {
    let session = peer.session.as_ref()?;
    let (pkt_type, plain) = session.decrypt_packet(data)?;
    if pkt_type != PKT_MSG_FILE_OFFER || plain.len() < 44 {
        return None;
    }
    let transfer_id = u32::from_le_bytes([plain[0], plain[1], plain[2], plain[3]]);
    let file_size = u64::from_le_bytes([
        plain[4], plain[5], plain[6], plain[7],
        plain[8], plain[9], plain[10], plain[11],
    ]);
    let mut sha256 = [0u8; 32];
    sha256.copy_from_slice(&plain[12..44]);
    let filename = String::from_utf8_lossy(&plain[44..]).to_string();
    Some((transfer_id, file_size, sha256, filename))
}

/// Send a FILE_ACCEPT: [4B transfer_id]
pub fn send_file_accept(peer: &PeerSession, socket: &UdpSocket, transfer_id: u32) {
    if let Some(session) = &peer.session {
        let pkt = session.encrypt_packet(PKT_MSG_FILE_ACCEPT, &transfer_id.to_le_bytes());
        socket.send_to(&pkt, peer.peer_addr).ok();
    }
}

/// Handle an incoming FILE_ACCEPT. Returns transfer_id.
pub fn handle_file_accept(data: &[u8], peer: &PeerSession) -> Option<u32> {
    let session = peer.session.as_ref()?;
    let (pkt_type, plain) = session.decrypt_packet(data)?;
    if pkt_type != PKT_MSG_FILE_ACCEPT || plain.len() < 4 {
        return None;
    }
    Some(u32::from_le_bytes([plain[0], plain[1], plain[2], plain[3]]))
}

/// Send a FILE_REJECT: [4B transfer_id]
pub fn send_file_reject(peer: &PeerSession, socket: &UdpSocket, transfer_id: u32) {
    if let Some(session) = &peer.session {
        let pkt = session.encrypt_packet(PKT_MSG_FILE_REJECT, &transfer_id.to_le_bytes());
        socket.send_to(&pkt, peer.peer_addr).ok();
    }
}

/// Handle an incoming FILE_REJECT. Returns transfer_id.
pub fn handle_file_reject(data: &[u8], peer: &PeerSession) -> Option<u32> {
    let session = peer.session.as_ref()?;
    let (pkt_type, plain) = session.decrypt_packet(data)?;
    if pkt_type != PKT_MSG_FILE_REJECT || plain.len() < 4 {
        return None;
    }
    Some(u32::from_le_bytes([plain[0], plain[1], plain[2], plain[3]]))
}

/// Send a FILE_CHUNK: [4B transfer_id][4B chunk_index LE][data]
pub fn send_file_chunk(
    peer: &PeerSession,
    socket: &UdpSocket,
    transfer_id: u32,
    chunk_index: u32,
    data: &[u8],
) {
    if let Some(session) = &peer.session {
        let mut payload = Vec::with_capacity(8 + data.len());
        payload.extend_from_slice(&transfer_id.to_le_bytes());
        payload.extend_from_slice(&chunk_index.to_le_bytes());
        payload.extend_from_slice(data);
        let pkt = session.encrypt_packet(PKT_MSG_FILE_CHUNK, &payload);
        socket.send_to(&pkt, peer.peer_addr).ok();
    }
}

/// Handle an incoming FILE_CHUNK. Returns (transfer_id, chunk_index, data).
pub fn handle_file_chunk(data: &[u8], peer: &PeerSession) -> Option<(u32, u32, Vec<u8>)> {
    let session = peer.session.as_ref()?;
    let (pkt_type, plain) = session.decrypt_packet(data)?;
    if pkt_type != PKT_MSG_FILE_CHUNK || plain.len() < 8 {
        return None;
    }
    let transfer_id = u32::from_le_bytes([plain[0], plain[1], plain[2], plain[3]]);
    let chunk_index = u32::from_le_bytes([plain[4], plain[5], plain[6], plain[7]]);
    let chunk_data = plain[8..].to_vec();
    Some((transfer_id, chunk_index, chunk_data))
}

/// Send a FILE_ACK: [4B transfer_id][4B ack_through LE]
pub fn send_file_ack(peer: &PeerSession, socket: &UdpSocket, transfer_id: u32, ack_through: u32) {
    if let Some(session) = &peer.session {
        let mut payload = [0u8; 8];
        payload[..4].copy_from_slice(&transfer_id.to_le_bytes());
        payload[4..].copy_from_slice(&ack_through.to_le_bytes());
        let pkt = session.encrypt_packet(PKT_MSG_FILE_ACK, &payload);
        socket.send_to(&pkt, peer.peer_addr).ok();
    }
}

/// Handle an incoming FILE_ACK. Returns (transfer_id, ack_through).
pub fn handle_file_ack(data: &[u8], peer: &PeerSession) -> Option<(u32, u32)> {
    let session = peer.session.as_ref()?;
    let (pkt_type, plain) = session.decrypt_packet(data)?;
    if pkt_type != PKT_MSG_FILE_ACK || plain.len() < 8 {
        return None;
    }
    let transfer_id = u32::from_le_bytes([plain[0], plain[1], plain[2], plain[3]]);
    let ack_through = u32::from_le_bytes([plain[4], plain[5], plain[6], plain[7]]);
    Some((transfer_id, ack_through))
}

/// Send a FILE_COMPLETE: [4B transfer_id][32B sha256]
pub fn send_file_complete(
    peer: &PeerSession,
    socket: &UdpSocket,
    transfer_id: u32,
    sha256: &[u8; 32],
) {
    if let Some(session) = &peer.session {
        let mut payload = Vec::with_capacity(36);
        payload.extend_from_slice(&transfer_id.to_le_bytes());
        payload.extend_from_slice(sha256);
        let pkt = session.encrypt_packet(PKT_MSG_FILE_COMPLETE, &payload);
        socket.send_to(&pkt, peer.peer_addr).ok();
    }
}

/// Handle an incoming FILE_COMPLETE. Returns (transfer_id, sha256).
pub fn handle_file_complete(data: &[u8], peer: &PeerSession) -> Option<(u32, [u8; 32])> {
    let session = peer.session.as_ref()?;
    let (pkt_type, plain) = session.decrypt_packet(data)?;
    if pkt_type != PKT_MSG_FILE_COMPLETE || plain.len() < 36 {
        return None;
    }
    let transfer_id = u32::from_le_bytes([plain[0], plain[1], plain[2], plain[3]]);
    let mut sha256 = [0u8; 32];
    sha256.copy_from_slice(&plain[4..36]);
    Some((transfer_id, sha256))
}

/// Send a FILE_CANCEL: [4B transfer_id][1B reason]
pub fn send_file_cancel(peer: &PeerSession, socket: &UdpSocket, transfer_id: u32, reason: u8) {
    if let Some(session) = &peer.session {
        let mut payload = [0u8; 5];
        payload[..4].copy_from_slice(&transfer_id.to_le_bytes());
        payload[4] = reason;
        let pkt = session.encrypt_packet(PKT_MSG_FILE_CANCEL, &payload);
        socket.send_to(&pkt, peer.peer_addr).ok();
    }
}

/// Handle an incoming FILE_CANCEL. Returns (transfer_id, reason).
pub fn handle_file_cancel(data: &[u8], peer: &PeerSession) -> Option<(u32, u8)> {
    let session = peer.session.as_ref()?;
    let (pkt_type, plain) = session.decrypt_packet(data)?;
    if pkt_type != PKT_MSG_FILE_CANCEL || plain.len() < 5 {
        return None;
    }
    let transfer_id = u32::from_le_bytes([plain[0], plain[1], plain[2], plain[3]]);
    Some((transfer_id, plain[4]))
}
