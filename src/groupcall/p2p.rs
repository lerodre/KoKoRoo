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

use crate::crypto::{self, PKT_GRP_HANGUP, PKT_GRP_HELLO, PKT_GRP_VOICE,
    PKT_GRP_SCREEN, PKT_GRP_SCREEN_OFFER, PKT_GRP_SCREEN_STOP};
use crate::group::{Group, GroupMember};

use crate::screen::{ScreenCommand, ScreenViewer, CaptureSource, GroupSendTarget};

use super::engine::{
    self, AudioFrames, AudioKeepAlive, GroupCallInfo, GroupChatMsg, GroupRole,
    FRAME_SIZE, DENOISE_FRAME, MAX_OPUS_PACKET,
};

/// A connected peer in P2P mode.
struct PeerConnection {
    #[allow(dead_code)]
    sender_index: u16,
    peer_addr: SocketAddr,
    last_activity: Instant,
}

/// Start a group call in P2P mesh mode.
/// Every peer sends audio directly to every other connected peer.
/// No leader/member distinction — all peers are equal.
pub fn start(
    group: Group,
    channel_id: &str,
    input_device: &cpal::Device,
    output_device: &cpal::Device,
    local_port: &str,
    running: Arc<AtomicBool>,
    mic_active: Arc<AtomicBool>,
    my_sender_index: u16,
) -> Result<(GroupCallInfo, AudioKeepAlive), String> {
    let target_peers = group.members.iter()
        .filter(|m| m.pubkey != group.members.iter()
            .find(|mm| mm.sender_index == my_sender_index)
            .map(|mm| mm.pubkey).unwrap_or([0u8; 32])
            && !m.address.is_empty() && !m.port.is_empty())
        .count();
    log_fmt!("[groupcall] starting P2P mesh for '{}' ({} potential peers)",
        group.name, target_peers);

    let bind_addr = format!("[::]:{local_port}");
    let socket = UdpSocket::bind(&bind_addr)
        .map_err(|e| format!("Failed to bind {bind_addr}: {e}"))?;
    socket.set_read_timeout(Some(std::time::Duration::from_millis(50))).ok();

    let group_key = group.group_key;
    let previous_key = group.previous_key;
    let send_counter = Arc::new(AtomicU32::new(0));

    // Chat channels (GUI <-> engine)
    let (chat_out_tx, chat_out_rx) = mpsc::channel::<String>();
    let (_chat_in_tx, chat_in_rx) = mpsc::channel::<GroupChatMsg>();
    let (roster_tx, roster_rx) = mpsc::channel::<Vec<GroupMember>>();

    let local_hangup = Arc::new(AtomicBool::new(false));

    // Screen sharing state
    let (screen_cmd_tx, screen_cmd_rx) = mpsc::channel::<ScreenCommand>();
    let screen_viewer = Arc::new(Mutex::new(ScreenViewer::new()));
    let screen_sharer: Arc<Mutex<Option<u16>>> = Arc::new(Mutex::new(None));
    let screen_active = Arc::new(AtomicBool::new(false));

    // Connected peers map
    let peers: Arc<Mutex<HashMap<u16, PeerConnection>>> =
        Arc::new(Mutex::new(HashMap::new()));

    // Per-sender decoded audio frames
    let audio_frames: AudioFrames = Arc::new(Mutex::new(HashMap::new()));

    // Per-sender voice levels (RMS) for speaking indicator
    let voice_levels: engine::VoiceLevels = Arc::new(Mutex::new(HashMap::new()));

    // Audio streams — streams are !Send, so caller must keep AudioKeepAlive alive
    let pipeline = engine::setup_audio_streams(input_device, output_device)?;
    let mut mic_consumer = pipeline.mic_consumer;
    let audio_keep_alive = AudioKeepAlive {
        _input_stream: pipeline._input_stream,
        _output_stream: pipeline._output_stream,
    };

    // Mixer thread
    engine::spawn_mixer_thread(running.clone(), audio_frames.clone(), pipeline.spk_producer);

    // Send GRP_HELLO to all known members to discover peers
    let my_pubkey = group.members.iter()
        .find(|m| m.sender_index == my_sender_index)
        .map(|m| m.pubkey)
        .unwrap_or([0u8; 32]);

    if let Some(gid_bytes) = crypto::group_id_to_bytes(&group.group_id) {
        let mut dummy_pubkey = [0u8; 32];
        use rand::RngCore;
        rand::thread_rng().fill_bytes(&mut dummy_pubkey);
        let hello = crypto::build_grp_hello(&dummy_pubkey, &gid_bytes);

        for member in &group.members {
            if member.pubkey == my_pubkey { continue; }
            if member.address.is_empty() || member.port.is_empty() { continue; }
            let addr_str = format!("[{}]:{}", member.address, member.port);
            if let Ok(addr) = addr_str.parse::<SocketAddr>() {
                for _ in 0..3 {
                    let _ = socket.send_to(&hello, addr);
                }
            }
        }
    }

    // ── Receiver thread ──
    let recv_socket = socket.try_clone().unwrap();
    let recv_running = running.clone();
    let recv_audio = audio_frames.clone();
    let recv_cipher = crypto::grp_cipher_from_key(&group_key);
    let recv_prev_cipher = previous_key.map(|k| crypto::grp_cipher_from_key(&k));
    let recv_group = group.clone();
    let recv_peers = peers.clone();
    let recv_screen_viewer = screen_viewer.clone();
    let recv_screen_sharer = screen_sharer.clone();
    let recv_voice_levels = voice_levels.clone();

    let _receiver = thread::spawn(move || {
        let mut recv_buf = [0u8; 4096];
        let mut decoders: HashMap<u16, Decoder> = HashMap::new();

        while recv_running.load(Ordering::Relaxed) {
            match recv_socket.recv_from(&mut recv_buf) {
                Ok((n, from)) => {
                    if n < 3 { continue; }
                    let pkt_type = recv_buf[0];

                    // Handle GRP_HELLO: new peer connecting
                    if pkt_type == PKT_GRP_HELLO {
                        if let Some((_, group_id_bytes)) = crypto::parse_grp_hello(&recv_buf[..n]) {
                            let gid = crypto::group_id_from_bytes(&group_id_bytes);
                            if gid == recv_group.group_id {
                                let from_ip = from.ip().to_string();
                                if let Some(member) = recv_group.members.iter()
                                    .find(|m| !m.address.is_empty() && from_ip.contains(&m.address))
                                {
                                    log_fmt!("[probe] IN GRP_HELLO from {} ({}), responding GRP_HELLO", member.nickname, from);
                                    let mut peer_map = recv_peers.lock().unwrap();
                                    if !peer_map.contains_key(&member.sender_index) {
                                        peer_map.insert(member.sender_index, PeerConnection {
                                            sender_index: member.sender_index,
                                            peer_addr: from,
                                            last_activity: Instant::now(),
                                        });
                                        log_fmt!("[p2p] peer joined: {} (idx={}) — {} peers connected",
                                            member.nickname, member.sender_index, peer_map.len());
                                        // Send HELLO back so they discover us too
                                        if let Some(gid_bytes) = crypto::group_id_to_bytes(&recv_group.group_id) {
                                            let mut dummy_pubkey = [0u8; 32];
                                            use rand::RngCore;
                                            rand::thread_rng().fill_bytes(&mut dummy_pubkey);
                                            let hello = crypto::build_grp_hello(&dummy_pubkey, &gid_bytes);
                                            let _ = recv_socket.send_to(&hello, from);
                                        }
                                    }
                                }
                            }
                        }
                        continue;
                    }

                    // Read header for group packets
                    let Some((pkt_type, sender_index)) = crypto::grp_read_header(&recv_buf[..n]) else {
                        continue;
                    };

                    // Update activity timestamp and auto-register
                    {
                        let mut peer_map = recv_peers.lock().unwrap();
                        if !peer_map.contains_key(&sender_index) {
                            if let Some(_member) = recv_group.members.iter()
                                .find(|m| m.sender_index == sender_index)
                            {
                                peer_map.insert(sender_index, PeerConnection {
                                    sender_index,
                                    peer_addr: from,
                                    last_activity: Instant::now(),
                                });
                            }
                        }
                        if let Some(p) = peer_map.get_mut(&sender_index) {
                            p.last_activity = Instant::now();
                            p.peer_addr = from;
                        }
                    }

                    match pkt_type {
                        PKT_GRP_VOICE => {
                            // Try current key, then fallback to previous key
                            let decrypted = crypto::grp_decrypt(&recv_cipher, &recv_buf[..n])
                                .or_else(|| recv_prev_cipher.as_ref().and_then(|c| {
                                    let r = crypto::grp_decrypt(c, &recv_buf[..n]);
                                    if r.is_some() {
                                        log_fmt!("[p2p] decrypted voice with previous key (peer needs rotation)");
                                    }
                                    r
                                }));
                            if let Some((_, si, opus_data)) = decrypted {
                                let decoder = decoders.entry(si).or_insert_with(|| {
                                    log_fmt!("[p2p] created decoder for peer idx={}", si);
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
                                        // Compute RMS for speaking indicator
                                        let rms = (frame.iter().map(|s| s * s).sum::<f32>() / frame.len() as f32).sqrt();
                                        recv_voice_levels.lock().unwrap().insert(si, rms);
                                        recv_audio.lock().unwrap().insert(si, frame);
                                    }
                                }
                            }
                        }

                        PKT_GRP_HANGUP => {
                            let nickname = recv_group.members.iter()
                                .find(|m| m.sender_index == sender_index)
                                .map(|m| m.nickname.as_str())
                                .unwrap_or("unknown");
                            let mut peer_map = recv_peers.lock().unwrap();
                            peer_map.remove(&sender_index);
                            log_fmt!("[p2p] peer left: {} (idx={}) — {} remaining",
                                nickname, sender_index, peer_map.len());
                            drop(peer_map);
                            decoders.remove(&sender_index);
                            recv_audio.lock().unwrap().remove(&sender_index);
                            recv_voice_levels.lock().unwrap().remove(&sender_index);
                        }

                        PKT_GRP_SCREEN | PKT_GRP_SCREEN_OFFER | PKT_GRP_SCREEN_STOP => {
                            if let Some((pt, si, data)) = crypto::grp_decrypt(&recv_cipher, &recv_buf[..n]) {
                                match pt {
                                    PKT_GRP_SCREEN => {
                                        recv_screen_viewer.lock().unwrap().receive_chunk(&data);
                                        *recv_screen_sharer.lock().unwrap() = Some(si);
                                    }
                                    PKT_GRP_SCREEN_OFFER => {
                                        *recv_screen_sharer.lock().unwrap() = Some(si);
                                        recv_screen_viewer.lock().unwrap().offer_active = true;
                                    }
                                    PKT_GRP_SCREEN_STOP => {
                                        *recv_screen_sharer.lock().unwrap() = None;
                                        recv_screen_viewer.lock().unwrap().stopped = true;
                                    }
                                    _ => {}
                                }
                            }
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

    // ── Screen command handler thread (P2P) ──
    let pscr_socket = socket.try_clone().unwrap();
    let pscr_running = running.clone();
    let pscr_active = screen_active.clone();
    let pscr_peers = peers.clone();
    let pscr_counter = send_counter.clone();
    let pscr_cipher = crypto::grp_cipher_from_key(&group_key);

    let _screen_handler = thread::spawn(move || {
        while pscr_running.load(Ordering::Relaxed) {
            match screen_cmd_rx.try_recv() {
                Ok(cmd) => {
                    match cmd {
                        ScreenCommand::StartScreen { quality, audio_device: _, display_index } => {
                            pscr_active.store(true, Ordering::Relaxed);
                            let peer_addrs: HashMap<u16, SocketAddr> = pscr_peers.lock().unwrap()
                                .iter().map(|(k, v)| (*k, v.peer_addr)).collect();
                            let target = GroupSendTarget::P2P {
                                peers: Arc::new(Mutex::new(peer_addrs)),
                            };
                            let sock = pscr_socket.try_clone().unwrap();
                            let cipher = pscr_cipher.clone();
                            let counter = pscr_counter.clone();
                            let active = pscr_active.clone();
                            let running = pscr_running.clone();

                            #[cfg(target_os = "linux")]
                            let source = if crate::screen::wayland::is_wayland() {
                                match crate::screen::wayland::WaylandPortal::request()
                                    .and_then(|p| p.new_capture())
                                {
                                    Some(cap) => CaptureSource::PipeWire { capture: cap },
                                    None => CaptureSource::Scrap { display_index },
                                }
                            } else {
                                CaptureSource::Scrap { display_index }
                            };
                            #[cfg(not(target_os = "linux"))]
                            let source = CaptureSource::Scrap { display_index };

                            thread::spawn(move || {
                                crate::screen::group_capture_loop(
                                    sock, cipher, my_sender_index, counter, target,
                                    active, running, quality, source,
                                );
                            });
                        }
                        ScreenCommand::StartWebcam { quality, device_index } => {
                            pscr_active.store(true, Ordering::Relaxed);
                            let peer_addrs: HashMap<u16, SocketAddr> = pscr_peers.lock().unwrap()
                                .iter().map(|(k, v)| (*k, v.peer_addr)).collect();
                            let target = GroupSendTarget::P2P {
                                peers: Arc::new(Mutex::new(peer_addrs)),
                            };
                            let sock = pscr_socket.try_clone().unwrap();
                            let cipher = pscr_cipher.clone();
                            let counter = pscr_counter.clone();
                            let active = pscr_active.clone();
                            let running = pscr_running.clone();
                            thread::spawn(move || {
                                crate::screen::group_capture_loop(
                                    sock, cipher, my_sender_index, counter, target,
                                    active, running, quality, CaptureSource::Webcam { device_index },
                                );
                            });
                        }
                        ScreenCommand::Stop => {
                            pscr_active.store(false, Ordering::Relaxed);
                        }
                        _ => {}
                    }
                }
                Err(mpsc::TryRecvError::Empty) => {
                    thread::sleep(std::time::Duration::from_millis(50));
                }
                Err(mpsc::TryRecvError::Disconnected) => break,
            }
        }
    });

    // ── Sender thread (send to ALL connected peers) ──
    let send_socket = socket.try_clone().unwrap();
    let sender_running = running.clone();
    let sender_mic_active = mic_active.clone();
    let sender_peers = peers.clone();
    let sender_counter = send_counter.clone();
    let sender_cipher = crypto::grp_cipher_from_key(&group_key);
    let sender_voice_levels = voice_levels.clone();

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
            // Drain outgoing chat (no chat in P2P voice channels)
            while chat_out_rx.try_recv().is_ok() {}

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

            // Local voice level for speaking indicator
            let local_rms = (pcm_frame.iter().map(|s| s * s).sum::<f32>() / pcm_frame.len() as f32).sqrt();
            sender_voice_levels.lock().unwrap().insert(my_sender_index, local_rms);

            // Encode + encrypt + send to ALL peers
            let encoded_len = match encoder.encode_float(&pcm_frame, &mut opus_buf) {
                Ok(n) => n,
                Err(_) => continue,
            };
            let counter = sender_counter.fetch_add(1, Ordering::Relaxed);
            let pkt = crypto::grp_encrypt(
                &sender_cipher, my_sender_index, counter,
                PKT_GRP_VOICE, &opus_buf[..encoded_len],
            );
            let peer_map = sender_peers.lock().unwrap();
            if counter % 250 == 0 {
                log_fmt!("[p2p] voice sent {} frames to {} peers", counter, peer_map.len());
            }
            for peer in peer_map.values() {
                let _ = send_socket.send_to(&pkt, peer.peer_addr);
            }
        }
    });

    // ── Housekeeping thread (roster + peer timeout) ──
    let hk_running = running.clone();
    let hk_peers = peers.clone();
    let hk_group = group.clone();
    let hk_roster_tx = roster_tx;

    let _housekeeping = thread::spawn(move || {
        while hk_running.load(Ordering::Relaxed) {
            thread::sleep(std::time::Duration::from_secs(5));
            if !hk_running.load(Ordering::Relaxed) { break; }

            let mut peer_map = hk_peers.lock().unwrap();

            // Build roster from connected peers + ourselves
            let connected_indices: Vec<u16> = peer_map.keys().copied().collect();
            let roster: Vec<GroupMember> = hk_group.members.iter()
                .filter(|m| m.sender_index == my_sender_index || connected_indices.contains(&m.sender_index))
                .cloned()
                .collect();
            let _ = hk_roster_tx.send(roster);

            // Remove timed-out peers (>15s)
            let timeout_indices: Vec<u16> = peer_map.iter()
                .filter(|(_, p)| p.last_activity.elapsed().as_secs() > 30)
                .map(|(idx, _)| *idx)
                .collect();
            for idx in timeout_indices {
                let nickname = hk_group.members.iter()
                    .find(|m| m.sender_index == idx)
                    .map(|m| m.nickname.as_str())
                    .unwrap_or("unknown");
                peer_map.remove(&idx);
                log_fmt!("[p2p] peer timed out: {} (idx={}) — {} remaining",
                    nickname, idx, peer_map.len());
            }
        }
    });

    Ok((GroupCallInfo {
        group,
        role: GroupRole::Leader, // In P2P, everyone is equal — use Leader as placeholder
        channel_id: channel_id.to_string(),
        running,
        mic_active,
        chat_tx: chat_out_tx,
        chat_rx: chat_in_rx,
        roster_rx,
        local_hangup,
        screen_cmd_tx,
        screen_viewer,
        screen_sharer,
        screen_active,
        voice_levels,
    }, audio_keep_alive))
}
