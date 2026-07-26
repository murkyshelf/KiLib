//! Searching the web for a component and turning the result into KiCad files.
//!
//! The provider is LCSC/EasyEDA, whose catalogue is public and needs no API
//! key. A search returns [`easyeda::Hit`]s; fetching one produces a
//! [`RemotePart`] holding the same three file kinds a Component Search Engine
//! ZIP would have contained, which is what lets
//! [`crate::importer::install`] place both through exactly the same code.

pub mod easyeda;
pub mod footprint;
pub mod svgpath;
pub mod symbol;

use crate::config::Config;
use crate::importer::{self, ImportSummary, Kind};
use crate::logging;

pub use easyeda::Hit;

/// How many search results to ask the catalogue for.
pub const SEARCH_LIMIT: usize = 25;

/// EasyEDA works in units of 10 mil.
const UNIT_MM: f64 = 10.0 * 0.0254;

/// A component downloaded and converted, ready to be written into the library.
pub struct RemotePart {
    pub name: String,
    pub lcsc: String,
    pub description: String,
    /// `(<file name>, <contents>)`.
    pub symbol: (String, String),
    pub footprint: Option<(String, String)>,
    pub model: Option<(String, Vec<u8>)>,
}

pub fn search(query: &str, limit: usize) -> Result<Vec<Hit>, String> {
    easyeda::search(query, limit)
}

/// Downloads one component and converts it to KiCad files.
///
/// `cfg` is needed only to work out the absolute path the footprint's 3D model
/// reference should point at — the same configured model directory the file
/// itself is about to be written into.
pub fn fetch(cfg: &Config, lcsc: &str) -> Result<RemotePart, String> {
    let component = easyeda::component(lcsc)?;
    let name = sanitize(&component.title);
    if name.is_empty() {
        return Err(format!("{lcsc}: component has no usable name"));
    }
    logging::info(format!(
        "fetched {lcsc} \"{}\" package \"{}\"",
        component.title, component.package_name
    ));

    let model_info = footprint::model_info(&component);
    let model_file = model_info.as_ref().map(|_| format!("{name}.stp"));

    // The footprint references the model by the absolute path it is about to
    // occupy, so the reference stays valid wherever the library is configured.
    let model_reference = model_file
        .as_ref()
        .map(|file| cfg.model_dir.join(file).display().to_string());

    let footprint_name = sanitize(&component.package_name);
    let footprint = (!footprint_name.is_empty()).then(|| {
        (
            format!("{footprint_name}.kicad_mod"),
            footprint::convert(&component, &footprint_name, model_reference.as_deref()),
        )
    });

    // KiCad addresses a footprint as `<library nickname>:<footprint>`, and the
    // nickname is the configured `.pretty` directory's own name.
    let nickname = cfg
        .footprint_dir
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let footprint_ref = (!footprint_name.is_empty()).then(|| {
        if nickname.is_empty() {
            footprint_name.clone()
        } else {
            format!("{nickname}:{footprint_name}")
        }
    });

    let symbol_text = symbol::convert(&component, footprint_ref.as_deref());

    // A part without a 3D model is normal and must not fail the whole fetch.
    let model = match (model_info, model_file) {
        (Some(info), Some(file)) => match easyeda::step_model(&info.uuid) {
            Ok(bytes) => Some((file, bytes)),
            Err(e) => {
                logging::warn(format!("{lcsc}: {e}"));
                None
            }
        },
        _ => None,
    };

    Ok(RemotePart {
        name: name.clone(),
        lcsc: component.lcsc.clone(),
        description: describe(&component),
        symbol: (format!("{name}.kicad_sym"), symbol_text),
        footprint,
        model,
    })
}

/// Downloads a component and writes it into the configured library tree,
/// replacing anything already there. Prefer [`prepare`] + [`importer::finish`]
/// in the interface, so the user is asked first.
pub fn add_to_library<F>(cfg: &Config, lcsc: &str, progress: F) -> Result<ImportSummary, String>
where
    F: Fn(f64, String),
{
    let pending = prepare(cfg, lcsc, progress)?;
    importer::finish(cfg, pending)
}

/// Downloads a component and converts it into staged files, without touching
/// the library. Goes through the same staging area a ZIP import uses, so both
/// routes are placed by the same code.
pub fn prepare<F>(cfg: &Config, lcsc: &str, progress: F) -> Result<importer::Pending, String>
where
    F: Fn(f64, String),
{
    progress(0.05, format!("{lcsc}: looking up"));
    let part = fetch(cfg, lcsc)?;
    logging::info(format!("adding {} — {}", part.lcsc, part.description));
    progress(0.75, format!("{}: writing files", part.name));

    let staging = importer::prepare_staging(cfg, &format!("web-{}", part.lcsc))?;
    let mut staged = vec![importer::stage(
        &staging,
        Kind::Symbol,
        &part.symbol.0,
        part.symbol.1.as_bytes(),
    )?];
    if let Some((name, text)) = &part.footprint {
        staged.push(importer::stage(
            &staging,
            Kind::Footprint,
            name,
            text.as_bytes(),
        )?);
    }
    if let Some((name, bytes)) = &part.model {
        staged.push(importer::stage(&staging, Kind::Model, name, bytes)?);
    }

    let conflicts = importer::conflicts(cfg, &part.name, &staged);
    if !conflicts.is_empty() {
        logging::info(format!(
            "{}: {} existing file(s) would be replaced",
            part.name,
            conflicts.len()
        ));
    }
    progress(1.0, part.name.clone());

    Ok(importer::Pending {
        part: part.name,
        source: format!("LCSC {}", part.lcsc),
        staged,
        staging,
        conflicts,
        zip: None,
    })
}

fn describe(component: &easyeda::Component) -> String {
    let mut parts = vec![component.title.clone()];
    if !component.manufacturer.is_empty() {
        parts.push(component.manufacturer.clone());
    }
    if !component.package_name.is_empty() {
        parts.push(component.package_name.clone());
    }
    parts.join(" · ")
}

// ------------------------------------------------------------------- helpers

/// EasyEDA units -> millimetres, formatted for a KiCad s-expression.
pub fn to_mm(units: f64) -> String {
    num(units * UNIT_MM)
}

/// EasyEDA units -> millimetres.
pub fn mm(units: f64) -> f64 {
    units * UNIT_MM
}

/// Three decimals is finer than any KiCad grid and avoids `-0`.
pub fn num(value: f64) -> String {
    let rounded = (value * 1000.0).round() / 1000.0;
    let rounded = if rounded == 0.0 { 0.0 } else { rounded };
    let mut text = format!("{rounded:.3}");
    if text.contains('.') {
        text = text.trim_end_matches('0').trim_end_matches('.').to_string();
    }
    text
}

/// Makes a name safe as both a file name and a KiCad library identifier.
pub fn sanitize(name: &str) -> String {
    let cleaned: String = name
        .trim()
        .chars()
        .map(|c| match c {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '-' | '_' | '.' | '+' => c,
            _ => '_',
        })
        .collect();
    // Collapse the runs of underscores that punctuation-heavy titles produce.
    let mut out = String::with_capacity(cleaned.len());
    for c in cleaned.chars() {
        if c == '_' && out.ends_with('_') {
            continue;
        }
        out.push(c);
    }
    out.trim_matches('_').to_string()
}

/// Escapes a string for a KiCad s-expression literal.
pub fn escape(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}
