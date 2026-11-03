//! Overhangs: how far each wall segment hangs past the layer below, as a fraction of its line
//! width (OrcaSlicer's overhang degrees, which pick the overhang speeds and fan), and making
//! steep overhangs printable by filling them out into cones.

use glam::DVec2;

use crate::geom::{self, Join, Op, Shape};
use crate::toolpath::Path;

/// The layer below, near where this layer's walls may hang past it.
pub struct Below {
    /// Where a line centre hangs at least 10 % (empty: nothing on this layer overhangs).
    zone: Vec<Shape>,
    bounds: Option<(DVec2, DVec2)>,
    below: Vec<Shape>,
    /// The layer below's edges near the zone, bucketed by `cell`.
    edges: std::collections::HashMap<(i64, i64), Vec<(DVec2, DVec2)>>,
    cell: f64,
    width: f64,
}

/// Hanging shares (percent of the line) are kept in 5 % steps.
const STEP: f64 = 5.0;

impl Below {
    /// For lines `width` wide on `layer`, over `below`.
    pub fn new(layer: &[Shape], below: &[Shape], width: f64) -> Below {
        // A line centred `x` outside the layer below hangs (x / w + ½) of its width.
        let first = geom::offset(below, width * (0.10 - 0.5), Join::Miter);
        // Wall centres sit half a width inside the layer; where those can hang 10 %.
        let centres = geom::offset(layer, -0.45 * width, Join::Miter);
        let zone = geom::without_slivers(geom::boolean(&centres, &first, Op::Difference), width * width * 0.01);
        let bounds =
            geom::bounds(&zone).map(|(lo, hi)| (lo - DVec2::splat(width * 2.0), hi + DVec2::splat(width * 2.0)));
        let cell = width.max(0.05);
        let mut edges: std::collections::HashMap<(i64, i64), Vec<(DVec2, DVec2)>> = std::collections::HashMap::new();
        if let Some((lo, hi)) = bounds {
            for c in below.iter().flatten() {
                let n = c.len();
                for i in 0..n {
                    let (Some(&a), Some(&b)) = (c.get(i), c.get((i + 1) % n)) else { continue };
                    if a.max(b).cmplt(lo).any() || a.min(b).cmpgt(hi).any() {
                        continue;
                    }
                    let steps = ((a.distance(b) / (cell / 2.0)).ceil() as usize).clamp(1, 100_000);
                    let mut last = None;
                    for k in 0..=steps {
                        let q = a.lerp(b, k as f64 / steps as f64);
                        let key = ((q.x / cell).floor() as i64, (q.y / cell).floor() as i64);
                        if last != Some(key) {
                            edges.entry(key).or_default().push((a, b));
                            last = Some(key);
                        }
                    }
                }
            }
        }
        let below = if zone.is_empty() { Vec::new() } else { below.to_vec() };
        Below { zone, bounds, below, edges, cell, width }
    }

    fn near(&self, p: DVec2) -> bool {
        self.bounds.is_some_and(|(lo, hi)| p.x >= lo.x && p.y >= lo.y && p.x <= hi.x && p.y <= hi.y)
    }

    /// How much of a line centred at `p` hangs past the layer below, in percent of its width
    /// (0 to 255), in 5 % steps.
    pub fn hang(&self, p: DVec2) -> u8 {
        if self.zone.is_empty() || !self.near(p) {
            return 0;
        }
        let inside = geom::contains(&self.below, p);
        // The nearest edge within two widths; past that the line is all in or all out.
        let (cx, cy) = ((p.x / self.cell).floor() as i64, (p.y / self.cell).floor() as i64);
        let reach = ((self.width * 2.0 / self.cell).ceil() as i64).clamp(1, 8);
        let mut best = f64::INFINITY;
        for x in cx - reach..=cx + reach {
            for y in cy - reach..=cy + reach {
                for (a, b) in self.edges.get(&(x, y)).into_iter().flatten() {
                    best = best.min(geom::point_segment_distance(p, *a, *b));
                }
            }
        }
        let signed = if inside { -best } else { best };
        let share = (signed / self.width + 0.5) * 100.0;
        if !share.is_finite() {
            return if inside { 0 } else { 255 };
        }
        ((share / STEP).round() * STEP).clamp(0.0, 255.0) as u8
    }

    /// The degree, 0 to 5, of a line centred at `p`.
    pub fn degree(&self, p: DVec2) -> u8 {
        crate::toolpath::degree_of(self.hang(p))
    }

