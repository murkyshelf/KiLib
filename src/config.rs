//! Every filesystem location the application touches lives here.
//!
//! No path is hardcoded at a use site. Defaults are *derived* at runtime from
//! the user's home directory, persisted to `config.toml`, and every consumer
//! reads them back off the [`Config`] struct.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use crate::logging;

/// What a [`Field`] holds, which decides how the Settings screen edits it and
/// whether startup validation checks it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    /// A directory that must exist before importing.
    Dir,
    /// A file path. Its parent must exist; the file itself is created on demand.
    File,
    Bool,
}

/// Every configurable setting, in the order the Settings screen shows them.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Field {
    LibraryRoot,
    DownloadDir,
    SymbolDir,
    FootprintDir,
    ModelDir,
    TempDir,
    ArchiveDir,
    MergedSymbolLib,
    ManifestPath,
    PerPartFootprintDirs,
    BackupBeforeOverwrite,
    DeleteZip,
}

impl Field {
    pub const ALL: [Field; 12] = [
        Field::LibraryRoot,
        Field::DownloadDir,
        Field::SymbolDir,
        Field::FootprintDir,
        Field::ModelDir,
        Field::TempDir,
        Field::ArchiveDir,
        Field::MergedSymbolLib,
        Field::ManifestPath,
        Field::PerPartFootprintDirs,
        Field::BackupBeforeOverwrite,
        Field::DeleteZip,
    ];

    /// The only two locations the user actually chooses.
    ///
    /// Everything else is inside the library root, so it follows from this
    /// answer; asking about each folder in turn is noise, not consent.
    pub const ASKED: [Field; 2] = [Field::LibraryRoot, Field::DownloadDir];

    /// Every directory the application uses. Reported on the Diagnostics page.
    pub const DIRS: [Field; 7] = [
        Field::LibraryRoot,
        Field::DownloadDir,
        Field::SymbolDir,
        Field::FootprintDir,
        Field::ModelDir,
        Field::TempDir,
        Field::ArchiveDir,
    ];

    /// Everything the library owns, and therefore everything that has to live
    /// under the library root.
    ///
    /// The ZIP folder is deliberately absent: archives are the *input*, they
    /// come from wherever the browser puts them.
    pub const INSIDE: [Field; 7] = [
        Field::SymbolDir,
        Field::FootprintDir,
        Field::ModelDir,
        Field::TempDir,
        Field::ArchiveDir,
        Field::MergedSymbolLib,
        Field::ManifestPath,
    ];

