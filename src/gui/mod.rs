mod sidebar;
mod profile;
mod contacts;
mod call;
mod settings;
mod error;
mod messages;
mod requests;

use cpal::traits::{DeviceTrait, HostTrait};
use eframe::egui;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::Instant;

use crate::chat::ChatHistory;
use crate::identity::{self, Contact, Identity, Settings};
use crate::messaging::{MsgCommand, MsgDaemon, MsgEvent};
use crate::screen::{ScreenCommand, ScreenViewer};

// ── App State Machine ──

pub(crate) enum Screen {
    Setup,
    Connecting,
    KeyWarning,
    InCall,
    Error(String),
}

#[derive(Clone, Copy, PartialEq)]
pub(crate) enum SidebarTab {
    Profile,
    Contacts,
    Requests,
    Messages,
    Call,
    Settings,
}

pub(crate) struct DeviceList {
    pub(crate) input_names: Vec<String>,
    pub(crate) output_names: Vec<String>,
}

pub(crate) fn list_audio_devices() -> DeviceList {
    let host = cpal::default_host();
    let input_names: Vec<String> = host.input_devices()
        .map(|devs| devs.map(|d| d.name().unwrap_or_else(|_| "unknown".into())).collect())
        .unwrap_or_default();
    let output_names: Vec<String> = host.output_devices()
        .map(|devs| devs.map(|d| d.name().unwrap_or_else(|_| "unknown".into())).collect())
        .unwrap_or_default();
    DeviceList { input_names, output_names }
}

/// Get the best (non-temporary, non-loopback) IPv6 address, optionally filtered by adapter.
pub(crate) fn get_best_ipv6(adapter: &str) -> String {
    let ifaces = get_network_interfaces();
    let filtered: Vec<&(String, String, String)> = if adapter.is_empty() {
        ifaces.iter().collect()
    } else {
        ifaces.iter().filter(|(iface, _, _)| iface == adapter).collect()
    };
    // Prefer global non-loopback
    for (_, ip, scope) in &filtered {
        if *ip != "::1" && scope == "global" {
            return ip.clone();
        }
    }
    // Fallback to link-local
    for (_, ip, _) in &filtered {
        if *ip != "::1" {
            return ip.clone();
        }
    }
    "::1".to_string()
}

/// Get unique network adapter names (excluding loopback and docker/veth).
pub(crate) fn get_adapter_names() -> Vec<String> {
    let ifaces = get_network_interfaces();
    let mut names: Vec<String> = Vec::new();
    for (iface, _, _) in &ifaces {
        if iface != "lo" && !names.contains(iface) {
            names.push(iface.clone());
        }
    }
    names
}

