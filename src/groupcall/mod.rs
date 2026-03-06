pub mod engine;
pub mod relay;
pub mod p2p;

pub use engine::{GroupCallInfo, GroupChatMsg, GroupRole};

use crate::crypto;
use crate::group::{CallMode, Group};
use std::net::UdpSocket;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Instant;

/// Start a group call with automatic leader detection.
///
/// - **Relay mode**: Sends GRP_HELLO to all members, waits 2s for a PKT_GRP_LEADER
///   response. If received, joins as member. If no response, starts as leader.
/// - **P2P mode**: Starts the mesh directly (no leader concept).
pub fn start_group_call(
    group: Group,
    channel_id: &str,
    input_device: &cpal::Device,
    output_device: &cpal::Device,
    local_port: &str,
    running: Arc<AtomicBool>,
    mic_active: Arc<AtomicBool>,
    my_sender_index: u16,
) -> Result<GroupCallInfo, String> {
    match group.call_mode {
        CallMode::P2P => {
            p2p::start(
                group, channel_id, input_device, output_device,
                local_port, running, mic_active, my_sender_index,
            )
        }
        CallMode::Relay => {
            // Auto-detect: probe for an existing leader
            let leader_addr = probe_for_leader(&group, local_port, my_sender_index)?;

            match leader_addr {
                Some(addr) => {
                    log_fmt!("[groupcall] found leader at {}, joining as member", addr);
                    relay::start_as_member(
                        group, channel_id, &addr, input_device, output_device,
                        local_port, running, mic_active, my_sender_index,
                    )
                }
                None => {
                    log_fmt!("[groupcall] no leader found, starting as leader");
                    relay::start_as_leader(
                        group, channel_id, input_device, output_device,
                        local_port, running, mic_active, my_sender_index,
                    )
                }
            }
        }
    }
}

/// Probe all group members for an existing leader. Returns leader address if found.
fn probe_for_leader(
    group: &Group,
    local_port: &str,
    my_sender_index: u16,
) -> Result<Option<String>, String> {
    let my_pubkey = group.members.iter()
        .find(|m| m.sender_index == my_sender_index)
        .map(|m| m.pubkey)
        .unwrap_or([0u8; 32]);

    // Bind a temporary socket for probing
    let bind_addr = format!("[::]:{local_port}");
    let socket = UdpSocket::bind(&bind_addr)
        .map_err(|e| format!("Failed to bind {bind_addr}: {e}"))?;
    socket.set_read_timeout(Some(std::time::Duration::from_millis(100))).ok();

    // Send GRP_HELLO to all known members
    if let Some(gid_bytes) = crypto::group_id_to_bytes(&group.group_id) {
        let mut dummy_pubkey = [0u8; 32];
        use rand::RngCore;
        rand::thread_rng().fill_bytes(&mut dummy_pubkey);
        let hello = crypto::build_grp_hello(&dummy_pubkey, &gid_bytes);

        let mut probe_count = 0;
        for member in &group.members {
            if member.pubkey == my_pubkey { continue; }
            if member.address.is_empty() || member.port.is_empty() { continue; }
            let addr_str = format!("[{}]:{}", member.address, member.port);
            if let Ok(addr) = addr_str.parse::<std::net::SocketAddr>() {
                let _ = socket.send_to(&hello, addr);
                log_fmt!("[probe] OUT GRP_HELLO -> {} ({})", member.nickname, addr_str);
                probe_count += 1;
            }
        }
        log_fmt!("[probe] OUT sent {} hellos, waiting 2s for leader...", probe_count);
    }

    // Wait 2s for a PKT_GRP_LEADER response
    let group_key = group.group_key;
    let cipher = crypto::grp_cipher_from_key(&group_key);
    let deadline = Instant::now() + std::time::Duration::from_secs(2);
    let mut buf = [0u8; 4096];

    while Instant::now() < deadline {
        match socket.recv_from(&mut buf) {
            Ok((n, from)) if n >= 3 => {
                // Any voice/roster/ping packet from another member means a leader exists
                if let Some((pkt_type, _si)) = crypto::grp_read_header(&buf[..n]) {
                    log_fmt!("[probe] IN pkt=0x{:02x} from {}", pkt_type, from);
                    match pkt_type {
                        crypto::PKT_GRP_VOICE | crypto::PKT_GRP_ROSTER | crypto::PKT_GRP_PING => {
                            log_fmt!("[probe] IN leader found at {}", from);
                            drop(socket);
                            return Ok(Some(from.to_string()));
                        }
                        crypto::PKT_GRP_LEADER => {
                            if crypto::grp_decrypt(&cipher, &buf[..n]).is_some() {
                                log_fmt!("[probe] IN PKT_GRP_LEADER from {}", from);
                                drop(socket);
                                return Ok(Some(from.to_string()));
                            }
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }

    // No leader found — we drop the probe socket so relay can bind the same port
    log_fmt!("[probe] no leader after 2s");
    drop(socket);
    Ok(None)
}
