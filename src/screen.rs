use std::collections::HashMap;
use std::net::{SocketAddr, UdpSocket};
use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use vpx_sys::*;
use vpx_sys::vpx_rc_mode::VPX_CBR;
use vpx_sys::vpx_kf_mode::VPX_KF_AUTO;
use vpx_sys::vp8e_enc_control_id::VP8E_SET_CPUUSED;
use vpx_sys::vpx_img_fmt::VPX_IMG_FMT_I420;
use vpx_sys::vpx_codec_cx_pkt_kind::VPX_CODEC_CX_FRAME_PKT;

use crate::crypto::{Session, PKT_SCREEN};

// ── Capture Source ──

/// How frames are captured. Determined at runtime.
pub enum CaptureSource {
    /// scrap crate (X11 on Linux, DXGI on Windows)
    Scrap { display_index: usize },
    /// Wayland XDG Desktop Portal + PipeWire (Linux only)
    #[cfg(target_os = "linux")]
    PipeWire {
        capture: crate::wayland_capture::PortalCapture,
    },
    /// Webcam via nokhwa (V4L2 on Linux, MediaFoundation on Windows)
    Webcam { device_index: usize },
}

// ── Quality Presets ──

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScreenQuality {
    Hd720p30,
    Hd1080p30,
    Hd1080p30Premium,
    Hd1080p60Premium,
}

impl ScreenQuality {
    pub const ALL: [ScreenQuality; 4] = [
        ScreenQuality::Hd720p30,
        ScreenQuality::Hd1080p30,
        ScreenQuality::Hd1080p30Premium,
        ScreenQuality::Hd1080p60Premium,
    ];

    pub fn width(self) -> u32 {
        match self {
            ScreenQuality::Hd720p30 => 1280,
            _ => 1920,
        }
    }

    pub fn height(self) -> u32 {
        match self {
            ScreenQuality::Hd720p30 => 720,
            _ => 1080,
        }
    }

    pub fn fps(self) -> u32 {
        match self {
            ScreenQuality::Hd1080p60Premium => 60,
            _ => 30,
        }
    }

    pub fn bitrate_kbps(self) -> u32 {
        match self {
            ScreenQuality::Hd720p30 => 2000,
            ScreenQuality::Hd1080p30 => 4000,
            ScreenQuality::Hd1080p30Premium => 6000,
            ScreenQuality::Hd1080p60Premium => 8000,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            ScreenQuality::Hd720p30 => "720p 2Mbps",
            ScreenQuality::Hd1080p30 => "1080p 4Mbps",
            ScreenQuality::Hd1080p30Premium => "1080p 6Mbps",
            ScreenQuality::Hd1080p60Premium => "1080p60 8Mbps",
        }
    }
}

/// Command from GUI to engine thread to start/stop screen/webcam sharing.
/// `audio_device`: None = no system audio, Some("") = default device, Some(name) = specific device.
#[derive(Debug, Clone)]
pub enum ScreenCommand {
    StartScreen { quality: ScreenQuality, audio_device: Option<String>, display_index: usize },
    StartWebcam { quality: ScreenQuality, device_index: usize },
    Stop,
}

/// List available displays with labels like "Display 1 (1920x1080)".
pub fn list_displays() -> Vec<String> {
    #[cfg(target_os = "linux")]
    {
        if crate::wayland_capture::is_wayland() {
            return vec!["Screen (system dialog)".to_string()];
        }
    }
    let displays = scrap::Display::all().unwrap_or_default();
    displays.iter().enumerate().map(|(i, d)| {
        format!("Display {} ({}x{})", i + 1, d.width(), d.height())
    }).collect()
}

/// List available webcam devices.
pub fn list_cameras() -> Vec<String> {
    use nokhwa::utils::ApiBackend;
    match nokhwa::query(ApiBackend::Auto) {
        Ok(devices) => devices.iter().enumerate().map(|(i, d)| {
            format!("Camera {} ({})", i, d.human_name())
        }).collect(),
        Err(e) => {
            log_fmt!("[screen] list_cameras error: {}", e);
            Vec::new()
        }
    }
}

// ── Constants ──

const CHUNK_MAX_PAYLOAD: usize = 1300; // fits in UDP MTU with encryption overhead
const KEYFRAME_INTERVAL: u32 = 90; // keyframe every 3 seconds

// Chunk header: [2: frame_id][2: chunk_index][2: total_chunks][1: flags][data...]
const CHUNK_HEADER_SIZE: usize = 7;
const FLAG_KEYFRAME: u8 = 0x01;

// VPX encoder deadline for realtime encoding
const VPX_DL_REALTIME: std::os::raw::c_ulong = 1;

