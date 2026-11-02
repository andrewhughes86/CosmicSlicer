//! Tessellation of trimmed surface faces: a surface with boundary loops (already sampled to
//! 3D points, shared with the neighbouring faces) becomes triangles within a chord tolerance.
//!
//! The loops are mapped into the surface's parameter plane. On a closed surface a loop may
//! run once around (a cylinder's end circles) or touch a collapsed point (a sphere's pole, a
//! cone's apex), so the parameter region is assembled from strips cut along a seam whose
//! points both sides share. A constrained Delaunay triangulation of the region is refined
//! until every triangle lies within the tolerance of the surface. Boundary points keep their
//! given positions, so adjacent faces meet exactly.

use std::collections::HashSet;

use glam::DVec3;
use spade::{ConstrainedDelaunayTriangulation, Point2, Triangulation};

use crate::geom::brep::{Pole, Surface, adaptive};

/// Most vertices in one face's triangulation.
const MAX_FACE_VERTICES: usize = 400_000;
/// Most refinement passes.
const MAX_PASSES: usize = 40;

/// A face to tessellate.
pub struct Face<'a> {
    pub surface: &'a Surface,
    /// Closed loops (last point not repeated), outer and inner in any order.
    pub loops: &'a [Vec<DVec3>],
    /// The face's outside is opposite the surface normal.
    pub reversed: bool,
    /// `loops[0]` is the outer boundary and the rest are holes, whatever their winding
    /// (formats such as IGES name the outer loop but promise no direction).
    pub outer_first: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrimError(pub String);

impl std::fmt::Display for TrimError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for TrimError {}

fn fail(what: impl Into<String>) -> TrimError {
    TrimError(what.into())
}

/// Triangles for `face`, wound counter-clockwise seen from its outside, within `tol` of the
/// surface. The triangulator runs under a panic guard.
pub fn tessellate(face: &Face<'_>, tol: f64) -> Result<Vec<[DVec3; 3]>, TrimError> {
    if !tol.is_finite() || tol <= 0.0 {
        return Err(fail("the tolerance must be positive"));
    }
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run(face, tol)))
        .unwrap_or_else(|_| Err(fail("the triangulator failed on this face")))
}

/// A point of the parameter region and the 3D point it stands for.
#[derive(Clone, Copy, Debug)]
struct Uv {
    uv: [f64; 2],
    pos: DVec3,
}

struct Ring {
    pts: Vec<Uv>,
    /// Times the ring runs around each periodic direction.
    wrap: [i64; 2],
}

struct Ctx<'a> {
    s: &'a Surface,
    tol: f64,
    swapped: bool,
    /// Periods after the swap.
    period: [Option<f64>; 2],
    /// Poles after the swap.
    poles: Vec<Pole>,
}

impl Ctx<'_> {
    fn eval(&self, uv: [f64; 2]) -> DVec3 {
        if self.swapped { self.s.point(uv[1], uv[0]) } else { self.s.point(uv[0], uv[1]) }
    }
}

/// Nearest `x + k·period` to `near`.
fn near_copy(x: f64, near: f64, period: f64) -> f64 {
    x + ((near - x) / period).round() * period
}

fn signed_area(pts: &[Uv]) -> f64 {
    let n = pts.len();
    (0..n)
        .map(|i| {
            let (a, b) = (pts[i].uv, pts[(i + 1) % n].uv);
            a[0] * b[1] - b[0] * a[1]
        })
        .sum::<f64>()
        / 2.0
}

fn run(face: &Face<'_>, tol: f64) -> Result<Vec<[DVec3; 3]>, TrimError> {
    let s = face.surface;
    let size = s.size_hint();
    let mut period = s.periods();
    // NURBS (and offsets of them) that close on themselves are periodic for the trimming.
    if let [Some((u0, u1)), Some((v0, v1))] = s.domain() {
        let closed = |along_u: bool| {
            (0..=4).all(|k| {
                let t = f64::from(k) / 4.0;
                let (a, b) = if along_u {
                    (s.point(u0, v0 + (v1 - v0) * t), s.point(u1, v0 + (v1 - v0) * t))
                } else {
                    (s.point(u0 + (u1 - u0) * t, v0), s.point(u0 + (u1 - u0) * t, v1))
                };
                (a - b).length() <= 1.0e-6 * size
            })
        };
        if period[0].is_none() && closed(true) {
            period[0] = Some(u1 - u0);
        }
        if period[1].is_none() && closed(false) {
            period[1] = Some(v1 - v0);
        }
    }
    let poles = s.poles();
    let pole_eps = (1.0e-6 * size).max(tol * 0.05);

    // Loops into the parameter plane, continuous across the periodic directions.
    let mut rings: Vec<(Ring, Vec<Option<Pole>>, usize)> = Vec::new();
    for (index, raw) in face.loops.iter().enumerate() {
        let mut pts: Vec<DVec3> = Vec::with_capacity(raw.len());
        for &p in raw {
            if !p.is_finite() {
                return Err(fail("a boundary point is not finite"));
            }
            if pts.last().is_none_or(|q: &DVec3| (*q - p).length() > 1.0e-12 * size) {
                pts.push(p);
            }
        }
        while pts.len() > 1 && pts.first().zip(pts.last()).is_some_and(|(a, b)| (*a - *b).length() <= 1.0e-12 * size) {
            pts.pop();
        }
        if pts.len() < 3 {
            continue;
        }
        let mut out: Vec<Uv> = Vec::with_capacity(pts.len());
        let mut singular: Vec<Option<Pole>> = Vec::with_capacity(pts.len());
        let mut prev: Option<[f64; 2]> = None;
        for &p in &pts {
            let pole = poles.iter().copied().find(|pl| (p - pl.point).length() <= pole_eps);
            let (u, v) = s.invert(p, prev.map(|q| (q[0], q[1])));
            let mut uv = [u, v];
            if let Some(q) = prev {
                for d in 0..2 {
                    if let Some(per) = period[d] {
                        uv[d] = near_copy(uv[d], q[d], per);
                    }
                }
            }
            if pole.is_none() {
                prev = Some(uv);
            }
            out.push(Uv { uv, pos: p });
            singular.push(pole);
        }
        // How far around the loop goes: the first good point seen again from the last.
        let good: Vec<&Uv> = out.iter().zip(&singular).filter(|(_, s)| s.is_none()).map(|(u, _)| u).collect();
        let mut wrap = [0i64; 2];
        if let (Some(first), Some(last)) = (good.first(), good.last()) {
            for d in 0..2 {
                if let Some(per) = period[d] {
                    let again = near_copy(first.uv[d], last.uv[d], per);
                    wrap[d] = ((again - first.uv[d]) / per).round() as i64;
                }
            }
        } else {
            continue;
        }
        rings.push((Ring { pts: out, wrap }, singular, index));
    }
    if rings.is_empty() && !(period[0].is_some() || period[1].is_some()) {
        return Err(fail("the face has no boundary"));
    }

    // Work with the wrapping direction as u.
    let wraps_u = rings.iter().any(|(r, _, _)| r.wrap[0] != 0);
    let wraps_v = rings.iter().any(|(r, _, _)| r.wrap[1] != 0);
    if rings.iter().any(|(r, _, _)| r.wrap[0] != 0 && r.wrap[1] != 0) || (wraps_u && wraps_v) {
        return Err(fail("loops wind around both directions of the surface"));
    }
    let swapped = wraps_v || (!wraps_u && period[0].is_none() && period[1].is_some());
    let ctx = Ctx {
        s,
        tol,
        swapped,
        period: if swapped { [period[1], period[0]] } else { period },
        poles: poles.iter().map(|p| Pole { along_u: p.along_u != swapped, ..*p }).collect(),
    };
    let sense = if face.reversed != swapped { -1.0 } else { 1.0 };
    for (r, sing, index) in &mut rings {
        if swapped {
            for p in &mut r.pts {
                p.uv.swap(0, 1);
            }
            r.wrap.swap(0, 1);
            for pl in sing.iter_mut().flatten() {
                pl.along_u = !pl.along_u;
            }
        }
        let (pts, crossed) = unfold_poles(&ctx, &r.pts, sing);
        r.pts = pts;
        if crossed {
            r.wrap[0] = 0;
        }
        // A named outer loop runs counter-clockwise about the outside, holes the other way.
        if face.outer_first && r.wrap == [0, 0] && (signed_area(&r.pts) * sense > 0.0) != (*index == 0) {
            r.pts.reverse();
        }
    }
    let rings: Vec<Ring> = rings.into_iter().map(|(r, _, _)| r).filter(|r| r.pts.len() >= 3).collect();

    let polygons = region(&ctx, rings, face.reversed)?;
    triangulate(&ctx, &polygons, face.reversed != swapped)
}

