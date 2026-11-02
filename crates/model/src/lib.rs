//! Meshes, objects and plates, and the readers that load model files.
//!
//! Units are millimetres. The plate frame has its origin at the front-left corner of the
//! bed, X to the right, Y to the back and Z up, as in OrcaSlicer.

#![allow(non_snake_case)]

pub mod arrange;
pub mod boolean;
pub mod cut;
pub mod layers;
pub mod mesh;
pub mod obj;
pub mod orient;
pub mod primitives;
pub mod repair;
pub mod shape2d;
pub mod stl;
pub mod threemf;

use std::path::Path;
use std::sync::Arc;

use glam::{EulerRot, Mat4, Quat, Vec3};

pub use mesh::{Aabb, Mesh};
pub use threemf::Paint;

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

/// Extensions the Import dialog offers: every model format the readers know, and zip
/// archives of them.
pub fn import_extensions() -> Vec<&'static str> {
    let mut out: Vec<&'static str> =
        CosmicSlicer_import::ModelFormat::ALL.iter().flat_map(|f| f.extensions().iter().copied()).collect();
    out.push("zip");
    out
}

/// One object read from a model file.
#[derive(Clone, Debug)]
pub struct Loaded {
    pub name: String,
    /// In millimetres, placed as the file places it.
    pub mesh: Mesh,
    pub paint: Option<Paint>,
}

/// What [`load`] read, and what it skipped or approximated.
#[derive(Clone, Debug, Default)]
pub struct LoadedFile {
    pub objects: Vec<Loaded>,
    pub warnings: Vec<String>,
}

/// Most model files read from one zip archive.
const MAX_ZIP_MODELS: usize = 500;

fn from_import(m: &CosmicSlicer_import::geom::Mesh) -> Mesh {
    let mut mesh = Mesh { vertices: m.positions().to_vec(), triangles: m.triangles().to_vec() };
    mesh.sanitise();
    mesh
}

/// Reads one model from `bytes`, its format from `name`'s extension.
fn load_bytes(name: &str, bytes: &[u8], out: &mut LoadedFile) -> ModelResult<()> {
    let stem = Path::new(name).file_name().and_then(|n| n.to_str()).unwrap_or("Object").to_owned();
    let ext = Path::new(name).extension().and_then(|e| e.to_str()).unwrap_or_default().to_ascii_lowercase();
    match ext.as_str() {
        "3mf" => {
            // Each object (and each copy of it) on its own, as placed.
            let p = threemf::read(bytes)?;
            out.warnings.extend(p.warnings);
            let others = p.objects.iter().flat_map(|o| &o.parts).filter(|p| !p.kind.is_solid()).count();
            if others > 0 {
                out.warnings.push(format!(
                    "{others} modifier, negative or support volume(s) were left out; open the file as a project to keep them"
                ));
            }
            for o in &p.objects {
                let (mesh, paint) = o.merged();
                for inst in &o.instances {
                    let mut placed = mesh.clone();
                    for v in &mut placed.vertices {
                        *v = inst.matrix.transform_point3(*v);
                    }
                    let name = if o.name.is_empty() { stem.clone() } else { o.name.clone() };
                    let paint = (!paint.is_empty()).then(|| paint.clone());
                    out.objects.push(Loaded { name, mesh: placed, paint });
                }
            }
        }
        "zip" => {
            let z = CosmicSlicer_import::zip::Zip::open(bytes).map_err(|e| ModelError::Format(e.to_string()))?;
            let names: Vec<String> = z.names().map(str::to_owned).collect();
            let mut read = 0;
            for entry in names {
                let known = CosmicSlicer_import::ModelFormat::from_path(Path::new(&entry)).is_ok();
                if !known || entry.ends_with('/') || entry.contains("__MACOSX") {
                    continue;
                }
                read += 1;
                if read > MAX_ZIP_MODELS {
                    out.warnings.push(format!("only the first {MAX_ZIP_MODELS} models in the archive were read"));
                    break;
                }
                let data = z.read(&entry).map_err(|e| ModelError::Format(e.to_string()))?;
                if let Err(e) = load_bytes(&entry, &data, out) {
                    out.warnings.push(format!("{entry}: {e}"));
                }
            }
        }
        _ => {
            let options = CosmicSlicer_import::Options::default();
            let (imported, _) =
                CosmicSlicer_import::import_mesh_bytes(name, bytes, None, options).map_err(|e| match e {
                    CosmicSlicer_import::ImportError::UnsupportedFormat(f) => ModelError::Unsupported(f.to_uppercase()),
                    e => ModelError::Format(format!("{stem}: {e}")),
                })?;
            out.warnings.extend(imported.warnings);
            out.objects.push(Loaded { name: stem, mesh: from_import(&imported.mesh), paint: None });
        }
    }
    Ok(())
}

