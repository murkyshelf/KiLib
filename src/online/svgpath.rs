//! The little bit of SVG geometry EasyEDA data needs.
//!
//! EasyEDA stores tracks, regions and arcs as SVG path strings. KiCad wants
//! explicit line segments and three-point arcs, so paths are flattened into
//! [`Seg`]s and elliptical arcs are converted from SVG's endpoint
//! parameterisation to a centre parameterisation to find the midpoint.

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Point {
    pub x: f64,
    pub y: f64,
}

impl Point {
    pub fn new(x: f64, y: f64) -> Self {
        Point { x, y }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Seg {
    Line {
        from: Point,
        to: Point,
    },
    /// KiCad's arc form: through `mid`, from `from` to `to`.
    Arc {
        from: Point,
        mid: Point,
        to: Point,
    },
}

/// Splits `"3981.36 2998.27 3981.36 2996.70"` into points.
pub fn parse_points(text: &str) -> Vec<Point> {
    let numbers: Vec<f64> = text
        .split([' ', ',', '\n', '\t'])
        .filter(|s| !s.is_empty())
        .filter_map(|s| s.parse().ok())
        .collect();
    numbers
        .chunks_exact(2)
        .map(|c| Point::new(c[0], c[1]))
        .collect()
}

/// Flattens an SVG path into segments.
///
/// Supports the commands EasyEDA actually emits: `M`/`m`, `L`/`l`, `H`/`h`,
/// `V`/`v`, `A`/`a` and `Z`/`z`. Anything else ends the current subpath rather
/// than producing wrong geometry.
pub fn parse_path(path: &str) -> Vec<Seg> {
    let tokens = tokenize(path);
    let mut segs = Vec::new();
    let mut cursor = Point::new(0.0, 0.0);
    let mut subpath_start = cursor;
    let mut i = 0;

    while i < tokens.len() {
        let Token::Cmd(cmd) = tokens[i] else {
            // A bare number here means an implicit repeat of the previous
            // command, which EasyEDA does not emit; skip it rather than guess.
            i += 1;
            continue;
        };
        i += 1;
        let relative = cmd.is_ascii_lowercase();
        let take = |i: &mut usize| -> Option<f64> {
            match tokens.get(*i) {
                Some(Token::Num(n)) => {
                    *i += 1;
                    Some(*n)
                }
                _ => None,
            }
        };

        match cmd.to_ascii_uppercase() {
            'M' => {
                let (Some(x), Some(y)) = (take(&mut i), take(&mut i)) else {
                    break;
                };
                cursor = offset(cursor, x, y, relative);
                subpath_start = cursor;
                // Extra coordinate pairs after a moveto are implicit linetos.
                while let (Some(x), Some(y)) = (take(&mut i), take(&mut i)) {
                    let to = offset(cursor, x, y, relative);
                    segs.push(Seg::Line { from: cursor, to });
                    cursor = to;
                }
            }
            'L' => {
                while let (Some(x), Some(y)) = (take(&mut i), take(&mut i)) {
                    let to = offset(cursor, x, y, relative);
                    segs.push(Seg::Line { from: cursor, to });
                    cursor = to;
                }
            }
            'H' => {
                while let Some(x) = take(&mut i) {
                    let to = Point::new(if relative { cursor.x + x } else { x }, cursor.y);
                    segs.push(Seg::Line { from: cursor, to });
                    cursor = to;
                }
            }
            'V' => {
                while let Some(y) = take(&mut i) {
                    let to = Point::new(cursor.x, if relative { cursor.y + y } else { y });
                    segs.push(Seg::Line { from: cursor, to });
                    cursor = to;
                }
            }
            'A' => {
                while let (Some(rx), Some(ry), Some(rot), Some(large), Some(sweep)) = (
                    take(&mut i),
                    take(&mut i),
                    take(&mut i),
                    take(&mut i),
                    take(&mut i),
                ) {
                    let (Some(x), Some(y)) = (take(&mut i), take(&mut i)) else {
                        break;
                    };
                    let to = offset(cursor, x, y, relative);
                    let arcs = arc_segments(cursor, to, rx, ry, rot, large != 0.0, sweep != 0.0);
                    if arcs.is_empty() {
                        // Degenerate arc (zero radius): SVG says draw a line.
                        segs.push(Seg::Line { from: cursor, to });
                    } else {
                        segs.extend(arcs);
                    }
                    cursor = to;
                }
            }
            'Z' => {
                if cursor != subpath_start {
                    segs.push(Seg::Line {
                        from: cursor,
                        to: subpath_start,
                    });
                }
                cursor = subpath_start;
            }
            _ => break,
        }
    }
    segs
}

/// The vertices a path visits, for shapes that become filled polygons.
pub fn path_points(path: &str) -> Vec<Point> {
    let segs = parse_path(path);
    let mut points = Vec::new();
    for seg in segs {
        let (from, to) = match seg {
            Seg::Line { from, to } => (from, to),
            // A filled region's arc is approximated by its chord plus midpoint;
            // good enough for the courtyard/fab outlines this is used for.
            Seg::Arc { from, mid, to } => {
                if points.last() != Some(&from) {
                    points.push(from);
                }
                points.push(mid);
                (mid, to)
            }
        };
        if points.last() != Some(&from) {
            points.push(from);
        }
        points.push(to);
    }
    points.dedup();
    points
}

fn offset(cursor: Point, x: f64, y: f64, relative: bool) -> Point {
    if relative {
        Point::new(cursor.x + x, cursor.y + y)
    } else {
        Point::new(x, y)
    }
}

#[derive(Debug, Clone, Copy)]
enum Token {
    Cmd(char),
    Num(f64),
}

fn tokenize(path: &str) -> Vec<Token> {
    let chars: Vec<char> = path.chars().collect();
    let mut tokens = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c.is_ascii_alphabetic() {
            tokens.push(Token::Cmd(c));
            i += 1;
        } else if c.is_ascii_digit() || c == '-' || c == '+' || c == '.' {
            let start = i;
            i += 1;
            while i < chars.len() {
                let d = chars[i];
                // A '-' only continues the number as an exponent sign.
                if d.is_ascii_digit() || d == '.' {
                    i += 1;
                } else if (d == 'e' || d == 'E') && i + 1 < chars.len() {
                    i += 1;
                    if chars[i] == '-' || chars[i] == '+' {
                        i += 1;
                    }
                } else {
                    break;
                }
            }
            let text: String = chars[start..i].iter().collect();
            if let Ok(n) = text.parse::<f64>() {
                tokens.push(Token::Num(n));
            }
        } else {
            i += 1;
        }
    }
    tokens
}

/// One SVG elliptical arc as one or two KiCad three-point arcs.
///
/// A sweep wider than a half turn is split in two: KiCad infers the direction
/// from the midpoint, so a nearly complete circle expressed as a single arc
/// would collapse — its start and end almost coincide.
fn arc_segments(
    from: Point,
    to: Point,
    rx: f64,
    ry: f64,
    rotation_deg: f64,
    large_arc: bool,
    sweep: bool,
) -> Vec<Seg> {
    let Some(arc) = centre_form(from, to, rx, ry, rotation_deg, large_arc, sweep) else {
        return Vec::new();
    };
    let at = |fraction: f64| arc.point(arc.theta + arc.delta * fraction);

    if arc.delta.abs() <= std::f64::consts::PI {
        vec![Seg::Arc {
            from,
            mid: at(0.5),
            to,
        }]
    } else {
        let middle = at(0.5);
        vec![
            Seg::Arc {
                from,
                mid: at(0.25),
                to: middle,
            },
            Seg::Arc {
                from: middle,
                mid: at(0.75),
                to,
            },
        ]
    }
}

/// An arc in centre form, as the SVG specification defines it.
struct Arc {
    centre: Point,
    rx: f64,
    ry: f64,
    phi: f64,
    theta: f64,
    delta: f64,
}

impl Arc {
    fn point(&self, theta: f64) -> Point {
        let (sin_phi, cos_phi) = self.phi.sin_cos();
        let (sin_t, cos_t) = theta.sin_cos();
        Point::new(
            self.centre.x + self.rx * cos_phi * cos_t - self.ry * sin_phi * sin_t,
            self.centre.y + self.rx * sin_phi * cos_t + self.ry * cos_phi * sin_t,
        )
    }
}

/// SVG endpoint-to-centre arc conversion (SVG 1.1 appendix F.6.5).
fn centre_form(
    from: Point,
    to: Point,
    rx: f64,
    ry: f64,
    rotation_deg: f64,
    large_arc: bool,
    sweep: bool,
) -> Option<Arc> {
    let (mut rx, mut ry) = (rx.abs(), ry.abs());
    if rx == 0.0 || ry == 0.0 || (from.x == to.x && from.y == to.y) {
        return None;
    }
    let phi = rotation_deg.to_radians();
    let (sin_phi, cos_phi) = phi.sin_cos();

    let dx = (from.x - to.x) / 2.0;
    let dy = (from.y - to.y) / 2.0;
    let x1p = cos_phi * dx + sin_phi * dy;
    let y1p = -sin_phi * dx + cos_phi * dy;

    // Enlarge the radii if they are too small to span the two endpoints.
    let lambda = (x1p * x1p) / (rx * rx) + (y1p * y1p) / (ry * ry);
    if lambda > 1.0 {
        let scale = lambda.sqrt();
        rx *= scale;
        ry *= scale;
    }

    let numerator = rx * rx * ry * ry - rx * rx * y1p * y1p - ry * ry * x1p * x1p;
    let denominator = rx * rx * y1p * y1p + ry * ry * x1p * x1p;
    if denominator == 0.0 {
        return None;
    }
    let sign = if large_arc == sweep { -1.0 } else { 1.0 };
    let coefficient = sign * (numerator / denominator).max(0.0).sqrt();

    let cxp = coefficient * rx * y1p / ry;
    let cyp = -coefficient * ry * x1p / rx;
    let cx = cos_phi * cxp - sin_phi * cyp + (from.x + to.x) / 2.0;
    let cy = sin_phi * cxp + cos_phi * cyp + (from.y + to.y) / 2.0;

    let start_vec = ((x1p - cxp) / rx, (y1p - cyp) / ry);
    let end_vec = ((-x1p - cxp) / rx, (-y1p - cyp) / ry);
    let theta = angle((1.0, 0.0), start_vec);
    let mut delta = angle(start_vec, end_vec) % (2.0 * std::f64::consts::PI);
    if !sweep && delta > 0.0 {
        delta -= 2.0 * std::f64::consts::PI;
    } else if sweep && delta < 0.0 {
        delta += 2.0 * std::f64::consts::PI;
    }

    Some(Arc {
        centre: Point::new(cx, cy),
        rx,
        ry,
        phi,
        theta,
        delta,
    })
}

fn angle(u: (f64, f64), v: (f64, f64)) -> f64 {
    let dot = u.0 * v.0 + u.1 * v.1;
    let len = (u.0 * u.0 + u.1 * u.1).sqrt() * (v.0 * v.0 + v.1 * v.1).sqrt();
    if len == 0.0 {
        return 0.0;
    }
    let sign = if u.0 * v.1 - u.1 * v.0 < 0.0 {
        -1.0
    } else {
        1.0
    };
    sign * (dot / len).clamp(-1.0, 1.0).acos()
}
