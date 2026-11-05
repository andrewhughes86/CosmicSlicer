//! What each part of a layer's fill is: OrcaSlicer's `prepare_infill` pipeline.
//!
//! Over all an object's layers: which surfaces face up and down (`detect_surfaces_type`), which
//! infill turns solid to keep the shell thick (`discover_vertical_shells`, or the horizontal
//! shells for the lesser settings), then how top, bottom and bridge surfaces grow into the fill
//! round them and which way bridges run (`process_external_surfaces`).

use glam::DVec2;
use rayon::prelude::*;

use crate::geom::{self, Join, Op, Polyline, Shape};
use crate::settings::{Settings, VerticalShell};

/// A layer as it comes out of wall generation.
pub struct Input {
    /// Everything on the layer: OrcaSlicer's `lslices`.
    pub slices: Vec<Shape>,
    /// Where infill goes: its `fill_expolygons`.
    pub fill: Vec<Shape>,
    pub height: f64,
}

/// A layer's fill by type.
#[derive(Clone, Debug, Default)]
pub struct Typed {
    pub top: Vec<Shape>,
    pub bottom: Vec<Shape>,
    /// Bridges over air, each with the angle its lines run at, radians.
    pub bridges: Vec<(Vec<Shape>, f64)>,
    pub solid: Vec<Shape>,
    pub sparse: Vec<Shape>,
}

fn opening(shapes: &[Shape], by: f64) -> Vec<Shape> {
    geom::offset(&geom::offset(shapes, -by, Join::Miter), by, Join::Miter)
}

fn closing(shapes: &[Shape], by: f64) -> Vec<Shape> {
    geom::offset(&geom::offset(shapes, by, Join::Miter), -by, Join::Miter)
}

fn diff(a: &[Shape], b: &[Shape]) -> Vec<Shape> {
    geom::boolean(a, b, Op::Difference)
}

fn both(a: &[Shape], b: &[Shape]) -> Vec<Shape> {
    geom::boolean(a, b, Op::Intersect)
}

fn all(mut a: Vec<Shape>, b: &[Shape]) -> Vec<Shape> {
    a.extend_from_slice(b);
    geom::union(&a)
}

/// The layer's outer wall width, as flows are read there.
fn widths(s: &Settings, first: bool) -> (f64, f64, f64) {
    if first {
        (s.widths.first_layer, s.widths.first_layer, s.widths.first_layer)
    } else {
        (s.widths.outer, s.widths.inner, s.widths.solid)
    }
}

/// `detect_surfaces_type`: what faces up, what faces down, on the whole outline.
fn detect(layers: &[Input], s: &Settings) -> Vec<(Vec<Shape>, Vec<Shape>, Vec<Shape>)> {
    (0..layers.len())
        .into_par_iter()
        .map(|i| {
            let Some(layer) = layers.get(i) else { return Default::default() };
            let first = i == 0;
            let (outer, _, _) = widths(s, first);
            let offset = outer / 10.0;
            let upper = layers.get(i + 1);
            let lower = i.checked_sub(1).and_then(|k| layers.get(k));
            let mut top = match upper {
                Some(u) => opening(&diff(&layer.slices, &u.slices), offset),
                None => layer.slices.clone(),
            };
            let mut bottom = match lower {
                Some(l) => opening(&diff(&layer.slices, &l.slices), offset),
                None => layer.slices.clone(),
            };
            if !top.is_empty() && !bottom.is_empty() {
                let cracks = both(&top, &bottom);
                if !cracks.is_empty() {
                    if lower.is_some() {
                        // Small cracks are left to the top, unless a bottom holds them.
                        let threshold = outer * 1.5;
                        for crack in &cracks {
                            let one = std::slice::from_ref(crack);
                            if !geom::offset(one, -threshold, Join::Miter).is_empty() {
                                continue;
                            }
                            let held = bottom.iter().any(|b| {
                                let se = std::slice::from_ref(b);
                                diff(one, se).is_empty()
                                    && geom::shape_area(b) > geom::shape_area(crack) * 2.0
                                    && !geom::offset(&diff(se, one), -threshold, Join::Miter).is_empty()
                            });
                            if !held {
                                bottom = diff(&bottom, &geom::offset(one, threshold, Join::Miter));
                            }
                        }
                    }
                    top = diff(&top, &bottom);
                }
            }
            let internal = diff(&diff(&layer.slices, &top), &bottom);
            (top, bottom, internal)
        })
        .collect()
}

