//! CosmicSlicer's slicing engine: from objects on a plate and their settings to G-code and
//! the moves the Preview draws.
//!
//! The stages follow OrcaSlicer's: slice the meshes into layers, inset walls, split each
//! layer's fill into top, bottom, solid and sparse regions, fill them, add skirt, brim and
//! supports, then order everything and write G-code with its estimates.
//!
//! The engine must never take the program down: every stage reads clamped settings, the
//! geometry library runs under `catch_unwind`, and [`slice`] itself catches any panic and
//! returns it as an error.

#![allow(non_snake_case)]

pub mod arcs;
pub mod fuzzy;
pub mod gcode;
pub mod gcodeview;
pub mod geom;
pub mod infill;
pub mod lightning;
pub mod macros;
pub mod medial;
pub mod motion;
pub mod overhang;
pub mod plan;
pub mod settings;
pub mod skirt;
pub mod slicing;
pub mod support;
pub mod thumbnail;
pub mod toolpath;
pub mod travel;
pub mod tree;
pub mod walls;

use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use glam::Mat4;

use CosmicSlicer_config::{Config, Schema};
use CosmicSlicer_model::Mesh;

pub use gcode::{Preview, PreviewLayer, Segment, Stats};
pub use settings::{Filament, Settings};
pub use toolpath::Role;

use geom::{Op, Shape};
use slicing::{LayerZ, Placed};
use toolpath::{LayerPlan, Path};

/// One object to print.
#[derive(Clone)]
pub struct JobObject {
    pub name: String,
    pub mesh: Arc<Mesh>,
    /// Placement in printer coordinates (the plate's front-left at the origin).
    pub matrix: Mat4,
    /// 0-based extruder / filament.
    pub extruder: usize,
    /// Process settings this object overrides.
    pub overrides: Vec<(String, String)>,
    /// Support enforcer and blocker volumes, placed like `matrix`.
    pub support_volumes: Vec<SupportVolume>,
    /// Painted support: triangles of `mesh` and what each asks for.
    pub support_paint: Vec<(u32, SupportPaint)>,
}

/// What a support volume or painted facet asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SupportPaint {
    Enforce,
    Block,
}

/// A support enforcer or blocker attached to an object.
#[derive(Clone)]
pub struct SupportVolume {
    pub kind: SupportPaint,
    pub mesh: Arc<Mesh>,
    /// Placement in printer coordinates.
    pub matrix: Mat4,
}

/// One plate to slice.
#[derive(Clone)]
pub struct Job {
    pub printer: Config,
    pub process: Config,
    /// One per extruder.
    pub filaments: Vec<Config>,
    /// The plate type's temperature option, e.g. `textured_plate_temp`.
    pub bed_key: String,
    /// The plate type's name, e.g. "Textured PEI Plate" (custom G-code's `curr_bed_type`).
    pub bed_type_name: String,
    pub objects: Vec<JobObject>,
    pub plate_name: String,
}

/// Shared with the UI while a slice runs: how far along, what stage, and a stop button.
#[derive(Default)]
pub struct Progress {
    permille: AtomicU32,
    stage: Mutex<String>,
    cancel: AtomicBool,
}

impl Progress {
    pub fn fraction(&self) -> f32 {
        self.permille.load(Ordering::Relaxed) as f32 / 1000.0
    }

    pub fn stage(&self) -> String {
        self.stage.lock().map(|s| s.clone()).unwrap_or_default()
    }

    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }

    fn set(&self, fraction: f64, stage: &str) {
        self.permille.store((fraction.clamp(0.0, 1.0) * 1000.0) as u32, Ordering::Relaxed);
        if let Ok(mut s) = self.stage.lock() {
            stage.clone_into(&mut s);
        }
    }
}

/// A sliced plate.
pub struct Sliced {
    pub gcode: String,
    pub preview: Preview,
    pub stats: Stats,
    pub warnings: Vec<String>,
    /// Filament colours ("#RRGGBB") by extruder, for the Preview.
    pub colours: Vec<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum SliceError {
    #[error("there is nothing on the plate to slice")]
    Nothing,
    #[error("slicing was cancelled")]
    Cancelled,
    #[error("an object is {0:.1} mm tall; the printer reaches {1:.1} mm")]
    TooTall(f64, f64),
    #[error("an object lies outside the printable area")]
    Outside,
    #[error("the printer's custom G-code could not be prepared: {0}")]
    Gcode(String),
    #[error("the slicing engine hit a bug ({0}); nothing was printed or changed")]
    Bug(String),
}

/// Reads a G-code file for the Preview, timed with the printer settings of `printer` and the
/// filaments of `filaments`. Never panics.
pub fn view_gcode(text: String, printer: &Config, filaments: &[Config]) -> Sliced {
    let run = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let schema = Schema::get();
        let s = Settings::read(schema, printer);
        let fil: Vec<Filament> = filaments.iter().map(|f| Filament::read(schema, f, "textured_plate_temp")).collect();
        let v = gcodeview::read(&text, &s, &fil);
        let colours = filaments.iter().map(|f| f.get(schema, "filament_colour").to_owned()).collect();
        (v, colours)
    }));
    match run {
        Ok((v, colours)) => Sliced { gcode: text, preview: v.preview, stats: v.stats, warnings: v.warnings, colours },
        Err(_) => Sliced {
            gcode: text,
            preview: Preview::default(),
            stats: Stats::default(),
            warnings: vec!["The G-code could not be read.".into()],
            colours: Vec::new(),
        },
    }
}

