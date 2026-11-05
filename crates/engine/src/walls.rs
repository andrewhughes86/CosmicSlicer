//! Walls: loops inset from each island's outline, and the area left inside them for infill.
//!
//! Parts too thin for a wall loop get one line down their middle at their own width (thin
//! walls), and the narrow gaps a loop cannot reach between walls are filled the same way: what
//! OrcaSlicer's Arachne generator does with variable-width walls, and its classic generator with
//! thin-wall detection and gap fill.

use crate::clipper::Paths;
use crate::geom::{self, Join, Op, Shape};
use crate::medial;
use crate::settings::Settings;
use crate::toolpath::{Path, Role};

/// One island's walls (outermost first) and the region inside them.
pub struct Walled {
    pub walls: Vec<Vec<Path>>,
    /// Lines filling narrow gaps between walls, printed after them.
    pub gaps: Vec<Path>,
    /// Where infill goes, already grown into the innermost wall by the overlap.
    pub infill_area: Vec<Shape>,
    /// The inner edge of the innermost wall, without overlap (for ironing and checks).
    pub inner_edge: Vec<Shape>,
}

/// OrcaSlicer's `INSET_OVERLAP_TOLERANCE`.
const OVERLAP_TOLERANCE: f64 = 0.4;

/// The integers `PerimeterGenerator::process_classic` works in.
struct Flows {
    /// Inner walls: width and spacing.
    pw: i64,
    ps: i64,
    /// The outer wall.
    epw: i64,
    eps: i64,
    /// From the outer wall to the first inner one.
    eps2: i64,
    /// Solid infill's spacing.
    solid: i64,
    min_spacing: i64,
    ext_min_spacing: i64,
    ext_min_smaller: i64,
}

/// `Flow::rounded_rectangle_extrusion_spacing`, in Orca's floats.
fn flow_spacing(width: f32, height: f32) -> f32 {
    width - height * (1.0 - 0.25 * std::f64::consts::PI) as f32
}

/// `scale_` of a float, truncated: `Flow::scaled_width`.
fn scale_f(v: f32) -> i64 {
    (f64::from(v) / 1.0e-6) as i64
}

fn half(v: i64) -> f64 {
    v as f64 / 2.0
}

impl Flows {
    fn new(s: &Settings, h: f64, first_layer: bool) -> Self {
        let height = h as f32;
        let (w_out, w_in, w_solid) = if first_layer {
            (s.widths.first_layer, s.widths.first_layer, s.widths.first_layer)
        } else {
            (s.widths.outer, s.widths.inner, s.widths.solid)
        };
        let (w_out, w_in) = (w_out as f32, w_in as f32);
        let (s_out, s_in) = (flow_spacing(w_out, height), flow_spacing(w_in, height));
        let (pw, ps) = (scale_f(w_in), scale_f(s_in));
        let (epw, eps) = (scale_f(w_out), scale_f(s_out));
        // `scaled<coord_t>` of a float divides in floats.
        let scaled = |v: f32| (v / 1.0e-6_f64 as f32) as i64;
        let eps2 = if s.walls.precise_outer { scaled(0.5 * (w_out + w_in)) } else { scaled(0.5 * (s_out + s_in)) };
        let solid = scale_f(flow_spacing(w_solid as f32, height));
        Self {
            pw,
            ps,
            epw,
            eps,
            eps2,
            solid,
            min_spacing: (ps as f64 * (1.0 - OVERLAP_TOLERANCE)) as i64,
            ext_min_spacing: (eps as f64 * (1.0 - OVERLAP_TOLERANCE)) as i64,
            ext_min_smaller: (eps as f64 * (1.0 - NARROW_OVERLAP)) as i64,
        }
    }
}

