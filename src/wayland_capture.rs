//! Wayland screen capture via XDG Desktop Portal + PipeWire.
//! This module is Linux-only (#[cfg(target_os = "linux")] in main.rs).

use std::cell::Cell;
use std::os::unix::io::OwnedFd;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ashpd::desktop::screencast::{CursorMode, Screencast, SourceType};
use ashpd::desktop::PersistMode;
use libspa::param::format::{FormatProperties, MediaSubtype, MediaType};
use pipewire as pw;
use pipewire::stream::StreamFlags;

/// Check if we're running on Wayland.
pub fn is_wayland() -> bool {
    std::env::var("WAYLAND_DISPLAY").is_ok()
        || std::env::var("XDG_SESSION_TYPE")
            .map(|v| v == "wayland")
            .unwrap_or(false)
}

/// Result of a successful portal negotiation.
pub struct PortalSession {
    pub pw_fd: OwnedFd,
    pub node_id: u32,
    pub width: u32,
    pub height: u32,
}

// OwnedFd is Send, so PortalSession is Send
unsafe impl Send for PortalSession {}

/// Request screen capture permission via XDG Desktop Portal.
/// This BLOCKS the calling thread while showing the OS permission dialog.
/// Returns None if user cancels or portal is unavailable.
pub fn request_screencast() -> Option<PortalSession> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .ok()?;

    rt.block_on(async {
        let proxy = Screencast::new().await.ok()?;
        let session = proxy.create_session().await.ok()?;
        proxy
            .select_sources(
                &session,
                CursorMode::Embedded,
                SourceType::Monitor.into(),
                false,
                None,
                PersistMode::DoNot,
            )
            .await
            .ok()?;

        let response = proxy.start(&session, None).await.ok()?.response().ok()?;
        let streams = response.streams();
        let stream = streams.first()?;
        let node_id = stream.pipe_wire_node_id();
        let (w, h) = stream.size().unwrap_or((1920, 1080));

        let pw_fd = proxy.open_pipe_wire_remote(&session).await.ok()?;
        Some(PortalSession {
            pw_fd,
            node_id,
            width: w as u32,
            height: h as u32,
        })
    })
}

/// Build SPA video format parameters requesting BGRx pixel format.
/// Uses the object!/property! macros from libspa for correct pod construction.
fn build_video_format_params(width: u32, height: u32) -> Vec<Vec<u8>> {
    use libspa::pod::serialize::PodSerializer;
    use libspa::pod::{object, property, Value};

    // SPA video format constants (from spa/param/video/format.h)
    // BGRx = 8, RGBx = 5
    const SPA_VIDEO_FORMAT_BGRX: u32 = 8;
    const SPA_VIDEO_FORMAT_RGBX: u32 = 5;

    // Use raw Id values for formats since they're not enums in the spa bindings
    #[derive(Clone, Copy)]
    struct RawId(u32);
    impl RawId {
        fn as_raw(self) -> u32 {
            self.0
        }
    }

    let obj = Value::Object(object! {
        libspa::utils::SpaTypes::ObjectParamFormat,
        libspa::param::ParamType::EnumFormat,
        property!(
            FormatProperties::MediaType,
            Id,
            MediaType::Video
        ),
        property!(
            FormatProperties::MediaSubtype,
            Id,
            MediaSubtype::Raw
        ),
        // Video format: BGRx preferred, RGBx as alternative
        property!(
            FormatProperties::VideoFormat,
            Choice,
            Enum,
            Id,
            RawId(SPA_VIDEO_FORMAT_BGRX),
            RawId(SPA_VIDEO_FORMAT_BGRX),
            RawId(SPA_VIDEO_FORMAT_RGBX)
        ),
        // Size: range from 1x1 to 7680x4320 with default at source size
        property!(
            FormatProperties::VideoSize,
            Choice,
            Range,
            Rectangle,
            libspa::utils::Rectangle { width, height },
            libspa::utils::Rectangle { width: 1, height: 1 },
            libspa::utils::Rectangle { width: 7680, height: 4320 }
        ),
        // Framerate: range from 1/1 to 120/1 with default 30/1
        property!(
            FormatProperties::VideoFramerate,
            Choice,
            Range,
            Fraction,
            libspa::utils::Fraction { num: 30, denom: 1 },
            libspa::utils::Fraction { num: 1, denom: 1 },
            libspa::utils::Fraction { num: 120, denom: 1 }
        ),
    });

    // Serialize to bytes
    let (bytes, _) = PodSerializer::serialize(std::io::Cursor::new(Vec::new()), &obj)
        .expect("Failed to serialize video format params");
    vec![bytes.into_inner()]
}

