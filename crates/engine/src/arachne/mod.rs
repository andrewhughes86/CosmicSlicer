//! A port of OrcaSlicer's Arachne (`WallToolPaths` and `SkeletalTrapezoidation`, from Cura):
//! variable-width lines that fill a shape, from its Voronoi diagram. Orca fills narrow internal
//! solid areas with it (`FillConcentricInternal`), and prints walls with it when
//! `wall_generator` is `arachne`. All in Orca's integer units (nanometres).

mod beading;
mod geo;
mod graph;
mod skeletal;
mod voronoi;

use std::panic::{AssertUnwindSafe, catch_unwind};

use crate::clipper::{ClipType, Clipper, FillType, PolyType, Pt};
use crate::geom;

pub use beading::{Make, Strategy};

/// `ExtrusionJunction`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExtrusionJunction {
    pub p: Pt,
    pub w: i64,
    pub perimeter_index: usize,
}

/// `ExtrusionLine`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ExtrusionLine {
    pub inset_idx: usize,
    pub is_odd: bool,
    pub is_closed: bool,
    pub junctions: Vec<ExtrusionJunction>,
}

/// `WallToolPathsParams`, in millimetres where Orca's are floats.
#[derive(Clone, Copy, Debug)]
pub struct Params {
    pub min_bead_width: f32,
    pub min_feature_size: f32,
    pub min_length_factor: f32,
    pub wall_transition_length: f32,
    pub wall_transition_angle: f32,
    pub wall_transition_filter_deviation: f32,
    pub wall_distribution_count: i32,
    pub is_top_or_bottom_layer: bool,
    pub wall_maximum_resolution: i64,
    pub wall_maximum_deviation: i64,
}

impl Params {
    /// What `FillConcentricInternal` asks for with a nozzle of `nozzle` mm. Orca leaves the
    /// minimum length factor and the top-or-bottom flag unset there; these are its defaults.
    pub fn concentric_internal(nozzle: f64) -> Params {
        Params {
            min_bead_width: (0.85 * nozzle) as f32,
            min_feature_size: (0.25 * nozzle) as f32,
            min_length_factor: 0.5,
            wall_transition_length: 0.4,
            wall_transition_angle: 10.0,
            wall_transition_filter_deviation: (0.25 * nozzle) as f32,
            wall_distribution_count: 1,
            is_top_or_bottom_layer: false,
            wall_maximum_resolution: 500_000,
            wall_maximum_deviation: 25_000,
        }
    }
}

/// `scaled<coord_t>` of a float: the division in single precision, truncated.
fn scaled_f(v: f32) -> i64 {
    (v / 1.0e-6f32) as i64
}

/// Orca's `offset` of polygons: `shrink` inwards, `expand` outwards.
fn offset(paths: &[Vec<Pt>], delta: f32) -> Vec<Vec<Pt>> {
    if delta < 0.0 { geom::shrink_polygons(paths, -delta) } else { geom::offset_polygons(paths, delta) }
}

fn union(paths: &[Vec<Pt>], fill: FillType, strictly_simple: bool) -> Vec<Vec<Pt>> {
    let run = || -> Option<Vec<Vec<Pt>>> {
        let mut c = Clipper::new();
        c.strictly_simple(strictly_simple);
        c.add_paths(paths, PolyType::Subject, true).ok()?;
        c.execute(ClipType::Union, fill, fill).ok()
    };
    catch_unwind(AssertUnwindSafe(run)).ok().flatten().unwrap_or_default()
}

/// `WallToolPaths::generate`: the lines of `outline` (polygons in Orca's units), by inset.
pub fn wall_toolpaths(
    outline: &[Vec<Pt>],
    bead_width_0: i64,
    bead_width_x: i64,
    inset_count: usize,
    wall_0_inset: i64,
    layer_height: f64,
    params: &Params,
) -> Vec<Vec<ExtrusionLine>> {
    let run = || generate(outline, bead_width_0, bead_width_x, inset_count, wall_0_inset, layer_height, params);
    catch_unwind(AssertUnwindSafe(run)).unwrap_or_default()
}

