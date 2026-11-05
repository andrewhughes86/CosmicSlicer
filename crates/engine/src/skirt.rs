//! Skirt and brim: loops around the first layer's footprint.

use glam::DVec2;

use crate::geom::{self, Join, Op, Shape};
use crate::settings::Settings;
use crate::toolpath::{Path, Role};

/// OrcaSlicer's bed adhesion coefficient and "thermal length" (mm) of a filament type: how
/// well it holds to the plate and how far it can run before it warps.
fn material(kind: &str) -> (f64, f64) {
    let k = kind.trim().to_ascii_uppercase();
    match k.as_str() {
        "TPU" | "FLEX" => (0.5, 1000.0),
        "PETG" | "PET" | "PET-CF" | "PET-GF" => (2.0, 100.0),
        "PCTG" => (2.0, 200.0),
        "PETG-CF" | "PETG-GF" => (1.0, 100.0),
        "PC" | "PC-PBT" => (1.0, 40.0),
        "PC-ABS" | "PC-CF" => (1.0, 80.0),
        _ if k.starts_with("ABS") || k.starts_with("ASA") || (k.starts_with("PA") && !k.starts_with("PAHT")) => {
            (1.0, 100.0)
        }
        _ => (1.0, 200.0),
    }
}

/// Second moments of area of `shapes` about their joint centroid (∫y² dA, ∫x² dA), and their
/// area: how hard the footprint is to tip over in each direction.
fn second_moments(shapes: &[Shape]) -> Option<(f64, f64)> {
    // Per contour: area, first moments and second moments about the origin.
    let (mut a, mut mx, mut my, mut ixx, mut iyy) = (0.0, 0.0, 0.0, 0.0, 0.0);
    for c in shapes.iter().flatten() {
        let n = c.len();
        for i in 0..n {
            let (Some(p), Some(q)) = (c.get(i), c.get((i + 1) % n)) else { continue };
            let cross = p.x * q.y - q.x * p.y;
            a += cross / 2.0;
            mx += (p.x + q.x) * cross / 6.0;
            my += (p.y + q.y) * cross / 6.0;
            ixx += (p.y * p.y + p.y * q.y + q.y * q.y) * cross / 12.0;
            iyy += (p.x * p.x + p.x * q.x + q.x * q.x) * cross / 12.0;
        }
    }
    if a.abs() < 1e-9 {
        return None;
    }
    let (cx, cy) = (mx / a, my / a);
    Some(((ixx - cy * cy * a).abs(), (iyy - cx * cx * a).abs()))
}

/// Brim width for "auto" (OrcaSlicer's): from how tall the part is against how wide its
/// footprint is in its weakest direction, how fast it prints and how well the filament holds;
/// none under 5 mm, at most 18 mm, in whole pairs of brim lines.
fn auto_brim_width(footprint: &[Shape], height: f64, max_speed: f64, material_kind: &str, spacing: f64) -> f64 {
    let Some((lo, hi)) = geom::bounds(footprint) else { return 0.0 };
    let Some((ixx, iyy)) = second_moments(footprint) else { return 0.0 };
    let (adhesion, reference) = material(material_kind);
    let size = hi - lo;
    let thermal = size.length();
    let height_to_area = (height / ixx.max(1e-9) * size.y).max(height / iyy.max(1e-9) * size.x) * height / 1920.0;
    let by_height = thermal * 8.0 / reference * height.min(30.0) / 30.0;
    let mut w = adhesion * (height_to_area * max_speed).max(by_height).min(18.0).min(1.5 * thermal);
    if w < 5.0 && w < 1.5 * thermal {
        w = 0.0;
    }
    pairs(w.min(18.0), spacing)
}

/// `w` rounded down to whole pairs of lines `spacing` apart.
fn pairs(w: f64, spacing: f64) -> f64 {
    if spacing > 0.0 { (w / spacing / 2.0).floor() * spacing * 2.0 } else { w }
}

