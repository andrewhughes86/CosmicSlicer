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
use crate::settings::{ArcFitting, Filament, Flavor, Settings, ZHop, bead_area};
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
    pub time_by_role: [f64; 17],
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
    ops: Vec<Op>,
    pos: DVec3,
    retracted: bool,
    /// Height lifted above the layer, while lifted.
    hop: f64,
    role: Option<Role>,
    width: f64,
    extruder: usize,
    fan: f64,
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
                Some(Role::Bridge) => a.bridge,
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
                Some(Role::SparseInfill | Role::SolidInfill | Role::BottomSurface | Role::Bridge | Role::GapFill) => {
                    m.jerk_infill
                }
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

    fn retract(&mut self) {
        let r = self.s().retract;
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
            if travelled > 0.0 {
                let during = r.length * share;
                let mut from = self.pos.truncate();
                for p in wipe {
                    let e = -during * from.distance(p) / travelled;
                    let z = self.pos.z;
                    self.mv(p.extend(z), e, self.s().speeds.travel * 0.8, MoveKind::Wipe);
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
        let r = self.s().retract;
        if self.hop > 0.0 {
            let to = DVec3::new(self.pos.x, self.pos.y, self.print_z);
            self.mv(to, 0.0, self.s().speeds.travel_z.max(5.0), MoveKind::Travel);
            self.hop = 0.0;
        }
        if self.retracted {
            let length = r.length + self.s().motion.restart_extra;
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

    /// Prints `path` from its first point (loops all the way round).
    fn extrude(&mut self, path: &Path) {
        let Some(start) = path.start() else { return };
        self.travel(start);
        if let Some(r) = self.pending_region.take() {
            self.region = Some(r);
        }
        self.unretract();
        let Some(fil) = self.filament() else { return };
        if self.role != Some(path.role) {
            self.role = Some(path.role);
            self.line(format!(";TYPE:{}", path.role.name()));
        }
        if (self.width - path.width).abs() > 1.0e-4 {
            self.width = path.width;
            self.line(format!(";WIDTH:{}", f3(path.width)));
        }
        self.set_dynamics(Some(path.role));
        // Bridges get the overhang fan.
        let fan = if path.role == Role::Bridge && self.layer >= fil.fan_off_layers {
            fil.overhang_fan.max(self.fan)
        } else {
            self.fan
        };
        let bridge_fan = (fan - self.fan).abs() > 0.5;
        if bridge_fan {
            self.line(fan_command(fan));
        }
        let h = self.height;
        let area = bead_area(path.width, h) * path.flow;
        let mut speed = path.speed * self.slow;
        if !matches!(path.role, Role::Skirt | Role::Brim | Role::Bridge) {
            speed = speed.max(fil.slow_down_min_speed.min(path.speed));
        }
        // The hot end can only melt so much per second.
        if area > 0.0 {
            speed = speed.min(fil.max_volumetric / area);
        }
        let speed = speed.max(1.0);
        let e_per_mm = area / fil.area() * fil.flow_ratio;
        let mut points: Vec<DVec2> = path.points.iter().skip(1).copied().collect();
        if path.closed {
            // Stop a little short of the start, so the seam does not blob.
            let gap = self.s().nozzle * 0.1;
            points.push(start);
            if let (Some(&last), Some(&before)) = (points.last(), points.iter().rev().nth(1)) {
                let seg = last.distance(before);
                if seg > gap * 2.0 {
                    let end = before + (last - before) * ((seg - gap) / seg);
                    if let Some(l) = points.last_mut() {
                        *l = end;
                    }
                }
            }
            if path.role == Role::OuterWall {
                self.preview.seams.push([start.x as f32, start.y as f32, self.pos.z as f32]);
            }
        }
        let mut all: Vec<DVec2> = Vec::with_capacity(points.len() + 1);
        all.push(self.pos.truncate());
        all.extend(points.iter().copied());
        let fit = match self.s().motion.arc_fitting {
            ArcFitting::All => true,
            ArcFitting::SparseInfill => path.role == Role::SparseInfill,
            ArcFitting::Off => false,
        };
        let pieces: Vec<Piece> =
            if fit { arcs::fit(&all, 0.02) } else { points.iter().map(|p| Piece::Line(*p)).collect() };
        let z = self.pos.z;
        let mut length = 0.0;
        for piece in pieces {
            let from = self.pos.truncate();
            let (end, arc, d) = match piece {
                Piece::Line(p) => (p, None, from.distance(p)),
                Piece::Arc { end, center, ccw } => (end, Some((center, ccw)), arcs::arc_length(from, end, center, ccw)),
            };
            if d < 1.0e-4 {
                continue;
            }
            length += d;
            self.ops.push(Op::Move {
                to: end.extend(z),
                e: d * e_per_mm,
                feed: speed,
                kind: MoveKind::Extrude(path.role),
                arc,
                xy: true,
            });
            self.pos = end.extend(z);
        }
        if let Some(mm) = self.stats.filament_mm.get_mut(self.extruder) {
            *mm += length * e_per_mm;
        }
        if let Some(v) = self.stats.volume_mm3.get_mut(self.extruder) {
            *v += length * area;
        }
        // The Preview draws a simplified copy, so huge prints stay within memory.
        let shown =
            if self.preview_tolerance > 0.0 { geom::simplify(&all, self.preview_tolerance) } else { all.clone() };
        let pz = z as f32;
        for pair in shown.windows(2) {
            let (a, b) = (pair[0], pair[1]);
            self.preview.segments.push(Segment {
                a: [a.x as f32, a.y as f32, pz],
                b: [b.x as f32, b.y as f32, pz],
                role: path.role,
                width: path.width as f32,
                height: h as f32,
                speed: speed as f32,
                fan: fan as f32,
                temp: self.temp as f32,
                extruder: self.extruder as u8,
                flow: (area * speed) as f32,
            });
        }
        self.last_path = all;
        if bridge_fan {
            let f = self.fan;
            self.line(fan_command(f));
        }
    }

    /// Picks where a loop starts, by the seam setting, and rotates it to start there.
    fn place_seam(&mut self, path: &mut Path, object: Option<usize>) {
        if !path.closed || path.points.len() < 3 {
            return;
        }
        let here = self.pos.truncate();
        let seam = self.s().seam.clone();
        let hint = object.and_then(|o| self.seams.get(o).copied().flatten());
        let index = match seam.as_str() {
            "nearest" => nearest_index(&path.points, here),
            "back" => rear_index(&path.points),
            "random" => {
                let n = path.points.len();
                ((self.layer as u64).wrapping_mul(2_654_435_761).wrapping_add(n as u64 * 40503) % n as u64) as usize
            }
            // Aligned: under the seam of the layer below, starting from the back.
            _ => match hint {
                Some(p) => nearest_index(&path.points, p),
                None => rear_index(&path.points),
            },
        };
        let last = path.points.len() - 1;
        path.points.rotate_left(index.min(last));
        if path.role == Role::OuterWall
            && let (Some(o), Some(&p)) = (object, path.points.first())
            && let Some(slot) = self.seams.get_mut(o)
        {
            *slot = Some(p);
        }
    }

    /// Prints paths nearest first, reversing open lines when their far end is nearer.
    fn print_nearest(&mut self, mut paths: Vec<Path>, object: Option<usize>) {
        while !paths.is_empty() {
            let here = self.pos.truncate();
            let mut best = (f64::INFINITY, 0usize, false);
            for (i, p) in paths.iter().enumerate() {
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
            let mut p = paths.swap_remove(best.1);
            if best.2 {
                p.points.reverse();
            }
            self.place_seam(&mut p, object);
            self.extrude(&p);
        }
    }

    fn island(&mut self, island: &Island, object: usize) {
        // Arriving uses the plain rules; travel within the island uses its region from the
        // first extrusion on.
        self.region = None;
        self.pending_region = Region::new(&island.outline, self.s().nozzle);
        let depth = island.walls.len();
        let order: Vec<usize> = if self.s().outer_first { (0..depth).collect() } else { (0..depth).rev().collect() };
        for k in order {
            if let Some(ws) = island.walls.get(k) {
                self.print_nearest(ws.clone(), Some(object));
            }
        }
        self.print_nearest(island.fills.clone(), Some(object));
        self.region = None;
        self.pending_region = None;
    }

    fn set_fan(&mut self, fan: f64) {
        if (fan - self.fan).abs() > 0.5 {
            self.fan = fan;
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

    fn tool_change(&mut self, extruder: usize) {
        if extruder == self.extruder {
            return;
        }
        let length = self.s().retract.toolchange_length;
        if length > 0.0 && !self.retracted {
            let to = self.pos;
            self.mv(to, -length, self.s().retract.speed, MoveKind::Retract);
        }
        self.extruder = extruder;
        self.line(format!("T{extruder}"));
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

fn nearest_index(points: &[DVec2], to: DVec2) -> usize {
    points
        .iter()
        .enumerate()
        .min_by(|a, b| a.1.distance_squared(to).total_cmp(&b.1.distance_squared(to)))
        .map_or(0, |(i, _)| i)
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
        ops: Vec::with_capacity(plans.len() * 2000),
        pos: DVec3::ZERO,
        retracted: false,
        hop: 0.0,
        role: None,
        width: 0.0,
        extruder: cx.object_extruder.first().copied().unwrap_or(0).min(n_ext - 1),
        fan: 0.0,
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
        slow: 1.0,
        preview_tolerance: preview_tolerance(plans),
        region: None,
        pending_region: None,
        xy_known: false,
        last_path: Vec::new(),
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

    for (li, plan) in plans.iter().enumerate() {
        w.layer = li;
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

        // Skirt and brim, support, then the objects, grouped by extruder.
        if !plan.skirt.is_empty() {
            w.print_nearest(plan.skirt.clone(), None);
        }
        if !plan.support.is_empty() {
            w.print_nearest(plan.support.clone(), None);
        }
        let mut objects: Vec<&(usize, Vec<Island>)> = plan.objects.iter().collect();
        // The current extruder's objects first, to save a tool change.
        let current = w.extruder;
        objects.sort_by_key(|(o, _)| {
            let e = cx.object_extruder.get(*o).copied().unwrap_or(0);
            (e != current, e)
        });
        for (o, islands) in objects {
            let e = cx.object_extruder.get(*o).copied().unwrap_or(0).min(n_ext - 1);
            w.tool_change(e);
            let name = cx.object_names.get(*o).map_or_else(String::new, |n| n.replace(['\n', '\r'], " "));
            w.line(format!("; printing object {name}"));
            if labels && m.flavor == Flavor::Klipper && m.exclude_object {
                w.line(format!("EXCLUDE_OBJECT_START NAME={}", label(&name, *o)));
            } else if labels && matches!(m.flavor, Flavor::Marlin | Flavor::Marlin2) {
                w.line(format!("M486 S{o}"));
            }
            let mut remaining: Vec<&Island> = islands.iter().collect();
            while !remaining.is_empty() {
                let here = w.pos.truncate();
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
                w.island(island, *o);
            }
            if labels && m.flavor == Flavor::Klipper && m.exclude_object {
                w.line(format!("EXCLUDE_OBJECT_END NAME={}", label(&name, *o)));
            } else if labels && matches!(m.flavor, Flavor::Marlin | Flavor::Marlin2) {
                w.line("M486 S-1");
            }
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
}
