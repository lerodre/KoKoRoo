use std::fs::OpenOptions;
use std::io::Write;
use std::sync::Mutex;
use std::time::Instant;

static LOG_START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
static LOG_FILE: std::sync::OnceLock<Mutex<std::fs::File>> = std::sync::OnceLock::new();

fn init() -> &'static Mutex<std::fs::File> {
    LOG_START.get_or_init(Instant::now);
    LOG_FILE.get_or_init(|| {
        let file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open("hostelD_log.txt")
            .expect("Failed to create hostelD_log.txt");
        Mutex::new(file)
    })
}

pub fn log(msg: &str) {
    let file_mutex = init();
    let elapsed = LOG_START.get().unwrap().elapsed();
    let secs = elapsed.as_secs_f64();
    let line = format!("[{secs:>10.3}] {msg}\n");

    // Write to file
    if let Ok(mut f) = file_mutex.lock() {
        let _ = f.write_all(line.as_bytes());
        let _ = f.flush();
    }

    // Also print to stderr for debugging
    eprint!("{line}");
}

/// Log with format args, use like: log_fmt!("value: {}", x);
#[macro_export]
macro_rules! log_fmt {
    ($($arg:tt)*) => {
        $crate::logger::log(&format!($($arg)*))
    };
}
