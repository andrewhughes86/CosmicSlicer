//! Travel moves inside a part: whether a straight move stays inside (no retraction needed),
//! and a route along the inside of the outline when it would cross a wall (OrcaSlicer's
//! "Avoid crossing walls").

use glam::DVec2;

use crate::geom::{self, Contour, Join, Shape};

/// Where travel inside one island may go.
pub struct Region {
    /// Points inside this are inside the part (the outline, a little in).
    test: Vec<Shape>,
    /// Detours follow this contour, further in, so they stay clear of the walls.
    walk: Vec<Contour>,
    edges: Vec<(DVec2, DVec2)>,
}

/// Whether segments `a`–`b` and `c`–`d` cross (touching at an end does not count).
fn crosses(a: DVec2, b: DVec2, c: DVec2, d: DVec2) -> Option<f64> {
    let r = b - a;
    let s = d - c;
    let denom = r.perp_dot(s);
    if denom.abs() < 1e-12 {
        return None;
    }
    let t = (c - a).perp_dot(s) / denom;
    let u = (c - a).perp_dot(r) / denom;
    (t > 1e-9 && t < 1.0 - 1e-9 && (0.0..=1.0).contains(&u)).then_some(t)
}

impl Region {
    /// For an island outline whose walls are about `nozzle` wide.
    pub fn new(island: &Shape, nozzle: f64) -> Option<Region> {
        let test = geom::offset(std::slice::from_ref(island), -nozzle * 0.25, Join::Miter);
        let walk_shapes = geom::offset(std::slice::from_ref(island), -nozzle * 0.75, Join::Miter);
        if test.is_empty() {
            return None;
        }
        let walk: Vec<Contour> = walk_shapes.iter().flatten().map(|c| geom::simplify_loop(c, nozzle * 0.25)).collect();
        let edges = test
            .iter()
            .flatten()
            .flat_map(|c| (0..c.len()).filter_map(move |i| Some((*c.get(i)?, *c.get((i + 1) % c.len())?))))
            .collect();
        Some(Region { test, walk, edges })
    }

    pub fn contains(&self, p: DVec2) -> bool {
        geom::contains(&self.test, p)
    }

    /// The straight move from `a` to `b` stays inside.
    pub fn segment_inside(&self, a: DVec2, b: DVec2) -> bool {
        if !self.contains(a) || !self.contains(b) {
            return false;
        }
        let (lo, hi) = (a.min(b), a.max(b));
        !self.edges.iter().any(|&(c, d)| {
            // Skip edges whose bounds miss the segment's.
            if c.x.max(d.x) < lo.x || c.x.min(d.x) > hi.x || c.y.max(d.y) < lo.y || c.y.min(d.y) > hi.y {
                return false;
            }
            crosses(a, b, c, d).is_some()
        }) && self.contains((a + b) / 2.0)
    }

    /// A path from `a` to `b` that stays inside, both being inside; `None` when there is none
    /// short enough (`max_detour` extra length, 0 for any).
    pub fn route(&self, a: DVec2, b: DVec2, max_detour: f64) -> Option<Vec<DVec2>> {
        if self.segment_inside(a, b) {
            return Some(vec![a, b]);
        }
        if !self.contains(a) || !self.contains(b) {
            return None;
        }
        let mut path = vec![a];
        let mut cur = a;
        for _ in 0..16 {
            if self.segment_inside(cur, b) {
                break;
            }
            // The walk contour the straight line crosses first, and where it leaves it last.
            let mut hits: Vec<(f64, usize, usize, DVec2)> = Vec::new();
            for (ci, c) in self.walk.iter().enumerate() {
                for i in 0..c.len() {
                    let (Some(&p), Some(&q)) = (c.get(i), c.get((i + 1) % c.len())) else { continue };
                    if let Some(t) = crosses(cur, b, p, q) {
                        hits.push((t, ci, i, cur + (b - cur) * t));
                    }
                }
            }
            hits.sort_by(|x, y| x.0.total_cmp(&y.0));
            let Some(&(_, ci, first, enter)) = hits.first() else { break };
            let Some(&(_, _, last, leave)) = hits.iter().rev().find(|h| h.1 == ci) else { break };
            let Some(c) = self.walk.get(ci) else { break };
            let n = c.len();
            // Round the contour either way; take the shorter.
            let forward: Vec<DVec2> =
                (1..=((last + n - first) % n)).filter_map(|k| c.get((first + k) % n).copied()).collect();
            let backward: Vec<DVec2> =
                (0..((first + n - last) % n)).filter_map(|k| c.get((first + n - k) % n).copied()).collect();
            let len = |pts: &[DVec2]| -> f64 {
                let mut l = 0.0;
                let mut prev = enter;
                for p in pts.iter().chain(std::iter::once(&leave)) {
                    l += prev.distance(*p);
                    prev = *p;
                }
                l
            };
            let way = if len(&forward) <= len(&backward) { forward } else { backward };
            path.push(enter);
            path.extend(way);
            path.push(leave);
            cur = leave;
        }
        path.push(b);
        // Cut corners where a straight move stays inside.
        let mut short = vec![a];
        let mut i = 0;
        while i + 1 < path.len() {
            let mut j = path.len() - 1;
            while j > i + 1 && !path.get(i).zip(path.get(j)).is_some_and(|(p, q)| self.segment_inside(*p, *q)) {
                j -= 1;
            }
            if let Some(&p) = path.get(j) {
                short.push(p);
            }
            i = j;
        }
        let total = geom::length(&short);
        let ok = short.windows(2).all(|w| self.segment_inside(w[0], w[1]));
        let within = max_detour <= 0.0 || total <= a.distance(b) + max_detour;
        (ok && within).then_some(short)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A U shape: 30 wide, a 10-wide slot cut down from the top.
    fn u_shape() -> Shape {
        vec![vec![
            DVec2::new(0.0, 0.0),
            DVec2::new(30.0, 0.0),
            DVec2::new(30.0, 30.0),
            DVec2::new(20.0, 30.0),
            DVec2::new(20.0, 10.0),
            DVec2::new(10.0, 10.0),
            DVec2::new(10.0, 30.0),
            DVec2::new(0.0, 30.0),
        ]]
    }

    #[test]
    fn straight_inside_and_across_the_gap() {
        let r = Region::new(&u_shape(), 0.4);
        assert!(r.is_some());
        let Some(r) = r else { return };
        assert!(r.segment_inside(DVec2::new(2.0, 2.0), DVec2::new(28.0, 2.0)));
        // Across the slot leaves the part.
        assert!(!r.segment_inside(DVec2::new(5.0, 25.0), DVec2::new(25.0, 25.0)));
    }

    #[test]
    fn routes_round_the_slot() {
        let Some(r) = Region::new(&u_shape(), 0.4) else { return };
        let (a, b) = (DVec2::new(5.0, 25.0), DVec2::new(25.0, 25.0));
        let route = r.route(a, b, 0.0);
        assert!(route.is_some());
        let Some(route) = route else { return };
        assert!(route.len() >= 3, "{route:?}");
        assert!(route.windows(2).all(|w| r.segment_inside(w[0], w[1])));
        // Down and round the bottom of the slot: longer than straight, shorter than the rim.
        let l = geom::length(&route);
        assert!(l > 20.0 && l < 60.0, "{l}");
        // A detour limit it cannot meet: no route.
        assert!(r.route(a, b, 5.0).is_none());
    }
}
