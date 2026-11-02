//! Lightning infill: trees that hold up only what needs holding. From the top down, points
//! under each solid surface start branches; each branch runs to its nearest neighbour nearer
//! the walls, or to the walls themselves, and layer by layer the branches lean in towards
//! what holds them (by the prune angle) until they merge or reach a wall.

use std::collections::HashMap;

use glam::DVec2;

use crate::geom::{self, Join, Polyline, Shape};

/// Most branch points carried on one layer.
const MAX_NODES: usize = 20_000;

/// Edges of a region bucketed on a grid, for nearest-edge queries.
struct Edges {
    cell: f64,
    edges: Vec<(DVec2, DVec2)>,
    grid: HashMap<(i64, i64), Vec<usize>>,
}

impl Edges {
    fn new(region: &[Shape], cell: f64) -> Edges {
        let edges: Vec<(DVec2, DVec2)> = region
            .iter()
            .flatten()
            .flat_map(|c| (0..c.len()).filter_map(move |k| Some((*c.get(k)?, *c.get((k + 1) % c.len())?))))
            .collect();
        let mut grid: HashMap<(i64, i64), Vec<usize>> = HashMap::new();
        for (i, (a, b)) in edges.iter().enumerate() {
            let (lo, hi) = (a.min(*b), a.max(*b));
            let (x0, y0) = ((lo.x / cell).floor() as i64, (lo.y / cell).floor() as i64);
            let (x1, y1) = ((hi.x / cell).floor() as i64, (hi.y / cell).floor() as i64);
            if (x1 - x0 + 1) * (y1 - y0 + 1) > 4096 {
                continue;
            }
            for x in x0..=x1 {
                for y in y0..=y1 {
                    grid.entry((x, y)).or_default().push(i);
                }
            }
        }
        Edges { cell, edges, grid }
    }

    /// The nearest point on an edge within `reach`, and its distance.
    fn nearest(&self, p: DVec2, reach: f64) -> Option<(f64, DVec2)> {
        let (cx, cy) = ((p.x / self.cell).floor() as i64, (p.y / self.cell).floor() as i64);
        let rings = (reach / self.cell).ceil() as i64 + 1;
        let mut best: Option<(f64, DVec2)> = None;
        for r in 0..=rings.min(64) {
            for x in cx - r..=cx + r {
                for y in cy - r..=cy + r {
                    if (x - cx).abs() != r && (y - cy).abs() != r {
                        continue;
                    }
                    for &i in self.grid.get(&(x, y)).map_or(&[][..], Vec::as_slice) {
                        let Some(&(a, b)) = self.edges.get(i) else { continue };
                        let q = closest_on_segment(p, a, b);
                        let d = p.distance(q);
                        if best.is_none_or(|bb| d < bb.0) {
                            best = Some((d, q));
                        }
                    }
                }
            }
            // Anything in a further ring is at least `r` cells away.
            if best.is_some_and(|bb| bb.0 <= r as f64 * self.cell) {
                break;
            }
        }
        best.filter(|bb| bb.0 <= reach)
    }
}

fn closest_on_segment(p: DVec2, a: DVec2, b: DVec2) -> DVec2 {
    let ab = b - a;
    let len2 = ab.length_squared();
    if len2 < 1e-18 {
        return a;
    }
    a + ab * ((p - a).dot(ab) / len2).clamp(0.0, 1.0)
}

/// Points on a plate-fixed grid of `spacing` inside `region`.
fn samples(region: &[Shape], spacing: f64) -> Vec<DVec2> {
    let Some((lo, hi)) = geom::bounds(region) else { return Vec::new() };
    let (i0, i1) = ((lo.x / spacing).ceil() as i64, (hi.x / spacing).floor() as i64);
    let (j0, j1) = ((lo.y / spacing).ceil() as i64, (hi.y / spacing).floor() as i64);
    if ((i1 - i0 + 1).max(0) * (j1 - j0 + 1).max(0)) as usize > MAX_NODES * 4 {
        return Vec::new();
    }
    let mut out = Vec::new();
    for i in i0..=i1 {
        for j in j0..=j1 {
            let p = DVec2::new(i as f64 * spacing, j as f64 * spacing);
            if geom::contains(region, p) {
                out.push(p);
            }
        }
    }
    out
}

