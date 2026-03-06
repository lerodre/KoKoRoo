mod sidebar;
mod profile;
mod contacts;
mod call;
mod settings;
mod error;
mod messages;
mod groups;
mod network;
mod notifications;
mod popups;
mod logs;

use cpal::traits::{DeviceTrait, HostTrait};
use eframe::egui;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::Instant;

use crate::chat::{ChatHistory, GroupChatHistory};
use crate::group::{self, Group, GroupMember};
use crate::groupcall::{GroupCallInfo, GroupChatMsg, GroupRole};
use crate::identity::{self, Contact, Identity, Settings};
use crate::messaging::{MsgCommand, MsgDaemon, MsgEvent};
use crate::screen::{ScreenCommand, ScreenViewer};

use groups::GroupView;

// Re-export functions used by submodules via super::
pub(crate) use network::{get_best_ipv6, get_adapter_names, format_peer_display, peer_display_job, censor_ip};
pub(crate) use notifications::{start_ringtone, play_notification_sound, send_desktop_notification};
pub(crate) use popups::{load_png_cropped, load_icon_texture_sized, load_avatar_texture};

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
    Friends,
    Messages,
    Groups,
    Call,
    Settings,
    Appearance,
    Logs,
}

#[derive(Clone, Copy, PartialEq)]
pub(crate) enum FriendsSubTab {
    List,
    Requests,
}

#[derive(Clone, Copy, PartialEq, Default)]
pub(crate) enum LogFilter {
    #[default]
    All,
    Daemon,
    Groups,
    Voice,
    Gui,
    Network,
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
    pub(crate) viewer_joined: Arc<AtomicBool>,
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

    pub(crate) viewing_screen: bool,
    pub(crate) viewer_joined: Option<Arc<AtomicBool>>,
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
    pub(crate) msg_visible_count: HashMap<String, usize>,
    /// After loading older messages, stores the old content height so next frame we can compensate scroll.
    pub(crate) msg_scroll_compensate: Option<f32>,
    pub(crate) last_key_press: Instant,
    pub(crate) last_presence_sent: crate::messaging::PresenceStatus,

    // File transfers
    pub(crate) file_transfer_progress: HashMap<(String, u32), (u64, u64)>,

    // Contact requests
    pub(crate) req_incoming: Vec<(String, String, String, String)>, // (request_id, nickname, ip, fingerprint)
    pub(crate) req_ip_input: String,
    pub(crate) req_port_input: String,
    pub(crate) req_status: String,

    // Friends tab sub-tab
    pub(crate) friends_sub_tab: FriendsSubTab,

    // Logs tab filter
    pub(crate) log_filter: LogFilter,

    // IP privacy: censored by default
    pub(crate) show_ips: bool,

    // Incoming call notification
    pub(crate) incoming_call: Option<IncomingCallInfo>,
    pub(crate) incoming_call_attention: bool,

    // Ringtone playback (background thread)
    pub(crate) ringtone_stop: Option<Arc<AtomicBool>>,

    // Group invite popup
    pub(crate) incoming_group_invite: Option<IncomingGroupInviteInfo>,

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

    // Avatar textures (loaded lazily, invalidated on change)
    pub(crate) own_avatar_texture: Option<egui::TextureHandle>,
    pub(crate) contact_avatar_textures: HashMap<String, egui::TextureHandle>,
    pub(crate) default_avatar_texture: Option<egui::TextureHandle>,

    // Crop editor state
    pub(crate) show_crop_editor: bool,
    pub(crate) crop_source_bytes: Option<Vec<u8>>,
    pub(crate) crop_source_texture: Option<egui::TextureHandle>,
    pub(crate) crop_source_dims: (u32, u32),
    pub(crate) crop_offset: (f32, f32),
    pub(crate) crop_size: f32,
    pub(crate) crop_dragging: bool,
    pub(crate) crop_drag_start: (f32, f32),

