use cpal::traits::{DeviceTrait, HostTrait};
use eframe::egui;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::Instant;

use crate::chat::ChatHistory;
use crate::identity::{self, Contact, Identity, Settings};
use crate::screen::{ScreenCommand, ScreenQuality, ScreenViewer};

// ── App State Machine ──

enum Screen {
    Setup,
    Connecting,
    KeyWarning,
    InCall,
    Error(String),
}

struct DeviceList {
    input_names: Vec<String>,
    output_names: Vec<String>,
}

fn list_audio_devices() -> DeviceList {
    let host = cpal::default_host();
    let input_names: Vec<String> = host.input_devices()
        .map(|devs| devs.map(|d| d.name().unwrap_or_else(|_| "unknown".into())).collect())
        .unwrap_or_default();
    let output_names: Vec<String> = host.output_devices()
        .map(|devs| devs.map(|d| d.name().unwrap_or_else(|_| "unknown".into())).collect())
        .unwrap_or_default();
    DeviceList { input_names, output_names }
}

fn get_ipv6_addresses() -> Vec<(String, String)> {
    let mut addrs = vec![("::1".to_string(), "::1 (loopback)".to_string())];

    #[cfg(target_os = "linux")]
    {
        if let Ok(output) = std::process::Command::new("ip").args(["-6", "addr", "show"]).output() {
            let text = String::from_utf8_lossy(&output.stdout);
            for line in text.lines() {
                let trimmed = line.trim();
                if trimmed.starts_with("inet6") {
                    if let Some(addr_cidr) = trimmed.split_whitespace().nth(1) {
                        let addr = addr_cidr.split('/').next().unwrap_or(addr_cidr);
                        if addr != "::1" {
                            let scope = if trimmed.contains("scope global") { "global" }
                                else if trimmed.contains("scope link") { "link-local" }
                                else { "other" };
                            addrs.push((addr.to_string(), format!("{addr} ({scope})")));
                        }
                    }
                }
            }
        }
    }

    #[cfg(target_os = "windows")]
    {
        // Windows: parse "netsh interface ipv6 show addresses" or fall back to "ipconfig"
        if let Ok(output) = std::process::Command::new("powershell")
            .args(["-Command", "(Get-NetIPAddress -AddressFamily IPv6).IPAddress"])
            .output()
        {
            let text = String::from_utf8_lossy(&output.stdout);
            for line in text.lines() {
                let addr = line.trim().to_string();
                if addr.is_empty() || addr == "::1" { continue; }
                let scope = if addr.starts_with("fe80") { "link-local" }
                    else if addr.starts_with("::1") { continue }
                    else { "global" };
                addrs.push((addr.clone(), format!("{addr} ({scope})")));
            }
        }
    }

    // Sort: global first, then link-local, then other
    if addrs.len() > 1 {
        addrs[1..].sort_by_key(|(_, label)| {
            if label.contains("global") { 0 }
            else if label.contains("link-local") { 1 }
            else { 2 }
        });
    }
    addrs
}

/// Format a contact/peer for display: "nickname #fingerprint" or just fingerprint.
fn format_peer_display(nickname: &str, fingerprint: &str) -> String {
    if nickname.is_empty() {
        fingerprint.to_string()
    } else {
        format!("{nickname} #{fingerprint}")
    }
}

// ── Connection result sent from background thread ──

struct CallInfo {
    verification_code: String,
    peer_fingerprint: String,
    peer_nickname: String,
    contact_id: String,
    key_change_warning: Option<String>,
    pending_contact: Option<Contact>,
    chat_tx: mpsc::Sender<String>,
    chat_rx: mpsc::Receiver<String>,
    local_hangup: Arc<AtomicBool>,
    screen_viewer: Arc<Mutex<ScreenViewer>>,
    screen_active: Arc<AtomicBool>,
    /// Channel to tell the engine thread to start/stop screen sharing
    screen_cmd_tx: mpsc::Sender<ScreenCommand>,
}

pub struct HostelApp {
    screen: Screen,
    identity: Identity,
    settings: Settings,

    // Setup
    network_mode: usize,
    selected_input: usize,
    selected_output: usize,
    selected_addr: usize,
    local_port: String,
    peer_ip: String,
    peer_port: String,
    devices: DeviceList,
    ipv6_addrs: Vec<(String, String)>,

    // Contact list
    contacts: Vec<Contact>,
    contact_search: String,