/// Reads a model file: STL, OBJ, 3MF, STEP, AMF and the other formats CosmicDMS reads, or a
/// zip archive of them. Each object comes back as the file places it.
pub fn load(path: &Path) -> ModelResult<LoadedFile> {
    let io = |source| ModelError::Io { path: path.display().to_string(), source };
    let len = std::fs::metadata(path).map_err(io)?.len();
    if len > MAX_FILE_BYTES {
        return Err(ModelError::Format(format!("{} is larger than 2 GB", path.display())));
    }
    let bytes = std::fs::read(path).map_err(io)?;
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("model");
    let mut out = LoadedFile::default();
    load_bytes(name, &bytes, &mut out)?;
    out.objects.retain(|o| o.mesh.triangle_count() > 0);
    if out.objects.is_empty() {
        return Err(ModelError::Format(format!("{} has no triangles", path.display())));
    }
    Ok(out)
}

/// What a part of an object is (OrcaSlicer's volume types).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum PartKind {
    #[default]
    Normal,
    Modifier,
    Negative,
    SupportEnforcer,
    SupportBlocker,
}

impl PartKind {
    pub fn orca(self) -> &'static str {
        match self {
            PartKind::Normal => "normal_part",
            PartKind::Modifier => "modifier_part",
            PartKind::Negative => "negative_part",
            PartKind::SupportEnforcer => "support_enforcer",
            PartKind::SupportBlocker => "support_blocker",
        }
    }

    pub fn from_orca(s: &str) -> PartKind {
        match s {
            "modifier_part" => PartKind::Modifier,
            "negative_part" | "negative_volume" => PartKind::Negative,
            "support_enforcer" => PartKind::SupportEnforcer,
            "support_blocker" => PartKind::SupportBlocker,
            _ => PartKind::Normal,
        }
    }
}

impl PartKind {
    pub const ALL: [PartKind; 5] =
        [PartKind::Normal, PartKind::Negative, PartKind::Modifier, PartKind::SupportEnforcer, PartKind::SupportBlocker];

    /// As OrcaSlicer's object list names it.
    pub fn label(self) -> &'static str {
        match self {
            PartKind::Normal => "Part",
            PartKind::Modifier => "Modifier",
            PartKind::Negative => "Negative volume",
            PartKind::SupportEnforcer => "Support enforcer",
            PartKind::SupportBlocker => "Support blocker",
        }
    }

    /// Whether it adds to the printed shape (normal parts) rather than changing settings or
    /// supports inside it.
    pub fn is_solid(self) -> bool {
        self == PartKind::Normal
    }
}

/// One more volume of an object besides its main mesh: a part, or a modifier, negative
/// volume or support enforcer/blocker.
#[derive(Clone, Debug)]
pub struct Volume {
    pub name: String,
    pub kind: PartKind,
    /// In the object's own frame (the frame its main mesh is in) before `transform`.
    pub mesh: Arc<Mesh>,
    /// Placement within the object.
    pub transform: Transform,
    /// Settings inside this volume (parts and modifiers).
    pub overrides: Vec<(String, String)>,
}

