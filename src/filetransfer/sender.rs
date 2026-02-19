use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::time::Instant;

use super::CHUNK_DATA_SIZE;

/// ACK-on-Error sender state.
///
/// The sender blasts all chunks as fast as possible, then sends FILE_COMPLETE.
/// If the receiver reports missing chunks (NACK), only those are retransmitted.
pub struct SenderState {
    pub transfer_id: u32,
    pub file_path: String,
    pub file_size: u64,
    pub sha256: [u8; 32],
    pub total_chunks: u32,
    /// Chunks queued for sending (initially 0..total_chunks, then only missing on retransmit).
    send_queue: Vec<u32>,
    /// Index into send_queue for the next chunk to send.
    queue_pos: usize,
    /// Whether we've finished sending all queued chunks (ready to send COMPLETE).
    pub all_sent: bool,
    /// Whether FILE_COMPLETE has been sent (waiting for ACK/NACK).
    pub complete_sent: bool,
    /// Time of last FILE_COMPLETE sent (for retry).
    pub complete_sent_at: Instant,
    /// Whether transfer is fully done (ACK received).
    pub done: bool,
    /// Number of unique chunks the receiver has confirmed (total - missing from last NACK).
    pub chunks_confirmed: u32,
    /// Cached file handle for sequential reads.
    file_reader: Option<BufReader<File>>,
    /// The chunk index that the cached reader is positioned at next.
    reader_pos: u32,
}

impl SenderState {
    pub fn new(transfer_id: u32, file_path: String, file_size: u64, sha256: [u8; 32]) -> Self {
        let total_chunks = if file_size == 0 {
            1
        } else {
            ((file_size + CHUNK_DATA_SIZE as u64 - 1) / CHUNK_DATA_SIZE as u64) as u32
        };
        let send_queue: Vec<u32> = (0..total_chunks).collect();
        let now = Instant::now();
        SenderState {
            transfer_id,
            file_path,
            file_size,
            sha256,
            total_chunks,
            send_queue,
            queue_pos: 0,
            all_sent: false,
            complete_sent: false,
            complete_sent_at: now,
            done: false,
            chunks_confirmed: 0,
            file_reader: None,
            reader_pos: 0,
        }
    }

    /// Progress bytes: how much the receiver has confirmed.
    pub fn progress_bytes(&self) -> u64 {
        if self.done {
            self.file_size
        } else {
            (self.chunks_confirmed as u64 * CHUNK_DATA_SIZE as u64).min(self.file_size)
        }
    }

    /// Read a chunk from the file at the given index using a cached reader.
    pub fn read_chunk(&mut self, chunk_index: u32) -> Option<Vec<u8>> {
        let offset = chunk_index as u64 * CHUNK_DATA_SIZE as u64;
        let remaining = self.file_size.saturating_sub(offset);
        let to_read = (remaining as usize).min(CHUNK_DATA_SIZE);

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

    /// Get the next batch of chunks to send (up to `max` chunks).
    /// Returns Vec<(chunk_index, chunk_data)>.
    pub fn next_chunks(&mut self, max: usize) -> Vec<(u32, Vec<u8>)> {
        let mut result = Vec::new();
        while result.len() < max && self.queue_pos < self.send_queue.len() {
            let idx = self.send_queue[self.queue_pos];
            self.queue_pos += 1;
            if let Some(data) = self.read_chunk(idx) {
                result.push((idx, data));
            }
        }
        if self.queue_pos >= self.send_queue.len() {
            self.all_sent = true;
        }
        result
    }

    /// Handle a NACK: queue the missing chunks for retransmission.
    pub fn on_nack(&mut self, missing: Vec<u32>) {
        // The receiver confirmed (total_chunks - missing) chunks
        self.chunks_confirmed = self.total_chunks.saturating_sub(missing.len() as u32);
        self.send_queue = missing;
        self.queue_pos = 0;
        self.all_sent = false;
        self.complete_sent = false;
    }

    /// Handle a final ACK: transfer is complete.
    pub fn on_ack(&mut self) {
        self.chunks_confirmed = self.total_chunks;
        self.done = true;
    }

    /// Should we resend FILE_COMPLETE? (timeout after 2 seconds with no response)
    pub fn should_resend_complete(&self) -> bool {
        self.complete_sent && !self.done && self.complete_sent_at.elapsed().as_secs() >= 2
    }

    /// Mark FILE_COMPLETE as sent.
    pub fn mark_complete_sent(&mut self) {
        self.complete_sent = true;
        self.complete_sent_at = Instant::now();
    }
}
