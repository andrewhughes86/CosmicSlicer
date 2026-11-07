//! From layer plans to G-code: print order, seams, travel routing and retraction, cooling,
//! speeds, motion commands, the time estimate, and the moves the Preview draws.
//!
//! The writer builds a program of [`Op`]s first. The motion planner ([`crate::motion`]) times
//! that program as the firmware will run it, and only then is it rendered as text, so the
//! estimate, the `M73` progress and the G-code always describe the same moves.

use std::fmt::Write as _;

use std::f64::consts::TAU;

use glam::{DVec2, DVec3};

use crate::arcs::{self, Piece};
use crate::gcodeview::Machine;
use crate::geom;
use crate::motion::{self, Block, BlockKind};
use crate::settings::{ArcFitting, Filament, Flavor, Scarf, Settings, ZHop, bead_area};
use crate::toolpath::{Island, LayerPlan, Path, Role};
use crate::travel::Region;
use CosmicSlicer_config::expr::Value;

/// One extrusion move for the Preview.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Segment {
    pub a: [f32; 3],
    pub b: [f32; 3],
    pub role: Role,
    pub width: f32,
    pub height: f32,
    /// mm/s.
    pub speed: f32,
    /// 0 to 100.
    pub fan: f32,
    pub temp: f32,
    /// 0-based.
    pub extruder: u8,
    /// mm³/s.
    pub flow: f32,
}

/// One layer's moves in the Preview.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PreviewLayer {
    pub z: f32,
    pub height: f32,
    pub segments: std::ops::Range<usize>,
    pub travels: std::ops::Range<usize>,
    pub seams: std::ops::Range<usize>,
    pub retractions: std::ops::Range<usize>,
    /// Seconds.
    pub time: f32,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Preview {
    pub layers: Vec<PreviewLayer>,
    pub segments: Vec<Segment>,
    pub travels: Vec<([f32; 3], [f32; 3])>,
    /// Where outer walls start.
    pub seams: Vec<[f32; 3]>,
    /// Where the filament is pulled back.
    pub retractions: Vec<[f32; 3]>,
}

/// The estimates OrcaSlicer shows under the legend.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Stats {
    /// Seconds.
    pub total_time: f64,
    pub time_by_role: [f64; Role::COUNT],
    /// Filament each feature takes, mm³.
    pub volume_by_role: [f64; Role::COUNT],
    pub travel_time: f64,
    /// Seconds before the first layer (the start G-code): OrcaSlicer's "prepare" time, which
    /// its "model printing time" leaves out.
    pub prepare_time: f64,
    /// Per extruder: mm of filament, mm³, grams, cost.
    pub filament_mm: Vec<f64>,
    pub volume_mm3: Vec<f64>,
    pub weight_g: Vec<f64>,
    pub cost: Vec<f64>,
    pub layers: usize,
}

/// Everything the emitter needs besides the plans.
pub struct Context<'a> {
    pub settings: &'a Settings,
    pub filaments: &'a [Filament],
    /// Extruder (0-based) per object.
    pub object_extruder: &'a [usize],
    pub object_names: &'a [String],
    /// Each object's outline on the plate (a convex polygon), for object labels.
    pub object_outlines: &'a [Vec<DVec2>],
    /// Custom G-code rendering.
    pub macros: &'a crate::macros::Macros<'a>,
    pub plate_name: String,
    /// Thumbnail blocks for the top of the file.
    pub thumbnails: String,
    /// Pauses, colour changes and custom G-code by height.
    pub events: &'a [(f64, LayerEvent)],
    /// A calibration print's per-layer changes.
    pub calib: Option<&'a crate::calib::Calib>,
    /// The settings as OrcaSlicer's config block lists them.
    pub config_block: String,
    /// `printable_area`, for that block.
    pub bed_shape: String,
    /// Where each object's loops start.
    pub seams: &'a [Option<crate::seam::ObjectSeams>],
    /// Where a printer with two nozzles parks for timelapse pictures.
    pub timelapse: Option<&'a crate::timelapse::Picker>,
    /// `filament_map`, 0-based: each filament's logical extruder.
    pub filament_map: &'a [usize],
    /// `physical_extruder_map`.
    pub physical_extruders: &'a [usize],
    /// The logical extruder that prints the most.
    pub most_used_extruder: usize,
}

impl Context<'_> {
    fn logical_extruder(&self, filament: usize) -> usize {
        self.filament_map.get(filament).copied().unwrap_or(0)
    }

    fn physical(&self, logical: usize) -> usize {
        self.physical_extruders.get(logical).copied().unwrap_or(logical)
    }
}

/// Renders custom G-code: what it is called, its text, and this layer's variables.
type Render<'r> = dyn FnMut(&str, &str, &[(&str, Value)]) -> String + 'r;

/// A point in Orca's units, for the chaining's distances.
fn nm_point(p: DVec2) -> [f64; 2] {
    [(p.x * 1.0e6).round(), (p.y * 1.0e6).round()]
}

/// A path's first or last point, for chaining.
fn end_point(path: &Path, first: bool) -> [f64; 2] {
    let at = if first || path.closed { path.points.first() } else { path.points.last() };
    at.map_or([0.0; 2], |p| nm_point(*p))
}

/// Something asked for at a layer from the layer slider.
#[derive(Clone, Debug, PartialEq)]
pub enum LayerEvent {
    /// The printer's pause G-code.
    Pause,
    /// M600: the printer asks for the next filament.
    ColorChange,
    Custom(String),
}

/// What a layer prints with one filament.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Part {
    /// Support (`true`: its interface).
    Support(bool),
    Islands,
}

/// What layer `plan` prints, in order, each with its filament: support first (with its own
/// filaments when set, else whatever is loaded), then the islands, the loaded filament's
/// first and the rest by number.
fn layer_steps(plan: &LayerPlan, cx: &Context<'_>, n_ext: usize, mut current: usize) -> Vec<(Part, usize)> {
    let mm = &cx.settings.multi;
    let mut out = Vec::new();
    for (interface, chosen) in [(false, mm.support_filament), (true, mm.support_interface_filament)] {
        if plan.support.iter().any(|p| (p.role == Role::SupportInterface) == interface) {
            let e = if chosen > 0 { (chosen - 1).min(n_ext - 1) } else { current };
            out.push((Part::Support(interface), e));
            current = e;
        }
    }
    let mut exts: Vec<usize> = plan
        .objects
        .iter()
        .flat_map(|(o, islands)| {
            let own = cx.object_extruder.get(*o).copied().unwrap_or(0);
            islands.iter().map(move |isl| isl.extruder.unwrap_or(own).min(n_ext - 1))
        })
        .collect();
    exts.sort_unstable();
    exts.dedup();
    exts.sort_by_key(|e| *e != current);
    out.extend(exts.into_iter().map(|e| (Part::Islands, e)));
    out
}

/// How much to purge changing from filament `from` to `to`, mm³: the flushing volumes table
/// when there is one, else from the two colours; never less than the prime volume.
fn flush_volume(cx: &Context<'_>, from: usize, to: usize) -> f64 {
    let mm = &cx.settings.multi;
    let n = cx.filaments.len();
    let table = mm.flush.get(from * n + to).copied().filter(|v| mm.flush.len() == n * n && *v > 0.0);
    let v = match table {
        Some(v) => v * mm.flush_multiplier,
        None => {
            let colour = |i: usize| cx.filaments.get(i).map_or("#808080", |f| f.colour.as_str());
            auto_flush(colour(from), colour(to))
        }
    };
    v.max(mm.prime_volume)
}

/// A flushing volume from one colour to another, mm³: more for light after dark, and more the
/// further apart the colours are (after OrcaSlicer's estimate).
pub fn auto_flush(from: &str, to: &str) -> f64 {
    let rgb = |hex: &str| {
        let v = hex
            .trim()
            .trim_start_matches('#')
            .get(..6)
            .and_then(|h| u32::from_str_radix(h, 16).ok())
            .unwrap_or(0x80_80_80);
        [(v >> 16) & 0xff, (v >> 8) & 0xff, v & 0xff].map(|c| f64::from(c) / 255.0)
    };
    let (a, b) = (rgb(from), rgb(to));
    let lum = |c: [f64; 3]| 0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2];
    let dist = ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt() / 3f64.sqrt();
    let lighter = (lum(b) - lum(a)).max(0.0);
    (70.0 + 450.0 * lighter + 250.0 * dist).round()
}

/// The prime tower: where it stands and how far up this layer's purges have filled it.
#[derive(Clone, Debug)]
struct Tower {
    at: DVec2,
    width: f64,
    depth: f64,
    /// Line width.
    w: f64,
    /// How far into the depth this layer's purges reach.
    used: f64,
    /// The layer whose walls are printed.
    walled: Option<usize>,
    /// The last layer with a filament change: the tower stops there.
    last: usize,
}

pub struct Output {
    /// The first custom G-code that could not be rendered; the slice fails with it.
    pub error: Option<String>,
    pub gcode: String,
    pub preview: Preview,
    pub stats: Stats,
    pub warnings: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MoveKind {
    Travel,
    /// A travel that names its Z even when unchanged (OrcaSlicer's moves while lifted).
    Lifted,
    Extrude(Role),
    /// Extruder only: retract or unretract (or its firmware command).
    Retract,
    Wipe,
}

/// One instruction of the program.
#[derive(Clone, Debug)]
enum Op {
    Text(String),
    /// A layer starts: index (M73 progress goes here).
    Layer(usize),
    /// `xy` is false for moves whose X and Y are not known (before the first travel, the
    /// head is wherever the start G-code left it): only Z and E are written then.
    Move {
        to: DVec3,
        e: f64,
        feed: f64,
        kind: MoveKind,
        arc: Option<(DVec2, bool)>,
        xy: bool,
    },
    /// Firmware retraction (G10) or recovery (G11), with the length it stands for.
    FirmwareRetract {
        retract: bool,
        length: f64,
        feed: f64,
    },
    Accel(f64),
    Jerk(f64),
    /// A helix up on the spot: to `z`, about the centre (`i`, `j`) from the head.
    Spiral {
        z: f64,
        i: f64,
        j: f64,
        feed: f64,
    },
    /// The part fan, %. The layer's speed is settled once the layer is written (it follows the
    /// layer's time after cooling), so the fan commands say what they are for.
    Fan(f64, Fan),
}

/// What a fan command is for.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Fan {
    /// The layer's fan.
    Layer,
    /// At least this (overhangs and bridges), or the layer's fan when that is higher.
    AtLeast,
    /// Exactly this (internal bridges, support interfaces and ironing with their own speed).
    Exact,
}

/// How a travel rises.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Lift {
    Normal,
    Slope,
    Spiral,
}

struct Writer<'a> {
    cx: &'a Context<'a>,
    /// What a calibration print changes on the current layer.
    calib: crate::calib::LayerCalib,
    /// How far the filament was drawn back by the last retraction.
    retracted_by: f64,
    ops: Vec<Op>,
    pos: DVec3,
    retracted: bool,
    /// Height lifted above the layer, while lifted.
    hop: f64,
    role: Option<Role>,
    width: f64,
    extruder: usize,
    /// A lift to make with the next travel (OrcaSlicer's lazy lift).
    to_lift: Option<Lift>,
    /// The layer's Z is still to be reached (the layer change moves no Z itself).
    z_pending: bool,
    /// Custom G-code ran: the next travel goes across first, then down.
    pos_unclear: bool,
    lifted_for_picture: bool,
    /// Object label to close, and to open, with the next travel.
    label_end: Option<String>,
    label_start: Option<String>,
    /// The fan as last commanded: (%, what for).
    out_fan: (f64, Fan),
    /// The fan ramp over the first layers (0 to 1; 0 while the fan is off).
    fan_ramp: f64,
    /// The fan asked for each Preview segment of this layer, settled by `cool_layer`.
    seg_fans: Vec<(f64, Fan)>,
    temp: f64,
    layer: usize,
    print_z: f64,
    height: f64,
    accel: f64,
    jerk: f64,
    stats: Stats,
    preview: Preview,
    /// Where the last loop's seam was, per object, for aligned seams.
    seams: Vec<Option<DVec2>>,
    /// The prime tower, when one is printed.
    tower: Option<Tower>,
    /// On a spiral vase layer: `Some(true)` on the first.
    spiral: Option<bool>,
    /// The island being printed's painted seams: where they go, where they must not.
    seam_paint: (Vec<geom::Shape>, Vec<geom::Shape>),
    /// How far the Preview's copy of a path may stray from it, mm.
    preview_tolerance: f64,
    /// The island being printed, for travel inside it.
    region: Option<Region>,
    /// The next island's region, installed once the head has arrived there.
    pending_region: Option<Region>,
    /// Travel regions made ahead for the islands of the coming layers, by island address.
    regions: std::collections::HashMap<usize, Option<Region>>,
    /// Whether `pos` X and Y are where the head is (not until the first travel).
    xy_known: bool,
    /// The end of the last extrusion, for wiping back along it.
    last_path: Vec<DVec2>,
    /// The speed it was printed at, for role-based wipes.
    last_speed: f64,
    warnings: Vec<String>,
    /// The layer below the island being printed, for its walls' overhang speeds.
    below: Option<std::sync::Arc<crate::overhang::Boundary>>,
}

/// Layers whose travel regions are made at once.
const REGIONS_AHEAD: usize = 32;

fn plain(v: f64, decimals: usize) -> String {
    let t = format!("{v:.decimals$}");
    let t = t.trim_end_matches('0').trim_end_matches('.');
    if t.is_empty() || t == "-0" { "0".to_owned() } else { t.to_owned() }
}

fn f3(v: f64) -> String {
    plain(v, 3)
}

/// printf's `%g`, for the sizes a line width comes in: six significant digits, no trailing zeros.
fn general(v: f64) -> String {
    if !v.is_finite() || v == 0.0 {
        return "0".into();
    }
    let scientific = format!("{v:.5e}");
    let exponent = scientific.split('e').nth(1).and_then(|e| e.parse::<i32>().ok()).unwrap_or(0);
    if !(-4..6).contains(&exponent) {
        return scientific;
    }
    let fixed = format!("{v:.*}", (5 - exponent) as usize);
    let trimmed = if fixed.contains('.') { fixed.trim_end_matches('0').trim_end_matches('.') } else { &fixed };
    trimmed.to_owned()
}

fn f5(v: f64) -> String {
    plain(v, 5)
}

/// `v` to `decimals` places without trailing zeros ("-0" as "0"), appended to `out` with no
/// allocation of its own: the G-code text has millions of numbers.
fn push_num(out: &mut String, v: f64, decimals: usize) {
    // OrcaSlicer's numbers: rounded at `decimals`, no trailing zeros, no zero before the point.
    let scale = 10i64.pow(decimals.min(9) as u32);
    let n = (v * scale as f64).round() as i64;
    if n < 0 {
        out.push('-');
    }
    let abs = n.unsigned_abs();
    let (whole, frac) = (abs / scale as u64, abs % scale as u64);
    if whole > 0 || frac == 0 {
        let _ = write!(out, "{whole}");
    }
    if frac > 0 {
        let start = out.len();
        let _ = write!(out, ".{frac:0decimals$}");
        let kept = out.get(start..).map_or(0, |t| t.trim_end_matches('0').len());
        out.truncate(start + kept);
    }
}

