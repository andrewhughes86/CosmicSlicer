//! Centre lines of narrow regions, with the width at each point: where a part is too thin for
//! a normal wall (thin walls) or a gap between walls is too narrow for another (gap fill), one
//! line of varying width fills it, as OrcaSlicer's Arachne and gap fill do.
//!
//! The region is rasterised, an exact Euclidean distance transform gives each pixel's distance
//! to the edge, the region is thinned to a one-pixel skeleton (Zhang–Suen), and the skeleton
//! is traced into polylines whose width at each point is twice that distance.

use std::panic::{AssertUnwindSafe, catch_unwind};

use boostvoronoi::prelude::*;
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

/// A centre line and the width at each end of each of its segments (OrcaSlicer's
/// `ThickPolyline`), mm.
#[derive(Clone, Debug, Default)]
pub struct Thick {
    pub points: Vec<DVec2>,
    /// Two for each segment: at its start and at its end.
    pub width: Vec<f64>,
    /// Whether each end is a real end, not a junction.
    pub ends: (bool, bool),
}

impl Thick {
    fn length(&self) -> f64 {
        self.points.windows(2).map(|w| w[0].distance(w[1])).sum()
    }

    fn reverse(&mut self) {
        self.points.reverse();
        self.width.reverse();
        self.ends = (self.ends.1, self.ends.0);
    }
}

const NM: f64 = 1e6;
const SCALED_EPSILON: f64 = 100.0;

#[derive(Clone, Copy, Default)]
struct EdgeData {
    active: bool,
    start: f64,
    end: f64,
}

struct Axis {
    diagram: Diagram,
    lines: Vec<[DVec2; 2]>,
    data: Vec<EdgeData>,
    min: f64,
    max: f64,
}

fn segment_distance(line: &[DVec2; 2], p: DVec2) -> f64 {
    let d = line[1] - line[0];
    let l = d.length_squared();
    if l <= 0.0 {
        return line[0].distance(p);
    }
    (line[0] + d * ((p - line[0]).dot(d) / l).clamp(0.0, 1.0)).distance(p)
}

impl Axis {
    fn point(&self, v: Option<VertexIndex>) -> Option<DVec2> {
        let v = self.diagram.vertex(v?).ok()?;
        Some(DVec2::new(v.x(), v.y()))
    }

    fn ends(&self, e: EdgeIndex) -> Option<(DVec2, DVec2)> {
        Some((self.point(self.diagram.edge_get_vertex0(e).ok()?)?, self.point(self.diagram.edge_get_vertex1(e).ok()?)?))
    }

    /// The input line of a cell, and the end of it the cell holds if it holds a point.
    fn site(&self, e: EdgeIndex) -> Option<(usize, Option<DVec2>)> {
        let cell = self.diagram.cell(self.diagram.edge_get_cell(e).ok()?).ok()?;
        let line = self.lines.get(cell.source_index().usize())?;
        let (_, category) = cell.source_index_2();
        let point = match category {
            SourceCategory::SegmentStart => Some(line[0]),
            SourceCategory::SegmentEnd => Some(line[1]),
            _ => None,
        };
        Some((cell.source_index().usize(), point))
    }

    /// `validate_edge`: whether this edge is a stretch of centre line, its widths if so.
    fn validate(&self, e: EdgeIndex) -> Option<(f64, f64)> {
        let (a, b) = self.ends(e)?;
        let twin = self.diagram.edge_get_twin(e).ok()?;
        let (left, left_point) = self.site(e)?;
        let (right, right_point) = self.site(twin)?;
        let (seg_l, seg_r) = (*self.lines.get(left)?, *self.lines.get(right)?);
        let w0 = right_point.map_or_else(|| segment_distance(&seg_r, a), |p| p.distance(a)) * 2.0;
        let w1 = left_point.map_or_else(|| segment_distance(&seg_l, b), |p| p.distance(b)) * 2.0;
        if left_point.is_none() && right_point.is_none() {
            let orient = |l: &[DVec2; 2]| {
                let d = l[1] - l[0];
                let a = d.y.atan2(d.x);
                if a < 0.0 { a + std::f64::consts::TAU } else { a }
            };
            let mut angle = (orient(&seg_r) - orient(&seg_l)).abs();
            if angle > std::f64::consts::PI {
                angle = std::f64::consts::TAU - angle;
            }
            if std::f64::consts::PI - angle > std::f64::consts::PI / 8.0
                && (w0 < SCALED_EPSILON || w1 < SCALED_EPSILON || a.distance(b) >= self.min)
            {
                return None;
            }
        } else if w0 < SCALED_EPSILON || w1 < SCALED_EPSILON {
            return None;
        }
        ((w0 >= self.min || w1 >= self.min) && (w0 <= self.max || w1 <= self.max)).then_some((w0, w1))
    }