/// Slices `job`, reporting to `progress`. Never panics.
pub fn slice(job: &Job, progress: &Progress) -> Result<Sliced, SliceError> {
    let run = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| slice_inner(job, progress)));
    match run {
        Ok(result) => result,
        Err(payload) => {
            let message = payload
                .downcast_ref::<&str>()
                .map(|s| (*s).to_owned())
                .or_else(|| payload.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "unknown error".into());
            Err(SliceError::Bug(message))
        }
    }
}

/// The merged settings custom G-code reads: printer and process values, and the filament
/// options as lists across the extruders, as OrcaSlicer passes them.
fn macro_table(schema: &Schema, base: &Config, filaments: &[Config]) -> Config {
    let mut table = base.clone();
    let mut keys: Vec<&String> = filaments.iter().flat_map(|f| f.values.keys()).collect();
    keys.sort();
    keys.dedup();
    for k in keys {
        let v: Vec<String> =
            filaments.iter().map(|f| f.get(schema, k).split(',').next().unwrap_or("").trim().to_owned()).collect();
        table.set(k.clone(), v.join(","));
    }
    table
}

fn slice_inner(job: &Job, progress: &Progress) -> Result<Sliced, SliceError> {
    let schema = Schema::get();
    let cancelled = || progress.is_cancelled();
    if job.objects.is_empty() {
        return Err(SliceError::Nothing);
    }
    progress.set(0.0, "Slicing mesh");
    let base = job.process.overlaid(&job.printer);
    let s = Settings::read(schema, &base);
    let filaments: Vec<Filament> = if job.filaments.is_empty() {
        vec![Filament::read(schema, &Config::default(), &job.bed_key)]
    } else {
        job.filaments.iter().map(|f| Filament::read(schema, f, &job.bed_key)).collect()
    };
    let colours = job.filaments.iter().map(|f| f.get(schema, "filament_colour").to_owned()).collect();
    let mut warnings = Vec::new();

    let placed: Vec<Placed> = job.objects.iter().map(|o| Placed::new(&o.mesh, &o.matrix)).collect();
    let top = placed.iter().map(Placed::top).fold(0.0, f64::max);
    if top <= 0.0 {
        return Err(SliceError::Nothing);
    }
    // A raft lifts the part by its layers and the contact gap; the part's first layer then
    // prints at the normal layer height.
    let raft_heights: Vec<f64> =
        (0..s.raft.layers).map(|k| if k == 0 { s.first_layer_height } else { s.layer_height }).collect();
    let lift = if raft_heights.is_empty() { 0.0 } else { raft_heights.iter().sum::<f64>() + s.raft.contact_distance };
    if top + lift > s.max_print_height + 0.01 {
        return Err(SliceError::TooTall(top + lift, s.max_print_height));
    }
    let first_height = if raft_heights.is_empty() { s.first_layer_height } else { s.layer_height };
    let layers = slicing::layer_heights(top, first_height, s.layer_height);
    let n = layers.len();
    let objects = placed.len().max(1) as f64;

    // Each object: slices, then walls and fills.
    let mut object_slices: Vec<Vec<Vec<Shape>>> = Vec::new();
    let mut object_plans: Vec<Vec<Vec<toolpath::Island>>> = Vec::new();
    let mut object_settings: Vec<Settings> = Vec::new();
    let mut object_marks: Vec<Marks> = Vec::new();
    for (oi, (p, o)) in placed.iter().zip(&job.objects).enumerate() {
        progress.set(0.05 + 0.25 * oi as f64 / objects, "Slicing mesh");
        let slices = slicing::slice(p, &layers, &cancelled);
        if cancelled() {
            return Err(SliceError::Cancelled);
        }
        progress.set(0.30 + 0.40 * oi as f64 / objects, "Generating walls and infill");
        let mut cfg = base.clone();
        for (k, v) in &o.overrides {
            cfg.set(k.clone(), v.clone());
        }
        let os = Settings::read(schema, &cfg);
        let mut slices = slices;
        if let Some(angle) = os.printable_overhang {
            let heights: Vec<f64> = layers.iter().map(|l| l.height).collect();
            overhang::make_printable(&mut slices, &heights, angle);
        }
        let islands = plan::object(&slices, &layers, &os, &cancelled);
        if cancelled() {
            return Err(SliceError::Cancelled);
        }
        if slices.first().is_none_or(Vec::is_empty)
            && p.vertices.iter().map(|v| v.z).fold(f64::INFINITY, f64::min) < s.first_layer_height
        {
            warnings.push(format!("\"{}\" has nothing on the first layer; check it rests on the plate", o.name));
        }
        let marks = support_marks(o, p, &layers, &cancelled);
        let enforced = marks.0.iter().any(|l| !l.is_empty());
        if enforced && !os.support.enabled {
            warnings.push(format!("\"{}\" has support enforcers, but support is turned off", o.name));
        }
        object_slices.push(slices);
        object_plans.push(islands);
        object_settings.push(os);
        object_marks.push(marks);
    }

    // Everything printed on each layer, for supports.
    progress.set(0.72, "Generating support");
    let all: Vec<Vec<Shape>> = (0..n)
        .map(|i| {
            let parts: Vec<Shape> = object_slices.iter().filter_map(|o| o.get(i)).flatten().cloned().collect();
            if object_slices.len() > 1 { geom::union(&parts) } else { parts }
        })
        .collect();
    let heights: Vec<f64> = layers.iter().map(|l| l.height).collect();
    let inputs: Vec<support::Input<'_>> = object_slices
        .iter()
        .zip(&object_settings)
        .zip(&object_marks)
        .map(|((slices, os), (enforce, block))| support::Input { slices, settings: os, enforce, block })
        .collect();
    let support = support::generate(&inputs, &all, &heights, &s, &cancelled);
    if cancelled() {
        return Err(SliceError::Cancelled);
    }

    progress.set(0.80, "Generating skirt & brim");
    let part_first: Vec<Shape> =
        all.first().cloned().unwrap_or_default().into_iter().chain(support.footprint(0)).collect();
    let raft = support::raft(&part_first, &s);
    // What sits on the plate: the raft's first layer, or the parts and their support.
    let footprint = match raft.first() {
        Some(r) => r.area.clone(),
        None => all.first().cloned().unwrap_or_default(),
    };
    let (brim, brim_area) = if raft.is_empty() {
        let objects: Vec<skirt::BrimObject<'_>> = object_slices
            .iter()
            .zip(&object_settings)
            .zip(&placed)
            .map(|((sl, os), p)| skirt::BrimObject {
                footprint: sl.first().map_or(&[][..], Vec::as_slice),
                settings: os,
                height: p.top(),
            })
            .collect();
        skirt::brims(&objects)
    } else {
        skirt::brim(&footprint, &s, top)
    };
    let skirt_around = geom::boolean(&footprint, &brim_area, Op::Union);
    let skirt_around =
        if raft.is_empty() { geom::boolean(&skirt_around, &support.footprint(0), Op::Union) } else { skirt_around };

    let skirt_for = |g: usize, height: f64| -> Vec<Path> {
        let first = g == 0;
        let mut skirt: Vec<Path> = Vec::new();
        if g < s.skirt_height {
            let w = if first { s.widths.first_layer } else { s.widths.outer };
            skirt = skirt::skirt(&skirt_around, &s, w, height);
        }
        if first {
            skirt.extend(brim.iter().cloned());
        }
        skirt
    };
    let mut raft_z = 0.0;
    let mut plans: Vec<LayerPlan> = raft
        .iter()
        .enumerate()
        .map(|(k, r)| {
            raft_z += r.height;
            LayerPlan {
                print_z: raft_z,
                height: r.height,
                skirt: skirt_for(k, r.height),
                support: support::raft_paths(r, k, &s, raft_z),
                objects: Vec::new(),
            }
        })
        .collect();
    let below = plans.len();
    plans.extend(layers.iter().enumerate().map(|(i, l)| {
        let g = below + i;
        let first = g == 0;
        LayerPlan {
            print_z: l.print_z + lift,
            height: l.height,
            skirt: skirt_for(g, l.height),
            support: support::paths(&support, i, &s, first, l.print_z + lift),
            objects: object_plans
                .iter_mut()
                .enumerate()
                .filter_map(|(o, p)| p.get_mut(i).map(|isl| (o, std::mem::take(isl))))
                .filter(|(_, isl)| !isl.is_empty())
                .collect(),
        }
    }));
    if cancelled() {
        return Err(SliceError::Cancelled);
    }

    progress.set(0.88, "Generating G-code");
    let (min, max) = geom::bounds(&footprint).map_or(([0.0, 0.0], [0.0, 0.0]), |(lo, hi)| ([lo.x, lo.y], [hi.x, hi.y]));
    let table = macro_table(schema, &base, &job.filaments);
    let (lo, size) = {
        let pts = CosmicSlicer_config::parse_points(base.get(schema, "printable_area"));
        let (mut lo, mut hi) = ([f64::INFINITY; 2], [f64::NEG_INFINITY; 2]);
        for [x, y] in &pts {
            lo = [lo[0].min(*x), lo[1].min(*y)];
            hi = [hi[0].max(*x), hi[1].max(*y)];
        }
        if lo[0].is_finite() { (lo, [hi[0] - lo[0], hi[1] - lo[1]]) } else { ([0.0, 0.0], [220.0, 220.0]) }
    };
    let used_extruders: Vec<usize> = {
        let mut u: Vec<usize> = job.objects.iter().map(|o| o.extruder.min(filaments.len().saturating_sub(1))).collect();
        u.sort_unstable();
        u.dedup();
        u
    };
    let facts = macros::PlateFacts {
        plate_name: job.plate_name.clone(),
        bed_type_name: job.bed_type_name.clone(),
        first_layer_min: min,
        first_layer_max: max,
        bed_min: lo,
        bed_max: [lo[0] + size[0], lo[1] + size[1]],
        layer_count: plans.len(),
        max_z: top + lift,
        initial_extruder: used_extruders.first().copied().unwrap_or(0),
        used_extruders,
    };
    let macros = macros::Macros::new(schema, &table, &s, &filaments, &facts);
    let names: Vec<String> = job.objects.iter().map(|o| o.name.clone()).collect();
    let extruders: Vec<usize> = job.objects.iter().map(|o| o.extruder.min(filaments.len().saturating_sub(1))).collect();
    // Each object's footprint, for the firmware's object labels (Klipper's exclude-object).
    let outlines: Vec<Vec<glam::DVec2>> = object_slices
        .iter()
        .map(|layers| {
            let step = (layers.len() / 50).max(1);
            let pts: Vec<glam::DVec2> =
                layers.iter().step_by(step).flatten().filter_map(|sh| sh.first()).flatten().copied().collect();
            geom::convex_hull(&pts)
        })
        .collect();
    let thumbnails = {
        let specs = thumbnail::parse(&s.motion.thumbnails, base.get(schema, "thumbnails_format"), &mut warnings);
        let colour = |e: usize| -> [u8; 3] {
            let hex = filaments.get(e).map_or("", |f| f.colour.as_str()).trim_start_matches('#');
            let v = hex.get(..6).and_then(|h| u32::from_str_radix(h, 16).ok()).unwrap_or(0xF2_75_4E);
            [(v >> 16) as u8, (v >> 8) as u8, v as u8]
        };
        let items: Vec<thumbnail::Item<'_>> = job
            .objects
            .iter()
            .map(|o| thumbnail::Item { mesh: &o.mesh, matrix: o.matrix, colour: colour(o.extruder) })
            .collect();
        thumbnail::blocks(&items, &specs)
    };
    let cx = gcode::Context {
        settings: &s,
        filaments: &filaments,
        object_extruder: &extruders,
        object_names: &names,
        object_outlines: &outlines,
        macros: &macros,
        plate_name: job.plate_name.clone(),
        thumbnails,
    };
    let out = gcode::emit(&plans, &cx);
    if let Some(e) = out.error {
        return Err(SliceError::Gcode(e));
    }
    warnings.extend(out.warnings);
    if extruders.iter().any(|e| *e != extruders.first().copied().unwrap_or(0)) {
        warnings.push("Several filaments print without a prime tower for now; expect some colour bleed.".into());
    }
    progress.set(1.0, "Slicing complete");
    Ok(Sliced { gcode: out.gcode, preview: out.preview, stats: out.stats, warnings, colours })
}