fn generate(
    outline: &[Vec<Pt>],
    bead_width_0: i64,
    bead_width_x: i64,
    inset_count: usize,
    wall_0_inset: i64,
    layer_height: f64,
    params: &Params,
) -> Vec<Vec<ExtrusionLine>> {
    if inset_count < 1 || bead_width_0 <= 0 || bead_width_x <= 0 {
        return Vec::new();
    }
    let smallest = params.wall_maximum_resolution;
    let allowed = params.wall_maximum_deviation;
    let epsilon = allowed / 2 - 1;
    // `Geometry::deg2rad` of a float.
    let transitioning_angle = std::f32::consts::PI * params.wall_transition_angle / 180.0f32;
    let step = 800_000;

    let mut prepared = offset(&offset(&offset(outline, -(epsilon as f32)), (epsilon * 2) as f32), -(epsilon as f32));
    simplify_polygons(&mut prepared, smallest, allowed);
    fix_self_intersections(epsilon, &mut prepared);
    remove_degenerate_verts(&mut prepared);
    remove_colinear_edges(&mut prepared, 0.005);
    fix_self_intersections(epsilon, &mut prepared);
    remove_degenerate_verts(&mut prepared);
    let small = bead_width_0 as f64 / 2.0;
    remove_small_areas(&mut prepared, small * small, false);
    let prepared = union(&prepared, FillType::NonZero, false);
    if prepared.iter().map(|p| polygon_area(p)).sum::<f64>() <= 0.0 {
        return Vec::new();
    }

    let height = layer_height as f32;
    let width_from_spacing =
        |s: i64| ((s as f32 * 1.0e-6f32) as f64 + f64::from(height) * (1.0 - 0.25 * std::f64::consts::PI)) as f32;
    let outer_width = width_from_spacing(bead_width_0);
    let inner_width = width_from_spacing(bead_width_x);
    let min_bead_width = scaled_f(params.min_bead_width);
    let min_feature_size = scaled_f(params.min_feature_size);
    let transition_length = scaled_f(params.wall_transition_length);
    let unscaled = |v: i64| v as f64 * 1.0e-6;
    let split = (2.0 * unscaled(min_bead_width) / f64::from(outer_width) - 1.0).clamp(0.01, 0.99);
    let add = (unscaled(min_bead_width) / f64::from(inner_width)).clamp(0.01, 0.99);
    let max_beads = (2 * inset_count).min(i64::MAX as usize) as i64;
    let strategy = Strategy::make(Make {
        outer: bead_width_0,
        inner: bead_width_x,
        transition_length,
        angle: transitioning_angle,
        thin_walls: true,
        min_bead_width,
        min_feature_size,
        split_middle: if split.is_nan() { 0.5 } else { split },
        add_middle: if add.is_nan() { 0.5 } else { add },
        max_beads,
        outer_offset: wall_0_inset,
        distribution: params.wall_distribution_count,
    });
    let filter_dist = scaled_f(100.0);
    let deviation = scaled_f(params.wall_transition_filter_deviation);
    let mut st = skeletal::Skeletal::new(
        &prepared,
        &strategy,
        strategy.transitioning_angle(),
        step,
        filter_dist,
        deviation,
        transition_length,
    );
    st.generate();
    let mut paths = std::mem::take(&mut st.out);
    stitch_toolpaths(&mut paths, bead_width_x);
    remove_small_lines(&mut paths, params);
    separate_out_inner_contour(&mut paths);
    let (res, dev) = (params.wall_maximum_resolution, params.wall_maximum_deviation);
    for lines in &mut paths {
        for line in lines.iter_mut() {
            simplify_line(line, res * res, dev * dev, 2_000_000);
        }
    }
    paths.retain(|p| !p.is_empty());
    paths
}

/// `Polygon::area`.
fn polygon_area(points: &[Pt]) -> f64 {
    if points.len() < 3 {
        return 0.0;
    }
    let mut a = 0.0;
    let mut p1 = points.last().copied().unwrap_or_default();
    for &p2 in points {
        a += p1.x as f64 * p2.y as f64 - p1.y as f64 * p2.x as f64;
        p1 = p2;
    }
    0.5 * a
}

// ---- Preparing the outline ------------------------------------------------------------------

