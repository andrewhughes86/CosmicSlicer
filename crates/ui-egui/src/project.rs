//! Project files: the session saved as a 3MF in OrcaSlicer's layout, and OrcaSlicer, Bambu
//! Studio and plain 3MF files opened as projects.
//!
//! Presets travel as OrcaSlicer's `project_settings.config`: every value of the selected
//! printer, process and filaments (filament values as one list entry per filament), with the
//! preset names. Opening a project selects those presets by name — installed ones first, then
//! the library's — and keeps each value the project changed as an unsaved edit; a preset found
//! nowhere becomes a project preset holding the project's values.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use CosmicSlicer_config::{Config, Preset, PresetBundle, Schema, Slot, TabKind, userfiles};
use CosmicSlicer_model::threemf::{self, Instance, Part, PartKind, Plate, Project, ProjectObject};
use CosmicSlicer_model::{HeightRange, Mesh, Model, Object, Transform, Volume};
use glam::{Mat4, Vec3};
use serde_json::{Map, Value};

use crate::recovery::Restored;
use crate::state::{BED_TYPES, EventKind, LayerEventSetting, PlateSettings, ProjectInfo, Session, plate_columns};

/// OrcaSlicer's names for the plate types of [`BED_TYPES`], in the same order.
const ORCA_BEDS: [&str; 6] = [
    "Supertack Plate",
    "Cool Plate",
    "Engineering Plate",
    "High Temp Plate",
    "Textured PEI Plate",
    "Textured Cool Plate",
];

/// The program recorded as the project's author.
const APPLICATION: &str = concat!("CosmicSlicer-", env!("CARGO_PKG_VERSION"));

/// Size of the plate thumbnails, as OrcaSlicer saves them.
const THUMBNAIL: u32 = 512;

/// Most plates a project may claim.
pub const MAX_PLATES: usize = 100;

/// Every option key and the tab it is on.
fn tab_table(schema: &Schema) -> HashMap<&str, TabKind> {
    let mut out = HashMap::new();
    for tab in [TabKind::Printer, TabKind::Filament, TabKind::Process] {
        for page in schema.pages(tab) {
            for group in &page.groups {
                for entry in &group.options {
                    for key in entry.keys() {
                        out.entry(key).or_insert(tab);
                    }
                }
            }
        }
    }
    out
}

/// The system preset a preset's values are measured against.
fn system_base<'a>(bundle: &'a PresetBundle, kind: TabKind, p: &'a Preset) -> &'a Config {
    if p.system || p.inherits.is_empty() {
        return &p.config;
    }
    bundle.list(kind).iter().find(|q| q.system && q.name == p.inherits).map_or(&p.config, |q| &q.config)
}

/// The presets as OrcaSlicer's `project_settings.config`.
pub fn settings_json(session: &Session) -> String {
    let schema = session.schema;
    let b = &session.presets;
    let mut m = Map::new();
    let filaments = b.project_filaments.len().max(1);
    let mut inherits = Vec::new();
    let mut different = Vec::new();
    let mut note = |kind: TabKind, i: usize| {
        let Some(p) = b.preset(kind, i) else {
            inherits.push(Value::String(String::new()));
            different.push(Value::String(String::new()));
            return;
        };
        let eff = b.effective(kind, i);
        let base = system_base(b, kind, p);
        let mut keys: Vec<&str> =
            eff.values.iter().filter(|(k, v)| base.values.get(*k) != Some(*v)).map(|(k, _)| k.as_str()).collect();
        keys.sort_unstable();
        inherits.push(Value::String(if p.system { String::new() } else { p.inherits.clone() }));
        different.push(Value::String(keys.join(";")));
    };
    note(TabKind::Process, 0);
    for i in 0..filaments {
        note(TabKind::Filament, i);
    }
    note(TabKind::Printer, 0);
    for kind in [TabKind::Printer, TabKind::Process] {
        for (k, v) in &b.effective(kind, 0).values {
            m.insert(k.clone(), userfiles::to_json_value(schema, k, v));
        }
    }
    let fils: Vec<Config> = (0..filaments).map(|i| b.effective(TabKind::Filament, i)).collect();
    let mut keys: Vec<&String> = fils.iter().flat_map(|f| f.values.keys()).collect();
    keys.sort();
    keys.dedup();
    for k in keys {
        let list = fils.iter().map(|f| Value::String(f.get(schema, k).to_owned())).collect();
        m.insert(k.clone(), Value::Array(list));
    }
    let name = |kind: TabKind, i: usize| b.preset(kind, i).map_or_else(String::new, |p| p.name.clone());
    m.insert("print_settings_id".into(), Value::String(name(TabKind::Process, 0)));
    m.insert("printer_settings_id".into(), Value::String(name(TabKind::Printer, 0)));
    m.insert(
        "filament_settings_id".into(),
        Value::Array((0..filaments).map(|i| Value::String(name(TabKind::Filament, i))).collect()),
    );
    m.insert("inherits_group".into(), Value::Array(inherits));
    m.insert("different_settings_to_system".into(), Value::Array(different));
    m.insert(
        "curr_bed_type".into(),
        Value::String(ORCA_BEDS.get(session.bed_type).copied().unwrap_or("Textured PEI Plate").into()),
    );
    m.insert("name".into(), Value::String("project_settings".into()));
    m.insert("from".into(), Value::String("project".into()));
    m.insert("version".into(), Value::String("2.3.0.0".into()));
    serde_json::to_string_pretty(&Value::Object(m)).unwrap_or_default()
}

