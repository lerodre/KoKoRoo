//! Wayland screen capture via XDG Desktop Portal + PipeWire.
//! This module is Linux-only (#[cfg(target_os = "linux")] in main.rs).

use std::cell::Cell;
use std::os::unix::io::OwnedFd;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ashpd::desktop::screencast::{CursorMode, Screencast, SourceType};
use ashpd::desktop::PersistMode;
use libspa::param::format::{FormatProperties, MediaSubtype, MediaType};
use libspa::param::video::VideoFormat;
use pipewire as pw;
use pipewire::stream::StreamFlags;

/// Check if we're running on Wayland.
pub fn is_wayland() -> bool {
    std::env::var("WAYLAND_DISPLAY").is_ok()
        || std::env::var("XDG_SESSION_TYPE")
            .map(|v| v == "wayland")
            .unwrap_or(false)
}

/// Long-lived portal session. Created once per call, stays alive until dropped.
/// The recording indicator in the system tray stays visible while this exists.
/// When dropped, the portal session is closed and the indicator disappears.
pub struct WaylandPortal {
    fd_request_tx: mpsc::Sender<mpsc::SyncSender<Option<OwnedFd>>>,
    pub node_id: u32,
    pub width: u32,
    pub height: u32,
}

// mpsc::Sender is Send
unsafe impl Send for WaylandPortal {}

/// Lightweight capture params for a single screen share session.
pub struct PortalCapture {
    pub pw_fd: OwnedFd,
    pub node_id: u32,
    pub width: u32,
    pub height: u32,
}

unsafe impl Send for PortalCapture {}

impl WaylandPortal {
    /// Request screen capture permission via XDG Desktop Portal.
    /// Shows the OS permission dialog. Blocks until user responds.
    /// Returns None if user cancels or portal is unavailable.
    pub fn request() -> Option<Self> {
        // Channel for the initial result (node_id, width, height)
        let (result_tx, result_rx) = mpsc::sync_channel::<Option<(u32, u32, u32)>>(1);
        // Channel for fd requests (reusable)
        let (fd_req_tx, fd_req_rx) = mpsc::channel::<mpsc::SyncSender<Option<OwnedFd>>>();

        std::thread::spawn(move || {
            let rt = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(_) => {
                    let _ = result_tx.send(None);
                    return;
                }
            };

            rt.block_on(async {
                log_fmt!("[wayland] portal: creating proxy...");
                let proxy = match Screencast::new().await {
                    Ok(p) => p,
                    Err(e) => {
                        log_fmt!("[wayland] portal: failed to create proxy: {e}");
                        let _ = result_tx.send(None);
                        return;
                    }
                };

                log_fmt!("[wayland] portal: creating session...");
                let session = match proxy.create_session().await {
                    Ok(s) => s,
                    Err(e) => {
                        log_fmt!("[wayland] portal: failed to create session: {e}");
                        let _ = result_tx.send(None);
                        return;
                    }
                };

                log_fmt!("[wayland] portal: selecting sources...");
                if let Err(e) = proxy
                    .select_sources(
                        &session,
                        CursorMode::Embedded,
                        SourceType::Monitor.into(),
                        false,
                        None,
                        PersistMode::DoNot,
                    )
                    .await
                {
                    log_fmt!("[wayland] portal: failed to select sources: {e}");
                    let _ = result_tx.send(None);
                    return;
                }

                log_fmt!("[wayland] portal: starting (waiting for user)...");
                let response = match proxy.start(&session, None).await {
                    Ok(r) => match r.response() {
                        Ok(resp) => resp,
                        Err(e) => {
                            log_fmt!("[wayland] portal: user cancelled or error: {e}");
                            let _ = result_tx.send(None);
                            return;
                        }
                    },
                    Err(e) => {
                        log_fmt!("[wayland] portal: start failed: {e}");
                        let _ = result_tx.send(None);
                        return;
                    }
                };

                log_fmt!("[wayland] portal: user accepted, getting streams...");
                let streams = response.streams();
                let stream = match streams.first() {
                    Some(s) => s,
                    None => {
                        log_fmt!("[wayland] portal: no streams returned");
                        let _ = result_tx.send(None);
                        return;
                    }
                };
                let node_id = stream.pipe_wire_node_id();
                let (w, h) = stream.size().unwrap_or((1920, 1080));

                log_fmt!("[wayland] portal: success! node_id={}, {}x{}", node_id, w, h);
                let _ = result_tx.send(Some((node_id, w as u32, h as u32)));

                // Event loop: handle fd requests and keep session alive.
                // The async sleep keeps the tokio event loop responsive for D-Bus.
                loop {
                    match fd_req_rx.try_recv() {
                        Ok(reply_tx) => {
                            log_fmt!("[wayland] portal: opening new pipewire fd...");
                            let fd = proxy.open_pipe_wire_remote(&session).await.ok();
                            if fd.is_some() {
                                log_fmt!("[wayland] portal: fd opened successfully");
                            } else {
                                log_fmt!("[wayland] portal: failed to open fd");
                            }
                            let _ = reply_tx.send(fd);
                        }
                        Err(mpsc::TryRecvError::Disconnected) => {
                            // WaylandPortal was dropped → close session
                            break;
                        }
                        Err(mpsc::TryRecvError::Empty) => {
                            tokio::time::sleep(Duration::from_millis(50)).await;
                        }
                    }
                }
                log_fmt!("[wayland] portal: session closing");
                // `session` drops here → D-Bus Close → indicator disappears
            });
        });

        // Block calling thread until portal negotiation completes
        let (node_id, width, height) = result_rx.recv().ok()??;

