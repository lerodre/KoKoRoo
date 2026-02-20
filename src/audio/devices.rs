use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use ringbuf::traits::{Consumer, Producer, Split};
use ringbuf::HeapRb;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// Lists all available audio devices (inputs and outputs).
pub fn list_devices() {
    let host = cpal::default_host();

    println!("=== Audio Host: {:?} ===", host.id());
    println!();

    // Input devices (microphones)
    println!("-- Input Devices (Microphones) --");
    match host.input_devices() {
        Ok(devices) => {
            for (i, dev) in devices.enumerate() {
                let name = dev.name().unwrap_or_else(|_| "unknown".into());
                let config = dev.default_input_config();
                match config {
                    Ok(cfg) => println!("  [{i}] {name} — {} ch, {}Hz, {:?}",
                        cfg.channels(), cfg.sample_rate().0, cfg.sample_format()),
                    Err(_) => println!("  [{i}] {name} — (no config available)"),
                }
            }
        }
        Err(e) => eprintln!("  Error listing input devices: {e}"),
    }

    println!();

    // Output devices (speakers)
    println!("-- Output Devices (Speakers) --");
    match host.output_devices() {
        Ok(devices) => {
            for (i, dev) in devices.enumerate() {
                let name = dev.name().unwrap_or_else(|_| "unknown".into());
                let config = dev.default_output_config();
                match config {
                    Ok(cfg) => println!("  [{i}] {name} — {} ch, {}Hz, {:?}",
                        cfg.channels(), cfg.sample_rate().0, cfg.sample_format()),
                    Err(_) => println!("  [{i}] {name} — (no config available)"),
                }
            }
        }
        Err(e) => eprintln!("  Error listing output devices: {e}"),
    }
}

/// Captures audio from the mic and plays it back through speakers in real time.
/// This is a loopback test — you'll hear yourself through your speakers/headphones.
pub fn mic_test() {
    let host = cpal::default_host();

    // Get default input (mic) and output (speakers)
    let input_device = host.default_input_device()
        .expect("No input device (microphone) found");
    let output_device = host.default_output_device()
        .expect("No output device (speakers) found");

    println!("Mic:      {}", input_device.name().unwrap_or_default());
    println!("Speakers: {}", output_device.name().unwrap_or_default());

    // Use a config that works for both input and output
    // Mono, 48kHz, f32 — this is what we'll use for voice
    let config = cpal::StreamConfig {
        channels: 1,       // mono is enough for voice
        sample_rate: cpal::SampleRate(48000),
        buffer_size: cpal::BufferSize::Default,
    };

    println!("Config:   1 ch, 48000 Hz, f32");
    println!();
    println!("Loopback active — speak into your mic, you should hear yourself.");
    println!("Press Ctrl+C to stop.");
    println!();

    // Ring buffer: lock-free, single-producer single-consumer
    // Size = 48000 samples = 1 second of audio buffer
    let ring = HeapRb::<f32>::new(48000);
    let (mut producer, mut consumer) = ring.split();

    // Flag to track errors
    let running = Arc::new(AtomicBool::new(true));
    let running_in = running.clone();
    let running_out = running.clone();

    // INPUT STREAM: mic → ring buffer
    let input_stream = input_device.build_input_stream(
        &config,
        move |data: &[f32], _: &cpal::InputCallbackInfo| {
            // Push mic samples into the ring buffer
            for &sample in data {
                let _ = producer.try_push(sample);
            }
        },
        move |err| {
            eprintln!("Input stream error: {err}");
            running_in.store(false, Ordering::Relaxed);
        },
        None,  // no timeout
    ).expect("Failed to build input stream");

    // OUTPUT STREAM: ring buffer → speakers
    let output_stream = output_device.build_output_stream(
        &config,
        move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
            // Pull samples from ring buffer into speaker output
            for sample in data.iter_mut() {
                *sample = consumer.try_pop().unwrap_or(0.0);
            }
        },
        move |err| {
            eprintln!("Output stream error: {err}");
            running_out.store(false, Ordering::Relaxed);
        },
        None,
    ).expect("Failed to build output stream");

    // Start both streams
    input_stream.play().expect("Failed to start input stream");
    output_stream.play().expect("Failed to start output stream");

    // Keep running until Ctrl+C or error
    while running.load(Ordering::Relaxed) {
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}
