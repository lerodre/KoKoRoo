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
    use libspa::pod::{ChoiceValue, Object, Property, PropertyFlags, Value};
    use libspa::utils::{
        Choice, ChoiceEnum, ChoiceFlags, Fraction, Id, Rectangle,
    };

    // SPA video format constants (from spa/param/video/raw.h enum)
    const SPA_VIDEO_FORMAT_RGBX: u32 = 7;
    const SPA_VIDEO_FORMAT_BGRX: u32 = 8;
    const SPA_VIDEO_FORMAT_RGBA: u32 = 11;
    const SPA_VIDEO_FORMAT_BGRA: u32 = 12;

    let obj = Value::Object(Object {
        type_: libspa::utils::SpaTypes::ObjectParamFormat.as_raw(),
        id: libspa::param::ParamType::EnumFormat.as_raw(),
        properties: vec![
            // media.type = video
            Property {
                key: FormatProperties::MediaType.as_raw(),
                flags: PropertyFlags::empty(),
                value: Value::Id(Id(MediaType::Video.as_raw())),
            },
            // media.subtype = raw
            Property {
                key: FormatProperties::MediaSubtype.as_raw(),
                flags: PropertyFlags::empty(),
                value: Value::Id(Id(MediaSubtype::Raw.as_raw())),
            },
            // format = BGRx preferred, with BGRA, RGBx, RGBA as alternatives
            Property {
                key: FormatProperties::VideoFormat.as_raw(),
                flags: PropertyFlags::empty(),
                value: Value::Choice(ChoiceValue::Id(Choice(
                    ChoiceFlags::empty(),
                    ChoiceEnum::Enum {
                        default: Id(SPA_VIDEO_FORMAT_BGRX),
                        alternatives: vec![
                            Id(SPA_VIDEO_FORMAT_BGRX),
                            Id(SPA_VIDEO_FORMAT_BGRA),
                            Id(SPA_VIDEO_FORMAT_RGBX),
                            Id(SPA_VIDEO_FORMAT_RGBA),
                        ],
                    },
                ))),
            },
            // size = width x height (with range)
            Property {
                key: FormatProperties::VideoSize.as_raw(),
                flags: PropertyFlags::empty(),
                value: Value::Choice(ChoiceValue::Rectangle(Choice(
                    ChoiceFlags::empty(),
                    ChoiceEnum::Range {
                        default: Rectangle { width, height },
                        min: Rectangle { width: 1, height: 1 },
                        max: Rectangle { width: 7680, height: 4320 },
                    },
                ))),
            },
            // framerate = 30/1 (with range 1-120)
            Property {
                key: FormatProperties::VideoFramerate.as_raw(),
                flags: PropertyFlags::empty(),
                value: Value::Choice(ChoiceValue::Fraction(Choice(
                    ChoiceFlags::empty(),
                    ChoiceEnum::Range {
                        default: Fraction { num: 30, denom: 1 },
                        min: Fraction { num: 1, denom: 1 },
                        max: Fraction { num: 120, denom: 1 },
                    },
                ))),
            },
        ],
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

    // Track negotiated video dimensions and format
    // (w, h, format_id, stride)
    let dims: Rc<Cell<(u32, u32, u32, u32)>> = Rc::new(Cell::new((portal.width, portal.height, 0, 0)));
    let dims_changed = dims.clone();
    let frame_buf_process = frame_buf.clone();

    const SPA_VIDEO_FORMAT_RGBX: u32 = 7;
    const SPA_VIDEO_FORMAT_BGRX: u32 = 8;
    const SPA_VIDEO_FORMAT_RGBA: u32 = 11;
    const SPA_VIDEO_FORMAT_BGRA: u32 = 12;

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

                // Get actual stride from chunk metadata
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

                    // Copy pixel data, handling stride (row padding)
                    let mut bgra = Vec::with_capacity(row_bytes * h as usize);
                    for row in 0..h as usize {
                        let start = row * actual_stride;
                        bgra.extend_from_slice(&slice[start..start + row_bytes]);
                    }

                    // Convert RGBx/RGBA to BGRx/BGRA by swapping R and B channels
                    if fmt == SPA_VIDEO_FORMAT_RGBX || fmt == SPA_VIDEO_FORMAT_RGBA {
                        for pixel in bgra.chunks_exact_mut(4) {
                            pixel.swap(0, 2); // R <-> B
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
