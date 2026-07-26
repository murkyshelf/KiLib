# KiLib 
 
Terminal UI for building a KiCad library. It imports Component Search Engine /
Samacsys ZIP archives, and searches the LCSC / EasyEDA catalogue online to add
parts that you do not have an archive for.

Every filesystem location is configurable, persisted, and shown on screen. No
directory is hardcoded anywhere in the codebase.

## Build and run

```bash
cargo build --release
./target/release/kilib
```
## Install 

```bash
cargo install --path .
```
## Command line

```bash
kilib
kilib --download-dir ~/Downloads/KiCad
kilib --library-root ~/Documents/PCB/lib
kilib --config ~/custom/config.toml

# Saved library locations.
kilib --list-projects
kilib --project synth

# Headless. These replace existing files without asking; the interface asks.
kilib --search "NE555P"
kilib --add C46749
kilib --import ~/Downloads/KiCad/LIB_NE555P.zip
kilib --project synth --add C46749
```

Arguments override the configuration file for that run. They are only persisted
if you save from the Settings screen.

## Configuration

Default location is `$XDG_CONFIG_HOME/KiLib/config.toml`
(`~/.config/KiLib/config.toml`). It is created with defaults on first run.

```toml
library_root = "/home/user/KiCad/lib"
download_dir = "/home/user/Downloads/KiCad"
symbol_dir = "/home/user/KiCad/lib/symbols"
footprint_dir = "/home/user/KiCad/lib/footprints.pretty"
model_dir = "/home/user/KiCad/lib/3dmodels"
temp_dir = "/home/user/KiCad/lib/cache"
archive_dir = "/home/user/KiCad/lib/archive"
merged_symbol_lib = "/home/user/KiCad/lib/murky-informis.kicad_sym"
manifest_path = "/home/user/KiCad/lib/library.json"
per_part_footprint_dirs = true
backup_before_overwrite = true
delete_zip_after_import = false

[[projects]]
name = "synth"
library_root = "/home/user/pcb/synth/lib"

[[projects]]
name = "eurorack-psu"
library_root = "/home/user/pcb/psu/lib"
```

`~` is expanded. Relative paths resolve against the config file's directory, not
the process working directory. All twelve settings are editable from the
Settings screen; clearing `merged_symbol_lib` turns merging off. Every path
except `download_dir` is kept under `library_root` — see below.

## Projects

A project is one KiCad library, and most boards want their own. Press `P` on the
main screen to switch between saved locations, and the whole configuration
follows: symbols, footprints, 3D models, temp, archive, the merged symbol library
and the manifest all move to the same relative place under the new root.

The same rule applies wherever the root changes — the `Library Root` row in
Settings, `--library-root`, and switching projects. The Settings screen names
exactly what moved, and nothing is written until `S`.

A library is always one self-contained folder. Symbols, footprints, 3D models,
temp, archive, the merged symbol library and the manifest live under the root,
whatever you call them: point one somewhere else and it keeps your name but is
re-anchored inside the root, which is reported on screen and in the log. This is
checked when the configuration is loaded too, so a hand-edited `config.toml`
cannot leave part of a library behind. The ZIP folder is the one exception — the
archives are the input, and they come from wherever your browser puts them.

Switching to a location that does not exist yet raises the usual startup
prompt, with `[A] Create All` so a fresh tree takes one answer instead of seven.
Forgetting a project removes the bookmark only — nothing on disk is touched.

Switching from the Projects screen is written to `config.toml` straight away, so
the next run opens where you left off. `--project` does not persist, which makes
it safe in scripts.

## Library layout

Both import routes — ZIP and web — produce the same tree:

```
3dmodels/<PART>.stp
footprints.pretty/<FOOTPRINT>.kicad_mod
footprints.pretty/<PART>.pretty/<FOOTPRINT>.kicad_mod
symbols/<PART>.kicad_sym
symbols/<PART>.bak
murky-informis.kicad_sym          merged library, every symbol
murky-informis.bak
library.json                      machine-readable description of the above
```

