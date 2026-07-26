//! Application state and the event loop.

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use notify::RecommendedWatcher;
use ratatui::widgets::ListState;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender};
use std::time::{Duration, Instant};

use crate::config::{Config, Field, Kind};
use crate::importer::{self, ImportSummary};
use crate::library;
use crate::logging;
use crate::manifest;
use crate::online::{self, Hit};
use crate::picker::{Picker, Target};
use crate::scan::{self, ZipEntry};
use crate::watcher;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Screen {
    Main,
    Settings,
    Picker,
    Diagnostics,
    Validate,
    Search,
    Projects,
    Overwrite,
}

/// Messages funnelled into the single event loop.
pub enum Event {
    Key(KeyEvent),
    /// The watcher saw something change under the download directory.
    FsChanged,
    ImportProgress {
        fraction: f64,
        name: String,
    },
    /// One job has been unpacked and knows what it would overwrite. Nothing has
    /// been written to the library yet.
    ImportPrepared(Box<Result<importer::Pending, String>>),
    ImportDone(Box<Result<ImportSummary, String>>),
    /// An online catalogue search finished.
    SearchDone(Box<Result<Vec<Hit>, String>>),
    Tick,
}

/// Which of the two main-screen lists the keyboard drives.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Focus {
    Queue,
    Library,
}

pub struct ImportState {
    pub name: String,
    pub fraction: f64,
}

/// One thing to import. A ZIP and a catalogue part differ only in how their
/// files are produced, so they queue and confirm identically.
pub enum Job {
    Zip(Box<ZipEntry>),
    Web(Box<Hit>),
}

impl Job {
    fn label(&self) -> String {
        match self {
            Job::Zip(entry) => entry.display.clone(),
            Job::Web(hit) => format!("{} ({})", hit.mpn, hit.id),
        }
    }
}

/// An import run: one job or a whole queue of them, tracked the same way so
/// there is a single path through preparation, confirmation and installation.
pub struct Run {
    jobs: std::collections::VecDeque<Job>,
    pub total: usize,
    pub done: usize,
    pub imported: usize,
    pub skipped: usize,
    pub failures: Vec<String>,
    /// Set by [A] on the overwrite prompt: stop asking for the rest of this run.
    pub overwrite_all: bool,
}

pub struct Settings {
    pub cursor: usize,
    /// `Some` while a row is being typed into.
    pub editing: Option<String>,
    /// Working copy; only written to disk on [S]ave.
    pub draft: Config,
    pub dirty: bool,
    /// What the last edit did beyond the row itself — currently the list of
    /// paths a Library Root change carried with it.
    pub note: String,
}

impl Settings {
    fn new(draft: Config) -> Self {
        Settings {
            cursor: 0,
            editing: None,
            draft,
            dirty: false,
            note: String::new(),
        }
    }
}

/// Saved library locations. Switching between them is what makes the settings
/// worth opening often, so it has its own screen rather than a settings row.
pub struct Projects {
    pub cursor: usize,
    /// `Some` while a name is being typed.
    pub naming: Option<Naming>,
    pub message: String,
}

pub struct Naming {
    pub buffer: String,
    /// The project being renamed, or `None` to save the current location as a
    /// new one.
    pub target: Option<usize>,
}

pub struct Validate {
    pub missing: Vec<Field>,
    pub index: usize,
}

/// Online component search: a query box above a result list.
pub struct Search {
    pub query: String,
    /// `true` while the query box has focus, `false` while browsing results.
    pub editing: bool,
    pub results: Vec<Hit>,
    pub cursor: usize,
    /// A search request is in flight.
    pub searching: bool,
    pub message: String,
}

impl Search {
    fn new() -> Self {
        Search {
            query: String::new(),
            editing: true,
            results: Vec::new(),
            cursor: 0,
            searching: false,
            message: "Type a part number or description, then press Enter.".to_string(),
        }
    }
}

pub struct App {
    pub config: Config,
    pub config_path: PathBuf,
    pub screen: Screen,

    pub zips: Vec<ZipEntry>,
    pub list: ListState,
    pub last_scan: Option<chrono::DateTime<chrono::Local>>,
    pub fs_errors: Vec<String>,

    /// Parts already in the library, read back from `library.json`.
    pub library: Vec<manifest::Part>,
    pub library_list: ListState,
    pub totals: manifest::Totals,

    pub focus: Focus,
    /// Incremental filter over the focused list. Empty means "show all".
    pub filter: String,
    /// `true` while the filter is being typed.
    pub filtering: bool,
    /// Indices into `zips` / `library` that survive the filter.
    pub queue_view: Vec<usize>,
    pub library_view: Vec<usize>,

    pub status: String,
    pub import: Option<ImportState>,
    pub settings: Option<Settings>,
    pub picker: Option<Picker>,
    pub validate: Option<Validate>,
    pub search: Option<Search>,
    pub projects: Option<Projects>,

    /// The import run in progress, if any.
    pub run: Option<Run>,
    /// An unpacked import waiting for the user to confirm an overwrite.
    pub pending: Option<importer::Pending>,
    /// The screen the overwrite prompt interrupted, and returns to.
    pub behind: Screen,

    pub cwd: String,
    pub exe: String,

    /// Diagnostics is scrollable: paths are shown in full there, so on a small
    /// terminal the content can exceed the viewport.
    pub diag_scroll: u16,
    /// Rendered line count, written by the UI so scrolling can be clamped.
    pub diag_lines: u16,
    pub diag_view: u16,

    tx: Sender<Event>,
    _watcher: Option<RecommendedWatcher>,
    pending_rescan: Option<Instant>,
    pub should_quit: bool,
}

