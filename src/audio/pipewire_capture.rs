//! Linux PipeWire selective system audio capture.
//!
//! Creates a virtual audio sink in the PipeWire graph and selectively links
//! all audio output nodes EXCEPT hostelD's own, preventing voice echo loops
//! when sharing system audio during calls.
//!
//! Architecture:
//!   1. Create a PipeWire stream acting as an internal audio sink
//!   2. Monitor the registry for Stream/Output/Audio nodes
//!   3. Auto-link non-self nodes to our sink (by comparing PIDs)
//!   4. Process callback pushes captured audio into the ring buffer

use ringbuf::traits::Producer;
use ringbuf::HeapProd;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use libspa::param::format::{FormatProperties, MediaSubtype, MediaType};
use libspa::param::format_utils::parse_format;
use pipewire as pw;
use pw::stream::StreamFlags;
use pw::types::ObjectType;

/// Stream handle for PipeWire selective system audio capture.
///
/// Captures all system audio except the current process, using a virtual
/// sink and selective link management in the PipeWire graph.
pub struct PipewireCapture {
    active: Arc<AtomicBool>,
    quit_flag: Arc<AtomicBool>,
    capture_thread: Option<thread::JoinHandle<()>>,
    producer: Arc<Mutex<Option<HeapProd<f32>>>>,
}

impl PipewireCapture {
    /// Recover the ring buffer producer for reuse after stopping capture.
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

/// Start capturing system audio while excluding the current process.
///
/// Returns `(Some(capture), None)` on success.
/// Returns `(None, Some(producer))` if PipeWire is unavailable, giving back
/// the producer so the caller can retry with a fallback method.
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

/// State shared between registry listener callbacks and the main loop.
struct GraphState {
    /// Our own PID — nodes from this process are excluded.
    our_pid: u32,
    /// Our sink stream's node ID (set after stream connects).
    our_node_id: Option<u32>,
    /// Active links: audio_node_id → link proxy (kept alive to maintain the link).
    links: HashMap<u32, pw::proxy::ProxyListener>,
}

/// Main PipeWire thread: creates the graph, monitors nodes, captures audio.
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

    // Shared graph state for registry callbacks
    let state = Rc::new(RefCell::new(GraphState {
        our_pid,
        our_node_id: None,
        links: HashMap::new(),
    }));

    // ── Create our capture stream (acts as an internal audio sink) ──
    let stream = pw::stream::StreamBox::new(
        &core,
        "hostelD-audio-capture",
        pipewire::properties::properties! {
            *pw::keys::MEDIA_TYPE => "Audio",
            *pw::keys::MEDIA_CATEGORY => "Capture",
            *pw::keys::MEDIA_ROLE => "Communication",
            *pw::keys::NODE_NAME => "hostelD-system-capture",
            "media.class" => "Audio/Sink/Internal",
        },
    )
    .map_err(|e| format!("create Stream: {e}"))?;

    // Stream state listener: grab our node ID once connected
    let state_for_stream = state.clone();
    let _stream_listener = stream
        .add_local_listener::<()>()
        .state_changed(move |stream, _data, _old, new| {
            if matches!(new, pw::stream::StreamState::Streaming) {
                let node_id = stream.node_id();
                log_fmt!("[sysaudio] PipeWire sink stream connected, node_id={}", node_id);
                state_for_stream.borrow_mut().our_node_id = Some(node_id);
            }
        })
        .process(move |stream, _data| {
            if let Some(mut buffer) = stream.dequeue_buffer() {
                let datas = buffer.datas_mut();
                if datas.is_empty() {
                    return;
                }
                let data = &datas[0];
                if let Some(slice) = data.data() {
                    let chunk_size = data.chunk().size() as usize;
                    let available = chunk_size.min(slice.len());
                    if let Ok(mut guard) = producer.lock() {
                        if let Some(ref mut prod) = *guard {
                            // Audio format: f32 stereo interleaved → mono downmix
                            // Each frame = 2 channels × 4 bytes = 8 bytes
                            let frame_bytes = 8;
                            for frame in slice[..available].chunks_exact(frame_bytes) {
                                let l = f32::from_le_bytes([frame[0], frame[1], frame[2], frame[3]]);
                                let r = f32::from_le_bytes([frame[4], frame[5], frame[6], frame[7]]);
                                let _ = prod.try_push((l + r) * 0.5);
                            }
                        }
                    }
                }
            }
        })
        .register()
        .map_err(|e| format!("register stream listener: {e}"))?;

