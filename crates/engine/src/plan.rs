//! Per-object layer planning: walls, then each island's fill split into top surface, bottom
//! surface or bridge, internal solid and sparse infill, from the layers above and below.

use rayon::prelude::*;

use crate::geom::{self, Join, Op, Shape};
use crate::infill::{self, Kind};
use crate::overhang;
use crate::settings::{GapFill, Settings};
use crate::slicing::LayerZ;
use crate::toolpath::{Island, Path, Role};
use crate::walls;

/// What OrcaSlicer calls the regions of a layer's fill.
struct Regions {
    top: Vec<Shape>,
    bottom: Vec<Shape>,
    solid: Vec<Shape>,
    sparse: Vec<Shape>,
}

/// The intersection of `slices[from..to]`; empty when any is missing (past the top or bottom),
/// which makes everything there solid.
fn common(slices: &[Vec<Shape>], from: isize, to: isize) -> Vec<Shape> {
    let mut acc: Option<Vec<Shape>> = None;
    for k in from..to {
        let Some(s) = (k >= 0).then(|| slices.get(k as usize)).flatten() else { return Vec::new() };
        acc = Some(match acc {
            None => s.clone(),
            Some(a) => geom::boolean(&a, s, Op::Intersect),
        });
        if acc.as_ref().is_some_and(Vec::is_empty) {
            return Vec::new();
        }
    }
    acc.unwrap_or_default()
}

fn classify(area: &[Shape], slices: &[Vec<Shape>], i: usize, s: &Settings, w: f64) -> Regions {
    let i = i as isize;
    let (top_n, bottom_n) = (s.top_layers as isize, s.bottom_layers as isize);
    let sliver = w * w * 2.0;
    let above_one = if top_n > 0 { common(slices, i + 1, i + 2) } else { area.to_vec() };
    let below_one = if bottom_n > 0 { common(slices, i - 1, i) } else { area.to_vec() };
    let top = geom::without_slivers(geom::boolean(area, &above_one, Op::Difference), sliver);
    let bottom_all = geom::boolean(area, &below_one, Op::Difference);
    let bottom = geom::without_slivers(geom::boolean(&bottom_all, &top, Op::Difference), sliver);
    let solid_all = if s.density >= 0.999 {
        area.to_vec()
    } else {
        let above = if top_n > 0 { common(slices, i + 1, i + 1 + top_n) } else { area.to_vec() };
        let below = if bottom_n > 0 { common(slices, i - bottom_n, i) } else { area.to_vec() };
        let a = geom::boolean(area, &above, Op::Difference);
        let b = geom::boolean(area, &below, Op::Difference);
        geom::boolean(&a, &b, Op::Union)
    };
    // Solid shells grow a little into the sparse area so they anchor on it.
    let solid_all = geom::boolean(&geom::offset(&solid_all, w, Join::Miter), area, Op::Intersect);
    let solid = geom::boolean(&geom::boolean(&solid_all, &top, Op::Difference), &bottom, Op::Difference);
    let solid = geom::without_slivers(solid, sliver);
    let sparse = geom::without_slivers(geom::boolean(area, &solid_all, Op::Difference), sliver);
    Regions { top, bottom, solid, sparse }
}

fn paths_of(lines: Vec<infill::Line>, role: Role, width: f64, speed: f64, flow: f64) -> impl Iterator<Item = Path> {
    lines
        .into_iter()
        .filter(|l| l.points.len() >= 2)
        .map(move |l| Path::new(role, l.points, l.closed, width, speed, flow))
}

/// An island after the first pass: its walls and fill regions.
struct Walled {
    outline: Shape,
    walled: walls::Walled,
    regions: Option<Regions>,
}