/// One object's first layer and settings, for its brim.
pub struct BrimObject<'a> {
    pub footprint: &'a [Shape],
    /// The object's settings (its overrides on the plate's).
    pub settings: &'a Settings,
    pub height: f64,
    /// Brim ears placed by hand (centre, radius), for the "Painted" brim type.
    pub ears: &'a [(DVec2, f64)],
    /// The first layer's filament type ("PLA", "PETG", …), for the auto brim.
    pub material: &'a str,
}

/// Every object's brim, each by its own settings and kept off the other objects, and the area
/// the brims cover.
pub fn brims(objects: &[BrimObject]) -> (Vec<Path>, Vec<Shape>) {
    let mut paths = Vec::new();
    let mut covered = Vec::new();
    for (k, o) in objects.iter().enumerate() {
        let (mut p, area) = if o.settings.brim_type == "painted" {
            painted_brim(o.footprint, o.settings, o.ears)
        } else {
            brim(o.footprint, o.settings, o.height, o.material)
        };
        if p.is_empty() {
            continue;
        }
        let others: Vec<Shape> = objects
            .iter()
            .enumerate()
            .filter(|(j, _)| *j != k)
            .flat_map(|(_, b)| {
                geom::offset(b.footprint, b.settings.brim_gap + b.settings.widths.first_layer, Join::Round)
            })
            .collect();
        if !others.is_empty() && !geom::boolean(&area, &others, Op::Intersect).is_empty() {
            p = keep_off(p, &others);
        }
        paths.extend(p);
        covered.extend(area);
    }
    (paths, geom::union(&covered))
}

/// `paths` with what crosses `others` cut away.
fn keep_off(paths: Vec<Path>, others: &[Shape]) -> Vec<Path> {
    let Some((lo, hi)) = geom::bounds(&paths.iter().map(|p| vec![p.points.clone()]).collect::<Vec<Shape>>()) else {
        return paths;
    };
    let room = geom::boolean(&[geom::rectangle(lo - 1.0, hi + 1.0)], others, Op::Difference);
    let mut out = Vec::new();
    for p in paths {
        let mut line = p.points.clone();
        if p.closed {
            line.extend(p.points.first().copied());
        }
        let pieces = geom::clip_lines(&[line.clone()], &room);
        let whole =
            pieces.len() == 1 && pieces.first().is_some_and(|l| (geom::length(l) - geom::length(&line)).abs() < 1e-6);
        if whole {
            out.push(p);
            continue;
        }
        for piece in pieces.into_iter().filter(|l| geom::length(l) > p.width * 2.0) {
            out.push(Path { points: piece, closed: false, ..p.clone() });
        }
    }
    out
}

/// The brim loops around `footprint` (one object's first layer), outermost last, and the area
/// the brim covers.
pub fn brim(footprint: &[Shape], s: &Settings, object_height: f64, material: &str) -> (Vec<Path>, Vec<Shape>) {
    let w = s.widths.first_layer;
    let kind = s.brim_type.as_str();
    if kind == "auto_brim" {
        return auto_brim(footprint, s, object_height, material);
    }
    let width = match kind {
        "no_brim" | "painted" => 0.0,
        "brim_ears" if s.brim_width <= 0.0 => 3.0,
        _ => s.brim_width,
    };
    if width <= 0.0 || footprint.is_empty() {
        return (Vec::new(), Vec::new());
    }
    let spacing = Settings::spacing(w, s.first_layer_height);
    let loops = ((width / spacing).round() as usize).clamp(1, 200);
    let outlines: Vec<Shape> = footprint.iter().filter_map(|sh| sh.first().map(|outer| vec![outer.clone()])).collect();
    let mut paths = Vec::new();
    let mut covered = Vec::new();
    let push = |c: &[DVec2], paths: &mut Vec<Path>| {
        paths.push(Path {
            role: Role::Brim,
            points: geom::simplify_loop(c, 0.01),
            closed: true,
            width: w,
            speed: s.speeds.first_layer,
            flow: 1.0,
            overhang: Vec::new(),
            group: 0,
            bends: Vec::new(),
            fitted: false,
        });
    };
    if kind == "brim_ears" {
        return ear_loops(&ears(&outlines, s, width), &outlines, s);
    }
    if kind != "inner_only" {
        // The outside of each part: the usual brim.
        for k in 0..loops {
            let d = s.brim_gap + w / 2.0 + k as f64 * spacing;
            for c in geom::offset(&outlines, d, Join::Round).iter().flatten() {
                push(c, &mut paths);
            }
        }
        covered.extend(geom::offset(&outlines, s.brim_gap + loops as f64 * spacing, Join::Round));
    }
    if matches!(kind, "inner_only" | "outer_and_inner") {
        // Inside each hole, each hole as its own outline.
        let holes: Vec<Shape> = footprint
            .iter()
            .flat_map(|sh| sh.iter().skip(1))
            .map(|h| {
                let mut c = h.clone();
                if geom::signed_area2(&c) < 0.0 {
                    c.reverse();
                }
                vec![c]
            })
            .collect();
        for k in 0..loops {
            let d = s.brim_gap + w / 2.0 + k as f64 * spacing;
            for c in geom::offset(&holes, -d, Join::Round).iter().flatten() {
                push(c, &mut paths);
            }
        }
        let inner = geom::offset(&holes, -s.brim_gap, Join::Round);
        covered.extend(geom::boolean(
            &inner,
            &geom::offset(&holes, -(s.brim_gap + loops as f64 * spacing), Join::Round),
            Op::Difference,
        ));
    }
    (paths, covered)
}

