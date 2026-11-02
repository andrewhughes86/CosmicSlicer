//! The open project and everything the UI edits: presets, objects, plates, selection and
//! the undo history. Panes read it and change it only through its methods, so every change
//! is undoable and the views stay in step.

use std::path::{Path, PathBuf};

use CosmicSlicer_config::{PresetBundle, Schema, TabKind, parse_points};
use CosmicSlicer_model::{Aabb, Model, Transform, primitives};
use glam::{Vec2, Vec3};

use crate::theme::Theme;

/// The pages along the tab bar.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Page {
    Home,
    #[default]
    Prepare,
    Preview,
    Device,
    MultiDevice,
    Project,
}

impl Page {
    pub fn from_name(name: &str) -> Option<Page> {
        Some(match name.to_ascii_lowercase().replace(['-', ' '], "").as_str() {
            "home" => Page::Home,
            "prepare" => Page::Prepare,
            "preview" => Page::Preview,
            "device" => Page::Device,
            "multidevice" => Page::MultiDevice,
            "project" => Page::Project,
            _ => return None,
        })
    }

    /// The 3D pages share the sidebar and the canvas.
    pub fn is_3d(self) -> bool {
        matches!(self, Page::Prepare | Page::Preview)
    }
}

/// How the Cut tool cuts.
#[derive(Clone, Debug, PartialEq)]
pub struct CutOptions {
    pub keep_above: bool,
    pub keep_below: bool,
    /// Turn the top piece over, its cut face on the plate.
    pub flip_above: bool,
    /// Keep the pieces as parts of one object.
    pub to_parts: bool,
    pub connector: Connector,
    pub connector_radius: f32,
    /// How far a connector reaches into each side, mm.
    pub connector_depth: f32,
    /// Clearance round a connector in its hole, mm.
    pub tolerance: f32,
    /// Where connectors go, on the cut plane (scene coordinates).
    pub connectors: Vec<Vec3>,
}

impl Default for CutOptions {
    fn default() -> Self {
        Self {
            keep_above: true,
            keep_below: true,
            flip_above: false,
            to_parts: false,
            connector: Connector::Plug,
            connector_radius: 2.5,
            connector_depth: 5.0,
            tolerance: 0.15,
            connectors: Vec::new(),
        }
    }
}

/// OrcaSlicer's cut connectors.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Connector {
    /// A peg on the top piece that fits a hole in the bottom one.
    Plug,
    /// Holes in both pieces and a separate pin to glue in.
    Dowel,
}

/// A cylinder of radius `r` from `from` along `dir` for `len`.
fn pin(r: f32, from: Vec3, dir: Vec3, len: f32) -> CosmicSlicer_model::Mesh {
    let mut m = primitives::cylinder(r, len.max(0.01), 32);
    let turn = glam::Quat::from_rotation_arc(Vec3::Z, dir.normalize_or(Vec3::Z));
    for v in &mut m.vertices {
        *v = from + turn * *v;
    }
    m
}

/// A volume of `kind` from a mesh in scene coordinates, for an object whose frame is `frame`.
fn volume_in(
    frame: glam::Mat4,
    name: &str,
    kind: CosmicSlicer_model::PartKind,
    mut mesh: CosmicSlicer_model::Mesh,
) -> CosmicSlicer_model::Volume {
    let inv = frame.inverse();
    for v in &mut mesh.vertices {
        *v = inv.transform_point3(*v);
    }
    CosmicSlicer_model::Volume {
        name: name.to_owned(),
        kind,
        mesh: std::sync::Arc::new(mesh),
        transform: Transform::default(),
        overrides: Vec::new(),
    }
}

/// A volume or height range of an object, as the object list selects it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sub {
    Volume(usize),
    Range(usize),
}

/// The plate surfaces OrcaSlicer offers ("Plate type"); their filament temperatures are the
/// `*_plate_temp` options.
pub const BED_TYPES: &[(&str, &str)] = &[
    ("Cool Plate (SuperTack)", "supertack_plate_temp"),
    ("Cool Plate", "cool_plate_temp"),
    ("Engineering Plate", "eng_plate_temp"),
    ("Smooth PEI Plate / High Temp Plate", "hot_plate_temp"),
    ("Textured PEI Plate", "textured_plate_temp"),
    ("Textured Cool Plate", "textured_cool_plate_temp"),
];

/// Preferences kept between runs.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Prefs {
    pub theme: Theme,
    /// Show every setting, not just the simple ones.
    pub advanced: bool,
    pub perspective: bool,
    pub show_labels: bool,
    pub show_grid: bool,
    pub multi_device: bool,
    /// Zoom towards the mouse rather than the screen centre.
    pub zoom_to_mouse: bool,
    /// Imperial shows inches in the object panels; settings stay in OrcaSlicer's units.
    pub imperial: bool,
    pub recent: Vec<PathBuf>,
    /// Printer presets installed in the setup wizard.
    pub installed_printers: Vec<String>,
    /// Filament base names made visible in the setup wizard ("Generic PLA").
    pub visible_filaments: Vec<String>,
    /// The setup wizard has been completed.
    pub setup_done: bool,
}

impl Default for Prefs {
    fn default() -> Self {
        Self {
            theme: Theme::Dark,
            advanced: false,
            perspective: true,
            show_labels: false,
            show_grid: true,
            multi_device: false,
            zoom_to_mouse: true,
            imperial: false,
            recent: Vec::new(),
            installed_printers: Vec::new(),
            visible_filaments: Vec::new(),
            setup_done: false,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    Info,
    Warning,
    Error,
}

#[derive(Clone, Debug)]
pub struct Notice {
    pub text: String,
    pub level: Level,
    /// Seconds since start when it appeared; info notices fade after a while.
    pub at: f64,
}

/// Project details shown on the Project page.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct ProjectInfo {
    pub name: String,
    pub path: Option<PathBuf>,
    pub author: String,
    pub license: String,
    pub description: String,
}

#[derive(Clone)]
struct Snapshot {
    model: Model,
    selection: Vec<u64>,
    locked: Vec<bool>,
}

/// Most undo steps kept.
const HISTORY: usize = 200;

/// Triangles past which an import warns that slicing will be slow.
const HUGE_MESH: usize = 2_000_000;

/// Models smaller than this (mm³) look as though they were drawn in inches (OrcaSlicer's
/// test).
const INCHES_VOLUME: f32 = 9.0;

pub struct Session {
    pub schema: &'static Schema,
    pub presets: PresetBundle,
    pub model: Model,
    pub selection: Vec<u64>,
    /// The current plate (0-based).
    pub plate: usize,
    /// Locked plates are left alone by Arrange.
    pub locked: Vec<bool>,
    pub bed_type: usize,
    pub project: ProjectInfo,
    pub prefs: Prefs,
    pub page: Page,
    pub notices: Vec<Notice>,
    /// Set by the UI each frame: seconds since start.
    pub now: f64,
    /// Bumped on every change to objects or plates, so the scene can rebuild.
    pub revision: u64,
    /// The last finished slice; it shows only while it matches the plate and settings.
    pub sliced: Option<crate::slicer::SliceState>,
    /// A G-code file open in the Preview (OrcaSlicer's G-code viewer): its name and contents.
    pub viewer: Option<(String, std::sync::Arc<CosmicSlicer_engine::Sliced>)>,
    /// A slice running: progress (0 to 1) and stage, for the progress card.
    pub slicing: Option<(f32, String)>,
    /// Set by the progress card's Cancel.
    pub cancel_slice: bool,
    undo: Vec<Snapshot>,
    redo: Vec<Snapshot>,
    /// Changes since the project was saved.
    pub dirty: bool,
    /// Objects copied with Copy, for Paste.
    clipboard: Vec<CosmicSlicer_model::Object>,
    /// Per-plate settings (name, bed type, sequence), one per plate.
    pub plate_settings: Vec<PlateSettings>,
    /// Objects just imported that look as if they are in inches, for the prompt.
    pub units_prompt: Option<Vec<u64>>,
}

/// OrcaSlicer's "Customize current plate" settings.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct PlateSettings {
    pub name: String,
    /// `None` follows the project's plate type.
    pub bed_type: Option<usize>,
    /// 0 same as global, 1 by layer, 2 by object.
    pub sequence: usize,
    /// 0 same as global, 1 on, 2 off.
    pub spiral: usize,
}

