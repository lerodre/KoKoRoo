pub mod protocol;
pub mod sender;
pub mod receiver;

/// Maximum data bytes per FILE_CHUNK packet (matches screen sharing chunk size).
pub const CHUNK_DATA_SIZE: usize = 1200;

/// Sliding window size: max chunks in flight before requiring an ACK.
pub const WINDOW_SIZE: u32 = 32;

/// Interval between chunks (microseconds) — same pacing as screen sharing.
pub const CHUNK_PACING_US: u64 = 200;

/// ACK timeout: retransmit if no ACK received within this duration.
pub const ACK_TIMEOUT_MS: u64 = 2000;

/// Offer timeout: cancel if no accept/reject within 30 seconds.
pub const OFFER_TIMEOUT_SECS: u64 = 30;

/// How often to emit progress events to the GUI (milliseconds).
pub const PROGRESS_INTERVAL_MS: u64 = 500;

/// Cancel reason codes.
pub const CANCEL_USER: u8 = 0x01;
pub const CANCEL_ERROR: u8 = 0x02;
pub const CANCEL_TIMEOUT: u8 = 0x03;

/// Format a byte size as a human-readable string.
pub fn format_size(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{} B", bytes)
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else if bytes < 1024 * 1024 * 1024 {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    } else {
        format!("{:.2} GB", bytes as f64 / (1024.0 * 1024.0 * 1024.0))
    }
}
