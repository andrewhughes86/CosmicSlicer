//! Cutting a mesh in two along a plane, each side closed with a flat face where it was cut.

use std::collections::HashMap;

use glam::{Vec2, Vec3};

use crate::Mesh;
use crate::shape2d::{PlaneFrame, fill};

/// The two sides of a cut: above the plane (the side its normal points to) and below.
#[derive(Clone, Debug, Default)]
pub struct Halves {
    pub above: Mesh,
    pub below: Mesh,
}

/// Builds one side's mesh, reusing vertices.
#[derive(Default)]
struct Side {
    mesh: Mesh,
    /// Original vertex → this side's.
    old: HashMap<u32, u32>,
    /// Cut point (by the edge it is on) → this side's.
    cut: HashMap<(u32, u32), u32>,
}

impl Side {
    fn old(&mut self, i: u32, at: Vec3) -> u32 {
        let next = self.mesh.vertices.len() as u32;
        let id = *self.old.entry(i).or_insert(next);
        if id == next {
            self.mesh.vertices.push(at);
        }
        id
    }

    fn cut(&mut self, edge: (u32, u32), at: Vec3) -> u32 {
        let key = (edge.0.min(edge.1), edge.0.max(edge.1));
        let next = self.mesh.vertices.len() as u32;
        let id = *self.cut.entry(key).or_insert(next);
        if id == next {
            self.mesh.vertices.push(at);
        }
        id
    }
}

/// A corner of a triangle as it is cut: an original vertex, or a point on an edge.
#[derive(Clone, Copy)]
enum Corner {
    Old(u32),
    Cut(u32, u32),
}

/// Cuts `mesh` by the plane through `origin` with `normal`, closing both sides.
pub fn cut(mesh: &Mesh, origin: Vec3, normal: Vec3) -> Halves {
    let normal = normal.normalize_or(Vec3::Z);
    let d: Vec<f32> = mesh.vertices.iter().map(|v| (*v - origin).dot(normal)).collect();
    let dist = |i: u32| d.get(i as usize).copied().unwrap_or(0.0);
    let pos = |i: u32| mesh.vertices.get(i as usize).copied().unwrap_or(Vec3::ZERO);
    let point = |a: u32, b: u32| {
        let (da, db) = (dist(a), dist(b));
        let t = if (da - db).abs() > 1e-12 { da / (da - db) } else { 0.5 };
        pos(a) + (pos(b) - pos(a)) * t.clamp(0.0, 1.0)
    };
    let (mut up, mut down) = (Side::default(), Side::default());
    // The cut edges, as the above side's triangles run them.
    let mut segments: Vec<(Vec3, Vec3)> = Vec::new();
    for t in &mesh.triangles {
        let above: Vec<bool> = t.iter().map(|&i| dist(i) >= 0.0).collect();
        let count = above.iter().filter(|a| **a).count();
        let emit = |side: &mut Side, corners: &[Corner]| {
            let ids: Vec<u32> = corners
                .iter()
                .map(|c| match *c {
                    Corner::Old(i) => side.old(i, pos(i)),
                    Corner::Cut(a, b) => side.cut((a, b), point(a, b)),
                })
                .collect();
            for k in 1..ids.len().saturating_sub(1) {
                if let (Some(&a), Some(&b), Some(&c)) = (ids.first(), ids.get(k), ids.get(k + 1)) {
                    side.mesh.triangles.push([a, b, c]);
                }
            }
        };
        match count {
            3 => emit(&mut up, &[Corner::Old(t[0]), Corner::Old(t[1]), Corner::Old(t[2])]),
            0 => emit(&mut down, &[Corner::Old(t[0]), Corner::Old(t[1]), Corner::Old(t[2])]),
            _ => {
                // Walk round the triangle, splitting each edge that crosses.
                let (mut a_side, mut b_side) = (Vec::new(), Vec::new());
                let mut crossing = Vec::new();
                for k in 0..3 {
                    let (i, j) = (t[k], t[(k + 1) % 3]);
                    let (ai, aj) =
                        (above.get(k).copied().unwrap_or(false), above.get((k + 1) % 3).copied().unwrap_or(false));
                    if ai {
                        a_side.push(Corner::Old(i));
                    } else {
                        b_side.push(Corner::Old(i));
                    }
                    if ai != aj {
                        a_side.push(Corner::Cut(i, j));
                        b_side.push(Corner::Cut(i, j));
                        crossing.push((i, j, ai));
                    }
                }
                emit(&mut up, &a_side);
                emit(&mut down, &b_side);
                // The cut runs from where the triangle leaves the above side to where it
                // comes back, so the above side's cap sees it the right way round.
                if let [(a0, b0, from_above0), (a1, b1, _)] = crossing.as_slice() {
                    let (p, q) = (point(*a0, *b0), point(*a1, *b1));
                    segments.push(if *from_above0 { (p, q) } else { (q, p) });
                }
            }
        }
    }
    // Caps: the cut outlines, filled, facing out of each side.
    let frame = PlaneFrame::new(origin, normal);
    let loops = chain(&segments, &frame);
    let (pts, tris) = fill(&loops);
    if !tris.is_empty() {
        for (side, flip) in [(&mut up, true), (&mut down, false)] {
            let base = side.mesh.vertices.len() as u32;
            side.mesh.vertices.extend(pts.iter().map(|p| frame.to_3d(*p)));
            for t in &tris {
                let t = t.map(|i| i + base);
                // The fill faces along the normal: right for the side below, turned for above.
                side.mesh.triangles.push(if flip { [t[0], t[2], t[1]] } else { t });
            }
        }
    }
    Halves { above: up.mesh, below: down.mesh }
}

