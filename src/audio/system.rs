#[cfg(not(target_os = "macos"))]
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
#[cfg(not(target_os = "macos"))]
use ringbuf::traits::Producer;
use ringbuf::HeapProd;
use std::sync::atomic::AtomicBool;
#[cfg(not(target_os = "macos"))]
use std::sync::atomic::Ordering;
use std::sync::Arc;

/// Wrapper enum so voice.rs can hold either a cpal or SCK stream handle.
#[allow(dead_code)]
pub enum SysAudioStream {
    Cpal(cpal::Stream),
    #[cfg(target_os = "macos")]
    Sck(super::sck::SckStream),
}

impl SysAudioStream {
    /// Recover the ring buffer producer before dropping, so it can be reused.
    pub fn take_producer(&self) -> Option<ringbuf::HeapProd<f32>> {
        match self {
            #[cfg(target_os = "macos")]
            SysAudioStream::Sck(s) => s.take_producer(),
            _ => None, // cpal streams don't hold the producer
        }
    }
}

/// List available loopback/monitor audio devices.
///
/// - **Windows**: Returns output device names (each can be used for WASAPI loopback).
/// - **Linux**: Returns input device names containing "Monitor".
pub fn list_loopback_devices() -> Vec<String> {
    #[cfg(target_os = "macos")]
    {
        return super::sck::list_system_audio_sources();
    }

    #[cfg(not(target_os = "macos"))]
    {
        let host = cpal::default_host();
        let mut names = Vec::new();

        #[cfg(windows)]
        {
            if let Ok(devices) = host.output_devices() {
                for dev in devices {
                    if let Ok(name) = dev.name() {
                        names.push(name);
                    }
                }
            }
        }

        #[cfg(target_os = "linux")]
        {
            if let Ok(devices) = host.input_devices() {
                for dev in devices {
                    if let Ok(name) = dev.name() {
                        if name.to_lowercase().contains("monitor") {
                            names.push(name);
                        }
                    }
                }
            }
        }

        names
    }
}

/// Start capturing system/desktop audio into a ring buffer.
///
/// - **Windows**: Uses WASAPI loopback (`output_device.build_input_stream()`).
/// - **Linux**: Finds a PipeWire/PulseAudio "Monitor" input device, or falls back
///   to `default_output_device().build_input_stream()`.
///
/// If `device_name` is Some, find that specific device instead of using default.
/// An empty string means use default.
///
/// Returns `Some(Stream)` on success — the stream captures while alive and stops on drop.
/// Returns (stream, leftover_producer). On failure, the producer is returned
/// so the caller can restore it for future retries.
pub fn start_system_audio_capture(
    producer: HeapProd<f32>,
    active: Arc<AtomicBool>,
    device_name: Option<&str>,
) -> (Option<SysAudioStream>, Option<HeapProd<f32>>) {
    #[cfg(windows)]
    {
        (capture_wasapi_loopback(producer, active, device_name).map(SysAudioStream::Cpal), None)
    }
    #[cfg(target_os = "linux")]
    {
        (capture_linux_monitor(producer, active, device_name).map(SysAudioStream::Cpal), None)
    }
    #[cfg(target_os = "macos")]
    {
        let _ = device_name;
        let (stream, leftover) = super::sck::start_capture(producer, active);
        (stream.map(SysAudioStream::Sck), leftover)
    }
    #[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
    {
        let _ = (active, device_name);
        log_fmt!("[sysaudio] system audio capture not supported on this platform");
        (None, Some(producer))
    }
}

