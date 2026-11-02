//! Tree supports: branches from points under each overhang down to the plate (or the part),
//! leaning in to merge with their neighbours and out round the part, thickening as they go.
//!
//! From the top down, each layer's new tips are sampled on a plate-fixed grid under what needs
//! holding. Every node then moves at most as far as the branch angle allows: off the part
//! when the part lies underneath (so branches slide down its sides to the plate), away from
//! it when too close, else towards its nearest neighbour; nodes that come close merge. Each
//! node is a circle on its layer, and a layer's circles, kept clear of the part, are its
//! branches. With "on build plate only", a branch that lands on the part is dropped whole.

use std::collections::HashMap;

use glam::DVec2;
use rayon::prelude::*;

use crate::geom::{self, Contour, Join, Op, Shape};
use crate::lightning::{self, Edges};
use crate::settings::{Settings, SupportStyle};

/// Most nodes carried on one layer.
const MAX_NODES: usize = 20_000;

/// How many layers ahead branches look for the part below them.
const LOOKAHEAD: [usize; 4] = [1, 4, 12, 30];

/// One object's trees.
pub struct Trees {
    pub area: Vec<Vec<Shape>>,
    pub interface: Vec<Vec<Shape>>,
    /// Brim round the trunks on the first layer.
    pub brim: Vec<Shape>,
}

#[derive(Clone, Copy)]
struct Node {
    pos: DVec2,
    /// Layers since the tip: sets the radius.
    age: usize,
    id: usize,
}

/// Branch sizes for a style.
struct Sizes {
    tip_r: f64,
    branch_r: f64,
    grow: f64,
    /// Neighbours this close draw together.
    attract: f64,
}

impl Sizes {
    fn new(s: &Settings) -> Sizes {
        let t = &s.support.tree;
        let w = s.widths.support;
        let tip_r = (t.tip_diameter / 2.0).max(w / 2.0);
        let d = t.branch_distance.max(w * 2.0);
        let (scale, grow, attract) = match s.support.style {
            SupportStyle::TreeSlim => (0.6, 0.5, d * 1.5),
            SupportStyle::TreeStrong => (1.0, 1.5, d * 1.5),
            SupportStyle::Organic => (1.0, 1.0, d * 3.0),
            _ => (1.0, 1.0, d * 1.5),
        };
        Sizes {
            tip_r,
            branch_r: (t.branch_diameter / 2.0 * scale).max(tip_r),
            grow: (t.diameter_angle * grow).tan(),
            attract: attract.max(4.0),
        }
    }

    /// Tips widen at 45° to the branch diameter, then by the diameter angle.
    fn radius(&self, age: usize, h: f64) -> f64 {
        let cone = self.tip_r + age as f64 * h;
        let r = if cone < self.branch_r {
            cone
        } else {
            let grown = (self.branch_r - self.tip_r) / h.max(1e-6);
            self.branch_r + (age as f64 - grown).max(0.0) * h * self.grow
        };
        r.min(self.branch_r * 4.0).min(15.0)
    }
}

