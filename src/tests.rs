//! Tests for the path handling and discovery logic — the parts that decide
//! whether ZIP files are found at all.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::Receiver;
use std::time::Duration;

use crate::app::{App, Focus, Screen};
use crate::config::{self, Config, Field};
use crate::importer;
use crate::manifest;
use crate::online::{self, easyeda};
use crate::scan;
use crate::symlib;

static COUNTER: AtomicU32 = AtomicU32::new(0);

/// Unique scratch directory per test; no external tempdir crate needed.
fn scratch(tag: &str) -> PathBuf {
    let id = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!(
        "cse-importer-test-{tag}-{}-{id}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn config_for(root: &Path) -> Config {
    let lib = root.join("lib");
    Config {
        download_dir: root.join("downloads"),
        symbol_dir: lib.join("symbols"),
        footprint_dir: lib.join("footprints.pretty"),
        model_dir: lib.join("3dmodels"),
        temp_dir: lib.join("cache"),
        archive_dir: lib.join("archive"),
        merged_symbol_lib: Some(lib.join("murky-informis.kicad_sym")),
        manifest_path: lib.join("library.json"),
        per_part_footprint_dirs: true,
        backup_before_overwrite: true,
        delete_zip_after_import: false,
        projects: Vec::new(),
        library_root: lib,
    }
}

fn touch(path: &Path, bytes: &[u8]) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, bytes).unwrap();
}

/// A single-symbol library in the shape SamacSys actually emits.
fn kicad_sym(part: &str) -> String {
    format!(
        r#"(kicad_symbol_lib (version 20211014) (generator SamacSys_ECAD_Model)
  (symbol "{part}" (in_bom yes) (on_board yes)
    (property "Reference" "IC" (at 0 0 0)
      (effects (font (size 1.27 1.27)) (justify left top))
    )
    (property "Value" "{part}" (at 0 -2.54 0)
      (effects (font (size 1.27 1.27)) (justify left top))
    )
    (symbol "{part}_0_0"
      (rectangle (start 0 0) (end 10 -10))
    )
  )
)
"#
    )
}

/// Builds a ZIP shaped like a real Component Search Engine export.
fn write_cse_zip(path: &Path, part: &str, footprint: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let file = std::fs::File::create(path).unwrap();
    let mut zip = zip::ZipWriter::new(file);
    let opts = zip::write::SimpleFileOptions::default();

    let mut add = |name: String, body: &str| {
        zip.start_file(name, opts).unwrap();
        zip.write_all(body.as_bytes()).unwrap();
    };

    add(format!("{part}/KiCad/{part}.kicad_sym"), &kicad_sym(part));
    add(format!("{part}/KiCad/{footprint}.kicad_mod"), "(footprint)");
    add(format!("{part}/KiCad/{part}.lib"), "legacy");
    add(format!("{part}/KiCad/{part}.dcm"), "legacy docs");
    add(format!("{part}/3D/{part}.stp"), "ISO-10303-21;");
    // Decoys from the other EDA tool trees that must not be imported.
    add(format!("{part}/Altium/{part}.SchLib"), "altium");
    add(format!("{part}/How_To_Use_Models.pdf"), "%PDF-1.4");
    add("license.txt".to_string(), "license");

    zip.finish().unwrap();
}

// ------------------------------------------------------------------- paths

#[test]
fn tilde_expands_to_home() {
    let home = config::home_dir();
    assert_eq!(config::expand_tilde(Path::new("~")), home);
    assert_eq!(
        config::expand_tilde(Path::new("~/Downloads/KiCad")),
        home.join("Downloads/KiCad")
    );
    // A bare `~foo` is a username reference, not this user's home.
    assert_eq!(
        config::expand_tilde(Path::new("~other/x")),
        PathBuf::from("~other/x")
    );
}

#[test]
fn relative_paths_resolve_against_base_not_cwd() {
    let base = PathBuf::from("/etc/cse-importer");
    assert_eq!(
        config::resolve(Path::new("lib"), &base),
        PathBuf::from("/etc/cse-importer/lib")
    );
    // Absolute input is left alone.
    assert_eq!(
        config::resolve(Path::new("/srv/lib"), &base),
        PathBuf::from("/srv/lib")
    );
}

#[test]
fn config_round_trips_through_toml_with_tilde_expanded() {
    let root = scratch("config");
    let config_path = root.join("config.toml");
    std::fs::write(
        &config_path,
        r#"
library_root = "~/KiCad/lib"
download_dir = "~/Downloads/KiCad"
symbol_dir = "~/KiCad/lib/symbols"
footprint_dir = "~/KiCad/lib/footprints"
model_dir = "~/KiCad/lib/3dmodels"
temp_dir = "~/KiCad/lib/cache"
archive_dir = "~/KiCad/lib/archive"
delete_zip_after_import = true
"#,
    )
    .unwrap();

    let cfg = Config::load(&config_path);
    let home = config::home_dir();
    assert_eq!(cfg.download_dir, home.join("Downloads/KiCad"));
    assert_eq!(cfg.symbol_dir, home.join("KiCad/lib/symbols"));
    assert!(cfg.delete_zip_after_import);

    // Saving and reloading must not drift.
    let second = root.join("second.toml");
    cfg.save(&second).unwrap();
    let reloaded = Config::load(&second);
    assert_eq!(reloaded.download_dir, cfg.download_dir);
    assert_eq!(reloaded.model_dir, cfg.model_dir);
}

#[test]
fn missing_dirs_only_covers_the_locations_the_user_chooses() {
    let root = scratch("missing");
    let cfg = config_for(&root);
    // Nothing exists yet, but only the library root and the ZIP folder are the
    // user's to answer for — the rest are inside the root.
    assert_eq!(
        cfg.missing_dirs(),
        vec![Field::LibraryRoot, Field::DownloadDir]
    );

    std::fs::create_dir_all(&cfg.download_dir).unwrap();
    assert_eq!(cfg.missing_dirs(), vec![Field::LibraryRoot]);

    std::fs::create_dir_all(&cfg.library_root).unwrap();
    assert!(
        cfg.missing_dirs().is_empty(),
        "an absent symbols folder is not a question for the user: {:?}",
        cfg.missing_dirs()
    );
}

// ---------------------------------------------------------------- discovery

#[test]
fn scan_finds_zips_in_nested_subdirectories() {
    let root = scratch("scan");
    let cfg = config_for(&root);
    std::fs::create_dir_all(&cfg.download_dir).unwrap();

    touch(&cfg.download_dir.join("STM32.zip"), b"x");
    touch(&cfg.download_dir.join("Memory/MT41K128.zip"), b"x");
    touch(&cfg.download_dir.join("USB/deep/nested/USB3343.zip"), b"x");
    touch(&cfg.download_dir.join("notes.txt"), b"x");

    let result = scan::scan(&cfg);
    assert!(
        result.errors.is_empty(),
        "unexpected errors: {:?}",
        result.errors
    );

    let found: Vec<&str> = result.entries.iter().map(|e| e.display.as_str()).collect();
    assert_eq!(found.len(), 3, "found: {found:?}");
    assert!(found.contains(&"STM32.zip"));
    assert!(found.contains(&"Memory/MT41K128.zip"));
    assert!(found.contains(&"USB/deep/nested/USB3343.zip"));
}

#[test]
fn scan_matches_uppercase_extension() {
    let root = scratch("case");
    let cfg = config_for(&root);
    std::fs::create_dir_all(&cfg.download_dir).unwrap();
    touch(&cfg.download_dir.join("PART.ZIP"), b"x");

    assert_eq!(scan::scan(&cfg).entries.len(), 1);
}

#[test]
fn scan_skips_archive_and_temp_subtrees() {
    let root = scratch("skip");
    let mut cfg = config_for(&root);
    // Put archive and temp *inside* the download dir, the worst case.
    cfg.archive_dir = cfg.download_dir.join("archive");
    cfg.temp_dir = cfg.download_dir.join("cache");
    std::fs::create_dir_all(&cfg.download_dir).unwrap();

    touch(&cfg.download_dir.join("live.zip"), b"x");
    touch(&cfg.archive_dir.join("already-imported.zip"), b"x");
    touch(&cfg.temp_dir.join("scratch.zip"), b"x");

    let entries = scan::scan(&cfg).entries;
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].display, "live.zip");
}