/// The plate type named `name` (OrcaSlicer's name or ours).
fn bed_index(name: &str) -> Option<usize> {
    let name = name.trim();
    ORCA_BEDS
        .iter()
        .position(|b| b.eq_ignore_ascii_case(name))
        .or_else(|| BED_TYPES.iter().position(|(label, _)| label.eq_ignore_ascii_case(name)))
        .or_else(|| BED_TYPES.iter().position(|(_, key)| key.trim_end_matches("_temp").eq_ignore_ascii_case(name)))
}

/// Selects the preset called `name` of `kind`, adding it from the library, or as a project
/// preset of `values` when it is found nowhere; returns its index.
fn find_or_add(
    bundle: &mut PresetBundle,
    kind: TabKind,
    name: &str,
    values: &Config,
    warnings: &mut Vec<String>,
) -> usize {
    let list = bundle.list(kind);
    if let Some(i) = list.iter().position(|p| p.name == name) {
        return i;
    }
    let added = match (!name.is_empty()).then(|| userfiles::find_parent(kind, name)).flatten() {
        Some(sp) => Preset {
            name: sp.name,
            system: true,
            vendor: sp.vendor,
            config: sp.config,
            inherits: String::new(),
            compatible_printers: sp.compatible_printers,
            condition: sp.condition,
        },
        None => {
            let shown =
                if name.is_empty() { format!("Project {}", kind.title().to_lowercase()) } else { name.to_owned() };
            warnings.push(format!("\"{shown}\" is not installed; the project's own values are used"));
            Preset {
                name: shown,
                system: false,
                vendor: "Project presets".into(),
                config: values.clone(),
                inherits: String::new(),
                compatible_printers: Vec::new(),
                condition: String::new(),
            }
        }
    };
    bundle.add_user(kind, added)
}

/// What `values` change from preset `index`.
fn edits(schema: &Schema, bundle: &PresetBundle, kind: TabKind, index: usize, values: &Config) -> Config {
    let mut out = Config::default();
    if let Some(p) = bundle.list(kind).get(index) {
        for (k, v) in &values.values {
            if p.config.get(schema, k) != v {
                out.set(k.clone(), v.clone());
            }
        }
    }
    out
}

