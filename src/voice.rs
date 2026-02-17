use audiopus::coder::{Decoder, Encoder};
use audiopus::packet::Packet;
use audiopus::{Application, Channels, MutSignals, SampleRate};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use ringbuf::traits::{Consumer, Producer, Split};
use ringbuf::HeapRb;
use std::net::UdpSocket;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;

use crate::chat;
use crate::crypto::{self, Session, PKT_CHAT, PKT_HELLO, PKT_IDENTITY, PKT_VOICE};
use crate::firewall::{Action, Firewall};
use crate::identity::{self, Identity};

/// Opus frame size: 960 samples @ 48kHz = 20ms.
const FRAME_SIZE: usize = 960;

/// Max encoded Opus packet size.
const MAX_OPUS_PACKET: usize = 512;

/// Start a full-duplex voice call from the CLI.
pub fn call(peer_ip: &str, peer_port: &str, local_port: &str) {
    let host = cpal::default_host();

    let input_device = host.default_input_device()
        .expect("No microphone found");
    let output_device = host.default_output_device()
        .expect("No speakers found");

    let identity = Identity::load_or_create();
    println!("=== hostelD Secure Voice Call ===");
    println!("Identity: {}", identity.fingerprint);
    println!("Mic:      {}", input_device.name().unwrap_or_default());
    println!("Speakers: {}", output_device.name().unwrap_or_default());
    println!("Local:    [::]:{local_port}");
    println!("Peer:     [{peer_ip}]:{peer_port}");
    println!();

    let running = Arc::new(AtomicBool::new(true));
    let mic_active = Arc::new(AtomicBool::new(true));

    let peer_addr = format!("[{peer_ip}]:{peer_port}");

    let result = start_engine(
        &input_device, &output_device,
        &peer_addr, local_port,
        running.clone(), mic_active,
        &identity,
    );

    match result {
        Ok(engine) => {
            println!("Verification code: {}", engine.verification_code);
            println!("Peer identity:     {}", engine.peer_fingerprint);
            println!();
            println!("Voice call active! (encrypted)");
            println!("Press Ctrl+C to hang up.");

            while running.load(Ordering::Relaxed) {
                // Print incoming chat messages in CLI mode
                if let Some(rx) = &engine.chat_rx {
                    while let Ok(msg) = rx.try_recv() {
                        println!("[chat] {}: {}", engine.peer_fingerprint, msg);
                    }
                }
                thread::sleep(std::time::Duration::from_millis(100));
            }
            println!("Call ended.");
        }
        Err(e) => {
            eprintln!("Failed to establish call: {e}");
        }
    }
}

/// Holds the active voice + chat call state.
pub struct VoiceEngine {
    pub running: Arc<AtomicBool>,
    pub verification_code: String,
    pub peer_fingerprint: String,
    pub contact_id: String,
    /// Send chat text to the network thread
    pub chat_tx: Option<mpsc::Sender<String>>,
    /// Receive chat text from the network thread
    pub chat_rx: Option<mpsc::Receiver<String>>,
    _input_stream: cpal::Stream,
    _output_stream: cpal::Stream,
    _sender: thread::JoinHandle<()>,
    _receiver: thread::JoinHandle<()>,
}

impl Drop for VoiceEngine {
    fn drop(&mut self) {
        self.running.store(false, Ordering::Relaxed);
    }
}