#[test]
fn scan_reports_missing_directory_instead_of_returning_empty_silently() {
    let root = scratch("absent");
    let cfg = config_for(&root); // download_dir was never created
    let result = scan::scan(&cfg);
    assert!(result.entries.is_empty());
    assert_eq!(result.errors.len(), 1);
    assert!(result.errors[0].contains("does not exist"));
}

// ------------------------------------------------------------------ import

#[test]
fn import_places_files_in_configured_directories() {
    let root = scratch("import");
    let cfg = config_for(&root);
    for field in Field::DIRS {
        std::fs::create_dir_all(field.dir(&cfg).unwrap()).unwrap();
    }

    let zip_path = cfg.download_dir.join("LIB_TPD12S016PWR.zip");
    write_cse_zip(&zip_path, "TPD12S016PWR", "SOP65P640X120-24N");

    let summary = importer::import(&cfg, &zip_path, |_, _| {}).unwrap();

    assert_eq!(summary.symbols, vec!["TPD12S016PWR.kicad_sym"]);
    assert_eq!(summary.footprints, vec!["SOP65P640X120-24N.kicad_mod"]);
    assert_eq!(summary.models, vec!["TPD12S016PWR.stp"]);

    assert!(cfg.symbol_dir.join("TPD12S016PWR.kicad_sym").is_file());
    assert!(cfg
        .footprint_dir
        .join("SOP65P640X120-24N.kicad_mod")
        .is_file());
    assert!(cfg.model_dir.join("TPD12S016PWR.stp").is_file());

    // The Altium/PDF/legacy decoys must not have been copied anywhere.
    assert!(!cfg.symbol_dir.join("TPD12S016PWR.lib").exists());
    assert!(!cfg.footprint_dir.join("TPD12S016PWR.SchLib").exists());

    // Source zip archived, staging cleaned up.
    assert!(!zip_path.exists());
    assert!(cfg.archive_dir.join("LIB_TPD12S016PWR.zip").is_file());
    assert!(!cfg.temp_dir.join("LIB_TPD12S016PWR").exists());
}

#[test]
fn import_deletes_source_zip_when_configured() {
    let root = scratch("delete");
    let mut cfg = config_for(&root);
    cfg.delete_zip_after_import = true;
    for field in Field::DIRS {
        std::fs::create_dir_all(field.dir(&cfg).unwrap()).unwrap();
    }

    let zip_path = cfg.download_dir.join("LIB_PART.zip");
    write_cse_zip(&zip_path, "PART", "QFN");

    let summary = importer::import(&cfg, &zip_path, |_, _| {}).unwrap();
    assert!(summary.deleted_zip);
    assert!(!zip_path.exists());
    assert!(std::fs::read_dir(&cfg.archive_dir)
        .unwrap()
        .next()
        .is_none());
}

#[test]
fn import_creates_target_dirs_and_reports_progress() {
    let root = scratch("progress");
    let cfg = config_for(&root);
    // Only the download dir exists; the importer must create the rest.
    std::fs::create_dir_all(&cfg.download_dir).unwrap();
    std::fs::create_dir_all(&cfg.temp_dir).unwrap();

    let zip_path = cfg.download_dir.join("LIB_PART.zip");
    write_cse_zip(&zip_path, "PART", "QFN");

    let seen = std::sync::Mutex::new(Vec::new());
    importer::import(&cfg, &zip_path, |fraction, _| {
        seen.lock().unwrap().push(fraction);
    })
    .unwrap();

    let seen = seen.into_inner().unwrap();
    assert!(seen.len() > 1);
    assert!(seen.iter().all(|f| (0.0..=1.0).contains(f)));
    assert_eq!(seen.last().copied(), Some(1.0));
    assert!(cfg.symbol_dir.join("PART.kicad_sym").is_file());
}

// ------------------------------------------------------- symbol library merge

#[test]
fn symlib_parses_header_and_symbols() {
    let lib = symlib::parse(&kicad_sym("TPD12S016PWR")).unwrap();
    assert!(lib
        .header
        .starts_with("(kicad_symbol_lib (version 20211014)"));
    // The nested unit symbol must not be mistaken for a top-level one.
    assert_eq!(lib.symbols.len(), 1);
    assert_eq!(lib.symbols[0].name, "TPD12S016PWR");
    assert!(lib.symbols[0].text.contains("TPD12S016PWR_0_0"));
}

#[test]
fn symlib_round_trips_through_render() {
    let original = kicad_sym("PART_A");
    let reparsed = symlib::parse(&symlib::render(&symlib::parse(&original).unwrap())).unwrap();
    assert_eq!(reparsed.symbols.len(), 1);
    assert_eq!(reparsed.symbols[0].name, "PART_A");
}

#[test]
fn symlib_merge_accumulates_parts_and_replaces_by_name() {
    let root = scratch("merge");
    let lib = root.join("murky-informis.kicad_sym");

    let added = symlib::merge_into(&lib, &kicad_sym("PART_A"), false).unwrap();
    assert_eq!(added, vec!["PART_A"]);
    symlib::merge_into(&lib, &kicad_sym("PART_B"), false).unwrap();

    let parsed = symlib::parse(&std::fs::read_to_string(&lib).unwrap()).unwrap();
    let names: Vec<&str> = parsed.symbols.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, vec!["PART_A", "PART_B"]);

    // Re-importing the same part replaces it rather than duplicating it.
    symlib::merge_into(&lib, &kicad_sym("PART_A"), false).unwrap();
    let parsed = symlib::parse(&std::fs::read_to_string(&lib).unwrap()).unwrap();
    assert_eq!(parsed.symbols.len(), 2);
}

#[test]
fn symlib_merge_writes_bak_and_rejects_garbage() {
    let root = scratch("bak");
    let lib = root.join("murky-informis.kicad_sym");
    symlib::merge_into(&lib, &kicad_sym("PART_A"), true).unwrap();
    // Nothing to back up on first write.
    assert!(!root.join("murky-informis.bak").exists());

    symlib::merge_into(&lib, &kicad_sym("PART_B"), true).unwrap();
    let backup = root.join("murky-informis.bak");
    assert!(backup.is_file());
    // The backup holds the previous revision, with only the first part.
    let previous = symlib::parse(&std::fs::read_to_string(&backup).unwrap()).unwrap();
    assert_eq!(previous.symbols.len(), 1);

    assert!(symlib::parse("not an s-expression").is_err());
}

// -------------------------------------------------------------- library tree

#[test]
fn import_produces_the_configured_library_tree() {
    let root = scratch("tree");
    let cfg = config_for(&root);
    for field in Field::DIRS {
        std::fs::create_dir_all(field.dir(&cfg).unwrap()).unwrap();
    }

    write_cse_zip(
        &cfg.download_dir.join("LIB_LFE5UM5G-85F-8MG285C.zip"),
        "LFE5UM5G-85F-8MG285C",
        "BGA285C50P18X18_1000X1000X130",
    );
    write_cse_zip(
        &cfg.download_dir.join("LIB_MT41K128M16JT-125_K.zip"),
        "MT41K128M16JT-125_K",
        "BGA96C80P9X16_800X1400X120",
    );

    for entry in scan::scan(&cfg).entries {
        importer::import(&cfg, &entry.path, |_, _| {}).unwrap();
    }

    // Symbols: one file per part.
    assert!(cfg
        .symbol_dir
        .join("LFE5UM5G-85F-8MG285C.kicad_sym")
        .is_file());
    assert!(cfg
        .symbol_dir
        .join("MT41K128M16JT-125_K.kicad_sym")
        .is_file());

    // Footprints: flat in the .pretty library *and* in a per-part .pretty.
    let fp = &cfg.footprint_dir;
    assert!(fp.join("BGA285C50P18X18_1000X1000X130.kicad_mod").is_file());
    assert!(fp.join("BGA96C80P9X16_800X1400X120.kicad_mod").is_file());
    assert!(fp
        .join("LFE5UM5G-85F-8MG285C.pretty/BGA285C50P18X18_1000X1000X130.kicad_mod")
        .is_file());
    assert!(fp
        .join("MT41K128M16JT-125_K.pretty/BGA96C80P9X16_800X1400X120.kicad_mod")
        .is_file());

    // 3D models keep the archive's own file names.
    assert!(cfg.model_dir.join("LFE5UM5G-85F-8MG285C.stp").is_file());
    assert!(cfg.model_dir.join("MT41K128M16JT-125_K.stp").is_file());

    // Both parts landed in the one merged library at the root.
    let merged = cfg.merged_symbol_lib.clone().unwrap();
    let parsed = symlib::parse(&std::fs::read_to_string(&merged).unwrap()).unwrap();
    let names: Vec<&str> = parsed.symbols.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, vec!["LFE5UM5G-85F-8MG285C", "MT41K128M16JT-125_K"]);
}