impl Default for Session {
    fn default() -> Self {
        Self::new()
    }
}

impl Session {
    pub fn new() -> Self {
        Self {
            schema: Schema::get(),
            presets: crate::profiles::build(&Prefs::default()),
            model: Model::new(),
            selection: Vec::new(),
            plate: 0,
            locked: vec![false],
            bed_type: 4,
            project: ProjectInfo { name: "Untitled".into(), ..Default::default() },
            prefs: Prefs::default(),
            page: Page::Prepare,
            notices: Vec::new(),
            now: 0.0,
            revision: 1,
            sliced: None,
            viewer: None,
            slicing: None,
            cancel_slice: false,
            undo: Vec::new(),
            redo: Vec::new(),
            dirty: false,
            clipboard: Vec::new(),
            plate_settings: vec![PlateSettings::default()],
            units_prompt: None,
        }
    }

    pub fn plate_settings_mut(&mut self, p: usize) -> &mut PlateSettings {
        if self.plate_settings.len() <= p {
            self.plate_settings.resize(p + 1, PlateSettings::default());
        }
        let last = self.plate_settings.len() - 1;
        &mut self.plate_settings[p.min(last)]
    }

    pub fn plate_name(&self, p: usize) -> String {
        match self.plate_settings.get(p).map(|s| s.name.trim()).filter(|n| !n.is_empty()) {
            Some(name) => format!("Plate {} · {name}", p + 1),
            None => format!("Plate {}", p + 1),
        }
    }

    pub fn copy(&mut self) {
        self.clipboard = self.selection.iter().filter_map(|id| self.model.get(*id).cloned()).collect();
    }

    pub fn can_paste(&self) -> bool {
        !self.clipboard.is_empty()
    }

    /// Pastes the copied objects onto the current plate, beside what is there.
    pub fn paste(&mut self) {
        if self.clipboard.is_empty() {
            return;
        }
        self.checkpoint();
        let mut ids = Vec::new();
        for mut o in self.clipboard.clone() {
            let shift = self.plate_origin(self.plate) - self.plate_origin(o.plate);
            o.transform.offset += shift;
            o.plate = self.plate;
            let id = self.model.insert(o);
            self.place_clear_of_others(id);
            ids.push(id);
        }
        self.selection = ids;
    }

    pub fn notify(&mut self, level: Level, text: impl Into<String>) {
        let text = text.into();
        self.notices.retain(|n| n.text != text);
        self.notices.push(Notice { text, level, at: self.now });
        if self.notices.len() > 6 {
            self.notices.remove(0);
        }
    }

    // ---- the bed ----

    /// The printable area of the selected printer: (min corner, size), in plate coordinates.
    pub fn bed(&self) -> (Vec2, Vec2) {
        let area = self.presets.value(self.schema, TabKind::Printer, 0, "printable_area");
        let pts = parse_points(&area);
        let (mut lo, mut hi) = (Vec2::splat(f32::INFINITY), Vec2::splat(f32::NEG_INFINITY));
        for [x, y] in &pts {
            let p = Vec2::new(*x as f32, *y as f32);
            lo = lo.min(p);
            hi = hi.max(p);
        }
        let size = hi - lo;
        if pts.len() < 3 || !lo.is_finite() || size.x < 1.0 || size.y < 1.0 || size.max_element() > 10_000.0 {
            return (Vec2::ZERO, Vec2::splat(220.0));
        }
        (lo, size)
    }

    pub fn bed_outline(&self) -> Vec<Vec2> {
        let area = self.presets.value(self.schema, TabKind::Printer, 0, "printable_area");
        let pts: Vec<Vec2> = parse_points(&area).iter().map(|[x, y]| Vec2::new(*x as f32, *y as f32)).collect();
        if pts.len() >= 3 && pts.len() <= 4096 && pts.iter().all(|p| p.is_finite()) {
            pts
        } else {
            let (lo, size) = self.bed();
            vec![lo, lo + Vec2::new(size.x, 0.0), lo + size, lo + Vec2::new(0.0, size.y)]
        }
    }

    pub fn printable_height(&self) -> f32 {
        let h = CosmicSlicer_config::first_number(&self.presets.value(
            self.schema,
            TabKind::Printer,
            0,
            "printable_height",
        ))
        .unwrap_or(250.0) as f32;
        if h.is_finite() && h > 0.0 { h.min(10_000.0) } else { 250.0 }
    }

    /// Where plate `p` sits in the scene: plates stand in a row, a fifth of a bed apart.
    pub fn plate_origin(&self, p: usize) -> Vec3 {
        let (_, size) = self.bed();
        let cols = plate_columns(self.model.plates);
        let (col, row) = (p % cols, p / cols);
        Vec3::new(col as f32 * size.x * 1.2, -(row as f32) * size.y * 1.2, 0.0)
    }

    /// The plate under scene point `x`, `y`, if any.
    pub fn plate_at(&self, x: f32, y: f32) -> Option<usize> {
        let (lo, size) = self.bed();
        (0..self.model.plates).find(|&p| {
            let o = self.plate_origin(p);
            let (min, max) = (o.truncate() + lo, o.truncate() + lo + size);
            x >= min.x && x <= max.x && y >= min.y && y <= max.y
        })
    }