impl<'a> Writer<'a> {
    fn s(&self) -> &'a Settings {
        self.cx.settings
    }

    fn filament(&self) -> Option<&'a Filament> {
        self.cx.filaments.get(self.extruder).or_else(|| self.cx.filaments.first())
    }

    fn line(&mut self, text: impl Into<String>) {
        self.ops.push(Op::Text(text.into()));
    }

    fn mv(&mut self, to: DVec3, e: f64, feed: f64, kind: MoveKind) {
        let xy = self.xy_known;
        self.ops.push(Op::Move { to, e, feed, kind, arc: None, xy });
        self.pos = to;
    }

    fn travel_speed(&self) -> f64 {
        if self.layer == 0 { self.s().motion.first_layer_travel_speed } else { self.s().speeds.travel }
    }

    /// The acceleration and corner speed for a role (None: travel), as the settings ask.
    fn dynamics(&self, role: Option<Role>) -> (f64, f64) {
        let s = self.s();
        let (a, m) = (&s.accel, &s.motion);
        let accel = if self.layer == 0 && role.is_some() {
            a.first_layer
        } else {
            match role {
                None => a.travel,
                Some(Role::OuterWall | Role::OverhangWall) => a.outer,
                Some(Role::InnerWall) => a.inner,
                Some(Role::SparseInfill) => a.sparse,
                Some(Role::SolidInfill | Role::BottomSurface | Role::GapFill) => a.solid,
                Some(Role::TopSurface | Role::Ironing) => a.top,
                Some(Role::Bridge | Role::InternalBridge) => a.bridge,
                Some(_) => a.default,
            }
        };
        let jerk = if self.layer == 0 && role.is_some() {
            m.jerk_first_layer
        } else {
            match role {
                None => m.jerk_travel,
                Some(Role::OuterWall | Role::OverhangWall) => m.jerk_outer,
                Some(Role::InnerWall) => m.jerk_inner,
                Some(Role::TopSurface | Role::Ironing) => m.jerk_top,
                Some(
                    Role::SparseInfill
                    | Role::SolidInfill
                    | Role::BottomSurface
                    | Role::Bridge
                    | Role::InternalBridge
                    | Role::GapFill,
                ) => m.jerk_infill,
                Some(_) => m.jerk_default,
            }
        };
        (accel, jerk)
    }

    /// Sets acceleration and jerk for the next moves, when the printer takes them per feature.
    fn set_dynamics(&mut self, role: Option<Role>) {
        let m = &self.s().motion;
        let (accel, jerk) = self.dynamics(role);
        // Never ask for more than the printer's limits allow.
        let cap = if role.is_some() { m.limits.accel_extruding } else { m.limits.accel_travel };
        let accel = if m.emit_limits { accel.min(cap) } else { accel };
        if m.accel_control && (accel - self.accel).abs() > 0.5 {
            self.accel = accel;
            self.ops.push(Op::Accel(accel));
        }
        if m.jerk_default > 0.0 && jerk > 0.0 && (jerk - self.jerk).abs() > 0.01 {
            self.jerk = jerk;
            self.ops.push(Op::Jerk(jerk));
        }
    }

    /// OrcaSlicer sets the travel's acceleration before anything else: for a short hop onto an
    /// outer wall, already the wall's (or an overhang's, the bridges').
    fn set_travel_dynamics(&mut self, next: Role, short: bool) {
        let m = &self.s().motion;
        let first = self.layer == 0;
        let (accel, jerk) = match next {
            Role::OverhangWall if short && !first => (self.dynamics(Some(Role::Bridge)).0, self.dynamics(Some(next)).1),
            Role::OuterWall if short && !first => self.dynamics(Some(next)),
            _ => self.dynamics(None),
        };
        // Only firmware with a travel acceleration of its own has a limit on it.
        let separate = matches!(m.flavor, Flavor::Marlin2 | Flavor::RepRap | Flavor::Repetier);
        let accel = if m.emit_limits && separate { accel.min(m.limits.accel_travel) } else { accel };
        if m.accel_control && (accel - self.accel).abs() > 0.5 {
            self.accel = accel;
            self.ops.push(Op::Accel(accel));
        }
        if m.jerk_default > 0.0 && jerk > 0.0 && (jerk - self.jerk).abs() > 0.01 {
            self.jerk = jerk;
            self.ops.push(Op::Jerk(jerk));
        }
    }

    fn may_lift(&self) -> bool {
        let r = self.retraction();
        let z = self.print_z;
        r.z_hop > 0.0 && z >= r.lift_above && (r.lift_below <= 0.0 || z <= r.lift_below)
    }

    /// The retraction settings, with the retraction tower's length for this layer.
    fn retraction(&self) -> crate::settings::Retract {
        let mut r = self.s().retract;
        if let Some(f) = self.filament() {
            r = r.with(&f.retract);
        }
        if let Some(l) = self.calib.retract_length {
            r.length = l;
        }
        r
    }

    fn retract(&mut self) {
        let r = self.retraction();
        if self.retracted || r.length <= 0.0 {
            return;
        }
        self.preview.retractions.push([self.pos.x as f32, self.pos.y as f32, self.pos.z as f32]);
        let firmware = self.s().motion.firmware_retraction;
        let mut left = r.length;
        // Wipe back along the last path (OrcaSlicer's split): `retract_before_wipe` of the
        // length first, standing, then the rest spread over the wipe as it moves, but only as
        // much as the extruder can pull back at its retraction speed in the wipe's time; what
        // does not fit is retracted standing too.
        if r.wipe && r.wipe_distance > 0.0 && self.last_path.len() >= 2 {
            let distance = r.wipe_distance;
            let mut wipe: Vec<DVec2> = Vec::new();
            let mut travelled = 0.0;
            let mut at = self.pos.truncate();
            for &p in self.last_path.iter().rev().skip(1) {
                let d = at.distance(p);
                if d < 1e-6 {
                    continue;
                }
                if travelled + d >= distance {
                    wipe.push(at + (p - at) * ((distance - travelled) / d));
                    travelled = distance;
                    break;
                }
                travelled += d;
                wipe.push(p);
                at = p;
            }
            let m = &self.s().motion;
            let wipe_speed = if m.role_based_wipe && self.last_speed > 0.0 {
                self.last_speed
            } else {
                m.wipe_speed.of(self.s().speeds.travel)
            }
            .max(10.0);
            if travelled > 0.0 && !firmware {
                let during = (r.length * (1.0 - r.before_wipe)).min(r.speed * travelled / wipe_speed);
                let before = r.length - during;
                if before > 1e-6 {
                    let to = self.pos;
                    self.mv(to, -before, r.speed, MoveKind::Retract);
                    left -= before;
                }
                let per_mm = during / travelled;
                let mut from = self.pos.truncate();
                self.line(if self.s().bbl { "; WIPE_START" } else { ";WIPE_START" });
                for p in wipe {
                    let e = -per_mm * from.distance(p);
                    let z = self.pos.z;
                    self.mv(p.extend(z), e, wipe_speed, MoveKind::Wipe);
                    left += e;
                    from = p;
                }
                self.line(if self.s().bbl { "; WIPE_END" } else { ";WIPE_END" });
            } else if travelled > 0.0 {
                for p in wipe {
                    let z = self.pos.z;
                    self.mv(p.extend(z), 0.0, wipe_speed, MoveKind::Wipe);
                }
            }
        }
        if left > 1e-6 {
            if firmware {
                self.ops.push(Op::FirmwareRetract { retract: true, length: left, feed: r.speed });
            } else {
                let to = self.pos;
                self.mv(to, -left, r.speed, MoveKind::Retract);
            }
        }
        self.retracted = true;
        self.retracted_by = r.length;
        self.last_path.clear();
    }

    /// Z moves' speed: `travel_speed_z`, or the travel speed when it is 0.
    fn z_speed(&self) -> f64 {
        let v = self.s().speeds.travel_z;
        if v > 0.0 { v } else { self.travel_speed() }
    }

    /// The lift for travels between paths.
    fn lift_kind(&self) -> Lift {
        match self.retraction().z_hop_type {
            ZHop::Normal => Lift::Normal,
            ZHop::Spiral => Lift::Spiral,
            ZHop::Slope | ZHop::Auto => Lift::Slope,
        }
    }

    /// Asks for a lift with the next travel, unless lifted or not allowed here.
    fn want_lift(&mut self, kind: Lift) {
        if self.hop == 0.0 && self.to_lift.is_none() && self.may_lift() {
            self.to_lift = Some(kind);
        }
    }

    /// Writes what the object labels owe: the last object's end, this one's start.
    fn flush_labels(&mut self) {
        if let Some(t) = self.label_end.take() {
            self.line(t);
        }
        if let Some(t) = self.label_start.take() {
            self.line(t);
        }
    }

    /// A helix up by the hop at once (for custom G-code that runs lifted), OrcaSlicer's eager
    /// lift: a turn of the travel slope's pitch, centred to the head's right.
    fn eager_lift(&mut self) {
        let hop = self.retraction().z_hop;
        self.to_lift = None;
        if !self.may_lift() || hop <= 0.0 {
            return;
        }
        let slope = self.s().motion.travel_slope.to_radians();
        let radius = hop / (std::f64::consts::TAU * slope.atan());
        let feed = self.z_speed();
        let z = self.print_z + hop;
        self.ops.push(Op::Spiral { z, i: radius, j: 0.0, feed });
        self.pos.z = z;
        self.hop = hop;
    }

    /// OrcaSlicer's `travel_to_xyz`: rises as asked on the way to `to`, ending there at the lift
    /// height (or at the layer's height when `force_z`).
    fn travel_xyz(&mut self, to: DVec2, force_z: bool) {
        let speed = self.travel_speed();
        let nominal = self.print_z;
        if let Some(kind) = self.to_lift.take() {
            let hop = self.retraction().z_hop;
            let target = nominal + hop;
            let here = self.pos.truncate();
            let run = here.distance(to);
            self.hop = hop;
            if run > 0.0 && self.xy_known && !self.pos_unclear {
                let slope = self.s().motion.travel_slope.to_radians();
                match kind {
                    Lift::Spiral => {
                        let dir = (to - here) / run;
                        let radius = hop / (std::f64::consts::TAU * slope.atan());
                        let feed = self.z_speed();
                        self.ops.push(Op::Spiral { z: target, i: -dir.y * radius, j: dir.x * radius, feed });
                        self.pos.z = target;
                    }
                    Lift::Slope if hop.atan2(run) < slope => {
                        let top = here + (to - here) / run * (hop / slope.tan());
                        self.mv(top.extend(target), 0.0, speed, MoveKind::Travel);
                    }
                    Lift::Normal => {
                        let up = self.pos.truncate().extend(target);
                        let z_speed = self.z_speed();
                        self.mv(up, 0.0, z_speed, MoveKind::Travel);
                    }
                    Lift::Slope => {}
                }
            }
            if self.pos_unclear {
                self.mv(to.extend(self.pos.z), 0.0, speed, MoveKind::Travel);
                let z_speed = self.z_speed();
                self.mv(to.extend(target), 0.0, z_speed, MoveKind::Travel);
            } else {
                self.mv(to.extend(target), 0.0, speed, MoveKind::Lifted);
            }
        } else if !force_z {
            self.mv(to.extend(self.pos.z), 0.0, speed, MoveKind::Travel);
        } else if self.pos_unclear {
            self.mv(to.extend(self.pos.z), 0.0, speed, MoveKind::Travel);
            let z_speed = self.z_speed();
            self.mv(to.extend(nominal), 0.0, z_speed, MoveKind::Travel);
            self.hop = 0.0;
        } else {
            self.mv(to.extend(nominal), 0.0, speed, MoveKind::Lifted);
            self.hop = 0.0;
        }
        self.pos_unclear = false;
    }

    fn lift_now(&mut self) {
        if self.hop > 0.0 || !self.may_lift() {
            return;
        }
        let hop = self.retraction().z_hop;
        let to = self.pos + DVec3::Z * hop;
        self.mv(to, 0.0, self.z_speed(), MoveKind::Travel);
        self.hop = hop;
    }

    fn unretract(&mut self) {
        let r = self.retraction();
        if self.hop > 0.0 {
            let to = DVec3::new(self.pos.x, self.pos.y, self.print_z);
            self.mv(to, 0.0, self.z_speed(), MoveKind::Travel);
            self.hop = 0.0;
        }
        if self.retracted {
            // What was taken back, even if the length changed since (the retraction tower).
            let length = self.retracted_by + r.restart_extra;
            if self.s().motion.firmware_retraction {
                self.ops.push(Op::FirmwareRetract { retract: false, length, feed: r.deretract_speed });
            } else {
                let to = self.pos;
                self.mv(to, length, r.deretract_speed, MoveKind::Retract);
            }
            self.retracted = false;
        }
    }

    /// Moves to `to` without printing, to print a `next` path: inside the island when it can,
    /// routed round walls when asked to, retracting and lifting as OrcaSlicer does (on leaving
    /// an outer wall, on the way to a wall, and wherever the move leaves the part).
    fn travel(&mut self, to: DVec2, next: Role) {
        let from = self.pos.truncate();
        let d = from.distance(to);
        if d < 1.0e-4 && self.xy_known && !self.z_pending {
            return;
        }
        if !self.xy_known {
            // The first travel after the start G-code: straight there, lifted if lifting.
            self.retract();
            self.lift_now();
            self.flush_labels();
            self.xy_known = true;
            let speed = self.travel_speed();
            let at = to.extend(self.print_z + self.hop);
            self.mv(at, 0.0, speed, MoveKind::Travel);
            return;
        }
        let m = &self.s().motion;
        let mut route = vec![from, to];
        let mut inside = false;
        if let Some(r) = &self.region {
            if r.segment_inside(from, to) {
                inside = true;
            } else if m.avoid_crossing_walls
                && let Some(path) = r.route(from, to, m.max_detour)
            {
                route = path;
                inside = true;
            }
        }
        let length = geom::length(&route);
        let leaving_outer = matches!(self.role, Some(Role::OuterWall | Role::OverhangWall));
        let quiet = inside && m.reduce_infill_retraction && !next.is_wall() && self.s().density > 0.0;
        let min_travel = self.retraction().min_travel;
        let retract = length > min_travel && (leaving_outer || !quiet);
        self.set_travel_dynamics(next, length < min_travel);
        if retract {
            self.retract();
            let kind = self.lift_kind();
            self.want_lift(kind);
        } else {
            // Not wiping along an old path after a travel without retraction.
            self.last_path.clear();
        }
        self.flush_labels();
        // The wipe may have moved the head.
        if let Some(first) = route.first_mut() {
            *first = self.pos.truncate();
        }
        let force = std::mem::take(&mut self.z_pending);
        let speed = self.travel_speed();
        for (i, p) in route.iter().enumerate().skip(1) {
            if i == 1 {
                self.travel_xyz(*p, force);
            } else {
                self.mv(p.extend(self.pos.z), 0.0, speed, MoveKind::Travel);
            }
        }
        let pz = self.pos.z as f32;
        for w in route.windows(2) {
            self.preview.travels.push(([w[0].x as f32, w[0].y as f32, pz], [w[1].x as f32, w[1].y as f32, pz]));
        }
    }

    /// Whether `path` gets a scarf seam, by the seam settings.
    fn wants_scarf(&self, path: &Path) -> bool {
        let sm = &self.s().seams;
        let kind_ok = match path.role {
            Role::OuterWall => true,
            Role::InnerWall => sm.scarf_inner,
            _ => false,
        };
        if sm.scarf == Scarf::None || !kind_ok || !path.closed || path.points.len() < 3 || self.layer == 0 {
            return false;
        }
        if self.s().spiral {
            return false;
        }
        // "External": outer contours only, which run the wall direction's way round.
        let contour = (geom::signed_area2(&path.points) > 0.0) == self.s().walls.ccw;
        if sm.scarf == Scarf::External && !contour {
            return false;
        }
        // "Conditional": only where the seam is on a smooth stretch, not hidden in a corner.
        !sm.scarf_conditional || corner_angle(&path.points, 0) >= sm.scarf_angle
    }

    /// The moves that print `path` from its start: each segment with its overhang mark; loops
    /// close short of the start by the seam gap, or ramp in and out as a scarf seam.
    fn steps(&self, path: &Path, scarf: bool) -> Vec<Step> {
        let n = path.points.len();
        let plain = |to: DVec2, i: usize| Step {
            to,
            bend: if scarf { None } else { path.bend_at(i) },
            hang: path.hang_at(i),
            dz: 0.0,
            flow: 1.0,
            scarf: false,
            speed: 0.0,
        };
        let mut steps: Vec<Step> = (1..n).filter_map(|k| path.points.get(k).map(|&p| plain(p, k - 1))).collect();
        let Some(start) = path.start() else { return steps };
        if !path.closed {
            return steps;
        }
        steps.push(plain(start, n - 1));
        // Spiral vase: the wall climbs from the last layer's height to this one's as it goes
        // round, with no seam (and on the first spiral layer, the flow grows from nothing).
        if let Some(first_turn) = self.spiral.filter(|_| path.role == Role::OuterWall) {
            let total = path.length().max(1e-6);
            let h = self.height;
            let mut along = 0.0;
            let mut at = start;
            for st in &mut steps {
                along += at.distance(st.to);
                at = st.to;
                let f = (along / total).min(1.0);
                st.dz = -h * (1.0 - f);
                if first_turn {
                    st.flow = f.max(0.05);
                }
            }
            return steps;
        }
        let sm = &self.s().seams;
        if !scarf {
            // Stop a little short of the start, so the seam does not blob.
            trim_end(start, &mut steps, sm.gap);
            return steps;
        }
        let total = path.length();
        let ramp = if sm.scarf_entire_loop { total } else { sm.scarf_length.min(total) };
        let n_steps = sm.scarf_steps.max(1);
        if ramp < 1e-3 {
            return steps;
        }
        let h = self.height.max(1e-3);
        let rise = (h - sm.scarf_start_height).max(0.0);
        let piece = ramp / n_steps as f64;
        let cuts: Vec<f64> = (1..=n_steps).map(|q| q as f64 * piece).collect();
        let at = |d: f64| ((d / piece - 1e-6).ceil().max(1.0) as usize).min(n_steps);
        // Up the ramp: from the start height to the layer, the flow growing with the height.
        let frac = |q: usize| (q as f64 - 0.5) / n_steps as f64;
        let mut out: Vec<Step> = split_steps(start, &steps, &cuts)
            .into_iter()
            .map(|(st, d)| {
                if d <= ramp + 1e-6 {
                    let f = frac(at(d));
                    Step { dz: -rise * (1.0 - f), flow: (h - rise * (1.0 - f)) / h, scarf: true, ..st }
                } else {
                    st
                }
            })
            .collect();
        // Round again over the ramp at the layer height, laying what the ramp left out.
        out.extend(split_steps(start, &steps, &cuts).into_iter().take_while(|(_, d)| *d <= ramp + 1e-6).map(
            |(st, d)| {
                let f = frac(at(d));
                Step { dz: 0.0, flow: 1.0 - (h - rise * (1.0 - f)) / h, scarf: true, ..st }
            },
        ));
        out
    }

    /// Prints `path` from its first point (loops all the way round).
    fn extrude(&mut self, path: &Path) {
        let Some(start) = path.start() else { return };
        self.travel(start, path.role);
        if let Some(r) = self.pending_region.take() {
            self.region = Some(r);
        }
        self.flush_labels();
        self.unretract();
        if self.filament().is_none() {
            return;
        }
        let scarf = self.wants_scarf(path);
        let mut steps = self.steps(path, scarf);
        if !scarf {
            self.overhang_speeds(path, start, &mut steps);
        }
        let z = self.pos.z;
        // A scarf starts low: down to its first height before extruding.
        if let Some(first) = steps.first().filter(|st| st.dz < 0.0) {
            let to = start.extend(z + first.dz);
            self.mv(to, 0.0, self.z_speed(), MoveKind::Travel);
        }
        self.set_dynamics(Some(path.role));
        let mut all: Vec<DVec2> = Vec::with_capacity(steps.len() + 1);
        all.push(self.pos.truncate());
        // Runs of steps printed alike.
        let mut i = 0;
        while i < steps.len() {
            let Some(first) = steps.get(i) else { break };
            let key = (first.hang, first.scarf, first.speed.to_bits());
            let j = (i..steps.len())
                .find(|&j| steps.get(j).is_some_and(|st| (st.hang, st.scarf, st.speed.to_bits()) != key))
                .unwrap_or(steps.len());
            if let Some(run) = steps.get(i..j) {
                all.extend(run.iter().map(|st| st.to));
                self.run(path, run, z);
            }
            i = j;
        }
        // Back to the layer's height and the layer's fan.
        if (self.pos.z - z).abs() > 1e-9 {
            let to = self.pos.truncate().extend(z);
            self.mv(to, 0.0, self.z_speed(), MoveKind::Travel);
        }
        self.fan_for(0.0, Fan::Layer);
        if path.closed && path.role == Role::OuterWall {
            self.preview.seams.push([start.x as f32, start.y as f32, z as f32]);
        }
        // `retract` wipes down this list from its end: an open path back the way it came, a
        // loop on past its start, along its first edges.
        if path.closed {
            all.reverse();
        }
        self.last_path = all;
        if path.closed && path.role == Role::OuterWall && self.s().motion.wipe_on_loops && self.spiral.is_none() {
            self.wipe_inwards(path);
        }
    }

    /// "Wipe on loops": from the end of a loop, a short move into the part along its first
    /// edge, so the nozzle leaves the seam off the surface.
    fn wipe_inwards(&mut self, path: &Path) {
        let (Some(&p0), Some(&p1)) = (path.points.first(), path.points.get(1)) else { return };
        let v = (p1 - p0).normalize_or_zero();
        // Loops run with the part on their left (counter-clockwise walls), else their right.
        let side = if self.s().walls.ccw { 1.0 } else { -1.0 };
        let target = p0 + (v + v.perp() * side).normalize_or_zero() * (self.s().nozzle * 2.0);
        let here = self.pos.truncate();
        if v == DVec2::ZERO || !self.region.as_ref().is_some_and(|r| r.segment_inside(here, target)) {
            return;
        }
        let z = self.pos.z;
        let speed = if self.last_speed > 0.0 { self.last_speed } else { self.travel_speed() };
        self.mv(target.extend(z), 0.0, speed, MoveKind::Travel);
    }

    /// `ExtrusionQualityEstimator::estimate_extrusion_quality` and `GCode::_extrude`'s use of
    /// it: each point's distance past the layer below (with points added where the wall crosses
    /// that edge and near it), each move's speed from the overhang speed sections, and when any
    /// differs from the wall's own the wall is printed point by point at those speeds. When none
    /// does, the wall keeps its own speed throughout.
    fn overhang_speeds(&self, path: &Path, start: DVec2, steps: &mut Vec<Step>) {
        let s = self.s();
        let Some(below) = self.below.as_deref() else { return };
        if !path.role.is_wall() || !s.walls.overhang_speed || self.layer == 0 || below.is_empty() || steps.is_empty() {
            return;
        }
        let Some(fil) = self.filament() else { return };
        let width = path.width as f32;
        let area = bead_area(path.width, self.height) * path.flow;
        let volumetric = |v: f64| if area > 0.0 { v.min(fil.max_volumetric / (area * fil.flow_ratio)) } else { v };
        let mut reference = if path.role == Role::OuterWall { s.speeds.outer } else { s.speeds.inner };
        if reference <= 0.0 {
            reference = volumetric(f64::MAX);
        }
        let reference = volumetric(reference) as f32;
        let original = volumetric(path.speed) as f32;
        // Overlaps 90, 75, 50, 25, 13 and 0 %: the wall speed, each overhang speed (0 for none),
        // and the bridge speed.
        let pct = |v: f64| if v < 0.5 { 100.0 } else { v * 100.0 / f64::from(reference) };
        let speeds = [
            100.0,
            pct(s.walls.overhang_speeds[0].of(f64::from(reference))),
            pct(s.walls.overhang_speeds[1].of(f64::from(reference))),
            pct(s.walls.overhang_speeds[2].of(f64::from(reference))),
            pct(s.walls.overhang_speeds[3].of(f64::from(reference))),
            s.speeds.bridge * 100.0 / f64::from(reference),
        ];
        let mut sections: Vec<(f32, f32)> = [90.0f64, 75.0, 50.0, 25.0, 13.0, 0.0]
            .iter()
            .zip(speeds)
            .map(|(o, v)| ((f64::from(width) * (1.0 - o / 100.0)) as f32, (f64::from(reference) * v / 100.0) as f32))
            .collect();
        sections.sort_by(|a, b| a.0.total_cmp(&b.0).then(b.1.total_cmp(&a.1)));
        let mut last = (f32::INFINITY, 0.0f32);
        for sec in &mut sections {
            if sec.0 == last.0 {
                sec.1 = last.1;
            } else {
                last = *sec;
            }
        }
        let min_distance =
            sections.iter().filter(|sec| sec.1 <= original).map(|sec| sec.0).fold(f32::INFINITY, f32::min);
        let min_distance = if min_distance.is_finite() { min_distance } else { -1.0 };

        let mut input: Vec<DVec2> = vec![start];
        input.extend(steps.iter().map(|st| st.to));
        let points = overhang_points(below, &input, width, min_distance);
        let (Some(first), Some(last)) = (sections.first().copied(), sections.last().copied()) else { return };
        let calc = |d: f32| -> f32 {
            let v = if d <= first.0 {
                original
            } else if d >= last.0 {
                last.1
            } else {
                let mut k = 0;
                while sections.get(k + 1).is_some_and(|n| d > n.0) {
                    k += 1;
                }
                let (Some(a), Some(b)) = (sections.get(k), sections.get(k + 1)) else { return original };
                let t = ((d - a.0) / (b.0 - a.0)).clamp(0.0, 1.0);
                (1.0 - t) * a.1 + t * b.1
            };
            v.round()
        };
        let processed: Vec<(DVec2, f32, f32)> = (0..points.len())
            .map(|i| {
                let (p, d) = points.get(i).copied().unwrap_or((start, 0.0));
                let (_, dn) = points.get(i + 1).or(points.get(i)).copied().unwrap_or((start, 0.0));
                let speed = calc(d).min(calc(dn)).min(original);
                let overlap = (1.0 - d / width).min(1.0 - dn / width);
                (p, speed, overlap)
            })
            .collect();
        let varies = processed.iter().any(|q| (f64::from(q.1) - f64::from(original)).abs() > 1.0);
        if !varies {
            for st in steps.iter_mut() {
                st.hang = 0;
                st.speed = -1.0;
            }
            return;
        }
        // `_extrude`'s feed: a move at the speed of the point it leaves, changed only past a
        // millimetre a second, and back to the wall's own within one of it.
        let mut out = Vec::with_capacity(processed.len());
        let mut set = processed.first().map_or(original, |q| q.1);
        for i in 1..processed.len() {
            let (Some(prev), Some(here)) = (processed.get(i - 1), processed.get(i)) else { continue };
            let new = prev.1;
            if (set - new).abs() > 1.0 {
                set = new;
            } else if (original - new).abs() <= 1.0 {
                set = original;
            }
            let hang = ((1.0 - here.2.min(prev.2)) * 100.0).clamp(0.0, 255.0).round() as u8;
            out.push(Step {
                to: here.0,
                bend: None,
                hang,
                dz: 0.0,
                flow: 1.0,
                scarf: false,
                speed: f64::from(set.max(1.0)),
            });
        }
        *steps = out;
    }

    /// Prints a run of `path`'s steps that hang alike (and scarf or not).
    fn run(&mut self, path: &Path, run: &[Step], z: f64) {
        let Some(fil) = self.filament() else { return };
        let Some(first) = run.first() else { return };
        let s = self.s();
        let hang = if path.role.is_wall() { first.hang } else { 0 };
        let deg = crate::toolpath::degree_of(hang);
        // OrcaSlicer's overhang wall: the line's centre over half a nozzle past the layer below.
        let over_air = f64::from(hang) / 100.0 > 0.5 + s.nozzle / (2.0 * path.width.max(1e-6));
        let role = if over_air { Role::OverhangWall } else { path.role };
        if self.role != Some(role) {
            self.role = Some(role);
            let tag = if s.bbl { "; FEATURE: " } else { ";TYPE:" };
            self.line(format!("{tag}{}", role.name()));
        }
        // Orca compares and prints the float its path holds.
        if self.width as f32 != path.width as f32 {
            self.width = path.width;
            let tag = if s.bbl { "; LINE_WIDTH: " } else { ";WIDTH:" };
            self.line(format!("{tag}{}", general(f64::from(path.width as f32))));
        }
        let mut base = path.speed;
        if path.role == Role::OuterWall
            && let Some(v) = self.calib.outer_speed
        {
            base = v;
        }
        let mut flow = path.flow;
        if first.speed > 0.0 {
            base = first.speed;
        } else if first.speed == 0.0 && deg > 0 && s.walls.overhang_speed && self.layer > 0 {
            base = overhang_speed(f64::from(hang) / 100.0, base, &s.walls.overhang_speeds, s.speeds.bridge);
        }
        if over_air {
            flow *= s.walls.overhang_flow;
        }
        if first.scarf {
            base = s.seams.scarf_speed.of(base);
            flow *= s.seams.scarf_flow;
        }
        // The fan (as OrcaSlicer's cooling buffer): bridges and overhangs past the filament's
        // threshold get the overhang fan when it is higher than the layer's; internal bridges,
        // support interfaces and ironing their own speed when they have one.
        let ramp = self.fan_ramp;
        let (fan, kind) = match (fil.overhang_fan_from, role) {
            _ if ramp <= 0.0 => (0.0, Fan::Layer),
            (_, Role::SupportInterface) if fil.support_interface_fan.is_some() => {
                (fil.support_interface_fan.unwrap_or(0.0), Fan::Exact)
            }
            (_, Role::Ironing) if fil.ironing_fan.is_some() => (fil.ironing_fan.unwrap_or(0.0), Fan::Exact),
            (_, Role::InternalBridge) if fil.internal_bridge_fan.is_some() => {
                (fil.internal_bridge_fan.unwrap_or(0.0), Fan::Exact)
            }
            (Some(_), Role::InternalBridge | Role::Bridge) => (fil.overhang_fan * ramp, Fan::AtLeast),
            (Some(t), _)
                if path.role.is_wall() && ((t == 0 && path.role == Role::OuterWall) || (t > 0 && deg >= t)) =>
            {
                (fil.overhang_fan * ramp, Fan::AtLeast)
            }
            _ => (0.0, Fan::Layer),
        };
        self.fan_for(fan, kind);
        let h = self.height;
        let area = bead_area(path.width, h) * flow;
        // Cooling slows the layer down once it is written (`cool_layer`).
        let mut speed = base;
        // The hot end can only melt so much per second.
        if area > 0.0 {
            // The filament's flow ratio counts: it is what the hot end really melts.
            speed = speed.min(fil.max_volumetric / (area * fil.flow_ratio));
        }
        let speed = speed.max(1.0);
        let e_per_mm = area / fil.area() * fil.flow_ratio;
        self.last_speed = speed;
        // A wall whose overhang speed varies goes out point by point, in lines.
        let varies = first.speed > 0.0;
        let fit = !varies
            && !first.scarf
            && run.iter().all(|st| st.dz == 0.0)
            && match s.motion.arc_fitting {
                ArcFitting::All => true,
                ArcFitting::SparseInfill => path.role == Role::SparseInfill,
                ArcFitting::Off => false,
            };
        let from = self.pos.truncate();
        let mut pts: Vec<DVec2> = Vec::with_capacity(run.len() + 1);
        pts.push(from);
        pts.extend(run.iter().map(|st| st.to));
        // Each step's own height and flow (a scarf's), else straight or fitted arcs.
        let pieces: Vec<(Piece, f64, f64)> = if varies {
            run.iter().map(|st| (Piece::Line(st.to), st.dz, st.flow)).collect()
        } else if path.fitted && run.iter().all(|st| st.dz == 0.0) {
            // Neighbouring steps on one arc print as that arc.
            let mut out: Vec<(Piece, f64, f64)> = Vec::with_capacity(run.len());
            let mut last_id = None;
            for st in run {
                match st.bend {
                    Some(b) if last_id == Some(b.id) => {
                        if let Some((Piece::Arc { end, .. }, ..)) = out.last_mut() {
                            *end = st.to;
                        }
                    }
                    Some(b) => out.push((Piece::Arc { end: st.to, center: b.center, ccw: b.ccw }, st.dz, st.flow)),
                    None => out.push((Piece::Line(st.to), st.dz, st.flow)),
                }
                last_id = st.bend.map(|b| b.id);
            }
            out
        } else if fit {
            let tolerance = if path.role == Role::SparseInfill { 0.04 } else { s.resolution };
            arcs::fit(&pts, tolerance).into_iter().map(|p| (p, 0.0, 1.0)).collect()
        } else if run.iter().all(|st| st.dz == 0.0 && st.flow == 1.0) && self.spiral.is_none() {
            // Without arcs, OrcaSlicer still simplifies each path.
            geom::simplify(&pts, s.resolution).into_iter().skip(1).map(|p| (Piece::Line(p), 0.0, 1.0)).collect()
        } else {
            run.iter().map(|st| (Piece::Line(st.to), st.dz, st.flow)).collect()
        };
        let mut length = 0.0;
        let mut volume = 0.0;
        for (piece, dz, f) in pieces {
            let at = self.pos.truncate();
            let (end, arc, d) = match piece {
                Piece::Line(p) => (p, None, at.distance(p)),
                Piece::Arc { end, center, ccw } => (end, Some((center, ccw)), arcs::arc_length(at, end, center, ccw)),
            };
            if d < 1.0e-4 || f <= 0.0 {
                if d >= 1.0e-4 {
                    // Nothing to lay here: move on without extruding.
                    self.mv(end.extend(z + dz), 0.0, speed, MoveKind::Travel);
                }
                continue;
            }
            length += d * f;
            volume += d * area * f;
            self.ops.push(Op::Move {
                to: end.extend(z + dz),
                e: d * e_per_mm * f,
                feed: speed,
                kind: MoveKind::Extrude(role),
                arc,
                xy: true,
            });
            self.pos = end.extend(z + dz);
        }
        if let Some(mm) = self.stats.filament_mm.get_mut(self.extruder) {
            *mm += length * e_per_mm;
        }
        if let Some(v) = self.stats.volume_mm3.get_mut(self.extruder) {
            *v += volume;
        }
        if let Some(v) = self.stats.volume_by_role.get_mut(role.index()) {
            *v += volume;
        }
        // The Preview draws a simplified copy, so huge prints stay within memory.
        let shown =
            if self.preview_tolerance > 0.0 { geom::simplify(&pts, self.preview_tolerance) } else { pts.clone() };
        let pz = (z + first.dz) as f32;
        for pair in shown.windows(2) {
            let (a, b) = (pair[0], pair[1]);
            self.preview.segments.push(Segment {
                a: [a.x as f32, a.y as f32, pz],
                b: [b.x as f32, b.y as f32, pz],
                role,
                width: path.width as f32,
                height: h as f32,
                speed: speed as f32,
                fan: fan as f32,
                temp: self.temp as f32,
                extruder: self.extruder as u8,
                flow: (area * speed) as f32,
            });
            self.seg_fans.push((fan, kind));
        }
    }

    /// Picks where a loop starts, by the seam setting, and rotates it to start there. `depth`
    /// is the wall's depth (0 outer), for staggered inner seams.
    fn place_seam(&mut self, path: &mut Path, object: Option<usize>, depth: Option<usize>) {
        if !path.closed || path.points.len() < 2 {
            return;
        }
        let here = self.pos.truncate();
        if self.spiral.is_none()
            && let Some(table) = object.and_then(|o| self.cx.seams.get(o)).and_then(Option::as_ref)
            && table.place(self.layer, path, here, self.s().seams.staggered_inner)
        {
            return;
        }
        let seam = self.s().seam.clone();
        let hint = object.and_then(|o| self.seams.get(o).copied().flatten());
        // Painted seams win: the best point inside an enforcer, never one inside a blocker.
        let (enforce, block) = (&self.seam_paint.0, &self.seam_paint.1);
        let allowed = |p: DVec2| !geom::contains(block, p);
        let painted = (!enforce.is_empty() || !block.is_empty())
            .then(|| {
                let target = hint.unwrap_or(here);
                let inside: Vec<usize> = (0..path.points.len())
                    .filter(|&i| path.points.get(i).is_some_and(|p| geom::contains(enforce, *p) && allowed(*p)))
                    .collect();
                if !inside.is_empty() {
                    return best_seam_among(path, target, &inside);
                }
                let free: Vec<usize> =
                    (0..path.points.len()).filter(|&i| path.points.get(i).is_some_and(|p| allowed(*p))).collect();
                if free.is_empty() || free.len() == path.points.len() {
                    None
                } else {
                    best_seam_among(path, target, &free)
                }
            })
            .flatten();
        // A spiral carries straight on from where the last turn ended.
        let painted = if self.spiral.is_some() {
            (0..path.points.len()).min_by(|&a, &b| {
                let d = |i: usize| path.points.get(i).map_or(f64::INFINITY, |p| p.distance_squared(here));
                d(a).total_cmp(&d(b))
            })
        } else {
            painted
        };
        let index = match (painted, seam.as_str()) {
            (Some(i), _) => i,
            (None, seam) => match seam {
                "nearest" => best_seam(path, here),
                "back" => rear_index(&path.points),
                "random" => {
                    let n = path.points.len();
                    ((self.layer as u64).wrapping_mul(2_654_435_761).wrapping_add(n as u64 * 40503) % n as u64) as usize
                }
                // Aligned: in a corner near the seam of the layer below, starting from the back.
                _ => {
                    let target =
                        hint.unwrap_or_else(|| path.points.get(rear_index(&path.points)).copied().unwrap_or(here));
                    best_seam(path, target)
                }
            },
        };
        path.rotate(index);
        // Staggered inner seams: each wall in starts a little further back.
        if let Some(d) = depth.filter(|d| *d > 0 && self.s().seams.staggered_inner && path.role == Role::InnerWall) {
            stagger(path, d as f64 * path.width);
        }
        if path.role == Role::OuterWall
            && let (Some(o), Some(&p)) = (object, path.points.first())
            && let Some(slot) = self.seams.get_mut(o)
        {
            *slot = Some(p);
        }
    }

    /// Prints paths nearest first, reversing open lines when their far end is nearer. Ordered
    /// groups print whole, in their order, from wherever the first of them is reached.
    /// Orca's `extrude_infill`: the regions chained, then the lines within each unless they keep
    /// their order; ironing last.
    /// `extrude_infill`: a sortable fill goes in with its paths loose (`Region::append`), so
    /// all of them are chained together; lines that keep their order go as one piece.
    fn print_fills(&mut self, paths: Vec<Path>, object: usize) {
        let (ironing, fills): (Vec<Path>, Vec<Path>) = paths.into_iter().partition(|p| p.role == Role::Ironing);
        for paths in [fills, ironing] {
            let mut sets: Vec<Vec<Path>> = Vec::new();
            for p in paths {
                match sets.iter_mut().find(|s| p.group != 0 && s.first().is_some_and(|f| f.group == p.group)) {
                    Some(set) => set.push(p),
                    None => sets.push(vec![p]),
                }
            }
            let ends: Vec<_> = sets
                .iter()
                .map(|s| {
                    (
                        s.first().map_or([0.0; 2], |p| end_point(p, true)),
                        s.last().map_or([0.0; 2], |p| end_point(p, false)),
                    )
                })
                .collect();
            let turns: Vec<bool> = sets.iter().map(|s| s.iter().all(|p| p.group == 0 && !p.one_way)).collect();
            let mut sets: Vec<Option<Vec<Path>>> = sets.into_iter().map(Some).collect();
            for (i, reversed) in crate::chain::chain(&ends, &turns, nm_point(self.pos.truncate())) {
                let Some(set) = sets.get_mut(i).and_then(Option::take) else { continue };
                for mut p in set {
                    if reversed && !p.closed {
                        p.reverse();
                    }
                    self.place_seam(&mut p, Some(object), None);
                    self.extrude(&p);
                }
            }
        }
    }

    /// A brim as Orca prints its collection: loops chained from here by their first points
    /// (`chained_path_from`), each split where the head is nearest it (`ExtrusionLoop::split_at`).
    fn print_brim(&mut self, paths: Vec<Path>) {
        if paths.is_empty() {
            return;
        }
        let ends: Vec<_> = paths.iter().map(|p| (end_point(p, true), end_point(p, true))).collect();
        let order = crate::chain::chain(&ends, &vec![true; paths.len()], nm_point(self.pos.truncate()));
        let mut paths: Vec<Option<Path>> = paths.into_iter().map(Some).collect();
        for (i, _) in order {
            let Some(mut p) = paths.get_mut(i).and_then(Option::take) else { continue };
            split_loop_at(&mut p, self.pos.truncate());
            self.extrude(&p);
        }
    }

    fn print_nearest(&mut self, paths: Vec<Path>, object: Option<usize>, depth: Option<usize>) {
        let mut groups: Vec<Vec<Path>> = Vec::new();
        let mut free: Vec<Path> = Vec::with_capacity(paths.len());
        for p in paths {
            if p.group == 0 {
                free.push(p);
            } else if let Some(g) = groups.iter_mut().find(|g| g.first().is_some_and(|f| f.group == p.group)) {
                g.push(p);
            } else {
                groups.push(vec![p]);
            }
        }
        loop {
            let here = self.pos.truncate();
            let mut best = (f64::INFINITY, 0usize, false);
            for (i, p) in free.iter().enumerate() {
                if p.closed {
                    let d = p.points.iter().map(|q| q.distance_squared(here)).fold(f64::INFINITY, f64::min);
                    if d < best.0 {
                        best = (d, i, false);
                    }
                } else {
                    let (Some(s), Some(e)) = (p.points.first(), p.points.last()) else { continue };
                    let (ds, de) = (s.distance_squared(here), e.distance_squared(here));
                    if ds.min(de) < best.0 {
                        best = (ds.min(de), i, de < ds);
                    }
                }
            }
            let nearest_group = groups
                .iter()
                .enumerate()
                .filter_map(|(i, g)| g.first().and_then(|p| p.start()).map(|s| (s.distance_squared(here), i)))
                .min_by(|a, b| a.0.total_cmp(&b.0));
            match nearest_group {
                Some((d, gi)) if d <= best.0 => {
                    let g = groups.swap_remove(gi);
                    for mut p in g {
                        self.place_seam(&mut p, object, depth);
                        self.extrude(&p);
                    }
                }
                _ if free.is_empty() => break,
                _ => {
                    let mut p = free.swap_remove(best.1);
                    if best.2 {
                        p.reverse();
                    }
                    self.place_seam(&mut p, object, depth);
                    self.extrude(&p);
                }
            }
        }
    }

    /// The islands layer `plan` prints with filament `e`, object by object.
    fn objects_in(&mut self, plan: &LayerPlan, e: usize, n_ext: usize, labels: bool) {
        let cx = self.cx;
        let m = &cx.settings.motion;
        for (o, islands) in &plan.objects {
            let own = cx.object_extruder.get(*o).copied().unwrap_or(0);
            let mut remaining: Vec<&Island> =
                islands.iter().filter(|isl| isl.extruder.unwrap_or(own).min(n_ext - 1) == e).collect();
            if remaining.is_empty() {
                continue;
            }
            let name = cx.object_names.get(*o).map_or_else(String::new, |n| n.replace(['\n', '\r'], " "));
            let bbl = cx.settings.bbl;
            if cx.settings.label_objects {
                self.line(format!("; printing object {name} id:{o} copy 0"));
            }
            // OrcaSlicer opens and closes object labels with the travels round them.
            if labels && bbl {
                self.label_start =
                    Some(format!("; start printing object, unique label id: {o}\nM624 {}", label_mask(*o)));
            } else if labels && m.flavor == Flavor::Klipper && m.exclude_object {
                self.label_start = Some(format!("EXCLUDE_OBJECT_START NAME={}", label(&name, *o)));
            } else if labels && matches!(m.flavor, Flavor::Marlin | Flavor::Marlin2) {
                self.label_start = Some(format!("M486 S{o}"));
            }
            while !remaining.is_empty() {
                let here = self.pos.truncate();
                let next = remaining
                    .iter()
                    .enumerate()
                    .map(|(i, isl)| {
                        let d = isl
                            .walls
                            .first()
                            .and_then(|ws| ws.first())
                            .or_else(|| isl.fills.first())
                            .map_or(f64::INFINITY, |p| {
                                p.points.iter().map(|q| q.distance_squared(here)).fold(f64::INFINITY, f64::min)
                            });
                        (i, d)
                    })
                    .min_by(|a, b| a.1.total_cmp(&b.1))
                    .map_or(0, |x| x.0);
                let island = remaining.swap_remove(next);
                self.island(island, *o);
            }
            if cx.settings.label_objects {
                self.line(format!("; stop printing object {name} id:{o} copy 0"));
            }
            if labels && bbl {
                self.label_end = Some(format!("; stop printing object, unique label id: {o}\nM625"));
            } else if labels && m.flavor == Flavor::Klipper && m.exclude_object {
                self.label_end = Some(format!("EXCLUDE_OBJECT_END NAME={}", label(&name, *o)));
            } else if labels && matches!(m.flavor, Flavor::Marlin | Flavor::Marlin2) {
                self.label_end = Some("M486 S-1".to_owned());
            }
        }
    }

    /// One tower path, printed as tower.
    fn tower_path(&mut self, points: Vec<DVec2>, closed: bool) {
        let Some(t) = &self.tower else { return };
        let speed = self.s().speeds.inner.min(90.0);
        let speed = if self.layer == 0 { self.s().speeds.first_layer } else { speed };
        let path = Path::new(Role::PrimeTower, points, closed, t.w, speed, 1.0);
        self.print_nearest(vec![path], None, None);
    }

    /// The tower's outline on this layer (and its brim on the first), once a layer.
    fn tower_walls(&mut self) {
        let layer = self.layer;
        let brim = self.s().multi.tower_brim;
        let Some(t) = self.tower.as_mut() else { return };
        if t.walled == Some(layer) || layer > t.last {
            return;
        }
        t.walled = Some(layer);
        t.used = 0.0;
        let (at, width, depth, w) = (t.at, t.width, t.depth, t.w);
        let rect = |inset: f64| {
            let (lo, hi) = (at + DVec2::splat(inset), at + DVec2::new(width, depth) - DVec2::splat(inset));
            vec![lo, DVec2::new(hi.x, lo.y), hi, DVec2::new(lo.x, hi.y)]
        };
        if layer == 0 {
            let loops = (brim / w).round() as usize;
            for k in (1..=loops.min(50)).rev() {
                self.tower_path(rect(-(k as f64) * w + w / 2.0), true);
            }
        }
        self.tower_path(rect(w / 2.0), true);
    }

    /// Purges `volume` mm³ into the tower: rows across it, from where the last purge stopped.
    fn tower_purge(&mut self, volume: f64) {
        self.tower_walls();
        let h = self.height.max(0.01);
        let Some(t) = &self.tower else { return };
        let (at, width, depth, w, used) = (t.at, t.width, t.depth, t.w, t.used);
        let span = (width - 2.0 * w).max(w);
        let rows = ((volume / (h * w * span)).ceil() as usize).clamp(1, 2000);
        let room = ((depth - 2.0 * w - used) / w).floor().max(0.0) as usize;
        let rows = rows.min(room.max(1));
        let mut pts = Vec::with_capacity(rows * 2);
        for k in 0..rows {
            let y = at.y + w + used + (k as f64 + 0.5) * w;
            let (a, b) = (at.x + w, at.x + w + span);
            if k % 2 == 0 {
                pts.extend([DVec2::new(a, y), DVec2::new(b, y)]);
            } else {
                pts.extend([DVec2::new(b, y), DVec2::new(a, y)]);
            }
        }
        if let Some(t) = self.tower.as_mut() {
            t.used += rows as f64 * w;
        }
        self.tower_path(pts, false);
    }

    /// Ends a tower layer: walls if nothing purged, and sparse rows over the rest of it so the
    /// layers above have something to stand on.
    fn tower_finish(&mut self) {
        let layer = self.layer;
        if self.tower.as_ref().is_none_or(|t| layer > t.last) {
            return;
        }
        self.tower_walls();
        let Some(t) = &self.tower else { return };
        let (at, width, depth, w, used) = (t.at, t.width, t.depth, t.w, t.used);
        let gap = w * 3.0;
        let mut y = at.y + w + used + gap / 2.0;
        let mut pts = Vec::new();
        let mut k = 0;
        while y < at.y + depth - w && k < 1000 {
            let (a, b) = (at.x + w, at.x + width - w);
            if k % 2 == 0 {
                pts.extend([DVec2::new(a, y), DVec2::new(b, y)]);
            } else {
                pts.extend([DVec2::new(b, y), DVec2::new(a, y)]);
            }
            y += gap;
            k += 1;
        }
        if pts.len() >= 2 {
            self.tower_path(pts, false);
        }
    }

    fn island(&mut self, island: &Island, object: usize) {
        self.below = island.below.clone();
        if let Some(k) = island.pressure_advance {
            let line = crate::calib::pressure_advance(self.s().motion.flavor, k, self.extruder);
            self.line(line);
        }
        // Arriving uses the plain rules; travel within the island uses its region from the
        // first extrusion on.
        self.region = None;
        // Made ahead for the coming layers in parallel (`regions`), or here.
        let key = std::ptr::from_ref(island) as usize;
        self.pending_region = match self.regions.remove(&key) {
            Some(r) => r,
            None => Region::new(&island.outline, self.s().nozzle),
        };
        self.seam_paint = (island.seam_enforce.clone(), island.seam_block.clone());
        let depth = island.walls.len();
        let order: Vec<usize> = if self.s().outer_first { (0..depth).collect() } else { (0..depth).rev().collect() };
        for k in order {
            if let Some(ws) = island.walls.get(k) {
                self.print_nearest(ws.clone(), Some(object), Some(k));
            }
        }
        self.print_fills(island.fills.clone(), object);
        self.region = None;
        self.pending_region = None;
    }

    /// Sets the fan for what is printed next, unless it already is.
    fn fan_for(&mut self, fan: f64, kind: Fan) {
        let same = match kind {
            Fan::Layer => self.out_fan.1 == Fan::Layer,
            _ => self.out_fan.1 == kind && (self.out_fan.0 - fan).abs() < 0.5,
        };
        if !same {
            self.out_fan = (fan, kind);
            self.ops.push(Op::Fan(fan, kind));
        }
    }

    /// Layer cooling, as OrcaSlicer's cooling buffer does it once a layer is written: a layer
    /// quicker than the filament's slow-down time has its fastest extrusions slowed to one
    /// common speed until it takes that long (none below the minimum speed, outer walls kept
    /// when the filament says so), and the layer's fan follows the time it then takes. Moves
    /// are timed as the buffer times them, length over feed.
    fn cool_layer(&mut self, (from, start): (usize, DVec3), f: &Filament, seg_start: usize) {
        let adjustable = |role: Role, feed: f64| {
            feed > f.slow_down_min_speed && !(f.keep_outer_wall_speed && role == Role::OuterWall)
        };
        let mut pos = start;
        let mut total = 0.0;
        let mut moves: Vec<(f64, f64)> = Vec::new();
        // As OrcaSlicer's cooling buffer: nothing before the layer's first extrusion counts. Its
        // parser also carries the last E word over to lines without one, so each speed line
        // (one starts every path and wipe) reads as an extruder move of that length: a few
        // hundredths of a second per layer that decide the slowed-down speed.
        let mut extruded = false;
        let mut last_e = 0.0f64;
        let mut before = None;
        for op in self.ops.get(from..).unwrap_or(&[]) {
            if let Op::Spiral { z, i, j, feed } = op {
                if extruded && *feed > 0.0 {
                    total += (TAU * i.hypot(*j)).hypot(z - pos.z) / feed;
                }
                pos.z = *z;
                before = None;
            }
            if let Op::Move { to, e, feed, kind, arc, xy } = op {
                let d = if *xy { *to - pos } else { DVec3::new(0.0, 0.0, to.z - pos.z) };
                let xyz = match arc {
                    Some((c, ccw)) if *xy => arcs::arc_length(pos.truncate(), to.truncate(), *c, *ccw).hypot(d.z),
                    _ => d.length(),
                };
                let len = if xyz > 1e-9 { xyz } else { e.abs() };
                pos = *to;
                let starts = match kind {
                    MoveKind::Extrude(_) | MoveKind::Wipe => before != Some((*kind, *feed)),
                    _ => false,
                };
                extruded |= matches!(kind, MoveKind::Extrude(_));
                if *feed > 0.0 && extruded {
                    total += len / feed;
                    if starts {
                        total += last_e.abs() / feed;
                    }
                }
                if let MoveKind::Extrude(role) = kind
                    && adjustable(*role, *feed)
                {
                    moves.push((len, *feed));
                    if starts {
                        moves.push((last_e.abs(), *feed));
                    }
                }
                if *e != 0.0 {
                    last_e = *e;
                }
                before = Some((*kind, *feed));
            }
        }
        let target = f.slow_down_layer_time * 1.001;
        let mut cap = f64::INFINITY;
        let mut time = total;
        if f.slow_down && total < target && !moves.is_empty() {
            cap = slow_down_cap(&moves, target - total, f.slow_down_min_speed).unwrap_or(f.slow_down_min_speed);
            time += moves.iter().map(|(len, v)| len / v.min(cap) - len / v).sum::<f64>();
            // OrcaSlicer writes a slowed-down feed as whole mm/min.
            cap = (cap * 60.0).round() / 60.0;
        }
        let fan = layer_fan(f, time, self.fan_ramp);
        for op in self.ops.get_mut(from..).into_iter().flatten() {
            match op {
                Op::Move { feed, kind: MoveKind::Extrude(role), .. } if adjustable(*role, *feed) => {
                    *feed = feed.min(cap);
                }
                Op::Fan(speed, Fan::Layer) => *speed = fan,
                Op::Fan(speed, Fan::AtLeast) => *speed = speed.max(fan),
                _ => {}
            }
        }
        let fans = std::mem::take(&mut self.seg_fans);
        for (sg, (asked, kind)) in self.preview.segments.get_mut(seg_start..).into_iter().flatten().zip(fans) {
            let v = f64::from(sg.speed);
            if adjustable(sg.role, v) && v > cap {
                sg.flow *= (cap / v) as f32;
                sg.speed = cap as f32;
            }
            sg.fan = match kind {
                Fan::Layer => fan,
                Fan::AtLeast => asked.max(fan),
                Fan::Exact => asked,
            } as f32;
        }
    }

    /// A fixed fan speed, outside the layers' cooling (the end of the print).
    fn set_fan(&mut self, fan: f64) {
        self.out_fan = (fan, Fan::Exact);
        self.ops.push(Op::Fan(fan, Fan::Exact));
    }

    fn pressure_advance(&mut self) {
        let Some(k) = self.filament().and_then(|f| f.pressure_advance) else { return };
        let line = match self.s().motion.flavor {
            Flavor::Klipper => format!("SET_PRESSURE_ADVANCE ADVANCE={}", f5(k)),
            Flavor::RepRap => format!("M572 D{} S{}", self.extruder, f5(k)),
            _ => format!("M900 K{}", f5(k)),
        };
        self.line(line);
    }

    /// Changes to filament `extruder`, running `custom` (the rendered change template); a
    /// template that sends its own `T` command replaces ours.
    fn tool_change(&mut self, extruder: usize, custom: &str) {
        if extruder == self.extruder {
            return;
        }
        let length = self.s().retract.toolchange_length;
        if length > 0.0 && !self.retracted {
            let to = self.pos;
            self.mv(to, -length, self.s().retract.speed, MoveKind::Retract);
        }
        // Ooze prevention: the hotend going idle drops to its standby temperature.
        if self.s().multi.ooze_prevention {
            let standby = self.temp + self.s().multi.standby_delta;
            self.line(format!("M104 T{} S{}", self.extruder, standby.round() as i64));
        }
        self.extruder = extruder;
        let own_t = custom.lines().any(|l| {
            let l = l.trim_start();
            l.strip_prefix('T').is_some_and(|rest| rest.starts_with(|c: char| c.is_ascii_digit() || c == '['))
        });
        if !custom.trim().is_empty() {
            self.line(custom.trim_end().to_owned());
        }
        if !own_t {
            self.line(format!("T{extruder}"));
        }
        if let Some(f) = self.filament() {
            let t = if self.layer == 0 { f.nozzle_temp_first } else { f.nozzle_temp };
            self.temp = t;
            self.line(format!("M109 S{}", t.round() as i64));
        }
        self.pressure_advance();
        if length > 0.0 {
            let to = self.pos;
            self.mv(to, length, self.s().retract.deretract_speed, MoveKind::Retract);
        }
        self.retracted = false;
        self.role = None;
    }
}