    /// Where this field sits relative to the library root by default. Used as a
    /// fallback when a path is pulled back inside and has no usable name.
    pub fn default_leaf(self) -> &'static str {
        match self {
            Field::SymbolDir => "symbols",
            Field::FootprintDir => "footprints.pretty",
            Field::ModelDir => "3dmodels",
            Field::TempDir => "cache",
            Field::ArchiveDir => "archive",
            Field::MergedSymbolLib => "murky-informis.kicad_sym",
            Field::ManifestPath => "library.json",
            _ => "",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Field::LibraryRoot => "Library Root",
            Field::DownloadDir => "ZIP Folder",
            Field::SymbolDir => "Symbols",
            Field::FootprintDir => "Footprints (.pretty)",
            Field::ModelDir => "3D Models",
            Field::TempDir => "Temp",
            Field::ArchiveDir => "Archive",
            Field::MergedSymbolLib => "Merged Symbol Library",
            Field::ManifestPath => "Manifest (JSON)",
            Field::PerPartFootprintDirs => "Per-part .pretty folders",
            Field::BackupBeforeOverwrite => "Backup before overwrite",
            Field::DeleteZip => "Delete ZIP after import",
        }
    }

    pub fn kind(self) -> Kind {
        match self {
            Field::LibraryRoot
            | Field::DownloadDir
            | Field::SymbolDir
            | Field::FootprintDir
            | Field::ModelDir
            | Field::TempDir
            | Field::ArchiveDir => Kind::Dir,
            Field::MergedSymbolLib | Field::ManifestPath => Kind::File,
            Field::PerPartFootprintDirs | Field::BackupBeforeOverwrite | Field::DeleteZip => {
                Kind::Bool
            }
        }
    }

    /// The directory this field names, for the fields that name one.
    pub fn dir(self, cfg: &Config) -> Option<&PathBuf> {
        match self {
            Field::LibraryRoot => Some(&cfg.library_root),
            Field::DownloadDir => Some(&cfg.download_dir),
            Field::SymbolDir => Some(&cfg.symbol_dir),
            Field::FootprintDir => Some(&cfg.footprint_dir),
            Field::ModelDir => Some(&cfg.model_dir),
            Field::TempDir => Some(&cfg.temp_dir),
            Field::ArchiveDir => Some(&cfg.archive_dir),
            _ => None,
        }
    }

    /// Current value as a path, for both `Dir` and `File` fields.
    pub fn path(self, cfg: &Config) -> Option<PathBuf> {
        match self {
            Field::MergedSymbolLib => cfg.merged_symbol_lib.clone(),
            Field::ManifestPath => Some(cfg.manifest_path.clone()),
            other => other.dir(cfg).cloned(),
        }
    }

    pub fn set_path(self, cfg: &mut Config, value: PathBuf) {
        match self {
            Field::LibraryRoot => cfg.library_root = value,
            Field::DownloadDir => cfg.download_dir = value,
            Field::SymbolDir => cfg.symbol_dir = value,
            Field::FootprintDir => cfg.footprint_dir = value,
            Field::ModelDir => cfg.model_dir = value,
            Field::TempDir => cfg.temp_dir = value,
            Field::ArchiveDir => cfg.archive_dir = value,
            // An empty path turns the merged library off.
            Field::MergedSymbolLib => {
                cfg.merged_symbol_lib = (!value.as_os_str().is_empty()).then_some(value);
            }
            Field::ManifestPath => cfg.manifest_path = value,
            _ => {}
        }
    }

    pub fn bool_value(self, cfg: &Config) -> bool {
        match self {
            Field::PerPartFootprintDirs => cfg.per_part_footprint_dirs,
            Field::BackupBeforeOverwrite => cfg.backup_before_overwrite,
            _ => cfg.delete_zip_after_import,
        }
    }

    pub fn set_bool(self, cfg: &mut Config, value: bool) {
        match self {
            Field::PerPartFootprintDirs => cfg.per_part_footprint_dirs = value,
            Field::BackupBeforeOverwrite => cfg.backup_before_overwrite = value,
            Field::DeleteZip => cfg.delete_zip_after_import = value,
            _ => {}
        }
    }

    pub fn toggle(self, cfg: &mut Config) {
        if self.kind() == Kind::Bool {
            self.set_bool(cfg, !self.bool_value(cfg));
        }
    }

    pub fn display(self, cfg: &Config) -> String {
        match self.kind() {
            Kind::Bool => self.bool_value(cfg).to_string(),
            _ => match self.path(cfg) {
                Some(p) => p.display().to_string(),
                None => "(disabled)".to_string(),
            },
        }
    }
}

/// A saved library location.
///
/// Everything below the root is derived by [`Config::rebase`], so a project is
/// only a name and where its tree lives — switching is one keypress rather than
/// nine path edits.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Project {
    pub name: String,
    pub library_root: PathBuf,
    /// The ZIP folder to read while this project is active. Absent means "keep
    /// whatever is configured", which is what a shared download folder wants.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub download_dir: Option<PathBuf>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub library_root: PathBuf,
    pub download_dir: PathBuf,
    pub symbol_dir: PathBuf,
    /// KiCad footprint libraries are directories named `*.pretty`.
    pub footprint_dir: PathBuf,
    pub model_dir: PathBuf,
    pub temp_dir: PathBuf,
    pub archive_dir: PathBuf,

    /// Combined symbol library every imported symbol is also appended to.
    /// `None` (an empty string in TOML) disables merging.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub merged_symbol_lib: Option<PathBuf>,
    /// JSON description of the library's file structure.
    pub manifest_path: PathBuf,

    /// Also write each footprint to `<footprint_dir>/<PART>.pretty/`.
    pub per_part_footprint_dirs: bool,
    /// Copy an existing symbol file to `<stem>.bak` before replacing it.
    pub backup_before_overwrite: bool,
    pub delete_zip_after_import: bool,

    /// Saved library locations, in the order the Projects screen shows them.
    ///
    /// Declared last on purpose: TOML requires arrays of tables to follow every
    /// plain key, and `toml::to_string_pretty` writes fields in this order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub projects: Vec<Project>,
}

impl Default for Config {
    fn default() -> Self {
        let root = home_dir().join("KiCad").join("lib");
        Self {
            download_dir: home_dir().join("Downloads").join("KiCad"),
            symbol_dir: root.join("symbols"),
            footprint_dir: root.join("footprints.pretty"),
            model_dir: root.join("3dmodels"),
            temp_dir: root.join("cache"),
            archive_dir: root.join("archive"),
            merged_symbol_lib: Some(root.join("murky-informis.kicad_sym")),
            manifest_path: root.join("library.json"),
            per_part_footprint_dirs: true,
            backup_before_overwrite: true,
            delete_zip_after_import: false,
            projects: Vec::new(),
            library_root: root,
        }
    }
}

