//! Windows WASAPI process-excluded audio loopback capture.
//!
//! Uses the Application Loopback API (Windows 10 Build 20348+) to capture
//! all system audio EXCEPT hostelD's own output, preventing voice echo loops
//! when sharing system audio during calls.

use ringbuf::traits::Producer;
use ringbuf::HeapProd;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;

use wasapi::{AudioCaptureClient, AudioClient, DeviceEnumerator, Direction, SampleType, StreamMode};

/// Stream handle for WASAPI process-excluded loopback capture.
///
/// Captures all system audio except the current process tree.
/// The capture runs in a dedicated thread and pushes mono f32 samples
/// into a lock-free ring buffer.
pub struct WasapiLoopbackStream {
    active: Arc<AtomicBool>,
    capture_thread: Option<thread::JoinHandle<()>>,
    producer: Arc<Mutex<Option<HeapProd<f32>>>>,
}

impl WasapiLoopbackStream {
    /// Recover the ring buffer producer for reuse after stopping capture.
    pub fn take_producer(&self) -> Option<HeapProd<f32>> {
        self.producer.lock().ok()?.take()
    }
}

impl Drop for WasapiLoopbackStream {
    fn drop(&mut self) {
        self.active.store(false, Ordering::Relaxed);
        if let Some(t) = self.capture_thread.take() {
            let _ = t.join();
        }
    }
}

/// Start capturing system audio while excluding the current process.
///
/// Returns `(Some(stream), None)` on success.
/// Returns `(None, Some(producer))` if the API is unavailable or initialization
/// fails, giving back the producer so the caller can retry with a fallback method.
pub fn start_capture(
    producer: HeapProd<f32>,
    active: Arc<AtomicBool>,
) -> (Option<WasapiLoopbackStream>, Option<HeapProd<f32>>) {
    let pid = std::process::id();

    // Quick API availability check before consuming the producer
    if AudioClient::new_application_loopback_client(pid, false).is_err() {
        log_fmt!("[sysaudio] WASAPI process loopback not available (Windows too old?)");
        return (None, Some(producer));
    }

    let shared_producer = Arc::new(Mutex::new(Some(producer)));
    let thread_producer = shared_producer.clone();
    let thread_active = active.clone();

    // Channel for the thread to confirm successful initialization
    let (ready_tx, ready_rx) = mpsc::sync_channel::<Result<(), String>>(1);

    let handle = match thread::Builder::new()
        .name("wasapi-loopback".into())
        .spawn(move || {
            if let Err(e) = wasapi::initialize_mta().ok() {
                let _ = ready_tx.send(Err(format!("COM init: {e}")));
                return;
            }
            capture_loop(pid, thread_producer, thread_active, ready_tx);
        }) {
        Ok(h) => h,
        Err(_) => {
            let leftover = shared_producer.lock().ok().and_then(|mut g| g.take());
            return (None, leftover);
        }
    };

    // Wait for the thread to confirm capture is running (up to 3s)
    match ready_rx.recv_timeout(std::time::Duration::from_secs(3)) {
        Ok(Ok(())) => {
            log_fmt!("[sysaudio] WASAPI process-excluded loopback confirmed (PID {})", pid);
            (
                Some(WasapiLoopbackStream {
                    active,
                    capture_thread: Some(handle),
                    producer: shared_producer,
                }),
                None,
            )
        }
        Ok(Err(e)) => {
            log_fmt!("[sysaudio] WASAPI process loopback init failed: {}", e);
            let _ = handle.join();
            let leftover = shared_producer.lock().ok().and_then(|mut g| g.take());
            (None, leftover)
        }
        Err(_) => {
            log_fmt!("[sysaudio] WASAPI process loopback init timed out");
            active.store(false, Ordering::Relaxed);
            let _ = handle.join();
            let leftover = shared_producer.lock().ok().and_then(|mut g| g.take());
            (None, leftover)
        }
    }
}

/// Main capture loop running in a dedicated thread.
/// Sends Ok(()) on `ready_tx` once capture is confirmed running,
/// or Err(reason) if initialization fails.
fn capture_loop(
    pid: u32,
    producer: Arc<Mutex<Option<HeapProd<f32>>>>,
    active: Arc<AtomicBool>,
    ready_tx: mpsc::SyncSender<Result<(), String>>,
) {
    let result = init_capture(pid);
    let (client, capture_client, event, channels, bytes_per_sample, is_float) = match result {
        Ok(v) => v,
        Err(e) => {
            let _ = ready_tx.send(Err(e));
            return;
        }
    };

    // Signal success to the caller
    let _ = ready_tx.send(Ok(()));

    let bytes_per_frame = bytes_per_sample * channels;
    let mut raw_buf = vec![0u8; 48000 * bytes_per_frame]; // ~1s buffer

    while active.load(Ordering::Relaxed) {
        // Wait for audio data with timeout so we can check `active` periodically
        if event.wait_for_event(100).is_err() {
            continue;
        }

        read_available_frames(
            &capture_client,
            &producer,
            &mut raw_buf,
            channels,
            bytes_per_sample,
            is_float,
        );
    }

    client.stop_stream().ok();
    log_fmt!("[sysaudio] process loopback capture stopped");
}