/// Where gap fill goes in solid infill: the region less its narrow strips, and the gap fill
/// lines down those strips.
fn with_gap_fill(region: Vec<Shape>, s: &Settings, w: f64, wanted: bool, speed: f64) -> (Vec<Shape>, Vec<Path>) {
    if !wanted || region.is_empty() {
        return (region, Vec::new());
    }
    let narrow = walls::narrow_parts(&region, w, s.walls.min_feature);
    if narrow.is_empty() {
        return (region, Vec::new());
    }
    let gaps =
        walls::fill_narrow(&narrow, s, w * 1.5, Role::GapFill, speed, s.walls.gap_flow, s.walls.filter_gap.max(w));
    let rest = geom::boolean(&region, &narrow, Op::Difference);
    (geom::without_slivers(rest, w * w), gaps)
}

/// A bridge's width and flow: thick bridges are round threads the nozzle's width.
fn bridge_bead(s: &Settings, thick: bool, w: f64, h: f64, flow: f64) -> (f64, f64) {
    if thick {
        let d = s.nozzle;
        let round = std::f64::consts::PI * (d / 2.0).powi(2);
        (d, flow * round / crate::settings::bead_area(d, h).max(1e-6))
    } else {
        (w, flow)
    }
}

/// The islands of one object on every layer.
pub fn object(
    slices: &[Vec<Shape>],
    layers: &[LayerZ],
    s: &Settings,
    cancelled: &(dyn Fn() -> bool + Sync),
) -> Vec<Vec<Island>> {
    // Spiral vase: one wall, solid bottom, nothing else.
    let mut s = s.clone();
    if s.spiral {
        s.wall_loops = 1;
        s.top_layers = 0;
        s.density = 0.0;
    }
    let s = &s;
    // First pass: walls and fill regions of every island.
    let walled: Vec<Vec<Walled>> = (0..slices.len())
        .into_par_iter()
        .map(|i| {
            if cancelled() {
                return Vec::new();
            }
            let (Some(slice), Some(layer)) = (slices.get(i), layers.get(i)) else { return Vec::new() };
            let first = i == 0;
            // "Only one wall on top": top layers (nothing above) get just the outer wall.
            let top_layer = slices.get(i + 1).is_none_or(Vec::is_empty);
            let loops = if s.only_one_wall_top && top_layer { s.wall_loops.min(1) } else { s.wall_loops };
            slice
                .iter()
                .map(|island| {
                    let walled = walls::walls(island, s, layer.height, first, loops, 1.0);
                    let solid_w = if first { s.widths.first_layer } else { s.widths.solid };
                    let regions =
                        (!walled.infill_area.is_empty()).then(|| classify(&walled.infill_area, slices, i, s, solid_w));
                    Walled { outline: island.clone(), walled, regions }
                })
                .collect()
        })
        .collect();
    // Each layer's sparse infill, which solid infill over it has to bridge.
    let sparse: Vec<Vec<Shape>> = walled
        .iter()
        .map(|layer| layer.iter().filter_map(|w| w.regions.as_ref()).flat_map(|r| r.sparse.iter().cloned()).collect())
        .collect();
    // Second pass: paths.
    (0..walled.len())
        .into_par_iter()
        .map(|i| {
            if cancelled() {
                return Vec::new();
            }
            let (Some(layer_walled), Some(layer)) = (walled.get(i), layers.get(i)) else { return Vec::new() };
            let below = i.checked_sub(1).and_then(|b| slices.get(b));
            let sparse_below = i.checked_sub(1).and_then(|b| sparse.get(b));
            let overhangs = match below {
                Some(below) if s.walls.detect_overhang => {
                    let layer_shapes = slices.get(i).map_or(&[][..], Vec::as_slice);
                    Some(overhang::Below::new(layer_shapes, below, s.widths.outer))
                }
                _ => None,
            };
            layer_walled
                .iter()
                .map(|w| {
                    let mut walls = w.walled.walls.clone();
                    if let Some(b) = &overhangs {
                        for p in walls.iter_mut().flatten() {
                            b.mark(p);
                        }
                    }
                    let mut out = Island { outline: w.outline.clone(), walls, fills: w.walled.gaps.clone() };
                    if let Some(r) = &w.regions {
                        fills(&mut out.fills, r, s, i, layer, below, sparse_below);
                    }
                    out
                })
                .collect()
        })
        .collect()
}