/// Returns (interface_name, ip, scope) for all non-temporary IPv6 addresses.
fn get_network_interfaces() -> Vec<(String, String, String)> {
    let mut result: Vec<(String, String, String)> = Vec::new();

    #[cfg(target_os = "linux")]
    {
        if let Ok(output) = std::process::Command::new("ip").args(["-6", "addr", "show"]).output() {
            let text = String::from_utf8_lossy(&output.stdout);
            let mut current_iface = String::new();
            for line in text.lines() {
                let trimmed = line.trim();
                // Interface line: "3: wlp3s0: <BROADCAST,..."
                if !trimmed.starts_with("inet6") && trimmed.contains(": <") {
                    if let Some(name) = trimmed.split(':').nth(1) {
                        current_iface = name.trim().to_string();
                        // Strip @... suffix (e.g. "vethd3f93b1@enp2s0")
                        if let Some(pos) = current_iface.find('@') {
                            current_iface.truncate(pos);
                        }
                    }
                }
                if trimmed.starts_with("inet6") {
                    // Skip temporary privacy extension addresses
                    if trimmed.contains("temporary") {
                        continue;
                    }
                    if let Some(addr_cidr) = trimmed.split_whitespace().nth(1) {
                        let addr = addr_cidr.split('/').next().unwrap_or(addr_cidr);
                        if addr == "::1" { continue; }
                        let scope = if trimmed.contains("scope global") { "global" }
                            else if trimmed.contains("scope link") { "link-local" }
                            else { "other" };
                        result.push((current_iface.clone(), addr.to_string(), scope.to_string()));
                    }
                }
            }
        }
    }

    #[cfg(target_os = "windows")]
    {
        // Use PowerShell CSV output to handle adapter names with spaces (e.g. "Ethernet 2", "vEthernet (WSL)")
        if let Ok(output) = std::process::Command::new("powershell")
            .args(["-Command", "Get-NetIPAddress -AddressFamily IPv6 | Where-Object { $_.SuffixOrigin -ne 'Random' -and $_.IPAddress -ne '::1' -and $_.AddressState -eq 'Preferred' } | Select-Object InterfaceAlias, IPAddress | ConvertTo-Csv -NoTypeInformation"])
            .output()
        {
            let text = String::from_utf8_lossy(&output.stdout);
            for line in text.lines().skip(1) { // skip CSV header
                let line = line.trim();
                if line.is_empty() { continue; }
                // CSV format: "InterfaceAlias","IPAddress"
                let fields: Vec<&str> = line.split(',').collect();
                if fields.len() >= 2 {
                    let iface = fields[0].trim_matches('"').to_string();
                    let addr = fields[1].trim_matches('"').to_string();
                    if addr == "::1" || addr.is_empty() { continue; }
                    let scope = if addr.starts_with("fe80") { "link-local" } else { "global" };
                    result.push((iface, addr, scope.to_string()));
                }
            }
        }
    }

    // Sort: global first per interface
    result.sort_by_key(|(_, _, scope)| {
        match scope.as_str() {
            "global" => 0,
            "link-local" => 1,
            _ => 2,
        }
    });
    result
}

/// Format a contact/peer for display: "nickname #fingerprint" or just fingerprint.
pub(crate) fn format_peer_display(nickname: &str, fingerprint: &str) -> String {
    if nickname.is_empty() {
        fingerprint.to_string()
    } else {
        format!("{nickname} #{fingerprint}")
    }
}

/// Censor an IP address: show first group, mask the rest.
/// e.g. "2803:c600:d310:..." → "2803:****"
pub(crate) fn censor_ip(ip: &str) -> String {
    if ip == "::1" || ip.is_empty() {
        return ip.to_string();
    }
    // Find the first ':' and keep everything before it
    if let Some(pos) = ip.find(':') {
        format!("{}:****", &ip[..pos])
    } else if let Some(pos) = ip.find('.') {
        // IPv4: show first octet
        format!("{}.***.***", &ip[..pos])
    } else {
        "****".to_string()
    }
}

// ── Connection result sent from background thread ──

pub(crate) struct CallInfo {
    pub(crate) verification_code: String,
    pub(crate) peer_fingerprint: String,
    pub(crate) peer_nickname: String,
    pub(crate) contact_id: String,
    pub(crate) key_change_warning: Option<String>,
    pub(crate) pending_contact: Option<Contact>,
    pub(crate) chat_tx: mpsc::Sender<String>,
    pub(crate) chat_rx: mpsc::Receiver<String>,
    pub(crate) local_hangup: Arc<AtomicBool>,
    pub(crate) screen_viewer: Arc<Mutex<ScreenViewer>>,
    pub(crate) screen_active: Arc<AtomicBool>,
    pub(crate) screen_cmd_tx: mpsc::Sender<ScreenCommand>,
    pub(crate) auto_banned_ips: Arc<Mutex<Vec<String>>>,
}

pub struct HostelApp {
    pub(crate) screen: Screen,
    pub(crate) identity: Identity,
    pub(crate) settings: Settings,

    // Sidebar
    pub(crate) active_tab: SidebarTab,
    pub(crate) best_ipv6: String,
    pub(crate) viewing_contact: Option<Contact>,
    pub(crate) viewing_chat: Option<ChatHistory>,

