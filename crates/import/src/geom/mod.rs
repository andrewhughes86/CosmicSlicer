//! The geometry the readers need: meshes, and the exact CAD surfaces (B-rep, NURBS, trimmed
//! faces) that STEP, IGES and SAT models are tessellated from. From CosmicDMS's kernel.

pub mod brep;
pub mod mesh;
pub mod nurbs;
pub mod trim;

pub use glam::{DMat3, DMat4, DVec2, DVec3, Mat3, Mat4, Quat, Vec2, Vec3, Vec4};
pub use mesh::{Mesh, MeshError};

/// Axis-aligned bounding box in millimetres.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Aabb {
    pub min: Vec3,
    pub max: Vec3,
}

impl Aabb {
    /// An empty box that any `include` call will replace.
    pub const EMPTY: Self = Self { min: Vec3::splat(f32::INFINITY), max: Vec3::splat(f32::NEG_INFINITY) };

    pub fn new(a: Vec3, b: Vec3) -> Self {
        Self { min: a.min(b), max: a.max(b) }
    }

    /// Builds the box around `points`, skipping non-finite points.
    pub fn from_points(points: impl IntoIterator<Item = Vec3>) -> Self {
        let mut aabb = Self::EMPTY;
        for p in points {
            aabb.include(p);
        }
        aabb
    }

    pub fn include(&mut self, p: Vec3) {
        if p.is_finite() {
            self.min = self.min.min(p);
            self.max = self.max.max(p);
        }
    }

    pub fn is_empty(&self) -> bool {
        !(self.min.x <= self.max.x && self.min.y <= self.max.y && self.min.z <= self.max.z)
    }

    pub fn center(&self) -> Vec3 {
        if self.is_empty() { Vec3::ZERO } else { (self.min + self.max) * 0.5 }
    }

    pub fn size(&self) -> Vec3 {
        if self.is_empty() { Vec3::ZERO } else { self.max - self.min }
    }

    /// Radius of the sphere through the corners; zero for an empty box.
    pub fn radius(&self) -> f32 {
        self.size().length() * 0.5
    }
}

impl Default for Aabb {
    fn default() -> Self {
        Self::EMPTY
    }
}