/// Replaces each run of loop points at a pole by points along the pole line, from the
/// parameter of the point before it to that of the point after. A loop through a pole cannot
/// also wind around the surface, so where the direction along a periodic pole line is in
/// doubt (half a cone has two equal choices) the loop is made to wind zero times. Returns the
/// new points and whether a periodic pole line was crossed.
fn unfold_poles(ctx: &Ctx<'_>, pts: &[Uv], singular: &[Option<Pole>]) -> (Vec<Uv>, bool) {
    let n = pts.len();
    let good = |i: usize| singular.get(i).is_some_and(Option::is_none);
    let Some(first_good) = (0..n).find(|&i| good(i)) else { return (Vec::new(), false) };
    if (0..n).all(good) {
        return (pts.to_vec(), false);
    }
    // Start at a good point so no run of pole points straddles the ends.
    let order: Vec<usize> = (0..n).map(|k| (first_good + k) % n).collect();
    let at = |k: usize| order.get(k).and_then(|&i| pts.get(i)).copied();
    let pole_at = |k: usize| order.get(k).and_then(|&i| singular.get(i).copied().flatten());
    // Runs of pole points: (first index, last index, pole), in walking order.
    let mut runs: Vec<(usize, usize, Pole)> = Vec::new();
    let mut k = 0;
    while k < n {
        if let Some(p) = pole_at(k) {
            let start = k;
            while k + 1 < n && pole_at(k + 1).is_some() {
                k += 1;
            }
            runs.push((start, k, p));
        }
        k += 1;
    }
    let free = runs.first().map_or(0, |r| if r.2.along_u { 0 } else { 1 });
    let period = ctx.period[free];
    // Travel along the free direction between good neighbours, and across each run.
    let value = |k: usize| at(k % n).map_or(0.0, |p| p.uv[free]);
    let mut deltas: Vec<f64> = runs
        .iter()
        .map(|&(a, b, _)| {
            let (from, to) = (value(a - 1), value(b + 1));
            match period {
                Some(per) => near_copy(to, from, per) - from,
                None => to - from,
            }
        })
        .collect();
    let crossed = period.is_some();
    if let Some(per) = period {
        let mut rest = 0.0;
        for k in 0..n {
            let j = (k + 1) % n;
            if pole_at(k).is_none() && pole_at(j).is_none() {
                let (a, b) = (value(k), value(j));
                rest += if j == 0 { near_copy(b, a, per) - a } else { b - a };
            }
        }
        let total = rest + deltas.iter().sum::<f64>();
        if let Some(last) = deltas.last_mut() {
            *last -= (total / per).round() * per;
        }
    }
    let mut out = Vec::with_capacity(n + 64);
    let mut shift = 0.0;
    let mut k = 0;
    let mut r = 0;
    while k < n {
        match runs.get(r) {
            Some(&(a, b, pole)) if a == k => {
                let from = value(a - 1) + shift;
                let delta = deltas.get(r).copied().unwrap_or(0.0);
                let to = from + delta;
                let span = period.unwrap_or(1.0);
                let steps = (((to - from).abs() / span) * 48.0).ceil().clamp(1.0, 96.0) as usize;
                for j in 0..=steps {
                    let mut uv = [0.0; 2];
                    uv[free] = from + (to - from) * j as f64 / steps as f64;
                    uv[1 - free] = pole.value;
                    out.push(Uv { uv, pos: pole.point });
                }
                // Points after the run continue from where the pole line ended.
                shift += delta - (value(b + 1) - value(a - 1));
                k = b + 1;
                r += 1;
            }
            _ => {
                if let Some(mut p) = at(k) {
                    p.uv[free] += shift;
                    out.push(p);
                }
                k += 1;
            }
        }
    }
    (out, crossed)
}

