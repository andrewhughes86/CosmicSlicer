//! An indexed triangle list that readers fill, with the shared caps.

use std::collections::HashMap;

use crate::geom::{Mat4, Mesh, Vec3};

use crate::{ImportError, MAX_TRIANGLES};

/// Most vertices in one imported model.
const MAX_VERTICES: usize = 3 * MAX_TRIANGLES;

#[derive(Default)]
pub struct Soup {
    positions: Vec<Vec3>,
    triangles: Vec<[u32; 3]>,
}

impl Soup {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn triangle_count(&self) -> usize {
        self.triangles.len()
    }

    /// Adds a vertex, returning its index. Non-finite points are refused when the mesh is built.
    pub fn vertex(&mut self, p: Vec3) -> Result<u32, ImportError> {
        if self.positions.len() >= MAX_VERTICES {
            return Err(ImportError::Malformed("too many vertices".into()));
        }
        let i = u32::try_from(self.positions.len()).map_err(|_| ImportError::Malformed("too many vertices".into()))?;
        self.positions.push(p);
        Ok(i)
    }

    /// Adds a triangle of earlier vertices; repeated corners are dropped.
    pub fn triangle(&mut self, a: u32, b: u32, c: u32) -> Result<(), ImportError> {
        let n = self.positions.len();
        if [a, b, c].iter().any(|&i| i as usize >= n) {
            return Err(ImportError::Malformed(format!("a triangle refers to vertex {} of {n}", a.max(b).max(c))));
        }
        if a == b || b == c || a == c {
            return Ok(());
        }
        if self.triangles.len() >= MAX_TRIANGLES {
            return Err(ImportError::TooManyTriangles);
        }
        self.triangles.push([a, b, c]);
        Ok(())
    }

    /// Adds a polygon of earlier vertices as a fan of triangles.
    pub fn polygon(&mut self, corners: &[u32]) -> Result<(), ImportError> {
        if let Some((&first, rest)) = corners.split_first() {
            for w in rest.windows(2) {
                if let [b, c] = *w {
                    self.triangle(first, b, c)?;
                }
            }
        }
        Ok(())
    }

    /// Adds a triangle by its corners.
    pub fn push(&mut self, t: [Vec3; 3]) -> Result<(), ImportError> {
        let a = self.vertex(t[0])?;
        let b = self.vertex(t[1])?;
        let c = self.vertex(t[2])?;
        self.triangle(a, b, c)
    }

    /// Adds an indexed mesh moved by `transform`. A mirroring transform flips the winding, so
    /// outsides stay outside. Indices must lie inside `positions`.
    pub fn add(&mut self, positions: &[Vec3], triangles: &[[u32; 3]], transform: &Mat4) -> Result<(), ImportError> {
        let base = self.positions.len();
        let mirrored = transform.determinant() < 0.0;
        for p in positions {
            self.vertex(transform.transform_point3(*p))?;
        }
        let at = |i: u32| -> Result<u32, ImportError> {
            if i as usize >= positions.len() {
                return Err(ImportError::Malformed(format!("index {i} is past the {} vertices", positions.len())));
            }
            u32::try_from(base + i as usize).map_err(|_| ImportError::Malformed("too many vertices".into()))
        };
        for t in triangles {
            let (a, b, c) = (at(t[0])?, at(t[1])?, at(t[2])?);
            if mirrored { self.triangle(a, c, b)? } else { self.triangle(a, b, c)? }
        }
        Ok(())
    }

    /// The size of the box around the vertices.
    pub fn extent(&self) -> f32 {
        let b = crate::geom::Aabb::from_points(self.positions.iter().copied());
        if b.min.x <= b.max.x { (b.max - b.min).length() } else { 0.0 }
    }

    pub fn into_mesh(self) -> Result<Mesh, ImportError> {
        Ok(Mesh::new(self.positions, self.triangles)?)
    }

    /// The mesh with vertices closer than `eps` merged (CAD faces meet at shared edges whose
    /// end points may differ in the last bits). Triangles that collapse are dropped.
    pub fn into_mesh_welded_within(self, eps: f32) -> Result<Mesh, ImportError> {
        if !eps.is_finite() || eps <= 0.0 {
            return self.into_welded_mesh();
        }
        let Self { positions, triangles } = self;
        // Cells much wider than `eps`: a point looks in a neighbour only near that wall.
        let size = eps * 64.0;
        let mut grid: HashMap<[i64; 3], Vec<u32>> = HashMap::new();
        let mut kept: Vec<Vec3> = Vec::new();
        let mut remap: Vec<u32> = Vec::with_capacity(positions.len());
        for &p in &positions {
            let q = p / size;
            if !q.is_finite() {
                return Err(ImportError::Malformed("a vertex is not finite".into()));
            }
            let c = [q.x.floor() as i64, q.y.floor() as i64, q.z.floor() as i64];
            let f = q - q.floor();
            let near = |x: f32| -> &'static [i64] {
                if x * 64.0 <= 1.0 {
                    &[0, -1]
                } else if (1.0 - x) * 64.0 <= 1.0 {
                    &[0, 1]
                } else {
                    &[0]
                }
            };
            let mut found = None;
            'search: for &dx in near(f.x) {
                for &dy in near(f.y) {
                    for &dz in near(f.z) {
                        if let Some(list) = grid.get(&[c[0] + dx, c[1] + dy, c[2] + dz]) {
                            for &i in list {
                                if kept.get(i as usize).is_some_and(|k| (*k - p).length() <= eps) {
                                    found = Some(i);
                                    break 'search;
                                }
                            }
                        }
                    }
                }
            }
            let i = match found {
                Some(i) => i,
                None => {
                    let i =
                        u32::try_from(kept.len()).map_err(|_| ImportError::Malformed("too many vertices".into()))?;
                    kept.push(p);
                    grid.entry(c).or_default().push(i);
                    i
                }
            };
            remap.push(i);
        }
        let tris: Vec<[u32; 3]> = triangles
            .into_iter()
            .map(|t| t.map(|i| remap.get(i as usize).copied().unwrap_or(0)))
            .filter(|t| t[0] != t[1] && t[1] != t[2] && t[0] != t[2])
            .collect();
        Ok(Mesh::new(kept, tris)?)
    }

    /// The mesh with vertices at identical coordinates merged, for formats that repeat them.
    pub fn into_welded_mesh(self) -> Result<Mesh, ImportError> {
        let Self { positions, triangles } = self;
        let soup = triangles.into_iter().map(|t| t.map(|i| positions.get(i as usize).copied().unwrap_or(Vec3::NAN)));
        Ok(Mesh::from_soup(soup)?)
    }
}