/// Support enforcers and blockers per layer.
type Marks = (Vec<Vec<Shape>>, Vec<Vec<Shape>>);

/// One object's support enforcers and blockers per layer, from its volumes and painted facets.
fn support_marks(o: &JobObject, placed: &Placed, layers: &[LayerZ], cancelled: &(dyn Fn() -> bool + Sync)) -> Marks {
    let n = layers.len();
    let (mut enforce, mut block) = (vec![Vec::new(); n], vec![Vec::new(); n]);
    for v in &o.support_volumes {
        let slices = slicing::slice(&Placed::new(&v.mesh, &v.matrix), layers, cancelled);
        let dst = if v.kind == SupportPaint::Enforce { &mut enforce } else { &mut block };
        for (d, sh) in dst.iter_mut().zip(slices) {
            d.extend(sh);
        }
    }
    for (kind, dst) in [(SupportPaint::Enforce, &mut enforce), (SupportPaint::Block, &mut block)] {
        let tris: Vec<u32> = o.support_paint.iter().filter(|(_, k)| *k == kind).map(|(t, _)| *t).collect();
        if !tris.is_empty() {
            for (d, sh) in dst.iter_mut().zip(slicing::painted(placed, &tris, layers)) {
                d.extend(sh);
            }
        }
    }
    if o.support_volumes.len() + o.support_paint.len() > 1 {
        for l in enforce.iter_mut().chain(block.iter_mut()).filter(|l| l.len() > 1) {
            *l = geom::union(l);
        }
    }
    (enforce, block)
}

