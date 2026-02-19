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
    Appearance,
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

    #[cfg(target_os = "macos")]
    {
        if let Ok(output) = std::process::Command::new("ifconfig").output() {
            let text = String::from_utf8_lossy(&output.stdout);
            let mut current_iface = String::new();
            for line in text.lines() {
                // Interface header: "en0: flags=8863<UP,..."
                if !line.starts_with('\t') && !line.starts_with(' ') {
                    if let Some(name) = line.split(':').next() {
                        current_iface = name.to_string();
                    }
                }
                let trimmed = line.trim();
                if trimmed.starts_with("inet6") {
                    // Skip temporary/deprecated addresses
                    if trimmed.contains("deprecated") || trimmed.contains("temporary") {
                        continue;
                    }
                    // Format: "inet6 fe80::1%en0 prefixlen 64 scopeid 0x4"
                    // or:     "inet6 2001:db8::1 prefixlen 64"
                    if let Some(addr_raw) = trimmed.split_whitespace().nth(1) {
                        // Strip %interface suffix (e.g. "fe80::1%en0" → "fe80::1")
                        let addr = addr_raw.split('%').next().unwrap_or(addr_raw);
                        if addr == "::1" { continue; }
                        // Skip loopback interface
                        if current_iface == "lo0" { continue; }
                        let scope = if addr.starts_with("fe80") { "link-local" } else { "global" };
                        result.push((current_iface.clone(), addr.to_string(), scope.to_string()));
                    }
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

/// Build a LayoutJob with nickname bold/large and #fingerprint smaller/gray.
pub(crate) fn peer_display_job(nickname: &str, fingerprint: &str, base_size: f32, name_color: egui::Color32, dim_color: egui::Color32) -> egui::text::LayoutJob {
    let mut job = egui::text::LayoutJob::default();
    if nickname.is_empty() {
        job.append(
            fingerprint,
            0.0,
            egui::TextFormat {
                font_id: egui::FontId::proportional(base_size),
                color: name_color,
                ..Default::default()
            },
        );
    } else {
        job.append(
            nickname,
            0.0,
            egui::TextFormat {
                font_id: egui::FontId::proportional(base_size + 1.0),
                color: name_color,
                ..Default::default()
            },
        );
        job.append(
            &format!(" #{fingerprint}"),
            0.0,
            egui::TextFormat {
                font_id: egui::FontId::proportional(base_size - 2.0),
                color: dim_color,
                ..Default::default()
            },
        );
    }
    job
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

/// Start playing the ringtone in a loop on a background thread.
/// Returns a stop flag; set it to true to stop playback.
fn start_ringtone() -> Arc<AtomicBool> {
    let stop = Arc::new(AtomicBool::new(false));
    let stop_clone = stop.clone();
    std::thread::spawn(move || {
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        let path = format!("{}/.hostelD/ringtone.mp3", std::env::var("HOME").unwrap_or_default());
        #[cfg(target_os = "windows")]
        let path = format!("{}\\.hostelD\\ringtone.mp3", std::env::var("USERPROFILE").unwrap_or_default());

        if !std::path::Path::new(&path).exists() {
            return;
        }

        while !stop_clone.load(std::sync::atomic::Ordering::Relaxed) {
            #[cfg(target_os = "linux")]
            let mut child = match std::process::Command::new("gst-play-1.0")
                .arg(&path)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
            {
                Ok(c) => c,
                Err(_) => break,
            };

            #[cfg(target_os = "windows")]
            let mut child = match std::process::Command::new("powershell")
                .args([
                    "-WindowStyle", "Hidden", "-Command",
                    &format!(
                        "Add-Type -TypeDefinition 'using System; using System.Runtime.InteropServices; \
                         public class WinMM {{ [DllImport(\"winmm.dll\")] \
                         public static extern int mciSendString(string cmd, System.Text.StringBuilder buf, int sz, IntPtr cb); }}'; \
                         $null=[WinMM]::mciSendString('open \"{}\" alias hostelring', $null, 0, [IntPtr]::Zero); \
                         $null=[WinMM]::mciSendString('play hostelring wait', $null, 0, [IntPtr]::Zero); \
                         $null=[WinMM]::mciSendString('close hostelring', $null, 0, [IntPtr]::Zero)",
                        path.replace('\\', "/")
                    ),
                ])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
            {
                Ok(c) => c,
                Err(_) => break,
            };

            #[cfg(target_os = "macos")]
            let mut child = match std::process::Command::new("afplay")
                .arg(&path)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
            {
                Ok(c) => c,
                Err(_) => break,
            };

            loop {
                if stop_clone.load(std::sync::atomic::Ordering::Relaxed) {
                    child.kill().ok();
                    child.wait().ok();
                    return;
                }
                match child.try_wait() {
                    Ok(Some(_)) => break, // Finished playing, loop again
                    Ok(None) => std::thread::sleep(std::time::Duration::from_millis(100)),
                    Err(_) => return,
                }
            }
        }
    });
    stop
}

/// Play the notification sound once in a background thread.
fn play_notification_sound() {
    std::thread::spawn(|| {
        #[cfg(target_os = "linux")]
        let home = std::env::var("HOME").unwrap_or_default();
        #[cfg(target_os = "windows")]
        let home = std::env::var("USERPROFILE").unwrap_or_default();
        #[cfg(target_os = "macos")]
        let home = std::env::var("HOME").unwrap_or_default();

        let sep = if cfg!(windows) { "\\" } else { "/" };
        let path = format!("{home}{sep}.hostelD{sep}notification.mp3");

        if !std::path::Path::new(&path).exists() {
            return;
        }

        #[cfg(target_os = "linux")]
        {
            std::process::Command::new("gst-play-1.0")
                .arg(&path)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn().ok();
        }

        #[cfg(target_os = "windows")]
        {
            std::process::Command::new("powershell")
                .args([
                    "-WindowStyle", "Hidden", "-Command",
                    &format!(
                        "Add-Type -TypeDefinition 'using System; using System.Runtime.InteropServices; \
                         public class WinMM {{ [DllImport(\"winmm.dll\")] \
                         public static extern int mciSendString(string cmd, System.Text.StringBuilder buf, int sz, IntPtr cb); }}'; \
                         $null=[WinMM]::mciSendString('open \"{}\" alias hostelnotif', $null, 0, [IntPtr]::Zero); \
                         $null=[WinMM]::mciSendString('play hostelnotif wait', $null, 0, [IntPtr]::Zero); \
                         $null=[WinMM]::mciSendString('close hostelnotif', $null, 0, [IntPtr]::Zero)",
                        path.replace('\\', "/")
                    ),
                ])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn().ok();
        }

        #[cfg(target_os = "macos")]
        {
            std::process::Command::new("afplay")
                .arg(&path)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn().ok();
        }
    });
}

/// Write the embedded notification sound to ~/.hostelD/ if it doesn't exist yet.
fn ensure_notification_sound() {
    #[cfg(target_os = "linux")]
    let home = std::env::var("HOME").unwrap_or_default();
    #[cfg(target_os = "windows")]
    let home = std::env::var("USERPROFILE").unwrap_or_default();
    #[cfg(target_os = "macos")]
    let home = std::env::var("HOME").unwrap_or_default();

    let sep = if cfg!(windows) { "\\" } else { "/" };
    let path = format!("{home}{sep}.hostelD{sep}notification.mp3");

    if !std::path::Path::new(&path).exists() {
        let bytes = include_bytes!("../../assets/notification.mp3");
        std::fs::write(&path, bytes).ok();
    }
}

/// Send an OS-level desktop notification (notify-send on Linux, PowerShell balloon on Windows).
fn send_desktop_notification(title: &str, body: &str) {
    let t = title.to_string();
    let b = body.to_string();
    std::thread::spawn(move || {
        #[cfg(target_os = "linux")]
        {
            std::process::Command::new("notify-send")
                .args(["-u", "critical", "-a", "hostelD", &t, &b])
                .spawn()
                .ok();
        }
        #[cfg(target_os = "windows")]
        {
            let script = format!(
                "[void] [System.Reflection.Assembly]::LoadWithPartialName('System.Windows.Forms'); \
                 $n = New-Object System.Windows.Forms.NotifyIcon; \
                 $n.Icon = [System.Drawing.SystemIcons]::Information; \
                 $n.Visible = $true; \
                 $n.ShowBalloonTip(5000, '{}', '{}', 'Info'); \
                 Start-Sleep -Seconds 6; $n.Dispose()",
                t.replace('\'', "''"),
                b.replace('\'', "''"),
            );
            std::process::Command::new("powershell")
                .args(["-WindowStyle", "Hidden", "-Command", &script])
                .spawn()
                .ok();
        }
        #[cfg(target_os = "macos")]
        {
            let script = format!(
                "display notification \"{}\" with title \"{}\"",
                b.replace('\\', "\\\\").replace('"', "\\\""),
                t.replace('\\', "\\\\").replace('"', "\\\""),
            );
            std::process::Command::new("osascript")
                .args(["-e", &script])
                .spawn()
                .ok();
        }
    });
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
    pub(crate) msg_peer_presence: HashMap<String, crate::messaging::PresenceStatus>,
    pub(crate) msg_confirm_delete_chat: Option<String>,
    pub(crate) last_key_press: Instant,
    pub(crate) last_presence_sent: crate::messaging::PresenceStatus,

    // File transfers
    pub(crate) file_transfer_progress: HashMap<(String, u32), (u64, u64)>,

    // Contact requests
    pub(crate) req_incoming: Vec<(String, String, String, String)>, // (request_id, nickname, ip, fingerprint)
    pub(crate) req_ip_input: String,
    pub(crate) req_port_input: String,
    pub(crate) req_status: String,

    // IP privacy: censored by default
    pub(crate) show_ips: bool,

    // Incoming call notification
    pub(crate) incoming_call: Option<IncomingCallInfo>,
    pub(crate) incoming_call_attention: bool,

    // Ringtone playback (background thread)
    pub(crate) ringtone_stop: Option<Arc<AtomicBool>>,

    // Notification sound cooldown
    pub(crate) last_notification_sound: Option<Instant>,

    // Icon textures (loaded once from embedded PNGs, cropped + LINEAR filtered)
    pub(crate) logo_texture: Option<egui::TextureHandle>,
    pub(crate) call_icon_texture: Option<egui::TextureHandle>,
    pub(crate) settings_icon_texture: Option<egui::TextureHandle>,
    pub(crate) enablecam_icon_texture: Option<egui::TextureHandle>,

    // Color palette editor
    pub(crate) color_hex_inputs: HashMap<String, String>,
    pub(crate) color_locks: std::collections::HashSet<String>,

    // Firewall prompt
    pub(crate) show_firewall_prompt: bool,
    pub(crate) firewall_old_port: String,

    // Settings feedback
    pub(crate) port_saved_at: Option<Instant>,
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

        // Auto-connect all contacts that have a known address
        let connect_contacts: Vec<_> = contacts.iter()
            .filter_map(|c| {
                if c.last_address.is_empty() || c.last_port.is_empty() {
                    return None;
                }
                let addr_str = format!("[{}]:{}", c.last_address, c.last_port);
                let addr: std::net::SocketAddr = addr_str.parse().ok()?;
                Some((c.contact_id.clone(), addr, c.pubkey))
            })
            .collect();
        if !connect_contacts.is_empty() {
            cmd_tx.send(MsgCommand::ConnectAll { contacts: connect_contacts }).ok();
        }

        let needs_firewall_prompt = settings.firewall_port != local_port;

        // Write embedded notification sound to ~/.hostelD/ if missing
        ensure_notification_sound();

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
            msg_peer_presence: HashMap::new(),
            msg_confirm_delete_chat: None,
            last_key_press: Instant::now(),
            last_presence_sent: crate::messaging::PresenceStatus::Online,
            file_transfer_progress: HashMap::new(),
            req_incoming: Vec::new(),
            req_ip_input: String::new(),
            req_port_input: String::new(),
            req_status: String::new(),
            show_ips: false,
            incoming_call: None,
            incoming_call_attention: false,
            ringtone_stop: None,
            last_notification_sound: None,
            logo_texture: None,
            call_icon_texture: None,
            settings_icon_texture: None,
            enablecam_icon_texture: None,
            color_hex_inputs: HashMap::new(),
            color_locks: std::collections::HashSet::new(),
            show_firewall_prompt: needs_firewall_prompt,
            firewall_old_port: String::new(),
            port_saved_at: None,
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

            let any_key = ctx.input(|i| !i.events.is_empty());
            if any_key {
                self.last_key_press = Instant::now();
            }

            // Away detection: inactive >15 min = Away, otherwise Online
            {
                let last_activity = self.last_mouse_move.max(self.last_key_press);
                let current_presence = if last_activity.elapsed().as_secs() > 15 * 60 {
                    crate::messaging::PresenceStatus::Away
                } else {
                    crate::messaging::PresenceStatus::Online
                };
                if current_presence != self.last_presence_sent {
                    self.last_presence_sent = current_presence;
                    if let Some(tx) = &self.msg_cmd_tx {
                        tx.send(MsgCommand::UpdatePresence { status: current_presence }).ok();
                    }
                }
            }

            let repaint_ms = if self.screen_texture.is_some() { 33 } else { 200 };
            ctx.request_repaint_after(std::time::Duration::from_millis(repaint_ms));
        }

        // Poll messaging daemon events
        let mut accepted_contact_id: Option<String> = None;
        if let Some(rx) = &self.msg_event_rx {
            while let Ok(evt) = rx.try_recv() {
                match evt {
                    MsgEvent::IncomingMessage { contact_id, text } => {
                        // Append to chat history
                        let history = self.msg_chat_histories.entry(contact_id.clone())
                            .or_insert_with(|| ChatHistory::load(&contact_id, &self.identity.secret));
                        history.add_message(false, text);

                        let viewing_this_chat = self.active_tab == SidebarTab::Messages
                            && self.msg_active_chat.as_deref() == Some(contact_id.as_str());

                        // Increment unread if not viewing this specific chat
                        if !viewing_this_chat {
                            *self.msg_unread.entry(contact_id).or_insert(0) += 1;
                        }
                        // Play notification sound if not in a call and not viewing this chat (3s cooldown)
                        if !matches!(self.screen, Screen::InCall | Screen::Connecting | Screen::KeyWarning)
                            && !viewing_this_chat
                        {
                            let should_play = self.last_notification_sound
                                .map_or(true, |t| t.elapsed().as_secs_f32() > 3.0);
                            if should_play {
                                self.last_notification_sound = Some(Instant::now());
                                play_notification_sound();
                            }
                        }
                    }
                    MsgEvent::MessageDelivered => {
                        // Could update delivery status UI here
                    }
                    MsgEvent::PeerStatus { contact_id, online } => {
                        self.msg_peer_online.insert(contact_id.clone(), online);
                        if !online {
                            self.msg_peer_presence.insert(contact_id, crate::messaging::PresenceStatus::Offline);
                        }
                    }
                    MsgEvent::PresenceUpdate { contact_id, status } => {
                        self.msg_peer_presence.insert(contact_id, status);
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
                    MsgEvent::PeerAddressUpdate { contact_id, ip, port } => {
                        // Update local contact list with new address
                        if let Some(contact) = self.contacts.iter_mut().find(|c| c.contact_id == contact_id) {
                            contact.last_address = ip;
                            contact.last_port = port;
                        }
                    }
                    MsgEvent::IncomingFileOffer { contact_id, transfer_id, filename, file_size } => {
                        let history = self.msg_chat_histories.entry(contact_id.clone())
                            .or_insert_with(|| ChatHistory::load(&contact_id, &self.identity.secret));
                        history.add_file_message(false, crate::chat::FileTransferInfo {
                            filename: filename.clone(),
                            file_size,
                            transfer_id,
                            status: crate::chat::FileTransferStatus::Offered,
                            saved_path: None,
                        });
                        let viewing_this_chat = self.active_tab == SidebarTab::Messages
                            && self.msg_active_chat.as_deref() == Some(contact_id.as_str());
                        if !viewing_this_chat {
                            *self.msg_unread.entry(contact_id).or_insert(0) += 1;
                        }
                    }
                    MsgEvent::FileTransferProgress { contact_id, transfer_id, bytes_transferred, total_bytes } => {
                        let is_new = !self.file_transfer_progress.contains_key(&(contact_id.clone(), transfer_id));
                        self.file_transfer_progress.insert(
                            (contact_id.clone(), transfer_id),
                            (bytes_transferred, total_bytes),
                        );
                        // Transition Offered → Accepted on first progress event (sender side)
                        if is_new {
                            if let Some(history) = self.msg_chat_histories.get_mut(&contact_id) {
                                history.update_file_status(
                                    transfer_id,
                                    crate::chat::FileTransferStatus::Accepted,
                                    None,
                                );
                            }
                        }
                    }
                    MsgEvent::FileTransferComplete { contact_id, transfer_id, saved_path } => {
                        self.file_transfer_progress.remove(&(contact_id.clone(), transfer_id));
                        let history = self.msg_chat_histories.entry(contact_id.clone())
                            .or_insert_with(|| ChatHistory::load(&contact_id, &self.identity.secret));
                        let save_path = if saved_path.is_empty() { None } else { Some(saved_path) };
                        history.update_file_status(
                            transfer_id,
                            crate::chat::FileTransferStatus::Completed,
                            save_path,
                        );
                    }
                    MsgEvent::FileTransferFailed { contact_id, transfer_id, reason } => {
                        self.file_transfer_progress.remove(&(contact_id.clone(), transfer_id));
                        let history = self.msg_chat_histories.entry(contact_id.clone())
                            .or_insert_with(|| ChatHistory::load(&contact_id, &self.identity.secret));
                        history.update_file_status(
                            transfer_id,
                            crate::chat::FileTransferStatus::Failed(reason),
                            None,
                        );
                    }
                    MsgEvent::IncomingCall { nickname, fingerprint, ip, port } => {
                        log_fmt!("[gui] IncomingCall event: nick='{}' fp='{}' ip='{}' port='{}' screen={}",
                            nickname, fingerprint, ip, port,
                            match &self.screen { Screen::Setup => "Setup", Screen::Connecting => "Connecting", Screen::InCall => "InCall", Screen::KeyWarning => "KeyWarning", Screen::Error(_) => "Error" });
                        // Only show if not already in a call
                        if matches!(self.screen, Screen::Setup) {
                            // OS-level desktop notification
                            send_desktop_notification(
                                "hostelD — Incoming Call",
                                &format_peer_display(&nickname, &fingerprint),
                            );

                            // Play ringtone
                            if self.ringtone_stop.is_none() {
                                self.ringtone_stop = Some(start_ringtone());
                            }

                            self.incoming_call = Some(IncomingCallInfo {
                                nickname, fingerprint, ip, port,
                            });
                            self.incoming_call_attention = true;
                        }
                    }
                }
            }
        }
        // Deferred: open chat for newly accepted contact (after borrow of msg_event_rx ends)
        if let Some(cid) = accepted_contact_id {
            self.open_msg_chat(&cid);
        }

        // Flash taskbar/window for incoming call (system-level attention)
        if self.incoming_call_attention {
            self.incoming_call_attention = false;
            ctx.send_viewport_cmd(egui::ViewportCommand::RequestUserAttention(
                egui::UserAttentionType::Critical,
            ));
        }

        // Style + theme visuals
        let mut style = (*ctx.style()).clone();
        // Bump all font sizes by 1 point (from defaults, not current — update runs every frame)
        let defaults = egui::Style::default();
        for (text_style, font_id) in style.text_styles.iter_mut() {
            if let Some(def) = defaults.text_styles.get(text_style) {
                font_id.size = def.size + 1.0;
            }
        }
        style.spacing.item_spacing = egui::vec2(8.0, 6.0);
        style.spacing.button_padding = egui::vec2(14.0, 6.0);
        let t = &self.settings.theme;
        style.visuals.panel_fill = t.panel_bg();
        style.visuals.window_fill = t.panel_bg();
        style.visuals.extreme_bg_color = t.panel_bg();
        style.visuals.faint_bg_color = t.panel_bg();
        style.visuals.widgets.noninteractive.fg_stroke.color = t.text_primary();
        style.visuals.widgets.noninteractive.bg_stroke.color = t.separator();
        style.visuals.widgets.inactive.weak_bg_fill = t.widget_bg();
        style.visuals.widgets.inactive.bg_fill = t.widget_bg();
        style.visuals.widgets.inactive.fg_stroke.color = t.text_primary();
        style.visuals.widgets.hovered.weak_bg_fill = t.widget_hovered();
        style.visuals.widgets.hovered.bg_fill = t.widget_hovered();
        style.visuals.widgets.hovered.fg_stroke.color = t.text_primary();
        style.visuals.widgets.active.weak_bg_fill = t.widget_active();
        style.visuals.widgets.active.bg_fill = t.widget_active();
        style.visuals.widgets.active.fg_stroke.color = t.text_primary();
        style.visuals.selection.bg_fill = t.widget_active();
        style.visuals.selection.stroke.color = t.text_primary();
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
        let sidebar_frame = egui::Frame::none()
            .fill(self.settings.theme.sidebar_bg())
            .inner_margin(egui::Margin::same(4.0));
        egui::SidePanel::left("sidebar")
            .exact_width(125.0)
            .resizable(false)
            .frame(sidebar_frame)
            .show_separator_line(false)
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
                SidebarTab::Appearance => self.draw_appearance_tab(ui),
            }
        });

        // ── Incoming call popup (overlay on top of everything) ──
        if self.incoming_call.is_some() {
            self.draw_incoming_call_popup(ctx);
        }

        // ── Firewall prompt popup ──
        if self.show_firewall_prompt {
            self.draw_firewall_prompt(ctx);
        }

        // Always schedule periodic repaints so we detect daemon events
        // (incoming calls, messages, peer status) even when idle.
        ctx.request_repaint_after(std::time::Duration::from_secs(4));
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        // Drop the command sender so the daemon thread detects disconnection and exits.
        self.msg_cmd_tx.take();
        // Give the daemon a moment to send BYE packets to peers.
        std::thread::sleep(std::time::Duration::from_millis(200));
        // Force-terminate: daemon or audio threads may still be alive.
        std::process::exit(0);
    }
}

impl HostelApp {
    fn draw_incoming_call_popup(&mut self, ctx: &egui::Context) {
        let call_info = match &self.incoming_call {
            Some(info) => info,
            None => return,
        };
        let caller_job = peer_display_job(&call_info.nickname, &call_info.fingerprint, 16.0, self.settings.theme.text_primary(), self.settings.theme.text_dim());
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
                            .color(self.settings.theme.accent()),
                    );
                    ui.add_space(8.0);
                    ui.label(caller_job);
                    ui.colored_label(self.settings.theme.text_muted(), &ip_display);
                    ui.add_space(12.0);
                });
                ui.horizontal(|ui| {
                    let accept_btn = egui::Button::new(
                        egui::RichText::new("Accept").size(16.0).color(egui::Color32::WHITE),
                    )
                    .min_size(egui::vec2(120.0, 38.0))
                    .fill(self.settings.theme.btn_positive());
                    if ui.add(accept_btn).clicked() {
                        accept = true;
                    }

                    let reject_btn = egui::Button::new(
                        egui::RichText::new("Reject").size(16.0).color(egui::Color32::WHITE),
                    )
                    .min_size(egui::vec2(120.0, 38.0))
                    .fill(self.settings.theme.btn_negative());
                    if ui.add(reject_btn).clicked() {
                        reject = true;
                    }
                });
                ui.add_space(4.0);
            });

        if accept {
            if let Some(info) = self.incoming_call.take() {
                // Stop ringtone
                if let Some(stop) = self.ringtone_stop.take() {
                    stop.store(true, Ordering::Relaxed);
                }
                // Clear cooldown (no reject — we're accepting)
                if let Some(tx) = &self.msg_cmd_tx {
                    tx.send(MsgCommand::DismissIncomingCall { ip: info.ip.clone(), reject: false }).ok();
                }
                self.peer_ip = info.ip;
                self.peer_port = info.port;
                self.active_tab = SidebarTab::Call;
                self.start_call();
            }
        }
        if reject {
            if let Some(info) = self.incoming_call.take() {
                // Stop ringtone
                if let Some(stop) = self.ringtone_stop.take() {
                    stop.store(true, Ordering::Relaxed);
                }
                // Reject: daemon will complete handshake + send HANGUP to cut caller
                if let Some(tx) = &self.msg_cmd_tx {
                    tx.send(MsgCommand::DismissIncomingCall { ip: info.ip, reject: true }).ok();
                }
            }
        }

        ctx.request_repaint_after(std::time::Duration::from_millis(200));
    }

    fn draw_firewall_prompt(&mut self, ctx: &egui::Context) {
        let is_port_change = !self.firewall_old_port.is_empty();
        let port = self.local_port.clone();

        let mut action = false;
        let mut skip = false;

        egui::Window::new("Firewall")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.add_space(8.0);
                ui.vertical_centered(|ui| {
                    if is_port_change {
                        ui.label(
                            egui::RichText::new(format!("Port changed to {}.", port))
                                .size(15.0),
                        );
                        ui.label(
                            egui::RichText::new("The old firewall rule will be replaced.")
                                .size(14.0),
                        );
                    } else {
                        ui.label(
                            egui::RichText::new(format!(
                                "hostelD needs to open UDP port {} in the firewall", port
                            ))
                            .size(15.0),
                        );
                        ui.label(
                            egui::RichText::new("for calls and messages to work.")
                                .size(14.0),
                        );
                    }
                    ui.add_space(2.0);
                    ui.colored_label(
                        self.settings.theme.text_muted(),
                        "This requires admin permission.",
                    );
                    ui.add_space(12.0);
                });
                ui.horizontal(|ui| {
                    let action_label = if is_port_change { "Update" } else { "Open Port" };
                    let action_btn = egui::Button::new(
                        egui::RichText::new(action_label).size(15.0).color(egui::Color32::WHITE),
                    )
                    .min_size(egui::vec2(120.0, 36.0))
                    .fill(self.settings.theme.btn_positive());
                    if ui.add(action_btn).clicked() {
                        action = true;
                    }

                    let skip_btn = egui::Button::new(
                        egui::RichText::new("Skip").size(15.0),
                    )
                    .min_size(egui::vec2(120.0, 36.0));
                    if ui.add(skip_btn).clicked() {
                        skip = true;
                    }
                });
                ui.add_space(4.0);
            });

        if action {
            // Remove old rule if port changed
            if is_port_change {
                if let Ok(old_port) = self.firewall_old_port.parse::<u16>() {
                    match crate::sysfirewall::remove_udp_port_rule(old_port) {
                        Ok(true) => log_fmt!("[firewall] Removed old rule for UDP port {}", old_port),
                        Ok(false) => log_fmt!("[firewall] No old rule found for UDP port {}", old_port),
                        Err(e) => log_fmt!("[firewall] WARNING removing old rule: {}", e),
                    }
                }
            }
            // Add new rule
            if let Ok(port_num) = port.parse::<u16>() {
                match crate::sysfirewall::ensure_udp_port_open(port_num) {
                    Ok(true) => log_fmt!("[firewall] Added rule for UDP port {}", port_num),
                    Ok(false) => log_fmt!("[firewall] Rule already exists for UDP port {}", port_num),
                    Err(e) => log_fmt!("[firewall] WARNING: {}", e),
                }
            }
            // Update tracked port
            self.settings.firewall_port = self.local_port.clone();
            self.settings.save();
            self.firewall_old_port.clear();
            self.show_firewall_prompt = false;
        }
        if skip {
            self.firewall_old_port.clear();
            self.show_firewall_prompt = false;
        }
    }
}