// Use ABI versions from the bindings (matches the installed libvpx)

// Flag to force keyframe
const VPX_EFLAG_FORCE_KF: vpx_enc_frame_flags_t = 1;

// ── Color Conversion Helpers ──

/// Convert BGRA pixels to I420 (YUV planar) for VP8 encoding.
fn bgra_to_i420(bgra: &[u8], width: usize, height: usize) -> Vec<u8> {
    let y_size = width * height;
    let uv_size = (width / 2) * (height / 2);
    let mut yuv = vec![0u8; y_size + uv_size * 2];

    let (y_plane, uv_planes) = yuv.split_at_mut(y_size);
    let (u_plane, v_plane) = uv_planes.split_at_mut(uv_size);

    for row in 0..height {
        for col in 0..width {
            let px = (row * width + col) * 4;
            let b = bgra[px] as i32;
            let g = bgra[px + 1] as i32;
            let r = bgra[px + 2] as i32;

            let y = ((66 * r + 129 * g + 25 * b + 128) >> 8) + 16;
            y_plane[row * width + col] = y.clamp(0, 255) as u8;

            if row % 2 == 0 && col % 2 == 0 {
                let uv_idx = (row / 2) * (width / 2) + (col / 2);
                let u = ((-38 * r - 74 * g + 112 * b + 128) >> 8) + 128;
                let v = ((112 * r - 94 * g - 18 * b + 128) >> 8) + 128;
                u_plane[uv_idx] = u.clamp(0, 255) as u8;
                v_plane[uv_idx] = v.clamp(0, 255) as u8;
            }
        }
    }
    yuv
}

/// Convert I420 (YUV planar) to RGBA pixels for display.
#[allow(dead_code)]
fn i420_to_rgba(yuv: &[u8], width: usize, height: usize) -> Vec<u8> {
    let y_size = width * height;
    let uv_stride = width / 2;
    let mut rgba = vec![0u8; width * height * 4];

    for row in 0..height {
        for col in 0..width {
            let y_val = yuv[row * width + col] as i32 - 16;
            let uv_idx = (row / 2) * uv_stride + (col / 2);
            let u_val = yuv[y_size + uv_idx] as i32 - 128;
            let v_val = yuv[y_size + y_size / 4 + uv_idx] as i32 - 128;

            let c = 298 * y_val;
            let r = (c + 409 * v_val + 128) >> 8;
            let g = (c - 100 * u_val - 208 * v_val + 128) >> 8;
            let b = (c + 516 * u_val + 128) >> 8;

            let px = (row * width + col) * 4;
            rgba[px] = r.clamp(0, 255) as u8;
            rgba[px + 1] = g.clamp(0, 255) as u8;
            rgba[px + 2] = b.clamp(0, 255) as u8;
            rgba[px + 3] = 255;
        }
    }
    rgba
}

/// Convert I420 from vpx_image_t plane pointers to RGBA pixels for display.
/// Reads directly from the image planes using their strides.
unsafe fn vpx_image_to_rgba(img: &vpx_image_t) -> Vec<u8> {
    let width = img.d_w as usize;
    let height = img.d_h as usize;
    let mut rgba = vec![0u8; width * height * 4];

    let y_plane = img.planes[0];
    let u_plane = img.planes[1];
    let v_plane = img.planes[2];
    let y_stride = img.stride[0] as usize;
    let u_stride = img.stride[1] as usize;
    let v_stride = img.stride[2] as usize;

    for row in 0..height {
        for col in 0..width {
            let y_val = *y_plane.add(row * y_stride + col) as i32 - 16;
            let u_val = *u_plane.add((row / 2) * u_stride + col / 2) as i32 - 128;
            let v_val = *v_plane.add((row / 2) * v_stride + col / 2) as i32 - 128;

            let c = 298 * y_val;
            let r = (c + 409 * v_val + 128) >> 8;
            let g = (c - 100 * u_val - 208 * v_val + 128) >> 8;
            let b = (c + 516 * u_val + 128) >> 8;

            let px = (row * width + col) * 4;
            rgba[px] = r.clamp(0, 255) as u8;
            rgba[px + 1] = g.clamp(0, 255) as u8;
            rgba[px + 2] = b.clamp(0, 255) as u8;
            rgba[px + 3] = 255;
        }
    }
    rgba
}

/// Nearest-neighbor resize for BGRA frames.
fn scale_bgra(src: &[u8], src_w: usize, src_h: usize, dst_w: usize, dst_h: usize) -> Vec<u8> {
    if src_w == dst_w && src_h == dst_h {
        return src.to_vec();
    }
    let mut dst = vec![0u8; dst_w * dst_h * 4];
    for y in 0..dst_h {
        let src_y = y * src_h / dst_h;
        for x in 0..dst_w {
            let src_x = x * src_w / dst_w;
            let si = (src_y * src_w + src_x) * 4;
            let di = (y * dst_w + x) * 4;
            dst[di..di + 4].copy_from_slice(&src[si..si + 4]);
        }
    }
    dst
}