    // Setup
    pub(crate) selected_input: usize,
    pub(crate) selected_output: usize,
    pub(crate) local_port: String,
    pub(crate) peer_ip: String,
    pub(crate) peer_port: String,
    pub(crate) devices: DeviceList,
    pub(crate) adapter_names: Vec<String>,

    // Contact list
    pub(crate) contacts: Vec<Contact>,
    pub(crate) contact_search: String,

    // Call state
    pub(crate) running: Arc<AtomicBool>,
    pub(crate) mic_active: Arc<AtomicBool>,
    pub(crate) local_hangup: Option<Arc<AtomicBool>>,
    pub(crate) verification_code: String,
    pub(crate) peer_fingerprint: String,
    pub(crate) peer_nickname: String,
    pub(crate) contact_id: String,
    pub(crate) key_change_warning: Option<String>,
    pub(crate) pending_contact: Option<Contact>,

    // Chat
    pub(crate) chat_tx: Option<mpsc::Sender<String>>,
    pub(crate) chat_rx: Option<mpsc::Receiver<String>>,
    pub(crate) chat_input: String,
    pub(crate) chat_history: Option<ChatHistory>,

    // Screen sharing
    pub(crate) screen_sharing: bool,
    pub(crate) screen_texture: Option<egui::TextureHandle>,
    pub(crate) screen_viewer: Option<Arc<Mutex<ScreenViewer>>>,
    pub(crate) screen_active: Option<Arc<AtomicBool>>,
    pub(crate) screen_cmd_tx: Option<mpsc::Sender<ScreenCommand>>,
    pub(crate) selected_screen_quality: usize,
    pub(crate) show_screen_popup: bool,
    pub(crate) show_hangup_confirm: bool,
    pub(crate) selected_audio_device: usize,
    pub(crate) loopback_devices: Vec<String>,
    pub(crate) selected_display: usize,
    pub(crate) display_names: Vec<String>,
    // Webcam sharing
    pub(crate) show_webcam_popup: bool,
    pub(crate) webcam_sharing: bool,
    pub(crate) selected_camera: usize,
    pub(crate) camera_names: Vec<String>,

    pub(crate) auto_banned_ips: Option<Arc<Mutex<Vec<String>>>>,

    pub(crate) video_fullscreen: bool,
    pub(crate) last_mouse_move: Instant,
    pub(crate) last_frame_time: Option<Instant>,
    pub(crate) is_fullscreen: bool,

    // Async connection result
    pub(crate) connect_result: Arc<std::sync::Mutex<Option<Result<CallInfo, String>>>>,

    // Messaging daemon
    pub(crate) msg_cmd_tx: Option<mpsc::Sender<MsgCommand>>,
    pub(crate) msg_event_rx: Option<mpsc::Receiver<MsgEvent>>,
    pub(crate) msg_active_chat: Option<String>,
    pub(crate) msg_chat_input: String,
    pub(crate) msg_chat_histories: HashMap<String, ChatHistory>,
    pub(crate) msg_unread: HashMap<String, u32>,
    pub(crate) msg_peer_online: HashMap<String, bool>,
    pub(crate) msg_show_contact_picker: bool,

    // Contact requests
    pub(crate) req_incoming: Vec<(String, String, String, String)>, // (request_id, nickname, ip, fingerprint)
    pub(crate) req_ip_input: String,
    pub(crate) req_port_input: String,
    pub(crate) req_status: String,

    // IP privacy: censored by default
    pub(crate) show_ips: bool,

    // Incoming call notification
    pub(crate) incoming_call: Option<IncomingCallInfo>,
}

pub(crate) struct IncomingCallInfo {
    pub(crate) nickname: String,
    pub(crate) fingerprint: String,
    pub(crate) ip: String,
    pub(crate) port: String,
}

