//! Mesh repair on import, as OrcaSlicer does it: degenerate triangles dropped, each piece's
//! triangles turned the same way round and outwards, and small holes closed. What was done is
//! reported for the user.

use std::collections::{HashMap, VecDeque};

use glam::Vec3;

use crate::Mesh;

/// Holes with more edges than this are left open (they are rarely holes).
const MAX_HOLE_EDGES: usize = 500;

/// What [`repair`] changed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Report {
    /// Triangles with no area, or two corners the same, removed.
    pub degenerate: usize,
    /// Triangles turned round to face the same way as their neighbours (or outwards).
    pub flipped: usize,
    pub holes_closed: usize,
    /// Edges still with a triangle on one side only.
    pub open_edges: usize,
}

impl Report {
    pub fn changed(&self) -> bool {
        self.degenerate + self.flipped + self.holes_closed > 0
    }

    /// For the notice after an import.
    pub fn describe(&self) -> String {
        let mut parts = Vec::new();
        if self.degenerate > 0 {
            parts.push(format!("removed {} degenerate triangle(s)", self.degenerate));
        }
        if self.flipped > 0 {
            parts.push(format!("turned {} reversed triangle(s)", self.flipped));
        }
        if self.holes_closed > 0 {
            parts.push(format!("closed {} hole(s)", self.holes_closed));
        }
        let mut text = if parts.is_empty() { String::new() } else { format!("Repaired: {}.", parts.join(", ")) };
        if self.open_edges > 0 {
            if !text.is_empty() {
                text.push(' ');
            }
            text.push_str(&format!("{} open edge(s) remain; the model may slice with gaps.", self.open_edges));
        }
        text
    }
}

fn key(a: u32, b: u32) -> u64 {
    let (lo, hi) = if a < b { (a, b) } else { (b, a) };
    (u64::from(lo) << 32) | u64::from(hi)
}

