//! EasyEDA schematic shapes -> a one-symbol `.kicad_sym` library.
//!
//! EasyEDA works in 10-mil units with y growing downwards; KiCad symbols are
//! in millimetres with y growing upwards. Every coordinate therefore goes
//! through [`super::to_mm`] relative to the schematic origin, with the y axis
//! negated.

use super::easyeda::{groups, Component, Record};
use super::{escape, sanitize, to_mm};
use crate::online::svgpath::{self, Seg};

/// Renders a complete `.kicad_sym` containing this one component.
pub fn convert(component: &Component, footprint_ref: Option<&str>) -> String {
    let name = sanitize(&component.title);
    let (ox, oy) = component.symbol_origin;

    let mut graphics = String::new();
    let mut pins = String::new();

    for shape in &component.symbol_shapes {
        // Pins carry `^^` sub-records; every other shape is a flat record.
        let record = Record::new(groups(shape).first().copied().unwrap_or(shape));
        match record.kind() {
            "P" => pins.push_str(&pin(shape, ox, oy)),
            "R" => graphics.push_str(&rectangle(&record, ox, oy)),
            "E" => graphics.push_str(&ellipse(&record, ox, oy)),
            "PL" | "PG" => graphics.push_str(&polyline(&record, ox, oy)),
            "PT" | "A" => graphics.push_str(&path_shape(&record, ox, oy)),
            _ => {}
        }
    }

    let reference = component.prefix.trim_end_matches('?');
    let reference = if reference.is_empty() { "U" } else { reference };

    let mut out = String::new();
    out.push_str("(kicad_symbol_lib (version 20211014) (generator cse-importer)\n");
    out.push_str(&format!(
        "  (symbol \"{}\" (pin_names (offset 0.254)) (in_bom yes) (on_board yes)\n",
        escape(&name)
    ));

    let mut id = 0;
    let mut property = |key: &str, value: &str, hidden: bool| -> String {
        let text = format!(
            "    (property \"{}\" \"{}\" (id {id}) (at 0 0 0)\n      (effects (font (size 1.27 1.27)){})\n    )\n",
            escape(key),
            escape(value),
            if hidden { " hide" } else { "" }
        );
        id += 1;
        text
    };

    out.push_str(&property("Reference", reference, false));
    out.push_str(&property("Value", &name, false));
    out.push_str(&property("Footprint", footprint_ref.unwrap_or(""), true));
    out.push_str(&property("Datasheet", &component.datasheet, true));
    out.push_str(&property("LCSC", &component.lcsc, true));
    if !component.manufacturer.is_empty() {
        out.push_str(&property("Manufacturer", &component.manufacturer, true));
    }

    // KiCad splits a symbol into a shared graphic unit (`_0_1`) and one unit
    // per gate (`_1_1`); LCSC parts are single-gate.
    out.push_str(&format!("    (symbol \"{}_0_1\"\n", escape(&name)));
    out.push_str(&graphics);
    out.push_str("    )\n");
    out.push_str(&format!("    (symbol \"{}_1_1\"\n", escape(&name)));
    out.push_str(&pins);
    out.push_str("    )\n");

    out.push_str("  )\n)\n");
    out
}

// ------------------------------------------------------------------- shapes

fn rectangle(record: &Record, ox: f64, oy: f64) -> String {
    // R~x~y~rx~ry~width~height~stroke~strokeWidth~strokeStyle~fill~id~locked
    let (x, y) = (record.num(1), record.num(2));
    let (w, h) = (record.num(5), record.num(6));
    format!(
        "      (rectangle (start {} {}) (end {} {})\n{}{}      )\n",
        to_mm(x - ox),
        to_mm(oy - y),
        to_mm(x + w - ox),
        to_mm(oy - (y + h)),
        stroke(record.num(8)),
        fill(record.str(10)),
    )
}

fn ellipse(record: &Record, ox: f64, oy: f64) -> String {
    // E~cx~cy~rx~ry~stroke~strokeWidth~strokeStyle~fill~id~locked
    let (cx, cy) = (record.num(1), record.num(2));
    // KiCad symbols have no ellipse primitive; the mean radius is the closest
    // faithful circle.
    let radius = (record.num(3).abs() + record.num(4).abs()) / 2.0;
    format!(
        "      (circle (center {} {}) (radius {})\n{}{}      )\n",
        to_mm(cx - ox),
        to_mm(oy - cy),
        to_mm(radius),
        stroke(record.num(6)),
        fill(record.str(8)),
    )
}

fn polyline(record: &Record, ox: f64, oy: f64) -> String {
    // PL/PG~points~stroke~strokeWidth~strokeStyle~fill~id~locked
    let mut points = svgpath::parse_points(record.str(1));
    if points.len() < 2 {
        return String::new();
    }
    // A polygon is a closed polyline.
    if record.kind() == "PG" && points.first() != points.last() {
        points.push(points[0]);
    }
    let pts: String = points
        .iter()
        .map(|p| format!("(xy {} {}) ", to_mm(p.x - ox), to_mm(oy - p.y)))
        .collect();
    format!(
        "      (polyline\n        (pts {})\n{}{}      )\n",
        pts.trim_end(),
        stroke(record.num(3)),
        fill(record.str(5)),
    )
}

