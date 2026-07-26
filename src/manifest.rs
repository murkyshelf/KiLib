//! `library.json` — a machine-readable description of the KiCad library's file
//! structure and everything that has been imported into it.
//!
//! Paths are stored relative to the library root so the file stays valid if the
//! whole tree is moved.

use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::config::Config;
use crate::logging;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Totals {
    pub parts: usize,
    pub symbols: usize,
    pub footprints: usize,
    pub models: usize,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Layout {
    pub symbols: String,
    pub footprints: String,
    pub models: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub merged_symbol_lib: Option<String>,
    pub archive: String,
    pub per_part_footprint_dirs: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Part {
    pub name: String,
    /// Where the part came from: a ZIP file name, or an online catalogue entry
    /// such as `LCSC C7420`.
    #[serde(alias = "source_zip")]
    pub source: String,
    pub imported_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub symbol_file: Option<String>,
    pub symbol_names: Vec<String>,
    pub footprints: Vec<String>,
    pub models: Vec<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Manifest {
    pub generated: String,
    pub library_root: String,
    pub layout: Layout,
    pub parts: Vec<Part>,
}

impl Manifest {
    /// Reads the manifest, returning an empty one when absent or unreadable.
    /// A corrupt manifest is never fatal — it is rebuilt as parts are imported.
    pub fn load(path: &Path) -> Self {
        match std::fs::read_to_string(path) {
            Ok(text) => serde_json::from_str(&text).unwrap_or_else(|e| {
                logging::error(format!(
                    "manifest at {} is not valid JSON ({e}); starting a new one",
                    path.display()
                ));
                Manifest::default()
            }),
            Err(_) => Manifest::default(),
        }
    }

    pub fn save(&self, path: &Path) -> Result<(), String> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("creating {}: {e}", parent.display()))?;
        }
        let text = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        std::fs::write(path, text).map_err(|e| format!("writing {}: {e}", path.display()))?;
        logging::info(format!(
            "manifest updated: {} ({} part(s))",
            path.display(),
            self.parts.len()
        ));
        Ok(())
    }

    /// Refreshes the layout section from the live configuration.
    pub fn sync_layout(&mut self, cfg: &Config) {
        let root = &cfg.library_root;
        self.library_root = root.display().to_string();
        self.layout = Layout {
            symbols: rel(root, &cfg.symbol_dir),
            footprints: rel(root, &cfg.footprint_dir),
            models: rel(root, &cfg.model_dir),
            merged_symbol_lib: cfg.merged_symbol_lib.as_ref().map(|p| rel(root, p)),
            archive: rel(root, &cfg.archive_dir),
            per_part_footprint_dirs: cfg.per_part_footprint_dirs,
        };
        self.generated = chrono::Local::now().to_rfc3339();
    }

    /// Inserts a part, replacing any earlier entry with the same name.
    pub fn upsert(&mut self, part: Part) {
        self.parts.retain(|p| p.name != part.name);
        self.parts.push(part);
        self.parts.sort_by_key(|p| p.name.to_lowercase());
    }
}

/// What a set of parts holds, for the dashboard.
///
/// Files are counted once even though a footprint is deliberately written to
/// both the flat `.pretty` library and a per-part folder.
pub fn totals(parts: &[Part]) -> Totals {
    let mut symbols = HashSet::new();
    let mut footprints = HashSet::new();
    let mut models = HashSet::new();
    for part in parts {
        symbols.extend(part.symbol_names.iter().cloned());
        footprints.extend(part.footprints.iter().filter_map(|p| leaf(p)));
        models.extend(part.models.iter().filter_map(|p| leaf(p)));
    }
    Totals {
        parts: parts.len(),
        symbols: symbols.len(),
        footprints: footprints.len(),
        models: models.len(),
    }
}

/// The file name at the end of a manifest path.
fn leaf(path: &str) -> Option<String> {
    path.rsplit('/').next().map(str::to_string)
}

/// `path` relative to `root` when it lives inside it, else the absolute path.
pub fn rel(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| path.display().to_string())
}

/// Loads, updates and writes the manifest in one step.
pub fn record(cfg: &Config, part: Part) -> Result<PathBuf, String> {
    let path = cfg.manifest_path.clone();
    let mut manifest = Manifest::load(&path);
    manifest.sync_layout(cfg);
    manifest.upsert(part);
    manifest.save(&path)?;
    Ok(path)
}