#[cfg(test)]
mod tests {
    use super::*;
    use CosmicSlicer_model::primitives;
    use glam::Vec3;

    fn job(objects: Vec<JobObject>) -> Job {
        let mut printer = Config::default();
        printer.set("printable_area", "0x0,220x0,220x220,0x220");
        let mut filament = Config::default();
        filament.set("filament_density", "1.24");
        Job {
            printer,
            process: Config::default(),
            filaments: vec![filament],
            bed_key: "textured_plate_temp".into(),
            bed_type_name: "Textured PEI Plate".into(),
            objects,
            plate_name: "Plate 1".into(),
        }
    }

    fn cube(at: Vec3, size: f32) -> JobObject {
        JobObject {
            name: "cube".into(),
            mesh: Arc::new(primitives::cube(size)),
            matrix: Mat4::from_translation(at),
            extruder: 0,
            overrides: Vec::new(),
            support_volumes: Vec::new(),
            support_paint: Vec::new(),
        }
    }

    #[test]
    fn a_cube_becomes_gcode() {
        let progress = Progress::default();
        let out = slice(&job(vec![cube(Vec3::new(100.0, 100.0, 0.0), 20.0)]), &progress);
        assert!(out.is_ok(), "{:?}", out.as_ref().err());
        let Ok(out) = out else { return };
        if let Ok(path) = std::env::var("CUBE_GCODE") {
            let _ = std::fs::write(path, &out.gcode);
        }
        // Reading our own G-code back gives the same picture and estimate.
        let back = view_gcode(out.gcode.clone(), &Config::default(), &[Config::default()]);
        assert_eq!(back.preview.layers.len(), out.preview.layers.len());
        let (a, b) = (back.stats.total_time, out.stats.total_time);
        assert!((a - b).abs() / b < 0.02, "read back {a} vs sliced {b}");
        assert!((back.stats.filament_mm[0] - out.stats.filament_mm[0]).abs() / out.stats.filament_mm[0] < 0.01);
        assert_eq!(out.stats.layers, 100);
        assert_eq!(out.preview.layers.len(), 100);
        assert!(out.gcode.contains(";TYPE:Outer wall"));
        assert!(out.gcode.contains(";TYPE:Sparse infill"));
        assert!(out.gcode.contains("M109 S"));
        // Extruded volume is roughly the cube's (8000 mm³ at 20% infill: shells plus some).
        let v = out.stats.volume_mm3[0];
        assert!(v > 2000.0 && v < 8000.0, "{v}");
        let t = out.stats.total_time;
        assert!(t > 300.0 && t < 10_000.0, "{t}");
        // Every G1 stays on the cube (100..120) or its skirt.
        for line in out.gcode.lines().filter(|l| l.starts_with("G1 X")) {
            let x: f64 =
                line.split_whitespace().find_map(|w| w.strip_prefix('X')).and_then(|v| v.parse().ok()).unwrap_or(0.0);
            assert!((95.0..=125.0).contains(&x), "{line}");
        }
        assert!((progress.fraction() - 1.0).abs() < 1e-6);
    }