/// `estimate_points_properties<true, true, true, true>` without curvature: `input`'s points,
/// each with its signed distance past `below` plus half the line, and the points Orca adds.
fn overhang_points(
    below: &crate::overhang::Boundary,
    input: &[DVec2],
    width: f32,
    min_distance: f32,
) -> Vec<(DVec2, f32)> {
    const EPSILON: f32 = 1.0e-4;
    let offset = 0.5 * width;
    let min_spacing = f64::from(width) * 0.25;
    let dist = |p: DVec2| (below.signed(p) + f64::from(offset)) as f32;
    let mut points: Vec<(DVec2, f32)> = Vec::with_capacity(input.len() * 2);
    let Some(&first) = input.first() else { return points };
    points.push((first, dist(first)));
    for &next in input.iter().skip(1) {
        let d = dist(next);
        if let Some(&(prev, pd)) = points.last()
            && (pd > offset + EPSILON) != (d > offset + EPSILON)
        {
            for c in below.crossings(prev, next) {
                if c.distance(prev) > min_spacing && next.distance(c) > min_spacing {
                    points.push((c, offset));
                }
            }
        }
        points.push((next, d));
    }
    let mut out: Vec<(DVec2, f32)> = Vec::with_capacity(points.len() * 2);
    let Some(&head) = points.first() else { return out };
    out.push(head);
    for pair in points.windows(2) {
        let (curr, next) = (pair[0], pair[1]);
        let near = |d: f32| d > -offset && d < offset + 2.0;
        if near(curr.1) || near(next.1) {
            let len = curr.0.distance(next.0);
            let far = |d: f32| d.abs() > min_distance;
            if (min_distance > 0.0 && (far(curr.1) || far(next.1)) && len >= 2.0) || (min_distance <= 0.0 && len > 4.0)
            {
                let a0 = (f64::from(curr.1 + 3.0 * offset) / len).clamp(0.0, 1.0);
                let a1 = (f64::from(1.0f32) - f64::from(next.1 + 3.0 * offset) / len).clamp(0.0, 1.0);
                let (t0, t1) = (a0.min(a1), a0.max(a1));
                let mut add = |t: f64| {
                    let p = curr.0 + (next.0 - curr.0) * t;
                    let raw = below.signed(p);
                    if (raw.abs() > f64::from(min_distance) || min_distance <= 0.0)
                        && p.distance(curr.0) > min_spacing
                        && next.0.distance(p) > min_spacing
                    {
                        out.push((p, (raw + f64::from(offset)) as f32));
                    }
                };
                if t0 < 1.0 {
                    add(t0);
                }
                if t1 > 0.0 {
                    add(t1);
                }
            }
        }
        out.push(next);
    }
    out
}