/// One side of a strip of the parameter region.
enum Bound<'r> {
    Ring(&'r Ring),
    Pole(Pole),
    /// A seam line `v = value` of a surface closed in v (its copy at `value + period`).
    Iso(f64),
}

/// The polygons (outer boundaries and holes, by the even-odd rule) of the face's region.
fn region(ctx: &Ctx<'_>, rings: Vec<Ring>, reversed: bool) -> Result<Vec<Vec<Uv>>, TrimError> {
    let Some(pu) = ctx.period[0] else {
        // An open surface: the loops are the region as they stand.
        return Ok(rings.into_iter().map(|r| r.pts).collect());
    };
    // Outer loops run counter-clockwise about the face's outside.
    let sense = if reversed != ctx.swapped { -1.0 } else { 1.0 };
    let (wrapping, plain): (Vec<Ring>, Vec<Ring>) = rings.into_iter().partition(|r| r.wrap[0] != 0);
    let mut holes: Vec<Ring> = Vec::new();
    let mut outers: Vec<Ring> = Vec::new();
    for r in plain {
        if signed_area(&r.pts) * sense > 0.0 { outers.push(r) } else { holes.push(r) }
    }
    let lo_pole = ctx.poles.iter().filter(|p| p.along_u).min_by(|a, b| a.value.total_cmp(&b.value)).copied();
    let hi_pole = ctx.poles.iter().filter(|p| p.along_u).max_by(|a, b| a.value.total_cmp(&b.value)).copied();

    if wrapping.is_empty() {
        if !outers.is_empty() {
            let mut polys: Vec<Vec<Uv>> = outers.iter().map(|r| r.pts.clone()).collect();
            for h in holes {
                polys.push(place_hole(ctx, h.pts, &outers));
            }
            return Ok(polys);
        }
        // Only holes (or nothing): the whole closed surface minus them.
        let v_closed = ctx.period[1].is_some();
        let both_poles = lo_pole.zip(hi_pole).is_some_and(|(a, b)| b.value > a.value);
        if !(v_closed || both_poles) {
            // Not a closed surface after all: take the loops as they are.
            return Ok(holes.into_iter().map(|r| r.pts).collect());
        }
        let us = seam_u(&holes, pu, None);
        let mut polys = Vec::new();
        if let (Some(pv), false) = (ctx.period[1], both_poles) {
            let vs = seam_v(&holes, pv);
            polys.push(strip(ctx, Bound::Iso(vs), Bound::Iso(vs + pv), us)?);
            for h in holes {
                polys.push(shift_into(h.pts, us, pu, Some((vs, pv))));
            }
        } else if let (Some(lo), Some(hi)) = (lo_pole, hi_pole) {
            polys.push(strip(ctx, Bound::Pole(lo), Bound::Pole(hi), us)?);
            for h in holes {
                polys.push(shift_into(h.pts, us, pu, None));
            }
        }
        return Ok(polys);
    }

    // Loops around the surface bound strips, bottom to top; poles close odd ones out.
    let mut wrapping = wrapping;
    let mean_v = |r: &Ring| r.pts.iter().map(|p| p.uv[1]).sum::<f64>() / r.pts.len().max(1) as f64;
    wrapping.sort_by(|a, b| mean_v(a).total_cmp(&mean_v(b)));
    let is_top = |r: &Ring| (r.wrap[0] as f64) * sense < 0.0;
    if let Some(pv) = ctx.period[1] {
        // Closed in v too (a torus): start from a lower side, moving tops a period up.
        let mut turns = 0;
        while wrapping.first().is_some_and(is_top) && turns < wrapping.len() {
            let mut r = wrapping.remove(0);
            for p in &mut r.pts {
                p.uv[1] += pv;
            }
            wrapping.push(r);
            turns += 1;
        }
    }
    let sorted: Vec<&Ring> = wrapping.iter().collect();
    let mut bounds: Vec<Bound<'_>> = sorted.iter().map(|r| Bound::Ring(r)).collect();
    if bounds.len() % 2 == 1 {
        let lowest_is_top = sorted.first().is_some_and(|r| is_top(r));
        let below = lo_pole.filter(|p| sorted.first().is_some_and(|r| p.value < mean_v(r)));
        let above = hi_pole.filter(|p| sorted.last().is_some_and(|r| p.value > mean_v(r)));
        match (below, above) {
            (Some(p), Some(_)) if lowest_is_top => bounds.insert(0, Bound::Pole(p)),
            (Some(p), None) => bounds.insert(0, Bound::Pole(p)),
            (_, Some(p)) => bounds.push(Bound::Pole(p)),
            (None, None) => return Err(fail("a loop around the surface has nothing to close against")),
        }
    }
    let first_ring = sorted.first().map(|r| r.pts.as_slice()).unwrap_or_default();
    let us = seam_u(&holes, pu, first_ring.first().map(|p| p.uv[0]));
    let mut polys = Vec::new();
    let mut it = bounds.into_iter();
    while let (Some(lower), Some(upper)) = (it.next(), it.next()) {
        polys.push(strip(ctx, lower, upper, us)?);
    }
    for h in holes {
        polys.push(shift_into(h.pts, us, pu, None));
    }
    Ok(polys)
}

/// `pts` moved by whole periods so its middle lies in `[us, us + pu)` (and `[vs, vs + pv)`).
fn shift_into(mut pts: Vec<Uv>, us: f64, pu: f64, v: Option<(f64, f64)>) -> Vec<Uv> {
    let (lo, hi) = pts.iter().fold((f64::INFINITY, f64::NEG_INFINITY), |(a, b), p| (a.min(p.uv[0]), b.max(p.uv[0])));
    let mid = (lo + hi) / 2.0;
    let k = ((mid - us) / pu).floor();
    for p in &mut pts {
        p.uv[0] -= k * pu;
    }
    if let Some((vs, pv)) = v {
        let (lo, hi) =
            pts.iter().fold((f64::INFINITY, f64::NEG_INFINITY), |(a, b), p| (a.min(p.uv[1]), b.max(p.uv[1])));
        let k = (((lo + hi) / 2.0 - vs) / pv).floor();
        for p in &mut pts {
            p.uv[1] -= k * pv;
        }
    }
    pts
}