/// Trees under `need[i]` (what layer `i` of the object leaves hanging), round `all` (every
/// object on each layer); `heights` are the layer heights.
pub fn grow(
    need: &[Vec<Shape>],
    all: &[Vec<Shape>],
    heights: &[f64],
    s: &Settings,
    cancelled: &dyn Fn() -> bool,
) -> Trees {
    let n = all.len();
    let sp = &s.support;
    let w = s.widths.support;
    let h = s.layer_height;
    let gap_top = (sp.top_z_distance / h).round() as usize;
    let gap_bottom = (sp.bottom_z_distance / h).round() as usize;
    let iface = sp.interface_layers;
    let d = sp.tree.branch_distance.max(w * 2.0);
    let size = Sizes::new(s);
    let clearance = |i: usize, extra: f64| {
        let gap = if i == 0 { sp.first_layer_gap } else { sp.xy_distance };
        all.get(i).map_or_else(Vec::new, |here| geom::offset(here, gap + extra, Join::Round))
    };

    // Interfaces: the layers just under each overhang, past the gap.
    let mut interface: Vec<Vec<Shape>> = vec![Vec::new(); n];
    if iface > 0 {
        for (l, area) in need.iter().enumerate().filter(|(_, a)| !a.is_empty()) {
            let Some(top) = l.checked_sub(1 + gap_top) else { continue };
            for j in (0..iface).filter_map(|k| top.checked_sub(k)) {
                if let Some(f) = interface.get_mut(j) {
                    f.extend(area.iter().cloned());
                }
            }
        }
        for (j, f) in interface.iter_mut().enumerate() {
            if !f.is_empty() {
                let merged = geom::union(f);
                *f = geom::without_slivers(geom::boolean(&merged, &clearance(j, w / 2.0), Op::Difference), w * w * 2.0);
            }
        }
    }

    let mut circles: Vec<Vec<(DVec2, f64, usize)>> = vec![Vec::new(); n];
    // By node id: what it continues into below (after a merge), and whether it ends well.
    let mut child: Vec<Option<usize>> = Vec::new();
    let mut ends_ok: Vec<bool> = Vec::new();
    let mut nodes: Vec<Node> = Vec::new();
    // Each layer's outline edges and bounds, for keeping branches off the part.
    let cell = d.max(2.0);
    let edges: Vec<Option<Edges>> =
        all.par_iter().map(|layer| (!layer.is_empty()).then(|| Edges::new(layer, cell))).collect();
    let bounds: Vec<Option<(DVec2, DVec2)>> = all.iter().map(|l| geom::bounds(l)).collect();
    let on = |j: usize, p: DVec2| {
        bounds.get(j).copied().flatten().is_some_and(|(lo, hi)| p.cmpge(lo).all() && p.cmple(hi).all())
            && all.get(j).is_some_and(|l| geom::contains(l, p))
    };
    for i in (0..n).rev() {
        if cancelled() {
            break;
        }
        let hi = heights.get(i).copied().unwrap_or(h);
        let step = hi * sp.tree.branch_angle.tan();

        // Move: off the part when it lies below (looking a few layers ahead, so branches start
        // sliding early), away from it when too close, or towards a neighbour.
        let snapshot: Vec<DVec2> = nodes.iter().map(|nd| nd.pos).collect();
        let near = Grid::new(&snapshot, size.attract);
        for (k, nd) in nodes.iter_mut().enumerate() {
            let avoid = sp.xy_distance + size.radius(nd.age, hi);
            let mut p = nd.pos;
            let mut moved = false;
            let ahead = LOOKAHEAD.iter().filter_map(|a| i.checked_sub(*a)).find(|j| on(*j, p));
            if let Some(e) = ahead.and_then(|j| edges.get(j)).and_then(Option::as_ref)
                && let Some((_, q)) = e.nearest(p, 50.0)
            {
                let goal = q + (q - p).normalize_or_zero() * avoid;
                p += (goal - p).clamp_length_max(step);
                moved = true;
            }
            for j in [i.checked_sub(1), Some(i)].into_iter().flatten() {
                if moved {
                    break;
                }
                if let Some((dist, q)) = edges.get(j).and_then(Option::as_ref).and_then(|e| e.nearest(p, avoid))
                    && !on(j, p)
                {
                    p += (p - q).normalize_or_zero() * (avoid - dist).min(step);
                    moved = true;
                }
            }
            if !moved && let Some(q) = near.nearest(&snapshot, p, k, size.attract) {
                let dist = p.distance(q);
                p += (q - p).normalize_or_zero() * step.min(dist / 2.0);
            }
            if p.is_finite() {
                nd.pos = p;
            }
        }

        // Merge nodes that came close.
        let merge = step.max(w);
        if nodes.len() > 1 {
            let positions: Vec<DVec2> = nodes.iter().map(|nd| nd.pos).collect();
            let grid = Grid::new(&positions, merge);
            let mut gone = vec![false; nodes.len()];
            let mut merged: Vec<Node> = Vec::new();
            for a in 0..nodes.len() {
                if gone.get(a).copied().unwrap_or(true) {
                    continue;
                }
                let Some(&na) = nodes.get(a) else { continue };
                let partner = grid
                    .within(&positions, na.pos, merge)
                    .into_iter()
                    .find(|b| *b != a && !gone.get(*b).copied().unwrap_or(true));
                if let Some((b, nb)) = partner.and_then(|b| nodes.get(b).copied().map(|nb| (b, nb))) {
                    if let Some(g) = gone.get_mut(a) {
                        *g = true;
                    }
                    if let Some(g) = gone.get_mut(b) {
                        *g = true;
                    }
                    let id = new_id(&mut child, &mut ends_ok);
                    for old in [na.id, nb.id] {
                        if let Some(c) = child.get_mut(old) {
                            *c = Some(id);
                        }
                    }
                    let (wa, wb) = (na.age as f64 + 1.0, nb.age as f64 + 1.0);
                    merged.push(Node { pos: (na.pos * wa + nb.pos * wb) / (wa + wb), age: na.age.max(nb.age), id });
                }
            }
            let mut kept: Vec<Node> = nodes
                .iter()
                .enumerate()
                .filter(|(k, _)| !gone.get(*k).copied().unwrap_or(true))
                .map(|(_, nd)| *nd)
                .collect();
            kept.extend(merged);
            nodes = kept;
        }

        // New tips under what needs holding above the interface layers.
        if let Some(area) = need.get(i + 1 + gap_top + iface).filter(|a| !a.is_empty()) {
            let region = geom::boolean(area, &clearance(i, size.tip_r), Op::Difference);
            let positions: Vec<DVec2> = nodes.iter().map(|nd| nd.pos).collect();
            let grid = Grid::new(&positions, d);
            for p in tips(&region, d) {
                if nodes.len() >= MAX_NODES {
                    break;
                }
                if grid.within(&positions, p, d * 0.5).is_empty() {
                    let id = new_id(&mut child, &mut ends_ok);
                    nodes.push(Node { pos: p, age: 0, id });
                }
            }
        }

        // Nodes that reach the part (or its bottom gap) stop here.
        nodes.retain(|nd| {
            let blocked = (0..=gap_bottom).filter_map(|k| i.checked_sub(k)).any(|j| on(j, nd.pos));
            if blocked && let Some(e) = ends_ok.get_mut(nd.id) {
                *e = !sp.build_plate_only;
            }
            !blocked
        });
        if let Some(c) = circles.get_mut(i) {
            c.extend(nodes.iter().map(|nd| (nd.pos, size.radius(nd.age, hi), nd.id)));
        }
        for nd in &mut nodes {
            nd.age += 1;
        }
    }
    for nd in &nodes {
        if let Some(e) = ends_ok.get_mut(nd.id) {
            *e = true;
        }
    }

    // A branch is kept when the chain it runs into ends well.
    let mut alive = vec![false; child.len()];
    for id in (0..child.len()).rev() {
        let ok = match child.get(id).copied().flatten() {
            Some(c) => alive.get(c).copied().unwrap_or(false),
            None => ends_ok.get(id).copied().unwrap_or(false),
        };
        if let Some(a) = alive.get_mut(id) {
            *a = ok;
        }
    }

    let mut area: Vec<Vec<Shape>> = vec![Vec::new(); n];
    let mut brim = Vec::new();
    for (i, layer) in circles.iter().enumerate() {
        let loops: Vec<Contour> = layer
            .iter()
            .filter(|(_, _, id)| alive.get(*id).copied().unwrap_or(false))
            .map(|(p, r, _)| circle(*p, *r))
            .collect();
        if loops.is_empty() {
            continue;
        }
        let clear = clearance(i, w / 2.0);
        let mut a = geom::boolean(&geom::union_loops(&loops), &clear, Op::Difference);
        if let Some(f) = interface.get(i).filter(|f| !f.is_empty()) {
            a = geom::boolean(&a, f, Op::Difference);
        }
        if i == 0 && sp.tree.brim > 0.0 {
            let wide = geom::boolean(&geom::offset(&a, sp.tree.brim, Join::Round), &clear, Op::Difference);
            brim = geom::without_slivers(geom::boolean(&wide, &a, Op::Difference), w * w * 2.0);
        }
        if let Some(slot) = area.get_mut(i) {
            *slot = geom::without_slivers(a, w * w * 2.0);
        }
    }
    Trees { area, interface, brim }
}