/// The auto brim (OrcaSlicer's): one width for the object, then for each island of its first
/// layer some more when it is small and tall, and never more than the island is across.
fn auto_brim(footprint: &[Shape], s: &Settings, height: f64, material: &str) -> (Vec<Path>, Vec<Shape>) {
    let spacing = Settings::spacing(s.widths.first_layer, s.first_layer_height);
    let max_speed = [s.speeds.inner, s.speeds.outer, s.speeds.sparse, s.speeds.solid, s.speeds.top, s.speeds.support]
        .into_iter()
        .fold(0.0, f64::max);
    let max_speed = if max_speed > 0.0 { max_speed } else { 250.0 };
    let width = auto_brim_width(footprint, height, max_speed, material, spacing);
    let extra = pairs(5.0, spacing);
    let mut by_width: Vec<(f64, Vec<Shape>)> = Vec::new();
    for sh in footprint {
        let Some(outer) = sh.first() else { continue };
        let mut w = width;
        // A narrow island on a tall part gets some brim even when the part as a whole gets none.
        let perimeter = geom::length(outer) + outer.first().zip(outer.last()).map_or(0.0, |(a, b)| a.distance(*b));
        let thin = perimeter > 0.0 && geom::area(std::slice::from_ref(sh)) / perimeter < 1.1;
        if w < 5.0 && height > 10.0 && thin && w < spacing {
            w += extra;
        }
        if let Some((lo, hi)) = geom::bounds(std::slice::from_ref(sh)) {
            w = w.min((hi - lo).max_element());
        }
        let w = pairs(w, spacing);
        if w <= 0.0 {
            continue;
        }
        match by_width.iter_mut().find(|(bw, _)| (bw - w).abs() < 1e-9) {
            Some((_, group)) => group.push(sh.clone()),
            None => by_width.push((w, vec![sh.clone()])),
        }
    }
    let mut paths = Vec::new();
    let mut covered = Vec::new();
    for (w, group) in by_width {
        let mut one = s.clone();
        one.brim_type = "outer_only".into();
        one.brim_width = w;
        let (p, c) = brim(&group, &one, height, material);
        paths.extend(p);
        covered.extend(c);
    }
    (paths, covered)
}

/// The "Painted" brim: ears placed by hand.
pub fn painted_brim(footprint: &[Shape], s: &Settings, ears: &[(DVec2, f64)]) -> (Vec<Path>, Vec<Shape>) {
    let discs: Vec<Vec<DVec2>> = ears
        .iter()
        .filter(|(c, r)| c.is_finite() && r.is_finite() && *r > 0.0)
        .map(|(c, r)| {
            (0..32)
                .map(|i| {
                    let t = std::f64::consts::TAU * i as f64 / 32.0;
                    *c + DVec2::new(t.cos(), t.sin()) * r.min(100.0)
                })
                .collect()
        })
        .collect();
    if discs.is_empty() || footprint.is_empty() {
        return (Vec::new(), Vec::new());
    }
    let outlines: Vec<Shape> = footprint.iter().filter_map(|sh| sh.first().map(|outer| vec![outer.clone()])).collect();
    ear_loops(&geom::union_loops(&discs), &outlines, s)
}

