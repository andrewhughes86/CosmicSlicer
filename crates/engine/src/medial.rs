//! Centre lines of narrow regions, with the width at each point: where a part is too thin for
//! a normal wall (thin walls) or a gap between walls is too narrow for another (gap fill), one
//! line of varying width fills it, as OrcaSlicer's Arachne and gap fill do.
//!
//! The region is rasterised, an exact Euclidean distance transform gives each pixel's distance
//! to the edge, the region is thinned to a one-pixel skeleton (Zhang–Suen), and the skeleton
//! is traced into polylines whose width at each point is twice that distance.

use glam::DVec2;

use crate::geom::{self, Shape};
use crate::toolpath::{Path, Role};

/// A centre line and its width at each point.
#[derive(Clone, Debug, PartialEq)]
pub struct WideLine {
    pub points: Vec<DVec2>,
    pub widths: Vec<f64>,
}

/// Most pixels rasterised for one region.
const MAX_PIXELS: usize = 4_000_000;

/// 1-D squared distance transform (Felzenszwalb–Huttenlocher) of `f` in place.
fn edt_1d(f: &mut [f64], v: &mut [usize], z: &mut [f64], out: &mut [f64]) {
    let n = f.len();
    if n == 0 {
        return;
    }
    let mut k = 0usize;
    v[0] = 0;
    z[0] = f64::NEG_INFINITY;
    z[1] = f64::INFINITY;
    for q in 1..n {
        loop {
            let p = v[k];
            let s = ((f[q] + (q * q) as f64) - (f[p] + (p * p) as f64)) / (2.0 * q as f64 - 2.0 * p as f64);
            if s <= z[k] && k > 0 {
                k -= 1;
                continue;
            }
            if s <= z[k] {
                // k == 0: replace the first parabola.
                v[0] = q;
                z[0] = f64::NEG_INFINITY;
                z[1] = f64::INFINITY;
                break;
            }
            k += 1;
            v[k] = q;
            z[k] = s;
            z[k + 1] = f64::INFINITY;
            break;
        }
    }
    k = 0;
    for (q, o) in out.iter_mut().enumerate().take(n) {
        while z[k + 1] < q as f64 {
            k += 1;
        }
        let p = v[k];
        *o = (q as f64 - p as f64).powi(2) + f[p];
    }
    f.copy_from_slice(&out[..n]);
}