/// OrcaSlicer's gap fill for classic walls: what the walls leave between them is found loop by
/// loop (`found`) and filled along its medial axis. Returns the lines and, in Clipper's units,
/// the area they cover, which infill keeps out of.
fn classic_gaps(found: &[Shape], f: &Flows, h: f64, s: &Settings, speed: f64) -> (Vec<Path>, Paths) {
    if found.is_empty() {
        return (Vec::new(), Vec::new());
    }
    let min = 0.2 * f.pw.min(f.epw) as f64 * (1.0 - OVERLAP_TOLERANCE);
    let max = 2.0 * f.ps as f64;
    let opened = geom::offset2_nm(found, -((min / 2.0) as f32), (min / 2.0) as f32, Join::Miter);
    let closed = geom::offset2_nm(found, -((max / 2.0) as f32), (max / 2.0 + 10.0) as f32, Join::Miter);
    let parts = geom::boolean(&opened, &closed, Op::Difference);
    let mut lines = Vec::new();
    let tolerance = surface_resolution(s);
    for part in &parts {
        let simple: Shape = geom::paths_of(part)
            .iter()
            .map(|c| {
                let mut closed = c.clone();
                closed.extend(c.first().copied());
                let mut p = geom::douglas_peucker(&closed, tolerance / 1.0e-6);
                p.pop();
                geom::from_nm(&p)
            })
            .filter(|c: &Vec<glam::DVec2>| c.len() >= 3)
            .collect();
        match medial::medial_axis(&simple, min * 1.0e-6, max * 1.0e-6) {
            Some(axis) => lines.extend(
                axis.into_iter()
                    .filter(|l| l.points.windows(2).map(|w| w[0].distance(w[1])).sum::<f64>() >= s.walls.filter_gap),
            ),
            None => log::warn!("gap fill: no Voronoi diagram for a region"),
        }
    }
    let paths = medial::variable_width(&lines, Role::GapFill, speed, s.walls.gap_flow, h);
    let covered = paths
        .iter()
        .flat_map(|p| {
            let delta = (p.width / 2.0 / 1.0e-6) as f32 + 10.0;
            p.points.windows(2).flat_map(move |w| geom::offset_line(w, delta))
        })
        .collect();
    (paths, covered)
}

/// `surface_simplify_resolution`, in millimetres: finer under arc fitting.
fn surface_resolution(s: &Settings) -> f64 {
    if s.motion.arc_fitting == crate::settings::ArcFitting::Off { s.resolution } else { 0.2 * s.resolution }
}

struct TopSplit {
    /// The fill on top surfaces, where inner walls give way.
    top: Vec<Shape>,
    /// What inner walls still go round.
    rest: Vec<Shape>,
    /// Keeps the top's fill inside the outer wall.
    clip: Vec<Shape>,
}

/// OrcaSlicer's `split_top_surfaces`, for "only one wall on top". `orig` is the outer wall's
/// centre line.
fn split_top_surfaces(orig: &[Shape], upper: &[Shape], lower: &[Shape], s: &Settings, f: &Flows) -> TopSplit {
    let n = s.wall_loops as i64;
    let inner_share = 0.9 * if n <= 1 { 0.0 } else { (f.ps * (n - 1)) as f64 };
    let mut offset_top = if n == 0 { 0 } else { (0.9 * ((f.epw + f.ps * (n - 1)) as f64 * 1.0e-6) / 1.0e-6) as i64 };
    offset_top = if offset_top as f64 > inner_share { offset_top - inner_share as i64 } else { 0 };
    let min_width = (half(f.eps) + 10.0).max(s.walls.min_top_width / 1.0e-6);

    let upper_paths = geom::offset_polygons(&geom::shape_paths(upper), min_width as f32);
    let orig_paths = geom::shape_paths(orig);
    let first_clip = geom::offset_nm(orig, -(f.eps as f32), Join::Miter);
    let checker = if lower.is_empty() {
        Vec::new()
    } else {
        let bridge_offset = (f.eps as f64).max(f.pw as f64);
        let margin = f64::from((1.0_f64 / 1.0e-6) as f32).min(f64::from((s.nozzle * 1.0 / 0.4 / 1.0e-6) as f32));
        let exposed = geom::diff(&orig_paths, &geom::shape_paths(lower), true);
        geom::offset_nm(&exposed, (1.5 * bridge_offset + margin + half(f.ps)) as f32, Join::Miter)
    };
    let no_bridge = geom::diff(&orig_paths, &geom::shape_paths(&checker), true);
    let top = geom::diff(&geom::shape_paths(&no_bridge), &upper_paths, true);
    let temp_gap = geom::diff(&geom::shape_paths(&top), &geom::shape_paths(&first_clip), false);
    let grown_top = geom::offset_nm(&top, (offset_top as f64 + min_width - half(f.eps)) as f32, Join::Miter);
    let inner_polygons = geom::diff(&orig_paths, &geom::shape_paths(&grown_top), true);
    let top = geom::diff(&geom::shape_paths(&first_clip), &geom::shape_paths(&inner_polygons), true);
    let clip = geom::offset_nm(orig, (half(f.eps) - s.widths.sparse / 2.0 / 1.0e-6) as f32, Join::Miter);
    let rest = geom::boolean(&inner_polygons, orig, Op::Intersect);
    let rest = if s.speeds.gap > 0.0 {
        let both: Vec<Shape> = rest.iter().chain(&temp_gap).cloned().collect();
        geom::union(&both)
    } else {
        rest
    };
    TopSplit { top, rest, clip }
}

