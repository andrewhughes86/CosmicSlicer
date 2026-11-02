//! Per-object layer planning: walls, then each island's fill split into top surface, bottom
//! surface or bridge, internal solid and sparse infill, from the layers above and below.
//!
//! Two passes: the first finds every island's walls and fill regions; between them come the
//! things that need several layers at once (lightning trees, combined infill); the second
//! lays the paths.
//!
//! Parts with their own settings and modifiers split an island's fill into pieces, each filled
//! by its own settings (the last listed wins where they overlap); a height range sets a whole
//! layer's settings, walls included.

use std::sync::Arc;

use rayon::prelude::*;

use crate::geom::{self, Join, Op, Polyline, Shape};
use crate::infill::{self, Kind, Params};
use crate::settings::{self, FuzzyWhere, GapFill, IroningKind, Settings, VerticalShell, bead_area};
use crate::slicing::LayerZ;
use crate::toolpath::{Island, Path, Role};
use crate::{fuzzy, lightning, overhang, walls};

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

/// Splits an island's infill `area` on layer `i` into regions. `shell` is solid infill that
/// "ensure vertical shell thickness" adds on this layer.
fn classify(area: &[Shape], slices: &[Vec<Shape>], i: usize, s: &Settings, w: f64, shell: &[Shape]) -> Regions {
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
        let mut all = geom::boolean(&a, &b, Op::Union);
        if !shell.is_empty() {
            all = geom::boolean(&all, &geom::boolean(shell, area, Op::Intersect), Op::Union);
        }
        all
    };
    // Solid shells grow a little into the sparse area so they anchor on it.
    let mut solid_all = geom::boolean(&geom::offset(&solid_all, w, Join::Miter), area, Op::Intersect);
    let mut sparse = geom::without_slivers(geom::boolean(area, &solid_all, Op::Difference), sliver);
    // "Filter out small internal areas": sparse bits too small to bother with print solid.
    if s.infill.min_sparse_area > 0.0 {
        let (small, big): (Vec<Shape>, Vec<Shape>) =
            sparse.into_iter().partition(|p| geom::shape_area(p) < s.infill.min_sparse_area);
        if !small.is_empty() {
            solid_all = geom::boolean(&solid_all, &small, Op::Union);
        }
        sparse = big;
    }
    let solid = geom::boolean(&geom::boolean(&solid_all, &top, Op::Difference), &bottom, Op::Difference);
    let solid = geom::without_slivers(solid, sliver);
    Regions { top, bottom, solid, sparse }
}

/// Paths from fill lines; lines that must keep their order share a new group.
fn paths_of(lines: Vec<infill::Line>, role: Role, width: f64, speed: f64, flow: f64) -> Vec<Path> {
    let group = if lines.iter().any(|l| l.ordered) { Path::new_group() } else { 0 };
    lines
        .into_iter()
        .filter(|l| l.points.len() >= 2)
        .map(|l| {
            let mut p = Path::new(role, l.points, l.closed, width, speed, flow);
            if l.ordered {
                p.group = group;
            }
            p
        })
        .collect()
}

/// One layer's settings where they differ from the object's.
#[derive(Clone, Debug, Default)]
pub struct LayerRegions {
    /// The whole layer's settings (a height range), when not the object's.
    pub base: Option<Arc<Settings>>,
    /// Areas filled by other settings (parts, modifiers), later ones winning.
    pub pieces: Vec<(Vec<Shape>, Arc<Settings>)>,
    /// Painted seams: where they go, and where they must not.
    pub seam_enforce: Vec<Shape>,
    pub seam_block: Vec<Shape>,
    /// Painted fuzzy skin: walls here are fuzzy.
    pub fuzzy: Vec<Shape>,
}

/// One piece of an island's fill and the settings it is filled by (`None`: the layer's).
struct Piece {
    regions: Regions,
    area: Vec<Shape>,
    settings: Option<Arc<Settings>>,
}

/// An island after the first pass: its walls and fill regions.
struct Walled {
    outline: Shape,
    walled: walls::Walled,
    pieces: Vec<Piece>,
}