/// Lightning lines for every layer. `sparse[i]` is where layer `i` has sparse infill,
/// `demand[i]` the part of it under solid surfaces of the layer above; `heights[i]` the layer
/// heights. Branch points sit `spacing` apart and lean in by `prune` (radians) a layer.
pub fn grow(
    sparse: &[Vec<Shape>],
    demand: &[Vec<Shape>],
    heights: &[f64],
    spacing: f64,
    prune: f64,
    width: f64,
    cancelled: &(dyn Fn() -> bool + Sync),
) -> Vec<Vec<Polyline>> {
    let n = sparse.len();
    let mut out: Vec<Vec<Polyline>> = vec![Vec::new(); n];
    if !(spacing > width * 0.5 && spacing.is_finite()) {
        return out;
    }
    let lean = prune.tan().max(0.0);
    let mut nodes: Vec<DVec2> = Vec::new();
    for i in (0..n).rev() {
        if cancelled() {
            return out;
        }
        let Some(region) = sparse.get(i).filter(|r| !r.is_empty()) else {
            nodes.clear();
            continue;
        };
        if let Some(d) = demand.get(i) {
            nodes.extend(samples(d, spacing));
        }
        // Over walls or solid below, a point is held already.
        nodes.retain(|p| geom::contains(region, *p));
        dedup(&mut nodes, width);
        nodes.truncate(MAX_NODES);
        if nodes.is_empty() {
            continue;
        }
        let edges = Edges::new(region, spacing * 2.0);
        let reach = spacing * 50.0;
        // Nearest the walls first: they anchor there; the rest hang off a neighbour nearer in.
        let mut order: Vec<(f64, DVec2, DVec2)> =
            nodes.iter().filter_map(|p| edges.nearest(*p, reach).map(|(d, q)| (d, *p, q))).collect();
        order.sort_by(|a, b| a.0.total_cmp(&b.0));
        let cell = spacing * 2.0;
        let mut placed: HashMap<(i64, i64), Vec<usize>> = HashMap::new();
        let mut parent: Vec<DVec2> = Vec::with_capacity(order.len());
        let mut points: Vec<DVec2> = Vec::with_capacity(order.len());
        let mut to_node: Vec<Option<usize>> = Vec::with_capacity(order.len());
        for (d, p, wall) in &order {
            let key = ((p.x / cell).floor() as i64, (p.y / cell).floor() as i64);
            let mut best: Option<(f64, usize)> = None;
            for x in key.0 - 1..=key.0 + 1 {
                for y in key.1 - 1..=key.1 + 1 {
                    for &k in placed.get(&(x, y)).map_or(&[][..], Vec::as_slice) {
                        let Some(q) = points.get(k) else { continue };
                        let dd = p.distance(*q);
                        if dd < *d && best.is_none_or(|b| dd < b.0) {
                            best = Some((dd, k));
                        }
                    }
                }
            }
            let idx = points.len();
            points.push(*p);
            match best {
                Some((_, k)) => {
                    parent.push(points.get(k).copied().unwrap_or(*wall));
                    to_node.push(Some(k));
                }
                None => {
                    parent.push(*wall);
                    to_node.push(None);
                }
            }
            placed.entry(key).or_default().push(idx);
        }
        // Branches: from each leaf along its parents to the wall, or to a branch already drawn.
        let mut has_child = vec![false; points.len()];
        for k in to_node.iter().flatten() {
            if let Some(c) = has_child.get_mut(*k) {
                *c = true;
            }
        }
        let mut drawn = vec![false; points.len()];
        let mut lines: Vec<Polyline> = Vec::new();
        let mut leaves: Vec<usize> =
            (0..points.len()).filter(|&k| !has_child.get(k).copied().unwrap_or(true)).collect();
        // Then anything left over (cycles cannot occur, but be safe).
        leaves.extend(0..points.len());
        for leaf in leaves {
            if drawn.get(leaf).copied().unwrap_or(true) {
                continue;
            }
            let mut line = Vec::new();
            let mut k = leaf;
            for _ in 0..points.len() + 1 {
                let Some(&p) = points.get(k) else { break };
                line.push(p);
                if let Some(slot) = drawn.get_mut(k) {
                    *slot = true;
                }
                match to_node.get(k).copied().flatten() {
                    Some(next) if !drawn.get(next).copied().unwrap_or(true) => k = next,
                    Some(next) => {
                        if let Some(&q) = points.get(next) {
                            line.push(q);
                        }
                        break;
                    }
                    None => {
                        if let Some(&q) = parent.get(k) {
                            line.push(q);
                        }
                        break;
                    }
                }
            }
            if line.len() >= 2 && geom::length(&line) > width {
                lines.push(line);
            }
        }
        let grown = geom::offset(region, width * 0.5, Join::Miter);
        if let Some(slot) = out.get_mut(i) {
            *slot = geom::clip_lines(&lines, &grown);
        }
        // Down a layer: every point leans towards what holds it; those that reach it merge.
        let step = heights.get(i).copied().unwrap_or(0.2) * lean;
        nodes = points
            .iter()
            .zip(&parent)
            .filter_map(|(p, q)| {
                let d = p.distance(*q);
                (d > step + width * 0.5).then(|| *p + (*q - *p) * (step / d))
            })
            .collect();
    }
    out
}

