use std::collections::HashSet;
use std::fs::{self, File, OpenOptions};
use std::io::{BufWriter, Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::Instant;

use sha2::{Digest, Sha256};

use super::CHUNK_DATA_SIZE;

pub enum WriterCmd {
    Chunk(u32, Vec<u8>),
    Flush,
    Done,
}

/// ACK-on-Error receiver state.
///
/// Receives chunks in any order, tracks which arrived via a HashSet.
/// When FILE_COMPLETE arrives, computes missing list and sends NACK or final ACK.
pub struct ReceiverState {
    pub filename: String,
    pub file_size: u64,
    pub expected_sha256: [u8; 32],
    pub total_chunks: u32,
    /// Set of chunk indices received.
    received: HashSet<u32>,
    /// Temp file path for writing chunks as they arrive.
    pub temp_path: PathBuf,
    /// Contact ID (for final destination directory).
    pub contact_id: String,
    /// Total bytes received so far.
    pub bytes_received: u64,
    /// Time of last chunk received.
    pub last_chunk_time: Instant,
    /// Channel to the writer thread.
    write_tx: Option<mpsc::SyncSender<WriterCmd>>,
}

impl ReceiverState {
    pub fn new(
        transfer_id: u32,
        filename: String,
        file_size: u64,
        expected_sha256: [u8; 32],
        contact_id: String,
    ) -> Self {
        let total_chunks = if file_size == 0 {
            1
        } else {
            ((file_size + CHUNK_DATA_SIZE as u64 - 1) / CHUNK_DATA_SIZE as u64) as u32
        };

        let tmp_dir = files_tmp_dir();
        fs::create_dir_all(&tmp_dir).ok();
        let temp_path = tmp_dir.join(format!("{transfer_id}.part"));

        // Pre-allocate the temp file
        if let Ok(f) = File::create(&temp_path) {
            f.set_len(file_size).ok();
        }

        // Spawn writer thread that owns the BufWriter
        let (write_tx, write_rx) = mpsc::sync_channel::<WriterCmd>(4096);
        let temp_path_clone = temp_path.clone();
        std::thread::spawn(move || {
            let file_writer = OpenOptions::new()
                .write(true)
                .open(&temp_path_clone)
                .ok()
                .map(|f| BufWriter::with_capacity(256 * 1024, f));
            if let Some(mut writer) = file_writer {
                while let Ok(cmd) = write_rx.recv() {
                    match cmd {
                        WriterCmd::Chunk(chunk_index, data) => {
                            let offset = chunk_index as u64 * CHUNK_DATA_SIZE as u64;
                            if writer.seek(SeekFrom::Start(offset)).is_ok() {
                                writer.write_all(&data).ok();
                            }
                        }
                        WriterCmd::Flush => {
                            writer.flush().ok();
                        }
                        WriterCmd::Done => {
                            writer.flush().ok();
                            break;
                        }
                    }
                }
            }
        });

        ReceiverState {
            filename,
            file_size,
            expected_sha256,
            total_chunks,
            received: HashSet::with_capacity(total_chunks as usize),
            temp_path,
            contact_id,
            bytes_received: 0,
            last_chunk_time: Instant::now(),
            write_tx: Some(write_tx),
        }
    }

    /// Write a chunk to the temp file via the writer thread.
    pub fn on_chunk(&mut self, chunk_index: u32, data: &[u8]) {
        if chunk_index >= self.total_chunks {
            return;
        }

        if let Some(ref tx) = self.write_tx {
            tx.send(WriterCmd::Chunk(chunk_index, data.to_vec())).ok();
        }

        if self.received.insert(chunk_index) {
            self.bytes_received += data.len() as u64;
        }
        self.last_chunk_time = Instant::now();
    }

    /// Flush the writer before verification or finalization.
    pub fn flush(&mut self) {
        if let Some(ref tx) = self.write_tx {
            tx.send(WriterCmd::Flush).ok();
        }
    }

    /// Compute the list of missing chunk indices.
    pub fn missing_chunks(&self) -> Vec<u32> {
        let mut missing = Vec::new();
        for i in 0..self.total_chunks {
            if !self.received.contains(&i) {
                missing.push(i);
            }
        }
        missing
    }

    /// Check if all chunks have been received.
    pub fn is_complete(&self) -> bool {
        self.received.len() as u32 >= self.total_chunks
    }

    /// Verify the SHA-256 hash of the received file. Returns true if it matches.
    pub fn verify_hash(&mut self) -> bool {
        // Close the writer thread before reading back
        if let Some(tx) = self.write_tx.take() {
            tx.send(WriterCmd::Done).ok();
            // Give the writer thread a moment to finish
            std::thread::sleep(std::time::Duration::from_millis(50));
        }

        let mut hasher = Sha256::new();
        if let Ok(mut file) = File::open(&self.temp_path) {
            let mut buf = [0u8; 8192];
            loop {
                match std::io::Read::read(&mut file, &mut buf) {
                    Ok(0) => break,
                    Ok(n) => hasher.update(&buf[..n]),
                    Err(_) => return false,
                }
            }
        } else {
            return false;
        }
        let hash = hasher.finalize();
        hash.as_slice() == self.expected_sha256
    }

    /// Move the temp file to its final destination.
    pub fn finalize(&mut self) -> Option<String> {
        // Ensure writer thread is closed before renaming
        if let Some(tx) = self.write_tx.take() {
            tx.send(WriterCmd::Done).ok();
            std::thread::sleep(std::time::Duration::from_millis(50));
        }

        let dest_dir = files_dir().join(&self.contact_id);
        fs::create_dir_all(&dest_dir).ok()?;

        let mut dest = dest_dir.join(&self.filename);

        if dest.exists() {
            let stem = std::path::Path::new(&self.filename)
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| self.filename.clone());
            let ext = std::path::Path::new(&self.filename)
                .extension()
                .map(|s| format!(".{}", s.to_string_lossy()))
                .unwrap_or_default();
            let mut counter = 1u32;
            loop {
                dest = dest_dir.join(format!("{stem} ({counter}){ext}"));
                if !dest.exists() {
                    break;
                }
                counter += 1;
                if counter > 10000 {
                    return None;
                }
            }
        }

        fs::rename(&self.temp_path, &dest).ok()?;
        Some(dest.to_string_lossy().to_string())
    }

    /// Clean up temp file on cancel/failure.
    pub fn cleanup(&mut self) {
        // Close writer thread first
        if let Some(tx) = self.write_tx.take() {
            tx.send(WriterCmd::Done).ok();
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        fs::remove_file(&self.temp_path).ok();
    }
}

fn files_dir() -> PathBuf {
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join(".kokoroo").join("files")
}

fn files_tmp_dir() -> PathBuf {
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join(".kokoroo").join("files_tmp")
}