/// The parts of `region` narrower than `width`, less features narrower than `min_feature`.
pub fn narrow_parts(region: &[Shape], width: f64, min_feature: f64) -> Vec<Shape> {
    if region.is_empty() {
        return Vec::new();
    }
    narrow_parts_eroded(region, &geom::offset(region, -width / 2.0, Join::Miter), width, min_feature)
}

/// [`narrow_parts`] given `eroded`, the region already offset in by half the width (the
/// walls have it: their centre line), which saves an offset.
fn narrow_parts_eroded(region: &[Shape], eroded: &[Shape], width: f64, min_feature: f64) -> Vec<Shape> {
    let opened = geom::offset(eroded, width / 2.0, Join::Miter);
    // The opening lies inside the region, so the narrow parts' total area is at most what it
    // removed: under the sliver threshold (with a margin for rounding), every piece would be
    // dropped below. Most regions end here, saving a boolean and two offsets (the bulk of
    // the walls' time).
    if geom::area(region) - geom::area(&opened) <= min_feature * min_feature * 0.5 {
        return Vec::new();
    }
    let thin = geom::boolean(region, &opened, Op::Difference);
    let thin = geom::offset(&geom::offset(&thin, -min_feature / 2.0, Join::Miter), min_feature / 2.0, Join::Miter);
    geom::without_slivers(thin, min_feature * min_feature)
}

/// Centre lines of narrow `parts`, as paths of near-constant width.
pub(crate) fn fill_narrow(
    parts: &[Shape],
    s: &Settings,
    max_w: f64,
    role: Role,
    speed: f64,
    flow: f64,
    min_len: f64,
) -> Vec<Path> {
    let mut out = Vec::new();
    // One part at a time keeps each raster small.
    for part in parts {
        let lines = medial::centre_lines(std::slice::from_ref(part), s.walls.min_bead, max_w, min_len);
        out.extend(medial::to_paths(&lines, role, speed, flow, min_len));
    }
    out
}

/// Orders a shape's loops for printing: the outer contour in the wall direction, holes the
/// other way.
fn oriented(shape: &Shape, ccw: bool) -> Vec<Vec<glam::DVec2>> {
    shape
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let want_positive = (i == 0) == ccw;
            let mut c = c.clone();
            if (geom::signed_area2(&c) > 0.0) != want_positive {
                c.reverse();
            }
            c
        })
        .collect()
}

/// OrcaSlicer's narrow outer loops: islands under `wo + spacing × (1 - this)` across get a
/// loop narrowed by half this share of the spacing...
const NARROW_OVERLAP: f64 = 0.22;
/// ...when their area is under that width times this many millimetres.
const NARROW_LOOP_LENGTH: f64 = 10.0;