/// A layer's fill while it is being sorted out, and its typed outline.
#[derive(Clone, Default)]
struct Fill {
    top: Vec<Shape>,
    bottom: Vec<Shape>,
    bridge: Vec<Shape>,
    internal: Vec<Shape>,
    solid: Vec<Shape>,
    slice_top: Vec<Shape>,
    slice_bottom: Vec<Shape>,
}

/// `discover_vertical_shells` with "ensure vertical shell thickness" on all: on each layer, what
/// is under a top or over a bottom within the shell's reach, and what any layer in that reach
/// leaves without infill, turns solid.
fn vertical_shells(fills: &mut [Fill], layers: &[Input], s: &Settings) {
    struct Cache {
        top: Vec<Shape>,
        bottom: Vec<Shape>,
        holes: Vec<Shape>,
    }
    let cache: Vec<Cache> = fills
        .par_iter()
        .zip(layers.par_iter())
        .enumerate()
        .map(|(i, (f, l))| {
            let (_, _, solid) = widths(s, i == 0);
            let by = Settings::spacing(solid, l.height) * 0.05;
            Cache {
                top: geom::offset(&f.slice_top, by, Join::Miter),
                bottom: geom::offset(&f.slice_bottom, by, Join::Miter),
                holes: l.fill.clone(),
            }
        })
        .collect();
    let (top_n, bottom_n) = (s.top_layers as i64, s.bottom_layers as i64);
    let n = fills.len() as i64;
    let done: Vec<Option<(Vec<Shape>, Vec<Shape>)>> = (0..n)
        .into_par_iter()
        .map(|i| {
            let at = i as usize;
            let (Some(this), Some(f), Some(layer)) = (cache.get(at), fills.get(at), layers.get(at)) else {
                return None;
            };
            let (outer, _, solid) = widths(s, i == 0);
            let spacing = Settings::spacing(solid, layer.height);
            let mut shell: Vec<Shape> = Vec::new();
            let mut holes = this.holes.clone();
            let combine_holes = |holes: &mut Vec<Shape>, other: &[Shape]| {
                *holes = if holes.is_empty() || other.is_empty() { Vec::new() } else { both(holes, other) };
            };
            let lslices_of = |k: i64| layers.get(k as usize).map_or(&[][..], |l| l.slices.as_slice());
            if top_n > 0 {
                let mut k = i + 1;
                let last = i + top_n;
                let mut projected = false;
                while k < n && k < last {
                    projected = true;
                    if let Some(c) = cache.get(k as usize) {
                        combine_holes(&mut holes, &c.holes);
                        shell.extend_from_slice(&c.top);
                    }
                    k += 1;
                }
                if !projected && k < n {
                    let reach = Settings::spacing(outer, layer.height);
                    shell.extend(both(&geom::offset(&this.top, reach, Join::Miter), lslices_of(k)));
                }
            }
            if bottom_n > 0 {
                let mut k = i - 1;
                let last = i - bottom_n;
                let mut projected = false;
                while k >= 0 && k > last {
                    projected = true;
                    if let Some(c) = cache.get(k as usize) {
                        combine_holes(&mut holes, &c.holes);
                        shell.extend_from_slice(&c.bottom);
                    }
                    k -= 1;
                }
                if !projected && k >= 0 {
                    let reach = Settings::spacing(outer, layer.height);
                    shell.extend(both(&geom::offset(&this.bottom, reach, Join::Miter), lslices_of(k)));
                }
            }
            let inside = all(f.internal.clone(), &f.solid);
            let shell = geom::union(&shell);
            let mut shell = both(&shell, &inside);
            shell.extend(diff(&inside, &holes));
            if shell.is_empty() {
                return None;
            }
            shell.extend_from_slice(&f.solid);
            let wide = spacing * 1.05;
            let (narrow, gap, tiny) = (0.5 * 0.65 * wide, 0.5 * 1.2 * wide, 0.2 * wide);
            let grown =
                geom::offset(&geom::offset(&geom::union(&shell), -narrow, Join::Miter), narrow + gap, Join::Miter);
            let regular = geom::offset(&grown, -(gap - tiny), Join::Miter);
            // Small pieces that would leave the fill no more broken up go.
            let covered = i
                .checked_sub(1)
                .and_then(|k| layers.get(k as usize))
                .zip(layers.get(at + 1))
                .map_or(Vec::new(), |(a, b)| both(&a.slices, &b.slices));
            let volume = closing(&inside, 1e-4);
            let pieces = |x: &[Shape]| x.len();
            let regular: Vec<Shape> = regular
                .into_iter()
                .filter(|p| {
                    let a = geom::shape_area(p);
                    let one = std::slice::from_ref(p);
                    let small = a < wide * 1.5 || (a < wide * 8.0 && diff(one, &covered).is_empty());
                    !(small && pieces(&diff(&volume, &geom::offset(one, wide, Join::Miter))) >= pieces(&volume))
                })
                .collect();
            if regular.is_empty() {
                return None;
            }
            Some((diff(&f.internal, &regular), both(&inside, &regular)))
        })
        .collect();
    for (f, d) in fills.iter_mut().zip(done) {
        if let Some((internal, solid)) = d {
            f.internal = internal;
            f.solid = solid;
        }
    }
}