// ── Screen Encoder (wraps libvpx directly) ──

pub struct ScreenEncoder {
    ctx: vpx_codec_ctx_t,
    img: vpx_image_t,
    frame_count: u32,
    width: u32,
    height: u32,
}

impl ScreenEncoder {
    pub fn new(width: u32, height: u32, fps: u32, bitrate_kbps: u32) -> Self {
        log_fmt!("[screen] ScreenEncoder::new({}x{}, {}fps, {}kbps)", width, height, fps, bitrate_kbps);

        // Set panic hook to log panics before process dies
        std::panic::set_hook(Box::new(|info| {
            crate::logger::log(&format!("[PANIC] {}", info));
        }));

        unsafe {
            log_fmt!("[screen] calling vpx_codec_vp8_cx...");
            let iface = vpx_codec_vp8_cx();
            log_fmt!("[screen] vpx_codec_vp8_cx() returned (null={})", iface.is_null());

            let mut cfg: vpx_codec_enc_cfg_t = std::mem::MaybeUninit::zeroed().assume_init();
            log_fmt!("[screen] calling vpx_codec_enc_config_default...");
            let ret = vpx_codec_enc_config_default(iface, &mut cfg, 0);
            log_fmt!("[screen] vpx_codec_enc_config_default -> {:?}", ret);
            if ret != VPX_CODEC_OK {
                log_fmt!("[screen] FATAL: enc_config_default failed: {:?}", ret);
                panic!("Failed to get default VP8 config: {:?}", ret);
            }

            cfg.g_w = width;
            cfg.g_h = height;
            cfg.g_timebase.num = 1;
            cfg.g_timebase.den = fps as i32;
            cfg.rc_target_bitrate = bitrate_kbps;
            cfg.rc_end_usage = VPX_CBR;
            cfg.g_error_resilient = 1;
            cfg.g_lag_in_frames = 0;
            cfg.g_threads = if width >= 1920 { 4 } else { 2 };
            cfg.kf_mode = VPX_KF_AUTO;
            cfg.kf_max_dist = KEYFRAME_INTERVAL;
            log_fmt!("[screen] encoder config set, calling vpx_codec_enc_init_ver...");

            let mut ctx: vpx_codec_ctx_t = std::mem::MaybeUninit::zeroed().assume_init();
            let enc_abi = vpx_sys::VPX_ENCODER_ABI_VERSION as std::os::raw::c_int;
            log_fmt!("[screen] enc_init ABI={}", enc_abi);
            let ret = vpx_codec_enc_init_ver(
                &mut ctx,
                iface,
                &cfg,
                0,
                enc_abi,
            );
            log_fmt!("[screen] encoder init -> {:?}", ret);
            if ret != VPX_CODEC_OK {
                log_fmt!("[screen] FATAL: encoder init failed: {:?}", ret);
                panic!("Failed to init VP8 encoder: {:?}", ret);
            }

            log_fmt!("[screen] setting CPU speed...");
            vpx_codec_control_(&mut ctx, VP8E_SET_CPUUSED as i32, 8i32);

            log_fmt!("[screen] allocating image...");
            let mut img: vpx_image_t = std::mem::MaybeUninit::zeroed().assume_init();
            vpx_img_alloc(&mut img, VPX_IMG_FMT_I420, width, height, 1);
            log_fmt!("[screen] ScreenEncoder ready!");

            ScreenEncoder {
                ctx,
                img,
                frame_count: 0,
                width,
                height,
            }
        }
    }