/// The fill paths of one island's regions on layer `i`.
fn fills(
    out: &mut Vec<Path>,
    r: &Regions,
    s: &Settings,
    i: usize,
    layer: &LayerZ,
    below: Option<&Vec<Shape>>,
    sparse_below: Option<&Vec<Shape>>,
) {
    let first = i == 0;
    let (h, z) = (layer.height, layer.print_z);
    let solid_w = if first { s.widths.first_layer } else { s.widths.solid };
    let solid_angle = s.solid_angle;
    let (infill_speed, solid_speed, top_speed) = if first {
        (s.speeds.first_layer_infill, s.speeds.first_layer_infill, s.speeds.first_layer_infill)
    } else {
        (s.speeds.sparse, s.speeds.solid, s.speeds.top)
    };
    let gap_speed = if first { s.speeds.first_layer } else { s.speeds.gap };
    let surfaces_gap = s.walls.gap_fill != GapFill::Nowhere;
    let top_w = if first { s.widths.first_layer } else { s.widths.top };
    let (top, gaps) = with_gap_fill(r.top.clone(), s, top_w, surfaces_gap, gap_speed);
    out.extend(paths_of(
        infill::fill(&top, infill::kind_of(&s.top_pattern), top_w, 1.0, solid_angle, i, z),
        Role::TopSurface,
        top_w,
        top_speed,
        1.0,
    ));
    out.extend(gaps);
    if s.ironing && !first {
        let ironed = geom::offset(&r.top, -top_w / 2.0, Join::Miter);
        let lines = infill::fill(
            &ironed,
            Kind::Lines { dirs: 1, alternate: false, cubic: false },
            0.1,
            1.0,
            solid_angle + std::f64::consts::FRAC_PI_4,
            i,
            z,
        );
        out.extend(paths_of(lines, Role::Ironing, top_w, 30.0, 0.1 * 0.1 / top_w.max(0.1)));
    }
    if first {
        let (bottom, gaps) = with_gap_fill(r.bottom.clone(), s, solid_w, surfaces_gap, gap_speed);
        out.extend(paths_of(
            infill::fill(&bottom, infill::kind_of(&s.bottom_pattern), solid_w, 1.0, solid_angle, i, z),
            Role::BottomSurface,
            solid_w,
            solid_speed,
            1.0,
        ));
        out.extend(gaps);
    } else {
        // Over air: a bridge, at its own speed and flow, the way it is best anchored.
        let b = &s.bridges;
        let (w, flow) = bridge_bead(s, b.thick, solid_w, h, s.bridge_flow);
        let anchors = below.map_or(&[][..], Vec::as_slice);
        for part in &r.bottom {
            let part = std::slice::from_ref(part);
            let angle = b.angle.map_or_else(|| bridge_angle(part, anchors, w), f64::to_radians);
            out.extend(paths_of(
                infill::fill(part, Kind::Lines { dirs: 1, alternate: false, cubic: false }, w, b.density, angle, i, z),
                Role::Bridge,
                w,
                s.speeds.bridge,
                flow,
            ));
        }
    }
    // Solid infill over sparse infill bridges it: an internal bridge.
    let (internal, solid) = match sparse_below {
        Some(sb) if !sb.is_empty() && !r.solid.is_empty() => {
            let over = geom::without_slivers(geom::boolean(&r.solid, sb, Op::Intersect), solid_w * solid_w * 4.0);
            if over.is_empty() {
                (Vec::new(), r.solid.clone())
            } else {
                // Grown a little so the bridge lands on the solid round it.
                let over = geom::boolean(&geom::offset(&over, solid_w, Join::Miter), &r.solid, Op::Intersect);
                let rest = geom::without_slivers(geom::boolean(&r.solid, &over, Op::Difference), solid_w * solid_w);
                (over, rest)
            }
        }
        _ => (Vec::new(), r.solid.clone()),
    };
    if !internal.is_empty() {
        let b = &s.bridges;
        let (w, flow) = bridge_bead(s, b.thick_internal, solid_w, h, b.internal_flow);
        // Anchored on what is solid below: the layer less its sparse infill.
        let anchors = match (below, sparse_below) {
            (Some(bl), Some(sb)) => geom::boolean(bl, sb, Op::Difference),
            _ => Vec::new(),
        };
        for part in &internal {
            let part = std::slice::from_ref(part);
            let angle = b.internal_angle.map_or_else(|| bridge_angle(part, &anchors, w), f64::to_radians);
            out.extend(paths_of(
                infill::fill(
                    part,
                    Kind::Lines { dirs: 1, alternate: false, cubic: false },
                    w,
                    b.internal_density,
                    angle,
                    i,
                    z,
                ),
                Role::InternalBridge,
                w,
                b.internal_speed,
                flow,
            ));
        }
    }
    let (solid, gaps) = with_gap_fill(solid, s, solid_w, s.walls.gap_fill == GapFill::Everywhere, gap_speed);
    // Narrow strips of solid infill go round concentrically, quicker than short zig-zags.
    let (narrow, solid): (Vec<Shape>, Vec<Shape>) = if s.walls.narrow_solid {
        solid
            .into_iter()
            .partition(|part| geom::offset(std::slice::from_ref(part), -solid_w * 1.5, Join::Miter).is_empty())
    } else {
        (Vec::new(), solid)
    };
    out.extend(paths_of(
        infill::fill(&narrow, Kind::Concentric, solid_w, 1.0, solid_angle, i, z),
        Role::SolidInfill,
        solid_w,
        solid_speed,
        1.0,
    ));
    out.extend(paths_of(
        infill::fill(&solid, infill::kind_of(&s.solid_pattern), solid_w, 1.0, solid_angle, i, z),
        Role::SolidInfill,
        solid_w,
        solid_speed,
        1.0,
    ));
    out.extend(gaps);
    let sparse_w = if first { s.widths.first_layer } else { s.widths.sparse };
    out.extend(paths_of(
        infill::fill(&r.sparse, infill::kind_of(&s.sparse_pattern), sparse_w, s.density, s.infill_angle, i, z),
        Role::SparseInfill,
        sparse_w,
        infill_speed,
        1.0,
    ));
}