    /// `process_edge_neighbors`: follows the line while there is one way on.
    fn walk(&mut self, from: EdgeIndex, line: &mut Thick) {
        let mut edge = from;
        for _ in 0..self.data.len() + 1 {
            let Ok(twin) = self.diagram.edge_get_twin(edge) else { return };
            let mut neighbour = self.diagram.edge_rot_next(twin);
            let (mut count, mut first) = (0, None);
            for _ in 0..64 {
                let Some(n) = neighbour.filter(|n| *n != twin) else { break };
                if self.data.get(n.usize() / 2).is_some_and(|d| d.active) {
                    if count == 0 {
                        first = Some(n);
                    }
                    count += 1;
                }
                neighbour = self.diagram.edge_rot_next(n);
            }
            match (count, first) {
                (1, Some(next)) => {
                    let flipped = next.usize() % 2 == 1;
                    let Some(d) = self.data.get_mut(next.usize() / 2) else { return };
                    d.active = false;
                    let (start, end) = (d.start, d.end);
                    let Some((_, to)) = self.ends(next) else { return };
                    line.points.push(to);
                    if flipped {
                        line.width.extend([end, start]);
                    } else {
                        line.width.extend([start, end]);
                    }
                    edge = next;
                }
                (0, _) => {
                    line.ends.1 = true;
                    return;
                }
                _ => return,
            }
        }
    }
}

/// Runs the Voronoi builder, which has no error path for everything bad input can do.
fn voronoi(lines: &[[DVec2; 2]]) -> Option<Diagram> {
    let segments: Vec<[i32; 4]> = lines.iter().map(|[a, b]| [a.x as i32, a.y as i32, b.x as i32, b.y as i32]).collect();
    catch_unwind(AssertUnwindSafe(|| Builder::<i32>::default().with_segments(segments.iter()).ok()?.build().ok()))
        .ok()
        .flatten()
}