/// `area` split by the pieces' areas, last first; what is left over is the layer's own.
fn split_fill(
    area: &[Shape],
    pieces: &[(Vec<Shape>, Arc<Settings>)],
    w: f64,
) -> Vec<(Vec<Shape>, Option<Arc<Settings>>)> {
    let mut rest = area.to_vec();
    let mut out = Vec::new();
    for (zone, rs) in pieces.iter().rev() {
        if rest.is_empty() {
            break;
        }
        let piece = geom::without_slivers(geom::boolean(&rest, zone, Op::Intersect), w * w);
        if !piece.is_empty() {
            rest = geom::without_slivers(geom::boolean(&rest, zone, Op::Difference), w * w);
            out.push((piece, Some(Arc::clone(rs))));
        }
    }
    if !rest.is_empty() {
        out.push((rest, None));
    }
    out
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
        (d, flow * round / bead_area(d, h).max(1e-6))
    } else {
        (w, flow)
    }
}

/// How far in from the outline infill starts with all walls: where each layer's "core" is,
/// for comparing layers.
fn infill_inset(s: &Settings, h: f64) -> f64 {
    let (wo, wi) = (s.widths.outer, s.widths.inner);
    let si = Settings::spacing(wi, h);
    match s.wall_loops {
        0 => 0.0,
        1 => wo * (1.0 - s.infill_wall_overlap),
        n => wo + (n - 1) as f64 * si - s.infill_wall_overlap * wi,
    }
}

/// "Ensure vertical shell thickness": on each layer, where the layers above or below are
/// narrower (a sloped side), the walls alone would leave the shell thin there, so the infill
/// next to them is made solid.
fn vertical_shells(slices: &[Vec<Shape>], layers: &[LayerZ], s: &Settings) -> Vec<Vec<Shape>> {
    let n = slices.len();
    let reach = |count: usize| match s.infill.vertical_shell {
        VerticalShell::None => 0,
        VerticalShell::Critical => count.min(1),
        VerticalShell::Moderate => count.div_ceil(2),
        VerticalShell::All => count,
    };
    let (up, down) = (reach(s.top_layers), reach(s.bottom_layers));
    if (up == 0 && down == 0) || s.density >= 0.999 || s.spiral {
        return vec![Vec::new(); n];
    }
    let cores: Vec<Vec<Shape>> = (0..n)
        .into_par_iter()
        .map(|i| {
            let h = layers.get(i).map_or(s.layer_height, |l| l.height);
            let core = slices.get(i).map_or_else(Vec::new, |sl| geom::offset(sl, -infill_inset(s, h), Join::Miter));
            // Coarser outlines: this only looks for strips a line wide.
            core.iter()
                .map(|sh| sh.iter().map(|c| geom::simplify_loop(c, 0.02)).filter(|c| c.len() >= 3).collect::<Shape>())
                .filter(|sh: &Shape| !sh.is_empty())
                .collect()
        })
        .collect();
    let w = s.widths.solid;
    // Layers matching this one (straight sides) change nothing.
    let looks = |sh: &[Shape]| (geom::area(sh), geom::bounds(sh));
    let summary: Vec<(f64, Option<(glam::DVec2, glam::DVec2)>)> = cores.iter().map(|c| looks(c)).collect();
    let same = |a: usize, b: usize| match (summary.get(a), summary.get(b)) {
        (Some((aa, Some((alo, ahi)))), Some((ba, Some((blo, bhi))))) => {
            (aa - ba).abs() < 1e-3 && alo.distance(*blo) < 1e-3 && ahi.distance(*bhi) < 1e-3
        }
        _ => false,
    };
    (0..n)
        .into_par_iter()
        .map(|i| {
            let Some(core) = cores.get(i).filter(|c| !c.is_empty()) else { return Vec::new() };
            // The window's ends and next neighbours decide it where the side slopes one way.
            let mut others: Vec<usize> = vec![i.saturating_sub(down), i.saturating_sub(1), i + 1, i + up];
            others.retain(|&j| j != i && j < n && !same(i, j));
            others.sort_unstable();
            others.dedup();
            if others.is_empty() {
                return Vec::new();
            }
            let mut shared = core.clone();
            for j in others {
                let Some(other) = cores.get(j) else { continue };
                shared = geom::boolean(&shared, other, Op::Intersect);
                if shared.is_empty() {
                    break;
                }
            }
            let thin = geom::boolean(core, &shared, Op::Difference);
            // Only strips at least a line wide.
            let opened = geom::offset(&geom::offset(&thin, -w / 2.0, Join::Miter), w / 2.0, Join::Miter);
            geom::without_slivers(opened, w * w * 2.0)
        })
        .collect()
}

