//! Recursive ZIP discovery under the *configured* download directory.

use std::path::{Path, PathBuf};
use walkdir::WalkDir;

use crate::config::Config;
use crate::logging;

#[derive(Clone, Debug)]
pub struct ZipEntry {
    pub path: PathBuf,
    /// Path relative to the download directory, so nested files read as
    /// `Memory/MT41K128.zip` rather than losing their subfolder.
    pub display: String,
    pub size: u64,
}

pub struct ScanResult {
    pub entries: Vec<ZipEntry>,
    pub errors: Vec<String>,
}

/// Walks `config.download_dir` recursively and returns every `.zip` beneath it.
///
/// The archive and temp directories are skipped so that importing a file does
/// not immediately re-queue it.
pub fn scan(cfg: &Config) -> ScanResult {
    let root = &cfg.download_dir;
    let mut entries = Vec::new();
    let mut errors = Vec::new();

    logging::info(format!("scanning {} for *.zip (recursive)", root.display()));

    if !root.is_dir() {
        let msg = format!("ZIP directory does not exist: {}", root.display());
        logging::error(&msg);
        errors.push(msg);
        return ScanResult { entries, errors };
    }

    let skip: Vec<PathBuf> = [&cfg.archive_dir, &cfg.temp_dir]
        .into_iter()
        .cloned()
        .collect();

    let walker = WalkDir::new(root).follow_links(false).into_iter();
    for item in walker.filter_entry(|e| !skip.iter().any(|s| e.path() == s)) {
        match item {
            Ok(entry) => {
                if !entry.file_type().is_file() {
                    continue;
                }
                if !has_zip_extension(entry.path()) {
                    continue;
                }
                let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
                let display = entry
                    .path()
                    .strip_prefix(root)
                    .unwrap_or(entry.path())
                    .to_string_lossy()
                    .into_owned();
                logging::info(format!(
                    "  found {} ({} bytes)",
                    entry.path().display(),
                    size
                ));
                entries.push(ZipEntry {
                    path: entry.path().to_path_buf(),
                    display,
                    size,
                });
            }
            Err(e) => {
                // A permission error on one subtree must not hide the rest.
                let msg = match e.path() {
                    Some(p) => format!("cannot read {}: {e}", p.display()),
                    None => format!("walk error: {e}"),
                };
                logging::error(&msg);
                errors.push(msg);
            }
        }
    }

    entries.sort_by_key(|e| e.display.to_lowercase());
    logging::info(format!(
        "scan complete: {} zip file(s), {} error(s)",
        entries.len(),
        errors.len()
    ));

    ScanResult { entries, errors }
}

fn has_zip_extension(path: &Path) -> bool {
    path.extension()
        .map(|e| e.eq_ignore_ascii_case("zip"))
        .unwrap_or(false)
}