/// Walls for one island at layer height `h`. `loops` overrides the wall count (top layers
/// with "only one wall on top", spiral vase). `last_layer` is the object's top layer, whose
/// infill overlaps the walls as much as the first layer's. `around` is the layers above and
/// below, for "only one wall on top": on top surfaces inner walls give way to the top's fill.
#[allow(clippy::too_many_arguments)]
pub fn walls(
    island: &Shape,
    s: &Settings,
    h: f64,
    first_layer: bool,
    last_layer: bool,
    loops: usize,
    speed_scale: f64,
    around: Option<(&[Shape], &[Shape])>,
) -> Walled {
    if !s.walls.arachne {
        return classic_walls(island, s, h, first_layer, last_layer, loops, speed_scale, around);
    }
    let wo = if first_layer { s.widths.first_layer } else { s.widths.outer };
    let wi = if first_layer { s.widths.first_layer } else { s.widths.inner };
    let (so, si) = (Settings::spacing(wo, h), Settings::spacing(wi, h));
    let (outer_speed, inner_speed) = if first_layer {
        (s.speeds.first_layer, s.speeds.first_layer)
    } else {
        (s.speeds.outer * speed_scale, s.speeds.inner * speed_scale)
    };
    let loops = if first_layer && s.walls.one_wall_first_layer { loops.min(1) } else { loops };
    let source = std::slice::from_ref(island);
    let mut walls: Vec<Vec<Path>> = Vec::new();
    let mut gaps: Vec<Path> = Vec::new();
    let min_feature = s.walls.min_feature;
    // Distance from the outline to each wall's centre line.
    let mut centre = wo / 2.0;
    let mut inner_edge_at = 0.0;
    let mut complete = true;
    // What inner walls are offset from, and how far that already is from the outline.
    let inner_from: Vec<Shape> = source.to_vec();
    let inner_base = 0.0;
    for k in 0..loops {
        if k == 1 {
            // "Precise outer wall": the first inner wall sits a full width in, so the outer
            // wall is not squashed outwards and the part keeps its size (Arachne lays its
            // beads at their spacing).
            let outer = if s.walls.precise_outer { wo } else { (wo + so) / 2.0 };
            centre = outer + si / 2.0;
        } else if k > 1 {
            centre += si;
        }
        let width = if k == 0 { wo } else { wi };
        // What this wall has to cover: from its outer edge inwards.
        let edge = (centre - width / 2.0).max(0.0);
        let available = if edge > 0.0 {
            geom::offset(&inner_from, -(edge - inner_base).max(0.0), Join::Miter)
        } else {
            source.to_vec()
        };
        // The centre line: `available` offset in by half the width.
        let centred = geom::offset(&inner_from, -(centre - inner_base), Join::Miter);
        let rings = geom::without_slivers(centred.clone(), wo * wo * 0.25);
        // Where this wall cannot reach: thin walls at the outside, gaps further in.
        let narrow = if rings.is_empty() {
            available.clone()
        } else {
            narrow_parts_eroded(&available, &centred, width, min_feature)
        };
        if !narrow.is_empty() {
            if k == 0 {
                if s.walls.thin_walls {
                    let thin = fill_narrow(&narrow, s, wo * 1.5, Role::OuterWall, outer_speed, 1.0, wo);
                    if !thin.is_empty() {
                        walls.push(Vec::new());
                        if let Some(w0) = walls.first_mut() {
                            w0.extend(thin);
                        }
                    }
                }
            } else {
                // Arachne prints these as inner walls of varying width.
                gaps.extend(fill_narrow(
                    &narrow,
                    s,
                    wi * 1.5,
                    Role::InnerWall,
                    inner_speed,
                    1.0,
                    s.walls.filter_gap.max(wi),
                ));
            }
        }
        if rings.is_empty() {
            complete = false;
            break;
        }
        let role = if k == 0 { Role::OuterWall } else { Role::InnerWall };
        let speed = if k == 0 { outer_speed } else { inner_speed };
        let paths: Vec<Path> = rings
            .iter()
            .flat_map(|shape| oriented(shape, s.walls.ccw))
            .map(|c| geom::simplify_loop(&c, 0.01))
            .filter(|c| c.len() >= 3)
            .map(|points| Path::new(role, points, true, width, speed, 1.0))
            .collect();
        // Infill starts half the wall's spacing in from its centre (OrcaSlicer's).
        let spacing = if k == 0 { so } else { si };
        inner_edge_at = centre + spacing / 2.0;
        // Thin walls found at this depth went in first; the loops join them.
        if let Some(level) = walls.get_mut(k) {
            level.splice(0..0, paths);
        } else {
            walls.push(paths);
        }
    }
    let has_loops = walls.iter().flatten().any(|p| p.closed);
    let (inner_edge, infill_area) = if !has_loops {
        // No room for walls (or none wanted): the whole island is infill unless walls were
        // wanted, when thin walls took what there was.
        (vec![island.clone()], if loops == 0 { vec![island.clone()] } else { Vec::new() })
    } else if !complete {
        let edge = geom::offset(source, -inner_edge_at, Join::Miter);
        (edge, Vec::new())
    } else {
        let s_last = if walls.len() == 1 { so } else { si };
        let edge = geom::offset(&inner_from, -(inner_edge_at - inner_base), Join::Miter);
        // OrcaSlicer's overlap: the share of the last wall's spacing.
        let share = if first_layer || last_layer { s.solid_wall_overlap } else { s.infill_wall_overlap };
        let mut area = geom::offset(&inner_from, -(inner_edge_at - share * s_last - inner_base), Join::Miter);
        // Strips inside the last wall too narrow for infill go to a line of their own.
        let narrow = narrow_parts(&edge, wi, min_feature);
        if !narrow.is_empty() {
            gaps.extend(fill_narrow(
                &narrow,
                s,
                wi * 1.5,
                Role::InnerWall,
                inner_speed,
                1.0,
                s.walls.filter_gap.max(wi),
            ));
            area = geom::boolean(&area, &geom::offset(&narrow, wi * 0.25, Join::Miter), Op::Difference);
        }
        (edge, area)
    };
    Walled { walls, gaps, infill_area: geom::without_slivers(infill_area, wi * wi), inner_edge }
}

