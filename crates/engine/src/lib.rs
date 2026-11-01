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

pub mod gcode;
pub mod geom;
pub mod infill;
pub mod plan;
pub mod settings;
pub mod skirt;
pub mod slicing;
pub mod support;
pub mod toolpath;
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
use slicing::Placed;
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
    #[error("the slicing engine hit a bug ({0}); nothing was printed or changed")]
    Bug(String),
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

/// The values custom G-code can ask for.
fn lookup_table(
    schema: &Schema,
    base: &Config,
    filaments: &[Config],
    fil: &[Filament],
    extra: Vec<(&str, String)>,
) -> Config {
    let mut table = base.clone();
    // Filament options as lists across the extruders, as OrcaSlicer passes them.
    let mut keys: Vec<&String> = filaments.iter().flat_map(|f| f.values.keys()).collect();
    keys.sort();
    keys.dedup();
    for k in keys {
        let v: Vec<String> =
            filaments.iter().map(|f| f.get(schema, k).split(',').next().unwrap_or("").trim().to_owned()).collect();
        table.set(k.clone(), v.join(","));
    }
    let join = |f: &dyn Fn(&Filament) -> f64| {
        fil.iter().map(|x| format!("{}", f(x).round() as i64)).collect::<Vec<_>>().join(",")
    };
    let bed_first = fil.iter().map(|f| f.bed_temp_first).fold(0.0, f64::max);
    let bed = fil.iter().map(|f| f.bed_temp).fold(0.0, f64::max);
    for (k, v) in [
        ("nozzle_temperature_initial_layer", join(&|f| f.nozzle_temp_first)),
        ("first_layer_temperature", join(&|f| f.nozzle_temp_first)),
        ("nozzle_temperature", join(&|f| f.nozzle_temp)),
        ("temperature", join(&|f| f.nozzle_temp)),
        ("bed_temperature_initial_layer_single", format!("{}", bed_first.round() as i64)),
        ("bed_temperature_initial_layer", format!("{}", bed_first.round() as i64)),
        ("first_layer_bed_temperature", format!("{}", bed_first.round() as i64)),
        ("bed_temperature", format!("{}", bed.round() as i64)),
        ("initial_extruder", "0".into()),
        ("initial_tool", "0".into()),
        ("current_extruder", "0".into()),
        ("filament_extruder_id", "0".into()),
    ] {
        table.set(k, v);
    }
    for (k, v) in extra {
        table.set(k, v);
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
    if top > s.max_print_height + 0.01 {
        return Err(SliceError::TooTall(top, s.max_print_height));
    }
    let layers = slicing::layer_heights(top, s.first_layer_height, s.layer_height);
    let n = layers.len();
    let objects = placed.len().max(1) as f64;

    // Each object: slices, then walls and fills.
    let mut object_slices: Vec<Vec<Vec<Shape>>> = Vec::new();
    let mut object_plans: Vec<Vec<Vec<toolpath::Island>>> = Vec::new();
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
        let islands = plan::object(&slices, &layers, &os, &cancelled);
        if cancelled() {
            return Err(SliceError::Cancelled);
        }
        if slices.first().is_none_or(Vec::is_empty)
            && p.vertices.iter().map(|v| v.z).fold(f64::INFINITY, f64::min) < s.first_layer_height
        {
            warnings.push(format!("\"{}\" has nothing on the first layer; check it rests on the plate", o.name));
        }
        object_slices.push(slices);
        object_plans.push(islands);
    }

    // Everything printed on each layer, for supports.
    progress.set(0.72, "Generating support");
    let all: Vec<Vec<Shape>> = (0..n)
        .map(|i| {
            let parts: Vec<Shape> = object_slices.iter().filter_map(|o| o.get(i)).flatten().cloned().collect();
            if object_slices.len() > 1 { geom::union(&parts) } else { parts }
        })
        .collect();
    let support = support::generate(&all, &s, &cancelled);
    if s.support.enabled && s.support.tree {
        warnings.push("Tree supports are printed as normal supports for now.".into());
    }
    if cancelled() {
        return Err(SliceError::Cancelled);
    }

    progress.set(0.80, "Generating skirt & brim");
    let footprint = all.first().cloned().unwrap_or_default();
    let (brim, brim_area) = skirt::brim(&footprint, &s, top);
    let skirt_around = geom::boolean(&footprint, &brim_area, Op::Union);
    let support_first: Vec<Shape> = support.base.first().cloned().unwrap_or_default();
    let skirt_around = geom::boolean(&skirt_around, &support_first, Op::Union);

    let plans: Vec<LayerPlan> = layers
        .iter()
        .enumerate()
        .map(|(i, l)| {
            let first = i == 0;
            let mut skirt: Vec<Path> = Vec::new();
            if i < s.skirt_height {
                let w = if first { s.widths.first_layer } else { s.widths.outer };
                skirt = skirt::skirt(&skirt_around, &s, w, l.height);
            }
            if first {
                skirt.extend(brim.iter().cloned());
            }
            LayerPlan {
                print_z: l.print_z,
                height: l.height,
                skirt,
                support: support::paths(&support, i, &s, first, l.print_z),
                objects: object_plans
                    .iter_mut()
                    .enumerate()
                    .filter_map(|(o, p)| p.get_mut(i).map(|isl| (o, std::mem::take(isl))))
                    .filter(|(_, isl)| !isl.is_empty())
                    .collect(),
            }
        })
        .collect();
    if cancelled() {
        return Err(SliceError::Cancelled);
    }

    progress.set(0.88, "Generating G-code");
    let (min, max) = geom::bounds(&footprint).map_or(([0.0, 0.0], [0.0, 0.0]), |(lo, hi)| ([lo.x, lo.y], [hi.x, hi.y]));
    let table = lookup_table(
        schema,
        &base,
        &job.filaments,
        &filaments,
        vec![
            ("total_layer_count", n.to_string()),
            ("first_layer_print_min", format!("{:.3},{:.3}", min[0], min[1])),
            ("first_layer_print_max", format!("{:.3},{:.3}", max[0], max[1])),
            ("print_bed_min", format!("{:.3},{:.3}", min[0], min[1])),
            ("print_bed_max", format!("{:.3},{:.3}", max[0], max[1])),
            ("first_layer_height", format!("{}", s.first_layer_height)),
            ("max_layer_z", format!("{:.3}", top)),
            ("plate_name", job.plate_name.clone()),
        ],
    );
    let lookup = |key: &str| -> Option<String> {
        table.values.get(key).cloned().or_else(|| schema.option(key).map(|_| table.get(schema, key).to_owned()))
    };
    let names: Vec<String> = job.objects.iter().map(|o| o.name.clone()).collect();
    let extruders: Vec<usize> = job.objects.iter().map(|o| o.extruder.min(filaments.len().saturating_sub(1))).collect();
    let cx = gcode::Context {
        settings: &s,
        filaments: &filaments,
        object_extruder: &extruders,
        object_names: &names,
        lookup: &lookup,
        plate_name: job.plate_name.clone(),
    };
    let out = gcode::emit(&plans, &cx);
    warnings.extend(out.warnings);
    if extruders.iter().any(|e| *e != extruders.first().copied().unwrap_or(0)) {
        warnings.push("Several filaments print without a prime tower for now; expect some colour bleed.".into());
    }
    if base.get(schema, "raft_layers").trim() != "0" {
        warnings.push("Rafts are not printed yet.".into());
    }
    progress.set(1.0, "Slicing complete");
    Ok(Sliced { gcode: out.gcode, preview: out.preview, stats: out.stats, warnings, colours })
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

    #[test]
    fn errors_not_panics() {
        assert!(matches!(slice(&job(vec![]), &Progress::default()), Err(SliceError::Nothing)));
        let tall = cube(Vec3::new(100.0, 100.0, 0.0), 400.0);
        assert!(matches!(slice(&job(vec![tall]), &Progress::default()), Err(SliceError::TooTall(..))));
        let p = Progress::default();
        p.cancel();
        assert!(matches!(slice(&job(vec![cube(Vec3::new(100.0, 100.0, 0.0), 20.0)]), &p), Err(SliceError::Cancelled)));
    }

    /// `SLICE_FILE=model.stl SLICE_OUT=out.gcode cargo test --release -p CosmicSlicer-engine
    /// slice_a_file -- --nocapture` slices a model file centred on a 220 mm bed.
    #[test]
    fn slice_a_file() {
        let Ok(path) = std::env::var("SLICE_FILE") else { return };
        let mesh = CosmicSlicer_model::load(std::path::Path::new(&path));
        assert!(mesh.is_ok(), "{:?}", mesh.as_ref().err());
        let Ok(mesh) = mesh else { return };
        let mesh = mesh.centred();
        let mut j = job(vec![JobObject {
            name: path.clone(),
            mesh: Arc::new(mesh),
            matrix: Mat4::from_translation(Vec3::new(110.0, 110.0, 0.0)),
            extruder: 0,
            overrides: Vec::new(),
        }]);
        for (k, v) in std::env::var("SLICE_SET").unwrap_or_default().split(';').filter_map(|kv| kv.split_once('=')) {
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