/// The `simplify` of a polygon in `WallToolPaths.cpp`.
fn simplify_polygon(poly: &mut Vec<Pt>, smallest_sq: i64, allowed_sq: i64) {
    if poly.len() < 3 {
        poly.clear();
        return;
    }
    if poly.len() == 3 {
        return;
    }
    let n = poly.len();
    let at = |i: usize| poly.get(i).copied().unwrap_or_default();
    let mut out: Vec<Pt> = Vec::new();
    let mut previous = at(n - 1);
    let mut previous_previous = at(n - 2);
    let mut current = at(0);
    let five = 5_000i64;
    let mut accumulated = previous.x.wrapping_mul(current.y).wrapping_sub(previous.y.wrapping_mul(current.x));
    for i in 0..n {
        current = at(i % n);
        let next = if i + 1 < n {
            at(i + 1)
        } else if i + 1 == n && out.len() > 1 {
            out.first().copied().unwrap_or_default()
        } else {
            at((i + 1) % n)
        };
        let removed_next = current.x.wrapping_mul(next.y).wrapping_sub(current.y.wrapping_mul(next.x));
        let closing = next.x.wrapping_mul(previous.y).wrapping_sub(next.y.wrapping_mul(previous.x));
        accumulated = accumulated.wrapping_add(removed_next);
        let length2 = geo::sq(geo::sub(current, previous));
        if length2 < five * five {
            continue;
        }
        let so_far = accumulated.wrapping_add(closing);
        let base2 = geo::sq(geo::sub(next, previous));
        if base2 == 0 {
            continue;
        }
        let height2 = (so_far as f64 * so_far as f64 / base2 as f64) as i64;
        if height2 <= five * five && geo::infinite_sq(current, previous, next).0.sqrt() <= 5_000.0 {
            continue;
        }
        if length2 < smallest_sq && height2 <= allowed_sq {
            let next_length2 = geo::sq(geo::sub(current, next));
            if next_length2 > 4 * smallest_sq {
                let hit = geo::intersection_infinite(previous_previous, previous, current, next);
                match hit {
                    Some(p)
                        if geo::infinite_sq(p, previous, current).0 <= allowed_sq as f64
                            && geo::sq(geo::sub(p, previous)) <= smallest_sq
                            && geo::sq(geo::sub(p, next)) <= smallest_sq =>
                    {
                        current = p;
                        if out.pop().is_some() {
                            previous = previous_previous;
                        }
                    }
                    _ => {}
                }
            } else {
                continue;
            }
        }
        accumulated = removed_next;
        previous_previous = previous;
        previous = current;
        out.push(current);
    }
    *poly = out;
}

fn simplify_polygons(polys: &mut Vec<Vec<Pt>>, smallest: i64, allowed: i64) {
    let (s2, a2) = (smallest * smallest, allowed * allowed);
    for p in polys.iter_mut() {
        simplify_polygon(p, s2, a2);
    }
    polys.retain(|p| p.len() >= 3);
}

/// `fixSelfIntersections`: points nearer than half `epsilon` to another segment are pushed
/// off it, then Clipper's strictly simple union.
fn fix_self_intersections(epsilon: i64, polys: &mut Vec<Vec<Pt>>) {
    if epsilon < 1 {
        *polys = union(polys, FillType::EvenOdd, true);
        return;
    }
    let half = (epsilon + 1) / 2;
    let grid = grid::LineGrid::new(polys, 2_000_000);
    let move_dist = (half - 2).max(2);
    let half_sq = half * half;
    for pi in 0..polys.len() {
        let len = polys.get(pi).map_or(0, Vec::len);
        for qi in 0..len {
            let Some(&pt0) = polys.get(pi).and_then(|p| p.get(qi)) else { continue };
            let mut pt = pt0;
            for (lp, li) in grid.nearby(pt, epsilon) {
                let Some(line_poly) = polys.get(lp) else { continue };
                let n = line_poly.len().max(1);
                let next_idx = (li + 1) % n;
                if pi == lp && (qi == li || qi == next_idx) {
                    continue;
                }
                let (Some(&a), Some(&b)) = (line_poly.get(li), line_poly.get(next_idx)) else { continue };
                let (_, closest) = geo::segment_sq(pt, a, b);
                if half_sq >= geo::sq(geo::sub(pt, closest)) {
                    let other = polys.get(pi).and_then(|p| p.get((qi + 1) % len)).copied().unwrap_or_default();
                    let vec = if geo::left_of(other, a, b) > 0 { geo::sub(b, a) } else { geo::sub(a, b) };
                    let l = geo::norm(vec);
                    if l != 0 {
                        pt.x += (-vec.y * move_dist) / l;
                        pt.y += (vec.x * move_dist) / l;
                    }
                }
            }
            if let Some(slot) = polys.get_mut(pi).and_then(|p| p.get_mut(qi)) {
                *slot = pt;
            }
        }
    }
    *polys = union(polys, FillType::EvenOdd, true);
}