    /// Encode a BGRA frame. Returns encoded VP8 bytes.
    pub fn encode(&mut self, bgra_frame: &[u8], force_keyframe: bool) -> Vec<u8> {
        let i420 = bgra_to_i420(bgra_frame, self.width as usize, self.height as usize);

        // Copy I420 data into vpx_image planes
        unsafe {
            let y_size = (self.width * self.height) as usize;
            let uv_size = y_size / 4;
            let y_stride = self.img.stride[0] as usize;
            let u_stride = self.img.stride[1] as usize;
            let v_stride = self.img.stride[2] as usize;
            let w = self.width as usize;
            let h = self.height as usize;

            // Copy Y plane
            for row in 0..h {
                ptr::copy_nonoverlapping(
                    i420[row * w..].as_ptr(),
                    self.img.planes[0].add(row * y_stride),
                    w,
                );
            }
            // Copy U plane
            let uw = w / 2;
            let uh = h / 2;
            for row in 0..uh {
                ptr::copy_nonoverlapping(
                    i420[y_size + row * uw..].as_ptr(),
                    self.img.planes[1].add(row * u_stride),
                    uw,
                );
            }
            // Copy V plane
            for row in 0..uh {
                ptr::copy_nonoverlapping(
                    i420[y_size + uv_size + row * uw..].as_ptr(),
                    self.img.planes[2].add(row * v_stride),
                    uw,
                );
            }
        }

        let flags: vpx_enc_frame_flags_t = if force_keyframe { VPX_EFLAG_FORCE_KF } else { 0 };
        let pts = self.frame_count as i64;
        self.frame_count += 1;

        unsafe {
            let ret = vpx_codec_encode(
                &mut self.ctx,
                &self.img,
                pts,
                1, // duration
                flags,
                VPX_DL_REALTIME,
            );
            if ret != VPX_CODEC_OK {
                return Vec::new();
            }

            let mut encoded = Vec::new();
            let mut iter: vpx_codec_iter_t = ptr::null();
            loop {
                let pkt = vpx_codec_get_cx_data(&mut self.ctx, &mut iter);
                if pkt.is_null() {
                    break;
                }
                if (*pkt).kind == VPX_CODEC_CX_FRAME_PKT {
                    let frame = &(*pkt).data.frame;
                    let data = std::slice::from_raw_parts(
                        frame.buf as *const u8,
                        frame.sz as usize,
                    );
                    encoded.extend_from_slice(data);
                }
            }
            encoded
        }
    }

    /// Split encoded data into UDP-safe chunks with header.
    pub fn fragment(encoded: &[u8], frame_id: u16, is_keyframe: bool) -> Vec<Vec<u8>> {
        let max_data = CHUNK_MAX_PAYLOAD - CHUNK_HEADER_SIZE;
        let total_chunks = ((encoded.len() + max_data - 1) / max_data) as u16;
        let mut chunks = Vec::with_capacity(total_chunks as usize);

        for (i, chunk_data) in encoded.chunks(max_data).enumerate() {
            let mut chunk = Vec::with_capacity(CHUNK_HEADER_SIZE + chunk_data.len());
            chunk.extend_from_slice(&frame_id.to_be_bytes());
            chunk.extend_from_slice(&(i as u16).to_be_bytes());
            chunk.extend_from_slice(&total_chunks.to_be_bytes());
            chunk.push(if is_keyframe { FLAG_KEYFRAME } else { 0 });
            chunk.extend_from_slice(chunk_data);
            chunks.push(chunk);
        }
        chunks
    }
}

impl Drop for ScreenEncoder {
    fn drop(&mut self) {
        unsafe {
            vpx_img_free(&mut self.img);
            vpx_codec_destroy(&mut self.ctx);
        }
    }
}

// ── Screen Decoder ──

pub struct ScreenDecoder {
    ctx: vpx_codec_ctx_t,
}

impl ScreenDecoder {
    pub fn new() -> Self {
        let dec_abi = vpx_sys::VPX_DECODER_ABI_VERSION as std::os::raw::c_int;
        log_fmt!("[screen] ScreenDecoder::new(ABI={})", dec_abi);
        unsafe {
            let mut ctx: vpx_codec_ctx_t = std::mem::MaybeUninit::zeroed().assume_init();
            let iface = vpx_codec_vp8_dx();
            let ret = vpx_codec_dec_init_ver(
                &mut ctx,
                iface,
                ptr::null(),
                0,
                dec_abi,
            );
            log_fmt!("[screen] decoder init -> {:?}", ret);
            assert_eq!(ret, VPX_CODEC_OK, "Failed to init VP8 decoder");
            ScreenDecoder { ctx }
        }
    }

    /// Decode VP8 data -> (RGBA pixels, width, height).
    /// Width/height are read from the VP8 bitstream so the receiver auto-detects resolution.
    pub fn decode(&mut self, encoded: &[u8]) -> Option<(Vec<u8>, u32, u32)> {
        unsafe {
            let ret = vpx_codec_decode(
                &mut self.ctx,
                encoded.as_ptr(),
                encoded.len() as std::os::raw::c_uint,
                ptr::null_mut(),
                0,
            );
            if ret != VPX_CODEC_OK {
                return None;
            }

            let mut iter: vpx_codec_iter_t = ptr::null();
            let img = vpx_codec_get_frame(&mut self.ctx, &mut iter);
            if img.is_null() {
                return None;
            }

            let w = (*img).d_w;
            let h = (*img).d_h;
            Some((vpx_image_to_rgba(&*img), w, h))
        }
    }
}

