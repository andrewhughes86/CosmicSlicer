//! From layer plans to G-code: print order, seams, travel routing and retraction, cooling,
//! speeds, motion commands, the time estimate, and the moves the Preview draws.
//!
//! The writer builds a program of [`Op`]s first. The motion planner ([`crate::motion`]) times
//! that program as the firmware will run it, and only then is it rendered as text, so the
//! estimate, the `M73` progress and the G-code always describe the same moves.

use std::fmt::Write as _;

use glam::{DVec2, DVec3};

use crate::arcs::{self, Piece};
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
    pub travel_time: f64,
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
    /// The layer's fan, %.
    fan: f64,
    /// The fan as last commanded (the overhang fan may have it above the layer's).
    out_fan: f64,
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
    /// Speed factor for layer cooling.
    slow: f64,
    /// How far the Preview's copy of a path may stray from it, mm.
    preview_tolerance: f64,
    /// The island being printed, for travel inside it.
    region: Option<Region>,
    /// The next island's region, installed once the head has arrived there.
    pending_region: Option<Region>,
    /// Whether `pos` X and Y are where the head is (not until the first travel).
    xy_known: bool,
    /// The end of the last extrusion, for wiping back along it.
    last_path: Vec<DVec2>,
    /// The speed it was printed at, for role-based wipes.
    last_speed: f64,
    warnings: Vec<String>,
}