/// A hole moved by whole periods into the outer loop that holds it.
fn place_hole(ctx: &Ctx<'_>, pts: Vec<Uv>, outers: &[Ring]) -> Vec<Uv> {
    let probe = pts.first().map(|p| p.uv).unwrap_or_default();
    let pu = ctx.period[0].unwrap_or(0.0);
    let pv = ctx.period[1].unwrap_or(0.0);
    for ku in [0.0, -1.0, 1.0, -2.0, 2.0] {
        for kv in [0.0, -1.0, 1.0] {
            if (kv != 0.0 && pv == 0.0) || (ku != 0.0 && pu == 0.0) {
                continue;
            }
            let q = [probe[0] + ku * pu, probe[1] + kv * pv];
            if outers.iter().any(|o| inside_polygon(&o.pts, q)) {
                return pts
                    .into_iter()
                    .map(|p| Uv { uv: [p.uv[0] + ku * pu, p.uv[1] + kv * pv], pos: p.pos })
                    .collect();
            }
        }
    }
    pts
}

fn inside_polygon(pts: &[Uv], q: [f64; 2]) -> bool {
    let n = pts.len();
    let mut inside = false;
    for i in 0..n {
        let (a, b) = (pts[i].uv, pts[(i + 1) % n].uv);
        if (a[1] > q[1]) != (b[1] > q[1]) && q[0] < a[0] + (q[1] - a[1]) / (b[1] - a[1]) * (b[0] - a[0]) {
            inside = !inside;
        }
    }
    inside
}

/// A seam position in u clear of the holes (or `default` when there are none).
fn seam_u(holes: &[Ring], period: f64, default: Option<f64>) -> f64 {
    let intervals: Vec<(f64, f64)> = holes
        .iter()
        .map(|h| h.pts.iter().fold((f64::INFINITY, f64::NEG_INFINITY), |(a, b), p| (a.min(p.uv[0]), b.max(p.uv[0]))))
        .collect();
    gap_middle(&intervals, period).or(default).unwrap_or(0.0)
}

fn seam_v(holes: &[Ring], period: f64) -> f64 {
    let intervals: Vec<(f64, f64)> = holes
        .iter()
        .map(|h| h.pts.iter().fold((f64::INFINITY, f64::NEG_INFINITY), |(a, b), p| (a.min(p.uv[1]), b.max(p.uv[1]))))
        .collect();
    gap_middle(&intervals, period).unwrap_or(0.0)
}

/// The middle of the widest gap between intervals on a circle of `period`.
fn gap_middle(intervals: &[(f64, f64)], period: f64) -> Option<f64> {
    if intervals.is_empty() {
        return None;
    }
    let mut iv: Vec<(f64, f64)> = intervals
        .iter()
        .map(|&(a, b)| {
            let a0 = a.rem_euclid(period);
            (a0, a0 + (b - a).min(period))
        })
        .collect();
    iv.sort_by(|a, b| a.0.total_cmp(&b.0));
    // Merge overlaps (on the unrolled line), then look at the gaps, wrapping around.
    let mut merged: Vec<(f64, f64)> = Vec::new();
    for (a, b) in iv {
        match merged.last_mut() {
            Some(last) if a <= last.1 => last.1 = last.1.max(b),
            _ => merged.push((a, b)),
        }
    }
    let mut best = (f64::NEG_INFINITY, 0.0);
    for i in 0..merged.len() {
        let end = merged[i].1;
        let next = if i + 1 < merged.len() { merged[i + 1].0 } else { merged[0].0 + period };
        if next - end > best.0 {
            best = (next - end, (end + next) / 2.0);
        }
    }
    Some(best.1)
}

/// Points strictly between `a` and `b` on the straight parameter line, close enough
/// together that the surface between them stays within the tolerance.
fn line_points(ctx: &Ctx<'_>, a: [f64; 2], b: [f64; 2]) -> Vec<Uv> {
    let at = |t: f64| [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t];
    let seeds: Vec<f64> = (0..=8).map(|k| f64::from(k) / 8.0).collect();
    let ts = adaptive(&seeds, ctx.tol, &|t| ctx.eval(at(t)));
    ts.iter().filter(|&&t| t > 0.0 && t < 1.0).map(|&t| Uv { uv: at(t), pos: ctx.eval(at(t)) }).collect()
}

/// A ring walked from point `start` once around so that u moves by `+period·dir`, its u
/// continuous and the first point moved to `first_u`.
fn walk_ring(ring: &Ring, start: usize, dir: f64, period: f64, first_u: f64) -> Vec<Uv> {
    let n = ring.pts.len();
    let forward = (ring.wrap[0] as f64) * dir > 0.0;
    let mut out: Vec<Uv> = Vec::with_capacity(n + 1);
    for k in 0..=n {
        let i = if forward { (start + k) % n } else { (start + n - k % n) % n };
        let Some(&p) = ring.pts.get(i) else { continue };
        let mut q = p;
        match out.last() {
            Some(prev) => q.uv[0] = near_copy(q.uv[0], prev.uv[0], period),
            None => q.uv[0] = first_u,
        }
        // The closing point is the start once around, exactly.
        if k == n {
            q.uv[0] = first_u + dir * period;
            q.uv[1] = out.first().map_or(q.uv[1], |f| f.uv[1]);
        }
        out.push(q);
    }
    out
}

/// The point of `ring` whose u is nearest `u` around the period.
fn nearest_u(ring: &Ring, u: f64, period: f64) -> usize {
    let mut best = (0usize, f64::INFINITY);
    for (i, p) in ring.pts.iter().enumerate() {
        let d = (near_copy(p.uv[0], u, period) - u).abs();
        if d < best.1 {
            best = (i, d);
        }
    }
    best.0
}

