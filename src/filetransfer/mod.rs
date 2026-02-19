pub mod protocol;
pub mod sender;
pub mod receiver;

/// Maximum data bytes per FILE_CHUNK packet.
/// UDP-safe size: 1200 payload + headers stays well under typical 1280 IPv6 MTU.
pub const CHUNK_DATA_SIZE: usize = 1200;

/// Max chunks to send per daemon tick.
/// 1024 × 1200 ≈ 1.2 MB per tick. At 20 ticks/s ≈ 24 MB/s.
/// Combined with 16 MB socket buffers this avoids packet loss.
pub const CHUNKS_PER_TICK: usize = 1024;

/// Offer timeout: cancel if no accept/reject within 30 seconds.
pub const OFFER_TIMEOUT_SECS: u64 = 30;

/// Stale transfer timeout: cancel if no progress for this many seconds.
pub const STALE_TIMEOUT_SECS: u64 = 30;

/// How often to emit progress events to the GUI (milliseconds).
pub const PROGRESS_INTERVAL_MS: u64 = 500;

/// Cancel reason codes.
pub const CANCEL_USER: u8 = 0x01;

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
