//! cse-importer — imports Component Search Engine ZIP archives into a KiCad
//! library tree.
//!
//! Every filesystem location is configurable, persisted in `config.toml`, and
//! overridable on the command line.

mod app;
mod config;
mod http;
mod importer;
mod library;
mod logging;
mod manifest;
mod online;
mod picker;
mod scan;
mod symlib;
mod ui;
mod watcher;

#[cfg(test)]
mod tests;

use clap::Parser;
use crossterm::event::{self, Event as CrossEvent};
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::Duration;

use crate::app::{App, Event};
use crate::config::Config;

#[derive(Parser, Debug)]
#[command(
    name = "cse-importer",
    about = "Import Component Search Engine ZIP archives into a KiCad library",
    version
)]
struct Cli {
    /// Directory scanned recursively for ZIP archives.
    #[arg(long, value_name = "DIR")]
    download_dir: Option<PathBuf>,

    /// Root of the KiCad library tree.
    #[arg(long, value_name = "DIR")]
    library_root: Option<PathBuf>,

    /// Configuration file to read and write.
    #[arg(long, value_name = "FILE")]
    config: Option<PathBuf>,

    /// Start in a saved project, moving the whole library tree with it.
    #[arg(long, value_name = "NAME")]
    project: Option<String>,

    /// List the saved projects and exit.
    #[arg(long)]
    list_projects: bool,

    /// Search the online catalogue and print the results instead of starting
    /// the interface.
    #[arg(long, value_name = "QUERY")]
    search: Option<String>,

    /// Download an LCSC part number straight into the library and exit.
    #[arg(long, value_name = "LCSC")]
    add: Option<String>,

    /// Import one ZIP archive and exit. Replaces existing files without asking;
    /// the interface asks first.
    #[arg(long, value_name = "FILE")]
    import: Option<PathBuf>,
}

fn main() -> std::io::Result<()> {
    let cli = Cli::parse();

    // Config location is itself configurable, and must be resolved before the
    // log file (which sits beside it).
    let cwd = std::env::current_dir().unwrap_or_else(|_| config::home_dir());
    let config_path = match &cli.config {
        Some(p) => config::resolve(p, &cwd),
        None => config::default_config_path(),
    };
    let log_path = config_path.parent().unwrap_or(&cwd).join("importer.log");
    if let Err(e) = logging::init(&log_path) {
        eprintln!(
            "warning: could not open log file {}: {e}",
            log_path.display()
        );
    }

    logging::info("=== cse-importer starting ===");
    logging::info(format!("version {}", env!("CARGO_PKG_VERSION")));
    logging::info(format!("working directory: {}", cwd.display()));
    logging::info(format!(
        "executable: {}",
        std::env::current_exe()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|e| format!("<unavailable: {e}>"))
    ));
    logging::info(format!("config file: {}", config_path.display()));

    let mut cfg = Config::load(&config_path);

    if cli.list_projects {
        return list_projects(&cfg);
    }

    // A project moves the whole tree, so it is applied before the individual
    // overrides below — those are meant to win.
    if let Some(name) = &cli.project {
        let Some(project) = cfg.find_project(name).cloned() else {
            let known: Vec<&str> = cfg.projects.iter().map(|p| p.name.as_str()).collect();
            eprintln!("no saved project called \"{name}\".");
            if known.is_empty() {
                eprintln!("no projects are saved yet; press [P] in the interface to save one.");
            } else {
                eprintln!("known projects: {}", known.join(", "));
            }
            std::process::exit(1);
        };
        let moved = cfg.switch_to(&project);
        logging::info(format!(
            "--project {} => {} ({} path(s) moved with the root)",
            project.name,
            cfg.library_root.display(),
            moved.len()
        ));
    }

    // Command-line arguments win over the file, and are not persisted unless
    // the user saves from the Settings screen.
    if let Some(dir) = &cli.download_dir {
        let resolved = config::resolve(dir, &cwd);
        logging::info(format!(
            "--download-dir overrides config: {}",
            resolved.display()
        ));
        cfg.download_dir = resolved;
    }
    if let Some(dir) = &cli.library_root {
        let resolved = config::resolve(dir, &cwd);
        // Everything that lived inside the old root follows it; leaving the
        // symbol and footprint folders behind would write this run's parts into
        // the previous library.
        let moved = cfg.rebase(&resolved);
        logging::info(format!(
            "--library-root overrides config: {} ({} path(s) moved with it)",
            resolved.display(),
            moved.len()
        ));
    }

    for field in config::Field::ALL {
        logging::info(format!(
            "config {} = {}",
            field.label(),
            field.display(&cfg)
        ));
    }

    // The two headless modes exist so the online catalogue can be driven from a
    // script, and so a failure is reported on stderr rather than inside a TUI.
    if let Some(query) = &cli.search {
        return run_search(query);
    }
    if let Some(part) = &cli.add {
        return run_add(&cfg, part);
    }
    if let Some(zip) = &cli.import {
        return run_import(&cfg, &config::resolve(zip, &cwd));
    }

    // Terminal input runs on its own thread and feeds the same queue as the
    // watcher and the import worker.
    let (tx, rx) = mpsc::channel::<Event>();
    spawn_input_thread(tx.clone());

    let mut application = App::new(cfg, config_path, tx);

    let mut terminal = ratatui::init();
    let result = app::run(&mut terminal, &mut application, rx);
    ratatui::restore();

    logging::info("=== cse-importer exiting ===");
    result
}