/// Initialize WASAPI capture and return all the pieces needed for the loop.
fn init_capture(
    pid: u32,
) -> Result<(AudioClient, AudioCaptureClient, wasapi::Handle, usize, usize, bool), String> {
    let mut client = AudioClient::new_application_loopback_client(pid, false)
        .map_err(|e| format!("create loopback client: {e}"))?;

    // get_mixformat() returns E_NOTIMPL on process loopback clients.
    // Get the format from the default render device instead.
    let mix_format = DeviceEnumerator::new()
        .and_then(|e| e.get_default_device(&Direction::Render))
        .and_then(|d| d.get_device_format())
        .map_err(|e| format!("get mix format from default device: {e}"))?;

    let channels = mix_format.get_nchannels() as usize;
    let sample_rate = mix_format.get_samplespersec();
    let bits = mix_format.get_bitspersample();
    let sample_type = mix_format
        .get_subformat()
        .map_err(|e| format!("get subformat: {e}"))?;

    log_fmt!(
        "[sysaudio] mix format: {}ch {}Hz {}bit {:?}",
        channels, sample_rate, bits, sample_type
    );

    client
        .initialize_client(
            &mix_format,
            &Direction::Capture,
            &StreamMode::EventsShared {
                autoconvert: true,
                buffer_duration_hns: 0,
            },
        )
        .map_err(|e| format!("initialize client: {e}"))?;

    let event = client
        .set_get_eventhandle()
        .map_err(|e| format!("get event handle: {e}"))?;

    client
        .start_stream()
        .map_err(|e| format!("start stream: {e}"))?;

    let capture_client = client
        .get_audiocaptureclient()
        .map_err(|e| format!("get capture client: {e}"))?;

    let bytes_per_sample = (bits / 8) as usize;
    let is_float = matches!(sample_type, SampleType::Float);

    log_fmt!("[sysaudio] capture loop started, bytes_per_frame={}", bytes_per_sample * channels);

    Ok((client, capture_client, event, channels, bytes_per_sample, is_float))
}

/// Read all available frames from the capture client and push mono samples.
fn read_available_frames(
    capture_client: &AudioCaptureClient,
    producer: &Arc<Mutex<Option<HeapProd<f32>>>>,
    raw_buf: &mut Vec<u8>,
    channels: usize,
    bytes_per_sample: usize,
    is_float: bool,
) {
    loop {
        let packet_size = match capture_client.get_next_packet_size() {
            Ok(Some(n)) if n > 0 => n as usize,
            _ => break,
        };

        let bytes_per_frame = bytes_per_sample * channels;
        let needed = packet_size * bytes_per_frame;
        if raw_buf.len() < needed {
            raw_buf.resize(needed, 0);
        }

        let (frames_read, _buffer_info) = match capture_client.read_from_device(&mut raw_buf[..needed]) {
            Ok(r) => r,
            Err(_) => break,
        };

        if frames_read == 0 {
            break;
        }

        let mut guard = match producer.lock() {
            Ok(g) => g,
            Err(_) => break,
        };
        let prod = match guard.as_mut() {
            Some(p) => p,
            None => break,
        };

        let total_bytes = frames_read as usize * bytes_per_frame;
        push_samples_mono(prod, &raw_buf[..total_bytes], channels, bytes_per_sample, is_float);
    }
}

/// Convert raw audio bytes to mono f32 and push to ring buffer.
fn push_samples_mono(
    producer: &mut HeapProd<f32>,
    data: &[u8],
    channels: usize,
    bytes_per_sample: usize,
    is_float: bool,
) {
    let bytes_per_frame = bytes_per_sample * channels;

    for frame in data.chunks_exact(bytes_per_frame) {
        let mut mono = 0.0f32;
        for ch in 0..channels {
            let offset = ch * bytes_per_sample;
            let sample = decode_sample(&frame[offset..offset + bytes_per_sample], is_float);
            mono += sample;
        }
        mono /= channels as f32;
        let _ = producer.try_push(mono);
    }
}

/// Decode a single audio sample from raw bytes to f32.
fn decode_sample(bytes: &[u8], is_float: bool) -> f32 {
    match (is_float, bytes.len()) {
        (true, 4) => f32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
        (false, 2) => {
            let val = i16::from_le_bytes([bytes[0], bytes[1]]);
            val as f32 / i16::MAX as f32
        }
        (false, 4) => {
            let val = i32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
            val as f32 / i32::MAX as f32
        }
        (false, 3) => {
            let val = i32::from_le_bytes([0, bytes[0], bytes[1], bytes[2]]) >> 8;
            val as f32 / 8_388_607.0
        }
        _ => 0.0,
    }
}
