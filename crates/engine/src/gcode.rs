//! From layer plans to G-code: print order, seams, travel and retraction, cooling, speeds,
//! the estimates, and the moves the Preview draws.

use std::fmt::Write as _;

use glam::{DVec2, DVec3};

use crate::geom;
use crate::settings::{Filament, Settings, bead_area};
use crate::toolpath::{Island, LayerPlan, Path, Role};

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
    /// Seconds.
    pub time: f32,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Preview {
    pub layers: Vec<PreviewLayer>,
    pub segments: Vec<Segment>,
    pub travels: Vec<([f32; 3], [f32; 3])>,
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
    /// Values for `[key]` / `{key}` placeholders in custom G-code.
    pub lookup: &'a dyn Fn(&str) -> Option<String>,
    pub plate_name: String,
}

pub struct Output {
    pub gcode: String,
    pub preview: Preview,
    pub stats: Stats,
    pub warnings: Vec<String>,
}

/// Time to cover `d` mm at `v` mm/s, speeding up and slowing down at `a` mm/s².
fn move_time(d: f64, v: f64, a: f64) -> f64 {
    if d <= 0.0 || v <= 0.0 {
        return 0.0;
    }
    let a = a.max(1.0);
    if d >= v * v / a { d / v + v / a } else { 2.0 * (d / a).sqrt() }
}