    /// Is the object inside its plate's printable volume?
    pub fn fits(&self, id: u64) -> bool {
        let Some(o) = self.model.get(id) else { return true };
        let (lo, size) = self.bed();
        let origin = self.plate_origin(o.plate);
        let b = o.bounds();
        let min = origin + lo.extend(0.0) - Vec3::splat(0.01);
        let max = origin + (lo + size).extend(self.printable_height()) + Vec3::splat(0.01);
        b.is_valid() && b.min.cmpge(min).all() && b.max.cmple(max).all()
    }

    pub fn plate_bounds(&self, p: usize) -> Aabb {
        let (lo, size) = self.bed();
        let o = self.plate_origin(p);
        Aabb { min: o + lo.extend(0.0), max: o + (lo + size).extend(self.printable_height()) }
    }

    // ---- history ----

    fn snapshot(&self) -> Snapshot {
        Snapshot { model: self.model.clone(), selection: self.selection.clone(), locked: self.locked.clone() }
    }

    /// Records the state before a change.
    pub fn checkpoint(&mut self) {
        self.undo.push(self.snapshot());
        if self.undo.len() > HISTORY {
            self.undo.remove(0);
        }
        self.redo.clear();
        self.touch();
    }

    fn touch(&mut self) {
        self.revision = self.revision.wrapping_add(1);
        self.dirty = true;
        self.sliced = None;
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    fn restore(&mut self, s: Snapshot) {
        self.model = s.model;
        self.selection = s.selection;
        self.locked = s.locked;
        self.plate = self.plate.min(self.model.plates.saturating_sub(1));
        self.touch();
    }

    pub fn undo(&mut self) {
        if let Some(s) = self.undo.pop() {
            self.redo.push(self.snapshot());
            self.restore(s);
        }
    }

    pub fn redo(&mut self) {
        if let Some(s) = self.redo.pop() {
            self.undo.push(self.snapshot());
            self.restore(s);
        }
    }

    // ---- objects ----

    fn plate_centre(&self, p: usize) -> Vec3 {
        let (lo, size) = self.bed();
        self.plate_origin(p) + (lo + size / 2.0).extend(0.0)
    }

    /// Adds a primitive ("Cube", "Cylinder", ...) to the current plate and selects it.
    pub fn add_primitive(&mut self, name: &str) {
        let Some(mesh) = primitives::by_name(name) else {
            self.notify(Level::Error, format!("there is no primitive called {name}"));
            return;
        };
        self.checkpoint();
        let id = self.model.add(name, mesh, self.plate_centre(self.plate), self.plate);
        self.place_clear_of_others(id);
        self.selection = vec![id];
    }

    /// Imports a model file onto the current plate. A file of several objects (a 3MF, a zip)
    /// keeps them as it laid them out, centred on the plate.
    pub fn import(&mut self, path: &Path) {
        match CosmicSlicer_model::load(path) {
            Ok(file) => {
                self.checkpoint();
                for w in &file.warnings {
                    self.notify(Level::Warning, w.clone());
                }
                let single = file.objects.len() == 1;
                let group = file.objects.iter().fold(Aabb::EMPTY, |b, o| b.union(&o.mesh.bounds()));
                let centre = self.plate_centre(self.plate);
                let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("Object").to_owned();
                let mut ids = Vec::new();
                let mut triangles = 0;
                let mut fixed = CosmicSlicer_model::repair::Report::default();
                for mut o in file.objects {
                    let r = CosmicSlicer_model::repair::repair(&mut o.mesh);
                    fixed.degenerate += r.degenerate;
                    fixed.flipped += r.flipped;
                    fixed.holes_closed += r.holes_closed;
                    fixed.open_edges += r.open_edges;
                    if r.degenerate + r.holes_closed > 0 {
                        // Triangles moved; painted facets no longer line up.
                        o.paint = None;
                    }
                    let rel = o.mesh.bounds().centre() - group.centre();
                    let at = centre + Vec3::new(rel.x, rel.y, 0.0);
                    let name = if single { file_name.clone() } else { o.name };
                    triangles += o.mesh.triangle_count();
                    let id = self.model.add(name, o.mesh, at, self.plate);
                    if let (Some(paint), Some(obj)) = (o.paint, self.model.get_mut(id)) {
                        obj.paint = Some(std::sync::Arc::new(paint));
                    }
                    ids.push(id);
                }
                if let [id] = ids.as_slice() {
                    self.place_clear_of_others(*id);
                }
                if self.project.name == "Untitled" && self.model.objects.len() == ids.len() {
                    self.project.name = path.file_stem().and_then(|n| n.to_str()).unwrap_or("Untitled").to_owned();
                }
                self.remember(path);
                if ids.iter().any(|id| !self.fits(*id)) {
                    self.notify(
                        Level::Warning,
                        "An object is laid over the boundary of the plate or exceeds the height limit.",
                    );
                }
                if fixed.changed() || fixed.open_edges > 0 {
                    let level = if fixed.open_edges > 0 { Level::Warning } else { Level::Info };
                    self.notify(level, fixed.describe());
                }
                if triangles > HUGE_MESH {
                    self.notify(
                        Level::Warning,
                        format!(
                            "This model has {triangles} triangles; slicing it may be slow and use a lot of memory."
                        ),
                    );
                }
                // Tiny models were probably drawn in inches (OrcaSlicer asks the same).
                let volume: f32 = ids.iter().filter_map(|id| self.model.get(*id)).map(|o| o.mesh.volume().abs()).sum();
                if volume > 0.0 && volume < INCHES_VOLUME {
                    self.units_prompt = Some(ids.clone());
                }
                self.selection = ids;
            }
            Err(e) => self.notify(Level::Error, e.to_string()),
        }
    }

    /// A primitive or model to add to object `id` as a volume of `kind`, sized to the object
    /// and placed at its middle. Returns the new volume's index.
    fn add_volume_mesh(
        &mut self,
        id: u64,
        kind: CosmicSlicer_model::PartKind,
        name: &str,
        mesh: CosmicSlicer_model::Mesh,
        fit: bool,
    ) -> Option<usize> {
        let o = self.model.get(id)?;
        let local = o.mesh.bounds();
        let at = local.centre();
        let mut mesh = mesh;
        if fit {
            // Half the object's size, as OrcaSlicer sizes a new modifier.
            let want = (local.size().max_element() * 0.5).max(5.0);
            let have = mesh.bounds().size().max_element().max(1e-3);
            for v in &mut mesh.vertices {
                *v *= want / have;
            }
        }
        self.checkpoint();
        let index = self.model.add_volume(id, name, kind, mesh, at)?;
        self.selection = vec![id];
        Some(index)
    }

    /// Adds primitive `primitive` to object `id` as a volume of `kind`.
    pub fn add_volume(&mut self, id: u64, kind: CosmicSlicer_model::PartKind, primitive: &str) -> Option<usize> {
        let mesh = primitives::by_name(primitive)?;
        let name = format!("{} {}", kind.label(), primitive.to_lowercase());
        self.add_volume_mesh(id, kind, &name, mesh, true)
    }

    /// Adds the model in `path` to object `id` as a volume of `kind`, at its own size.
    pub fn add_volume_file(&mut self, id: u64, kind: CosmicSlicer_model::PartKind, path: &Path) -> Option<usize> {
        match CosmicSlicer_model::load(path) {
            Ok(file) => {
                let mut mesh = CosmicSlicer_model::Mesh::default();
                for o in file.objects {
                    let base = mesh.vertices.len() as u32;
                    mesh.vertices.extend(o.mesh.vertices);
                    mesh.triangles.extend(o.mesh.triangles.iter().map(|t| t.map(|i| i + base)));
                }
                let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("Part").to_owned();
                self.add_volume_mesh(id, kind, &name, mesh, false)
            }
            Err(e) => {
                self.notify(Level::Error, e.to_string());
                None
            }
        }
    }

    pub fn remove_volume(&mut self, id: u64, index: usize) {
        self.checkpoint();
        self.model.remove_volume(id, index);
    }

    pub fn set_volume_kind(&mut self, id: u64, index: usize, kind: CosmicSlicer_model::PartKind) {
        self.checkpoint();
        if let Some(v) = self.model.get_mut(id).and_then(|o| o.parts.get_mut(index)) {
            v.kind = kind;
        }
        if let Some(o) = self.model.get_mut(id) {
            o.drop_to_bed();
        }
    }

    /// Moves or scales a volume within its object, as its panel is dragged.
    pub fn set_volume_transform(&mut self, id: u64, index: usize, t: Transform) {
        if !(t.offset.is_finite() && t.rotation.is_finite() && t.scale.is_finite() && t.scale.min_element() > 0.0) {
            return;
        }
        if let Some(v) = self.model.get_mut(id).and_then(|o| o.parts.get_mut(index)) {
            v.transform = t;
        }
        if let Some(o) = self.model.get_mut(id) {
            o.drop_to_bed();
        }
        self.touch();
    }

    /// Splits each selected object into parts of itself.
    pub fn split_to_parts(&mut self) {
        self.checkpoint();
        let mut single = 0;
        for id in self.selection.clone() {
            if self.model.split_to_parts(id) < 2 {
                single += 1;
            }
        }
        if single > 0 {
            self.notify(Level::Info, "The object is a single piece; there is nothing to split.");
        }
    }

    /// Joins the selected objects into one object of several parts.
    pub fn assemble(&mut self) {
        if self.selection.len() < 2 {
            return;
        }
        self.checkpoint();
        if let Some(id) = self.model.assemble(&self.selection.clone()) {
            self.selection = vec![id];
        }
    }

    /// Cuts the other selected objects out of the first: they become its negative volumes.
    pub fn subtract(&mut self) {
        let Some(&first) = self.selection.first() else { return };
        if self.selection.len() < 2 {
            return;
        }
        let before = self.model.get(first).map_or(0, |o| o.parts.len());
        self.assemble();
        if let Some(o) = self.model.get_mut(first) {
            for p in o.parts.iter_mut().skip(before) {
                p.kind = CosmicSlicer_model::PartKind::Negative;
            }
            o.drop_to_bed();
        }
    }

    /// Scales object `id` evenly to the largest size its plate and the printer's height allow,
    /// in the middle of the plate.
    pub fn scale_to_fit(&mut self, id: u64) {
        let (_, bed) = self.bed();
        let height = self.printable_height();
        let Some(o) = self.model.get(id) else { return };
        let size = o.bounds().size();
        if size.min_element() <= 0.0 {
            return;
        }
        let k = (bed.x * 0.95 / size.x).min(bed.y * 0.95 / size.y).min(height * 0.99 / size.z);
        if !(k.is_finite() && k > 0.0) {
            return;
        }
        let centre = self.plate_centre(o.plate);
        self.checkpoint();
        if let Some(o) = self.model.get_mut(id) {
            o.transform.scale *= k;
            let b = o.bounds();
            o.transform.offset += Vec3::new(centre.x - b.centre().x, centre.y - b.centre().y, 0.0);
            o.drop_to_bed();
        }
    }

    /// Adds a height range over the middle third of object `id`.
    pub fn add_range(&mut self, id: u64) -> Option<usize> {
        let b = self.model.get(id)?.bounds();
        let h = b.size().z;
        self.checkpoint();
        let o = self.model.get_mut(id)?;
        let (lo, hi) = ((h / 3.0 * 10.0).round() / 10.0, (h * 2.0 / 3.0 * 10.0).round() / 10.0);
        o.ranges.push(CosmicSlicer_model::HeightRange { lo, hi: hi.max(lo + 0.1), overrides: Vec::new() });
        Some(o.ranges.len() - 1)
    }

    pub fn remove_range(&mut self, id: u64, index: usize) {
        self.checkpoint();
        if let Some(o) = self.model.get_mut(id)
            && index < o.ranges.len()
        {
            o.ranges.remove(index);
        }
    }

    /// Sets a height range's ends, kept in order.
    pub fn set_range(&mut self, id: u64, index: usize, lo: f32, hi: f32) {
        if !(lo.is_finite() && hi.is_finite()) {
            return;
        }
        if let Some(r) = self.model.get_mut(id).and_then(|o| o.ranges.get_mut(index)) {
            r.lo = lo.max(0.0).min(hi);
            r.hi = hi.max(r.lo + 0.01);
        }
        self.touch();
    }

    /// Moves object `id` to place `to` in the list.
    pub fn reorder(&mut self, id: u64, to: usize) {
        self.checkpoint();
        self.model.reorder(id, to);
    }

    /// Paints triangles `tris` of object `id` (no undo step: a stroke takes one at its start).
    pub fn paint(&mut self, id: u64, kind: CosmicSlicer_model::threemf::PaintKind, tris: &[u32], state: u8) {
        let Some(o) = self.model.get_mut(id) else { return };
        let paint = o.paint.get_or_insert_with(Default::default);
        std::sync::Arc::make_mut(paint).set(kind, tris, state);
        if paint.is_empty() {
            o.paint = None;
        }
        self.touch();
    }

    /// Clears what is painted on the selected objects for `kind`.
    pub fn clear_paint(&mut self, kind: CosmicSlicer_model::threemf::PaintKind) {
        self.checkpoint();
        for id in self.selection.clone() {
            if let Some(o) = self.model.get_mut(id)
                && let Some(p) = o.paint.as_mut()
            {
                std::sync::Arc::make_mut(p).clear(kind);
                if p.is_empty() {
                    o.paint = None;
                }
            }
        }
    }

    /// Adds a brim ear under object `id` at scene point `at`.
    pub fn add_brim_ear(&mut self, id: u64, at: Vec3, radius: f32) {
        let Some(o) = self.model.get(id) else { return };
        let inv = o.matrix().inverse();
        // On the plate, under the point.
        let local = inv.transform_point3(Vec3::new(at.x, at.y, o.bounds().min.z));
        if !local.is_finite() || !(radius.is_finite() && radius > 0.0) {
            return;
        }
        self.checkpoint();
        if let Some(o) = self.model.get_mut(id) {
            o.brim_ears.push(CosmicSlicer_model::BrimEar { at: local, radius });
        }
    }

    pub fn clear_brim_ears(&mut self, id: u64) {
        self.checkpoint();
        if let Some(o) = self.model.get_mut(id) {
            o.brim_ears.clear();
        }
    }

    /// Sets one of object `id`'s own settings.
    pub fn set_override(&mut self, id: u64, key: &str, value: &str) {
        self.checkpoint();
        if let Some(o) = self.model.get_mut(id) {
            match o.overrides.iter_mut().find(|(k, _)| k == key) {
                Some(slot) => slot.1 = value.to_owned(),
                None => o.overrides.push((key.to_owned(), value.to_owned())),
            }
        }
    }

    /// Removes object `id`'s brim ear nearest scene point `at`.
    pub fn remove_brim_ear(&mut self, id: u64, at: Vec3) {
        let Some(o) = self.model.get(id) else { return };
        let m = o.matrix();
        let nearest = o
            .brim_ears
            .iter()
            .enumerate()
            .map(|(i, e)| (i, m.transform_point3(e.at).truncate().distance(at.truncate())))
            .min_by(|a, b| a.1.total_cmp(&b.1));
        if let Some((i, _)) = nearest {
            self.checkpoint();
            if let Some(o) = self.model.get_mut(id) {
                o.brim_ears.remove(i);
            }
        }
    }

    /// Adds an object from a mesh in scene coordinates, keeping where it is; returns its id.
    fn add_in_place(
        &mut self,
        name: &str,
        mut mesh: CosmicSlicer_model::Mesh,
        like: &CosmicSlicer_model::Object,
    ) -> u64 {
        let b = mesh.bounds();
        let c = Vec3::new(b.centre().x, b.centre().y, b.min.z);
        for v in &mut mesh.vertices {
            *v -= c;
        }
        let mut o = like.clone();
        o.name = name.to_owned();
        o.mesh = std::sync::Arc::new(mesh);
        o.transform = Transform { offset: c, ..Default::default() };
        o.paint = None;
        o.parts.clear();
        o.brim_ears.clear();
        self.model.insert(o)
    }

    /// Adds outlines `loops` extruded `depth` mm: as an object of its own, or on the top of
    /// object `target` as a raised part, an engraving or a modifier.
    pub fn add_emboss(
        &mut self,
        name: &str,
        loops: &[Vec<glam::Vec2>],
        depth: f32,
        mode: crate::viewport::tools::EmbossMode,
        target: Option<u64>,
    ) {
        use crate::viewport::tools::EmbossMode;
        use CosmicSlicer_model::PartKind;
        let engrave = matches!(mode, EmbossMode::Engrave | EmbossMode::Modifier);
        // Engravings reach a little above the surface so they cut it cleanly.
        let mesh = CosmicSlicer_model::shape2d::extrude(loops, if engrave { depth + 0.1 } else { depth });
        if mesh.triangles.is_empty() {
            self.notify(Level::Warning, "There is nothing to emboss.");
            return;
        }
        let kind = match mode {
            EmbossMode::Object => None,
            EmbossMode::Emboss => Some(PartKind::Normal),
            EmbossMode::Engrave => Some(PartKind::Negative),
            EmbossMode::Modifier => Some(PartKind::Modifier),
        };
        match (kind, target.and_then(|id| self.model.get(id)).map(|o| (o.id, o.mesh.bounds()))) {
            (Some(kind), Some((id, b))) => {
                let h = mesh.bounds().size().z;
                // Centred on the top: raised sits on it, engraved sinks into it.
                let z = if engrave { b.max.z - depth + h / 2.0 } else { b.max.z + h / 2.0 };
                self.checkpoint();
                self.model.add_volume(id, name, kind, mesh, Vec3::new(b.centre().x, b.centre().y, z));
                self.selection = vec![id];
            }
            _ => {
                self.checkpoint();
                let id = self.model.add(name, mesh, self.plate_centre(self.plate), self.plate);
                self.place_clear_of_others(id);
                self.selection = vec![id];
            }
        }
    }

    /// Cuts object `id` by the plane through `origin` (scene coordinates) with `normal`.
    pub fn cut(&mut self, id: u64, origin: Vec3, normal: Vec3, opts: &CutOptions) {
        use CosmicSlicer_model::PartKind;
        let Some(o) = self.model.get(id).cloned() else { return };
        let n = normal.normalize_or(Vec3::Z);
        // Everything that prints, in scene coordinates.
        let mut world = CosmicSlicer_model::Mesh::default();
        for (mesh, m) in o.solids() {
            let base = world.vertices.len() as u32;
            world.vertices.extend(mesh.vertices.iter().map(|v| m.transform_point3(*v)));
            world.triangles.extend(mesh.triangles.iter().map(|t| t.map(|i| i + base)));
        }
        let halves = CosmicSlicer_model::cut::cut(&world, origin, n);
        if halves.above.triangles.is_empty() || halves.below.triangles.is_empty() {
            self.notify(Level::Warning, "The cut plane does not cross the object.");
            return;
        }
        self.checkpoint();
        let (r, depth, tol) = (opts.connector_radius.max(0.2), opts.connector_depth.max(0.2), opts.tolerance.max(0.0));
        let mut above_volumes = Vec::new();
        let mut below_volumes = Vec::new();
        let mut dowels = Vec::new();
        for c in &opts.connectors {
            match opts.connector {
                Connector::Plug => {
                    above_volumes.push(("Plug", PartKind::Normal, pin(r, *c - n * depth, n, depth + 0.01)));
                    below_volumes.push((
                        "Plug hole",
                        PartKind::Negative,
                        pin(r + tol, *c - n * (depth + 0.2), n, depth + 0.4),
                    ));
                }
                Connector::Dowel => {
                    above_volumes.push(("Dowel hole", PartKind::Negative, pin(r + tol, *c - n * 0.2, n, depth + 0.2)));
                    below_volumes.push((
                        "Dowel hole",
                        PartKind::Negative,
                        pin(r + tol, *c - n * depth, n, depth + 0.2),
                    ));
                    dowels.push(primitives::cylinder(r, (2.0 * depth - 0.4).max(0.5), 32));
                }
            }
        }
        // The top piece turned over puts its cut face down.
        let flip = opts.flip_above && !opts.to_parts;
        let turn =
            if flip { glam::Mat4::from_quat(glam::Quat::from_rotation_arc(n, Vec3::Z)) } else { glam::Mat4::IDENTITY };
        let mut above = halves.above;
        for v in &mut above.vertices {
            *v = turn.transform_point3(*v);
        }
        self.model.remove(&[id]);
        let mut ids = Vec::new();
        let name = o.name.clone();
        if opts.to_parts {
            let new = self.add_in_place(&name, above, &o);
            let frame = self.model.get(new).map(CosmicSlicer_model::Object::matrix).unwrap_or_default();
            if let Some(obj) = self.model.get_mut(new) {
                obj.parts.push(volume_in(frame, &format!("{name} (bottom)"), PartKind::Normal, halves.below));
            }
            ids.push(new);
        } else {
            if opts.keep_above {
                let new = self.add_in_place(&format!("{name} (top)"), above, &o);
                let frame = self.model.get(new).map(CosmicSlicer_model::Object::matrix).unwrap_or_default();
                if let Some(obj) = self.model.get_mut(new) {
                    for (vn, kind, mut mesh) in above_volumes {
                        for v in &mut mesh.vertices {
                            *v = turn.transform_point3(*v);
                        }
                        obj.parts.push(volume_in(frame, vn, kind, mesh));
                    }
                }
                ids.push(new);
            }
            if opts.keep_below {
                let new = self.add_in_place(&format!("{name} (bottom)"), halves.below, &o);
                let frame = self.model.get(new).map(CosmicSlicer_model::Object::matrix).unwrap_or_default();
                if let Some(obj) = self.model.get_mut(new) {
                    for (vn, kind, mesh) in below_volumes {
                        obj.parts.push(volume_in(frame, vn, kind, mesh));
                    }
                }
                ids.push(new);
            }
            for d in dowels {
                let at = self.plate_centre(o.plate);
                let new = self.model.add("Dowel", d, at, o.plate);
                if let Some(obj) = self.model.get_mut(new) {
                    obj.filament = o.filament;
                }
                ids.push(new);
            }
        }
        for id in &ids {
            if let Some(obj) = self.model.get_mut(*id) {
                obj.drop_to_bed();
            }
        }
        // Pieces beside each other rather than on top of one another.
        for id in ids.iter().skip(1) {
            self.place_clear_of_others(*id);
        }
        self.selection = ids;
    }

    /// Puts `path` at the top of the recent files.
    pub fn remember(&mut self, path: &Path) {
        self.prefs.recent.retain(|p| p != path);
        self.prefs.recent.insert(0, path.to_owned());
        self.prefs.recent.truncate(12);
    }

    /// Scales objects by `factor` about their own centres (inches to millimetres), resting
    /// them on the plate again.
    pub fn scale_objects(&mut self, ids: &[u64], factor: f32) {
        if !(factor.is_finite() && factor > 0.0) {
            return;
        }
        self.checkpoint();
        for id in ids {
            if let Some(o) = self.model.get_mut(*id) {
                o.transform.scale *= factor;
                o.drop_to_bed();
            }
        }
        if let [id] = ids {
            self.place_clear_of_others(*id);
        }
    }

    /// Nudges a newly added object along X until it overlaps nothing on its plate.
    fn place_clear_of_others(&mut self, id: u64) {
        let others: Vec<Aabb> = self
            .model
            .objects
            .iter()
            .filter(|o| o.id != id && self.model.get(id).is_some_and(|n| n.plate == o.plate))
            .map(|o| o.bounds())
            .collect();
        for _ in 0..64 {
            let Some(o) = self.model.get(id) else { return };
            let b = o.bounds();
            let hit = others
                .iter()
                .find(|a| a.min.x < b.max.x && b.min.x < a.max.x && a.min.y < b.max.y && b.min.y < a.max.y);
            let Some(a) = hit else { return };
            let shift = a.max.x - b.min.x + 5.0;
            if let Some(o) = self.model.get_mut(id) {
                o.transform.offset.x += shift;
            }
        }
    }

    pub fn select_all(&mut self) {
        self.selection = self.model.objects.iter().filter(|o| o.plate == self.plate).map(|o| o.id).collect();
    }

    pub fn delete_selected(&mut self) {
        if self.selection.is_empty() {
            return;
        }
        self.checkpoint();
        let ids = std::mem::take(&mut self.selection);
        self.model.remove(&ids);
    }

    pub fn delete_all(&mut self) {
        if self.model.objects.is_empty() {
            return;
        }
        self.checkpoint();
        self.model.objects.clear();
        self.selection.clear();
    }

    pub fn clone_selected(&mut self) {
        if self.selection.is_empty() {
            return;
        }
        self.checkpoint();
        let copies: Vec<u64> = self.selection.clone().into_iter().filter_map(|id| self.model.duplicate(id)).collect();
        for &id in &copies {
            self.place_clear_of_others(id);
        }
        self.selection = copies;
    }

    /// Arranges the current plate (or every unlocked plate).
    pub fn arrange(&mut self, all_plates: bool) {
        self.checkpoint();
        let (lo, size) = self.bed();
        let plates: Vec<usize> = if all_plates { (0..self.model.plates).collect() } else { vec![self.plate] };
        let gap = 6.0;
        for p in plates {
            if self.locked.get(p).copied().unwrap_or(false) {
                continue;
            }
            let origin = self.plate_origin(p);
            self.model.arrange(p, origin + lo.extend(0.0), size.extend(self.printable_height()), gap);
        }
    }

    /// Drops the selected objects (or all on the plate) onto the bed.
    pub fn drop_to_bed(&mut self) {
        self.checkpoint();
        let targets = self.targets();
        for id in targets {
            if let Some(o) = self.model.get_mut(id) {
                o.drop_to_bed();
            }
        }
    }

    /// Turns each target so its largest flat area rests on the plate. Shapes with no flat
    /// bigger than a tenth of their surface are left as they are.
    pub fn auto_orient(&mut self) {
        let targets = self.targets();
        if targets.is_empty() {
            return;
        }
        self.checkpoint();
        for id in targets {
            let Some(o) = self.model.get_mut(id) else { continue };
            if let Some(n) = o.mesh.largest_face_normal(&o.matrix()) {
                let mut t = o.transform;
                t.set_quat(glam::Quat::from_rotation_arc(n, Vec3::NEG_Z) * t.quat());
                o.transform = t;
            }
            o.drop_to_bed();
        }
    }

    /// Splits each selected object into its separate pieces.
    pub fn split_selected(&mut self) {
        let mut pieces_made = 0;
        let ids = self.selection.clone();
        let mut new_selection = Vec::new();
        for id in ids {
            let Some(o) = self.model.get(id).cloned() else { continue };
            let pieces = o.mesh.split();
            if pieces.len() < 2 {
                new_selection.push(id);
                continue;
            }
            if pieces_made == 0 {
                self.checkpoint();
            }
            pieces_made += pieces.len();
            let m = o.matrix();
            self.model.remove(&[id]);
            for (i, mut piece) in pieces.into_iter().enumerate() {
                for v in &mut piece.vertices {
                    *v = m.transform_point3(*v);
                }
                let b = piece.bounds();
                let at = Vec3::new(b.centre().x, b.centre().y, b.min.z);
                let new = self.model.add(format!("{}_{}", o.name, i + 1), piece, at, o.plate);
                if let Some(n) = self.model.get_mut(new) {
                    n.filament = o.filament;
                }
                new_selection.push(new);
            }
        }
        if pieces_made == 0 {
            self.notify(Level::Info, "The selected object is one piece; there is nothing to split.");
        }
        self.selection = new_selection;
    }

    /// The selection, or every object on the current plate when nothing is selected.
    pub fn targets(&self) -> Vec<u64> {
        if self.selection.is_empty() {
            self.model.objects.iter().filter(|o| o.plate == self.plate).map(|o| o.id).collect()
        } else {
            self.selection.clone()
        }
    }

    /// Sets one object's transform (one undo step per call).
    pub fn set_transform(&mut self, id: u64, t: Transform) {
        let finite = t.offset.is_finite() && t.rotation.is_finite() && t.scale.is_finite();
        if !finite || t.scale.min_element() <= 0.0 || self.model.get(id).is_none_or(|o| o.transform == t) {
            return;
        }
        self.checkpoint();
        if let Some(o) = self.model.get_mut(id) {
            o.transform = t;
        }
    }

    /// Moves objects while dragging: callers checkpoint once when the drag starts.
    pub fn nudge(&mut self, ids: &[u64], delta: Vec3) {
        if !delta.is_finite() {
            return;
        }
        for &id in ids {
            if let Some(o) = self.model.get_mut(id) {
                o.transform.offset += delta;
            }
        }
        self.revision = self.revision.wrapping_add(1);
    }

    /// Ends a drag: objects belong to the plate their centre is over.
    pub fn settle(&mut self, ids: &[u64]) {
        for &id in ids {
            let Some(c) = self.model.get(id).map(|o| o.bounds().centre()) else { continue };
            if let Some(p) = self.plate_at(c.x, c.y)
                && let Some(o) = self.model.get_mut(id)
            {
                o.plate = p;
            }
        }
        self.touch();
    }

    pub fn set_filament(&mut self, id: u64, filament: usize) {
        if self.model.get(id).is_some_and(|o| o.filament != filament) {
            self.checkpoint();
            if let Some(o) = self.model.get_mut(id) {
                o.filament = filament;
            }
        }
    }

    pub fn rename(&mut self, id: u64, name: String) {
        if !name.trim().is_empty() && self.model.get(id).is_some_and(|o| o.name != name) {
            self.checkpoint();
            if let Some(o) = self.model.get_mut(id) {
                o.name = name;
            }
        }
    }

    pub fn toggle_visible(&mut self, id: u64) {
        self.checkpoint();
        if let Some(o) = self.model.get_mut(id) {
            o.visible = !o.visible;
        }
    }

    pub fn mirror(&mut self, axis: usize) {
        if self.selection.is_empty() {
            return;
        }
        self.checkpoint();
        for id in self.selection.clone() {
            if let Some(o) = self.model.get_mut(id) {
                match axis {
                    0 => o.transform.scale.x = -o.transform.scale.x,
                    1 => o.transform.scale.y = -o.transform.scale.y,
                    _ => o.transform.scale.z = -o.transform.scale.z,
                }
                o.drop_to_bed();
            }
        }
    }

    // ---- plates ----

    pub fn add_plate(&mut self) {
        if self.model.plates >= 100 {
            return;
        }
        self.checkpoint();
        // Objects keep their place on their own plate as the grid grows.
        let before: Vec<Vec3> = (0..self.model.plates).map(|p| self.plate_origin(p)).collect();
        self.model.plates += 1;
        self.locked.push(false);
        self.plate_settings.resize(self.model.plates, PlateSettings::default());
        self.shift_objects(&before);
        self.plate = self.model.plates - 1;
    }

    pub fn delete_plate(&mut self, p: usize) {
        if self.model.plates <= 1 || p >= self.model.plates {
            return;
        }
        self.checkpoint();
        let before: Vec<Vec3> = (0..self.model.plates).map(|q| self.plate_origin(q)).collect();
        let doomed: Vec<u64> = self.model.objects.iter().filter(|o| o.plate == p).map(|o| o.id).collect();
        self.model.remove(&doomed);
        self.selection.retain(|id| !doomed.contains(id));
        let mut old_origins = before.clone();
        for o in &mut self.model.objects {
            if o.plate > p {
                o.plate -= 1;
            }
        }
        old_origins.remove(p);
        self.model.plates -= 1;
        if p < self.locked.len() {
            self.locked.remove(p);
        }
        if p < self.plate_settings.len() {
            self.plate_settings.remove(p);
        }
        self.shift_objects(&old_origins);
        self.plate = self.plate.min(self.model.plates - 1);
    }

    /// Moves every object by the change in its plate's origin.
    fn shift_objects(&mut self, old_origins: &[Vec3]) {
        let new: Vec<Vec3> = (0..self.model.plates).map(|p| self.plate_origin(p)).collect();
        for o in &mut self.model.objects {
            if let (Some(a), Some(b)) = (old_origins.get(o.plate), new.get(o.plate)) {
                o.transform.offset += *b - *a;
            }
        }
    }

    pub fn toggle_lock(&mut self, p: usize) {
        if let Some(l) = self.locked.get_mut(p) {
            *l = !*l;
        }
    }

    /// Clears the project.
    pub fn new_project(&mut self) {
        self.checkpoint();
        self.model = Model::new();
        self.selection.clear();
        self.plate = 0;
        self.locked = vec![false];
        self.plate_settings = vec![PlateSettings::default()];
        self.project = ProjectInfo { name: "Untitled".into(), ..Default::default() };
        self.dirty = false;
    }

    /// The filament number objects print with when the list shrinks.
    pub fn clamp_filaments(&mut self) {
        let n = self.presets.project_filaments.len().max(1);
        for o in &mut self.model.objects {
            o.filament = o.filament.clamp(1, n);
        }
    }

    /// What the Preview shows: an opened G-code file, else the current plate's slice.
    pub fn shown_result(&self) -> Option<std::sync::Arc<CosmicSlicer_engine::Sliced>> {
        if let Some((_, v)) = &self.viewer {
            return Some(std::sync::Arc::clone(v));
        }
        self.sliced.as_ref().filter(|s| s.is_current(self)).map(|s| std::sync::Arc::clone(&s.result))
    }

    pub fn title(&self) -> String {
        format!("{}{}", self.project.name, if self.dirty { "*" } else { "" })
    }
}

/// Plates per row in the scene.
pub fn plate_columns(plates: usize) -> usize {
    (plates as f32).sqrt().ceil().max(1.0) as usize
}

#[cfg(test)]
mod tests {
    use super::*;
    use CosmicSlicer_model::PartKind;
    use CosmicSlicer_model::threemf::PaintKind;