/// Puts a project's presets into `bundle`; returns the plate type it names.
fn apply_settings(schema: &Schema, bundle: &mut PresetBundle, json: &str, warnings: &mut Vec<String>) -> Option<usize> {
    let v: Value = match serde_json::from_str(json.trim_start_matches('\u{feff}')) {
        Ok(v) => v,
        Err(e) => {
            warnings.push(format!("the project's settings could not be read ({e}); the current presets are kept"));
            return None;
        }
    };
    let m = v.as_object()?;
    let text = |k: &str| m.get(k).map(userfiles::from_json_value).unwrap_or_default();
    let names: Vec<String> = match m.get("filament_settings_id") {
        Some(Value::Array(a)) => a.iter().map(userfiles::from_json_value).collect(),
        Some(other) => vec![userfiles::from_json_value(other)],
        None => Vec::new(),
    };
    let filaments = names.len().clamp(1, CosmicSlicer_config::presets::MAX_FILAMENTS);
    let tabs = tab_table(schema);
    let (mut printer, mut process) = (Config::default(), Config::default());
    let mut fils = vec![Config::default(); filaments];
    for (k, value) in m {
        // A few options live outside the settings tabs (the sidebar's filament colour).
        let tab = tabs
            .get(k.as_str())
            .copied()
            .or_else(|| (k.starts_with("filament_") && schema.option(k).is_some()).then_some(TabKind::Filament));
        match tab.as_ref() {
            Some(TabKind::Printer) => printer.set(k.clone(), userfiles::from_json_value(value)),
            Some(TabKind::Process) => process.set(k.clone(), userfiles::from_json_value(value)),
            Some(TabKind::Filament) => {
                for (i, f) in fils.iter_mut().enumerate() {
                    let item = match value {
                        Value::Array(a) => a.get(i).or_else(|| a.first()).map(userfiles::from_json_value),
                        other => Some(userfiles::from_json_value(other)),
                    };
                    if let Some(item) = item {
                        f.set(k.clone(), item);
                    }
                }
            }
            None => {}
        }
    }
    let p = find_or_add(bundle, TabKind::Printer, &text("printer_settings_id"), &printer, warnings);
    bundle.select(TabKind::Printer, 0, p);
    bundle.printer.edits = edits(schema, bundle, TabKind::Printer, p, &printer);
    let q = find_or_add(bundle, TabKind::Process, &text("print_settings_id"), &process, warnings);
    bundle.process = Slot { preset: q, edits: edits(schema, bundle, TabKind::Process, q, &process) };
    let mut slots = Vec::new();
    for (i, values) in fils.iter().enumerate() {
        let name = names.get(i).cloned().unwrap_or_default();
        let f = find_or_add(bundle, TabKind::Filament, &name, values, warnings);
        slots.push(Slot { preset: f, edits: edits(schema, bundle, TabKind::Filament, f, values) });
    }
    bundle.project_filaments = slots;
    bed_index(&text("curr_bed_type"))
}

fn plate_origin(p: usize, plates: usize, size: glam::Vec2) -> Vec3 {
    let cols = plate_columns(plates);
    Vec3::new((p % cols) as f32 * size.x * 1.2, -((p / cols) as f32) * size.y * 1.2, 0.0)
}

/// Plate `p` drawn `size` pixels square (RGBA), as OrcaSlicer's plate thumbnails; `None`
/// for an empty plate.
pub fn plate_image(session: &Session, p: usize, size: u32) -> Option<Vec<u8>> {
    let colours: Vec<[u8; 3]> = (0..session.presets.project_filaments.len())
        .map(|i| hex_colour(&session.presets.filament_colour(session.schema, i)))
        .collect();
    let items: Vec<CosmicSlicer_engine::thumbnail::Item<'_>> = session
        .model
        .objects
        .iter()
        .filter(|o| o.plate == p && o.visible)
        .flat_map(|o| {
            let colour = colours.get(o.filament.saturating_sub(1)).copied().unwrap_or([0xF2, 0x75, 0x4E]);
            o.solids().into_iter().map(move |(mesh, matrix)| CosmicSlicer_engine::thumbnail::Item {
                mesh,
                matrix,
                colour,
            })
        })
        .collect();
    (!items.is_empty()).then(|| CosmicSlicer_engine::thumbnail::render(&items, size, size))
}

