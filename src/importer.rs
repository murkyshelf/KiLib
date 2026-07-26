//! Extracts a Component Search Engine / Samacsys archive into the configured
//! KiCad library folders.
//!
//! Archive layout produced by CSE:
//!
//! ```text
//! <PART>/KiCad/<PART>.kicad_sym      -> symbol_dir
//! <PART>/KiCad/<FOOTPRINT>.kicad_mod -> footprint_dir
//! <PART>/3D/<PART>.stp               -> model_dir
//! ```
//!
//! Resulting library tree:
//!
//! ```text
//! 3dmodels/<PART>.stp
//! footprints.pretty/<FOOTPRINT>.kicad_mod
//! footprints.pretty/<PART>.pretty/<FOOTPRINT>.kicad_mod
//! symbols/<PART>.kicad_sym
//! <merged>.kicad_sym
//! library.json
//! ```

use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};

use crate::config::Config;
use crate::logging;
use crate::manifest::{self, Part};
use crate::symlib;

#[derive(Debug, Default, Clone)]
pub struct ImportSummary {
    pub part: String,
    pub symbols: Vec<String>,
    pub footprints: Vec<String>,
    pub models: Vec<String>,
    /// Symbol names appended to the merged library.
    pub merged_symbols: Vec<String>,
    pub archived_to: Option<PathBuf>,
    pub deleted_zip: bool,
}