#[test]
fn per_part_footprint_dirs_can_be_turned_off() {
    let root = scratch("flatfp");
    let mut cfg = config_for(&root);
    cfg.per_part_footprint_dirs = false;
    for field in Field::DIRS {
        std::fs::create_dir_all(field.dir(&cfg).unwrap()).unwrap();
    }

    let zip = cfg.download_dir.join("LIB_PART.zip");
    write_cse_zip(&zip, "PART", "QFN_7050_6pins");
    importer::import(&cfg, &zip, |_, _| {}).unwrap();

    assert!(cfg.footprint_dir.join("QFN_7050_6pins.kicad_mod").is_file());
    assert!(!cfg.footprint_dir.join("PART.pretty").exists());
}

#[test]
fn merged_library_can_be_disabled() {
    let root = scratch("nomerge");
    let mut cfg = config_for(&root);
    cfg.merged_symbol_lib = None;
    for field in Field::DIRS {
        std::fs::create_dir_all(field.dir(&cfg).unwrap()).unwrap();
    }

    let zip = cfg.download_dir.join("LIB_PART.zip");
    write_cse_zip(&zip, "PART", "QFN");
    importer::import(&cfg, &zip, |_, _| {}).unwrap();

    assert!(cfg.symbol_dir.join("PART.kicad_sym").is_file());
    assert!(!root.join("lib/murky-informis.kicad_sym").exists());

    // Symbol names still reach the manifest without merging.
    let m = manifest::Manifest::load(&cfg.manifest_path);
    assert_eq!(m.parts[0].symbol_names, vec!["PART"]);
}

// ----------------------------------------------------------------- manifest

#[test]
fn manifest_records_layout_and_parts_relative_to_root() {
    let root = scratch("manifest");
    let cfg = config_for(&root);
    for field in Field::DIRS {
        std::fs::create_dir_all(field.dir(&cfg).unwrap()).unwrap();
    }

    let zip = cfg.download_dir.join("LIB_TPD12S016PWR.zip");
    write_cse_zip(&zip, "TPD12S016PWR", "SOP65P640X120-24N");
    importer::import(&cfg, &zip, |_, _| {}).unwrap();

    assert!(cfg.manifest_path.is_file());
    let m = manifest::Manifest::load(&cfg.manifest_path);

    assert_eq!(m.library_root, cfg.library_root.display().to_string());
    assert_eq!(m.layout.symbols, "symbols");
    assert_eq!(m.layout.footprints, "footprints.pretty");
    assert_eq!(m.layout.models, "3dmodels");
    assert_eq!(
        m.layout.merged_symbol_lib.as_deref(),
        Some("murky-informis.kicad_sym")
    );
    assert!(m.layout.per_part_footprint_dirs);

    assert_eq!(m.parts.len(), 1);
    let part = &m.parts[0];
    assert_eq!(part.name, "TPD12S016PWR");
    assert_eq!(part.source, "LIB_TPD12S016PWR.zip");
    assert_eq!(
        part.symbol_file.as_deref(),
        Some("symbols/TPD12S016PWR.kicad_sym")
    );
    assert_eq!(part.symbol_names, vec!["TPD12S016PWR"]);
    assert_eq!(part.models, vec!["3dmodels/TPD12S016PWR.stp"]);
    // Both footprint destinations are recorded, per-part first.
    assert!(part.footprints.contains(
        &"footprints.pretty/TPD12S016PWR.pretty/SOP65P640X120-24N.kicad_mod".to_string()
    ));
    assert!(part
        .footprints
        .contains(&"footprints.pretty/SOP65P640X120-24N.kicad_mod".to_string()));
    assert!(!part.imported_at.is_empty());
}

#[test]
fn manifest_replaces_a_part_on_reimport_rather_than_duplicating() {
    let root = scratch("reimport");
    let cfg = config_for(&root);
    for field in Field::DIRS {
        std::fs::create_dir_all(field.dir(&cfg).unwrap()).unwrap();
    }

    for _ in 0..2 {
        let zip = cfg.download_dir.join("LIB_PART.zip");
        write_cse_zip(&zip, "PART", "QFN");
        importer::import(&cfg, &zip, |_, _| {}).unwrap();
    }

    let m = manifest::Manifest::load(&cfg.manifest_path);
    assert_eq!(m.parts.len(), 1, "part duplicated in manifest");

    // The second import backed up the symbol file it replaced.
    assert!(cfg.symbol_dir.join("PART.bak").is_file());
}

#[test]
fn manifest_survives_a_corrupt_file() {
    let root = scratch("corrupt");
    let cfg = config_for(&root);
    for field in Field::DIRS {
        std::fs::create_dir_all(field.dir(&cfg).unwrap()).unwrap();
    }
    std::fs::write(&cfg.manifest_path, "{ not json").unwrap();

    let zip = cfg.download_dir.join("LIB_PART.zip");
    write_cse_zip(&zip, "PART", "QFN");
    importer::import(&cfg, &zip, |_, _| {}).unwrap();

    let m = manifest::Manifest::load(&cfg.manifest_path);
    assert_eq!(m.parts.len(), 1);
}

// ----------------------------------------------------------- main screen

/// A library with every configured directory present, so `App::new` lands on
/// the main screen instead of startup validation.
fn ready_config(root: &Path) -> Config {
    let cfg = config_for(root);
    for field in Field::DIRS {
        if let Some(dir) = field.dir(&cfg) {
            std::fs::create_dir_all(dir).unwrap();
        }
    }
    cfg
}

fn app_for(cfg: &Config, root: &Path) -> App {
    let (tx, _rx) = std::sync::mpsc::channel();
    App::new(cfg.clone(), root.join("config.toml"), tx)
}

/// An app whose worker events can still be received, for driving a real import
/// to completion inside a test.
fn app_and_events(cfg: &Config, root: &Path) -> (App, Receiver<crate::app::Event>) {
    let (tx, rx) = std::sync::mpsc::channel();
    (App::new(cfg.clone(), root.join("config.toml"), tx), rx)
}

/// Runs the event loop until the import settles — finished, or stopped on a
/// question for the user.
fn pump(app: &mut App, rx: &Receiver<crate::app::Event>) {
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    while std::time::Instant::now() < deadline {
        if app.pending.is_some() || app.run.is_none() {
            return;
        }
        match rx.recv_timeout(Duration::from_millis(250)) {
            Ok(event) => app.handle(event),
            Err(_) => panic!("no progress from the import worker"),
        }
    }
    panic!("import did not settle");
}

#[test]
fn manifest_totals_count_each_file_once() {
    let root = scratch("totals");
    let cfg = config_for(&root);

    // A footprint is deliberately written twice — flat, and in the part's own
    // .pretty folder — but it is still one footprint.
    manifest::record(
        &cfg,
        manifest::Part {
            name: "NE555P".to_string(),
            source: "LCSC C46749".to_string(),
            imported_at: "2026-07-26T00:00:00+00:00".to_string(),
            symbol_file: Some("symbols/NE555P.kicad_sym".to_string()),
            symbol_names: vec!["NE555P".to_string()],
            footprints: vec![
                "footprints.pretty/NE555P.pretty/DIP-8.kicad_mod".to_string(),
                "footprints.pretty/DIP-8.kicad_mod".to_string(),
            ],
            models: vec!["3dmodels/NE555P.stp".to_string()],
        },
    )
    .unwrap();

    let totals = manifest::totals(&manifest::Manifest::load(&cfg.manifest_path).parts);
    assert_eq!(totals.parts, 1);
    assert_eq!(totals.symbols, 1);
    assert_eq!(
        totals.footprints, 1,
        "the same file in two places is one file"
    );
    assert_eq!(totals.models, 1);
}

#[test]
fn the_main_screen_shows_the_queue_and_the_library_together() {
    let root = scratch("panes");
    let cfg = ready_config(&root);
    touch(&cfg.download_dir.join("LIB_TPD12S016PWR.zip"), b"x");
    touch(&cfg.download_dir.join("Memory/LIB_MT41K128M16.zip"), b"x");
    manifest::record(
        &cfg,
        manifest::Part {
            name: "NE555P".to_string(),
            source: "LCSC C46749".to_string(),
            ..Default::default()
        },
    )
    .unwrap();

    let app = app_for(&cfg, &root);
    assert_eq!(app.screen, Screen::Main);
    assert_eq!(app.queue_view.len(), 2);
    assert_eq!(app.library_view.len(), 1);
    assert_eq!(app.totals.parts, 1);
    // Both panes start with something selected, so Details is never blank.
    assert!(app.selected_zip().is_some());
    assert!(app.selected_part().is_some());
}