impl HostelApp {
    pub fn new(_cc: &eframe::CreationContext<'_>) -> Self {
        let devices = list_audio_devices();
        let identity = Identity::load_or_create();
        let settings = Settings::load();
        let contacts = identity::load_all_contacts();
        let best_ipv6 = get_best_ipv6(&settings.network_adapter);

        let selected_input = if !settings.mic.is_empty() {
            devices.input_names.iter().position(|n| n == &settings.mic).unwrap_or(0)
        } else {
            0
        };
        let selected_output = if !settings.speakers.is_empty() {
            devices.output_names.iter().position(|n| n == &settings.speakers).unwrap_or(0)
        } else {
            0
        };
        let local_port = if settings.local_port.is_empty() {
            "9000".to_string()
        } else {
            settings.local_port.clone()
        };

        // Spawn messaging daemon
        let (cmd_tx, cmd_rx) = mpsc::channel::<MsgCommand>();
        let (evt_tx, evt_rx) = mpsc::channel::<MsgEvent>();
        let daemon_port = local_port.clone();
        let daemon_identity = Identity {
            secret: identity.secret,
            pubkey: identity.pubkey,
            fingerprint: identity.fingerprint.clone(),
        };
        let daemon_nickname = settings.nickname.clone();
        thread::spawn(move || {
            let mut daemon = MsgDaemon::new(
                daemon_port, daemon_identity, daemon_nickname, cmd_rx, evt_tx,
            );
            daemon.run();
        });

        Self {
            screen: Screen::Setup,
            identity,
            settings,
            active_tab: SidebarTab::Call,
            best_ipv6,
            viewing_contact: None,
            viewing_chat: None,
            selected_input,
            selected_output,
            local_port,
            peer_ip: "::1".to_string(),
            peer_port: "9000".to_string(),
            devices,
            adapter_names: Vec::new(),
            contacts,
            contact_search: String::new(),
            running: Arc::new(AtomicBool::new(false)),
            mic_active: Arc::new(AtomicBool::new(true)),
            local_hangup: None,
            verification_code: String::new(),
            peer_fingerprint: String::new(),
            peer_nickname: String::new(),
            contact_id: String::new(),
            key_change_warning: None,
            pending_contact: None,
            chat_tx: None,
            chat_rx: None,
            chat_input: String::new(),
            chat_history: None,
            screen_sharing: false,
            screen_texture: None,
            screen_viewer: None,
            screen_active: None,
            screen_cmd_tx: None,
            selected_screen_quality: 0,
            show_screen_popup: false,
            show_hangup_confirm: false,
            selected_audio_device: 0,
            loopback_devices: Vec::new(),
            selected_display: 0,
            display_names: Vec::new(),
            show_webcam_popup: false,
            webcam_sharing: false,
            selected_camera: 0,
            camera_names: Vec::new(),
            auto_banned_ips: None,
            video_fullscreen: false,
            last_mouse_move: Instant::now(),
            last_frame_time: None,
            is_fullscreen: false,
            connect_result: Arc::new(std::sync::Mutex::new(None)),
            msg_cmd_tx: Some(cmd_tx),
            msg_event_rx: Some(evt_rx),
            msg_active_chat: None,
            msg_chat_input: String::new(),
            msg_chat_histories: HashMap::new(),
            msg_unread: HashMap::new(),
            msg_peer_online: HashMap::new(),
            msg_show_contact_picker: false,
            req_incoming: Vec::new(),
            req_ip_input: String::new(),
            req_port_input: String::new(),
            req_status: String::new(),
            show_ips: false,
            incoming_call: None,
        }
    }

