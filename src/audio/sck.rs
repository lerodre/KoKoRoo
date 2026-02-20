//! macOS system audio capture via ScreenCaptureKit (macOS 13+).
//!
//! Provides native system audio loopback without requiring
//! third-party virtual audio drivers (BlackHole, Soundflower, etc.).

use ringbuf::traits::Producer;
use ringbuf::HeapProd;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{define_class, msg_send, AllocAnyThread, DefinedClass};
use objc2_core_media::CMSampleBuffer;
use objc2_foundation::{NSArray, NSError, NSObject, NSObjectProtocol};
use objc2_screen_capture_kit::{
    SCContentFilter, SCShareableContent, SCStream, SCStreamConfiguration,
    SCStreamOutput, SCStreamOutputType, SCWindow,
};

// CoreMedia C function for audio buffer extraction
extern "C" {
    fn CMSampleBufferGetAudioBufferListWithRetainedBlockBuffer(
        sbuf: *const std::ffi::c_void,
        buffer_list_size_needed_out: *mut usize,
        buffer_list_out: *mut AudioBufList,
        buffer_list_size: usize,
        structure_allocator: *const std::ffi::c_void,
        block_allocator: *const std::ffi::c_void,
        flags: u32,
        block_buffer_out: *mut *mut std::ffi::c_void,
    ) -> i32;
    fn CFRelease(cf: *mut std::ffi::c_void);
}

#[repr(C)]
struct AudioBuf {
    number_channels: u32,
    data_byte_size: u32,
    data: *mut std::ffi::c_void,
}

#[repr(C)]
struct AudioBufList {
    number_buffers: u32,
    buffers: [AudioBuf; 2], // SCK stereo non-interleaved needs 2 buffers
}

/// Wrapper that keeps an SCStream alive; stops capture on drop.
pub struct SckStream {
    stream: Retained<SCStream>,
    _handler: Retained<AudioOutputHandler>,
}

impl SckStream {
    /// Recover the ring buffer producer before dropping.
    /// Must be called before drop to reuse the producer for future captures.
    pub fn take_producer(&self) -> Option<HeapProd<f32>> {
        self._handler.take_producer()
    }
}

impl Drop for SckStream {
    fn drop(&mut self) {
        let done = Arc::new((Mutex::new(false), Condvar::new()));
        let done_c = done.clone();
        let block = RcBlock::new(move |_error: *mut NSError| {
            let (lock, cvar) = &*done_c;
            *lock.lock().unwrap() = true;
            cvar.notify_one();
        });
        unsafe {
            self.stream.stopCaptureWithCompletionHandler(Some(&block));
        }
        let (lock, cvar) = &*done;
        let _ = cvar.wait_timeout_while(
            lock.lock().unwrap(),
            std::time::Duration::from_secs(2),
            |d| !*d,
        );
        log_fmt!("[sck_audio] capture stopped");
    }
}

/// Returns available system audio sources.
pub fn list_system_audio_sources() -> Vec<String> {
    vec!["System Audio".to_string()]
}

struct AudioOutputIvars {
    producer: Mutex<Option<HeapProd<f32>>>,
    active: Arc<AtomicBool>,
    sample_count: std::sync::atomic::AtomicU64,
    callback_count: std::sync::atomic::AtomicU64,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[ivars = AudioOutputIvars]
    #[name = "HostelDAudioOutput"]
    struct AudioOutputHandler;

    unsafe impl NSObjectProtocol for AudioOutputHandler {}

    unsafe impl SCStreamOutput for AudioOutputHandler {
        #[unsafe(method(stream:didOutputSampleBuffer:ofType:))]
        fn _stream_did_output(
            &self,
            _stream: &SCStream,
            sample_buffer: &CMSampleBuffer,
            of_type: SCStreamOutputType,
        ) {
            let cb = self.ivars().callback_count.fetch_add(1, Ordering::Relaxed);
            // SCStreamOutputType: Screen=0, Audio=1
            if of_type.0 != 1 {
                if cb < 3 {
                    log_fmt!("[sck_audio] callback #{}: type={} (skipping non-audio)", cb, of_type.0);
                }
                return;
            }
            if !self.ivars().active.load(Ordering::Relaxed) {
                return;
            }
            let before = self.ivars().sample_count.load(Ordering::Relaxed);
            self.extract_audio(sample_buffer);
            let after = self.ivars().sample_count.load(Ordering::Relaxed);
            let new_samples = after - before;
            // Log first few callbacks and then every ~1 second (50 callbacks at 48kHz/960 samples)
            if cb < 5 || (cb % 50 == 0) {
                log_fmt!("[sck_audio] audio callback #{}: +{} samples (total={})", cb, new_samples, after);
            }
        }
    }
);

