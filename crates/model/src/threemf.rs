//! 3MF projects as OrcaSlicer and Bambu Studio write them, and plain 3MF models from other
//! programs.
//!
//! A 3MF file is a zip. `3D/3dmodel.model` lists the objects and where each instance stands
//! (the `build` items). OrcaSlicer keeps each object's parts in `3D/Objects/object_N.model`
//! (components of the main object), their names, filaments, part types and per-object
//! settings in `Metadata/model_settings.config` along with the plates, and the presets in
//! `Metadata/project_settings.config` (JSON). Plain 3MF files hold their meshes inline and
//! nothing else. Everything read is checked: counts are capped and indices range-checked, so
//! a damaged file is an error, never a crash.

use std::collections::HashMap;
use std::fmt::Write as _;

use CosmicSlicer_import::xml::{self, Ev};
use CosmicSlicer_import::zip::{self, Zip};
use glam::{Mat4, Vec3, Vec4};

use crate::{Mesh, ModelError, ModelResult};

/// Most vertices or triangles in one file.
const MAX_ITEMS: usize = 50_000_000;
/// Most objects, components or build items.
const MAX_OBJECTS: usize = 100_000;
/// Deepest nesting of components.
const MAX_DEPTH: usize = 16;

/// What a part of an object is (OrcaSlicer's volume types).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
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

    fn from_orca(s: &str) -> PartKind {
        match s {
            "modifier_part" => PartKind::Modifier,
            "negative_part" | "negative_volume" => PartKind::Negative,
            "support_enforcer" => PartKind::SupportEnforcer,
            "support_blocker" => PartKind::SupportBlocker,
            _ => PartKind::Normal,
        }
    }
}

/// Painted facets: OrcaSlicer's encoded strings by triangle, kept as they are so they save
/// back unchanged.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Paint {
    pub supports: Vec<(u32, String)>,
    pub seam: Vec<(u32, String)>,
    pub color: Vec<(u32, String)>,
}

impl Paint {
    pub fn is_empty(&self) -> bool {
        self.supports.is_empty() && self.seam.is_empty() && self.color.is_empty()
    }

    /// Appends `other`, whose triangles follow this mesh's `offset` triangles.
    pub fn append(&mut self, other: &Paint, offset: u32) {
        let shift =
            |v: &[(u32, String)]| v.iter().map(|(t, s)| (t.saturating_add(offset), s.clone())).collect::<Vec<_>>();
        self.supports.extend(shift(&other.supports));
        self.seam.extend(shift(&other.seam));
        self.color.extend(shift(&other.color));
    }

    /// Each painted triangle's support state: 1 enforced, 2 blocked (for a triangle painted
    /// in pieces, the state covering most of it).
    pub fn support_states(&self) -> Vec<(u32, u8)> {
        self.supports
            .iter()
            .filter_map(|(t, code)| decode_state(code).filter(|s| *s == 1 || *s == 2).map(|s| (*t, s)))
            .collect()
    }