/// `removeDegenerateVerts`.
fn remove_degenerate_verts(polys: &mut Vec<Vec<Pt>>) {
    let degenerate = |last: Pt, now: Pt, next: Pt| {
        let (a, b) = (geo::sub(now, last), geo::sub(next, now));
        geo::dot(a, b) == -geo::norm(a).wrapping_mul(geo::norm(b))
    };
    let mut i = 0;
    while i < polys.len() {
        let Some(poly) = polys.get(i).cloned() else { break };
        let mut result: Vec<Pt> = Vec::new();
        let mut changed = false;
        for idx in 0..poly.len() {
            let last = result.last().copied().unwrap_or_else(|| poly.last().copied().unwrap_or_default());
            if idx + 1 == poly.len() && result.is_empty() {
                break;
            }
            let next = if idx + 1 == poly.len() {
                result.first().copied().unwrap_or_default()
            } else {
                poly.get(idx + 1).copied().unwrap_or_default()
            };
            let here = poly.get(idx).copied().unwrap_or_default();
            if degenerate(last, here, next) {
                changed = true;
                while result.len() > 1 {
                    let (Some(&b), Some(&a)) = (result.last(), result.get(result.len() - 2)) else { break };
                    if degenerate(a, b, next) {
                        result.pop();
                    } else {
                        break;
                    }
                }
            } else {
                result.push(here);
            }
        }
        if changed {
            if result.len() > 2 {
                if let Some(slot) = polys.get_mut(i) {
                    *slot = result;
                }
            } else {
                polys.remove(i);
                continue;
            }
        }
        i += 1;
    }
}

/// `removeSmallAreas`.
fn remove_small_areas(polys: &mut Vec<Vec<Pt>>, min_area: f64, remove_holes: bool) {
    let mut end = polys.len();
    if remove_holes {
        let mut i = 0;
        while i < end {
            if geo::area(polys.get(i).map_or(&[][..], Vec::as_slice)).abs() < min_area {
                end -= 1;
                polys.swap(i, end);
                continue;
            }
            i += 1;
        }
    } else {
        let mut small_holes: Vec<usize> = Vec::new();
        let mut i = 0;
        while i < end {
            let a = geo::area(polys.get(i).map_or(&[][..], Vec::as_slice));
            if a.abs() < min_area {
                if a >= 0.0 {
                    end -= 1;
                    if i < end {
                        polys.swap(i, end);
                        continue;
                    }
                    break;
                }
                small_holes.push(i);
            }
            i += 1;
        }
        // Orca moves the polygon at the end into its copy of the hole, not into the list, so
        // what goes is the last kept polygon, not the hole.
        let removed_start = end;
        let firsts: Vec<Pt> =
            small_holes.iter().filter_map(|&h| polys.get(h).and_then(|p| p.first()).copied()).collect();
        for first in firsts.iter().rev() {
            if (removed_start..polys.len()).any(|o| polys.get(o).is_some_and(|p| contains(p, *first))) {
                end = end.saturating_sub(1);
            }
        }
    }
    polys.truncate(end);
}

/// `Polygon::contains`: crossings of a ray to the right.
fn contains(poly: &[Pt], p: Pt) -> bool {
    let mut inside = false;
    let n = poly.len();
    for i in 0..n {
        let (Some(a), Some(b)) = (poly.get(i), poly.get((i + n - 1) % n)) else { continue };
        if (a.y > p.y) != (b.y > p.y)
            && (p.x as f64) < (b.x - a.x) as f64 * (p.y - a.y) as f64 / (b.y - a.y) as f64 + a.x as f64
        {
            inside = !inside;
        }
    }
    inside
}

/// `removeColinearEdges` of one polygon.
fn remove_colinear(poly: &mut Vec<Pt>, max_angle: f64) {
    loop {
        let mut removed = 0;
        let mut process = vec![true; poly.len()];
        let mut go = true;
        while go {
            go = false;
            let n = poly.len();
            if n <= 3 {
                return;
            }
            let mut skip = vec![false; n];
            let mut out: Vec<Pt> = Vec::new();
            let mut i = 0;
            while i < n {
                let at = |k: usize| poly.get(k).copied().unwrap_or_default();
                if !process.get(i).copied().unwrap_or(true) {
                    out.push(at(i));
                    i += 1;
                    continue;
                }
                if i == n - 1 && skip.first().copied().unwrap_or(false) {
                    if let Some(s) = skip.get_mut(out.len()) {
                        *s = true;
                    }
                    go = true;
                    out.push(at(i));
                    break;
                }
                let (prev, pt, next) = (at((i + n - 1) % n), at(i), at((i + 1) % n));
                let mut angle = geo::angle_left(prev, pt, next);
                if angle >= std::f32::consts::PI {
                    angle -= std::f32::consts::PI;
                }
                if f64::from(angle) > max_angle && f64::from(angle) < std::f64::consts::PI - max_angle {
                    out.push(pt);
                } else if i != n - 1 {
                    if let Some(s) = skip.get_mut(out.len()) {
                        *s = true;
                    }
                    go = true;
                    out.push(next);
                    i += 1;
                }
                i += 1;
            }
            removed += n - out.len();
            *poly = out;
            process = skip;
        }
        if removed == 0 {
            break;
        }
    }
}

fn remove_colinear_edges(polys: &mut Vec<Vec<Pt>>, max_angle: f64) {
    for p in polys.iter_mut() {
        remove_colinear(p, max_angle);
    }
    polys.retain(|p| p.len() >= 3);
}

