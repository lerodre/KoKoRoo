//! Linux PipeWire system audio capture with process exclusion.
//!
//! Captures all system audio EXCEPT hostelD's own playback, preventing
//! the peer's voice from being echoed back during screen sharing.
//!
//! Architecture:
//!   1. Create a virtual null-audio-sink ("capture mixer")
//!   2. Create a capture stream that AUTOCONNECTS to the mixer's monitor
//!   3. Monitor the registry for audio output nodes AND their ports
//!   4. Link non-hostelD output ports → mixer input ports (explicit port IDs)
//!   5. Process callback downmixes stereo f32 → mono → ring buffer

use ringbuf::traits::Producer;
use ringbuf::HeapProd;
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use libspa::param::format::{FormatProperties, MediaSubtype, MediaType};
use pipewire as pw;
use pw::stream::StreamFlags;
use pw::types::ObjectType;

/// Stream handle for PipeWire system audio capture.
pub struct PipewireCapture {
    active: Arc<AtomicBool>,
    quit_flag: Arc<AtomicBool>,
    capture_thread: Option<thread::JoinHandle<()>>,
    producer: Arc<Mutex<Option<HeapProd<f32>>>>,
}

impl PipewireCapture {
    pub fn take_producer(&self) -> Option<HeapProd<f32>> {
        self.producer.lock().ok()?.take()
    }
}

impl Drop for PipewireCapture {
    fn drop(&mut self) {
        self.active.store(false, Ordering::Relaxed);
        self.quit_flag.store(true, Ordering::Relaxed);
        if let Some(t) = self.capture_thread.take() {
            let _ = t.join();
        }
    }
}

pub fn start_capture(
    producer: HeapProd<f32>,
    active: Arc<AtomicBool>,
) -> (Option<PipewireCapture>, Option<HeapProd<f32>>) {
    let shared_producer = Arc::new(Mutex::new(Some(producer)));
    let thread_producer = shared_producer.clone();
    let thread_active = active.clone();
    let quit_flag = Arc::new(AtomicBool::new(false));
    let thread_quit = quit_flag.clone();

    let handle = match thread::Builder::new()
        .name("pw-audio-capture".into())
        .spawn(move || {
            if let Err(e) = capture_thread(thread_producer, thread_active, thread_quit) {
                log_fmt!("[sysaudio] PipeWire capture error: {}", e);
            }
        }) {
        Ok(h) => h,
        Err(_) => {
            let leftover = shared_producer.lock().ok().and_then(|mut g| g.take());
            return (None, leftover);
        }
    };

    log_fmt!("[sysaudio] PipeWire process-excluded audio capture started");
    (
        Some(PipewireCapture {
            active,
            quit_flag,
            capture_thread: Some(handle),
            producer: shared_producer,
        }),
        None,
    )
}

const MIXER_NODE_NAME: &str = "hostelD-capture-mixer";

/// Port info tracked from the registry.
#[derive(Clone, Debug)]
struct PortInfo {
    global_id: u32,
    node_id: u32,
    direction: String, // "out" or "in"
    channel: String,   // "FL", "FR", "MONO", etc.
}

/// Try to create links between a source node's output ports and the mixer's
/// input ports. Uses explicit port IDs for reliable linking.
fn try_link_node_to_mixer(
    core_ptr: *const pw::core::Core,
    source_node_id: u32,
    mixer_node_id: u32,
    ports: &HashMap<u32, PortInfo>,
    linked_ports: &mut HashSet<u32>,
) -> u32 {
    // Collect output ports of source node
    let out_ports: Vec<&PortInfo> = ports
        .values()
        .filter(|p| p.node_id == source_node_id && p.direction == "out")
        .collect();

    // Collect input ports of mixer node
    let in_ports: Vec<&PortInfo> = ports
        .values()
        .filter(|p| p.node_id == mixer_node_id && p.direction == "in")
        .collect();

    let mut created = 0u32;
    let core_ref = unsafe { &*core_ptr };

    // Match by channel name (FL→FL, FR→FR) or by index if names don't match
    for out_port in &out_ports {
        if linked_ports.contains(&out_port.global_id) {
            continue;
        }

        // Find matching input port by channel
        let matching_in = in_ports
            .iter()
            .find(|p| p.channel == out_port.channel)
            .or_else(|| {
                // Fallback: match by position (first out → first in, etc.)
                let out_idx = out_ports
                    .iter()
                    .position(|p| p.global_id == out_port.global_id)
                    .unwrap_or(0);
                in_ports.get(out_idx)
            })
            .copied();

        if let Some(in_port) = matching_in {
            let result = core_ref.create_object::<pw::link::Link>(
                "link-factory",
                &pipewire::properties::properties! {
                    "link.output.node" => source_node_id.to_string(),
                    "link.output.port" => out_port.global_id.to_string(),
                    "link.input.node" => mixer_node_id.to_string(),
                    "link.input.port" => in_port.global_id.to_string(),
                    "object.linger" => "true",
                },
            );
            match result {
                Ok(_) => {
                    linked_ports.insert(out_port.global_id);
                    created += 1;
                }
                Err(e) => {
                    log_fmt!(
                        "[sysaudio] link failed: out port {} → in port {}: {}",
                        out_port.global_id, in_port.global_id, e
                    );
                }
            }
        }
    }
    created
}