    pub(crate) fn start_call(&mut self) {
        // Tell messaging daemon to release the socket for voice
        if let Some(tx) = &self.msg_cmd_tx {
            tx.send(MsgCommand::YieldSocket).ok();
        }

        // Save settings before starting the call
        self.settings.mic = self.devices.input_names.get(self.selected_input)
            .cloned().unwrap_or_default();
        self.settings.speakers = self.devices.output_names.get(self.selected_output)
            .cloned().unwrap_or_default();
        self.settings.local_port = self.local_port.clone();
        self.settings.save();

        // Detach old thread: set old running=false so it exits, then create fresh Arcs
        // so old threads can't interfere with the new call.
        self.running.store(false, Ordering::Relaxed);
        self.running = Arc::new(AtomicBool::new(true));
        self.mic_active = Arc::new(AtomicBool::new(true));
        self.connect_result = Arc::new(std::sync::Mutex::new(None));

        self.screen = Screen::Connecting;
        self.active_tab = SidebarTab::Call;

        let peer_addr = format!("[{}]:{}", self.peer_ip, self.peer_port);
        let local_port = self.local_port.clone();
        let running = self.running.clone();
        let mic_active = self.mic_active.clone();
        let result = self.connect_result.clone();
        let input_idx = self.selected_input;
        let output_idx = self.selected_output;

        let identity_secret = self.identity.secret;
        let identity_pubkey = self.identity.pubkey;
        let identity_fingerprint = self.identity.fingerprint.clone();
        let our_nickname = self.settings.nickname.clone();

        thread::spawn(move || {
            let result2 = result.clone();
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let host = cpal::default_host();
                let input_device = host.input_devices().ok().and_then(|mut d| d.nth(input_idx));
                let output_device = host.output_devices().ok().and_then(|mut d| d.nth(output_idx));

                let (input_device, output_device) = match (input_device, output_device) {
                    (Some(i), Some(o)) => (i, o),
                    _ => {
                        *result.lock().unwrap() = Some(Err("Audio device not found".into()));
                        return;
                    }
                };

                let identity = Identity {
                    secret: identity_secret,
                    pubkey: identity_pubkey,
                    fingerprint: identity_fingerprint,
                };

                if let Ok(port_num) = local_port.parse::<u16>() {
                    match crate::sysfirewall::ensure_udp_port_open(port_num) {
                        Ok(true) => log_fmt!("[firewall] Added rule for UDP port {}", port_num),
                        Ok(false) => log_fmt!("[firewall] Rule already exists for UDP port {}", port_num),
                        Err(e) => log_fmt!("[firewall] WARNING: {}", e),
                    }
                }

                let (screen_cmd_tx, screen_cmd_rx) = mpsc::channel::<ScreenCommand>();

                match crate::voice::start_engine(
                    &input_device, &output_device,
                    &peer_addr, &local_port,
                    running.clone(), mic_active, &identity,
                    &our_nickname,
                ) {
                    Ok(mut engine) => {
                        let info = CallInfo {
                            verification_code: engine.verification_code.clone(),
                            peer_fingerprint: engine.peer_fingerprint.clone(),
                            peer_nickname: engine.peer_nickname.clone(),
                            contact_id: engine.contact_id.clone(),
                            key_change_warning: engine.key_change_warning.clone(),
                            pending_contact: engine.pending_contact.take(),
                            chat_tx: engine.chat_tx.take().unwrap(),
                            chat_rx: engine.chat_rx.take().unwrap(),
                            local_hangup: engine.local_hangup.clone(),
                            screen_viewer: engine.screen_viewer.clone(),
                            screen_active: engine.screen_active.clone(),
                            screen_cmd_tx,
                            auto_banned_ips: engine.auto_banned_ips.clone(),
                        };
                        *result.lock().unwrap() = Some(Ok(info));

                        while running.load(Ordering::Relaxed) {
                            while let Ok(cmd) = screen_cmd_rx.try_recv() {
                                match cmd {
                                    ScreenCommand::StartScreen { quality, audio_device, display_index } => engine.start_screen_share(quality, audio_device, display_index),
                                    ScreenCommand::StartWebcam { quality, device_index } => engine.start_webcam_share(quality, device_index),
                                    ScreenCommand::Stop => engine.stop_screen_share(),
                                }
                            }
                            thread::sleep(std::time::Duration::from_millis(100));
                        }
                        drop(engine);
                    }
                    Err(e) => {
                        *result.lock().unwrap() = Some(Err(e));
                    }
                }
            }));