// ---- After the skeleton -------------------------------------------------------------------

/// `stitchToolPaths`.
fn stitch_toolpaths(paths: &mut [Vec<ExtrusionLine>], bead_width_x: i64) {
    let stitch_distance = bead_width_x - 1;
    for lines in paths.iter_mut() {
        let (mut open, closed) = stitch::stitch(lines, stitch_distance, 10_000);
        for mut polygon in closed {
            let (Some(&first), Some(&last)) = (polygon.junctions.first(), polygon.junctions.last()) else { continue };
            let gap = geo::sub(last.p, first.p);
            if first.p != last.p && ((gap.x as f64).powi(2) + (gap.y as f64).powi(2)).sqrt() < stitch_distance as f64 {
                polygon.junctions.push(first);
            }
            polygon.is_closed = true;
            open.push(polygon);
        }
        *lines = open;
    }
}

/// `shorterThan` of a line, closing segment first.
fn shorter_than(line: &ExtrusionLine, check: i64) -> bool {
    let Some(mut p0) = line.junctions.last().map(|j| j.p) else { return true };
    let mut length = 0i64;
    for j in &line.junctions {
        length += geo::norm(geo::sub(p0, j.p));
        if length >= check {
            return false;
        }
        p0 = j.p;
    }
    true
}

/// `removeSmallLines`.
fn remove_small_lines(paths: &mut [Vec<ExtrusionLine>], params: &Params) {
    for inset in paths.iter_mut() {
        let mut i = 0;
        while i < inset.len() {
            let Some(line) = inset.get(i) else { break };
            let min_width = line.junctions.iter().map(|j| j.w).min().unwrap_or(i64::MAX);
            let limit = if params.is_top_or_bottom_layer {
                min_width / 2
            } else {
                (min_width as f32 * params.min_length_factor) as i64
            };
            if line.is_odd && !line.is_closed && shorter_than(line, limit) {
                inset.swap_remove(i);
                continue;
            }
            i += 1;
        }
    }
}

/// `separateOutInnerContour`, keeping the lines: insets of zero width go.
fn separate_out_inner_contour(paths: &mut Vec<Vec<ExtrusionLine>>) {
    paths.retain(|inset| {
        if inset.is_empty() {
            return false;
        }
        let mut contour = false;
        for line in inset {
            if let Some(j) = line.junctions.first() {
                contour = j.w == 0;
            }
        }
        !contour
    });
}

/// `ExtrusionLine::calculateExtrusionAreaDeviationError`.
fn area_deviation(a: ExtrusionJunction, b: ExtrusionJunction, c: ExtrusionJunction) -> i64 {
    let ab = geo::norm(geo::sub(b.p, a.p));
    let bc = geo::norm(geo::sub(c.p, b.p));
    let diff = (b.w - a.w).abs().max((c.w - b.w).abs());
    if diff > 1 {
        let (abw, bcw) = ((a.w + b.w) / 2, (b.w + c.w) / 2);
        let total = ab + bc;
        if total == 0 {
            return 0;
        }
        let average = (ab * abw + bc * bcw) / total;
        let ac = geo::norm(geo::sub(c.p, a.p));
        ((abw * ab + bcw * bc) - average * ac).abs()
    } else if ab > bc {
        diff * bc
    } else {
        diff * ab
    }
}