struct Writer<'a> {
    cx: &'a Context<'a>,
    out: String,
    pos: DVec3,
    /// Absolute E when not relative.
    e: f64,
    retracted: bool,
    hopped: bool,
    feed: f64,
    role: Option<Role>,
    width: f64,
    extruder: usize,
    fan: f64,
    temp: f64,
    layer: usize,
    print_z: f64,
    height: f64,
    stats: Stats,
    preview: Preview,
    layer_time: f64,
    /// Where the last loop's seam was, per object, for aligned seams.
    seams: Vec<Option<DVec2>>,
    /// Speed factor for layer cooling.
    slow: f64,
    /// How far the Preview's copy of a path may stray from it, mm.
    preview_tolerance: f64,
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

    fn line(&mut self, text: &str) {
        self.out.push_str(text);
        self.out.push('\n');
    }

    fn comment(&mut self, text: &str) {
        let _ = writeln!(self.out, "; {text}");
    }

    fn accel(&self, role: Option<Role>) -> f64 {
        let a = &self.s().accel;
        if self.layer == 0 {
            return a.first_layer;
        }
        match role {
            None => a.travel,
            Some(Role::OuterWall) => a.outer,
            Some(_) => a.default,
        }
    }

    fn set_feed(&mut self, mm_s: f64) -> String {
        let f = (mm_s * 60.0).round().max(1.0);
        if (f - self.feed).abs() > 0.5 {
            self.feed = f;
            format!(" F{}", f as i64)
        } else {
            String::new()
        }
    }

    fn e_word(&mut self, de: f64) -> String {
        if self.s().gcode.relative_e {
            format!(" E{}", f5(de))
        } else {
            self.e += de;
            format!(" E{}", f5(self.e))
        }
    }

    fn retract(&mut self) {
        let r = self.s().retract;
        if self.retracted || r.length <= 0.0 {
            return;
        }
        let f = self.set_feed(r.speed);
        let e = self.e_word(-r.length);
        self.line(&format!("G1{e}{f}"));
        self.retracted = true;
        let t = r.length / r.speed;
        self.add_time(None, t);
        if r.z_hop > 0.0 && !self.hopped {
            let z = self.print_z + r.z_hop + self.s().z_offset;
            let f = self.set_feed(self.s().speeds.travel_z.max(5.0));
            self.line(&format!("G1 Z{}{f}", f3(z)));
            self.hopped = true;
            self.add_time(None, r.z_hop / self.s().speeds.travel_z.max(5.0));
        }
    }

    fn unretract(&mut self) {
        let r = self.s().retract;
        if self.hopped {
            let z = self.print_z + self.s().z_offset;
            let f = self.set_feed(self.s().speeds.travel_z.max(5.0));
            self.line(&format!("G1 Z{}{f}", f3(z)));
            self.hopped = false;
        }
        if self.retracted {
            let f = self.set_feed(r.deretract_speed);
            let e = self.e_word(r.length);
            self.line(&format!("G1{e}{f}"));
            self.retracted = false;
            self.add_time(None, r.length / r.deretract_speed);
        }
    }

    fn add_time(&mut self, role: Option<Role>, t: f64) {
        if !t.is_finite() {
            return;
        }
        self.stats.total_time += t;
        self.layer_time += t;
        match role {
            Some(r) => {
                if let Some(slot) = self.stats.time_by_role.get_mut(r.index()) {
                    *slot += t;
                }
            }
            None => self.stats.travel_time += t,
        }
    }

    /// Moves to `to` without printing, retracting first when the move is long.
    fn travel(&mut self, to: DVec2) {
        let from = self.pos.truncate();
        let d = from.distance(to);
        if d < 1.0e-4 {
            return;
        }
        if d > self.s().retract.min_travel {
            self.retract();
        }
        let speed = self.s().speeds.travel;
        let f = self.set_feed(speed);
        self.line(&format!("G0 X{} Y{}{f}", f3(to.x), f3(to.y)));
        let z = self.pos.z as f32;
        self.preview.travels.push(([from.x as f32, from.y as f32, z], [to.x as f32, to.y as f32, z]));
        self.pos = DVec3::new(to.x, to.y, self.pos.z);
        let t = move_time(d, speed, self.accel(None));
        self.add_time(None, t);
    }

    /// Prints `path` from its first point (loops all the way round).
    fn extrude(&mut self, path: &Path) {
        let Some(start) = path.start() else { return };
        self.travel(start);
        self.unretract();
        let Some(fil) = self.filament() else { return };
        if self.role != Some(path.role) {
            self.role = Some(path.role);
            let name = path.role.name();
            self.line(&format!(";TYPE:{name}"));
        }
        if (self.width - path.width).abs() > 1.0e-4 {
            self.width = path.width;
            let w = f3(path.width);
            self.line(&format!(";WIDTH:{w}"));
        }
        // Bridges and overhangs get the overhang fan.
        let fan = if path.role == Role::Bridge && self.layer >= fil.fan_off_layers {
            fil.overhang_fan.max(self.fan)
        } else {
            self.fan
        };
        let bridge_fan = (fan - self.fan).abs() > 0.5;
        if bridge_fan {
            self.line(&fan_command(fan));
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
        }
        let mut length = 0.0;
        let mut first = true;
        // The Preview draws a simplified copy, so huge prints stay within memory.
        let mut shown: Vec<DVec2> = Vec::with_capacity(points.len() + 1);
        shown.push(self.pos.truncate());
        shown.extend(points.iter().copied());
        let shown = if self.preview_tolerance > 0.0 { geom::simplify(&shown, self.preview_tolerance) } else { shown };
        for p in points {
            let from = self.pos.truncate();
            let d = from.distance(p);
            if d < 1.0e-4 {
                continue;
            }
            length += d;
            let e = self.e_word(d * e_per_mm);
            let f = if first { self.set_feed(speed) } else { String::new() };
            first = false;
            self.line(&format!("G1 X{} Y{}{e}{f}", f3(p.x), f3(p.y)));
            if let Some(mm) = self.stats.filament_mm.get_mut(self.extruder) {
                *mm += d * e_per_mm;
            }
            if let Some(v) = self.stats.volume_mm3.get_mut(self.extruder) {
                *v += d * area;
            }
            self.pos = DVec3::new(p.x, p.y, self.pos.z);
        }
        let z = self.pos.z as f32;
        for pair in shown.windows(2) {
            let (Some(a), Some(b)) = (pair.first(), pair.get(1)) else { continue };
            self.preview.segments.push(Segment {
                a: [a.x as f32, a.y as f32, z],
                b: [b.x as f32, b.y as f32, z],
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
        let t = move_time(length, speed, self.accel(Some(path.role)));
        self.add_time(Some(path.role), t);
        if bridge_fan {
            let fan = self.fan;
            self.line(&fan_command(fan));
        }
        // A wipe back along the loop hides the seam.
        if path.closed && self.s().retract.wipe {
            self.retract();
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
        let depth = island.walls.len();
        let order: Vec<usize> = if self.s().outer_first { (0..depth).collect() } else { (0..depth).rev().collect() };
        for k in order {
            if let Some(ws) = island.walls.get(k) {
                self.print_nearest(ws.clone(), Some(object));
            }
        }
        self.print_nearest(island.fills.clone(), Some(object));
    }

    fn set_fan(&mut self, fan: f64) {
        if (fan - self.fan).abs() > 0.5 {
            self.fan = fan;
            self.line(&fan_command(fan));
        }
    }

    fn tool_change(&mut self, extruder: usize) {
        if extruder == self.extruder {
            return;
        }
        let length = self.s().retract.toolchange_length;
        if length > 0.0 && !self.retracted {
            let f = self.set_feed(self.s().retract.speed);
            let e = self.e_word(-length);
            self.line(&format!("G1{e}{f}"));
        }
        self.extruder = extruder;
        self.line(&format!("T{extruder}"));
        if let Some(f) = self.filament() {
            let t = if self.layer == 0 { f.nozzle_temp_first } else { f.nozzle_temp };
            self.temp = t;
            self.line(&format!("M109 S{}", t.round() as i64));
        }
        if length > 0.0 {
            let f = self.set_feed(self.s().retract.deretract_speed);
            let e = self.e_word(length);
            self.line(&format!("G1{e}{f}"));
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

/// Fills `[key]`, `{key}` and `{key[0]}` from `lookup`; anything else is left as written.
pub fn placeholders(text: &str, lookup: &dyn Fn(&str) -> Option<String>) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(i) = rest.find(['[', '{']) {
        out.push_str(&rest[..i]);
        let open = rest.as_bytes().get(i).copied().unwrap_or(b'[');
        let close = if open == b'[' { ']' } else { '}' };
        let after = &rest[i + 1..];
        let Some(j) = after.find(close) else {
            out.push_str(&rest[i..]);
            rest = "";
            break;
        };
        let inner = after[..j].trim();
        // `name` or `name[0]`.
        let name = inner.split('[').next().unwrap_or("").trim();
        let valid = !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
        match valid.then(|| lookup(name)).flatten() {
            Some(v) => {
                let index = inner.split('[').nth(1).and_then(|s| s.trim_end_matches(']').trim().parse::<usize>().ok());
                let value = v.split(',').nth(index.unwrap_or(0)).unwrap_or(&v).trim().to_owned();
                out.push_str(&value);
            }
            None => {
                out.push_str(&rest[i..i + 1 + j + 1]);
            }
        }
        rest = &after[j + 1..];
    }
    out.push_str(rest);
    out
}

fn has_command(gcode: &str, words: &[&str]) -> bool {
    gcode.lines().any(|l| {
        let code = l.split(';').next().unwrap_or("").trim_start();
        words.iter().any(|w| code.strip_prefix(w).is_some_and(|rest| rest.is_empty() || rest.starts_with([' ', '\t'])))
    })
}

fn hms(t: f64) -> String {
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

/// Writes the G-code for `plans`.
pub fn emit(plans: &[LayerPlan], cx: &Context<'_>) -> Output {
    let s = cx.settings;
    let n_ext = cx.filaments.len().max(1);
    let mut w = Writer {
        cx,
        out: String::with_capacity(plans.len() * 20_000),
        pos: DVec3::new(0.0, 0.0, 0.0),
        e: 0.0,
        retracted: false,
        hopped: false,
        feed: 0.0,
        role: None,
        width: 0.0,
        extruder: cx.object_extruder.first().copied().unwrap_or(0).min(n_ext - 1),
        fan: 0.0,
        temp: 0.0,
        layer: 0,
        print_z: 0.0,
        height: 0.0,
        stats: Stats {
            filament_mm: vec![0.0; n_ext],
            volume_mm3: vec![0.0; n_ext],
            weight_g: vec![0.0; n_ext],
            cost: vec![0.0; n_ext],
            layers: plans.len(),
            ..Default::default()
        },
        preview: Preview::default(),
        layer_time: 0.0,
        seams: vec![None; cx.object_names.len()],
        slow: 1.0,
        preview_tolerance: preview_tolerance(plans),
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
    let start = placeholders(&s.gcode.start, cx.lookup);
    w.line("M73 P0 R0");
    if !has_command(&start, &["M140", "M190"]) {
        w.line(&format!("M140 S{}", bed_first.round() as i64));
    }
    if !has_command(&start, &["M104", "M109"]) {
        w.line(&format!("M104 S{}", nozzle_first.round() as i64));
    }
    if !has_command(&start, &["M140", "M190"]) {
        w.line(&format!("M190 S{} ; wait for bed", bed_first.round() as i64));
    }
    if !has_command(&start, &["M104", "M109"]) {
        w.line(&format!("M109 S{} ; wait for nozzle", nozzle_first.round() as i64));
    }
    w.line("; machine start G-code");
    w.out.push_str(start.trim_end());
    w.out.push('\n');
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
        w.line(&format!("T{}", w.extruder));
    }
    // Start from wherever the start G-code left the head; assume over the front-left corner.
    w.pos = DVec3::new(0.0, 0.0, 0.0);
    w.feed = 0.0;

    let total_layers = plans.len().max(1);
    let mut estimated_body_time = 0.0;
    for (li, plan) in plans.iter().enumerate() {
        w.layer = li;
        w.height = plan.height;
        w.layer_time = 0.0;
        let seg_start = w.preview.segments.len();
        let travel_start = w.preview.travels.len();
        let prev_print_z = w.print_z;
        w.print_z = plan.print_z;
        let z = plan.print_z + s.z_offset;
        let lookup_layer = |key: &str| -> Option<String> {
            match key {
                "layer_num" => Some(li.to_string()),
                "layer_z" => Some(f3(plan.print_z)),
                "previous_layer_z" => Some(f3(prev_print_z)),
                "max_layer_z" => Some(f3(plans.last().map_or(0.0, |p| p.print_z))),
                _ => (cx.lookup)(key),
            }
        };
        w.line(";LAYER_CHANGE");
        w.line(&format!(";Z:{}", f3(plan.print_z)));
        w.line(&format!(";HEIGHT:{}", f3(plan.height)));
        let before = placeholders(&s.gcode.before_layer, &lookup_layer);
        if !before.trim().is_empty() {
            w.out.push_str(before.trim_end());
            w.out.push('\n');
        }
        let percent = li * 100 / total_layers;
        let remaining = ((estimated_body_time - w.stats.total_time).max(0.0) / 60.0).round() as i64;
        w.line(&format!("M73 P{percent} R{remaining}"));
        if s.retract.on_layer_change {
            w.retract();
        }
        let f = w.set_feed(s.speeds.travel_z.max(5.0));
        w.line(&format!("G1 Z{}{f}", f3(z)));
        w.hopped = false;
        w.pos.z = plan.print_z;
        let after = placeholders(&s.gcode.layer_change, &lookup_layer);
        if !after.trim().is_empty() {
            w.out.push_str(after.trim_end());
            w.out.push('\n');
        }
        // After the first layer: printing temperatures.
        if li == 1 {
            for (i, f) in cx.filaments.iter().enumerate() {
                if (f.nozzle_temp - f.nozzle_temp_first).abs() > 0.5 {
                    let tool = if n_ext > 1 { format!(" T{i}") } else { String::new() };
                    w.line(&format!("M104 S{}{tool}", f.nozzle_temp.round() as i64));
                }
            }
            if let Some(f) = cx.filaments.get(w.extruder) {
                w.temp = f.nozzle_temp;
            }
            if (bed - bed_first).abs() > 0.5 {
                w.line(&format!("M140 S{}", bed.round() as i64));
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
            if let Some(name) = cx.object_names.get(*o) {
                let name = name.replace(['\n', '\r'], " ");
                w.comment(&format!("printing object {name}"));
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
        }
        w.preview.layers.push(PreviewLayer {
            z: plan.print_z as f32,
            height: plan.height as f32,
            segments: seg_start..w.preview.segments.len(),
            travels: travel_start..w.preview.travels.len(),
            time: w.layer_time as f32,
        });
        if li == 0 {
            estimated_body_time = w.stats.total_time * total_layers as f64;
        }
    }

    // ---- end ----
    w.retract();
    w.set_fan(0.0);
    w.line("M73 P100 R0");
    w.line("; machine end G-code");
    let end = placeholders(&s.gcode.end, cx.lookup);
    w.out.push_str(end.trim_end());
    w.out.push('\n');

    for (i, f) in cx.filaments.iter().enumerate() {
        let v = w.stats.volume_mm3.get(i).copied().unwrap_or(0.0);
        let g = v / 1000.0 * f.density;
        if let Some(slot) = w.stats.weight_g.get_mut(i) {
            *slot = g;
        }
        if let Some(slot) = w.stats.cost.get_mut(i) {
            *slot = g / 1000.0 * f.cost;
        }
    }
    let stats = w.stats.clone();
    let mut header = String::new();
    let _ = writeln!(header, "; generated by CosmicSlicer {}", env!("CARGO_PKG_VERSION"));
    let _ = writeln!(header, "; plate: {}", cx.plate_name);
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
    header.push('\n');
    let mut gcode = header;
    gcode.push_str(&w.out);
    Output { gcode, preview: w.preview, stats, warnings: w.warnings }
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
    fn placeholders_fill_known_keys_only() {
        let lookup = |k: &str| match k {
            "bed_temperature_initial_layer_single" => Some("60".to_owned()),
            "nozzle_temperature_initial_layer" => Some("215,220".to_owned()),
            _ => None,
        };
        let out = placeholders(
            "M190 S[bed_temperature_initial_layer_single]\nM109 S{nozzle_temperature_initial_layer[1]}\n{if x}G1{endif} [unknown] [",
            &lookup,
        );
        assert_eq!(out, "M190 S60\nM109 S220\n{if x}G1{endif} [unknown] [");
    }

    #[test]
    fn spots_temperature_commands() {
        assert!(has_command("G28\nM190 S60 ; wait", &["M190"]));
        assert!(!has_command("; M190 is in a comment\nM1900", &["M190"]));
    }

    #[test]
    fn time_model_is_sane() {
        assert!((move_time(100.0, 50.0, 1000.0) - (2.0 + 0.05)).abs() < 1e-9);
        assert!(move_time(0.0, 50.0, 1000.0) == 0.0);
        assert!(move_time(1.0, 500.0, 1000.0) < 0.1);
    }
}
