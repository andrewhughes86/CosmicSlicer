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
pub mod calib;
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
pub mod segment;
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

pub use gcode::{LayerEvent, Preview, PreviewLayer, Segment, Stats, auto_flush};
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
    /// Painted seams: triangles of `mesh`, enforced or blocked.
    pub seam_paint: Vec<(u32, SupportPaint)>,
    /// Painted fuzzy skin: triangles of `mesh`.
    pub fuzzy_paint: Vec<u32>,
    /// Painted colours: triangles of `mesh` and their filament (1-based).
    pub color_paint: Vec<(u32, u8)>,
    /// Brim ears placed by hand: centre on the plate (printer coordinates) and radius.
    pub brim_ears: Vec<(glam::DVec2, f64)>,
    /// More parts, modifiers and negative volumes, placed like `matrix`.
    pub volumes: Vec<JobVolume>,
    /// Settings by height, millimetres above the plate.
    pub ranges: Vec<HeightSettings>,
    /// Variable layer height: (height above the plate, layer height) points, in order.
    pub layer_profile: Vec<(f64, f64)>,
}

/// Settings for the layers between two heights: (from, to, settings).
pub type HeightSettings = (f64, f64, Vec<(String, String)>);

/// What a volume of an object does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VolumeKind {
    /// Prints, with the object (and its own settings inside it, when it has any).
    Part,
    /// Changes the settings inside it.
    Modifier,
    /// Cuts the object away inside it.
    Negative,
}

/// A part, modifier or negative volume of an object.
#[derive(Clone)]
pub struct JobVolume {
    pub kind: VolumeKind,
    pub mesh: Arc<Mesh>,
    /// Placement in printer coordinates.
    pub matrix: Mat4,
    pub overrides: Vec<(String, String)>,
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
    /// Pauses, colour changes and custom G-code by print height (mm).
    pub events: Vec<(f64, LayerEvent)>,
    /// A calibration print (OrcaSlicer's Calibration menu).
    pub calib: Option<calib::Calib>,
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
    let volumes: Vec<Vec<Placed>> =
        job.objects.iter().map(|o| o.volumes.iter().map(|v| Placed::new(&v.mesh, &v.matrix)).collect()).collect();
    // How tall each object is with its parts.
    let heights_of: Vec<f64> = placed
        .iter()
        .zip(&volumes)
        .zip(&job.objects)
        .map(|((p, vs), o)| {
            vs.iter()
                .zip(&o.volumes)
                .filter(|(_, v)| v.kind == VolumeKind::Part)
                .map(|(vp, _)| vp.top())
                .fold(p.top(), f64::max)
        })
        .collect();
    let top = heights_of.iter().copied().fold(0.0, f64::max);
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
    let varies = job
        .objects
        .iter()
        .any(|o| !o.layer_profile.is_empty() || o.ranges.iter().any(|r| r.2.iter().any(|(k, _)| k == "layer_height")));
    let layers = if varies {
        // The finest height any object asks for at each height.
        let asked = |z: f64| -> f64 {
            job.objects
                .iter()
                .map(|o| {
                    let ranged = o
                        .ranges
                        .iter()
                        .filter(|r| z >= r.0 && z < r.1)
                        .filter_map(|r| r.2.iter().find(|(k, _)| k == "layer_height"))
                        .filter_map(|(_, v)| v.trim().parse::<f64>().ok())
                        .fold(f64::INFINITY, f64::min);
                    profile_at(&o.layer_profile, z).unwrap_or(s.layer_height).min(ranged)
                })
                .fold(f64::INFINITY, f64::min)
        };
        let hi = s.max_layer_height.max(s.min_layer_height);
        slicing::layer_heights_by(top, first_height, s.min_layer_height, hi, &asked)
    } else {
        slicing::layer_heights(top, first_height, s.layer_height)
    };
    let n = layers.len();
    let objects = placed.len().max(1) as f64;

