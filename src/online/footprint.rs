//! EasyEDA package shapes -> a `.kicad_mod` footprint.
//!
//! Same unit and axis conversion as the symbol side: 10-mil units with y down
//! become millimetres with y up, relative to the package origin.

use serde_json::Value;

use super::easyeda::{Component, Record};
use super::{escape, mm, num, to_mm};
use crate::logging;
use crate::online::svgpath::{self, Point, Seg};

/// KiCad footprint file format version this writes.
const FORMAT_VERSION: &str = "20221018";
/// KiCad's default silkscreen line width, used when EasyEDA gives none.
const DEFAULT_LINE_MM: f64 = 0.12;

pub struct Model3d {
    pub uuid: String,
    /// Millimetres, relative to the footprint origin.
    pub translation: (f64, f64, f64),
    /// Degrees.
    pub rotation: (f64, f64, f64),
}

/// Reads the `SVGNODE` record that names the component's 3D model.
pub fn model_info(component: &Component) -> Option<Model3d> {
    let (fx, fy) = component.package_origin;

    for shape in &component.package_shapes {
        let Some(json) = shape.strip_prefix("SVGNODE~") else {
            continue;
        };
        let Ok(node) = serde_json::from_str::<Value>(json) else {
            logging::warn("footprint has an SVGNODE that is not valid JSON");
            continue;
        };
        let attrs = &node["attrs"];
        let uuid = attrs["uuid"].as_str().unwrap_or_default();
        if uuid.is_empty() {
            continue;
        }

        let origin = pair(attrs["c_origin"].as_str().unwrap_or_default());
        let (rx, ry, rz) = triple(attrs["c_rotation"].as_str().unwrap_or_default());

        let title = attrs["title"].as_str().unwrap_or_default().to_string();
        logging::info(format!(
            "footprint references 3D model \"{title}\" ({uuid})"
        ));

        return Some(Model3d {
            uuid: uuid.to_string(),
            translation: (
                round2(mm(origin.0 - fx)),
                // KiCad's 3D viewer is y-up like the board, EasyEDA's data is
                // y-down like the canvas.
                round2(-mm(origin.1 - fy)),
                // Deliberately zero. The record's `z` field reports where the
                // STEP model's underside already sits (it matches the model's
                // own minimum z), so it describes the model rather than asking
                // for a shift; applying it would sink the part into the board
                // by its own depth.
                0.0,
            ),
            rotation: (
                (360.0 - rx) % 360.0,
                (360.0 - ry) % 360.0,
                (360.0 - rz) % 360.0,
            ),
        });
    }
    None
}

/// Renders the footprint. `model_path` is written into the `(model …)` record
/// when the part has a 3D model.
pub fn convert(component: &Component, name: &str, model_path: Option<&str>) -> String {
    let (ox, oy) = component.package_origin;
    let mut body = String::new();
    let mut has_smd = false;
    let mut has_through_hole = false;

    for shape in &component.package_shapes {
        let record = Record::new(shape);
        match record.kind() {
            "PAD" => {
                let (text, through) = pad(&record, ox, oy);
                if !text.is_empty() {
                    has_through_hole |= through;
                    has_smd |= !through;
                    body.push_str(&text);
                }
            }
            "TRACK" => body.push_str(&track(&record, ox, oy)),
            "CIRCLE" => body.push_str(&circle(&record, ox, oy)),
            "ARC" => body.push_str(&arc(&record, ox, oy)),
            "RECT" => body.push_str(&rect(&record, ox, oy)),
            "SOLIDREGION" => body.push_str(&solid_region(&record, ox, oy)),
            "HOLE" => body.push_str(&hole(&record, ox, oy)),
            "VIA" => body.push_str(&via(&record, ox, oy)),
            // TEXT is skipped: the reference and value fields below replace the
            // silkscreen labels EasyEDA bakes into the package.
            _ => {}
        }
    }

    let attribute = match (has_smd, has_through_hole) {
        (true, false) => "  (attr smd)\n",
        (false, true) => "  (attr through_hole)\n",
        _ => "",
    };

    let mut out = String::new();
    out.push_str(&format!(
        "(footprint \"{}\" (version {FORMAT_VERSION}) (generator cse-importer)\n",
        escape(name)
    ));
    out.push_str("  (layer \"F.Cu\")\n");
    out.push_str(&format!("  (descr \"{}\")\n", escape(&component.title)));
    out.push_str(&format!(
        "  (tags \"{}\")\n",
        escape(&format!("{} {}", component.lcsc, component.package_name))
    ));
    out.push_str(attribute);
    out.push_str(
        "  (fp_text reference \"REF**\" (at 0 0) (layer \"F.SilkS\")\n    (effects (font (size 1 1) (thickness 0.15)))\n  )\n",
    );
    out.push_str(&format!(
        "  (fp_text value \"{}\" (at 0 0) (layer \"F.Fab\") hide\n    (effects (font (size 1 1) (thickness 0.15)))\n  )\n",
        escape(name)
    ));
    out.push_str(&body);

    if let Some(path) = model_path {
        if let Some(model) = model_info(component) {
            out.push_str(&format!(
                "  (model \"{}\"\n    (offset (xyz {} {} {}))\n    (scale (xyz 1 1 1))\n    (rotate (xyz {} {} {}))\n  )\n",
                escape(path),
                num(model.translation.0),
                num(model.translation.1),
                num(model.translation.2),
                num(model.rotation.0),
                num(model.rotation.1),
                num(model.rotation.2),
            ));
        }
    }
    out.push_str(")\n");
    out
}

