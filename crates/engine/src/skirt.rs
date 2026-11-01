//! Skirt and brim: loops around the first layer's footprint.

use crate::geom::{self, Join, Shape};
use crate::settings::Settings;
use crate::toolpath::{Path, Role};

/// Brim width for "auto": tall, thin parts get one, squat ones none (OrcaSlicer's rule of thumb
/// on height against footprint).
fn auto_brim_width(footprint: &[Shape], height: f64, nozzle: f64) -> f64 {
    let Some((lo, hi)) = geom::bounds(footprint) else { return 0.0 };
    let short_side = (hi - lo).min_element().max(1.0e-3);
    let ratio = height / short_side;
    if ratio > 3.0 {
        (ratio * nozzle * 4.0).clamp(3.0, 10.0)
    } else if geom::area(footprint) < 100.0 {
        3.0
    } else {
        0.0
    }
}

/// The brim loops around `footprint` (the first layer of every object), outermost last, and
/// the area the brim covers.
pub fn brim(footprint: &[Shape], s: &Settings, object_height: f64) -> (Vec<Path>, Vec<Shape>) {
    let w = s.widths.first_layer;
    let width = match s.brim_type.as_str() {
        "no_brim" | "painted" | "brim_ears" => 0.0,
        "auto_brim" => {
            if s.brim_width > 0.0 {
                s.brim_width
            } else {
                auto_brim_width(footprint, object_height, s.nozzle)
            }
        }
        _ => s.brim_width,
    };
    if width <= 0.0 || footprint.is_empty() {
        return (Vec::new(), Vec::new());
    }
    let spacing = Settings::spacing(w, s.first_layer_height);
    let loops = ((width / spacing).round() as usize).clamp(1, 200);
    // Only the outside of each part: brims inside holes rarely come off cleanly.
    let outlines: Vec<Shape> = footprint.iter().filter_map(|sh| sh.first().map(|outer| vec![outer.clone()])).collect();
    let mut paths = Vec::new();
    for k in 0..loops {
        let d = s.brim_gap + w / 2.0 + k as f64 * spacing;
        for c in geom::offset(&outlines, d, Join::Round).iter().flatten() {
            paths.push(Path {
                role: Role::Brim,
                points: geom::simplify_loop(c, 0.01),
                closed: true,
                width: w,
                speed: s.speeds.first_layer,
                flow: 1.0,
            });
        }
    }
    let covered = geom::offset(&outlines, s.brim_gap + loops as f64 * spacing, Join::Round);
    (paths, covered)
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
            });
        }
    }
    // Innermost first, as it is printed outwards from the part.
    paths.reverse();
    paths
}
