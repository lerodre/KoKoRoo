mod devices;
pub mod system;
#[cfg(target_os = "macos")]
pub mod sck;
#[cfg(windows)]
mod process_loopback;
#[cfg(target_os = "linux")]
mod pipewire_capture;

pub use devices::{list_devices, mic_test};
pub use system::{SysAudioStream, list_loopback_devices, start_system_audio_capture};