/// `discover_horizontal_shells`, for the lesser vertical shell settings: solid infill spreads
/// from top and bottom surfaces through the shell's layers.
fn horizontal_shells(fills: &mut [Fill], layers: &[Input], s: &Settings) {
    let n = fills.len() as i64;
    let factor = if s.density <= 0.0 {
        1.0
    } else {
        match s.infill.vertical_shell {
            VerticalShell::None => 0.5,
            VerticalShell::Critical => 0.2,
            _ => 0.0,
        }
    };
    for i in 0..n {
        for kind in 0..3 {
            let count = if kind == 0 { s.top_layers as i64 } else { s.bottom_layers as i64 };
            if count == 0 {
                continue;
            }
            let Some(f) = fills.get(i as usize) else { continue };
            let mut solid = match kind {
                0 => all(f.slice_top.clone(), &f.top),
                1 if i == 0 => all(f.slice_bottom.clone(), &f.bottom),
                1 => Vec::new(),
                _ if i == 0 => Vec::new(),
                _ => all(f.slice_bottom.clone(), &f.bridge),
            };
            if solid.is_empty() {
                continue;
            }
            let (step, within): (i64, Box<dyn Fn(i64) -> bool>) = if kind == 0 {
                (-1, Box::new(move |k| k >= 0 && i - k < count))
            } else {
                (1, Box::new(move |k| k < n && k - i < count))
            };
            let mut k = i + step;
            while within(k) {
                let at = k as usize;
                let Some(next) = fills.get(at) else { break };
                let h = layers.get(at).map_or(s.layer_height, |l| l.height);
                let (outer, _, solid_w) = widths(s, k == 0);
                let inside = all(next.internal.clone(), &next.solid);
                let mut fresh = both(&solid, &inside);
                if fresh.is_empty() {
                    if s.density <= 0.0 || s.infill.vertical_shell == VerticalShell::Critical {
                        break;
                    }
                    k += step;
                    continue;
                }
                if factor > 0.0 {
                    let margin = outer * factor;
                    let narrow = diff(&fresh, &opening(&fresh, margin));
                    if !narrow.is_empty() {
                        fresh = diff(&fresh, &narrow);
                        solid = fresh.clone();
                    }
                }
                let margin = if s.infill.vertical_shell == VerticalShell::None { 1.0 } else { 3.0 }
                    * Settings::spacing(solid_w, h);
                let narrow = diff(&fresh, &opening(&fresh, margin));
                if !narrow.is_empty() {
                    fresh.extend(both(&geom::offset(&narrow, margin, Join::Miter), &next.internal));
                }
                let new_solid = all(fresh, &next.solid);
                if let Some(next) = fills.get_mut(at) {
                    next.internal = diff(&next.internal, &new_solid);
                    next.top = diff(&next.top, &new_solid);
                    next.bottom = diff(&next.bottom, &new_solid);
                    next.bridge = diff(&next.bridge, &new_solid);
                    next.solid = new_solid;
                }
                k += step;
            }
        }
    }
}