            if let Err(panic) = outcome {
                let msg = if let Some(s) = panic.downcast_ref::<&str>() {
                    format!("Internal error (panic): {s}")
                } else if let Some(s) = panic.downcast_ref::<String>() {
                    format!("Internal error (panic): {s}")
                } else {
                    "Internal error (panic): unknown".to_string()
                };
                *result2.lock().unwrap() = Some(Err(msg));
            }
        });
    }

    pub(crate) fn hang_up(&mut self) {
        if let Some(ref lh) = self.local_hangup {
            lh.store(true, Ordering::Relaxed);
        }
        self.running.store(false, Ordering::Relaxed);
        self.cleanup_call();
    }

    pub(crate) fn on_remote_hangup(&mut self) {
        self.running.store(false, Ordering::Relaxed);
        self.cleanup_call();
    }

    pub(crate) fn cleanup_call(&mut self) {
        self.local_hangup = None;
        self.verification_code.clear();
        self.peer_fingerprint.clear();
        self.peer_nickname.clear();
        self.contact_id.clear();
        self.key_change_warning = None;
        self.pending_contact = None;
        self.chat_tx = None;
        self.chat_rx = None;
        self.chat_input.clear();
        self.chat_history = None;
        self.screen_sharing = false;
        self.show_screen_popup = false;
        self.webcam_sharing = false;
        self.show_webcam_popup = false;
        self.selected_camera = 0;
        self.camera_names.clear();
        self.show_hangup_confirm = false;
        self.selected_audio_device = 0;
        self.loopback_devices.clear();
        self.selected_display = 0;
        self.display_names.clear();
        self.screen_texture = None;
        self.screen_viewer = None;
        self.screen_active = None;
        self.screen_cmd_tx = None;
        self.auto_banned_ips = None;
        self.video_fullscreen = false;
        self.last_frame_time = None;
        self.is_fullscreen = false;
        self.viewing_contact = None;
        self.viewing_chat = None;
        *self.connect_result.lock().unwrap() = None;
        self.contacts = identity::load_all_contacts();
        self.screen = Screen::Setup;

        // Tell messaging daemon to reclaim the socket
        if let Some(tx) = &self.msg_cmd_tx {
            tx.send(MsgCommand::ReclaimSocket).ok();
        }
    }
}

