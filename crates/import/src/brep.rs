//! Tessellation of the faces CAD readers (STEP, IGES, SAT) collect: in parallel where threads
//! exist, with a planar fallback for faces the trimmer cannot handle.

use crate::geom::DVec3;
use crate::geom::brep::{Axes, Curve, Surface};
use crate::geom::trim::{Face, tessellate};

/// A face to tessellate: its surface, boundary loops (3D points, shared with its
/// neighbours) and whether its outside is opposite the surface normal.
pub struct FaceJob {
    pub surface: Surface,
    pub loops: Vec<Vec<DVec3>>,
    pub reversed: bool,
    /// `loops[0]` is the outer boundary, whatever the loops' winding.
    pub outer_first: bool,
    /// The file does not say which side is outside; [`orient_outward`] decides.
    pub side_unknown: bool,
}

/// What came of a batch of faces.
#[derive(Default)]
pub struct Tessellated {
    /// Triangles per face (empty for a face that failed).
    pub faces: Vec<Vec<[DVec3; 3]>>,
    /// Faces drawn flat because their surface could not be trimmed.
    pub flattened: usize,
    /// Faces left out.
    pub failed: usize,
}

/// The plane best fitting `loops` (Newell's method), for the fallback.
fn best_plane(loops: &[Vec<DVec3>]) -> Option<Axes> {
    let mut n = DVec3::ZERO;
    let mut c = DVec3::ZERO;
    let mut count = 0.0f64;
    for l in loops {
        for (i, p) in l.iter().enumerate() {
            let q = l.get((i + 1) % l.len()).copied().unwrap_or(*p);
            n += DVec3::new((p.y - q.y) * (p.z + q.z), (p.z - q.z) * (p.x + q.x), (p.x - q.x) * (p.y + q.y));
            c += *p;
            count += 1.0;
        }
    }
    let z = n.try_normalize()?;
    Some(Axes::new(c / count.max(1.0), Some(z), None))
}

/// One face's triangles, whether it was drawn flat, and whether it failed.
type Outcome = (Vec<[DVec3; 3]>, bool, bool);

fn one(job: &FaceJob, tol: f64) -> Outcome {
    match tessellate(
        &Face { surface: &job.surface, loops: &job.loops, reversed: job.reversed, outer_first: job.outer_first },
        tol,
    ) {
        Ok(t) if !t.is_empty() => (t, false, false),
        _ => {
            // Flat: Newell's normal already follows the outer loop's winding.
            let Some(frame) = best_plane(&job.loops) else { return (Vec::new(), false, true) };
            let plane = Surface::Plane(frame);
            match tessellate(
                &Face { surface: &plane, loops: &job.loops, reversed: false, outer_first: job.outer_first },
                tol,
            ) {
                Ok(t) if !t.is_empty() => (t, true, false),
                _ => (Vec::new(), false, true),
            }
        }
    }
}

pub fn tessellate_faces(jobs: &[FaceJob], tol: f64) -> Tessellated {
    let results: Vec<Outcome> = {
        #[cfg(not(target_arch = "wasm32"))]
        {
            let threads = std::thread::available_parallelism().map_or(1, |n| n.get()).clamp(1, 16);
            if threads > 1 && jobs.len() > 8 {
                let chunk = jobs.len().div_ceil(threads * 4).max(1);
                let chunks: Vec<&[FaceJob]> = jobs.chunks(chunk).collect();
                let next = std::sync::atomic::AtomicUsize::new(0);
                let mut parts: Vec<(usize, Vec<Outcome>)> = std::thread::scope(|scope| {
                    let workers: Vec<_> = (0..threads)
                        .map(|_| {
                            scope.spawn(|| {
                                let mut done = Vec::new();
                                loop {
                                    let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                                    let Some(c) = chunks.get(i) else { break };
                                    done.push((i, c.iter().map(|j| one(j, tol)).collect()));
                                }
                                done
                            })
                        })
                        .collect();
                    workers.into_iter().flat_map(|w| w.join().unwrap_or_default()).collect()
                });
                parts.sort_by_key(|(i, _)| *i);
                parts.into_iter().flat_map(|(_, r)| r).collect()
            } else {
                jobs.iter().map(|j| one(j, tol)).collect()
            }
        }
        #[cfg(target_arch = "wasm32")]
        {
            jobs.iter().map(|j| one(j, tol)).collect()
        }
    };
    let mut out = Tessellated::default();
    for (t, flat, failed) in results {
        out.flattened += usize::from(flat);
        out.failed += usize::from(failed);
        out.faces.push(t);
    }
    out
}