/// A new node id: continuing nowhere yet, not ending well yet.
fn new_id(child: &mut Vec<Option<usize>>, ends_ok: &mut Vec<bool>) -> usize {
    child.push(None);
    ends_ok.push(false);
    child.len() - 1
}

/// Tip points in `region`: a plate-fixed grid, and points round the outline of any part too
/// small for the grid to reach.
fn tips(region: &[Shape], d: f64) -> Vec<DVec2> {
    let mut out = Vec::new();
    for shape in region {
        let one = std::slice::from_ref(shape);
        let inside = lightning::samples(one, d);
        if !inside.is_empty() {
            out.extend(inside);
            continue;
        }
        let Some(outer) = shape.first().filter(|c| c.len() >= 3) else { continue };
        let perimeter = geom::length(outer) + outer.first().zip(outer.last()).map_or(0.0, |(a, b)| a.distance(*b));
        if perimeter < d {
            let c = outer.iter().copied().sum::<DVec2>() / outer.len() as f64;
            out.push(c);
            continue;
        }
        // Along the outline every `d`.
        let mut left = 0.0;
        for k in 0..outer.len() {
            let (Some(a), Some(b)) = (outer.get(k), outer.get((k + 1) % outer.len())) else { continue };
            let len = a.distance(*b);
            let mut t = left;
            while t < len {
                out.push(*a + (*b - *a) * (t / len.max(1e-12)));
                t += d;
            }
            left = t - len;
        }
    }
    out
}