    // Groups
    pub(crate) groups: Vec<Group>,
    pub(crate) group_view: GroupView,
    pub(crate) group_create_name: String,
    pub(crate) group_selected_members: Vec<bool>,
    pub(crate) group_detail_idx: Option<usize>,
    pub(crate) group_detail_chat_input: String,
    pub(crate) group_settings_idx: Option<usize>,
    pub(crate) group_rename_input: String,
    pub(crate) group_avatar_textures: HashMap<String, egui::TextureHandle>,
    pub(crate) group_avatar_crop_group_id: Option<String>,
    pub(crate) group_settings_invite_mode: bool,
    pub(crate) group_settings_selected_members: Vec<bool>,
    pub(crate) group_selected_channel: String,
    pub(crate) group_channel_create_name: String,
    pub(crate) group_channel_creating: bool,
    pub(crate) voice_channel_creating: bool,
    pub(crate) voice_channel_create_name: String,

    // Group call state
    pub(crate) group_call_channel_id: Option<String>,
    pub(crate) group_screen_sharing: bool,
    pub(crate) group_webcam_sharing: bool,
    pub(crate) group_call_running: Arc<AtomicBool>,
    pub(crate) group_call_mic: Arc<AtomicBool>,
    pub(crate) group_call_hangup: Option<Arc<AtomicBool>>,
    pub(crate) group_call_chat_tx: Option<mpsc::Sender<String>>,
    pub(crate) group_call_chat_rx: Option<mpsc::Receiver<GroupChatMsg>>,
    pub(crate) group_call_roster_rx: Option<mpsc::Receiver<Vec<GroupMember>>>,
    pub(crate) group_call_chat_input: String,
    pub(crate) group_call_messages: Vec<GroupChatMsg>,
    pub(crate) group_call_members: Vec<GroupMember>,
    pub(crate) group_call_group: Option<Group>,
    pub(crate) group_call_role: Option<GroupRole>,
    pub(crate) group_chat_history: Option<GroupChatHistory>,
    pub(crate) group_connect_result: Arc<std::sync::Mutex<Option<Result<GroupCallInfo, String>>>>,
    /// After hang-up, if others were still in the call, remember the locked mode for that channel.
    /// Key = channel_id, Value = the CallMode that was active. Cleared when we re-join or group updates.
    pub(crate) group_call_ongoing: std::collections::HashMap<String, crate::group::CallMode>,
}

pub(crate) struct IncomingCallInfo {
    pub(crate) nickname: String,
    pub(crate) fingerprint: String,
    pub(crate) ip: String,
    pub(crate) port: String,
}

pub(crate) struct IncomingGroupInviteInfo {
    pub(crate) from_nickname: String,
    pub(crate) from_contact_id: String,
    pub(crate) group_name: String,
    pub(crate) member_count: usize,
    pub(crate) invite_lite: crate::group::GroupInviteLite,
}