/// How a surface grows into a zone: `RegionExpansionParameters::build`.
#[derive(Clone, Copy)]
struct Wave {
    tiny: f64,
    step: f64,
    steps: usize,
}

impl Wave {
    fn new(full: f64) -> Wave {
        const STEP: f64 = 0.1;
        const MAX_STEPS: usize = 5;
        let mut tiny = (0.25 * full).min(0.05);
        let mut steps = (((full - tiny) / STEP).ceil() as usize).clamp(1, MAX_STEPS);
        let mut step = (full - tiny) / steps as f64;
        if steps > 1 && 0.25 * step < tiny {
            steps = (((full - tiny) / (4.0 * tiny)).floor() as usize).max(1);
            step = (full - tiny) / steps as f64;
        }
        if 0.25 * step < tiny || steps == 1 {
            tiny = 0.2 * full;
            step = 0.8 * full;
        }
        Wave { tiny, step, steps }
    }

    /// `src` growing into `zone` from where they touch.
    fn into(self, src: &[Shape], zone: &[Shape]) -> Vec<Shape> {
        if src.is_empty() || zone.is_empty() {
            return Vec::new();
        }
        let seed = geom::offset(src, self.tiny, Join::Miter);
        let mut front = both(&seed, zone);
        if front.is_empty() {
            return front;
        }
        // The first step takes the seed along the edge it touches.
        front = both(&geom::offset(&front, self.step, Join::Round), zone);
        for _ in 1..self.steps {
            if front.is_empty() {
                break;
            }
            front = both(&geom::offset(&front, self.step, Join::Round), zone);
        }
        front
    }
}

/// The principal axes' shorter direction, `compute_principal_components`' second.
fn short_axis(area: &[Shape]) -> Option<DVec2> {
    let (mut a, mut cx, mut cy, mut xx, mut yy, mut xy) = (0.0, 0.0, 0.0, 0.0, 0.0, 0.0);
    for c in area.iter().flatten() {
        let n = c.len();
        for i in 0..n {
            let (Some(&p), Some(&q)) = (c.get(i), c.get((i + 1) % n)) else { continue };
            let k = p.perp_dot(q);
            a += k;
            cx += (p.x + q.x) * k;
            cy += (p.y + q.y) * k;
            xx += (p.x * p.x + p.x * q.x + q.x * q.x) * k;
            yy += (p.y * p.y + p.y * q.y + q.y * q.y) * k;
            xy += (2.0 * p.x * p.y + p.x * q.y + q.x * p.y + 2.0 * q.x * q.y) * k;
        }
    }
    if a <= 0.0 {
        return None;
    }
    let area = a / 2.0;
    let (mx, my) = (cx / (6.0 * area), cy / (6.0 * area));
    let (vx, vy, cov) = (xx / (12.0 * area) - mx * mx, yy / (12.0 * area) - my * my, xy / (24.0 * area) - mx * my);
    if cov.abs() < 1e-4 {
        return Some(if vy > vx { DVec2::X } else { DVec2::Y });
    }
    let theta = 0.5 * (2.0 * cov).atan2(vx - vy);
    Some(DVec2::new(-theta.sin(), theta.cos()))
}

/// `detect_bridging_direction`: the way across that leaves the least edge hanging.
fn bridging_direction(floating: &[[DVec2; 2]], area: &[Shape]) -> DVec2 {
    if floating.is_empty() {
        return short_axis(area).unwrap_or(DVec2::X);
    }
    let mut directions: Vec<(i64, DVec2)> = Vec::new();
    for [a, b] in floating {
        let d = *b - *a;
        let normal = DVec2::new(d.y, -d.x).normalize_or_zero();
        let key = (normal.y.atan2(normal.x) * 1000.0).ceil() as i64;
        if !directions.iter().any(|(k, _)| *k == key) {
            directions.push((key, normal));
        }
    }
    directions.sort_by_key(|d| d.0);
    let mut best = (f64::MAX, DVec2::ONE);
    for (_, dir) in directions {
        let cost: f64 = floating.iter().map(|[a, b]| (*b - *a).dot(dir).abs()).sum();
        if cost < best.0 {
            best = (cost, DVec2::new(dir.y, -dir.x));
        }
    }
    best.1
}