/// Combined infill on one layer: what to leave out, and what to print for several layers.
#[derive(Default)]
struct Combined {
    /// Printed on a later layer of the stack.
    skip: Vec<Shape>,
    /// Printed here for `layers` layers.
    print: Vec<Shape>,
    layers: usize,
}

/// "Infill combination": sparse infill printed once every few layers, as thick as they are
/// together, where all of them have it.
fn combine(sparse: &[Vec<Shape>], layers: &[LayerZ], s: &Settings) -> Vec<Combined> {
    let n = sparse.len();
    let mut out: Vec<Combined> = (0..n).map(|_| Combined::default()).collect();
    let Some(max_h) = s.infill.combination else { return out };
    let per = (max_h / s.layer_height + 1e-6).floor() as usize;
    if per < 2 || s.spiral {
        return out;
    }
    // Stacks of `per` layers from the second layer up.
    let mut start = 1;
    while start < n {
        let end = (start + per).min(n);
        let same_height =
            layers.get(start..end).is_some_and(|ls| ls.iter().all(|l| (l.height - s.layer_height).abs() < 1e-6));
        if end - start >= 2 && same_height {
            let mut shared: Option<Vec<Shape>> = None;
            for j in start..end {
                let here = sparse.get(j).cloned().unwrap_or_default();
                shared = Some(match shared {
                    None => here,
                    Some(acc) => geom::boolean(&acc, &here, Op::Intersect),
                });
            }
            let shared = geom::without_slivers(shared.unwrap_or_default(), s.widths.sparse.powi(2) * 4.0);
            if !shared.is_empty() {
                for j in start..end - 1 {
                    if let Some(c) = out.get_mut(j) {
                        c.skip = shared.clone();
                    }
                }
                if let Some(c) = out.get_mut(end - 1) {
                    c.skip = shared.clone();
                    c.print = shared;
                    c.layers = end - start;
                }
            }
        }
        start = end;
    }
    out
}

/// Lightning lines for every layer: trees under the solid surfaces of each layer above.
fn lightning_lines(
    walled: &[Vec<Walled>],
    sparse: &[Vec<Shape>],
    layers: &[LayerZ],
    s: &Settings,
    cancelled: &(dyn Fn() -> bool + Sync),
) -> Vec<Vec<Polyline>> {
    let n = walled.len();
    let demand: Vec<Vec<Shape>> = (0..n)
        .into_par_iter()
        .map(|i| {
            let (Some(here), Some(up)) = (sparse.get(i), walled.get(i + 1)) else { return Vec::new() };
            let solid_above: Vec<Shape> = up
                .iter()
                .flat_map(|w| &w.pieces)
                .flat_map(|p| p.regions.top.iter().chain(&p.regions.bottom).chain(&p.regions.solid).cloned())
                .collect();
            if solid_above.is_empty() || here.is_empty() {
                return Vec::new();
            }
            geom::boolean(here, &solid_above, Op::Intersect)
        })
        .collect();
    let heights: Vec<f64> = layers.iter().map(|l| l.height).collect();
    let w = s.widths.sparse;
    lightning::grow(sparse, &demand, &heights, w / s.density.max(0.01), s.infill.lightning_prune, w, cancelled)
}