/// OrcaSlicer's `ExPolygon::medial_axis`: the centre lines of `shape` where it is `min` to `max`
/// wide, from its Voronoi diagram, run out to the edge. `None` when that cannot be built.
pub fn medial_axis(shape: &Shape, min: f64, max: f64) -> Option<Vec<Thick>> {
    let (min, max) = (min * NM, max * NM);
    let scaled: Shape = shape.iter().map(|c| c.iter().map(|p| (*p * NM).round()).collect()).collect();
    if scaled.iter().flatten().any(|p| p.abs().max_element() > 2.0e9) {
        return None;
    }
    let lines: Vec<[DVec2; 2]> = scaled
        .iter()
        .flat_map(|c| (0..c.len()).filter_map(|i| Some([*c.get(i)?, *c.get((i + 1) % c.len())?])))
        .filter(|[a, b]| a != b)
        .collect();
    if lines.len() < 3 {
        return None;
    }
    let diagram = voronoi(&lines)?;
    let inside = |v: Option<VertexIndex>, d: &Diagram| -> bool {
        let Some(vertex) = v.and_then(|v| d.vertex(v).ok()) else { return false };
        !vertex.is_site_point() && geom::contains(std::slice::from_ref(&scaled), DVec2::new(vertex.x(), vertex.y()))
    };
    let count = diagram.edges().len() / 2;
    let mut axis = Axis { diagram, lines, data: vec![EdgeData::default(); count], min, max };
    for i in 0..count {
        let e = axis.diagram.edge_index_unchecked(i * 2);
        let Ok(edge) = axis.diagram.edge(e) else { continue };
        if !edge.is_primary() || axis.diagram.edge_is_infinite(e).unwrap_or(true) {
            continue;
        }
        let v0 = axis.diagram.edge_get_vertex0(e).ok().flatten();
        let v1 = axis.diagram.edge_get_vertex1(e).ok().flatten();
        if !(inside(v0, &axis.diagram) || inside(v1, &axis.diagram)) {
            continue;
        }
        if let Some((w0, w1)) = axis.validate(e)
            && let Some(d) = axis.data.get_mut(i)
        {
            *d = EdgeData { active: true, start: w0, end: w1 };
        }
    }
    let mut out: Vec<Thick> = Vec::new();
    for i in 0..count {
        if !axis.data.get(i).is_some_and(|d| d.active) {
            continue;
        }
        let e = axis.diagram.edge_index_unchecked(i * 2);
        let Some(d) = axis.data.get_mut(i) else { continue };
        d.active = false;
        let (start, end) = (d.start, d.end);
        let Some((a, b)) = axis.ends(e) else { continue };
        let mut line = Thick { points: vec![a, b], width: vec![start, end], ends: (false, false) };
        axis.walk(e, &mut line);
        let mut back = Thick::default();
        if let Ok(twin) = axis.diagram.edge_get_twin(e) {
            axis.walk(twin, &mut back);
        }
        back.points.reverse();
        back.width.reverse();
        let mut points = back.points;
        points.extend(line.points);
        let mut width = back.width;
        width.extend(line.width);
        let closed = points.first() == points.last();
        out.push(Thick { points, width, ends: if closed { (false, false) } else { (back.ends.1, line.ends.1) } });
    }

    // Each line runs out to the edge, and short ones go.
    let max_w = out.iter().flat_map(|l| l.width.iter().copied()).fold(0.0, f64::max);
    let contour_lines: Vec<[DVec2; 2]> = scaled
        .first()
        .map_or(Vec::new(), |c| (0..c.len()).filter_map(|i| Some([*c.get(i)?, *c.get((i + 1) % c.len())?])).collect());
    let on_boundary = |p: DVec2| axis.lines.iter().any(|l| segment_distance(l, p) <= SCALED_EPSILON);
    let hit = |a: DVec2, b: DVec2| -> Option<DVec2> {
        contour_lines.iter().find_map(|[c, d]| {
            let (r, q) = (b - a, *d - *c);
            let det = r.perp_dot(q);
            if det.abs() < 1e-9 {
                return None;
            }
            let t = (*c - a).perp_dot(q) / det;
            let u = (*c - a).perp_dot(r) / det;
            ((0.0..=1.0).contains(&t) && (0.0..=1.0).contains(&u)).then(|| a + r * t)
        })
    };
    let mut removed = false;
    out.retain_mut(|line| {
        let n = line.points.len();
        let (Some(&front), Some(&back), Some(&second), Some(&before)) =
            (line.points.first(), line.points.last(), line.points.get(1), line.points.get(n.wrapping_sub(2)))
        else {
            return false;
        };
        let (mut new_front, mut new_back) = (front, back);
        if line.ends.0 && !on_boundary(front) {
            let p2 = if n == 2 { (front + second) * 0.5 } else { second };
            let p1 = front - (p2 - front).normalize_or_zero() * max;
            new_front = hit(p1, p2).unwrap_or(front);
        }
        if line.ends.1 && !on_boundary(back) {
            let p1 = if n == 2 { (before + back) * 0.5 } else { before };
            let p2 = back + (back - p1).normalize_or_zero() * max;
            new_back = hit(p1, p2).unwrap_or(back);
        }
        if let Some(p) = line.points.first_mut() {
            *p = new_front;
        }
        if let Some(p) = line.points.last_mut() {
            *p = new_back;
        }
        let keep = !((line.ends.0 || line.ends.1) && line.length() < max_w * 2.0);
        removed |= !keep;
        keep
    });
    // With short branches gone, what met at them can join up.
    if removed {
        let mut i = 0;
        while i < out.len() {
            let mut j = i + 1;
            while j < out.len() {
                let (Some(a), Some(b)) = (out.get(i), out.get(j)) else { break };
                if a.ends.0 && a.ends.1 {
                    break;
                }
                let (af, al) = (a.points.first().copied(), a.points.last().copied());
                let (bf, bl) = (b.points.first().copied(), b.points.last().copied());
                let (flip_a, flip_b) = if al == bl {
                    (false, true)
                } else if af == bl {
                    (true, true)
                } else if af == bf {
                    (true, false)
                } else if al == bf {
                    (false, false)
                } else {
                    j += 1;
                    continue;
                };
                let mut other = out.remove(j);
                if flip_b {
                    other.reverse();
                }
                if let Some(line) = out.get_mut(i) {
                    if flip_a {
                        line.reverse();
                    }
                    line.points.extend(other.points.into_iter().skip(1));
                    line.width.extend(other.width);
                    line.ends.1 = other.ends.1;
                }
                j = i + 1;
            }
            i += 1;
        }
    }
    for line in &mut out {
        line.points.iter_mut().for_each(|p| *p /= NM);
        line.width.iter_mut().for_each(|w| *w /= NM);
    }
    Some(out)
}

