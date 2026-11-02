//! Indexed triangle meshes: the faceted form of imported models.

use std::collections::HashMap;

use crate::geom::{Aabb, Vec3};

/// A triangle mesh in millimetres. Every index in `triangles` is below `positions.len()`;
/// [`Mesh::new`] checks it, so code holding a `Mesh` may rely on it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Mesh {
    positions: Vec<Vec3>,
    triangles: Vec<[u32; 3]>,
    bounds: Aabb,
}

/// Why [`Mesh::new`] refused its input.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MeshError {
    /// A triangle refers to a vertex that does not exist.
    IndexOutOfRange { triangle: usize, index: u32 },
    /// A vertex has a NaN or infinite coordinate.
    NonFinite { vertex: usize },
    /// More vertices than a `u32` index can address.
    TooManyVertices,
}

impl std::fmt::Display for MeshError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::IndexOutOfRange { triangle, index } => {
                write!(f, "triangle {triangle} refers to missing vertex {index}")
            }
            Self::NonFinite { vertex } => write!(f, "vertex {vertex} has a non-finite coordinate"),
            Self::TooManyVertices => write!(f, "too many vertices"),
        }
    }
}

impl std::error::Error for MeshError {}

impl Mesh {
    pub fn new(positions: Vec<Vec3>, triangles: Vec<[u32; 3]>) -> Result<Self, MeshError> {
        if u32::try_from(positions.len()).is_err() {
            return Err(MeshError::TooManyVertices);
        }
        if let Some(vertex) = positions.iter().position(|p| !p.is_finite()) {
            return Err(MeshError::NonFinite { vertex });
        }
        let n = positions.len();
        for (triangle, t) in triangles.iter().enumerate() {
            if let Some(&index) = t.iter().find(|&&i| i as usize >= n) {
                return Err(MeshError::IndexOutOfRange { triangle, index });
            }
        }
        let bounds = Aabb::from_points(positions.iter().copied());
        Ok(Self { positions, triangles, bounds })
    }

    /// Builds a mesh from triangle soup, merging vertices with identical coordinates.
    /// Non-finite triangles are dropped; so are triangles that collapse to a line or point
    /// after merging.
    pub fn from_soup(soup: impl IntoIterator<Item = [Vec3; 3]>) -> Result<Self, MeshError> {
        let mut index: HashMap<[u32; 3], u32> = HashMap::new();
        let mut positions = Vec::new();
        let mut triangles = Vec::new();
        for tri in soup {
            if !tri.iter().all(|p| p.is_finite()) {
                continue;
            }
            let mut t = [0u32; 3];
            for (slot, p) in t.iter_mut().zip(tri) {
                // -0.0 and 0.0 are the same point.
                let key = (p + Vec3::ZERO).to_array().map(f32::to_bits);
                *slot = match index.get(&key) {
                    Some(&i) => i,
                    None => {
                        let i = u32::try_from(positions.len()).map_err(|_| MeshError::TooManyVertices)?;
                        positions.push(p);
                        index.insert(key, i);
                        i
                    }
                };
            }
            if t[0] != t[1] && t[1] != t[2] && t[0] != t[2] {
                triangles.push(t);
            }
        }
        Self::new(positions, triangles)
    }

    pub fn positions(&self) -> &[Vec3] {
        &self.positions
    }

    pub fn triangles(&self) -> &[[u32; 3]] {
        &self.triangles
    }

    pub fn bounds(&self) -> Aabb {
        self.bounds
    }

    pub fn is_empty(&self) -> bool {
        self.triangles.is_empty()
    }

    /// The three corners of triangle `t`.
    pub fn corners(&self, t: [u32; 3]) -> [Vec3; 3] {
        t.map(|i| self.positions.get(i as usize).copied().unwrap_or(Vec3::ZERO))
    }

    /// Unit normal of triangle `t` by its winding; zero for a degenerate triangle.
    pub fn face_normal(&self, t: [u32; 3]) -> Vec3 {
        let [a, b, c] = self.corners(t);
        (b - a).cross(c - a).normalize_or_zero()
    }

    /// The same mesh scaled about the origin. `factor` must be finite and positive.
    pub fn scaled(&self, factor: f32) -> Result<Self, MeshError> {
        Self::new(self.positions.iter().map(|p| *p * factor).collect(), self.triangles.clone())
    }