/// A share of `ratio_over`: `ConfigOptionFloatOrPercent::get_abs_value` of a percentage.
fn percent_of(share: f64, ratio_over: f64) -> f64 {
    ratio_over * ((share * 100.0 * 1.0e6).round() / 1.0e6) / 100.0
}

/// `PerimeterGenerator::process_classic`, in its integers: each loop is the one before offset
/// in, and what that leaves behind is the gaps.
#[allow(clippy::too_many_arguments)]
fn classic_walls(
    island: &Shape,
    s: &Settings,
    h: f64,
    first_layer: bool,
    last_layer: bool,
    loops: usize,
    speed_scale: f64,
    around: Option<(&[Shape], &[Shape])>,
) -> Walled {
    let f = Flows::new(s, h, first_layer);
    let wo = if first_layer { s.widths.first_layer } else { s.widths.outer };
    let wi = if first_layer { s.widths.first_layer } else { s.widths.inner };
    let (outer_speed, inner_speed) = if first_layer {
        (s.speeds.first_layer, s.speeds.first_layer)
    } else {
        (s.speeds.outer * speed_scale, s.speeds.inner * speed_scale)
    };
    let gap_speed = if first_layer { s.speeds.first_layer } else { s.speeds.gap * speed_scale };
    let loops = if first_layer && s.walls.one_wall_first_layer { loops.min(1) } else { loops };
    let has_gap_fill = s.speeds.gap > 0.0;

    let mut last = geom::union_paths(&geom::simplify_p(island, surface_resolution(s) / 1.0e-6));
    let mut walls: Vec<Vec<Path>> = Vec::new();
    let mut gaps_found: Vec<Shape> = Vec::new();
    let mut top_fills: Vec<Shape> = Vec::new();
    let mut fill_clip: Vec<Shape> = Vec::new();
    let mut thin_lines = Vec::new();
    // Orca's `loop_number`: the last loop's index, -1 for none.
    let mut loop_number = loops as i64 - 1;
    if loops > 0 {
        let mut i = 0_i64;
        loop {
            let mut offsets: Vec<Shape> = Vec::new();
            let mut smaller: Vec<Shape> = Vec::new();
            if i == 0 {
                if s.walls.thin_walls {
                    offsets = geom::offset2_nm(
                        &last,
                        -((half(f.epw) + half(f.ext_min_spacing) - 1.0) as f32),
                        (half(f.ext_min_spacing) - 1.0) as f32,
                        Join::Miter,
                    );
                    let min_width = (s.nozzle / 3.0 / 1.0e-6) as i64;
                    let around_walls = geom::offset_loops(&offsets, (half(f.epw) as f32) + 10.0);
                    let thin = geom::diff(&geom::shape_paths(&last), &around_walls, false);
                    let d = (min_width as f64 / 2.0) as f32;
                    for part in geom::offset2_nm(&thin, -d, d, Join::Miter) {
                        let (lo, hi) = (min_width as f64 * 1.0e-6, (f.epw + f.eps2) as f64 * 1.0e-6);
                        thin_lines.extend(medial::medial_axis(&part, lo, hi).unwrap_or_default());
                    }
                } else {
                    for shape in &last {
                        let one = std::slice::from_ref(shape);
                        let inset = geom::offset2_nm(
                            one,
                            -((half(f.epw) + half(f.ext_min_smaller)) as f32),
                            half(f.ext_min_smaller) as f32,
                            Join::Miter,
                        );
                        let limit = (f.epw + f.ext_min_smaller) as f64 * (NARROW_LOOP_LENGTH / 1.0e-6);
                        if inset.is_empty() && geom::shape_area(shape) / 1.0e-12 < limit {
                            let thinner = 1.0e-6 * (f.epw as f64 - 0.5 * NARROW_OVERLAP * f.eps as f64);
                            let thinner = scale_f(thinner as f32);
                            smaller.extend(geom::offset_nm(one, -(half(thinner) as f32), Join::Miter));
                        } else {
                            offsets.extend(geom::offset_nm(one, -(half(f.epw) as f32), Join::Miter));
                        }
                    }
                }
            } else {
                let distance = if i == 1 { f.eps2 } else { f.ps };
                offsets = geom::offset2_nm(
                    &last,
                    -((distance as f64 + half(f.min_spacing) - 1.0) as f32),
                    (half(f.min_spacing) - 1.0) as f32,
                    Join::Miter,
                );
                if has_gap_fill {
                    let d = 0.5 * distance as f64;
                    gaps_found.extend(geom::diff(
                        &geom::offset_loops(&last, -(d as f32)),
                        &geom::offset_loops(&offsets, (d + 10.0) as f32),
                        false,
                    ));
                }
            }
            if offsets.is_empty() && smaller.is_empty() {
                loop_number = i - 1;
                last.clear();
                break;
            } else if i > loop_number {
                break;
            }
            let width = if i == 0 { wo } else { wi };
            let (role, speed) = if i == 0 { (Role::OuterWall, outer_speed) } else { (Role::InnerWall, inner_speed) };
            let mut paths: Vec<Path> = Vec::new();
            for (set, width) in [(&offsets, width), (&smaller, wo - 0.5 * NARROW_OVERLAP * Settings::spacing(wo, h))] {
                paths.extend(
                    set.iter()
                        .flat_map(|shape| oriented(shape, s.walls.ccw))
                        .filter(|c| c.len() >= 3)
                        .map(|points| Path::new(role, points, true, width, speed, 1.0)),
                );
            }
            walls.push(paths);
            last = offsets;
            if i == 0
                && i != loop_number
                && s.only_one_wall_top
                && let Some((upper, lower)) = around
            {
                let split = split_top_surfaces(&last, upper, lower, s, &f);
                top_fills = split.top;
                fill_clip = split.clip;
                last = split.rest;
            }
            if i == loop_number && !has_gap_fill {
                break;
            }
            i += 1;
        }
    }

    if !thin_lines.is_empty() {
        let thin = medial::variable_width(&thin_lines, Role::OuterWall, outer_speed, 1.0, h);
        if walls.is_empty() {
            walls.push(Vec::new());
        }
        if let Some(outer) = walls.first_mut() {
            outer.extend(thin);
        }
    }

    let (gap_lines, covered) = classic_gaps(&gaps_found, &f, h, s, gap_speed);
    if !covered.is_empty() {
        last = geom::diff(&geom::shape_paths(&last), &covered, false);
    }

    let inset = match loop_number {
        n if n < 0 => 0,
        0 => f.eps / 2,
        _ => f.ps / 2,
    };
    let outer_share = if first_layer || last_layer { s.solid_wall_overlap } else { s.infill_wall_overlap };
    let of = |share: f64| (percent_of(share, (inset + f.solid / 2) as f64 * 1.0e-6) / 1.0e-6) as i64;
    let overlap = if inset > 0 { of(outer_share) } else { 0 };
    let top_overlap = if inset > 0 && !(first_layer || last_layer) { of(s.solid_wall_overlap) } else { 0 };
    let edge_inset = inset;
    let inset = inset - overlap;

    let tolerance = s.resolution / 1.0e-6;
    let simple: Paths = last.iter().flat_map(|shape| geom::simplify_p(shape, tolerance)).collect();
    let not_filled = geom::union_paths(&simple);
    let least = (f.solid as f64 * (1.0 - OVERLAP_TOLERANCE)) as i64;
    let mut infill_area =
        geom::offset2_nm(&not_filled, (-(inset as f64) - half(least)) as f32, half(least) as f32, Join::Miter);
    if !top_fills.is_empty() {
        let grown = geom::offset_nm(&top_fills, (f.eps / 2) as f32, Join::Miter);
        let top = geom::boolean(&fill_clip, &grown, Op::Intersect);
        let reach = geom::offset_nm(&top, top_overlap as f32, Join::Miter);
        let both: Vec<Shape> = infill_area.iter().chain(&reach).cloned().collect();
        infill_area = geom::union(&both);
    }
    let inner_edge = if walls.iter().flatten().any(|p| p.closed) {
        geom::offset_nm(&not_filled, -(edge_inset as f32), Join::Miter)
    } else {
        vec![island.clone()]
    };
    if !walls.iter().flatten().any(|p| p.closed) && loops > 0 {
        infill_area.clear();
    }
    Walled { walls, gaps: gap_lines, infill_area, inner_edge }
}

