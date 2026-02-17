use cpal::traits::{DeviceTrait, HostTrait};
use eframe::egui;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::thread;

use crate::chat::ChatHistory;
use crate::identity::Identity;

// ── App State Machine ──

enum Screen {
    Setup,
    Connecting,
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
    addrs
}

// ── Connection result sent from background thread ──

struct CallInfo {
    verification_code: String,
    peer_fingerprint: String,
    contact_id: String,
    chat_tx: mpsc::Sender<String>,
    chat_rx: mpsc::Receiver<String>,
}

pub struct HostelApp {
    screen: Screen,
    identity: Identity,

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

    // Call state
    running: Arc<AtomicBool>,
    mic_active: Arc<AtomicBool>,
    verification_code: String,
    peer_fingerprint: String,
    contact_id: String,

    // Chat
    chat_tx: Option<mpsc::Sender<String>>,
    chat_rx: Option<mpsc::Receiver<String>>,
    chat_input: String,
    chat_history: Option<ChatHistory>,

    // Async connection result
    connect_result: Arc<std::sync::Mutex<Option<Result<CallInfo, String>>>>,
}

impl HostelApp {
    pub fn new(_cc: &eframe::CreationContext<'_>) -> Self {
        let devices = list_audio_devices();
        let ipv6_addrs = get_ipv6_addresses();
        let identity = Identity::load_or_create();

        Self {
            screen: Screen::Setup,
            identity,
            network_mode: 0,
            selected_input: 0,
            selected_output: 0,
            selected_addr: 0,
            local_port: "9000".to_string(),
            peer_ip: "::1".to_string(),
            peer_port: "9000".to_string(),
            devices,
            ipv6_addrs,
            running: Arc::new(AtomicBool::new(false)),
            mic_active: Arc::new(AtomicBool::new(true)),
            verification_code: String::new(),
            peer_fingerprint: String::new(),
            contact_id: String::new(),
            chat_tx: None,
            chat_rx: None,
            chat_input: String::new(),
            chat_history: None,
            connect_result: Arc::new(std::sync::Mutex::new(None)),
        }
    }

    fn start_call(&mut self) {
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

        thread::spawn(move || {
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

            match crate::voice::start_engine(
                &input_device, &output_device,
                &peer_addr, &local_port,
                running.clone(), mic_active, &identity,
            ) {
                Ok(mut engine) => {
                    let info = CallInfo {
                        verification_code: engine.verification_code.clone(),
                        peer_fingerprint: engine.peer_fingerprint.clone(),
                        contact_id: engine.contact_id.clone(),
                        chat_tx: engine.chat_tx.take().unwrap(),
                        chat_rx: engine.chat_rx.take().unwrap(),
                    };
                    *result.lock().unwrap() = Some(Ok(info));

                    // Keep engine alive until call ends
                    while running.load(Ordering::Relaxed) {
                        thread::sleep(std::time::Duration::from_millis(100));
                    }
                    drop(engine);
                }
                Err(e) => {
                    *result.lock().unwrap() = Some(Err(e));
                }
            }
        });
    }