    /// The nearest triangle a ray hits from either side: (distance along the unit
    /// `direction`, triangle index). Brute force; fine for picking at click rate.
    pub fn raycast(&self, origin: Vec3, direction: Vec3) -> Option<(f32, usize)> {
        let dir = direction.normalize_or_zero();
        if dir == Vec3::ZERO || !origin.is_finite() {
            return None;
        }
        let mut best: Option<(f32, usize)> = None;
        for (i, &t) in self.triangles.iter().enumerate() {
            let [a, b, c] = self.corners(t);
            // Möller–Trumbore.
            let (e1, e2) = (b - a, c - a);
            let pv = dir.cross(e2);
            let det = e1.dot(pv);
            if det.abs() < 1.0e-12 {
                continue;
            }
            let inv = 1.0 / det;
            let tv = origin - a;
            let u = tv.dot(pv) * inv;
            if !(0.0..=1.0).contains(&u) {
                continue;
            }
            let qv = tv.cross(e1);
            let v = dir.dot(qv) * inv;
            if v < 0.0 || u + v > 1.0 {
                continue;
            }
            let dist = e2.dot(qv) * inv;
            if dist > 0.0 && dist.is_finite() && best.is_none_or(|(d, _)| dist < d) {
                best = Some((dist, i));
            }
        }
        best
    }

    /// Edges to outline: open boundaries, edges shared by more than two triangles and
    /// edges whose neighbouring faces meet at more than `crease_degrees`.
    pub fn feature_edges(&self, crease_degrees: f32) -> Vec<[u32; 2]> {
        let cos_limit = crease_degrees.to_radians().cos();
        let mut faces: HashMap<[u32; 2], Vec<u32>> = HashMap::with_capacity(self.triangles.len() * 2);
        for (fi, t) in self.triangles.iter().enumerate() {
            for (a, b) in [(t[0], t[1]), (t[1], t[2]), (t[2], t[0])] {
                faces.entry([a.min(b), a.max(b)]).or_default().push(fi as u32);
            }
        }
        let normal = |fi: u32| self.triangles.get(fi as usize).map_or(Vec3::ZERO, |t| self.face_normal(*t));
        let mut edges: Vec<[u32; 2]> = faces
            .into_iter()
            .filter(|(_, f)| match f.as_slice() {
                [a, b] => normal(*a).dot(normal(*b)) < cos_limit,
                _ => true,
            })
            .map(|(e, _)| e)
            .collect();
        edges.sort_unstable();
        edges
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cube() -> Mesh {
        let c = |x: f32, y: f32, z: f32| Vec3::new(x, y, z);
        let p = [
            c(0., 0., 0.),
            c(1., 0., 0.),
            c(1., 1., 0.),
            c(0., 1., 0.),
            c(0., 0., 1.),
            c(1., 0., 1.),
            c(1., 1., 1.),
            c(0., 1., 1.),
        ];
        let quads = [[0, 3, 2, 1], [4, 5, 6, 7], [0, 1, 5, 4], [2, 3, 7, 6], [1, 2, 6, 5], [3, 0, 4, 7]];
        let soup = quads.iter().flat_map(|q: &[usize; 4]| [[p[q[0]], p[q[1]], p[q[2]]], [p[q[0]], p[q[2]], p[q[3]]]]);
        Mesh::from_soup(soup).unwrap_or_default()
    }

    #[test]
    fn soup_is_welded() {
        let m = cube();
        assert_eq!(m.positions().len(), 8);
        assert_eq!(m.triangles().len(), 12);
        assert_eq!(m.bounds(), Aabb::new(Vec3::ZERO, Vec3::ONE));
    }

    #[test]
    fn cube_outlines_its_twelve_edges_not_diagonals() {
        assert_eq!(cube().feature_edges(30.0).len(), 12);
    }

    #[test]
    fn raycast_hits_the_nearest_face() {
        let m = cube();
        let hit = m.raycast(Vec3::new(0.5, 0.5, 5.0), Vec3::NEG_Z);
        assert!(hit.is_some_and(|(d, t)| (d - 4.0).abs() < 1e-5 && m.face_normal(m.triangles()[t]).z > 0.99));
        assert!(m.raycast(Vec3::new(5.0, 5.0, 5.0), Vec3::NEG_Z).is_none());
        assert!(m.raycast(Vec3::new(0.5, 0.5, 5.0), Vec3::ZERO).is_none());
    }

    #[test]
    fn new_rejects_bad_indices_and_coordinates() {
        assert_eq!(
            Mesh::new(vec![Vec3::ZERO], vec![[0, 0, 1]]),
            Err(MeshError::IndexOutOfRange { triangle: 0, index: 1 })
        );
        assert_eq!(Mesh::new(vec![Vec3::NAN], vec![]), Err(MeshError::NonFinite { vertex: 0 }));
    }

    #[test]
    fn soup_drops_degenerate_and_non_finite_triangles() {
        let a = Vec3::ZERO;
        let b = Vec3::X;
        let m = Mesh::from_soup([[a, a, b], [a, b, Vec3::INFINITY], [a, b, Vec3::Y]]).unwrap_or_default();
        assert_eq!(m.triangles().len(), 1);
        assert_eq!(m.positions().len(), 3);
    }
}