/// OrcaSlicer's `variable_width`: a thick line as paths of near-constant width (within 0.05 mm),
/// each at the average of its stretch and widened back from spacing to extrusion width.
pub fn variable_width(lines: &[Thick], role: Role, speed: f64, flow: f64, height: f64) -> Vec<Path> {
    const TOLERANCE: f64 = 0.05;
    let extra_f32 = (1.0 - 0.25 * std::f64::consts::PI) as f32;
    let mut out = Vec::new();
    for thick in lines {
        // (from, to, width at from, width at to)
        let mut segs: Vec<(DVec2, DVec2, f64, f64)> = thick
            .points
            .windows(2)
            .enumerate()
            .map(|(i, w)| {
                let a = thick.width.get(2 * i).copied().unwrap_or(0.0);
                let b = thick.width.get(2 * i + 1).copied().unwrap_or(a);
                (w[0], w[1], a, b)
            })
            .collect();
        let mut paths: Vec<Path> = Vec::new();
        let mut push = |run: &[(DVec2, DVec2, f64, f64)], ending: bool| {
            let (Some(first), Some(last)) = (run.first(), run.last()) else { return };
            let (mut length, mut sum) = (0.0, 0.0);
            for s in run {
                let l = s.0.distance(s.1);
                length += l;
                sum += if ending { l * s.2 } else { l * 0.5 * (s.2 + s.3) };
            }
            if length <= 1e-4 {
                return;
            }
            let mut points: Vec<DVec2> = run.iter().map(|s| s.0).collect();
            points.push(last.1);
            let _ = first;
            // `flow.with_width(unscale<float>(w) + height * float(1 - π/4))`, in Orca's floats.
            let width = f64::from(((sum / length) / 1.0e-6) as f32 * 1.0e-6f32 + height as f32 * extra_f32);
            paths.push(Path::new(role, points, false, width, speed, flow));
        };
        let (mut start, mut i) = (0usize, 0usize);
        let (mut max_w, mut min_w) = segs.first().map_or((0.0, 0.0), |s| (s.2, s.2));
        // Each split adds segments, but no more than the width changes allow for.
        let mut budget = segs.len() * 64 + 1024;
        while let Some(&line) = segs.get(i) {
            if budget == 0 {
                break;
            }
            budget -= 1;
            let len = line.0.distance(line.1);
            if len < 1e-4 {
                i += 1;
                continue;
            }
            let delta = (max_w - line.3).abs().max((min_w - line.3).abs());
            if delta > TOLERANCE {
                if start != i {
                    push(segs.get(start..i).unwrap_or(&[]), false);
                }
                start = i;
                max_w = line.2;
                min_w = line.2;
                let own = (line.2 - line.3).abs();
                if own > TOLERANCE {
                    let pieces = (own / TOLERANCE).ceil().min(256.0) as usize;
                    let at = |k: usize| k as f64 / pieces as f64;
                    let split: Vec<_> = (0..pieces)
                        .map(|k| {
                            (
                                line.0.lerp(line.1, at(k)),
                                line.0.lerp(line.1, at(k + 1)),
                                line.2 + (line.3 - line.2) * at(k),
                                line.2 + (line.3 - line.2) * at(k + 1),
                            )
                        })
                        .collect();
                    segs.splice(i..=i, split);
                    continue;
                }
            } else {
                max_w = max_w.max(line.2.max(line.3));
                min_w = min_w.min(line.2.min(line.3));
            }
            i += 1;
        }
        if start < segs.len() {
            push(segs.get(start..).unwrap_or(&[]), true);
        }
        out.extend(paths);
    }
    out
}

#[cfg(test)]
mod voronoi_tests {
    use super::*;

    #[test]
    fn the_medial_axis_comes_out_the_same_every_time() {
        // A wavy strip: a few hundred lines with near-parallel neighbours.
        let n = 300;
        let wave = |i: usize| (i as f64 * 0.07).sin() * 2.0 + (i as f64 * 0.013).cos();
        let mut c: Vec<DVec2> = (0..n).map(|i| DVec2::new(i as f64 * 0.1, wave(i))).collect();
        c.extend((0..n).rev().map(|i| DVec2::new(i as f64 * 0.1, wave(i) + 0.3 + (i as f64 * 0.2).sin() * 0.02)));
        let shape: Shape = vec![c];
        let first = medial_axis(&shape, 0.02, 0.8).map(|l| format!("{l:?}"));
        assert!(first.is_some());
        for _ in 0..20 {
            assert_eq!(medial_axis(&shape, 0.02, 0.8).map(|l| format!("{l:?}")), first);
        }
    }
}
