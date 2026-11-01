//! Infill patterns: lines clipped to a region, joined where neighbours meet.
//!
//! OrcaSlicer's pattern names map onto a few generators: parallel lines in one, two or three
//! directions (rectilinear, grid, triangles / cubic), concentric loops and the gyroid. Patterns
//! not built yet fall back to the nearest of these.

use std::collections::HashMap;
use std::f64::consts::{FRAC_PI_2, PI, TAU};

use glam::DVec2;

use crate::geom::{self, Join, Polyline, Shape};

/// A fill line: open, or a loop for concentric patterns.
#[derive(Clone, Debug, PartialEq)]
pub struct Line {
    pub points: Polyline,
    pub closed: bool,
}

/// How a pattern lays its lines.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Kind {
    /// Parallel lines in `dirs` directions spread over 180°, alternating 90° per layer when
    /// `alternate`.
    Lines {
        dirs: usize,
        alternate: bool,
        cubic: bool,
    },
    Concentric,
    Gyroid,
}

/// The generator for an OrcaSlicer pattern name.
pub fn kind_of(pattern: &str) -> Kind {
    match pattern {
        "grid" | "rectilinear-grid" | "tri-hexagon" | "crosshatch" => {
            Kind::Lines { dirs: 2, alternate: false, cubic: false }
        }
        "triangles" | "honeycomb" | "3dhoneycomb" | "lateral-honeycomb" | "lateral-lattice" => {
            Kind::Lines { dirs: 3, alternate: false, cubic: false }
        }
        "cubic" | "adaptivecubic" | "quartercubic" | "supportcubic" => {
            Kind::Lines { dirs: 3, alternate: false, cubic: true }
        }
        "gyroid" | "tpmsd" | "tpmsfk" => Kind::Gyroid,
        "concentric" | "spiralinset" | "archimedeanchords" | "octagramspiral" => Kind::Concentric,
        "alignedrectilinear" => Kind::Lines { dirs: 1, alternate: false, cubic: false },
        _ => Kind::Lines { dirs: 1, alternate: true, cubic: false },
    }
}

/// Fills `area` with lines `width` wide at `density` (0..1) of solid.
///
/// `angle` is the base direction in radians, `layer` the layer index (for alternation),
/// `z` the height (for cubic and gyroid).
pub fn fill(area: &[Shape], kind: Kind, width: f64, density: f64, angle: f64, layer: usize, z: f64) -> Vec<Line> {
    if area.is_empty() || density <= 0.0 || width <= 0.0 {
        return Vec::new();
    }
    let density = density.min(1.0);
    match kind {
        Kind::Concentric => concentric(area, width / density),
        Kind::Gyroid => gyroid(area, width, density, angle, z),
        Kind::Lines { dirs, alternate, cubic } => {
            // Each direction gets `dirs` times the spacing so the total stays at `density`.
            let spacing = width * dirs as f64 / density;
            let base = if alternate && layer % 2 == 1 { angle + FRAC_PI_2 } else { angle };
            let mut lines = Vec::new();
            for d in 0..dirs {
                let a = base + PI * d as f64 / dirs as f64;
                // Cubic: each direction slides with height, so the lines build tilted planes.
                let phase = if cubic {
                    (z * [1.0, -1.0, 1.0][d % 3] * std::f64::consts::FRAC_1_SQRT_2).rem_euclid(spacing)
                } else {
                    0.0
                };
                lines.extend(parallel(area, spacing, a, phase, width));
            }
            lines
        }
    }
}

/// Lines `spacing` apart at angle `a`, clipped and joined zig-zag where they meet.
fn parallel(area: &[Shape], spacing: f64, a: f64, phase: f64, width: f64) -> Vec<Line> {
    let rows = geom::hatch_lines(area, spacing, a, phase, 100_000);
    let clipped = geom::clip_lines(&rows, area);
    let dir = DVec2::new(a.cos(), a.sin());
    let normal = dir.perp();
    // Order the pieces by row, then along the row.
    let mut pieces: Vec<(f64, f64, Polyline)> = clipped
        .into_iter()
        .filter(|l| geom::length(l) > width * 0.5)
        .map(|mut l| {
            let (Some(&s), Some(&e)) = (l.first(), l.last()) else { return (0.0, 0.0, l) };
            if (e - s).dot(dir) < 0.0 {
                l.reverse();
            }
            let s = l.first().copied().unwrap_or_default();
            ((s.dot(normal) / spacing).round(), s.dot(dir), l)
        })
        .collect();
    pieces.sort_by(|x, y| x.0.total_cmp(&y.0).then(x.1.total_cmp(&y.1)));
    join_zigzag(pieces.into_iter().map(|p| p.2).collect(), area, spacing * 1.9)
}