    fn cube_session() -> (Session, u64) {
        let mut s = Session::new();
        s.add_primitive("Cube");
        let id = s.model.objects.first().map_or(0, |o| o.id);
        (s, id)
    }

    #[test]
    fn cutting_makes_closed_pieces_and_connectors() {
        let (mut s, id) = cube_session();
        let b = s.model.get(id).map(|o| o.bounds()).unwrap_or(Aabb::EMPTY);
        let origin = Vec3::new(b.centre().x, b.centre().y, b.min.z + 5.0);
        let mut opts = CutOptions { connectors: vec![origin], ..Default::default() };
        s.cut(id, origin, Vec3::Z, &opts);
        assert_eq!(s.model.objects.len(), 2);
        let vol = |o: &CosmicSlicer_model::Object| o.mesh.volume() * o.transform.scale.x.powi(3);
        let total: f32 = s.model.objects.iter().map(vol).sum();
        assert!((total - 8000.0).abs() < 2.0, "{total}");
        // Plug on top, hole below; both rest on the plate.
        let kinds: Vec<Vec<PartKind>> =
            s.model.objects.iter().map(|o| o.parts.iter().map(|p| p.kind).collect()).collect();
        assert_eq!(kinds, vec![vec![PartKind::Normal], vec![PartKind::Negative]]);
        assert!(s.model.objects.iter().all(|o| o.bounds().min.z.abs() < 1e-3));
        // Undo brings the cube back whole.
        s.undo();
        assert_eq!(s.model.objects.len(), 1);
        // Dowels: holes both sides and a pin; the top turned over.
        opts.connector = Connector::Dowel;
        opts.flip_above = true;
        s.cut(id, origin, Vec3::Z, &opts);
        assert_eq!(s.model.objects.len(), 3);
        // As parts of one object.
        s.undo();
        let opts = CutOptions { to_parts: true, ..Default::default() };
        s.cut(id, origin, Vec3::Z, &opts);
        assert_eq!(s.model.objects.len(), 1);
        assert!(s.model.objects.first().is_some_and(|o| o.parts.len() == 1));
    }

