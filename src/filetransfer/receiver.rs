use std::collections::HashSet;
use std::fs::{self, File, OpenOptions};
use std::io::{Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::time::Instant;

use sha2::{Digest, Sha256};

use super::CHUNK_DATA_SIZE;

/// ACK-on-Error receiver state.
///
/// Receives chunks in any order, tracks which arrived via a HashSet.
/// When FILE_COMPLETE arrives, computes missing list and sends NACK or final ACK.
pub struct ReceiverState {
    pub transfer_id: u32,
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

        ReceiverState {
            transfer_id,
            filename,
            file_size,
            expected_sha256,
            total_chunks,
            received: HashSet::with_capacity(total_chunks as usize),
            temp_path,
            contact_id,
            bytes_received: 0,
            last_chunk_time: Instant::now(),
        }
    }

    /// Write a chunk to the temp file.
    pub fn on_chunk(&mut self, chunk_index: u32, data: &[u8]) {
        if chunk_index >= self.total_chunks {
            return;
        }
        let offset = chunk_index as u64 * CHUNK_DATA_SIZE as u64;

        if let Ok(mut file) = OpenOptions::new().write(true).open(&self.temp_path) {
            if file.seek(SeekFrom::Start(offset)).is_ok() {
                file.write_all(data).ok();
            }
        }

        if self.received.insert(chunk_index) {
            self.bytes_received += data.len() as u64;
        }
        self.last_chunk_time = Instant::now();
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
    pub fn verify_hash(&self) -> bool {
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
    pub fn finalize(&self) -> Option<String> {
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
    pub fn cleanup(&self) {
        fs::remove_file(&self.temp_path).ok();
    }
}

fn files_dir() -> PathBuf {
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join(".hostelD").join("files")
}

fn files_tmp_dir() -> PathBuf {
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join(".hostelD").join("files_tmp")
}