/// The polygon of the strip between `lower` and `upper`, cut along a seam near `us`.
fn strip(ctx: &Ctx<'_>, lower: Bound<'_>, upper: Bound<'_>, us: f64) -> Result<Vec<Uv>, TrimError> {
    let pu = ctx.period[0].ok_or_else(|| fail("a strip on an open surface"))?;
    let pole_line = |p: &Pole, from: f64, to: f64| -> Vec<Uv> {
        let k = 48usize;
        (0..=k).map(|j| Uv { uv: [from + (to - from) * j as f64 / k as f64, p.value], pos: p.point }).collect()
    };
    let iso_line = |v: f64, from: f64| -> Vec<Uv> {
        let a = [from, v];
        let b = [from + pu, v];
        let mut pts = vec![Uv { uv: a, pos: ctx.eval(a) }];
        pts.extend(line_points(ctx, a, b));
        pts.push(Uv { uv: b, pos: pts.first().map_or(ctx.eval(b), |f| f.pos) });
        pts
    };
    // Lower side, left to right (u increasing by one period).
    let low: Vec<Uv> = match &lower {
        Bound::Ring(r) => {
            let i = nearest_u(r, us, pu);
            let first = r.pts.get(i).map_or(us, |p| near_copy(p.uv[0], us, pu));
            walk_ring(r, i, 1.0, pu, first)
        }
        Bound::Pole(p) => pole_line(p, us, us + pu),
        Bound::Iso(v) => iso_line(*v, us),
    };
    let Some(a) = low.first().copied() else {
        return Err(fail("an empty strip side"));
    };
    // Upper side, right to left, starting above the end of the lower side.
    let up: Vec<Uv> = match &upper {
        Bound::Ring(r) => {
            let i = nearest_u(r, a.uv[0], pu);
            let first = r.pts.get(i).map_or(a.uv[0], |p| near_copy(p.uv[0], a.uv[0], pu)) + pu;
            walk_ring(r, i, -1.0, pu, first)
        }
        Bound::Pole(p) => pole_line(p, a.uv[0] + pu, a.uv[0]),
        Bound::Iso(v) => {
            // The copy of the lower seam line one period up: same points, reversed.
            let base = v - ctx.period[1].unwrap_or(0.0);
            let mut line = iso_line(base, a.uv[0]);
            for p in &mut line {
                p.uv[1] = *v;
            }
            line.reverse();
            line
        }
    };
    let Some(b) = up.last().copied() else {
        return Err(fail("an empty strip side"));
    };
    // The seam from A up to B, shared by its copy one period to the right.
    let seam = line_points(ctx, a.uv, b.uv);
    let mut poly = low;
    poly.extend(seam.iter().map(|p| Uv { uv: [p.uv[0] + pu, p.uv[1]], pos: p.pos }));
    poly.extend(up);
    poly.extend(seam.iter().rev().copied());
    Ok(poly)
}

/// Even-odd point-in-region test over many polygon edges, bucketed by v.
struct Inside {
    bands: Vec<Vec<[f64; 4]>>,
    y0: f64,
    dy: f64,
}

impl Inside {
    fn new(polys: &[Vec<[f64; 2]>]) -> Self {
        let edges: Vec<[f64; 4]> = polys
            .iter()
            .flat_map(|p| (0..p.len()).map(move |i| (p[i], p[(i + 1) % p.len()])))
            .filter(|(a, b)| a[1] != b[1])
            .map(|(a, b)| [a[0], a[1], b[0], b[1]])
            .collect();
        let (y0, y1) = edges
            .iter()
            .fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), e| (lo.min(e[1]).min(e[3]), hi.max(e[1]).max(e[3])));
        let n = (edges.len() / 8).clamp(1, 4096);
        let dy = if y1 > y0 { (y1 - y0) / n as f64 } else { 1.0 };
        let mut bands = vec![Vec::new(); n];
        for e in edges {
            let lo = (((e[1].min(e[3]) - y0) / dy).floor().max(0.0) as usize).min(n - 1);
            let hi = (((e[1].max(e[3]) - y0) / dy).floor().max(0.0) as usize).min(n - 1);
            for band in bands.iter_mut().take(hi + 1).skip(lo) {
                band.push(e);
            }
        }
        Self { bands, y0, dy }
    }

    fn contains(&self, q: [f64; 2]) -> bool {
        let i = ((q[1] - self.y0) / self.dy).floor();
        if i.is_nan() || i < 0.0 {
            return false;
        }
        let Some(band) = self.bands.get(i as usize) else { return false };
        let mut inside = false;
        for e in band {
            if (e[1] > q[1]) != (e[3] > q[1]) && q[0] < e[0] + (q[1] - e[1]) / (e[3] - e[1]) * (e[2] - e[0]) {
                inside = !inside;
            }
        }
        inside
    }
}

type Cdt = ConstrainedDelaunayTriangulation<Point2<f64>>;

/// A vertex's output position and the surface point at its parameters. They differ only on
/// boundaries that stray from their surface; refinement measures against the surface point.
type Spot = (DVec3, DVec3);

/// Adds a point (its positions kept by vertex index); a repeated point is the same vertex.
fn insert(
    cdt: &mut Cdt,
    pos: &mut Vec<Spot>,
    xy: [f64; 2],
    spot: Spot,
) -> Result<spade::handles::FixedVertexHandle, TrimError> {
    let xy = snap(xy);
    let h = cdt.insert(Point2::new(xy[0], xy[1])).map_err(|e| fail(format!("triangulation: {e:?}")))?;
    if h.index() == pos.len() {
        pos.push(spot);
    }
    Ok(h)
}

/// Which inner faces lie inside the region, by the parity of constraint edges crossed on the
/// way in from the outer face. Indexed by face index; `None` for faces not reached.
fn classify(cdt: &Cdt) -> Vec<Option<bool>> {
    let count = cdt.inner_faces().map(|f| f.fix().index() + 1).max().unwrap_or(0);
    let mut inside: Vec<Option<bool>> = vec![None; count];
    let mut queue = std::collections::VecDeque::new();
    for f in cdt.inner_faces() {
        for e in f.adjacent_edges() {
            if e.rev().face().is_outer()
                && let Some(slot) = inside.get_mut(f.fix().index())
                && slot.is_none()
            {
                *slot = Some(e.is_constraint_edge());
                queue.push_back(f.fix());
            }
        }
    }
    while let Some(h) = queue.pop_front() {
        let f = cdt.face(h);
        let here = inside.get(h.index()).copied().flatten().unwrap_or(false);
        for e in f.adjacent_edges() {
            if let Some(nb) = e.rev().face().as_inner()
                && let Some(slot) = inside.get_mut(nb.fix().index())
                && slot.is_none()
            {
                *slot = Some(here != e.is_constraint_edge());
                queue.push_back(nb.fix());
            }
        }
    }
    inside
}