/// Loops filling ear discs `ears`, kept the brim gap off `outlines`.
fn ear_loops(ears: &[Shape], outlines: &[Shape], s: &Settings) -> (Vec<Path>, Vec<Shape>) {
    let w = s.widths.first_layer;
    let spacing = Settings::spacing(w, s.first_layer_height);
    let clear = geom::offset(outlines, s.brim_gap, Join::Round);
    let area = geom::boolean(ears, &clear, Op::Difference);
    let mut paths = Vec::new();
    let mut ring = geom::offset(&area, -w / 2.0, Join::Round);
    let mut guard = 0;
    while !ring.is_empty() && guard < 200 {
        for c in ring.iter().flatten() {
            paths.push(Path {
                role: Role::Brim,
                points: geom::simplify_loop(c, 0.01),
                closed: true,
                width: w,
                speed: s.speeds.first_layer,
                flow: 1.0,
                overhang: Vec::new(),
                group: 0,
                bends: Vec::new(),
                fitted: false,
            });
        }
        ring = geom::offset(&ring, -spacing, Join::Round);
        guard += 1;
    }
    // Inside out, as the other brims print.
    paths.reverse();
    (paths, area)
}

/// Mouse ears: discs `width` across at each corner sharper than `brim_ears_max_angle`.
fn ears(outlines: &[Shape], s: &Settings, width: f64) -> Vec<Shape> {
    let max = s.brim_ears_max_angle.to_radians();
    let mut discs = Vec::new();
    for c in outlines.iter().filter_map(|sh| sh.first()) {
        let c = geom::simplify_loop(c, s.brim_ears_detection_length.max(0.01));
        let n = c.len();
        if n < 3 {
            continue;
        }
        let ccw = geom::signed_area2(&c) > 0.0;
        for k in 0..n {
            let (Some(a), Some(p), Some(b)) = (c.get((k + n - 1) % n), c.get(k), c.get((k + 1) % n)) else { continue };
            let (u, v) = (*a - *p, *b - *p);
            if u.length() < 1e-9 || v.length() < 1e-9 {
                continue;
            }
            // Convex corners only: the outline turns left on a counter-clockwise loop.
            let turn = (*p - *a).perp_dot(*b - *p);
            if (turn > 0.0) != ccw {
                continue;
            }
            let angle = u.angle_to(v).abs();
            if angle < max {
                discs.push(
                    (0..32)
                        .map(|i| {
                            let t = std::f64::consts::TAU * i as f64 / 32.0;
                            *p + DVec2::new(t.cos(), t.sin()) * width
                        })
                        .collect::<Vec<DVec2>>(),
                );
            }
        }
    }
    geom::union_loops(&discs)
}

/// The skirt loops around `around` (footprint and brim), for one layer.
pub fn skirt(around: &[Shape], s: &Settings, width: f64, height: f64) -> Vec<Path> {
    if s.skirt_loops == 0 || around.is_empty() {
        return Vec::new();
    }
    let spacing = Settings::spacing(width, height);
    // One loop round everything: the union of the parts' outsides.
    let outside: Vec<Shape> =
        geom::union(around).into_iter().filter_map(|sh| sh.first().map(|o| vec![o.clone()])).collect();
    let mut paths = Vec::new();
    for k in 0..s.skirt_loops {
        let d = s.skirt_distance + width / 2.0 + k as f64 * spacing;
        for c in geom::offset(&outside, d, Join::Round).iter().flatten() {
            paths.push(Path {
                role: Role::Skirt,
                points: geom::simplify_loop(c, 0.01),
                closed: true,
                width,
                speed: s.speeds.skirt,
                flow: 1.0,
                overhang: Vec::new(),
                group: 0,
                bends: Vec::new(),
                fitted: false,
            });
        }
    }
    // Innermost first, as it is printed outwards from the part.
    paths.reverse();
    paths
}

