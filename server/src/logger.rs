//! Minimal file logger that writes to a `.log` file next to the executable.
//!
//! Because the binary is built with `#![windows_subsystem = "windows"]` and only
//! attaches to a parent console when one happens to exist, `eprintln!` output and
//! panic messages are silently lost when the process is started without a console
//! (e.g. from Task Scheduler or a shortcut). This module captures both regular log
//! lines and panics into a persistent file so crashes can be diagnosed afterwards.

use once_cell::sync::Lazy;
use std::fs::OpenOptions;
use std::io::Write;
use std::sync::Mutex;

static LOG_FILE: Lazy<Mutex<Option<std::fs::File>>> = Lazy::new(|| Mutex::new(None));

/// Path of the log file: same directory and stem as the executable, `.log` ext.
fn log_path() -> Option<std::path::PathBuf> {
    let mut exe = std::env::current_exe().ok()?;
    exe.set_extension("log");
    Some(exe)
}

/// Local time formatted as `YYYY-MM-DD HH:MM:SS.mmm`.
fn timestamp() -> String {
    use winapi::um::minwinbase::SYSTEMTIME;
    use winapi::um::sysinfoapi::GetLocalTime;

    let mut st: SYSTEMTIME = unsafe { std::mem::zeroed() };
    unsafe { GetLocalTime(&mut st) };
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}.{:03}",
        st.wYear, st.wMonth, st.wDay, st.wHour, st.wMinute, st.wSecond, st.wMilliseconds
    )
}

/// Open the log file and install a panic hook. Call once at startup.
pub fn init() {
    if let Some(path) = log_path() {
        match OpenOptions::new().create(true).append(true).open(&path) {
            Ok(file) => *LOG_FILE.lock().unwrap() = Some(file),
            Err(err) => eprintln!("Failed to open log file {}: {}", path.display(), err),
        }
    }

    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let location = info
            .location()
            .map(|l| format!("{}:{}", l.file(), l.line()))
            .unwrap_or_else(|| "<unknown>".to_string());
        let msg = if let Some(s) = info.payload().downcast_ref::<&str>() {
            (*s).to_string()
        } else if let Some(s) = info.payload().downcast_ref::<String>() {
            s.clone()
        } else {
            "<non-string panic payload>".to_string()
        };
        write_log("PANIC", &format!("panicked at {}: {}", location, msg));
        default_hook(info);
    }));
}

/// Write a single log line. Best-effort: never panics, flushes immediately.
pub fn write_log(level: &str, msg: &str) {
    let line = format!("[{}] {:<5} {}\n", timestamp(), level, msg);
    if let Ok(mut guard) = LOG_FILE.lock() {
        if let Some(file) = guard.as_mut() {
            let _ = file.write_all(line.as_bytes());
            let _ = file.flush();
        }
    }
    // Still emit to stderr in case a console is attached.
    eprint!("{}", line);
}

#[macro_export]
macro_rules! linfo {
    ($($arg:tt)*) => { $crate::logger::write_log("INFO", &format!($($arg)*)) };
}

#[macro_export]
macro_rules! lerror {
    ($($arg:tt)*) => { $crate::logger::write_log("ERROR", &format!($($arg)*)) };
}