    #[test]
    fn solid_infill_uses_about_the_cube_volume() {
        let mut j = job(vec![cube(Vec3::new(100.0, 100.0, 0.0), 10.0)]);
        j.process.set("sparse_infill_density", "100%");
        j.printer.set("skirt_loops", "0");
        j.process.set("skirt_loops", "0");
        let out = slice(&j, &Progress::default());
        assert!(out.is_ok(), "{:?}", out.as_ref().err());
        let Ok(out) = out else { return };
        let v = out.stats.volume_mm3[0];
        assert!((v - 1000.0).abs() / 1000.0 < 0.12, "{v}");
    }

    #[test]
    fn supports_appear_under_overhangs() {
        // A T: a bar on a post.
        let mut j = job(vec![cube(Vec3::new(100.0, 100.0, 0.0), 6.0), cube(Vec3::new(100.0, 100.0, 6.0), 20.0)]);
        j.process.set("enable_support", "1");
        let out = slice(&j, &Progress::default());
        assert!(out.is_ok(), "{:?}", out.as_ref().err());
        let Ok(out) = out else { return };
        assert!(out.gcode.contains(";TYPE:Support"));
        assert!(out.gcode.contains(";TYPE:Bridge") || out.gcode.contains(";TYPE:Support interface"));
    }

    fn object(name: &str, mesh: Mesh, at: Vec3) -> JobObject {
        JobObject { name: name.into(), mesh: Arc::new(mesh), matrix: Mat4::from_translation(at), ..cube(at, 1.0) }
    }

    /// Slices, checks the G-code reads back with the same time, and returns it.
    fn sliced(j: &Job) -> Option<Sliced> {
        let out = slice(j, &Progress::default());
        assert!(out.is_ok(), "{:?}", out.as_ref().err());
        let out = out.ok()?;
        let back = view_gcode(out.gcode.clone(), &Config::default(), &[Config::default()]);
        let (a, b) = (back.stats.total_time, out.stats.total_time);
        assert!((a - b).abs() / b < 0.02, "read back {a} vs sliced {b}");
        Some(out)
    }

    #[test]
    fn orcas_support_test_shapes_get_every_style() {
        // An arch (two posts and a lintel), a T, and a figure (a ball on a stick).
        let arch = vec![
            cube(Vec3::new(60.0, 100.0, 0.0), 4.0),
            cube(Vec3::new(76.0, 100.0, 0.0), 4.0),
            object("lintel", primitives::cube(20.0), Vec3::new(60.0, 100.0, 4.0)),
        ];
        let t = vec![cube(Vec3::new(107.0, 100.0, 0.0), 6.0), cube(Vec3::new(100.0, 93.0, 6.0), 20.0)];
        let figure = vec![
            object("stick", primitives::cylinder(1.5, 10.0, 32), Vec3::new(160.0, 110.0, 0.0)),
            object("ball", primitives::sphere(10.0, 64), Vec3::new(160.0, 110.0, 9.5)),
        ];
        for (kind, style) in [
            ("normal(auto)", "grid"),
            ("normal(auto)", "snug"),
            ("tree(auto)", "organic"),
            ("tree(auto)", "tree_hybrid"),
        ] {
            for (name, objects) in [("arch", &arch), ("T", &t), ("figure", &figure)] {
                let mut j = job(objects.clone());
                j.process.set("enable_support", "1");
                j.process.set("support_type", kind);
                j.process.set("support_style", style);
                let Some(out) = sliced(&j) else { return };
                if std::env::var("DEBUG_SUPPORT").is_ok() {
                    let mut types: Vec<&str> = out.gcode.lines().filter_map(|l| l.strip_prefix(";TYPE:")).collect();
                    types.sort_unstable();
                    types.dedup();
                    eprintln!("{name} {style}: {types:?} {:?} layers {}", out.warnings, out.stats.layers);
                }
                assert!(out.gcode.contains(";TYPE:Support"), "{name} {style}");
                assert!(out.gcode.contains(";TYPE:Support interface"), "{name} {style}");
                assert!(!out.warnings.iter().any(|w| w.contains("support")), "{:?}", out.warnings);
            }
        }
    }