impl Config {
    /// Reads `path`, falling back to defaults when it does not exist yet.
    /// A missing file is written out so the user has something to edit.
    pub fn load(path: &Path) -> Self {
        match std::fs::read_to_string(path) {
            Ok(text) => match toml::from_str::<Config>(&text) {
                Ok(mut cfg) => {
                    logging::info(format!("loaded config from {}", path.display()));
                    cfg.normalize(path);
                    cfg
                }
                Err(e) => {
                    logging::error(format!(
                        "config at {} is not valid TOML ({e}); falling back to defaults",
                        path.display()
                    ));
                    let mut cfg = Config::default();
                    cfg.normalize(path);
                    cfg
                }
            },
            Err(e) => {
                logging::warn(format!(
                    "no config at {} ({e}); writing defaults",
                    path.display()
                ));
                let mut cfg = Config::default();
                cfg.normalize(path);
                if let Err(e) = cfg.save(path) {
                    logging::error(format!("could not write default config: {e}"));
                }
                cfg
            }
        }
    }

    pub fn save(&self, path: &Path) -> Result<(), String> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("creating {}: {e}", parent.display()))?;
        }
        let text = toml::to_string_pretty(self).map_err(|e| e.to_string())?;
        std::fs::write(path, text).map_err(|e| format!("writing {}: {e}", path.display()))?;
        logging::info(format!("saved config to {}", path.display()));
        for f in Field::ALL {
            logging::info(format!("  {} = {}", f.label(), f.display(self)));
        }
        for p in &self.projects {
            logging::info(format!(
                "  project {} = {}",
                p.name,
                p.library_root.display()
            ));
        }
        Ok(())
    }

    /// Expands `~` and resolves any remaining relative path so that later
    /// filesystem calls never depend on the process working directory.
    pub fn normalize(&mut self, config_path: &Path) {
        let base = config_path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(home_dir);
        for f in Field::ALL {
            if f.kind() == Kind::Bool {
                continue;
            }
            let Some(current) = f.path(self) else {
                continue;
            };
            let resolved = resolve(&current, &base);
            if resolved != current {
                logging::info(format!(
                    "{} resolved {} -> {}",
                    f.label(),
                    current.display(),
                    resolved.display()
                ));
            }
            f.set_path(self, resolved);
        }
        for project in &mut self.projects {
            project.library_root = resolve(&project.library_root, &base);
            project.download_dir = project.download_dir.as_ref().map(|p| resolve(p, &base));
        }

        // A hand-edited file can put a library folder anywhere; the root is the
        // authority, so bring the strays in and say which ones moved.
        for field in self.contain() {
            logging::warn(format!(
                "{} was outside the library root; moved to {}",
                field.label(),
                field.display(self)
            ));
        }
    }

    /// Moves the library root to `root`, taking the whole library with it.
    ///
    /// A path already inside the old tree keeps its position within it, so a
    /// nested layout survives the move. Anything else is pulled in by
    /// [`Config::contain`]. Returns the fields that moved, for the status line
    /// and the log.
    pub fn rebase(&mut self, root: &Path) -> Vec<Field> {
        let old = self.library_root.clone();
        let mut moved = Vec::new();
        for field in Field::INSIDE {
            let Some(current) = field.path(self) else {
                continue;
            };
            let Ok(rest) = current.strip_prefix(&old) else {
                continue;
            };
            let target = root.join(rest);
            if target != current {
                field.set_path(self, target);
                moved.push(field);
            }
        }
        self.library_root = root.to_path_buf();

        for field in self.contain() {
            if !moved.contains(&field) {
                moved.push(field);
            }
        }
        moved
    }

    /// Pulls every library file back under the library root.
    ///
    /// A KiCad library is one self-contained folder: the symbols, footprints, 3D
    /// models, merged library and manifest all belong inside it, so the root can
    /// be moved, copied or handed to someone else in one piece. A path pointing
    /// somewhere else keeps its own name but is re-anchored under the root.
    ///
    /// Returns the fields that had to be moved.
    pub fn contain(&mut self) -> Vec<Field> {
        let root = self.library_root.clone();
        let mut moved = Vec::new();
        for field in Field::INSIDE {
            let Some(current) = field.path(self) else {
                continue;
            };
            if current.starts_with(&root) {
                continue;
            }
            // The user's own name for the folder or file is worth keeping; only
            // a path with no usable last component falls back to the default.
            let leaf = current
                .file_name()
                .filter(|n| !n.is_empty())
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from(field.default_leaf()));
            let target = root.join(leaf);
            if target != current {
                field.set_path(self, target);
                moved.push(field);
            }
        }
        moved
    }

    /// The saved project the live configuration is currently sitting in.
    pub fn active_project(&self) -> Option<&Project> {
        self.projects
            .iter()
            .find(|p| p.library_root == self.library_root)
    }

    /// Switches to a saved project: rebases onto its root, and takes its ZIP
    /// folder when it has one of its own.
    pub fn switch_to(&mut self, project: &Project) -> Vec<Field> {
        let mut moved = self.rebase(&project.library_root);
        if let Some(dir) = &project.download_dir {
            if self.download_dir != *dir {
                self.download_dir = dir.clone();
                moved.push(Field::DownloadDir);
            }
        }
        moved
    }

    /// Saves the current location under `name`, replacing any project already
    /// using it. Returns the index it landed at.
    ///
    /// Insertion order is preserved rather than sorted: the user's own ordering
    /// of their projects is more useful than an alphabetical one.
    pub fn remember_project(&mut self, name: &str) -> usize {
        let name = name.trim();
        let project = Project {
            name: name.to_string(),
            library_root: self.library_root.clone(),
            download_dir: Some(self.download_dir.clone()),
        };
        match self
            .projects
            .iter()
            .position(|p| p.name.eq_ignore_ascii_case(name))
        {
            Some(i) => {
                self.projects[i] = project;
                i
            }
            None => {
                self.projects.push(project);
                self.projects.len() - 1
            }
        }
    }

    /// `base`, or `base 2`, `base 3`… — whatever is not already taken by a
    /// project pointing somewhere else. Two different folders both called `lib`
    /// must not overwrite each other's bookmark.
    pub fn unique_project_name(&self, base: &str) -> String {
        let free = |candidate: &str| {
            self.projects.iter().all(|p| {
                !p.name.eq_ignore_ascii_case(candidate) || p.library_root == self.library_root
            })
        };
        if free(base) {
            return base.to_string();
        }
        (2..)
            .map(|n| format!("{base} {n}"))
            .find(|candidate| free(candidate))
            .unwrap_or_else(|| base.to_string())
    }

    pub fn find_project(&self, name: &str) -> Option<&Project> {
        let name = name.trim();
        self.projects
            .iter()
            .find(|p| p.name.eq_ignore_ascii_case(name))
    }

    /// The chosen locations that are not present on disk.
    ///
    /// Only [`Field::ASKED`] is checked. The folders inside the library are
    /// created along with the root, and the importer creates any that are
    /// missing later, so prompting for them individually asks the same question
    /// five times over.
    pub fn missing_dirs(&self) -> Vec<Field> {
        Field::ASKED
            .into_iter()
            .filter(|f| f.dir(self).map(|p| !p.is_dir()).unwrap_or(false))
            .collect()
    }

    /// The library's own folders, in the order they would be created.
    pub fn library_dirs(&self) -> Vec<PathBuf> {
        let mut dirs = vec![self.library_root.clone()];
        for field in Field::INSIDE {
            if field.kind() != Kind::Dir {
                continue;
            }
            if let Some(dir) = field.path(self) {
                dirs.push(dir);
            }
        }
        dirs
    }
}