#[cfg(test)]
mod tests {
    use super::*;
    use CosmicSlicer_config::{Config, Schema};
    use glam::DVec2;

    fn settings(generator: &str) -> Settings {
        let mut c = Config::default();
        c.set("wall_generator", generator);
        Settings::read(Schema::get(), &c)
    }

    #[test]
    fn pin_tips_get_a_thinner_loop() {
        // A 0.62 mm square (a pin's tip) is too narrow for a 0.42 mm outer wall with its
        // spacing; classic walls give it one loop of a thinner line, as OrcaSlicer does.
        let mut s = settings("classic");
        s.walls.thin_walls = false;
        let tip = geom::rectangle(DVec2::ZERO, DVec2::splat(0.62));
        let w = walls(&tip, &s, 0.2, false, false, 2, 1.0, None);
        let loops: Vec<&Path> = w.walls.iter().flatten().collect();
        assert_eq!(loops.len(), 1);
        assert!(loops[0].width < s.widths.outer && loops[0].width > 0.3, "{}", loops[0].width);
        // A 2 mm square keeps the usual width.
        let w = walls(&geom::rectangle(DVec2::ZERO, DVec2::splat(2.0)), &s, 0.2, false, false, 2, 1.0, None);
        assert!(w.walls.iter().flatten().all(|p| p.width >= s.widths.outer.min(s.widths.inner) - 1e-9));
    }