    #[test]
    fn painting_and_undo() {
        let (mut s, id) = cube_session();
        s.checkpoint();
        s.paint(id, PaintKind::Seam, &[0, 1], 1);
        s.paint(id, PaintKind::Seam, &[1], 2);
        let states = s.model.get(id).and_then(|o| o.paint.clone()).map(|p| p.states(PaintKind::Seam));
        assert_eq!(states.map(|m| (m.get(&0).copied(), m.get(&1).copied())), Some((Some(1), Some(2))));
        s.undo();
        assert!(s.model.get(id).is_some_and(|o| o.paint.is_none()));
        s.paint(id, PaintKind::Fuzzy, &[3], 1);
        s.selection = vec![id];
        s.clear_paint(PaintKind::Fuzzy);
        assert!(s.model.get(id).is_some_and(|o| o.paint.is_none()));
    }

    #[test]
    fn emboss_difference_and_fit() {
        let (mut s, id) = cube_session();
        let loops = vec![vec![
            glam::Vec2::ZERO,
            glam::Vec2::new(5.0, 0.0),
            glam::Vec2::new(5.0, 5.0),
            glam::Vec2::new(0.0, 5.0),
        ]];
        s.add_emboss("Tag", &loops, 1.0, crate::viewport::tools::EmbossMode::Engrave, Some(id));
        assert!(s.model.get(id).is_some_and(|o| o.parts.last().is_some_and(|p| p.kind == PartKind::Negative)));
        s.add_emboss("Tag", &loops, 1.0, crate::viewport::tools::EmbossMode::Object, None);
        assert_eq!(s.model.objects.len(), 2);
        // Difference: the second becomes a negative volume of the first.
        s.selection = s.model.objects.iter().map(|o| o.id).collect();
        s.subtract();
        assert_eq!(s.model.objects.len(), 1);
        assert!(s.model.get(id).is_some_and(|o| o.parts.iter().all(|p| p.kind == PartKind::Negative)));
        // Scale to fit fills the plate's width or the printer's height.
        s.scale_to_fit(id);
        assert!(s.fits(id));
        let size = s.model.get(id).map(|o| o.size()).unwrap_or(Vec3::ZERO);
        let (_, bed) = s.bed();
        assert!(size.x > bed.x * 0.9 || size.z > s.printable_height() * 0.9, "{size:?}");
    }

