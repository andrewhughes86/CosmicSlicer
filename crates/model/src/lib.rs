//! Meshes, objects and plates, and the readers that load model files.
//!
//! Units are millimetres. The plate frame has its origin at the front-left corner of the
//! bed, X to the right, Y to the back and Z up, as in OrcaSlicer.

#![allow(non_snake_case)]

pub mod mesh;
pub mod obj;
pub mod primitives;
pub mod stl;

use std::path::Path;
use std::sync::Arc;

use glam::{EulerRot, Mat4, Quat, Vec3};

pub use mesh::{Aabb, Mesh};

#[derive(Debug, thiserror::Error)]
pub enum ModelError {
    #[error("could not read {path}: {source}")]
    Io { path: String, source: std::io::Error },
    #[error("{0}")]
    Format(String),
    #[error("{0} files are not supported yet")]
    Unsupported(String),
}

pub type ModelResult<T> = Result<T, ModelError>;

/// Largest model file read, so a hostile file cannot exhaust memory.
pub const MAX_FILE_BYTES: u64 = 2 << 30;

/// Extensions the Import dialog offers.
pub const IMPORT_EXTENSIONS: &[&str] = &["stl", "obj"];

/// Reads a model file into a mesh, by its extension.
pub fn load(path: &Path) -> ModelResult<Mesh> {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or_default().to_ascii_lowercase();
    let io = |source| ModelError::Io { path: path.display().to_string(), source };
    let len = std::fs::metadata(path).map_err(io)?.len();
    if len > MAX_FILE_BYTES {
        return Err(ModelError::Format(format!("{} is larger than 2 GB", path.display())));
    }
    let bytes = std::fs::read(path).map_err(io)?;
    let mesh = match ext.as_str() {
        "stl" => stl::parse(&bytes)?,
        "obj" => obj::parse(&bytes)?,
        other => return Err(ModelError::Unsupported(other.to_uppercase())),
    };
    if mesh.triangle_count() == 0 {
        return Err(ModelError::Format(format!("{} has no triangles", path.display())));
    }
    Ok(mesh)
}

/// Placement of an object on the plate: scale, then rotate, then move.
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Transform {
    pub offset: Vec3,
    /// Rotation about X, Y, Z in degrees, applied in that order.
    pub rotation: Vec3,
    pub scale: Vec3,
}

impl Default for Transform {
    fn default() -> Self {
        Self { offset: Vec3::ZERO, rotation: Vec3::ZERO, scale: Vec3::ONE }
    }
}

impl Transform {
    pub fn matrix(&self) -> Mat4 {
        Mat4::from_scale_rotation_translation(self.scale, self.quat(), self.offset)
    }

    pub fn quat(&self) -> Quat {
        let r = self.rotation * (std::f32::consts::PI / 180.0);
        Quat::from_euler(EulerRot::ZYX, r.z, r.y, r.x)
    }

    pub fn set_quat(&mut self, q: Quat) {
        let (z, y, x) = q.normalize().to_euler(EulerRot::ZYX);
        let deg = Vec3::new(x, y, z) * (180.0 / std::f32::consts::PI);
        // Tidy the round-off so the panels show 90, not 89.99999.
        self.rotation = (deg * 1e4).round() / 1e4;
    }
}

/// One printable object: a mesh, where it sits, and what prints it.
#[derive(Clone, Debug)]
pub struct Object {
    pub id: u64,
    pub name: String,
    /// The mesh as loaded, centred on its own origin with its base at Z 0.
    pub mesh: Arc<Mesh>,
    pub transform: Transform,
    /// 1-based filament (extruder) number.
    pub filament: usize,
    pub plate: usize,
    pub visible: bool,
    /// Settings this object overrides, by option key.
    pub overrides: Vec<(String, String)>,
}

impl Object {
    pub fn matrix(&self) -> Mat4 {
        self.transform.matrix()
    }

    /// Bounds on the plate after the transform.
    pub fn bounds(&self) -> Aabb {
        self.mesh.bounds_transformed(&self.matrix())
    }

    /// Size on the plate after the transform.
    pub fn size(&self) -> Vec3 {
        self.bounds().size()
    }

    /// Moves the object so its lowest point touches the plate.
    pub fn drop_to_bed(&mut self) {
        let b = self.bounds();
        if b.is_valid() {
            self.transform.offset.z -= b.min.z;
        }
    }
}

/// The objects in a project, the plates they sit on, and the selection.
#[derive(Clone, Debug, Default)]
pub struct Model {
    pub objects: Vec<Object>,
    pub plates: usize,
    next_id: u64,
}

impl Model {
    pub fn new() -> Self {
        Self { objects: Vec::new(), plates: 1, next_id: 1 }
    }

    /// Adds `mesh` centred at `at` on the plate, resting on it; returns the new object's id.
    pub fn add(&mut self, name: impl Into<String>, mesh: Mesh, at: Vec3, plate: usize) -> u64 {
        let mesh = mesh.centred();
        let id = self.next_id.max(1);
        self.next_id = id + 1;
        let mut object = Object {
            id,
            name: name.into(),
            mesh: Arc::new(mesh),
            transform: Transform { offset: at, ..Default::default() },
            filament: 1,
            plate,
            visible: true,
            overrides: Vec::new(),
        };
        object.drop_to_bed();
        self.objects.push(object);
        id
    }