    // Call state
    running: Arc<AtomicBool>,
    mic_active: Arc<AtomicBool>,
    local_hangup: Option<Arc<AtomicBool>>,
    verification_code: String,
    peer_fingerprint: String,
    peer_nickname: String,
    contact_id: String,
    key_change_warning: Option<String>,
    pending_contact: Option<Contact>,

    // Chat
    chat_tx: Option<mpsc::Sender<String>>,
    chat_rx: Option<mpsc::Receiver<String>>,
    chat_input: String,
    chat_history: Option<ChatHistory>,

    // Screen sharing
    screen_sharing: bool,
    screen_texture: Option<egui::TextureHandle>,
    screen_viewer: Option<Arc<Mutex<ScreenViewer>>>,
    screen_active: Option<Arc<AtomicBool>>,
    screen_cmd_tx: Option<mpsc::Sender<ScreenCommand>>,
    selected_screen_quality: usize,
    show_screen_popup: bool,
    selected_audio_device: usize, // 0=None, 1=Default, 2+=specific loopback device
    loopback_devices: Vec<String>,
    video_fullscreen: bool,
    last_mouse_move: Instant,
    last_frame_time: Option<Instant>,
    is_fullscreen: bool,

    // Async connection result
    connect_result: Arc<std::sync::Mutex<Option<Result<CallInfo, String>>>>,
}

impl HostelApp {
    pub fn new(_cc: &eframe::CreationContext<'_>) -> Self {
        let devices = list_audio_devices();
        let ipv6_addrs = get_ipv6_addresses();
        let identity = Identity::load_or_create();
        let settings = Settings::load();
        let contacts = identity::load_all_contacts();

        // Match saved device names to indices
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

        Self {
            screen: Screen::Setup,
            identity,
            settings,
            network_mode: 0,
            selected_input,
            selected_output,
            selected_addr: ipv6_addrs.iter().position(|(ip, _)| ip != "::1").unwrap_or(0),
            local_port,
            peer_ip: "::1".to_string(),
            peer_port: "9000".to_string(),
            devices,
            ipv6_addrs,
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
            selected_audio_device: 0,
            loopback_devices: Vec::new(),
            video_fullscreen: false,
            last_mouse_move: Instant::now(),
            last_frame_time: None,
            is_fullscreen: false,
            connect_result: Arc::new(std::sync::Mutex::new(None)),
        }
    }

