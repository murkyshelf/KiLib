//! Built-in filesystem browser used whenever a path needs choosing.

use std::path::{Path, PathBuf};

use crate::config::Field;
use crate::logging;

/// Where the chosen directory should be written back to.
#[derive(Clone, Copy, PartialEq)]
pub enum Target {
    /// A row on the Settings screen.
    Setting(Field),
    /// The "[C] Change Path" branch of startup validation.
    Validation(Field),
    /// A library root chosen from the Projects screen. Everything under the old
    /// root follows it, and the location is saved as a project.
    ProjectRoot,
}

impl Target {
    pub fn label(self) -> &'static str {
        match self {
            Target::Setting(f) | Target::Validation(f) => f.label(),
            Target::ProjectRoot => "Library Root for a project",
        }
    }
}

pub struct Picker {
    pub cwd: PathBuf,
    pub entries: Vec<PathBuf>,
    pub selected: usize,
    pub target: Target,
    pub error: Option<String>,
}

impl Picker {
    /// Opens at `start`, walking up to the nearest existing ancestor if the
    /// configured path does not exist yet.
    pub fn new(start: &Path, target: Target) -> Self {
        let mut cwd = start.to_path_buf();
        while !cwd.is_dir() {
            match cwd.parent() {
                Some(p) => cwd = p.to_path_buf(),
                None => {
                    cwd = PathBuf::from("/");
                    break;
                }
            }
        }
        let mut picker = Picker {
            cwd,
            entries: Vec::new(),
            selected: 0,
            target,
            error: None,
        };
        picker.reload();
        picker
    }

    pub fn reload(&mut self) {
        self.entries.clear();
        self.error = None;
        match std::fs::read_dir(&self.cwd) {
            Ok(read) => {
                for entry in read.flatten() {
                    let path = entry.path();
                    // Directories only: the picker selects folders.
                    if !path.is_dir() {
                        continue;
                    }
                    let hidden = path
                        .file_name()
                        .map(|n| n.to_string_lossy().starts_with('.'))
                        .unwrap_or(false);
                    if hidden {
                        continue;
                    }
                    self.entries.push(path);
                }
                self.entries
                    .sort_by_key(|p| p.file_name().map(|n| n.to_string_lossy().to_lowercase()));
            }
            Err(e) => {
                let msg = format!("cannot open {}: {e}", self.cwd.display());
                logging::error(&msg);
                self.error = Some(msg);
            }
        }
        self.selected = 0;
    }

    pub fn move_by(&mut self, delta: isize) {
        if self.entries.is_empty() {
            return;
        }
        let len = self.entries.len() as isize;
        let next = (self.selected as isize + delta).rem_euclid(len);
        self.selected = next as usize;
    }

    /// Enter: descend into the highlighted folder.
    pub fn open(&mut self) {
        if let Some(path) = self.entries.get(self.selected).cloned() {
            self.cwd = path;
            self.reload();
        }
    }

    /// Backspace: go to the parent folder.
    pub fn parent(&mut self) {
        if let Some(parent) = self.cwd.parent().map(Path::to_path_buf) {
            self.cwd = parent;
            self.reload();
        }
    }

    /// Space: accept the current directory.
    pub fn selection(&self) -> PathBuf {
        self.cwd.clone()
    }
}