impl Drop for ScreenDecoder {
    fn drop(&mut self) {
        unsafe {
            vpx_codec_destroy(&mut self.ctx);
        }
    }
}

// Safety: ScreenDecoder is only accessed through a Mutex, ensuring single-threaded access.
// The raw pointers inside vpx_codec_ctx_t are owned by the codec and not shared.
unsafe impl Send for ScreenDecoder {}
unsafe impl Sync for ScreenDecoder {}

// ── Frame Assembler ──

struct PendingFrame {
    chunks: Vec<Option<Vec<u8>>>,
    total_chunks: u16,
    received: u16,
    flags: u8,
}

pub struct FrameAssembler {
    frames: HashMap<u16, PendingFrame>,
}

impl FrameAssembler {
    pub fn new() -> Self {
        FrameAssembler {
            frames: HashMap::new(),
        }
    }

    /// Add a chunk. Returns assembled encoded frame when all chunks are received.
    pub fn add_chunk(
        &mut self,
        frame_id: u16,
        chunk_index: u16,
        total_chunks: u16,
        flags: u8,
        data: &[u8],
    ) -> Option<(Vec<u8>, u8)> {
        // Keep only latest 3 frame_ids -- drop stale ones
        if !self.frames.contains_key(&frame_id) && self.frames.len() >= 3 {
            let oldest = *self
                .frames
                .keys()
                .min_by_key(|&&id| frame_id.wrapping_sub(id))
                .unwrap();
            self.frames.remove(&oldest);
        }

        let pending = self.frames.entry(frame_id).or_insert_with(|| PendingFrame {
            chunks: vec![None; total_chunks as usize],
            total_chunks,
            received: 0,
            flags,
        });

        let idx = chunk_index as usize;
        if idx < pending.chunks.len() && pending.chunks[idx].is_none() {
            pending.chunks[idx] = Some(data.to_vec());
            pending.received += 1;
            pending.flags |= flags;
        }

        if pending.received == pending.total_chunks {
            let flags = pending.flags;
            let mut assembled = Vec::new();
            for chunk in &pending.chunks {
                if let Some(data) = chunk {
                    assembled.extend_from_slice(data);
                }
            }
            self.frames.remove(&frame_id);
            Some((assembled, flags))
        } else {
            None
        }
    }
}

// ── Screen Viewer (used by voice.rs receiver + gui.rs) ──

pub struct ScreenViewer {
    assembler: FrameAssembler,
    decoder: ScreenDecoder,
    pub latest_frame: Option<Vec<u8>>, // RGBA pixels
    pub frame_width: u32,
    pub frame_height: u32,
    /// Set to true when peer sends PKT_SCREEN_STOP; cleared on next receive_chunk.
    pub stopped: bool,
}

impl ScreenViewer {
    pub fn new() -> Self {
        ScreenViewer {
            assembler: FrameAssembler::new(),
            decoder: ScreenDecoder::new(),
            latest_frame: None,
            frame_width: 1280,
            frame_height: 720,
            stopped: false,
        }
    }

    /// Parse chunk header and feed to assembler. Decode when frame is complete.
    pub fn receive_chunk(&mut self, payload: &[u8]) {
        if payload.len() < CHUNK_HEADER_SIZE {
            return;
        }
        self.stopped = false;
        let frame_id = u16::from_be_bytes([payload[0], payload[1]]);
        let chunk_index = u16::from_be_bytes([payload[2], payload[3]]);
        let total_chunks = u16::from_be_bytes([payload[4], payload[5]]);
        let flags = payload[6];
        let data = &payload[CHUNK_HEADER_SIZE..];

        if let Some((encoded, _flags)) = self.assembler.add_chunk(
            frame_id,
            chunk_index,
            total_chunks,
            flags,
            data,
        ) {
            if let Some((rgba, w, h)) = self.decoder.decode(&encoded) {
                self.frame_width = w;
                self.frame_height = h;
                self.latest_frame = Some(rgba);
            }
        }
    }

    /// Take the latest decoded RGBA frame (consumed -- returns None until next frame).
    pub fn take_frame(&mut self) -> Option<Vec<u8>> {
        self.latest_frame.take()
    }
}

// ── Capture Loop (thread function) ──

pub fn capture_loop(
    socket: UdpSocket,
    session: Session,
    peer_addr: SocketAddr,
    active: Arc<AtomicBool>,
    running: Arc<AtomicBool>,
    quality: ScreenQuality,
    source: CaptureSource,
) {
    match source {
        CaptureSource::Scrap { display_index } => {
            capture_loop_scrap(socket, session, peer_addr, active, running, quality, display_index);
        }
        #[cfg(target_os = "linux")]
        CaptureSource::PipeWire { capture } => {
            capture_loop_pipewire(socket, session, peer_addr, active, running, quality, capture);
        }
        CaptureSource::Webcam { device_index } => {
            capture_loop_webcam(socket, session, peer_addr, active, running, quality, device_index);
        }
    }
}