/// Centre lines of `region` with widths, for widths between `min_width` and `max_width`.
/// Lines shorter than `min_length` are dropped.
pub fn centre_lines(region: &[Shape], min_width: f64, max_width: f64, min_length: f64) -> Vec<WideLine> {
    if !(min_width.is_finite() && max_width.is_finite() && min_width > 0.0) {
        return Vec::new();
    }
    let Some((lo, hi)) = geom::bounds(region) else { return Vec::new() };
    let size = hi - lo;
    // Fine enough to resolve the thinnest width, within the pixel budget.
    let mut cell = (min_width / 6.0).max(0.01);
    let area = (size.x + 4.0 * cell) * (size.y + 4.0 * cell);
    if area / (cell * cell) > MAX_PIXELS as f64 {
        cell = (area / MAX_PIXELS as f64).sqrt();
    }
    if !(cell.is_finite() && cell > 0.0) {
        return Vec::new();
    }
    let origin = lo - DVec2::splat(2.0 * cell);
    let (nx, ny) = (((size.x / cell).ceil() as usize + 4).max(3), ((size.y / cell).ceil() as usize + 4).max(3));
    if nx.saturating_mul(ny) > MAX_PIXELS * 2 {
        return Vec::new();
    }
    let at = |i: usize, j: usize| origin + DVec2::new((i as f64 + 0.5) * cell, (j as f64 + 0.5) * cell);
    // Inside test per row by scanline crossings (fast, exact enough at pixel centres).
    let mut inside = vec![false; nx * ny];
    let edges: Vec<(DVec2, DVec2)> = region
        .iter()
        .flatten()
        .flat_map(|c| (0..c.len()).filter_map(move |k| Some((*c.get(k)?, *c.get((k + 1) % c.len())?))))
        .collect();
    for j in 0..ny {
        let y = at(0, j).y;
        let mut xs: Vec<f64> = edges
            .iter()
            .filter(|(a, b)| (a.y > y) != (b.y > y))
            .map(|(a, b)| a.x + (y - a.y) * (b.x - a.x) / (b.y - a.y))
            .collect();
        xs.sort_by(f64::total_cmp);
        for pair in xs.chunks(2) {
            let [x0, x1] = pair else { continue };
            let i0 = (((x0 - origin.x) / cell) - 0.5).ceil().max(0.0) as usize;
            let i1 = (((x1 - origin.x) / cell) - 0.5).floor().min(nx as f64 - 1.0);
            if i1 < 0.0 {
                continue;
            }
            for i in i0..=(i1 as usize) {
                if let Some(c) = inside.get_mut(j * nx + i) {
                    *c = true;
                }
            }
        }
    }
    // Squared distance (in pixels) from each inside pixel to the nearest outside one.
    let big = ((nx + ny) * (nx + ny)) as f64;
    let mut d: Vec<f64> = inside.iter().map(|&c| if c { big } else { 0.0 }).collect();
    let n = nx.max(ny);
    let (mut f, mut v, mut z, mut out) = (vec![0.0; n], vec![0usize; n], vec![0.0; n + 1], vec![0.0; n]);
    for j in 0..ny {
        let row = &mut d[j * nx..(j + 1) * nx];
        f[..nx].copy_from_slice(row);
        edt_1d(&mut f[..nx], &mut v, &mut z, &mut out);
        row.copy_from_slice(&f[..nx]);
    }
    for i in 0..nx {
        for j in 0..ny {
            f[j] = d[j * nx + i];
        }
        edt_1d(&mut f[..ny], &mut v, &mut z, &mut out);
        for j in 0..ny {
            d[j * nx + i] = f[j];
        }
    }
    // Zhang–Suen thinning to a one-pixel skeleton.
    let mut skel = inside.clone();
    let get = |s: &[bool], i: isize, j: isize| -> bool {
        i >= 0 && j >= 0 && (i as usize) < nx && (j as usize) < ny && s[j as usize * nx + i as usize]
    };
    // Only pixels still set are visited: thin regions in big bounds stay quick.
    let mut live: Vec<usize> = (0..nx * ny).filter(|&k| skel[k]).collect();
    loop {
        let mut changed = false;
        for step in 0..2 {
            let mut remove = Vec::new();
            for &k in &live {
                let (i, j) = (k % nx, k / nx);
                if i == 0 || j == 0 || i + 1 >= nx || j + 1 >= ny || !skel[k] {
                    continue;
                }
                {
                    let (ii, jj) = (i as isize, j as isize);
                    // Neighbours P2..P9 clockwise from north.
                    let p = [
                        get(&skel, ii, jj + 1),
                        get(&skel, ii + 1, jj + 1),
                        get(&skel, ii + 1, jj),
                        get(&skel, ii + 1, jj - 1),
                        get(&skel, ii, jj - 1),
                        get(&skel, ii - 1, jj - 1),
                        get(&skel, ii - 1, jj),
                        get(&skel, ii - 1, jj + 1),
                    ];
                    let b = p.iter().filter(|x| **x).count();
                    if !(2..=6).contains(&b) {
                        continue;
                    }
                    let a = (0..8).filter(|&k| !p[k] && p[(k + 1) % 8]).count();
                    if a != 1 {
                        continue;
                    }
                    let ok = if step == 0 {
                        !(p[0] && p[2] && p[4]) && !(p[2] && p[4] && p[6])
                    } else {
                        !(p[0] && p[2] && p[6]) && !(p[0] && p[4] && p[6])
                    };
                    if ok {
                        remove.push(j * nx + i);
                    }
                }
            }
            changed |= !remove.is_empty();
            for k in remove {
                skel[k] = false;
            }
        }
        live.retain(|&k| skel[k]);
        if !changed {
            break;
        }
    }
    // Trace the skeleton: start at ends (one neighbour), then whatever loops remain.
    let neighbours = |s: &[bool], k: usize| -> Vec<usize> {
        let (i, j) = ((k % nx) as isize, (k / nx) as isize);
        let mut out = Vec::new();
        for (di, dj) in [(0, 1), (1, 1), (1, 0), (1, -1), (0, -1), (-1, -1), (-1, 0), (-1, 1)] {
            if get(s, i + di, j + dj) {
                out.push(((j + dj) as usize) * nx + (i + di) as usize);
            }
        }
        out
    };
    let mut visited = vec![false; nx * ny];
    let mut starts = live;
    starts.sort_by_key(|&k| neighbours(&skel, k).len() != 1);
    let mut lines = Vec::new();
    for s in starts {
        if visited[s] {
            continue;
        }
        let mut chain = vec![s];
        visited[s] = true;
        let mut cur = s;
        loop {
            let next = neighbours(&skel, cur).into_iter().find(|n| !visited[*n]);
            let Some(nk) = next else { break };
            visited[nk] = true;
            chain.push(nk);
            cur = nk;
            if chain.len() > nx * ny {
                break;
            }
        }
        if chain.len() < 2 {
            continue;
        }
        let pts: Vec<DVec2> = chain.iter().map(|&k| at(k % nx, k / nx)).collect();
        let widths: Vec<f64> =
            chain.iter().map(|&k| (2.0 * d[k].sqrt() * cell).max(0.0).min(max_width * 4.0)).collect();
        if geom::length(&pts) < min_length {
            continue;
        }
        lines.push(simplify_wide(&pts, &widths, cell * 0.75, min_width, max_width));
    }
    lines.into_iter().filter(|l| l.points.len() >= 2).collect()
}

