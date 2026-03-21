pub mod protocol;
pub mod sender;
pub mod receiver;

/// Maximum data bytes per FILE_CHUNK packet.
/// 4000 bytes + encryption overhead ≈ 4029 bytes on wire.
/// May IP-fragment on strict 1280-MTU links, but UDP reassembly handles it.
pub const CHUNK_DATA_SIZE: usize = 4000;

/// Max chunks to send per daemon tick.
/// 400 × 4000 ≈ 1.6 MB per tick. At 20 ticks/s ≈ 32 MB/s.
pub const CHUNKS_PER_TICK: usize = 400;

/// Offer timeout: cancel if no accept/reject within 30 seconds.
pub const OFFER_TIMEOUT_SECS: u64 = 30;

/// Stale transfer timeout: cancel if no progress for this many seconds.
/// Large files may have gaps during NACK/retransmit cycles, so be generous.
pub const STALE_TIMEOUT_SECS: u64 = 120;

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
