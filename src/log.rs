//! A small append-only log with local timestamps and size-based rotation.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

/// Size at which the current log is moved aside before writing.
pub const MAX_LOG_BYTES: u64 = 1024 * 1024;

/// Writes lines to a file, to standard output, or both.
pub struct Logger {
    file: Option<File>,
    echo: bool,
}

impl Logger {
    /// Logs to standard output only.
    pub fn stdout() -> Self {
        Self {
            file: None,
            echo: true,
        }
    }

    /// Logs to `path`, creating its directory, rotating a large existing
    /// file to `.old`, and echoing to standard output when `echo` is set.
    /// If the file cannot be opened, lines still go to standard output.
    pub fn to_file(path: &Path, echo: bool) -> Self {
        let file = open_log_file(path).ok();
        Self { file, echo }
    }

    /// Whether a file is being written.
    pub fn has_file(&self) -> bool {
        self.file.is_some()
    }

    /// Appends one timestamped line.
    pub fn log(&mut self, message: &str) {
        let line = format_line(&timestamp(), message);
        if let Some(file) = &mut self.file {
            let _ = file.write_all(line.as_bytes());
            let _ = file.flush();
        }
        if self.echo {
            print!("{line}");
        }
    }
}

/// The default log location: `%LOCALAPPDATA%\ezrama\ezrama.log`.
pub fn default_log_path() -> Option<PathBuf> {
    let base = std::env::var_os("LOCALAPPDATA")?;
    Some(PathBuf::from(base).join("ezrama").join("ezrama.log"))
}

/// Whether a log of `size` bytes should be moved aside before appending.
pub fn should_rotate(size: u64) -> bool {
    size >= MAX_LOG_BYTES
}

/// One log line: timestamp, a space, the message, a newline.
pub fn format_line(timestamp: &str, message: &str) -> String {
    format!("{timestamp} {message}\n")
}

fn open_log_file(path: &Path) -> std::io::Result<File> {
    if let Some(directory) = path.parent() {
        fs::create_dir_all(directory)?;
    }
    if let Ok(metadata) = fs::metadata(path) {
        if should_rotate(metadata.len()) {
            let _ = fs::rename(path, path.with_extension("old"));
        }
    }
    OpenOptions::new().create(true).append(true).open(path)
}

#[cfg(windows)]
fn timestamp() -> String {
    use crate::win::{GetLocalTime, SYSTEMTIME};
    let mut time = SYSTEMTIME::default();
    unsafe { GetLocalTime(&mut time) };
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        time.wYear, time.wMonth, time.wDay, time.wHour, time.wMinute, time.wSecond
    )
}

#[cfg(not(windows))]
fn timestamp() -> String {
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("{seconds}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rotation_threshold() {
        assert!(!should_rotate(0));
        assert!(!should_rotate(MAX_LOG_BYTES - 1));
        assert!(should_rotate(MAX_LOG_BYTES));
    }

    #[test]
    fn line_format() {
        assert_eq!(format_line("t", "hello"), "t hello\n");
    }

    #[test]
    fn timestamp_has_a_stable_shape() {
        let stamp = timestamp();
        assert!(!stamp.is_empty());
        assert!(stamp.chars().all(|c| c.is_ascii_digit() || " -:".contains(c)));
    }

    #[test]
    fn writes_and_rotates_a_file() {
        let directory = std::env::temp_dir().join(format!("ezrama-log-test-{}", std::process::id()));
        let path = directory.join("nested").join("ezrama.log");
        let _ = fs::remove_dir_all(&directory);

        let mut logger = Logger::to_file(&path, false);
        assert!(logger.has_file());
        logger.log("first");
        drop(logger);
        let written = fs::read_to_string(&path).unwrap();
        assert!(written.ends_with(" first\n"), "{written:?}");

        let big = vec![b'x'; MAX_LOG_BYTES as usize];
        fs::write(&path, &big).unwrap();
        let mut logger = Logger::to_file(&path, false);
        logger.log("after rotation");
        drop(logger);
        assert_eq!(fs::metadata(path.with_extension("old")).unwrap().len(), MAX_LOG_BYTES);
        let fresh = fs::read_to_string(&path).unwrap();
        assert!(fresh.ends_with(" after rotation\n"));
        assert!(fresh.len() < 100);

        let _ = fs::remove_dir_all(&directory);
    }

    #[test]
    fn unwritable_path_falls_back_to_stdout_only() {
        let logger = Logger::to_file(Path::new(""), true);
        assert!(!logger.has_file());
    }
}
