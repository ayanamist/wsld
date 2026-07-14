//! Minimal file logger that writes to a `.log` file next to the executable.
//!
//! Because the binary is built with `#![windows_subsystem = "windows"]` and only
//! attaches to a parent console when one happens to exist, `eprintln!` output and
//! panic messages are silently lost when the process is started without a console
//! (e.g. from Task Scheduler or a shortcut). This module captures both regular log
//! lines and panics into a persistent file so crashes can be diagnosed afterwards.

use once_cell::sync::Lazy;
use std::backtrace::{Backtrace, BacktraceStatus};
use std::fs::OpenOptions;
use std::io::Write;
use std::os::windows::io::AsRawHandle;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

static LOG_FILE: Lazy<Mutex<Option<std::fs::File>>> = Lazy::new(|| Mutex::new(None));
static STDIO_REDIRECTED: AtomicBool = AtomicBool::new(false);

/// Path of the log file: same directory and stem as the executable, `.log` ext.
fn log_path() -> Option<PathBuf> {
    Some(log_path_from(&std::env::current_exe().ok()?))
}

fn log_path_from(exe: &Path) -> PathBuf {
    let mut path = exe.to_path_buf();
    path.set_extension("log");
    path
}

fn should_echo_to_stderr(stdio_redirected: bool) -> bool {
    !stdio_redirected
}

fn redirect_stdio_to_log(file: &std::fs::File) -> std::io::Result<()> {
    use winapi::um::handleapi::DuplicateHandle;
    use winapi::um::processenv::SetStdHandle;
    use winapi::um::processthreadsapi::GetCurrentProcess;
    use winapi::um::winbase::{STD_ERROR_HANDLE, STD_OUTPUT_HANDLE};
    use winapi::um::winnt::{DUPLICATE_SAME_ACCESS, HANDLE};

    unsafe fn duplicate_for_std_handle(file: &std::fs::File) -> std::io::Result<HANDLE> {
        let process = GetCurrentProcess();
        let mut duplicated = std::ptr::null_mut();
        let ok = DuplicateHandle(
            process,
            file.as_raw_handle() as HANDLE,
            process,
            &mut duplicated,
            0,
            1,
            DUPLICATE_SAME_ACCESS,
        );
        if ok == 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(duplicated)
    }

    unsafe {
        let stdout = duplicate_for_std_handle(file)?;
        if SetStdHandle(STD_OUTPUT_HANDLE, stdout) == 0 {
            return Err(std::io::Error::last_os_error());
        }

        let stderr = duplicate_for_std_handle(file)?;
        if SetStdHandle(STD_ERROR_HANDLE, stderr) == 0 {
            return Err(std::io::Error::last_os_error());
        }
    }

    STDIO_REDIRECTED.store(true, Ordering::Relaxed);
    Ok(())
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
    let path = match log_path() {
        Some(path) => path,
        None => {
            eprintln!("Failed to determine current executable path for logging");
            std::process::exit(1);
        }
    };

    if let Some(parent) = path.parent() {
        if let Err(err) = std::fs::create_dir_all(parent) {
            eprintln!("Failed to create log dir {}: {}", parent.display(), err);
            std::process::exit(1);
        }
    }

    match OpenOptions::new().create(true).append(true).open(&path) {
        Ok(file) => {
            if let Err(err) = redirect_stdio_to_log(&file) {
                eprintln!(
                    "Failed to redirect stdout/stderr to {}: {}",
                    path.display(),
                    err
                );
                std::process::exit(1);
            }
            *LOG_FILE.lock().unwrap() = Some(file);
            write_log("INFO", &format!("logging to {}", path.display()));
        }
        Err(err) => {
            eprintln!("Failed to open log file {}: {}", path.display(), err);
            std::process::exit(1);
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
        let thread = std::thread::current();
        let thread = thread.name().unwrap_or("<unnamed>").to_string();
        let backtrace = Backtrace::force_capture();
        write_log(
            "PANIC",
            &format!("thread '{}' panicked at {}: {}", thread, location, msg),
        );
        if backtrace.status() == BacktraceStatus::Captured {
            write_log("PANIC", &format!("backtrace:\n{}", backtrace));
        } else {
            write_log(
                "PANIC",
                &format!("backtrace unavailable: {:?}", backtrace.status()),
            );
        }
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
    if should_echo_to_stderr(STDIO_REDIRECTED.load(Ordering::Relaxed)) {
        eprint!("{}", line);
    }
}

#[macro_export]
macro_rules! linfo {
    ($($arg:tt)*) => { $crate::logger::write_log("INFO", &format!($($arg)*)) };
}

#[macro_export]
macro_rules! lerror {
    ($($arg:tt)*) => { $crate::logger::write_log("ERROR", &format!($($arg)*)) };
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    #[test]
    fn log_path_uses_executable_path_with_log_extension() {
        let path = super::log_path_from(Path::new(r"C:\tools\wsldhost.exe"));

        assert_eq!(path, PathBuf::from(r"C:\tools\wsldhost.log"));
    }

    #[test]
    fn log_path_has_no_fallback_candidate() {
        let exe = Path::new(r"C:\Program Files\wsldhost.exe");
        let first = super::log_path_from(exe);
        let second = super::log_path_from(exe);

        assert_eq!(first, second);
    }

    #[test]
    fn stderr_echo_is_disabled_after_stdio_redirection() {
        assert!(!super::should_echo_to_stderr(true));
        assert!(super::should_echo_to_stderr(false));
    }
}