/// Repairs `mesh` in place.
pub fn repair(mesh: &mut Mesh) -> Report {
    let mut r = Report::default();
    let n = mesh.vertices.len() as u32;
    let before = mesh.triangles.len();
    let verts = mesh.vertices.clone();
    let at = |i: u32| verts.get(i as usize).copied().unwrap_or(Vec3::ZERO);
    mesh.triangles.retain(|t| {
        let [a, b, c] = *t;
        if a >= n || b >= n || c >= n || a == b || b == c || a == c {
            return false;
        }
        let area = (at(b) - at(a)).cross(at(c) - at(a)).length();
        area.is_finite() && area > 1e-12
    });
    r.degenerate = before - mesh.triangles.len();

    // Triangles by undirected edge.
    let mut edges: HashMap<u64, Vec<u32>> = HashMap::with_capacity(mesh.triangles.len() * 3 / 2);
    for (ti, t) in mesh.triangles.iter().enumerate() {
        for k in 0..3 {
            edges.entry(key(t[k], t[(k + 1) % 3])).or_default().push(ti as u32);
        }
    }
    // Walk each piece, turning neighbours to match across shared (manifold) edges.
    let count = mesh.triangles.len();
    let mut seen = vec![false; count];
    let mut piece_of = vec![usize::MAX; count];
    let mut pieces = 0;
    for start in 0..count {
        if seen.get(start).copied().unwrap_or(true) {
            continue;
        }
        let mut queue = VecDeque::from([start]);
        if let Some(s) = seen.get_mut(start) {
            *s = true;
        }
        while let Some(ti) = queue.pop_front() {
            if let Some(p) = piece_of.get_mut(ti) {
                *p = pieces;
            }
            let Some(t) = mesh.triangles.get(ti).copied() else { continue };
            for k in 0..3 {
                let (a, b) = (t[k], t[(k + 1) % 3]);
                let Some(list) = edges.get(&key(a, b)) else { continue };
                if list.len() != 2 {
                    continue;
                }
                for &nb in list.iter().filter(|&&nb| nb as usize != ti) {
                    let nb = nb as usize;
                    if seen.get(nb).copied().unwrap_or(true) {
                        continue;
                    }
                    if let Some(s) = seen.get_mut(nb) {
                        *s = true;
                    }
                    // A neighbour facing the same way runs the shared edge the other way.
                    if let Some(u) = mesh.triangles.get_mut(nb) {
                        let consistent = (0..3).any(|j| u[j] == b && u[(j + 1) % 3] == a);
                        if !consistent {
                            u.swap(1, 2);
                            r.flipped += 1;
                        }
                    }
                    queue.push_back(nb);
                }
            }
        }
        pieces += 1;
    }
    // Each closed-ish piece faces outwards: positive volume.
    let mut volume = vec![0.0f64; pieces];
    for (ti, t) in mesh.triangles.iter().enumerate() {
        let (a, b, c) = (at(t[0]).as_dvec3(), at(t[1]).as_dvec3(), at(t[2]).as_dvec3());
        if let Some(v) = piece_of.get(ti).and_then(|p| volume.get_mut(*p)) {
            *v += a.dot(b.cross(c)) / 6.0;
        }
    }
    for (ti, t) in mesh.triangles.iter_mut().enumerate() {
        if piece_of.get(ti).and_then(|p| volume.get(*p)).is_some_and(|v| *v < 0.0) {
            t.swap(1, 2);
            r.flipped += 1;
        }
    }

    // Holes: loops of edges with a triangle on one side only, closed with a fan.
    let mut next: HashMap<u32, Vec<u32>> = HashMap::new();
    let mut open = 0;
    for t in &mesh.triangles {
        for k in 0..3 {
            let (a, b) = (t[k], t[(k + 1) % 3]);
            if edges.get(&key(a, b)).is_some_and(|l| l.len() == 1) {
                // The hole runs the other way round from its rim's triangles.
                next.entry(b).or_default().push(a);
                open += 1;
            }
        }
    }
    let mut used: HashMap<(u32, u32), bool> = HashMap::new();
    let starts: Vec<u32> = next.keys().copied().collect();
    let mut closed_edges = 0;
    for s in starts {
        while let Some(&first) = next.get(&s).and_then(|l| l.iter().find(|&&v| !used.contains_key(&(s, v)))) {
            let mut ring = vec![s];
            let mut cur = first;
            used.insert((s, first), true);
            let mut ok = false;
            while ring.len() <= MAX_HOLE_EDGES {
                if cur == s {
                    ok = true;
                    break;
                }
                ring.push(cur);
                let Some(&nx) = next.get(&cur).and_then(|l| l.iter().find(|&&v| !used.contains_key(&(cur, v)))) else {
                    break;
                };
                used.insert((cur, nx), true);
                cur = nx;
            }
            if !ok || ring.len() < 3 {
                continue;
            }
            let centre = ring.iter().map(|&i| at(i)).sum::<Vec3>() / ring.len() as f32;
            if !centre.is_finite() || mesh.vertices.len() >= u32::MAX as usize {
                continue;
            }
            let c = mesh.vertices.len() as u32;
            mesh.vertices.push(centre);
            for k in 0..ring.len() {
                let (Some(&a), Some(&b)) = (ring.get(k), ring.get((k + 1) % ring.len())) else { continue };
                mesh.triangles.push([a, b, c]);
            }
            closed_edges += ring.len();
            r.holes_closed += 1;
        }
    }
    r.open_edges = open - closed_edges.min(open);
    r
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::primitives;

    #[test]
    fn a_clean_mesh_is_left_alone() {
        let mut m = primitives::cube(10.0);
        let before = m.clone();
        let r = repair(&mut m);
        assert!(!r.changed() && r.open_edges == 0, "{r:?}");
        assert_eq!(m.triangles, before.triangles);
    }

    #[test]
    fn reversed_triangles_turn_round_and_faces_point_out() {
        let mut m = primitives::cube(10.0);
        if let Some(t) = m.triangles.get_mut(3) {
            t.swap(1, 2);
        }
        let r = repair(&mut m);
        assert_eq!(r.flipped, 1, "{r:?}");
        assert!((m.volume() - 1000.0).abs() < 1e-2);
        // Inside out entirely: all turned back.
        let mut inside_out = primitives::cube(10.0);
        for t in &mut inside_out.triangles {
            t.swap(1, 2);
        }
        let r = repair(&mut inside_out);
        assert_eq!(r.flipped, 12);
        let signed: f32 = inside_out
            .triangles
            .iter()
            .filter_map(|t| inside_out.corners(t))
            .map(|[a, b, c]| a.dot(b.cross(c)) / 6.0)
            .sum();
        assert!((signed - 1000.0).abs() < 1e-2, "{signed}");
    }

    #[test]
    fn holes_close_and_degenerates_go() {
        let mut m = primitives::cube(10.0);
        // Take away the top's two triangles, and add a sliver.
        let top: Vec<usize> = (0..m.triangles.len())
            .filter(|&i| m.corners(&m.triangles[i]).is_some_and(|c| c.iter().all(|v| v.z > 9.0)))
            .collect();
        for i in top.into_iter().rev() {
            m.triangles.remove(i);
        }
        m.triangles.push([0, 0, 1]);
        let r = repair(&mut m);
        assert_eq!(r.degenerate, 1);
        assert_eq!(r.holes_closed, 1);
        assert_eq!(r.open_edges, 0);
        assert!((m.volume() - 1000.0).abs() < 1.0, "{}", m.volume());
    }
}