    // Each object: slices, then walls and fills.
    let mut object_slices: Vec<Vec<Vec<Shape>>> = Vec::new();
    let mut object_plans: Vec<Vec<Vec<toolpath::Island>>> = Vec::new();
    let mut object_settings: Vec<Settings> = Vec::new();
    let mut object_marks: Vec<Marks> = Vec::new();
    for (oi, (p, o)) in placed.iter().zip(&job.objects).enumerate() {
        progress.set(0.05 + 0.25 * oi as f64 / objects, "Slicing mesh");
        let mut slices = slicing::slice(p, &layers, &cancelled);
        // The other volumes: parts join the object, negative volumes cut it.
        let vslices: Vec<Vec<Vec<Shape>>> = volumes
            .get(oi)
            .map_or(&[][..], Vec::as_slice)
            .iter()
            .map(|vp| slicing::slice(vp, &layers, &cancelled))
            .collect();
        for kind in [VolumeKind::Part, VolumeKind::Negative] {
            for (_, vs) in o.volumes.iter().zip(&vslices).filter(|(v, _)| v.kind == kind) {
                for (layer, cut) in slices.iter_mut().zip(vs).filter(|(_, c)| !c.is_empty()) {
                    let op = if kind == VolumeKind::Part { Op::Union } else { Op::Difference };
                    *layer = geom::boolean(layer, cut, op);
                }
            }
        }
        if cancelled() {
            return Err(SliceError::Cancelled);
        }
        progress.set(0.30 + 0.40 * oi as f64 / objects, "Generating walls and infill");
        let mut cfg = base.clone();
        for (k, v) in &o.overrides {
            cfg.set(k.clone(), v.clone());
        }
        let os = Settings::read(schema, &cfg);
        let painted = paint_layers(o, p, &layers, &cancelled);
        let regions = layer_regions(schema, &cfg, o, &layers, &vslices, &painted);
        if let Some(angle) = os.printable_overhang {
            let heights: Vec<f64> = layers.iter().map(|l| l.height).collect();
            overhang::make_printable(&mut slices, &heights, angle);
        }
        let mut islands = if o.color_paint.is_empty() {
            plan::object(&slices, &layers, &os, &regions, &cancelled)
        } else {
            coloured(o, p, &slices, &layers, &os, &regions, filaments.len(), &cancelled)
        };
        // OrcaSlicer's per-object flow ratio (the flow calibration's blocks).
        let ratio = cfg.get(schema, "print_flow_ratio").trim().parse::<f64>().ok().filter(|r| r.is_finite());
        if let Some(r) = ratio.map(|r| r.clamp(0.1, 3.0)).filter(|r| (r - 1.0).abs() > 1e-9) {
            for path in
                islands.iter_mut().flatten().flat_map(|i| i.walls.iter_mut().flatten().chain(i.fills.iter_mut()))
            {
                path.flow *= r;
            }
        }
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
            .zip(&heights_of)
            .zip(&job.objects)
            .map(|(((sl, os), h), o)| skirt::BrimObject {
                footprint: sl.first().map_or(&[][..], Vec::as_slice),
                settings: os,
                height: *h,
                ears: &o.brim_ears,
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
                new_object: false,
            }
        })
        .collect();
    let below = plans.len();
    // One object at a time: each object's layers, shortest object first.
    let by_object = s.multi.by_object && job.objects.len() > 1 && plans.is_empty();
    if by_object {
        let mut order: Vec<usize> = (0..job.objects.len()).collect();
        order.sort_by(|a, b| {
            heights_of.get(*a).copied().unwrap_or(0.0).total_cmp(&heights_of.get(*b).copied().unwrap_or(0.0))
        });
        check_clearance(&order, &object_slices, &heights_of, &s, &mut warnings);
        for (k, &o) in order.iter().enumerate() {
            // Each object's own supports, round itself only.
            let own = object_slices.get(o).map_or(&[][..], Vec::as_slice);
            let input: Vec<support::Input<'_>> = inputs.get(o).copied().into_iter().collect();
            let sl = support::generate(&input, own, &heights, &s, &cancelled);
            let mut started = false;
            for (i, l) in layers.iter().enumerate() {
                let islands =
                    object_plans.get_mut(o).and_then(|p| p.get_mut(i)).map(std::mem::take).unwrap_or_default();
                let sup = support::paths(&sl, i, &s, i == 0, l.print_z);
                if islands.is_empty() && sup.is_empty() {
                    continue;
                }
                // The skirt with the first object; each object's own brim before it.
                let mut ring = Vec::new();
                if k == 0 && i < s.skirt_height {
                    let w = if i == 0 { s.widths.first_layer } else { s.widths.outer };
                    ring = skirt::skirt(&skirt_around, &s, w, l.height);
                }
                if !started
                    && let (Some(sl), Some(os), Some(o_job)) =
                        (object_slices.get(o), object_settings.get(o), job.objects.get(o))
                {
                    let one = skirt::BrimObject {
                        footprint: sl.first().map_or(&[][..], Vec::as_slice),
                        settings: os,
                        height: heights_of.get(o).copied().unwrap_or(0.0),
                        ears: &o_job.brim_ears,
                    };
                    ring.extend(skirt::brims(&[one]).0);
                }
                plans.push(LayerPlan {
                    print_z: l.print_z,
                    height: l.height,
                    skirt: ring,
                    support: sup,
                    objects: vec![(o, islands)],
                    new_object: k > 0 && !started,
                });
                started = true;
            }
        }
    }
    plans.extend(layers.iter().enumerate().filter(|_| !by_object).map(|(i, l)| {
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
            new_object: false,
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
    // The pressure advance line and pattern are drawn where their placeholder stands.
    if let Some(c) = job.calib.as_ref().filter(|c| c.drawn()) {
        let b = placed.first().map(|p| {
            let (lo, hi) = p
                .vertices
                .iter()
                .fold((glam::DVec2::splat(f64::INFINITY), glam::DVec2::splat(f64::NEG_INFINITY)), |(a, b), v| {
                    (a.min(v.truncate()), b.max(v.truncate()))
                });
            (lo, hi)
        });
        let (at, room) = match b {
            Some((lo, hi)) if lo.is_finite() && hi.is_finite() => (lo, hi - lo),
            _ => (glam::DVec2::new(lo[0] + 10.0, lo[1] + 10.0), glam::DVec2::new(size[0] - 20.0, size[1] - 20.0)),
        };
        plans = match c.mode {
            calib::Mode::PaLine => {
                let fast = s.speeds.outer.max(20.0);
                let slow = (fast / 5.0).max(10.0);
                calib::pa_line(c, &s, at, glam::DVec2::new(room.x, room.y.max(40.0)), fast, slow)
            }
            _ => calib::pa_pattern(c, &s, at, glam::DVec2::new(room.x.max(60.0), room.y)),
        };
    }
    let cx = gcode::Context {
        settings: &s,
        filaments: &filaments,
        object_extruder: &extruders,
        object_names: &names,
        object_outlines: &outlines,
        macros: &macros,
        plate_name: job.plate_name.clone(),
        thumbnails,
        events: &job.events,
        calib: job.calib.as_ref(),
    };
    let out = gcode::emit(&plans, &cx);
    if let Some(e) = out.error {
        return Err(SliceError::Gcode(e));
    }
    warnings.extend(out.warnings);
    let several = extruders.iter().any(|e| *e != extruders.first().copied().unwrap_or(0))
        || job.objects.iter().any(|o| !o.color_paint.is_empty());
    if several && filaments.len() > 1 {
        if !s.multi.tower {
            warnings.push(
                "Several filaments print without a prime tower; expect colours to bleed into one another.".into(),
            );
        } else {
            // The tower must stand on the bed, clear of the objects.
            let (x, y) = (s.multi.tower_at[0], s.multi.tower_at[1]);
            let tower =
                vec![geom::rectangle(glam::DVec2::new(x, y), glam::DVec2::new(x + s.multi.tower_width, y + 20.0))];
            let on_bed = x >= lo[0] - 0.5 && y >= lo[1] - 0.5 && x + s.multi.tower_width <= lo[0] + size[0] + 0.5;
            if !on_bed || y > lo[1] + size[1] {
                warnings.push(
                    "The prime tower stands partly off the bed; move it in the Process settings (Multimaterial)."
                        .into(),
                );
            }
            if !geom::boolean(&tower, &footprint, Op::Intersect).is_empty() {
                warnings.push("The prime tower overlaps an object; move one of them.".into());
            }
        }
    }
    progress.set(1.0, "Slicing complete");
    Ok(Sliced { gcode: out.gcode, preview: out.preview, stats: out.stats, warnings, colours })
}

/// Each layer's settings where an object's height ranges, parts and modifiers change them.
fn layer_regions(
    schema: &Schema,
    cfg: &Config,
    o: &JobObject,
    layers: &[LayerZ],
    volume_slices: &[Vec<Vec<Shape>>],
    painted: &PaintLayers,
) -> Vec<plan::LayerRegions> {
    let zones: Vec<usize> = o
        .volumes
        .iter()
        .enumerate()
        .filter(|(_, v)| v.kind == VolumeKind::Modifier || (v.kind == VolumeKind::Part && !v.overrides.is_empty()))
        .map(|(k, _)| k)
        .collect();
    if o.ranges.is_empty() && zones.is_empty() && painted.is_empty() {
        return Vec::new();
    }
    let at = |layers: &[Vec<Shape>], i: usize| layers.get(i).cloned().unwrap_or_default();
    // Settings for each mix of height ranges and volume, read once.
    let mut cache: std::collections::HashMap<(Vec<usize>, Option<usize>), Arc<Settings>> = Default::default();
    let mut settings = |active: &[usize], volume: Option<usize>| -> Arc<Settings> {
        let key = (active.to_vec(), volume);
        let made = cache.entry(key).or_insert_with(|| {
            let mut c = cfg.clone();
            for (k, v) in active.iter().filter_map(|r| o.ranges.get(*r)).flat_map(|r| &r.2) {
                c.set(k.clone(), v.clone());
            }
            for (k, v) in volume.and_then(|i| o.volumes.get(i)).into_iter().flat_map(|v| &v.overrides) {
                c.set(k.clone(), v.clone());
            }
            Arc::new(Settings::read(schema, &c))
        });
        Arc::clone(made)
    };
    layers
        .iter()
        .enumerate()
        .map(|(i, l)| {
            let z = l.slice_z();
            let active: Vec<usize> =
                o.ranges.iter().enumerate().filter(|(_, r)| z >= r.0 && z < r.1).map(|(k, _)| k).collect();
            let base = (!active.is_empty()).then(|| settings(&active, None));
            let pieces = zones
                .iter()
                .filter_map(|&k| {
                    let area = volume_slices.get(k)?.get(i)?;
                    (!area.is_empty()).then(|| (area.clone(), settings(&active, Some(k))))
                })
                .collect();
            plan::LayerRegions {
                base,
                pieces,
                seam_enforce: at(&painted.seam_enforce, i),
                seam_block: at(&painted.seam_block, i),
                fuzzy: at(&painted.fuzzy, i),
            }
        })
        .collect()
}

/// Printing one object at a time: objects closer than the extruder clearance would be hit by
/// the print head, and only the last may be taller than the head's clearance.
fn check_clearance(
    order: &[usize],
    slices: &[Vec<Vec<Shape>>],
    heights: &[f64],
    s: &Settings,
    warnings: &mut Vec<String>,
) {
    let footprint = |o: usize| -> Vec<Shape> {
        let pts: Vec<glam::DVec2> =
            slices.get(o).into_iter().flatten().flatten().filter_map(|sh| sh.first()).flatten().copied().collect();
        let hull = geom::convex_hull(&pts);
        if hull.len() < 3 {
            Vec::new()
        } else {
            geom::offset(&[vec![hull]], s.multi.clearance_radius / 2.0, geom::Join::Round)
        }
    };
    let zones: Vec<Vec<Shape>> = order.iter().map(|&o| footprint(o)).collect();
    let mut close = false;
    for (i, a) in zones.iter().enumerate() {
        for b in zones.iter().skip(i + 1) {
            if !a.is_empty() && !b.is_empty() && !geom::boolean(a, b, Op::Intersect).is_empty() {
                close = true;
            }
        }
    }
    if close {
        warnings.push(format!(
            "Printing one object at a time, some objects are closer than the extruder clearance ({:.0} mm); the print head may hit them.",
            s.multi.clearance_radius
        ));
    }
    let tall = order
        .iter()
        .rev()
        .skip(1)
        .filter(|&&o| heights.get(o).copied().unwrap_or(0.0) > s.multi.clearance_height)
        .count();
    if tall > 0 {
        warnings.push(format!(
            "Printing one object at a time, only the last object may be taller than {:.0} mm (the extruder clearance height).",
            s.multi.clearance_height
        ));
    }
}

/// A variable layer height profile's height at `z`, straight between its points.
pub fn profile_at(profile: &[(f64, f64)], z: f64) -> Option<f64> {
    let first = profile.first()?;
    if z <= first.0 {
        return Some(first.1);
    }
    for pair in profile.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        if z <= b.0 {
            let t = if b.0 - a.0 > 1e-9 { (z - a.0) / (b.0 - a.0) } else { 1.0 };
            return Some(a.1 + (b.1 - a.1) * t);
        }
    }
    profile.last().map(|p| p.1)
}

/// An object with colour painted on it: each layer split among the painted filaments, each
/// part planned on its own and its islands marked with its filament.
#[allow(clippy::too_many_arguments)]
fn coloured(
    o: &JobObject,
    placed: &Placed,
    slices: &[Vec<Shape>],
    layers: &[LayerZ],
    s: &Settings,
    regions: &[plan::LayerRegions],
    filaments: usize,
    cancelled: &(dyn Fn() -> bool + Sync),
) -> Vec<Vec<toolpath::Island>> {
    use rayon::prelude::*;
    let mut by_filament: std::collections::BTreeMap<usize, Vec<u32>> = Default::default();
    for (t, f) in &o.color_paint {
        let f = usize::from(*f).saturating_sub(1);
        // Colours past the project's filaments print in the object's own.
        if f < filaments && f != o.extruder {
            by_filament.entry(f).or_default().push(*t);
        }
    }
    if by_filament.is_empty() {
        return plan::object(slices, layers, s, regions, cancelled);
    }
    let bands: Vec<(usize, Vec<Vec<Shape>>)> = by_filament
        .into_iter()
        .map(|(f, tris)| {
            let layers_of = slicing::painted(placed, &tris, layers, 0.6)
                .into_iter()
                .map(|l| if l.is_empty() { l } else { geom::offset(&l, 0.4, geom::Join::Round) })
                .collect();
            (f, layers_of)
        })
        .collect();
    let step = s.widths.outer.max(0.3);
    // Per layer: each colour's region, and what is left for the object's filament.
    let split: Vec<Vec<(usize, Vec<Shape>)>> = (0..slices.len())
        .into_par_iter()
        .map(|i| {
            if cancelled() {
                return Vec::new();
            }
            let Some(slice) = slices.get(i) else { return Vec::new() };
            let here: Vec<(usize, Vec<Shape>)> =
                bands.iter().map(|(f, l)| (*f, l.get(i).cloned().unwrap_or_default())).collect();
            segment::split(slice, &here, s.multi.segment_width, step)
        })
        .collect();
    let mut pieces: Vec<(Option<usize>, Vec<Vec<Shape>>)> = Vec::new();
    let rest: Vec<Vec<Shape>> = slices
        .iter()
        .zip(&split)
        .map(|(slice, parts)| {
            let painted: Vec<Shape> = parts.iter().flat_map(|(_, r)| r.iter().cloned()).collect();
            if painted.is_empty() { slice.clone() } else { geom::boolean(slice, &painted, Op::Difference) }
        })
        .collect();
    pieces.push((None, rest));
    for (f, _) in &bands {
        let mine: Vec<Vec<Shape>> = split
            .iter()
            .map(|parts| parts.iter().filter(|(g, _)| g == f).flat_map(|(_, r)| r.iter().cloned()).collect())
            .collect();
        pieces.push((Some(*f), mine));
    }
    let mut out: Vec<Vec<toolpath::Island>> = vec![Vec::new(); slices.len()];
    for (f, piece) in pieces {
        if piece.iter().all(Vec::is_empty) {
            continue;
        }
        for (layer, islands) in out.iter_mut().zip(plan::object(&piece, layers, s, regions, cancelled)) {
            layer.extend(islands.into_iter().map(|mut isl| {
                isl.extruder = f;
                isl
            }));
        }
    }
    out
}

/// An object's painted seams and fuzzy skin per layer (empty when none are painted).
#[derive(Default)]
struct PaintLayers {
    seam_enforce: Vec<Vec<Shape>>,
    seam_block: Vec<Vec<Shape>>,
    fuzzy: Vec<Vec<Shape>>,
}

impl PaintLayers {
    fn is_empty(&self) -> bool {
        self.seam_enforce.is_empty() && self.seam_block.is_empty() && self.fuzzy.is_empty()
    }
}

/// Where object `o`'s painted seam and fuzzy facets lie on each layer: a band round the walls
/// they paint, a little wider than the walls so the wall lines fall inside it.
fn paint_layers(
    o: &JobObject,
    placed: &Placed,
    layers: &[LayerZ],
    cancelled: &(dyn Fn() -> bool + Sync),
) -> PaintLayers {
    let band = |tris: Vec<u32>| -> Vec<Vec<Shape>> {
        if tris.is_empty() || cancelled() {
            return Vec::new();
        }
        slicing::painted(placed, &tris, layers, 0.6)
            .into_iter()
            .map(|l| if l.is_empty() { l } else { geom::offset(&l, 0.4, geom::Join::Round) })
            .collect()
    };
    let of = |want: SupportPaint| o.seam_paint.iter().filter(|(_, k)| *k == want).map(|(t, _)| *t).collect();
    PaintLayers {
        seam_enforce: band(of(SupportPaint::Enforce)),
        seam_block: band(of(SupportPaint::Block)),
        fuzzy: band(o.fuzzy_paint.clone()),
    }
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
            for (d, sh) in dst.iter_mut().zip(slicing::painted(placed, &tris, layers, 0.2)) {
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
            events: Vec::new(),
            calib: None,
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
            seam_paint: Vec::new(),
            fuzzy_paint: Vec::new(),
            color_paint: Vec::new(),
            brim_ears: Vec::new(),
            volumes: Vec::new(),
            ranges: Vec::new(),
            layer_profile: Vec::new(),
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
    #[test]
    fn calibration_towers_step_their_values_up_the_layers() {
        use calib::{Calib, Mode};
        // A temperature tower from 230 down to 210: falling M104s, 5 apart.
        let mut j = job(vec![cube(Vec3::new(100.0, 100.0, 0.0), 10.0)]);
        j.calib = Some(Calib::new(Mode::TempTower, 230.0, 210.0, 5.0));
        let Some(out) = sliced(&j) else { return };
        let temps: Vec<i64> = out
            .gcode
            .lines()
            .filter(|l| l.ends_with("; temperature tower"))
            .filter_map(|l| l.strip_prefix("M104 S")?.split(' ').next()?.parse().ok())
            .collect();
        assert_eq!(temps.first(), Some(&230));
        assert_eq!(temps.last(), Some(&210));
        assert!(temps.windows(2).all(|w| w[1] <= w[0] && (w[0] - w[1]) % 5 == 0), "{temps:?}");
        // A retraction tower: retractions grow with height, and every one is given back.
        let mut j = job(vec![cube(Vec3::new(60.0, 100.0, 0.0), 6.0), cube(Vec3::new(140.0, 100.0, 0.0), 6.0)]);
        j.process.set("retraction_length", "0.8");
        j.calib = Some(Calib::new(Mode::Retraction, 0.2, 1.0, 0.2));
        let Some(out) = sliced(&j) else { return };
        let lengths: Vec<f64> =
            out.gcode.lines().filter_map(|l| l.split("length:").nth(1)?.trim().parse().ok()).collect();
        assert!(lengths.first().is_some_and(|l| (*l - 0.2).abs() < 1e-6), "{lengths:?}");
        assert!(lengths.last().is_some_and(|l| (*l - 1.2).abs() < 1e-6), "{lengths:?}");
        // A PA tower sets pressure advance on every layer.
        let mut j = job(vec![cube(Vec3::new(100.0, 100.0, 0.0), 6.0)]);
        j.printer.set("gcode_flavor", "klipper");
        j.calib = Some(Calib::new(Mode::PaTower, 0.0, 0.1, 0.002));
        let Some(out) = sliced(&j) else { return };
        let n = out.gcode.lines().filter(|l| l.starts_with("SET_PRESSURE_ADVANCE")).count();
        assert!(n >= 29, "{n}");
    }

    #[test]
    fn pressure_advance_line_and_pattern_are_drawn() {
        use calib::{Calib, Mode};
        // The placeholder only says where; the lines replace it.
        let mut plate = cube(Vec3::new(30.0, 30.0, 0.0), 1.0);
        plate.matrix =
            Mat4::from_translation(Vec3::new(30.0, 30.0, 0.0)) * Mat4::from_scale(Vec3::new(160.0, 150.0, 0.2));
        let mut j = job(vec![plate.clone()]);
        j.printer.set("gcode_flavor", "klipper");
        j.calib = Some(Calib::new(Mode::PaLine, 0.0, 0.07, 0.002));
        let Some(out) = sliced(&j) else { return };
        let pa: Vec<&str> = out.gcode.lines().filter(|l| l.starts_with("SET_PRESSURE_ADVANCE")).collect();
        // 36 values, plus the anchor lines and numbers at no PA.
        assert!(pa.len() >= 36, "{}", pa.len());
        assert!(pa.iter().any(|l| l.ends_with("ADVANCE=0.07")), "{pa:?}");
        assert_eq!(out.stats.layers, 2);
        let mut j = job(vec![plate]);
        j.calib = Some(Calib::new(Mode::PaPattern, 0.0, 0.08, 0.005));
        let Some(out) = sliced(&j) else { return };
        assert_eq!(out.stats.layers, 4);
        assert!(out.gcode.lines().filter(|l| l.starts_with("M900 K")).count() >= 17 * 4);
    }

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

    /// Sparse infill extruded (mm of line) per mm² inside and outside the box `lo`–`hi`, from
    /// the G-code.
    fn infill_density(gcode: &str, lo: [f64; 2], hi: [f64; 2]) -> (f64, f64) {
        let (mut inside, mut outside) = (0.0, 0.0);
        let (mut x, mut y) = (0.0f64, 0.0f64);
        let mut sparse = false;
        for line in gcode.lines() {
            if let Some(t) = line.strip_prefix(";TYPE:") {
                sparse = t == "Sparse infill";
                continue;
            }
            if !line.starts_with("G1 ") {
                continue;
            }
            let get =
                |c: char| line.split_whitespace().find_map(|w| w.strip_prefix(c)).and_then(|v| v.parse::<f64>().ok());
            let (nx, ny) = (get('X').unwrap_or(x), get('Y').unwrap_or(y));
            if sparse && get('E').is_some_and(|e| e > 0.0) {
                let len = ((nx - x).powi(2) + (ny - y).powi(2)).sqrt();
                let (mx, my) = ((nx + x) / 2.0, (ny + y) / 2.0);
                if mx > lo[0] && mx < hi[0] && my > lo[1] && my < hi[1] {
                    inside += len;
                } else {
                    outside += len;
                }
            }
            (x, y) = (nx, ny);
        }
        (inside, outside)
    }

    #[test]
    fn a_modifier_changes_the_infill_exactly_inside_it() {
        // A 30 mm cube at 10 %, with a 12 mm modifier at 60 % in one corner, through its height.
        let mut cube30 = cube(Vec3::new(100.0, 100.0, 0.0), 30.0);
        cube30.volumes.push(JobVolume {
            kind: VolumeKind::Modifier,
            mesh: Arc::new(primitives::cube(12.0)),
            matrix: Mat4::from_translation(Vec3::new(101.0, 101.0, -1.0)) * Mat4::from_scale(Vec3::new(1.0, 1.0, 3.0)),
            overrides: vec![("sparse_infill_density".into(), "60%".into())],
        });
        let mut j = job(vec![cube30]);
        j.process.set("sparse_infill_density", "10%");
        let Some(out) = sliced(&j) else { return };
        // Line per mm² in a box.
        let density =
            |lo: [f64; 2], hi: [f64; 2]| infill_density(&out.gcode, lo, hi).0 / ((hi[0] - lo[0]) * (hi[1] - lo[1]));
        // Well inside the modifier (101–113), just outside it, and far from it.
        let inside = density([103.0, 103.0], [111.0, 111.0]);
        let beside = density([114.0, 103.0], [117.0, 111.0]);
        let far = density([118.0, 118.0], [127.0, 127.0]);
        assert!(far > 0.0, "{far}");
        let ratio = inside / far;
        assert!(ratio > 4.0 && ratio < 9.0, "inside {inside} far {far}: ratio {ratio}");
        assert!(beside < far * 1.6, "beside {beside} far {far}");
    }

    #[test]
    fn parts_join_and_negative_volumes_cut() {
        let solid = |volumes: Vec<JobVolume>| {
            let mut c = cube(Vec3::new(100.0, 100.0, 0.0), 10.0);
            c.volumes = volumes;
            let mut j = job(vec![c]);
            j.process.set("sparse_infill_density", "100%");
            sliced(&j).map(|o| o.stats.volume_mm3[0]).unwrap_or(0.0)
        };
        let plain = solid(vec![]);
        let vol = |kind, at: Vec3, size: f32| JobVolume {
            kind,
            mesh: Arc::new(primitives::cube(size)),
            matrix: Mat4::from_translation(at),
            overrides: Vec::new(),
        };
        // A second 10 mm cube beside it doubles it; a 5 mm hole takes an eighth.
        let joined = solid(vec![vol(VolumeKind::Part, Vec3::new(110.0, 100.0, 0.0), 10.0)]);
        let cut = solid(vec![vol(VolumeKind::Negative, Vec3::new(100.0, 100.0, 5.0), 5.0)]);
        assert!((joined / plain - 2.0).abs() < 0.12, "{joined} vs {plain}");
        assert!((cut / plain - 7.0 / 8.0).abs() < 0.06, "{cut} vs {plain}");
    }

    #[test]
    fn a_height_range_sets_its_layers() {
        let mut c = cube(Vec3::new(100.0, 100.0, 0.0), 20.0);
        c.ranges.push((5.0, 10.0, vec![("sparse_infill_density".into(), "70%".into())]));
        let mut j = job(vec![c]);
        j.process.set("sparse_infill_density", "10%");
        let Some(out) = sliced(&j) else { return };
        // Sparse infill per layer: the layers in the range carry far more.
        let mut per_layer: Vec<f64> = Vec::new();
        let mut sparse = false;
        let mut layer = 0usize;
        let (mut x, mut y) = (0.0f64, 0.0f64);
        for line in out.gcode.lines() {
            if line.starts_with(";LAYER_CHANGE") {
                layer += 1;
            }
            if let Some(t) = line.strip_prefix(";TYPE:") {
                sparse = t == "Sparse infill";
            }
            if !line.starts_with("G1 ") {
                continue;
            }
            let get =
                |c: char| line.split_whitespace().find_map(|w| w.strip_prefix(c)).and_then(|v| v.parse::<f64>().ok());
            let (nx, ny) = (get('X').unwrap_or(x), get('Y').unwrap_or(y));
            if sparse && get('E').is_some_and(|e| e > 0.0) {
                if per_layer.len() < layer + 1 {
                    per_layer.resize(layer + 1, 0.0);
                }
                per_layer[layer] += ((nx - x).powi(2) + (ny - y).powi(2)).sqrt();
            }
            (x, y) = (nx, ny);
        }
        // Layer k (1-based) prints at 0.2 k mm: 30 is 6 mm, 70 is 14 mm.
        let (inside, outside) = (per_layer.get(30).copied().unwrap_or(0.0), per_layer.get(70).copied().unwrap_or(0.0));
        assert!(inside > outside * 4.0, "{inside} vs {outside}");
    }

    /// The cube's triangles on its local face `x == 0`.
    fn x0_face(size: f32) -> Vec<u32> {
        let m = primitives::cube(size);
        m.triangles
            .iter()
            .enumerate()
            .filter(|(_, t)| t.iter().all(|v| m.vertices.get(*v as usize).is_some_and(|p| p.x == 0.0)))
            .map(|(i, _)| i as u32)
            .collect()
    }

    /// Extruding moves of `kind` as (x, y).
    fn extrusions(gcode: &str, kind: &str) -> Vec<(f64, f64)> {
        let mut on = false;
        let mut out = Vec::new();
        for line in gcode.lines() {
            if let Some(t) = line.strip_prefix(";TYPE:") {
                on = t == kind;
            } else if on && line.starts_with("G1 ") && line.contains(" E") {
                let get = |c: char| {
                    line.split_whitespace().find_map(|w| w.strip_prefix(c)).and_then(|v| v.parse::<f64>().ok())
                };
                if let (Some(x), Some(y)) = (get('X'), get('Y')) {
                    out.push((x, y));
                }
            }
        }
        out
    }

    #[test]
    fn painted_seams_and_fuzzy_skin_go_where_painted() {
        let face = x0_face(20.0);
        assert_eq!(face.len(), 2);
        let mut c = cube(Vec3::new(100.0, 100.0, 0.0), 20.0);
        c.seam_paint = face.iter().map(|t| (*t, SupportPaint::Enforce)).collect();
        let mut j = job(vec![c]);
        j.process.set("seam_position", "back");
        let Some(out) = sliced(&j) else { return };
        // Every seam on the painted face (x = 100), not at the back as asked otherwise.
        assert!(!out.preview.seams.is_empty());
        assert!(out.preview.seams.iter().all(|s| s[0] < 101.0), "{:?}", &out.preview.seams[..3]);

        let mut c = cube(Vec3::new(100.0, 100.0, 0.0), 20.0);
        c.fuzzy_paint = face;
        let Some(out) = sliced(&job(vec![c])) else { return };
        let walls = extrusions(&out.gcode, "Outer wall");
        let min_x = walls.iter().map(|p| p.0).fold(f64::INFINITY, f64::min);
        // The next face along, away from the corner the paint's band reaches round.
        let min_y = walls.iter().filter(|p| p.0 > 102.0).map(|p| p.1).fold(f64::INFINITY, f64::min);
        // Rough on the painted face, straight on the others.
        assert!(min_x < 100.1, "{min_x}");
        assert!(min_y > 100.15, "{min_y}");
    }

    #[test]
    fn painted_brims_put_ears_where_placed() {
        let mut c = cube(Vec3::new(100.0, 100.0, 0.0), 20.0);
        c.overrides.push(("brim_type".into(), "painted".into()));
        c.brim_ears = vec![(glam::DVec2::new(100.0, 100.0), 4.0)];
        let Some(out) = sliced(&job(vec![c])) else { return };
        let brim = extrusions(&out.gcode, "Brim");
        assert!(!brim.is_empty());
        assert!(brim.iter().all(|(x, y)| (x - 100.0).hypot(y - 100.0) < 4.5), "{brim:?}");
    }

    /// Two filaments, black and white.
    fn two_filaments(j: &mut Job) {
        let mut white = Config::default();
        white.set("filament_density", "1.24");
        white.set("filament_colour", "#FFFFFF");
        if let Some(f) = j.filaments.first_mut() {
            f.set("filament_colour", "#000000");
        }
        j.filaments.push(white);
    }

    #[test]
    fn a_painted_two_colour_cube_prints_with_a_tower() {
        let mut c = cube(Vec3::new(100.0, 100.0, 0.0), 20.0);
        c.color_paint = x0_face(20.0).into_iter().map(|t| (t, 2)).collect();
        let mut j = job(vec![c]);
        two_filaments(&mut j);
        j.process.set("enable_prime_tower", "1");
        j.process.set("wipe_tower_x", "30");
        j.process.set("wipe_tower_y", "30");
        j.process.set("flush_volumes_matrix", "0,200,200,0");
        let out = slice(&j, &Progress::default());
        assert!(out.is_ok(), "{:?}", out.as_ref().err());
        let Ok(out) = out else { return };
        // It reads back with the same time and filament per extruder.
        let back = view_gcode(out.gcode.clone(), &Config::default(), &j.filaments);
        let (a, b) = (back.stats.total_time, out.stats.total_time);
        assert!((a - b).abs() / b < 0.02, "read back {a} vs sliced {b}");
        for (x, y) in back.stats.filament_mm.iter().zip(&out.stats.filament_mm) {
            assert!((x - y).abs() / y.max(1.0) < 0.02, "{x} vs {y}");
        }
        assert!(out.gcode.contains(";TYPE:Prime tower"));
        assert!(out.stats.filament_mm.iter().all(|f| *f > 10.0), "{:?}", out.stats.filament_mm);
        // At most two changes a layer (to white and back).
        let changes: Vec<usize> = out
            .gcode
            .split(";LAYER_CHANGE")
            .map(|layer| layer.lines().filter(|l| *l == "T0" || *l == "T1").count())
            .collect();
        assert!(changes.iter().all(|c| *c <= 2), "{changes:?}");
        assert!(changes.iter().filter(|c| **c > 0).count() > 50);
        // Each change purges about the table's 200 mm³ into the tower.
        let tower: Vec<(f64, f64)> = extrusions(&out.gcode, "Prime tower");
        assert!(tower.iter().all(|(x, y)| *x >= 29.0 && *x <= 91.0 && *y >= 29.0 && *y < 120.0), "{:?}", &tower[..3]);
        assert!(!out.warnings.iter().any(|w| w.contains("tower")), "{:?}", out.warnings);
        // White is on the painted face: x near 100.
        let painted: Vec<(f64, f64)> = {
            let mut on = false;
            out.gcode
                .lines()
                .filter_map(|l| {
                    if l == "T1" {
                        on = true;
                    } else if l == "T0" {
                        on = false;
                    }
                    let get = |c: char| {
                        l.split_whitespace().find_map(|w| w.strip_prefix(c)).and_then(|v| v.parse::<f64>().ok())
                    };
                    (on && l.starts_with("G1 ") && l.contains(" E") && get('X').is_some_and(|x| x > 95.0))
                        .then(|| (get('X').unwrap_or(0.0), get('Y').unwrap_or(0.0)))
                })
                .collect()
        };
        assert!(
            !painted.is_empty() && painted.iter().all(|(x, _)| *x < 112.0),
            "{:?}",
            painted.iter().map(|p| p.0).fold(0.0, f64::max)
        );
    }

    #[test]
    fn change_templates_ooze_prevention_and_layer_events() {
        let a = cube(Vec3::new(60.0, 100.0, 0.0), 10.0);
        let mut b = cube(Vec3::new(120.0, 100.0, 0.0), 10.0);
        b.extruder = 1;
        let mut j = job(vec![a, b]);
        two_filaments(&mut j);
        j.printer.set("change_filament_gcode", "; swap {previous_extruder} to {next_extruder}\nT[next_extruder]");
        j.process.set("ooze_prevention", "1");
        j.process.set("standby_temperature_delta", "-40");
        j.events =
            vec![(2.0, LayerEvent::Pause), (4.0, LayerEvent::ColorChange), (6.0, LayerEvent::Custom("; hello".into()))];
        let out = slice(&j, &Progress::default());
        assert!(out.is_ok(), "{:?}", out.as_ref().err());
        let Ok(out) = out else { return };
        assert!(out.gcode.contains("; swap 0 to 1"), "the template ran");
        // The template's own T is the only one.
        assert!(!out.gcode.contains("T1\nT1"));
        assert!(out.gcode.lines().any(|l| l.starts_with("M104 T0 S")));
        for (mark, z) in [("M601", "2"), ("M600", "4"), ("; hello", "6")] {
            let at = out.gcode.find(mark);
            assert!(at.is_some(), "{mark}");
            let before = &out.gcode[..at.unwrap_or(0)];
            assert!(before.rsplit(";Z:").next().is_some_and(|l| l.starts_with(z)), "{mark}");
        }
    }

    #[test]
    fn objects_print_one_at_a_time() {
        let tall = cube(Vec3::new(60.0, 100.0, 0.0), 10.0);
        let short = cube(Vec3::new(90.0, 100.0, 0.0), 5.0);
        let mut j = job(vec![tall, short]);
        j.process.set("print_sequence", "by object");
        let out = slice(&j, &Progress::default());
        assert!(out.is_ok(), "{:?}", out.as_ref().err());
        let Ok(out) = out else { return };
        // The short one first, all of it, then down again for the tall one.
        let zs: Vec<f64> =
            out.gcode.lines().filter_map(|l| l.strip_prefix(";Z:")).filter_map(|v| v.parse().ok()).collect();
        let drops = zs.windows(2).filter(|w| w[1] < w[0]).count();
        assert_eq!(drops, 1, "{zs:?}");
        assert!((zs[24] - 5.0).abs() < 1e-6 && (zs[25] - 0.2).abs() < 1e-6, "{:?}", &zs[20..30]);
        // 30 mm apart, within the 40 mm clearance: a warning.
        assert!(out.warnings.iter().any(|w| w.contains("clearance")), "{:?}", out.warnings);
    }

    #[test]
    fn light_after_dark_needs_more_flushing() {
        assert!(auto_flush("#000000", "#FFFFFF") > auto_flush("#FFFFFF", "#000000"));
        assert!(auto_flush("#FF0000", "#FF0000") < auto_flush("#FF0000", "#00FF00"));
    }

    #[test]
    fn variable_layer_heights_slice_as_drawn() {
        let mut c = cube(Vec3::new(100.0, 100.0, 0.0), 20.0);
        c.layer_profile = vec![(0.0, 0.1), (8.0, 0.1), (12.0, 0.3), (20.0, 0.3)];
        let Some(out) = sliced(&job(vec![c])) else { return };
        let heights = |lo: f32, hi: f32| -> Vec<f32> {
            out.preview.layers.iter().filter(|l| l.z > lo && l.z < hi).map(|l| l.height).collect()
        };
        assert!(heights(1.0, 7.0).iter().all(|h| (h - 0.1).abs() < 0.011), "{:?}", heights(1.0, 7.0));
        assert!(heights(13.0, 19.5).iter().all(|h| (h - 0.3).abs() < 0.011), "{:?}", heights(13.0, 19.5));
        // In between it thickens gradually.
        let mid = heights(8.5, 11.5);
        assert!(mid.windows(2).all(|w| w[1] >= w[0] - 1e-4), "{mid:?}");
        let top = out.preview.layers.last().map_or(0.0, |l| l.z);
        assert!((top - 20.0).abs() < 1e-3, "{top}");
        // A height range's layer height thins its layers.
        let mut c = cube(Vec3::new(100.0, 100.0, 0.0), 20.0);
        c.ranges.push((5.0, 10.0, vec![("layer_height".into(), "0.1".into())]));
        let Some(out) = sliced(&job(vec![c])) else { return };
        assert!(out.preview.layers.iter().filter(|l| l.z > 5.5 && l.z < 9.5).all(|l| (l.height - 0.1).abs() < 0.011));
        assert!(out.preview.layers.iter().filter(|l| l.z > 11.0).all(|l| (l.height - 0.2).abs() < 0.011));
    }

    #[test]
    fn a_spiral_vase_climbs_as_it_goes_round() {
        let v = object("vase", primitives::cylinder(15.0, 20.0, 64), Vec3::new(110.0, 110.0, 0.0));
        let mut j = job(vec![v]);
        j.process.set("spiral_mode", "1");
        let Some(out) = sliced(&j) else { return };
        // Extruding moves that also rise, within the layers above the bottom.
        let rising = out.gcode.lines().filter(|l| l.starts_with("G1 ") && l.contains(" Z") && l.contains(" E")).count();
        assert!(rising > 1000, "{rising}");
        let zs: Vec<f64> = out
            .gcode
            .lines()
            .filter(|l| l.starts_with("G1 ") && l.contains(" E"))
            .filter_map(|l| l.split_whitespace().find_map(|w| w.strip_prefix('Z')).and_then(|v| v.parse().ok()))
            .collect();
        // Steadily up, never down while printing.
        assert!(zs.windows(2).all(|w| w[1] >= w[0] - 1e-6), "{:?}", &zs[..10]);
    }

    #[test]
    fn one_at_a_time_each_object_has_its_own_brim() {
        let a = cube(Vec3::new(60.0, 100.0, 0.0), 5.0);
        let b = cube(Vec3::new(140.0, 100.0, 0.0), 5.0);
        let mut j = job(vec![a, b]);
        j.process.set("print_sequence", "by object");
        j.process.set("brim_type", "outer_only");
        j.process.set("brim_width", "3");
        let Some(out) = sliced(&j) else { return };
        // A brim at the start of each object, the second after the first is finished.
        let layers: Vec<&str> = out.gcode.split(";LAYER_CHANGE").collect();
        let with_brim: Vec<usize> =
            layers.iter().enumerate().filter(|(_, l)| l.contains(";TYPE:Brim")).map(|(i, _)| i).collect();
        assert_eq!(with_brim.len(), 2, "{with_brim:?}");
        assert!(with_brim[1] > 20, "{with_brim:?}");
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
            seam_paint: Vec::new(),
            fuzzy_paint: Vec::new(),
            color_paint: Vec::new(),
            brim_ears: Vec::new(),
            volumes: Vec::new(),
            ranges: Vec::new(),
            layer_profile: Vec::new(),
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
            seam_paint: Vec::new(),
            fuzzy_paint: Vec::new(),
            color_paint: Vec::new(),
            brim_ears: Vec::new(),
            volumes: Vec::new(),
            ranges: Vec::new(),
            layer_profile: Vec::new(),
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