/// `ExtrusionLine::simplify`.
fn simplify_line(line: &mut ExtrusionLine, smallest_sq: i64, allowed_sq: i64, max_area_deviation: i64) {
    let closed = line.is_closed;
    let min_size = if closed { 3 } else { 2 };
    let n = line.junctions.len();
    if n <= min_size {
        return;
    }
    let j = |i: usize| {
        line.junctions.get(i).copied().unwrap_or(ExtrusionJunction { p: Pt::default(), w: 0, perimeter_index: 0 })
    };
    let mut out = vec![j(0)];
    let mut previous = j(0);
    let mut previous_previous = if closed { j(n - 2) } else { j(0) };
    let initial = j(1);
    let mut accumulated = previous.p.x.wrapping_mul(initial.p.y).wrapping_sub(previous.p.y.wrapping_mul(initial.p.x));
    let five = 5_000i64;
    let end = n - usize::from(!closed);
    for i in 1..end {
        let is_last = i + 1 == n;
        let current = if is_last { out.first().copied().unwrap_or(j(0)) } else { j(i) };
        if closed && out.len() + (n - i) <= 3 {
            out.push(current);
            continue;
        }
        let spill = closed && i + 2 >= n && i + 2 - n < out.len();
        let next = if spill { out.get(i + 2 - n).copied().unwrap_or(j(0)) } else { j(i + 1) };
        let removed_next = current.p.x.wrapping_mul(next.p.y).wrapping_sub(current.p.y.wrapping_mul(next.p.x));
        let closing = next.p.x.wrapping_mul(previous.p.y).wrapping_sub(next.p.y.wrapping_mul(previous.p.x));
        accumulated = accumulated.wrapping_add(removed_next);
        let length2 = geo::sq(geo::sub(current.p, previous.p));
        if length2 < five * five {
            continue;
        }
        let so_far = accumulated.wrapping_add(closing);
        let base2 = geo::sq(geo::sub(next.p, previous.p));
        if base2 == 0 {
            continue;
        }
        let height2 = (so_far as f64 * so_far as f64 / base2 as f64) as i64;
        let area_error = area_deviation(previous, current, next);
        if height2 <= five * five
            && geo::infinite_sq(current.p, previous.p, next.p).0.sqrt() <= 5_000.0
            && area_error <= max_area_deviation
        {
            continue;
        }
        if length2 < smallest_sq && height2 <= allowed_sq {
            let next_length2 = geo::sq(geo::sub(current.p, next.p));
            if next_length2 > 4 * smallest_sq {
                let greater = |a: Pt, b: Pt, t: i64| {
                    let (dx, dy) = ((a.x - b.x).unsigned_abs(), (a.y - b.y).unsigned_abs());
                    let t = t as u64;
                    dx > t || dy > t || dx.wrapping_mul(dx).wrapping_add(dy.wrapping_mul(dy)) > t
                };
                let hit = geo::intersection_infinite(previous_previous.p, previous.p, current.p, next.p);
                if let Some(p) = hit
                    && geo::infinite_sq(p, previous.p, current.p).0 <= allowed_sq as f64
                    && !greater(p, previous.p, smallest_sq)
                    && !greater(p, current.p, smallest_sq)
                {
                    let added = ExtrusionJunction { p, w: current.w, perimeter_index: current.perimeter_index };
                    if out.pop().is_some() {
                        previous = previous_previous;
                    }
                    accumulated = removed_next;
                    previous_previous = previous;
                    previous = added;
                    out.push(added);
                    continue;
                }
            } else {
                continue;
            }
        }
        accumulated = removed_next;
        previous_previous = previous;
        previous = current;
        out.push(current);
    }
    if closed {
        if let Some(last) = out.last().map(|j| j.p)
            && let Some(first) = out.first_mut()
        {
            first.p = last;
        }
    } else if let Some(&last) = line.junctions.last() {
        out.push(last);
    }
    line.junctions = out;
}

mod grid {
    //! `SparseLineGrid` and `SparsePointGrid`: a square grid of cells, each holding what was put
    //! in it newest first (`std::unordered_multimap`'s order for equal keys), visited row by row.

    use std::collections::HashMap;

    use crate::clipper::Pt;

    pub struct Grid<T> {
        cell: i64,
        cells: HashMap<(i64, i64), Vec<T>>,
    }

    impl<T: Copy> Grid<T> {
        pub fn new(cell: i64) -> Grid<T> {
            Grid { cell: cell.max(1), cells: HashMap::new() }
        }

        pub fn to_cell(&self, v: i64) -> i64 {
            v / self.cell
        }

        pub fn insert_at(&mut self, cell: (i64, i64), v: T) {
            self.cells.entry(cell).or_default().insert(0, v);
        }

        pub fn insert(&mut self, p: Pt, v: T) {
            let c = (self.to_cell(p.x), self.to_cell(p.y));
            self.insert_at(c, v);
        }

        /// `processNearby`: until `f` says stop.
        pub fn visit(&self, p: Pt, radius: i64, f: &mut dyn FnMut(&T) -> bool) {
            let (x0, y0) = (self.to_cell(p.x - radius), self.to_cell(p.y - radius));
            let (x1, y1) = (self.to_cell(p.x + radius), self.to_cell(p.y + radius));
            if (x1 - x0).saturating_mul(y1 - y0) > 4_000_000 {
                return;
            }
            for y in y0..=y1 {
                for x in x0..=x1 {
                    for v in self.cells.get(&(x, y)).map_or(&[][..], Vec::as_slice) {
                        if !f(v) {
                            return;
                        }
                    }
                }
            }
        }