    fn start_call(&mut self) {
        // Save settings before starting the call
        self.settings.mic = self.devices.input_names.get(self.selected_input)
            .cloned().unwrap_or_default();
        self.settings.speakers = self.devices.output_names.get(self.selected_output)
            .cloned().unwrap_or_default();
        self.settings.local_port = self.local_port.clone();
        self.settings.save();

        self.screen = Screen::Connecting;
        self.running.store(true, Ordering::Relaxed);
        self.mic_active.store(true, Ordering::Relaxed);

        let peer_addr = format!("[{}]:{}", self.peer_ip, self.peer_port);
        let local_port = self.local_port.clone();
        let running = self.running.clone();
        let mic_active = self.mic_active.clone();
        let result = self.connect_result.clone();
        let input_idx = self.selected_input;
        let output_idx = self.selected_output;

        // Pass identity info to the thread
        let identity_secret = self.identity.secret;
        let identity_pubkey = self.identity.pubkey;
        let identity_fingerprint = self.identity.fingerprint.clone();
        let our_nickname = self.settings.nickname.clone();

        thread::spawn(move || {
            // Wrap everything in catch_unwind so panics show in GUI instead of silently dying
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

                // Channel for GUI to signal start/stop screen sharing
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
                        };
                        *result.lock().unwrap() = Some(Ok(info));

                        // Keep engine alive until call ends, also process screen commands
                        while running.load(Ordering::Relaxed) {
                            while let Ok(cmd) = screen_cmd_rx.try_recv() {
                                match cmd {
                                    ScreenCommand::Start { quality, audio_device } => engine.start_screen_share(quality, audio_device),
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

            // If the thread panicked, show the panic message in the GUI
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

    /// User clicked Hang Up — signal peer then clean up.
    fn hang_up(&mut self) {
        // Signal local hangup so sender thread sends PKT_HANGUP to peer
        if let Some(ref lh) = self.local_hangup {
            lh.store(true, Ordering::Relaxed);
        }
        self.running.store(false, Ordering::Relaxed);
        self.cleanup_call();
    }

    /// Remote peer hung up (or connection lost) — clean up without sending PKT_HANGUP.
    fn on_remote_hangup(&mut self) {
        // Don't set local_hangup — peer already knows the call is over
        self.running.store(false, Ordering::Relaxed);
        self.cleanup_call();
    }

    /// Shared cleanup for both local and remote hangup.
    fn cleanup_call(&mut self) {
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
        self.selected_audio_device = 0;
        self.loopback_devices.clear();
        self.screen_texture = None;
        self.screen_viewer = None;
        self.screen_active = None;
        self.screen_cmd_tx = None;
        self.video_fullscreen = false;
        self.last_frame_time = None;
        self.is_fullscreen = false;
        *self.connect_result.lock().unwrap() = None;
        // Reload contacts (the call may have created/updated one)
        self.contacts = identity::load_all_contacts();
        self.screen = Screen::Setup;
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
                        // Load chat history
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
            // Detect remote hangup: running was set to false by receiver thread
            if !self.running.load(Ordering::Relaxed) {
                self.on_remote_hangup();
                ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(false));
                return;
            }
            // ESC exits video fullscreen (or app fullscreen)
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

            // Frozen frame detection: if no new frame for >2s, clear texture
            if let Some(t) = self.last_frame_time {
                if t.elapsed().as_secs_f32() > 2.0 {
                    self.screen_texture = None;
                    self.last_frame_time = None;
                }
            }

            // Track mouse movement for fullscreen overlay auto-hide
            let mouse_delta = ctx.input(|i| i.pointer.delta());
            if mouse_delta != egui::Vec2::ZERO {
                self.last_mouse_move = Instant::now();
            }

            // Repaint faster when screen sharing is active (for smooth video)
            let repaint_ms = if self.screen_texture.is_some() { 33 } else { 200 };
            ctx.request_repaint_after(std::time::Duration::from_millis(repaint_ms));
        }

        // Style
        let mut style = (*ctx.style()).clone();
        style.spacing.item_spacing = egui::vec2(8.0, 6.0);
        style.spacing.button_padding = egui::vec2(14.0, 6.0);
        ctx.set_style(style);

        // Video-only fullscreen: skip all UI except video + overlay
        if self.video_fullscreen && matches!(self.screen, Screen::InCall) {
            egui::CentralPanel::default().show(ctx, |ui| {
                self.draw_fullscreen_video(ui);
            });
            return;
        }

        // Side panel for chat when video is active during a call
        let video_active = matches!(self.screen, Screen::InCall) && self.screen_texture.is_some();
        if video_active {
            egui::SidePanel::right("chat_panel")
                .default_width(280.0)
                .resizable(true)
                .min_width(160.0)
                .max_width(400.0)
                .show(ctx, |ui| {
                    self.draw_chat(ui);
                });
        }

        egui::CentralPanel::default().show(ctx, |ui| {
            match &self.screen {
                Screen::Setup => self.draw_setup(ui),
                Screen::Connecting => self.draw_connecting(ui, ctx),
                Screen::KeyWarning => self.draw_key_warning(ui),
                Screen::InCall => self.draw_call(ui),
                Screen::Error(_) => {
                    let msg = if let Screen::Error(m) = &self.screen { m.clone() } else { unreachable!() };
                    self.draw_error(ui, &msg);
                }
            }
        });
    }
}

impl HostelApp {
    fn draw_key_warning(&mut self, ui: &mut egui::Ui) {
        let peer_display = format_peer_display(&self.peer_nickname, &self.peer_fingerprint);
        let warning_text = self.key_change_warning.clone().unwrap_or_default();

        ui.vertical_centered(|ui| {
            ui.add_space(30.0);
            ui.colored_label(
                egui::Color32::from_rgb(255, 60, 60),
                egui::RichText::new("SECURITY WARNING").size(28.0).strong(),
            );
            ui.add_space(15.0);
        });

        ui.add_space(5.0);
        ui.colored_label(
            egui::Color32::from_rgb(255, 100, 100),
            egui::RichText::new(&warning_text).size(15.0).strong(),
        );

        ui.add_space(15.0);
        ui.separator();
        ui.add_space(10.0);

        ui.horizontal(|ui| {
            ui.label("Peer:");
            ui.strong(&peer_display);
        });
        ui.horizontal(|ui| {
            ui.label("Verify code:");
            ui.colored_label(
                egui::Color32::from_rgb(255, 200, 50),
                egui::RichText::new(&self.verification_code).size(18.0).strong(),
            );
        });

        ui.add_space(15.0);
        ui.label("Possible reasons:");
        ui.label("  - The peer reinstalled the app or changed devices");
        ui.label("  - Someone is impersonating the peer (MITM attack)");
        ui.add_space(5.0);
        ui.colored_label(
            egui::Color32::from_rgb(255, 200, 100),
            "Verify the code above with your peer through a trusted channel.",
        );

        ui.add_space(25.0);
        ui.vertical_centered(|ui| {
            ui.horizontal(|ui| {
                let proceed_btn = egui::Button::new(
                    egui::RichText::new("Proceed (Trust)").size(18.0).color(egui::Color32::WHITE),
                )
                .min_size(egui::vec2(180.0, 44.0))
                .fill(egui::Color32::from_rgb(40, 140, 60));

                if ui.add(proceed_btn).clicked() {
                    if let Some(contact) = self.pending_contact.take() {
                        identity::save_contact(&contact);
                    }
                    self.key_change_warning = None;
                    self.screen = Screen::InCall;
                }

                ui.add_space(20.0);

                let reject_btn = egui::Button::new(
                    egui::RichText::new("Reject (Hang Up)").size(18.0).color(egui::Color32::WHITE),
                )
                .min_size(egui::vec2(180.0, 44.0))
                .fill(egui::Color32::from_rgb(180, 40, 40));

                if ui.add(reject_btn).clicked() {
                    self.pending_contact = None;
                    self.hang_up();
                }
            });
        });
    }

    fn draw_setup(&mut self, ui: &mut egui::Ui) {
        ui.vertical_centered(|ui| {
            ui.add_space(8.0);
            ui.heading("hostelD");
            ui.label(format!("Your ID: {}", self.identity.fingerprint));
            ui.add_space(6.0);
        });

        ui.separator();

        // Nickname
        ui.horizontal(|ui| {
            ui.label("Nickname:");
            let resp = ui.add(
                egui::TextEdit::singleline(&mut self.settings.nickname)
                    .desired_width(200.0)
                    .hint_text("optional")
            );
            if resp.changed() {
                self.settings.save();
            }
        });

        // Network mode
        ui.label("Network mode:");
        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.network_mode, 0, "LAN");
            ui.selectable_value(&mut self.network_mode, 1, "Internet");
        });

        // IPv6
        let filtered_addrs: Vec<(usize, &(String, String))> = self.ipv6_addrs.iter()
            .enumerate()
            .filter(|(_, (ip, _))| ip != "::1")
            .collect();

        if !filtered_addrs.is_empty() {
            // Auto-select first global address, fallback to first available
            if !filtered_addrs.iter().any(|(i, _)| *i == self.selected_addr) {
                self.selected_addr = filtered_addrs.iter()
                    .find(|(_, (_, label))| label.contains("global"))
                    .unwrap_or(&filtered_addrs[0]).0;
            }

            ui.horizontal(|ui| {
                ui.label("Your IPv6:");
                let selected_ip = filtered_addrs.iter()
                    .find(|(i, _)| *i == self.selected_addr)
                    .map(|(_, (ip, _))| ip.clone())
                    .unwrap_or_else(|| filtered_addrs[0].1.0.clone());
                if ui.small_button("Copy").clicked() {
                    ui.ctx().copy_text(selected_ip);
                }
            });
            let selected_label = filtered_addrs.iter()
                .find(|(i, _)| *i == self.selected_addr)
                .map(|(_, (_, l))| l.clone())
                .unwrap_or_else(|| filtered_addrs[0].1.1.clone());
            egui::ComboBox::from_id_salt("ipv6")
                .width(300.0).selected_text(selected_label)
                .show_ui(ui, |ui| {
                    for (idx, (_, label)) in &filtered_addrs {
                        ui.selectable_value(&mut self.selected_addr, *idx, label.as_str());
                    }
                });
        }

        // Audio devices
        ui.label("Microphone:");
        egui::ComboBox::from_id_salt("mic").width(300.0)
            .selected_text(self.devices.input_names.get(self.selected_input).map(|s| s.as_str()).unwrap_or("none"))
            .show_ui(ui, |ui| {
                for (i, name) in self.devices.input_names.iter().enumerate() {
                    ui.selectable_value(&mut self.selected_input, i, name.as_str());
                }
            });

        ui.label("Speakers:");
        egui::ComboBox::from_id_salt("spk").width(300.0)
            .selected_text(self.devices.output_names.get(self.selected_output).map(|s| s.as_str()).unwrap_or("none"))
            .show_ui(ui, |ui| {
                for (i, name) in self.devices.output_names.iter().enumerate() {
                    ui.selectable_value(&mut self.selected_output, i, name.as_str());
                }
            });

        ui.separator();

        ui.label("Your port:");
        ui.add(egui::TextEdit::singleline(&mut self.local_port).desired_width(120.0));
        ui.label("Peer IPv6:");
        ui.add(egui::TextEdit::singleline(&mut self.peer_ip).desired_width(300.0));
        ui.label("Peer port:");
        ui.add(egui::TextEdit::singleline(&mut self.peer_port).desired_width(120.0));

        if self.network_mode == 1 {
            ui.colored_label(egui::Color32::YELLOW, "Internet: make sure port is open in firewall");
        }

        ui.add_space(10.0);
        ui.vertical_centered(|ui| {
            let btn = egui::Button::new(egui::RichText::new("Call").size(20.0))
                .min_size(egui::vec2(200.0, 42.0))
                .fill(egui::Color32::from_rgb(40, 140, 60));
            if ui.add(btn).clicked() { self.start_call(); }
        });

        // ── Contact List ──
        if !self.contacts.is_empty() {
            ui.add_space(8.0);
            ui.separator();
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Contacts").strong());
                ui.add(
                    egui::TextEdit::singleline(&mut self.contact_search)
                        .hint_text("Search...")
                        .desired_width(150.0)
                );
            });