/// The direction (radians) bridge lines across `area` are best anchored at both ends on
/// `anchors`, the layer below; OrcaSlicer's bridge detector tries directions the same way.
pub fn bridge_angle(area: &[Shape], anchors: &[Shape], w: f64) -> f64 {
    let Some((lo, hi)) = geom::bounds(area) else { return 0.0 };
    // Across the short side of the bounds when nothing anchors it.
    let fallback = if (hi.x - lo.x) < (hi.y - lo.y) { 0.0 } else { std::f64::consts::FRAC_PI_2 };
    if anchors.is_empty() {
        return fallback;
    }
    let spacing = (w * 2.0).max((hi - lo).max_element() / 40.0);
    let mut best = (f64::NEG_INFINITY, fallback);
    for step in 0..36 {
        let angle = step as f64 * std::f64::consts::PI / 36.0;
        let rows = geom::clip_lines(&geom::hatch_lines(area, spacing, angle, 0.0, 400), area);
        let dir = glam::DVec2::new(angle.cos(), angle.sin());
        let (mut score, mut longest) = (0.0, 0.0f64);
        for l in &rows {
            let (Some(&a), Some(&b)) = (l.first(), l.last()) else { continue };
            let len = a.distance(b);
            longest = longest.max(len);
            let d = if (b - a).dot(dir) >= 0.0 { dir } else { -dir };
            let held = |p: glam::DVec2| geom::contains(anchors, p);
            score += match (held(a - d * w), held(b + d * w)) {
                (true, true) => len,
                (true, false) | (false, true) => -0.3 * len,
                (false, false) => -len,
            };
        }
        // Shorter spans break ties.
        let score = score - 0.01 * longest;
        if score > best.0 + 1e-9 {
            best = (score, angle);
        }
    }
    best.1
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::slicing;
    use CosmicSlicer_config::{Config, Schema};
    use CosmicSlicer_model::primitives;
    use glam::{Mat4, Vec3};

    fn never() -> bool {
        false
    }

    #[test]
    fn a_cube_has_shells_and_sparse_infill() {
        let s = Settings::read(Schema::get(), &Config::default());
        let placed = slicing::Placed::new(&primitives::cube(20.0), &Mat4::from_translation(Vec3::new(10.0, 10.0, 0.0)));
        let layers = slicing::layer_heights(placed.top(), s.first_layer_height, s.layer_height);
        let slices = slicing::slice(&placed, &layers, &never);
        let plan = object(&slices, &layers, &s, &never);
        let roles =
            |i: usize| -> Vec<Role> { plan[i].iter().flat_map(|isl| isl.fills.iter().map(|p| p.role)).collect() };
        assert!(roles(0).contains(&Role::BottomSurface));
        assert!(roles(1).contains(&Role::SolidInfill));
        assert!(roles(50).contains(&Role::SparseInfill) && !roles(50).contains(&Role::SolidInfill));
        assert!(roles(99).contains(&Role::TopSurface));
        assert!(plan[50][0].walls.len() == 2);
    }

    #[test]
    fn bridges_run_between_their_anchors() {
        // A 4 mm gap between two pillars, along X: lines should run along X.
        let pillars = vec![
            geom::rectangle(glam::DVec2::new(0.0, 0.0), glam::DVec2::new(5.0, 20.0)),
            geom::rectangle(glam::DVec2::new(9.0, 0.0), glam::DVec2::new(14.0, 20.0)),
        ];
        let gap = vec![geom::rectangle(glam::DVec2::new(5.0, 0.0), glam::DVec2::new(9.0, 20.0))];
        let a = bridge_angle(&gap, &pillars, 0.45);
        assert!(a.sin().abs() < 0.1, "{}", a.to_degrees());
        // Without anchors, across the short side.
        assert!(bridge_angle(&gap, &[], 0.45).abs() < 1e-9);
    }

    #[test]
    fn solid_infill_over_sparse_is_an_internal_bridge() {
        let s = Settings::read(Schema::get(), &Config::default());
        let placed = slicing::Placed::new(&primitives::cube(20.0), &Mat4::from_translation(Vec3::new(10.0, 10.0, 0.0)));
        let layers = slicing::layer_heights(placed.top(), s.first_layer_height, s.layer_height);
        let slices = slicing::slice(&placed, &layers, &never);
        let plan = object(&slices, &layers, &s, &never);
        let has = |i: usize, r: Role| plan[i].iter().flat_map(|isl| isl.fills.iter()).any(|p| p.role == r);
        // The first solid layer under the top skin sits on sparse infill.
        let bridged = (51..plan.len()).find(|&i| has(i, Role::InternalBridge));
        assert!(
            bridged.is_some_and(|i| has(i - 1, Role::SparseInfill) && has(i + 1, Role::SolidInfill)),
            "{bridged:?}"
        );
        assert!(!has(50, Role::InternalBridge) && !has(2, Role::InternalBridge));
    }

    #[test]
    fn a_ledge_gets_overhang_marks() {
        // A 10 mm cube with a 20 mm slab on top: the slab's walls hang over air.
        let s = Settings::read(Schema::get(), &Config::default());
        let mut slices: Vec<Vec<Shape>> = vec![vec![geom::rectangle(glam::DVec2::ZERO, glam::DVec2::splat(10.0))]; 10];
        slices.extend(vec![vec![geom::rectangle(glam::DVec2::ZERO, glam::DVec2::new(20.0, 10.0))]; 5]);
        let layers = slicing::layer_heights(3.0, 0.2, 0.2);
        let plan = object(&slices, &layers, &s, &never);
        let marked =
            |i: usize| plan[i].iter().flat_map(|isl| isl.walls.iter().flatten()).any(|p| p.overhang.contains(&5));
        assert!(marked(10) && !marked(9) && !marked(11));
        assert!(plan[10][0].fills.iter().any(|p| p.role == Role::Bridge));
    }
}