#[cfg(target_os = "linux")]
fn capture_loop_pipewire(
    socket: UdpSocket,
    session: Session,
    peer_addr: SocketAddr,
    active: Arc<AtomicBool>,
    running: Arc<AtomicBool>,
    quality: ScreenQuality,
    capture: crate::wayland_capture::PortalCapture,
) {
    use std::sync::Mutex;
    use std::thread;

    log_fmt!("[screen] capture_loop_pipewire: starting, peer={}, quality={:?}", peer_addr, quality);

    let native_w = capture.width;
    let native_h = capture.height;

    let frame_buf: Arc<Mutex<Option<(Vec<u8>, u32, u32)>>> = Arc::new(Mutex::new(None));

    // Spawn PipeWire thread (MainLoop must run on its own thread, objects are !Send)
    let pw_handle = {
        let fb = frame_buf.clone();
        let a = active.clone();
        let r = running.clone();
        thread::spawn(move || {
            crate::wayland_capture::run_pipewire_capture(capture, fb, a, r);
        })
    };

    // Cap encoder resolution to native capture dims — avoid upscaling small screens
    let enc_w = quality.width().min(native_w);
    let enc_h = quality.height().min(native_h);
    let enc_fps = quality.fps();
    let enc_bps = quality.bitrate_kbps();
    log_fmt!("[screen] pw encoder: {}x{} {}fps {}kbps (native {}x{})",
        enc_w, enc_h, enc_fps, enc_bps, native_w, native_h);
    let mut encoder = ScreenEncoder::new(enc_w, enc_h, enc_fps, enc_bps);
    let frame_duration = Duration::from_secs_f64(1.0 / enc_fps as f64);
    let mut frame_id: u16 = 0;
    let mut frame_count: u32 = 0;

    while active.load(Ordering::Relaxed) && running.load(Ordering::Relaxed) {
        let frame_start = Instant::now();

        let frame = { frame_buf.lock().unwrap().take() };

        if let Some((bgra, w, h)) = frame {
            let scaled = scale_bgra(&bgra, w as usize, h as usize, enc_w as usize, enc_h as usize);
            let force_kf = frame_count % KEYFRAME_INTERVAL == 0;
            let encoded = encoder.encode(&scaled, force_kf);

            if !encoded.is_empty() {
                let chunks = ScreenEncoder::fragment(&encoded, frame_id, force_kf);
                for (i, chunk) in chunks.iter().enumerate() {
                    let packet = session.encrypt_packet(PKT_SCREEN, chunk);
                    let _ = socket.send_to(&packet, peer_addr);
                    if i + 1 < chunks.len() {
                        std::thread::sleep(Duration::from_micros(200));
                    }
                }
                if frame_count % 30 == 0 || frame_count < 5 {
                    log_fmt!("[screen] pw frame #{} sent ({} bytes, {} chunks, kf={})",
                        frame_count, encoded.len(), chunks.len(), force_kf);
                }
                frame_id = frame_id.wrapping_add(1);
            }
            frame_count += 1;
        }

        let elapsed = frame_start.elapsed();
        if elapsed < frame_duration {
            std::thread::sleep(frame_duration - elapsed);
        }
    }

    active.store(false, Ordering::Relaxed);
    let _ = pw_handle.join();
    log_fmt!("[screen] capture_loop_pipewire exited");
}

