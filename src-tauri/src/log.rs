//! A small error log: %APPDATA%\app.docspace.desktop\docspace.log
//!
//! Every failed command (from the page), indexing problems, and crashes (panics) are written
//! here with a timestamp. The Log window in the app shows the newest lines and copies them,
//! with version and settings, so they can be pasted into a support chat.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

/// Start a fresh file once the log passes this size (the previous one is kept as docspace.old.log).
const MAX_BYTES: u64 = 1024 * 1024;

static PATH: OnceLock<PathBuf> = OnceLock::new();
static LOCK: Mutex<()> = Mutex::new(());

pub fn init(path: PathBuf) {
    let _ = PATH.set(path);
    // Crashes inside DocSpace's Rust code: record where and why, then let Rust report as usual.
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let at = info.location().map(|l| format!(" at {}:{}", l.file(), l.line())).unwrap_or_default();
        let msg = info
            .payload()
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "unknown panic".into());
        write("PANIC", "rust", &format!("{msg}{at}"));
        default_hook(info);
    }));
    write("INFO", "app", &format!("DocSpace {} started", env!("CARGO_PKG_VERSION")));
}

pub fn path() -> Option<&'static PathBuf> {
    PATH.get()
}

/// Append one line: `2026-10-22 23:55:01Z ERROR [index] message`.
pub fn write(level: &str, area: &str, message: &str) {
    let Some(path) = PATH.get() else { return };
    let _guard = LOCK.lock();
    if fs::metadata(path).map(|m| m.len() > MAX_BYTES).unwrap_or(false) {
        let _ = fs::rename(path, path.with_file_name("docspace.old.log"));
    }
    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(path) {
        // One entry per line, so multi-line errors stay readable when pasted.
        let one_line = message.replace("\r\n", " | ").replace('\n', " | ");
        let _ = writeln!(f, "{} {level:<5} [{area}] {one_line}", timestamp());
    }
}

/// The newest `max_lines` lines of the log.
pub fn tail(max_lines: usize) -> String {
    let Some(path) = PATH.get() else { return String::new() };
    let text = fs::read_to_string(path).unwrap_or_default();
    let lines: Vec<&str> = text.lines().collect();
    lines[lines.len().saturating_sub(max_lines)..].join("\n")
}

pub fn clear() {
    if let Some(path) = PATH.get() {
        let _guard = LOCK.lock();
        let _ = fs::write(path, "");
    }
}

/// UTC time as `YYYY-MM-DD HH:MM:SSZ`, without a date library.
fn timestamp() -> String {
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs()) as i64;
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    let (h, m, s) = (rem / 3600, rem % 3600 / 60, rem % 60);
    // Days since 1970-01-01 → civil date (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let mo = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(mo <= 2);
    format!("{y:04}-{mo:02}-{d:02} {h:02}:{m:02}:{s:02}Z")
}

#[cfg(test)]
mod tests {
    #[test]
    fn timestamp_shape() {
        let t = super::timestamp();
        assert_eq!(t.len(), 20, "{t}");
        assert!(t.ends_with('Z') && t.as_bytes()[4] == b'-' && t.as_bytes()[10] == b' ');
        assert!(t.starts_with("20"));
    }
}