            let search = self.contact_search.to_lowercase();
            let max_height = ui.available_height().max(80.0);
            egui::ScrollArea::vertical()
                .max_height(max_height)
                .id_salt("contacts_scroll")
                .show(ui, |ui| {
                    for contact in &self.contacts {
                        if !search.is_empty()
                            && !contact.nickname.to_lowercase().contains(&search)
                            && !contact.fingerprint.to_lowercase().contains(&search)
                        {
                            continue;
                        }
                        let display = format_peer_display(&contact.nickname, &contact.fingerprint);
                        let addr_info = if !contact.last_address.is_empty() {
                            format!("  [{}]:{}", contact.last_address, contact.last_port)
                        } else {
                            String::new()
                        };
                        let label = format!("{display}{addr_info}");

                        if ui.add(
                            egui::Button::new(&label)
                                .min_size(egui::vec2(ui.available_width(), 0.0))
                                .frame(false)
                        ).clicked() {
                            if !contact.last_address.is_empty() {
                                self.peer_ip = contact.last_address.clone();
                            }
                            if !contact.last_port.is_empty() {
                                self.peer_port = contact.last_port.clone();
                            }
                        }
                    }
                });
        }
    }

    fn draw_connecting(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        ui.vertical_centered(|ui| {
            ui.add_space(60.0);
            ui.heading("Connecting...");
            ui.add_space(15.0);
            ui.spinner();
            ui.add_space(15.0);
            ui.label("Key exchange + identity verification");
            ui.label(format!("Peer: [{}]:{}", self.peer_ip, self.peer_port));
            ui.add_space(20.0);
            let btn = egui::Button::new(egui::RichText::new("Cancel").size(16.0))
                .min_size(egui::vec2(120.0, 34.0))
                .fill(egui::Color32::from_rgb(160, 50, 50));
            if ui.add(btn).clicked() {
                self.running.store(false, Ordering::Relaxed);
                self.cleanup_call();
            }
        });
        ctx.request_repaint_after(std::time::Duration::from_millis(200));
    }

    fn draw_call(&mut self, ui: &mut egui::Ui) {
        let mic_on = self.mic_active.load(Ordering::Relaxed);
        let peer_display = format_peer_display(&self.peer_nickname, &self.peer_fingerprint);
        let has_video = self.screen_texture.is_some();

        // ── Top bar: status ──
        ui.horizontal(|ui| {
            ui.heading("hostelD");
            ui.separator();
            ui.colored_label(egui::Color32::from_rgb(80, 200, 80), "ENCRYPTED");
            ui.separator();
            ui.label(format!("Peer: {peer_display}"));
        });

        ui.separator();

        // ── Info row ──
        ui.horizontal(|ui| {
            ui.label("Verify:");
            ui.colored_label(
                egui::Color32::from_rgb(255, 200, 50),
                egui::RichText::new(&self.verification_code).size(18.0).strong(),
            );
            ui.separator();
            ui.label("Opus 64kbps");
            ui.separator();
            ui.label(if self.network_mode == 0 { "LAN" } else { "Internet" });
        });

        ui.add_space(4.0);

        // ── Controls row: Mic + Screen + Hang Up ... (right) Fullscreen ──
        ui.horizontal(|ui| {
            // Mic toggle
            let (btn_text, btn_color) = if mic_on {
                ("Mic: ON", egui::Color32::from_rgb(40, 140, 60))
            } else {
                ("Mic: MUTED", egui::Color32::from_rgb(180, 40, 40))
            };
            let mic_btn = egui::Button::new(
                egui::RichText::new(btn_text).size(16.0).color(egui::Color32::WHITE)
            ).min_size(egui::vec2(120.0, 35.0)).fill(btn_color);
            if ui.add(mic_btn).clicked() {
                self.mic_active.store(!mic_on, Ordering::Relaxed);
            }

            // Screen share toggle: OFF → open popup, ON → stop sharing
            let (scr_text, scr_color) = if self.screen_sharing {
                ("Screen: ON", egui::Color32::from_rgb(40, 100, 180))
            } else {
                ("Screen: OFF", egui::Color32::from_rgb(100, 100, 100))
            };
            let scr_btn = egui::Button::new(
                egui::RichText::new(scr_text).size(16.0).color(egui::Color32::WHITE)
            ).min_size(egui::vec2(130.0, 35.0)).fill(scr_color);
            if ui.add(scr_btn).clicked() {
                if self.screen_sharing {
                    // Stop sharing
                    self.screen_sharing = false;
                    if let Some(tx) = &self.screen_cmd_tx {
                        let _ = tx.send(ScreenCommand::Stop);
                    }
                } else {
                    // Open popup to configure before sharing
                    if self.loopback_devices.is_empty() {
                        self.loopback_devices = crate::sysaudio::list_loopback_devices();
                    }
                    self.show_screen_popup = true;
                }
            }

            // Hang Up
            let hangup_btn = egui::Button::new(
                egui::RichText::new("Hang Up").size(16.0).color(egui::Color32::WHITE)
            ).min_size(egui::vec2(100.0, 35.0)).fill(egui::Color32::from_rgb(180, 40, 40));
            if ui.add(hangup_btn).clicked() {
                if self.is_fullscreen || self.video_fullscreen {
                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::Fullscreen(false));
                    self.is_fullscreen = false;
                    self.video_fullscreen = false;
                }
                self.hang_up();
                return;
            }

            // Right-aligned fullscreen button (only when receiving video)
            if has_video {
                // Push to the right
                let remaining = ui.available_width() - 120.0;
                if remaining > 0.0 {
                    ui.add_space(remaining);
                }
                let fs_btn = egui::Button::new(
                    egui::RichText::new("Fullscreen").size(14.0).color(egui::Color32::WHITE)
                ).min_size(egui::vec2(110.0, 35.0)).fill(egui::Color32::from_rgb(80, 80, 120));
                if ui.add(fs_btn).clicked() {
                    self.video_fullscreen = true;
                    self.is_fullscreen = true;
                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::Fullscreen(true));
                }
            }
        });

        // ── Screen share config popup ──
        if self.show_screen_popup {
            let mut open = true;
            egui::Window::new("Screen Share")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .open(&mut open)
                .show(ui.ctx(), |ui| {
                    ui.label("Quality:");
                    let current_label = ScreenQuality::ALL[self.selected_screen_quality].label();
                    egui::ComboBox::from_id_salt("popup_quality")
                        .width(160.0)
                        .selected_text(current_label)
                        .show_ui(ui, |ui| {
                            for (i, q) in ScreenQuality::ALL.iter().enumerate() {
                                ui.selectable_value(&mut self.selected_screen_quality, i, q.label());
                            }
                        });

                    ui.add_space(4.0);
                    ui.label("System Audio:");
                    let audio_label = match self.selected_audio_device {
                        0 => "None".to_string(),
                        1 => "Default".to_string(),
                        n => self.loopback_devices.get(n - 2)
                            .cloned().unwrap_or_else(|| "Unknown".to_string()),
                    };
                    egui::ComboBox::from_id_salt("popup_audio")
                        .width(240.0)
                        .selected_text(&audio_label)
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut self.selected_audio_device, 0, "None");
                            ui.selectable_value(&mut self.selected_audio_device, 1, "Default");
                            for (i, name) in self.loopback_devices.iter().enumerate() {
                                ui.selectable_value(&mut self.selected_audio_device, i + 2, name.as_str());
                            }
                        });

                    ui.add_space(8.0);
                    let share_btn = egui::Button::new(
                        egui::RichText::new("Share Screen").size(16.0).color(egui::Color32::WHITE)
                    ).min_size(egui::vec2(160.0, 35.0)).fill(egui::Color32::from_rgb(40, 100, 180));
                    if ui.add(share_btn).clicked() {
                        let quality = ScreenQuality::ALL[self.selected_screen_quality];
                        let audio_device = match self.selected_audio_device {
                            0 => None,
                            1 => Some(String::new()), // empty = default
                            n => self.loopback_devices.get(n - 2).cloned(),
                        };
                        if let Some(tx) = &self.screen_cmd_tx {
                            let _ = tx.send(ScreenCommand::Start { quality, audio_device });
                        }
                        self.screen_sharing = true;
                        self.show_screen_popup = false;
                    }
                });
            if !open {
                self.show_screen_popup = false;
            }
        }

        // ── Screen viewer (incoming screen from peer) ──
        let mut video_w: u32 = 1280;
        let mut video_h: u32 = 720;
        if let Some(viewer) = &self.screen_viewer {
            if let Ok(mut v) = viewer.lock() {
                video_w = v.frame_width;
                video_h = v.frame_height;
                if let Some(rgba_frame) = v.take_frame() {
                    self.last_frame_time = Some(Instant::now());
                    let image = egui::ColorImage::from_rgba_unmultiplied(
                        [video_w as usize, video_h as usize],
                        &rgba_frame,
                    );
                    self.screen_texture = Some(
                        ui.ctx().load_texture("screen_share", image, Default::default())
                    );
                }
            }
        }
        if let Some(tex) = &self.screen_texture {
            ui.separator();
            let available_width = ui.available_width();
            let available_height = if has_video {
                ui.available_height()
            } else {
                ui.available_height() * 0.6
            };
            let aspect = video_w as f32 / video_h as f32;
            let (display_w, display_h) = {
                let w_from_width = available_width;
                let h_from_width = available_width / aspect;
                let h_from_height = available_height;
                let w_from_height = available_height * aspect;
                if h_from_width <= available_height {
                    (w_from_width, h_from_width)
                } else {
                    (w_from_height, h_from_height)
                }
            };
            let pad_x = (available_width - display_w).max(0.0) / 2.0;
            ui.horizontal(|ui| {
                ui.add_space(pad_x);
                ui.image(egui::load::SizedTexture::new(
                    tex.id(),
                    egui::vec2(display_w, display_h),
                ));
            });
        }

        // ── Chat area (only when no video — otherwise chat is in the side panel) ──
        if !has_video {
            ui.separator();
            self.draw_chat(ui);
        }
    }

    /// Render video-only fullscreen with auto-hiding overlay.
    fn draw_fullscreen_video(&mut self, ui: &mut egui::Ui) {
        // Update video from screen viewer
        let mut video_w: u32 = 1280;
        let mut video_h: u32 = 720;
        if let Some(viewer) = &self.screen_viewer {
            if let Ok(mut v) = viewer.lock() {
                video_w = v.frame_width;
                video_h = v.frame_height;
                if let Some(rgba_frame) = v.take_frame() {
                    self.last_frame_time = Some(Instant::now());
                    let image = egui::ColorImage::from_rgba_unmultiplied(
                        [video_w as usize, video_h as usize],
                        &rgba_frame,
                    );
                    self.screen_texture = Some(
                        ui.ctx().load_texture("screen_share", image, Default::default())
                    );
                }
            }
        }

        // Render video filling entire area
        if let Some(tex) = &self.screen_texture {
            let available_width = ui.available_width();
            let available_height = ui.available_height();
            let aspect = video_w as f32 / video_h as f32;
            let (display_w, display_h) = {
                let w_from_width = available_width;
                let h_from_width = available_width / aspect;
                let h_from_height = available_height;
                let w_from_height = available_height * aspect;
                if h_from_width <= available_height {
                    (w_from_width, h_from_width)
                } else {
                    (w_from_height, h_from_height)
                }
            };
            let pad_x = (available_width - display_w).max(0.0) / 2.0;
            let pad_y = (available_height - display_h).max(0.0) / 2.0;
            ui.add_space(pad_y);
            ui.horizontal(|ui| {
                ui.add_space(pad_x);
                ui.image(egui::load::SizedTexture::new(
                    tex.id(),
                    egui::vec2(display_w, display_h),
                ));
            });
        } else {
            // No video — exit fullscreen
            self.video_fullscreen = false;
            self.is_fullscreen = false;
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Fullscreen(false));
            return;
        }

        // Semi-transparent overlay when mouse moved within last 3 seconds
        let show_overlay = self.last_mouse_move.elapsed().as_secs_f32() < 3.0;
        if show_overlay {
            egui::Area::new(egui::Id::new("fs_overlay"))
                .fixed_pos(egui::pos2(0.0, 0.0))
                .order(egui::Order::Foreground)
                .show(ui.ctx(), |ui| {
                    let screen_width = ui.ctx().screen_rect().width();
                    let frame = egui::Frame::none()
                        .fill(egui::Color32::from_rgba_premultiplied(0, 0, 0, 160))
                        .inner_margin(egui::Margin::same(8.0));
                    frame.show(ui, |ui: &mut egui::Ui| {
                        ui.set_min_width(screen_width);
                        ui.horizontal(|ui: &mut egui::Ui| {
                            ui.colored_label(
                                egui::Color32::WHITE,
                                egui::RichText::new("hostelD").size(16.0),
                            );
                            ui.separator();
                            let exit_btn = egui::Button::new(
                                egui::RichText::new("Exit Fullscreen").size(14.0).color(egui::Color32::WHITE)
                            ).fill(egui::Color32::from_rgb(80, 80, 120));
                            if ui.add(exit_btn).clicked() {
                                self.video_fullscreen = false;
                                self.is_fullscreen = false;
                                ui.ctx().send_viewport_cmd(egui::ViewportCommand::Fullscreen(false));
                            }
                        });
                    });
                });
        }
    }

    /// Render chat messages + input. Used both in the main panel and the side panel.
    fn draw_chat(&mut self, ui: &mut egui::Ui) {
        ui.label(egui::RichText::new("Chat").strong());

        // Message history (scrollable)
        let available_height = ui.available_height() - 35.0; // leave room for input
        let peer_label = if self.peer_nickname.is_empty() {
            "Peer:".to_string()
        } else {
            format!("{}:", self.peer_nickname)
        };
        egui::ScrollArea::vertical()
            .max_height(available_height)
            .stick_to_bottom(true)
            .id_salt("chat_scroll")
            .show(ui, |ui| {
                if let Some(history) = &self.chat_history {
                    if history.messages.is_empty() {
                        ui.colored_label(egui::Color32::GRAY, "No messages yet. Type below.");
                    }
                    for msg in &history.messages {
                        let time = ChatHistory::format_time(msg.timestamp);
                        if msg.from_me {
                            ui.horizontal_wrapped(|ui| {
                                ui.colored_label(egui::Color32::GRAY, &time);
                                ui.colored_label(egui::Color32::from_rgb(100, 180, 255), "You:");
                                ui.label(&msg.text);
                            });
                        } else {
                            ui.horizontal_wrapped(|ui| {
                                ui.colored_label(egui::Color32::GRAY, &time);
                                ui.colored_label(egui::Color32::from_rgb(180, 255, 100), &peer_label);
                                ui.label(&msg.text);
                            });
                        }
                    }
                }
            });

        // ── Chat input ──
        ui.horizontal(|ui| {
            let response = ui.add(
                egui::TextEdit::singleline(&mut self.chat_input)
                    .desired_width(ui.available_width() - 70.0)
                    .hint_text("Type a message...")
            );

            let send_clicked = ui.add(
                egui::Button::new("Send").min_size(egui::vec2(55.0, 30.0))
            ).clicked();

            let enter_pressed = response.lost_focus()
                && ui.input(|i| i.key_pressed(egui::Key::Enter));

            if (send_clicked || enter_pressed) && !self.chat_input.trim().is_empty() {
                let text = self.chat_input.trim().to_string();
                // Send to network
                if let Some(tx) = &self.chat_tx {
                    let _ = tx.send(text.clone());
                }
                // Save to local history
                if let Some(history) = &mut self.chat_history {
                    history.add_message(true, text);
                }
                self.chat_input.clear();
                response.request_focus();
            }
        });
    }

    fn draw_error(&mut self, ui: &mut egui::Ui, message: &str) {
        ui.vertical_centered(|ui| {
            ui.add_space(60.0);
            ui.colored_label(egui::Color32::from_rgb(220, 60, 60),
                egui::RichText::new("Connection Failed").size(24.0));
            ui.add_space(15.0);
            ui.label(message);
            ui.add_space(25.0);
            if ui.add(egui::Button::new("Back").min_size(egui::vec2(140.0, 40.0))).clicked() {
                self.screen = Screen::Setup;
            }
        });
    }
}

pub fn run() {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([800.0, 750.0])
            .with_min_inner_size([400.0, 600.0])
            .with_title("hostelD — Secure P2P Voice + Chat + Screen"),
        ..Default::default()
    };
    eframe::run_native(
        "hostelD",
        options,
        Box::new(|cc| Ok(Box::new(HostelApp::new(cc)))),
    ).expect("Failed to start GUI");
}