        Some(WaylandPortal {
            fd_request_tx: fd_req_tx,
            node_id,
            width,
            height,
        })
    }

    /// Get a new PipeWire fd for a capture session.
    /// Can be called multiple times on the same portal.
    pub fn new_capture(&self) -> Option<PortalCapture> {
        let (tx, rx) = mpsc::sync_channel(1);
        self.fd_request_tx.send(tx).ok()?;
        let pw_fd = rx.recv().ok()??;
        Some(PortalCapture {
            pw_fd,
            node_id: self.node_id,
            width: self.width,
            height: self.height,
        })
    }
}

/// Build SPA video format parameters using the object!/property! macros.
fn build_video_format_params() -> Vec<u8> {
    use libspa::pod::serialize::PodSerializer;
    use libspa::pod::Value;

    let obj = libspa::pod::object!(
        libspa::utils::SpaTypes::ObjectParamFormat,
        libspa::param::ParamType::EnumFormat,
        libspa::pod::property!(
            FormatProperties::MediaType,
            Id,
            MediaType::Video
        ),
        libspa::pod::property!(
            FormatProperties::MediaSubtype,
            Id,
            MediaSubtype::Raw
        ),
        libspa::pod::property!(
            FormatProperties::VideoFormat,
            Choice,
            Enum,
            Id,
            VideoFormat::BGRx,
            VideoFormat::BGRx,
            VideoFormat::BGRA,
            VideoFormat::RGBx,
            VideoFormat::RGBA,
        ),
        libspa::pod::property!(
            FormatProperties::VideoSize,
            Choice,
            Range,
            Rectangle,
            libspa::utils::Rectangle { width: 256, height: 256 },
            libspa::utils::Rectangle { width: 1, height: 1 },
            libspa::utils::Rectangle { width: 8192, height: 8192 }
        ),
        libspa::pod::property!(
            FormatProperties::VideoFramerate,
            Choice,
            Range,
            Fraction,
            libspa::utils::Fraction { num: 0, denom: 1 },
            libspa::utils::Fraction { num: 0, denom: 1 },
            libspa::utils::Fraction { num: 1000, denom: 1 }
        ),
    );

    PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &Value::Object(obj),
    )
    .expect("Failed to serialize video format params")
    .0
    .into_inner()
}

/// Run PipeWire MainLoop on a dedicated thread.
/// Stores latest captured frame in `frame_buf` for the encoder thread to read.
pub fn run_pipewire_capture(
    capture: PortalCapture,
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
    let core = match context.connect_fd(capture.pw_fd, None) {
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

    // Track negotiated video dimensions and format
    let dims: Rc<Cell<(u32, u32, u32, u32)>> =
        Rc::new(Cell::new((capture.width, capture.height, 0, 0)));
    let dims_changed = dims.clone();
    let frame_buf_process = frame_buf.clone();

    let fmt_rgbx = VideoFormat::RGBx.as_raw();
    let fmt_rgba = VideoFormat::RGBA.as_raw();

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
                        let (mut w, mut h, mut fmt, stride) = dims_changed.get();
                        for prop in &obj.properties {
                            if prop.key == FormatProperties::VideoSize.as_raw() {
                                if let libspa::pod::Value::Rectangle(rect) = &prop.value {
                                    w = rect.width;
                                    h = rect.height;
                                }
                            }
                            if prop.key == FormatProperties::VideoFormat.as_raw() {
                                if let libspa::pod::Value::Id(id) = &prop.value {
                                    fmt = id.0;
                                }
                            }
                        }
                        log_fmt!(
                            "[wayland] negotiated: {}x{}, format={}",
                            w, h, fmt
                        );
                        dims_changed.set((w, h, fmt, stride));
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
                let (w, h, fmt, _) = dims.get();
                if w == 0 || h == 0 {
                    return;
                }

                let stride = data.chunk().stride() as u32;
                let chunk_size = data.chunk().size() as usize;

                if let Some(slice) = data.data() {
                    let available = chunk_size.min(slice.len());
                    let row_bytes = (w * 4) as usize;
                    let actual_stride = if stride > 0 { stride as usize } else { row_bytes };
                    let needed = actual_stride * (h as usize);

                    if available < needed {
                        return;
                    }

                    let mut bgra = Vec::with_capacity(row_bytes * h as usize);
                    for row in 0..h as usize {
                        let start = row * actual_stride;
                        bgra.extend_from_slice(&slice[start..start + row_bytes]);
                    }

                    if fmt == fmt_rgbx || fmt == fmt_rgba {
                        for pixel in bgra.chunks_exact_mut(4) {
                            pixel.swap(0, 2);
                        }
                    }

                    if let Ok(mut buf) = frame_buf_process.lock() {
                        *buf = Some((bgra, w, h));
                    }
                }
            }
        })
        .register()
        .expect("Failed to register PipeWire stream listener");

    let param_bytes = build_video_format_params();
    let pod = libspa::pod::Pod::from_bytes(&param_bytes).expect("invalid pod");
    let mut params = vec![pod];

    if let Err(e) = stream.connect(
        libspa::utils::Direction::Input,
        Some(capture.node_id),
        StreamFlags::AUTOCONNECT | StreamFlags::MAP_BUFFERS,
        &mut params,
    ) {
        log_fmt!("[wayland] Failed to connect PipeWire stream: {e}");
        active.store(false, Ordering::Relaxed);
        return;
    }

    log_fmt!(
        "[wayland] PipeWire mainloop running, node_id={}",
        capture.node_id
    );
    mainloop.run();
    log_fmt!("[wayland] PipeWire mainloop exited");
}