    fn attrs(&self) -> HashMap<u32, Vec<(&'static str, &str)>> {
        let mut out: HashMap<u32, Vec<(&'static str, &str)>> = HashMap::new();
        for (key, list) in
            [("paint_supports", &self.supports), ("paint_seam", &self.seam), ("paint_color", &self.color)]
        {
            for (t, code) in list {
                out.entry(*t).or_default().push((key, code.as_str()));
            }
        }
        out
    }
}

/// The state most of a painted triangle has, from OrcaSlicer's encoding: hex digits read
/// from the end, each a node of the triangle's split tree (low two bits the number of
/// splits, the rest the state, 3 meaning "the next digit plus 3").
pub fn decode_state(code: &str) -> Option<u8> {
    let mut nibbles = code.chars().rev().filter_map(|c| c.to_digit(16));
    let mut share: HashMap<u8, f64> = HashMap::new();
    fn walk(
        nibbles: &mut dyn Iterator<Item = u32>,
        weight: f64,
        depth: usize,
        share: &mut HashMap<u8, f64>,
    ) -> Option<()> {
        if depth > 32 {
            return None;
        }
        let code = nibbles.next()?;
        let splits = code & 0b11;
        if splits == 0 {
            let mut state = (code >> 2) as u8;
            if state == 3 {
                state = 3u8.saturating_add(nibbles.next()? as u8);
            }
            *share.entry(state).or_default() += weight;
            return Some(());
        }
        let children = splits + 1;
        for _ in 0..children {
            walk(nibbles, weight / f64::from(children), depth + 1, share)?;
        }
        Some(())
    }
    walk(&mut nibbles, 1.0, 0, &mut share)?;
    share.into_iter().filter(|(s, _)| *s != 0).max_by(|a, b| a.1.total_cmp(&b.1)).map(|(s, _)| s)
}

/// A whole triangle in one state, encoded as OrcaSlicer reads it.
pub fn encode_state(state: u8) -> String {
    match state {
        0 => String::new(),
        1 | 2 => format!("{:X}", state << 2),
        s => format!("{:X}C", s.saturating_sub(3).min(15)),
    }
}

/// One part of an object, its mesh in the object's coordinates.
#[derive(Clone, Debug, Default)]
pub struct Part {
    pub name: String,
    pub kind: PartKind,
    pub mesh: Mesh,
    pub paint: Paint,
    pub settings: Vec<(String, String)>,
}

/// Where one copy of an object stands: its placement in the scene (plates laid out side by
/// side, as OrcaSlicer does) and the plate it belongs to, when the file says.
#[derive(Clone, Debug)]
pub struct Instance {
    pub matrix: Mat4,
    pub plate: Option<usize>,
}

#[derive(Clone, Debug, Default)]
pub struct ProjectObject {
    pub name: String,
    /// 1-based filament.
    pub extruder: usize,
    pub parts: Vec<Part>,
    pub instances: Vec<Instance>,
    pub settings: Vec<(String, String)>,
}

impl ProjectObject {
    /// The normal parts as one mesh, with their paint.
    pub fn merged(&self) -> (Mesh, Paint) {
        let mut mesh = Mesh::default();
        let mut paint = Paint::default();
        for p in self.parts.iter().filter(|p| p.kind == PartKind::Normal) {
            let base = mesh.vertices.len() as u32;
            paint.append(&p.paint, mesh.triangles.len() as u32);
            mesh.vertices.extend_from_slice(&p.mesh.vertices);
            mesh.triangles.extend(p.mesh.triangles.iter().map(|t| t.map(|i| i.saturating_add(base))));
        }
        (mesh, paint)
    }
}

#[derive(Clone, Debug, Default)]
pub struct Plate {
    pub name: String,
    pub locked: bool,
    /// The plate's own settings as OrcaSlicer saves them (`curr_bed_type`,
    /// `print_sequence`, `spiral_mode`, …).
    pub settings: Vec<(String, String)>,
    /// PNG.
    pub thumbnail: Option<Vec<u8>>,
}

#[derive(Clone, Debug, Default)]
pub struct Project {
    /// The program that wrote it, e.g. "OrcaSlicer-2.3.0".
    pub application: String,
    /// Title, Designer, Description, License, … from the model's metadata.
    pub metadata: Vec<(String, String)>,
    pub objects: Vec<ProjectObject>,
    pub plates: Vec<Plate>,
    /// `Metadata/project_settings.config`: the presets, as OrcaSlicer's JSON.
    pub settings: Option<String>,
    pub warnings: Vec<String>,
}

impl Project {
    /// Whether the file is a slicer project (plates and settings), not just a model.
    pub fn is_project(&self) -> bool {
        self.settings.is_some() || !self.plates.is_empty()
    }
}

fn bad(what: impl Into<String>) -> ModelError {
    ModelError::Format(format!("damaged 3MF: {}", what.into()))
}

fn import_error(e: CosmicSlicer_import::ImportError) -> ModelError {
    ModelError::Format(format!("3MF: {e}"))
}

// ---- reading ---------------------------------------------------------------------------

/// A mesh as a model file holds it, before placement.
#[derive(Default)]
struct ObjDef {
    name: String,
    vertices: Vec<Vec3>,
    triangles: Vec<[u32; 3]>,
    paint: Paint,
    /// (file, object id, transform); `None` file is this one.
    components: Vec<(Option<String>, String, Mat4)>,
}

/// One model file: its objects, build items (object id, transform) and metadata.
#[derive(Default)]
struct ModelFile {
    objects: HashMap<String, ObjDef>,
    items: Vec<(String, Mat4)>,
    metadata: Vec<(String, String)>,
    scale: f32,
}

/// A 3MF `transform` (twelve numbers, rows of a 4×3 matrix) as a matrix.
fn parse_transform(s: &str) -> Option<Mat4> {
    let v: Vec<f32> = s.split_whitespace().filter_map(|t| t.parse::<f32>().ok()).filter(|x| x.is_finite()).collect();
    if v.len() != 12 {
        return None;
    }
    let g = |i: usize| v.get(i).copied().unwrap_or(0.0);
    Some(Mat4::from_cols(
        Vec4::new(g(0), g(1), g(2), 0.0),
        Vec4::new(g(3), g(4), g(5), 0.0),
        Vec4::new(g(6), g(7), g(8), 0.0),
        Vec4::new(g(9), g(10), g(11), 1.0),
    ))
}

fn transform_text(m: &Mat4) -> String {
    let c = m.to_cols_array();
    let pick = [0, 1, 2, 4, 5, 6, 8, 9, 10, 12, 13, 14];
    pick.iter().map(|i| fmt_f(c.get(*i).copied().unwrap_or(0.0))).collect::<Vec<_>>().join(" ")
}

fn fmt_f(v: f32) -> String {
    if v == v.round() && v.abs() < 1e7 {
        format!("{}", v as i64)
    } else {
        let s = format!("{v:.6}");
        s.trim_end_matches('0').trim_end_matches('.').to_owned()
    }
}

fn unit_scale(unit: &str) -> f32 {
    match unit {
        "micron" => 0.001,
        "centimeter" => 10.0,
        "inch" => 25.4,
        "foot" => 304.8,
        "meter" => 1000.0,
        _ => 1.0,
    }
}

fn read_model_file(bytes: &[u8]) -> ModelResult<ModelFile> {
    let mut f = ModelFile { scale: 1.0, ..Default::default() };
    let mut current: Option<(String, ObjDef)> = None;
    let mut in_build = false;
    let mut meta_key: Option<String> = None;
    let mut meta_text = String::new();
    let mut count = 0usize;
    xml::stream(bytes, |ev| {
        let attr = |attrs: &[(String, String)], k: &str| attrs.iter().find(|(a, _)| a == k).map(|(_, v)| v.clone());
        match ev {
            Ev::Start(name, attrs) => match name {
                "model" => f.scale = unit_scale(&attr(attrs, "unit").unwrap_or_default()),
                "metadata" => {
                    meta_key = attr(attrs, "name");
                    meta_text.clear();
                }
                "object" => {
                    if f.objects.len() >= MAX_OBJECTS {
                        return Err(CosmicSlicer_import::ImportError::Malformed("too many objects".into()));
                    }
                    let id = attr(attrs, "id").unwrap_or_default();
                    current =
                        Some((id, ObjDef { name: attr(attrs, "name").unwrap_or_default(), ..Default::default() }));
                }
                "vertex" => {
                    if let Some((_, o)) = current.as_mut() {
                        count += 1;
                        if count > MAX_ITEMS {
                            return Err(CosmicSlicer_import::ImportError::TooManyTriangles);
                        }
                        let c = |k: &str| attr(attrs, k).and_then(|v| v.parse::<f32>().ok()).filter(|v| v.is_finite());
                        o.vertices.push(Vec3::new(c("x").unwrap_or(0.0), c("y").unwrap_or(0.0), c("z").unwrap_or(0.0)));
                    }
                }
                "triangle" => {
                    if let Some((_, o)) = current.as_mut() {
                        count += 1;
                        if count > MAX_ITEMS {
                            return Err(CosmicSlicer_import::ImportError::TooManyTriangles);
                        }
                        let i = |k: &str| attr(attrs, k).and_then(|v| v.parse::<u32>().ok());
                        let (Some(a), Some(b), Some(c)) = (i("v1"), i("v2"), i("v3")) else { return Ok(()) };
                        let t = o.triangles.len() as u32;
                        o.triangles.push([a, b, c]);
                        for (k, v) in attrs {
                            let list = match k.as_str() {
                                "paint_supports" | "custom_supports" => &mut o.paint.supports,
                                "paint_seam" | "custom_seam" => &mut o.paint.seam,
                                "paint_color" | "mmu_segmentation" => &mut o.paint.color,
                                _ => continue,
                            };
                            if !v.is_empty() {
                                list.push((t, v.clone()));
                            }
                        }
                    }
                }
                "component" => {
                    if let Some((_, o)) = current.as_mut()
                        && o.components.len() < MAX_OBJECTS
                    {
                        let m = attr(attrs, "transform").and_then(|t| parse_transform(&t)).unwrap_or(Mat4::IDENTITY);
                        o.components.push((attr(attrs, "path"), attr(attrs, "objectid").unwrap_or_default(), m));
                    }
                }
                "build" => in_build = true,
                "item"
                    if in_build && f.items.len() < MAX_OBJECTS && attr(attrs, "printable").as_deref() != Some("0") =>
                {
                    let m = attr(attrs, "transform").and_then(|t| parse_transform(&t)).unwrap_or(Mat4::IDENTITY);
                    f.items.push((attr(attrs, "objectid").unwrap_or_default(), m));
                }
                _ => {}
            },
            Ev::Text(t) => {
                if meta_key.is_some() {
                    meta_text.push_str(t);
                }
            }
            Ev::End(name) => match name {
                "object" => {
                    if let Some((id, o)) = current.take() {
                        f.objects.insert(id, o);
                    }
                }
                "metadata" => {
                    if let Some(k) = meta_key.take()
                        && current.is_none()
                    {
                        f.metadata.push((k, meta_text.trim().to_owned()));
                    }
                }
                "build" => in_build = false,
                _ => {}
            },
        }
        Ok(())
    })
    .map_err(import_error)?;
    Ok(f)
}

/// Per-object and per-part details from `Metadata/model_settings.config`.
#[derive(Default)]
struct ObjectConfig {
    name: Option<String>,
    extruder: Option<usize>,
    settings: Vec<(String, String)>,
    /// By part (component object) id.
    parts: HashMap<String, PartConfig>,
}

/// A part's name, kind and settings from `model_settings.config`.
type PartConfig = (Option<String>, PartKind, Vec<(String, String)>);

#[derive(Default)]
struct PlateConfig {
    index: usize,
    plate: Plate,
    thumbnail_file: Option<String>,
    /// (object id, instance index).
    instances: Vec<(String, usize)>,
}

/// Keys of `model_settings.config` that describe rather than set.
const OBJECT_META: &[&str] = &[
    "name",
    "extruder",
    "matrix",
    "source_file",
    "source_object_id",
    "source_volume_id",
    "source_offset_x",
    "source_offset_y",
    "source_offset_z",
    "source_in_inches",
    "source_in_meters",
    "mesh_stat",
];
const PLATE_META: &[&str] = &[
    "plater_id",
    "plater_name",
    "locked",
    "thumbnail_file",
    "thumbnail_no_light_file",
    "top_file",
    "pick_file",
    "pattern_bbox_file",
    "gcode_file",
    "slice_info_file",
];

fn read_model_settings(bytes: &[u8]) -> ModelResult<(HashMap<String, ObjectConfig>, Vec<PlateConfig>)> {
    let root = xml::tree(bytes).map_err(import_error)?;
    let mut objects = HashMap::new();
    let mut plates = Vec::new();
    let Some(config) = root.child("config") else { return Ok((objects, plates)) };
    let meta = |n: &xml::Node| -> Vec<(String, String)> {
        n.children_named("metadata")
            .filter_map(|m| Some((m.attr("key")?.to_owned(), m.attr("value").unwrap_or("").to_owned())))
            .collect()
    };
    for o in config.children_named("object") {
        let Some(id) = o.attr("id") else { continue };
        let mut oc = ObjectConfig::default();
        for (k, v) in meta(o) {
            match k.as_str() {
                "name" => oc.name = Some(v),
                "extruder" => oc.extruder = v.trim().parse().ok(),
                _ if OBJECT_META.contains(&k.as_str()) => {}
                _ => oc.settings.push((k, v)),
            }
        }
        for p in o.children_named("part") {
            let Some(pid) = p.attr("id") else { continue };
            let kind = PartKind::from_orca(p.attr("subtype").unwrap_or("normal_part"));
            let mut name = None;
            let mut settings = Vec::new();
            for (k, v) in meta(p) {
                match k.as_str() {
                    "name" => name = Some(v),
                    _ if OBJECT_META.contains(&k.as_str()) => {}
                    _ => settings.push((k, v)),
                }
            }
            oc.parts.insert(pid.to_owned(), (name, kind, settings));
        }
        objects.insert(id.to_owned(), oc);
    }
    for (n, p) in config.children_named("plate").enumerate() {
        let mut pc = PlateConfig { index: n, ..Default::default() };
        for (k, v) in meta(p) {
            match k.as_str() {
                "plater_id" => pc.index = v.trim().parse::<usize>().ok().and_then(|i| i.checked_sub(1)).unwrap_or(n),
                "plater_name" => pc.plate.name = v,
                "locked" => pc.plate.locked = v == "true" || v == "1",
                "thumbnail_file" => pc.thumbnail_file = Some(v),
                _ if PLATE_META.contains(&k.as_str()) => {}
                _ => pc.plate.settings.push((k, v)),
            }
        }
        for mi in p.children_named("model_instance") {
            let m = meta(mi);
            let get = |k: &str| m.iter().find(|(a, _)| a == k).map(|(_, v)| v.clone());
            if let Some(oid) = get("object_id") {
                pc.instances.push((oid, get("instance_id").and_then(|v| v.parse().ok()).unwrap_or(0)));
            }
        }
        plates.push(pc);
    }
    Ok((objects, plates))
}

/// The model file a 3MF's relationships name as its start part.
fn start_part(z: &Zip) -> String {
    let fallback = "3D/3dmodel.model".to_owned();
    let Ok(rels) = z.read("_rels/.rels") else { return fallback };
    let Ok(root) = xml::tree(&rels) else { return fallback };
    root.descendants("Relationship")
        .into_iter()
        .find(|r| r.attr("Type").is_some_and(|t| t.ends_with("/3dmodel")))
        .and_then(|r| r.attr("Target"))
        .map(|t| t.trim_start_matches('/').to_owned())
        .unwrap_or(fallback)
}

/// Collects the parts of object `id` in `file`, placed by `m`.
fn collect_parts(
    files: &HashMap<String, ModelFile>,
    file: &str,
    id: &str,
    m: Mat4,
    depth: usize,
    out: &mut Vec<(String, ObjDef)>,
) {
    if depth > MAX_DEPTH || out.len() >= MAX_OBJECTS {
        return;
    }
    let Some(f) = files.get(file) else { return };
    let Some(o) = f.objects.get(id) else { return };
    let scale = Mat4::from_scale(Vec3::splat(f.scale));
    if !o.triangles.is_empty() {
        let mm = m * scale;
        let def = ObjDef {
            name: o.name.clone(),
            vertices: o.vertices.iter().map(|v| mm.transform_point3(*v)).collect(),
            triangles: o.triangles.clone(),
            paint: o.paint.clone(),
            components: Vec::new(),
        };
        out.push((id.to_owned(), def));
    }
    for (path, cid, cm) in &o.components {
        let target = path.as_deref().map(|p| p.trim_start_matches('/').to_owned()).unwrap_or_else(|| file.to_owned());
        // Component placements are in the file's units, as the meshes are.
        let cm = scale * *cm * scale.inverse();
        collect_parts(files, &target, cid, m * cm, depth + 1, out);
    }
}

/// A part's mesh with bad triangles dropped (and their paint with them).
fn checked_mesh(def: ObjDef) -> (Mesh, Paint) {
    let n = def.vertices.len() as u32;
    let finite: Vec<bool> = def.vertices.iter().map(|v| v.is_finite()).collect();
    let ok = |t: &[u32; 3]| t.iter().all(|&i| i < n && finite.get(i as usize).copied().unwrap_or(false));
    let mut map: Vec<Option<u32>> = Vec::with_capacity(def.triangles.len());
    let mut triangles = Vec::with_capacity(def.triangles.len());
    for t in &def.triangles {
        if ok(t) {
            map.push(Some(triangles.len() as u32));
            triangles.push(*t);
        } else {
            map.push(None);
        }
    }
    let remap = |v: Vec<(u32, String)>| -> Vec<(u32, String)> {
        v.into_iter().filter_map(|(t, s)| map.get(t as usize).copied().flatten().map(|t| (t, s))).collect()
    };
    let paint =
        Paint { supports: remap(def.paint.supports), seam: remap(def.paint.seam), color: remap(def.paint.color) };
    (Mesh { vertices: def.vertices, triangles }, paint)
}

/// Reads a 3MF file: an OrcaSlicer or Bambu Studio project, or a plain 3MF model.
pub fn read(bytes: &[u8]) -> ModelResult<Project> {
    let z = Zip::open(bytes).map_err(import_error)?;
    let start = start_part(&z);
    let mut files: HashMap<String, ModelFile> = HashMap::new();
    let main = read_model_file(&z.read(&start).map_err(import_error)?)?;
    // Every model file components point into.
    let mut wanted: Vec<String> = main
        .objects
        .values()
        .flat_map(|o| o.components.iter().filter_map(|(p, _, _)| p.clone()))
        .map(|p| p.trim_start_matches('/').to_owned())
        .collect();
    files.insert(start.clone(), main);
    let mut guard = 0;
    while let Some(path) = wanted.pop() {
        guard += 1;
        if guard > MAX_OBJECTS {
            break;
        }
        if files.contains_key(&path) {
            continue;
        }
        let f = read_model_file(&z.read(&path).map_err(import_error)?)?;
        wanted.extend(
            f.objects
                .values()
                .flat_map(|o| o.components.iter().filter_map(|(p, _, _)| p.clone()))
                .map(|p| p.trim_start_matches('/').to_owned()),
        );
        files.insert(path, f);
    }
    let mut project = Project::default();
    let Some(main) = files.get(&start) else { return Err(bad("no model")) };
    project.metadata = main.metadata.clone();
    project.application =
        main.metadata.iter().find(|(k, _)| k == "Application").map(|(_, v)| v.clone()).unwrap_or_default();

    let (configs, plate_configs) = match z.read("Metadata/model_settings.config") {
        Ok(b) => read_model_settings(&b)?,
        Err(_) => (HashMap::new(), Vec::new()),
    };
    // Plates, and which plate each (object, instance) sits on.
    let mut plate_of: HashMap<(String, usize), usize> = HashMap::new();
    let mut plates: Vec<PlateConfig> = plate_configs;
    plates.sort_by_key(|p| p.index);
    for (n, pc) in plates.iter_mut().enumerate() {
        pc.index = n;
        for key in &pc.instances {
            plate_of.insert(key.clone(), n);
        }
        if let Some(t) = pc.thumbnail_file.as_deref() {
            pc.plate.thumbnail = z.read(t).ok();
        }
    }
    project.plates = plates.into_iter().map(|p| p.plate).collect();

    // Objects, in build order; items naming the same object are its instances.
    let mut order: Vec<String> = Vec::new();
    let mut instances: HashMap<String, Vec<Mat4>> = HashMap::new();
    for (id, m) in &main.items {
        if !instances.contains_key(id) {
            order.push(id.clone());
        }
        instances.entry(id.clone()).or_default().push(*m);
    }
    let main_scale = Mat4::from_scale(Vec3::splat(main.scale));
    let mut dropped = 0;
    for id in order {
        let mut defs = Vec::new();
        collect_parts(&files, &start, &id, Mat4::IDENTITY, 0, &mut defs);
        if defs.is_empty() {
            continue;
        }
        let cfg = configs.get(&id);
        let fallback_name = main.objects.get(&id).map(|o| o.name.clone()).unwrap_or_default();
        let mut obj = ProjectObject {
            name: cfg.and_then(|c| c.name.clone()).filter(|n| !n.is_empty()).unwrap_or(fallback_name),
            extruder: cfg.and_then(|c| c.extruder).unwrap_or(1).max(1),
            settings: cfg.map(|c| c.settings.clone()).unwrap_or_default(),
            ..Default::default()
        };
        for (pid, def) in defs {
            let (pname, kind, settings) = cfg.and_then(|c| c.parts.get(&pid)).cloned().unwrap_or_default();
            let name = pname.unwrap_or_else(|| def.name.clone());
            let (mesh, paint) = checked_mesh(def);
            if kind != PartKind::Normal {
                dropped += 1;
            }
            obj.parts.push(Part { name, kind, mesh, paint, settings });
        }
        let ms = instances.remove(&id).unwrap_or_default();
        for (k, m) in ms.into_iter().enumerate() {
            // Build transforms are in the main file's units.
            let m = main_scale * m * main_scale.inverse();
            obj.instances.push(Instance { matrix: m, plate: plate_of.get(&(id.clone(), k)).copied() });
        }
        if obj.name.is_empty() {
            obj.name = format!("Object {}", project.objects.len() + 1);
        }
        project.objects.push(obj);
    }
    if dropped > 0 {
        project.warnings.push(format!(
            "{dropped} modifier, negative or support part(s) were left out; parts come in a later version"
        ));
    }
    project.settings = z.read("Metadata/project_settings.config").ok().and_then(|b| String::from_utf8(b).ok());
    if project.objects.is_empty() && !project.is_project() {
        return Err(ModelError::Format("the 3MF file has no objects".into()));
    }
    Ok(project)
}

// ---- writing ---------------------------------------------------------------------------

fn esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            c if (c as u32) < 0x20 && c != '\n' && c != '\t' => {}
            c => out.push(c),
        }
    }
    out
}