        /// `processLineCells`: the cells a segment passes through.
        pub fn line_cells(&self, a: Pt, b: Pt) -> Vec<(i64, i64)> {
            let (mut start, mut end) = (a, b);
            if end.x < start.x {
                std::mem::swap(&mut start, &mut end);
            }
            let sign = |z: i64| if z >= 0 { 1 } else { -1 };
            let (sc, ec) = ((self.to_cell(start.x), self.to_cell(start.y)), (self.to_cell(end.x), self.to_cell(end.y)));
            let y_diff = end.y - start.y;
            let y_dir = sign(y_diff);
            let mut x_start = sc.0;
            let mut out = Vec::new();
            let mut cy = sc.1;
            while cy * y_dir <= ec.1 * y_dir && out.len() < 1_000_000 {
                let nearest_next_y = (cy + if sign(cy) == y_dir || cy == 0 { y_dir } else { 0 }) * self.cell;
                let x_end = if y_diff == 0 {
                    ec.0
                } else {
                    let area = (end.x - start.x) * (nearest_next_y - start.y);
                    let corresponding = start.x + area / y_diff;
                    let e = self.to_cell(corresponding + i64::from(corresponding < 0 && area % y_diff != 0));
                    if e < sc.0 { x_start } else { e }
                };
                let mut cx = x_start;
                while cx <= x_end {
                    out.push((cx, cy));
                    if (cx, cy) == ec {
                        return out;
                    }
                    cx += 1;
                }
                x_start = x_end;
                cy += y_dir;
            }
            out
        }
    }

    /// The outline's segments by the cells they cross, for `fixSelfIntersections`.
    pub struct LineGrid {
        grid: Grid<(usize, usize)>,
    }

    impl LineGrid {
        pub fn new(polys: &[Vec<Pt>], cell: i64) -> LineGrid {
            let mut grid = Grid::new(cell);
            for (pi, poly) in polys.iter().enumerate() {
                let n = poly.len();
                for qi in 0..n {
                    let (Some(&a), Some(&b)) = (poly.get(qi), poly.get((qi + 1) % n)) else { continue };
                    for c in grid.line_cells(a, b) {
                        grid.insert_at(c, (pi, qi));
                    }
                }
            }
            LineGrid { grid }
        }

        /// `getNearby`: every segment in the cells within `radius`, a segment once per cell.
        pub fn nearby(&self, p: Pt, radius: i64) -> Vec<(usize, usize)> {
            let mut out = Vec::new();
            self.grid.visit(p, radius, &mut |v| {
                out.push(*v);
                true
            });
            out
        }
    }
}

mod stitch {
    //! `PolylineStitcher` for extrusion lines.

    use super::grid::Grid;
    use super::{ExtrusionLine, geo};
    use crate::clipper::Pt;

    fn length(line: &ExtrusionLine) -> i64 {
        let Some(mut prev) = line.junctions.first().map(|j| j.p) else { return 0 };
        let mut len = 0;
        for j in &line.junctions {
            len += geo::norm(geo::sub(j.p, prev));
            prev = j.p;
        }
        if line.is_closed
            && let (Some(f), Some(b)) = (line.junctions.first(), line.junctions.last())
        {
            len += geo::norm(geo::sub(f.p, b.p));
        }
        len
    }

