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
use crate::surfaces;
use crate::toolpath::{Island, Path, Role};
use crate::{fuzzy, lightning, overhang, walls};

/// What OrcaSlicer calls the regions of a layer's fill.
#[derive(Default)]
struct Regions {
    top: Vec<Shape>,
    bottom: Vec<Shape>,
    /// Bridges whose angle is known, on layers `prepare`d by [`surfaces`].
    bridges: Vec<(Vec<Shape>, f64)>,
    /// Solid infill bridging sparse infill: `Some` once [`surfaces`] has found them.
    internal: Option<Vec<(Vec<Shape>, f64)>>,
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
    let mut solid_all = regularize_shell(solid_all, area, slices, i, s, w);
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
    if s.wall_loops == 0 {
        return Regions { top, bottom, solid, sparse, bridges: Vec::new(), internal: None };
    }
    // OrcaSlicer's external surface expansion: top and bottom surfaces reach out under the
    // walls' thickness (×√2) into the solid infill round them, and a wall's spacing into
    // sparse infill, so they are anchored.
    let h = s.layer_height;
    let (so, si) = (Settings::spacing(s.widths.outer, h), Settings::spacing(s.widths.inner, h));
    let shell = 0.5 * s.widths.outer + so + si * (s.wall_loops - 1) as f64;
    let (into_solid, into_sparse) = (shell * std::f64::consts::SQRT_2, si);
    let mut solid = solid;
    let mut grow = |surface: Vec<Shape>| -> Vec<Shape> {
        if surface.is_empty() {
            return surface;
        }
        let from_solid = geom::boolean(&geom::offset(&surface, into_solid, Join::Miter), &solid, Op::Intersect);
        let from_sparse = geom::boolean(&geom::offset(&surface, into_sparse, Join::Miter), &sparse, Op::Intersect);
        let mut all = surface;
        all.extend(from_solid);
        all.extend(from_sparse);
        let grown = geom::union(&all);
        solid = geom::without_slivers(geom::boolean(&solid, &grown, Op::Difference), sliver);
        sparse = geom::without_slivers(geom::boolean(&sparse, &grown, Op::Difference), sliver);
        grown
    };
    let bottom = grow(bottom);
    let top = grow(geom::boolean(&top, &bottom, Op::Difference));
    Regions { top, bottom, solid, sparse, bridges: Vec::new(), internal: None }
}

/// OrcaSlicer's clean-up of the solid shell on a layer ("ensure vertical shell thickness"):
/// strips narrower than about two thirds of a line go, gaps under a line and a bit close, the
/// shell overlaps the sparse infill by a fifth of a line, and pieces too small to print well
/// go: under 1.5 mm² × the line spacing, or under 8 mm² × it where the layers above and below
/// cover them anyway.
fn regularize_shell(
    shell: Vec<Shape>,
    area: &[Shape],
    slices: &[Vec<Shape>],
    i: isize,
    s: &Settings,
    w: f64,
) -> Vec<Shape> {
    if shell.is_empty() {
        return shell;
    }
    let spacing = Settings::spacing(w, s.layer_height) * 1.05;
    let (narrow, gaps, overlap) = (0.5 * 0.65 * spacing, 0.5 * 1.2 * spacing, 0.2 * spacing);
    let opened = geom::offset(&geom::offset(&shell, -narrow, Join::Miter), narrow + gaps, Join::Miter);
    let regular = geom::offset(&opened, -(gaps - overlap), Join::Miter);
    // What the layers below and above both cover.
    let around = geom::boolean(&common(slices, i - 1, i), &common(slices, i + 1, i + 2), Op::Intersect);
    let kept: Vec<Shape> = regular
        .into_iter()
        .filter(|p| {
            let a = geom::shape_area(p);
            let tiny = a < spacing * 1.5
                || (a < spacing * 8.0
                    && !around.is_empty()
                    && geom::boolean(std::slice::from_ref(p), &around, Op::Difference).is_empty());
            !tiny
        })
        .collect();
    geom::boolean(&kept, area, Op::Intersect)
}