impl AudioOutputHandler {
    fn new(producer: HeapProd<f32>, active: Arc<AtomicBool>) -> Retained<Self> {
        let this = Self::alloc();
        let this = this.set_ivars(AudioOutputIvars {
            producer: Mutex::new(Some(producer)),
            active,
            sample_count: std::sync::atomic::AtomicU64::new(0),
            callback_count: std::sync::atomic::AtomicU64::new(0),
        });
        unsafe { msg_send![super(this), init] }
    }

    /// Take the producer back (used to recover on failure before capture starts).
    fn take_producer(&self) -> Option<HeapProd<f32>> {
        self.ivars().producer.lock().ok()?.take()
    }

    fn extract_audio(&self, sample_buffer: &CMSampleBuffer) {
        let cb = self.ivars().callback_count.load(Ordering::Relaxed);
        let mut abl = std::mem::MaybeUninit::<AudioBufList>::uninit();
        let mut block_buffer: *mut std::ffi::c_void = std::ptr::null_mut();

        let status = unsafe {
            CMSampleBufferGetAudioBufferListWithRetainedBlockBuffer(
                sample_buffer as *const CMSampleBuffer as *const std::ffi::c_void,
                std::ptr::null_mut(),
                abl.as_mut_ptr(),
                std::mem::size_of::<AudioBufList>(),
                std::ptr::null(),
                std::ptr::null(),
                0,
                &mut block_buffer,
            )
        };

        if status != 0 {
            if cb < 3 { log_fmt!("[sck_audio] extract_audio: CMSampleBuffer status={}", status); }
            return;
        }

        let abl = unsafe { abl.assume_init() };

        if abl.number_buffers == 0 {
            if cb < 3 { log_fmt!("[sck_audio] extract_audio: no buffers"); }
            if !block_buffer.is_null() {
                unsafe { CFRelease(block_buffer) };
            }
            return;
        }

        let num_buffers = abl.number_buffers as usize;
        if cb < 3 {
            for i in 0..num_buffers.min(2) {
                let b = &abl.buffers[i];
                log_fmt!("[sck_audio] extract_audio: buf[{}] channels={}, byte_size={}", i, b.number_channels, b.data_byte_size);
            }
        }

        let mut pushed = 0u64;
        if let Ok(mut guard) = self.ivars().producer.lock() {
            if let Some(ref mut producer) = *guard {
                if num_buffers == 1 {
                    // Interleaved: one buffer with N channels
                    let buf = &abl.buffers[0];
                    let channels = buf.number_channels as usize;
                    let byte_size = buf.data_byte_size as usize;
                    if channels > 0 && byte_size % 4 == 0 && !buf.data.is_null() {
                        let data = unsafe { std::slice::from_raw_parts(buf.data as *const u8, byte_size) };
                        if channels <= 1 {
                            for chunk in data.chunks_exact(4) {
                                let s = f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
                                let _: Result<(), f32> = producer.try_push(s);
                                pushed += 1;
                            }
                        } else {
                            let frame_bytes = channels * 4;
                            for frame in data.chunks_exact(frame_bytes) {
                                let mut sum = 0.0f32;
                                for ch in 0..channels {
                                    let off = ch * 4;
                                    sum += f32::from_le_bytes([frame[off], frame[off+1], frame[off+2], frame[off+3]]);
                                }
                                let _ = producer.try_push(sum / channels as f32);
                                pushed += 1;
                            }
                        }
                    }
                } else {
                    // Non-interleaved: N separate mono buffers, one per channel
                    // Read each buffer as f32 slices, then average per-sample across channels
                    let mut channel_slices: Vec<&[u8]> = Vec::new();
                    let mut min_samples = usize::MAX;
                    for i in 0..num_buffers.min(2) {
                        let b = &abl.buffers[i];
                        if b.data.is_null() || b.data_byte_size == 0 || b.data_byte_size % 4 != 0 {
                            continue;
                        }
                        let slice = unsafe { std::slice::from_raw_parts(b.data as *const u8, b.data_byte_size as usize) };
                        min_samples = min_samples.min(slice.len() / 4);
                        channel_slices.push(slice);
                    }
                    let n_ch = channel_slices.len();
                    if n_ch > 0 {
                        for s in 0..min_samples {
                            let off = s * 4;
                            let mut sum = 0.0f32;
                            for ch_data in &channel_slices {
                                sum += f32::from_le_bytes([ch_data[off], ch_data[off+1], ch_data[off+2], ch_data[off+3]]);
                            }
                            let _ = producer.try_push(sum / n_ch as f32);
                            pushed += 1;
                        }
                    }
                }
            }
        }
        self.ivars().sample_count.fetch_add(pushed, Ordering::Relaxed);

        if !block_buffer.is_null() {
            unsafe { CFRelease(block_buffer) };
        }
    }
}