    // Build audio format parameters
    let param_bytes = build_audio_format_params();
    let pod = libspa::pod::Pod::from_bytes(&param_bytes)
        .ok_or_else(|| "invalid audio format pod".to_string())?;
    let mut params = vec![pod];

    // Do NOT use AUTOCONNECT — we manage links manually via the registry
    // listener. AUTOCONNECT would let the session manager connect us to
    // the default audio source (microphone), causing mic audio to leak in.
    stream
        .connect(
            libspa::utils::Direction::Input,
            None,
            StreamFlags::MAP_BUFFERS | StreamFlags::RT_PROCESS,
            &mut params,
        )
        .map_err(|e| format!("connect stream: {e}"))?;

    // ── Monitor registry for audio output nodes ──
    let registry = core
        .get_registry()
        .map_err(|e| format!("get registry: {e}"))?;

    let state_for_add = state.clone();
    let core_for_links = Rc::new(core);
    let core_for_add = core_for_links.clone();

    let _registry_listener = registry
        .add_listener_local()
        .global(move |global| {
            // Only care about Node objects
            if global.type_ != ObjectType::Node {
                return;
            }
            let props = match &global.props {
                Some(p) => p,
                None => return,
            };
            let props = props.as_ref();

            // Only care about audio output streams (apps playing audio)
            let media_class = match props.get("media.class") {
                Some(c) => c,
                None => return,
            };
            if media_class != "Stream/Output/Audio" {
                return;
            }

            // Check PID to exclude our own app's audio
            let pid_str = props.get("application.process.id").unwrap_or("");
            let node_pid: u32 = pid_str.parse().unwrap_or(0);
            let app_name = props.get("application.name").unwrap_or("unknown");

            let s = state_for_add.borrow();
            if node_pid == s.our_pid {
                log_fmt!(
                    "[sysaudio] skipping own audio node {} (app={}, PID={})",
                    global.id, app_name, node_pid
                );
                return;
            }

            let our_sink = match s.our_node_id {
                Some(id) => id,
                None => {
                    log_fmt!(
                        "[sysaudio] sink not ready yet, skipping node {} (app={})",
                        global.id, app_name
                    );
                    return;
                }
            };
            drop(s); // release borrow before mutable borrow

            // Create a link from this app's output to our capture sink
            log_fmt!(
                "[sysaudio] linking audio node {} (app={}, PID={}) → sink {}",
                global.id, app_name, node_pid, our_sink
            );

            let link_result = core_for_add.create_object::<pw::link::Link, _>(
                "link-factory",
                &pipewire::properties::properties! {
                    "link.output.node" => global.id.to_string(),
                    "link.input.node" => our_sink.to_string(),
                    "object.linger" => "false",
                },
            );

            match link_result {
                Ok(link) => {
                    // Keep the link's listener alive to maintain the connection.
                    // When the listener is dropped, PipeWire cleans up the link.
                    let listener = link
                        .add_listener_local()
                        .removed(|| {})
                        .register();
                    state_for_add.borrow_mut().links.insert(global.id, listener);
                }
                Err(e) => {
                    log_fmt!("[sysaudio] failed to create link for node {}: {}", global.id, e);
                }
            }
        })
        .global_remove(move |id| {
            // Clean up link when an audio node disappears
            if state.borrow_mut().links.remove(&id).is_some() {
                log_fmt!("[sysaudio] audio node {} removed, link cleaned up", id);
            }
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

/// Build SPA audio format parameters for our capture stream.
/// Requests f32 stereo at 48kHz.
fn build_audio_format_params() -> Vec<u8> {
    use libspa::pod::serialize::PodSerializer;
    use libspa::pod::Value;

    let obj = libspa::pod::object!(
        libspa::utils::SpaTypes::ObjectParamFormat,
        libspa::param::ParamType::EnumFormat,
        libspa::pod::property!(
            FormatProperties::MediaType,
            Id,
            MediaType::Audio
        ),
        libspa::pod::property!(
            FormatProperties::MediaSubtype,
            Id,
            MediaSubtype::Raw
        ),
        libspa::pod::property!(
            FormatProperties::AudioFormat,
            Choice,
            Enum,
            Id,
            libspa::param::audio::AudioFormat::F32LE,
            libspa::param::audio::AudioFormat::F32LE
        ),
        libspa::pod::property!(
            FormatProperties::AudioRate,
            Int,
            48000
        ),
        libspa::pod::property!(
            FormatProperties::AudioChannels,
            Int,
            2
        ),
    );

    PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &Value::Object(obj),
    )
    .expect("Failed to serialize audio format params")
    .0
    .into_inner()
}