fn capture_loop_webcam(
    socket: UdpSocket,
    session: Session,
    peer_addr: SocketAddr,
    active: Arc<AtomicBool>,
    running: Arc<AtomicBool>,
    quality: ScreenQuality,
    device_index: usize,
) {
    use nokhwa::pixel_format::RgbFormat;
    use nokhwa::utils::{CameraIndex, RequestedFormat, RequestedFormatType};
    use nokhwa::Camera;

    log_fmt!("[screen] capture_loop_webcam: starting, device_index={}, peer={}, quality={:?}",
        device_index, peer_addr, quality);

    let index = CameraIndex::Index(device_index as u32);
    let requested = RequestedFormat::new::<RgbFormat>(RequestedFormatType::AbsoluteHighestFrameRate);

    let mut camera = match Camera::new(index, requested) {
        Ok(c) => c,
        Err(e) => {
            log_fmt!("[screen] ERROR: failed to open camera {}: {}", device_index, e);
            active.store(false, Ordering::Relaxed);
            return;
        }
    };

    if let Err(e) = camera.open_stream() {
        log_fmt!("[screen] ERROR: failed to open camera stream: {}", e);
        active.store(false, Ordering::Relaxed);
        return;
    }

    let res = camera.resolution();
    let native_w = res.width() as u32;
    let native_h = res.height() as u32;
    let cam_fmt = camera.camera_format();
    log_fmt!("[screen] camera resolution: {}x{}, format: {:?}", native_w, native_h, cam_fmt);

    // Cap encoder resolution to camera native resolution
    let enc_w = quality.width().min(native_w);
    let enc_h = quality.height().min(native_h);
    let enc_fps = quality.fps();
    let enc_bps = quality.bitrate_kbps();
    log_fmt!("[screen] webcam encoder: {}x{} {}fps {}kbps (native {}x{})",
        enc_w, enc_h, enc_fps, enc_bps, native_w, native_h);

    let mut encoder = ScreenEncoder::new(enc_w, enc_h, enc_fps, enc_bps);
    let frame_duration = Duration::from_secs_f64(1.0 / enc_fps as f64);
    let mut frame_id: u16 = 0;
    let mut frame_count: u32 = 0;

    while active.load(Ordering::Relaxed) && running.load(Ordering::Relaxed) {
        let frame_start = Instant::now();

        let frame = match camera.frame() {
            Ok(f) => f,
            Err(e) => {
                log_fmt!("[screen] webcam frame error: {}", e);
                std::thread::sleep(Duration::from_millis(10));
                continue;
            }
        };

        // Decode frame to RGB regardless of camera's native format (YUYV, MJPEG, etc.)
        let decoded = match frame.decode_image::<RgbFormat>() {
            Ok(img) => img,
            Err(e) => {
                if frame_count < 3 {
                    log_fmt!("[screen] webcam decode error: {}", e);
                }
                std::thread::sleep(Duration::from_millis(10));
                continue;
            }
        };
        let w = decoded.width() as usize;
        let h = decoded.height() as usize;
        let rgb_data = decoded.into_raw();

        if frame_count < 3 {
            log_fmt!("[screen] webcam frame: {}x{}, rgb_data.len={}, expected={}", w, h, rgb_data.len(), w * h * 3);
        }

        // Validate RGB data size
        let expected_rgb = w * h * 3;
        if rgb_data.len() < expected_rgb {
            if frame_count < 5 {
                log_fmt!("[screen] webcam: RGB data too short ({} < {}), skipping", rgb_data.len(), expected_rgb);
            }
            std::thread::sleep(Duration::from_millis(10));
            continue;
        }

        // Convert RGB → BGRA
        let mut bgra = vec![0u8; w * h * 4];
        for i in 0..(w * h) {
            bgra[i * 4] = rgb_data[i * 3 + 2];     // B
            bgra[i * 4 + 1] = rgb_data[i * 3 + 1]; // G
            bgra[i * 4 + 2] = rgb_data[i * 3];      // R
            bgra[i * 4 + 3] = 255;                   // A
        }

        let scaled = scale_bgra(&bgra, w, h, enc_w as usize, enc_h as usize);
        let force_kf = frame_count % KEYFRAME_INTERVAL == 0;
        let encoded = encoder.encode(&scaled, force_kf);

        if !encoded.is_empty() {
            let chunks = ScreenEncoder::fragment(&encoded, frame_id, force_kf);
            for (i, chunk) in chunks.iter().enumerate() {
                let packet = session.encrypt_packet(PKT_SCREEN, chunk);
                let _ = socket.send_to(&packet, peer_addr);
                if i + 1 < chunks.len() {
                    std::thread::sleep(Duration::from_micros(200));
                }
            }
            if frame_count % 30 == 0 || frame_count < 5 {
                log_fmt!("[screen] webcam frame #{} sent ({} bytes, {} chunks, kf={})",
                    frame_count, encoded.len(), chunks.len(), force_kf);
            }
            frame_id = frame_id.wrapping_add(1);
        }
        frame_count += 1;

        let elapsed = frame_start.elapsed();
        if elapsed < frame_duration {
            std::thread::sleep(frame_duration - elapsed);
        }
    }

    active.store(false, Ordering::Relaxed);
    log_fmt!("[screen] capture_loop_webcam exited");
}