impl eframe::App for HostelApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Check for async connection result
        if matches!(self.screen, Screen::Connecting) {
            let mut lock = self.connect_result.lock().unwrap();
            if let Some(res) = lock.take() {
                match res {
                    Ok(info) => {
                        self.verification_code = info.verification_code;
                        self.peer_fingerprint = info.peer_fingerprint;
                        self.peer_nickname = info.peer_nickname;
                        self.contact_id = info.contact_id.clone();
                        self.key_change_warning = info.key_change_warning.clone();
                        self.pending_contact = info.pending_contact;
                        self.chat_tx = Some(info.chat_tx);
                        self.chat_rx = Some(info.chat_rx);
                        self.local_hangup = Some(info.local_hangup);
                        self.screen_viewer = Some(info.screen_viewer);
                        self.screen_active = Some(info.screen_active);
                        self.screen_cmd_tx = Some(info.screen_cmd_tx);
                        self.auto_banned_ips = Some(info.auto_banned_ips);
                        self.chat_history = Some(ChatHistory::load(
                            &info.contact_id,
                            &self.identity.secret,
                        ));
                        if info.key_change_warning.is_some() {
                            self.screen = Screen::KeyWarning;
                        } else {
                            self.screen = Screen::InCall;
                        }
                    }
                    Err(e) => {
                        self.running.store(false, Ordering::Relaxed);
                        if e == "Cancelled" {
                            self.screen = Screen::Setup;
                        } else {
                            self.screen = Screen::Error(e);
                        }
                    }
                }
            }
            drop(lock);
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }

        // Detect remote hangup while on KeyWarning screen
        if matches!(self.screen, Screen::KeyWarning) {
            if !self.running.load(Ordering::Relaxed) {
                self.on_remote_hangup();
                return;
            }
            ctx.request_repaint_after(std::time::Duration::from_millis(200));
        }

        // Poll incoming chat messages + detect remote hangup
        if matches!(self.screen, Screen::InCall) {
            if let Some(rx) = &self.chat_rx {
                while let Ok(text) = rx.try_recv() {
                    if let Some(history) = &mut self.chat_history {
                        history.add_message(false, text);
                    }
                }
            }
            if !self.running.load(Ordering::Relaxed) {
                self.on_remote_hangup();
                ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(false));
                return;
            }
            if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
                if self.video_fullscreen {
                    self.video_fullscreen = false;
                    self.is_fullscreen = false;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(false));
                } else if self.is_fullscreen {
                    self.is_fullscreen = false;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(false));
                }
            }

            let mouse_delta = ctx.input(|i| i.pointer.delta());
            if mouse_delta != egui::Vec2::ZERO {
                self.last_mouse_move = Instant::now();
            }

            let repaint_ms = if self.screen_texture.is_some() { 33 } else { 200 };
            ctx.request_repaint_after(std::time::Duration::from_millis(repaint_ms));
        }

        // Poll messaging daemon events
        let mut accepted_contact_id: Option<String> = None;
        if let Some(rx) = &self.msg_event_rx {
            while let Ok(evt) = rx.try_recv() {
                match evt {
                    MsgEvent::IncomingMessage { contact_id, text, .. } => {
                        // Append to chat history
                        let history = self.msg_chat_histories.entry(contact_id.clone())
                            .or_insert_with(|| ChatHistory::load(&contact_id, &self.identity.secret));
                        history.add_message(false, text);
                        // Increment unread if not viewing this chat
                        if self.msg_active_chat.as_deref() != Some(&contact_id) {
                            *self.msg_unread.entry(contact_id).or_insert(0) += 1;
                        }
                    }
                    MsgEvent::MessageDelivered { .. } => {
                        // Could update delivery status UI here
                    }
                    MsgEvent::PeerStatus { contact_id, online } => {
                        self.msg_peer_online.insert(contact_id, online);
                    }
                    MsgEvent::IncomingRequest { request_id, nickname, ip, fingerprint } => {
                        // Add to incoming requests if not already present
                        if !self.req_incoming.iter().any(|(rid, ..)| rid == &request_id) {
                            self.req_incoming.push((request_id, nickname, ip, fingerprint));
                        }
                    }
                    MsgEvent::RequestAccepted { contact_id } => {
                        self.req_status = "Request accepted! Contact saved.".to_string();
                        // Reload contacts to include the new one
                        self.contacts = identity::load_all_contacts();
                        accepted_contact_id = Some(contact_id);
                    }
                    MsgEvent::RequestFailed { peer_addr, reason } => {
                        self.req_status = format!("Failed ({}): {}", peer_addr, reason);
                    }
                    MsgEvent::IncomingCall { nickname, fingerprint, ip, port } => {
                        // Only show if not already in a call
                        if matches!(self.screen, Screen::Setup) {
                            self.incoming_call = Some(IncomingCallInfo {
                                nickname, fingerprint, ip, port,
                            });
                        }
                    }
                }
            }
        }
        // Deferred: open chat for newly accepted contact (after borrow of msg_event_rx ends)
        if let Some(cid) = accepted_contact_id {
            self.open_msg_chat(&cid);
        }

        // Style
        let mut style = (*ctx.style()).clone();
        style.spacing.item_spacing = egui::vec2(8.0, 6.0);
        style.spacing.button_padding = egui::vec2(14.0, 6.0);
        ctx.set_style(style);

        // Force Call tab when call is active
        let in_call = matches!(self.screen, Screen::Connecting | Screen::KeyWarning | Screen::InCall | Screen::Error(_));
        if in_call {
            self.active_tab = SidebarTab::Call;
        }

        // Video-only fullscreen: skip all UI except video + overlay
        if self.video_fullscreen && matches!(self.screen, Screen::InCall) {
            egui::CentralPanel::default().show(ctx, |ui| {
                self.draw_fullscreen_video(ui);
            });
            return;
        }

        // ── Left sidebar ──
        egui::SidePanel::left("sidebar")
            .exact_width(84.0)
            .resizable(false)
            .show(ctx, |ui| {
                self.draw_sidebar(ui, in_call);
            });

        // ── Central panel: dispatch by active tab ──
        egui::CentralPanel::default().show(ctx, |ui| {
            match self.active_tab {
                SidebarTab::Profile => self.draw_profile_tab(ui),
                SidebarTab::Contacts => self.draw_contacts_tab(ui),
                SidebarTab::Requests => self.draw_requests_tab(ui),
                SidebarTab::Messages => self.draw_messages_tab(ui),
                SidebarTab::Call => {
                    match &self.screen {
                        Screen::Setup => self.draw_call_tab(ui),
                        Screen::Connecting => self.draw_connecting(ui, ctx),
                        Screen::KeyWarning => self.draw_key_warning(ui),
                        Screen::InCall => self.draw_call(ui),
                        Screen::Error(_) => {
                            let msg = if let Screen::Error(m) = &self.screen { m.clone() } else { unreachable!() };
                            self.draw_error(ui, &msg);
                        }
                    }
                }
                SidebarTab::Settings => self.draw_settings_tab(ui),
            }
        });

        // ── Incoming call popup (overlay on top of everything) ──
        if self.incoming_call.is_some() {
            self.draw_incoming_call_popup(ctx);
        }
    }
}

