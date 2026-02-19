use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::time::Instant;

use super::{CHUNK_DATA_SIZE, WINDOW_SIZE, ACK_TIMEOUT_MS};

/// Tracks the state of an outgoing file transfer (sender side).
pub struct SenderState {
    pub transfer_id: u32,
    pub file_path: String,
    pub file_size: u64,
    pub sha256: [u8; 32],
    pub total_chunks: u32,
    /// Next chunk index to send.
    pub next_to_send: u32,
    /// Highest ACK received (all chunks up to and including this index are confirmed).
    /// Starts at u32::MAX to indicate no ACKs yet.
    pub ack_through: Option<u32>,
    /// Time of last ACK received (or transfer start).
    pub last_ack_time: Instant,
    /// Time of last chunk sent (for pacing).
    pub last_chunk_sent: Instant,
    /// Bytes confirmed delivered.
    pub bytes_confirmed: u64,
    /// Whether FILE_COMPLETE has been sent.
    pub complete_sent: bool,
    /// Cached file handle for sequential reads.
    file_reader: Option<BufReader<File>>,
    /// The chunk index that the cached reader is positioned at.
    reader_pos: u32,
}

impl SenderState {
    pub fn new(transfer_id: u32, file_path: String, file_size: u64, sha256: [u8; 32]) -> Self {
        let total_chunks = if file_size == 0 {
            1 // Send at least one empty chunk for zero-byte files
        } else {
            ((file_size + CHUNK_DATA_SIZE as u64 - 1) / CHUNK_DATA_SIZE as u64) as u32
        };
        let now = Instant::now();
        SenderState {
            transfer_id,
            file_path,
            file_size,
            sha256,
            total_chunks,
            next_to_send: 0,
            ack_through: None,
            last_ack_time: now,
            last_chunk_sent: now,
            bytes_confirmed: 0,
            complete_sent: false,
            file_reader: None,
            reader_pos: 0,
        }
    }

    /// Returns the base of the send window (first unacked chunk).
    fn window_base(&self) -> u32 {
        match self.ack_through {
            Some(ack) => ack + 1,
            None => 0,
        }
    }

    /// Returns how many chunks can still be sent in the current window.
    pub fn window_available(&self) -> u32 {
        let base = self.window_base();
        let window_end = (base + WINDOW_SIZE).min(self.total_chunks);
        if self.next_to_send < window_end {
            window_end - self.next_to_send
        } else {
            0
        }
    }

    /// Read a chunk from the file at the given index using a cached reader.
    pub fn read_chunk(&mut self, chunk_index: u32) -> Option<Vec<u8>> {
        let offset = chunk_index as u64 * CHUNK_DATA_SIZE as u64;
        let remaining = self.file_size.saturating_sub(offset);
        let to_read = (remaining as usize).min(CHUNK_DATA_SIZE);

        // Open or reuse the cached file reader
        let needs_seek = if self.file_reader.is_none() {
            let file = File::open(&self.file_path).ok()?;
            self.file_reader = Some(BufReader::with_capacity(64 * 1024, file));
            true
        } else {
            chunk_index != self.reader_pos
        };

        let reader = self.file_reader.as_mut()?;
        if needs_seek {
            reader.seek(SeekFrom::Start(offset)).ok()?;
        }

        let mut buf = vec![0u8; to_read];
        reader.read_exact(&mut buf).ok()?;
        self.reader_pos = chunk_index + 1;
        Some(buf)
    }

    /// Get the next chunk to send (if within window). Advances next_to_send.
    /// Returns (chunk_index, chunk_data).
    pub fn next_chunk(&mut self) -> Option<(u32, Vec<u8>)> {
        if self.window_available() == 0 || self.next_to_send >= self.total_chunks {
            return None;
        }
        let idx = self.next_to_send;
        let data = self.read_chunk(idx)?;
        self.next_to_send = idx + 1;
        self.last_chunk_sent = Instant::now();
        Some((idx, data))
    }

    /// Handle an ACK. Returns true if all chunks are now confirmed.
    pub fn on_ack(&mut self, ack_through: u32) -> bool {
        let prev = self.ack_through.unwrap_or(0);
        if self.ack_through.is_none() || ack_through > prev {
            self.ack_through = Some(ack_through);
            self.last_ack_time = Instant::now();
            // Update confirmed bytes
            let confirmed_chunks = ack_through as u64 + 1;
            self.bytes_confirmed = (confirmed_chunks * CHUNK_DATA_SIZE as u64).min(self.file_size);
        }
        self.is_fully_acked()
    }

    /// Check if all chunks have been ACKed.
    pub fn is_fully_acked(&self) -> bool {
        match self.ack_through {
            Some(ack) => ack + 1 >= self.total_chunks,
            None => false,
        }
    }

    /// Check if we should retransmit (ACK timeout expired).
    pub fn should_retransmit(&self) -> bool {
        self.last_ack_time.elapsed().as_millis() >= ACK_TIMEOUT_MS as u128
            && !self.is_fully_acked()
    }

    /// Reset next_to_send to retransmit from the window base.
    pub fn retransmit(&mut self) {
        self.next_to_send = self.window_base();
        self.last_ack_time = Instant::now();
    }
}