    #[test]
    fn two_walls_round_a_square() {
        let s = settings("arachne");
        let sq = geom::rectangle(DVec2::ZERO, DVec2::splat(20.0));
        let w = walls(&sq, &s, 0.2, false, false, 2, 1.0, None);
        assert_eq!(w.walls.len(), 2);
        assert_eq!(w.walls[0][0].role, Role::OuterWall);
        // The outer wall's centre is half a line in: a 19.58 mm square.
        let outer = w.walls[0][0].length();
        assert!((outer - 4.0 * (20.0 - 0.42)).abs() < 0.05, "{outer}");
        let a = geom::area(&w.infill_area);
        assert!(a > 15.0 * 15.0 && a < 19.0 * 19.0, "{a}");
        // A plain square leaves no gaps.
        assert!(w.gaps.is_empty(), "{:?}", w.gaps.len());
        // Counter-clockwise by default.
        assert!(geom::signed_area2(&w.walls[0][0].points) > 0.0);
    }

    #[test]
    fn walls_turn_the_way_asked() {
        let mut c = Config::default();
        c.set("wall_direction", "cw");
        let s = Settings::read(Schema::get(), &c);
        let ring = vec![
            vec![DVec2::ZERO, DVec2::new(20.0, 0.0), DVec2::new(20.0, 20.0), DVec2::new(0.0, 20.0)],
            vec![DVec2::new(5.0, 5.0), DVec2::new(5.0, 15.0), DVec2::new(15.0, 15.0), DVec2::new(15.0, 5.0)],
        ];
        let w = walls(&ring, &s, 0.2, false, false, 1, 1.0, None);
        let areas: Vec<f64> = w.walls[0].iter().map(|p| geom::signed_area2(&p.points)).collect();
        // The outside clockwise, the hole counter-clockwise.
        assert!(areas.iter().any(|a| *a < 0.0 && a.abs() > 300.0), "{areas:?}");
        assert!(areas.iter().any(|a| *a > 0.0 && a.abs() < 300.0), "{areas:?}");
    }