/// Joins each line to the next when the step between their ends is short and stays inside
/// `area`, flipping every other line, so neighbouring rows print as one zig-zag.
fn join_zigzag(lines: Vec<Polyline>, area: &[Shape], max_step: f64) -> Vec<Line> {
    // Line ends lie on the boundary, so the step between them runs along it: test against the
    // area grown by a little.
    let area = &geom::offset(area, max_step * 0.25, Join::Miter);
    let mut out: Vec<Line> = Vec::new();
    let mut current: Option<Polyline> = None;
    for mut l in lines {
        if let Some(cur) = current.as_mut() {
            let end = cur.last().copied().unwrap_or_default();
            // Try the nearer end of the next line.
            let (Some(&s), Some(&e)) = (l.first(), l.last()) else { continue };
            if e.distance(end) < s.distance(end) {
                l.reverse();
            }
            let start = l.first().copied().unwrap_or_default();
            let step = start.distance(end);
            if step < max_step && geom::contains(area, (start + end) / 2.0) {
                cur.extend(l);
                continue;
            }
            if let Some(done) = current.take() {
                out.push(Line { points: done, closed: false });
            }
        }
        current = Some(l);
    }
    if let Some(done) = current {
        out.push(Line { points: done, closed: false });
    }
    out
}

/// Loops inset `spacing` apart until the area runs out.
fn concentric(area: &[Shape], spacing: f64) -> Vec<Line> {
    let mut out = Vec::new();
    let mut ring = geom::offset(area, -spacing / 2.0, Join::Miter);
    let mut guard = 0;
    while !ring.is_empty() && guard < 10_000 {
        for c in ring.iter().flatten() {
            out.push(Line { points: geom::simplify_loop(c, 0.01), closed: true });
        }
        ring = geom::offset(&ring, -spacing, Join::Miter);
        guard += 1;
    }
    out
}

/// A marching-squares grid edge: (i, j, 0) runs along x from node (i, j), (i, j, 1) along y.
type GridEdge = (usize, usize, u8);