/// `ExtrusionLoop::split_at` without overhang preference: the loop starts at the point of it
/// nearest `here`, snapped to a vertex within `SCALED_EPSILON`.
fn split_loop_at(path: &mut Path, here: DVec2) {
    let n = path.points.len();
    if n < 2 {
        return;
    }
    let mut best = (f64::INFINITY, 0usize, here);
    for i in 0..n {
        let (Some(&a), Some(&b)) = (path.points.get(i), path.points.get((i + 1) % n)) else { continue };
        let ab = b - a;
        let l2 = ab.length_squared();
        let t = if l2 > 0.0 { ((here - a).dot(ab) / l2).clamp(0.0, 1.0) } else { 0.0 };
        let q = a + ab * t;
        let d = q.distance_squared(here);
        if d < best.0 {
            best = (d, i, q);
        }
    }
    let (_, i, q) = best;
    let (Some(&a), Some(&b)) = (path.points.get(i), path.points.get((i + 1) % n)) else { return };
    let eps = 1.0e-4;
    let (da, db) = (here.distance_squared(a), here.distance_squared(b));
    let q = if da < db && da < eps * eps {
        a
    } else if db <= da && db < eps * eps {
        b
    } else {
        q
    };
    if q == a {
        path.rotate(i);
    } else if q == b {
        path.rotate((i + 1) % n);
    } else {
        path.start_on_segment(i, q);
    }
}