/// Drops points within `tol` of one kept before them.
fn dedup(points: &mut Vec<DVec2>, tol: f64) {
    let mut seen: HashMap<(i64, i64), Vec<DVec2>> = HashMap::new();
    points.retain(|p| {
        let key = ((p.x / tol).floor() as i64, (p.y / tol).floor() as i64);
        for x in key.0 - 1..=key.0 + 1 {
            for y in key.1 - 1..=key.1 + 1 {
                if seen.get(&(x, y)).is_some_and(|v| v.iter().any(|q| q.distance(*p) < tol)) {
                    return false;
                }
            }
        }
        seen.entry(key).or_default().push(*p);
        true
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn never() -> bool {
        false
    }

    #[test]
    fn trees_hold_up_the_top_and_thin_out_below() {
        // A 40 mm box, sparse inside, with a solid top over layer 119: 24 mm of leaning below
        // it, more than the 19 mm from the middle to a wall.
        let inside = vec![geom::rectangle(DVec2::splat(1.0), DVec2::splat(39.0))];
        let sparse: Vec<Vec<Shape>> = vec![inside.clone(); 120];
        let mut demand: Vec<Vec<Shape>> = vec![Vec::new(); 120];
        demand[119] = inside.clone();
        let lines = grow(&sparse, &demand, &[0.2; 120], 2.0, 45f64.to_radians(), 0.45, &never);
        let len = |i: usize| -> f64 { lines[i].iter().map(|l| geom::length(l)).sum() };
        assert!(len(119) > 100.0, "{}", len(119));
        // Branches merge going down: less and less line, a few short trunks at the bottom.
        assert!(len(100) < len(119) * 0.8, "{} vs {}", len(100), len(119));
        assert!(len(0) < len(119) * 0.1, "{}", len(0));
        // All inside the box.
        assert!(lines.iter().flatten().flatten().all(|p| p.x > 0.7 && p.x < 39.3 && p.y > 0.7 && p.y < 39.3));
    }

    #[test]
    fn nothing_needed_nothing_drawn() {
        let inside = vec![geom::rectangle(DVec2::ZERO, DVec2::splat(20.0))];
        let lines = grow(&[inside.clone(), inside], &[Vec::new(), Vec::new()], &[0.2; 2], 2.0, 0.7, 0.45, &never);
        assert!(lines.iter().all(Vec::is_empty));
    }
}