/// The edges of `shape` away from `anchors`.
fn floating_edges(shape: &Shape, anchors: &[Shape]) -> Vec<[DVec2; 2]> {
    let rings: Vec<Polyline> = shape
        .iter()
        .map(|c| {
            let mut r = c.clone();
            r.extend(c.first().copied());
            r
        })
        .collect();
    let near = geom::offset(anchors, 1e-4, Join::Miter);
    let lines = if near.is_empty() { rings } else { geom::clip_lines_outside(&rings, &near) };
    lines.iter().flat_map(|l| l.windows(2).map(|w| [w[0], w[1]])).collect()
}

/// `process_external_surfaces`: bridges, bottoms and tops grow into the fill round them.
fn external(f: Fill, s: &Settings, first: bool, h: f64) -> Typed {
    let (outer, inner, solid_w) = widths(s, first);
    let (so, si) = (Settings::spacing(outer, h), Settings::spacing(inner, h));
    let (shell, least) =
        if s.wall_loops > 0 { (0.5 * outer + so + si * (s.wall_loops - 1) as f64, si) } else { (1e-4, 1e-4) };
    let expansion = (shell as f32 * std::f32::consts::SQRT_2) as f64;
    let close_by = 0.55 * 0.65 * 1.05 * Settings::spacing(solid_w, h);
    let into_solid = Wave::new(expansion);
    let into_sparse = Wave::new(least);
    let Fill { top, bottom, bridge, internal, solid, .. } = f;
    let (mut shells, mut sparse, mut top) = (geom::union(&solid), geom::union(&internal), geom::union(&top));

    // Bridges: each grows into the fill; those that meet print as one.
    struct Grown {
        source: Shape,
        grown: Vec<Shape>,
        angle: f64,
    }
    let mut bridges: Vec<Grown> = Vec::new();
    for b in geom::union(&bridge) {
        let one = std::slice::from_ref(&b);
        let (a, c, d) = (into_solid.into(one, &shells), into_sparse.into(one, &sparse), into_solid.into(one, &top));
        // What it anchors on: the pieces of fill it touches.
        let touching = |zone: &[Shape], wave: Wave| -> Vec<Shape> {
            let seed = geom::offset(one, wave.tiny, Join::Miter);
            zone.iter().filter(|z| !both(&seed, std::slice::from_ref(z)).is_empty()).cloned().collect()
        };
        let mut anchors = touching(&shells, into_solid);
        anchors.extend(touching(&sparse, into_sparse));
        anchors.extend(touching(&top, into_solid));
        let dir = bridging_direction(&floating_edges(&b, &anchors), one);
        let angle = std::f64::consts::PI + dir.y.atan2(dir.x);
        let mut grown = vec![b.clone()];
        grown.extend(a);
        grown.extend(c);
        grown.extend(d);
        bridges.push(Grown { source: b, grown: geom::union(&grown), angle });
    }
    let mut group: Vec<usize> = (0..bridges.len()).collect();
    for i in 0..bridges.len() {
        for j in i + 1..bridges.len() {
            let (Some(a), Some(b)) = (bridges.get(i), bridges.get(j)) else { continue };
            if !both(&a.grown, &b.grown).is_empty() {
                let (x, y) = (group.get(i).copied().unwrap_or(i), group.get(j).copied().unwrap_or(j));
                let (keep, gone) = (x.min(y), x.max(y));
                group.iter_mut().filter(|g| **g == gone).for_each(|g| *g = keep);
            }
        }
    }
    let mut out_bridges: Vec<(Vec<Shape>, f64)> = Vec::new();
    for head in 0..bridges.len() {
        if group.get(head) != Some(&head) {
            continue;
        }
        let mut acc: Vec<Shape> = Vec::new();
        for (g, b) in group.iter().zip(&bridges) {
            if *g == head {
                acc.push(b.source.clone());
                acc.extend_from_slice(&b.grown);
            }
        }
        let merged = closing(&geom::union(&acc), close_by);
        shells = diff(&shells, &merged);
        sparse = diff(&sparse, &merged);
        top = diff(&top, &merged);
        out_bridges.push((merged, bridges.get(head).map_or(0.0, |b| b.angle)));
    }

    let mut grow = |src: Vec<Shape>, shells: &mut Vec<Shape>, sparse: &mut Vec<Shape>| -> Vec<Shape> {
        if src.is_empty() {
            return src;
        }
        let (a, b) = (into_solid.into(&src, shells), into_sparse.into(&src, sparse));
        let mut acc = src;
        acc.extend(a);
        acc.extend(b);
        let merged = closing(&geom::union(&acc), close_by);
        *shells = diff(shells, &merged);
        *sparse = diff(sparse, &merged);
        merged
    };
    let bottom = grow(geom::union(&bottom), &mut shells, &mut sparse);
    let top = grow(top, &mut shells, &mut sparse);
    if s.density > 0.0 && s.infill.min_sparse_area > 0.0 {
        let (small, big): (Vec<Shape>, Vec<Shape>) =
            sparse.into_iter().partition(|p| geom::shape_area(p) <= s.infill.min_sparse_area);
        sparse = big;
        if !small.is_empty() {
            shells = all(shells, &small);
        }
    }
    Typed { top, bottom, bridges: out_bridges, solid: shells, sparse }
}

