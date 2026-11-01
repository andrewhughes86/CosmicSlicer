//! Indexed triangle meshes.

use glam::{Mat4, Vec3};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Aabb {
    pub min: Vec3,
    pub max: Vec3,
}

impl Aabb {
    pub const EMPTY: Self = Self { min: Vec3::splat(f32::INFINITY), max: Vec3::splat(f32::NEG_INFINITY) };

    pub fn include(&mut self, p: Vec3) {
        self.min = self.min.min(p);
        self.max = self.max.max(p);
    }

    pub fn union(&self, other: &Aabb) -> Aabb {
        Aabb { min: self.min.min(other.min), max: self.max.max(other.max) }
    }

    pub fn is_valid(&self) -> bool {
        self.min.is_finite() && self.max.is_finite() && self.min.cmple(self.max).all()
    }

    pub fn size(&self) -> Vec3 {
        if self.is_valid() { self.max - self.min } else { Vec3::ZERO }
    }

    pub fn centre(&self) -> Vec3 {
        if self.is_valid() { (self.min + self.max) / 2.0 } else { Vec3::ZERO }
    }

    /// Distance along the ray to where it enters the box, if it does.
    pub fn ray_hit(&self, origin: Vec3, dir: Vec3) -> Option<f32> {
        let inv = dir.recip();
        let t0 = (self.min - origin) * inv;
        let t1 = (self.max - origin) * inv;
        let near = t0.min(t1).max_element();
        let far = t0.max(t1).min_element();
        (near <= far && far >= 0.0).then_some(near.max(0.0))
    }
}

/// Triangles over shared vertices.
#[derive(Clone, Debug, Default)]
pub struct Mesh {
    pub vertices: Vec<Vec3>,
    pub triangles: Vec<[u32; 3]>,
}

impl Mesh {
    pub fn triangle_count(&self) -> usize {
        self.triangles.len()
    }

    pub fn bounds(&self) -> Aabb {
        let mut b = Aabb::EMPTY;
        for v in &self.vertices {
            b.include(*v);
        }
        b
    }

    pub fn bounds_transformed(&self, m: &Mat4) -> Aabb {
        let mut b = Aabb::EMPTY;
        for v in &self.vertices {
            b.include(m.transform_point3(*v));
        }
        b
    }

    /// The corners of triangle `t`, if its indices are in range.
    pub fn corners(&self, t: &[u32; 3]) -> Option<[Vec3; 3]> {
        let v = |i: u32| self.vertices.get(i as usize).copied();
        Some([v(t[0])?, v(t[1])?, v(t[2])?])
    }

    /// The same mesh moved so its bounds are centred on X/Y 0 with the base at Z 0.
    pub fn centred(mut self) -> Self {
        let b = self.bounds();
        if b.is_valid() {
            let shift = Vec3::new(-b.centre().x, -b.centre().y, -b.min.z);
            for v in &mut self.vertices {
                *v += shift;
            }
        }
        self
    }

    /// Drops triangles that reference missing vertices or have non-finite corners.
    pub fn sanitise(&mut self) {
        let n = self.vertices.len() as u32;
        let finite: Vec<bool> = self.vertices.iter().map(|v| v.is_finite()).collect();
        self.triangles.retain(|t| t.iter().all(|&i| i < n && finite.get(i as usize).copied().unwrap_or(false)));
    }

    /// Nearest hit of a ray against the mesh transformed by `m` (Möller–Trumbore): the
    /// distance along the ray, in units of `dir`, and the triangle's index.
    pub fn ray_hit(&self, m: &Mat4, origin: Vec3, dir: Vec3) -> Option<(f32, usize)> {
        let inv = m.inverse();
        if !inv.is_finite() {
            return None;
        }
        let o = inv.transform_point3(origin);
        let d = inv.transform_vector3(dir);
        let mut best: Option<(f32, usize)> = None;
        for (index, t) in self.triangles.iter().enumerate() {
            let Some([a, b, c]) = self.corners(t) else { continue };
            let (e1, e2) = (b - a, c - a);
            let p = d.cross(e2);
            let det = e1.dot(p);
            if det.abs() < 1e-12 {
                continue;
            }
            let inv_det = 1.0 / det;
            let s = o - a;
            let u = s.dot(p) * inv_det;
            if !(0.0..=1.0).contains(&u) {
                continue;
            }
            let q = s.cross(e1);
            let v = d.dot(q) * inv_det;
            if v < 0.0 || u + v > 1.0 {
                continue;
            }
            let dist = e2.dot(q) * inv_det;
            if dist > 0.0 && best.is_none_or(|(b, _)| dist < b) {
                best = Some((dist, index));
            }
        }
        // `dist` is in units of `d`, which is `dir` mapped into object space, so it is
        // already the distance along the world ray in units of `dir`.
        best
    }