impl HostelApp {
    pub fn new(_cc: &eframe::CreationContext<'_>) -> Self {
        let devices = list_audio_devices();
        let identity = Identity::load_or_create();
        let settings = Settings::load();
        let contacts = identity::load_all_contacts();
        let mut groups = group::load_all_groups();
        // Ensure all groups have general + fallback channels + voice channels (migration for old groups)
        for grp in &mut groups {
            let had_channels = !grp.text_channels.is_empty();
            let had_voice = !grp.voice_channels.is_empty();
            group::ensure_general_channel(grp);
            group::ensure_fallback_channel(grp);
            group::ensure_general_voice_channel(grp);
            if !had_channels || !had_voice {
                group::save_group(grp);
            }
        }
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
        notifications::ensure_notification_sound();

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
            viewing_screen: false,
            viewer_joined: None,
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
            msg_visible_count: HashMap::new(),
            msg_scroll_compensate: None,
            last_key_press: Instant::now(),
            last_presence_sent: crate::messaging::PresenceStatus::Online,
            file_transfer_progress: HashMap::new(),
            req_incoming: Vec::new(),
            req_ip_input: String::new(),
            req_port_input: String::new(),
            req_status: String::new(),
            friends_sub_tab: FriendsSubTab::List,
            log_filter: LogFilter::All,
            show_ips: false,
            incoming_call: None,
            incoming_call_attention: false,
            incoming_group_invite: None,
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
            own_avatar_texture: None,
            contact_avatar_textures: HashMap::new(),
            default_avatar_texture: None,
            show_crop_editor: false,
            crop_source_bytes: None,
            crop_source_texture: None,
            crop_source_dims: (0, 0),
            crop_offset: (0.0, 0.0),
            crop_size: 1.0,
            crop_dragging: false,
            crop_drag_start: (0.0, 0.0),
            groups,
            group_view: GroupView::List,
            group_create_name: String::new(),
            group_selected_members: Vec::new(),
            group_detail_idx: None,
            group_detail_chat_input: String::new(),
            group_settings_idx: None,
            group_rename_input: String::new(),
            group_avatar_textures: HashMap::new(),
            group_avatar_crop_group_id: None,
            group_settings_invite_mode: false,
            group_settings_selected_members: Vec::new(),
            group_selected_channel: "general".to_string(),
            group_channel_create_name: String::new(),
            group_channel_creating: false,
            voice_channel_creating: false,
            voice_channel_create_name: String::new(),
            group_call_channel_id: None,
            group_screen_sharing: false,
            group_webcam_sharing: false,
            group_call_running: Arc::new(AtomicBool::new(false)),
            group_call_mic: Arc::new(AtomicBool::new(true)),
            group_call_hangup: None,
            group_call_chat_tx: None,
            group_call_chat_rx: None,
            group_call_roster_rx: None,
            group_call_chat_input: String::new(),
            group_call_messages: Vec::new(),
            group_call_members: Vec::new(),
            group_call_group: None,
            group_call_role: None,
            group_chat_history: None,
            group_connect_result: Arc::new(std::sync::Mutex::new(None)),
            group_call_ongoing: std::collections::HashMap::new(),
        }
    }

    pub(crate) fn start_call(&mut self) {
        log_fmt!("[gui] start_call: peer=[{}]:{}", self.peer_ip, self.peer_port);
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
                            viewer_joined: engine.viewer_joined.clone(),
                            auto_banned_ips: engine.auto_banned_ips.clone(),
                        };
                        *result.lock().unwrap() = Some(Ok(info));

                        while running.load(Ordering::Relaxed) {
                            while let Ok(cmd) = screen_cmd_rx.try_recv() {
                                match cmd {
                                    ScreenCommand::StartScreen { quality, audio_device, display_index } => engine.start_screen_share(quality, audio_device, display_index),
                                    ScreenCommand::StartWebcam { quality, device_index } => engine.start_webcam_share(quality, device_index),
                                    ScreenCommand::Stop => engine.stop_screen_share(),
                                    ScreenCommand::JoinViewing => engine.send_screen_join(),
                                    ScreenCommand::LeaveViewing => engine.send_screen_leave(),
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
        log_fmt!("[gui] hang_up");
        if let Some(ref lh) = self.local_hangup {
            lh.store(true, Ordering::Relaxed);
        }
        self.running.store(false, Ordering::Relaxed);
        self.cleanup_call();
    }

    pub(crate) fn on_remote_hangup(&mut self) {
        log_fmt!("[gui] remote hangup");
        self.running.store(false, Ordering::Relaxed);
        self.cleanup_call();
    }

    pub(crate) fn cleanup_call(&mut self) {
        log_fmt!("[gui] cleanup_call");
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
        self.viewing_screen = false;
        self.viewer_joined = None;
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

    pub(crate) fn start_group_call(&mut self, channel_id: &str) {
        log_fmt!("[gui] start_group_call: channel={}", channel_id);
        let idx = match self.group_detail_idx {
            Some(i) if i < self.groups.len() => i,
            _ => return,
        };

        // If already in a different voice channel, cleanup first
        let was_switching = self.group_call_channel_id.is_some();
        if was_switching {
            self.cleanup_group_call();
        }

        // Yield socket from messaging daemon
        if let Some(tx) = &self.msg_cmd_tx {
            tx.send(MsgCommand::YieldSocket).ok();
        }

        // Save settings
        self.settings.mic = self.devices.input_names.get(self.selected_input)
            .cloned().unwrap_or_default();
        self.settings.speakers = self.devices.output_names.get(self.selected_output)
            .cloned().unwrap_or_default();
        self.settings.save();

        // Fresh arcs
        self.group_call_running.store(false, Ordering::Relaxed);
        self.group_call_running = Arc::new(AtomicBool::new(true));
        self.group_call_mic = Arc::new(AtomicBool::new(true));
        self.group_connect_result = Arc::new(std::sync::Mutex::new(None));
        self.group_call_members.clear();
        self.group_call_channel_id = Some(channel_id.to_string());
        self.group_call_ongoing.remove(channel_id);
        self.group_screen_sharing = false;
        self.group_webcam_sharing = false;

        self.group_view = GroupView::Connecting;

        let group = self.groups[idx].clone();
        let channel_id_owned = channel_id.to_string();
        let local_port = self.local_port.clone();
        let running = self.group_call_running.clone();
        let mic_active = self.group_call_mic.clone();
        let result = self.group_connect_result.clone();
        let input_idx = self.selected_input;
        let output_idx = self.selected_output;
        let my_pubkey = self.identity.pubkey;

        let my_sender_index = group.members.iter()
            .find(|m| m.pubkey == my_pubkey)
            .map(|m| m.sender_index)
            .unwrap_or(0);

        thread::spawn(move || {
            // Wait for daemon to yield socket (longer if switching channels)
            let wait_ms = if was_switching { 1000 } else { 500 };
            thread::sleep(std::time::Duration::from_millis(wait_ms));

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

            // Retry socket binding up to 3 times (old call threads may still hold the socket)
            let mut call_result = Err("Not started".to_string());
            for attempt in 0..3 {
                if attempt > 0 {
                    thread::sleep(std::time::Duration::from_millis(500));
                }
                if !running.load(Ordering::Relaxed) {
                    call_result = Err("Cancelled".to_string());
                    break;
                }
                call_result = crate::groupcall::start_group_call(
                    group.clone(), &channel_id_owned, &input_device, &output_device,
                    &local_port, running.clone(), mic_active.clone(), my_sender_index,
                );
                if call_result.is_ok() { break; }
            }

            *result.lock().unwrap() = Some(call_result);
        });
    }

    pub(crate) fn cleanup_group_call(&mut self) {
        log_fmt!("[gui] cleanup_group_call");
        // If others were still in the call, mark the channel as having an ongoing call
        // so the mode selector is locked when we view the idle screen.
        let had_others = self.group_call_members.len() > 1;
        if had_others {
            if let (Some(ch_id), Some(grp_idx)) = (self.group_call_channel_id.clone(), self.group_detail_idx) {
                if let Some(grp) = self.groups.get(grp_idx) {
                    self.group_call_ongoing.insert(ch_id, grp.call_mode);
                }
            }
        } else if let Some(ch_id) = &self.group_call_channel_id {
            self.group_call_ongoing.remove(ch_id);
        }

        self.group_call_running.store(false, Ordering::Relaxed);
        if let Some(ref h) = self.group_call_hangup {
            h.store(true, Ordering::Relaxed);
        }
        // Save chat history before clearing
        if let Some(ref h) = self.group_chat_history {
            h.save();
        }
        self.group_chat_history = None;
        self.group_call_hangup = None;
        self.group_call_chat_tx = None;
        self.group_call_chat_rx = None;
        self.group_call_roster_rx = None;
        self.group_call_chat_input.clear();
        self.group_call_messages.clear();
        self.group_call_members.clear();
        self.group_call_group = None;
        self.group_call_role = None;
        self.group_call_channel_id = None;
        self.group_screen_sharing = false;
        self.group_webcam_sharing = false;
        *self.group_connect_result.lock().unwrap() = None;
        // Stay on group detail view (not back to list)
        self.group_view = GroupView::Detail;

        // Reclaim socket
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
                        self.viewer_joined = Some(info.viewer_joined);
                        self.auto_banned_ips = Some(info.auto_banned_ips);
                        self.chat_history = Some(ChatHistory::load(
                            &info.contact_id,
                            &self.identity.secret,
                        ));
                        if info.key_change_warning.is_some() {
                            self.screen = Screen::KeyWarning;
                        } else {
                            log_fmt!("[gui] call connected");
                            self.screen = Screen::InCall;
                        }
                    }
                    Err(e) => {
                        self.running.store(false, Ordering::Relaxed);
                        if e == "Cancelled" {
                            self.screen = Screen::Setup;
                        } else {
                            log_fmt!("[gui] call error: {}", e);
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

        // Check for group call connecting result
        if self.group_call_channel_id.is_some() && self.group_call_chat_rx.is_none() {
            let grp_result = self.group_connect_result.lock().unwrap().take();
            if let Some(res) = grp_result {
                match res {
                    Ok(info) => {
                        self.group_call_group = Some(info.group);
                        self.group_call_role = Some(info.role);
                        self.group_call_hangup = Some(info.local_hangup);
                        self.group_call_chat_tx = Some(info.chat_tx);
                        self.group_call_chat_rx = Some(info.chat_rx);
                        self.group_call_roster_rx = Some(info.roster_rx);
                        self.group_call_members = self.group_call_group.as_ref()
                            .and_then(|g| g.members.iter()
                                .find(|m| m.pubkey == self.identity.pubkey).cloned())
                            .into_iter().collect();
                        self.group_view = GroupView::InCall;
                    }
                    Err(e) => {
                        log_fmt!("[group] call failed: {}", e);
                        self.cleanup_group_call();
                    }
                }
            }
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }

        // Poll group call chat messages + roster updates + detect hangup
        if self.group_call_channel_id.is_some() && self.group_call_chat_rx.is_some() {
            if let Some(rx) = &self.group_call_chat_rx {
                while let Ok(msg) = rx.try_recv() {
                    // Derive fingerprint from local group roster (trusted data, not peer-supplied)
                    let sender_fp = self.group_call_group.as_ref()
                        .and_then(|g| g.members.iter().find(|m| m.sender_index == msg.sender_index))
                        .map(|m| m.fingerprint.clone())
                        .unwrap_or_default();
                    // Persist to group chat history
                    if let Some(ref mut hist) = self.group_chat_history {
                        hist.add_message(
                            sender_fp,
                            msg.sender_nickname.clone(),
                            msg.text.clone(),
                        );
                    }
                    self.group_call_messages.push(msg);
                }
            }
            if let Some(rx) = &self.group_call_roster_rx {
                while let Ok(roster) = rx.try_recv() {
                    self.group_call_members = roster;
                }
            }
            if !self.group_call_running.load(Ordering::Relaxed) {
                self.cleanup_group_call();
                return;
            }
            ctx.request_repaint_after(std::time::Duration::from_millis(200));
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
                    MsgEvent::AvatarReceived { contact_id } => {
                        // Invalidate cached texture so it reloads from disk next frame
                        self.contact_avatar_textures.remove(&contact_id);
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
                    MsgEvent::IncomingGroupInvite { from_nickname, from_contact_id, invite_json } => {
                        // Show popup for user to accept/reject
                        if let Ok(lite) = serde_json::from_slice::<crate::group::GroupInviteLite>(&invite_json) {
                            if !self.groups.iter().any(|g| g.group_id == lite.group_id) {
                                log_fmt!("[gui] group invite from {}: '{}' ({} members)",
                                    from_nickname, lite.name, lite.member_count);
                                send_desktop_notification(
                                    "hostelD — Group Invitation",
                                    &format!("{} invited you to '{}'", from_nickname, lite.name),
                                );
                                self.incoming_group_invite = Some(IncomingGroupInviteInfo {
                                    from_nickname,
                                    from_contact_id,
                                    group_name: lite.name.clone(),
                                    member_count: lite.member_count as usize,
                                    invite_lite: lite,
                                });
                            }
                        }
                    }
                    MsgEvent::GroupInviteRejected { contact_id, group_id } => {
                        // Peer rejected our group invite — remove them from the group
                        if let Some(grp) = self.groups.iter_mut().find(|g| g.group_id == group_id) {
                            if let Some(contact) = self.contacts.iter().find(|c| c.contact_id == contact_id) {
                                group::remove_member(grp, &contact.pubkey);
                            }
                        }
                    }
                    MsgEvent::GroupUpdated { group_json } => {
                        if let Ok(received_group) = serde_json::from_slice::<Group>(&group_json) {
                            let my_pubkey = self.identity.pubkey;
                            // Check if we were removed from the group
                            let still_member = received_group.members.iter().any(|m| m.pubkey == my_pubkey);
                            if !still_member {
                                // We were kicked — remove local group
                                if let Some(pos) = self.groups.iter().position(|g| g.group_id == received_group.group_id) {
                                    let gid = self.groups[pos].group_id.clone();
                                    group::delete_group(&gid);
                                    self.groups.remove(pos);
                                    if self.group_detail_idx == Some(pos) {
                                        self.group_detail_idx = None;
                                        self.group_view = GroupView::List;
                                    } else if let Some(active) = self.group_detail_idx {
                                        if active > pos {
                                            self.group_detail_idx = Some(active - 1);
                                        }
                                    }
                                    if self.group_settings_idx == Some(pos) {
                                        self.group_settings_idx = None;
                                        self.group_view = GroupView::List;
                                    }
                                }
                            } else {
                                // Update existing group
                                if let Some(grp) = self.groups.iter_mut().find(|g| g.group_id == received_group.group_id) {
                                    grp.name = received_group.name.clone();
                                    grp.members = received_group.members.clone();
                                    grp.avatar_sha256 = received_group.avatar_sha256;

                                    // Merge text channels ("fallbackfix" protocol)
                                    let local_channels = std::mem::take(&mut grp.text_channels);
                                    let mut merged: std::collections::HashMap<String, group::TextChannel> = std::collections::HashMap::new();

                                    // Start with all local channels
                                    for ch in &local_channels {
                                        merged.insert(ch.channel_id.clone(), ch.clone());
                                    }

                                    // Merge remote channels
                                    let identity_secret = self.identity.secret;
                                    for rch in &received_group.text_channels {
                                        if let Some(lch) = merged.get(&rch.channel_id) {
                                            if rch.deleted && !lch.deleted {
                                                // Remote deleted it — migrate messages to fallback
                                                let name = lch.name.clone();
                                                crate::chat::migrate_messages_to_fallback(
                                                    &grp.group_id, &rch.channel_id, &name, &identity_secret,
                                                );
                                                merged.insert(rch.channel_id.clone(), rch.clone());
                                            } else if !rch.deleted && lch.deleted {
                                                // Our delete stands — keep local
                                            } else if rch.deleted && lch.deleted {
                                                // Both deleted — keep latest by deleted_at
                                                let r_at = rch.deleted_at.unwrap_or(0);
                                                let l_at = lch.deleted_at.unwrap_or(0);
                                                if r_at > l_at {
                                                    merged.insert(rch.channel_id.clone(), rch.clone());
                                                }
                                            } else {
                                                // Both alive — accept remote metadata
                                                merged.insert(rch.channel_id.clone(), rch.clone());
                                            }
                                        } else {
                                            // New from remote
                                            merged.insert(rch.channel_id.clone(), rch.clone());
                                        }
                                    }

                                    // Ensure general + fallback always exist
                                    let now = std::time::SystemTime::now()
                                        .duration_since(std::time::UNIX_EPOCH)
                                        .unwrap_or_default()
                                        .as_secs();
                                    let thirty_days = 30 * 24 * 3600;

                                    // Purge old tombstones (>30 days)
                                    merged.retain(|_, ch| {
                                        if ch.deleted {
                                            ch.deleted_at.map_or(true, |t| now.saturating_sub(t) < thirty_days)
                                        } else {
                                            true
                                        }
                                    });

                                    grp.text_channels = merged.into_values().collect();
                                    group::ensure_general_channel(grp);
                                    group::ensure_fallback_channel(grp);

                                    // Check if merged result differs from received — if admin, re-broadcast
                                    let merged_differs = grp.text_channels.len() != received_group.text_channels.len()
                                        || grp.text_channels.iter().any(|ch| {
                                            !received_group.text_channels.iter().any(|rch| rch.channel_id == ch.channel_id && rch.deleted == ch.deleted)
                                        });

                                    group::save_group(grp);

                                    // If local user is admin and merged result differs, re-broadcast
                                    let my_pubkey = self.identity.pubkey;
                                    let is_admin = grp.members.iter().any(|m| m.pubkey == my_pubkey && m.is_admin);
                                    if is_admin && merged_differs {
                                        if let Some(tx) = &self.msg_cmd_tx {
                                            let group_json = serde_json::to_vec(grp).unwrap_or_default();
                                            let member_contacts: Vec<_> = grp.members.iter()
                                                .filter(|m| m.pubkey != my_pubkey && !m.address.is_empty() && !m.port.is_empty())
                                                .filter_map(|m| {
                                                    let addr_str = format!("[{}]:{}", m.address, m.port);
                                                    addr_str.parse().ok().map(|addr| {
                                                        let cid = crate::identity::derive_contact_id(&my_pubkey, &m.pubkey);
                                                        (cid, addr, m.pubkey)
                                                    })
                                                })
                                                .collect();
                                            tx.send(crate::messaging::MsgCommand::SendGroupUpdate {
                                                group_id: grp.group_id.clone(),
                                                group_json,
                                                member_contacts,
                                            }).ok();
                                        }
                                    }
                                }
                            }
                        }
                    }
                    MsgEvent::GroupMemberSynced { group_id, member } => {
                        // Add synced member to the group (from invite accept flow)
                        if let Some(grp) = self.groups.iter_mut().find(|g| g.group_id == group_id) {
                            // Don't add duplicates
                            if !grp.members.iter().any(|m| m.pubkey == member.pubkey) {
                                log_fmt!("[gui] member synced for group={}: {} (idx={})",
                                    group_id, member.nickname, member.sender_index);
                                grp.members.push(member);
                                group::save_group(grp);
                            }
                        }
                    }
                    MsgEvent::GroupAvatarReceived { group_id } => {
                        // Invalidate cached group avatar texture
                        self.group_avatar_textures.remove(&group_id);
                    }
                    MsgEvent::IncomingGroupChat { group_id, channel_id, sender_fingerprint, sender_nickname, text } => {
                        // If channel is deleted in our local group, redirect to fallback
                        let effective_channel = if let Some(grp) = self.groups.iter().find(|g| g.group_id == group_id) {
                            if grp.text_channels.iter().any(|ch| ch.channel_id == channel_id && !ch.deleted) {
                                channel_id.clone()
                            } else {
                                "fallback".to_string()
                            }
                        } else {
                            channel_id.clone()
                        };
                        // Save to group chat history
                        use crate::chat::GroupChatHistory;
                        let mut history = GroupChatHistory::load(&group_id, &effective_channel, &self.identity.secret);
                        history.add_message(sender_fingerprint, sender_nickname.clone(), text.clone());

                        // If currently viewing this group, update the live messages
                        if let Some(idx) = self.group_detail_idx {
                            if idx < self.groups.len() && self.groups[idx].group_id == group_id {
                                // Detail view reloads history from disk each frame
                            }
                        }
                        // If in a group call for this group, push to live messages
                        if let Some(ref g) = self.group_call_group {
                            if g.group_id == group_id && matches!(self.group_view, groups::GroupView::InCall) {
                                self.group_call_messages.push(crate::groupcall::GroupChatMsg {
                                    sender_index: 0,
                                    sender_nickname,
                                    text,
                                });
                            }
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

        // Force Call tab when 1:1 call is active (fullscreen UI)
        let in_call = matches!(self.screen, Screen::Connecting | Screen::KeyWarning | Screen::InCall | Screen::Error(_));
        let in_group_call = self.group_call_channel_id.is_some();
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
                self.draw_sidebar(ui, in_call, in_group_call);
            });

        // ── Central panel: dispatch by active tab ──
        egui::CentralPanel::default().show(ctx, |ui| {
            match self.active_tab {
                SidebarTab::Profile => self.draw_profile_tab(ui),
                SidebarTab::Friends => self.draw_friends_tab(ui),
                SidebarTab::Messages => self.draw_messages_tab(ui),
                SidebarTab::Groups => self.draw_groups_tab(ui),
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
                SidebarTab::Logs => self.draw_logs_tab(ui),
            }
        });

        // ── Incoming call popup (overlay on top of everything) ──
        if self.incoming_call.is_some() {
            self.draw_incoming_call_popup(ctx);
        }

        // ── Group invite popup ──
        if self.incoming_group_invite.is_some() {
            self.draw_incoming_group_invite_popup(ctx);
        }

        // ── Firewall prompt popup ──
        if self.show_firewall_prompt {
            self.draw_firewall_prompt(ctx);
        }

        // ── Crop editor popup ──
        if self.show_crop_editor {
            self.draw_crop_editor(ctx);
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

pub fn run() {
    // On Linux, install .desktop file and icon so the desktop environment
    // (GNOME, KDE, etc.) shows the app logo instead of a generic gear.
    #[cfg(target_os = "linux")]
    {
        let logo_bytes = include_bytes!("../../assets/logo.png");
        if let Some(data_home) = std::env::var_os("XDG_DATA_HOME")
            .map(std::path::PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".local/share")))
        {
            let icon_dir = data_home.join("icons/hicolor/512x512/apps");
            let icon_path = icon_dir.join("hostelD.png");
            if !icon_path.exists() {
                let _ = std::fs::create_dir_all(&icon_dir);
                let _ = std::fs::write(&icon_path, logo_bytes);
            }
            let desktop_dir = data_home.join("applications");
            let desktop_path = desktop_dir.join("hostelD.desktop");
            if !desktop_path.exists() {
                let _ = std::fs::create_dir_all(&desktop_dir);
                let desktop_entry = "[Desktop Entry]\n\
                    Type=Application\n\
                    Name=hostelD\n\
                    Comment=Secure P2P Voice + Chat + Screen\n\
                    Icon=hostelD\n\
                    Exec=hostelD\n\
                    Terminal=false\n\
                    Categories=Network;InstantMessaging;\n\
                    StartupWMClass=hostelD\n";
                let _ = std::fs::write(&desktop_path, desktop_entry);
            }
        }
    }

    // Window icon from cropped logo (cross-platform: Windows, Linux, macOS)
    let (rgba, w, h) = load_png_cropped(include_bytes!("../../assets/logo.png"));
    let icon = egui::IconData { rgba, width: w, height: h };

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([884.0, 750.0])
            .with_min_inner_size([484.0, 600.0])
            .with_title("hostelD — Secure P2P Voice + Chat + Screen")
            .with_icon(std::sync::Arc::new(icon))
            .with_drag_and_drop(true)
            .with_app_id("hostelD".to_string()),
        ..Default::default()
    };
    eframe::run_native(
        "hostelD",
        options,
        Box::new(|cc| Ok(Box::new(HostelApp::new(cc)))),
    ).expect("Failed to start GUI");
}