    /// A copy of `id` placed beside it; returns the copy's id.
    pub fn duplicate(&mut self, id: u64) -> Option<u64> {
        let source = self.objects.iter().find(|o| o.id == id)?.clone();
        let new_id = self.next_id.max(1);
        self.next_id = new_id + 1;
        let mut copy = source;
        copy.id = new_id;
        copy.transform.offset.x += copy.size().x + 5.0;
        self.objects.push(copy);
        Some(new_id)
    }

    /// Adds `object` under a new id; returns the id.
    pub fn insert(&mut self, mut object: Object) -> u64 {
        let id = self.next_id.max(1);
        self.next_id = id + 1;
        object.id = id;
        self.objects.push(object);
        id
    }

    pub fn get(&self, id: u64) -> Option<&Object> {
        self.objects.iter().find(|o| o.id == id)
    }

    pub fn get_mut(&mut self, id: u64) -> Option<&mut Object> {
        self.objects.iter_mut().find(|o| o.id == id)
    }

    pub fn remove(&mut self, ids: &[u64]) {
        self.objects.retain(|o| !ids.contains(&o.id));
    }

    pub fn triangle_count(&self) -> usize {
        self.objects.iter().map(|o| o.mesh.triangle_count()).sum()
    }

    /// Lays the objects of `plate` out in rows across a `bed` sized plate, `gap` apart,
    /// largest first, centred on the bed.
    pub fn arrange(&mut self, plate: usize, bed_min: Vec3, bed_size: Vec3, gap: f32) {
        let mut order: Vec<usize> = (0..self.objects.len()).filter(|&i| self.objects[i].plate == plate).collect();
        order.sort_by(|&a, &b| {
            let (sa, sb) = (self.objects[a].size(), self.objects[b].size());
            (sb.x * sb.y).total_cmp(&(sa.x * sa.y))
        });
        /// Objects (index, centre x from the row's left) in one row, filled left to right;
        /// each row is as deep as its deepest object.
        #[derive(Default)]
        struct Row {
            items: Vec<(usize, f32)>,
            width: f32,
            depth: f32,
        }
        let mut rows: Vec<Row> = Vec::new();
        let mut current = Row::default();
        for &i in &order {
            let s = self.objects[i].size();
            if !current.items.is_empty() && current.width + gap + s.x > bed_size.x {
                rows.push(std::mem::take(&mut current));
            }
            let left = if current.items.is_empty() { 0.0 } else { current.width + gap };
            current.items.push((i, left + s.x / 2.0));
            current.width = left + s.x;
            current.depth = current.depth.max(s.y);
        }
        if !current.items.is_empty() {
            rows.push(current);
        }
        let total_d: f32 = rows.iter().map(|r| r.depth).sum::<f32>() + gap * rows.len().saturating_sub(1) as f32;
        let mut y = bed_min.y + bed_size.y / 2.0 + total_d / 2.0;
        for Row { items, width, depth } in rows {
            let left = bed_min.x + (bed_size.x - width) / 2.0;
            for (i, cx) in items {
                let Some(o) = self.objects.get_mut(i) else { continue };
                let b = o.bounds();
                let centre = (b.min + b.max) / 2.0;
                o.transform.offset.x += left + cx - centre.x;
                o.transform.offset.y += y - depth / 2.0 - centre.y;
            }
            y -= depth + gap;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arrange_keeps_objects_apart_and_on_the_bed() {
        let mut m = Model::new();
        for _ in 0..5 {
            m.add("cube", primitives::cube(20.0), Vec3::ZERO, 0);
        }
        m.arrange(0, Vec3::ZERO, Vec3::new(220.0, 220.0, 250.0), 6.0);
        let boxes: Vec<Aabb> = m.objects.iter().map(Object::bounds).collect();
        for (i, a) in boxes.iter().enumerate() {
            assert!(a.min.x >= 0.0 && a.max.x <= 220.0 && a.min.y >= 0.0 && a.max.y <= 220.0, "{a:?}");
            assert!(a.min.z.abs() < 1e-4);
            for b in boxes.iter().skip(i + 1) {
                let apart = a.max.x <= b.min.x + 1e-3
                    || b.max.x <= a.min.x + 1e-3
                    || a.max.y <= b.min.y + 1e-3
                    || b.max.y <= a.min.y + 1e-3;
                assert!(apart, "{a:?} overlaps {b:?}");
            }
        }
    }

    #[test]
    fn quaternions_round_trip() {
        let mut t = Transform { rotation: Vec3::new(30.0, -45.0, 120.0), ..Default::default() };
        let q = t.quat();
        t.set_quat(q);
        assert!(t.quat().angle_between(q) < 1e-4);
    }

    #[test]
    fn rotated_objects_still_rest_on_the_bed() {
        let mut m = Model::new();
        let id = m.add("cyl", primitives::cylinder(10.0, 30.0, 32), Vec3::new(50.0, 50.0, 0.0), 0);
        let o = m.get_mut(id);
        assert!(o.is_some());
        if let Some(o) = o {
            o.transform.rotation.x = 90.0;
            o.drop_to_bed();
            let b = o.bounds();
            assert!(b.min.z.abs() < 1e-3, "{b:?}");
            assert!((b.size().y - 30.0).abs() < 1e-3);
        }
    }
}