const NS_CORE: &str = "http://schemas.microsoft.com/3dmanufacturing/core/2015/02";
const NS_PROD: &str = "http://schemas.microsoft.com/3dmanufacturing/production/2015/06";
const NS_BBS: &str = "http://schemas.bambulab.com/package/2021";
const REL_MODEL: &str = "http://schemas.microsoft.com/3dmanufacturing/2013/01/3dmodel";
const REL_THUMB: &str = "http://schemas.openxmlformats.org/package/2006/relationships/metadata/thumbnail";

fn uuid(kind: u32, n: usize) -> String {
    format!("{:08x}-{:04x}-4000-8000-{:012x}", n, kind & 0xffff, n)
}

fn object_file(index: usize) -> String {
    format!("3D/Objects/object_{}.model", index + 1)
}

/// Writes `project` as an OrcaSlicer-style 3MF.
pub fn write(project: &Project) -> ModelResult<Vec<u8>> {
    let mut files: Vec<(String, Vec<u8>)> = Vec::new();
    // Part ids run on across objects, each object's id after its parts, as OrcaSlicer numbers.
    let mut ids: Vec<(usize, Vec<usize>)> = Vec::new();
    let mut first = 1usize;
    for o in &project.objects {
        let parts: Vec<usize> = (first..first + o.parts.len()).collect();
        let id = first + o.parts.len();
        ids.push((id, parts));
        first = id + 1;
    }

    // Each object's parts.
    for (k, (o, (_, part_ids))) in project.objects.iter().zip(&ids).enumerate() {
        let mut s = String::new();
        let _ = writeln!(s, r#"<?xml version="1.0" encoding="UTF-8"?>"#);
        let _ = writeln!(
            s,
            r#"<model unit="millimeter" xml:lang="en-US" xmlns="{NS_CORE}" xmlns:BambuStudio="{NS_BBS}" xmlns:p="{NS_PROD}" requiredextensions="p">"#
        );
        let _ = writeln!(s, r#" <metadata name="BambuStudio:3mfVersion">1</metadata>"#);
        let _ = writeln!(s, " <resources>");
        for (part, pid) in o.parts.iter().zip(part_ids) {
            let _ = writeln!(s, r#"  <object id="{pid}" p:UUID="{}" type="model">"#, uuid(1, *pid));
            let _ = writeln!(s, "   <mesh>\n    <vertices>");
            for v in &part.mesh.vertices {
                let _ = writeln!(s, r#"     <vertex x="{}" y="{}" z="{}"/>"#, fmt_f(v.x), fmt_f(v.y), fmt_f(v.z));
            }
            let _ = writeln!(s, "    </vertices>\n    <triangles>");
            let paint = part.paint.attrs();
            for (t, tri) in part.mesh.triangles.iter().enumerate() {
                let _ = write!(s, r#"     <triangle v1="{}" v2="{}" v3="{}""#, tri[0], tri[1], tri[2]);
                for (key, code) in paint.get(&(t as u32)).into_iter().flatten() {
                    let _ = write!(s, r#" {key}="{}""#, esc(code));
                }
                let _ = writeln!(s, "/>");
            }
            let _ = writeln!(s, "    </triangles>\n   </mesh>\n  </object>");
        }
        let _ = writeln!(s, " </resources>\n <build/>\n</model>");
        files.push((object_file(k), s.into_bytes()));
    }

    // The main model: objects made of their parts, and the build.
    let mut s = String::new();
    let _ = writeln!(s, r#"<?xml version="1.0" encoding="UTF-8"?>"#);
    let _ = writeln!(
        s,
        r#"<model unit="millimeter" xml:lang="en-US" xmlns="{NS_CORE}" xmlns:BambuStudio="{NS_BBS}" xmlns:p="{NS_PROD}" requiredextensions="p">"#
    );
    let mut meta = vec![
        ("Application".to_owned(), project.application.clone()),
        ("BambuStudio:3mfVersion".to_owned(), "1".to_owned()),
    ];
    meta.extend(project.metadata.iter().filter(|(k, _)| k != "Application" && k != "BambuStudio:3mfVersion").cloned());
    for (k, v) in &meta {
        let _ = writeln!(s, r#" <metadata name="{}">{}</metadata>"#, esc(k), esc(v));
    }
    let _ = writeln!(s, " <resources>");
    for (k, (oid, part_ids)) in ids.iter().enumerate() {
        let _ = writeln!(s, r#"  <object id="{oid}" p:UUID="{}" type="model">"#, uuid(2, *oid));
        let _ = writeln!(s, "   <components>");
        for pid in part_ids {
            let _ = writeln!(
                s,
                r#"    <component p:path="/{}" objectid="{pid}" p:UUID="{}" transform="1 0 0 0 1 0 0 0 1 0 0 0"/>"#,
                object_file(k),
                uuid(3, *pid)
            );
        }
        let _ = writeln!(s, "   </components>\n  </object>");
    }
    let _ = writeln!(s, " </resources>");
    let _ = writeln!(s, r#" <build p:UUID="{}">"#, uuid(4, 0));
    let mut n = 0;
    for (o, (oid, _)) in project.objects.iter().zip(&ids) {
        for inst in &o.instances {
            n += 1;
            let _ = writeln!(
                s,
                r#"  <item objectid="{oid}" p:UUID="{}" transform="{}" printable="1"/>"#,
                uuid(5, n),
                transform_text(&inst.matrix)
            );
        }
    }
    let _ = writeln!(s, " </build>\n</model>");
    files.push(("3D/3dmodel.model".into(), s.into_bytes()));

    // Relationships and content types.
    let mut rels = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">\n",
    );
    for k in 0..project.objects.len() {
        let _ =
            writeln!(rels, r#" <Relationship Target="/{}" Id="rel-{}" Type="{REL_MODEL}"/>"#, object_file(k), k + 1);
    }
    rels.push_str("</Relationships>\n");
    files.push(("3D/_rels/3dmodel.model.rels".into(), rels.into_bytes()));
    let mut top = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">\n",
    );
    let _ = writeln!(top, r#" <Relationship Target="/3D/3dmodel.model" Id="rel-1" Type="{REL_MODEL}"/>"#);
    if project.plates.first().is_some_and(|p| p.thumbnail.is_some()) {
        let _ = writeln!(top, r#" <Relationship Target="/Metadata/plate_1.png" Id="rel-2" Type="{REL_THUMB}"/>"#);
    }
    top.push_str("</Relationships>\n");
    files.push(("_rels/.rels".into(), top.into_bytes()));
    files.push((
        "[Content_Types].xml".into(),
        concat!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n",
            "<Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\">\n",
            " <Default Extension=\"rels\" ContentType=\"application/vnd.openxmlformats-package.relationships+xml\"/>\n",
            " <Default Extension=\"model\" ContentType=\"application/vnd.ms-package.3dmanufacturing-3dmodel+xml\"/>\n",
            " <Default Extension=\"png\" ContentType=\"image/png\"/>\n",
            " <Default Extension=\"gcode\" ContentType=\"text/x.gcode\"/>\n",
            "</Types>\n"
        )
        .as_bytes()
        .to_vec(),
    ));

    // Names, parts, settings and plates.
    let mut c = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<config>\n");
    let kv = |c: &mut String, indent: &str, k: &str, v: &str| {
        let _ = writeln!(c, r#"{indent}<metadata key="{}" value="{}"/>"#, esc(k), esc(v));
    };
    for (o, (oid, part_ids)) in project.objects.iter().zip(&ids) {
        let _ = writeln!(c, r#"  <object id="{oid}">"#);
        kv(&mut c, "    ", "name", &o.name);
        kv(&mut c, "    ", "extruder", &o.extruder.max(1).to_string());
        for (k, v) in &o.settings {
            kv(&mut c, "    ", k, v);
        }
        for (part, pid) in o.parts.iter().zip(part_ids) {
            let _ = writeln!(c, r#"    <part id="{pid}" subtype="{}">"#, part.kind.orca());
            kv(&mut c, "      ", "name", &part.name);
            kv(&mut c, "      ", "matrix", "1 0 0 0 0 1 0 0 0 0 1 0 0 0 0 1");
            for (k, v) in &part.settings {
                kv(&mut c, "      ", k, v);
            }
            let _ = writeln!(
                c,
                r#"      <mesh_stat face_count="{}" edges_fixed="0" degenerate_facets="0" facets_removed="0" facets_reversed="0" backwards_edges="0"/>"#,
                part.mesh.triangles.len()
            );
            let _ = writeln!(c, "    </part>");
        }
        let _ = writeln!(c, "  </object>");
    }
    let mut identify = 0usize;
    for (p, plate) in project.plates.iter().enumerate() {
        let _ = writeln!(c, "  <plate>");
        kv(&mut c, "    ", "plater_id", &(p + 1).to_string());
        kv(&mut c, "    ", "plater_name", &plate.name);
        kv(&mut c, "    ", "locked", if plate.locked { "true" } else { "false" });
        for (k, v) in &plate.settings {
            kv(&mut c, "    ", k, v);
        }
        if plate.thumbnail.is_some() {
            kv(&mut c, "    ", "thumbnail_file", &format!("Metadata/plate_{}.png", p + 1));
        }
        for (o, (oid, _)) in project.objects.iter().zip(&ids) {
            for (k, inst) in o.instances.iter().enumerate() {
                if inst.plate.unwrap_or(0) != p {
                    continue;
                }
                identify += 1;
                let _ = writeln!(c, "    <model_instance>");
                kv(&mut c, "      ", "object_id", &oid.to_string());
                kv(&mut c, "      ", "instance_id", &k.to_string());
                kv(&mut c, "      ", "identify_id", &identify.to_string());
                let _ = writeln!(c, "    </model_instance>");
            }
        }
        let _ = writeln!(c, "  </plate>");
    }
    c.push_str("</config>\n");
    files.push(("Metadata/model_settings.config".into(), c.into_bytes()));
    if let Some(settings) = &project.settings {
        files.push(("Metadata/project_settings.config".into(), settings.clone().into_bytes()));
    }
    for (p, plate) in project.plates.iter().enumerate() {
        if let Some(png) = &plate.thumbnail {
            files.push((format!("Metadata/plate_{}.png", p + 1), png.clone()));
        }
    }
    zip::write(&files).map_err(|e| ModelError::Format(format!("could not pack the project: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::primitives;

    fn sample() -> Project {
        let cube = primitives::cube(10.0);
        let mut paint = Paint::default();
        paint.supports.push((0, encode_state(1)));
        paint.supports.push((3, encode_state(2)));
        paint.seam.push((5, "4".into()));
        Project {
            application: "Test-1.0".into(),
            metadata: vec![("Title".into(), "A & B".into())],
            objects: vec![
                ProjectObject {
                    name: "Cube <one>".into(),
                    extruder: 2,
                    parts: vec![
                        Part {
                            name: "body".into(),
                            kind: PartKind::Normal,
                            mesh: cube.clone(),
                            paint,
                            settings: vec![],
                        },
                        Part {
                            name: "mod".into(),
                            kind: PartKind::Modifier,
                            mesh: primitives::cube(2.0),
                            paint: Paint::default(),
                            settings: vec![("sparse_infill_density".into(), "50%".into())],
                        },
                    ],
                    instances: vec![
                        Instance { matrix: Mat4::from_translation(Vec3::new(50.0, 60.0, 0.0)), plate: Some(0) },
                        Instance {
                            matrix: Mat4::from_scale_rotation_translation(
                                Vec3::splat(2.0),
                                glam::Quat::from_rotation_z(0.5),
                                Vec3::new(300.0, 60.0, 0.0),
                            ),
                            plate: Some(1),
                        },
                    ],
                    settings: vec![("wall_loops".into(), "4".into())],
                },
                ProjectObject {
                    name: "Ball".into(),
                    extruder: 1,
                    parts: vec![Part { name: "Ball".into(), mesh: primitives::sphere(5.0, 16), ..Default::default() }],
                    instances: vec![Instance {
                        matrix: Mat4::from_translation(Vec3::new(100.0, 100.0, 0.0)),
                        plate: Some(0),
                    }],
                    settings: vec![],
                },
            ],
            plates: vec![
                Plate {
                    name: "first".into(),
                    locked: true,
                    settings: vec![("print_sequence".into(), "by object".into())],
                    thumbnail: Some(vec![137, 80, 78, 71]),
                },
                Plate::default(),
            ],
            settings: Some(r#"{"layer_height": "0.16"}"#.into()),
            warnings: vec![],
        }
    }

    #[test]
    fn a_project_round_trips() -> ModelResult<()> {
        let p = sample();
        let back = read(&write(&p)?)?;
        assert_eq!(back.application, "Test-1.0");
        assert!(back.metadata.iter().any(|(k, v)| k == "Title" && v == "A & B"));
        assert_eq!(back.objects.len(), 2);
        let (a, b) = (&back.objects[0], &p.objects[0]);
        assert_eq!(a.name, "Cube <one>");
        assert_eq!(a.extruder, 2);
        assert_eq!(a.settings, b.settings);
        assert_eq!(a.parts.len(), 2);
        assert_eq!(a.parts[1].kind, PartKind::Modifier);
        assert_eq!(a.parts[1].settings, b.parts[1].settings);
        assert_eq!(a.parts[0].mesh.triangles, b.parts[0].mesh.triangles);
        assert_eq!(a.parts[0].paint, b.parts[0].paint);
        assert_eq!(a.parts[0].paint.support_states(), vec![(0, 1), (3, 2)]);
        assert_eq!(a.instances.len(), 2);
        for (x, y) in a.instances.iter().zip(&b.instances) {
            assert!(x.matrix.abs_diff_eq(y.matrix, 1e-4), "{:?} {:?}", x.matrix, y.matrix);
            assert_eq!(x.plate, y.plate);
        }
        assert_eq!(back.plates.len(), 2);
        assert_eq!(back.plates[0].name, "first");
        assert!(back.plates[0].locked && !back.plates[1].locked);
        assert_eq!(back.plates[0].settings, p.plates[0].settings);
        assert_eq!(back.plates[0].thumbnail.as_deref(), Some(&[137u8, 80, 78, 71][..]));
        assert_eq!(back.settings, p.settings);
        assert!(back.is_project());
        Ok(())
    }

    #[test]
    fn plain_3mf_with_inline_meshes_and_units() -> ModelResult<()> {
        let model = br#"<?xml version="1.0"?>
<model unit="centimeter" xmlns="http://schemas.microsoft.com/3dmanufacturing/core/2015/02">
 <resources>
  <object id="1" type="model" name="tri"><mesh><vertices>
   <vertex x="0" y="0" z="0"/><vertex x="1" y="0" z="0"/><vertex x="0" y="1" z="0"/><vertex x="0" y="0" z="1"/>
  </vertices><triangles>
   <triangle v1="0" v2="2" v3="1"/><triangle v1="0" v2="1" v3="3"/><triangle v1="0" v2="3" v3="2"/><triangle v1="1" v2="2" v3="3"/><triangle v1="0" v2="1" v3="99"/>
  </triangles></mesh></object>
  <object id="2" type="model"><components><component objectid="1" transform="1 0 0 0 1 0 0 0 1 2 0 0"/></components></object>
 </resources>
 <build><item objectid="2" transform="1 0 0 0 1 0 0 0 1 0 0 1"/></build>
</model>"#;
        let z = zip::write(&[("3D/3dmodel.model".into(), model.to_vec())]).map_err(import_error)?;
        let p = read(&z)?;
        assert!(!p.is_project());
        assert_eq!(p.objects.len(), 1);
        let o = &p.objects[0];
        let (mesh, _) = o.merged();
        // The bad triangle is dropped; centimetres become millimetres.
        assert_eq!(mesh.triangles.len(), 4);
        let world: Vec<Vec3> = mesh.vertices.iter().map(|v| o.instances[0].matrix.transform_point3(*v)).collect();
        assert!(world.iter().any(|v| v.abs_diff_eq(Vec3::new(30.0, 0.0, 10.0), 1e-4)), "{world:?}");
        assert!(world.iter().any(|v| v.abs_diff_eq(Vec3::new(20.0, 0.0, 10.0), 1e-4)));
        Ok(())
    }

    #[test]
    fn split_triangles_decode_to_the_larger_share() {
        assert_eq!(decode_state("4"), Some(1));
        assert_eq!(decode_state("8"), Some(2));
        assert_eq!(decode_state(""), None);
        assert_eq!(decode_state("0"), None);
        // Split in two (code 1): read from the end, children enforce, enforce... majority.
        assert_eq!(decode_state("4441"), Some(1));
        assert_eq!(decode_state("8881"), Some(2));
        assert_eq!(encode_state(1), "4");
        assert_eq!(encode_state(2), "8");
    }

    #[test]
    fn damaged_files_are_errors() -> ModelResult<()> {
        let good = write(&sample())?;
        for cut in (0..good.len()).step_by(97) {
            let _ = read(&good[..cut]);
        }
        let mut flipped = good.clone();
        for i in (0..flipped.len()).step_by(53) {
            if let Some(b) = flipped.get_mut(i) {
                *b ^= 0x5a;
            }
        }
        let _ = read(&flipped);
        assert!(read(b"not a zip").is_err());
        Ok(())
    }
}