/// Which of a layer's solid infill bridges the sparse infill below (OrcaSlicer's "bridge over
/// infill"), and the rest. Only what lies over open sparse infill counts: the sparse area below
/// less three lines round its edge (strips along walls and next to solid infill are held up
/// well enough). A solid piece partly held up bridges only when the open part is over three
/// lines square; the bridge takes four lines round it, and leftover scraps of the piece.
fn internal_bridges(solid: &[Shape], sparse_below: &[Shape], s: &Settings, h: f64) -> (Vec<Shape>, Vec<Shape>) {
    let spacing = Settings::spacing(s.widths.solid, h);
    let open = geom::offset(sparse_below, -3.0 * spacing, Join::Miter);
    if open.is_empty() {
        return (Vec::new(), solid.to_vec());
    }
    let mut bridged: Vec<Shape> = Vec::new();
    for piece in solid {
        let piece = std::slice::from_ref(piece);
        let unsupported = geom::boolean(piece, &open, Op::Intersect);
        let a = geom::area(&unsupported);
        if a <= 0.0 {
            continue;
        }
        let partly = a < geom::area(piece) - 1e-6;
        if partly && a <= 9.0 * spacing * spacing {
            continue;
        }
        let mut worth = geom::boolean(piece, &geom::offset(&unsupported, 4.0 * spacing, Join::Miter), Op::Intersect);
        let left = geom::boolean(piece, &geom::offset(&worth, spacing, Join::Miter), Op::Difference);
        worth.extend(left.into_iter().filter(|p| {
            let a = geom::shape_area(p);
            a > spacing * spacing && a < spacing * 12.0
        }));
        bridged.extend(geom::boolean(&geom::union(&worth), piece, Op::Intersect));
    }
    if bridged.is_empty() {
        return (Vec::new(), solid.to_vec());
    }
    let rest = geom::without_slivers(geom::boolean(solid, &bridged, Op::Difference), spacing * spacing);
    (bridged, rest)
}

/// Paths from fill lines; lines that must keep their order share a new group.
fn paths_of(lines: Vec<infill::Line>, role: Role, width: f64, speed: f64, flow: f64) -> Vec<Path> {
    let group = if lines.iter().any(|l| l.ordered) { Path::new_group() } else { 0 };
    lines
        .into_iter()
        .filter(|l| l.points.len() >= 2)
        .map(|l| {
            let mut p = Path::new(role, l.points, l.closed, width, speed, flow);
            p.one_way = l.one_way;
            if l.ordered {
                p.group = group;
            }
            p
        })
        .collect()
}

