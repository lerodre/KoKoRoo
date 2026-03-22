use audiopus::coder::{Decoder, Encoder};
use audiopus::packet::Packet;
use audiopus::{Application, Channels, MutSignals, SampleRate};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use ringbuf::traits::{Consumer, Producer, Split};
use ringbuf::{HeapProd, HeapRb};
use std::net::UdpSocket;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Instant;

use nnnoiseless::DenoiseState;

use crate::chat;
use crate::crypto::{self, Session, PKT_CHAT, PKT_HANGUP, PKT_HELLO, PKT_IDENTITY, PKT_SCREEN, PKT_SCREEN_STOP, PKT_SCREEN_OFFER, PKT_SCREEN_JOIN, PKT_VOICE};
use crate::firewall::{Action, Firewall};
use crate::identity::{self, Identity};
use crate::screen::{ScreenQuality, ScreenViewer};

/// Opus frame size: 960 samples @ 48kHz = 20ms.
const FRAME_SIZE: usize = 960;

/// RNNoise frame size: 480 samples @ 48kHz = 10ms.
/// Our Opus frame (960) = exactly 2 RNNoise frames.
const DENOISE_FRAME: usize = 480;

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
    let settings = identity::Settings::load();
    println!("=== KoKoRoo Secure Voice Call ===");
    println!("Identity: {}", identity.fingerprint);
    if !settings.nickname.is_empty() {
        println!("Nickname: {}", settings.nickname);
    }
    println!("Mic:      {}", input_device.name().unwrap_or_default());
    println!("Speakers: {}", output_device.name().unwrap_or_default());
    println!("Local:    [::]:{local_port}");
    println!("Peer:     [{peer_ip}]:{peer_port}");
    println!();

    let running = Arc::new(AtomicBool::new(true));
    let mic_active = Arc::new(AtomicBool::new(true));

    let peer_addr = format!("[{peer_ip}]:{peer_port}");

    if let Ok(port_num) = local_port.parse::<u16>() {
        match crate::platform::ensure_udp_port_open(port_num) {
            Ok(true) => log_fmt!("[firewall] Added rule for UDP port {}", port_num),
            Ok(false) => log_fmt!("[firewall] Rule already exists for UDP port {}", port_num),
            Err(e) => log_fmt!("[firewall] WARNING: {}", e),
        }
    }

    let result = start_engine(
        &input_device, &output_device,
        &peer_addr, local_port,
        running.clone(), mic_active,
        &identity,
        &settings.nickname,
    );

    match result {
        Ok(mut engine) => {
            let peer_display = if engine.peer_nickname.is_empty() {
                engine.peer_fingerprint.clone()
            } else {
                format!("{} #{}", engine.peer_nickname, engine.peer_fingerprint)
            };
            if let Some(ref warning) = engine.key_change_warning {
                println!("\x1b[1;31m{warning}\x1b[0m");
                println!();
            }
            // CLI mode: auto-confirm pending contact
            engine.confirm_contact();
            println!("Verification code: {}", engine.verification_code);
            println!("Peer:              {peer_display}");
            println!();
            println!("Voice call active! (encrypted)");
            println!("Press Ctrl+C to hang up.");

            while running.load(Ordering::Relaxed) {
                // Print incoming chat messages in CLI mode
                if let Some(rx) = &engine.chat_rx {
                    while let Ok(msg) = rx.try_recv() {
                        println!("[chat] {}: {}", peer_display, msg);
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
    pub local_hangup: Arc<AtomicBool>,
    pub verification_code: String,
    pub peer_fingerprint: String,
    pub peer_nickname: String,
    pub contact_id: String,
    /// TOFU warning if peer's key changed or nickname conflict detected
    pub key_change_warning: Option<String>,
    /// Contact pending user confirmation (when key_change_warning is set)
    pub pending_contact: Option<identity::Contact>,
    /// Send chat text to the network thread
    pub chat_tx: Option<mpsc::Sender<String>>,
    /// Receive chat text from the network thread
    pub chat_rx: Option<mpsc::Receiver<String>>,
    /// Screen viewer: assembles + decodes incoming screen frames
    pub screen_viewer: Arc<Mutex<ScreenViewer>>,
    /// Flag: is screen sharing active (we are sharing)?
    pub screen_active: Arc<AtomicBool>,
    /// Flag: has the remote viewer joined our screen share?
    pub viewer_joined: Arc<AtomicBool>,
    /// Screen capture thread handle
    screen_thread: Option<thread::JoinHandle<()>>,
    /// System audio capture: active flag, stream handle, and producer for ring buffer
    sys_audio_active: Arc<AtomicBool>,
    sys_audio_stream: Option<crate::audio::SysAudioStream>,
    sys_audio_producer: Arc<Mutex<Option<HeapProd<f32>>>>,
    /// Wayland portal session (kept alive for the entire call)
    #[cfg(target_os = "linux")]
    wayland_portal: Option<crate::screen::wayland::WaylandPortal>,
    /// Auto-banned IPs reported by the firewall during this call
    pub auto_banned_ips: Arc<Mutex<Vec<String>>>,
    /// Cloned session for screen capture thread to encrypt independently
    session: Arc<Mutex<Session>>,
    /// Socket clone for screen sharing
    send_socket: UdpSocket,
    /// Peer address for screen sharing
    peer_addr: String,
    _input_stream: cpal::Stream,
    _output_stream: cpal::Stream,
    _sender: thread::JoinHandle<()>,
    _receiver: thread::JoinHandle<()>,
}

impl Drop for VoiceEngine {
    fn drop(&mut self) {
        log_fmt!("[voice] call ended");
        self.sys_audio_active.store(false, Ordering::Relaxed);
        // Recover producer before dropping stream (not strictly needed in drop, but clean)
        if let Some(ref stream) = self.sys_audio_stream {
            let _ = stream.take_producer();
        }
        self.sys_audio_stream = None;
        self.screen_active.store(false, Ordering::Relaxed);
        if let Some(t) = self.screen_thread.take() {
            let _ = t.join();
        }
        self.running.store(false, Ordering::Relaxed);
    }
}

impl VoiceEngine {
    /// Start sharing our screen to the peer at the given quality.
    /// `audio_device`: None = no system audio, Some("") = default device, Some(name) = specific device.
    pub fn start_screen_share(&mut self, quality: ScreenQuality, audio_device: Option<String>, display_index: usize) {
        if self.screen_active.load(Ordering::Relaxed) {
            log_fmt!("[voice] start_screen_share: already active, ignoring");
            return;
        }
        log_fmt!("[voice] start_screen_share: launching capture thread, peer={}, quality={:?}, audio_device={:?}", self.peer_addr, quality, audio_device);
        self.screen_active.store(true, Ordering::Relaxed);

        // System audio capture
        if let Some(ref device_name) = audio_device {
            if let Some(producer) = self.sys_audio_producer.lock().unwrap().take() {
                self.sys_audio_active.store(true, Ordering::Relaxed);
                let dev = if device_name.is_empty() { None } else { Some(device_name.as_str()) };
                let (stream, leftover) = crate::audio::start_system_audio_capture(
                    producer,
                    self.sys_audio_active.clone(),
                    dev,
                );
                self.sys_audio_stream = stream;
                if self.sys_audio_stream.is_none() {
                    log_fmt!("[voice] WARNING: system audio capture failed to start");
                    self.sys_audio_active.store(false, Ordering::Relaxed);
                    // Restore producer for future retries
                    if let Some(p) = leftover {
                        *self.sys_audio_producer.lock().unwrap() = Some(p);
                    }
                }
            } else {
                log_fmt!("[voice] WARNING: sys_audio_producer already taken");
            }
        }

        log_fmt!("[voice] screen share: preparing socket/session...");
        let socket = self.send_socket.try_clone().unwrap();
        let session = {
            let sess = self.session.lock().unwrap();
            sess.clone_for_sending()
        };
        let peer_addr: std::net::SocketAddr = self.peer_addr.parse().unwrap();
        let active = self.screen_active.clone();
        let running = self.running.clone();
        log_fmt!("[voice] screen share: spawning capture thread...");

        // Determine capture source: Wayland PipeWire or scrap (X11/Windows)
        #[cfg(target_os = "linux")]
        let source = if crate::screen::wayland::is_wayland() {
            // Create portal session on first use, reuse for subsequent shares
            if self.wayland_portal.is_none() {
                log_fmt!("[voice] Wayland detected, requesting screencast via portal...");
                self.wayland_portal = crate::screen::wayland::WaylandPortal::request();
            }
            match self.wayland_portal.as_ref().and_then(|p| p.new_capture()) {
                Some(capture) => {
                    log_fmt!("[voice] Portal capture: node_id={}, {}x{}", capture.node_id, capture.width, capture.height);
                    crate::screen::CaptureSource::PipeWire { capture }
                }
                None => {
                    log_fmt!("[voice] Wayland portal: user cancelled or unavailable, falling back to scrap");
                    self.wayland_portal = None; // Clear stale portal
                    crate::screen::CaptureSource::Scrap { display_index }
                }
            }
        } else {
            crate::screen::CaptureSource::Scrap { display_index }
        };

        #[cfg(not(target_os = "linux"))]
        let source = crate::screen::CaptureSource::Scrap { display_index };

        let viewer_joined = self.viewer_joined.clone();
        self.screen_thread = Some(thread::spawn(move || {
            log_fmt!("[voice] screen capture thread starting...");
            match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                crate::screen::capture_loop(socket, session, peer_addr, active.clone(), running, quality, source, viewer_joined);
            })) {
                Ok(()) => log_fmt!("[voice] screen capture thread exited normally"),
                Err(e) => {
                    let msg = if let Some(s) = e.downcast_ref::<&str>() {
                        s.to_string()
                    } else if let Some(s) = e.downcast_ref::<String>() {
                        s.clone()
                    } else {
                        "unknown panic".to_string()
                    };
                    log_fmt!("[voice] screen capture thread PANICKED: {}", msg);
                    active.store(false, Ordering::Relaxed);
                }
            }
        }));
    }

    /// Start sharing our webcam to the peer at the given quality.
    pub fn start_webcam_share(&mut self, quality: ScreenQuality, device_index: usize) {
        if self.screen_active.load(Ordering::Relaxed) {
            log_fmt!("[voice] start_webcam_share: already active, stopping first");
            self.stop_screen_share();
        }
        log_fmt!("[voice] start_webcam_share: launching capture thread, peer={}, quality={:?}, device={}",
            self.peer_addr, quality, device_index);
        self.screen_active.store(true, Ordering::Relaxed);

        let socket = self.send_socket.try_clone().unwrap();
        let session = {
            let sess = self.session.lock().unwrap();
            sess.clone_for_sending()
        };
        let peer_addr: std::net::SocketAddr = self.peer_addr.parse().unwrap();
        let active = self.screen_active.clone();
        let running = self.running.clone();

        let source = crate::screen::CaptureSource::Webcam { device_index };
        let viewer_joined = self.viewer_joined.clone();

        self.screen_thread = Some(thread::spawn(move || {
            crate::screen::capture_loop(socket, session, peer_addr, active, running, quality, source, viewer_joined);
            log_fmt!("[voice] webcam capture thread exited");
        }));
    }

    /// Confirm and save the pending contact (used after user accepts a TOFU warning).
    pub fn confirm_contact(&mut self) {
        if let Some(contact) = self.pending_contact.take() {
            identity::save_contact(&contact);
        }
    }

    /// Send PKT_SCREEN_JOIN(0x01) to tell the peer we want to receive their screen frames.
    pub fn send_screen_join(&self) {
        let pkt = {
            let sess = self.session.lock().unwrap();
            sess.encrypt_packet(PKT_SCREEN_JOIN, &[0x01])
        };
        for _ in 0..3 {
            let _ = self.send_socket.send_to(&pkt, &self.peer_addr);
        }
    }

    /// Send PKT_SCREEN_JOIN(0x00) to tell the peer we no longer want screen frames.
    pub fn send_screen_leave(&self) {
        let pkt = {
            let sess = self.session.lock().unwrap();
            sess.encrypt_packet(PKT_SCREEN_JOIN, &[0x00])
        };
        for _ in 0..3 {
            let _ = self.send_socket.send_to(&pkt, &self.peer_addr);
        }
    }

    /// Stop sharing our screen.
    pub fn stop_screen_share(&mut self) {
        log_fmt!("[voice] stop_screen_share");
        self.screen_active.store(false, Ordering::Relaxed);
        // Stop system audio capture — recover producer for reuse
        self.sys_audio_active.store(false, Ordering::Relaxed);
        if let Some(ref stream) = self.sys_audio_stream {
            if let Some(p) = stream.take_producer() {
                log_fmt!("[voice] recovered sys_audio producer for reuse");
                *self.sys_audio_producer.lock().unwrap() = Some(p);
            }
        }
        self.sys_audio_stream = None;
        if let Some(t) = self.screen_thread.take() {
            let _ = t.join();
        }
        // Notify peer that screen sharing has stopped
        let pkt = {
            let sess = self.session.lock().unwrap();
            sess.encrypt_packet(crypto::PKT_SCREEN_STOP, &[])
        };
        for _ in 0..3 {
            let _ = self.send_socket.send_to(&pkt, &self.peer_addr);
        }
    }
}

/// Perform the E2E key exchange handshake over UDP.
fn handshake(
    socket: &UdpSocket,
    peer_addr: &str,
    running: &Arc<AtomicBool>,
) -> Result<Session, String> {
    let (our_secret, our_pubkey) = crypto::generate_keypair();
    let hello = crypto::build_hello(&our_pubkey);

    println!("Key exchange: sending HELLOs to {peer_addr}...");
    println!("Local socket: {:?}", socket.local_addr());

    let mut buf = [0u8; 1024];
    let mut peer_pubkey = None;

    for attempt in 0..60 {
        if !running.load(Ordering::Relaxed) {
            return Err("Cancelled".to_string());
        }
        match socket.send_to(&hello, peer_addr) {
            Ok(n) => {
                if attempt % 10 == 0 {
                    println!("  HELLO #{attempt} sent ({n} bytes) → {peer_addr}");
                }
            }
            Err(e) => println!("  HELLO #{attempt} send error: {e}"),
        }
        socket.set_read_timeout(Some(std::time::Duration::from_millis(500))).unwrap();

        match socket.recv_from(&mut buf) {
            Ok((n, from)) => {
                println!("  Received {n} bytes from {from} (type=0x{:02x})", buf[0]);
                if let Some(pk) = crypto::parse_hello(&buf[..n]) {
                    peer_pubkey = Some(pk);
                    break;
                }
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => continue,
            Err(ref e) if e.kind() == std::io::ErrorKind::TimedOut => continue,
            // Windows WSAECONNRESET (10054): ICMP Port Unreachable from peer not yet listening.
            // Normal during startup — just retry.
            Err(ref e) if e.kind() == std::io::ErrorKind::ConnectionReset => continue,
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

    let (session, _) = crypto::complete_handshake(our_secret, &peer_pubkey);
    println!("Key exchange: complete!");

    Ok(session)
}

/// Exchange persistent identity keys + nicknames over the encrypted session.
/// Returns (peer_pubkey, peer_nickname, our_identity_packet) — the packet is kept
/// so the receiver thread can re-send it if the peer is slow to reach this phase.
fn exchange_identity(
    socket: &UdpSocket,
    peer_addr: &str,
    session: &Arc<Mutex<Session>>,
    our_identity: &Identity,
    our_nickname: &str,
    running: &Arc<AtomicBool>,
) -> Result<([u8; 32], String, Vec<u8>), String> {
    // Build payload: [32-byte pubkey][utf8 nickname bytes]
    let mut payload = Vec::with_capacity(32 + our_nickname.len());
    payload.extend_from_slice(&our_identity.pubkey);
    payload.extend_from_slice(our_nickname.as_bytes());

    let identity_packet = {
        let sess = session.lock().unwrap();
        sess.encrypt_packet(PKT_IDENTITY, &payload)
    };

    let mut buf = [0u8; 1024];
    let mut peer_identity = None;
    let mut peer_nickname = String::new();

    // Send identity and wait for peer's
    for _attempt in 0..30 {
        if !running.load(Ordering::Relaxed) {
            return Err("Cancelled".to_string());
        }
        let _ = socket.send_to(&identity_packet, peer_addr);
        socket.set_read_timeout(Some(std::time::Duration::from_millis(500))).unwrap();

        match socket.recv_from(&mut buf) {
            Ok((n, _)) => {
                let sess = session.lock().unwrap();
                if let Some((pkt_type, plaintext)) = sess.decrypt_packet(&buf[..n]) {
                    if pkt_type == PKT_HANGUP {
                        return Err("Call rejected".to_string());
                    }
                    if pkt_type == PKT_IDENTITY && plaintext.len() >= 32 {
                        let mut pk = [0u8; 32];
                        pk.copy_from_slice(&plaintext[..32]);
                        peer_nickname = String::from_utf8_lossy(&plaintext[32..]).to_string();
                        peer_identity = Some(pk);
                        break;
                    }
                }
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => continue,
            Err(ref e) if e.kind() == std::io::ErrorKind::TimedOut => continue,
            Err(ref e) if e.kind() == std::io::ErrorKind::ConnectionReset => continue,
            Err(e) => return Err(format!("Socket error during identity exchange: {e}")),
        }
    }

    // Send identity a few more times for the peer
    for _ in 0..10 {
        let _ = socket.send_to(&identity_packet, peer_addr);
        thread::sleep(std::time::Duration::from_millis(100));
    }

    peer_identity
        .map(|pk| (pk, peer_nickname, identity_packet))
        .ok_or_else(|| "Identity exchange timeout".to_string())
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
    our_nickname: &str,
) -> Result<VoiceEngine, String> {
    log_fmt!("[voice] start_engine: peer={} local_port={}", peer_addr, local_port);

    // Query each device's native channel count (Voicemeeter on Windows only supports stereo)
    let input_channels = input_device.default_input_config()
        .map(|c| c.channels()).unwrap_or(1);
    let output_channels = output_device.default_output_config()
        .map(|c| c.channels()).unwrap_or(1);
    log_fmt!("[voice] audio: input={}ch, output={}ch", input_channels, output_channels);

    // On macOS, CoreAudio may not respect Fixed buffer sizes and adds its own
    // internal buffering. Using Default lets it pick optimal values for low latency.
    #[cfg(target_os = "macos")]
    let buf_size = cpal::BufferSize::Default;
    #[cfg(not(target_os = "macos"))]
    let buf_size = cpal::BufferSize::Fixed(FRAME_SIZE as u32);

    let input_config = cpal::StreamConfig {
        channels: input_channels,
        sample_rate: cpal::SampleRate(48000),
        buffer_size: buf_size.clone(),
    };
    let output_config = cpal::StreamConfig {
        channels: output_channels,
        sample_rate: cpal::SampleRate(48000),
        buffer_size: buf_size,
    };

    // ── UDP Socket ──
    let bind_addr = format!("[::]:{local_port}");
    let socket = UdpSocket::bind(&bind_addr).map_err(|e| {
        format!("Failed to bind to {bind_addr}: {e}")
    })?;

    // ── Key Exchange Handshake ──
    let session = handshake(&socket, peer_addr, &running)?;
    let verification_code = session.verification_code.clone();
    let session = Arc::new(Mutex::new(session));

    // ── Identity Exchange (with nickname) ──
    let (peer_identity_pubkey, peer_nickname, our_identity_packet) =
        exchange_identity(&socket, peer_addr, &session, our_identity, our_nickname, &running)?;
    let peer_fingerprint = crypto::fingerprint(&peer_identity_pubkey);
    let contact_id = identity::derive_contact_id(&our_identity.pubkey, &peer_identity_pubkey);

    log_fmt!("[voice] call established with {}", if peer_nickname.is_empty() { &peer_fingerprint } else { &peer_nickname });
    println!("Peer identity: {peer_fingerprint}");
    if !peer_nickname.is_empty() {
        println!("Peer nickname: {peer_nickname}");
    }
    println!("Contact ID:    {contact_id}");

    // ── Blocked contact check ──
    let peer_hex = identity::pubkey_hex(&peer_identity_pubkey);
    let settings_check = identity::Settings::load();
    if settings_check.is_blocked(&peer_hex) {
        running.store(false, Ordering::Relaxed);
        return Err("Contact is blocked".to_string());
    }

    // Parse peer address to extract IP and port for contact storage
    let (peer_ip_str, peer_port_str) = parse_peer_addr(peer_addr);

    // ── TOFU: Trust On First Use ──
    let existing = identity::load_contact(&peer_identity_pubkey);
    let mut key_change_warning: Option<String> = None;

    // Check 1: Does this nickname belong to a DIFFERENT key we already know?
    if !peer_nickname.is_empty() {
        let known = identity::find_contacts_by_nickname(&peer_nickname);
        let impostor = known.iter().any(|c| c.pubkey != peer_identity_pubkey);
        if impostor {
            let warning = format!(
                "WARNING: \"{}\" connected with a DIFFERENT key than previously known! Possible impersonation.",
                peer_nickname
            );
            eprintln!("{warning}");
            key_change_warning = Some(warning);
        }
    }

    // Check 2: Did this pubkey previously have a different nickname?
    if let Some(ref ex) = existing {
        if !peer_nickname.is_empty() && !ex.nickname.is_empty() && ex.nickname != peer_nickname {
            let note = format!(
                "Note: Contact changed nickname from \"{}\" to \"{}\"",
                ex.nickname, peer_nickname
            );
            println!("{note}");
            if let Some(ref mut w) = key_change_warning {
                w.push_str(&format!("\n{note}"));
            }
        }
    }

    // Check 3: Unknown pubkey connecting from an address we associate with a known contact?
    // Catches impersonation when attacker doesn't send a nickname.
    if existing.is_none() {
        let all_contacts = identity::load_all_contacts();
        let same_addr = all_contacts.iter().find(|c| {
            c.pubkey != peer_identity_pubkey
                && !c.last_address.is_empty()
                && c.last_address == peer_ip_str
                && c.last_port == peer_port_str
        });
        if let Some(old_contact) = same_addr {
            let warning = format!(
                "WARNING: New unknown key connecting from same address as known contact \"{}\"! Previous key was different.",
                if old_contact.nickname.is_empty() { &old_contact.fingerprint } else { &old_contact.nickname }
            );
            eprintln!("{warning}");
            key_change_warning = Some(warning);
        }
    }

    // Save/update contact — never inherit nickname from a different pubkey
    let contact = identity::Contact {
        fingerprint: peer_fingerprint.clone(),
        pubkey: peer_identity_pubkey,
        nickname: if !peer_nickname.is_empty() {
            peer_nickname.clone()
        } else if let Some(ref ex) = existing {
            // Only reuse nickname if same pubkey (existing != None means same key)
            ex.nickname.clone()
        } else {
            String::new()
        },
        contact_id: contact_id.clone(),
        first_seen: existing.as_ref().map(|c| c.first_seen.clone()).unwrap_or_else(identity::now_timestamp),
        last_seen: identity::now_timestamp(),
        last_address: peer_ip_str,
        last_port: peer_port_str,
        call_count: existing.as_ref().map(|c| c.call_count).unwrap_or(0) + 1,
    };

    // If there's a TOFU warning, defer saving until user confirms
    let pending_contact = if key_change_warning.is_some() {
        Some(contact.clone())
    } else {
        identity::save_contact(&contact);
        None
    };

    if let Some(ref ex) = existing {
        println!("Known contact: {} (call #{})", contact.nickname, contact.call_count);
        if ex.call_count > 5 && key_change_warning.is_none() {
            println!("Trusted contact ({} previous calls)", ex.call_count);
        }
    } else if key_change_warning.is_none() {
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

    // Speaker ring: keep small to minimize audio-video desync.
    // 48000 = 1 second was too much; 9600 = 200ms max audio delay.
    let spk_ring = HeapRb::<f32>::new(9600);
    let (mut spk_producer, mut spk_consumer) = spk_ring.split();

    // System audio ring buffer (for desktop audio sharing)
    let sys_ring = HeapRb::<f32>::new(48000);
    let (sys_producer, mut sys_consumer) = sys_ring.split();
    let sys_audio_active = Arc::new(AtomicBool::new(false));
    let sys_audio_producer = Arc::new(Mutex::new(Some(sys_producer)));

    let send_socket = socket.try_clone().unwrap();
    let screen_socket = socket.try_clone().unwrap();
    let peer_addr_owned = peer_addr.to_string();

    // ── Local hangup flag ──
    let local_hangup = Arc::new(AtomicBool::new(false));

    // ── Sender Thread: mic + chat → encrypt → UDP ──
    let running_s = running.clone();
    let mic_active_s = mic_active.clone();
    let session_s = session.clone();
    let local_hangup_s = local_hangup.clone();
    let sys_audio_active_s = sys_audio_active.clone();

    let sender = thread::spawn(move || {
        let mut encoder = Encoder::new(
            SampleRate::Hz48000, Channels::Mono, Application::Voip,
        ).expect("Failed to create Opus encoder");
        encoder.set_bitrate(audiopus::Bitrate::BitsPerSecond(64000)).unwrap();

        // RNNoise denoiser — processes 480-sample (10ms) frames at 48kHz
        let mut denoiser = DenoiseState::new();
        let mut denoise_in = [0f32; DENOISE_FRAME];
        let mut denoise_out = [0f32; DENOISE_FRAME];

        let mut pcm_frame = vec![0f32; FRAME_SIZE];
        let mut opus_buf = vec![0u8; MAX_OPUS_PACKET];

        while running_s.load(Ordering::Relaxed) {
            // Check for outgoing chat messages (non-blocking)
            while let Ok(text) = outgoing_rx.try_recv() {
                let chat_bytes = chat::encode_chat_text(&text);
                let packet = {
                    let sess = session_s.lock().unwrap();
                    sess.encrypt_packet(PKT_CHAT, &chat_bytes)
                };
                let _ = send_socket.send_to(&packet, &peer_addr_owned);
            }

            // Collect one audio frame
            let mut collected = 0;
            while collected < FRAME_SIZE {
                // Drain all available samples in a burst
                while collected < FRAME_SIZE {
                    if let Some(sample) = mic_consumer.try_pop() {
                        pcm_frame[collected] = if mic_active_s.load(Ordering::Relaxed) {
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
                    if !running_s.load(Ordering::Relaxed) { break; }
                    // Also check for chat while waiting for audio
                    while let Ok(text) = outgoing_rx.try_recv() {
                        let chat_bytes = chat::encode_chat_text(&text);
                        let packet = {
                            let sess = session_s.lock().unwrap();
                            sess.encrypt_packet(PKT_CHAT, &chat_bytes)
                        };
                        let _ = send_socket.send_to(&packet, &peer_addr_owned);
                    }
                }
            }

            if !running_s.load(Ordering::Relaxed) { break; }

            // ── Noise suppression: process 2x 480-sample RNNoise frames ──
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

            // ── Mix system audio (after denoise so RNNoise only cleans mic) ──
            if sys_audio_active_s.load(Ordering::Relaxed) {
                for sample in pcm_frame.iter_mut() {
                    if let Some(s) = sys_consumer.try_pop() {
                        *sample = (*sample + s).clamp(-1.0, 1.0);
                    }
                }
            }

            // Encode + encrypt + send voice
            let encoded_len = match encoder.encode_float(&pcm_frame, &mut opus_buf) {
                Ok(n) => n,
                Err(_) => continue,
            };
            let packet = {
                let sess = session_s.lock().unwrap();
                sess.encrypt_packet(crate::crypto::PKT_VOICE, &opus_buf[..encoded_len])
            };
            let _ = send_socket.send_to(&packet, &peer_addr_owned);
        }

        // If this was a local hangup, send PKT_HANGUP to the peer
        if local_hangup_s.load(Ordering::Relaxed) {
            for _ in 0..3 {
                let packet = {
                    let sess = session_s.lock().unwrap();
                    sess.encrypt_packet(PKT_HANGUP, &[])
                };
                let _ = send_socket.send_to(&packet, &peer_addr_owned);
                thread::sleep(std::time::Duration::from_millis(50));
            }
        }
    });

    // ── Auto-banned IPs sink (shared with GUI for display + persistence) ──
    let auto_banned_ips: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));

    // ── Screen Viewer (shared with GUI) ──
    let screen_viewer = Arc::new(Mutex::new(ScreenViewer::new()));
    let screen_active = Arc::new(AtomicBool::new(false));
    let viewer_joined = Arc::new(AtomicBool::new(false));

    // ── Receiver Thread: UDP → decrypt → voice/chat/screen dispatch ──
    let running_r = running.clone();
    let session_r = session.clone();
    let screen_viewer_r = screen_viewer.clone();
    let viewer_joined_r = viewer_joined.clone();
    let reply_socket = socket.try_clone().unwrap();
    let identity_reply = our_identity_packet.clone();
    let peer_addr_for_recv = peer_addr.to_string();
    socket.set_read_timeout(Some(std::time::Duration::from_millis(100))).unwrap();

    let peer_ip = peer_addr.parse::<std::net::SocketAddr>()
        .map(|sa| sa.ip())
        .unwrap_or(std::net::IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED));
    let auto_banned_ips_r = auto_banned_ips.clone();
    let banned_ips_seed = settings_check.banned_ips.clone();
    let receiver = thread::spawn(move || {
        let mut decoder = Decoder::new(SampleRate::Hz48000, Channels::Mono)
            .expect("Failed to create Opus decoder");
        let mut firewall = Firewall::new_with_bans(&banned_ips_seed, auto_banned_ips_r);
        let mut recv_buf = [0u8; 4096]; // larger for screen chunks + chat
        let mut pcm_out = vec![0f32; FRAME_SIZE];
        let mut screen_chunk_count: u64 = 0;

        // Extract peer's /64 prefix for IPv6 privacy extension handling
        let peer_prefix_64 = match peer_ip {
            std::net::IpAddr::V6(v6) => {
                let seg = v6.segments();
                Some([seg[0], seg[1], seg[2], seg[3]])
            }
            _ => None,
        };

        while running_r.load(Ordering::Relaxed) {
            match socket.recv_from(&mut recv_buf) {
                Ok((bytes_read, from)) => {
                    let ip = from.ip();

                    // Skip firewall for IPs in the peer's /64 subnet
                    // (IPv6 privacy extensions may use different addresses)
                    let is_peer_subnet = match (ip, peer_prefix_64) {
                        (std::net::IpAddr::V6(v6), Some(prefix)) => {
                            let seg = v6.segments();
                            [seg[0], seg[1], seg[2], seg[3]] == prefix
                        }
                        _ => ip == peer_ip,
                    };

                    if !is_peer_subnet && firewall.check(ip) == Action::Deny {
                        continue;
                    }

                    // Skip HELLO packets (handshake is done)
                    if bytes_read > 0 && recv_buf[0] == PKT_HELLO {
                        continue;
                    }

                    // Late IDENTITY packet: peer is still in identity exchange
                    // Re-send our identity so they can complete
                    if bytes_read > 0 && recv_buf[0] == PKT_IDENTITY {
                        let _ = reply_socket.send_to(&identity_reply, &peer_addr_for_recv);
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
                        Some((PKT_SCREEN, screen_data)) => {
                            screen_chunk_count += 1;
                            if let Ok(mut viewer) = screen_viewer_r.lock() {
                                let had_frame_before = viewer.latest_frame.is_some();
                                viewer.receive_chunk(&screen_data);
                                let has_frame_now = viewer.latest_frame.is_some();
                                if !had_frame_before && has_frame_now {
                                    log_fmt!("[voice] screen: decoded frame! (after {} chunks)", screen_chunk_count);
                                    screen_chunk_count = 0;
                                }
                            }
                            if screen_chunk_count > 0 && screen_chunk_count % 100 == 0 {
                                log_fmt!("[voice] screen: received {} chunks (no complete frame yet)", screen_chunk_count);
                            }
                        }
                        Some((PKT_SCREEN_STOP, _)) => {
                            // Remote peer stopped screen sharing
                            log_fmt!("[voice] screen: peer stopped sharing");
                            if let Ok(mut viewer) = screen_viewer_r.lock() {
                                viewer.stopped = true;
                                viewer.offer_active = false;
                            }
                        }
                        Some((PKT_SCREEN_OFFER, _)) => {
                            // Remote peer is offering screen share (beacon)
                            if let Ok(mut viewer) = screen_viewer_r.lock() {
                                viewer.offer_active = true;
                                viewer.last_offer_time = Some(Instant::now());
                            }
                        }
                        Some((PKT_SCREEN_JOIN, data)) => {
                            // Remote viewer is joining/leaving our screen share
                            if data.first() == Some(&0x01) {
                                log_fmt!("[voice] screen: viewer joined");
                                viewer_joined_r.store(true, Ordering::Relaxed);
                            } else {
                                log_fmt!("[voice] screen: viewer left");
                                viewer_joined_r.store(false, Ordering::Relaxed);
                            }
                        }
                        Some((PKT_HANGUP, _)) => {
                            // Remote peer hung up
                            running_r.store(false, Ordering::Relaxed);
                            break;
                        }
                        Some(_) => {
                            // Unknown packet type, ignore
                        }
                        None => {
                            firewall.record_failure(ip);
                        }
                    }
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(std::time::Duration::from_millis(1));
                }
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
        local_hangup,
        verification_code,
        peer_fingerprint,
        peer_nickname,
        contact_id,
        key_change_warning,
        pending_contact,
        chat_tx: Some(outgoing_tx),
        chat_rx: Some(incoming_rx),
        screen_viewer,
        screen_active,
        viewer_joined,
        screen_thread: None,
        #[cfg(target_os = "linux")]
        wayland_portal: None,
        sys_audio_active,
        sys_audio_stream: None,
        sys_audio_producer,
        auto_banned_ips,
        session,
        send_socket: screen_socket,
        peer_addr: peer_addr.to_string(),
        _input_stream: input_stream,
        _output_stream: output_stream,
        _sender: sender,
        _receiver: receiver,
    })
}

/// Parse a peer address like `[::1]:9000` into (ip, port) strings.
fn parse_peer_addr(peer_addr: &str) -> (String, String) {
    // Format: [ip]:port
    if let Some(bracket_end) = peer_addr.rfind(']') {
        let ip = peer_addr[1..bracket_end].to_string();
        let port = if bracket_end + 2 < peer_addr.len() {
            peer_addr[bracket_end + 2..].to_string()
        } else {
            String::new()
        };
        (ip, port)
    } else {
        (peer_addr.to_string(), String::new())
    }
}