fn capture_thread(
    producer: Arc<Mutex<Option<HeapProd<f32>>>>,
    active: Arc<AtomicBool>,
    quit_flag: Arc<AtomicBool>,
) -> Result<(), String> {
    pw::init();

    let mainloop = pw::main_loop::MainLoopRc::new(None)
        .map_err(|e| format!("create MainLoop: {e}"))?;

    let context = pw::context::ContextBox::new(&mainloop.loop_(), None)
        .map_err(|e| format!("create Context: {e}"))?;

    let core = context
        .connect(None)
        .map_err(|e| format!("connect to PipeWire: {e}"))?;

    let our_pid = std::process::id();
    log_fmt!("[sysaudio] PipeWire connected, our PID={}", our_pid);

    let core_ptr = &*core as *const pw::core::Core;

    // ── Step 1: Create virtual null sink ──
    let _null_sink = core
        .create_object::<pw::node::Node>(
            "adapter",
            &pipewire::properties::properties! {
                "factory.name" => "support.null-audio-sink",
                "node.name" => MIXER_NODE_NAME,
                "node.description" => "hostelD Capture Mixer",
                "media.class" => "Audio/Sink",
                "audio.channels" => "2",
                "audio.rate" => "48000",
                "object.linger" => "false",
            },
        )
        .map_err(|e| format!("create null sink: {e}"))?;

    log_fmt!("[sysaudio] null sink '{}' created", MIXER_NODE_NAME);

    // ── Step 2: Create capture stream targeting the mixer ──
    let stream = pw::stream::StreamBox::new(
        &core,
        "hostelD-audio-capture",
        pipewire::properties::properties! {
            *pw::keys::MEDIA_TYPE => "Audio",
            *pw::keys::MEDIA_CATEGORY => "Capture",
            *pw::keys::MEDIA_ROLE => "Communication",
            *pw::keys::NODE_NAME => "hostelD-system-capture",
            "media.class" => "Stream/Input/Audio",
            "stream.capture.sink" => "true",
            "node.target" => MIXER_NODE_NAME,
        },
    )
    .map_err(|e| format!("create Stream: {e}"))?;

    let process_count = Rc::new(Cell::new(0u64));
    let _stream_listener = stream
        .add_local_listener::<()>()
        .state_changed(move |_stream, _data, _old, new| {
            log_fmt!("[sysaudio] stream state: {:?} -> {:?}", _old, new);
        })
        .process({
            let process_count = process_count.clone();
            move |stream, _data| {
                if let Some(mut buffer) = stream.dequeue_buffer() {
                    let datas = buffer.datas_mut();
                    if datas.is_empty() {
                        return;
                    }
                    let data = &mut datas[0];
                    let chunk_size = data.chunk().size() as usize;
                    if let Some(slice) = data.data() {
                        let available = chunk_size.min(slice.len());
                        let count = process_count.get();
                        if count < 5 || (count % 2000 == 0) {
                            log_fmt!(
                                "[sysaudio] process cb #{}: chunk={} avail={}",
                                count, chunk_size, available
                            );
                        }
                        process_count.set(count + 1);
                        if let Ok(mut guard) = producer.lock() {
                            if let Some(ref mut prod) = *guard {
                                let frame_bytes = 8;
                                for frame in slice[..available].chunks_exact(frame_bytes) {
                                    let l = f32::from_le_bytes([
                                        frame[0], frame[1], frame[2], frame[3],
                                    ]);
                                    let r = f32::from_le_bytes([
                                        frame[4], frame[5], frame[6], frame[7],
                                    ]);
                                    let _ = prod.try_push((l + r) * 0.5);
                                }
                            }
                        }
                    }
                }
            }
        })
        .register()
        .map_err(|e| format!("register stream listener: {e}"))?;

    let param_bytes = build_audio_format_params();
    let pod = libspa::pod::Pod::from_bytes(&param_bytes)
        .ok_or_else(|| "invalid audio format pod".to_string())?;
    let mut params = vec![pod];

    stream
        .connect(
            libspa::utils::Direction::Input,
            None,
            StreamFlags::AUTOCONNECT | StreamFlags::MAP_BUFFERS | StreamFlags::RT_PROCESS,
            &mut params,
        )
        .map_err(|e| format!("connect stream: {e}"))?;

    // ── Step 3: Registry — track nodes, ports, and create links ──
    let registry = core
        .get_registry()
        .map_err(|e| format!("get registry: {e}"))?;

    // Shared state
    let mixer_node_id: Rc<Cell<Option<u32>>> = Rc::new(Cell::new(None));
    // Nodes eligible for linking: node_id → (app_name, pid)
    let eligible_nodes: Rc<RefCell<HashMap<u32, (String, u32)>>> =
        Rc::new(RefCell::new(HashMap::new()));
    // All known ports: global_id → PortInfo
    let all_ports: Rc<RefCell<HashMap<u32, PortInfo>>> =
        Rc::new(RefCell::new(HashMap::new()));
    // Ports we've already linked
    let linked_ports: Rc<RefCell<HashSet<u32>>> =
        Rc::new(RefCell::new(HashSet::new()));
    // Nodes we've fully linked (both channels)
    let linked_nodes: Rc<RefCell<HashSet<u32>>> =
        Rc::new(RefCell::new(HashSet::new()));

    let mixer_id_c = mixer_node_id.clone();
    let eligible_c = eligible_nodes.clone();
    let ports_c = all_ports.clone();
    let lports_c = linked_ports.clone();
    let lnodes_c = linked_nodes.clone();
    let lnodes_rm = linked_nodes.clone();
    let ports_rm = all_ports.clone();

    let _registry_listener = registry
        .add_listener_local()
        .global(move |global| {
            let props = match &global.props {
                Some(p) => p,
                None => return,
            };
            let props = props.as_ref();

            match global.type_ {
                ObjectType::Node => {
                    // Check if this is our mixer
                    if let Some(name) = props.get("node.name") {
                        if name == MIXER_NODE_NAME {
                            log_fmt!("[sysaudio] mixer node_id={}", global.id);
                            mixer_id_c.set(Some(global.id));
                            // Try linking any eligible nodes that were discovered before mixer
                            let eligible = eligible_c.borrow();
                            let ports = ports_c.borrow();
                            let mut lp = lports_c.borrow_mut();
                            let mut ln = lnodes_c.borrow_mut();
                            for (&nid, (app, _pid)) in eligible.iter() {
                                if ln.contains(&nid) {
                                    continue;
                                }
                                let n = try_link_node_to_mixer(
                                    core_ptr, nid, global.id, &ports, &mut lp,
                                );
                                if n > 0 {
                                    log_fmt!(
                                        "[sysaudio] linked {} ports for '{}' (node {}) → mixer",
                                        n, app, nid
                                    );
                                    ln.insert(nid);
                                }
                            }
                            return;
                        }
                    }

                    // Check for audio output streams
                    let media_class = match props.get("media.class") {
                        Some(c) => c,
                        None => return,
                    };
                    if media_class != "Stream/Output/Audio" {
                        return;
                    }

                    // PID check — try multiple property names
                    let pid_str = props
                        .get("pipewire.sec.pid")
                        .or_else(|| props.get("application.process.id"))
                        .unwrap_or("");
                    let node_pid: u32 = pid_str.parse().unwrap_or(0);
                    let app_name = props
                        .get("application.name")
                        .unwrap_or("unknown");

                    // Also check node.name for hostelD
                    let node_name = props.get("node.name").unwrap_or("");
                    let is_self = node_pid == our_pid
                        || (node_pid == 0 && node_name.contains("hostelD"));

                    if is_self {
                        log_fmt!(
                            "[sysaudio] SKIP own node {} (app={}, PID={}, name={})",
                            global.id, app_name, node_pid, node_name
                        );
                        return;
                    }

                    log_fmt!(
                        "[sysaudio] eligible node {} (app={}, PID={})",
                        global.id, app_name, node_pid
                    );
                    eligible_c
                        .borrow_mut()
                        .insert(global.id, (app_name.to_string(), node_pid));

                    // Try linking immediately if mixer and ports are ready
                    if let Some(mixer) = mixer_id_c.get() {
                        let ports = ports_c.borrow();
                        let mut lp = lports_c.borrow_mut();
                        let mut ln = lnodes_c.borrow_mut();
                        if !ln.contains(&global.id) {
                            let n = try_link_node_to_mixer(
                                core_ptr, global.id, mixer, &ports, &mut lp,
                            );
                            if n > 0 {
                                log_fmt!(
                                    "[sysaudio] linked {} ports for '{}' (node {}) → mixer",
                                    n, app_name, global.id
                                );
                                ln.insert(global.id);
                            }
                        }
                    }
                }

                ObjectType::Port => {
                    // Track port info for linking
                    let node_id_str = match props.get("node.id") {
                        Some(s) => s,
                        None => return,
                    };
                    let node_id: u32 = match node_id_str.parse() {
                        Ok(v) => v,
                        Err(_) => return,
                    };
                    let direction = props.get("port.direction").unwrap_or("").to_string();
                    let channel = props
                        .get("audio.channel")
                        .unwrap_or(
                            props.get("port.name").unwrap_or("unknown"),
                        )
                        .to_string();

                    let port_info = PortInfo {
                        global_id: global.id,
                        node_id,
                        direction: direction.clone(),
                        channel: channel.clone(),
                    };

                    ports_c.borrow_mut().insert(global.id, port_info);

                    // When a new output port appears for an eligible node, try linking
                    if direction == "out" {
                        let eligible = eligible_c.borrow();
                        if eligible.contains_key(&node_id) {
                            if let Some(mixer) = mixer_id_c.get() {
                                let ports = ports_c.borrow();
                                let mut lp = lports_c.borrow_mut();
                                let mut ln = lnodes_c.borrow_mut();
                                if !ln.contains(&node_id) {
                                    let n = try_link_node_to_mixer(
                                        core_ptr, node_id, mixer, &ports, &mut lp,
                                    );
                                    if n > 0 {
                                        let app = &eligible[&node_id].0;
                                        log_fmt!(
                                            "[sysaudio] linked {} ports for '{}' (node {}) → mixer",
                                            n, app, node_id
                                        );
                                        ln.insert(node_id);
                                    }
                                }
                            }
                        }
                    }

                    // When a new input port appears for the mixer, try linking pending nodes
                    if direction == "in" {
                        if let Some(mixer) = mixer_id_c.get() {
                            if node_id == mixer {
                                let eligible = eligible_c.borrow();
                                let ports = ports_c.borrow();
                                let mut lp = lports_c.borrow_mut();
                                let mut ln = lnodes_c.borrow_mut();
                                for (&nid, (app, _)) in eligible.iter() {
                                    if ln.contains(&nid) {
                                        continue;
                                    }
                                    let n = try_link_node_to_mixer(
                                        core_ptr, nid, mixer, &ports, &mut lp,
                                    );
                                    if n > 0 {
                                        log_fmt!(
                                            "[sysaudio] linked {} ports for '{}' (node {}) → mixer",
                                            n, app, nid
                                        );
                                        ln.insert(nid);
                                    }
                                }
                            }
                        }
                    }
                }

                _ => {}
            }
        })
        .global_remove(move |id| {
            if lnodes_rm.borrow_mut().remove(&id) {
                log_fmt!("[sysaudio] node {} removed", id);
            }
            ports_rm.borrow_mut().remove(&id);
        })
        .register();

    // ── Timer to check quit flags ──
    let mainloop_weak = mainloop.downgrade();
    let _timer = mainloop.loop_().add_timer(move |_| {
        if quit_flag.load(Ordering::Relaxed) || !active.load(Ordering::Relaxed) {
            if let Some(ml) = mainloop_weak.upgrade() {
                ml.quit();
            }
        }
    });
    _timer.update_timer(
        Some(Duration::from_millis(100)),
        Some(Duration::from_millis(100)),
    );

    log_fmt!("[sysaudio] PipeWire mainloop running");
    mainloop.run();
    log_fmt!("[sysaudio] PipeWire mainloop exited");

    Ok(())
}

fn build_audio_format_params() -> Vec<u8> {
    use libspa::pod::serialize::PodSerializer;
    use libspa::pod::Value;

    let obj = libspa::pod::object!(
        libspa::utils::SpaTypes::ObjectParamFormat,
        libspa::param::ParamType::EnumFormat,
        libspa::pod::property!(FormatProperties::MediaType, Id, MediaType::Audio),
        libspa::pod::property!(FormatProperties::MediaSubtype, Id, MediaSubtype::Raw),
        libspa::pod::property!(
            FormatProperties::AudioFormat,
            Choice, Enum, Id,
            libspa::param::audio::AudioFormat::F32LE,
            libspa::param::audio::AudioFormat::F32LE
        ),
        libspa::pod::property!(FormatProperties::AudioRate, Int, 48000),
        libspa::pod::property!(FormatProperties::AudioChannels, Int, 2),
    );

    PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &Value::Object(obj),
    )
    .expect("Failed to serialize audio format params")
    .0
    .into_inner()
}
