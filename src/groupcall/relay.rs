use audiopus::coder::{Decoder, Encoder};
use audiopus::packet::Packet;
use audiopus::{Application, Channels, MutSignals, SampleRate};
use ringbuf::traits::Consumer;
use std::collections::HashMap;
use std::net::{SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::Instant;

use nnnoiseless::DenoiseState;

use crate::crypto::{self, PKT_GRP_CHAT, PKT_GRP_HANGUP, PKT_GRP_HELLO, PKT_GRP_PING,
    PKT_GRP_PONG, PKT_GRP_ROSTER, PKT_GRP_VOICE,
    PKT_GRP_ALIVE, PKT_GRP_SPEED_DATA, PKT_GRP_SPEED_RESULT, PKT_GRP_LEADER};
use crate::group::{Group, GroupMember};

use super::engine::{
    self, AudioFrames, GroupCallInfo, GroupChatMsg, GroupRole,
    FRAME_SIZE, DENOISE_FRAME, MAX_OPUS_PACKET,
};

/// Leader tracks each connected member at runtime.
struct ConnectedMember {
    #[allow(dead_code)]
    sender_index: u16,
    peer_addr: SocketAddr,
    last_activity: Instant,
    rtt_ms: Option<u32>,
    ping_sent_at: Option<Instant>,
}

/// Start a group call as the leader (relay).
pub fn start_as_leader(
    group: Group,
    channel_id: &str,
    input_device: &cpal::Device,
    output_device: &cpal::Device,
    local_port: &str,
    running: Arc<AtomicBool>,
    mic_active: Arc<AtomicBool>,
    my_sender_index: u16,
) -> Result<GroupCallInfo, String> {
    log_fmt!("[groupcall] starting as LEADER for '{}' (mode=Relay, {} members)",
        group.name, group.members.len());

    let bind_addr = format!("[::]:{local_port}");
    let socket = UdpSocket::bind(&bind_addr)
        .map_err(|e| format!("Failed to bind {bind_addr}: {e}"))?;
    socket.set_read_timeout(Some(std::time::Duration::from_millis(50))).ok();

    let group_key = group.group_key;
    let _cipher = crypto::grp_cipher_from_key(&group_key);
    let send_counter = Arc::new(AtomicU32::new(0));

    // Chat channels (GUI <-> engine)
    let (chat_out_tx, chat_out_rx) = mpsc::channel::<String>();
    let (chat_in_tx, chat_in_rx) = mpsc::channel::<GroupChatMsg>();
    let (roster_tx, roster_rx) = mpsc::channel::<Vec<GroupMember>>();

    let local_hangup = Arc::new(AtomicBool::new(false));

    // Connected members map (shared between relay and housekeeping)
    let connected: Arc<Mutex<HashMap<u16, ConnectedMember>>> =
        Arc::new(Mutex::new(HashMap::new()));

    // Per-member audio storage
    let audio_frames: AudioFrames = Arc::new(Mutex::new(HashMap::new()));

    // Audio streams
    let pipeline = engine::setup_audio_streams(input_device, output_device)?;
    let mut mic_consumer = pipeline.mic_consumer;
    let _input = pipeline._input_stream;
    let _output = pipeline._output_stream;

    // Mixer thread
    engine::spawn_mixer_thread(running.clone(), audio_frames.clone(), pipeline.spk_producer);

    // ── Relay + Receiver thread ──
    let relay_socket = socket.try_clone().unwrap();
    let relay_running = running.clone();
    let relay_connected = connected.clone();
    let relay_audio = audio_frames.clone();
    let relay_chat_in = chat_in_tx.clone();
    let relay_group = group.clone();
    let relay_cipher = crypto::grp_cipher_from_key(&group_key);
    let relay_counter = send_counter.clone();
    let relay_my_pubkey = group.members.iter()
        .find(|m| m.sender_index == my_sender_index)
        .map(|m| m.pubkey)
        .unwrap_or([0u8; 32]);

    let _relay = thread::spawn(move || {
        let mut recv_buf = [0u8; 4096];
        let mut decoders: HashMap<u16, Decoder> = HashMap::new();

        while relay_running.load(Ordering::Relaxed) {
            match relay_socket.recv_from(&mut recv_buf) {
                Ok((n, from)) => {
                    if n < 3 { continue; }
                    let pkt_type = recv_buf[0];

                    // Handle GRP_HELLO: new member joining
                    if pkt_type == PKT_GRP_HELLO {
                        if let Some((_, group_id_bytes)) = crypto::parse_grp_hello(&recv_buf[..n]) {
                            let gid = crypto::group_id_from_bytes(&group_id_bytes);
                            if gid == relay_group.group_id {
                                if let Some(member) = relay_group.members.iter()
                                    .find(|m| from.ip().to_string().contains(&m.address) || m.address.is_empty())
                                {
                                    log_fmt!("[probe] IN GRP_HELLO from {} ({}), responding PKT_GRP_LEADER", member.nickname, from);
                                    let mut conn = relay_connected.lock().unwrap();
                                    if !conn.contains_key(&member.sender_index) {
                                        conn.insert(member.sender_index, ConnectedMember {
                                            sender_index: member.sender_index,
                                            peer_addr: from,
                                            last_activity: Instant::now(),
                                            rtt_ms: None,
                                            ping_sent_at: None,
                                        });
                                        log_fmt!("[groupcall] member joined: {} (idx={}) — {} connected",
                                            member.nickname, member.sender_index, conn.len());
                                    }
                                } else {
                                    log_fmt!("[probe] IN GRP_HELLO from unknown address {}", from);
                                }
                                // Respond with PKT_GRP_LEADER so probing peers know we exist
                                let counter = relay_counter.fetch_add(1, Ordering::Relaxed);
                                let leader_pkt = crypto::grp_encrypt(
                                    &relay_cipher, my_sender_index, counter,
                                    PKT_GRP_LEADER, &relay_my_pubkey,
                                );
                                let _ = relay_socket.send_to(&leader_pkt, from);
                                log_fmt!("[probe] OUT PKT_GRP_LEADER -> {}", from);
                            }
                        }
                        continue;
                    }

                    // Read header for group packets
                    let Some((pkt_type, sender_index)) = crypto::grp_read_header(&recv_buf[..n]) else {
                        continue;
                    };

                    // Update activity timestamp
                    {
                        let mut conn = relay_connected.lock().unwrap();
                        if !conn.contains_key(&sender_index) {
                            if let Some(member) = relay_group.members.iter()
                                .find(|m| m.sender_index == sender_index)
                            {
                                conn.insert(sender_index, ConnectedMember {
                                    sender_index,
                                    peer_addr: from,
                                    last_activity: Instant::now(),
                                    rtt_ms: None,
                                    ping_sent_at: None,
                                });
                                log_fmt!("[group] member auto-registered: {} (idx={})", member.nickname, sender_index);
                            }
                        }
                        if let Some(m) = conn.get_mut(&sender_index) {
                            m.last_activity = Instant::now();
                            m.peer_addr = from;
                        }
                    }

                    match pkt_type {
                        PKT_GRP_VOICE => {
                            // Decode for leader's own playback
                            if let Some((_, _, opus_data)) = crypto::grp_decrypt(&relay_cipher, &recv_buf[..n]) {
                                let decoder = decoders.entry(sender_index).or_insert_with(|| {
                                    Decoder::new(SampleRate::Hz48000, Channels::Mono)
                                        .expect("opus decoder")
                                });
                                let mut pcm = vec![0f32; FRAME_SIZE];
                                let output = match MutSignals::try_from(&mut pcm[..]) {
                                    Ok(s) => s,
                                    Err(_) => continue,
                                };
                                if let Ok(packet) = Packet::try_from(opus_data.as_slice()) {
                                    if let Ok(decoded) = decoder.decode_float(Some(packet), output, false) {
                                        let frame = pcm[..decoded].to_vec();
                                        relay_audio.lock().unwrap().insert(sender_index, frame);
                                    }
                                }
                            }

                            // Relay raw packet to all OTHER connected members
                            let conn = relay_connected.lock().unwrap();
                            for (idx, member) in conn.iter() {
                                if *idx != sender_index {
                                    let _ = relay_socket.send_to(&recv_buf[..n], member.peer_addr);
                                }
                            }
                        }

                        PKT_GRP_CHAT => {
                            if let Some((_, si, text_bytes)) = crypto::grp_decrypt(&relay_cipher, &recv_buf[..n]) {
                                let text = String::from_utf8_lossy(&text_bytes).to_string();
                                let nickname = relay_group.members.iter()
                                    .find(|m| m.sender_index == si)
                                    .map(|m| m.nickname.clone())
                                    .unwrap_or_else(|| format!("member-{}", si));
                                log_fmt!("[group-chat] leader recv from si={} '{}': '{}'", si, nickname, text);
                                let _ = relay_chat_in.send(GroupChatMsg {
                                    sender_index: si,
                                    sender_nickname: nickname,
                                    text,
                                });
                            } else {
                                log_fmt!("[group-chat] leader failed to decrypt chat pkt ({} bytes)", n);
                            }

                            // Relay to all OTHER members
                            let conn = relay_connected.lock().unwrap();
                            for (idx, member) in conn.iter() {
                                if *idx != sender_index {
                                    log_fmt!("[group-chat] leader relay chat to si={} addr={}", idx, member.peer_addr);
                                    let _ = relay_socket.send_to(&recv_buf[..n], member.peer_addr);
                                }
                            }
                        }

                        PKT_GRP_PONG => {
                            if let Some((_, _, data)) = crypto::grp_decrypt(&relay_cipher, &recv_buf[..n]) {
                                if data.len() >= 8 {
                                    let mut conn = relay_connected.lock().unwrap();
                                    if let Some(m) = conn.get_mut(&sender_index) {
                                        if let Some(sent_at) = m.ping_sent_at.take() {
                                            m.rtt_ms = Some(sent_at.elapsed().as_millis() as u32);
                                        }
                                    }
                                }
                            }
                        }

                        PKT_GRP_HANGUP => {
                            let nickname = relay_group.members.iter()
                                .find(|m| m.sender_index == sender_index)
                                .map(|m| m.nickname.as_str())
                                .unwrap_or("unknown");
                            let mut conn = relay_connected.lock().unwrap();
                            conn.remove(&sender_index);
                            log_fmt!("[groupcall] member left: {} (idx={}) — {} remaining",
                                nickname, sender_index, conn.len());
                            drop(conn);
                            decoders.remove(&sender_index);
                            relay_audio.lock().unwrap().remove(&sender_index);
                        }

                        _ => {}
                    }
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(ref e) if e.kind() == std::io::ErrorKind::TimedOut => {}
                Err(ref e) if e.kind() == std::io::ErrorKind::ConnectionReset => {}
                Err(_) => {}
            }
        }
    });

    // ── Leader's own sender thread ──
    let send_socket = socket.try_clone().unwrap();
    let sender_running = running.clone();
    let sender_mic_active = mic_active.clone();
    let sender_connected = connected.clone();
    let sender_counter = send_counter.clone();
    let sender_cipher = crypto::grp_cipher_from_key(&group_key);
    let chat_out_rx_arc = Arc::new(Mutex::new(chat_out_rx));
    let chat_out_rx_sender = chat_out_rx_arc.clone();

    let _sender = thread::spawn(move || {
        let mut encoder = Encoder::new(
            SampleRate::Hz48000, Channels::Mono, Application::Voip,
        ).expect("opus encoder");
        encoder.set_bitrate(audiopus::Bitrate::BitsPerSecond(64000)).unwrap();

        let mut denoiser = DenoiseState::new();
        let mut denoise_in = [0f32; DENOISE_FRAME];
        let mut denoise_out = [0f32; DENOISE_FRAME];
        let mut pcm_frame = vec![0f32; FRAME_SIZE];
        let mut opus_buf = vec![0u8; MAX_OPUS_PACKET];

        while sender_running.load(Ordering::Relaxed) {
            // Outgoing chat
            if let Ok(rx) = chat_out_rx_sender.lock() {
                while let Ok(text) = rx.try_recv() {
                    log_fmt!("[group-chat] leader sending own chat: '{}'", text);
                    let counter = sender_counter.fetch_add(1, Ordering::Relaxed);
                    let pkt = crypto::grp_encrypt(&sender_cipher, my_sender_index, counter, PKT_GRP_CHAT, text.as_bytes());
                    let conn = sender_connected.lock().unwrap();
                    for member in conn.values() {
                        let _ = send_socket.send_to(&pkt, member.peer_addr);
                    }
                }
            }

            // Collect audio frame
            let mut collected = 0;
            while collected < FRAME_SIZE {
                while collected < FRAME_SIZE {
                    if let Some(sample) = mic_consumer.try_pop() {
                        pcm_frame[collected] = if sender_mic_active.load(Ordering::Relaxed) {
                            sample
                        } else {
                            0.0
                        };
                        collected += 1;
                    } else {
                        break;
                    }
                }
                if collected < FRAME_SIZE {
                    let remaining = FRAME_SIZE - collected;
                    let sleep_us = (remaining as u64 * 1_000_000 / 48000) * 3 / 4;
                    thread::sleep(std::time::Duration::from_micros(sleep_us.max(1000)));
                    if !sender_running.load(Ordering::Relaxed) { break; }
                }
            }
            if !sender_running.load(Ordering::Relaxed) { break; }

            // Denoise
            for half in 0..2 {
                let offset = half * DENOISE_FRAME;
                for i in 0..DENOISE_FRAME {
                    denoise_in[i] = pcm_frame[offset + i] * 32768.0;
                }
                denoiser.process_frame(&mut denoise_out, &denoise_in);
                for i in 0..DENOISE_FRAME {
                    pcm_frame[offset + i] = denoise_out[i] / 32768.0;
                }
            }

            // Encode + encrypt + send to all members
            let encoded_len = match encoder.encode_float(&pcm_frame, &mut opus_buf) {
                Ok(n) => n,
                Err(_) => continue,
            };
            let counter = sender_counter.fetch_add(1, Ordering::Relaxed);
            let pkt = crypto::grp_encrypt(
                &sender_cipher, my_sender_index, counter,
                PKT_GRP_VOICE, &opus_buf[..encoded_len],
            );
            let conn = sender_connected.lock().unwrap();
            for member in conn.values() {
                let _ = send_socket.send_to(&pkt, member.peer_addr);
            }
        }
    });

    // ── Housekeeping thread (ping + roster broadcast) ──
    let hk_socket = socket.try_clone().unwrap();
    let hk_running = running.clone();
    let hk_connected = connected.clone();
    let hk_group = group.clone();
    let hk_cipher = crypto::grp_cipher_from_key(&group_key);
    let hk_counter = send_counter.clone();
    let hk_roster_tx = roster_tx;

    let _housekeeping = thread::spawn(move || {
        while hk_running.load(Ordering::Relaxed) {
            thread::sleep(std::time::Duration::from_secs(5));
            if !hk_running.load(Ordering::Relaxed) { break; }

            let mut conn = hk_connected.lock().unwrap();

            // Send PING to each member
            let now_ns = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap().as_nanos() as u64;
            for member in conn.values_mut() {
                let counter = hk_counter.fetch_add(1, Ordering::Relaxed);
                let pkt = crypto::grp_encrypt(
                    &hk_cipher, my_sender_index, counter,
                    PKT_GRP_PING, &now_ns.to_le_bytes(),
                );
                let _ = hk_socket.send_to(&pkt, member.peer_addr);
                member.ping_sent_at = Some(Instant::now());
            }

            // Build roster: only leader (ourselves) + actually connected members
            let connected_indices: Vec<u16> = conn.keys().copied().collect();
            let mut roster: Vec<GroupMember> = hk_group.members.iter()
                .filter(|m| m.sender_index == my_sender_index || connected_indices.contains(&m.sender_index))
                .cloned()
                .collect();
            for m in roster.iter_mut() {
                if let Some(cm) = conn.get(&m.sender_index) {
                    if let Some(_rtt) = cm.rtt_ms {
                        m.address = cm.peer_addr.ip().to_string();
                        m.port = cm.peer_addr.port().to_string();
                    }
                }
            }

            let _ = hk_roster_tx.send(roster.clone());

            if let Ok(roster_json) = serde_json::to_vec(&roster) {
                let counter = hk_counter.fetch_add(1, Ordering::Relaxed);
                let pkt = crypto::grp_encrypt(
                    &hk_cipher, my_sender_index, counter,
                    PKT_GRP_ROSTER, &roster_json,
                );
                for member in conn.values() {
                    let _ = hk_socket.send_to(&pkt, member.peer_addr);
                }
            }

            // Remove timed-out members (>15s)
            let timeout_indices: Vec<u16> = conn.iter()
                .filter(|(_, m)| m.last_activity.elapsed().as_secs() > 15)
                .map(|(idx, _)| *idx)
                .collect();
            for idx in timeout_indices {
                let nickname = hk_group.members.iter()
                    .find(|m| m.sender_index == idx)
                    .map(|m| m.nickname.as_str())
                    .unwrap_or("unknown");
                conn.remove(&idx);
                log_fmt!("[groupcall] member timed out: {} (idx={}) — {} remaining",
                    nickname, idx, conn.len());
            }
        }
    });

    Ok(GroupCallInfo {
        group,
        role: GroupRole::Leader,
        channel_id: channel_id.to_string(),
        running,
        mic_active,
        chat_tx: chat_out_tx,
        chat_rx: chat_in_rx,
        roster_rx,
        local_hangup,
    })
}

/// Start a group call as a member (client) with failover support.
pub fn start_as_member(
    group: Group,
    channel_id: &str,
    leader_addr: &str,
    input_device: &cpal::Device,
    output_device: &cpal::Device,
    local_port: &str,
    running: Arc<AtomicBool>,
    mic_active: Arc<AtomicBool>,
    my_sender_index: u16,
) -> Result<GroupCallInfo, String> {
    log_fmt!("[groupcall] starting as MEMBER for '{}' (mode=Relay, leader={})",
        group.name, leader_addr);

    let bind_addr = format!("[::]:{local_port}");
    let socket = UdpSocket::bind(&bind_addr)
        .map_err(|e| format!("Failed to bind {bind_addr}: {e}"))?;
    socket.set_read_timeout(Some(std::time::Duration::from_millis(100))).ok();

    let group_key = group.group_key;
    let send_counter = Arc::new(AtomicU32::new(0));

    // Shared leader address (updated by failover)
    let leader_addr_shared: Arc<Mutex<String>> = Arc::new(Mutex::new(leader_addr.to_string()));

    // Send GRP_HELLO to leader
    if let Some(gid_bytes) = crypto::group_id_to_bytes(&group.group_id) {
        let mut dummy_pubkey = [0u8; 32];
        use rand::RngCore;
        rand::thread_rng().fill_bytes(&mut dummy_pubkey);
        let hello = crypto::build_grp_hello(&dummy_pubkey, &gid_bytes);
        for _ in 0..5 {
            let _ = socket.send_to(&hello, leader_addr);
            thread::sleep(std::time::Duration::from_millis(200));
        }
    }

    // Chat channels
    let (chat_out_tx, chat_out_rx) = mpsc::channel::<String>();
    let (chat_in_tx, chat_in_rx) = mpsc::channel::<GroupChatMsg>();
    let (roster_tx, roster_rx) = mpsc::channel::<Vec<GroupMember>>();

    let local_hangup = Arc::new(AtomicBool::new(false));

    // Per-sender decoded audio frames
    let audio_frames: AudioFrames = Arc::new(Mutex::new(HashMap::new()));

    // Audio streams
    let pipeline = engine::setup_audio_streams(input_device, output_device)?;
    let mut mic_consumer = pipeline.mic_consumer;
    let _input = pipeline._input_stream;
    let _output = pipeline._output_stream;

    // Mixer thread
    engine::spawn_mixer_thread(running.clone(), audio_frames.clone(), pipeline.spk_producer);

    // ── Receiver thread (with failover detection) ──
    let recv_socket = socket.try_clone().unwrap();
    let recv_running = running.clone();
    let recv_audio = audio_frames.clone();
    let recv_cipher = crypto::grp_cipher_from_key(&group_key);
    let recv_group = group.clone();
    let recv_leader_addr = leader_addr_shared.clone();
    let my_pubkey = recv_group.members.iter()
        .find(|m| m.sender_index == my_sender_index)
        .map(|m| m.pubkey)
        .unwrap_or([0u8; 32]);

    let _receiver = thread::spawn(move || {
        let mut recv_buf = [0u8; 4096];
        let mut decoders: HashMap<u16, Decoder> = HashMap::new();
        let mut last_leader_packet = Instant::now();
        let mut failover_in_progress = false;

        while recv_running.load(Ordering::Relaxed) {
            match recv_socket.recv_from(&mut recv_buf) {
                Ok((n, _from)) => {
                    if n < 3 { continue; }

                    let Some((pkt_type, sender_index)) = crypto::grp_read_header(&recv_buf[..n]) else {
                        continue;
                    };

                    last_leader_packet = Instant::now();
                    failover_in_progress = false;

                    match pkt_type {
                        PKT_GRP_VOICE => {
                            if let Some((_, si, opus_data)) = crypto::grp_decrypt(&recv_cipher, &recv_buf[..n]) {
                                let decoder = decoders.entry(si).or_insert_with(|| {
                                    Decoder::new(SampleRate::Hz48000, Channels::Mono)
                                        .expect("opus decoder")
                                });
                                let mut pcm = vec![0f32; FRAME_SIZE];
                                let output = match MutSignals::try_from(&mut pcm[..]) {
                                    Ok(s) => s,
                                    Err(_) => continue,
                                };
                                if let Ok(packet) = Packet::try_from(opus_data.as_slice()) {
                                    if let Ok(decoded) = decoder.decode_float(Some(packet), output, false) {
                                        let frame = pcm[..decoded].to_vec();
                                        recv_audio.lock().unwrap().insert(si, frame);
                                    }
                                }
                            }
                        }

                        PKT_GRP_CHAT => {
                            if let Some((_, si, text_bytes)) = crypto::grp_decrypt(&recv_cipher, &recv_buf[..n]) {
                                let text = String::from_utf8_lossy(&text_bytes).to_string();
                                let nickname = recv_group.members.iter()
                                    .find(|m| m.sender_index == si)
                                    .map(|m| m.nickname.clone())
                                    .unwrap_or_else(|| format!("member-{}", si));
                                log_fmt!("[group-chat] member recv from si={} '{}': '{}'", si, nickname, text);
                                let _ = chat_in_tx.send(GroupChatMsg {
                                    sender_index: si,
                                    sender_nickname: nickname,
                                    text,
                                });
                            } else {
                                log_fmt!("[group-chat] member failed to decrypt chat pkt ({} bytes)", n);
                            }
                        }

                        PKT_GRP_ROSTER => {
                            if let Some((_, _, data)) = crypto::grp_decrypt(&recv_cipher, &recv_buf[..n]) {
                                if let Ok(roster) = serde_json::from_slice::<Vec<GroupMember>>(&data) {
                                    let _ = roster_tx.send(roster);
                                }
                            }
                        }

                        PKT_GRP_PING => {
                            if let Some((_, _, ping_data)) = crypto::grp_decrypt(&recv_cipher, &recv_buf[..n]) {
                                let counter = send_counter.fetch_add(1, Ordering::Relaxed);
                                let pong = crypto::grp_encrypt(
                                    &recv_cipher, my_sender_index, counter,
                                    PKT_GRP_PONG, &ping_data,
                                );
                                let _ = recv_socket.send_to(&pong, _from);
                            }
                        }

                        PKT_GRP_LEADER => {
                            if let Some((_, _, data)) = crypto::grp_decrypt(&recv_cipher, &recv_buf[..n]) {
                                if data.len() >= 32 {
                                    let old_leader = recv_leader_addr.lock().unwrap().clone();
                                    let leader_nick = recv_group.members.iter()
                                        .find(|m| m.sender_index == sender_index)
                                        .map(|m| m.nickname.as_str())
                                        .unwrap_or("unknown");
                                    log_fmt!("[groupcall] leader changed: {} (idx={}) at {} (was {})",
                                        leader_nick, sender_index, _from, old_leader);
                                    *recv_leader_addr.lock().unwrap() = _from.to_string();
                                    last_leader_packet = Instant::now();
                                }
                            }
                        }

                        PKT_GRP_HANGUP => {
                            let nickname = recv_group.members.iter()
                                .find(|m| m.sender_index == sender_index)
                                .map(|m| m.nickname.as_str())
                                .unwrap_or("unknown");
                            log_fmt!("[groupcall] member disconnected (via relay): {} (idx={})",
                                nickname, sender_index);
                            recv_audio.lock().unwrap().remove(&sender_index);
                            decoders.remove(&sender_index);
                        }

                        _ => {}
                    }
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut => {
                    // Check for leader timeout (10s)
                    if last_leader_packet.elapsed().as_secs() > 10 && !failover_in_progress {
                        log_fmt!("[group] leader timeout ({}s), starting failover",
                            last_leader_packet.elapsed().as_secs());

                        match run_failover(
                            &recv_socket, &recv_group,
                            my_sender_index, &my_pubkey, &group_key,
                        ) {
                            Some(FailoverResult::NewLeaderAddr(addr)) => {
                                log_fmt!("[group] failover: new leader at {}", addr);
                                *recv_leader_addr.lock().unwrap() = addr;
                                last_leader_packet = Instant::now();
                                failover_in_progress = false;
                            }
                            Some(FailoverResult::BecomeLeader) => {
                                log_fmt!("[group] failover: we become leader — stopping member engine");
                                recv_running.store(false, Ordering::Relaxed);
                                break;
                            }
                            None => {
                                log_fmt!("[group] failover failed (internet down?)");
                                failover_in_progress = false;
                                last_leader_packet = Instant::now();
                            }
                        }
                    }
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::ConnectionReset => {}
                Err(_) => {}
            }
        }
    });

    // ── Sender thread ──
    let send_socket2 = socket.try_clone().unwrap();
    let sender_running = running.clone();
    let sender_mic_active = mic_active.clone();
    let sender_counter2 = Arc::new(AtomicU32::new(0));
    let sender_cipher2 = crypto::grp_cipher_from_key(&group_key);
    let sender_leader_addr = leader_addr_shared.clone();

    let _sender = thread::spawn(move || {
        let mut encoder = Encoder::new(
            SampleRate::Hz48000, Channels::Mono, Application::Voip,
        ).expect("opus encoder");
        encoder.set_bitrate(audiopus::Bitrate::BitsPerSecond(64000)).unwrap();

        let mut denoiser = DenoiseState::new();
        let mut denoise_in = [0f32; DENOISE_FRAME];
        let mut denoise_out = [0f32; DENOISE_FRAME];
        let mut pcm_frame = vec![0f32; FRAME_SIZE];
        let mut opus_buf = vec![0u8; MAX_OPUS_PACKET];

        while sender_running.load(Ordering::Relaxed) {
            let current_leader = sender_leader_addr.lock().unwrap().clone();

            // Outgoing chat
            while let Ok(text) = chat_out_rx.try_recv() {
                log_fmt!("[group-chat] member sending chat to leader {}: '{}'", current_leader, text);
                let counter = sender_counter2.fetch_add(1, Ordering::Relaxed);
                let pkt = crypto::grp_encrypt(
                    &sender_cipher2, my_sender_index, counter,
                    PKT_GRP_CHAT, text.as_bytes(),
                );
                let _ = send_socket2.send_to(&pkt, &current_leader);
            }

            // Collect audio frame
            let mut collected = 0;
            while collected < FRAME_SIZE {
                while collected < FRAME_SIZE {
                    if let Some(sample) = mic_consumer.try_pop() {
                        pcm_frame[collected] = if sender_mic_active.load(Ordering::Relaxed) {
                            sample
                        } else {
                            0.0
                        };
                        collected += 1;
                    } else {
                        break;
                    }
                }
                if collected < FRAME_SIZE {
                    let remaining = FRAME_SIZE - collected;
                    let sleep_us = (remaining as u64 * 1_000_000 / 48000) * 3 / 4;
                    thread::sleep(std::time::Duration::from_micros(sleep_us.max(1000)));
                    if !sender_running.load(Ordering::Relaxed) { break; }
                }
            }
            if !sender_running.load(Ordering::Relaxed) { break; }

            // Denoise
            for half in 0..2 {
                let offset = half * DENOISE_FRAME;
                for i in 0..DENOISE_FRAME {
                    denoise_in[i] = pcm_frame[offset + i] * 32768.0;
                }
                denoiser.process_frame(&mut denoise_out, &denoise_in);
                for i in 0..DENOISE_FRAME {
                    pcm_frame[offset + i] = denoise_out[i] / 32768.0;
                }
            }

            // Encode + encrypt + send to current leader
            let encoded_len = match encoder.encode_float(&pcm_frame, &mut opus_buf) {
                Ok(n) => n,
                Err(_) => continue,
            };
            let counter = sender_counter2.fetch_add(1, Ordering::Relaxed);
            let pkt = crypto::grp_encrypt(
                &sender_cipher2, my_sender_index, counter,
                PKT_GRP_VOICE, &opus_buf[..encoded_len],
            );
            let _ = send_socket2.send_to(&pkt, &current_leader);
        }
    });

    Ok(GroupCallInfo {
        group,
        role: GroupRole::Member,
        channel_id: channel_id.to_string(),
        running,
        mic_active,
        chat_tx: chat_out_tx,
        chat_rx: chat_in_rx,
        roster_rx,
        local_hangup,
    })
}

// ── Failover protocol ──

enum FailoverResult {
    NewLeaderAddr(String),
    BecomeLeader,
}

/// Run the full failover protocol (~5s): DNS probe -> discovery -> speed test -> election.
fn run_failover(
    socket: &UdpSocket,
    group: &Group,
    my_sender_index: u16,
    my_pubkey: &[u8; 32],
    group_key: &[u8; 32],
) -> Option<FailoverResult> {
    use std::time::Duration;

    // 1. DNS probe
    if !dns_probe() {
        log_fmt!("[failover] DNS probe failed — own internet is down");
        return None;
    }
    log_fmt!("[failover] DNS OK, discovering survivors...");

    // 2. Send PKT_GRP_ALIVE to all other members
    let now_ns = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap().as_nanos() as u64;
    let failover_cipher = crypto::grp_cipher_from_key(group_key);

    for member in &group.members {
        if member.pubkey == *my_pubkey { continue; }
        if member.address.is_empty() || member.port.is_empty() { continue; }
        let addr_str = format!("[{}]:{}", member.address, member.port);
        if let Ok(addr) = addr_str.parse::<SocketAddr>() {
            let pkt = crypto::grp_encrypt(
                &failover_cipher, my_sender_index, 0,
                PKT_GRP_ALIVE, &now_ns.to_le_bytes(),
            );
            let _ = socket.send_to(&pkt, addr);
        }
    }

    // 3. Collect ALIVE responses for 2 seconds
    let mut alive: Vec<(u16, SocketAddr, [u8; 32])> = Vec::new();
    alive.push((my_sender_index, socket.local_addr().unwrap_or_else(|_| "[::]:0".parse().unwrap()), *my_pubkey));

    let discover_end = Instant::now() + Duration::from_secs(2);
    let mut buf = [0u8; 4096];
    while Instant::now() < discover_end {
        match socket.recv_from(&mut buf) {
            Ok((n, from)) if n >= 3 => {
                if let Some((pkt_type, si)) = crypto::grp_read_header(&buf[..n]) {
                    if pkt_type == PKT_GRP_ALIVE {
                        if crypto::grp_decrypt(&failover_cipher, &buf[..n]).is_some() {
                            if !alive.iter().any(|(idx, _, _)| *idx == si) {
                                let pk = group.members.iter()
                                    .find(|m| m.sender_index == si)
                                    .map(|m| m.pubkey)
                                    .unwrap_or([0u8; 32]);
                                alive.push((si, from, pk));
                                log_fmt!("[failover] alive: idx={} from={}", si, from);
                                let pkt = crypto::grp_encrypt(
                                    &failover_cipher, my_sender_index, 0,
                                    PKT_GRP_ALIVE, &now_ns.to_le_bytes(),
                                );
                                let _ = socket.send_to(&pkt, from);
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }
    log_fmt!("[failover] {} survivors found (including self)", alive.len());

    if alive.len() <= 1 {
        return Some(FailoverResult::BecomeLeader);
    }

    // 4. Speed test
    let burst_data = [0xABu8; 1200];
    for &(si, addr, _) in &alive {
        if si == my_sender_index { continue; }
        for i in 0u32..50 {
            let pkt = crypto::grp_encrypt(
                &failover_cipher, my_sender_index, i,
                PKT_GRP_SPEED_DATA, &burst_data,
            );
            let _ = socket.send_to(&pkt, addr);
        }
    }

    // 5. Collect speed data + results for 2 seconds
    let mut recv_counts: HashMap<u16, (Instant, Instant, usize)> = HashMap::new();
    let mut peer_speeds: HashMap<u16, u32> = HashMap::new();

    let speed_end = Instant::now() + Duration::from_secs(2);
    while Instant::now() < speed_end {
        match socket.recv_from(&mut buf) {
            Ok((n, _from)) if n >= 3 => {
                if let Some((pkt_type, si)) = crypto::grp_read_header(&buf[..n]) {
                    match pkt_type {
                        PKT_GRP_SPEED_DATA => {
                            let entry = recv_counts.entry(si)
                                .or_insert((Instant::now(), Instant::now(), 0));
                            entry.1 = Instant::now();
                            entry.2 += 1;
                        }
                        PKT_GRP_SPEED_RESULT => {
                            if let Some((_, _, data)) = crypto::grp_decrypt(&failover_cipher, &buf[..n]) {
                                if data.len() >= 4 {
                                    let kbps = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
                                    peer_speeds.insert(si, kbps);
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }

    // Send our measured results back
    for (&si, &(first, last, count)) in &recv_counts {
        let elapsed_ms = last.duration_since(first).as_millis().max(1) as u64;
        let kbps = (count as u64 * 1200 * 1000 / elapsed_ms / 1024) as u32;
        if let Some(&(_, addr, _)) = alive.iter().find(|(idx, _, _)| *idx == si) {
            let pkt = crypto::grp_encrypt(
                &failover_cipher, my_sender_index, 0,
                PKT_GRP_SPEED_RESULT, &kbps.to_le_bytes(),
            );
            let _ = socket.send_to(&pkt, addr);
        }
    }

    // Wait briefly for final results
    thread::sleep(Duration::from_millis(500));
    while let Ok((n, _)) = socket.recv_from(&mut buf) {
        if n >= 3 {
            if let Some((pkt_type, si)) = crypto::grp_read_header(&buf[..n]) {
                if pkt_type == PKT_GRP_SPEED_RESULT {
                    if let Some((_, _, data)) = crypto::grp_decrypt(&failover_cipher, &buf[..n]) {
                        if data.len() >= 4 {
                            let kbps = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
                            peer_speeds.insert(si, kbps);
                        }
                    }
                }
            }
        }
    }

    // 6. Election
    let my_avg_speed = if peer_speeds.is_empty() {
        0u32
    } else {
        peer_speeds.values().sum::<u32>() / peer_speeds.len() as u32
    };

    let mut candidates: Vec<(u32, [u8; 32], u16, Option<SocketAddr>)> = Vec::new();
    candidates.push((my_avg_speed, *my_pubkey, my_sender_index, None));

    for &(si, addr, pk) in &alive {
        if si == my_sender_index { continue; }
        candidates.push((0, pk, si, Some(addr)));
    }

    candidates.sort_by(|a, b| {
        b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1))
    });

    if let Some((_, winner_pk, winner_idx, winner_addr)) = candidates.first() {
        if *winner_pk == *my_pubkey {
            log_fmt!("[failover] WE are elected as new leader (speed={}KB/s)", my_avg_speed);
            for &(si, addr, _) in &alive {
                if si == my_sender_index { continue; }
                let pkt = crypto::grp_encrypt(
                    &failover_cipher, my_sender_index, 0,
                    PKT_GRP_LEADER, my_pubkey,
                );
                let _ = socket.send_to(&pkt, addr);
            }
            Some(FailoverResult::BecomeLeader)
        } else {
            log_fmt!("[failover] new leader elected: idx={}", winner_idx);
            if let Some(addr) = winner_addr {
                Some(FailoverResult::NewLeaderAddr(addr.to_string()))
            } else {
                None
            }
        }
    } else {
        None
    }
}

/// DNS probe: send a simple query to 8.8.8.8:53 to verify own internet works.
fn dns_probe() -> bool {
    let probe = match UdpSocket::bind("0.0.0.0:0") {
        Ok(s) => s,
        Err(_) => return false,
    };
    probe.set_read_timeout(Some(std::time::Duration::from_secs(2))).ok();

    let dns_query: [u8; 28] = [
        0x00, 0x01, 0x01, 0x00,
        0x00, 0x01, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00,
        0x06, b'g', b'o', b'o', b'g', b'l', b'e',
        0x03, b'c', b'o', b'm', 0x00,
        0x00, 0x01, 0x00, 0x01,
    ];

    if probe.send_to(&dns_query, "8.8.8.8:53").is_err() {
        return false;
    }

    let mut buf = [0u8; 512];
    probe.recv_from(&mut buf).is_ok()
}