#[test]
fn the_filter_narrows_both_panes_and_keeps_selections_valid() {
    let root = scratch("filter");
    let cfg = ready_config(&root);
    touch(&cfg.download_dir.join("LIB_TPD12S016PWR.zip"), b"x");
    touch(&cfg.download_dir.join("Memory/LIB_MT41K128M16.zip"), b"x");
    for name in ["NE555P", "MT41K128M16"] {
        manifest::record(
            &cfg,
            manifest::Part {
                name: name.to_string(),
                source: format!("LIB_{name}.zip"),
                ..Default::default()
            },
        )
        .unwrap();
    }

    let mut app = app_for(&cfg, &root);
    // Point at the last row, then filter it away: the selection must follow.
    app.library_list.select(Some(1));
    app.filter = "tpd".to_string();
    app.recompute_views();

    assert_eq!(app.queue_view.len(), 1);
    assert_eq!(app.selected_zip().unwrap().display, "LIB_TPD12S016PWR.zip");
    assert!(app.library_view.is_empty());
    assert!(app.selected_part().is_none(), "no row means no selection");

    // Matching is case-insensitive and covers the recorded source.
    app.filter = "MT41K".to_string();
    app.recompute_views();
    assert_eq!(app.queue_view.len(), 1);
    assert_eq!(app.library_view.len(), 1);
    assert_eq!(app.selected_part().unwrap().name, "MT41K128M16");

    app.filter.clear();
    app.recompute_views();
    assert_eq!(app.queue_view.len(), 2);
    assert_eq!(app.library_view.len(), 2);
}

#[test]
fn search_results_already_in_the_library_are_recognised() {
    let root = scratch("owned");
    let cfg = ready_config(&root);
    manifest::record(
        &cfg,
        manifest::Part {
            name: "NE555P".to_string(),
            source: "LCSC C46749".to_string(),
            ..Default::default()
        },
    )
    .unwrap();

    let app = app_for(&cfg, &root);
    let owned = online::Hit {
        id: "C46749".to_string(),
        mpn: "NE555P".to_string(),
        ..Default::default()
    };
    // A different LCSC code for the same manufacturer part still counts, and an
    // unrelated part does not.
    let same_part = online::Hit {
        id: "C99999".to_string(),
        mpn: "NE555P".to_string(),
        ..Default::default()
    };
    let other = online::Hit {
        id: "C7420".to_string(),
        mpn: "NC7SZ125M5X".to_string(),
        ..Default::default()
    };

    assert!(app.library_has(&owned));
    assert!(app.library_has(&same_part));
    assert!(!app.library_has(&other));
}

#[test]
fn an_empty_queue_starts_focus_on_the_library() {
    let root = scratch("focus");
    let cfg = ready_config(&root);
    manifest::record(
        &cfg,
        manifest::Part {
            name: "NE555P".to_string(),
            source: "LCSC C46749".to_string(),
            ..Default::default()
        },
    )
    .unwrap();

    assert_eq!(app_for(&cfg, &root).focus, Focus::Library);

    // With archives waiting, importing them is the more likely next action.
    touch(&cfg.download_dir.join("LIB_X.zip"), b"x");
    assert_eq!(app_for(&cfg, &root).focus, Focus::Queue);
}

// ------------------------------------------------------- online conversion

/// A component in the shape the EasyEDA API returns, without the network.
fn stub_component() -> easyeda::Component {
    easyeda::Component {
        lcsc: "C7420".to_string(),
        title: "NC7SZ125M5X".to_string(),
        manufacturer: "onsemi".to_string(),
        datasheet: "https://lcsc.com/product-detail/C7420.html".to_string(),
        prefix: "U?".to_string(),
        symbol_origin: (400.0, 300.0),
        symbol_shapes: vec![
            // An input pin on the left, pointing right towards the body.
            "P~show~1~1~380~310~180~gge11~0^^380~310^^M 380 310 h 20~#880000\
             ^^1~402~313~0~OE~start~~~#0000FF^^1~395~309~0~1~end~~~#0000FF^^0~397~310^^0~"
                .to_string(),
            // A power pin on the right, pointing left, drawn with a bubble.
            "P~show~4~2~470~300~0~gge18~0^^470~300^^M 470 300 h -20~#880000\
             ^^1~448~303~0~VCC~end~~~#0000FF^^1~455~299~0~2~start~~~#0000FF^^1~467~300^^0~"
                .to_string(),
            "R~400~300~~~50~40~#000000~1~0~none~gge60~0~".to_string(),
        ],
        package_name: "SOT-23-5".to_string(),
        package_origin: (100.0, 100.0),
        package_shapes: vec![
            "PAD~RECT~110~90~4~2~1~~1~0~~0~gge1~0~~Y".to_string(),
            "PAD~ELLIPSE~90~110~6~6~11~~2~1.5~~0~gge2~0~~Y".to_string(),
            "TRACK~1~3~~100 100 110 100~gge3~0".to_string(),
            "SOLIDREGION~99~~M 100 100 L 110 100 L 110 110 Z~solid~gge4~~~~0".to_string(),
            "HOLE~100~100~2~gge5~0".to_string(),
            r#"SVGNODE~{"gId":"g1_outline","attrs":{"c_origin":"100,100","z":"-13.7795","c_rotation":"0,0,180","uuid":"abc123","title":"SOT-23-5"}}"#.to_string(),
        ],
    }
}

#[test]
fn svg_paths_flatten_to_lines_and_close_subpaths() {
    use crate::online::svgpath::{parse_path, Seg};

    let segs = parse_path("M 10 10 L 20 10 v 10 Z");
    assert_eq!(segs.len(), 3, "{segs:?}");
    let Seg::Line { from, to } = segs[0] else {
        panic!("expected a line, got {:?}", segs[0]);
    };
    assert_eq!((from.x, from.y), (10.0, 10.0));
    assert_eq!((to.x, to.y), (20.0, 10.0));
    // `Z` closes back to the start of the subpath.
    let Seg::Line { to, .. } = segs[2] else {
        panic!("expected a closing line");
    };
    assert_eq!((to.x, to.y), (10.0, 10.0));
}

#[test]
fn a_full_circle_arc_becomes_two_kicad_arcs() {
    use crate::online::svgpath::{parse_path, Seg};

    // EasyEDA draws pin-1 dots as one arc whose end almost meets its start.
    let segs = parse_path("M 0 1 A 1 1 0 1 1 0.01 1");
    assert_eq!(segs.len(), 2, "a >180 degree sweep must be split: {segs:?}");
    for seg in &segs {
        let Seg::Arc { from, mid, to } = seg else {
            panic!("expected arcs, got {seg:?}");
        };
        // Every point stays on the unit circle centred at the origin.
        for p in [from, mid, to] {
            let radius = (p.x * p.x + p.y * p.y).sqrt();
            assert!((radius - 1.0).abs() < 0.02, "{p:?} is not on the circle");
        }
        // A midpoint that coincides with an endpoint is the collapse this
        // split exists to prevent.
        assert!((mid.x - from.x).abs() + (mid.y - from.y).abs() > 0.1);
    }
}

#[test]
fn short_easyeda_records_do_not_panic() {
    let record = easyeda::Record::new("PAD~RECT~110");
    assert_eq!(record.kind(), "PAD");
    assert_eq!(record.num(2), 110.0);
    // Fields past the end read as empty/zero rather than failing the import.
    assert_eq!(record.str(9), "");
    assert_eq!(record.num(9), 0.0);
    assert_eq!(record.opt_num(9), None);
}