/// Fills each region of `area` on its own, as OrcaSlicer does: each gets its own line spacing
/// and width.
fn solid_paths(area: &[Shape], kind: Kind, p: &Params, role: Role, speed: f64, flow: f64) -> Vec<Path> {
    area.iter()
        .flat_map(|shape| {
            let one = std::slice::from_ref(shape);
            paths_of(infill::fill_with(one, kind, p), role, infill::solid_width(one, kind, p), speed, flow)
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
    let set = s.bridges.width;
    if thick {
        let d = if set > 0.0 { set } else { s.nozzle };
        let round = std::f64::consts::PI * (d / 2.0).powi(2);
        (d, flow * round / bead_area(d, h).max(1e-6))
    } else if set > 0.0 {
        (set, flow)
    } else {
        (w, flow)
    }
}

/// How far apart bridge lines go (OrcaSlicer's): round threads their diameter and 0.05 mm,
/// flat ones as solid infill does.
fn bridge_spacing(thick: bool, w: f64, h: f64) -> f64 {
    if thick { w + 0.05 } else { Settings::spacing(w, h) }
}

/// An internal bridge grown as OrcaSlicer anchors it: each of its lines (along `angle`,
/// `spacing` apart) runs on past the area to the first infill line below it, and a line's
/// width beyond, within `limit`; then cleaned of slivers and small gaps.
fn anchored(part: &[Shape], angle: f64, lines: &[Polyline], limit: &[Shape], spacing: f64, w: f64) -> Vec<Shape> {
    let dir = glam::DVec2::new(angle.cos(), angle.sin());
    let normal = dir.perp();
    let segs: Vec<(glam::DVec2, glam::DVec2)> =
        lines.iter().flat_map(|l| l.windows(2).filter_map(|p| Some((*p.first()?, *p.get(1)?)))).collect();
    let rows = geom::clip_lines(&geom::hatch_lines(part, spacing, angle, spacing / 2.0, 100_000), part);
    let mut strips: Vec<Shape> = Vec::with_capacity(rows.len());
    for row in rows {
        let (Some(&a), Some(&b)) = (row.first(), row.last()) else { continue };
        let (a, b) = if (b - a).dot(dir) >= 0.0 { (a, b) } else { (b, a) };
        // Where the row's line crosses the lines below, as distances along it from `a`.
        let len = a.distance(b);
        let (mut back, mut on) = (None::<f64>, None::<f64>);
        for (p, q) in &segs {
            let e = *q - *p;
            let denom = dir.perp_dot(e);
            if denom.abs() < 1e-12 {
                continue;
            }
            let t = (*p - a).perp_dot(e) / denom;
            let u = (*p - a).perp_dot(dir) / denom;
            if !(0.0..=1.0).contains(&u) {
                continue;
            }
            if t < 0.0 {
                back = Some(back.map_or(t, |x: f64| x.max(t)));
            } else if t > len {
                on = Some(on.map_or(t, |x: f64| x.min(t)));
            }
        }
        let from = a + dir * back.map_or(0.0, |t| t - w);
        let to = a + dir * on.map_or(len, |t| t + w);
        let h = normal * (spacing / 2.0);
        strips.push(vec![vec![from - h, to - h, to + h, from + h]]);
    }
    if strips.is_empty() {
        return part.to_vec();
    }
    let grown = geom::union(&strips);
    let opened = geom::offset(&geom::offset(&grown, -spacing * 0.375, Join::Miter), spacing * 0.375, Join::Miter);
    let closed = geom::offset(&geom::offset(&opened, spacing / 2.0, Join::Miter), -spacing / 2.0, Join::Miter);
    let mut out = geom::boolean(&closed, limit, Op::Intersect);
    out.extend(part.iter().cloned());
    geom::union(&out)
}

/// The sparse infill angle on layer `i` before turning by layer.
fn sparse_angle_at(s: &Settings, i: usize) -> f64 {
    s.infill_angle + settings::template_angle(&s.infill.sparse_template, i)
}

/// OrcaSlicer's internal bridge direction: square to the lines it crosses. Every 2 mm round the
/// bridge's edge, the nearest line below votes for the direction across it; the direction
/// with most votes within 18° wins.
fn across_lines(area: &[Shape], lines: &[Polyline]) -> Option<f64> {
    use std::f64::consts::PI;
    let segs: Vec<(glam::DVec2, glam::DVec2)> =
        lines.iter().flat_map(|l| l.windows(2).filter_map(|w| Some((*w.first()?, *w.get(1)?)))).collect();
    if segs.is_empty() {
        return None;
    }
    let mut votes: Vec<f64> = Vec::new();
    for c in area.iter().flatten() {
        let n = c.len();
        for k in 0..n {
            let (Some(&a), Some(&b)) = (c.get(k), c.get((k + 1) % n)) else { continue };
            let steps = (a.distance(b) / 2.0).ceil().max(1.0) as usize;
            for j in 0..steps {
                let q = a.lerp(b, j as f64 / steps as f64);
                let nearest = segs.iter().min_by(|x, y| {
                    geom::point_segment_distance(q, x.0, x.1).total_cmp(&geom::point_segment_distance(q, y.0, y.1))
                });
                if let Some((p0, p1)) = nearest {
                    let d = *p1 - *p0;
                    votes.push((d.y.atan2(d.x) + PI / 2.0).rem_euclid(PI));
                }
            }
            if votes.len() > 20_000 {
                break;
            }
        }
    }
    if votes.is_empty() {
        return None;
    }
    // Votes by degree, then the best 36° window round, averaged.
    let mut bins = [0usize; 180];
    for v in &votes {
        if let Some(b) = bins.get_mut((v.to_degrees() as usize).min(179)) {
            *b += 1;
        }
    }
    let window = |c: usize| (0..37).map(|k| bins.get((c + 180 + k - 18) % 180).copied().unwrap_or(0)).sum::<usize>();
    let centre = (0..180).max_by_key(|c| window(*c))?;
    let mid = (centre as f64 + 0.5).to_radians();
    let near: Vec<f64> = votes
        .iter()
        .map(|&u| mid + ((u - mid + PI / 2.0).rem_euclid(PI) - PI / 2.0))
        .filter(|u| (u - mid).abs() <= PI * 0.1)
        .collect();
    (!near.is_empty()).then(|| (near.iter().sum::<f64>() / near.len() as f64).rem_euclid(PI))
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
    // OrcaSlicer's windows: with N shell layers, the N - 1 layers past this one (this layer
    // makes the Nth).
    let reach = |count: usize| match s.infill.vertical_shell {
        VerticalShell::None => 0,
        VerticalShell::Critical => count.min(1),
        VerticalShell::Moderate => count.div_ceil(2),
        VerticalShell::All => count.saturating_sub(1),
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
                .flat_map(|p| {
                    let bridges = p.regions.bridges.iter().chain(p.regions.internal.iter().flatten());
                    let bridges = bridges.flat_map(|b| b.0.iter());
                    p.regions.top.iter().chain(&p.regions.bottom).chain(&p.regions.solid).chain(bridges).cloned()
                })
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
    /// The middle of the object on the plate: infill patterns are fixed to it.
    origin: glam::DVec2,
    layer: &'a LayerZ,
    below: Option<&'a Vec<Shape>>,
    sparse_below: Option<&'a Vec<Shape>>,
    lightning: Option<&'a Vec<Polyline>>,
    combined: Option<&'a Combined>,
    /// The object's last layer with anything on it.
    topmost: bool,
}

/// The islands of one object on every layer.
/// Where the object's fill patterns are measured from: its centre, as Orca keeps its slices
/// (the mesh's when known, which a slice's outline can miss between layers).
fn object_origin(slices: &[Vec<Shape>], s: &Settings) -> glam::DVec2 {
    s.origin.unwrap_or_else(|| {
        slices
            .iter()
            .filter_map(|sl| geom::bounds(sl))
            .reduce(|(a, b), (c, d)| (a.min(c), b.max(d)))
            .map_or(glam::DVec2::ZERO, |(lo, hi)| (lo + hi) / 2.0)
    })
}

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
    // OrcaSlicer's own region pipeline where one set of settings covers each layer.
    let whole = !s.spiral && regions.iter().all(|r| r.pieces.is_empty() && r.base.is_none());
    let shells = if whole { vec![Vec::new(); slices.len()] } else { vertical_shells(slices, layers, s) };
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
                    let around =
                        (i > 0).then_some(()).and(slices.get(i + 1)).zip(i.checked_sub(1).and_then(|k| slices.get(k)));
                    let around = around.map(|(u, l)| (u.as_slice(), l.as_slice()));
                    let walled =
                        walls::walls(island, s, layer.height, first, i + 1 == slices.len(), loops, 1.0, around);
                    let solid_w = if first { s.widths.first_layer } else { s.widths.solid };
                    let pieces = split_fill(&walled.infill_area, pieces, solid_w)
                        .into_iter()
                        .map(|(area, settings)| {
                            let rs = settings.as_deref().unwrap_or(s);
                            let w = if first { rs.widths.first_layer } else { rs.widths.solid };
                            let regions =
                                if whole { Regions::default() } else { classify(&area, slices, i, rs, w, shell) };
                            Piece { regions, area, settings }
                        })
                        .collect();
                    Walled { outline: island.clone(), walled, pieces }
                })
                .collect()
        })
        .collect();
    let mut walled = walled;
    if whole {
        let input: Vec<surfaces::Input> = (0..walled.len())
            .map(|i| surfaces::Input {
                slices: slices.get(i).cloned().unwrap_or_default(),
                fill: walled
                    .get(i)
                    .map(|l| l.iter().flat_map(|w| w.walled.infill_area.iter().cloned()).collect())
                    .unwrap_or_default(),
                height: layers.get(i).map_or(s.layer_height, |l| l.height),
            })
            .collect();
        let origin = object_origin(slices, s);
        // The sparse infill's lines, unjoined, for the bridges above to anchor on.
        let lines_below = |k: usize, near: &[Shape]| -> Vec<Polyline> {
            let area: Vec<Shape> = input.get(k).map_or(Vec::new(), |l| geom::boolean(&l.fill, near, Op::Intersect));
            let Some(layer) = layers.get(k) else { return Vec::new() };
            let mut p = Params::new(s.widths.sparse, s.density, sparse_angle_at(s, k), k, layer.print_z);
            p.origin = origin;
            p.inset = true;
            p.height = s.layer_height;
            p.bare = true;
            p.turn_by_layer = s.infill.sparse_template.is_empty();
            infill::fill_with(&area, infill::kind_of(&s.sparse_pattern), &p).into_iter().map(|l| l.points).collect()
        };
        let typed = surfaces::prepare(&input, s, &lines_below);
        walled.par_iter_mut().zip(typed.into_par_iter()).for_each(|(layer, t)| {
            let many = layer.iter().flat_map(|w| &w.pieces).count() > 1;
            for piece in layer.iter_mut().flat_map(|w| &mut w.pieces) {
                let clip = |x: &[Shape]| -> Vec<Shape> {
                    if many {
                        geom::boolean(x, &geom::offset(&piece.area, 0.5, Join::Miter), Op::Intersect)
                    } else {
                        x.to_vec()
                    }
                };
                let bridges: Vec<(Vec<Shape>, f64)> =
                    t.bridges.iter().map(|(b, a)| (clip(b), *a)).filter(|(b, _)| !b.is_empty()).collect();
                let internal = t.internal_bridges.iter().map(|(b, a)| (clip(b), *a)).filter(|(b, _)| !b.is_empty());
                let internal = Some(internal.collect());
                let regions = Regions {
                    top: clip(&t.top),
                    bottom: clip(&t.bottom),
                    bridges,
                    internal,
                    solid: clip(&t.solid),
                    sparse: clip(&t.sparse),
                };
                piece.regions = regions;
            }
        });
    }
    // Each layer's sparse infill, which solid infill over it has to bridge.
    let sparse: Vec<Vec<Shape>> = walled
        .iter()
        .map(|layer| layer.iter().flat_map(|w| &w.pieces).flat_map(|p| p.regions.sparse.iter().cloned()).collect())
        .collect();
    let lightning = (infill::kind_of(&s.sparse_pattern) == Kind::Lightning && s.density > 0.0)
        .then(|| lightning_lines(&walled, &sparse, layers, s, cancelled));
    let combined = combine(&sparse, layers, s);
    let topmost = slices.iter().rposition(|sl| !sl.is_empty()).unwrap_or(0);
    // The middle of the object's footprint over all its layers (OrcaSlicer's object origin).
    let origin = object_origin(slices, s);
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
                origin,
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
                        pressure_advance: None,
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
                    let arcs = s.motion.arc_fitting == crate::settings::ArcFitting::All;
                    for p in out.walls.iter_mut().flatten().chain(&mut out.fills) {
                        crate::toolpath::simplify(p, s.resolution, arcs && !s.spiral);
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
        p.origin = at.origin;
        p.inset = true;
        p.height = h;
        p.turn_by_layer = !templated;
        p
    };
    let solid_params = |w: f64, density: f64| {
        let mut p = params(w, density, solid_angle, !inf.solid_template.is_empty());
        p.fit_solid = true;
        p
    };
    let (infill_speed, solid_speed, top_speed) = if first {
        (s.speeds.first_layer_infill, s.speeds.first_layer_infill, s.speeds.first_layer_infill)
    } else {
        (s.speeds.sparse, s.speeds.solid, s.speeds.top)
    };
    let gap_speed = if first { s.speeds.first_layer } else { s.speeds.gap };
    let surfaces_gap = s.walls.gap_fill != GapFill::Nowhere;
    let top_w = if first { s.widths.first_layer } else { s.widths.top };
    let (top, gaps) = with_gap_fill(r.top.clone(), s, top_w, surfaces_gap, gap_speed);
    out.extend(solid_paths(
        &top,
        infill::kind_of(&s.top_pattern),
        &solid_params(top_w, inf.top_density),
        Role::TopSurface,
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
        p.origin = at.origin;
        p.turn_by_layer = false;
        // A film: the set share of a full line at the ironing spacing.
        let flow = iron.flow * h * iron.spacing / bead_area(top_w, h).max(1e-6);
        out.extend(paths_of(infill::fill_with(&area, kind, &p), Role::Ironing, top_w, iron.speed, flow));
    }
    if first {
        let (bottom, gaps) = with_gap_fill(r.bottom.clone(), s, solid_w, surfaces_gap, gap_speed);
        out.extend(solid_paths(
            &bottom,
            infill::kind_of(&s.bottom_pattern),
            &solid_params(solid_w, inf.bottom_density),
            Role::BottomSurface,
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
        for (area, angle) in &r.bridges {
            for part in area {
                let part = std::slice::from_ref(part);
                let angle = b.angle.map_or(*angle, f64::to_radians);
                let mut p = Params::new(bridge_spacing(b.thick, w, h), b.density, angle, i, z);
                p.origin = at.origin;
                p.inset = true;
                p.bridge = true;
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
        for part in &r.bottom {
            let part = std::slice::from_ref(part);
            if inf.extra_overhang_perimeters
                && let Some(loops) = overhang_perimeters(part, infill_area, anchors, s, solid_w)
            {
                out.extend(loops);
                continue;
            }
            let angle = b.angle.map_or_else(|| bridge_angle(part, anchors, w), f64::to_radians);
            let mut p = Params::new(bridge_spacing(b.thick, w, h), b.density, angle, i, z);
            p.origin = at.origin;
            p.inset = true;
            p.bridge = true;
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
    let (internal, mut solid) = match at.sparse_below {
        _ if r.internal.is_some() => (Vec::new(), r.solid.clone()),
        Some(sb) if !sb.is_empty() && !r.solid.is_empty() => internal_bridges(&r.solid, sb, s, h),
        _ => (Vec::new(), r.solid.clone()),
    };
    for (area, angle) in r.internal.iter().flatten() {
        let b = &s.bridges;
        let (w, flow) = bridge_bead(s, b.thick_internal, solid_w, h, b.internal_flow);
        for part in area {
            let mut p = Params::new(bridge_spacing(b.thick_internal, w, h), b.internal_density, *angle, i, z);
            p.origin = at.origin;
            p.inset = true;
            p.bridge = true;
            p.turn_by_layer = false;
            out.extend(paths_of(
                infill::fill_with(std::slice::from_ref(part), Kind::Rectilinear { alternate: false }, &p),
                Role::InternalBridge,
                w,
                b.internal_speed,
                flow,
            ));
        }
    }
    // Sparse infill left once bridges have reached out over it.
    let mut sparse_left: Option<Vec<Shape>> = None;
    if !internal.is_empty() {
        let b = &s.bridges;
        let (w, flow) = bridge_bead(s, b.thick_internal, solid_w, h, b.internal_flow);
        // Anchored on what is solid below: the layer less its sparse infill.
        let anchors = match (at.below, at.sparse_below) {
            (Some(bl), Some(sb)) => geom::boolean(bl, sb, Op::Difference),
            _ => Vec::new(),
        };
        // The sparse infill below, which the bridge crosses (OrcaSlicer runs it square to
        // those lines).
        let below_lines: Vec<Polyline> = match at.sparse_below {
            Some(sb) if i > 0 => {
                let near = geom::boolean(sb, &geom::offset(&internal, 8.0, Join::Miter), Op::Intersect);
                let mut p = Params::new(s.widths.sparse, s.density, sparse_angle_at(s, i - 1), i - 1, z - h);
                p.origin = at.origin;
                p.inset = true;
                p.height = h;
                p.turn_by_layer = inf.sparse_template.is_empty();
                infill::fill_with(&near, infill::kind_of(&s.sparse_pattern), &p).into_iter().map(|l| l.points).collect()
            }
            _ => Vec::new(),
        };
        // Bridges reach along their lines to the infill below (OrcaSlicer's anchoring), over
        // the solid and sparse infill round them.
        let reach_into = geom::union(&[r.solid.clone(), r.sparse.clone()].concat());
        let spacing = bridge_spacing(b.thick_internal, w, h);
        let mut bridged: Vec<(Vec<Shape>, f64)> = Vec::new();
        for part in &internal {
            let part = std::slice::from_ref(part);
            let angle = match b.internal_angle {
                Some(a) => a.to_radians(),
                None => across_lines(part, &below_lines).unwrap_or_else(|| bridge_angle(part, &anchors, w)),
            };
            bridged.push((anchored(part, angle, &below_lines, &reach_into, spacing, w), angle));
        }
        let all: Vec<Shape> = bridged.iter().flat_map(|(a, _)| a.iter().cloned()).collect();
        let all = geom::union(&all);
        solid = geom::without_slivers(geom::boolean(&solid, &all, Op::Difference), solid_w * solid_w);
        sparse_left = Some(geom::without_slivers(geom::boolean(&r.sparse, &all, Op::Difference), solid_w * solid_w));
        for (part, angle) in &bridged {
            let (part, angle) = (part.as_slice(), *angle);
            let mut p = Params::new(spacing, b.internal_density, angle, i, z);
            p.origin = at.origin;
            p.inset = true;
            p.bridge = true;
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
    let (solid, narrow) = if s.walls.narrow_solid {
        // The lines' direction on this layer, as the fill will lay them.
        let as_set = !inf.solid_template.is_empty() || i.is_multiple_of(2);
        let angle = solid_angle + if as_set { 0.0 } else { std::f64::consts::FRAC_PI_2 };
        let line_pattern = infill::is_line_pattern(infill::kind_of(&s.solid_pattern));
        infill::split_narrow_solid(&solid, Settings::spacing(solid_w, h), angle, line_pattern)
    } else {
        (solid, Vec::new())
    };
    out.extend(solid_paths(
        &solid,
        infill::kind_of(&s.solid_pattern),
        &solid_params(solid_w, 1.0),
        Role::SolidInfill,
        solid_speed,
        inf.solid_flow,
    ));
    out.extend(paths_of(
        infill::fill_with(&narrow, Kind::Concentric, &solid_params(solid_w, 1.0)),
        Role::SolidInfill,
        solid_w,
        solid_speed,
        inf.solid_flow,
    ));
    out.extend(gaps);
    match sparse_left {
        Some(sparse) => {
            let left = Regions { sparse, ..Regions::default() };
            sparse_fill(out, &left, s, at, sparse_angle, infill_speed);
        }
        None => sparse_fill(out, r, s, at, sparse_angle, infill_speed),
    }
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
    p.origin = at.origin;
    p.inset = true;
    p.height = at.layer.height;
    p.turn_by_layer = inf.sparse_template.is_empty();
    p.anchor = inf.anchor;
    p.anchor_max = inf.anchor_max;
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
        let slices = slicing::slice(&placed, &layers, 0.049, 0.0025, &never);
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
        let slices = slicing::slice(&placed, &layers, 0.049, 0.0025, &never);
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
        let marked = |i: usize| {
            plan[i]
                .iter()
                .flat_map(|isl| isl.walls.iter().flatten())
                .any(|p| p.overhang.iter().any(|h| crate::toolpath::degree_of(*h) == 5))
        };
        assert!(marked(10) && !marked(9) && !marked(11));
        assert!(plan[10][0].fills.iter().any(|p| p.role == Role::Bridge));
    }
}