    /// Marks each segment of `path` with how far it hangs, splitting long segments near
    /// overhangs so the marks follow the overhang's edge.
    pub fn mark(&self, path: &mut Path) {
        if self.zone.is_empty() || path.points.len() < 2 {
            return;
        }
        let step = self.width.max(0.1);
        let n = path.points.len();
        let segs = if path.closed { n } else { n - 1 };
        // Points, and whether each was added here.
        let mut points: Vec<(DVec2, bool)> = Vec::with_capacity(n);
        for i in 0..segs {
            let (Some(&a), Some(&b)) = (path.points.get(i), path.points.get((i + 1) % n)) else { continue };
            points.push((a, false));
            let len = a.distance(b);
            if len > step && (self.near(a) || self.near(b) || self.near((a + b) / 2.0)) {
                let k = ((len / step).ceil() as usize).min(10_000);
                points.extend((1..k).map(|j| (a + (b - a) * (j as f64 / k as f64), true)));
            }
        }
        if !path.closed
            && let Some(&last) = path.points.last()
        {
            points.push((last, false));
        }
        let m = points.len();
        let hangs: Vec<u8> = (0..m)
            .map(|i| match (points.get(i), points.get((i + 1) % m)) {
                (Some(&(a, _)), Some(&(b, _))) if path.closed || i + 1 < m => self.hang((a + b) / 2.0),
                _ => 0,
            })
            .collect();
        if !hangs.iter().any(|d| crate::toolpath::degree_of(*d) > 0) {
            return;
        }
        // Added points stay only where the share changes.
        let (mut kept, mut marks) = (Vec::with_capacity(n), Vec::with_capacity(n));
        for (i, (&(p, added), &d)) in points.iter().zip(&hangs).enumerate() {
            let before = if i == 0 { None } else { hangs.get(i - 1).copied() };
            if !added || before != Some(d) {
                kept.push(p);
                marks.push(d);
            }
        }
        path.points = kept;
        path.overhang = marks;
    }
}

/// "Make overhangs printable": from the top down, each layer takes in the one above shrunk by
/// what an overhang of `angle` degrees from vertical allows, so nothing hangs further.
pub fn make_printable(slices: &mut [Vec<Shape>], heights: &[f64], angle: f64) {
    let tan = angle.to_radians().tan();
    if !(tan.is_finite() && tan >= 0.0) {
        return;
    }
    for i in (0..slices.len().saturating_sub(1)).rev() {
        let (Some(above), Some(&h)) = (slices.get(i + 1), heights.get(i + 1)) else { continue };
        if above.is_empty() {
            continue;
        }
        let held = geom::offset(above, -h * tan, Join::Round);
        if held.is_empty() {
            continue;
        }
        if let Some(here) = slices.get(i) {
            let merged = geom::boolean(here, &held, Op::Union);
            if let Some(slot) = slices.get_mut(i) {
                *slot = merged;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::toolpath::Role;

    #[test]
    fn a_ledge_is_marked_by_how_far_it_hangs() {
        // The layer reaches 2 mm further right than the one below.
        let below = vec![geom::rectangle(DVec2::ZERO, DVec2::splat(10.0))];
        let layer = vec![geom::rectangle(DVec2::ZERO, DVec2::new(12.0, 10.0))];
        let w = 0.4;
        let b = Below::new(&layer, &below, w);
        assert_eq!(b.degree(DVec2::new(5.0, 5.0)), 0);
        assert_eq!(b.degree(DVec2::new(11.8, 5.0)), 5);
        // Centred 0.05 mm past the edge: five eighths hang.
        assert_eq!(b.degree(DVec2::new(10.05, 5.0)), 3);
        // 0.05 mm inside: three eighths hang (not 9.9, which is exactly on degree 2's edge and
        // goes either way with rounding).
        assert_eq!(b.degree(DVec2::new(9.95, 5.0)), 2);
        let mut wall = Path::new(
            Role::OuterWall,
            crate::geom::rectangle(DVec2::splat(0.2), DVec2::new(11.8, 9.8)).remove(0),
            true,
            w,
            60.0,
            1.0,
        );
        b.mark(&mut wall);
        assert_eq!(wall.overhang.len(), wall.points.len());
        // The right side hangs fully; the left does not.
        let hanging: f64 = (0..wall.points.len())
            .filter(|i| wall.overhang_at(*i) == 5)
            .map(|i| wall.points[i].distance(wall.points[(i + 1) % wall.points.len()]))
            .sum();
        assert!(hanging > 9.0, "{hanging}");
        assert_eq!(wall.overhang_at(wall.points.iter().position(|p| p.x < 1.0 && p.y < 1.0).unwrap_or(0)), 0);
    }

    #[test]
    fn straight_walls_are_not_marked() {
        let sq = vec![geom::rectangle(DVec2::ZERO, DVec2::splat(10.0))];
        let b = Below::new(&sq, &sq, 0.4);
        assert!(b.zone.is_empty());
        let inset = geom::rectangle(DVec2::splat(0.2), DVec2::splat(9.8)).remove(0);
        let mut wall = Path::new(Role::OuterWall, inset, true, 0.4, 60.0, 1.0);
        b.mark(&mut wall);
        assert!(wall.overhang.is_empty());
        assert_eq!(wall.points.len(), 4);
    }

    #[test]
    fn printable_overhangs_fill_out_into_cones() {
        // A 10 mm disc floating over a 2 mm post: the layers under the disc grow out.
        let post = vec![geom::rectangle(DVec2::splat(-1.0), DVec2::splat(1.0))];
        let disc = vec![geom::rectangle(DVec2::splat(-5.0), DVec2::splat(5.0))];
        let mut slices = vec![post.clone(), post.clone(), post.clone(), post.clone(), disc];
        make_printable(&mut slices, &[0.2; 5], 45.0);
        let areas: Vec<f64> = slices.iter().map(|s| geom::area(s)).collect();
        assert!(areas[3] > areas[2] && areas[2] > areas[1], "{areas:?}");
        assert!(areas[3] < 100.0 && areas[0] >= 4.0 - 1e-6, "{areas:?}");
    }
}