/// Joins cut segments end to end into loops, in the plane's 2D frame.
fn chain(segments: &[(Vec3, Vec3)], frame: &PlaneFrame) -> Vec<Vec<Vec2>> {
    let key = |p: Vec3| ((p.x * 1e4).round() as i64, (p.y * 1e4).round() as i64, (p.z * 1e4).round() as i64);
    let mut from: HashMap<(i64, i64, i64), Vec<usize>> = HashMap::new();
    for (i, (a, _)) in segments.iter().enumerate() {
        from.entry(key(*a)).or_default().push(i);
    }
    let mut used = vec![false; segments.len()];
    let mut loops = Vec::new();
    for start in 0..segments.len() {
        if used.get(start).copied().unwrap_or(true) {
            continue;
        }
        let mut ring = Vec::new();
        let mut cur = start;
        loop {
            if let Some(u) = used.get_mut(cur) {
                *u = true;
            }
            let Some(&(a, b)) = segments.get(cur) else { break };
            ring.push(frame.to_2d(a));
            let next = from
                .get(&key(b))
                .and_then(|list| list.iter().copied().find(|&i| !used.get(i).copied().unwrap_or(true)));
            match next {
                Some(n) => cur = n,
                None => break,
            }
            if ring.len() > segments.len() {
                break;
            }
        }
        if ring.len() >= 3 {
            loops.push(ring);
        }
    }
    loops
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::primitives;

    #[test]
    fn a_cube_cut_in_two_makes_two_closed_boxes() {
        let cube = primitives::cube(10.0);
        let h = cut(&cube, Vec3::new(0.0, 0.0, 3.0), Vec3::Z);
        assert!((h.above.volume() - 700.0).abs() < 0.5, "{}", h.above.volume());
        assert!((h.below.volume() - 300.0).abs() < 0.5, "{}", h.below.volume());
        for m in [&h.above, &h.below] {
            let signed: f32 =
                m.triangles.iter().filter_map(|t| m.corners(t)).map(|[a, b, c]| a.dot(b.cross(c)) / 6.0).sum();
            assert!(signed > 0.0, "{signed}");
        }
        // A tilted cut through a cylinder closes too.
        let cyl = primitives::cylinder(5.0, 20.0, 32);
        let h = cut(&cyl, Vec3::new(0.0, 0.0, 10.0), Vec3::new(0.3, 0.0, 1.0));
        let total = h.above.volume() + h.below.volume();
        assert!((total - cyl.volume()).abs() < 1.0, "{total} vs {}", cyl.volume());
    }

    #[test]
    fn a_ring_cut_keeps_its_hole() {
        let torus = primitives::torus(15.0, 5.0, 48, 24);
        let h = cut(&torus, Vec3::new(0.0, 0.0, torus.bounds().centre().z), Vec3::Z);
        let total = h.above.volume() + h.below.volume();
        assert!((total - torus.volume()).abs() / torus.volume() < 0.01, "{total} vs {}", torus.volume());
    }
}