/// `PT` (free path) and `A` (arc) both hold an SVG path in field 1.
fn path_shape(record: &Record, ox: f64, oy: f64) -> String {
    let mut out = String::new();
    let stroke_text = stroke(record.num(3));
    let fill_text = fill(record.str(5));
    let mut run: Vec<svgpath::Point> = Vec::new();

    let flush = |run: &mut Vec<svgpath::Point>, out: &mut String| {
        if run.len() >= 2 {
            let pts: String = run
                .iter()
                .map(|p| format!("(xy {} {}) ", to_mm(p.x - ox), to_mm(oy - p.y)))
                .collect();
            out.push_str(&format!(
                "      (polyline\n        (pts {})\n{stroke_text}{fill_text}      )\n",
                pts.trim_end()
            ));
        }
        run.clear();
    };

    for seg in svgpath::parse_path(record.str(1)) {
        match seg {
            Seg::Line { from, to } => {
                if run.last() != Some(&from) {
                    flush(&mut run, &mut out);
                    run.push(from);
                }
                run.push(to);
            }
            Seg::Arc { from, mid, to } => {
                flush(&mut run, &mut out);
                out.push_str(&format!(
                    "      (arc (start {} {}) (mid {} {}) (end {} {})\n{stroke_text}{fill_text}      )\n",
                    to_mm(from.x - ox),
                    to_mm(oy - from.y),
                    to_mm(mid.x - ox),
                    to_mm(oy - mid.y),
                    to_mm(to.x - ox),
                    to_mm(oy - to.y),
                ));
            }
        }
    }
    flush(&mut run, &mut out);
    out
}

// --------------------------------------------------------------------- pins

fn pin(shape: &str, ox: f64, oy: f64) -> String {
    let parts = groups(shape);
    let settings = Record::new(parts.first().copied().unwrap_or(""));

    // Group 1 is the connection point, group 2 the pin's own path, which is
    // what actually says which way the pin points and how long it is.
    let dot = Record::new(parts.get(1).copied().unwrap_or(""));
    let (px, py) = (
        dot.opt_num(0).unwrap_or_else(|| settings.num(4)),
        dot.opt_num(1).unwrap_or_else(|| settings.num(5)),
    );

    let path = parts
        .get(2)
        .and_then(|g| g.split('~').next())
        .unwrap_or_default();
    let segments = svgpath::parse_path(path);
    let (dx, dy) = match (segments.first(), segments.last()) {
        (Some(first), Some(last)) => {
            let start = match first {
                Seg::Line { from, .. } | Seg::Arc { from, .. } => *from,
            };
            let end = match last {
                Seg::Line { to, .. } | Seg::Arc { to, .. } => *to,
            };
            (end.x - start.x, end.y - start.y)
        }
        // No path: fall back to a default-length pin pointing at the body.
        _ => (10.0, 0.0),
    };

    let length = (dx * dx + dy * dy).sqrt();
    let length = if length <= 0.0 { 10.0 } else { length };
    // KiCad's angle points from the connection end towards the body, in a
    // y-up frame — hence the negated dy.
    let angle = snap_angle((-dy).atan2(dx).to_degrees());

    let name = text_of(parts.get(3).copied().unwrap_or(""));
    let number = text_of(parts.get(4).copied().unwrap_or(""));
    let name = if name.is_empty() {
        "~".to_string()
    } else {
        name
    };
    let number = if number.is_empty() {
        settings.str(3).to_string()
    } else {
        number
    };

    let inverted = Record::new(parts.get(5).copied().unwrap_or("")).str(0) == "1";
    let clock = Record::new(parts.get(6).copied().unwrap_or("")).str(0) == "1";
    let graphic = match (inverted, clock) {
        (true, true) => "inverted_clock",
        (true, false) => "inverted",
        (false, true) => "clock",
        (false, false) => "line",
    };
    let hidden = settings.str(1) != "show";

    format!(
        "      (pin {} {} (at {} {} {angle}) (length {}){}\n        (name \"{}\" (effects (font (size 1.27 1.27))))\n        (number \"{}\" (effects (font (size 1.27 1.27))))\n      )\n",
        electrical(settings.str(2)),
        graphic,
        to_mm(px - ox),
        to_mm(oy - py),
        to_mm(length),
        if hidden { " hide" } else { "" },
        escape(&name),
        escape(&number),
    )
}

/// A pin label group is `visible~x~y~rotation~text~anchor~…`.
fn text_of(group: &str) -> String {
    Record::new(group).str(4).trim().to_string()
}

fn electrical(code: &str) -> &'static str {
    match code {
        "1" => "input",
        "2" => "output",
        "3" => "bidirectional",
        "4" => "power_in",
        _ => "unspecified",
    }
}

fn snap_angle(degrees: f64) -> i32 {
    ((degrees / 90.0).round() as i32 * 90).rem_euclid(360)
}

// ------------------------------------------------------------------- styling

/// EasyEDA stroke widths are in the same 10-mil units as coordinates; `0`
/// tells KiCad to use its default, which is what a hairline should be.
fn stroke(width: f64) -> String {
    let mm = if width > 1.0 {
        to_mm(width)
    } else {
        "0".to_string()
    };
    format!("        (stroke (width {mm}) (type default))\n")
}

fn fill(color: &str) -> String {
    let kind = if color.is_empty() || color.eq_ignore_ascii_case("none") {
        "none"
    } else {
        "background"
    };
    format!("        (fill (type {kind}))\n")
}