/// One move of a path as printed: to `to`, `dz` below the layer, at `flow` of the full bead.
#[derive(Clone, Copy, Debug)]
struct Step {
    to: DVec2,
    /// The segment's fitted arc.
    bend: Option<crate::toolpath::Bend>,
    /// How much of the line hangs past the layer below, percent.
    hang: u8,
    dz: f64,
    flow: f64,
    scarf: bool,
    /// Orca's processed overhang speed for this move, mm/s; 0 to work it out from `hang`, below
    /// 0 to keep the path's own.
    speed: f64,
}

/// A wall's speed where `share` of its line hangs past the layer below (OrcaSlicer's dynamic
/// overhang speed): the wall speed up to 10 %, then on to each overhang speed at 25, 50, 75 and
/// 87 % (0 keeps the wall speed) and the bridge speed when all of it hangs, in straight lines
/// between; never faster than `original`.
fn overhang_speed(share: f64, original: f64, speeds: &[crate::settings::RelSpeed; 4], bridge: f64) -> f64 {
    const AT: [f64; 6] = [0.10, 0.25, 0.50, 0.75, 0.87, 1.0];
    let level = |k: usize| -> f64 {
        match k {
            0 => original,
            5 => bridge,
            _ => speeds.get(k - 1).map_or(original, |sp| sp.of(original)),
        }
    };
    let v = if share <= AT[0] {
        original
    } else if share >= AT[5] {
        level(5)
    } else {
        let k = AT.windows(2).position(|w| share <= w[1]).unwrap_or(4);
        let (a, b) = (AT.get(k).copied().unwrap_or(0.1), AT.get(k + 1).copied().unwrap_or(1.0));
        let t = ((share - a) / (b - a)).clamp(0.0, 1.0);
        (1.0 - t) * level(k) + t * level(k + 1)
    };
    v.round().min(original).max(1.0)
}

/// Splits steps starting at `from` where they pass the distances `cuts` (ascending), giving
/// each step with the distance along at its end.
fn split_steps(from: DVec2, steps: &[Step], cuts: &[f64]) -> Vec<(Step, f64)> {
    let mut out = Vec::with_capacity(steps.len() + cuts.len());
    let (mut at, mut dist, mut c) = (from, 0.0, 0usize);
    for st in steps {
        let len = at.distance(st.to);
        while let Some(&cut) = cuts.get(c) {
            if cut <= dist + 1e-9 {
                c += 1;
            } else if cut < dist + len - 1e-9 {
                let mut to = at + (st.to - at) * ((cut - dist) / len);
                // Still on its arc (`ArcSegment::clip_end`).
                if let Some(b) = st.bend {
                    let radius = st.to.distance(b.center);
                    to = b.center + (to - b.center).normalize_or_zero() * radius;
                }
                out.push((Step { to, ..*st }, cut));
                c += 1;
            } else {
                break;
            }
        }
        dist += len;
        out.push((*st, dist));
        at = st.to;
    }
    out
}

/// Takes `gap` mm off the end of the steps that start at `from`.
fn trim_end(from: DVec2, steps: &mut Vec<Step>, gap: f64) {
    let mut left = gap;
    while left > 1e-9 && steps.len() > 1 {
        let n = steps.len();
        let (Some(last), Some(before)) = (steps.get(n - 1).copied(), steps.get(n - 2).map(|s| s.to)) else { return };
        let seg = before.distance(last.to);
        if seg > left {
            if let Some(l) = steps.last_mut() {
                l.to = last.to + (before - last.to) * (left / seg);
            }
            return;
        }
        left -= seg;
        steps.pop();
    }
    // A single step: shorten it unless that would leave nothing.
    if left > 1e-9
        && let Some(l) = steps.first_mut()
    {
        let seg = from.distance(l.to);
        if seg > left * 2.0 {
            l.to = l.to + (from - l.to) * (left / seg);
        }
    }
}

/// The angle at point `i` of a loop between its two edges, degrees: 180 on a straight.
fn corner_angle(points: &[DVec2], i: usize) -> f64 {
    let n = points.len();
    let (Some(&p), Some(&prev), Some(&next)) = (points.get(i), points.get((i + n - 1) % n), points.get((i + 1) % n))
    else {
        return 180.0;
    };
    let (a, b) = ((prev - p).normalize_or_zero(), (next - p).normalize_or_zero());
    if a == DVec2::ZERO || b == DVec2::ZERO {
        return 180.0;
    }
    a.angle_to(b).abs().to_degrees()
}

/// Where a loop's seam hides best near `target`: concave corners most, sharp convex corners
/// less, never on an overhang (OrcaSlicer's seam placer scores points the same way).
fn best_seam(path: &Path, target: DVec2) -> usize {
    let all: Vec<usize> = (0..path.points.len()).collect();
    best_seam_among(path, target, &all).unwrap_or(0)
}