impl Volume {
    pub fn matrix(&self) -> Mat4 {
        self.transform.matrix()
    }
}

/// A brim ear placed by hand (for the "Painted" brim type): a disc on the plate under the object.
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct BrimEar {
    /// In the object's own frame.
    pub at: Vec3,
    pub radius: f32,
}

/// Settings for the layers between two heights (OrcaSlicer's "height range modifier").
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct HeightRange {
    /// Millimetres above the plate.
    pub lo: f32,
    pub hi: f32,
    pub overrides: Vec<(String, String)>,
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
    /// Painted supports, seams and colours, by triangle of `mesh`.
    pub paint: Option<Arc<Paint>>,
    /// More parts, modifiers, negative volumes and support enforcers/blockers.
    pub parts: Vec<Volume>,
    /// Settings by height.
    pub ranges: Vec<HeightRange>,
    /// Brim ears placed by hand.
    pub brim_ears: Vec<BrimEar>,
    /// Variable layer height: (height above the plate, layer height) points; empty for none.
    pub layer_profile: Vec<(f32, f32)>,
    /// Objects with the same non-zero `shared` are instances of one OrcaSlicer object: one
    /// mesh, parts, paint and settings, each placed on its own.
    pub shared: u64,
}

impl Object {
    pub fn matrix(&self) -> Mat4 {
        self.transform.matrix()
    }

    /// Bounds on the plate after the transform: the main mesh and the other parts that print.
    pub fn bounds(&self) -> Aabb {
        let m = self.matrix();
        self.parts
            .iter()
            .filter(|p| p.kind.is_solid())
            .fold(self.mesh.bounds_transformed(&m), |b, p| b.union(&p.mesh.bounds_transformed(&(m * p.matrix()))))
    }

    /// Every printed mesh with its placement on the plate.
    pub fn solids(&self) -> Vec<(&Mesh, Mat4)> {
        let m = self.matrix();
        std::iter::once((self.mesh.as_ref(), m))
            .chain(self.parts.iter().filter(|p| p.kind.is_solid()).map(|p| (p.mesh.as_ref(), m * p.matrix())))
            .collect()
    }

    /// Size on the plate after the transform.
    pub fn size(&self) -> Vec3 {
        self.bounds().size()
    }

    /// The convex hull of the object's shadow on the plate (what prints).
    pub fn footprint(&self) -> Vec<glam::Vec2> {
        let points: Vec<glam::Vec2> = self
            .solids()
            .iter()
            .flat_map(|(mesh, m)| mesh.vertices.iter().map(move |v| m.transform_point3(*v).truncate()))
            .collect();
        arrange::hull(&points)
    }

    /// Turns the object `angle` radians about Z round `pivot` and moves the pivot to `to`.
    pub fn turn_about(&mut self, angle: f32, pivot: glam::Vec2, to: glam::Vec2) {
        if !(angle.is_finite() && pivot.is_finite() && to.is_finite()) {
            return;
        }
        let q = Quat::from_rotation_z(angle);
        let rel = self.transform.offset - pivot.extend(0.0);
        let offset = q * rel + to.extend(0.0);
        if angle.abs() > 1e-6 {
            let mut t = self.transform;
            t.set_quat(q * t.quat());
            self.transform = t;
        }
        self.transform.offset = Vec3::new(offset.x, offset.y, self.transform.offset.z);
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
            paint: None,
            parts: Vec::new(),
            ranges: Vec::new(),
            brim_ears: Vec::new(),
            layer_profile: Vec::new(),
            shared: 0,
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
        copy.shared = 0;
        copy.transform.offset.x += copy.size().x + 5.0;
        self.objects.push(copy);
        Some(new_id)
    }