/// The session as a project.
pub fn to_project(session: &Session) -> Project {
    let mut objects: Vec<ProjectObject> = Vec::new();
    // Instances of one object are one object with several placements, as in OrcaSlicer.
    let mut groups: std::collections::HashMap<u64, (usize, &Arc<Mesh>)> = std::collections::HashMap::new();
    for o in session.model.objects.iter().filter(|o| o.visible) {
        if o.shared != 0
            && let Some(&(k, mesh)) = groups.get(&o.shared)
            && Arc::ptr_eq(mesh, &o.mesh)
            && let Some(first) = objects.get_mut(k)
        {
            first.instances.push(Instance { matrix: o.matrix(), plate: Some(o.plate) });
            continue;
        }
        if o.shared != 0 {
            groups.insert(o.shared, (objects.len(), &o.mesh));
        }
        let paint = o.paint.as_deref().cloned().unwrap_or_default();
        let mut parts = vec![Part {
            name: o.name.clone(),
            kind: PartKind::Normal,
            mesh: (*o.mesh).clone(),
            paint,
            settings: Vec::new(),
        }];
        // The other volumes with their placement in the object baked in.
        for v in &o.parts {
            parts.push(Part {
                name: v.name.clone(),
                kind: v.kind,
                mesh: transformed(&v.mesh, &v.matrix()),
                paint: Default::default(),
                settings: v.overrides.clone(),
            });
        }
        objects.push(ProjectObject {
            name: o.name.clone(),
            extruder: o.filament.max(1),
            parts,
            instances: vec![Instance { matrix: o.matrix(), plate: Some(o.plate) }],
            settings: o.overrides.clone(),
            ranges: o.ranges.iter().map(|r| (r.lo, r.hi, r.overrides.clone())).collect(),
            brim_ears: o.brim_ears.iter().map(|e| (e.at, e.radius)).collect(),
            layer_profile: o.layer_profile.clone(),
        });
    }
    let plates = (0..session.model.plates.max(1))
        .map(|p| {
            let ps = session.plate_settings.get(p).cloned().unwrap_or_default();
            let mut settings = Vec::new();
            if let Some(b) = ps.bed_type.and_then(|b| ORCA_BEDS.get(b)) {
                settings.push(("curr_bed_type".to_owned(), (*b).to_owned()));
            }
            match ps.sequence {
                1 => settings.push(("print_sequence".into(), "by layer".into())),
                2 => settings.push(("print_sequence".into(), "by object".into())),
                _ => {}
            }
            match ps.spiral {
                1 => settings.push(("spiral_mode".into(), "true".into())),
                2 => settings.push(("spiral_mode".into(), "false".into())),
                _ => {}
            }
            let thumbnail = plate_image(session, p, THUMBNAIL)
                .and_then(|rgba| CosmicSlicer_engine::thumbnail::png(&rgba, THUMBNAIL, THUMBNAIL));
            let events = ps
                .events
                .iter()
                .map(|e| {
                    let kind = match e.kind {
                        EventKind::ColorChange => 0,
                        EventKind::Pause => 1,
                        EventKind::Custom => 2,
                    };
                    let gcode = match e.kind {
                        EventKind::ColorChange => "M600".to_owned(),
                        EventKind::Pause => "M601".to_owned(),
                        EventKind::Custom => e.gcode.clone(),
                    };
                    (e.z, kind, gcode)
                })
                .collect();
            Plate {
                name: ps.name,
                locked: session.locked.get(p).copied().unwrap_or(false),
                settings,
                thumbnail,
                events,
            }
        })
        .collect();
    let info = &session.project;
    let mut metadata = vec![
        ("Title".to_owned(), info.name.clone()),
        ("Designer".to_owned(), info.author.clone()),
        ("Description".to_owned(), info.description.clone()),
        ("License".to_owned(), info.license.clone()),
    ];
    metadata.retain(|(_, v)| !v.is_empty());
    Project {
        application: APPLICATION.into(),
        metadata,
        objects,
        plates,
        settings: Some(settings_json(session)),
        warnings: Vec::new(),
    }
}

fn hex_colour(hex: &str) -> [u8; 3] {
    let v =
        hex.trim().trim_start_matches('#').get(..6).and_then(|h| u32::from_str_radix(h, 16).ok()).unwrap_or(0xF2_75_4E);
    [(v >> 16) as u8, (v >> 8) as u8, v as u8]
}

/// Saves the session as a 3MF project at `path`.
pub fn save(session: &Session, path: &Path) -> Result<(), String> {
    let bytes = threemf::write(&to_project(session)).map_err(|e| e.to_string())?;
    crate::store::write_atomic(path, &bytes).map_err(|e| format!("could not save {}: {e}", path.display()))
}

/// One placed copy of a mesh as an object: the mesh centred as [`Model::add`] keeps it, the
/// placement split into scale, rotation and offset (or baked into the mesh when it shears).
/// `mesh` with `m` applied to it (turned inside out back when `m` mirrors).
fn transformed(mesh: &Mesh, m: &Mat4) -> Mesh {
    let mut out = mesh.clone();
    for v in &mut out.vertices {
        *v = m.transform_point3(*v);
    }
    if m.determinant() < 0.0 {
        for tri in &mut out.triangles {
            tri.swap(1, 2);
        }
    }
    out
}