/// How badly a seam at point `i` shows: distance from `target`, less how well a corner hides
/// it, more if it hangs.
fn seam_cost(path: &Path, target: DVec2, i: usize, orient: f64) -> f64 {
    let pts = &path.points;
    let n = pts.len();
    let (Some(&p), Some(&prev), Some(&next)) = (pts.get(i), pts.get((i + n - 1) % n), pts.get((i + 1) % n)) else {
        return f64::INFINITY;
    };
    let (a, b) = ((p - prev).normalize_or_zero(), (next - p).normalize_or_zero());
    // Positive turning into the part (convex), negative away from it (concave).
    let turn = a.perp_dot(b).atan2(a.dot(b)) * orient;
    let hidden = if turn < 0.0 { -turn * 3.0 } else { turn * 1.2 };
    let hanging = path.overhang_at(i).max(path.overhang_at((i + n - 1) % n));
    p.distance(target) - hidden + if hanging >= 2 { 20.0 } else { 0.0 }
}

/// [`best_seam`] among the points `allowed`.
fn best_seam_among(path: &Path, target: DVec2, allowed: &[usize]) -> Option<usize> {
    let orient = if geom::signed_area2(&path.points) >= 0.0 { 1.0 } else { -1.0 };
    allowed.iter().map(|&i| (i, seam_cost(path, target, i, orient))).min_by(|a, b| a.1.total_cmp(&b.1)).map(|(i, _)| i)
}

/// Moves a loop's start `back` mm backwards along it.
fn stagger(path: &mut Path, back: f64) {
    let n = path.points.len();
    let mut left = back.min(path.length() * 0.5);
    let mut k = 0usize;
    while left > 1e-9 {
        // Segment from point k-1 (backwards) to point k.
        let prev = (k + n - 1) % n;
        let (Some(&p), Some(&q)) = (path.points.get(k), path.points.get(prev)) else { return };
        let seg = p.distance(q);
        if seg > left {
            let cut = p + (q - p) * (left / seg);
            // A new point on segment `prev`, which keeps its overhang mark.
            path.bends.clear();
            path.points.insert(prev + 1, cut);
            if path.overhang.len() == n {
                let d = path.hang_at(prev);
                path.overhang.insert(prev + 1, d);
            }
            path.rotate(prev + 1);
            return;
        }
        left -= seg;
        k = prev;
        if k == 0 {
            return;
        }
    }
    path.rotate(k);
}

/// The rear-most point, leftmost among equals.
fn rear_index(points: &[DVec2]) -> usize {
    points
        .iter()
        .enumerate()
        .max_by(|a, b| (a.1.y, -a.1.x).partial_cmp(&(b.1.y, -b.1.x)).unwrap_or(std::cmp::Ordering::Equal))
        .map_or(0, |(i, _)| i)
}

/// Object `o`'s bit, as the 64-bit mask in base 64 that Bambu Lab's M624 takes.
fn label_mask(o: usize) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mask = if o < 64 { 1u64 << o } else { 0 };
    let bytes = mask.to_le_bytes();
    let mut out = String::with_capacity(12);
    for chunk in bytes.chunks(3) {
        let b0 = u32::from(chunk.first().copied().unwrap_or(0));
        let b1 = u32::from(chunk.get(1).copied().unwrap_or(0));
        let b2 = u32::from(chunk.get(2).copied().unwrap_or(0));
        let n = (b0 << 16) | (b1 << 8) | b2;
        for (i, shift) in [18u32, 12, 6, 0].into_iter().enumerate() {
            let c = ALPHABET.get(((n >> shift) & 63) as usize).copied().unwrap_or(b'A');
            out.push(if i > chunk.len() { '=' } else { char::from(c) });
        }
    }
    out
}

fn fan_command(percent: f64) -> String {
    let s = (percent.clamp(0.0, 100.0) * 2.55).round() as i64;
    format!("M106 S{s}")
}

fn has_command(gcode: &str, words: &[&str]) -> bool {
    gcode.lines().any(|l| {
        let code = l.split(';').next().unwrap_or("").trim_start();
        words.iter().any(|w| code.strip_prefix(w).is_some_and(|rest| rest.is_empty() || rest.starts_with([' ', '\t'])))
    })
}

pub fn hms(t: f64) -> String {
    let t = t.max(0.0).round() as u64;
    let (h, m, s) = (t / 3600, (t / 60) % 60, t % 60);
    if h > 0 {
        format!("{h}h {m}m {s}s")
    } else if m > 0 {
        format!("{m}m {s}s")
    } else {
        format!("{s}s")
    }
}

/// A name the firmware's object commands accept.
fn label(name: &str, i: usize) -> String {
    let clean: String =
        name.chars().map(|c| if c.is_ascii_alphanumeric() || c == '.' || c == '-' { c } else { '_' }).collect();
    format!("{clean}_id_{i}_copy_0")
}

/// The machine limits, as the firmware expects them at the start.
/// The machine limits, as OrcaSlicer's `print_machine_envelope` writes them.
fn limits_gcode(s: &Settings) -> Vec<String> {
    let m = &s.motion;
    let l = &m.limits;
    let int = |v: f64| (v + 0.5).trunc() as i64;
    let factor = if m.flavor == Flavor::RepRap { 60.0 } else { 1.0 };
    if !matches!(m.flavor, Flavor::RepRap | Flavor::Marlin | Flavor::Marlin2) {
        return Vec::new();
    }
    let extruding = int(l.accel_extruding);
    let mut v = vec![
        format!(
            "M201 X{} Y{} Z{} E{}",
            int(l.max_accel[0]),
            int(l.max_accel[1]),
            int(l.max_accel[2]),
            int(l.max_accel[3])
        ),
        format!(
            "M203 X{} Y{} Z{} E{}",
            int(l.max_speed[0] * factor),
            int(l.max_speed[1] * factor),
            int(l.max_speed[2] * factor),
            int(l.max_speed[3] * factor)
        ),
        match m.flavor {
            Flavor::RepRap => {
                format!("M204 P{} T{} ; sets acceleration (P, T), mm/sec^2", extruding, int(l.accel_travel))
            }
            Flavor::Marlin2 => format!(
                "M204 P{} R{} T{} ; sets acceleration (P, T) and retract acceleration (R), mm/sec^2",
                extruding,
                int(l.accel_retracting),
                int(l.accel_travel)
            ),
            // Legacy Marlin has one acceleration for printing and travel.
            _ => format!("M204 P{} R{} T{}", extruding, int(l.accel_retracting), extruding),
        },
    ];
    let jerk = |i: usize| format!("{:.2}", l.jerk[i] * factor);
    v.push(if m.flavor == Flavor::RepRap {
        format!("M566 X{} Y{} Z{} E{} ; sets the jerk limits, mm/min", jerk(0), jerk(1), jerk(2), jerk(3))
    } else {
        format!("M205 X{} Y{} Z{} E{} ; sets the jerk limits, mm/sec", jerk(0), jerk(1), jerk(2), jerk(3))
    });
    if m.flavor == Flavor::Marlin2 && l.junction_deviation > 0.0 {
        v.push(format!("M205 J{:.3}", l.junction_deviation));
    }
    v
}

fn accel_gcode(flavor: Flavor, a: f64, to_decel: Option<f64>) -> String {
    match flavor {
        Flavor::Klipper => match to_decel {
            Some(f) => format!("SET_VELOCITY_LIMIT ACCEL={} ACCEL_TO_DECEL={}", f3(a), f3(a * f)),
            None => format!("SET_VELOCITY_LIMIT ACCEL={}", f3(a)),
        },
        Flavor::RepRap => format!("M204 P{} T{}", f3(a), f3(a)),
        Flavor::Repetier => format!("M201 X{} Y{}", f3(a), f3(a)),
        Flavor::Marlin | Flavor::Marlin2 => format!("M204 S{}", f3(a)),
    }
}

fn jerk_gcode(flavor: Flavor, j: f64) -> String {
    match flavor {
        Flavor::Klipper => format!("SET_VELOCITY_LIMIT SQUARE_CORNER_VELOCITY={}", f3(j)),
        Flavor::RepRap => format!("M566 X{} Y{}", f3(j * 60.0), f3(j * 60.0)),
        _ => format!("M205 X{} Y{}", f3(j), f3(j)),
    }
}