/// Windows: WASAPI loopback — call `build_input_stream` on an output device.
/// If `device_name` is Some and non-empty, find that specific device; otherwise use default.
#[cfg(windows)]
fn capture_wasapi_loopback(
    producer: HeapProd<f32>,
    active: Arc<AtomicBool>,
    device_name: Option<&str>,
) -> Option<cpal::Stream> {
    let host = cpal::default_host();
    let device = match device_name {
        Some(name) if !name.is_empty() => {
            log_fmt!("[sysaudio] looking for output device: {}", name);
            host.output_devices().ok()?
                .find(|d| d.name().map(|n| n == name).unwrap_or(false))
                .or_else(|| {
                    log_fmt!("[sysaudio] device '{}' not found, falling back to default", name);
                    host.default_output_device()
                })?
        }
        _ => host.default_output_device()?,
    };
    let name = device.name().unwrap_or_default();
    log_fmt!("[sysaudio] WASAPI loopback on: {}", name);

    let config = device.default_output_config().ok()?;
    let channels = config.channels() as usize;
    let sample_rate = config.sample_rate().0;
    log_fmt!("[sysaudio] config: {}ch {}Hz {:?}", channels, sample_rate, config.sample_format());

    let stream_config = cpal::StreamConfig {
        channels: channels as u16,
        sample_rate: cpal::SampleRate(sample_rate),
        buffer_size: cpal::BufferSize::Default,
    };

    build_capture_stream(&device, &stream_config, channels, producer, active)
}

/// Linux: find a "Monitor" input device (PipeWire/PulseAudio loopback), or fall back
/// to `default_output_device().build_input_stream()`.
/// If `device_name` is Some and non-empty, find that specific Monitor device.
#[cfg(target_os = "linux")]
fn capture_linux_monitor(
    producer: HeapProd<f32>,
    active: Arc<AtomicBool>,
    device_name: Option<&str>,
) -> Option<cpal::Stream> {
    let host = cpal::default_host();

    // Find a monitor device (don't consume producer yet)
    let monitor_device = host.input_devices().ok().and_then(|devices| {
        for dev in devices {
            let name = dev.name().unwrap_or_default();
            if let Some(req) = device_name {
                if !req.is_empty() && name != req {
                    continue;
                }
            }
            if name.to_lowercase().contains("monitor") {
                log_fmt!("[sysaudio] found monitor device: {}", name);
                return Some(dev);
            }
        }
        None
    });

    if let Some(dev) = monitor_device {
        if let Ok(config) = dev.default_input_config() {
            let channels = config.channels() as usize;
            let stream_config = cpal::StreamConfig {
                channels: channels as u16,
                sample_rate: cpal::SampleRate(config.sample_rate().0),
                buffer_size: cpal::BufferSize::Default,
            };
            if let Some(stream) = build_capture_stream(&dev, &stream_config, channels, producer, active) {
                return Some(stream);
            }
            return None; // producer consumed
        }
    }

    // Fallback: try build_input_stream on the default output device
    log_fmt!("[sysaudio] no monitor device found, trying output device loopback");
    let device = host.default_output_device()?;
    let config = device.default_output_config().ok()?;
    let channels = config.channels() as usize;
    let stream_config = cpal::StreamConfig {
        channels: channels as u16,
        sample_rate: cpal::SampleRate(config.sample_rate().0),
        buffer_size: cpal::BufferSize::Default,
    };
    build_capture_stream(&device, &stream_config, channels, producer, active)
}

#[cfg(not(target_os = "macos"))]
fn build_capture_stream(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    channels: usize,
    mut producer: HeapProd<f32>,
    active: Arc<AtomicBool>,
) -> Option<cpal::Stream> {
    let stream = device.build_input_stream(
        config,
        move |data: &[f32], _: &cpal::InputCallbackInfo| {
            if !active.load(Ordering::Relaxed) {
                return;
            }
            if channels <= 1 {
                for &sample in data {
                    let _ = producer.try_push(sample);
                }
            } else {
                // Stereo (or multi-channel) → mono downmix
                for frame in data.chunks(channels) {
                    let mono: f32 = frame.iter().sum::<f32>() / channels as f32;
                    let _ = producer.try_push(mono);
                }
            }
        },
        move |err| {
            log_fmt!("[sysaudio] capture error: {}", err);
        },
        None,
    ).ok()?;

    stream.play().ok()?;
    log_fmt!("[sysaudio] capture stream started");
    Some(stream)
}