/// A name to offer when saving `root` as a project.
///
/// The leaf folder is usually the right answer, except when it is a generic
/// container — `~/pcb/synth/lib` is the synth project, not the "lib" project.
pub fn project_name_for(root: &Path) -> String {
    const GENERIC: [&str; 6] = ["lib", "libs", "library", "libraries", "kicad", "kicad-lib"];
    let leaf = |p: &Path| {
        p.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .filter(|n| !n.is_empty())
    };
    match leaf(root) {
        Some(name) if GENERIC.contains(&name.to_lowercase().as_str()) => {
            root.parent().and_then(leaf).unwrap_or(name)
        }
        Some(name) => name,
        None => "library".to_string(),
    }
}

pub fn home_dir() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"))
}

/// Default config location: `$XDG_CONFIG_HOME/cse-importer/config.toml`.
pub fn default_config_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| home_dir().join(".config"))
        .join("cse-importer")
        .join("config.toml")
}

/// `~/foo` and `~` expand against the home directory; relative paths are
/// anchored to `base` (the config file's directory) rather than the cwd.
pub fn resolve(path: &Path, base: &Path) -> PathBuf {
    let expanded = expand_tilde(path);
    if expanded.is_absolute() {
        expanded
    } else {
        base.join(expanded)
    }
}

pub fn expand_tilde(path: &Path) -> PathBuf {
    let s = path.to_string_lossy();
    if s == "~" {
        return home_dir();
    }
    if let Some(rest) = s.strip_prefix("~/") {
        return home_dir().join(rest);
    }
    path.to_path_buf()
}
