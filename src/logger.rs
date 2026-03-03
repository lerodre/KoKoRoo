use std::collections::VecDeque;
use std::fs::OpenOptions;
use std::io::Write;
use std::sync::Mutex;
use std::time::Instant;

static LOG_START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
static LOG_FILE: std::sync::OnceLock<Mutex<std::fs::File>> = std::sync::OnceLock::new();
static LOG_BUFFER: std::sync::OnceLock<Mutex<VecDeque<String>>> = std::sync::OnceLock::new();

const MAX_LOG_LINES: usize = 2000;

fn init() -> &'static Mutex<std::fs::File> {
    LOG_START.get_or_init(Instant::now);
    LOG_FILE.get_or_init(|| {
        let home = std::env::var("HOME")
            .or_else(|_| std::env::var("USERPROFILE"))
            .unwrap_or_else(|_| std::env::temp_dir().to_string_lossy().to_string());
        let dir = std::path::PathBuf::from(home).join(".hostelD");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("hostelD_log.txt");
        let file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&path)
            .unwrap_or_else(|_| {
                // Fallback to temp dir if ~/.hostelD is also not writable
                let tmp = std::env::temp_dir().join("hostelD_log.txt");
                OpenOptions::new()
                    .create(true)
                    .write(true)
                    .truncate(true)
                    .open(tmp)
                    .expect("Failed to create hostelD_log.txt")
            });
        Mutex::new(file)
    })
}

pub fn get_log_buffer() -> &'static Mutex<VecDeque<String>> {
    log_buffer()
}

fn log_buffer() -> &'static Mutex<VecDeque<String>> {
    LOG_BUFFER.get_or_init(|| Mutex::new(VecDeque::new()))
}

pub fn log(msg: &str) {
    let file_mutex = init();
    let elapsed = LOG_START.get().unwrap().elapsed();
    let secs = elapsed.as_secs_f64();
    let line = format!("[{secs:>10.3}] {msg}");

    // Write to file
    if let Ok(mut f) = file_mutex.lock() {
        let _ = f.write_all(line.as_bytes());
        let _ = f.write_all(b"\n");
        let _ = f.flush();
    }

    // Push to in-memory buffer for GUI log viewer
    if let Ok(mut buf) = log_buffer().lock() {
        buf.push_back(line.clone());
        while buf.len() > MAX_LOG_LINES {
            buf.pop_front();
        }
    }

    // Also print to stderr for debugging
    eprintln!("{line}");
}

/// Get a snapshot of all log lines for the GUI viewer.
pub fn get_log_lines() -> Vec<String> {
    if let Ok(buf) = log_buffer().lock() {
        buf.iter().cloned().collect()
    } else {
        Vec::new()
    }
}

/// Log with format args, use like: log_fmt!("value: {}", x);
#[macro_export]
macro_rules! log_fmt {
    ($($arg:tt)*) => {
        $crate::logger::log(&format!($($arg)*))
    };
}
