//! Append-only log at `importer.log`, next to the active config file.
//!
//! Everything that touches the filesystem funnels a line through here so path
//! problems can be diagnosed after the fact without re-running the TUI.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

struct Logger {
    path: PathBuf,
    sink: Mutex<std::fs::File>,
}

static LOGGER: OnceLock<Logger> = OnceLock::new();

/// Opens (creating if needed) the log file. Called once, before anything else
/// so that config loading itself is logged.
pub fn init(path: &Path) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let file = OpenOptions::new().create(true).append(true).open(path)?;
    let _ = LOGGER.set(Logger {
        path: path.to_path_buf(),
        sink: Mutex::new(file),
    });
    Ok(())
}

pub fn path() -> Option<PathBuf> {
    LOGGER.get().map(|l| l.path.clone())
}

fn write_line(level: &str, msg: &str) {
    let Some(logger) = LOGGER.get() else { return };
    let stamp = chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f");
    if let Ok(mut sink) = logger.sink.lock() {
        let _ = writeln!(sink, "{stamp} [{level:<5}] {msg}");
        let _ = sink.flush();
    }
}

pub fn info(msg: impl AsRef<str>) {
    write_line("INFO", msg.as_ref());
}

pub fn warn(msg: impl AsRef<str>) {
    write_line("WARN", msg.as_ref());
}

pub fn error(msg: impl AsRef<str>) {
    write_line("ERROR", msg.as_ref());
}