impl App {
    pub fn new(config: Config, config_path: PathBuf, tx: Sender<Event>) -> Self {
        let cwd = std::env::current_dir()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|e| format!("<unavailable: {e}>"));
        let exe = std::env::current_exe()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|e| format!("<unavailable: {e}>"));

        let missing = config.missing_dirs();
        let screen = if missing.is_empty() {
            Screen::Main
        } else {
            Screen::Validate
        };
        let validate = (!missing.is_empty()).then_some(Validate { missing, index: 0 });

        let mut app = App {
            config,
            config_path,
            screen,
            zips: Vec::new(),
            list: ListState::default(),
            last_scan: None,
            fs_errors: Vec::new(),
            library: Vec::new(),
            library_list: ListState::default(),
            totals: manifest::Totals::default(),
            focus: Focus::Queue,
            filter: String::new(),
            filtering: false,
            queue_view: Vec::new(),
            library_view: Vec::new(),
            status: "Ready".to_string(),
            import: None,
            settings: None,
            picker: None,
            validate,
            search: None,
            projects: None,
            run: None,
            pending: None,
            behind: Screen::Main,
            cwd,
            exe,
            diag_scroll: 0,
            diag_lines: 0,
            diag_view: 0,
            tx,
            _watcher: None,
            pending_rescan: None,
            should_quit: false,
        };
        // Only start watching/scanning once the directories are known good.
        if app.screen == Screen::Main {
            app.refresh();
            app.restart_watcher();
            // Start on whichever pane has something in it. Only at startup —
            // moving focus under the user later would be worse than a dead pane.
            if app.zips.is_empty() && !app.library.is_empty() {
                app.focus = Focus::Library;
            }
        }
        app
    }

    // ---------------------------------------------------------------- scanning

    /// Full rescan of the configured ZIP directory, plus a reload of the
    /// library manifest so both panes stay in step.
    pub fn refresh(&mut self) {
        let result = scan::scan(&self.config);
        self.fs_errors = result.errors;
        self.zips = result.entries;
        self.last_scan = Some(chrono::Local::now());
        self.reload_library();
        self.recompute_views();

        // Deliberately does not reset the status line on success: a rescan is
        // often triggered by the watcher moments after an import, and would
        // otherwise wipe the result the user is trying to read.
        if let Some(err) = self.fs_errors.first() {
            self.status = format!("Scan error: {err}");
        }
    }

    /// The library pane shows what the folders actually contain: the parts
    /// `library.json` records, plus anything else already sitting in the tree.
    /// A library built before this tool existed is still a library.
    fn reload_library(&mut self) {
        let mut parts = manifest::Manifest::load(&self.config.manifest_path).parts;
        let mut discovered = 0usize;
        for entry in library::scan(&self.config) {
            match parts.iter_mut().find(|p| p.name == entry.name) {
                // An imported part can still have files beside it the manifest
                // never recorded — a model dropped in by hand, a footprint left
                // from an earlier import. The folders are the truth.
                Some(known) => absorb(known, entry),
                None => {
                    discovered += 1;
                    parts.push(entry);
                }
            }
        }
        parts.sort_by_key(|p| p.name.to_lowercase());

        if discovered > 0 {
            logging::info(format!(
                "{discovered} part(s) found in the library but not in the manifest"
            ));
        }
        self.totals = manifest::totals(&parts);
        self.library = parts;
    }

    /// Applies the filter and keeps both selections inside their lists.
    pub fn recompute_views(&mut self) {
        let needle = self.filter.trim().to_lowercase();
        let matches =
            |haystack: &str| needle.is_empty() || haystack.to_lowercase().contains(&needle);

        self.queue_view = self
            .zips
            .iter()
            .enumerate()
            .filter(|(_, z)| matches(&z.display))
            .map(|(i, _)| i)
            .collect();
        self.library_view = self
            .library
            .iter()
            .enumerate()
            .filter(|(_, p)| matches(&p.name) || matches(&p.source))
            .map(|(i, _)| i)
            .collect();

        clamp(&mut self.list, self.queue_view.len());
        clamp(&mut self.library_list, self.library_view.len());
    }

    /// The ZIP the queue pane is pointing at.
    pub fn selected_zip(&self) -> Option<&ZipEntry> {
        let index = *self.queue_view.get(self.list.selected()?)?;
        self.zips.get(index)
    }

    /// The library entry the library pane is pointing at.
    pub fn selected_part(&self) -> Option<&manifest::Part> {
        let index = *self.library_view.get(self.library_list.selected()?)?;
        self.library.get(index)
    }

    /// Whether a search result is already in the library, matched on the
    /// recorded source first because that is exact.
    pub fn library_has(&self, hit: &Hit) -> bool {
        let source = format!("LCSC {}", hit.id);
        let name = online::sanitize(&hit.mpn);
        self.library
            .iter()
            .any(|p| p.source == source || (!name.is_empty() && p.name == name))
    }

    pub fn restart_watcher(&mut self) {
        let tx = self.tx.clone();
        let (fs_tx, fs_rx) = std::sync::mpsc::channel::<()>();
        std::thread::spawn(move || {
            while fs_rx.recv().is_ok() {
                if tx.send(Event::FsChanged).is_err() {
                    break;
                }
            }
        });

        match watcher::watch(&self.config.download_dir, fs_tx) {
            Ok(w) => self._watcher = Some(w),
            Err(e) => {
                logging::error(&e);
                self.fs_errors.push(e.clone());
                self.status = format!("Watch failed: {e}");
                self._watcher = None;
            }
        }
    }

    /// Re-reads config-derived state after paths change.
    pub fn apply_config_change(&mut self) {
        logging::info("configuration changed; rescanning and re-watching");
        self.refresh();
        self.restart_watcher();
        if self.fs_errors.is_empty() {
            self.status = "Ready".to_string();
        }
    }

    // ------------------------------------------------------------------- loop

    pub fn handle(&mut self, event: Event) {
        match event {
            Event::Key(key) if key.kind == KeyEventKind::Press => self.on_key(key),
            Event::Key(_) => {}
            Event::FsChanged => {
                // Coalesce bursts: editors and browsers emit several events per
                // saved file.
                self.pending_rescan = Some(Instant::now());
            }
            Event::ImportProgress { fraction, name } => {
                self.import = Some(ImportState { fraction, name });
            }
            Event::ImportPrepared(result) => self.on_prepared(*result),
            Event::ImportDone(result) => self.job_done(*result),
            Event::SearchDone(result) => {
                let Some(search) = self.search.as_mut() else {
                    return;
                };
                search.searching = false;
                match *result {
                    Ok(hits) => {
                        search.cursor = 0;
                        search.message = if hits.is_empty() {
                            format!("No results for \"{}\".", search.query)
                        } else {
                            // Results take focus so Enter adds a part rather
                            // than repeating the search.
                            search.editing = false;
                            format!("{} result(s).", hits.len())
                        };
                        search.results = hits;
                    }
                    Err(e) => {
                        logging::error(format!("search failed: {e}"));
                        search.message = format!("Search failed: {e}");
                    }
                }
            }
            Event::Tick => {
                if let Some(at) = self.pending_rescan {
                    if at.elapsed() >= Duration::from_millis(300) {
                        self.pending_rescan = None;
                        logging::info("filesystem change detected; rescanning");
                        self.refresh();
                    }
                }
            }
        }
    }

    fn on_key(&mut self, key: KeyEvent) {
        // Ctrl-C always exits, whatever screen is up.
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            self.should_quit = true;
            return;
        }
        // F12 toggles diagnostics from anywhere except a text edit.
        if key.code == KeyCode::F(12) && !self.is_editing() {
            self.screen = if self.screen == Screen::Diagnostics {
                Screen::Main
            } else {
                self.diag_scroll = 0;
                Screen::Diagnostics
            };
            return;
        }

        match self.screen {
            Screen::Main => self.on_key_main(key),
            Screen::Settings => self.on_key_settings(key),
            Screen::Picker => self.on_key_picker(key),
            Screen::Diagnostics => self.on_key_diagnostics(key),
            Screen::Validate => self.on_key_validate(key),
            Screen::Search => self.on_key_search(key),
            Screen::Projects => self.on_key_projects(key),
            Screen::Overwrite => self.on_key_overwrite(key),
        }
    }

    fn on_key_diagnostics(&mut self, key: KeyEvent) {
        // Never scroll past the last line.
        let max = self.diag_lines.saturating_sub(self.diag_view);
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => self.screen = Screen::Main,
            KeyCode::Down | KeyCode::Char('j') => {
                self.diag_scroll = (self.diag_scroll + 1).min(max);
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.diag_scroll = self.diag_scroll.saturating_sub(1);
            }
            KeyCode::PageDown => {
                self.diag_scroll = (self.diag_scroll + self.diag_view.max(1)).min(max);
            }
            KeyCode::PageUp => {
                self.diag_scroll = self.diag_scroll.saturating_sub(self.diag_view.max(1));
            }
            KeyCode::Home => self.diag_scroll = 0,
            KeyCode::End => self.diag_scroll = max,
            _ => {}
        }
    }

    fn is_editing(&self) -> bool {
        let settings = self
            .settings
            .as_ref()
            .map(|s| s.editing.is_some())
            .unwrap_or(false);
        let search = self.search.as_ref().map(|s| s.editing).unwrap_or(false);
        let naming = self
            .projects
            .as_ref()
            .map(|p| p.naming.is_some())
            .unwrap_or(false);
        settings || search || naming || self.filtering
    }

    // ------------------------------------------------------------ main screen

    fn on_key_main(&mut self, key: KeyEvent) {
        // The filter is an incremental text field, so it takes character keys
        // before any single-letter shortcut can see them.
        if self.filtering {
            match key.code {
                KeyCode::Char(c) => {
                    self.filter.push(c);
                    self.recompute_views();
                }
                KeyCode::Backspace => {
                    self.filter.pop();
                    self.recompute_views();
                }
                // Enter keeps the filter and hands the keyboard back to the list.
                KeyCode::Enter | KeyCode::Down | KeyCode::Up => self.filtering = false,
                KeyCode::Esc => {
                    self.filtering = false;
                    self.filter.clear();
                    self.recompute_views();
                }
                _ => {}
            }
            return;
        }

        match key.code {
            KeyCode::Char('q') => self.should_quit = true,
            // Esc clears a filter first; only an unfiltered view quits.
            KeyCode::Esc => {
                if self.filter.is_empty() {
                    self.should_quit = true;
                } else {
                    self.filter.clear();
                    self.recompute_views();
                }
            }
            KeyCode::Char('/') => {
                self.filtering = true;
            }
            KeyCode::Tab | KeyCode::BackTab | KeyCode::Left | KeyCode::Right => {
                self.focus = match self.focus {
                    Focus::Queue => Focus::Library,
                    Focus::Library => Focus::Queue,
                };
            }
            KeyCode::Char('r') | KeyCode::Char('R') => {
                self.refresh();
                if self.fs_errors.is_empty() {
                    self.status = format!(
                        "Rescanned: {} ZIP file(s), {} part(s) in library",
                        self.zips.len(),
                        self.totals.parts
                    );
                }
            }
            KeyCode::Char('s') | KeyCode::Char('S') => {
                self.settings = Some(Settings::new(self.config.clone()));
                self.screen = Screen::Settings;
            }
            KeyCode::Char('p') | KeyCode::Char('P') => self.open_projects(),
            KeyCode::Char('w') | KeyCode::Char('W') => {
                self.search = Some(Search::new());
                self.screen = Screen::Search;
            }
            KeyCode::Down | KeyCode::Char('j') => self.move_selection(1),
            KeyCode::Up | KeyCode::Char('k') => self.move_selection(-1),
            KeyCode::Home => self.set_selection(0),
            KeyCode::End => self.set_selection(isize::MAX),
            KeyCode::Enter | KeyCode::Char('i') => self.start_import(),
            KeyCode::Char('a') | KeyCode::Char('A') => self.start_import_all(),
            _ => {}
        }
    }

    fn move_selection(&mut self, delta: isize) {
        let (state, len) = match self.focus {
            Focus::Queue => (&mut self.list, self.queue_view.len()),
            Focus::Library => (&mut self.library_list, self.library_view.len()),
        };
        if len == 0 {
            return;
        }
        let current = state.selected().unwrap_or(0) as isize;
        state.select(Some((current + delta).rem_euclid(len as isize) as usize));
    }

    fn set_selection(&mut self, index: isize) {
        let (state, len) = match self.focus {
            Focus::Queue => (&mut self.list, self.queue_view.len()),
            Focus::Library => (&mut self.library_list, self.library_view.len()),
        };
        if len == 0 {
            return;
        }
        state.select(Some(index.clamp(0, len as isize - 1) as usize));
    }

    fn start_import(&mut self) {
        // Importing is a queue action; pressing Enter in the library pane
        // should not silently act on some other list's selection.
        if self.focus == Focus::Library {
            self.status =
                "Switch to the queue [Tab] to import, or [W] to search online".to_string();
            return;
        }
        let Some(entry) = self.selected_zip().cloned() else {
            self.status = "Nothing to import".to_string();
            return;
        };
        self.start_run(vec![Job::Zip(Box::new(entry))]);
    }

    /// Imports every ZIP currently listed in the queue, one after another.
    fn start_import_all(&mut self) {
        let jobs: Vec<Job> = self
            .queue_view
            .iter()
            .filter_map(|&i| self.zips.get(i).cloned())
            .map(|entry| Job::Zip(Box::new(entry)))
            .collect();
        if jobs.is_empty() {
            self.status = "Nothing to import".to_string();
            return;
        }
        self.start_run(jobs);
    }

    // -------------------------------------------------------------- importing

    /// Begins a run. Every import goes through here, whether it is one archive,
    /// a whole queue or a part from the catalogue.
    fn start_run(&mut self, jobs: Vec<Job>) {
        if self.run.is_some() {
            self.status = "An import is already running".to_string();
            return;
        }
        let total = jobs.len();
        logging::info(format!("import run started: {total} job(s)"));
        self.run = Some(Run {
            jobs: jobs.into(),
            total,
            done: 0,
            imported: 0,
            skipped: 0,
            failures: Vec::new(),
            overwrite_all: false,
        });
        self.status = if total == 1 {
            "Importing...".to_string()
        } else {
            format!("Importing {total} archive(s)...")
        };
        self.next_job();
    }

    /// Starts unpacking the next job, or finishes the run when there are none
    /// left. Unpacking never writes to the library — [`finish_pending`] does.
    fn next_job(&mut self) {
        let Some(run) = self.run.as_mut() else {
            return;
        };
        let Some(job) = run.jobs.pop_front() else {
            self.end_run();
            return;
        };

        let label = job.label();
        let index = run.done;
        let total = run.total;
        self.import = Some(ImportState {
            name: Self::step_label(index, total, &label),
            fraction: index as f64 / total as f64,
        });

        let cfg = self.config.clone();
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let progress_tx = tx.clone();
            // One job's progress is a slice of the whole run.
            let progress = move |fraction: f64, name: String| {
                let _ = progress_tx.send(Event::ImportProgress {
                    fraction: (index as f64 + fraction) / total as f64,
                    name: Self::step_label(index, total, &name),
                });
            };
            let result = match job {
                Job::Zip(entry) => importer::prepare(&cfg, &entry.path, progress),
                Job::Web(hit) => online::prepare(&cfg, &hit.id, progress),
            };
            let _ = tx.send(Event::ImportPrepared(Box::new(result)));
        });
    }

    /// `(2/5) NE555P` while a run has more than one job in it.
    fn step_label(index: usize, total: usize, name: &str) -> String {
        if total <= 1 {
            name.to_string()
        } else {
            format!("({}/{total}) {name}", index + 1)
        }
    }

    /// A job finished, successfully or not; move on to the next.
    fn job_done(&mut self, outcome: Result<ImportSummary, String>) {
        let mut message = None;
        if let Some(run) = self.run.as_mut() {
            run.done += 1;
            match outcome {
                Ok(summary) => {
                    run.imported += 1;
                    if run.total == 1 {
                        message = Some(format!(
                            "Added {}: {} symbol(s), {} footprint(s), {} model(s)",
                            summary.part,
                            summary.symbols.len(),
                            summary.footprints.len(),
                            summary.models.len()
                        ));
                    }
                }
                Err(e) => {
                    // One bad archive must not abandon the rest of the queue.
                    logging::error(format!("import failed: {e}"));
                    self.fs_errors.push(e.clone());
                    run.failures.push(e.clone());
                    if run.total == 1 {
                        message = Some(format!("Import failed: {e}"));
                    }
                }
            }
        }

        // Rescan first: refresh() resets the status line, so the outcome has to
        // be written after it.
        self.refresh();
        if let Some(message) = message {
            if let Some(search) = self.search.as_mut() {
                search.message = message.clone();
            }
            self.status = message;
        }
        self.next_job();
    }

    fn end_run(&mut self) {
        let Some(run) = self.run.take() else {
            return;
        };
        self.import = None;
        self.refresh();

        // A single job already reported itself in full; only a real queue needs
        // a tally.
        if run.total > 1 {
            let mut parts = vec![format!("Imported {}", run.imported)];
            if run.skipped > 0 {
                parts.push(format!("{} skipped", run.skipped));
            }
            if !run.failures.is_empty() {
                parts.push(format!(
                    "{} failed — see [F12] Diagnostics",
                    run.failures.len()
                ));
            }
            self.status = parts.join(", ");
        }
        logging::info(format!(
            "import run finished: {} imported, {} skipped, {} failed",
            run.imported,
            run.skipped,
            run.failures.len()
        ));
    }

    /// An unpacked job is ready. Anything it would replace is confirmed first.
    fn on_prepared(&mut self, result: Result<importer::Pending, String>) {
        let pending = match result {
            Ok(pending) => pending,
            Err(e) => return self.job_done(Err(e)),
        };

        let ask = !pending.conflicts.is_empty()
            && !self.run.as_ref().map(|r| r.overwrite_all).unwrap_or(false);
        if ask {
            // The job is paused, not running; a progress bar behind the prompt
            // would say otherwise.
            self.import = None;
            self.status = format!(
                "{} would replace {} existing file(s)",
                pending.label(),
                pending.conflicts.len()
            );
            self.pending = Some(pending);
            self.behind = self.screen;
            self.screen = Screen::Overwrite;
        } else {
            self.finish_pending(pending);
        }
    }

    /// Places a confirmed import into the library.
    fn finish_pending(&mut self, pending: importer::Pending) {
        let cfg = self.config.clone();
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let result = importer::finish(&cfg, pending);
            let _ = tx.send(Event::ImportDone(Box::new(result)));
        });
    }

    fn on_key_overwrite(&mut self, key: KeyEvent) {
        let Some(pending) = self.pending.take() else {
            self.screen = Screen::Main;
            return;
        };
        self.screen = self.behind;

        match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') => self.finish_pending(pending),
            KeyCode::Char('a') | KeyCode::Char('A') => {
                if let Some(run) = self.run.as_mut() {
                    run.overwrite_all = true;
                }
                self.finish_pending(pending);
            }
            KeyCode::Char('s') | KeyCode::Char('S') => {
                let label = pending.label();
                importer::discard(pending);
                if let Some(run) = self.run.as_mut() {
                    run.skipped += 1;
                    run.done += 1;
                }
                self.status = format!("Skipped {label}; nothing was replaced");
                self.next_job();
            }
            // Cancelling stops the whole run: the user was shown one overwrite
            // and said no, so ploughing on through the rest would be worse.
            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc | KeyCode::Char('q') => {
                let label = pending.label();
                importer::discard(pending);
                if let Some(run) = self.run.as_mut() {
                    run.jobs.clear();
                    run.skipped += 1;
                    run.done += 1;
                }
                self.end_run();
                self.status = format!("Cancelled at {label}; the library is unchanged");
            }
            _ => {
                // Any other key leaves the question on screen.
                self.pending = Some(pending);
                self.screen = Screen::Overwrite;
            }
        }
    }

    // ---------------------------------------------------------- web search

    fn on_key_search(&mut self, key: KeyEvent) {
        let Some(search) = self.search.as_mut() else {
            self.screen = Screen::Main;
            return;
        };

        // Anything that needs `self` again is deferred past the borrow above.
        enum Act {
            None,
            Run,
            Add,
            Close,
        }
        let mut act = Act::None;

        // Typing the query swallows character keys, so the shortcuts below only
        // apply while the result list has focus.
        if search.editing {
            match key.code {
                KeyCode::Char(c) => search.query.push(c),
                KeyCode::Backspace => {
                    search.query.pop();
                }
                KeyCode::Enter => act = Act::Run,
                KeyCode::Down | KeyCode::Tab if !search.results.is_empty() => {
                    search.editing = false
                }
                KeyCode::Esc => act = Act::Close,
                _ => {}
            }
        } else {
            match key.code {
                KeyCode::Esc | KeyCode::Char('q') => act = Act::Close,
                KeyCode::Char('/') | KeyCode::Char('e') | KeyCode::Tab => search.editing = true,
                KeyCode::Down | KeyCode::Char('j') => {
                    if !search.results.is_empty() {
                        search.cursor = (search.cursor + 1) % search.results.len();
                    }
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    if !search.results.is_empty() {
                        search.cursor =
                            (search.cursor + search.results.len() - 1) % search.results.len();
                    }
                }
                KeyCode::Enter => act = Act::Add,
                _ => {}
            }
        }

        match act {
            Act::Run => self.run_search(),
            Act::Add => self.add_selected_result(),
            Act::Close => {
                self.search = None;
                self.screen = Screen::Main;
            }
            Act::None => {}
        }
    }

    fn run_search(&mut self) {
        let Some(search) = self.search.as_mut() else {
            return;
        };
        let query = search.query.trim().to_string();
        if query.is_empty() {
            search.message = "Enter something to search for.".to_string();
            return;
        }
        if search.searching {
            return;
        }
        search.searching = true;
        search.message = format!("Searching for \"{query}\"...");

        // The catalogue call blocks on the network, so it never runs on the
        // thread that draws the interface.
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let result = online::search(&query, online::SEARCH_LIMIT);
            let _ = tx.send(Event::SearchDone(Box::new(result)));
        });
    }

    fn add_selected_result(&mut self) {
        if self.run.is_some() {
            return;
        }
        let Some(search) = self.search.as_mut() else {
            return;
        };
        let Some(hit) = search.results.get(search.cursor).cloned() else {
            search.message = "Nothing selected.".to_string();
            return;
        };
        search.message = format!("Adding {} ({})...", hit.mpn, hit.id);

        // Same pipeline as a ZIP, so a part already in the library asks before
        // replacing it here too.
        self.start_run(vec![Job::Web(Box::new(hit.clone()))]);
        self.status = format!("Downloading {}...", hit.id);
    }

    // -------------------------------------------------------- project screen

    /// Opens the Projects screen with the cursor on wherever we are now.
    fn open_projects(&mut self) {
        let cursor = self
            .config
            .projects
            .iter()
            .position(|p| p.library_root == self.config.library_root)
            .unwrap_or(0);
        let message = match self.config.active_project() {
            Some(p) => format!("Working in \"{}\".", p.name),
            None => "This location is not saved yet — [N] names it.".to_string(),
        };
        self.projects = Some(Projects {
            cursor,
            naming: None,
            message,
        });
        self.screen = Screen::Projects;
    }

    fn on_key_projects(&mut self, key: KeyEvent) {
        // Taken rather than borrowed so the branches below can use the rest of
        // `self` freely; it is put back on every path that stays on the screen.
        let Some(mut projects) = self.projects.take() else {
            self.screen = Screen::Main;
            return;
        };

        // Naming is a text field, so it swallows keys before any shortcut.
        if let Some(mut naming) = projects.naming.take() {
            match key.code {
                KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    naming.buffer.clear();
                    projects.naming = Some(naming);
                }
                KeyCode::Char(c) => {
                    naming.buffer.push(c);
                    projects.naming = Some(naming);
                }
                KeyCode::Backspace => {
                    naming.buffer.pop();
                    projects.naming = Some(naming);
                }
                KeyCode::Esc => projects.message = "Cancelled.".to_string(),
                KeyCode::Enter => {
                    let name = naming.buffer.trim().to_string();
                    if name.is_empty() {
                        projects.message = "A project needs a name.".to_string();
                        projects.naming = Some(naming);
                    } else {
                        match naming.target {
                            Some(i) if i < self.config.projects.len() => {
                                self.config.projects[i].name = name.clone();
                                projects.message = format!("Renamed to \"{name}\".");
                            }
                            Some(_) => projects.message = "That project is gone.".to_string(),
                            None => {
                                projects.cursor = self.config.remember_project(&name);
                                projects.message = format!("Saved this location as \"{name}\".");
                            }
                        }
                        if let Err(e) = self.persist_config() {
                            projects.message = e;
                        }
                    }
                }
                _ => projects.naming = Some(naming),
            }
            self.projects = Some(projects);
            return;
        }

        let count = self.config.projects.len();
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                // Dropping `projects` here is deliberate: the screen is rebuilt
                // from the live configuration next time it is opened.
                self.screen = Screen::Main;
                return;
            }
            KeyCode::Down | KeyCode::Char('j') if count > 0 => {
                projects.cursor = (projects.cursor + 1) % count;
            }
            KeyCode::Up | KeyCode::Char('k') if count > 0 => {
                projects.cursor = (projects.cursor + count - 1) % count;
            }
            KeyCode::Home => projects.cursor = 0,
            KeyCode::End => projects.cursor = count.saturating_sub(1),
            KeyCode::Char('n') | KeyCode::Char('N') => {
                // Somewhere already saved offers its own name, so confirming
                // updates that bookmark instead of adding a duplicate.
                let buffer = match self.config.active_project() {
                    Some(project) => project.name.clone(),
                    None => {
                        let base = crate::config::project_name_for(&self.config.library_root);
                        self.config.unique_project_name(&base)
                    }
                };
                projects.naming = Some(Naming {
                    buffer,
                    target: None,
                });
                projects.message = "Name this location, then press Enter.".to_string();
            }
            KeyCode::Char('e') | KeyCode::Char('E') | KeyCode::F(2) => {
                match self.config.projects.get(projects.cursor) {
                    Some(project) => {
                        projects.naming = Some(Naming {
                            buffer: project.name.clone(),
                            target: Some(projects.cursor),
                        });
                        projects.message = "Rename, then press Enter.".to_string();
                    }
                    None => projects.message = "Nothing to rename.".to_string(),
                }
            }
            KeyCode::Char('d') | KeyCode::Char('D') | KeyCode::Delete
                if projects.cursor < count =>
            {
                let gone = self.config.projects.remove(projects.cursor);
                projects.cursor = projects
                    .cursor
                    .min(self.config.projects.len().saturating_sub(1));
                // Only the bookmark is removed; the library on disk is untouched.
                projects.message = format!("Forgot \"{}\" — its files are untouched.", gone.name);
                if let Err(e) = self.persist_config() {
                    projects.message = e;
                }
            }
            KeyCode::Char('b') | KeyCode::Char('B') | KeyCode::Char(' ') => {
                let start = self.config.library_root.clone();
                self.picker = Some(Picker::new(&start, Target::ProjectRoot));
                self.screen = Screen::Picker;
                self.projects = Some(projects);
                return;
            }
            KeyCode::Enter => match self.config.projects.get(projects.cursor).cloned() {
                Some(project) => {
                    let moved = self.config.switch_to(&project);
                    self.after_location_change(&format!("Switched to {}", project.name), moved);
                    return;
                }
                None => {
                    projects.message = "No projects saved yet — [N] saves this one.".to_string()
                }
            },
            _ => {}
        }
        self.projects = Some(projects);
    }

    /// The shared tail of every location change: log what followed the root,
    /// persist, then either rescan or ask about directories that do not exist.
    fn after_location_change(&mut self, what: &str, moved: Vec<Field>) {
        self.projects = None;
        logging::info(format!(
            "{what}: library root is now {}",
            self.config.library_root.display()
        ));
        for field in &moved {
            logging::info(format!(
                "  {} follows -> {}",
                field.label(),
                field.display(&self.config)
            ));
        }

        let mut status = format!("{what} — {} path(s) moved with it", moved.len());
        if let Err(e) = self.persist_config() {
            status = e;
        }

        let missing = self.config.missing_dirs();
        if missing.is_empty() {
            self.screen = Screen::Main;
            self.apply_config_change();
        } else {
            // A brand-new project's tree does not exist yet. The same prompt
            // that runs at startup asks before anything is created.
            self.validate = Some(Validate { missing, index: 0 });
            self.screen = Screen::Validate;
        }
        self.status = status;
    }

    fn persist_config(&mut self) -> Result<(), String> {
        match self.config.save(&self.config_path) {
            Ok(()) => Ok(()),
            Err(e) => {
                logging::error(format!("saving config: {e}"));
                self.fs_errors.push(e.clone());
                Err(format!("Save failed: {e}"))
            }
        }
    }

    // -------------------------------------------------------- settings screen

    fn on_key_settings(&mut self, key: KeyEvent) {
        let Some(settings) = self.settings.as_mut() else {
            self.screen = Screen::Main;
            return;
        };

        // Text entry swallows most keys.
        if let Some(buffer) = settings.editing.as_mut() {
            match key.code {
                KeyCode::Char(c) => buffer.push(c),
                KeyCode::Backspace => {
                    buffer.pop();
                }
                KeyCode::Enter => {
                    let text = buffer.clone();
                    let field = Field::ALL[settings.cursor];
                    settings.editing = None;
                    Self::commit_field(settings, field, &text, &self.config_path);
                }
                KeyCode::Esc => settings.editing = None,
                _ => {}
            }
            return;
        }

        match key.code {
            KeyCode::Esc => {
                self.settings = None;
                self.screen = Screen::Main;
                self.status = "Settings discarded".to_string();
            }
            KeyCode::Tab | KeyCode::Down | KeyCode::Char('j') => {
                settings.cursor = (settings.cursor + 1) % Field::ALL.len();
            }
            KeyCode::BackTab | KeyCode::Up | KeyCode::Char('k') => {
                settings.cursor = (settings.cursor + Field::ALL.len() - 1) % Field::ALL.len();
            }
            KeyCode::Char('e') | KeyCode::Char('E') | KeyCode::Char(' ') => {
                let field = Field::ALL[settings.cursor];
                if field.kind() == Kind::Bool {
                    field.toggle(&mut settings.draft);
                    settings.dirty = true;
                } else if key.code != KeyCode::Char(' ') {
                    // A disabled optional path starts from an empty buffer.
                    let current = field
                        .path(&settings.draft)
                        .map(|p| p.display().to_string())
                        .unwrap_or_default();
                    settings.editing = Some(current);
                }
            }
            KeyCode::Enter => {
                let field = Field::ALL[settings.cursor];
                if field.kind() != Kind::Bool {
                    // For a file field the picker chooses its parent folder;
                    // the file name is preserved on the way back.
                    let start = field
                        .path(&settings.draft)
                        .unwrap_or_else(crate::config::home_dir);
                    let start = if field.kind() == Kind::File {
                        start
                            .parent()
                            .map(|p| p.to_path_buf())
                            .unwrap_or_else(crate::config::home_dir)
                    } else {
                        start
                    };
                    self.picker = Some(Picker::new(&start, Target::Setting(field)));
                    self.screen = Screen::Picker;
                }
            }
            KeyCode::Char('s') | KeyCode::Char('S') => self.save_settings(),
            _ => {}
        }
    }

    /// Applies a typed value, expanding `~` and resolving relative input.
    fn commit_field(
        settings: &mut Settings,
        field: Field,
        text: &str,
        config_path: &std::path::Path,
    ) {
        let trimmed = text.trim();
        if field.kind() == Kind::Bool {
            let on = matches!(trimmed.to_lowercase().as_str(), "true" | "yes" | "1" | "on");
            field.set_bool(&mut settings.draft, on);
            settings.dirty = true;
            return;
        }
        // Clearing an optional file field disables it; a required directory
        // keeps its previous value rather than becoming empty.
        if trimmed.is_empty() {
            if field == Field::MergedSymbolLib {
                field.set_path(&mut settings.draft, PathBuf::new());
                settings.dirty = true;
            }
            return;
        }
        let base = config_path
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(crate::config::home_dir);
        let resolved = crate::config::resolve(std::path::Path::new(trimmed), &base);
        if field == Field::LibraryRoot {
            Self::rebase_draft(settings, resolved);
        } else {
            field.set_path(&mut settings.draft, resolved);
            settings.dirty = true;
            settings.note = Self::contain_draft(&mut settings.draft);
        }
    }

    /// Keeps the draft's library inside its own root, reporting anything it had
    /// to pull back in.
    fn contain_draft(draft: &mut Config) -> String {
        let moved = draft.contain();
        if moved.is_empty() {
            return String::new();
        }
        format!(
            "A library lives under its root, so this went inside it: {}",
            moved
                .iter()
                .map(|f| f.label())
                .collect::<Vec<_>>()
                .join(", ")
        )
    }

    /// A new library root drags every path inside the old tree along with it.
    ///
    /// Without this, moving to another project means editing nine rows by hand,
    /// and forgetting one silently writes that project's symbols into the last
    /// project's folder. Nothing is written until [S]ave, and all the rows it
    /// touches are visible on the same screen.
    fn rebase_draft(settings: &mut Settings, root: PathBuf) {
        let moved = settings.draft.rebase(&root);
        settings.note = if moved.is_empty() {
            "Nothing else was inside the old root, so no other path moved.".to_string()
        } else {
            format!(
                "Moved with the root: {}",
                moved
                    .iter()
                    .map(|f| f.label())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        };
        settings.dirty = true;
    }

    fn save_settings(&mut self) {
        let Some(settings) = self.settings.take() else {
            return;
        };
        let draft = settings.draft;
        match draft.save(&self.config_path) {
            Ok(()) => {
                self.config = draft;
                self.screen = Screen::Main;
                self.status = "Settings saved".to_string();

                // A saved path may point somewhere that does not exist yet.
                let missing = self.config.missing_dirs();
                if missing.is_empty() {
                    self.apply_config_change();
                } else {
                    self.validate = Some(Validate { missing, index: 0 });
                    self.screen = Screen::Validate;
                }
            }
            Err(e) => {
                logging::error(format!("saving config: {e}"));
                self.status = format!("Save failed: {e}");
                // Hand the draft back rather than the saved configuration, so a
                // failed write does not also discard the user's edits.
                self.settings = Some(Settings {
                    cursor: settings.cursor,
                    editing: None,
                    draft,
                    dirty: true,
                    note: settings.note,
                });
            }
        }
    }

    // ---------------------------------------------------------------- picker

    fn on_key_picker(&mut self, key: KeyEvent) {
        let Some(picker) = self.picker.as_mut() else {
            self.screen = Screen::Main;
            return;
        };
        match key.code {
            KeyCode::Down | KeyCode::Char('j') => picker.move_by(1),
            KeyCode::Up | KeyCode::Char('k') => picker.move_by(-1),
            KeyCode::Enter => picker.open(),
            KeyCode::Backspace => picker.parent(),
            KeyCode::Char(' ') => {
                let chosen = picker.selection();
                let target = picker.target;
                self.picker = None;
                self.accept_pick(target, chosen);
            }
            KeyCode::Esc => {
                let target = picker.target;
                self.picker = None;
                self.screen = match target {
                    Target::Setting(_) => Screen::Settings,
                    Target::Validation(_) => Screen::Validate,
                    Target::ProjectRoot => Screen::Projects,
                };
            }
            _ => {}
        }
    }

    fn accept_pick(&mut self, target: Target, chosen: PathBuf) {
        match target {
            Target::Setting(Field::LibraryRoot) => {
                if let Some(settings) = self.settings.as_mut() {
                    Self::rebase_draft(settings, chosen.clone());
                }
                logging::info(format!("picked Library Root = {}", chosen.display()));
                self.screen = Screen::Settings;
            }
            Target::Setting(field) => {
                if let Some(settings) = self.settings.as_mut() {
                    // The picker returns a folder; a file field keeps its name.
                    let value = if field.kind() == Kind::File {
                        let name = field
                            .path(&settings.draft)
                            .and_then(|p| p.file_name().map(|n| n.to_owned()));
                        match name {
                            Some(name) => chosen.join(name),
                            None => chosen.clone(),
                        }
                    } else {
                        chosen.clone()
                    };
                    field.set_path(&mut settings.draft, value);
                    settings.dirty = true;
                    settings.note = Self::contain_draft(&mut settings.draft);
                }
                logging::info(format!("picked {} = {}", field.label(), chosen.display()));
                self.screen = Screen::Settings;
            }
            Target::Validation(field) => {
                // Repointing the root during validation moves the library with
                // it, exactly as it does everywhere else.
                if field == Field::LibraryRoot {
                    self.config.rebase(&chosen);
                } else {
                    field.set_path(&mut self.config, chosen.clone());
                    self.config.contain();
                }
                logging::info(format!(
                    "validation: {} changed to {}",
                    field.label(),
                    chosen.display()
                ));
                if let Err(e) = self.config.save(&self.config_path) {
                    logging::error(format!("saving config: {e}"));
                    self.fs_errors.push(e);
                }
                self.recompute_validation();
            }
            Target::ProjectRoot => {
                // Browsing to a folder both moves there and bookmarks it, so
                // getting back is one keypress next time.
                let moved = self.config.rebase(&chosen);
                let base = crate::config::project_name_for(&chosen);
                let name = self.config.unique_project_name(&base);
                self.config.remember_project(&name);
                self.after_location_change(&format!("Switched to {name}"), moved);
            }
        }
    }

    // ----------------------------------------------------- startup validation

    fn on_key_validate(&mut self, key: KeyEvent) {
        let Some(validate) = self.validate.as_ref() else {
            self.screen = Screen::Main;
            return;
        };
        let Some(&field) = validate.missing.get(validate.index) else {
            self.recompute_validation();
            return;
        };

        match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') => {
                if self.create_for(field).is_some() {
                    // recompute_validation resets the status line, so the
                    // outcome has to be put back after it.
                    let done = self.status.clone();
                    self.recompute_validation();
                    self.status = done;
                }
            }
            KeyCode::Char('a') | KeyCode::Char('A') => {
                let all: Vec<Field> = self
                    .validate
                    .as_ref()
                    .map(|v| v.missing.clone())
                    .unwrap_or_default();
                let mut created = 0usize;
                let mut ok = true;
                for field in all {
                    match self.create_for(field) {
                        Some(n) => created += n,
                        None => {
                            ok = false;
                            break;
                        }
                    }
                }
                // One message for the whole batch, rather than whichever
                // directory happened to be created last.
                let done = if ok {
                    format!("Created {created} folder(s)")
                } else {
                    self.status.clone()
                };
                self.recompute_validation();
                self.status = done;
            }
            KeyCode::Char('c') | KeyCode::Char('C') => {
                let start = field
                    .dir(&self.config)
                    .cloned()
                    .unwrap_or_else(crate::config::home_dir);
                self.picker = Some(Picker::new(&start, Target::Validation(field)));
                self.screen = Screen::Picker;
            }
            KeyCode::Char('q') | KeyCode::Char('Q') => self.should_quit = true,
            _ => {}
        }
    }

    /// Creates what the user just approved, reporting the outcome on the status
    /// line. Returns whether it worked.
    ///
    /// Approving the library root approves the library: the folders it is made
    /// of are created with it, which is why validation does not ask about them
    /// one at a time. The prompt lists them before this runs.
    fn create_for(&mut self, field: Field) -> Option<usize> {
        let paths = if field == Field::LibraryRoot {
            self.config.library_dirs()
        } else {
            vec![field.dir(&self.config)?.clone()]
        };

        for path in &paths {
            if let Err(e) = std::fs::create_dir_all(path) {
                let msg = format!("could not create {}: {e}", path.display());
                logging::error(&msg);
                self.fs_errors.push(msg.clone());
                self.status = msg;
                return None;
            }
            logging::info(format!("created {}", path.display()));
        }

        self.status = match paths.as_slice() {
            [one] => format!("Created {}", one.display()),
            many => format!(
                "Created {} and the {} folders inside it",
                self.config.library_root.display(),
                many.len() - 1
            ),
        };
        Some(paths.len())
    }

    /// Recomputes what is still missing; drops into the main screen when the
    /// configuration is fully satisfiable.
    fn recompute_validation(&mut self) {
        let missing = self.config.missing_dirs();
        if missing.is_empty() {
            self.validate = None;
            self.screen = Screen::Main;
            self.apply_config_change();
        } else {
            self.validate = Some(Validate { missing, index: 0 });
            self.screen = Screen::Validate;
        }
    }
}