/// As [`placed`] returns it: the object's mesh, its placement, and the map from the file's
/// object frame into the mesh's (for the object's other parts).
type Placed = (Mesh, Transform, Mat4);

fn placed(name: &str, mesh: &Mesh, matrix: Mat4) -> Option<Placed> {
    let (scale, rot, offset) = matrix.to_scale_rotation_translation();
    let rebuilt = Mat4::from_scale_rotation_translation(scale, rot, offset);
    let tolerance = 1e-3 * scale.abs().max_element().max(1.0);
    let (mut local, mut t, baked) =
        if rebuilt.abs_diff_eq(matrix, tolerance) && scale.is_finite() && scale.min_element() > 0.0 {
            let mut t = Transform { offset, scale, ..Default::default() };
            t.set_quat(rot);
            (mesh.clone(), t, Mat4::IDENTITY)
        } else {
            // Shear or mirroring: bake the shape, keep the position.
            let linear = Mat4::from_cols(matrix.x_axis, matrix.y_axis, matrix.z_axis, glam::Vec4::W);
            (transformed(mesh, &linear), Transform { offset, ..Default::default() }, linear)
        };
    let b = local.bounds();
    if !b.is_valid() || !t.offset.is_finite() {
        log::warn!("{name}: placement is not finite; skipped");
        return None;
    }
    let shift = Vec3::new(-b.centre().x, -b.centre().y, -b.min.z);
    for v in &mut local.vertices {
        *v += shift;
    }
    let linear = Mat4::from_scale_rotation_translation(t.scale, t.quat(), Vec3::ZERO);
    t.offset -= linear.transform_vector3(shift);
    Some((local, t, Mat4::from_translation(shift) * baked))
}