// --------------------------------------------------------------------- pads

/// Returns the pad's text and whether it is a through-hole pad.
fn pad(record: &Record, ox: f64, oy: f64) -> (String, bool) {
    // PAD~shape~cx~cy~w~h~layer~net~number~holeR~points~rot~id~holeLen~holePt~plated
    let (cx, cy) = (record.num(2), record.num(3));
    let (width, height) = (record.num(4), record.num(5));
    let layer = record.str(6);
    let number = record.str(8);
    let hole_radius = record.num(9);
    let rotation = record.num(11);
    let hole_length = record.num(13);
    let plated = !record.str(15).eq_ignore_ascii_case("N");

    if width <= 0.0 && height <= 0.0 && hole_radius <= 0.0 {
        return (String::new(), false);
    }

    let shape = match record.str(1).to_uppercase().as_str() {
        "ELLIPSE" => {
            if (width - height).abs() < 1e-6 {
                "circle"
            } else {
                "oval"
            }
        }
        "OVAL" => "oval",
        // KiCad custom pads need a full primitive list; the bounding rectangle
        // keeps the copper footprint right even if the outline is simplified.
        "POLYGON" => "rect",
        _ => "rect",
    };

    let through = hole_radius > 0.0;
    let pad_type = match (through, plated) {
        (false, _) => "smd",
        (true, true) => "thru_hole",
        (true, false) => "np_thru_hole",
    };

    let layers = if through {
        "\"*.Cu\" \"*.Mask\""
    } else if layer == "2" {
        "\"B.Cu\" \"B.Paste\" \"B.Mask\""
    } else {
        "\"F.Cu\" \"F.Paste\" \"F.Mask\""
    };

    // EasyEDA rotates clockwise on a y-down canvas; KiCad rotates
    // counter-clockwise on a y-up board.
    let angle = (360.0 - rotation).rem_euclid(360.0);
    let at = if angle == 0.0 {
        format!("(at {} {})", to_mm(cx - ox), to_mm(oy - cy))
    } else {
        format!("(at {} {} {})", to_mm(cx - ox), to_mm(oy - cy), num(angle))
    };

    let drill = if !through {
        String::new()
    } else if hole_length > 0.0 {
        // A slotted hole: EasyEDA gives the extra length, KiCad wants both axes.
        format!(
            " (drill oval {} {})",
            num(mm(hole_length)),
            num(mm(hole_radius * 2.0))
        )
    } else {
        format!(" (drill {})", num(mm(hole_radius * 2.0)))
    };

    let text = format!(
        "  (pad \"{}\" {pad_type} {shape} {at} (size {} {}){drill} (layers {layers}))\n",
        escape(number),
        to_mm(width),
        to_mm(height),
    );
    (text, through)
}

fn hole(record: &Record, ox: f64, oy: f64) -> String {
    // HOLE~cx~cy~radius~id~locked
    let diameter = mm(record.num(3) * 2.0);
    if diameter <= 0.0 {
        return String::new();
    }
    format!(
        "  (pad \"\" np_thru_hole circle (at {} {}) (size {} {}) (drill {}) (layers \"*.Cu\" \"*.Mask\"))\n",
        to_mm(record.num(1) - ox),
        to_mm(oy - record.num(2)),
        num(diameter),
        num(diameter),
        num(diameter),
    )
}

fn via(record: &Record, ox: f64, oy: f64) -> String {
    // VIA~cx~cy~diameter~net~holeRadius~id~locked
    let outer = mm(record.num(3));
    let drill = mm(record.num(5) * 2.0);
    if outer <= 0.0 {
        return String::new();
    }
    format!(
        "  (pad \"\" thru_hole circle (at {} {}) (size {} {}) (drill {}) (layers \"*.Cu\" \"*.Mask\"))\n",
        to_mm(record.num(1) - ox),
        to_mm(oy - record.num(2)),
        num(outer),
        num(outer),
        num(drill.max(0.1)),
    )
}

// ----------------------------------------------------------------- graphics

fn track(record: &Record, ox: f64, oy: f64) -> String {
    // TRACK~strokeWidth~layer~net~points~id~locked
    let layer = layer_name(record.str(2));
    let width = line_width(record.num(1));
    let points = svgpath::parse_points(record.str(4));
    points
        .windows(2)
        .map(|pair| line(pair[0], pair[1], &width, layer, ox, oy))
        .collect()
}