/// What one layer's fills need from the layers round it.
#[derive(Clone)]
struct Around<'a> {
    i: usize,
    layer: &'a LayerZ,
    below: Option<&'a Vec<Shape>>,
    sparse_below: Option<&'a Vec<Shape>>,
    lightning: Option<&'a Vec<Polyline>>,
    combined: Option<&'a Combined>,
    /// The object's last layer with anything on it.
    topmost: bool,
}

/// The islands of one object on every layer.
pub fn object(
    slices: &[Vec<Shape>],
    layers: &[LayerZ],
    s: &Settings,
    regions: &[LayerRegions],
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
    let shells = vertical_shells(slices, layers, s);
    // First pass: walls and fill regions of every island.
    let walled: Vec<Vec<Walled>> = (0..slices.len())
        .into_par_iter()
        .map(|i| {
            if cancelled() {
                return Vec::new();
            }
            let (Some(slice), Some(layer)) = (slices.get(i), layers.get(i)) else { return Vec::new() };
            let lr = regions.get(i);
            let s = lr.and_then(|r| r.base.as_deref()).unwrap_or(s);
            let pieces = lr.map_or(&[][..], |r| r.pieces.as_slice());
            let first = i == 0;
            // "Only one wall on top": top layers (nothing above) get just the outer wall.
            let top_layer = slices.get(i + 1).is_none_or(Vec::is_empty);
            let loops = if s.only_one_wall_top && top_layer { s.wall_loops.min(1) } else { s.wall_loops };
            let shell = shells.get(i).map_or(&[][..], Vec::as_slice);
            slice
                .iter()
                .map(|island| {
                    let walled = walls::walls(island, s, layer.height, first, loops, 1.0);
                    let solid_w = if first { s.widths.first_layer } else { s.widths.solid };
                    let pieces = split_fill(&walled.infill_area, pieces, solid_w)
                        .into_iter()
                        .map(|(area, settings)| {
                            let rs = settings.as_deref().unwrap_or(s);
                            let w = if first { rs.widths.first_layer } else { rs.widths.solid };
                            let regions = classify(&area, slices, i, rs, w, shell);
                            Piece { regions, area, settings }
                        })
                        .collect();
                    Walled { outline: island.clone(), walled, pieces }
                })
                .collect()
        })
        .collect();
    // Each layer's sparse infill, which solid infill over it has to bridge.
    let sparse: Vec<Vec<Shape>> = walled
        .iter()
        .map(|layer| layer.iter().flat_map(|w| &w.pieces).flat_map(|p| p.regions.sparse.iter().cloned()).collect())
        .collect();
    let lightning = (infill::kind_of(&s.sparse_pattern) == Kind::Lightning && s.density > 0.0)
        .then(|| lightning_lines(&walled, &sparse, layers, s, cancelled));
    let combined = combine(&sparse, layers, s);
    let topmost = slices.iter().rposition(|sl| !sl.is_empty()).unwrap_or(0);
    // Second pass: paths.
    (0..walled.len())
        .into_par_iter()
        .map(|i| {
            if cancelled() {
                return Vec::new();
            }
            let (Some(layer_walled), Some(layer)) = (walled.get(i), layers.get(i)) else { return Vec::new() };
            let s = regions.get(i).and_then(|r| r.base.as_deref()).unwrap_or(s);
            let below = i.checked_sub(1).and_then(|b| slices.get(b));
            let around = Around {
                i,
                layer,
                below,
                sparse_below: i.checked_sub(1).and_then(|b| sparse.get(b)),
                lightning: lightning.as_ref().and_then(|l| l.get(i)),
                combined: combined.get(i),
                topmost: i == topmost,
            };
            let overhangs = match below {
                Some(below) if s.walls.detect_overhang => {
                    let layer_shapes = slices.get(i).map_or(&[][..], Vec::as_slice);
                    Some(overhang::Below::new(layer_shapes, below, s.widths.outer))
                }
                _ => None,
            };
            let fuzz = s.fuzzy.on != FuzzyWhere::None && (i > 0 || s.fuzzy.first_layer);
            let lr = regions.get(i);
            let painted_fuzz = lr.map_or(&[][..], |r| r.fuzzy.as_slice());
            layer_walled
                .iter()
                .map(|w| {
                    let mut walls = w.walled.walls.clone();
                    if fuzz {
                        make_fuzzy(&mut walls, s, layer.print_z, i);
                    } else if !painted_fuzz.is_empty() {
                        // Painted fuzzy skin: outer walls, inside the paint only.
                        for p in walls.iter_mut().take(1).flatten().filter(|p| p.closed) {
                            fuzzy::apply_masked(p, &s.fuzzy, layer.print_z, i, Some(painted_fuzz));
                        }
                    }
                    if let Some(b) = &overhangs {
                        for p in walls.iter_mut().flatten() {
                            b.mark(p);
                        }
                    }
                    let mut out = Island {
                        outline: w.outline.clone(),
                        walls,
                        fills: w.walled.gaps.clone(),
                        seam_enforce: lr.map(|r| r.seam_enforce.clone()).unwrap_or_default(),
                        seam_block: lr.map(|r| r.seam_block.clone()).unwrap_or_default(),
                        extruder: None,
                    };
                    for p in &w.pieces {
                        match p.settings.as_deref() {
                            None => fills(&mut out.fills, &p.regions, &p.area, s, &around),
                            // Their own settings: no lightning trees or combined infill from the
                            // object's.
                            Some(rs) => {
                                let own = Around { lightning: None, combined: None, ..around.clone() };
                                fills(&mut out.fills, &p.regions, &p.area, rs, &own);
                            }
                        }
                    }
                    out
                })
                .collect()
        })
        .collect()
}