    #[test]
    fn a_raft_lifts_the_part() {
        let mut j = job(vec![cube(Vec3::new(100.0, 100.0, 0.0), 10.0)]);
        j.process.set("raft_layers", "3");
        j.process.set("raft_contact_distance", "0.1");
        let Some(out) = sliced(&j) else { return };
        assert_eq!(out.preview.layers.len(), 53);
        let z: Vec<f32> = out.preview.layers.iter().map(|l| l.z).collect();
        // Raft 0.2 + 0.2 + 0.2, the gap, then the part at 0.2 a layer.
        assert!((z[2] - 0.6).abs() < 1e-3 && (z[3] - 0.9).abs() < 1e-3, "{:?}", &z[..5]);
        assert!((z[52] - 10.7).abs() < 1e-3);
        assert!(out.gcode.contains(";TYPE:Support interface"));
        assert!(!out.gcode.contains(";TYPE:Brim"));
    }

    #[test]
    fn enforcers_blockers_and_paint() {
        let t = || vec![cube(Vec3::new(100.0, 100.0, 0.0), 6.0), cube(Vec3::new(93.0, 93.0, 6.0), 20.0)];
        let manual = |objects: Vec<JobObject>| {
            let mut j = job(objects);
            j.process.set("enable_support", "1");
            j.process.set("support_type", "normal(manual)");
            j
        };
        let Some(out) = sliced(&manual(t())) else { return };
        assert!(!out.gcode.contains(";TYPE:Support"));
        // An enforcer box under one side of the slab.
        let mut objects = t();
        if let Some(slab) = objects.get_mut(1) {
            slab.support_volumes.push(SupportVolume {
                kind: SupportPaint::Enforce,
                mesh: Arc::new(primitives::cube(8.0)),
                matrix: Mat4::from_translation(Vec3::new(92.0, 92.0, 2.0)),
            });
        }
        let Some(out) = sliced(&manual(objects)) else { return };
        assert!(out.gcode.contains(";TYPE:Support"));
        // The slab's underside painted.
        let mut objects = t();
        if let Some(slab) = objects.get_mut(1) {
            let m = primitives::cube(20.0);
            slab.support_paint = m
                .triangles
                .iter()
                .enumerate()
                .filter(|(_, t)| t.iter().all(|v| m.vertices.get(*v as usize).is_some_and(|p| p.z == 0.0)))
                .map(|(i, _)| (i as u32, SupportPaint::Enforce))
                .collect();
            assert_eq!(slab.support_paint.len(), 2);
        }
        let Some(painted) = sliced(&manual(objects)) else { return };
        assert!(painted.gcode.contains(";TYPE:Support"));
        // Blocked everywhere: nothing, even with auto support.
        let mut objects = t();
        if let Some(slab) = objects.get_mut(1) {
            slab.support_volumes.push(SupportVolume {
                kind: SupportPaint::Block,
                mesh: Arc::new(primitives::cube(40.0)),
                matrix: Mat4::from_translation(Vec3::new(80.0, 80.0, 0.0)),
            });
        }
        let mut j = job(objects);
        j.process.set("enable_support", "1");
        let Some(out) = sliced(&j) else { return };
        assert!(!out.gcode.contains(";TYPE:Support"));
    }

    #[test]
    fn each_object_gets_its_own_brim() {
        let mut a = cube(Vec3::new(60.0, 100.0, 0.0), 10.0);
        a.overrides.push(("brim_type".into(), "no_brim".into()));
        let b = cube(Vec3::new(120.0, 100.0, 0.0), 10.0);
        let mut j = job(vec![a, b]);
        j.process.set("brim_type", "outer_only");
        j.process.set("brim_width", "5");
        let Some(out) = sliced(&j) else { return };
        // Brim moves only round the second cube.
        let mut in_brim = false;
        let mut xs = Vec::new();
        for line in out.gcode.lines() {
            if let Some(t) = line.strip_prefix(";TYPE:") {
                in_brim = t == "Brim";
            } else if in_brim
                && line.starts_with("G1 X")
                && line.contains(" E")
                && let Some(x) =
                    line.split_whitespace().find_map(|w| w.strip_prefix('X')).and_then(|v| v.parse::<f64>().ok())
            {
                xs.push(x);
            }
        }
        assert!(!xs.is_empty());
        assert!(xs.iter().all(|x| (114.0..=136.0).contains(x)), "{xs:?}");
    }