/// Writes the G-code for `plans`.
pub fn emit(plans: &[LayerPlan], cx: &Context<'_>) -> Output {
    let s = cx.settings;
    let m = &s.motion;
    let n_ext = cx.filaments.len().max(1);
    let mut w = Writer {
        cx,
        calib: Default::default(),
        retracted_by: 0.0,
        // About one move a point, plus travels, retractions and comments.
        ops: Vec::with_capacity(path_points(plans) / 5 * 4 + plans.len() * 256),
        pos: DVec3::ZERO,
        retracted: false,
        hop: 0.0,
        role: None,
        width: 0.0,
        extruder: cx.object_extruder.first().copied().unwrap_or(0).min(n_ext - 1),
        to_lift: None,
        z_pending: false,
        pos_unclear: false,
        lifted_for_picture: false,
        label_end: None,
        label_start: None,
        out_fan: (0.0, Fan::Exact),
        fan_ramp: 0.0,
        seg_fans: Vec::new(),
        temp: 0.0,
        layer: 0,
        print_z: 0.0,
        height: 0.0,
        accel: 0.0,
        jerk: 0.0,
        stats: Stats {
            filament_mm: vec![0.0; n_ext],
            volume_mm3: vec![0.0; n_ext],
            weight_g: vec![0.0; n_ext],
            cost: vec![0.0; n_ext],
            layers: plans.len(),
            ..Default::default()
        },
        preview: Preview::default(),
        seams: vec![None; cx.object_names.len()],
        seam_paint: (Vec::new(), Vec::new()),
        tower: None,
        spiral: None,
        preview_tolerance: preview_tolerance(plans),
        region: None,
        pending_region: None,
        regions: Default::default(),
        xy_known: false,
        last_path: Vec::new(),
        last_speed: 0.0,
        below: None,
        warnings: Vec::new(),
    };
    let first_fil = cx.filaments.get(w.extruder).cloned();
    let (nozzle_first, bed_first, bed) = first_fil.as_ref().map_or((200.0, 60.0, 60.0), |f| {
        // The bed runs at the hottest the filaments in use want.
        let bed_first = cx.filaments.iter().map(|f| f.bed_temp_first).fold(f.bed_temp_first, f64::max);
        let bed = cx.filaments.iter().map(|f| f.bed_temp).fold(f.bed_temp, f64::max);
        (f.nozzle_temp_first, bed_first, bed)
    });
    w.temp = nozzle_first;

    // ---- start ----
    let mut error: Option<String> = None;
    let mut render = |what: &str, template: &str, layer: &[(&str, Value)]| -> String {
        match cx.macros.render(what, template, layer) {
            Ok(t) => t,
            Err(e) => {
                error.get_or_insert(e);
                String::new()
            }
        }
    };
    let start = render("Machine start G-code", &s.gcode.start, &[]);
    w.line("; EXECUTABLE_BLOCK_START");
    // Object labels go before the start G-code, so the firmware knows them from the start.
    let labels = m.label_objects || m.exclude_object;
    if labels && m.flavor == Flavor::Klipper && m.exclude_object {
        for (i, (name, outline)) in cx.object_names.iter().zip(cx.object_outlines).enumerate() {
            let n = outline.len().max(1) as f64;
            let c = outline.iter().fold(DVec2::ZERO, |a, p| a + *p) / n;
            let poly: Vec<String> = outline.iter().map(|p| format!("[{},{}]", f3(p.x), f3(p.y))).collect();
            w.line(format!(
                "EXCLUDE_OBJECT_DEFINE NAME={} CENTER={},{} POLYGON=[{}]",
                label(name, i),
                f3(c.x),
                f3(c.y),
                poly.join(",")
            ));
        }
    } else if labels && !s.bbl && matches!(m.flavor, Flavor::Marlin | Flavor::Marlin2) {
        for (i, name) in cx.object_names.iter().enumerate() {
            w.line(format!("M486 S{i}"));
            w.line(format!("M486 A{}", label(name, i)));
            w.line("M486 S-1");
        }
    }
    // The first progress line.
    w.ops.push(Op::Layer(usize::MAX));
    if m.emit_limits {
        for l in limits_gcode(s) {
            w.line(l);
        }
    }
    if s.gcode.auxiliary_fan && first_fil.as_ref().is_some_and(|f| f.fan_off_layers > 0) {
        w.line("M106 S0");
        w.line("M106 P2 S0");
    }
    let klipper = m.flavor == Flavor::Klipper;
    let sets_bed = has_command(&start, &["M140", "M190"]);
    let sets_nozzle = has_command(&start, &["M104", "M109"]);
    if !klipper && !sets_bed {
        w.line(format!("M190 S{}", bed_first.round() as i64));
    }
    if !klipper && !sets_nozzle {
        w.line(format!("M104 S{}", nozzle_first.round() as i64));
    }
    w.line(if s.bbl { "; FEATURE: Custom" } else { ";TYPE:Custom" });
    w.line(start.trim_end().to_owned());
    let filament_vars = [("filament_extruder_id", Value::Num(w.extruder as f64)), ("layer_num", Value::Num(0.0))];
    let first_start =
        first_fil.as_ref().map_or(String::new(), |f| render("Filament start G-code", &f.start_gcode, &filament_vars));
    if s.bbl {
        w.line(first_start.trim_end().to_owned());
        w.line(format!(";VT{}", w.extruder));
        w.pressure_advance();
    }
    // Start G-code that does not heat the nozzle: nothing else will wait for it.
    if !sets_nozzle {
        w.line(format!("M109 S{}", nozzle_first.round() as i64));
    }
    w.line("G90");
    w.line("G21");
    if s.gcode.relative_e {
        w.line("M83 ; use relative distances for extrusion");
    } else {
        w.line("M82 ; use absolute distances for extrusion");
        w.line("G92 E0");
    }
    if n_ext > 1 {
        let e = w.extruder;
        w.line(format!("T{e}"));
    }
    if !s.bbl {
        w.line(first_start.trim_end().to_owned());
        w.pressure_advance();
    } else {
        w.line("M981 S1 P20000 ;open spaghetti detector");
    }

    // The prime tower: as deep as the layer with the most purging needs, up to the last change.
    if s.multi.tower && n_ext > 1 {
        let mut current = w.extruder;
        let mut most: f64 = 0.0;
        let mut last = None;
        let tw = s.widths.inner;
        let span = (s.multi.tower_width - 2.0 * tw).max(tw);
        for (li, plan) in plans.iter().enumerate() {
            let mut volume = 0.0;
            for (_, e) in layer_steps(plan, cx, n_ext, current) {
                if e != current {
                    volume += flush_volume(cx, current, e);
                    current = e;
                }
            }
            if volume > 0.0 {
                last = Some(li);
                most = most.max(volume / (plan.height.max(0.01) * span));
            }
        }
        if let Some(last) = last {
            let depth = (most + 4.0 * tw).clamp(5.0, 300.0);
            w.tower = Some(Tower {
                at: DVec2::new(s.multi.tower_at[0], s.multi.tower_at[1]),
                width: s.multi.tower_width,
                depth,
                w: tw,
                used: 0.0,
                walled: None,
                last,
            });
        }
    }

    let mut highest: f64 = 0.0;
    let mut spiral_started = false;
    let mut toolchanges = 0usize;
    for (li, plan) in plans.iter().enumerate() {
        w.layer = li;
        // The travel regions of the next layers' islands, made in parallel (one at a time,
        // they were most of the G-code stage).
        if li % REGIONS_AHEAD == 0 {
            use rayon::prelude::*;
            let nozzle = s.nozzle;
            let ahead: Vec<&Island> = plans
                .iter()
                .skip(li)
                .take(REGIONS_AHEAD)
                .flat_map(|p| p.objects.iter().flat_map(|(_, isl)| isl))
                .collect();
            w.regions =
                ahead.par_iter().map(|i| (std::ptr::from_ref(*i) as usize, Region::new(&i.outline, nozzle))).collect();
        }
        // Spiral vase: above the solid bottom, on layers that are a single loop.
        let single = plan.objects.len() == 1
            && plan.objects.first().is_some_and(|(_, isl)| {
                isl.len() == 1 && isl.first().is_some_and(|i| i.walls.iter().flatten().count() == 1)
            });
        w.spiral = (s.spiral && li >= s.bottom_layers.max(1) && single).then_some(!spiral_started);
        spiral_started |= w.spiral.is_some();
        w.height = plan.height;
        let seg_start = w.preview.segments.len();
        let travel_start = w.preview.travels.len();
        let seam_start = w.preview.seams.len();
        let retract_start = w.preview.retractions.len();
        let prev_print_z = w.print_z;
        w.print_z = plan.print_z;
        let layer_vars = [
            ("layer_num", Value::Num(li as f64)),
            ("layer_z", Value::Num(plan.print_z)),
            ("previous_layer_z", Value::Num(prev_print_z)),
        ];
        // The next object, one at a time: up clear of everything printed, across, then down.
        if plan.new_object {
            w.retract();
            let safe = (highest + 2.0).min(s.max_print_height.max(highest));
            let up = DVec3::new(w.pos.x, w.pos.y, safe);
            w.mv(up, 0.0, w.z_speed(), MoveKind::Travel);
            let start = plan
                .objects
                .iter()
                .flat_map(|(_, isl)| isl)
                .find_map(|isl| isl.walls.first().and_then(|ws| ws.first()).or_else(|| isl.fills.first()))
                .and_then(|p| p.points.first().copied());
            if let Some(p) = start {
                w.mv(DVec3::new(p.x, p.y, safe), 0.0, s.speeds.travel, MoveKind::Travel);
            }
        }
        highest = highest.max(plan.print_z);
        w.line(if s.bbl { "; CHANGE_LAYER" } else { ";LAYER_CHANGE" });
        w.line(format!("{}{}", if s.bbl { "; Z_HEIGHT: " } else { ";Z:" }, f3(plan.print_z)));
        w.line(format!("{}{}", if s.bbl { "; LAYER_HEIGHT: " } else { ";HEIGHT:" }, f3(plan.height)));
        let before = render("Before layer change G-code", &s.gcode.before_layer, &layer_vars);
        if !before.is_empty() {
            w.line(before);
        }
        w.ops.push(Op::Layer(li));
        // No Z move here: as in OrcaSlicer the first travel of the layer takes it there.
        if w.retraction().on_layer_change {
            w.retract();
            let kind = if w.retraction().z_hop_type == ZHop::Auto { Lift::Spiral } else { w.lift_kind() };
            w.want_lift(kind);
        }
        if let Some(t) = w.label_end.take() {
            w.line(t);
        }
        w.z_pending = true;
        // Still up from the picture at the end of the last layer, for the first travel to come down.
        if !std::mem::take(&mut w.lifted_for_picture) {
            w.hop = 0.0;
        }
        if !s.bbl && !s.gcode.timelapse.is_empty() {
            let t = render("Timelapse G-code", &s.gcode.timelapse, &layer_vars);
            w.line(t);
        }
        let after = render("Layer change G-code", &s.gcode.layer_change, &layer_vars);
        if !after.is_empty() {
            w.line(after);
        }
        if let Some(c) = cx.calib {
            let jd = s.motion.limits.junction_deviation > 0.0;
            w.calib = c.layer(li, plans.len(), plan.print_z, s.motion.flavor, w.extruder, jd);
            for g in w.calib.gcode.clone() {
                w.line(g);
            }
        }
        // After the first layer: printing temperatures.
        if li == 1 {
            for (i, f) in cx.filaments.iter().enumerate() {
                if (f.nozzle_temp - f.nozzle_temp_first).abs() > 0.5 {
                    let tool = if n_ext > 1 { format!(" T{i}") } else { String::new() };
                    w.line(format!("M104 S{}{tool}", f.nozzle_temp.round() as i64));
                }
            }
            if let Some(f) = cx.filaments.get(w.extruder) {
                w.temp = f.nozzle_temp;
            }
            if (bed - bed_first).abs() > 0.5 {
                w.line(format!("M140 S{}", bed.round() as i64));
            }
        }
        // The layer's fan; its speed is settled with the layer's cooling, once it is written.
        w.line(";_SET_FAN_SPEED_CHANGING_LAYER");
        let fil = w.filament().cloned();
        w.fan_ramp = fil.as_ref().map_or(0.0, |f| fan_ramp(f, li));
        let cool_from = (w.ops.len(), w.pos);
        w.seg_fans.clear();
        w.out_fan = (0.0, Fan::Exact);
        w.fan_for(0.0, Fan::Layer);
        // Bambu Lab's core-XY printers with one nozzle take their timelapse picture here, lifted
        // on a helix; the rest at the end of the layer or before leaving the pictured nozzle.
        let traditional = (s.bbl || !s.gcode.timelapse.is_empty()) && ((s.i3 && !s.spiral) || s.multi.nozzles > 1);
        let mut pictured = false;
        let snapshot = |w: &mut Writer<'_>, render: &mut Render<'_>| {
            if w.retraction().on_layer_change {
                w.retract();
                w.eager_lift();
            }
            w.flush_labels();
            if s.gcode.timelapse.is_empty() {
                return;
            }
            let curr = cx.logical_extruder(w.extruder);
            let at = cx.timelapse.map_or((0, 0), |t| t.pick(plan.print_z, cx.most_used_extruder, curr));
            let mut vars = layer_vars.to_vec();
            vars.extend([
                ("max_layer_z", Value::Num(highest)),
                ("most_used_physical_extruder_id", Value::Num(cx.physical(cx.most_used_extruder) as f64)),
                ("curr_physical_extruder_id", Value::Num(cx.physical(curr) as f64)),
                ("timelapse_pos_x", Value::Num(at.0 as f64)),
                ("timelapse_pos_y", Value::Num(at.1 as f64)),
                ("has_timelapse_safe_pos", Value::Bool(at != (0, 0))),
            ]);
            let t = render("Timelapse G-code", &s.gcode.timelapse, &vars);
            w.line(t);
            w.pos_unclear = true;
        };
        if s.bbl && !traditional {
            snapshot(&mut w, &mut render);
        }

        // Pauses, colour changes and custom G-code asked for at this layer.
        for (_, ev) in cx.events.iter().filter(|(z, _)| (z - plan.print_z).abs() < plan.height * 0.5) {
            match ev {
                LayerEvent::Pause => {
                    let t = render("Pause G-code", &s.gcode.pause, &layer_vars);
                    w.line(if t.trim().is_empty() { "M601".to_owned() } else { t.trim_end().to_owned() });
                }
                LayerEvent::ColorChange => w.line("M600"),
                LayerEvent::Custom(text) => {
                    let t = render("Custom G-code", text, &layer_vars);
                    w.line(t.trim_end().to_owned());
                }
            }
        }

        // Skirt and brim, then support and the objects grouped by filament.
        if !plan.skirt.is_empty() {
            let (brim, skirt): (Vec<Path>, Vec<Path>) =
                plan.skirt.iter().cloned().partition(|p| p.role == Role::Brim && p.closed && p.fitted);
            w.print_nearest(skirt, None, None);
            w.print_brim(brim);
        }
        let steps = layer_steps(plan, cx, n_ext, w.extruder);
        for (part, e) in steps {
            if e != w.extruder {
                let leaving_pictured = cx.logical_extruder(w.extruder) == cx.most_used_extruder;
                if traditional && !pictured && s.multi.nozzles == 2 && leaving_pictured {
                    snapshot(&mut w, &mut render);
                    pictured = true;
                }
                let from = w.extruder;
                toolchanges += 1;
                let change = crate::macros::ToolChange {
                    from,
                    to: e,
                    layer: li,
                    z: plan.print_z,
                    max_z: highest,
                    // With a prime tower OrcaSlicer purges at least 100 mm³ in the change itself
                    // (Bambu Lab's purge chute), in up to four parts.
                    flush: if w.tower.is_some() {
                        flush_volume(cx, from, e).max(100.0)
                    } else {
                        flush_volume(cx, from, e)
                    },
                    at: w.pos.to_array(),
                    count: toolchanges,
                };
                let vars = change.vars(s, cx.filaments);
                let custom = render("Change filament G-code", &s.gcode.change_filament, &vars);
                w.tool_change(e, &custom);
                if w.tower.is_some() {
                    w.tower_purge(flush_volume(cx, from, e));
                }
            }
            match part {
                Part::Support(interface) => {
                    let paths: Vec<Path> = plan
                        .support
                        .iter()
                        .filter(|p| (p.role == Role::SupportInterface) == interface)
                        .cloned()
                        .collect();
                    if !paths.is_empty() {
                        w.print_nearest(paths, None, None);
                    }
                }
                Part::Islands => w.objects_in(plan, e, n_ext, labels),
            }
        }
        if w.tower.is_some() {
            w.tower_finish();
        }
        if traditional && !pictured {
            snapshot(&mut w, &mut render);
            w.lifted_for_picture = w.hop > 0.0;
        }
        if let Some(f) = &fil {
            w.cool_layer(cool_from, f, seg_start);
        }
        w.preview.layers.push(PreviewLayer {
            z: plan.print_z as f32,
            height: plan.height as f32,
            segments: seg_start..w.preview.segments.len(),
            travels: travel_start..w.preview.travels.len(),
            seams: seam_start..w.preview.seams.len(),
            retractions: retract_start..w.preview.retractions.len(),
            time: 0.0,
        });
    }

    // ---- end ----
    w.retract();
    w.flush_labels();
    w.set_fan(0.0);
    if s.gcode.auxiliary_fan {
        w.line("M106 P2 S0");
    }
    if s.bbl {
        w.line("M981 S0 P20000 ; close spaghetti detector");
    }
    w.line(if s.bbl { "; FEATURE: Custom" } else { ";TYPE:Custom" });
    let last_layer = plans.len().saturating_sub(1) as f64;
    for (i, f) in cx.filaments.iter().enumerate() {
        let vars = [("filament_extruder_id", Value::Num(i as f64)), ("layer_num", Value::Num(last_layer))];
        let text = render("Filament end G-code", &f.end_gcode, &vars);
        if !text.trim().is_empty() {
            w.line(text.trim_end().to_owned());
        }
    }
    let end = render("Machine end G-code", &s.gcode.end, &[]);
    w.line(end.trim_end().to_owned());

    // ---- time, then text ----
    let times = time_ops(&w.ops, s);
    let mut stats = std::mem::take(&mut w.stats);
    let mut layer_times = vec![0.0f64; plans.len()];
    let mut layer: Option<usize> = None;
    for (op, t) in w.ops.iter().zip(&times) {
        match op {
            Op::Layer(i) => layer = Some(*i),
            Op::Move { kind: MoveKind::Extrude(role), .. } => {
                if let Some(slot) = stats.time_by_role.get_mut(role.index()) {
                    *slot += t;
                }
            }
            Op::Move { .. } | Op::FirmwareRetract { .. } | Op::Spiral { .. } => stats.travel_time += t,
            // Custom G-code: the start and end sequences, layer change, filament change.
            Op::Text(_) => {
                if let Some(slot) = stats.time_by_role.get_mut(Role::Custom.index()) {
                    *slot += t;
                }
            }
            _ => {}
        }
        match layer.and_then(|i| layer_times.get_mut(i)) {
            Some(slot) => *slot += t,
            None => stats.prepare_time += t,
        }
    }
    stats.total_time = times.iter().sum();
    for (pl, t) in w.preview.layers.iter_mut().zip(&layer_times) {
        pl.time = *t as f32;
    }
    for (i, f) in cx.filaments.iter().enumerate() {
        let v = stats.volume_mm3.get(i).copied().unwrap_or(0.0);
        let g = v / 1000.0 * f.density;
        if let Some(slot) = stats.weight_g.get_mut(i) {
            *slot = g;
        }
        if let Some(slot) = stats.cost.get_mut(i) {
            *slot = g / 1000.0 * f.cost;
        }
    }
    let first_layer_time = layer_times.first().copied().unwrap_or(0.0) + stats.prepare_time;
    let mut header = String::new();
    let _ = writeln!(header, "; HEADER_BLOCK_START");
    let _ = writeln!(header, "; generated by CosmicSlicer {}", env!("CARGO_PKG_VERSION"));
    if s.bbl {
        let _ = writeln!(
            header,
            "; model printing time: {}; total estimated time: {}",
            hms(stats.total_time - stats.prepare_time),
            hms(stats.total_time)
        );
        let _ = writeln!(header, "; estimated first layer printing time (normal mode) = {}", hms(first_layer_time));
    }
    let _ = writeln!(header, "; total layer number: {}", plans.len());
    if s.bbl {
        let ids: Vec<String> = (0..cx.object_names.len()).map(|i| i.to_string()).collect();
        let _ = writeln!(header, "; model label id: {}", ids.join(","));
    }
    let list = |f: &dyn Fn(&Filament) -> f64| {
        cx.filaments.iter().map(|x| format!("{:.2}", f(x))).collect::<Vec<_>>().join(",")
    };
    let _ = writeln!(header, "; filament_density: {}", plain_list(cx.filaments.iter().map(|f| f.density)));
    let _ = writeln!(header, "; filament_diameter: {}", list(&|f| f.diameter));
    let _ = writeln!(header, "; max_z_height: {:.2}", plans.last().map_or(0.0, |p| p.print_z));
    let _ = writeln!(
        header,
        "; filament: {}",
        (1..=cx.filaments.len()).map(|i| i.to_string()).collect::<Vec<_>>().join(",")
    );
    let _ = writeln!(header, "; HEADER_BLOCK_END");
    header.push('\n');
    if s.bbl {
        header.push_str(&config_block(cx, s));
    }
    let mut gcode = header;
    gcode.push_str(&cx.thumbnails);
    if !s.bbl {
        let widths = &s.widths;
        let _ = writeln!(gcode, "; external perimeters extrusion width = {:.2}mm", widths.outer);
        let _ = writeln!(gcode, "; perimeters extrusion width = {:.2}mm", widths.inner);
        let _ = writeln!(gcode, "; infill extrusion width = {:.2}mm", widths.sparse);
        let _ = writeln!(gcode, "; solid infill extrusion width = {:.2}mm", widths.solid);
        let _ = writeln!(gcode, "; top infill extrusion width = {:.2}mm", widths.top);
        if plans.iter().any(|p| !p.support.is_empty()) {
            let _ = writeln!(gcode, "; support material extrusion width = {:.2}mm", widths.support);
        }
        let _ = writeln!(gcode, "; first layer extrusion width = {:.2}mm", widths.first_layer);
        gcode.push('\n');
    }
    render_ops(&mut gcode, &w.ops, &times, stats.total_time, s);
    gcode.push_str("; EXECUTABLE_BLOCK_END\n\n");
    let _ = writeln!(gcode, "; filament used [mm] = {}", join(&stats.filament_mm, 2));
    let _ = writeln!(
        gcode,
        "; filament used [cm3] = {}",
        join(&stats.volume_mm3.iter().map(|v| v / 1000.0).collect::<Vec<_>>(), 2)
    );
    let _ = writeln!(gcode, "; filament used [g] = {}", join(&stats.weight_g, 2));
    let _ = writeln!(gcode, "; filament cost = {}", join(&stats.cost, 2));
    if !s.bbl {
        let _ = writeln!(gcode, "; total filament used [g] = {:.2}", stats.weight_g.iter().sum::<f64>());
        let _ = writeln!(gcode, "; total filament cost = {:.2}", stats.cost.iter().sum::<f64>());
        if toolchanges > 0 {
            let _ = writeln!(gcode, "; total filament change = {toolchanges}");
        }
        let _ = writeln!(gcode, "; total layers count = {}", plans.len());
        let _ = writeln!(gcode, "; estimated printing time (normal mode) = {}", hms(stats.total_time));
        let _ = writeln!(gcode, "; estimated first layer printing time (normal mode) = {}", hms(first_layer_time));
        gcode.push('\n');
        gcode.push_str(&config_block(cx, s));
    }
    gcode.push('\n');
    Output { error, gcode, preview: w.preview, stats, warnings: w.warnings }
}