/// Every layer's fill by type.
pub fn prepare(layers: &[Input], s: &Settings) -> Vec<Typed> {
    let typed = detect(layers, s);
    let mut fills: Vec<Fill> = typed
        .into_par_iter()
        .zip(layers.par_iter())
        .enumerate()
        .map(|(i, ((top, bottom, internal), l))| {
            let first = i == 0;
            let mut f = Fill {
                top: both(&top, &l.fill),
                internal: both(&internal, &l.fill),
                slice_top: top,
                slice_bottom: bottom.clone(),
                ..Fill::default()
            };
            if first {
                f.bottom = both(&bottom, &l.fill);
            } else {
                f.bridge = both(&bottom, &l.fill);
            }
            // `prepare_fill_surfaces`.
            if s.top_layers == 0 {
                f.internal = all(std::mem::take(&mut f.internal), &f.top);
                f.top.clear();
            }
            if s.bottom_layers == 0 {
                f.internal = all(std::mem::take(&mut f.internal), &f.bottom);
                f.bottom.clear();
            }
            if s.density >= 0.999 {
                f.solid = std::mem::take(&mut f.internal);
            }
            f
        })
        .collect();
    if s.infill.vertical_shell == VerticalShell::All {
        vertical_shells(&mut fills, layers, s);
    } else {
        horizontal_shells(&mut fills, layers, s);
    }
    fills
        .into_par_iter()
        .zip(layers.par_iter())
        .enumerate()
        .map(|(i, (f, l))| external(f, s, i == 0, l.height))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn waves_follow_orcas_steps() {
        let w = Wave::new(0.9);
        assert!((w.tiny + w.step * w.steps as f64 - 0.9).abs() < 1e-9);
        assert_eq!(w.steps, 5);
    }

    #[test]
    fn a_span_runs_across_the_gap() {
        // Held along the left and right: only top and bottom edges hang.
        let shape: Shape = vec![vec![DVec2::ZERO, DVec2::new(10.0, 0.0), DVec2::new(10.0, 4.0), DVec2::new(0.0, 4.0)]];
        let edges = [[DVec2::ZERO, DVec2::new(10.0, 0.0)], [DVec2::new(10.0, 4.0), DVec2::new(0.0, 4.0)]];
        let d = bridging_direction(&edges, std::slice::from_ref(&shape));
        assert!(d.y.abs() < 1e-9 && d.x.abs() > 0.99, "{d:?}");
    }

    #[test]
    fn with_no_loose_edge_a_bridge_takes_the_short_way() {
        let shape: Shape = vec![vec![DVec2::ZERO, DVec2::new(10.0, 0.0), DVec2::new(10.0, 4.0), DVec2::new(0.0, 4.0)]];
        let d = bridging_direction(&[], std::slice::from_ref(&shape));
        assert!(d.x.abs() < 1e-6 && d.y.abs() > 0.99, "{d:?}");
    }
}