    fn hang_up(&mut self) {
        self.running.store(false, Ordering::Relaxed);
        self.verification_code.clear();
        self.peer_fingerprint.clear();
        self.contact_id.clear();
        self.chat_tx = None;
        self.chat_rx = None;
        self.chat_input.clear();
        self.chat_history = None;
        *self.connect_result.lock().unwrap() = None;
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
                        self.contact_id = info.contact_id.clone();
                        self.chat_tx = Some(info.chat_tx);
                        self.chat_rx = Some(info.chat_rx);
                        // Load chat history
                        self.chat_history = Some(ChatHistory::load(
                            &info.contact_id,
                            &self.identity.secret,
                        ));
                        self.screen = Screen::InCall;
                    }
                    Err(e) => {
                        self.running.store(false, Ordering::Relaxed);
                        self.screen = Screen::Error(e);
                    }
                }
            }
            drop(lock);
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }

        // Poll incoming chat messages
        if matches!(self.screen, Screen::InCall) {
            if let Some(rx) = &self.chat_rx {
                while let Ok(text) = rx.try_recv() {
                    if let Some(history) = &mut self.chat_history {
                        history.add_message(false, text);
                    }
                }
            }
            ctx.request_repaint_after(std::time::Duration::from_millis(200));
        }

        // Style
        let mut style = (*ctx.style()).clone();
        style.spacing.item_spacing = egui::vec2(8.0, 6.0);
        style.spacing.button_padding = egui::vec2(14.0, 6.0);
        ctx.set_style(style);

        egui::CentralPanel::default().show(ctx, |ui| {
            match &self.screen {
                Screen::Setup => self.draw_setup(ui),
                Screen::Connecting => self.draw_connecting(ui, ctx),
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
    fn draw_setup(&mut self, ui: &mut egui::Ui) {
        ui.vertical_centered(|ui| {
            ui.add_space(8.0);
            ui.heading("hostelD");
            ui.label(format!("Your ID: {}", self.identity.fingerprint));
            ui.add_space(6.0);
        });

        ui.separator();

        // Network mode
        ui.label("Network mode:");
        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.network_mode, 0, "LAN");
            ui.selectable_value(&mut self.network_mode, 1, "Internet");
        });

        // IPv6
        let filtered_addrs: Vec<(usize, &(String, String))> = self.ipv6_addrs.iter()
            .enumerate()
            .filter(|(_, (_, label))| {
                if self.network_mode == 1 {
                    label.contains("global") || label.contains("loopback")
                } else {
                    label.contains("link-local") || label.contains("loopback")
                }
            }).collect();

        if !filtered_addrs.is_empty() {
            ui.label("Your IPv6:");
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
    }

    fn draw_connecting(&self, ui: &mut egui::Ui, ctx: &egui::Context) {
        ui.vertical_centered(|ui| {
            ui.add_space(60.0);
            ui.heading("Connecting...");
            ui.add_space(15.0);
            ui.spinner();
            ui.add_space(15.0);
            ui.label("Key exchange + identity verification");
            ui.label(format!("Peer: [{}]:{}", self.peer_ip, self.peer_port));
        });
        ctx.request_repaint_after(std::time::Duration::from_millis(200));
    }

    fn draw_call(&mut self, ui: &mut egui::Ui) {
        let mic_on = self.mic_active.load(Ordering::Relaxed);

        // ── Top bar: status ──
        ui.horizontal(|ui| {
            ui.heading("hostelD");
            ui.separator();
            ui.colored_label(egui::Color32::from_rgb(80, 200, 80), "ENCRYPTED");
            ui.separator();
            ui.label(format!("Peer: {}", self.peer_fingerprint));
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

        // ── Controls row: mic + hang up ──
        ui.horizontal(|ui| {
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

            let hangup_btn = egui::Button::new(
                egui::RichText::new("Hang Up").size(16.0).color(egui::Color32::WHITE)
            ).min_size(egui::vec2(100.0, 35.0)).fill(egui::Color32::from_rgb(180, 40, 40));
            if ui.add(hangup_btn).clicked() {
                self.hang_up();
                return;
            }
        });

        ui.separator();

        // ── Chat area ──
        ui.label(egui::RichText::new("Chat").strong());

        // Message history (scrollable)
        let available_height = ui.available_height() - 35.0; // leave room for input
        egui::ScrollArea::vertical()
            .max_height(available_height)
            .stick_to_bottom(true)
            .show(ui, |ui| {
                if let Some(history) = &self.chat_history {
                    if history.messages.is_empty() {
                        ui.colored_label(egui::Color32::GRAY, "No messages yet. Type below.");
                    }
                    for msg in &history.messages {
                        let time = ChatHistory::format_time(msg.timestamp);
                        if msg.from_me {
                            ui.horizontal(|ui| {
                                ui.colored_label(egui::Color32::GRAY, &time);
                                ui.colored_label(egui::Color32::from_rgb(100, 180, 255), "You:");
                                ui.label(&msg.text);
                            });
                        } else {
                            ui.horizontal(|ui| {
                                ui.colored_label(egui::Color32::GRAY, &time);
                                ui.colored_label(egui::Color32::from_rgb(180, 255, 100), "Peer:");
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
            .with_inner_size([450.0, 650.0])
            .with_min_inner_size([400.0, 550.0])
            .with_title("hostelD — Secure P2P Voice + Chat"),
        ..Default::default()
    };
    eframe::run_native(
        "hostelD",
        options,
        Box::new(|cc| Ok(Box::new(HostelApp::new(cc)))),
    ).expect("Failed to start GUI");
}