/// Run PipeWire MainLoop on a dedicated thread.
/// Stores latest captured frame in `frame_buf` for the encoder thread to read.
pub fn run_pipewire_capture(
    portal: PortalSession,
    frame_buf: Arc<Mutex<Option<(Vec<u8>, u32, u32)>>>,
    active: Arc<AtomicBool>,
    running: Arc<AtomicBool>,
) {
    pw::init();
    let mainloop = match pw::main_loop::MainLoopRc::new(None) {
        Ok(ml) => ml,
        Err(e) => {
            log_fmt!("[wayland] Failed to create PipeWire MainLoop: {e}");
            active.store(false, Ordering::Relaxed);
            return;
        }
    };
    let context = match pw::context::ContextBox::new(&mainloop.loop_(), None) {
        Ok(ctx) => ctx,
        Err(e) => {
            log_fmt!("[wayland] Failed to create PipeWire Context: {e}");
            active.store(false, Ordering::Relaxed);
            return;
        }
    };
    let core = match context.connect_fd(portal.pw_fd, None) {
        Ok(c) => c,
        Err(e) => {
            log_fmt!("[wayland] Failed to connect PipeWire fd: {e}");
            active.store(false, Ordering::Relaxed);
            return;
        }
    };

    let stream = match pw::stream::StreamBox::new(
        &core,
        "hostelD-screen",
        pipewire::properties::properties! {
            *pw::keys::MEDIA_TYPE => "Video",
            *pw::keys::MEDIA_CATEGORY => "Capture",
            *pw::keys::MEDIA_ROLE => "Screen",
        },
    ) {
        Ok(s) => s,
        Err(e) => {
            log_fmt!("[wayland] Failed to create PipeWire Stream: {e}");
            active.store(false, Ordering::Relaxed);
            return;
        }
    };

    // Timer to check active/running flags and quit MainLoop
    let mainloop_weak = mainloop.downgrade();
    let active_timer = active.clone();
    let running_timer = running.clone();
    let timer = mainloop.loop_().add_timer(move |_| {
        if !active_timer.load(Ordering::Relaxed) || !running_timer.load(Ordering::Relaxed) {
            if let Some(ml) = mainloop_weak.upgrade() {
                ml.quit();
            }
        }
    });
    timer.update_timer(
        Some(Duration::from_millis(100)),
        Some(Duration::from_millis(100)),
    );

    // Track negotiated video dimensions
    let dims: Rc<Cell<(u32, u32)>> = Rc::new(Cell::new((portal.width, portal.height)));
    let dims_changed = dims.clone();
    let frame_buf_process = frame_buf.clone();

    let _listener = stream
        .add_local_listener::<()>()
        .state_changed(|_stream, _data, old, new| {
            log_fmt!("[wayland] stream state: {:?} -> {:?}", old, new);
        })
        .param_changed(move |_stream, _data, id, pod| {
            if id != libspa::param::ParamType::Format.as_raw() {
                return;
            }
            if let Some(pod) = pod {
                if let Ok((_, value)) =
                    libspa::pod::deserialize::PodDeserializer::deserialize_any_from(pod.as_bytes())
                {
                    if let libspa::pod::Value::Object(obj) = value {
                        for prop in &obj.properties {
                            if prop.key
                                == FormatProperties::VideoSize.as_raw()
                            {
                                if let libspa::pod::Value::Rectangle(rect) = &prop.value {
                                    log_fmt!(
                                        "[wayland] negotiated size: {}x{}",
                                        rect.width,
                                        rect.height
                                    );
                                    dims_changed.set((rect.width, rect.height));
                                }
                            }
                        }
                    }
                }
            }
        })
        .process(move |stream, _data| {
            if let Some(mut buffer) = stream.dequeue_buffer() {
                let datas = buffer.datas_mut();
                if datas.is_empty() {
                    return;
                }
                let data = &mut datas[0];
                let (w, h) = dims.get();

                // Use chunk size for actual data length, data() returns maxsize
                let chunk_size = data.chunk().size() as usize;
                if let Some(slice) = data.data() {
                    let expected = (w * h * 4) as usize;
                    let available = chunk_size.min(slice.len());
                    if available >= expected {
                        let bgra = slice[..expected].to_vec();
                        if let Ok(mut buf) = frame_buf_process.lock() {
                            *buf = Some((bgra, w, h));
                        }
                    }
                }
            }
        })
        .register()
        .expect("Failed to register PipeWire stream listener");

    // Build video format parameters
    let param_bytes = build_video_format_params(portal.width, portal.height);
    let pods: Vec<&libspa::pod::Pod> = param_bytes
        .iter()
        .map(|bytes| libspa::pod::Pod::from_bytes(bytes).expect("invalid pod"))
        .collect();
    let mut param_refs: Vec<&libspa::pod::Pod> = pods;

    if let Err(e) = stream.connect(
        libspa::utils::Direction::Input,
        Some(portal.node_id),
        StreamFlags::AUTOCONNECT | StreamFlags::MAP_BUFFERS,
        &mut param_refs,
    ) {
        log_fmt!("[wayland] Failed to connect PipeWire stream: {e}");
        active.store(false, Ordering::Relaxed);
        return;
    }

    log_fmt!(
        "[wayland] PipeWire mainloop running, node_id={}",
        portal.node_id
    );
    mainloop.run();
    log_fmt!("[wayland] PipeWire mainloop exited");
}
