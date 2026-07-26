//! Dedicated rolling file logger for ACP traffic.
//!
//! ACP is a streaming spigot — every thought chunk, tool call, and terminal
//! byte gets logged. This volume would drown the main IDE log, so we write
//! to a separate set of round-robin files: `acp.0.log` … `acp.N.log`.
//!
//! Modelled after crow-acp's `FileLogger` but with size-based rotation so
//! we cap total disk usage (default: 20 files × 10 MB = 200 MB).

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

use chrono::Local;

/// Max bytes per file before we rotate to the next slot.
const MAX_FILE_BYTES: u64 = 10 * 1024 * 1024; // 10 MB
/// Number of round-robin file slots.
const NUM_SLOTS: usize = 20;

pub(crate) struct RollingLogger {
    dir: PathBuf,
    file: Mutex<File>,
    /// Current slot index (0..NUM_SLOTS).
    slot: Mutex<usize>,
    /// Bytes written to the current file.
    written: Mutex<u64>,
}

impl RollingLogger {
    fn new() -> Self {
        let dir = lapce_core::directory::Directory::logs_directory()
            .unwrap_or_else(|| PathBuf::from("/tmp"));
        let _ = fs::create_dir_all(&dir);

        // Find the best starting slot: the one *after* the most recently
        // modified acp.*.log, so we append to the freshest file if it
        // still has room, or start the next one.
        let mut best_slot = 0;
        let mut best_mtime = std::time::SystemTime::UNIX_EPOCH;
        for i in 0..NUM_SLOTS {
            let p = dir.join(format!("acp.{i}.log"));
            if let Ok(meta) = fs::metadata(&p) {
                if let Ok(mtime) = meta.modified() {
                    if mtime > best_mtime {
                        best_mtime = mtime;
                        best_slot = i;
                    }
                }
            }
        }

        // If the freshest file is already full, advance to the next slot.
        let path = dir.join(format!("acp.{best_slot}.log"));
        let start_full = fs::metadata(&path)
            .map(|m| m.len() >= MAX_FILE_BYTES)
            .unwrap_or(false);
        if start_full {
            best_slot = (best_slot + 1) % NUM_SLOTS;
        }

        let file = Self::open_slot(&dir, best_slot);
        let written = fs::metadata(dir.join(format!("acp.{best_slot}.log")))
            .map(|m| m.len())
            .unwrap_or(0);

        Self {
            dir,
            file: Mutex::new(file),
            slot: Mutex::new(best_slot),
            written: Mutex::new(written),
        }
    }

    fn open_slot(dir: &PathBuf, slot: usize) -> File {
        let path = dir.join(format!("acp.{slot}.log"));
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .expect("failed to open acp log file")
    }

    /// Rotate to the next slot, truncating (overwriting) the oldest file.
    fn rotate(&self, file: &mut File, slot: &mut usize, written: &mut u64) {
        *slot = (*slot + 1) % NUM_SLOTS;
        // Truncate: we're overwriting the oldest file in the ring.
        let path = self.dir.join(format!("acp.{}.log", *slot));
        match OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&path)
        {
            Ok(f) => *file = f,
            Err(_) => {} // keep writing to the old file
        }
        *written = 0;
    }

    pub(crate) fn log(&self, level: &str, msg: &str) {
        let ts = Local::now().format("%Y-%m-%d %H:%M:%S%.3f");
        let line = format!("[{}] [{}] {}\n", ts, level, msg);
        let bytes = line.as_bytes();

        let mut file = self.file.lock().unwrap();
        let mut slot = self.slot.lock().unwrap();
        let mut written = self.written.lock().unwrap();

        if *written + bytes.len() as u64 > MAX_FILE_BYTES {
            self.rotate(&mut file, &mut slot, &mut written);
        }

        let _ = file.write_all(bytes);
        *written += bytes.len() as u64;
    }
}

pub(crate) fn logger() -> &'static RollingLogger {
    static INSTANCE: OnceLock<RollingLogger> = OnceLock::new();
    INSTANCE.get_or_init(RollingLogger::new)
}

/// Log to the dedicated ACP rolling log files.
///
/// Usage: `acp_log!("INFO", "session {} ready", sid);`
#[macro_export]
macro_rules! acp_log {
    ($level:expr, $($arg:tt)*) => {
        $crate::acp::log::logger().log($level, &format!($($arg)*))
    };
}