/// Start capturing system audio via ScreenCaptureKit.
/// Returns (Some(stream), None) on success, (None, Some(producer)) on failure.
pub fn start_capture(
    producer: HeapProd<f32>,
    active: Arc<AtomicBool>,
) -> (Option<SckStream>, Option<HeapProd<f32>>) {
    // 1. Get shareable content (async → sync)
    log_fmt!("[sck_audio] requesting shareable content...");
    let content = match get_shareable_content() {
        Some(c) => c,
        None => {
            log_fmt!("[sck_audio] failed to get shareable content (Screen Recording permission needed?)");
            return (None, Some(producer));
        }
    };

    // 2. Get first display
    let displays = unsafe { content.displays() };
    if displays.count() == 0 {
        log_fmt!("[sck_audio] no displays found");
        return (None, Some(producer));
    }
    let display = displays.objectAtIndex(0);
    log_fmt!("[sck_audio] using display 0");

    // 3. Content filter: capture display, exclude no windows
    let empty_windows: Retained<NSArray<SCWindow>> = NSArray::new();
    let filter = unsafe {
        SCContentFilter::initWithDisplay_excludingWindows(
            SCContentFilter::alloc(),
            &display,
            &empty_windows,
        )
    };

    // 4. Configure: minimal video (2x2), audio at 48kHz stereo
    let config = unsafe { SCStreamConfiguration::new() };
    unsafe {
        config.setWidth(2);
        config.setHeight(2);
        config.setCapturesAudio(true);
        config.setSampleRate(48000);
        config.setChannelCount(2);
        config.setExcludesCurrentProcessAudio(true);
    }

    // 5. Create output handler (takes ownership of producer)
    let handler = AudioOutputHandler::new(producer, active);

    // 6. Create stream (no delegate)
    let stream = unsafe {
        SCStream::initWithFilter_configuration_delegate(
            SCStream::alloc(),
            &filter,
            &config,
            None,
        )
    };

    // 7. Register for audio output
    let proto_handler = ProtocolObject::from_ref(&*handler);
    if let Err(e) = unsafe {
        stream.addStreamOutput_type_sampleHandlerQueue_error(
            proto_handler,
            SCStreamOutputType::Audio,
            None,
        )
    } {
        log_fmt!("[sck_audio] add output handler failed: {:?}", e);
        let p = handler.take_producer();
        return (None, p);
    }

    // 8. Start capture (async → sync)
    log_fmt!("[sck_audio] starting capture...");
    let ok = Arc::new((Mutex::new(None::<bool>), Condvar::new()));
    let ok_c = ok.clone();
    let start_block = RcBlock::new(move |error: *mut NSError| {
        let (lock, cvar) = &*ok_c;
        *lock.lock().unwrap() = Some(error.is_null());
        cvar.notify_one();
    });
    unsafe {
        stream.startCaptureWithCompletionHandler(Some(&start_block));
    }

    let (lock, cvar) = &*ok;
    let guard = cvar
        .wait_timeout_while(
            lock.lock().unwrap(),
            std::time::Duration::from_secs(5),
            |s| s.is_none(),
        )
        .unwrap()
        .0;

    match *guard {
        Some(true) => {
            log_fmt!("[sck_audio] capture started (48kHz stereo→mono)");
            (
                Some(SckStream {
                    stream,
                    _handler: handler,
                }),
                None,
            )
        }
        _ => {
            log_fmt!("[sck_audio] startCapture failed or timed out");
            let p = handler.take_producer();
            (None, p)
        }
    }
}

fn get_shareable_content() -> Option<Retained<SCShareableContent>> {
    let result: Arc<(Mutex<Option<Retained<SCShareableContent>>>, Condvar)> =
        Arc::new((Mutex::new(None), Condvar::new()));
    let res_c = result.clone();

    let block = RcBlock::new(
        move |content: *mut SCShareableContent, _error: *mut NSError| {
            let (lock, cvar) = &*res_c;
            let mut guard = lock.lock().unwrap();
            if !content.is_null() {
                *guard = unsafe { Retained::retain(content) };
            }
            cvar.notify_one();
        },
    );

    unsafe {
        SCShareableContent::getShareableContentWithCompletionHandler(&block);
    }

    let (lock, cvar) = &*result;
    let guard = cvar
        .wait_timeout_while(
            lock.lock().unwrap(),
            std::time::Duration::from_secs(5),
            |r| r.is_none(),
        )
        .unwrap()
        .0;

    guard.clone()
}
