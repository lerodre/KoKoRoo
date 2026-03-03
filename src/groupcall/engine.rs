use cpal::traits::{DeviceTrait, StreamTrait};
use ringbuf::traits::{Consumer, Producer, Split};
use ringbuf::HeapRb;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;

use crate::group::{Group, GroupMember};

/// Opus frame size: 960 samples @ 48kHz = 20ms.
pub const FRAME_SIZE: usize = 960;

/// RNNoise frame size: 480 samples @ 48kHz = 10ms.
pub const DENOISE_FRAME: usize = 480;

/// Max encoded Opus packet size.
pub const MAX_OPUS_PACKET: usize = 512;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum GroupRole {
    Leader,
    Member,
}

/// A group chat message with sender info.
#[derive(Clone)]
pub struct GroupChatMsg {
    #[allow(dead_code)]
    pub sender_index: u16,
    pub sender_nickname: String,
    pub text: String,
}

/// Bridge between the group call engine and the GUI.
pub struct GroupCallInfo {
    pub group: Group,
    pub role: GroupRole,
    #[allow(dead_code)]
    pub channel_id: String,
    #[allow(dead_code)]
    pub running: Arc<AtomicBool>,
    #[allow(dead_code)]
    pub mic_active: Arc<AtomicBool>,
    pub chat_tx: mpsc::Sender<String>,
    pub chat_rx: mpsc::Receiver<GroupChatMsg>,
    pub roster_rx: mpsc::Receiver<Vec<GroupMember>>,
    pub local_hangup: Arc<AtomicBool>,
}

/// Audio streams + ring buffers returned by `setup_audio_streams()`.
pub struct AudioPipeline {
    pub mic_consumer: ringbuf::HeapCons<f32>,
    pub spk_producer: ringbuf::HeapProd<f32>,
    pub _input_stream: cpal::Stream,
    pub _output_stream: cpal::Stream,
}

/// Per-member decoded audio frames, keyed by sender_index.
pub type AudioFrames = Arc<Mutex<HashMap<u16, Vec<f32>>>>;

/// Set up mic input and speaker output streams with ring buffers.
pub fn setup_audio_streams(
    input_device: &cpal::Device,
    output_device: &cpal::Device,
) -> Result<AudioPipeline, String> {
    let mic_ring = HeapRb::<f32>::new(48000);
    let (mut mic_producer, mic_consumer) = mic_ring.split();
    let spk_ring = HeapRb::<f32>::new(9600);
    let (spk_producer, mut spk_consumer) = spk_ring.split();

    let input_channels = input_device.default_input_config()
        .map(|c| c.channels()).unwrap_or(1);
    let output_channels = output_device.default_output_config()
        .map(|c| c.channels()).unwrap_or(1);

    #[cfg(target_os = "macos")]
    let buf_size = cpal::BufferSize::Default;
    #[cfg(not(target_os = "macos"))]
    let buf_size = cpal::BufferSize::Fixed(FRAME_SIZE as u32);

    let input_config = cpal::StreamConfig {
        channels: input_channels,
        sample_rate: cpal::SampleRate(48000),
        buffer_size: buf_size.clone(),
    };
    let output_config = cpal::StreamConfig {
        channels: output_channels,
        sample_rate: cpal::SampleRate(48000),
        buffer_size: buf_size,
    };

    let input_stream = input_device.build_input_stream(
        &input_config,
        move |data: &[f32], _: &cpal::InputCallbackInfo| {
            if input_channels == 1 {
                for &sample in data {
                    let _ = mic_producer.try_push(sample);
                }
            } else {
                for chunk in data.chunks(input_channels as usize) {
                    let _ = mic_producer.try_push(chunk[0]);
                }
            }
        },
        |e| log_fmt!("[group] mic error: {e}"),
        None,
    ).map_err(|e| format!("Mic stream: {e}"))?;
    input_stream.play().map_err(|e| format!("Mic play: {e}"))?;

    let output_stream = output_device.build_output_stream(
        &output_config,
        move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
            if output_channels == 1 {
                for sample in data.iter_mut() {
                    *sample = spk_consumer.try_pop().unwrap_or(0.0);
                }
            } else {
                for chunk in data.chunks_mut(output_channels as usize) {
                    let s = spk_consumer.try_pop().unwrap_or(0.0);
                    for ch in chunk.iter_mut() {
                        *ch = s;
                    }
                }
            }
        },
        |e| log_fmt!("[group] speaker error: {e}"),
        None,
    ).map_err(|e| format!("Speaker stream: {e}"))?;
    output_stream.play().map_err(|e| format!("Speaker play: {e}"))?;

    Ok(AudioPipeline {
        mic_consumer,
        spk_producer,
        _input_stream: input_stream,
        _output_stream: output_stream,
    })
}

/// Spawn a mixer thread that mixes all member audio frames into the speaker buffer.
pub fn spawn_mixer_thread(
    running: Arc<AtomicBool>,
    audio_frames: AudioFrames,
    mut spk_producer: ringbuf::HeapProd<f32>,
) {
    thread::spawn(move || {
        while running.load(Ordering::Relaxed) {
            thread::sleep(std::time::Duration::from_millis(20));
            let frames = audio_frames.lock().unwrap();
            if frames.is_empty() { continue; }

            let mut mix = vec![0f32; FRAME_SIZE];
            for frame in frames.values() {
                for (i, &s) in frame.iter().enumerate() {
                    if i < FRAME_SIZE {
                        mix[i] += s;
                    }
                }
            }
            for s in mix.iter_mut() {
                *s = s.clamp(-1.0, 1.0);
            }
            drop(frames);

            for &s in &mix {
                let _ = spk_producer.try_push(s);
            }
        }
    });
}