    /// Another instance of `id` placed beside it, sharing its mesh, parts, paint and
    /// settings; returns the new instance's id.
    pub fn add_instance(&mut self, id: u64) -> Option<u64> {
        let o = self.get_mut(id)?;
        if o.shared == 0 {
            o.shared = o.id;
        }
        let mut copy = o.clone();
        copy.transform.offset.x += copy.size().x + 5.0;
        Some(self.insert(copy))
    }

    /// The objects sharing `id`'s mesh and settings, `id` included (just `id` when it has no
    /// other instances).
    pub fn instances_of(&self, id: u64) -> Vec<u64> {
        match self.get(id).map(|o| o.shared) {
            None => Vec::new(),
            Some(0) => vec![id],
            Some(g) => self.objects.iter().filter(|o| o.shared == g).map(|o| o.id).collect(),
        }
    }

    /// Copies what instances share (mesh, parts, paint, settings, name, filament, ears and
    /// layer heights) from `id` to its other instances.
    pub fn sync_instances(&mut self, id: u64) {
        let Some(src) = self.get(id).filter(|o| o.shared != 0).cloned() else { return };
        for o in self.objects.iter_mut().filter(|o| o.shared == src.shared && o.id != id) {
            o.name.clone_from(&src.name);
            o.mesh = Arc::clone(&src.mesh);
            o.paint.clone_from(&src.paint);
            o.parts.clone_from(&src.parts);
            o.ranges.clone_from(&src.ranges);
            o.overrides.clone_from(&src.overrides);
            o.filament = src.filament;
            o.brim_ears.clone_from(&src.brim_ears);
            o.layer_profile.clone_from(&src.layer_profile);
            o.drop_to_bed();
        }
    }

    /// Makes `id` an object of its own, no longer sharing with its instances.
    pub fn make_individual(&mut self, id: u64) {
        let Some(g) = self.get(id).map(|o| o.shared).filter(|g| *g != 0) else { return };
        if let Some(o) = self.get_mut(id) {
            o.shared = 0;
        }
        let rest: Vec<u64> = self.objects.iter().filter(|o| o.shared == g).map(|o| o.id).collect();
        if let [last] = rest.as_slice()
            && let Some(o) = self.get_mut(*last)
        {
            o.shared = 0;
        }
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
        self.objects
            .iter()
            .map(|o| o.mesh.triangle_count() + o.parts.iter().map(|p| p.mesh.triangle_count()).sum::<usize>())
            .sum()
    }

    /// Adds a volume to object `id`, `mesh` centred at `at` in the object's frame; returns
    /// its index.
    pub fn add_volume(
        &mut self,
        id: u64,
        name: impl Into<String>,
        kind: PartKind,
        mesh: Mesh,
        at: Vec3,
    ) -> Option<usize> {
        let o = self.get_mut(id)?;
        let mesh = mesh.centred();
        let lift = mesh.bounds().size().z / 2.0;
        o.parts.push(Volume {
            name: name.into(),
            kind,
            mesh: Arc::new(mesh),
            transform: Transform { offset: at - Vec3::new(0.0, 0.0, lift), ..Default::default() },
            overrides: Vec::new(),
        });
        if kind.is_solid() {
            o.drop_to_bed();
        }
        Some(o.parts.len() - 1)
    }

    pub fn remove_volume(&mut self, id: u64, index: usize) {
        if let Some(o) = self.get_mut(id)
            && index < o.parts.len()
        {
            o.parts.remove(index);
            o.drop_to_bed();
        }
    }

    /// Splits object `id`'s main mesh into its separate pieces, as parts of the same object.
    /// Returns how many parts it then has.
    pub fn split_to_parts(&mut self, id: u64) -> usize {
        let Some(o) = self.get_mut(id) else { return 0 };
        let mut pieces = o.mesh.split();
        if pieces.len() < 2 {
            return pieces.len();
        }
        // The largest piece stays the main mesh.
        pieces.sort_by_key(|p| std::cmp::Reverse(p.triangle_count()));
        let mut it = pieces.into_iter();
        if let Some(main) = it.next() {
            o.mesh = Arc::new(main);
            o.paint = None;
        }
        for (k, piece) in it.enumerate() {
            o.parts.push(Volume {
                name: format!("{} part {}", o.name, k + 2),
                kind: PartKind::Normal,
                mesh: Arc::new(piece),
                transform: Transform::default(),
                overrides: Vec::new(),
            });
        }
        o.parts.len() + 1
    }