fn circle(record: &Record, ox: f64, oy: f64) -> String {
    // CIRCLE~cx~cy~radius~strokeWidth~layer~id~locked
    let (cx, cy) = (record.num(1), record.num(2));
    let radius = record.num(3);
    if radius <= 0.0 {
        return String::new();
    }
    format!(
        "  (fp_circle (center {} {}) (end {} {}) (stroke (width {}) (type solid)) (fill none) (layer \"{}\"))\n",
        to_mm(cx - ox),
        to_mm(oy - cy),
        to_mm(cx + radius - ox),
        to_mm(oy - cy),
        line_width(record.num(4)),
        layer_name(record.str(5)),
    )
}

fn arc(record: &Record, ox: f64, oy: f64) -> String {
    // ARC~strokeWidth~layer~net~path~helperDots~id~locked
    let layer = layer_name(record.str(2));
    let width = line_width(record.num(1));
    let mut out = String::new();
    for seg in svgpath::parse_path(record.str(4)) {
        match seg {
            Seg::Line { from, to } => out.push_str(&line(from, to, &width, layer, ox, oy)),
            Seg::Arc { from, mid, to } => out.push_str(&format!(
                "  (fp_arc (start {} {}) (mid {} {}) (end {} {}) (stroke (width {width}) (type solid)) (layer \"{layer}\"))\n",
                to_mm(from.x - ox),
                to_mm(oy - from.y),
                to_mm(mid.x - ox),
                to_mm(oy - mid.y),
                to_mm(to.x - ox),
                to_mm(oy - to.y),
            )),
        }
    }
    out
}

fn rect(record: &Record, ox: f64, oy: f64) -> String {
    // RECT~x~y~width~height~layer~id~…
    let (x, y) = (record.num(1), record.num(2));
    let (w, h) = (record.num(3), record.num(4));
    if w == 0.0 || h == 0.0 {
        return String::new();
    }
    format!(
        "  (fp_rect (start {} {}) (end {} {}) (stroke (width {}) (type solid)) (fill none) (layer \"{}\"))\n",
        to_mm(x - ox),
        to_mm(oy - y),
        to_mm(x + w - ox),
        to_mm(oy - (y + h)),
        num(DEFAULT_LINE_MM),
        layer_name(record.str(5)),
    )
}

fn solid_region(record: &Record, ox: f64, oy: f64) -> String {
    // SOLIDREGION~layer~net~path~type~id~…
    // Cutouts subtract from a region; KiCad has no equivalent primitive, and
    // drawing them as filled polygons would be worse than leaving them out.
    if record.str(4).eq_ignore_ascii_case("cutout") {
        return String::new();
    }
    let points = svgpath::path_points(record.str(3));
    if points.len() < 3 {
        return String::new();
    }
    let pts: String = points
        .iter()
        .map(|p| format!("(xy {} {}) ", to_mm(p.x - ox), to_mm(oy - p.y)))
        .collect();
    format!(
        "  (fp_poly (pts {}) (stroke (width 0) (type solid)) (fill solid) (layer \"{}\"))\n",
        pts.trim_end(),
        layer_name(record.str(1)),
    )
}

fn line(from: Point, to: Point, width: &str, layer: &str, ox: f64, oy: f64) -> String {
    format!(
        "  (fp_line (start {} {}) (end {} {}) (stroke (width {width}) (type solid)) (layer \"{layer}\"))\n",
        to_mm(from.x - ox),
        to_mm(oy - from.y),
        to_mm(to.x - ox),
        to_mm(oy - to.y),
    )
}

fn line_width(units: f64) -> String {
    let width = mm(units);
    num(if width <= 0.0 { DEFAULT_LINE_MM } else { width })
}

/// EasyEDA layer ids -> KiCad layer names.
///
/// 99–101 are EasyEDA's component/lead outline layers, which correspond to
/// KiCad's fabrication layer.
fn layer_name(id: &str) -> &'static str {
    match id {
        "1" => "F.Cu",
        "2" => "B.Cu",
        "3" => "F.SilkS",
        "4" => "B.SilkS",
        "5" => "F.Paste",
        "6" => "B.Paste",
        "7" => "F.Mask",
        "8" => "B.Mask",
        "10" | "11" => "Edge.Cuts",
        "12" => "Cmts.User",
        "13" | "99" | "100" | "101" => "F.Fab",
        "14" => "B.Fab",
        _ => "Dwgs.User",
    }
}

// ------------------------------------------------------------------ parsing

fn pair(text: &str) -> (f64, f64) {
    let mut it = text
        .split(',')
        .map(|s| s.trim().parse::<f64>().unwrap_or(0.0));
    (it.next().unwrap_or(0.0), it.next().unwrap_or(0.0))
}

fn triple(text: &str) -> (f64, f64, f64) {
    let mut it = text
        .split(',')
        .map(|s| s.trim().parse::<f64>().unwrap_or(0.0));
    (
        it.next().unwrap_or(0.0),
        it.next().unwrap_or(0.0),
        it.next().unwrap_or(0.0),
    )
}

fn round2(value: f64) -> f64 {
    (value * 100.0).round() / 100.0
}