/// Adds an interior point unless it lands on a boundary edge (which it would split, leaving
/// the neighbouring face a T-junction) or on an existing vertex.
/// Coordinates so close to zero that the triangulator refuses them (files carry values
/// like 1e-48), made zero.
fn snap(xy: [f64; 2]) -> [f64; 2] {
    xy.map(|v| if v.abs() < 1.0e-30 { 0.0 } else { v })
}

/// Returns whether a vertex was added.
fn insert_interior(cdt: &mut Cdt, pos: &mut Vec<Spot>, xy: [f64; 2], spot: Spot) -> Result<bool, TrimError> {
    let xy = snap(xy);
    match cdt.locate(Point2::new(xy[0], xy[1])) {
        spade::PositionInTriangulation::OnVertex(_) => return Ok(false),
        spade::PositionInTriangulation::OnEdge(e) if cdt.is_constraint_edge(e.as_undirected()) => return Ok(false),
        _ => {}
    }
    let before = pos.len();
    insert(cdt, pos, xy, spot)?;
    Ok(pos.len() > before)
}

fn triangulate(ctx: &Ctx<'_>, polygons: &[Vec<Uv>], flip: bool) -> Result<Vec<[DVec3; 3]>, TrimError> {
    let all = || polygons.iter().flatten();
    if all().next().is_none() {
        return Err(fail("the face's region is empty"));
    }
    let (lo, hi) = all().fold(([f64::INFINITY; 2], [f64::NEG_INFINITY; 2]), |(lo, hi), p| {
        ([lo[0].min(p.uv[0]), lo[1].min(p.uv[1])], [hi[0].max(p.uv[0]), hi[1].max(p.uv[1])])
    });
    // Scale u and v so parameter distances match surface distances on average.
    let mut su = 0.0;
    let mut sv = 0.0;
    for i in 0..5 {
        for j in 0..5 {
            let uv = [
                lo[0] + (hi[0] - lo[0]) * (f64::from(i) + 0.5) / 5.0,
                lo[1] + (hi[1] - lo[1]) * (f64::from(j) + 0.5) / 5.0,
            ];
            let (du, dv) = if ctx.swapped { ctx.s.derivatives(uv[1], uv[0]) } else { ctx.s.derivatives(uv[0], uv[1]) };
            let (du, dv) = if ctx.swapped { (dv, du) } else { (du, dv) };
            su += du.length();
            sv += dv.length();
        }
    }
    let su = if su > 1.0e-12 { su / 25.0 } else { 1.0 };
    let sv = if sv > 1.0e-12 { sv / 25.0 } else { 1.0 };
    let scaled = |uv: [f64; 2]| [uv[0] * su, uv[1] * sv];
    let unscale = |x: f64, y: f64| [x / su, y / sv];
    let scaled_polys: Vec<Vec<[f64; 2]>> = polygons.iter().map(|p| p.iter().map(|q| scaled(q.uv)).collect()).collect();
    let inside = Inside::new(&scaled_polys);

    let mut cdt: Cdt = ConstrainedDelaunayTriangulation::new();
    let mut pos: Vec<Spot> = Vec::new();
    let mut constrained = true;
    let planar = matches!(ctx.s, Surface::Plane(_));
    for (poly, sp) in polygons.iter().zip(&scaled_polys) {
        let mut handles = Vec::with_capacity(poly.len());
        for (q, xy) in poly.iter().zip(sp) {
            let surf = if planar { q.pos } else { ctx.eval(q.uv) };
            handles.push(insert(&mut cdt, &mut pos, *xy, (q.pos, surf))?);
        }
        for i in 0..handles.len() {
            let (a, b) = (handles[i], handles[(i + 1) % handles.len()]);
            if a != b {
                if cdt.can_add_constraint(a, b) {
                    cdt.add_constraint(a, b);
                } else {
                    constrained = false;
                }
            }
        }
    }
    // Inside by the constraint parity when every boundary edge went in, else by the
    // centroid (which slivers along straight parameter lines can fool).
    let keep =
        |parity: &[Option<bool>],
         f: spade::handles::FaceHandle<'_, spade::handles::InnerTag, Point2<f64>, (), spade::CdtEdge<()>, ()>| {
            if constrained {
                parity.get(f.fix().index()).copied().flatten().unwrap_or(false)
            } else {
                let xy = f.vertices().map(|h| h.position());
                inside.contains([(xy[0].x + xy[1].x + xy[2].x) / 3.0, (xy[0].y + xy[1].y + xy[2].y) / 3.0])
            }
        };
    if !planar {
        // A coarse grid of interior points gives the refinement something to start from.
        let size = [(hi[0] - lo[0]) * su, (hi[1] - lo[1]) * sv];
        let step = size[0].max(size[1]) / 8.0;
        if step > 0.0 {
            let nx = ((size[0] / step).ceil() as usize).clamp(1, 64);
            let ny = ((size[1] / step).ceil() as usize).clamp(1, 64);
            for i in 1..nx {
                for j in 1..ny {
                    // Off the grid lines by an odd fraction, so seeds rarely meet straight boundaries.
                    let xy = [
                        lo[0] * su + size[0] * (i as f64 - 0.381_966) / nx as f64,
                        lo[1] * sv + size[1] * (j as f64 - 0.381_966) / ny as f64,
                    ];
                    if inside.contains(xy) {
                        let p = ctx.eval(unscale(xy[0], xy[1]));
                        insert_interior(&mut cdt, &mut pos, xy, (p, p))?;
                    }
                }
            }
        }
        // Split edges and triangles that stray from the surface.
        for _ in 0..MAX_PASSES {
            let mut add: Vec<[f64; 2]> = Vec::new();
            let mut seen: HashSet<[u64; 2]> = HashSet::new();
            let parity = if constrained { classify(&cdt) } else { Vec::new() };
            for f in cdt.inner_faces() {
                if !keep(&parity, f) {
                    continue;
                }
                let v = f.vertices();
                let xy = v.map(|h| h.position());
                let c = [(xy[0].x + xy[1].x + xy[2].x) / 3.0, (xy[0].y + xy[1].y + xy[2].y) / 3.0];
                let p = v.map(|h| pos.get(h.fix().index()).map_or(DVec3::ZERO, |s| s.1));
                let mut split = false;
                for e in f.adjacent_edges() {
                    if e.is_constraint_edge() {
                        continue;
                    }
                    let [from, to] = e.vertices();
                    let (a, b) = (from.position(), to.position());
                    let m = [(a.x + b.x) / 2.0, (a.y + b.y) / 2.0];
                    let pa = pos.get(from.fix().index()).map_or(DVec3::ZERO, |s| s.1);
                    let pb = pos.get(to.fix().index()).map_or(DVec3::ZERO, |s| s.1);
                    if (ctx.eval(unscale(m[0], m[1])) - (pa + pb) / 2.0).length() > ctx.tol {
                        if seen.insert([m[0].to_bits(), m[1].to_bits()]) {
                            add.push(m);
                        }
                        split = true;
                    }
                }
                if !split && (ctx.eval(unscale(c[0], c[1])) - (p[0] + p[1] + p[2]) / 3.0).length() > ctx.tol {
                    add.push(c);
                }
            }
            if add.is_empty() || pos.len() + add.len() > MAX_FACE_VERTICES {
                break;
            }
            let mut added = false;
            for xy in add {
                if inside.contains(xy) {
                    let p = ctx.eval(unscale(xy[0], xy[1]));
                    added |= insert_interior(&mut cdt, &mut pos, xy, (p, p))?;
                }
            }
            // Only points that would split the boundary are left: nothing more to gain.
            if !added {
                break;
            }
        }
    }
    let mut out = Vec::new();
    let parity = if constrained { classify(&cdt) } else { Vec::new() };
    for f in cdt.inner_faces() {
        if !keep(&parity, f) {
            continue;
        }
        let v = f.vertices();
        let p = v.map(|h| pos.get(h.fix().index()).map_or(DVec3::ZERO, |s| s.0));
        out.push(if flip { [p[0], p[2], p[1]] } else { p });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::brep::{Axes, Curve};

    fn circle(frame: Axes, r: f64, n: usize, reverse: bool) -> Vec<DVec3> {
        let c = Curve::Circle { frame, radius: r };
        let mut pts: Vec<DVec3> = (0..n).map(|k| c.point(std::f64::consts::TAU * k as f64 / n as f64)).collect();
        if reverse {
            pts.reverse();
        }
        pts
    }

    fn area_normal(tris: &[[DVec3; 3]]) -> DVec3 {
        tris.iter().map(|t| (t[1] - t[0]).cross(t[2] - t[0]) / 2.0).sum()
    }

    fn max_off(tris: &[[DVec3; 3]], dist: impl Fn(DVec3) -> f64) -> f64 {
        tris.iter()
            .flat_map(|t| [(t[0] + t[1]) / 2.0, (t[1] + t[2]) / 2.0, (t[0] + t[2]) / 2.0, (t[0] + t[1] + t[2]) / 3.0])
            .map(|p| dist(p).abs())
            .fold(0.0, f64::max)
    }

    #[test]
    fn a_plane_with_a_hole() -> Result<(), TrimError> {
        let s = Surface::Plane(Axes::WORLD);
        let outer =
            vec![DVec3::ZERO, DVec3::new(10.0, 0.0, 0.0), DVec3::new(10.0, 10.0, 0.0), DVec3::new(0.0, 10.0, 0.0)];
        let hole = circle(Axes::new(DVec3::new(5.0, 5.0, 0.0), None, None), 2.0, 32, true);
        let tris = tessellate(&Face { surface: &s, loops: &[outer, hole], reversed: false, outer_first: false }, 0.01)?;
        let n = area_normal(&tris);
        let hole_area = 0.5 * 32.0 * 4.0 * (std::f64::consts::TAU / 32.0).sin();
        assert!((n.z - (100.0 - hole_area)).abs() < 1e-9, "{n}");
        assert!(n.x.abs() < 1e-9 && n.y.abs() < 1e-9);
        let tris = tessellate(
            &Face { surface: &s, loops: &[vec![DVec3::ZERO, DVec3::X, DVec3::Y]], reversed: true, outer_first: false },
            0.01,
        )?;
        assert!(area_normal(&tris).z < 0.0);
        Ok(())
    }

    #[test]
    fn coordinates_next_to_zero_are_accepted() -> Result<(), TrimError> {
        // Files carry values like -8e-49 for zero, which the triangulator refuses as they stand.
        let s = Surface::Plane(Axes::new(DVec3::new(-8.8e-16, 2.0, -8.3e-49), Some(DVec3::Z), None));
        let lp = vec![
            DVec3::new(-8.3e-49, 0.0, 0.0),
            DVec3::new(1.0, 1.0e-45, 0.0),
            DVec3::new(1.0, 1.0, 0.0),
            DVec3::new(0.0, 1.0, 0.0),
        ];
        let tris = tessellate(&Face { surface: &s, loops: &[lp], reversed: false, outer_first: false }, 0.01)?;
        assert_eq!(tris.len(), 2);
        Ok(())
    }

    #[test]
    fn a_cylinder_between_two_circles() -> Result<(), TrimError> {
        // A Parasolid-style face: two end circles and no seam edge.
        let f = Axes::WORLD;
        let s = Surface::Cylinder { frame: f, radius: 5.0 };
        let bottom = circle(f, 5.0, 64, false);
        let top = circle(Axes::new(DVec3::new(0.0, 0.0, 8.0), None, None), 5.0, 64, true);
        let tris = tessellate(&Face { surface: &s, loops: &[bottom, top], reversed: false, outer_first: false }, 0.01)?;
        // Closed around: the area normals cancel; every triangle faces outward.
        assert!(area_normal(&tris).length() < 1e-6, "{}", area_normal(&tris));
        for t in &tris {
            let c = (t[0] + t[1] + t[2]) / 3.0;
            let n = (t[1] - t[0]).cross(t[2] - t[0]);
            assert!(n.dot(DVec3::new(c.x, c.y, 0.0)) > 0.0);
        }
        let area: f64 = tris.iter().map(|t| (t[1] - t[0]).cross(t[2] - t[0]).length() / 2.0).sum();
        let expect = 64.0 * 5.0 * 2.0 * (std::f64::consts::PI / 64.0).sin() * 8.0;
        assert!((area - expect).abs() < 1e-3 * expect, "{area} vs {expect}");
        Ok(())
    }

    #[test]
    fn a_hemisphere_closes_at_its_pole() -> Result<(), TrimError> {
        let f = Axes::WORLD;
        let s = Surface::Sphere { frame: f, radius: 10.0 };
        let equator = circle(f, 10.0, 128, false);
        let tris = tessellate(&Face { surface: &s, loops: &[equator], reversed: false, outer_first: false }, 0.01)?;
        let off = max_off(&tris, |p| p.length() - 10.0);
        assert!(off <= 0.0101, "{off}");
        assert!(tris.iter().all(|t| t.iter().all(|p| p.z >= -1e-9)), "the upper half");
        let n = area_normal(&tris);
        assert!(n.z > 0.0 && (n.z - std::f64::consts::PI * 100.0).abs() < 0.5, "{n}");
        Ok(())
    }

    #[test]
    fn a_whole_sphere_and_a_torus_band() -> Result<(), TrimError> {
        let f = Axes::WORLD;
        let s = Surface::Sphere { frame: f, radius: 3.0 };
        let tris = tessellate(&Face { surface: &s, loops: &[], reversed: false, outer_first: false }, 0.01)?;
        assert!(area_normal(&tris).length() < 1e-6);
        assert!(max_off(&tris, |p| p.length() - 3.0) <= 0.0101);

        // A fillet: the outer half of a torus, between its top and bottom circles.
        let t = Surface::Torus { frame: f, major: 10.0, minor: 2.0 };
        let top = circle(Axes::new(DVec3::new(0.0, 0.0, 2.0), None, None), 10.0, 128, true);
        let bottom = circle(Axes::new(DVec3::new(0.0, 0.0, -2.0), None, None), 10.0, 128, false);
        let tris = tessellate(&Face { surface: &t, loops: &[bottom, top], reversed: false, outer_first: false }, 0.01)?;
        assert!(
            tris.iter().all(|tr| tr.iter().all(|p| (p.x * p.x + p.y * p.y).sqrt() >= 10.0 - 1e-6)),
            "outer half only"
        );
        let tube = |p: DVec3| ((p.x * p.x + p.y * p.y).sqrt() - 10.0).hypot(p.z) - 2.0;
        let off = max_off(&tris, tube);
        let worst = tris.iter().map(|t| (t, max_off(&[*t], tube))).max_by(|a, b| a.1.total_cmp(&b.1));
        assert!(off <= 0.012, "{off} {worst:?}");
        Ok(())
    }

    #[test]
    fn a_cylinder_with_a_seam_edge() -> Result<(), TrimError> {
        // One loop: bottom circle, up the seam, top circle backwards, down the seam.
        let f = Axes::WORLD;
        let s = Surface::Cylinder { frame: f, radius: 5.0 };
        let mut lp = circle(f, 5.0, 64, false);
        lp.push(DVec3::new(5.0, 0.0, 0.0));
        lp.push(DVec3::new(5.0, 0.0, 4.0));
        let mut top = circle(Axes::new(DVec3::new(0.0, 0.0, 8.0), None, None), 5.0, 64, true);
        top.rotate_right(1);
        top.push(DVec3::new(5.0, 0.0, 8.0));
        lp.extend(top);
        lp.push(DVec3::new(5.0, 0.0, 4.0));
        let tris = tessellate(&Face { surface: &s, loops: &[lp], reversed: false, outer_first: false }, 0.01)?;
        let area: f64 = tris.iter().map(|t| (t[1] - t[0]).cross(t[2] - t[0]).length() / 2.0).sum();
        assert!(area_normal(&tris).length() < 1e-6, "{} {area} {}", area_normal(&tris), tris.len());
        let area: f64 = tris.iter().map(|t| (t[1] - t[0]).cross(t[2] - t[0]).length() / 2.0).sum();
        assert!((area - std::f64::consts::TAU * 5.0 * 8.0).abs() < 0.5, "{area}");
        Ok(())
    }
}