    #[test]
    fn overhangs_get_their_speed_type_and_fan() {
        // A slab on a post, as one object: the slab's first layer hangs over air.
        let mut j = job(vec![cube(Vec3::new(100.0, 100.0, 0.0), 6.0), cube(Vec3::new(100.0, 100.0, 6.0), 20.0)]);
        j.filaments[0].set("overhang_fan_speed", "77");
        j.filaments[0].set("fan_max_speed", "40");
        j.filaments[0].set("fan_min_speed", "40");
        let out = slice(&j, &Progress::default());
        assert!(out.is_ok(), "{:?}", out.as_ref().err());
        let Ok(out) = out else { return };
        assert!(out.gcode.contains(";TYPE:Overhang wall"));
        // 77 % is S196; the layer fan comes back after.
        if let Ok(p) = std::env::var("OVERHANG_GCODE") {
            let _ = std::fs::write(p, &out.gcode);
        }
        let at = out.gcode.find("M106 S196");
        assert!(at.is_some());
        assert!(out.gcode[at.unwrap_or(0)..].contains("M106 S102"));
        // Bridge speed (25 mm/s) on the overhanging walls.
        let overhang = out.gcode.split(";TYPE:").find(|b| b.starts_with("Overhang wall")).unwrap_or("");
        assert!(overhang.lines().any(|l| l.starts_with("G1") && l.ends_with("F1500")), "{overhang:.400}");
    }

    #[test]
    fn scarf_seams_ramp_up_and_read_back() {
        let mut j = job(vec![cube(Vec3::new(100.0, 100.0, 0.0), 10.0)]);
        j.process.set("seam_slope_type", "all");
        j.process.set("seam_slope_steps", "4");
        let out = slice(&j, &Progress::default());
        assert!(out.is_ok(), "{:?}", out.as_ref().err());
        let Ok(out) = out else { return };
        if let Ok(p) = std::env::var("SCARF_GCODE") {
            let _ = std::fs::write(p, &out.gcode);
        }
        // Layer 10 is at 2.0 mm: its outer wall goes down to 1.825 and extrudes up the ramp.
        assert!(out.gcode.contains("G1 Z1.825"));
        assert!(out.gcode.lines().any(|l| l.starts_with("G1") && l.contains(" Z1.875") && l.contains(" E")), "no ramp");
        let back = view_gcode(out.gcode.clone(), &Config::default(), &[Config::default()]);
        assert_eq!(back.preview.layers.len(), out.preview.layers.len());
        let (a, b) = (back.stats.filament_mm[0], out.stats.filament_mm[0]);
        assert!((a - b).abs() / b < 0.01, "{a} vs {b}");
    }

    #[test]
    fn wipes_follow_the_settings() {
        let wipes = |role_based: &str| {
            let mut j = job(vec![torus(Vec3::new(100.0, 100.0, 0.0))]);
            j.printer.set("wipe", "1");
            j.printer.set("travel_speed", "200");
            j.process.set("travel_speed", "200");
            j.process.set("wipe_speed", "50%");
            j.process.set("role_based_wipe_speed", role_based);
            j.process.set("wipe_on_loops", "1");
            let out = slice(&j, &Progress::default());
            assert!(out.is_ok(), "{:?}", out.as_ref().err());
            let gcode = out.map(|o| o.gcode).unwrap_or_default();
            let speeds: Vec<String> = gcode
                .lines()
                .filter(|l| l.starts_with("G1 X") && l.contains(" E-"))
                .filter_map(|l| l.split_whitespace().find(|w| w.starts_with('F')).map(str::to_owned))
                .collect();
            speeds
        };
        // Half of 200 mm/s: F6000.
        let fixed = wipes("0");
        assert!(!fixed.is_empty() && fixed.iter().all(|f| f == "F6000"), "{fixed:?}");
        // Role-based: the speeds of what was printed, not the travel's.
        let by_role = wipes("1");
        assert!(!by_role.is_empty() && by_role.iter().any(|f| f != "F6000"), "{by_role:?}");
    }

    #[test]
    fn errors_not_panics() {
        assert!(matches!(slice(&job(vec![]), &Progress::default()), Err(SliceError::Nothing)));
        let tall = cube(Vec3::new(100.0, 100.0, 0.0), 400.0);
        assert!(matches!(slice(&job(vec![tall]), &Progress::default()), Err(SliceError::TooTall(..))));
        let p = Progress::default();
        p.cancel();
        assert!(matches!(slice(&job(vec![cube(Vec3::new(100.0, 100.0, 0.0), 20.0)]), &p), Err(SliceError::Cancelled)));
    }

    fn torus(at: Vec3) -> JobObject {
        JobObject {
            name: "torus".into(),
            mesh: Arc::new(primitives::torus(15.0, 5.0, 96, 32)),
            matrix: Mat4::from_translation(at),
            extruder: 0,
            overrides: Vec::new(),
            support_volumes: Vec::new(),
            support_paint: Vec::new(),
        }
    }