    /// Joins objects `ids` into the first of them, the others becoming its parts where they
    /// stand. Returns the joined object's id.
    pub fn assemble(&mut self, ids: &[u64]) -> Option<u64> {
        let (&first, rest) = ids.split_first()?;
        let inverse = self.get(first)?.matrix().inverse();
        let mut taken = Vec::new();
        for id in rest {
            if *id == first {
                continue;
            }
            let Some(o) = self.get(*id).cloned() else { continue };
            let m = inverse * o.matrix();
            let mut t = Transform::default();
            let (scale, rot, offset) = m.to_scale_rotation_translation();
            t.offset = offset;
            t.scale = scale;
            t.set_quat(rot);
            taken.push(Volume {
                name: o.name.clone(),
                kind: PartKind::Normal,
                mesh: o.mesh,
                transform: t,
                overrides: o.overrides,
            });
            for mut p in o.parts {
                let m = inverse * o.transform.matrix() * p.matrix();
                let (scale, rot, offset) = m.to_scale_rotation_translation();
                p.transform = Transform { offset, scale, ..Default::default() };
                p.transform.set_quat(rot);
                taken.push(p);
            }
            self.remove(&[*id]);
        }
        let o = self.get_mut(first)?;
        o.parts.extend(taken);
        Some(first)
    }

    /// Moves object `id` to position `to` in the list.
    pub fn reorder(&mut self, id: u64, to: usize) {
        let Some(from) = self.objects.iter().position(|o| o.id == id) else { return };
        let o = self.objects.remove(from);
        let to = to.min(self.objects.len());
        self.objects.insert(to, o);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parts_split_assemble_and_count_in_the_bounds() {
        let mut m = Model::new();
        let a = m.add("a", primitives::cube(10.0), Vec3::new(50.0, 50.0, 0.0), 0);
        let b = m.add("b", primitives::cube(10.0), Vec3::new(80.0, 50.0, 0.0), 0);
        let before = m.get(a).map(Object::bounds);
        // A modifier does not change the bounds; a part does.
        m.add_volume(a, "mod", PartKind::Modifier, primitives::cube(30.0), Vec3::ZERO);
        assert_eq!(m.get(a).map(Object::bounds), before);
        assert_eq!(m.assemble(&[a, b]), Some(a));
        assert_eq!(m.objects.len(), 1);
        let o = m.get(a);
        assert!(
            o.is_some_and(|o| o.parts.len() == 2 && (o.bounds().size().x - 40.0).abs() < 1e-3),
            "{:?}",
            o.map(Object::bounds)
        );
        // Two separate cubes in one mesh split into two parts.
        let mut two = primitives::cube(5.0);
        let off = two.vertices.len() as u32;
        two.vertices.extend(primitives::cube(5.0).vertices.iter().map(|v| *v + Vec3::new(10.0, 0.0, 0.0)));
        let tris: Vec<[u32; 3]> = primitives::cube(5.0).triangles.iter().map(|t| t.map(|i| i + off)).collect();
        two.triangles.extend(tris);
        let c = m.add("two", two, Vec3::new(50.0, 100.0, 0.0), 0);
        assert_eq!(m.split_to_parts(c), 2);
        assert!(m.get(c).is_some_and(|o| (o.bounds().size().x - 15.0).abs() < 1e-3));
        // Reordering.
        m.reorder(c, 0);
        assert_eq!(m.objects.first().map(|o| o.id), Some(c));
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