Every one of those is under the library root — see [Projects](#projects) — so the
whole library can be moved, copied or handed over in one piece.

`library.json` records the layout (which directory holds what) and one entry per
part: where it came from, when, its symbol names, and every footprint and model
file, all relative to the library root so the tree can be moved.

Per-part `.pretty` folders and the `.bak` copies can each be switched off in
Settings.

## Keys

The main screen is a dashboard: what the library holds, the queue of archives
waiting to be imported, and the parts already in it, side by side. `Tab` moves
between the two lists and the Details box follows whichever has focus.

| Screen | Key | Action |
| --- | --- | --- |
| Main | `Tab` | Switch between the queue and the library |
| Main | `Enter` / `i` | Import the selected archive |
| Main | `A` | Import every archive in the queue |
| Main | `W` | Search the web for a component |
| Main | `P` | Switch library location (Projects) |
| Main | `/` | Filter both lists as you type (`Esc` clears) |
| Main | `↑ ↓` `Home` `End` | Move in the focused list |
| Main | `R` | Force a full rescan |
| Main | `S` | Open Settings |
| Main | `F12` | Diagnostics |
| Main | `Q` | Quit |
| Search | `Enter` | Run the search, then add the highlighted part |
| Search | `✓` | Marks results already in the library |
| Search | `↑ ↓` | Move through results |
| Search | `/` | Edit the query again |
| Search | `Esc` | Back |
| Overwrite | `Y` | Replace the files it listed |
| Overwrite | `A` | Replace, and stop asking for this queue |
| Overwrite | `S` | Skip this one, keep going |
| Overwrite | `N` / `Esc` | Cancel; nothing is written |
| Projects | `Enter` | Switch to the highlighted location |
| Projects | `B` | Browse to another library, and save it |
| Projects | `N` | Save where you are now under a name |
| Projects | `E` | Rename |
| Projects | `D` | Forget (the bookmark only) |
| Projects | `Esc` | Back |
| Settings | `E` | Edit the highlighted path |
| Settings | `Space` | Toggle a yes/no setting |
| Settings | `Tab` | Next field |
| Settings | `Enter` | Browse for a folder |
| Settings | `S` | Save to `config.toml` |
| Settings | `Esc` | Discard changes |
| Picker | `↑ ↓` | Move |
| Picker | `Enter` | Open folder |
| Picker | `Backspace` | Parent folder |
| Picker | `Space` | Select this folder |
| Picker | `Esc` | Cancel |
| Diagnostics | `↑ ↓` | Scroll |

## Behaviour

- **Discovery** is recursive (`walkdir`) under the configured ZIP folder, so
  `Downloads/Memory/MT41K128.zip` is queued alongside top-level archives. The
  archive and temp folders are skipped.
- **Watching** uses `notify`; adding, removing or renaming a ZIP updates the
  queue with no keypress.
- **Startup validation** asks about the two locations you actually choose — the
  library root and the ZIP folder — with `[Y] Create / [C] Change Path /
  [Q] Quit`, and `[A]` when both are missing. Approving the root creates the
  folders the library is made of, which the prompt lists by name first. The rest
  are created on demand during an import, so a deleted `symbols/` is not a
  question, and `[C]` on the root moves the whole library with it.
- **Import** takes `*/KiCad/*.kicad_sym` into the symbol folder,
  `*.kicad_mod` into the footprint folder and `*/3D/*.stp|.step|.wrl` into the
  3D folder. Files are extracted to the temp folder first, then placed. The
  source ZIP is moved to the archive folder, or deleted when
  `delete_zip_after_import` is set.
- **Web search** queries the public LCSC / EasyEDA catalogue — no account or API
  key. Adding a result downloads the component and converts it:
  the schematic becomes a `.kicad_sym`, the package becomes a `.kicad_mod`, and
  the STEP model is saved next to them with the footprint's `(model …)` record
  pointing at the configured 3D folder. The converted files then go through the
  same code that places files from a ZIP.
- **Existing libraries** are picked up automatically. The Library pane lists what
  the folders actually hold, not just what `library.json` records, so pointing
  the application at a library built by hand or by an older tool shows that
  library straight away. Those parts are dimmed and marked `found on disk` until
  something is imported over them. A part is recognised by its symbol file, its
  3D model or its per-part `.pretty` folder; a flat footprint is attached to
  whichever symbols ask for it by their `Footprint` property. Files sitting
  beside an imported part are listed against it too.
- **Overwriting is confirmed first.** An import is unpacked into the temp folder
  and checked against the library before anything is written. If it would replace
  existing files they are named on screen, and nothing is touched until you
  answer `[Y] Overwrite` or `[N] Cancel` — a whole-queue import also offers
  `[A] Overwrite All` and `[S] Skip`. Cancelling leaves both the library and the
  source archive exactly as they were.
- **Merging** appends every imported symbol to `merged_symbol_lib`, replacing any
  symbol already there under the same name, so one library entry in KiCad covers
  the whole collection.
- **Logging** goes to `importer.log` beside the config file: startup, config
  load, working directory, scan path, every discovered ZIP, every HTTP request
  and response size, every import and every filesystem error.

### Conversion notes

EasyEDA geometry is in units of 10 mil with y growing downwards; KiCad is in
millimetres with y growing upwards, so every coordinate is converted relative to
the schematic or package origin with the y axis negated. Beyond that:

- Pin direction and length come from the pin's own SVG path rather than its
  rotation field, which is the reliable signal in the source data.
- SVG elliptical arcs are converted to KiCad's three-point form; a sweep wider
  than half a turn is split in two, because a near-complete circle written as one
  arc would otherwise collapse.
- Filled-region cutouts are dropped rather than drawn as solid polygons — KiCad
  has no subtractive primitive, and drawing them would be worse than omitting
  them.
- The 3D model's `z` field is *not* applied as an offset. It reports where the
  STEP model's underside already sits, so applying it would sink the part into
  the board by its own depth.

## Tests

```sh
cargo test
```

Two tests are ignored by default. The first hits the live catalogue; the second
needs Component Search Engine archives, which are copied to a scratch directory
and never imported in place:

```sh
cargo test -- --ignored --nocapture
CSE_FIXTURE_DIR=~/Downloads/KiCad cargo test -- --ignored --nocapture
```