#[test]
fn names_are_made_safe_for_files_and_kicad() {
    assert_eq!(online::sanitize("NE555P"), "NE555P");
    assert_eq!(online::sanitize("TI(德州仪器) / 555"), "TI_555");
    assert_eq!(online::sanitize("  2.54-1*5  "), "2.54-1_5");
    assert_eq!(online::escape(r#"a"b\c"#), r#"a\"b\\c"#);
}

#[test]
fn symbol_conversion_produces_a_loadable_library() {
    let component = stub_component();
    let text = online::symbol::convert(&component, Some("footprints:SOT-23-5"));

    // It must survive the same parser the merged library uses.
    let lib = symlib::parse(&text).expect("generated symbol should parse");
    assert_eq!(lib.symbols.len(), 1, "one top-level symbol");
    assert_eq!(lib.symbols[0].name, "NC7SZ125M5X");

    assert!(text.contains(r#"(property "Footprint" "footprints:SOT-23-5""#));
    assert!(text.contains(r#"(property "LCSC" "C7420""#));

    // Origin (400,300), pin dot at (380,310): 20 units left and 10 below, and
    // the y axis flips. The path runs +20 units in x, so the body is to the
    // right, which is KiCad's angle 0.
    assert!(
        text.contains(r#"(pin input line (at -5.08 -2.54 0) (length 5.08)"#),
        "{text}"
    );
    assert!(text.contains(r#"(name "OE""#));
    // The right-hand pin points back at the body, and its bubble makes it
    // an inverted pin.
    assert!(
        text.contains(r#"(pin power_in inverted (at 17.78 0 180) (length 5.08)"#),
        "{text}"
    );
    // Body rectangle: (400,300) to (450,340) with y flipped.
    assert!(
        text.contains("(rectangle (start 0 0) (end 12.7 -10.16))")
            || text.contains("(rectangle (start 0 0) (end 12.7 -10.16)"),
        "{text}"
    );
}

#[test]
fn footprint_conversion_maps_pads_layers_and_holes() {
    let component = stub_component();
    let text = online::footprint::convert(&component, "SOT-23-5", None);

    // Surface pad: 10 units right and 10 up from the origin, 4x2 units.
    assert!(
        text.contains(r#"(pad "1" smd rect (at 2.54 2.54) (size 1.016 0.508) (layers "F.Cu" "F.Paste" "F.Mask"))"#),
        "{text}"
    );
    // Through-hole pad: a 1.5-unit hole radius becomes a 3-unit drill.
    assert!(
        text.contains(r#"(pad "2" thru_hole circle (at -2.54 -2.54) (size 1.524 1.524) (drill 0.762) (layers "*.Cu" "*.Mask"))"#),
        "{text}"
    );
    assert!(
        text.contains(r#"(fp_line (start 0 0) (end 2.54 0)"#),
        "{text}"
    );
    assert!(text.contains(r#"(layer "F.SilkS")"#));
    // EasyEDA layer 99 is a component outline, which is KiCad's fab layer.
    assert!(
        text.contains(r#"(fp_poly"#) && text.contains(r#"(layer "F.Fab")"#),
        "{text}"
    );
    assert!(text.contains(r#"np_thru_hole circle (at 0 0)"#), "{text}");
    // Mixed pad kinds must not claim to be a pure SMD footprint.
    assert!(!text.contains("(attr smd)"), "{text}");
}

#[test]
fn the_3d_model_reference_ignores_the_easyeda_z_field() {
    let component = stub_component();
    let model = online::footprint::model_info(&component).expect("stub has an SVGNODE");
    assert_eq!(model.uuid, "abc123");
    // z is descriptive, not an offset — see the note in footprint::model_info.
    assert_eq!(model.translation, (0.0, 0.0, 0.0));
    assert_eq!(model.rotation, (0.0, 0.0, 180.0));

    let text = online::footprint::convert(&component, "SOT-23-5", Some("/lib/3dmodels/X.stp"));
    assert!(text.contains(r#"(model "/lib/3dmodels/X.stp""#), "{text}");
    assert!(text.contains("(offset (xyz 0 0 0))"), "{text}");
    assert!(text.contains("(rotate (xyz 0 0 180))"), "{text}");
}

#[test]
fn a_downloaded_part_lands_in_the_same_tree_as_a_zip_import() {
    let root = scratch("install");
    let cfg = config_for(&root);
    let component = stub_component();

    // Exactly what online::add_to_library stages, without the network.
    let staging = importer::prepare_staging(&cfg, "web-C7420").unwrap();
    let staged = vec![
        importer::stage(
            &staging,
            importer::Kind::Symbol,
            "NC7SZ125M5X.kicad_sym",
            online::symbol::convert(&component, Some("footprints.pretty:SOT-23-5")).as_bytes(),
        )
        .unwrap(),
        importer::stage(
            &staging,
            importer::Kind::Footprint,
            "SOT-23-5.kicad_mod",
            online::footprint::convert(&component, "SOT-23-5", None).as_bytes(),
        )
        .unwrap(),
        importer::stage(
            &staging,
            importer::Kind::Model,
            "NC7SZ125M5X.stp",
            b"ISO-10303-21;",
        )
        .unwrap(),
    ];

    let summary = importer::install(&cfg, "NC7SZ125M5X", "LCSC C7420", staged).unwrap();
    assert_eq!(summary.symbols.len(), 1);
    assert_eq!(summary.merged_symbols, vec!["NC7SZ125M5X".to_string()]);

    assert!(cfg.symbol_dir.join("NC7SZ125M5X.kicad_sym").is_file());
    assert!(cfg.footprint_dir.join("SOT-23-5.kicad_mod").is_file());
    assert!(cfg
        .footprint_dir
        .join("NC7SZ125M5X.pretty/SOT-23-5.kicad_mod")
        .is_file());
    assert!(cfg.model_dir.join("NC7SZ125M5X.stp").is_file());

    // The merged library gained the part, and the manifest records where it
    // came from.
    let merged = std::fs::read_to_string(cfg.merged_symbol_lib.as_ref().unwrap()).unwrap();
    assert!(merged.contains(r#"(symbol "NC7SZ125M5X""#));

    let m = manifest::Manifest::load(&cfg.manifest_path);
    let part = m.parts.iter().find(|p| p.name == "NC7SZ125M5X").unwrap();
    assert_eq!(part.source, "LCSC C7420");
    assert_eq!(
        part.symbol_file.as_deref(),
        Some("symbols/NC7SZ125M5X.kicad_sym")
    );
    assert_eq!(part.models, vec!["3dmodels/NC7SZ125M5X.stp".to_string()]);
    assert!(part
        .footprints
        .contains(&"footprints.pretty/SOT-23-5.kicad_mod".to_string()));
}

/// Hits the live LCSC/EasyEDA catalogue. Ignored by default; run with:
/// `cargo test -- --ignored --nocapture`
#[test]
#[ignore]
fn online_search_and_add_a_real_part() {
    let root = scratch("online-live");
    let cfg = config_for(&root);

    let hits = online::search("NE555P", 10).expect("search should succeed");
    assert!(!hits.is_empty(), "expected results for NE555P");
    assert!(hits.iter().all(|h| h.id.starts_with('C')));

    let summary = online::add_to_library(&cfg, "C46749", |_, _| {}).unwrap();
    assert_eq!(summary.part, "NE555P");
    assert_eq!(summary.symbols.len(), 1);
    assert_eq!(summary.footprints.len(), 1);
    assert_eq!(summary.models.len(), 1);

    let symbol = std::fs::read_to_string(cfg.symbol_dir.join("NE555P.kicad_sym")).unwrap();
    assert_eq!(symlib::parse(&symbol).unwrap().symbols.len(), 1);
    // Eight pins on a 555.
    assert_eq!(symbol.matches("(pin ").count(), 8);

    let model = std::fs::read(cfg.model_dir.join("NE555P.stp")).unwrap();
    assert!(model.starts_with(b"ISO-10303-21"));
}

/// End-to-end check against real Component Search Engine archives.
///
/// Ignored by default because it needs fixtures. Run with:
/// `CSE_FIXTURE_DIR=~/Downloads/KiCad cargo test -- --ignored --nocapture`
/// The fixtures are copied into a scratch directory, never imported in place.
#[test]
#[ignore]
fn import_real_cse_archives() {
    let Ok(fixture_dir) = std::env::var("CSE_FIXTURE_DIR") else {
        panic!("set CSE_FIXTURE_DIR to a folder containing real CSE zips");
    };
    let fixture_dir = config::expand_tilde(Path::new(&fixture_dir));

    let root = scratch("real");
    let cfg = config_for(&root);
    for field in Field::DIRS {
        std::fs::create_dir_all(field.dir(&cfg).unwrap()).unwrap();
    }

    // Copy fixtures in; the originals are read-only as far as this test cares.
    let mut copied = Vec::new();
    for entry in std::fs::read_dir(&fixture_dir).unwrap().flatten() {
        let path = entry.path();
        if path.extension().map(|e| e.eq_ignore_ascii_case("zip")) != Some(true) {
            continue;
        }
        let dest = cfg.download_dir.join(path.file_name().unwrap());
        std::fs::copy(&path, &dest).unwrap();
        copied.push(dest);
    }
    assert!(!copied.is_empty(), "no zips in {}", fixture_dir.display());

    let discovered = scan::scan(&cfg).entries;
    assert_eq!(discovered.len(), copied.len(), "scan missed files");
    println!("discovered {} archive(s)", discovered.len());

    for entry in &discovered {
        let summary = importer::import(&cfg, &entry.path, |_, _| {})
            .unwrap_or_else(|e| panic!("importing {}: {e}", entry.display));
        println!(
            "{}: {} symbol(s), {} footprint(s), {} model(s)",
            entry.display,
            summary.symbols.len(),
            summary.footprints.len(),
            summary.models.len()
        );
        assert_eq!(summary.symbols.len(), 1, "{} symbols", entry.display);
        assert!(
            !summary.footprints.is_empty(),
            "{} footprints",
            entry.display
        );
        assert!(!summary.models.is_empty(), "{} models", entry.display);
    }

    // Everything consumed, nothing left queued, archive holds the originals.
    assert!(scan::scan(&cfg).entries.is_empty());
    assert_eq!(
        std::fs::read_dir(&cfg.archive_dir).unwrap().count(),
        copied.len()
    );
    for dir in [&cfg.symbol_dir, &cfg.footprint_dir, &cfg.model_dir] {
        let count = std::fs::read_dir(dir).unwrap().count();
        println!("{} -> {count} file(s)", dir.display());
        assert_eq!(count, copied.len());
    }
}

#[test]
fn import_fails_loudly_when_archive_has_no_kicad_files() {
    let root = scratch("empty");
    let cfg = config_for(&root);
    for field in Field::DIRS {
        std::fs::create_dir_all(field.dir(&cfg).unwrap()).unwrap();
    }

    let zip_path = cfg.download_dir.join("LIB_NOTHING.zip");
    let file = std::fs::File::create(&zip_path).unwrap();
    let mut zip = zip::ZipWriter::new(file);
    zip.start_file("readme.txt", zip::write::SimpleFileOptions::default())
        .unwrap();
    zip.write_all(b"nothing here").unwrap();
    zip.finish().unwrap();

    let err = importer::import(&cfg, &zip_path, |_, _| {}).unwrap_err();
    assert!(err.contains("no KiCad files"), "got: {err}");
    // The source must be left untouched so the user can inspect it.
    assert!(zip_path.exists());
}

#[test]
fn the_key_bar_drops_hints_to_fit_but_never_the_last_one() {
    let keys = [
        ("Enter", "Import"),
        ("A", "All"),
        ("W", "Search"),
        ("Tab", "Pane"),
        ("/", "Filter"),
        ("R", "Rescan"),
        ("S", "Settings"),
        ("F12", "Diag"),
        ("Q", "Quit"),
    ];

    let full = crate::ui::hints(&keys, 200);
    assert!(full.width() <= 200);
    assert!(full.width() > 80, "a wide terminal shows every hint");

    for width in [80, 40, 20, 8] {
        let line = crate::ui::hints(&keys, width);
        assert!(
            line.width() <= width as usize,
            "{width} columns produced {} columns",
            line.width()
        );
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(
            text.contains("[Q] Quit"),
            "the way out survives at {width}: {text}"
        );
    }

    // Hints are dropped from the end inwards, so the leading ones stay as long
    // as there is room for them at all.
    let text: String = crate::ui::hints(&keys, 40)
        .spans
        .iter()
        .map(|s| s.content.as_ref())
        .collect();
    assert_eq!(text, "[Enter] Import  [A] All  [Q] Quit");
}

// ------------------------------------------------------------------ projects

fn press(app: &mut App, code: KeyCode) {
    app.handle(crate::app::Event::Key(KeyEvent::new(
        code,
        KeyModifiers::NONE,
    )));
}

fn type_text(app: &mut App, text: &str) {
    for c in text.chars() {
        press(app, KeyCode::Char(c));
    }
}

#[test]
fn a_saved_project_switches_the_whole_tree_in_one_keypress() {
    let root = scratch("project-switch");
    let mut cfg = ready_config(&root);

    // A second library, laid out the same way, one directory over.
    let other = root.join("other-project");
    let mut other_cfg = cfg.clone();
    other_cfg.rebase(&other);
    for field in Field::DIRS {
        std::fs::create_dir_all(field.dir(&other_cfg).unwrap()).unwrap();
    }

    cfg.projects = vec![
        config::Project {
            name: "here".to_string(),
            library_root: cfg.library_root.clone(),
            download_dir: None,
        },
        config::Project {
            name: "there".to_string(),
            library_root: other.clone(),
            download_dir: None,
        },
    ];

    let mut app = app_for(&cfg, &root);
    assert_eq!(
        app.config.active_project().map(|p| p.name.as_str()),
        Some("here")
    );

    press(&mut app, KeyCode::Char('p'));
    assert_eq!(app.screen, Screen::Projects);
    press(&mut app, KeyCode::Down);
    press(&mut app, KeyCode::Enter);

    assert_eq!(
        app.screen,
        Screen::Main,
        "the other tree exists, so nothing needs confirming"
    );
    assert_eq!(app.config.library_root, other);
    assert_eq!(app.config.symbol_dir, other.join("symbols"));
    assert_eq!(app.config.footprint_dir, other.join("footprints.pretty"));
    assert_eq!(app.config.model_dir, other.join("3dmodels"));
    assert_eq!(app.config.archive_dir, other.join("archive"));
    assert_eq!(app.config.manifest_path, other.join("library.json"));
    assert_eq!(
        app.config.merged_symbol_lib,
        Some(other.join("murky-informis.kicad_sym"))
    );
    // The ZIP folder was parked outside the library on purpose, so it stayed.
    assert_eq!(app.config.download_dir, root.join("downloads"));

    // Persisted, so the next run opens where this one left off.
    let saved = Config::load(&root.join("config.toml"));
    assert_eq!(saved.library_root, other);
    assert_eq!(saved.symbol_dir, other.join("symbols"));
    assert_eq!(saved.projects.len(), 2);
}

#[test]
fn switching_to_a_new_project_asks_before_creating_its_tree() {
    let root = scratch("new-project");
    let mut cfg = ready_config(&root);
    let fresh = root.join("fresh");
    cfg.projects = vec![config::Project {
        name: "fresh".to_string(),
        library_root: fresh.clone(),
        download_dir: None,
    }];

    let mut app = app_for(&cfg, &root);
    press(&mut app, KeyCode::Char('p'));
    press(&mut app, KeyCode::Enter);

    assert_eq!(app.screen, Screen::Validate);
    assert!(
        !fresh.exists(),
        "nothing is created before the user answers"
    );
    // One question, not seven: only the root is the user's to decide.
    assert_eq!(
        app.validate.as_ref().map(|v| v.missing.clone()),
        Some(vec![Field::LibraryRoot])
    );

    // Approving the root creates the folders that make up the library.
    press(&mut app, KeyCode::Char('y'));
    assert_eq!(app.screen, Screen::Main);
    assert_eq!(app.config.symbol_dir, fresh.join("symbols"));
    for field in Field::DIRS {
        let dir = field.dir(&app.config).unwrap();
        assert!(dir.is_dir(), "{} was not created", dir.display());
    }
}

#[test]
fn naming_a_location_saves_it_and_marks_it_active() {
    let root = scratch("project-name");
    let cfg = ready_config(&root);
    let mut app = app_for(&cfg, &root);
    assert!(app.config.active_project().is_none());

    press(&mut app, KeyCode::Char('p'));
    press(&mut app, KeyCode::Char('n'));
    // The suggested name is the folder's, which the user can replace outright.
    let suggested = app
        .projects
        .as_ref()
        .and_then(|p| p.naming.as_ref())
        .map(|n| n.buffer.clone())
        .unwrap();
    assert_eq!(
        suggested,
        config::project_name_for(&app.config.library_root)
    );
    for _ in 0..suggested.chars().count() {
        press(&mut app, KeyCode::Backspace);
    }
    type_text(&mut app, "Synth VCO");
    press(&mut app, KeyCode::Enter);

    assert_eq!(
        app.config.active_project().map(|p| p.name.as_str()),
        Some("Synth VCO")
    );
    assert_eq!(
        Config::load(&root.join("config.toml")).projects.len(),
        1,
        "a saved project outlives the session"
    );
}

#[test]
fn editing_the_library_root_in_settings_moves_the_paths_under_it() {
    let root = scratch("settings-rebase");
    let cfg = ready_config(&root);
    let mut app = app_for(&cfg, &root);

    press(&mut app, KeyCode::Char('s'));
    assert_eq!(app.screen, Screen::Settings);
    // Row zero is the library root.
    press(&mut app, KeyCode::Char('e'));
    let current = app
        .settings
        .as_ref()
        .and_then(|s| s.editing.clone())
        .unwrap();
    for _ in 0..current.chars().count() {
        press(&mut app, KeyCode::Backspace);
    }
    let target = root.join("elsewhere");
    type_text(&mut app, &target.display().to_string());
    press(&mut app, KeyCode::Enter);

    let settings = app.settings.as_ref().unwrap();
    assert_eq!(settings.draft.library_root, target);
    assert_eq!(settings.draft.symbol_dir, target.join("symbols"));
    assert_eq!(
        settings.draft.footprint_dir,
        target.join("footprints.pretty")
    );
    assert_eq!(settings.draft.model_dir, target.join("3dmodels"));
    assert_eq!(settings.draft.temp_dir, target.join("cache"));
    assert_eq!(settings.draft.archive_dir, target.join("archive"));
    assert_eq!(settings.draft.manifest_path, target.join("library.json"));
    assert_eq!(
        settings.draft.merged_symbol_lib,
        Some(target.join("murky-informis.kicad_sym"))
    );
    // Outside the old root, so deliberately left where it was.
    assert_eq!(settings.draft.download_dir, root.join("downloads"));
    assert!(
        settings.note.contains("Symbols"),
        "the screen says what moved: {}",
        settings.note
    );
}

#[test]
fn two_libraries_called_lib_keep_separate_bookmarks() {
    // A generic leaf folder is named after its parent, which is what the user
    // actually calls the project.
    assert_eq!(
        config::project_name_for(Path::new("/home/u/pcb/synth/lib")),
        "synth"
    );
    assert_eq!(
        config::project_name_for(Path::new("/home/u/pcb/synth")),
        "synth"
    );

    let mut cfg = Config {
        library_root: PathBuf::from("/a/synth/lib"),
        ..Default::default()
    };
    let first = cfg.unique_project_name(&config::project_name_for(&cfg.library_root));
    assert_eq!(first, "synth");
    cfg.remember_project(&first);

    cfg.library_root = PathBuf::from("/b/synth/lib");
    let second = cfg.unique_project_name(&config::project_name_for(&cfg.library_root));
    assert_eq!(
        second, "synth 2",
        "a different folder must not overwrite the first bookmark"
    );
    cfg.remember_project(&second);
    assert_eq!(cfg.projects.len(), 2);

    // Re-saving a location under the name it already has updates it in place.
    cfg.remember_project("synth 2");
    assert_eq!(cfg.projects.len(), 2);
    assert_eq!(cfg.projects[1].library_root, PathBuf::from("/b/synth/lib"));
}

#[test]
fn projects_survive_a_config_round_trip() {
    let root = scratch("config-roundtrip");
    let path = root.join("config.toml");
    let mut cfg = config_for(&root);
    cfg.remember_project("main");
    cfg.library_root = root.join("other");
    cfg.remember_project("other");
    cfg.save(&path).unwrap();

    // TOML arrays of tables swallow every key written after them, so `projects`
    // has to be the last field serialised.
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("[[projects]]"), "{text}");
    let table = text.find("[[projects]]").unwrap();
    assert!(
        !text[table..].contains("library_root = ") || text[table..].contains("name = "),
        "plain settings must not follow the project tables"
    );

    let back = Config::load(&path);
    assert_eq!(back.projects.len(), 2);
    assert_eq!(back.projects[0].name, "main");
    assert_eq!(back.projects[0].library_root, root.join("lib"));
    assert_eq!(back.projects[1].library_root, root.join("other"));
    assert_eq!(back.library_root, root.join("other"));
    // The root was moved without the folders under it, which loading corrects:
    // a library is always one self-contained tree.
    assert_eq!(back.symbol_dir, root.join("other/symbols"));
    assert_eq!(back.manifest_path, root.join("other/library.json"));
}

#[test]
fn every_library_file_is_kept_under_the_library_root() {
    let root = scratch("contain");
    let lib = root.join("lib");
    let mut cfg = config_for(&root);

    // Scattered by hand, the way a half-finished config file looks.
    cfg.symbol_dir = root.join("stray/sym");
    cfg.model_dir = PathBuf::from("/var/tmp/models");
    cfg.merged_symbol_lib = Some(root.join("shared/all-parts.kicad_sym"));
    cfg.manifest_path = root.join("index.json");

    let moved = cfg.contain();
    assert_eq!(moved.len(), 4, "moved: {moved:?}");

    // Each keeps the name the user chose, under the root it belongs to.
    assert_eq!(cfg.symbol_dir, lib.join("sym"));
    assert_eq!(cfg.model_dir, lib.join("models"));
    assert_eq!(cfg.merged_symbol_lib, Some(lib.join("all-parts.kicad_sym")));
    assert_eq!(cfg.manifest_path, lib.join("index.json"));
    // Untouched: they were already inside.
    assert_eq!(cfg.footprint_dir, lib.join("footprints.pretty"));
    assert_eq!(cfg.archive_dir, lib.join("archive"));
    // ZIPs are the input, not part of the library, so this stays outside.
    assert_eq!(cfg.download_dir, root.join("downloads"));

    // Already contained, so a second pass is a no-op.
    assert!(cfg.contain().is_empty());
}

#[test]
fn moving_the_root_brings_a_stray_folder_in_with_it() {
    let root = scratch("rebase-stray");
    let mut cfg = config_for(&root);
    // Nested deeper than the default, and one folder left outside entirely.
    cfg.symbol_dir = cfg.library_root.join("kicad/symbols");
    cfg.model_dir = root.join("elsewhere/3dmodels");

    let target = root.join("moved");
    let moved = cfg.rebase(&target);

    assert_eq!(cfg.library_root, target);
    // A nested layout keeps its shape.
    assert_eq!(cfg.symbol_dir, target.join("kicad/symbols"));
    // The stray is pulled in rather than left behind.
    assert_eq!(cfg.model_dir, target.join("3dmodels"));
    assert_eq!(cfg.footprint_dir, target.join("footprints.pretty"));
    assert!(moved.contains(&Field::ModelDir));
    assert!(moved.contains(&Field::SymbolDir));
    assert!(!moved.contains(&Field::DownloadDir));
}

#[test]
fn a_setting_pointed_outside_the_root_is_pulled_back_in() {
    let root = scratch("settings-contain");
    let cfg = ready_config(&root);
    let mut app = app_for(&cfg, &root);

    press(&mut app, KeyCode::Char('s'));
    // Row 2 is Symbols.
    press(&mut app, KeyCode::Down);
    press(&mut app, KeyCode::Down);
    press(&mut app, KeyCode::Char('e'));
    let current = app
        .settings
        .as_ref()
        .and_then(|s| s.editing.clone())
        .unwrap();
    for _ in 0..current.chars().count() {
        press(&mut app, KeyCode::Backspace);
    }
    type_text(&mut app, "/var/tmp/some-other-place");
    press(&mut app, KeyCode::Enter);

    let settings = app.settings.as_ref().unwrap();
    assert_eq!(
        settings.draft.symbol_dir,
        cfg.library_root.join("some-other-place"),
        "the folder name is kept, the location is not"
    );
    assert!(
        settings.note.contains("Symbols"),
        "the screen says what happened: {}",
        settings.note
    );
}

// ------------------------------------------------ existing libraries on disk

#[test]
fn a_library_built_elsewhere_shows_up_without_a_manifest() {
    let root = scratch("detect");
    let cfg = ready_config(&root);

    // A library laid out by hand, or by an older tool: no library.json at all.
    // Its symbol declares the footprint it uses, the way KiCad's do.
    let symbol = kicad_sym("NE555P").replace(
        "(property \"Value\"",
        "(property \"Footprint\" \"footprints:DIP-8_THT\" (at 0 0 0))\n    (property \"Value\"",
    );
    touch(&cfg.symbol_dir.join("NE555P.kicad_sym"), symbol.as_bytes());
    touch(&cfg.symbol_dir.join("NE555P.bak"), b"a backup, not a part");
    touch(&cfg.model_dir.join("NE555P.stp"), b"ISO-10303-21;");
    touch(
        &cfg.footprint_dir.join("NE555P.pretty/DIP-8_THT.kicad_mod"),
        b"(footprint)",
    );
    touch(
        &cfg.footprint_dir.join("DIP-8_THT.kicad_mod"),
        b"(footprint)",
    );
    // Nobody's symbol asks for this one, so it belongs to no part.
    touch(&cfg.footprint_dir.join("SOT-23.kicad_mod"), b"(footprint)");
    // A part with only a 3D model is still worth listing.
    touch(&cfg.model_dir.join("USB3343.step"), b"ISO-10303-21;");

    let found = crate::library::scan(&cfg);
    let names: Vec<&str> = found.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names, vec!["NE555P", "USB3343"], "found: {names:?}");

    let ne555 = &found[0];
    assert_eq!(ne555.source, crate::library::FOUND);
    assert_eq!(
        ne555.symbol_file.as_deref(),
        Some("symbols/NE555P.kicad_sym")
    );
    assert_eq!(ne555.symbol_names, vec!["NE555P"]);
    assert_eq!(ne555.models.len(), 1);
    assert_eq!(
        ne555.footprints,
        vec![
            "footprints.pretty/NE555P.pretty/DIP-8_THT.kicad_mod",
            "footprints.pretty/DIP-8_THT.kicad_mod",
        ],
        "the per-part copy, and the flat one the symbol asks for"
    );
    assert!(!ne555.imported_at.is_empty(), "the file date stands in");

    // And the pane shows them, with each file counted once.
    let app = app_for(&cfg, &root);
    assert_eq!(app.library.len(), 2);
    assert_eq!(app.totals.parts, 2);
    assert_eq!(app.totals.footprints, 1);
    assert_eq!(app.totals.models, 2);
    assert_eq!(
        app.focus,
        Focus::Library,
        "an empty queue starts on the library"
    );
}

#[test]
fn an_imported_part_keeps_its_provenance_over_the_disk_scan() {
    let root = scratch("detect-merge");
    let cfg = ready_config(&root);
    write_cse_zip(&cfg.download_dir.join("LIB_NE555P.zip"), "NE555P", "DIP-8");
    importer::import(&cfg, &cfg.download_dir.join("LIB_NE555P.zip"), |_, _| {}).unwrap();

    // A second part that only exists as files.
    touch(
        &cfg.symbol_dir.join("MT41K128.kicad_sym"),
        kicad_sym("MT41K128").as_bytes(),
    );

    let app = app_for(&cfg, &root);
    let by_name = |name: &str| {
        app.library
            .iter()
            .find(|p| p.name == name)
            .unwrap_or_else(|| panic!("{name} missing from {:?}", app.library))
    };
    assert_eq!(by_name("NE555P").source, "LIB_NE555P.zip");
    assert!(!crate::library::is_found(by_name("NE555P")));
    assert!(crate::library::is_found(by_name("MT41K128")));
    assert_eq!(app.library.len(), 2, "no part is listed twice");

    // A file dropped in beside an imported part still shows up against it,
    // without disturbing where the part came from.
    touch(&cfg.model_dir.join("NE555P.wrl"), b"a model added by hand");
    let mut app = app_for(&cfg, &root);
    app.refresh();
    let ne555 = app.library.iter().find(|p| p.name == "NE555P").unwrap();
    assert_eq!(ne555.source, "LIB_NE555P.zip");
    assert!(
        ne555.models.iter().any(|m| m.ends_with("NE555P.wrl")),
        "models: {:?}",
        ne555.models
    );
}

// ------------------------------------------------------ overwrite protection

#[test]
fn an_import_that_would_replace_files_names_them_before_touching_anything() {
    let root = scratch("conflict");
    let cfg = ready_config(&root);
    let zip = cfg.download_dir.join("LIB_NE555P.zip");
    write_cse_zip(&zip, "NE555P", "DIP-8");

    // First time through there is nothing to replace.
    let first = importer::prepare(&cfg, &zip, |_, _| {}).unwrap();
    assert!(first.conflicts.is_empty(), "{:?}", first.conflicts);
    importer::finish(&cfg, first).unwrap();

    // The archive was filed away, so import the same part a second time.
    let again = cfg.download_dir.join("LIB_NE555P.zip");
    write_cse_zip(&again, "NE555P", "DIP-8");
    let second = importer::prepare(&cfg, &again, |_, _| {}).unwrap();
    assert_eq!(
        second.conflicts,
        vec![
            "symbols/NE555P.kicad_sym",
            "footprints.pretty/NE555P.pretty/DIP-8.kicad_mod",
            "footprints.pretty/DIP-8.kicad_mod",
            "3dmodels/NE555P.stp",
        ]
    );

    // Preparing does not write to the library, and discarding leaves both the
    // library and the source archive exactly as they were.
    let before = std::fs::read(cfg.symbol_dir.join("NE555P.kicad_sym")).unwrap();
    importer::discard(second);
    assert_eq!(
        std::fs::read(cfg.symbol_dir.join("NE555P.kicad_sym")).unwrap(),
        before
    );
    assert!(again.is_file(), "a cancelled import leaves the zip alone");
}

#[test]
fn importing_over_an_existing_part_asks_first() {
    let root = scratch("confirm");
    let cfg = ready_config(&root);
    // A symbol already in the library, with contents worth not losing.
    touch(
        &cfg.symbol_dir.join("NE555P.kicad_sym"),
        b"mine, hand-edited",
    );
    write_cse_zip(&cfg.download_dir.join("LIB_NE555P.zip"), "NE555P", "DIP-8");

    let (mut app, rx) = app_and_events(&cfg, &root);
    press(&mut app, KeyCode::Enter);
    pump(&mut app, &rx);

    assert_eq!(app.screen, Screen::Overwrite);
    let pending = app.pending.as_ref().expect("waiting on an answer");
    assert_eq!(pending.conflicts, vec!["symbols/NE555P.kicad_sym"]);
    assert_eq!(
        std::fs::read(cfg.symbol_dir.join("NE555P.kicad_sym")).unwrap(),
        b"mine, hand-edited",
        "nothing is written while the question is on screen"
    );

    // Saying no leaves the library untouched.
    press(&mut app, KeyCode::Char('n'));
    pump(&mut app, &rx);
    assert_eq!(app.screen, Screen::Main);
    assert!(app.pending.is_none());
    assert!(app.run.is_none());
    assert_eq!(
        std::fs::read(cfg.symbol_dir.join("NE555P.kicad_sym")).unwrap(),
        b"mine, hand-edited"
    );
    assert!(app.status.contains("unchanged"), "status: {}", app.status);
}

#[test]
fn confirming_the_overwrite_completes_the_import() {
    let root = scratch("confirm-yes");
    let cfg = ready_config(&root);
    touch(&cfg.symbol_dir.join("NE555P.kicad_sym"), b"old");
    write_cse_zip(&cfg.download_dir.join("LIB_NE555P.zip"), "NE555P", "DIP-8");

    let (mut app, rx) = app_and_events(&cfg, &root);
    press(&mut app, KeyCode::Enter);
    pump(&mut app, &rx);
    assert_eq!(app.screen, Screen::Overwrite);

    press(&mut app, KeyCode::Char('y'));
    pump(&mut app, &rx);

    assert_eq!(app.screen, Screen::Main);
    let written = std::fs::read_to_string(cfg.symbol_dir.join("NE555P.kicad_sym")).unwrap();
    assert!(
        written.contains("kicad_symbol_lib"),
        "the import went through"
    );
    assert_eq!(app.totals.parts, 1);
}

#[test]
fn a_fresh_import_is_not_interrupted_by_the_prompt() {
    let root = scratch("no-conflict");
    let cfg = ready_config(&root);
    write_cse_zip(&cfg.download_dir.join("LIB_NE555P.zip"), "NE555P", "DIP-8");

    let (mut app, rx) = app_and_events(&cfg, &root);
    press(&mut app, KeyCode::Enter);
    pump(&mut app, &rx);

    assert_eq!(
        app.screen,
        Screen::Main,
        "nothing existed, so nothing to ask"
    );
    assert!(app.pending.is_none());
    assert_eq!(app.totals.parts, 1);
    assert!(
        app.status.starts_with("Added NE555P"),
        "status: {}",
        app.status
    );
}