fn capture_loop_scrap(
    socket: UdpSocket,
    session: Session,
    peer_addr: SocketAddr,
    active: Arc<AtomicBool>,
    running: Arc<AtomicBool>,
    quality: ScreenQuality,
    display_index: usize,
) {
    let displays = scrap::Display::all().unwrap_or_default();
    log_fmt!("[screen] found {} displays, requested index {}", displays.len(), display_index);
    let display = match displays.into_iter().nth(display_index)
        .or_else(|| scrap::Display::all().ok().and_then(|d| d.into_iter().next()))
    {
        Some(d) => d,
        None => {
            log_fmt!("[screen] ERROR: no display found");
            active.store(false, Ordering::Relaxed);
            return;
        }
    };

    let native_w = display.width();
    let native_h = display.height();
    log_fmt!("[screen] display {}: {}x{}", display_index, native_w, native_h);

    // Cap encoder resolution to native — avoid upscaling small screens
    let enc_w = quality.width().min(native_w as u32);
    let enc_h = quality.height().min(native_h as u32);
    let enc_fps = quality.fps();
    let enc_bps = quality.bitrate_kbps();
    log_fmt!("[screen] capture_loop started, peer={}, quality={:?} ({}x{} {}fps {}kbps, native {}x{})",
        peer_addr, quality, enc_w, enc_h, enc_fps, enc_bps, native_w, native_h);

    let mut capturer = match scrap::Capturer::new(display) {
        Ok(c) => {
            log_fmt!("[screen] capturer created OK");
            c
        }
        Err(e) => {
            log_fmt!("[screen] ERROR: failed to create capturer: {e}");
            active.store(false, Ordering::Relaxed);
            return;
        }
    };

    let mut encoder = ScreenEncoder::new(enc_w, enc_h, enc_fps, enc_bps);
    log_fmt!("[screen] encoder created, starting capture loop");
    let frame_duration = Duration::from_secs_f64(1.0 / enc_fps as f64);
    let mut frame_id: u16 = 0;
    let mut frame_count: u32 = 0;

    let mut wouldblock_count: u64 = 0;

    while active.load(Ordering::Relaxed) && running.load(Ordering::Relaxed) {
        let frame_start = Instant::now();

        // Capture frame
        let bgra_frame = match capturer.frame() {
            Ok(frame) => {
                if wouldblock_count > 0 {
                    log_fmt!("[screen] got frame after {} WouldBlock retries", wouldblock_count);
                    wouldblock_count = 0;
                }
                let stride = frame.len() / native_h;
                if stride == native_w * 4 {
                    frame.to_vec()
                } else {
                    let mut clean = Vec::with_capacity(native_w * native_h * 4);
                    for row in 0..native_h {
                        let start = row * stride;
                        clean.extend_from_slice(&frame[start..start + native_w * 4]);
                    }
                    clean
                }
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                wouldblock_count += 1;
                if wouldblock_count % 1000 == 0 {
                    log_fmt!("[screen] WouldBlock x{} (still waiting for frame)", wouldblock_count);
                }
                std::thread::sleep(Duration::from_millis(5));
                continue;
            }
            Err(e) => {
                log_fmt!("[screen] capture error: {e}");
                std::thread::sleep(Duration::from_millis(10));
                continue;
            }
        };

        // Scale to target resolution if needed
        let scaled = scale_bgra(
            &bgra_frame,
            native_w,
            native_h,
            enc_w as usize,
            enc_h as usize,
        );

        // Encode
        let force_keyframe = frame_count % KEYFRAME_INTERVAL == 0;
        let enc_start = Instant::now();
        let encoded = encoder.encode(&scaled, force_keyframe);
        let enc_ms = enc_start.elapsed().as_millis();

        if encoded.is_empty() {
            log_fmt!("[screen] frame #{} encode returned empty ({}ms)", frame_count, enc_ms);
            frame_count += 1;
            continue;
        }

        // Fragment
        let chunks = ScreenEncoder::fragment(&encoded, frame_id, force_keyframe);

        // Encrypt and send each chunk (with pacing to avoid burst loss)
        let send_start = Instant::now();
        for (i, chunk) in chunks.iter().enumerate() {
            let packet = session.encrypt_packet(PKT_SCREEN, chunk);
            let _ = socket.send_to(&packet, peer_addr);
            // Pace chunks: 200μs between packets prevents UDP burst loss
            if i + 1 < chunks.len() {
                std::thread::sleep(Duration::from_micros(200));
            }
        }
        let send_ms = send_start.elapsed().as_millis();

        if frame_count % 30 == 0 || frame_count < 5 {
            log_fmt!("[screen] frame #{} sent ({} bytes, {} chunks, kf={}, enc={}ms, send={}ms)",
                frame_count, encoded.len(), chunks.len(), force_keyframe, enc_ms, send_ms);
        }

        frame_id = frame_id.wrapping_add(1);
        frame_count += 1;

        // Sleep to maintain target FPS
        let elapsed = frame_start.elapsed();
        if elapsed < frame_duration {
            std::thread::sleep(frame_duration - elapsed);
        }
    }

    active.store(false, Ordering::Relaxed);
}