/// The program's moves as motion blocks, and each op's time. The blocks go to the planner as
/// they are made and the times come back in the same order, so no copy of the program is held.
/// Custom G-code text is timed too, line by line (its moves, waits and acceleration changes).
fn time_ops(ops: &[Op], s: &Settings) -> Vec<f64> {
    let m = &s.motion;
    let machine = std::cell::Cell::new(Machine::default());
    // The op each block came from.
    let owner: std::cell::RefCell<Vec<u32>> = std::cell::RefCell::new(Vec::with_capacity(ops.len()));
    let mine = |i: usize, b: Block| {
        owner.borrow_mut().push(i as u32);
        b
    };
    let blocks = ops.iter().enumerate().flat_map(|(i, op)| {
        let mut st = machine.get();
        let (one, text) = match op {
            Op::Accel(a) => {
                st.accel = *a;
                (None, "")
            }
            Op::Jerk(j) => {
                st.jerk = *j;
                (None, "")
            }
            Op::Move { to, e, feed, kind, arc, xy } => {
                // Before the head's position is known, count only Z and E.
                let d = if *xy { *to - st.pos } else { DVec3::new(0.0, 0.0, to.z - st.pos.z) };
                let length = arc.map(|(c, ccw)| arcs::arc_length(st.pos.truncate(), to.truncate(), c, ccw));
                let bk = match kind {
                    MoveKind::Extrude(_) => BlockKind::Extrude,
                    MoveKind::Retract => BlockKind::Retract,
                    MoveKind::Travel | MoveKind::Lifted | MoveKind::Wipe => BlockKind::Travel,
                };
                st.pos = *to;
                let b = Block {
                    delta: [d.x, d.y, d.z, *e],
                    length,
                    feed: *feed,
                    accel: st.accel,
                    kind: bk,
                    jerk: st.jerk,
                    dwell: 0.0,
                };
                (Some(b), "")
            }
            Op::FirmwareRetract { retract, length, feed } => {
                let e = if *retract { -length } else { *length };
                let b = Block {
                    delta: [0.0, 0.0, 0.0, e],
                    length: None,
                    feed: *feed,
                    accel: st.accel,
                    kind: BlockKind::Retract,
                    jerk: st.jerk,
                    dwell: 0.0,
                };
                (Some(b), "")
            }
            Op::Spiral { z, i, j, feed } => {
                let length = std::f64::consts::TAU * i.hypot(*j);
                let dz = *z - st.pos.z;
                st.pos.z = *z;
                let b = Block {
                    delta: [0.0, 0.0, dz, 0.0],
                    length: Some(length.hypot(dz)),
                    feed: *feed,
                    accel: st.accel,
                    kind: BlockKind::Travel,
                    jerk: st.jerk,
                    dwell: 0.0,
                };
                (Some(b), "")
            }
            Op::Text(t) => (None, t.as_str()),
            _ => (None, ""),
        };
        machine.set(st);
        let from_text = text.lines().filter_map(|l| {
            let mut st = machine.get();
            let b = st.line(l);
            machine.set(st);
            b
        });
        one.into_iter().chain(from_text).map(move |b| mine(i, b))
    });
    let times = motion::plan(blocks, &m.limits, m.flavor);
    let mut out = vec![0.0; ops.len()];
    for (i, t) in owner.borrow().iter().zip(times) {
        if let Some(slot) = out.get_mut(*i as usize) {
            *slot += t;
        }
    }
    out
}

/// A list of numbers as a C++ stream prints them, comma-separated.
fn plain_list(values: impl Iterator<Item = f64>) -> String {
    values.map(|v| plain(v, 6)).collect::<Vec<_>>().join(",")
}

/// `; CONFIG_BLOCK_START` to its end: every setting, then what older readers look for.
fn config_block(cx: &Context<'_>, s: &Settings) -> String {
    let mut out = String::from("; CONFIG_BLOCK_START\n");
    out.push_str(&cx.config_block);
    let first = cx.filaments.first();
    let _ = writeln!(out, "; first_layer_bed_temperature = {}", first.map_or(0, |f| f.bed_temp_first.round() as i64));
    if !s.bbl {
        let _ = writeln!(out, "; bed_shape = {}", cx.bed_shape);
    }
    let _ = writeln!(out, "; first_layer_temperature = {}", first.map_or(0, |f| f.nozzle_temp_first.round() as i64));
    if !s.bbl {
        let _ = writeln!(out, "; first_layer_height = {:.3}", s.first_layer_height);
    }
    out.push_str("; CONFIG_BLOCK_END\n\n");
    out
}

/// The program as G-code text, with M73 progress at each layer.
fn render_ops(out: &mut String, ops: &[Op], times: &[f64], total: f64, s: &Settings) {
    let flavor = s.motion.flavor;
    out.reserve(ops.len() * 28);
    let mut pos = DVec3::new(f64::NAN, f64::NAN, f64::NAN);
    let mut feed = -1.0;
    let mut e_abs = 0.0;
    let mut elapsed = 0.0;
    let mut last_fan: Option<String> = None;
    let line = |out: &mut String, s: &str| {
        out.push_str(s);
        out.push('\n');
    };
    for (op, t) in ops.iter().zip(times) {
        match op {
            Op::Text(text) => {
                line(out, text);
                // Custom G-code may have set the fan itself.
                if text.contains("M106") || text.contains("M107") {
                    last_fan = None;
                }
            }
            Op::Spiral { z, i, j, feed: f } => {
                out.push_str("G17\nG3 Z");
                push_num(out, *z + s.z_offset, 3);
                out.push_str(" I");
                push_num(out, *i, 3);
                out.push_str(" J");
                push_num(out, *j, 3);
                out.push_str(" P1  F");
                let fm = (f * 60.0).max(1.0);
                push_num(out, fm, 3);
                out.push('\n');
                feed = fm;
                pos.z = *z;
            }
            Op::Fan(f, _) => {
                let command = fan_command(*f);
                if last_fan.as_deref() != Some(command.as_str()) {
                    line(out, &command);
                    last_fan = Some(command);
                }
            }
            Op::Layer(_) => {
                // Orca's cooling buffer works layer by layer, so each repeats its first feed.
                feed = -1.0;
                let p = if total > 0.0 { (elapsed / total * 100.0).floor() as i64 } else { 0 };
                let r = ((total - elapsed).max(0.0) / 60.0).round() as i64;
                line(out, &format!("M73 P{} R{r}", p.clamp(0, 99)));
            }
            Op::Accel(a) => line(out, &accel_gcode(flavor, *a, s.motion.accel_to_decel)),
            Op::Jerk(j) => line(out, &jerk_gcode(flavor, *j)),
            Op::FirmwareRetract { retract, .. } => {
                line(out, if *retract { "G10 ; retract" } else { "G11 ; unretract" })
            }
            Op::Move { to, e, feed: f, kind, arc, xy } => {
                // Written straight into the text (millions of moves); dropped again if it
                // says nothing. As OrcaSlicer writes them: extrusions and wipes set their
                // speed on a line of their own, and any move in the plane names X and Y.
                let fm = (f * 60.0).max(1.0);
                let changed = (fm - feed).abs() > 5e-4;
                if changed && matches!(kind, MoveKind::Extrude(_) | MoveKind::Wipe) {
                    out.push_str("G1 F");
                    push_num(out, fm, 3);
                    out.push('\n');
                    feed = fm;
                }
                let start = out.len();
                out.push_str(match arc {
                    Some((_, true)) => "G3",
                    Some((_, false)) => "G2",
                    None => "G1",
                });
                let planar = (to.x - pos.x).abs() > 5e-4 || (to.y - pos.y).abs() > 5e-4;
                if *xy && (planar || pos.x.is_nan() || pos.y.is_nan() || arc.is_some()) {
                    out.push_str(" X");
                    push_num(out, to.x, 3);
                    out.push_str(" Y");
                    push_num(out, to.y, 3);
                }
                let named_z = *kind == MoveKind::Lifted;
                if named_z || (to.z - pos.z).abs() > 5e-4 || (pos.z.is_nan() && *kind != MoveKind::Retract) {
                    out.push_str(" Z");
                    push_num(out, to.z + s.z_offset, 3);
                }
                if let Some((c, _)) = arc {
                    out.push_str(" I");
                    push_num(out, c.x - pos.x, 3);
                    out.push_str(" J");
                    push_num(out, c.y - pos.y, 3);
                }
                if e.abs() > 1e-7 {
                    out.push_str(" E");
                    if s.gcode.relative_e {
                        push_num(out, *e, 5);
                    } else {
                        e_abs += e;
                        push_num(out, e_abs, 5);
                    }
                }
                if out.len() - start > 2 {
                    if (fm - feed).abs() > 5e-4 {
                        out.push_str(" F");
                        push_num(out, fm, 3);
                        feed = fm;
                    }
                    out.push('\n');
                } else {
                    out.truncate(start);
                }
                pos = if *xy { *to } else { DVec3::new(pos.x, pos.y, to.z) };
            }
        }
        elapsed += t;
    }
    line(out, "M73 P100 R0");
}

fn join(v: &[f64], decimals: usize) -> String {
    v.iter().map(|x| format!("{x:.decimals$}")).collect::<Vec<_>>().join(", ")
}

/// Coarser Preview copies for prints with more points, keeping it to a few million segments.
fn preview_tolerance(plans: &[LayerPlan]) -> f64 {
    match path_points(plans) {
        0..=3_000_000 => 0.0,
        3_000_001..=8_000_000 => 0.05,
        8_000_001..=20_000_000 => 0.15,
        20_000_001..=50_000_000 => 0.4,
        _ => 1.0,
    }
}

/// Points on every path of the plans.
fn path_points(plans: &[LayerPlan]) -> usize {
    plans
        .iter()
        .map(|p| {
            p.skirt.iter().chain(&p.support).map(|x| x.points.len()).sum::<usize>()
                + p.objects
                    .iter()
                    .flat_map(|(_, isl)| isl.iter().flat_map(|i| i.walls.iter().flatten().chain(&i.fills)))
                    .map(|x| x.points.len())
                    .sum::<usize>()
        })
        .sum()
}

/// OrcaSlicer's fan ramp over the first layers: 0 while the fan is off, then up to 1 at
/// `full_fan_speed_layer`.
fn fan_ramp(f: &Filament, li: usize) -> f64 {
    if li < f.fan_off_layers {
        return 0.0;
    }
    if li + 1 < f.full_fan_layer {
        return (li + 1 - f.fan_off_layers) as f64 / (f.full_fan_layer - f.fan_off_layers) as f64;
    }
    1.0
}

/// The layer's fan from its time after cooling (OrcaSlicer's): full below the slow-down time,
/// from full to the minimum up to the fan cooling time, then the minimum (or off, unless the
/// filament keeps it running), all on the ramp over the first layers.
fn layer_fan(f: &Filament, layer_time: f64, ramp: f64) -> f64 {
    if ramp <= 0.0 {
        return 0.0;
    }
    let fan = if layer_time < f.slow_down_layer_time {
        f.fan_max
    } else if layer_time < f.fan_cooling_layer_time {
        let t = (layer_time - f.slow_down_layer_time) / (f.fan_cooling_layer_time - f.slow_down_layer_time);
        (t * f.fan_min + (1.0 - t) * f.fan_max + 0.5).floor()
    } else if f.keep_fan_on {
        f.fan_min
    } else {
        0.0
    };
    if ramp < 1.0 { (fan * ramp + 0.5).floor() } else { fan }
}

/// The feed cap that makes moves of (length, feed) take `extra` seconds longer, no feed going
/// below `min`; `None` when even `min` is not slow enough.
fn slow_down_cap(moves: &[(f64, f64)], extra: f64, min: f64) -> Option<f64> {
    let added = |cap: f64| -> f64 { moves.iter().map(|(len, f)| len / f.min(cap) - len / f).sum() };
    if added(min) < extra {
        return None;
    }
    let (mut lo, mut hi) = (min, moves.iter().map(|m| m.1).fold(min, f64::max));
    for _ in 0..60 {
        let mid = (lo + hi) / 2.0;
        if added(mid) > extra { lo = mid } else { hi = mid }
    }
    Some(lo)
}

#[cfg(test)]
mod general_tests {
    use super::general;

    #[test]
    fn it_prints_as_printf_does() {
        assert_eq!(general(0.45), "0.45");
        assert_eq!(general(f64::from(0.419999f32)), "0.419999");
        assert_eq!(general(f64::from(0.346559f32)), "0.346559");
        assert_eq!(general(1.0), "1");
        assert_eq!(general(0.0), "0");
        assert_eq!(general(123.4567), "123.457");
        assert_eq!(general(f64::NAN), "0");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spots_temperature_commands() {
        assert!(has_command("G28\nM190 S60 ; wait", &["M190"]));
        assert!(!has_command("; M190 is in a comment\nM1900", &["M190"]));
    }

    #[test]
    fn object_labels_are_firmware_safe() {
        assert_eq!(label("my part (1).stl", 3), "my_part__1_.stl_id_3_copy_0");
    }

    fn l_shape() -> Path {
        // Counter-clockwise L: the corner at (5, 5) is concave.
        let pts = vec![
            DVec2::new(0.0, 0.0),
            DVec2::new(10.0, 0.0),
            DVec2::new(10.0, 5.0),
            DVec2::new(5.0, 5.0),
            DVec2::new(5.0, 10.0),
            DVec2::new(0.0, 10.0),
        ];
        Path::new(Role::OuterWall, pts, true, 0.4, 60.0, 1.0)
    }

    #[test]
    fn overhang_speeds_run_smoothly_between_degrees() {
        use crate::settings::RelSpeed;
        // X1C's: 200 mm/s walls, overhangs off / 50 / 30 / 10, bridges 50.
        let speeds = [RelSpeed::Off, RelSpeed::Mm(50.0), RelSpeed::Mm(30.0), RelSpeed::Mm(10.0)];
        let v = |share: f64| overhang_speed(share, 200.0, &speeds, 50.0);
        assert_eq!(v(0.05), 200.0);
        assert_eq!(v(0.25), 200.0);
        // Halfway from 25 % (wall speed) to 50 % (50 mm/s).
        assert_eq!(v(0.375), 125.0);
        assert_eq!(v(0.5), 50.0);
        assert_eq!(v(0.87), 10.0);
        // Then up to the bridge speed when all of it hangs.
        assert_eq!(v(1.2), 50.0);
        // Never faster than the line's own speed.
        assert_eq!(overhang_speed(0.3, 40.0, &speeds, 50.0), 40.0);
    }

    #[test]
    fn seams_hide_in_concave_corners_but_not_on_overhangs() {
        let mut p = l_shape();
        // Near the top-right of the L, the concave corner wins over the convex ones nearby.
        assert_eq!(best_seam(&p, DVec2::new(8.0, 8.0)), 3);
        // Overhanging there: the seam goes elsewhere.
        p.overhang = vec![0, 0, 60, 60, 0, 0];
        assert_ne!(best_seam(&p, DVec2::new(8.0, 8.0)), 3);
        assert!((corner_angle(&p.points, 3) - 90.0).abs() < 1e-9);
    }

    #[test]
    fn loops_stop_short_by_the_seam_gap() {
        let p = l_shape();
        let plain = |to: DVec2| Step { to, bend: None, hang: 0, dz: 0.0, flow: 1.0, scarf: false, speed: 0.0 };
        let mut steps: Vec<Step> = p.points.iter().skip(1).chain(p.points.first()).map(|q| plain(*q)).collect();
        trim_end(DVec2::ZERO, &mut steps, 0.04);
        assert_eq!(steps.len(), 6);
        assert!((steps[5].to - DVec2::new(0.0, 0.04)).length() < 1e-9);
        // A gap longer than the last segment eats into the one before.
        trim_end(DVec2::ZERO, &mut steps, 10.0);
        assert_eq!(steps.len(), 5);
        assert!((steps[4].to - DVec2::new(0.04, 10.0)).length() < 1e-9, "{:?}", steps[4].to);
    }

    #[test]
    fn steps_split_at_distances() {
        let plain = |to: DVec2| Step { to, bend: None, hang: 0, dz: 0.0, flow: 1.0, scarf: false, speed: 0.0 };
        let steps = vec![plain(DVec2::new(10.0, 0.0)), plain(DVec2::new(10.0, 10.0))];
        let split = split_steps(DVec2::ZERO, &steps, &[2.5, 5.0, 10.0, 12.0]);
        let ds: Vec<f64> = split.iter().map(|(_, d)| *d).collect();
        assert_eq!(ds, vec![2.5, 5.0, 10.0, 12.0, 20.0]);
        assert!((split[3].0.to - DVec2::new(10.0, 2.0)).length() < 1e-9);
    }

    #[test]
    fn staggered_seams_start_further_back() {
        let mut p = l_shape();
        stagger(&mut p, 2.0);
        // Two millimetres back from (0, 0) along the closing edge.
        assert_eq!(p.points[0], DVec2::new(0.0, 2.0));
        assert_eq!(p.points.len(), 7);
        assert!((p.length() - l_shape().length()).abs() < 1e-9);
    }
}
