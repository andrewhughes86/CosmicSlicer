//! Flat shapes made solid: filling outlines (with holes) with triangles, and extruding them.
//! Cut faces and embossed text and SVG are made this way.

use glam::{Vec2, Vec3};
use spade::{ConstrainedDelaunayTriangulation, Point2, Triangulation};

use crate::Mesh;

/// Most outline points filled at once.
const MAX_POINTS: usize = 2_000_000;

/// Whether `p` is inside the loops, by the even-odd rule (holes are loops inside loops).
pub fn inside(loops: &[Vec<Vec2>], p: Vec2) -> bool {
    let mut odd = false;
    for l in loops {
        let n = l.len();
        for i in 0..n {
            let (Some(a), Some(b)) = (l.get(i), l.get((i + 1) % n)) else { continue };
            if (a.y > p.y) != (b.y > p.y) {
                let x = a.x + (p.y - a.y) / (b.y - a.y) * (b.x - a.x);
                if p.x < x {
                    odd = !odd;
                }
            }
        }
    }
    odd
}

/// Triangles filling `loops` (even-odd), as indices into the returned points, counter-clockwise.
pub fn fill(loops: &[Vec<Vec2>]) -> (Vec<Vec2>, Vec<[u32; 3]>) {
    let total: usize = loops.iter().map(Vec::len).sum();
    if !(3..=MAX_POINTS).contains(&total) {
        return (Vec::new(), Vec::new());
    }
    let mut cdt: ConstrainedDelaunayTriangulation<Point2<f64>> = ConstrainedDelaunayTriangulation::new();
    for l in loops.iter().filter(|l| l.len() >= 3) {
        let handles: Vec<_> = l
            .iter()
            .filter(|p| p.is_finite())
            .filter_map(|p| cdt.insert(Point2::new(f64::from(p.x), f64::from(p.y))).ok())
            .collect();
        for k in 0..handles.len() {
            let (Some(&a), Some(&b)) = (handles.get(k), handles.get((k + 1) % handles.len())) else { continue };
            if a != b && cdt.can_add_constraint(a, b) {
                cdt.add_constraint(a, b);
            }
        }
    }
    let points: Vec<Vec2> = cdt.vertices().map(|v| Vec2::new(v.position().x as f32, v.position().y as f32)).collect();
    let mut tris = Vec::new();
    for f in cdt.inner_faces() {
        let [a, b, c] = f.vertices();
        let (pa, pb, pc) = (a.position(), b.position(), c.position());
        let centre = Vec2::new(((pa.x + pb.x + pc.x) / 3.0) as f32, ((pa.y + pb.y + pc.y) / 3.0) as f32);
        if inside(loops, centre) {
            tris.push([a.index() as u32, b.index() as u32, c.index() as u32]);
        }
    }
    (points, tris)
}

/// `loops` (even-odd) extruded from z 0 up to `depth`.
pub fn extrude(loops: &[Vec<Vec2>], depth: f32) -> Mesh {
    let mut mesh = Mesh::default();
    let (points, tris) = fill(loops);
    if tris.is_empty() || !(depth.is_finite() && depth > 0.0) {
        return mesh;
    }
    let n = points.len() as u32;
    mesh.vertices.extend(points.iter().map(|p| p.extend(0.0)));
    mesh.vertices.extend(points.iter().map(|p| p.extend(depth)));
    for t in &tris {
        // Bottom faces down, top faces up.
        mesh.triangles.push([t[0], t[2], t[1]]);
        mesh.triangles.push([t[0] + n, t[1] + n, t[2] + n]);
    }
    // Sides: each outline edge, facing out of the filled side.
    for l in loops.iter().filter(|l| l.len() >= 3) {
        let k = l.len();
        for i in 0..k {
            let (Some(&a), Some(&b)) = (l.get(i), l.get((i + 1) % k)) else { continue };
            // A side built along a→b faces to the right of it; turn it round when the filled
            // side is on the right, whichever way the outline winds.
            let mid = (a + b) / 2.0;
            let right = Vec2::new(b.y - a.y, a.x - b.x).normalize_or_zero();
            let filled_left = inside(loops, mid - right * 1e-3);
            let (a, b) = if filled_left { (a, b) } else { (b, a) };
            let base = mesh.vertices.len() as u32;
            mesh.vertices.extend([a.extend(0.0), b.extend(0.0), b.extend(depth), a.extend(depth)]);
            mesh.triangles.push([base, base + 1, base + 2]);
            mesh.triangles.push([base, base + 2, base + 3]);
        }
    }
    mesh
}

/// Twice the signed area of a loop: positive counter-clockwise.
pub fn signed_area(l: &[Vec2]) -> f32 {
    let n = l.len();
    (0..n).filter_map(|i| Some(l.get(i)?.perp_dot(*l.get((i + 1) % n)?))).sum()
}

/// Points in 3D on a plane through `origin` with unit `normal`, and back.
#[derive(Clone, Copy, Debug)]
pub struct PlaneFrame {
    pub origin: Vec3,
    pub normal: Vec3,
    u: Vec3,
    v: Vec3,
}

impl PlaneFrame {
    pub fn new(origin: Vec3, normal: Vec3) -> PlaneFrame {
        let normal = normal.normalize_or(Vec3::Z);
        let u = normal.any_orthonormal_vector();
        let v = normal.cross(u);
        PlaneFrame { origin, normal, u, v }
    }

    pub fn to_2d(&self, p: Vec3) -> Vec2 {
        let d = p - self.origin;
        Vec2::new(d.dot(self.u), d.dot(self.v))
    }

    pub fn to_3d(&self, p: Vec2) -> Vec3 {
        self.origin + self.u * p.x + self.v * p.y
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn square(lo: f32, hi: f32) -> Vec<Vec2> {
        vec![Vec2::new(lo, lo), Vec2::new(hi, lo), Vec2::new(hi, hi), Vec2::new(lo, hi)]
    }

    #[test]
    fn a_square_with_a_hole_fills_and_extrudes() {
        let mut hole = square(4.0, 6.0);
        hole.reverse();
        let loops = vec![square(0.0, 10.0), hole];
        let (pts, tris) = fill(&loops);
        let area: f32 = tris
            .iter()
            .map(|t| {
                let [a, b, c] = t.map(|i| pts[i as usize]);
                (b - a).perp_dot(c - a) / 2.0
            })
            .sum();
        assert!((area - 96.0).abs() < 1e-3, "{area}");
        let m = extrude(&loops, 2.0);
        assert!((m.volume() - 192.0).abs() < 1e-2, "{}", m.volume());
        // Closed and facing out: the signed volume is positive.
        let signed: f32 =
            m.triangles.iter().filter_map(|t| m.corners(t)).map(|[a, b, c]| a.dot(b.cross(c)) / 6.0).sum();
        assert!((signed - 192.0).abs() < 1e-2, "{signed}");
    }
}