#[cfg(test)]
mod cone_tests {
    use super::*;
    use crate::geom::brep::{Axes, Curve};

    /// Edges used once by the triangles, against the loop's own segments.
    fn border_mismatch(tris: &[[DVec3; 3]], loops: &[Vec<DVec3>]) -> (usize, usize) {
        let key = |p: DVec3| [p.x.to_bits(), p.y.to_bits(), p.z.to_bits()];
        let mut edges: std::collections::HashMap<([u64; 3], [u64; 3]), i32> = std::collections::HashMap::new();
        for t in tris {
            for k in 0..3 {
                let (a, b) = (key(t[k]), key(t[(k + 1) % 3]));
                *edges.entry((a.min(b), a.max(b))).or_insert(0) += 1;
            }
        }
        let mut want = HashSet::new();
        for l in loops {
            for k in 0..l.len() {
                let (a, b) = (key(l[k]), key(l[(k + 1) % l.len()]));
                want.insert((a.min(b), a.max(b)));
            }
        }
        let border: HashSet<_> = edges.iter().filter(|(_, n)| **n == 1).map(|(k, _)| *k).collect();
        (want.difference(&border).count(), border.difference(&want).count())
    }

    #[test]
    fn half_a_drill_point_through_its_apex() -> Result<(), TrimError> {
        // A drill point split in two: up one generator to the apex, down another, round the rim.
        let frame = Axes::new(DVec3::new(0.0, 0.0, 0.0), Some(-DVec3::Z), Some(DVec3::X));
        let tan = 1.6642794823505316;
        let s = Surface::Cone { frame, radius: 0.0, tan };
        let v = 6.0;
        let r = v * tan;
        let rim =
            Curve::Circle { frame: Axes::new(DVec3::new(0.0, 0.0, -v), Some(-DVec3::Z), Some(DVec3::X)), radius: r };
        let mut lp = vec![rim.point(std::f64::consts::PI), DVec3::ZERO];
        for k in 0..=36 {
            lp.push(rim.point(std::f64::consts::PI * f64::from(k) / 36.0));
        }
        lp.pop();
        lp.rotate_left(0);
        let loops = vec![lp];
        for reversed in [false, true] {
            let tris = tessellate(&Face { surface: &s, loops: &loops, reversed, outer_first: false }, 0.01)?;
            assert_eq!(border_mismatch(&tris, &loops), (0, 0), "reversed {reversed}");
        }
        Ok(())
    }
}