#[cfg(test)]
mod tests {
    use super::*;
    use CosmicSlicer_config::{Config, Schema};

    fn settings(pairs: &[(&str, &str)]) -> Settings {
        let mut c = Config::default();
        for (k, v) in pairs {
            c.set((*k).to_owned(), (*v).to_owned());
        }
        Settings::read(Schema::get(), &c)
    }

    #[test]
    fn mouse_ears_sit_on_sharp_corners_only() {
        let s = settings(&[("brim_type", "brim_ears"), ("brim_width", "4")]);
        let square = geom::rectangle(DVec2::ZERO, DVec2::splat(20.0));
        let (paths, area) = brim(&[square], &s, 10.0, "PLA");
        assert!(!paths.is_empty());
        // Four discs of 4 mm, three quarters of each outside the square.
        let a = geom::area(&area);
        let disc = std::f64::consts::PI * 16.0;
        assert!(a > disc * 2.0 && a < disc * 3.2, "{a}");
        let round: Vec<DVec2> = (0..64)
            .map(|i| {
                let t = std::f64::consts::TAU * i as f64 / 64.0;
                DVec2::new(t.cos(), t.sin()) * 10.0
            })
            .collect();
        assert!(brim(&[vec![round]], &s, 10.0, "PLA").0.is_empty());
    }

    #[test]
    fn auto_brims_go_to_tall_narrow_parts_only() {
        // OrcaSlicer's formula: a 20 mm cube needs none; a 5 mm post 60 tall gets one, but
        // never wider than the post itself, in whole pairs of lines.
        let s = settings(&[("brim_type", "auto_brim"), ("brim_width", "5")]);
        let cube = geom::rectangle(DVec2::ZERO, DVec2::splat(20.0));
        assert!(brim(&[cube], &s, 20.0, "PLA").0.is_empty());
        let post = geom::rectangle(DVec2::ZERO, DVec2::splat(5.0));
        let (paths, area) = brim(std::slice::from_ref(&post), &s, 60.0, "PLA");
        assert!(!paths.is_empty());
        let (lo, hi) = geom::bounds(&area).unwrap_or_default();
        let width = (hi - lo).x / 2.0 - 2.5;
        assert!(width > 3.0 && width <= 5.0 + s.brim_gap + 1e-6, "{width}");
        let spacing = Settings::spacing(s.widths.first_layer, s.first_layer_height);
        assert_eq!(paths.len() % 2, 0, "{} loops at {spacing}", paths.len());
        // A short wide part in PLA: none; the same in TPU (half the adhesion): none either.
        let slab = geom::rectangle(DVec2::ZERO, DVec2::new(100.0, 10.0));
        assert!(brim(std::slice::from_ref(&slab), &s, 5.0, "PLA").0.is_empty());
        assert!(brim(&[slab], &s, 5.0, "TPU").0.is_empty());
    }

    #[test]
    fn inner_brims_go_inside_holes() {
        let mut ring = geom::rectangle(DVec2::ZERO, DVec2::splat(30.0));
        let mut hole = geom::rectangle(DVec2::splat(10.0), DVec2::splat(20.0)).remove(0);
        hole.reverse();
        ring.push(hole);
        let inside = |kind: &str| {
            let s = settings(&[("brim_type", kind), ("brim_width", "3")]);
            let (paths, _) = brim(&[ring.clone()], &s, 10.0, "PLA");
            let pts: Vec<DVec2> = paths.iter().flat_map(|p| p.points.clone()).collect();
            let within =
                pts.iter().filter(|p| p.cmpgt(DVec2::splat(10.0)).all() && p.cmplt(DVec2::splat(20.0)).all()).count();
            (within, pts.len() - within)
        };
        let (i, o) = inside("inner_only");
        assert!(i > 0 && o == 0);
        let (i, o) = inside("outer_only");
        assert!(i == 0 && o > 0);
        let (i, o) = inside("outer_and_inner");
        assert!(i > 0 && o > 0);
    }
}