/// Fuzzy skin on the walls it is asked for.
fn make_fuzzy(walls: &mut [Vec<Path>], s: &Settings, z: f64, layer: usize) {
    for (k, level) in walls.iter_mut().enumerate() {
        for p in level.iter_mut().filter(|p| p.closed) {
            // Outer contours run the wall direction's way round, holes the other.
            let contour = (geom::signed_area2(&p.points) > 0.0) == s.walls.ccw;
            let wanted = match s.fuzzy.on {
                FuzzyWhere::None => false,
                FuzzyWhere::External => k == 0 && contour,
                FuzzyWhere::Holes => k == 0 && !contour,
                FuzzyWhere::All => k == 0,
                FuzzyWhere::AllWalls => true,
            };
            if wanted {
                fuzzy::apply(p, &s.fuzzy, z, layer);
            }
        }
    }
}

/// "Extra perimeters on overhangs": over air, loops that grow out from the part below one
/// line at a time, each printed on the one before, instead of a bridge. `None` when nothing
/// below holds the area (then it is bridged).
fn overhang_perimeters(
    part: &[Shape],
    infill_area: &[Shape],
    below: &[Shape],
    s: &Settings,
    w: f64,
) -> Option<Vec<Path>> {
    let held = geom::boolean(infill_area, below, Op::Intersect);
    let reach = geom::offset(part, w, Join::Miter);
    if geom::boolean(&held, &reach, Op::Intersect).is_empty() {
        return None;
    }
    let group = Path::new_group();
    let mut out = Vec::new();
    for k in 0..200 {
        let grown = geom::offset(&held, (k as f64 + 0.5) * w, Join::Miter);
        let rings: Vec<Polyline> = grown
            .iter()
            .flatten()
            .map(|c| {
                let mut r = c.clone();
                if let Some(&f) = c.first() {
                    r.push(f);
                }
                r
            })
            .collect();
        let lines = geom::clip_lines(&rings, part);
        if lines.is_empty() {
            break;
        }
        for l in lines.into_iter().filter(|l| geom::length(l) > w) {
            let mut p = Path::new(Role::OverhangWall, l, false, w, s.speeds.bridge, s.bridge_flow);
            p.group = group;
            out.push(p);
        }
        let covered = geom::offset(&held, (k as f64 + 1.0) * w, Join::Miter);
        if geom::without_slivers(geom::boolean(part, &covered, Op::Difference), w * w).is_empty() {
            break;
        }
    }
    Some(out)
}