    /// The pieces that share no vertex, each as its own mesh (vertices renumbered).
    pub fn split(&self) -> Vec<Mesh> {
        let n = self.vertices.len();
        let mut parent: Vec<usize> = (0..n).collect();
        fn root(parent: &mut [usize], mut i: usize) -> usize {
            while let Some(&p) = parent.get(i) {
                if p == i {
                    break;
                }
                let grand = parent.get(p).copied().unwrap_or(p);
                if let Some(slot) = parent.get_mut(i) {
                    *slot = grand;
                }
                i = grand;
            }
            i
        }
        for t in &self.triangles {
            let a = root(&mut parent, t[0] as usize);
            for &v in &t[1..] {
                let b = root(&mut parent, v as usize);
                if a != b
                    && let Some(slot) = parent.get_mut(b)
                {
                    *slot = a;
                }
            }
        }
        let mut pieces: std::collections::BTreeMap<usize, (Mesh, std::collections::HashMap<u32, u32>)> =
            std::collections::BTreeMap::new();
        for t in &self.triangles {
            let r = root(&mut parent, t[0] as usize);
            let (mesh, map) = pieces.entry(r).or_default();
            let mut tri = [0u32; 3];
            for (k, &v) in t.iter().enumerate() {
                let next = mesh.vertices.len() as u32;
                let id = *map.entry(v).or_insert(next);
                if id == next {
                    mesh.vertices.push(self.vertices.get(v as usize).copied().unwrap_or(Vec3::ZERO));
                }
                tri[k] = id;
            }
            mesh.triangles.push(tri);
        }
        pieces.into_values().map(|(m, _)| m).collect()
    }

    /// The direction the largest flat area faces after `m`: the side to put down.
    pub fn largest_face_normal(&self, m: &Mat4) -> Option<Vec3> {
        let mut areas: std::collections::HashMap<[i32; 3], (f32, Vec3)> = std::collections::HashMap::new();
        for t in &self.triangles {
            let Some([a, b, c]) = self.corners(t) else { continue };
            let [a, b, c] = [a, b, c].map(|p| m.transform_point3(p));
            let cross = (b - a).cross(c - a);
            let area = cross.length() / 2.0;
            let n = cross.normalize_or_zero();
            if area <= 0.0 || n == Vec3::ZERO {
                continue;
            }
            // Faces within about 3° count as one flat.
            let key = (n * 20.0).round().as_ivec3().to_array();
            let e = areas.entry(key).or_insert((0.0, Vec3::ZERO));
            e.0 += area;
            e.1 += n * area;
        }
        areas
            .into_values()
            .max_by(|a, b| a.0.total_cmp(&b.0))
            .map(|(_, n)| n.normalize_or_zero())
            .filter(|n| *n != Vec3::ZERO)
    }

    /// Enclosed volume in mm³ (closed meshes only).
    pub fn volume(&self) -> f32 {
        self.triangles
            .iter()
            .filter_map(|t| self.corners(t))
            .map(|[a, b, c]| a.dot(b.cross(c)) / 6.0)
            .sum::<f32>()
            .abs()
    }
}

#[cfg(test)]
mod tests {
    use crate::primitives;
    use glam::{Mat4, Vec3};

    #[test]
    fn ray_hits_the_cube_top() {
        let m = primitives::cube(10.0).centred();
        let hit = m.ray_hit(&Mat4::IDENTITY, Vec3::new(0.0, 0.0, 50.0), Vec3::NEG_Z);
        assert!(hit.is_some_and(|(d, _)| (d - 40.0).abs() < 1e-4), "{hit:?}");
        let moved = Mat4::from_translation(Vec3::new(100.0, 0.0, 0.0));
        assert!(m.ray_hit(&moved, Vec3::new(0.0, 0.0, 50.0), Vec3::NEG_Z).is_none());
    }

    #[test]
    fn splits_into_pieces() {
        let mut two = primitives::cube(10.0);
        let other = primitives::cube(5.0);
        let base = two.vertices.len() as u32;
        two.vertices.extend(other.vertices.iter().map(|v| *v + Vec3::new(20.0, 0.0, 0.0)));
        two.triangles.extend(other.triangles.iter().map(|t| t.map(|i| i + base)));
        let pieces = two.split();
        assert_eq!(pieces.len(), 2);
        assert!(pieces.iter().all(|p| p.triangles.len() == 12 && p.vertices.len() == 8));
    }

    #[test]
    fn a_cube_lies_on_a_side() {
        let n = primitives::cube(10.0).largest_face_normal(&Mat4::IDENTITY);
        assert!(n.is_some_and(|n| n.abs().max_element() > 0.99));
    }

    #[test]
    fn cube_volume() {
        assert!((primitives::cube(10.0).volume() - 1000.0).abs() < 1e-2);
    }
}