fn list_projects(cfg: &Config) -> std::io::Result<()> {
    if cfg.projects.is_empty() {
        println!("No saved projects. Press [P] in the interface to save one.");
        return Ok(());
    }
    for project in &cfg.projects {
        let active = if project.library_root == cfg.library_root {
            "*"
        } else {
            " "
        };
        println!(
            "{active} {:<24} {}",
            project.name,
            project.library_root.display()
        );
    }
    Ok(())
}

fn run_search(query: &str) -> std::io::Result<()> {
    match online::search(query, online::SEARCH_LIMIT) {
        Ok(hits) if hits.is_empty() => println!("No results for \"{query}\"."),
        Ok(hits) => {
            for hit in hits {
                println!(
                    "{:<12} {:<26} {:<20} {:<18} {} in stock",
                    hit.id, hit.mpn, hit.manufacturer, hit.package, hit.stock
                );
            }
        }
        Err(e) => {
            eprintln!("search failed: {e}");
            std::process::exit(1);
        }
    }
    Ok(())
}

fn run_import(cfg: &Config, zip: &std::path::Path) -> std::io::Result<()> {
    match importer::import(cfg, zip, |_, _| {}) {
        Ok(summary) => {
            println!(
                "Imported {}: {} symbol(s), {} footprint(s), {} model(s)",
                summary.part,
                summary.symbols.len(),
                summary.footprints.len(),
                summary.models.len()
            );
            Ok(())
        }
        Err(e) => {
            eprintln!("could not import {}: {e}", zip.display());
            std::process::exit(1);
        }
    }
}

fn run_add(cfg: &Config, part: &str) -> std::io::Result<()> {
    match online::add_to_library(cfg, part, |_, _| {}) {
        Ok(summary) => {
            println!(
                "Added {}: {} symbol(s), {} footprint(s), {} model(s)",
                summary.part,
                summary.symbols.len(),
                summary.footprints.len(),
                summary.models.len()
            );
            Ok(())
        }
        Err(e) => {
            eprintln!("could not add {part}: {e}");
            std::process::exit(1);
        }
    }
}

/// Reads key events, and emits a periodic tick so debounced rescans fire even
/// when the user is idle.
fn spawn_input_thread(tx: mpsc::Sender<Event>) {
    std::thread::spawn(move || loop {
        match event::poll(Duration::from_millis(200)) {
            Ok(true) => match event::read() {
                Ok(CrossEvent::Key(key)) => {
                    if tx.send(Event::Key(key)).is_err() {
                        break;
                    }
                }
                Ok(_) => {
                    // Resize and mouse events still warrant a redraw.
                    if tx.send(Event::Tick).is_err() {
                        break;
                    }
                }
                Err(e) => {
                    logging::error(format!("input error: {e}"));
                    break;
                }
            },
            Ok(false) => {
                if tx.send(Event::Tick).is_err() {
                    break;
                }
            }
            Err(e) => {
                logging::error(format!("input poll error: {e}"));
                break;
            }
        }
    });
}