fn circle(c: DVec2, r: f64) -> Contour {
    let segments = if r < 2.0 { 16 } else { 32 };
    (0..segments)
        .map(|k| {
            let a = std::f64::consts::TAU * k as f64 / segments as f64;
            c + DVec2::new(a.cos(), a.sin()) * r
        })
        .collect()
}

/// Points bucketed by cell, for neighbour queries.
struct Grid {
    cell: f64,
    cells: HashMap<(i64, i64), Vec<usize>>,
}

impl Grid {
    fn new(points: &[DVec2], cell: f64) -> Grid {
        let cell = cell.max(1e-3);
        let mut cells: HashMap<(i64, i64), Vec<usize>> = HashMap::new();
        for (i, p) in points.iter().enumerate() {
            cells.entry(Self::key(*p, cell)).or_default().push(i);
        }
        Grid { cell, cells }
    }

    fn key(p: DVec2, cell: f64) -> (i64, i64) {
        ((p.x / cell).floor() as i64, (p.y / cell).floor() as i64)
    }

    /// Indices of points within `r` of `p` (`r` at most the cell size).
    fn within(&self, points: &[DVec2], p: DVec2, r: f64) -> Vec<usize> {
        let (cx, cy) = Self::key(p, self.cell);
        let mut out = Vec::new();
        for x in cx - 1..=cx + 1 {
            for y in cy - 1..=cy + 1 {
                for &i in self.cells.get(&(x, y)).map_or(&[][..], Vec::as_slice) {
                    if points.get(i).is_some_and(|q| q.distance(p) <= r) {
                        out.push(i);
                    }
                }
            }
        }
        out
    }

    /// The nearest other point (not `skip`) within `r`.
    fn nearest(&self, points: &[DVec2], p: DVec2, skip: usize, r: f64) -> Option<DVec2> {
        self.within(points, p, r)
            .into_iter()
            .filter(|i| *i != skip)
            .filter_map(|i| points.get(i).copied())
            .filter(|q| q.distance(p) > 1e-9)
            .min_by(|a, b| a.distance(p).total_cmp(&b.distance(p)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tips_cover_small_and_large_areas() {
        let big = geom::rectangle(DVec2::ZERO, DVec2::new(20.0, 20.0));
        assert!(tips(&[big], 5.0).len() >= 9);
        let strip = geom::rectangle(DVec2::new(0.3, 0.3), DVec2::new(20.3, 0.6));
        let t = tips(&[strip], 5.0);
        assert!(t.len() >= 4 && t.len() <= 12, "{}", t.len());
        let dot = geom::rectangle(DVec2::new(1.1, 1.1), DVec2::new(1.4, 1.4));
        assert_eq!(tips(&[dot], 5.0).len(), 1);
    }

    #[test]
    fn radius_grows_and_stays_bounded() {
        let s = Settings::read(CosmicSlicer_config::Schema::get(), &CosmicSlicer_config::Config::default());
        let size = Sizes::new(&s);
        let r: Vec<f64> = (0..2000).step_by(100).map(|a| size.radius(a, 0.2)).collect();
        assert!(r.windows(2).all(|p| p[1] >= p[0]));
        assert!(r.iter().all(|x| x.is_finite() && *x <= 15.0));
    }
}