/// Reads the project at `path` into what the session takes; `session` gives the presets to
/// choose from. Returns warnings for what could not be kept.
pub fn open(session: &Session, path: &Path) -> Result<(Restored, Vec<String>), String> {
    let len = std::fs::metadata(path).map_err(|e| format!("could not read {}: {e}", path.display()))?.len();
    if len > CosmicSlicer_model::MAX_FILE_BYTES {
        return Err(format!("{} is larger than 2 GB", path.display()));
    }
    let bytes = std::fs::read(path).map_err(|e| format!("could not read {}: {e}", path.display()))?;
    let project = threemf::read(&bytes).map_err(|e| e.to_string())?;
    let mut warnings = project.warnings.clone();
    let schema = session.schema;
    let mut presets = session.presets.clone();
    let mut bed_type = session.bed_type;
    if let Some(json) = &project.settings
        && let Some(b) = apply_settings(schema, &mut presets, json, &mut warnings)
    {
        bed_type = b;
    }
    let area = CosmicSlicer_config::parse_points(&presets.value(schema, TabKind::Printer, 0, "printable_area"));
    let (mut lo, mut hi) = (glam::Vec2::splat(f32::INFINITY), glam::Vec2::splat(f32::NEG_INFINITY));
    for [x, y] in &area {
        lo = lo.min(glam::Vec2::new(*x as f32, *y as f32));
        hi = hi.max(glam::Vec2::new(*x as f32, *y as f32));
    }
    let size = if lo.is_finite() && (hi - lo).min_element() >= 1.0 { hi - lo } else { glam::Vec2::splat(220.0) };

    let claimed =
        project.objects.iter().flat_map(|o| o.instances.iter().filter_map(|i| i.plate)).max().map_or(0, |p| p + 1);
    let plates = project.plates.len().max(claimed).clamp(1, MAX_PLATES);
    let mut model = Model::new();
    model.plates = plates;
    let filaments = presets.project_filaments.len().max(1);
    let keep = |list: &[(String, String)]| -> Vec<(String, String)> {
        list.iter().filter(|(k, _)| schema.option(k).is_some()).cloned().collect()
    };
    for o in &project.objects {
        // The first part that prints is the object's mesh; the rest are its volumes.
        let Some(main) = o.parts.iter().position(|p| p.kind == PartKind::Normal) else { continue };
        let Some(first) = o.parts.get(main) else { continue };
        if first.mesh.triangle_count() == 0 {
            continue;
        }
        let paint = first.paint.clone();
        let ranges: Vec<HeightRange> = o
            .ranges
            .iter()
            .map(|(lo, hi, settings)| HeightRange { lo: *lo, hi: *hi, overrides: keep(settings) })
            .collect();
        // The first placement's mesh, shared by the others that need no baking.
        let mut shared: Option<(Arc<Mesh>, Mat4, u64)> = None;
        for inst in &o.instances {
            let Some((local, transform, frame)) = placed(&o.name, &first.mesh, inst.matrix) else { continue };
            let reuse =
                shared.as_ref().filter(|(_, f, _)| f.abs_diff_eq(frame, 1e-5)).map(|(m, _, g)| (Arc::clone(m), *g));
            let mesh = reuse.as_ref().map_or_else(|| Arc::new(local), |(m, _)| Arc::clone(m));
            let parts: Vec<Volume> = o
                .parts
                .iter()
                .enumerate()
                .filter(|(k, p)| *k != main && p.mesh.triangle_count() > 0)
                .map(|(_, p)| Volume {
                    name: p.name.clone(),
                    kind: p.kind,
                    mesh: Arc::new(transformed(&p.mesh, &frame)),
                    transform: Transform::default(),
                    overrides: keep(&p.settings),
                })
                .collect();
            // Files without plates: the plate the object stands on.
            let plate = inst.plate.unwrap_or_else(|| {
                let p = transform.offset.truncate() - lo.min(glam::Vec2::ZERO);
                (0..plates)
                    .find(|&k| {
                        let origin = plate_origin(k, plates, size).truncate();
                        let rel = p - origin;
                        rel.x >= -1.0 && rel.y >= -1.0 && rel.x <= size.x + 1.0 && rel.y <= size.y + 1.0
                    })
                    .unwrap_or(0)
            });
            let id = model.insert(Object {
                id: 0,
                name: o.name.clone(),
                mesh: Arc::clone(&mesh),
                transform,
                filament: o.extruder.clamp(1, filaments),
                plate: plate.min(plates - 1),
                visible: true,
                overrides: keep(&o.settings),
                paint: (!paint.is_empty()).then(|| Arc::new(paint.clone())),
                parts,
                ranges: ranges.clone(),
                // Ears are kept in the frame the main mesh had in the file.
                brim_ears: o
                    .brim_ears
                    .iter()
                    .map(|(p, r)| CosmicSlicer_model::BrimEar { at: frame.transform_point3(*p), radius: *r })
                    .collect(),
                layer_profile: o.layer_profile.clone(),
                shared: 0,
            });
            if o.instances.len() > 1 {
                let group = reuse.map_or(id, |(_, g)| g);
                if let Some(obj) = model.get_mut(id) {
                    obj.shared = group;
                }
                if shared.is_none() {
                    shared = Some((mesh, frame, group));
                }
            }
        }
    }
    let plate_settings = (0..plates)
        .map(|p| {
            let Some(pl) = project.plates.get(p) else { return PlateSettings::default() };
            let get = |k: &str| pl.settings.iter().find(|(a, _)| a == k).map(|(_, v)| v.trim().to_owned());
            PlateSettings {
                events: pl
                    .events
                    .iter()
                    .filter_map(|(z, kind, gcode)| {
                        let kind = match kind {
                            0 => EventKind::ColorChange,
                            1 => EventKind::Pause,
                            2 => EventKind::Custom,
                            _ => return None,
                        };
                        // Only custom G-code keeps its text; the others are the printer's.
                        let gcode = if kind == EventKind::Custom { gcode.clone() } else { String::new() };
                        Some(LayerEventSetting { z: *z, kind, gcode })
                    })
                    .collect(),
                name: pl.name.clone(),
                bed_type: get("curr_bed_type").or_else(|| get("bed_type")).and_then(|b| bed_index(&b)),
                sequence: match get("print_sequence").as_deref() {
                    Some("by layer") => 1,
                    Some("by object") => 2,
                    _ => 0,
                },
                spiral: match get("spiral_mode").as_deref() {
                    Some("true" | "1") => 1,
                    Some("false" | "0") => 2,
                    _ => 0,
                },
            }
        })
        .collect();
    let locked = (0..plates).map(|p| project.plates.get(p).is_some_and(|pl| pl.locked)).collect();
    let meta = |k: &str| project.metadata.iter().find(|(a, _)| a == k).map(|(_, v)| v.clone()).unwrap_or_default();
    let name = path.file_stem().and_then(|n| n.to_str()).unwrap_or("Project").to_owned();
    let restored = Restored {
        project: ProjectInfo {
            name,
            path: Some(path.to_owned()),
            author: meta("Designer"),
            license: meta("License"),
            description: meta("Description"),
        },
        model,
        locked,
        plate_settings,
        bed_type,
        plate: 0,
        presets,
    };
    Ok((restored, warnings))
}