    #[test]
    fn a_thin_fin_gets_a_thin_wall() {
        // A body with a 0.3 mm fin: too thin for a loop.
        let s = settings("arachne");
        let fin = geom::rectangle(DVec2::ZERO, DVec2::new(20.0, 0.3));
        let w = walls(&fin, &s, 0.2, false, false, 2, 1.0, None);
        let thin: Vec<&Path> = w.walls.iter().flatten().filter(|p| !p.closed).collect();
        assert!(!thin.is_empty());
        let len: f64 = thin.iter().map(|p| p.length()).sum();
        assert!(len > 18.0, "{len}");
        assert!(thin.iter().all(|p| p.role == Role::OuterWall && p.width < 0.42), "{:?}", thin[0].width);
        // Classic without thin-wall detection drops it, as OrcaSlicer does.
        let none = walls(&fin, &settings("classic"), 0.2, false, false, 2, 1.0, None);
        assert!(none.walls.iter().flatten().next().is_none());
    }

    #[test]
    fn a_gap_between_walls_is_filled() {
        // 1.9 mm wide: two walls a side leave a strip in the middle no wall fits.
        let s = settings("classic");
        let bar = geom::rectangle(DVec2::ZERO, DVec2::new(30.0, 1.9));
        let w = walls(&bar, &s, 0.2, false, false, 2, 1.0, None);
        assert_eq!(w.walls.len(), 2);
        let gap: f64 = w.gaps.iter().filter(|p| p.role == Role::GapFill).map(Path::length).sum();
        assert!(gap > 20.0, "{gap}");
        assert!(w.gaps.iter().all(|p| p.width > 0.05 && p.width < 1.0));
    }

    #[test]
    fn a_thin_island_gets_what_fits() {
        let s = settings("classic");
        let sliver = geom::rectangle(DVec2::ZERO, DVec2::new(20.0, 0.6));
        let w = walls(&sliver, &s, 0.2, false, false, 3, 1.0, None);
        assert!(w.walls.len() <= 1);
        let none = geom::rectangle(DVec2::ZERO, DVec2::new(20.0, 0.1));
        assert!(walls(&none, &s, 0.2, false, false, 3, 1.0, None).walls.is_empty());
    }
}