    #[test]
    fn arcs_read_back_the_same() {
        let mut j = job(vec![torus(Vec3::new(100.0, 100.0, 0.0))]);
        j.process.set("enable_arc_fitting", "enabled");
        let out = slice(&j, &Progress::default());
        assert!(out.is_ok(), "{:?}", out.as_ref().err());
        let Ok(out) = out else { return };
        let arcs = out.gcode.lines().filter(|l| l.starts_with("G2 ") || l.starts_with("G3 ")).count();
        assert!(arcs > 100, "{arcs}");
        let back = view_gcode(out.gcode.clone(), &Config::default(), &[Config::default()]);
        let (a, b) = (back.stats.filament_mm[0], out.stats.filament_mm[0]);
        assert!((a - b).abs() / b < 0.01, "{a} vs {b}");
        let (a, b) = (back.stats.total_time, out.stats.total_time);
        assert!((a - b).abs() / b < 0.05, "{a} vs {b}");
    }

    #[test]
    fn avoiding_walls_saves_retractions() {
        let count = |avoid: bool| {
            let mut j = job(vec![torus(Vec3::new(100.0, 100.0, 0.0))]);
            j.process.set("reduce_crossing_wall", if avoid { "1" } else { "0" });
            j.process.set("reduce_infill_retraction", "1");
            slice(&j, &Progress::default())
                .map(|o| o.gcode.lines().filter(|l| l.starts_with("G1 E-")).count())
                .unwrap_or(0)
        };
        let (plain, avoided) = (count(false), count(true));
        assert!(avoided < plain, "{avoided} retractions avoiding walls vs {plain}");
    }

    #[test]
    fn klipper_gets_its_own_commands() {
        let mut j = job(vec![cube(Vec3::new(100.0, 100.0, 0.0), 10.0)]);
        j.printer.set("gcode_flavor", "klipper");
        j.process.set("exclude_object", "1");
        j.filaments[0].set("enable_pressure_advance", "1");
        j.filaments[0].set("pressure_advance", "0.04");
        let out = slice(&j, &Progress::default());
        assert!(out.is_ok(), "{:?}", out.as_ref().err());
        let Ok(out) = out else { return };
        for expect in [
            "SET_VELOCITY_LIMIT ACCEL=",
            "EXCLUDE_OBJECT_DEFINE NAME=cube_id_0_copy_0",
            "EXCLUDE_OBJECT_START",
            "SET_PRESSURE_ADVANCE ADVANCE=0.04",
            "; thumbnail begin 300x300",
        ] {
            assert!(out.gcode.contains(expect), "missing {expect}");
        }
        assert!(!out.gcode.contains("M204"));
    }

    /// `SLICE_FILE=model.stl SLICE_OUT=out.gcode cargo test --release -p CosmicSlicer-engine
    /// slice_a_file -- --nocapture` slices a model file centred on a 220 mm bed.
    #[test]
    fn slice_a_file() {
        let Ok(path) = std::env::var("SLICE_FILE") else { return };
        let file = CosmicSlicer_model::load(std::path::Path::new(&path));
        assert!(file.is_ok(), "{:?}", file.as_ref().err());
        let Some(mesh) = file.ok().and_then(|f| f.objects.into_iter().next()) else { return };
        let mesh = mesh.mesh.centred();
        let mut j = job(vec![JobObject {
            name: path.clone(),
            mesh: Arc::new(mesh),
            matrix: Mat4::from_translation(Vec3::new(110.0, 110.0, 0.0)),
            extruder: 0,
            overrides: Vec::new(),
            support_volumes: Vec::new(),
            support_paint: Vec::new(),
        }]);
        for (k, v) in std::env::var("SLICE_SET").unwrap_or_default().split(';').filter_map(|kv| kv.split_once('=')) {
            // Printer keys (printable_height, …) as well as process ones.
            j.printer.set(k.trim(), v.trim());
            j.process.set(k.trim(), v.trim());
        }
        let started = std::time::Instant::now();
        let out = slice(&j, &Progress::default());
        assert!(out.is_ok(), "{:?}", out.as_ref().err());
        let Ok(out) = out else { return };
        println!(
            "sliced in {:.2}s: {} layers, {} segments, {:.0} s print, {:.1} m filament",
            started.elapsed().as_secs_f64(),
            out.stats.layers,
            out.preview.segments.len(),
            out.stats.total_time,
            out.stats.filament_mm.iter().sum::<f64>() / 1000.0
        );
        let back = view_gcode(out.gcode.clone(), &j.printer, &j.filaments);
        println!(
            "read back: {} layers, {:.0} s print, {:.1} m filament",
            back.preview.layers.len(),
            back.stats.total_time,
            back.stats.filament_mm.iter().sum::<f64>() / 1000.0
        );
        if let Ok(o) = std::env::var("SLICE_OUT") {
            let _ = std::fs::write(o, &out.gcode);
        }
    }

    #[test]
    fn every_pattern_slices() {
        for pattern in [
            "rectilinear",
            "grid",
            "triangles",
            "cubic",
            "gyroid",
            "concentric",
            "lightning",
            "honeycomb",
            "crosshatch",
        ] {
            let mut j = job(vec![cube(Vec3::new(100.0, 100.0, 0.0), 15.0)]);
            j.process.set("sparse_infill_pattern", pattern);
            let out = slice(&j, &Progress::default());
            assert!(out.is_ok_and(|o| o.gcode.contains(";TYPE:Sparse infill")), "{pattern}");
        }
    }
}