    /// The stitched open lines and the closed ones.
    pub fn stitch(lines: &[ExtrusionLine], max: i64, snap: i64) -> (Vec<ExtrusionLine>, Vec<ExtrusionLine>) {
        let (mut open, mut closed) = (Vec::new(), Vec::new());
        if lines.is_empty() {
            return (open, closed);
        }
        let point = |l: usize, p: usize| lines.get(l).and_then(|x| x.junctions.get(p)).map_or(Pt::default(), |j| j.p);
        let mut grid: Grid<(usize, usize)> = Grid::new(max);
        for (i, line) in lines.iter().enumerate() {
            let last = line.junctions.len().saturating_sub(1);
            grid.insert(point(i, 0), (i, 0));
            grid.insert(point(i, last), (i, last));
        }
        let mut processed = vec![false; lines.len()];
        for (i, line) in lines.iter().enumerate() {
            if processed.get(i).copied().unwrap_or(true) {
                continue;
            }
            if let Some(p) = processed.get_mut(i) {
                *p = true;
            }
            let mut should_close = line.is_odd;
            let mut chain = line.clone();
            let mut closing = false;
            for reverse in [false, true] {
                if reverse {
                    chain.junctions.reverse();
                }
                let mut chain_length = length(&chain);
                while let Some(from) = chain.junctions.last().map(|j| j.p) {
                    let front = chain.junctions.first().map_or(Pt::default(), |j| j.p);
                    let mut closest: Option<(usize, usize)> = None;
                    let mut closest_dist = i64::MAX;
                    grid.visit(from, max, &mut |&(l, p)| {
                        let here = point(l, p);
                        let mut dist = geo::norm(geo::sub(here, from));
                        if dist > max {
                            return true;
                        }
                        let mut is_closing = false;
                        if geo::sq(geo::sub(here, front)) < snap * snap {
                            if chain_length + dist < 3 * max || chain.junctions.len() <= 2 {
                                return true;
                            }
                            is_closing = true;
                            if !should_close {
                                dist += 10_000;
                            } else {
                                dist -= 10_000;
                            }
                        } else if processed.get(l).copied().unwrap_or(true) {
                            return true;
                        }
                        let Some(other) = lines.get(l) else { return true };
                        let would_reverse = (p != 0) != reverse;
                        if !other.is_odd && would_reverse {
                            return true;
                        }
                        if chain.is_odd != other.is_odd {
                            return true;
                        }
                        if dist < closest_dist {
                            closest_dist = dist;
                            closest = Some((l, p));
                            closing = is_closing;
                        }
                        dist >= snap
                    });
                    let Some((l, p)) = closest else { break };
                    if closing {
                        break;
                    }
                    let Some(other) = lines.get(l) else { break };
                    let segment = geo::norm(geo::sub(from, point(l, p)));
                    let old = chain.junctions.len();
                    let skip = usize::from(segment < snap);
                    if p == 0 {
                        chain.junctions.extend(other.junctions.iter().skip(skip).copied());
                    } else {
                        chain.junctions.extend(other.junctions.iter().rev().skip(skip).copied());
                    }
                    for k in old.max(1)..chain.junctions.len() {
                        if let (Some(a), Some(b)) = (chain.junctions.get(k), chain.junctions.get(k - 1)) {
                            chain_length += geo::norm(geo::sub(a.p, b.p));
                        }
                    }
                    should_close &= !other.is_odd;
                    if let Some(x) = processed.get_mut(l) {
                        *x = true;
                    }
                }
                if closing {
                    if reverse {
                        chain.junctions.reverse();
                    }
                    break;
                }
            }
            if closing {
                closed.push(chain);
            } else {
                if !line.is_odd {
                    chain.junctions.reverse();
                }
                open.push(chain);
            }
        }
        (open, closed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn square(side: i64) -> Vec<Pt> {
        vec![Pt::new(0, 0), Pt::new(side, 0), Pt::new(side, side), Pt::new(0, side)]
    }

    fn rect(w: i64, h: i64) -> Vec<Pt> {
        vec![Pt::new(0, 0), Pt::new(w, 0), Pt::new(w, h), Pt::new(0, h)]
    }

    #[test]
    fn a_square_fills_with_loops() {
        let spacing = 450_000;
        let params = Params::concentric_internal(0.4);
        let paths =
            wall_toolpaths(&[square(5_000_000)], spacing, spacing, 5_000_000 / spacing as usize + 1, 0, 0.2, &params);
        assert!(!paths.is_empty());
        let lines: usize = paths.iter().map(Vec::len).sum();
        assert!(lines >= 4, "{lines} lines");
        for line in paths.iter().flatten() {
            for j in &line.junctions {
                assert!(j.p.x >= -1 && j.p.x <= 5_000_001 && j.p.y >= -1 && j.p.y <= 5_000_001, "{j:?}");
                assert!(j.w > 0 && j.w < 2_000_000, "{j:?}");
            }
        }
    }

    #[test]
    fn a_narrow_strip_gets_one_wide_line() {
        let params = Params::concentric_internal(0.4);
        let paths = wall_toolpaths(&[rect(10_000_000, 600_000)], 450_000, 450_000, 3, 0, 0.2, &params);
        let lines: Vec<&ExtrusionLine> = paths.iter().flatten().collect();
        assert!(!lines.is_empty());
        let widest = lines.iter().flat_map(|l| &l.junctions).map(|j| j.w).max().unwrap_or(0);
        assert!(widest > 450_000 && widest <= 650_000, "{widest}");
    }

    #[test]
    fn garbage_does_not_panic() {
        let params = Params::concentric_internal(0.4);
        let cases: Vec<Vec<Vec<Pt>>> = vec![
            vec![],
            vec![vec![Pt::new(0, 0)]],
            vec![vec![Pt::new(0, 0), Pt::new(5, 5), Pt::new(0, 0), Pt::new(5, 5)]],
            vec![square(1_000_000), square(1_000_000)],
            vec![vec![Pt::new(0, 0), Pt::new(4_000_000, 4_000_000), Pt::new(4_000_000, 0), Pt::new(0, 4_000_000)]],
            vec![square(3_000_000_000)],
        ];
        for c in cases {
            let _ = wall_toolpaths(&c, 450_000, 450_000, 4, 0, 0.2, &params);
            let _ = wall_toolpaths(&c, 0, -5, 4, 0, f64::NAN, &params);
        }
    }
}