impl ImportSummary {
    pub fn total(&self) -> usize {
        self.symbols.len() + self.footprints.len() + self.models.len()
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Kind {
    Symbol,
    Footprint,
    Model,
}

/// A file waiting in the staging area to be placed into the library.
///
/// Both sources — an extracted CSE archive and a component downloaded from the
/// web — end up as a list of these, so [`install`] is the single place that
/// decides where anything in the library goes.
pub struct Staged {
    pub kind: Kind,
    pub name: String,
    pub path: PathBuf,
}

/// Decides where an archive member belongs, or `None` to ignore it.
///
/// Only KiCad-relevant files are taken; the Altium/OrCAD/EAGLE trees and the
/// bundled PDF are skipped.
fn classify(path: &Path) -> Option<Kind> {
    let ext = path.extension()?.to_string_lossy().to_lowercase();
    let in_dir = |name: &str| {
        path.components()
            .any(|c| c.as_os_str().to_string_lossy().eq_ignore_ascii_case(name))
    };

    match ext.as_str() {
        "kicad_sym" => Some(Kind::Symbol),
        "kicad_mod" => Some(Kind::Footprint),
        "stp" | "step" | "wrl" => in_dir("3D").then_some(Kind::Model),
        _ => None,
    }
}

/// The part name is the archive's top-level folder, e.g. `TPD12S016PWR`.
fn part_name_from(entry: &Path) -> Option<String> {
    entry
        .components()
        .next()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .filter(|s| !s.is_empty())
}

/// An import that has been unpacked but not yet placed into the library.
///
/// Splitting the work here is what allows the library files that are about to be
/// replaced to be named *before* anything is overwritten.
pub struct Pending {
    pub part: String,
    pub source: String,
    pub staged: Vec<Staged>,
    pub staging: PathBuf,
    /// Library files this import would replace, relative to the library root.
    pub conflicts: Vec<String>,
    /// The archive to file away once the import goes through.
    pub zip: Option<PathBuf>,
}

impl Pending {
    /// A short description for the confirmation prompt and the status line.
    pub fn label(&self) -> String {
        if self.part.is_empty() {
            self.source.clone()
        } else {
            self.part.clone()
        }
    }
}

/// Every library path a staged file will occupy.
///
/// The single source of truth for both the overwrite check and [`install`], so
/// the two cannot disagree about what is about to be written. The last entry is
/// where the staged file itself is moved; any earlier ones are copies.
pub fn destinations(cfg: &Config, part: &str, item: &Staged) -> Vec<PathBuf> {
    match item.kind {
        Kind::Symbol => vec![cfg.symbol_dir.join(&item.name)],
        Kind::Model => vec![cfg.model_dir.join(&item.name)],
        Kind::Footprint => {
            let mut out = Vec::new();
            if cfg.per_part_footprint_dirs && !part.is_empty() {
                out.push(
                    cfg.footprint_dir
                        .join(format!("{part}.pretty"))
                        .join(&item.name),
                );
            }
            out.push(cfg.footprint_dir.join(&item.name));
            out
        }
    }
}

/// The library files this import would replace, relative to the library root.
pub fn conflicts(cfg: &Config, part: &str, staged: &[Staged]) -> Vec<String> {
    let mut found = Vec::new();
    for item in staged {
        for dest in destinations(cfg, part, item) {
            if dest.exists() {
                found.push(manifest::rel(&cfg.library_root, &dest));
            }
        }
    }
    found
}

/// Imports one ZIP, replacing anything already there. Prefer
/// [`prepare`] + [`finish`] in the interface, so the user is asked first.
pub fn import<F>(cfg: &Config, zip_path: &Path, progress: F) -> Result<ImportSummary, String>
where
    F: Fn(f64, String),
{
    let pending = prepare(cfg, zip_path, progress)?;
    finish(cfg, pending)
}

/// Unpacks a ZIP into the staging area and works out what it would overwrite.
/// Nothing in the library is touched. `progress` is called with a 0.0..=1.0
/// fraction and the name of the file being read.
pub fn prepare<F>(cfg: &Config, zip_path: &Path, progress: F) -> Result<Pending, String>
where
    F: Fn(f64, String),
{
    logging::info(format!("import started: {}", zip_path.display()));

    let stem = zip_path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "archive".to_string());
    let staging = prepare_staging(cfg, &stem)?;

    let file = File::open(zip_path).map_err(|e| format!("opening {}: {e}", zip_path.display()))?;
    let mut archive =
        zip::ZipArchive::new(file).map_err(|e| format!("reading {}: {e}", zip_path.display()))?;

    let total = archive.len();
    let mut staged: Vec<Staged> = Vec::new();
    let mut part = String::new();

    for i in 0..total {
        progress(i as f64 / total.max(1) as f64, stem.clone());

        let mut member = archive
            .by_index(i)
            .map_err(|e| format!("reading entry {i} of {}: {e}", zip_path.display()))?;
        if member.is_dir() {
            continue;
        }
        // `enclosed_name` rejects `..` traversal and absolute paths.
        let Some(name) = member.enclosed_name() else {
            logging::warn(format!("skipping unsafe archive path: {}", member.name()));
            continue;
        };
        let Some(kind) = classify(&name) else {
            continue;
        };
        let Some(file_name) = name.file_name() else {
            continue;
        };

        if part.is_empty() {
            if let Some(found) = part_name_from(&name) {
                part = found;
            }
        }

        // Extract into staging first, then place it. Keeps a partially read
        // archive from leaving half-written files in the library.
        let path = staging.join(file_name);
        let mut out =
            File::create(&path).map_err(|e| format!("writing {}: {e}", path.display()))?;
        io::copy(&mut member, &mut out)
            .map_err(|e| format!("extracting {}: {e}", name.display()))?;
        drop(out);

        staged.push(Staged {
            kind,
            name: file_name.to_string_lossy().into_owned(),
            path,
        });
    }

    progress(1.0, stem.clone());

    if staged.is_empty() {
        let _ = std::fs::remove_dir_all(&staging);
        let msg = format!(
            "no KiCad files found in {} (expected */KiCad/*.kicad_sym)",
            zip_path.display()
        );
        logging::error(&msg);
        return Err(msg);
    }

    if part.is_empty() {
        part = stem.trim_start_matches("LIB_").to_string();
    }

    let source_zip = zip_path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let conflicts = conflicts(cfg, &part, &staged);
    if !conflicts.is_empty() {
        logging::info(format!(
            "{part}: {} existing file(s) would be replaced",
            conflicts.len()
        ));
    }

    Ok(Pending {
        part,
        source: source_zip,
        staged,
        staging,
        conflicts,
        zip: Some(zip_path.to_path_buf()),
    })
}

/// Places a prepared import into the library, replacing what it conflicts with,
/// and files the source archive away.
pub fn finish(cfg: &Config, pending: Pending) -> Result<ImportSummary, String> {
    let Pending {
        part,
        source,
        staged,
        staging,
        zip,
        ..
    } = pending;

    let mut summary = install(cfg, &part, &source, staged)?;

    // Only dispose of the source archive once something was actually imported.
    if let Some(zip_path) = zip {
        if cfg.delete_zip_after_import {
            std::fs::remove_file(&zip_path)
                .map_err(|e| format!("deleting {}: {e}", zip_path.display()))?;
            logging::info(format!("deleted source zip {}", zip_path.display()));
            summary.deleted_zip = true;
        } else {
            std::fs::create_dir_all(&cfg.archive_dir)
                .map_err(|e| format!("creating {}: {e}", cfg.archive_dir.display()))?;
            let dest = cfg.archive_dir.join(&source);
            place(&zip_path, &dest)?;
            logging::info(format!("archived zip -> {}", dest.display()));
            summary.archived_to = Some(dest);
        }
    }

    let _ = std::fs::remove_dir_all(&staging);
    Ok(summary)
}

/// Throws away a prepared import. The library and the source archive are left
/// exactly as they were.
pub fn discard(pending: Pending) {
    logging::info(format!("import of {} cancelled", pending.label()));
    let _ = std::fs::remove_dir_all(&pending.staging);
}

/// Empties and returns a per-import scratch folder under the configured
/// temporary directory, so a previous failed run cannot leak stale files into
/// the library.
pub fn prepare_staging(cfg: &Config, group: &str) -> Result<PathBuf, String> {
    let staging = cfg.temp_dir.join(group);
    if staging.exists() {
        let _ = std::fs::remove_dir_all(&staging);
    }
    std::fs::create_dir_all(&staging)
        .map_err(|e| format!("creating temp dir {}: {e}", staging.display()))?;
    Ok(staging)
}

/// Writes downloaded content into the staging area so it can be installed
/// exactly like a file extracted from an archive.
pub fn stage(dir: &Path, kind: Kind, name: &str, contents: &[u8]) -> Result<Staged, String> {
    let path = dir.join(name);
    std::fs::write(&path, contents).map_err(|e| format!("writing {}: {e}", path.display()))?;
    Ok(Staged {
        kind,
        name: name.to_string(),
        path,
    })
}

/// Moves staged files into the configured library tree, merges symbols and
/// records the part in `library.json`.
pub fn install(
    cfg: &Config,
    part: &str,
    source: &str,
    staged: Vec<Staged>,
) -> Result<ImportSummary, String> {
    for dir in [&cfg.symbol_dir, &cfg.footprint_dir, &cfg.model_dir] {
        std::fs::create_dir_all(dir).map_err(|e| format!("creating {}: {e}", dir.display()))?;
    }

    let mut summary = ImportSummary {
        part: part.to_string(),
        ..ImportSummary::default()
    };
    // Collected for the manifest, relative to the library root.
    let mut rel_symbol: Option<String> = None;
    let mut rel_footprints: Vec<String> = Vec::new();
    let mut rel_models: Vec<String> = Vec::new();
    let mut symbol_names: Vec<String> = Vec::new();
    // Footprints go to two places, so they are handled after the loop.
    let mut footprints: Vec<Staged> = Vec::new();

    for item in staged {
        match item.kind {
            Kind::Symbol => {
                let dest = last_destination(cfg, part, &item);
                // Read while the staged copy is still in place: the symbol
                // names go into the manifest whether or not merging is on.
                let text = std::fs::read_to_string(&item.path)
                    .map_err(|e| format!("reading {}: {e}", item.path.display()))?;
                symbol_names.extend(symlib::symbol_names(&text));

                if let Some(merged) = &cfg.merged_symbol_lib {
                    match symlib::merge_into(merged, &text, cfg.backup_before_overwrite) {
                        Ok(names) => summary.merged_symbols.extend(names),
                        Err(e) => {
                            // A malformed library must not lose the import.
                            logging::error(format!("merge into {} failed: {e}", merged.display()));
                        }
                    }
                }
                if cfg.backup_before_overwrite {
                    symlib::backup_file(&dest)?;
                }
                place(&item.path, &dest)?;
                logging::info(format!("  installed {} -> {}", item.name, dest.display()));
                rel_symbol = Some(manifest::rel(&cfg.library_root, &dest));
                summary.symbols.push(item.name);
            }
            Kind::Footprint => footprints.push(item),
            Kind::Model => {
                let dest = last_destination(cfg, part, &item);
                place(&item.path, &dest)?;
                logging::info(format!("  installed {} -> {}", item.name, dest.display()));
                rel_models.push(manifest::rel(&cfg.library_root, &dest));
                summary.models.push(item.name);
            }
        }
    }

    // Footprints go to two places: optionally a per-part `.pretty` subfolder,
    // then the flat library. `destinations` lists the copies first and the move
    // last, so the staged file is consumed exactly once.
    for item in footprints {
        let mut dests = destinations(cfg, part, &item);
        let Some(flat) = dests.pop() else {
            continue;
        };
        for copy in dests {
            std::fs::create_dir_all(copy.parent().unwrap())
                .map_err(|e| format!("creating per-part footprint folder: {e}"))?;
            std::fs::copy(&item.path, &copy)
                .map_err(|e| format!("copying to {}: {e}", copy.display()))?;
            logging::info(format!("  installed {} -> {}", item.name, copy.display()));
            rel_footprints.push(manifest::rel(&cfg.library_root, &copy));
        }
        place(&item.path, &flat)?;
        logging::info(format!("  installed {} -> {}", item.name, flat.display()));
        rel_footprints.push(manifest::rel(&cfg.library_root, &flat));
        summary.footprints.push(item.name);
    }

    if summary.total() == 0 {
        return Err(format!("{part}: nothing to install"));
    }

    // Record the part in library.json. A manifest failure is reported but does
    // not undo a successful import.
    let record = Part {
        name: summary.part.clone(),
        source: source.to_string(),
        imported_at: chrono::Local::now().to_rfc3339(),
        symbol_file: rel_symbol,
        symbol_names,
        footprints: rel_footprints,
        models: rel_models,
    };
    if let Err(e) = manifest::record(cfg, record) {
        logging::error(format!("manifest update failed: {e}"));
    }

    logging::info(format!(
        "installed {part}: {} symbol(s), {} footprint(s), {} model(s)",
        summary.symbols.len(),
        summary.footprints.len(),
        summary.models.len()
    ));
    Ok(summary)
}

/// Where a staged file itself ends up, as opposed to the copies made of it.
fn last_destination(cfg: &Config, part: &str, item: &Staged) -> PathBuf {
    destinations(cfg, part, item)
        .pop()
        .unwrap_or_else(|| cfg.library_root.join(&item.name))
}

/// Moves `src` to `dest`, falling back to copy+delete when the two live on
/// different filesystems (rename fails with EXDEV there).
fn place(src: &Path, dest: &Path) -> Result<(), String> {
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("creating {}: {e}", parent.display()))?;
    }
    match std::fs::rename(src, dest) {
        Ok(()) => Ok(()),
        Err(_) => {
            std::fs::copy(src, dest).map_err(|e| format!("copying to {}: {e}", dest.display()))?;
            std::fs::remove_file(src).map_err(|e| format!("removing {}: {e}", src.display()))?;
            Ok(())
        }
    }
}