/// Adds files found on disk to a part the manifest already knows, keeping the
/// manifest's provenance and never listing the same file twice.
fn absorb(known: &mut manifest::Part, found: manifest::Part) {
    if known.symbol_file.is_none() {
        known.symbol_file = found.symbol_file;
    }
    for name in found.symbol_names {
        if !known.symbol_names.contains(&name) {
            known.symbol_names.push(name);
        }
    }
    for path in found.footprints {
        if !known.footprints.contains(&path) {
            known.footprints.push(path);
        }
    }
    for path in found.models {
        if !known.models.contains(&path) {
            known.models.push(path);
        }
    }
}

/// Keeps a list selection valid after the list behind it changed length.
fn clamp(state: &mut ListState, len: usize) {
    if len == 0 {
        state.select(None);
    } else {
        state.select(Some(state.selected().unwrap_or(0).min(len - 1)));
    }
}

/// Drives the loop until the user quits.
pub fn run(
    terminal: &mut ratatui::DefaultTerminal,
    app: &mut App,
    rx: Receiver<Event>,
) -> std::io::Result<()> {
    while !app.should_quit {
        terminal.draw(|frame| crate::ui::draw(frame, app))?;
        match rx.recv() {
            Ok(event) => app.handle(event),
            Err(_) => break,
        }
    }
    Ok(())
}