fn f3(v: f64) -> String {
    let s = format!("{v:.3}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s == "-0" || s.is_empty() { "0".into() } else { s.to_owned() }
}

fn f5(v: f64) -> String {
    let s = format!("{v:.5}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s == "-0" || s.is_empty() { "0".into() } else { s.to_owned() }
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

    fn may_lift(&self) -> bool {
        let m = &self.s().motion;
        let z = self.print_z;
        self.s().retract.z_hop > 0.0 && z >= m.lift_above && (m.lift_below <= 0.0 || z <= m.lift_below)
    }

    /// The retraction settings, with the retraction tower's length for this layer.
    fn retraction(&self) -> crate::settings::Retract {
        let mut r = self.s().retract;
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
        let mut left = r.length;
        // Wipe back along the last path, retracting part of the way while moving.
        if r.wipe && self.s().motion.wipe_distance > 0.0 && self.last_path.len() >= 2 {
            let distance = self.s().motion.wipe_distance;
            let share = self.s().motion.retract_before_wipe;
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
            };
            if travelled > 0.0 {
                let during = r.length * share;
                let mut from = self.pos.truncate();
                for p in wipe {
                    let e = -during * from.distance(p) / travelled;
                    let z = self.pos.z;
                    self.mv(p.extend(z), e, wipe_speed, MoveKind::Wipe);
                    from = p;
                }
                left -= during;
            }
        }
        if left > 1e-6 {
            if self.s().motion.firmware_retraction {
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

    fn lift_now(&mut self) {
        if self.hop > 0.0 || !self.may_lift() {
            return;
        }
        let hop = self.s().retract.z_hop;
        let to = self.pos + DVec3::Z * hop;
        self.mv(to, 0.0, self.s().speeds.travel_z.max(5.0), MoveKind::Travel);
        self.hop = hop;
    }

    fn unretract(&mut self) {
        let r = self.retraction();
        if self.hop > 0.0 {
            let to = DVec3::new(self.pos.x, self.pos.y, self.print_z);
            self.mv(to, 0.0, self.s().speeds.travel_z.max(5.0), MoveKind::Travel);
            self.hop = 0.0;
        }
        if self.retracted {
            // What was taken back, even if the length changed since (the retraction tower).
            let length = self.retracted_by + self.s().motion.restart_extra;
            if self.s().motion.firmware_retraction {
                self.ops.push(Op::FirmwareRetract { retract: false, length, feed: r.deretract_speed });
            } else {
                let to = self.pos;
                self.mv(to, length, r.deretract_speed, MoveKind::Retract);
            }
            self.retracted = false;
        }
    }

    /// Moves to `to` without printing: inside the island when it can, routed round walls when
    /// asked to, retracting and lifting when the move leaves the part or is long.
    fn travel(&mut self, to: DVec2) {
        let from = self.pos.truncate();
        let d = from.distance(to);
        if d < 1.0e-4 && self.xy_known {
            return;
        }
        if !self.xy_known {
            // The first travel after the start G-code: straight there, lifted if lifting.
            self.retract();
            self.lift_now();
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
        let retract = length > self.s().retract.min_travel && !(inside && m.reduce_infill_retraction);
        if retract {
            self.retract();
        }
        self.set_dynamics(None);
        let speed = self.travel_speed();
        let lifting = retract && self.may_lift() && self.hop == 0.0;
        let z = self.print_z;
        let hop = self.s().retract.z_hop;
        for (i, p) in route.iter().enumerate().skip(1) {
            let prev = route.get(i - 1).copied().unwrap_or(from);
            if lifting && i == 1 && m.z_hop == ZHop::Slope {
                // Rise along the start of the travel.
                let rise_run = hop / m.travel_slope.to_radians().tan();
                let seg = prev.distance(*p);
                if seg > rise_run * 1.2 {
                    let mid = prev + (*p - prev) * (rise_run / seg);
                    self.mv(mid.extend(z + hop), 0.0, speed, MoveKind::Travel);
                    self.hop = hop;
                } else {
                    self.lift_now();
                }
            } else if lifting && i == 1 {
                self.lift_now();
            }
            let at = p.extend(z + self.hop);
            self.mv(at, 0.0, speed, MoveKind::Travel);
        }
        if lifting && self.hop == 0.0 {
            self.lift_now();
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
        let plain = |to: DVec2, deg: u8| Step { to, deg, dz: 0.0, flow: 1.0, scarf: false };
        let mut steps: Vec<Step> =
            (1..n).filter_map(|k| path.points.get(k).map(|&p| plain(p, path.overhang_at(k - 1)))).collect();
        let Some(start) = path.start() else { return steps };
        if !path.closed {
            return steps;
        }
        steps.push(plain(start, path.overhang_at(n - 1)));
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
        self.travel(start);
        if let Some(r) = self.pending_region.take() {
            self.region = Some(r);
        }
        self.unretract();
        if self.filament().is_none() {
            return;
        }
        let scarf = self.wants_scarf(path);
        let steps = self.steps(path, scarf);
        let z = self.pos.z;
        // A scarf starts low: down to its first height before extruding.
        if let Some(first) = steps.first().filter(|st| st.dz < 0.0) {
            let to = start.extend(z + first.dz);
            self.mv(to, 0.0, self.s().speeds.travel_z.max(5.0), MoveKind::Travel);
        }
        self.set_dynamics(Some(path.role));
        let mut all: Vec<DVec2> = Vec::with_capacity(steps.len() + 1);
        all.push(self.pos.truncate());
        // Runs of steps printed alike.
        let mut i = 0;
        while i < steps.len() {
            let Some(first) = steps.get(i) else { break };
            let key = (first.deg, first.scarf);
            let j = (i..steps.len())
                .find(|&j| steps.get(j).is_some_and(|st| (st.deg, st.scarf) != key))
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
            self.mv(to, 0.0, self.s().speeds.travel_z.max(5.0), MoveKind::Travel);
        }
        if (self.out_fan - self.fan).abs() > 0.5 {
            self.out_fan = self.fan;
            let f = self.fan;
            self.line(fan_command(f));
        }
        if path.closed && path.role == Role::OuterWall {
            self.preview.seams.push([start.x as f32, start.y as f32, z as f32]);
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

    /// Prints a run of `path`'s steps that share an overhang degree (and scarf or not).
    fn run(&mut self, path: &Path, run: &[Step], z: f64) {
        let Some(fil) = self.filament() else { return };
        let Some(first) = run.first() else { return };
        let s = self.s();
        let deg = if path.role.is_wall() { first.deg } else { 0 };
        // Half or more of the line over air: an overhang wall in the Preview and G-code.
        let role = if deg >= 3 { Role::OverhangWall } else { path.role };
        if self.role != Some(role) {
            self.role = Some(role);
            self.line(format!(";TYPE:{}", role.name()));
        }
        if (self.width - path.width).abs() > 1.0e-4 {
            self.width = path.width;
            self.line(format!(";WIDTH:{}", f3(path.width)));
        }
        let mut base = path.speed;
        if path.role == Role::OuterWall
            && let Some(v) = self.calib.outer_speed
        {
            base = v;
        }
        let mut flow = path.flow;
        if deg > 0 && s.walls.overhang_speed {
            base = match s.walls.overhang_speeds.get(usize::from(deg) - 1) {
                Some(sp) => sp.of(base),
                // Hanging (almost) entirely: bridge speed.
                None => s.speeds.bridge,
            };
        }
        if deg == 5 {
            flow *= s.walls.overhang_flow;
        }
        if first.scarf {
            base = s.seams.scarf_speed.of(base);
            flow *= s.seams.scarf_flow;
        }
        // The fan: bridges and overhangs past the filament's threshold get the overhang fan.
        let fan_on = self.layer >= fil.fan_off_layers;
        let fan = match (fil.overhang_fan_from, role) {
            (Some(_), Role::InternalBridge) if fan_on => {
                fil.internal_bridge_fan.unwrap_or(fil.overhang_fan).max(self.fan)
            }
            (Some(_), Role::Bridge) if fan_on => fil.overhang_fan.max(self.fan),
            (Some(t), _)
                if fan_on
                    && path.role.is_wall()
                    && ((t == 0 && path.role == Role::OuterWall) || (t > 0 && deg >= t)) =>
            {
                fil.overhang_fan.max(self.fan)
            }
            _ => self.fan,
        };
        if (fan - self.out_fan).abs() > 0.5 {
            self.out_fan = fan;
            self.line(fan_command(fan));
        }
        let h = self.height;
        let area = bead_area(path.width, h) * flow;
        let mut speed = base * self.slow;
        let bridging = role.is_bridge() || deg == 5;
        if !matches!(role, Role::Skirt | Role::Brim) && !bridging {
            speed = speed.max(fil.slow_down_min_speed.min(base));
        }
        // The hot end can only melt so much per second.
        if area > 0.0 {
            speed = speed.min(fil.max_volumetric / area);
        }
        let speed = speed.max(1.0);
        let e_per_mm = area / fil.area() * fil.flow_ratio;
        self.last_speed = speed;
        let fit = !first.scarf
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
        let pieces: Vec<(Piece, f64, f64)> = if fit {
            arcs::fit(&pts, 0.02).into_iter().map(|p| (p, 0.0, 1.0)).collect()
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
        }
    }

    /// Picks where a loop starts, by the seam setting, and rotates it to start there. `depth`
    /// is the wall's depth (0 outer), for staggered inner seams.
    fn place_seam(&mut self, path: &mut Path, object: Option<usize>, depth: Option<usize>) {
        if !path.closed || path.points.len() < 3 {
            return;
        }
        let here = self.pos.truncate();
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
            self.line(format!("; printing object {name}"));
            if labels && m.flavor == Flavor::Klipper && m.exclude_object {
                self.line(format!("EXCLUDE_OBJECT_START NAME={}", label(&name, *o)));
            } else if labels && matches!(m.flavor, Flavor::Marlin | Flavor::Marlin2) {
                self.line(format!("M486 S{o}"));
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
            if labels && m.flavor == Flavor::Klipper && m.exclude_object {
                self.line(format!("EXCLUDE_OBJECT_END NAME={}", label(&name, *o)));
            } else if labels && matches!(m.flavor, Flavor::Marlin | Flavor::Marlin2) {
                self.line("M486 S-1");
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
        if let Some(k) = island.pressure_advance {
            let line = crate::calib::pressure_advance(self.s().motion.flavor, k, self.extruder);
            self.line(line);
        }
        // Arriving uses the plain rules; travel within the island uses its region from the
        // first extrusion on.
        self.region = None;
        self.pending_region = Region::new(&island.outline, self.s().nozzle);
        self.seam_paint = (island.seam_enforce.clone(), island.seam_block.clone());
        let depth = island.walls.len();
        let order: Vec<usize> = if self.s().outer_first { (0..depth).collect() } else { (0..depth).rev().collect() };
        for k in order {
            if let Some(ws) = island.walls.get(k) {
                self.print_nearest(ws.clone(), Some(object), Some(k));
            }
        }
        self.print_nearest(island.fills.clone(), Some(object), None);
        self.region = None;
        self.pending_region = None;
    }

    fn set_fan(&mut self, fan: f64) {
        if (fan - self.fan).abs() > 0.5 || (fan - self.out_fan).abs() > 0.5 {
            self.fan = fan;
            self.out_fan = fan;
            self.line(fan_command(fan));
        }
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

/// One move of a path as printed: to `to`, `dz` below the layer, at `flow` of the full bead.
#[derive(Clone, Copy, Debug)]
struct Step {
    to: DVec2,
    deg: u8,
    dz: f64,
    flow: f64,
    scarf: bool,
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
                out.push((Step { to: at + (st.to - at) * ((cut - dist) / len), ..*st }, cut));
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
            path.points.insert(prev + 1, cut);
            if path.overhang.len() == n {
                let d = path.overhang_at(prev);
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

fn fan_command(percent: f64) -> String {
    let s = (percent.clamp(0.0, 100.0) * 2.55).round() as i64;
    if s == 0 { "M107".into() } else { format!("M106 S{s}") }
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
fn limits_gcode(s: &Settings) -> Vec<String> {
    let m = &s.motion;
    let l = &m.limits;
    match m.flavor {
        Flavor::Klipper => vec![format!(
            "SET_VELOCITY_LIMIT VELOCITY={} ACCEL={} SQUARE_CORNER_VELOCITY={}",
            f3(l.max_speed[0].min(l.max_speed[1])),
            f3(l.accel_extruding),
            f3(l.jerk[0].min(l.jerk[1])),
        )],
        Flavor::RepRap => vec![
            format!(
                "M201 X{} Y{} Z{} E{}",
                f3(l.max_accel[0]),
                f3(l.max_accel[1]),
                f3(l.max_accel[2]),
                f3(l.max_accel[3])
            ),
            // RepRapFirmware takes speeds and jerk in mm/min.
            format!(
                "M203 X{} Y{} Z{} E{}",
                f3(l.max_speed[0] * 60.0),
                f3(l.max_speed[1] * 60.0),
                f3(l.max_speed[2] * 60.0),
                f3(l.max_speed[3] * 60.0)
            ),
            format!("M204 P{} T{}", f3(l.accel_extruding), f3(l.accel_travel)),
            format!(
                "M566 X{} Y{} Z{} E{}",
                f3(l.jerk[0] * 60.0),
                f3(l.jerk[1] * 60.0),
                f3(l.jerk[2] * 60.0),
                f3(l.jerk[3] * 60.0)
            ),
        ],
        Flavor::Repetier => Vec::new(),
        Flavor::Marlin | Flavor::Marlin2 => {
            let mut v = vec![
                format!(
                    "M201 X{} Y{} Z{} E{}",
                    f3(l.max_accel[0]),
                    f3(l.max_accel[1]),
                    f3(l.max_accel[2]),
                    f3(l.max_accel[3])
                ),
                format!(
                    "M203 X{} Y{} Z{} E{}",
                    f3(l.max_speed[0]),
                    f3(l.max_speed[1]),
                    f3(l.max_speed[2]),
                    f3(l.max_speed[3])
                ),
                format!("M204 P{} R{} T{}", f3(l.accel_extruding), f3(l.accel_retracting), f3(l.accel_travel)),
            ];
            if m.flavor == Flavor::Marlin2 && l.junction_deviation > 0.0 {
                v.push(format!("M205 J{}", f3(l.junction_deviation)));
            } else {
                v.push(format!("M205 X{} Y{} Z{} E{}", f3(l.jerk[0]), f3(l.jerk[1]), f3(l.jerk[2]), f3(l.jerk[3])));
            }
            v
        }
    }
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
        ops: Vec::with_capacity(plans.len() * 2000),
        pos: DVec3::ZERO,
        retracted: false,
        hop: 0.0,
        role: None,
        width: 0.0,
        extruder: cx.object_extruder.first().copied().unwrap_or(0).min(n_ext - 1),
        fan: 0.0,
        out_fan: 0.0,
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
        slow: 1.0,
        preview_tolerance: preview_tolerance(plans),
        region: None,
        pending_region: None,
        xy_known: false,
        last_path: Vec::new(),
        last_speed: 0.0,
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
    } else if labels && matches!(m.flavor, Flavor::Marlin | Flavor::Marlin2) {
        w.line(format!("M486 T{}", cx.object_names.len()));
    }
    if m.emit_limits {
        for l in limits_gcode(s) {
            w.line(l);
        }
    }
    if !has_command(&start, &["M140", "M190"]) {
        w.line(format!("M140 S{}", bed_first.round() as i64));
    }
    if !has_command(&start, &["M104", "M109"]) {
        w.line(format!("M104 S{}", nozzle_first.round() as i64));
    }
    if !has_command(&start, &["M140", "M190"]) {
        w.line(format!("M190 S{} ; wait for bed", bed_first.round() as i64));
    }
    if !has_command(&start, &["M104", "M109"]) {
        w.line(format!("M109 S{} ; wait for nozzle", nozzle_first.round() as i64));
    }
    w.line("; machine start G-code");
    w.line(start.trim_end().to_owned());
    w.line("; end of machine start G-code");
    w.line("G90 ; absolute coordinates");
    w.line("G21 ; millimetres");
    if s.gcode.relative_e {
        w.line("M83 ; relative extrusion");
    } else {
        w.line("M82 ; absolute extrusion");
        w.line("G92 E0");
    }
    if n_ext > 1 {
        let e = w.extruder;
        w.line(format!("T{e}"));
    }
    w.pressure_advance();

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
    for (li, plan) in plans.iter().enumerate() {
        w.layer = li;
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
            w.mv(up, 0.0, s.speeds.travel_z.max(5.0), MoveKind::Travel);
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
        w.line(";LAYER_CHANGE");
        w.line(format!(";Z:{}", f3(plan.print_z)));
        w.line(format!(";HEIGHT:{}", f3(plan.height)));
        let before = render("Before layer change G-code", &s.gcode.before_layer, &layer_vars);
        if !before.trim().is_empty() {
            w.line(before.trim_end().to_owned());
        }
        w.ops.push(Op::Layer(li));
        if s.retract.on_layer_change {
            w.retract();
        }
        let to = DVec3::new(w.pos.x, w.pos.y, plan.print_z);
        w.mv(to, 0.0, s.speeds.travel_z.max(5.0), MoveKind::Travel);
        w.hop = 0.0;
        let after = render("Layer change G-code", &s.gcode.layer_change, &layer_vars);
        if !after.trim().is_empty() {
            w.line(after.trim_end().to_owned());
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
        // Layer cooling: a quick layer prints slower so it has time to set.
        let fil = w.filament().cloned();
        let rough = rough_layer_time(plan);
        w.slow = 1.0;
        let mut fan = 0.0;
        if let Some(f) = &fil
            && li >= f.fan_off_layers
        {
            fan = f.fan_min;
            if f.full_fan_layer > f.fan_off_layers && li < f.full_fan_layer {
                let t = (li - f.fan_off_layers + 1) as f64 / (f.full_fan_layer - f.fan_off_layers + 1) as f64;
                fan = f.fan_min * t;
            }
            if f.slow_down_layer_time > 0.0 && rough > 0.0 && rough < f.slow_down_layer_time {
                w.slow = (rough / f.slow_down_layer_time).clamp(0.05, 1.0);
                fan = f.fan_max;
            }
        }
        w.set_fan(fan);

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
            w.print_nearest(plan.skirt.clone(), None, None);
        }
        let steps = layer_steps(plan, cx, n_ext, w.extruder);
        for (part, e) in steps {
            if e != w.extruder {
                let from = w.extruder;
                let vars = [
                    ("layer_num", Value::Num(li as f64)),
                    ("layer_z", Value::Num(plan.print_z)),
                    ("previous_extruder", Value::Num(from as f64)),
                    ("next_extruder", Value::Num(e as f64)),
                    ("toolchange_z", Value::Num(plan.print_z)),
                ];
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
    w.set_fan(0.0);
    let end = render("Machine end G-code", &s.gcode.end, &[]);
    w.line("; machine end G-code");
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
            Op::Move { .. } | Op::FirmwareRetract { .. } => stats.travel_time += t,
            _ => {}
        }
        if let Some(slot) = layer.and_then(|i| layer_times.get_mut(i)) {
            *slot += t;
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
    let body = render_ops(&w.ops, &times, stats.total_time, s);
    let mut header = String::new();
    let _ = writeln!(header, "; HEADER_BLOCK_START");
    let _ = writeln!(header, "; generated by CosmicSlicer {}", env!("CARGO_PKG_VERSION"));
    let _ = writeln!(header, "; plate: {}", cx.plate_name);
    let _ = writeln!(
        header,
        "; model printing time: {}; total estimated time: {}",
        hms(stats.total_time - stats.travel_time),
        hms(stats.total_time)
    );
    let _ = writeln!(header, "; estimated printing time (normal mode) = {}", hms(stats.total_time));
    let _ = writeln!(header, "; total layer number: {}", plans.len());
    let _ = writeln!(header, "; filament used [mm] = {}", join(&stats.filament_mm, 2));
    let _ = writeln!(
        header,
        "; filament used [cm3] = {}",
        join(&stats.volume_mm3.iter().map(|v| v / 1000.0).collect::<Vec<_>>(), 2)
    );
    let _ = writeln!(header, "; filament used [g] = {}", join(&stats.weight_g, 2));
    let _ = writeln!(header, "; filament cost = {}", join(&stats.cost, 2));
    let _ = writeln!(header, "; layer_height = {}", f3(s.layer_height));
    let _ = writeln!(header, "; initial_layer_print_height = {}", f3(s.first_layer_height));
    let _ = writeln!(header, "; nozzle_diameter = {}", f3(s.nozzle));
    let _ = writeln!(header, "; HEADER_BLOCK_END");
    header.push('\n');
    let mut gcode = header;
    gcode.push_str(&cx.thumbnails);
    gcode.push_str(&body);
    Output { error, gcode, preview: w.preview, stats, warnings: w.warnings }
}

/// The program's moves as motion blocks, and each op's time.
fn time_ops(ops: &[Op], s: &Settings) -> Vec<f64> {
    let m = &s.motion;
    let mut blocks: Vec<Block> = Vec::new();
    let mut index: Vec<Option<usize>> = Vec::with_capacity(ops.len());
    let mut pos = DVec3::ZERO;
    let mut accel = 0.0;
    let mut jerk = 0.0;
    for op in ops {
        match op {
            Op::Accel(a) => {
                accel = *a;
                index.push(None);
            }
            Op::Jerk(j) => {
                jerk = *j;
                index.push(None);
            }
            Op::Move { to, e, feed, kind, arc, xy } => {
                // Before the head's position is known, count only Z and E.
                let d = if *xy { *to - pos } else { DVec3::new(0.0, 0.0, to.z - pos.z) };
                let length = arc.map(|(c, ccw)| arcs::arc_length(pos.truncate(), to.truncate(), c, ccw));
                let bk = match kind {
                    MoveKind::Extrude(_) => BlockKind::Extrude,
                    MoveKind::Retract => BlockKind::Retract,
                    MoveKind::Travel | MoveKind::Wipe => BlockKind::Travel,
                };
                index.push(Some(blocks.len()));
                blocks.push(Block { delta: [d.x, d.y, d.z, *e], length, feed: *feed, accel, kind: bk, jerk });
                pos = *to;
            }
            Op::FirmwareRetract { retract, length, feed } => {
                index.push(Some(blocks.len()));
                let e = if *retract { -length } else { *length };
                blocks.push(Block {
                    delta: [0.0, 0.0, 0.0, e],
                    length: None,
                    feed: *feed,
                    accel,
                    kind: BlockKind::Retract,
                    jerk,
                });
            }
            _ => index.push(None),
        }
    }
    let t = motion::plan(&blocks, &m.limits, m.flavor);
    index.into_iter().map(|i| i.and_then(|i| t.get(i).copied()).unwrap_or(0.0)).collect()
}

/// The program as G-code text, with M73 progress at each layer.
fn render_ops(ops: &[Op], times: &[f64], total: f64, s: &Settings) -> String {
    let flavor = s.motion.flavor;
    let mut out = String::with_capacity(ops.len() * 28);
    let mut pos = DVec3::new(f64::NAN, f64::NAN, f64::NAN);
    let mut feed = -1.0;
    let mut e_abs = 0.0;
    let mut elapsed = 0.0;
    let line = |out: &mut String, s: &str| {
        out.push_str(s);
        out.push('\n');
    };
    line(&mut out, &format!("M73 P0 R{}", (total / 60.0).round() as i64));
    for (op, t) in ops.iter().zip(times) {
        match op {
            Op::Text(text) => line(&mut out, text),
            Op::Layer(_) => {
                let p = if total > 0.0 { (elapsed / total * 100.0).floor() as i64 } else { 0 };
                let r = ((total - elapsed).max(0.0) / 60.0).round() as i64;
                line(&mut out, &format!("M73 P{} R{r}", p.clamp(0, 99)));
            }
            Op::Accel(a) => line(&mut out, &accel_gcode(flavor, *a, s.motion.accel_to_decel)),
            Op::Jerk(j) => line(&mut out, &jerk_gcode(flavor, *j)),
            Op::FirmwareRetract { retract, .. } => {
                line(&mut out, if *retract { "G10 ; retract" } else { "G11 ; unretract" })
            }
            Op::Move { to, e, feed: f, kind, arc, xy } => {
                let mut cmd = match arc {
                    Some((_, true)) => String::from("G3"),
                    Some((_, false)) => String::from("G2"),
                    None => String::from("G1"),
                };
                if *xy && ((to.x - pos.x).abs() > 5e-4 || pos.x.is_nan() || arc.is_some()) {
                    let _ = write!(cmd, " X{}", f3(to.x));
                }
                if *xy && ((to.y - pos.y).abs() > 5e-4 || pos.y.is_nan() || arc.is_some()) {
                    let _ = write!(cmd, " Y{}", f3(to.y));
                }
                if (to.z - pos.z).abs() > 5e-4 || (pos.z.is_nan() && *kind != MoveKind::Retract) {
                    let _ = write!(cmd, " Z{}", f3(to.z + s.z_offset));
                }
                if let Some((c, _)) = arc {
                    let _ = write!(cmd, " I{} J{}", f3(c.x - pos.x), f3(c.y - pos.y));
                }
                if e.abs() > 1e-7 {
                    if s.gcode.relative_e {
                        let _ = write!(cmd, " E{}", f5(*e));
                    } else {
                        e_abs += e;
                        let _ = write!(cmd, " E{}", f5(e_abs));
                    }
                }
                let fm = (f * 60.0).round().max(1.0);
                if (fm - feed).abs() > 0.5 {
                    let _ = write!(cmd, " F{}", fm as i64);
                    feed = fm;
                }
                if cmd.len() > 2 {
                    line(&mut out, &cmd);
                }
                pos = if *xy { *to } else { DVec3::new(pos.x, pos.y, to.z) };
            }
        }
        elapsed += t;
    }
    line(&mut out, "M73 P100 R0");
    out
}

fn join(v: &[f64], decimals: usize) -> String {
    v.iter().map(|x| format!("{x:.decimals$}")).collect::<Vec<_>>().join(", ")
}

/// Coarser Preview copies for prints with more points, keeping it to a few million segments.
fn preview_tolerance(plans: &[LayerPlan]) -> f64 {
    let points: usize = plans
        .iter()
        .map(|p| {
            p.skirt.iter().chain(&p.support).map(|x| x.points.len()).sum::<usize>()
                + p.objects
                    .iter()
                    .flat_map(|(_, isl)| isl.iter().flat_map(|i| i.walls.iter().flatten().chain(&i.fills)))
                    .map(|x| x.points.len())
                    .sum::<usize>()
        })
        .sum();
    match points {
        0..=3_000_000 => 0.0,
        3_000_001..=8_000_000 => 0.05,
        8_000_001..=20_000_000 => 0.15,
        20_000_001..=50_000_000 => 0.4,
        _ => 1.0,
    }
}

/// Printing time of a layer before cooling, from its path lengths and speeds.
fn rough_layer_time(plan: &LayerPlan) -> f64 {
    let all = plan.skirt.iter().chain(&plan.support).chain(
        plan.objects.iter().flat_map(|(_, isl)| isl.iter().flat_map(|i| i.walls.iter().flatten().chain(&i.fills))),
    );
    all.map(|p| p.length() / p.speed.max(1.0)).sum()
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
    fn seams_hide_in_concave_corners_but_not_on_overhangs() {
        let mut p = l_shape();
        // Near the top-right of the L, the concave corner wins over the convex ones nearby.
        assert_eq!(best_seam(&p, DVec2::new(8.0, 8.0)), 3);
        // Overhanging there: the seam goes elsewhere.
        p.overhang = vec![0, 0, 3, 3, 0, 0];
        assert_ne!(best_seam(&p, DVec2::new(8.0, 8.0)), 3);
        assert!((corner_angle(&p.points, 3) - 90.0).abs() < 1e-9);
    }

    #[test]
    fn loops_stop_short_by_the_seam_gap() {
        let p = l_shape();
        let plain = |to: DVec2| Step { to, deg: 0, dz: 0.0, flow: 1.0, scarf: false };
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
        let plain = |to: DVec2| Step { to, deg: 0, dz: 0.0, flow: 1.0, scarf: false };
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