/// Warnings for a tessellation's fallbacks.
pub fn warnings(t: &Tessellated, total: usize) -> Vec<String> {
    let mut w = Vec::new();
    if t.flattened > 0 {
        w.push(format!("{} of {total} faces could not be trimmed on their surface and were drawn flat", t.flattened));
    }
    if t.failed > 0 {
        w.push(format!("{} of {total} faces could not be tessellated and were left out", t.failed));
    }
    w
}

/// Turns faces whose side is unknown (IGES trimmed surfaces carry none) so their triangles
/// face out of the solid: a point just off each face along its normal is tested for being
/// inside the model by counting crossings along the three axes (majority vote). On an open
/// surface set the count is meaningless but harmless.
///
/// The probes (one per face) are gridded, not the triangles: a sliver triangle spanning the
/// model would sit in thousands of cells, and a few hundred thousand of them once took GBs.
/// Each triangle visits the probes in the cells it covers, or all of them when that is fewer.
pub fn orient_outward(faces: &mut [Vec<[DVec3; 3]>], unknown: &[bool]) {
    if !unknown.iter().any(|u| *u) {
        return;
    }
    let (lo, hi) = faces
        .iter()
        .flatten()
        .flatten()
        .fold((DVec3::splat(f64::INFINITY), DVec3::splat(f64::NEG_INFINITY)), |(a, b), p| (a.min(*p), b.max(*p)));
    if !(lo.is_finite() && hi.is_finite()) {
        return;
    }
    let size = (hi - lo).length().max(1.0e-9);
    // One probe per face of unknown side: just off its largest triangle, along its normal.
    let probes: Vec<(usize, DVec3)> = faces
        .iter()
        .zip(unknown)
        .enumerate()
        .filter(|(_, (_, u))| **u)
        .filter_map(|(i, (face, _))| {
            let area = |t: &[DVec3; 3]| (t[1] - t[0]).cross(t[2] - t[0]).length();
            let big = face.iter().max_by(|x, y| area(x).total_cmp(&area(y)))?;
            let normal = (big[1] - big[0]).cross(big[2] - big[0]).try_normalize()?;
            Some((i, (big[0] + big[1] + big[2]) / 3.0 + normal * size * 1.0e-5))
        })
        .collect();
    if probes.is_empty() {
        return;
    }
    let n = ((probes.len() as f64).sqrt().ceil()).clamp(1.0, 512.0) as usize;
    let cell = |v: f64, i: usize| {
        (((v - lo[i]) / (hi[i] - lo[i]).max(1.0e-12) * n as f64).floor().max(0.0) as usize).min(n - 1)
    };
    // Crossings above each probe, per axis.
    let mut crossings = vec![[0u32; 3]; probes.len()];
    for k in 0..3 {
        let (a, b) = ((k + 1) % 3, (k + 2) % 3);
        let mut cells: Vec<Vec<u32>> = vec![Vec::new(); n * n];
        for (pi, (_, p)) in probes.iter().enumerate() {
            if let Some(c) = cells.get_mut(cell(p[a], a) * n + cell(p[b], b)) {
                c.push(pi as u32);
            }
        }
        for tri in faces.iter().flatten() {
            let mut test = |pi: u32| {
                let Some((_, p)) = probes.get(pi as usize) else { return };
                let q = |v: DVec3| (v[a] - p[a], v[b] - p[b]);
                let (p0, p1, p2) = (q(tri[0]), q(tri[1]), q(tri[2]));
                // The probe inside the projected triangle, by edge signs (half-open on ties).
                let edge = |u: (f64, f64), w: (f64, f64)| u.0 * w.1 - u.1 * w.0;
                let (d0, d1, d2) = (edge(p0, p1), edge(p1, p2), edge(p2, p0));
                let all_pos = d0 > 0.0 && d1 > 0.0 && d2 > 0.0;
                let all_neg = d0 < 0.0 && d1 < 0.0 && d2 < 0.0;
                if !(all_pos || all_neg) {
                    return;
                }
                let area = d0 + d1 + d2;
                // Height of the crossing along the axis, by barycentric weights.
                let h = (d1 * tri[0][k] + d2 * tri[1][k] + d0 * tri[2][k]) / area;
                if h > p[k]
                    && let Some(c) = crossings.get_mut(pi as usize).and_then(|c| c.get_mut(k))
                {
                    *c += 1;
                }
            };
            let (mut ia0, mut ia1, mut ib0, mut ib1) = (n, 0, n, 0);
            for p in tri {
                let (ca, cb) = (cell(p[a], a), cell(p[b], b));
                ia0 = ia0.min(ca);
                ia1 = ia1.max(ca);
                ib0 = ib0.min(cb);
                ib1 = ib1.max(cb);
            }
            let covered = (ia1.saturating_sub(ia0) + 1) * (ib1.saturating_sub(ib0) + 1);
            if covered >= probes.len() {
                (0..probes.len() as u32).for_each(&mut test);
            } else {
                for i in ia0..=ia1 {
                    for j in ib0..=ib1 {
                        for &pi in cells.get(i * n + j).map_or(&[][..], Vec::as_slice) {
                            test(pi);
                        }
                    }
                }
            }
        }
    }
    for ((face, _), counts) in probes.iter().zip(&crossings) {
        let votes = counts.iter().filter(|c| *c % 2 == 1).count();
        if votes >= 2
            && let Some(face) = faces.get_mut(*face)
        {
            for t in face.iter_mut() {
                t.swap(1, 2);
            }
        }
    }
}

