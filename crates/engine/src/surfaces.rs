//! What each part of a layer's fill is: OrcaSlicer's `prepare_infill` pipeline.
//!
//! Over all an object's layers: which surfaces face up and down (`detect_surfaces_type`), which
//! infill turns solid to keep the shell thick (`discover_vertical_shells`, or the horizontal
//! shells for the lesser settings), then how top, bottom and bridge surfaces grow into the fill
//! round them and which way bridges run (`process_external_surfaces`).

use glam::DVec2;
use rayon::prelude::*;

use crate::expansion::{self, ExPoly, Params};
use crate::geom::{self, Join, Op, Polyline, Shape};
use crate::seam::Outline;
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
    /// Solid infill bridging sparse infill below, with its angles.
    pub internal_bridges: Vec<(Vec<Shape>, f64)>,
    pub solid: Vec<Shape>,
    pub sparse: Vec<Shape>,
}

fn opening(shapes: &[Shape], by: f64) -> Vec<Shape> {
    geom::offset(&geom::offset(shapes, -by, Join::Miter), by, Join::Miter)
}

/// Closing. Orca's closings by `SCALED_EPSILON` only weld what touches: a union does that, and
/// offsets that small send the offsetter into minutes of work.
fn closing(shapes: &[Shape], by: f64) -> Vec<Shape> {
    if by < 1e-3 {
        return geom::union(shapes);
    }
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

/// `compute_moments_of_area_of_triangle`, in Orca's floats: area, first moments, second
/// moments and the covariance moment, none divided by the area.
fn triangle_moments(a: [f32; 2], b: [f32; 2], c: [f32; 2]) -> (f32, [f32; 2], [f32; 2], f32) {
    let jac = ((b[0] - a[0]) * (c[1] - a[1]) - (c[0] - a[0]) * (b[1] - a[1])).abs();
    let second = |k: usize| {
        let (a, b, c) = (a[k], b[k], c[k]);
        jac * (a * a + b * b + b * c + c * c + a * (b + c)) / 12.0
    };
    let cov = jac
        * (1.0 / 24.0)
        * (a[1] * (b[0] + c[0])
            + a[0] * (2.0 * a[1] + b[1] + c[1])
            + b[1] * c[0]
            + b[0] * c[1]
            + 2.0 * b[0] * b[1]
            + 2.0 * c[0] * c[1]);
    let first = |k: usize| jac * (a[k] + b[k] + c[k]) / 6.0;
    (jac * 0.5, [first(0), first(1)], [second(0), second(1)], cov)
}

/// `compute_principal_components`' second vector (the one with the smaller eigenvalue), in
/// Orca's floats over each polygon's fan from its first point; zero when there is no area.
fn second_principal(polys: &[crate::clipper::Path]) -> [f32; 2] {
    let (mut centroid, mut second, mut cov, mut area) = ([0f32; 2], [0f32; 2], 0f32, 0f32);
    let at = |p: &crate::clipper::Pt| [(p.x as f64 * 1e-6) as f32, (p.y as f64 * 1e-6) as f32];
    for poly in polys {
        let Some(p0) = poly.first().map(at) else { continue };
        for i in 2..poly.len() {
            let (Some(p1), Some(p2)) = (poly.get(i - 1).map(at), poly.get(i).map(at)) else { continue };
            let (u, v) = ([p1[0] - p0[0], p1[1] - p0[1]], [p2[0] - p1[0], p2[1] - p1[1]]);
            let sign: f32 = if u[0] * v[1] - u[1] * v[0] > 0.0 { 1.0 } else { -1.0 };
            let (ta, first, sec, tc) = triangle_moments(p0, p1, p2);
            area += sign * ta;
            for k in 0..2 {
                centroid[k] += sign * first[k];
                second[k] += sign * sec[k];
            }
            cov += sign * tc;
        }
    }
    if area <= 0.0 {
        return [0.0; 2];
    }
    let c = [centroid[0] / area, centroid[1] / area];
    let var = [second[0] / area - c[0] * c[0], second[1] / area - c[1] * c[1]];
    let covariance = f64::from(cov / area - c[0] * c[1]);
    if covariance.abs() < 1e-4 {
        return if var[1] > var[0] { [var[0], 0.0] } else { [0.0, var[1]] };
    }
    let d = f64::from((var[0] - var[1]) * (var[0] - var[1])) + 4.0 * covariance * covariance;
    let ea = (0.5 * (f64::from(var[0] + var[1]) + d.sqrt())) as f32;
    let eb = (0.5 * (f64::from(var[0] + var[1]) - d.sqrt())) as f32;
    let vector = |e: f32| [(f64::from(e - var[1]) / covariance) as f32, 1.0];
    if ea > eb { vector(eb) } else { vector(ea) }
}

/// The principal axes' shorter direction, as `detect_bridging_direction` takes it.
fn short_axis(polys: &[crate::clipper::Path]) -> Option<DVec2> {
    let [x, y] = second_principal(polys);
    if x == 0.0 && y == 0.0 {
        return None;
    }
    let n = (x * x + y * y).sqrt();
    Some(DVec2::new(f64::from(x / n), f64::from(y / n)))
}

/// `detect_bridging_direction`: the way across that leaves the least edge hanging.
fn bridging_direction(floating: &[[DVec2; 2]], area: &[crate::clipper::Path]) -> DVec2 {
    if floating.is_empty() {
        return short_axis(area).unwrap_or(DVec2::X);
    }
    // In Orca's units, as `Line`s of points.
    let nm = |p: DVec2| DVec2::new((p.x * 1e6).round(), (p.y * 1e6).round());
    let lines: Vec<DVec2> = floating.iter().map(|[a, b]| nm(*b) - nm(*a)).collect();
    // `std::unordered_map<double, Vec2d>` by quantised angle, first normal kept.
    let (mut keys, mut normals): (Vec<f64>, Vec<DVec2>) = (Vec::new(), Vec::new());
    for d in &lines {
        let normal = DVec2::new(d.y, -d.x);
        let n2 = normal.length_squared();
        let normal = if n2 > 0.0 { normal / n2.sqrt() } else { normal };
        let key = (normal.y.atan2(normal.x) * 1000.0).ceil();
        if !keys.contains(&key) {
            keys.push(key);
            normals.push(normal);
        }
    }
    let hashes: Vec<u64> = keys.iter().map(|k| crate::connect::std_hash_f64(*k)).collect();
    let mut best = (f64::MAX, DVec2::ONE);
    for k in crate::connect::std_unordered_order(&hashes) {
        let Some(&dir) = normals.get(k) else { continue };
        let cost = lines.iter().fold(0.0, |acc, l| acc + l.dot(dir).abs());
        if cost < best.0 {
            best = (cost, DVec2::new(dir.y, -dir.x));
        }
    }
    best.1
}

/// An `ExpansionZone`: fill a surface may grow into, and how far.
struct Zone {
    expolys: Vec<ExPoly>,
    params: Params,
    expanded_into: bool,
}

fn trim_zones(zones: &mut [Zone], by: &[Shape]) {
    for z in zones.iter_mut().filter(|z| z.expanded_into) {
        z.expolys = geom::shapes_nm(&diff(&geom::shapes_mm(&z.expolys), by));
    }
}

/// `expand_bridges_detect_orientations`: bridges grow into the zones in waves; those whose
/// waves meet in a zone print as one, at the angle the first of them finds against the zones
/// it touches.
fn expand_bridges(bridges: Vec<ExPoly>, zones: &mut [Zone], closing: f32, origin: DVec2) -> Vec<(Vec<Shape>, f64)> {
    if bridges.is_empty() {
        return Vec::new();
    }
    let n = bridges.len();
    // `expand_expolygons`.
    let (mut anchors, mut expansions) = (Vec::new(), Vec::new());
    let mut processed = 0u32;
    for z in zones.iter_mut() {
        let mut seeds = expansion::wave_seeds(&bridges, &z.expolys, z.params.tiny);
        let mut grown = expansion::propagate_waves_ex(&seeds, &z.expolys, &z.params);
        seeds.iter_mut().for_each(|s| s.boundary += processed);
        grown.iter_mut().for_each(|e| e.boundary += processed);
        z.expanded_into = !grown.is_empty();
        anchors.extend(seeds);
        expansions.extend(grown);
        processed += z.expolys.len() as u32;
    }
    // `get_grouped_bridges`.
    let mut group: Vec<u32> = (0..n as u32).collect();
    fn root(group: &[u32], mut id: u32) -> u32 {
        for _ in 0..group.len() + 1 {
            match group.get(id as usize) {
                Some(&g) if g != id => id = g,
                _ => break,
            }
        }
        id
    }
    let contour_box = |e: &expansion::ExpansionEx| geom::bounds(&geom::shapes_mm(std::slice::from_ref(&e.expoly)));
    let mut i = 0;
    while let Some(first) = expansions.get(i) {
        let end = (i + 1..expansions.len())
            .find(|&j| expansions.get(j).is_some_and(|e| e.boundary != first.boundary))
            .unwrap_or(expansions.len());
        let run = expansions.get(i..end).unwrap_or(&[]);
        let boxes: Vec<_> = run.iter().map(contour_box).collect();
        for (a, ea) in run.iter().enumerate() {
            for (c, ec) in run.iter().enumerate().skip(a + 1) {
                let near = match (boxes.get(a).copied().flatten(), boxes.get(c).copied().flatten()) {
                    (Some((alo, ahi)), Some((clo, chi))) => {
                        alo.x <= chi.x && clo.x <= ahi.x && alo.y <= chi.y && clo.y <= ahi.y
                    }
                    _ => false,
                };
                let touch = || {
                    let (Some(ca), Some(cc)) = (ea.expoly.first(), ec.expoly.first()) else { return false };
                    !geom::intersect_paths(std::slice::from_ref(ca), std::slice::from_ref(cc)).is_empty()
                };
                if ea.src != ec.src && near && touch() {
                    let (id, id2) = (root(&group, ea.src), root(&group, ec.src));
                    if id < id2 {
                        if let Some(g) = group.get_mut(id2 as usize) {
                            *g = id;
                        }
                    } else if let Some(g) = group.get_mut(id as usize) {
                        *g = id2;
                    }
                }
            }
        }
        i = end;
    }
    // `detect_bridge_directions`: each bridge's edges away from the zones it touches.
    crate::connect::std_sort(&mut anchors, &|l: &expansion::Seed, r: &expansion::Seed| {
        l.src < r.src || (l.src == r.src && l.boundary < r.boundary)
    });
    let (ox, oy) = ((origin.x * 1e6).round() as i64, (origin.y * 1e6).round() as i64);
    let mm = |p: &crate::clipper::Pt| DVec2::new(p.x as f64 / 1e6, p.y as f64 / 1e6);
    let mut angles = vec![0.0f64; n];
    let mut k = 0;
    for (id, bridge) in bridges.iter().enumerate() {
        let mut areas: Vec<crate::clipper::Path> = Vec::new();
        let mut last: i64 = -1;
        while let Some(a) = anchors.get(k).filter(|a| a.src as usize == id) {
            if last != i64::from(a.boundary) {
                last = i64::from(a.boundary);
                let mut start = 0usize;
                for z in zones.iter() {
                    let end = start + z.expolys.len();
                    if (a.boundary as usize) < end {
                        areas.extend(z.expolys.get(a.boundary as usize - start).cloned().unwrap_or_default());
                        break;
                    }
                    start = end;
                }
            }
            k += 1;
        }
        let rings: Vec<crate::clipper::Path> = bridge
            .iter()
            .map(|c| {
                let mut r = c.clone();
                r.extend(c.first().copied());
                r
            })
            .collect();
        let clip = geom::offset_polygons(&areas, 100.0);
        let pieces = geom::clip_open(&rings, &clip, crate::clipper::ClipType::Difference);
        let lines: Vec<[DVec2; 2]> = pieces.iter().flat_map(|p| p.windows(2).map(|w| [mm(&w[0]), mm(&w[1])])).collect();
        // Orca's slices sit round the object's origin, which its floats' rounding sees.
        let centred: Vec<crate::clipper::Path> =
            bridge.iter().map(|p| p.iter().map(|q| crate::clipper::Pt::new(q.x - ox, q.y - oy)).collect()).collect();
        let dir = bridging_direction(&lines, &centred);
        if let Some(a) = angles.get_mut(id) {
            *a = std::f64::consts::PI + dir.y.atan2(dir.x);
        }
    }
    // `merge_bridges`.
    crate::connect::std_sort(&mut expansions, &|l: &expansion::ExpansionEx, r: &expansion::ExpansionEx| {
        l.src < r.src || (l.src == r.src && l.boundary < r.boundary)
    });
    let mut out: Vec<(Vec<Shape>, f64)> = Vec::new();
    for id in 0..n as u32 {
        if root(&group, id) != id {
            continue;
        }
        let mut acc: Vec<crate::clipper::Path> = Vec::new();
        for other in id..n as u32 {
            if root(&group, other) != id {
                continue;
            }
            acc.extend(bridges.get(other as usize).cloned().unwrap_or_default());
            for e in expansions.iter().filter(|e| e.src == other) {
                acc.extend(e.expoly.iter().cloned());
            }
        }
        let merged = geom::closing_polygons(&acc, closing);
        if !merged.is_empty() {
            out.push((merged, angles.get(id as usize).copied().unwrap_or(0.0)));
        }
    }
    let all: Vec<Shape> = out.iter().flat_map(|(b, _)| b.iter().cloned()).collect();
    trim_zones(zones, &all);
    out
}

/// `expand_merge_surfaces`: each surface with the waves it sends into the zones.
fn expand_merge(src: Vec<ExPoly>, zones: &mut [Zone], closing: f32) -> Vec<Shape> {
    if src.is_empty() {
        return Vec::new();
    }
    let mut expansions = Vec::new();
    let mut processed = 0u32;
    for z in zones.iter_mut() {
        let seeds = expansion::wave_seeds(&src, &z.expolys, z.params.tiny);
        let mut grown = expansion::propagate_waves(&seeds, &z.expolys, &z.params);
        z.expanded_into = !grown.is_empty();
        grown.iter_mut().for_each(|e| e.boundary += processed);
        processed += z.expolys.len() as u32;
        expansions.extend(grown);
    }
    let merged = expansion::merge_expansions(src, expansions);
    // `closing_ex` of expolygons.
    let expanded = geom::offset2_nm(&geom::shapes_mm(&merged), closing, -closing, Join::Miter);
    trim_zones(zones, &expanded);
    expanded
}

/// `process_external_surfaces`: bridges, bottoms and tops grow into the fill round them.
fn external(f: Fill, s: &Settings, first: bool, h: f64, origin: DVec2) -> Typed {
    let (outer, inner, solid_w) = widths(s, first);
    // `Flow::scaled_width` and `scaled_spacing`: the floats' millimetres, truncated.
    let scaled = |v: f32| (f64::from(v) / 1e-6) as i64;
    let h32 = h as f32;
    let (shell_width, expansion_min): (f32, f32) = if s.wall_loops > 0 {
        let ext_spacing = scaled(crate::walls::flow_spacing(outer as f32, h32));
        let spacing = scaled(crate::walls::flow_spacing(inner as f32, h32));
        let mut w = 0.5f32 * scaled(outer as f32) as f32 + ext_spacing as f32;
        w += (spacing * (s.wall_loops as i64 - 1)) as f32;
        (w, spacing as f32)
    } else {
        (100.0, 100.0)
    };
    let expansion = (f64::from(shell_width) * std::f64::consts::SQRT_2) as f32;
    let step = (0.1f64 / 1e-6) as f32;
    let closing = 0.55f32 * 0.65f32 * 1.05f32 * scaled(crate::walls::flow_spacing(solid_w as f32, h32)) as f32;
    let into_solid = Params::build(expansion, step, 5);
    let into_sparse = Params::build(expansion_min, step, 5);
    let Fill { top, bottom, bridge, internal, solid, .. } = f;
    let zone = |shapes: &[Shape], params: Params| Zone {
        expolys: geom::shapes_nm(&geom::union(shapes)),
        params,
        expanded_into: false,
    };
    let mut zones = vec![zone(&solid, into_solid), zone(&internal, into_sparse), zone(&top, into_solid)];
    let bridges = expand_bridges(geom::shapes_nm(&geom::union(&bridge)), &mut zones, closing, origin);
    // What is left of the top zone is the top surfaces now, which grow next.
    let top_now = zones.pop().map(|z| z.expolys).unwrap_or_default();
    let bottom = expand_merge(geom::shapes_nm(&geom::union(&bottom)), &mut zones, closing);
    let top = expand_merge(top_now, &mut zones, closing);
    let mut shells = zones.first().map(|z| geom::shapes_mm(&z.expolys)).unwrap_or_default();
    let mut sparse = zones.get(1).map(|z| geom::shapes_mm(&z.expolys)).unwrap_or_default();
    if s.density > 0.0 && s.infill.min_sparse_area > 0.0 {
        let (small, big): (Vec<Shape>, Vec<Shape>) =
            sparse.into_iter().partition(|p| geom::shape_area(p) <= s.infill.min_sparse_area);
        sparse = big;
        if !small.is_empty() {
            shells = all(shells, &small);
        }
    }
    Typed { top, bottom, bridges, internal_bridges: Vec::new(), solid: shells, sparse }
}

// ---- Bridges over sparse infill --------------------------------------------------------------

fn rotated(shapes: &[Shape], (sin, cos): (f64, f64)) -> Vec<Shape> {
    shapes.iter().map(|sh| sh.iter().map(|c| c.iter().map(|p| turn(*p, (sin, cos))).collect()).collect()).collect()
}

fn turn(p: DVec2, (sin, cos): (f64, f64)) -> DVec2 {
    DVec2::new(p.x * cos - p.y * sin, p.x * sin + p.y * cos)
}

fn lines_of(shapes: &[Shape]) -> Vec<[DVec2; 2]> {
    shapes
        .iter()
        .flatten()
        .flat_map(|c| (0..c.len()).filter_map(|i| Some([*c.get(i)?, *c.get((i + 1) % c.len())?])))
        .collect()
}

/// The way a bridge runs: square to most of the anchor lines near its outline, by a vote over
/// points 2 mm apart in windows of ±18°.
fn determine_bridging_angle(area: &[Shape], anchors: &[[DVec2; 2]]) -> f64 {
    use std::f64::consts::PI;
    let tree = Outline::of_lines(anchors.to_vec());
    let mut counted: Vec<(f64, i32)> = Vec::new();
    for c in area.iter().flatten() {
        let mut acc = 0.0;
        for w in c.windows(2) {
            let (start, next) = (w[0], w[1]);
            let dist = start.distance(next);
            acc += dist;
            if acc <= 2.0 {
                continue;
            }
            acc = 0.0;
            let v = (next - start).normalize_or_zero();
            let count = (dist / 2.0).ceil() as usize;
            let step = dist / count.max(1) as f64;
            for i in 0..count {
                let q = start + v * (i as f64 * step);
                let Some(line) = tree.nearest(q) else { continue };
                let d = line[1] - line[0];
                let mut angle = d.y.atan2(d.x);
                if angle < 0.0 {
                    angle += 2.0 * PI;
                }
                if angle > PI {
                    angle -= PI;
                }
                counted.push((angle + PI * 0.5, 1));
            }
        }
    }
    counted.sort_by(|a, b| a.0.total_cmp(&b.0));
    counted.dedup_by(|b, a| {
        let same = a.0 == b.0;
        if same {
            a.1 += b.1;
        }
        same
    });
    let mut best = (0.0, 0);
    for &(dir, _) in &counted {
        let (start, end) = (dir - PI * 0.1, dir + PI * 0.1);
        let (mut score, mut acc) = (0, 0.0);
        for &(a, n) in counted.iter().filter(|(a, _)| *a >= start && *a <= end) {
            acc += a * f64::from(n);
            score += n;
        }
        if start < 0.5 * PI {
            for &(a, n) in counted.iter().filter(|(a, _)| *a >= 1.5 * PI - (0.5 * PI - start)) {
                acc += (a - PI) * f64::from(n);
                score += n;
            }
        }
        if start > 1.5 * PI {
            for &(a, n) in counted.iter().filter(|(a, _)| *a <= start - 1.5 * PI) {
                acc += (a + PI) * f64::from(n);
                score += n;
            }
        }
        if score > best.1 {
            best = (acc / f64::from(score), score);
        }
    }
    if best.0 == 0.0 { 0.001 } else { best.0 }
}

/// Lines sorted into the upright rows they cross, so a row need not look at all of them.
fn by_row(lines: &[[DVec2; 2]], x0: f64, spacing: f64, rows: usize) -> Vec<Vec<[DVec2; 2]>> {
    let mut out: Vec<Vec<[DVec2; 2]>> = vec![Vec::new(); rows];
    let row = |x: f64| (x - x0) / spacing - 0.5;
    for line @ [a, b] in lines {
        let (lo, hi) = (a.x.min(b.x), a.x.max(b.x));
        let first = row(lo).floor().max(0.0) as usize;
        let last = (row(hi).ceil().max(0.0) as usize).min(rows.saturating_sub(1));
        for list in out.iter_mut().take(last + 1).skip(first) {
            list.push(*line);
        }
    }
    out
}

/// Crossings of the upright line at `x` with `lines`, by height.
fn crossings(lines: &[[DVec2; 2]], x: f64) -> Vec<f64> {
    let mut ys: Vec<f64> = lines
        .iter()
        .filter(|[a, b]| (a.x <= x) != (b.x <= x))
        .map(|[a, b]| a.y + (x - a.x) / (b.x - a.x) * (b.y - a.y))
        .collect();
    ys.sort_by(f64::total_cmp);
    ys
}

/// `construct_anchored_polygon`: each bridge line, along `angle`, runs on to the first anchor
/// past either end, a line's width beyond.
fn anchored_polygon(area: &[Shape], anchors: &[[DVec2; 2]], width: f64, spacing: f64, angle: f64) -> Vec<Shape> {
    let aligning = -angle + std::f64::consts::FRAC_PI_2;
    let to = aligning.sin_cos();
    let back = (-aligning).sin_cos();
    let turned = rotated(area, to);
    let anchors: Vec<[DVec2; 2]> = anchors.iter().map(|[a, b]| [turn(*a, to), turn(*b, to)]).collect();
    let Some((lo, hi)) = geom::bounds(&turned) else { return Vec::new() };
    let walls = lines_of(&turned);
    let rows = (((hi.x - lo.x) + spacing - 1e-6) / spacing).floor().clamp(0.0, 100_000.0) as usize;
    let limit = 36.0 * spacing * spacing;
    let half = DVec2::new(spacing / 2.0, 0.0);
    let outline = Outline::of_lines(walls.clone());
    let (wall_rows, anchor_rows) = (by_row(&walls, lo.x, spacing, rows), by_row(&anchors, lo.x, spacing, rows));
    struct Traced {
        lows: Vec<DVec2>,
        highs: Vec<DVec2>,
    }
    let mut out: Vec<Shape> = Vec::new();
    let close = |t: Traced, out: &mut Vec<Shape>| {
        let mut c = t.lows;
        c.extend(t.highs.into_iter().rev());
        if c.len() >= 3 {
            if geom::signed_area2(&c) < 0.0 {
                c.reverse();
            }
            out.push(vec![c]);
        }
    };
    let mut open: Vec<Traced> = Vec::new();
    for row in 0..rows {
        let x = lo.x + (row as f64 + 0.5) * spacing;
        let ys = crossings(wall_rows.get(row).map_or(&[][..], Vec::as_slice), x);
        let mut sections: Vec<(f64, f64)> = ys
            .windows(2)
            .filter(|w| outline.inside(DVec2::new(x, (w[0] + w[1]) / 2.0)))
            .map(|w| (w[0], w[1]))
            .collect();
        let marks = crossings(anchor_rows.get(row).map_or(&[][..], Vec::as_slice), x);
        for sec in &mut sections {
            let below = marks.partition_point(|y| *y < sec.0);
            if below > 0
                && let Some(y) = marks.get(below - 1)
            {
                sec.0 = *y - width;
            }
            let above = marks.partition_point(|y| *y <= sec.1);
            if let Some(y) = marks.get(above) {
                sec.1 = *y + width;
            }
        }
        for k in 0..sections.len().saturating_sub(1) {
            let (Some(&a), Some(&b)) = (sections.get(k), sections.get(k + 1)) else { continue };
            let overlaps = (a.0 >= b.0 && a.0 <= b.1)
                || (a.1 >= b.0 && a.1 <= b.1)
                || (b.0 >= a.0 && b.0 <= a.1)
                || (b.1 >= a.0 && b.1 <= a.1);
            if overlaps {
                if let Some(s) = sections.get_mut(k + 1) {
                    *s = (a.0.min(b.0), a.1.max(b.1));
                }
                if let Some(s) = sections.get_mut(k) {
                    s.0 = s.1;
                }
            }
        }
        sections.retain(|s| s.0 != s.1);
        sections.sort_by(|a, b| a.0.total_cmp(&b.0));
        let lines: Vec<(DVec2, DVec2)> = sections.iter().map(|s| (DVec2::new(x, s.0), DVec2::new(x, s.1))).collect();
        let mut used = vec![false; lines.len()];
        let mut still = Vec::with_capacity(open.len());
        for mut t in open.drain(..) {
            let (Some(&low), Some(&high)) = (t.lows.last(), t.highs.last()) else { continue };
            let first = lines.partition_point(|l| l.1.y <= low.y);
            let end = lines.partition_point(|l| l.0.y <= high.y);
            let pick = (first..end).find(|k| !used.get(*k).copied().unwrap_or(true));
            match pick.and_then(|k| lines.get(k).map(|l| (k, *l))) {
                Some((k, (a, b))) => {
                    if low.distance_squared(a) < limit {
                        t.lows.push(a);
                    } else {
                        t.lows.extend([low + half, a - half, a]);
                    }
                    if high.distance_squared(b) < limit {
                        t.highs.push(b);
                    } else {
                        t.highs.extend([high + half, b - half, b]);
                    }
                    if let Some(u) = used.get_mut(k) {
                        *u = true;
                    }
                    still.push(t);
                }
                None => {
                    t.lows.push(low + half);
                    t.highs.push(high + half);
                    close(t, &mut out);
                }
            }
        }
        open = still;
        for (line, was_used) in lines.iter().zip(&used) {
            if !*was_used {
                open.push(Traced { lows: vec![line.0 - half, line.0], highs: vec![line.1 - half, line.1] });
            }
        }
    }
    for t in open {
        close(t, &mut out);
    }
    rotated(&geom::union(&out), back)
}

fn ring_lines(shapes: &[Shape]) -> Vec<Polyline> {
    shapes
        .iter()
        .flatten()
        .map(|c| {
            let mut r = c.clone();
            r.extend(c.first().copied());
            r
        })
        .collect()
}

fn polyline_lines(lines: &[Polyline]) -> Vec<[DVec2; 2]> {
    lines.iter().flat_map(|l| l.windows(2).map(|w| [w[0], w[1]])).collect()
}

/// A solid surface's candidate for bridging, and what becomes of it.
struct Candidate {
    surface: usize,
    polys: Vec<Shape>,
    angle: f64,
}

/// `bridge_over_infill`: solid infill over sparse infill bridges it, anchored on the lines below.
fn bridge_over_infill(
    typed: &mut [Typed],
    layers: &[Input],
    s: &Settings,
    lines_below: &(dyn Fn(usize, &[Shape]) -> Vec<Polyline> + Sync),
) {
    let n = typed.len();
    let solid_spacing = |i: usize| {
        let h = layers.get(i).map_or(s.layer_height, |l| l.height);
        Settings::spacing(widths(s, i == 0).2, h)
    };
    // Which solid surfaces have unsupported parts.
    let mut by_layer: Vec<Vec<Candidate>> = (0..n)
        .into_par_iter()
        .map(|i| {
            let Some(below) = i.checked_sub(1).and_then(|k| typed.get(k).zip(layers.get(k))) else {
                return Vec::new();
            };
            let (Some(this), (lower, lower_in)) = (typed.get(i), below) else { return Vec::new() };
            let spacing = solid_spacing(i);
            let mut lower_solids: Vec<Shape> = lower.top.clone();
            lower_solids.extend_from_slice(&lower.bottom);
            lower_solids.extend_from_slice(&lower.solid);
            lower_solids.extend(lower.bridges.iter().flat_map(|b| b.0.iter().cloned()));
            let lower_solids = geom::offset(&geom::union(&lower_solids), -spacing, Join::Miter);
            let lower_solids = geom::offset(&lower_solids, 4.0 * spacing, Join::Miter);
            let unsupported_area = closing(&lower_in.fill, 1e-4);
            let unsupported_area = geom::offset(&unsupported_area, -3.0 * spacing, Join::Miter);
            let unsupported_area = diff(&unsupported_area, &lower_solids);
            let mut out = Vec::new();
            for (k, surface) in this.solid.iter().enumerate() {
                let one = std::slice::from_ref(surface);
                let unsupported = both(one, &unsupported_area);
                if unsupported.is_empty() {
                    continue;
                }
                let area = geom::area(&unsupported);
                let partly = area < geom::area(one) - 1e-4;
                if partly && area <= 9.0 * spacing * spacing {
                    continue;
                }
                let mut worth = both(one, &geom::offset(&unsupported, 4.0 * spacing, Join::Miter));
                for p in diff(one, &geom::offset(&worth, spacing, Join::Miter)) {
                    let a = geom::shape_area(&p);
                    if a < spacing * 12.0 && a > spacing * spacing {
                        worth.push(p);
                    }
                }
                let worth = both(&closing(&worth, 1e-4), one);
                out.push(Candidate { surface: k, polys: worth, angle: 0.0 });
            }
            out
        })
        .collect();

    // Grown along the bridge lines, lowest layer first: each sees what lies below.
    let thick = if s.bridges.width > 0.0 { s.bridges.width } else { s.nozzle };
    let (flow_w, flow_spacing, target) = (thick, thick + 0.05, thick * 0.9);
    let z_of = |i: usize| layers.iter().take(i + 1).map(|l| l.height).sum::<f64>();
    for lidx in 0..n {
        if by_layer.get(lidx).is_none_or(Vec::is_empty) {
            continue;
        }
        let top_z = z_of(lidx);
        let bottom_z = top_z - target * 0.9 - 1e-4;
        let (mut sparse_near, mut not_sparse) = (Vec::new(), Vec::new());
        for k in (0..lidx).rev() {
            if z_of(k) < bottom_z && k + 1 < lidx {
                break;
            }
            let Some(t) = typed.get(k) else { continue };
            sparse_near.extend_from_slice(&t.sparse);
            not_sparse.extend_from_slice(&t.top);
            not_sparse.extend_from_slice(&t.bottom);
            not_sparse.extend_from_slice(&t.solid);
            not_sparse.extend(t.bridges.iter().flat_map(|b| b.0.iter().cloned()));
        }
        let mut deep = diff(&closing(&geom::union(&sparse_near), 1e-4), &closing(&geom::union(&not_sparse), 1e-4));
        let filled_z = top_z - target - 1e-4;
        for k in (0..lidx).rev() {
            if z_of(k) < filled_z {
                break;
            }
            let done: Vec<Shape> =
                by_layer.get(k).into_iter().flatten().flat_map(|c| c.polys.iter().cloned()).collect();
            if !done.is_empty() {
                deep = diff(&deep, &done);
            }
        }
        let deep = geom::offset(&deep, flow_spacing * 1.5, Join::Miter);
        let Some(this) = typed.get(lidx) else { continue };
        let top_area = geom::union(&this.top);
        let mut expansion = closing(&all(this.sparse.clone(), &this.solid), 1e-4);
        let total_fill = closing(&layers.get(lidx).map_or(Vec::new(), |l| l.fill.clone()), 1e-4);
        expansion = both(&expansion, &deep);
        let below_lines = lines_below(lidx - 1, &deep);
        let anchors = geom::clip_lines(&below_lines, &geom::offset(&expansion, -flow_spacing, Join::Miter));
        let unsupported_core = geom::offset(&deep, -flow_spacing * 4.5, Join::Miter);
        let mut sorted = by_layer.get_mut(lidx).map(std::mem::take).unwrap_or_default();
        let corner = |c: &Candidate| geom::bounds(&c.polys).map_or((0.0, 0.0), |(lo, _)| (lo.x, lo.y));
        sorted.sort_by(|a, b| {
            let (x, y) = (corner(a), corner(b));
            x.0.total_cmp(&y.0).then(x.1.total_cmp(&y.1))
        });
        if sorted.len() > 2 {
            let origin = sorted.first().and_then(|c| geom::bounds(&c.polys)).map_or(DVec2::ZERO, |b| b.1);
            let from = |c: &Candidate| geom::bounds(&c.polys).map_or(0.0, |(lo, _)| lo.distance_squared(origin));
            if let Some(rest) = sorted.get_mut(1..) {
                rest.sort_by(|a, b| from(a).total_cmp(&from(b)));
            }
        }
        let mut done: Vec<Candidate> = Vec::with_capacity(sorted.len());
        for c in sorted {
            let area = both(&geom::offset(&c.polys, flow_spacing, Join::Miter), &deep);
            let area: Vec<Shape> =
                area.into_iter().filter(|p| !both(std::slice::from_ref(p), &unsupported_core).is_empty()).collect();
            if area.is_empty() {
                continue;
            }
            let limiting = all(area.clone(), &expansion);
            let mut boundary = ring_lines(&geom::offset(&total_fill, 1.3 * flow_spacing, Join::Miter));
            boundary.extend(ring_lines(&limiting));
            let mut angle = if anchors.is_empty() {
                determine_bridging_angle(&area, &polyline_lines(&boundary))
            } else {
                determine_bridging_angle(&area, &polyline_lines(&anchors))
            };
            if let Some(set) = s.bridges.internal_angle {
                angle = set.to_radians();
            }
            boundary.extend_from_slice(&anchors);
            let bound = polyline_lines(&boundary);
            let mut bridging = anchored_polygon(&area, &bound, flow_w, flow_spacing, angle);
            let near = geom::offset(&bridging, 3.0 * flow_spacing, Join::Miter);
            if let Some(other) = done.iter().find(|o| !both(&o.polys, &near).is_empty()) {
                angle = other.angle;
                bridging = anchored_polygon(&area, &bound, flow_w, flow_spacing, angle);
            }
            let bridging = closing(&opening(&bridging, flow_spacing * 0.75), flow_spacing);
            let bridging = both(&both(&bridging, &limiting), &total_fill);
            let bridging = diff(&bridging, &top_area);
            expansion = diff(&expansion, &bridging);
            done.push(Candidate { surface: c.surface, polys: bridging, angle });
        }
        if let Some(slot) = by_layer.get_mut(lidx) {
            *slot = done;
        }
    }

    // The infill round them gives way, and the layer below keeps a solid edge under them.
    let extra: Vec<Vec<Shape>> = (0..n)
        .map(|i| {
            by_layer
                .get(i + 1)
                .into_iter()
                .flatten()
                .flat_map(|c| diff(&c.polys, &geom::offset(&c.polys, -solid_spacing(i + 1), Join::Miter)))
                .collect()
        })
        .collect();
    typed.par_iter_mut().zip(by_layer.par_iter()).zip(extra.par_iter()).enumerate().for_each(
        |(i, ((t, here), add))| {
            if here.is_empty() && add.is_empty() {
                return;
            }
            let spacing = solid_spacing(i);
            let cut: Vec<Shape> = here.iter().flat_map(|c| c.polys.iter().cloned()).collect();
            let mut everything: Vec<Shape> = t.top.clone();
            everything.extend_from_slice(&t.bottom);
            everything.extend_from_slice(&t.solid);
            everything.extend_from_slice(&t.sparse);
            everything.extend(t.bridges.iter().flat_map(|b| b.0.iter().cloned()));
            let everything = geom::union(&everything);
            let near = diff(&everything, &geom::offset(&everything, -spacing, Join::Miter));
            let ensuring = both(add, &near);
            t.sparse = diff(&diff(&t.sparse, &cut), &ensuring);
            t.solid = diff(&all(std::mem::take(&mut t.solid), &ensuring), &cut);
            t.internal_bridges =
                here.iter().flat_map(|c| geom::union(&c.polys).into_iter().map(|p| (vec![p], c.angle))).collect();
        },
    );
}

/// Every layer's fill by type. `lines_below` gives a layer's sparse infill lines within an area,
/// which bridges above anchor on.
pub fn prepare(
    layers: &[Input],
    s: &Settings,
    origin: DVec2,
    lines_below: &(dyn Fn(usize, &[Shape]) -> Vec<Polyline> + Sync),
) -> Vec<Typed> {
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
    let mut typed: Vec<Typed> = fills
        .into_par_iter()
        .zip(layers.par_iter())
        .enumerate()
        .map(|(i, (f, l))| external(f, s, i == 0, l.height, origin))
        .collect();
    if s.density > 0.0 && s.density < 0.999 {
        bridge_over_infill(&mut typed, layers, s, lines_below);
    }
    typed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_span_runs_across_the_gap() {
        // Held along the left and right: only top and bottom edges hang.
        let shape: Shape = vec![vec![DVec2::ZERO, DVec2::new(10.0, 0.0), DVec2::new(10.0, 4.0), DVec2::new(0.0, 4.0)]];
        let edges = [[DVec2::ZERO, DVec2::new(10.0, 0.0)], [DVec2::new(10.0, 4.0), DVec2::new(0.0, 4.0)]];
        let d = bridging_direction(&edges, &geom::paths_of(&shape));
        assert!(d.y.abs() < 1e-9 && d.x.abs() > 0.99, "{d:?}");
    }

    #[test]
    fn with_no_loose_edge_a_bridge_takes_the_short_way() {
        let shape: Shape = vec![vec![DVec2::ZERO, DVec2::new(10.0, 0.0), DVec2::new(10.0, 4.0), DVec2::new(0.0, 4.0)]];
        let d = bridging_direction(&[], &geom::paths_of(&shape));
        assert!(d.x.abs() < 1e-6 && d.y.abs() > 0.99, "{d:?}");
    }
}