/// The gyroid surface cut at `z`: the zero set of sin x cos y + sin y cos z + sin z cos x,
/// traced by marching squares and clipped to the area.
fn gyroid(area: &[Shape], width: f64, density: f64, angle: f64, z: f64) -> Vec<Line> {
    let Some((lo, hi)) = geom::bounds(area) else { return Vec::new() };
    // A period whose curves sit about `width / density` apart, as OrcaSlicer's does.
    let period = 2.41 * width / density;
    let k = TAU / period;
    let cell = (period / 9.0).clamp(width * 0.5, width * 3.0);
    let (nx, ny) = (((hi.x - lo.x) / cell).ceil() as usize + 2, ((hi.y - lo.y) / cell).ceil() as usize + 2);
    if nx.saturating_mul(ny) > 8_000_000 {
        return concentric(area, width / density);
    }
    let (sa, ca) = angle.sin_cos();
    let (sz, cz) = (z * k).sin_cos();
    let f = |p: DVec2| {
        // Rotate the field so "infill direction" turns it like the other patterns.
        let q = DVec2::new(p.x * ca + p.y * sa, -p.x * sa + p.y * ca) * k;
        q.x.sin() * q.y.cos() + q.y.sin() * cz + sz * q.x.cos()
    };
    let at = |i: usize, j: usize| lo + DVec2::new((i as f64 - 1.0) * cell, (j as f64 - 1.0) * cell);
    let values: Vec<f64> = (0..ny).flat_map(|j| (0..nx).map(move |i| (i, j))).map(|(i, j)| f(at(i, j))).collect();
    let v = |i: usize, j: usize| values.get(j * nx + i).copied().unwrap_or(0.0);
    // Crossing points keyed by grid edge: (i, j, 0) runs along x from (i, j), (i, j, 1) along y.
    let point = |e: (usize, usize, u8)| {
        let (i, j, d) = e;
        let (a, b, pa, pb) = if d == 0 {
            (v(i, j), v(i + 1, j), at(i, j), at(i + 1, j))
        } else {
            (v(i, j), v(i, j + 1), at(i, j), at(i, j + 1))
        };
        let t = if (b - a).abs() > 1e-12 { (a / (a - b)).clamp(0.0, 1.0) } else { 0.5 };
        pa + (pb - pa) * t
    };
    let mut links: HashMap<GridEdge, Vec<GridEdge>> = HashMap::new();
    for j in 0..ny - 1 {
        for i in 0..nx - 1 {
            let corners = [v(i, j), v(i + 1, j), v(i + 1, j + 1), v(i, j + 1)];
            let edges = [(i, j, 0u8), (i + 1, j, 1u8), (i, j + 1, 0u8), (i, j, 1u8)];
            let crossed: Vec<(usize, usize, u8)> =
                (0..4).filter(|&e| (corners[e] >= 0.0) != (corners[(e + 1) % 4] >= 0.0)).map(|e| edges[e]).collect();
            let pairs: &[(usize, usize)] = if crossed.len() == 2 {
                &[(0, 1)]
            } else if crossed.len() == 4 {
                &[(0, 1), (2, 3)]
            } else {
                &[]
            };
            for &(a, b) in pairs {
                let (Some(&ea), Some(&eb)) = (crossed.get(a), crossed.get(b)) else { continue };
                links.entry(ea).or_default().push(eb);
                links.entry(eb).or_default().push(ea);
            }
        }
    }
    // Walk the links into polylines.
    let mut lines: Vec<Polyline> = Vec::new();
    let mut keys: Vec<(usize, usize, u8)> = links.keys().copied().collect();
    keys.sort_unstable();
    // Start from line ends (one link) first, then whatever loops remain.
    keys.sort_by_key(|k| links.get(k).map_or(0, Vec::len));
    for start in keys {
        if links.get(&start).is_none_or(Vec::is_empty) {
            continue;
        }
        let mut line = vec![point(start)];
        let mut at_key = start;
        while let Some(next) = links.get_mut(&at_key).and_then(Vec::pop) {
            if let Some(back) = links.get_mut(&next)
                && let Some(pos) = back.iter().position(|e| *e == at_key)
            {
                back.swap_remove(pos);
            }
            line.push(point(next));
            at_key = next;
            if line.len() > 1_000_000 {
                break;
            }
        }
        if line.len() >= 2 {
            lines.push(geom::simplify(&line, 0.03));
        }
    }
    let clipped = geom::clip_lines(&lines, area);
    let mut pieces: Vec<Polyline> = clipped.into_iter().filter(|l| geom::length(l) > width).collect();
    // Nearest-first order keeps travel short between the waves.
    pieces.sort_by(|a, b| {
        let (pa, pb) = (a.first().copied().unwrap_or_default(), b.first().copied().unwrap_or_default());
        (pa.y, pa.x).partial_cmp(&(pb.y, pb.x)).unwrap_or(std::cmp::Ordering::Equal)
    });
    pieces.into_iter().map(|points| Line { points, closed: false }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn square(size: f64) -> Vec<Shape> {
        vec![geom::rectangle(DVec2::ZERO, DVec2::splat(size))]
    }

    fn total(lines: &[Line]) -> f64 {
        lines.iter().map(|l| geom::length(&l.points)).sum()
    }

    #[test]
    fn density_sets_how_much_line_there_is() {
        let area = square(50.0);
        for pattern in ["rectilinear", "grid", "triangles", "cubic", "gyroid", "concentric"] {
            let lines = fill(&area, kind_of(pattern), 0.45, 0.2, 0.785, 3, 5.0);
            // Solid would be area / width of line; 20% of that, give or take.
            let expected = 2500.0 / 0.45 * 0.2;
            let got = total(&lines);
            assert!((got - expected).abs() / expected < 0.3, "{pattern}: {got} vs {expected}");
            assert!(lines.iter().flat_map(|l| &l.points).all(|p| p.x >= -1e-6 && p.x <= 50.0 + 1e-6), "{pattern}");
        }
    }

    #[test]
    fn solid_rectilinear_zigzags() {
        let lines = fill(&square(20.0), kind_of("monotonic"), 0.42, 1.0, 0.0, 0, 0.0);
        // Joined into one zig-zag rather than ~48 separate lines.
        assert!(lines.len() <= 3, "{}", lines.len());
        let got = total(&lines);
        assert!(got > 900.0, "{got}");
    }

    #[test]
    fn nothing_from_nothing() {
        assert!(fill(&[], kind_of("grid"), 0.45, 0.2, 0.0, 0, 0.0).is_empty());
        assert!(fill(&square(10.0), kind_of("grid"), 0.45, 0.0, 0.0, 0, 0.0).is_empty());
    }
}