/// Perform the E2E key exchange handshake over UDP.
fn handshake(
    socket: &UdpSocket,
    peer_addr: &str,
) -> Result<Session, String> {
    let (our_secret, our_pubkey) = crypto::generate_keypair();
    let hello = crypto::build_hello(&our_pubkey);

    println!("Key exchange: waiting for peer (sending HELLOs)...");

    let mut buf = [0u8; 1024];
    let mut peer_pubkey = None;

    for _attempt in 0..60 {
        let _ = socket.send_to(&hello, peer_addr);
        socket.set_read_timeout(Some(std::time::Duration::from_millis(500))).unwrap();

        match socket.recv_from(&mut buf) {
            Ok((n, _from)) => {
                if let Some(pk) = crypto::parse_hello(&buf[..n]) {
                    peer_pubkey = Some(pk);
                    break;
                }
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => continue,
            Err(ref e) if e.kind() == std::io::ErrorKind::TimedOut => continue,
            Err(e) => return Err(format!("Socket error during handshake: {e}")),
        }
    }

    let peer_pubkey = peer_pubkey
        .ok_or_else(|| "Handshake timeout: peer did not respond".to_string())?;

    println!("Key exchange: received peer's public key");

    for _ in 0..10 {
        let _ = socket.send_to(&hello, peer_addr);
        thread::sleep(std::time::Duration::from_millis(100));
    }

    let session = crypto::complete_handshake(our_secret, &peer_pubkey);
    println!("Key exchange: complete!");

    Ok(session)
}

/// Exchange persistent identity keys over the encrypted session.
/// Returns the peer's identity public key.
fn exchange_identity(
    socket: &UdpSocket,
    peer_addr: &str,
    session: &Arc<Mutex<Session>>,
    our_identity: &Identity,
) -> Result<[u8; 32], String> {
    // Send our identity pubkey encrypted
    let identity_packet = {
        let mut sess = session.lock().unwrap();
        sess.encrypt_packet(PKT_IDENTITY, &our_identity.pubkey)
    };

    let mut buf = [0u8; 1024];
    let mut peer_identity = None;

    // Send identity and wait for peer's
    for _attempt in 0..30 {
        let _ = socket.send_to(&identity_packet, peer_addr);
        socket.set_read_timeout(Some(std::time::Duration::from_millis(500))).unwrap();

        match socket.recv_from(&mut buf) {
            Ok((n, _)) => {
                let sess = session.lock().unwrap();
                if let Some((pkt_type, plaintext)) = sess.decrypt_packet(&buf[..n]) {
                    if pkt_type == PKT_IDENTITY && plaintext.len() == 32 {
                        let mut pk = [0u8; 32];
                        pk.copy_from_slice(&plaintext);
                        peer_identity = Some(pk);
                        break;
                    }
                }
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => continue,
            Err(ref e) if e.kind() == std::io::ErrorKind::TimedOut => continue,
            Err(e) => return Err(format!("Socket error during identity exchange: {e}")),
        }
    }

    // Send identity a few more times for the peer
    for _ in 0..5 {
        let _ = socket.send_to(&identity_packet, peer_addr);
        thread::sleep(std::time::Duration::from_millis(100));
    }

    peer_identity.ok_or_else(|| "Identity exchange timeout".to_string())
}

/// Starts the secure voice + chat engine.
pub fn start_engine(
    input_device: &cpal::Device,
    output_device: &cpal::Device,
    peer_addr: &str,
    local_port: &str,
    running: Arc<AtomicBool>,
    mic_active: Arc<AtomicBool>,
    our_identity: &Identity,
) -> Result<VoiceEngine, String> {
    // Query each device's native channel count (Voicemeeter on Windows only supports stereo)
    let input_channels = input_device.default_input_config()
        .map(|c| c.channels()).unwrap_or(1);
    let output_channels = output_device.default_output_config()
        .map(|c| c.channels()).unwrap_or(1);

    let input_config = cpal::StreamConfig {
        channels: input_channels,
        sample_rate: cpal::SampleRate(48000),
        buffer_size: cpal::BufferSize::Fixed(FRAME_SIZE as u32),
    };
    let output_config = cpal::StreamConfig {
        channels: output_channels,
        sample_rate: cpal::SampleRate(48000),
        buffer_size: cpal::BufferSize::Fixed(FRAME_SIZE as u32),
    };

    // ── UDP Socket ──
    let bind_addr = format!("[::]:{local_port}");
    let socket = UdpSocket::bind(&bind_addr).map_err(|e| {
        format!("Failed to bind to {bind_addr}: {e}")
    })?;

    // ── Key Exchange Handshake ──
    let session = handshake(&socket, peer_addr)?;
    let verification_code = session.verification_code.clone();
    let session = Arc::new(Mutex::new(session));

    // ── Identity Exchange ──
    let peer_identity_pubkey = exchange_identity(&socket, peer_addr, &session, our_identity)?;
    let peer_fingerprint = crypto::fingerprint(&peer_identity_pubkey);
    let contact_id = identity::derive_contact_id(&our_identity.pubkey, &peer_identity_pubkey);

    println!("Peer identity: {peer_fingerprint}");
    println!("Contact ID:    {contact_id}");

    // Save/update contact
    let existing = identity::load_contact(&peer_fingerprint);
    let contact = identity::Contact {
        fingerprint: peer_fingerprint.clone(),
        pubkey: peer_identity_pubkey,
        nickname: existing.as_ref().map(|c| c.nickname.clone()).unwrap_or_else(|| peer_fingerprint.clone()),
        contact_id: contact_id.clone(),
        first_seen: existing.as_ref().map(|c| c.first_seen.clone()).unwrap_or_else(identity::now_timestamp),
        last_seen: identity::now_timestamp(),
    };
    identity::save_contact(&contact);

    if existing.is_some() {
        println!("Known contact: {}", contact.nickname);
    } else {
        println!("New contact saved!");
    }

    // ── Chat channels (GUI ↔ network threads) ──
    // outgoing_tx: GUI sends text → sender thread picks it up
    let (outgoing_tx, outgoing_rx) = mpsc::channel::<String>();
    // incoming_tx: receiver thread sends text → GUI reads it
    let (incoming_tx, incoming_rx) = mpsc::channel::<String>();

    // ── Ring Buffers ──
    let mic_ring = HeapRb::<f32>::new(48000);
    let (mut mic_producer, mut mic_consumer) = mic_ring.split();

    let spk_ring = HeapRb::<f32>::new(48000);
    let (mut spk_producer, mut spk_consumer) = spk_ring.split();

    let send_socket = socket.try_clone().unwrap();
    let peer_addr_owned = peer_addr.to_string();

    // ── Sender Thread: mic + chat → encrypt → UDP ──
    let running_s = running.clone();
    let mic_active_s = mic_active.clone();
    let session_s = session.clone();

    let sender = thread::spawn(move || {
        let mut encoder = Encoder::new(
            SampleRate::Hz48000, Channels::Mono, Application::Voip,
        ).expect("Failed to create Opus encoder");
        encoder.set_bitrate(audiopus::Bitrate::BitsPerSecond(64000)).unwrap();

        let mut pcm_frame = vec![0f32; FRAME_SIZE];
        let mut opus_buf = vec![0u8; MAX_OPUS_PACKET];

        while running_s.load(Ordering::Relaxed) {
            // Check for outgoing chat messages (non-blocking)
            while let Ok(text) = outgoing_rx.try_recv() {
                let chat_bytes = chat::encode_chat_text(&text);
                let packet = {
                    let mut sess = session_s.lock().unwrap();
                    sess.encrypt_packet(PKT_CHAT, &chat_bytes)
                };
                let _ = send_socket.send_to(&packet, &peer_addr_owned);
            }

            // Collect one audio frame
            let mut collected = 0;
            while collected < FRAME_SIZE {
                if let Some(sample) = mic_consumer.try_pop() {
                    pcm_frame[collected] = if mic_active_s.load(Ordering::Relaxed) {
                        sample
                    } else {
                        0.0
                    };
                    collected += 1;
                } else {
                    thread::sleep(std::time::Duration::from_micros(200));
                    if !running_s.load(Ordering::Relaxed) { return; }
                    // Also check for chat while waiting for audio
                    while let Ok(text) = outgoing_rx.try_recv() {
                        let chat_bytes = chat::encode_chat_text(&text);
                        let packet = {
                            let mut sess = session_s.lock().unwrap();
                            sess.encrypt_packet(PKT_CHAT, &chat_bytes)
                        };
                        let _ = send_socket.send_to(&packet, &peer_addr_owned);
                    }
                }
            }

            // Encode + encrypt + send voice
            let encoded_len = match encoder.encode_float(&pcm_frame, &mut opus_buf) {
                Ok(n) => n,
                Err(_) => continue,
            };
            let packet = {
                let mut sess = session_s.lock().unwrap();
                sess.encrypt_voice(&opus_buf[..encoded_len])
            };
            let _ = send_socket.send_to(&packet, &peer_addr_owned);
        }
    });

    // ── Receiver Thread: UDP → decrypt → voice/chat dispatch ──
    let running_r = running.clone();
    let session_r = session.clone();
    socket.set_read_timeout(Some(std::time::Duration::from_millis(100))).unwrap();

    let receiver = thread::spawn(move || {
        let mut decoder = Decoder::new(SampleRate::Hz48000, Channels::Mono)
            .expect("Failed to create Opus decoder");
        let mut firewall = Firewall::new();
        let mut recv_buf = [0u8; 2048]; // larger for chat messages
        let mut pcm_out = vec![0f32; FRAME_SIZE];

        while running_r.load(Ordering::Relaxed) {
            match socket.recv_from(&mut recv_buf) {
                Ok((bytes_read, from)) => {
                    let ip = from.ip();

                    if firewall.check(ip) == Action::Deny {
                        continue;
                    }

                    // Skip HELLO and IDENTITY packets (handshake is done)
                    if bytes_read > 0 && (recv_buf[0] == PKT_HELLO || recv_buf[0] == PKT_IDENTITY) {
                        continue;
                    }

                    // Decrypt with generic method
                    let decrypted = {
                        let sess = session_r.lock().unwrap();
                        sess.decrypt_packet(&recv_buf[..bytes_read])
                    };

                    match decrypted {
                        Some((PKT_VOICE, opus_data)) => {
                            let packet = match Packet::try_from(opus_data.as_slice()) {
                                Ok(p) => p,
                                Err(_) => continue,
                            };
                            let output = match MutSignals::try_from(&mut pcm_out[..]) {
                                Ok(s) => s,
                                Err(_) => continue,
                            };
                            if let Ok(decoded) = decoder.decode_float(Some(packet), output, false) {
                                for &sample in &pcm_out[..decoded] {
                                    let _ = spk_producer.try_push(sample);
                                }
                            }
                        }
                        Some((PKT_CHAT, chat_data)) => {
                            if let Some(text) = chat::decode_chat_text(&chat_data) {
                                let _ = incoming_tx.send(text);
                            }
                        }
                        Some(_) => {
                            // Unknown packet type, ignore
                        }
                        None => {
                            firewall.record_failure(ip);
                        }
                    }
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(ref e) if e.kind() == std::io::ErrorKind::TimedOut => {}
                Err(_) => {}
            }
        }
    });

    // ── Audio Streams ──
    // If the device is stereo, convert to/from mono at the stream boundary.
    // Opus always works with mono internally.
    let in_ch = input_channels;
    let running_in = running.clone();
    let input_stream = input_device.build_input_stream(
        &input_config,
        move |data: &[f32], _: &cpal::InputCallbackInfo| {
            if in_ch == 1 {
                for &sample in data {
                    let _ = mic_producer.try_push(sample);
                }
            } else {
                // Stereo → mono: average each pair of samples
                for frame in data.chunks(in_ch as usize) {
                    let mono: f32 = frame.iter().sum::<f32>() / in_ch as f32;
                    let _ = mic_producer.try_push(mono);
                }
            }
        },
        move |err| { eprintln!("Mic error: {err}"); running_in.store(false, Ordering::Relaxed); },
        None,
    ).map_err(|e| format!("Failed to build mic stream: {e}"))?;

    let out_ch = output_channels;
    let running_out = running.clone();
    let output_stream = output_device.build_output_stream(
        &output_config,
        move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
            if out_ch == 1 {
                for sample in data.iter_mut() {
                    *sample = spk_consumer.try_pop().unwrap_or(0.0);
                }
            } else {
                // Mono → stereo: duplicate each sample to all channels
                for frame in data.chunks_mut(out_ch as usize) {
                    let mono = spk_consumer.try_pop().unwrap_or(0.0);
                    for ch in frame.iter_mut() {
                        *ch = mono;
                    }
                }
            }
        },
        move |err| { eprintln!("Spk error: {err}"); running_out.store(false, Ordering::Relaxed); },
        None,
    ).map_err(|e| format!("Failed to build speaker stream: {e}"))?;

    input_stream.play().map_err(|e| format!("Failed to start mic: {e}"))?;
    output_stream.play().map_err(|e| format!("Failed to start speakers: {e}"))?;

    Ok(VoiceEngine {
        running,
        verification_code,
        peer_fingerprint,
        contact_id,
        chat_tx: Some(outgoing_tx),
        chat_rx: Some(incoming_rx),
        _input_stream: input_stream,
        _output_stream: output_stream,
        _sender: sender,
        _receiver: receiver,
    })
}
