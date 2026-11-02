//! Skirt and brim: loops around the first layer's footprint.

use glam::DVec2;

use crate::geom::{self, Join, Op, Shape};
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

/// One object's first layer and settings, for its brim.
pub struct BrimObject<'a> {
    pub footprint: &'a [Shape],
    /// The object's settings (its overrides on the plate's).
    pub settings: &'a Settings,
    pub height: f64,
}

/// Every object's brim, each by its own settings and kept off the other objects, and the area
/// the brims cover.
pub fn brims(objects: &[BrimObject]) -> (Vec<Path>, Vec<Shape>) {
    let mut paths = Vec::new();
    let mut covered = Vec::new();
    for (k, o) in objects.iter().enumerate() {
        let (mut p, area) = brim(o.footprint, o.settings, o.height);
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
pub fn brim(footprint: &[Shape], s: &Settings, object_height: f64) -> (Vec<Path>, Vec<Shape>) {
    let w = s.widths.first_layer;
    let kind = s.brim_type.as_str();
    let width = match kind {
        "no_brim" | "painted" => 0.0,
        "auto_brim" => {
            if s.brim_width > 0.0 {
                s.brim_width
            } else {
                auto_brim_width(footprint, object_height, s.nozzle)
            }
        }
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
        });
    };
    if kind == "brim_ears" {
        let ears = ears(&outlines, s, width);
        let clear = geom::offset(&outlines, s.brim_gap, Join::Round);
        let area = geom::boolean(&ears, &clear, Op::Difference);
        let mut ring = geom::offset(&area, -w / 2.0, Join::Round);
        let mut guard = 0;
        while !ring.is_empty() && guard < 200 {
            for c in ring.iter().flatten() {
                push(c, &mut paths);
            }
            ring = geom::offset(&ring, -spacing, Join::Round);
            guard += 1;
        }
        // Inside out, as the other brims print.
        paths.reverse();
        return (paths, area);
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
        let (paths, area) = brim(&[square], &s, 10.0);
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
        assert!(brim(&[vec![round]], &s, 10.0).0.is_empty());
    }

    #[test]
    fn inner_brims_go_inside_holes() {
        let mut ring = geom::rectangle(DVec2::ZERO, DVec2::splat(30.0));
        let mut hole = geom::rectangle(DVec2::splat(10.0), DVec2::splat(20.0)).remove(0);
        hole.reverse();
        ring.push(hole);
        let inside = |kind: &str| {
            let s = settings(&[("brim_type", kind), ("brim_width", "3")]);
            let (paths, _) = brim(&[ring.clone()], &s, 10.0);
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
