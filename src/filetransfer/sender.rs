use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::net::{SocketAddr, UdpSocket};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use super::CHUNK_DATA_SIZE;

pub enum SenderThreadCmd {
    Nack(Vec<u32>),
    Ack,
    Cancel,
}

pub struct SenderThreadEvent {
    pub contact_id: String,
    pub transfer_id: u32,
    pub kind: SenderEventKind,
}

pub enum SenderEventKind {
    Progress { bytes_sent: u64, total: u64 },
    AllChunksSent,
    Done,
    Error(String),
}

/// Run a sender thread that reads chunks from file, encrypts, and sends via UDP.
pub fn sender_thread_run(
    mut sender: SenderState,
    socket: UdpSocket,
    session: crate::crypto::Session,
    peer_addr: SocketAddr,
    cmd_rx: mpsc::Receiver<SenderThreadCmd>,
    event_tx: mpsc::Sender<SenderThreadEvent>,
    contact_id: String,
) {
    let transfer_id = sender.transfer_id;
    let file_size = sender.file_size;

    loop {
        // Send all queued chunks
        let mut last_progress = Instant::now();

        while !sender.all_sent {
            // Send 50 chunks per batch (~200KB), sleep 5ms → ~40MB/s max
            let batch = sender.next_chunks(50);
            if batch.is_empty() {
                break;
            }
            for (chunk_idx, chunk_data) in &batch {
                let mut payload = Vec::with_capacity(8 + chunk_data.len());
                payload.extend_from_slice(&transfer_id.to_le_bytes());
                payload.extend_from_slice(&chunk_idx.to_le_bytes());
                payload.extend_from_slice(chunk_data);

                let pkt = session.encrypt_packet(crate::crypto::PKT_MSG_FILE_CHUNK, &payload);
                socket.send_to(&pkt, peer_addr).ok();
            }
            // Pace: 5ms between batches to avoid saturating UDP buffers
            std::thread::sleep(Duration::from_millis(5));

            // Report progress every 500ms
            if last_progress.elapsed() >= Duration::from_millis(500) {
                let bytes_sent = (sender.progress_bytes_from_queue()).min(file_size);
                event_tx.send(SenderThreadEvent {
                    contact_id: contact_id.clone(),
                    transfer_id,
                    kind: SenderEventKind::Progress { bytes_sent, total: file_size },
                }).ok();
                last_progress = Instant::now();
            }
        }

        // All chunks sent — notify daemon
        event_tx.send(SenderThreadEvent {
            contact_id: contact_id.clone(),
            transfer_id,
            kind: SenderEventKind::AllChunksSent,
        }).ok();

        // Wait for Nack/Ack/Cancel from daemon
        match cmd_rx.recv() {
            Ok(SenderThreadCmd::Nack(missing)) => {
                log_fmt!("[sender_thread] NACK received: {} missing chunks for tid={}", missing.len(), transfer_id);
                sender.on_nack(missing);
                // Re-enter the sending loop
                continue;
            }
            Ok(SenderThreadCmd::Ack) => {
                log_fmt!("[sender_thread] ACK received for tid={}", transfer_id);
                event_tx.send(SenderThreadEvent {
                    contact_id: contact_id.clone(),
                    transfer_id,
                    kind: SenderEventKind::Done,
                }).ok();
                return;
            }
            Ok(SenderThreadCmd::Cancel) => {
                log_fmt!("[sender_thread] Cancel received for tid={}", transfer_id);
                return;
            }
            Err(_) => {
                // Channel closed — daemon shut down
                return;
            }
        }
    }
}

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

    /// Progress bytes based on how far through the send queue we are.
    /// Used by the sender thread for progress reporting.
    pub fn progress_bytes_from_queue(&self) -> u64 {
        (self.queue_pos as u64 * CHUNK_DATA_SIZE as u64).min(self.file_size)
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