/// An edge's points along `curve` from `p1` to `p2` (its vertices), within `tol`; `same` when the
/// edge runs the way the curve does. Lines are their two ends.
pub fn sample_edge(curve: &Curve, p1: DVec3, p2: DVec3, same: bool, tol: f64) -> Vec<DVec3> {
    if matches!(curve, Curve::Line { .. }) {
        return vec![p1, p2];
    }
    // Walk the curve forwards from where the edge starts along it.
    let (from, to) = if same { (p1, p2) } else { (p2, p1) };
    let eps = tol * 1.0e-3 + 1.0e-9 * from.length();
    let closed_edge = (from - to).length() <= eps;
    let ta = curve.closest(from);
    let mut tb = curve.closest(to);
    let ts: Vec<f64> = if let Some(per) = curve.period() {
        if closed_edge {
            tb = ta + per;
        }
        while tb <= ta + 1.0e-12 {
            tb += per;
        }
        curve.sample(ta, tb, tol)
    } else {
        let (lo, hi) = curve.domain().unwrap_or((ta.min(tb), ta.max(tb)));
        let curve_closed = (curve.point(lo) - curve.point(hi)).length() <= eps.max(tol * 0.01);
        if curve_closed && (closed_edge || tb < ta) {
            // Around a closed spline, through its start.
            let (ta, tb) = if closed_edge { (ta, ta) } else { (ta, tb) };
            let mut a = curve.sample(ta, hi, tol);
            let b = curve.sample(lo, tb, tol);
            if (ta - lo).abs() <= 1.0e-12 * (hi - lo) && closed_edge {
                a = curve.sample(lo, hi, tol);
            } else {
                a.pop();
                a.extend(b);
            }
            a
        } else if tb >= ta {
            curve.sample(ta, tb, tol)
        } else {
            let mut r = curve.sample(tb, ta, tol);
            r.reverse();
            r
        }
    };
    let mut pts: Vec<DVec3> = ts.iter().map(|&t| curve.point(t)).collect();
    if let Some(f) = pts.first_mut() {
        *f = from;
    }
    if let Some(l) = pts.last_mut() {
        *l = to;
    }
    if !same {
        pts.reverse();
    }
    pts
}