    #[test]
    fn undo_and_redo_round_trip() {
        let mut s = Session::new();
        s.add_primitive("Cube");
        s.add_primitive("Sphere");
        assert_eq!(s.model.objects.len(), 2);
        s.undo();
        assert_eq!(s.model.objects.len(), 1);
        s.redo();
        assert_eq!(s.model.objects.len(), 2);
        s.select_all();
        s.delete_selected();
        assert!(s.model.objects.is_empty());
        s.undo();
        assert_eq!(s.model.objects.len(), 2);
    }

    #[test]
    fn new_objects_do_not_overlap_and_fit() {
        let mut s = Session::new();
        for _ in 0..4 {
            s.add_primitive("Cube");
        }
        let ids: Vec<u64> = s.model.objects.iter().map(|o| o.id).collect();
        for &id in &ids {
            assert!(s.fits(id));
        }
        let boxes: Vec<Aabb> = s.model.objects.iter().map(|o| o.bounds()).collect();
        for (i, a) in boxes.iter().enumerate() {
            for b in boxes.iter().skip(i + 1) {
                assert!(a.max.x <= b.min.x + 1e-3 || b.max.x <= a.min.x + 1e-3);
            }
        }
    }

    #[test]
    fn plates_keep_their_objects() {
        let mut s = Session::new();
        s.add_primitive("Cube");
        let id = s.selection[0];
        s.add_plate();
        s.add_plate();
        assert_eq!(s.plate, 2);
        s.add_primitive("Cone");
        let cone = s.selection[0];
        assert!(s.fits(id) && s.fits(cone));
        s.delete_plate(0);
        assert!(s.model.get(id).is_none());
        let c = s.model.get(cone);
        assert!(c.is_some_and(|c| c.plate == 1));
        assert!(s.fits(cone));
    }

    #[test]
    fn a_bad_printable_area_falls_back() {
        let mut s = Session::new();
        let schema = s.schema;
        s.presets.set_value(schema, TabKind::Printer, 0, "printable_area", "garbage".into());
        assert_eq!(s.bed().1, Vec2::splat(220.0));
        s.presets.set_value(schema, TabKind::Printer, 0, "printable_area", "0x0,1e30x0,1e30x1e30".into());
        assert_eq!(s.bed().1, Vec2::splat(220.0));
    }
}
