//! What is actually sitting in the library folders, as opposed to what
//! `library.json` says was imported.
//!
//! Pointing the application at a library someone else built — or one from
//! before this tool existed — should show that library, not an empty screen. The
//! files on disk are the truth; the manifest only adds provenance to the parts
//! it knows about.

use std::collections::BTreeMap;
use std::path::Path;

use crate::config::Config;
use crate::manifest::{self, Part};

/// The `source` recorded for a part that was found rather than imported.
pub const FOUND: &str = "found on disk";

/// Whether this part was discovered on disk rather than imported.
pub fn is_found(part: &Part) -> bool {
    part.source == FOUND
}

const MODEL_EXTS: [&str; 3] = ["stp", "step", "wrl"];

/// Every part the library folders contain, keyed by part name.
///
/// A part is named by its symbol file, its 3D model or its per-part `.pretty`
/// folder — all three are `<PART>.<something>`. A flat footprint is named after
/// the *footprint*, so it is attached to whichever part's symbol asks for it,
/// and otherwise left alone rather than invented into a part of its own.
pub fn scan(cfg: &Config) -> Vec<Part> {
    let mut parts: BTreeMap<String, Part> = BTreeMap::new();
    // Footprint name -> every part whose symbol asks for it. One footprint
    // serving a dozen parts is the normal case in a real library.
    let mut declared: BTreeMap<String, Vec<String>> = BTreeMap::new();

    for (path, stem) in files_in(&cfg.symbol_dir, &["kicad_sym"]) {
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        for footprint in declared_footprints(&text) {
            declared.entry(footprint).or_default().push(stem.clone());
        }
        let part = parts.entry(stem.clone()).or_insert_with(|| blank(&stem));
        part.symbol_file = Some(manifest::rel(&cfg.library_root, &path));
        part.symbol_names = crate::symlib::symbol_names(&text);
        part.imported_at = modified_at(&path);
    }

    for (path, stem) in files_in(&cfg.model_dir, &MODEL_EXTS) {
        let part = parts.entry(stem.clone()).or_insert_with(|| blank(&stem));
        part.models.push(manifest::rel(&cfg.library_root, &path));
        if part.imported_at.is_empty() {
            part.imported_at = modified_at(&path);
        }
    }

    // Per-part `.pretty` folders name the part directly.
    for dir in subdirs(&cfg.footprint_dir) {
        let Some(stem) = dir.file_stem().map(|s| s.to_string_lossy().into_owned()) else {
            continue;
        };
        let footprints = files_in(&dir, &["kicad_mod"]);
        if footprints.is_empty() {
            continue;
        }
        let part = parts.entry(stem.clone()).or_insert_with(|| blank(&stem));
        for (path, _) in footprints {
            part.footprints
                .push(manifest::rel(&cfg.library_root, &path));
            if part.imported_at.is_empty() {
                part.imported_at = modified_at(&path);
            }
        }
    }

    // A flat footprint is named after the footprint, so it belongs to the part
    // whose symbol asks for it — or, failing that, to a part of the same name.
    for (path, stem) in files_in(&cfg.footprint_dir, &["kicad_mod"]) {
        let owners = declared.get(&stem).cloned().unwrap_or_else(|| vec![stem]);
        for owner in owners {
            if let Some(part) = parts.get_mut(&owner) {
                part.footprints
                    .push(manifest::rel(&cfg.library_root, &path));
            }
        }
    }

    parts.into_values().collect()
}

/// The footprints a symbol file asks for: the value of each `Footprint`
/// property, with KiCad's `library:` prefix stripped.
fn declared_footprints(text: &str) -> Vec<String> {
    let mut found = Vec::new();
    for line in text.lines() {
        let Some(rest) = line.trim_start().strip_prefix("(property \"Footprint\"") else {
            continue;
        };
        let mut quoted = rest.splitn(3, '"');
        quoted.next();
        let Some(value) = quoted.next().filter(|v| !v.is_empty()) else {
            continue;
        };
        found.push(value.rsplit(':').next().unwrap_or(value).to_string());
    }
    found
}

fn blank(name: &str) -> Part {
    Part {
        name: name.to_string(),
        source: FOUND.to_string(),
        ..Part::default()
    }
}

/// Files directly inside `dir` with one of `exts`, as (path, file stem).
///
/// Matching on extension is what keeps the `.bak` copies this tool writes from
/// being mistaken for parts of their own.
fn files_in(dir: &Path, exts: &[&str]) -> Vec<(std::path::PathBuf, String)> {
    let Ok(read) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut found = Vec::new();
    for entry in read.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let matches = path
            .extension()
            .map(|e| {
                let e = e.to_string_lossy().to_lowercase();
                exts.iter().any(|want| *want == e)
            })
            .unwrap_or(false);
        if !matches {
            continue;
        }
        let Some(stem) = path.file_stem().map(|s| s.to_string_lossy().into_owned()) else {
            continue;
        };
        found.push((path, stem));
    }
    found.sort();
    found
}

fn subdirs(dir: &Path) -> Vec<std::path::PathBuf> {
    let Ok(read) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut found: Vec<std::path::PathBuf> = read
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    found.sort();
    found
}

/// A found part has no import date, so its file's timestamp stands in.
fn modified_at(path: &Path) -> String {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .map(|t| chrono::DateTime::<chrono::Local>::from(t).to_rfc3339())
        .unwrap_or_default()
}
