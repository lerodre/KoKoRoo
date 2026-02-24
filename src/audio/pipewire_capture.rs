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
use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use libspa::param::format::{FormatProperties, MediaSubtype, MediaType};
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

    // Raw pointer to core for use in `'static` closures (required by PipeWire-rs).
    // SAFETY: `core` lives on the stack for the entire function. `mainloop.run()`
    // blocks until quit, and `_registry_listener` is declared after `core` so it
    // drops first (Rust drops locals in reverse declaration order).
    let core_ptr = &*core as *const pw::core::Core;

    // Shared state for callbacks — all types are `'static` so closures compile.
    let our_node_id: Rc<Cell<Option<u32>>> = Rc::new(Cell::new(None));
    let linked_nodes: Rc<RefCell<HashSet<u32>>> = Rc::new(RefCell::new(HashSet::new()));

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
    let node_id_for_stream = our_node_id.clone();
    let _stream_listener = stream
        .add_local_listener::<()>()
        .state_changed(move |stream, _data, _old, new| {
            if matches!(new, pw::stream::StreamState::Streaming) {
                let node_id = stream.node_id();
                log_fmt!("[sysaudio] PipeWire sink stream connected, node_id={}", node_id);
                node_id_for_stream.set(Some(node_id));
            }
        })
        .process(move |stream, _data| {
            if let Some(mut buffer) = stream.dequeue_buffer() {
                let datas = buffer.datas_mut();
                if datas.is_empty() {
                    return;
                }
                let data = &mut datas[0];
                // Read chunk size before taking mutable data slice
                let chunk_size = data.chunk().size() as usize;
                if let Some(slice) = data.data() {
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

    let node_id_for_reg = our_node_id.clone();
    let linked_for_add = linked_nodes.clone();
    let linked_for_remove = linked_nodes.clone();

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

            if node_pid == our_pid {
                log_fmt!(
                    "[sysaudio] skipping own audio node {} (app={}, PID={})",
                    global.id, app_name, node_pid
                );
                return;
            }

            let our_sink = match node_id_for_reg.get() {
                Some(id) => id,
                None => {
                    log_fmt!(
                        "[sysaudio] sink not ready yet, skipping node {} (app={})",
                        global.id, app_name
                    );
                    return;
                }
            };

            // Skip if already linked
            if !linked_for_add.borrow_mut().insert(global.id) {
                return;
            }

            // Create a link from this app's output to our capture sink
            log_fmt!(
                "[sysaudio] linking audio node {} (app={}, PID={}) → sink {}",
                global.id, app_name, node_pid, our_sink
            );

            // SAFETY: core_ptr is valid for the entire mainloop.run() duration.
            // See safety comment at the declaration site above.
            let core_ref = unsafe { &*core_ptr };
            let link_result = core_ref.create_object::<pw::link::Link>(
                "link-factory",
                &pipewire::properties::properties! {
                    "link.output.node" => global.id.to_string(),
                    "link.input.node" => our_sink.to_string(),
                    "object.linger" => "true",
                },
            );

            match link_result {
                Ok(_link) => {
                    // Link persists server-side (linger=true) until one endpoint
                    // disappears. The proxy can safely drop here.
                }
                Err(e) => {
                    log_fmt!("[sysaudio] failed to create link for node {}: {}", global.id, e);
                    linked_for_add.borrow_mut().remove(&global.id);
                }
            }
        })
        .global_remove(move |id| {
            // Clean up tracking when an audio node disappears
            if linked_for_remove.borrow_mut().remove(&id) {
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
