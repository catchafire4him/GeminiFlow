//! Diagnostics to both the dev console and a file, so a failure that happens
//! while the console is buried is still recoverable after the fact.
//!
//! Log lives at %APPDATA%/GeminiFlow/geminiflow.log.

use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

/// Gates the high-volume diagnostics. Off by default: the hotkey counter alone
/// writes a line every five seconds forever.
static DEBUG: AtomicBool = AtomicBool::new(false);

pub fn set_debug(on: bool) {
    DEBUG.store(on, Ordering::SeqCst);
}

pub fn debug_enabled() -> bool {
    DEBUG.load(Ordering::SeqCst)
}

/// Keeps interleaved writes from different threads on separate lines.
static LOCK: Mutex<()> = Mutex::new(());

pub fn log(message: impl AsRef<str>) {
    let line = format!(
        "{} {}\n",
        chrono::Local::now().format("%H:%M:%S%.3f"),
        message.as_ref()
    );

    let _guard = LOCK.lock();
    eprint!("{line}");

    if let Ok(dir) = crate::store::data_dir() {
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join("geminiflow.log"))
        {
            let _ = file.write_all(line.as_bytes());
        }
    }
}

/// Truncates the log at startup so it always covers the current run only.
pub fn start_session() {
    if let Ok(dir) = crate::store::data_dir() {
        let _ = std::fs::write(dir.join("geminiflow.log"), b"");
    }
    log(format!(
        "=== GeminiFlow {} started ===",
        env!("CARGO_PKG_VERSION")
    ));
}

#[macro_export]
macro_rules! logln {
    ($($arg:tt)*) => { $crate::logging::log(format!($($arg)*)) };
}