/// Load a PNG from bytes, crop transparent padding, return (rgba, width, height).
pub(crate) fn load_png_cropped(png_bytes: &[u8]) -> (Vec<u8>, u32, u32) {
    let img = image::load_from_memory(png_bytes).expect("failed to decode PNG");
    let rgba = img.to_rgba8();
    let (w, h) = (rgba.width(), rgba.height());

    // Find bounding box of non-transparent pixels
    let mut min_x = w;
    let mut min_y = h;
    let mut max_x = 0u32;
    let mut max_y = 0u32;
    for y in 0..h {
        for x in 0..w {
            if rgba.get_pixel(x, y)[3] > 0 {
                min_x = min_x.min(x);
                min_y = min_y.min(y);
                max_x = max_x.max(x);
                max_y = max_y.max(y);
            }
        }
    }

    if max_x < min_x || max_y < min_y {
        return (rgba.into_raw(), w, h);
    }

    let crop_w = max_x - min_x + 1;
    let crop_h = max_y - min_y + 1;
    let cropped = image::imageops::crop_imm(&rgba, min_x, min_y, crop_w, crop_h).to_image();
    (cropped.into_raw(), crop_w, crop_h)
}

/// Load a PNG, crop transparent padding, downscale with Lanczos3, and create an egui texture.
/// max_size caps the largest dimension (0 = no downscale).
pub(crate) fn load_icon_texture_sized(ctx: &egui::Context, name: &str, png_bytes: &[u8], max_size: u32) -> egui::TextureHandle {
    let (rgba, w, h) = load_png_cropped(png_bytes);
    let img = image::RgbaImage::from_raw(w, h, rgba).unwrap();
    let img = if max_size > 0 && (w > max_size || h > max_size) {
        // Preserve aspect ratio: scale so largest dimension = max_size
        let scale = max_size as f32 / w.max(h) as f32;
        let nw = ((w as f32 * scale).round() as u32).max(1);
        let nh = ((h as f32 * scale).round() as u32).max(1);
        image::imageops::resize(&img, nw, nh, image::imageops::FilterType::Lanczos3)
    } else {
        img
    };
    let (fw, fh) = (img.width(), img.height());
    let pixels = egui::ColorImage::from_rgba_unmultiplied([fw as usize, fh as usize], &img);
    ctx.load_texture(name, pixels, egui::TextureOptions::LINEAR)
}

pub fn run() {
    // Window icon from cropped logo (cross-platform: Windows, Linux, macOS)
    let (rgba, w, h) = load_png_cropped(include_bytes!("../../assets/logo.png"));
    let icon = egui::IconData { rgba, width: w, height: h };

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([884.0, 750.0])
            .with_min_inner_size([484.0, 600.0])
            .with_title("hostelD — Secure P2P Voice + Chat + Screen")
            .with_icon(std::sync::Arc::new(icon))
            .with_drag_and_drop(true),
        ..Default::default()
    };
    eframe::run_native(
        "hostelD",
        options,
        Box::new(|cc| Ok(Box::new(HostelApp::new(cc)))),
    ).expect("Failed to start GUI");
}