/// Whether the 3MF at `path` is a slicer project (plates or settings), not just a model.
pub fn is_project_file(path: &Path) -> bool {
    let Ok(bytes) = std::fs::read(path) else { return false };
    let Ok(z) = CosmicSlicer_import::zip::Zip::open(&bytes) else { return false };
    z.names().any(|n| {
        let n = n.trim_start_matches('/');
        n.eq_ignore_ascii_case("Metadata/project_settings.config")
            || n.eq_ignore_ascii_case("Metadata/model_settings.config")
    })
}

/// The first plate's thumbnail of a project file, for the Home page.
pub fn thumbnail(path: &Path) -> Option<Vec<u8>> {
    let len = std::fs::metadata(path).ok()?.len();
    if len > CosmicSlicer_model::MAX_FILE_BYTES {
        return None;
    }
    let bytes = std::fs::read(path).ok()?;
    let z = CosmicSlicer_import::zip::Zip::open(&bytes).ok()?;
    z.read("Metadata/plate_1.png").ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("cosmicslicer-project-{name}-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&d);
        d
    }

    #[test]
    fn a_session_round_trips_through_a_project() -> Result<(), String> {
        let mut s = Session::new();
        s.add_primitive("cube");
        s.add_primitive("cylinder");
        s.model.plates = 2;
        s.locked = vec![false, true];
        let ids: Vec<u64> = s.model.objects.iter().map(|o| o.id).collect();
        let second = s.plate_origin(1);
        if let Some(o) = ids.get(1).and_then(|id| s.model.get_mut(*id)) {
            o.transform.rotation = Vec3::new(0.0, 0.0, 30.0);
            o.transform.scale = Vec3::new(1.5, 1.0, 2.0);
            o.overrides.push(("wall_loops".into(), "5".into()));
            o.plate = 1;
            o.transform.offset += second;
            let mut paint = CosmicSlicer_model::Paint::default();
            paint.supports.push((3, "4".into()));
            o.paint = Some(Arc::new(paint));
        }
        let first = ids.first().copied().unwrap_or(0);
        s.add_volume(first, CosmicSlicer_model::PartKind::Modifier, "Cube");
        s.add_volume(first, CosmicSlicer_model::PartKind::Normal, "Sphere");
        if let Some(v) = s.model.get_mut(first).and_then(|o| o.parts.get_mut(0)) {
            v.overrides.push(("sparse_infill_density".into(), "55%".into()));
        }
        s.add_range(first);
        s.plate_settings_mut(1).name = "second".into();
        s.add_event(0, 2.4, EventKind::ColorChange, String::new());
        s.add_event(0, 4.0, EventKind::Custom, "; hi".into());
        s.plate_settings_mut(1).sequence = 2;
        s.plate_settings_mut(0).bed_type = Some(1);
        s.project.author = "Someone".into();
        s.bed_type = 2;
        let schema = s.schema;
        s.presets.set_value(schema, TabKind::Process, 0, "layer_height", "0.12".into());
        s.presets.set_value(schema, TabKind::Filament, 0, "filament_colour", "#123456".into());
        let path = dir("round").join("p.3mf");
        save(&s, &path)?;
        assert!(thumbnail(&path).is_some_and(|png| png.starts_with(&[137, 80, 78, 71])));
        let (r, warnings) = open(&Session::new(), &path)?;
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(r.model.objects.len(), 2);
        assert_eq!(r.model.plates, 2);
        assert_eq!(r.locked, vec![false, true]);
        assert_eq!(r.plate_settings[1].name, "second");
        assert_eq!(r.plate_settings[0].events, s.plate_settings[0].events);
        assert_eq!(r.plate_settings[1].sequence, 2);
        assert_eq!(r.plate_settings[0].bed_type, Some(1));
        assert_eq!(r.bed_type, 2);
        assert_eq!(r.project.author, "Someone");
        for (a, b) in r.model.objects.iter().zip(&s.model.objects) {
            assert_eq!(a.plate, b.plate);
            let (ba, bb) = (a.bounds(), b.bounds());
            assert!(ba.min.abs_diff_eq(bb.min, 1e-3) && ba.max.abs_diff_eq(bb.max, 1e-3), "{ba:?} {bb:?}");
            assert_eq!(a.overrides, b.overrides);
            assert_eq!(a.paint, b.paint);
            assert_eq!(a.ranges, b.ranges);
            assert_eq!(a.parts.len(), b.parts.len());
            for (p, q) in a.parts.iter().zip(&b.parts) {
                assert_eq!((p.kind, &p.overrides), (q.kind, &q.overrides));
                let (pb, qb) = (p.mesh.bounds_transformed(&p.matrix()), q.mesh.bounds_transformed(&q.matrix()));
                assert!(pb.min.abs_diff_eq(qb.min, 1e-3) && pb.max.abs_diff_eq(qb.max, 1e-3), "{pb:?} {qb:?}");
            }
        }
        assert_eq!(r.presets.value(schema, TabKind::Process, 0, "layer_height"), "0.12");
        assert_eq!(r.presets.value(schema, TabKind::Filament, 0, "filament_colour"), "#123456");
        assert_eq!(r.presets.printer.preset, s.presets.printer.preset);
        let _ = std::fs::remove_dir_all(path.parent().unwrap_or(&path));
        Ok(())
    }

    #[test]
    fn an_orca_style_project_opens() -> Result<(), String> {
        // As OrcaSlicer writes them: settings naming presets we may not have, two plates.
        let s = Session::new();
        let mut p = to_project(&s);
        let mesh = CosmicSlicer_model::primitives::cube(20.0);
        p.objects = vec![ProjectObject {
            name: "Box".into(),
            extruder: 1,
            parts: vec![Part { name: "Box".into(), mesh, ..Default::default() }],
            instances: vec![
                Instance { matrix: Mat4::from_translation(Vec3::new(100.0, 100.0, 0.0)), plate: Some(0) },
                Instance { matrix: Mat4::from_translation(Vec3::new(400.0, 100.0, 0.0)), plate: Some(1) },
            ],
            settings: vec![("sparse_infill_density".into(), "35%".into()), ("not_an_option".into(), "x".into())],
            ranges: vec![],
            brim_ears: vec![],
            layer_profile: vec![],
        }];
        p.plates = vec![Plate::default(), Plate { name: "two".into(), ..Default::default() }];
        p.settings = Some(
            r#"{"printer_settings_id": "Some Printer 0.4 nozzle", "print_settings_id": "0.16mm Optimal @Somewhere",
                "filament_settings_id": ["Brand PLA", "Brand PETG"], "layer_height": "0.16",
                "filament_type": ["PLA", "PETG"], "nozzle_diameter": ["0.4"], "curr_bed_type": "Cool Plate",
                "printable_area": ["0x0", "250x0", "250x210", "0x210"]}"#
                .into(),
        );
        let path = dir("orca").join("o.3mf");
        let bytes = threemf::write(&p).map_err(|e| e.to_string())?;
        crate::store::write_atomic(&path, &bytes).map_err(|e| e.to_string())?;
        let (r, warnings) = open(&s, &path)?;
        assert_eq!(r.model.objects.len(), 2);
        assert_eq!(r.model.objects.iter().map(|o| o.plate).collect::<Vec<_>>(), [0, 1]);
        assert_eq!(r.model.objects[0].overrides, vec![("sparse_infill_density".to_owned(), "35%".to_owned())]);
        // One object placed twice opens as two instances sharing one mesh.
        let (a, b) = (&r.model.objects[0], &r.model.objects[1]);
        assert!(a.shared != 0 && a.shared == b.shared && Arc::ptr_eq(&a.mesh, &b.mesh));
        assert_eq!(r.bed_type, 1);
        let schema = s.schema;
        assert_eq!(r.presets.project_filaments.len(), 2);
        assert_eq!(r.presets.value(schema, TabKind::Process, 0, "layer_height"), "0.16");
        assert_eq!(r.presets.value(schema, TabKind::Filament, 1, "filament_type"), "PETG");
        assert_eq!(r.presets.value(schema, TabKind::Printer, 0, "printable_area"), "0x0,250x0,250x210,0x210");
        assert!(warnings.iter().any(|w| w.contains("Some Printer")), "{warnings:?}");
        let _ = std::fs::remove_dir_all(path.parent().unwrap_or(&path));
        Ok(())
    }
}