/// Splits wide lines into paths of near-constant width (within 15 %), each printed at its
/// mean width; lines shorter than `min_length` overall are dropped.
pub fn to_paths(lines: &[WideLine], role: Role, speed: f64, flow: f64, min_length: f64) -> Vec<Path> {
    let mut out = Vec::new();
    for l in lines {
        if geom::length(&l.points) < min_length {
            continue;
        }
        let mut run: Vec<DVec2> = Vec::new();
        let mut run_w: Vec<f64> = Vec::new();
        let flush = |run: &mut Vec<DVec2>, run_w: &mut Vec<f64>, out: &mut Vec<Path>| {
            if run.len() >= 2 && !run_w.is_empty() {
                let w = run_w.iter().sum::<f64>() / run_w.len() as f64;
                out.push(Path::new(role, std::mem::take(run), false, w, speed, flow));
            }
            run.clear();
            run_w.clear();
        };
        for (p, w) in l.points.iter().zip(&l.widths) {
            let first = run_w.first().copied().unwrap_or(*w);
            if run.len() >= 2 && (w - first).abs() > first * 0.15 {
                // The new run starts where the old one ended, so there is no gap.
                let last = run.last().copied();
                flush(&mut run, &mut run_w, &mut out);
                if let Some(q) = last {
                    run.push(q);
                    run_w.push(*w);
                }
            }
            run.push(*p);
            run_w.push(*w);
        }
        flush(&mut run, &mut run_w, &mut out);
    }
    out
}

/// Simplifies a line, keeping widths at the kept points, clamped to the printable range.
fn simplify_wide(pts: &[DVec2], widths: &[f64], tol: f64, min_w: f64, max_w: f64) -> WideLine {
    let kept = geom::simplify(pts, tol);
    let mut out = WideLine { points: Vec::with_capacity(kept.len()), widths: Vec::with_capacity(kept.len()) };
    let mut search = 0;
    for p in kept {
        let i = (search..pts.len()).find(|&i| pts[i] == p).unwrap_or(search);
        search = i;
        out.points.push(p);
        // min/max, not clamp: the bounds come from settings and may cross.
        out.widths.push(widths.get(i).copied().unwrap_or(min_w).max(min_w).min(max_w));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_thin_strip_gives_its_centre_line() {
        // 20 mm long, 0.6 mm wide.
        let strip = geom::rectangle(DVec2::new(0.0, 0.0), DVec2::new(20.0, 0.6));
        let lines = centre_lines(&[strip], 0.2, 1.0, 0.5);
        assert!(!lines.is_empty());
        let longest = lines.iter().max_by(|a, b| geom::length(&a.points).total_cmp(&geom::length(&b.points)));
        let Some(l) = longest else { return };
        assert!(geom::length(&l.points) > 18.0, "{}", geom::length(&l.points));
        // Down the middle, about as wide as the strip.
        assert!(l.points.iter().all(|p| (p.y - 0.3).abs() < 0.1), "{:?}", l.points);
        let mid = l.widths[l.widths.len() / 2];
        assert!((mid - 0.6).abs() < 0.12, "{mid}");
    }

    #[test]
    fn a_wedge_widens() {
        let wedge: Shape = vec![vec![DVec2::new(0.0, 0.0), DVec2::new(20.0, -0.5), DVec2::new(20.0, 0.5)]];
        let lines = centre_lines(&[wedge], 0.1, 1.2, 0.5);
        let Some(l) = lines.iter().max_by(|a, b| geom::length(&a.points).total_cmp(&geom::length(&b.points))) else {
            return assert!(!lines.is_empty());
        };
        let (narrow, wide) = if l.points[0].x < l.points[l.points.len() - 1].x {
            (l.widths[0], l.widths[l.widths.len() - 1])
        } else {
            (l.widths[l.widths.len() - 1], l.widths[0])
        };
        assert!(wide > narrow + 0.3, "{narrow} → {wide}");
    }

    #[test]
    fn widths_split_into_runs() {
        let l = WideLine {
            points: (0..6).map(|i| DVec2::new(i as f64, 0.0)).collect(),
            widths: vec![0.3, 0.31, 0.3, 0.6, 0.6, 0.61],
        };
        let paths = to_paths(&[l], Role::GapFill, 30.0, 1.0, 0.0);
        assert_eq!(paths.len(), 2);
        assert!((paths[0].width - 0.3).abs() < 0.02 && paths[1].width > 0.5);
        // Joined end to start.
        assert_eq!(paths[0].points.last(), paths[1].points.first());
    }

    #[test]
    fn nothing_from_nothing() {
        assert!(centre_lines(&[], 0.2, 1.0, 0.5).is_empty());
    }
}