/// The fill paths of one island's regions.
fn fills(out: &mut Vec<Path>, r: &Regions, infill_area: &[Shape], s: &Settings, at: &Around<'_>) {
    let i = at.i;
    let first = i == 0;
    let (h, z) = (at.layer.height, at.layer.print_z);
    let solid_w = if first { s.widths.first_layer } else { s.widths.solid };
    let inf = &s.infill;
    // Rotation templates set each layer's angle outright; otherwise patterns turn by layer.
    let solid_angle = s.solid_angle + settings::template_angle(&inf.solid_template, i);
    let sparse_angle = s.infill_angle + settings::template_angle(&inf.sparse_template, i);
    let params = |w: f64, density: f64, angle: f64, templated: bool| {
        let mut p = Params::new(w, density, angle, i, z);
        p.turn_by_layer = !templated;
        p
    };
    let solid_params = |w: f64, density: f64| params(w, density, solid_angle, !inf.solid_template.is_empty());
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
        infill::fill_with(&top, infill::kind_of(&s.top_pattern), &solid_params(top_w, inf.top_density)),
        Role::TopSurface,
        top_w,
        top_speed,
        inf.top_flow,
    ));
    out.extend(gaps);
    let iron = &s.ironing;
    let ironed: Vec<Shape> = match iron.kind {
        _ if first => Vec::new(),
        IroningKind::None => Vec::new(),
        IroningKind::Top => r.top.clone(),
        IroningKind::Topmost if at.topmost => r.top.clone(),
        IroningKind::Topmost => Vec::new(),
        IroningKind::Solid => geom::boolean(&r.top, &r.solid, Op::Union),
    };
    if !ironed.is_empty() {
        let area = geom::offset(&ironed, -iron.inset.max(top_w / 2.0), Join::Miter);
        let angle = if iron.fixed { iron.angle } else { solid_angle + std::f64::consts::FRAC_PI_4 + iron.angle };
        let kind = if iron.concentric { Kind::Concentric } else { Kind::Rectilinear { alternate: false } };
        let mut p = Params::new(iron.spacing, 1.0, angle, i, z);
        p.turn_by_layer = false;
        // A film: the set share of a full line at the ironing spacing.
        let flow = iron.flow * h * iron.spacing / bead_area(top_w, h).max(1e-6);
        out.extend(paths_of(infill::fill_with(&area, kind, &p), Role::Ironing, top_w, iron.speed, flow));
    }
    if first {
        let (bottom, gaps) = with_gap_fill(r.bottom.clone(), s, solid_w, surfaces_gap, gap_speed);
        out.extend(paths_of(
            infill::fill_with(&bottom, infill::kind_of(&s.bottom_pattern), &solid_params(solid_w, inf.bottom_density)),
            Role::BottomSurface,
            solid_w,
            solid_speed,
            inf.bottom_flow,
        ));
        out.extend(gaps);
    } else {
        // Over air: a bridge, at its own speed and flow, the way it is best anchored; or loops
        // grown out from the part below.
        let b = &s.bridges;
        let (w, flow) = bridge_bead(s, b.thick, solid_w, h, s.bridge_flow);
        let anchors = at.below.map_or(&[][..], Vec::as_slice);
        for part in &r.bottom {
            let part = std::slice::from_ref(part);
            if inf.extra_overhang_perimeters
                && let Some(loops) = overhang_perimeters(part, infill_area, anchors, s, solid_w)
            {
                out.extend(loops);
                continue;
            }
            let angle = b.angle.map_or_else(|| bridge_angle(part, anchors, w), f64::to_radians);
            let mut p = Params::new(w, b.density, angle, i, z);
            p.turn_by_layer = false;
            out.extend(paths_of(
                infill::fill_with(part, Kind::Rectilinear { alternate: false }, &p),
                Role::Bridge,
                w,
                s.speeds.bridge,
                flow,
            ));
        }
    }
    // Solid infill over sparse infill bridges it: an internal bridge.
    let (internal, solid) = match at.sparse_below {
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
        let anchors = match (at.below, at.sparse_below) {
            (Some(bl), Some(sb)) => geom::boolean(bl, sb, Op::Difference),
            _ => Vec::new(),
        };
        for part in &internal {
            let part = std::slice::from_ref(part);
            let angle = b.internal_angle.map_or_else(|| bridge_angle(part, &anchors, w), f64::to_radians);
            let mut p = Params::new(w, b.internal_density, angle, i, z);
            p.turn_by_layer = false;
            out.extend(paths_of(
                infill::fill_with(part, Kind::Rectilinear { alternate: false }, &p),
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
        infill::fill_with(&narrow, Kind::Concentric, &solid_params(solid_w, 1.0)),
        Role::SolidInfill,
        solid_w,
        solid_speed,
        inf.solid_flow,
    ));
    out.extend(paths_of(
        infill::fill_with(&solid, infill::kind_of(&s.solid_pattern), &solid_params(solid_w, 1.0)),
        Role::SolidInfill,
        solid_w,
        solid_speed,
        inf.solid_flow,
    ));
    out.extend(gaps);
    sparse_fill(out, r, s, at, sparse_angle, infill_speed);
}

/// Sparse infill: lightning trees, or the pattern, less what combined infill prints later and
/// plus what it prints here.
fn sparse_fill(out: &mut Vec<Path>, r: &Regions, s: &Settings, at: &Around<'_>, angle: f64, speed: f64) {
    if r.sparse.is_empty() {
        return;
    }
    let first = at.i == 0;
    let w = if first { s.widths.first_layer } else { s.widths.sparse };
    let flow = s.infill.sparse_flow;
    if let Some(lines) = at.lightning {
        let clipped = geom::clip_lines(lines, &geom::offset(&r.sparse, w * 0.5, Join::Miter));
        out.extend(
            clipped
                .into_iter()
                .filter(|l| geom::length(l) > w)
                .map(|l| Path::new(Role::SparseInfill, l, false, w, speed, flow)),
        );
        return;
    }
    let inf = &s.infill;
    let mut p = Params::new(w, s.density, angle, at.i, at.layer.print_z);
    p.turn_by_layer = inf.sparse_template.is_empty();
    p.anchor = inf.anchor;
    p.lattice = inf.lattice;
    p.shift_step = inf.shift_step;
    p.skin = inf.skin;
    let kind = infill::kind_of(&s.sparse_pattern);
    let (here, thick) = match at.combined {
        Some(c) if !c.skip.is_empty() => {
            let here = geom::without_slivers(geom::boolean(&r.sparse, &c.skip, Op::Difference), w * w * 2.0);
            let thick = if c.layers >= 2 { geom::boolean(&r.sparse, &c.print, Op::Intersect) } else { Vec::new() };
            (here, (!thick.is_empty()).then_some((thick, c.layers)))
        }
        _ => (r.sparse.clone(), None),
    };
    out.extend(paths_of(infill::fill_with(&here, kind, &p), Role::SparseInfill, w, speed, flow));
    if let Some((thick, n)) = thick {
        // As tall as the layers it stands for.
        let h = at.layer.height;
        let factor = bead_area(w, h * n as f64) / bead_area(w, h).max(1e-6);
        out.extend(paths_of(infill::fill_with(&thick, kind, &p), Role::SparseInfill, w, speed, flow * factor));
    }
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
        let plan = object(&slices, &layers, &s, &[], &never);
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
        let plan = object(&slices, &layers, &s, &[], &never);
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
        let plan = object(&slices, &layers, &s, &[], &never);
        let marked =
            |i: usize| plan[i].iter().flat_map(|isl| isl.walls.iter().flatten()).any(|p| p.overhang.contains(&5));
        assert!(marked(10) && !marked(9) && !marked(11));
        assert!(plan[10][0].fills.iter().any(|p| p.role == Role::Bridge));
    }
}