impl HostelApp {
    fn draw_incoming_call_popup(&mut self, ctx: &egui::Context) {
        let call_info = match &self.incoming_call {
            Some(info) => info,
            None => return,
        };
        let caller = if call_info.nickname.is_empty() {
            format!("#{}", call_info.fingerprint)
        } else {
            format!("{} #{}", call_info.nickname, call_info.fingerprint)
        };
        let ip_display = if self.show_ips {
            format!("[{}]:{}", call_info.ip, call_info.port)
        } else {
            format!("[{}]:{}", censor_ip(&call_info.ip), call_info.port)
        };

        let mut accept = false;
        let mut reject = false;

        egui::Window::new("Incoming Call")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.add_space(8.0);
                ui.vertical_centered(|ui| {
                    ui.label(
                        egui::RichText::new("Incoming Call")
                            .size(20.0)
                            .strong()
                            .color(egui::Color32::from_rgb(80, 200, 80)),
                    );
                    ui.add_space(8.0);
                    ui.label(egui::RichText::new(&caller).size(16.0).strong());
                    ui.colored_label(egui::Color32::GRAY, &ip_display);
                    ui.add_space(12.0);
                });
                ui.horizontal(|ui| {
                    let accept_btn = egui::Button::new(
                        egui::RichText::new("Accept").size(16.0).color(egui::Color32::WHITE),
                    )
                    .min_size(egui::vec2(120.0, 38.0))
                    .fill(egui::Color32::from_rgb(40, 140, 60));
                    if ui.add(accept_btn).clicked() {
                        accept = true;
                    }

                    let reject_btn = egui::Button::new(
                        egui::RichText::new("Reject").size(16.0).color(egui::Color32::WHITE),
                    )
                    .min_size(egui::vec2(120.0, 38.0))
                    .fill(egui::Color32::from_rgb(180, 40, 40));
                    if ui.add(reject_btn).clicked() {
                        reject = true;
                    }
                });
                ui.add_space(4.0);
            });

        if accept {
            if let Some(info) = self.incoming_call.take() {
                self.peer_ip = info.ip;
                self.peer_port = info.port;
                self.active_tab = SidebarTab::Call;
                self.start_call();
            }
        }
        if reject {
            self.incoming_call = None;
        }

        ctx.request_repaint_after(std::time::Duration::from_millis(200));
    }
}

pub fn run() {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([884.0, 750.0])
            .with_min_inner_size([484.0, 600.0])
            .with_title("hostelD — Secure P2P Voice + Chat + Screen"),
        ..Default::default()
    };
    eframe::run_native(
        "hostelD",
        options,
        Box::new(|cc| Ok(Box::new(HostelApp::new(cc)))),
    ).expect("Failed to start GUI");
}
