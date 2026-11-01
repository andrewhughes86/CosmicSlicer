//! Reading G-code back for the Preview (OrcaSlicer's G-code viewer): moves, features,
//! layers, fan and temperature, and a time estimate from the same motion planner the slicer
//! uses. Files from OrcaSlicer, PrusaSlicer, Cura and CosmicSlicer are understood; anything
//! else still draws, with unknown features as "Custom".

use glam::{DVec2, DVec3};

use crate::gcode::{Preview, PreviewLayer, Segment, Stats};
use crate::motion::{self, Block, BlockKind};
use crate::settings::{Filament, Settings, bead_area};
use crate::toolpath::Role;

/// Most lines read and segments drawn, so a huge or hostile file cannot exhaust memory.
const MAX_LINES: usize = 60_000_000;
const MAX_SEGMENTS: usize = 12_000_000;

/// A feature name from a `;TYPE:` or `; FEATURE:` comment.
pub fn role_of(name: &str) -> Role {
    let n = name.trim().to_ascii_lowercase();
    match n.as_str() {
        "inner wall" | "perimeter" | "wall-inner" => Role::InnerWall,
        "outer wall" | "external perimeter" | "wall-outer" => Role::OuterWall,
        "overhang wall" | "overhang perimeter" => Role::OverhangWall,
        "sparse infill" | "internal infill" | "fill" => Role::SparseInfill,
        "internal solid infill" | "solid infill" => Role::SolidInfill,
        "top surface" | "top solid infill" | "skin" => Role::TopSurface,
        "bottom surface" => Role::BottomSurface,
        "bridge" | "bridge infill" => Role::Bridge,
        "internal bridge" | "internal bridge infill" => Role::InternalBridge,
        "gap infill" | "gap fill" => Role::GapFill,
        "ironing" => Role::Ironing,
        "skirt" | "skirt/brim" => Role::Skirt,
        "brim" => Role::Brim,
        "support" | "support material" => Role::Support,
        "support interface" | "support material interface" | "support-interface" => Role::SupportInterface,
        "support transition" => Role::SupportTransition,
        "prime tower" | "wipe tower" => Role::PrimeTower,
        _ => Role::Custom,
    }
}

/// What the reader found, besides the Preview and estimate.
pub struct Viewed {
    pub preview: Preview,
    pub stats: Stats,
    pub warnings: Vec<String>,
}

fn word(line: &str, letter: char) -> Option<f64> {
    line.split_whitespace().find_map(|w| {
        let mut c = w.chars();
        let first = c.next()?;
        (first.eq_ignore_ascii_case(&letter))
            .then(|| c.as_str().parse::<f64>().ok())
            .flatten()
            .filter(|v| v.is_finite())
    })
}

fn param(line: &str, key: &str) -> Option<f64> {
    line.split_whitespace()
        .find_map(|w| w.strip_prefix(key)?.strip_prefix('=')?.parse::<f64>().ok())
        .filter(|v| v.is_finite())
}

/// Reads `text`; `s` and `fil` give the printer limits and filament for the estimate.
pub fn read(text: &str, s: &Settings, fil: &[Filament]) -> Viewed {
    let mut warnings = Vec::new();
    let mut preview = Preview::default();
    let mut blocks: Vec<Block> = Vec::new();
    // Per block: (role, layer) for the time split.
    let mut owners: Vec<(Option<Role>, usize)> = Vec::new();
    let n_ext = fil.len().max(1);
    let mut stats = Stats {
        filament_mm: vec![0.0; n_ext],
        volume_mm3: vec![0.0; n_ext],
        weight_g: vec![0.0; n_ext],
        cost: vec![0.0; n_ext],
        ..Default::default()
    };
    let (mut abs_xyz, mut abs_e) = (true, true);
    let mut pos = DVec3::ZERO;
    let mut e = 0.0f64;
    let mut feed = 30.0f64;
    let mut role = Role::Custom;
    let mut width: Option<f64> = None;
    let mut height: Option<f64> = None;
    let mut fan = 0.0f64;
    let mut temp = 0.0f64;
    let mut tool = 0usize;
    let mut accel = 0.0f64;
    let mut jerk = 0.0f64;
    let mut layer_z = f64::NEG_INFINITY;
    let mut layer_start = (0usize, 0usize, 0usize);
    let mut layers: Vec<PreviewLayer> = Vec::new();
    let mut truncated = false;
    // Files that mark their layers (";LAYER_CHANGE", ";Z:") are split there, so moves at
    // another height inside a layer (scarf seams, Z hops) stay in it; others by height.
    let mut marked = false;
    let mut new_layer = false;
    let mut marked_z: Option<f64> = None;
    let close_layer =
        |layers: &mut Vec<PreviewLayer>, preview: &Preview, start: (usize, usize, usize), z: f64, h: f64| {
            if preview.segments.len() > start.0 {
                layers.push(PreviewLayer {
                    z: z as f32,
                    height: h as f32,
                    segments: start.0..preview.segments.len(),
                    travels: start.1..preview.travels.len(),
                    seams: 0..0,
                    retractions: start.2..preview.retractions.len(),
                    time: 0.0,
                });
            }
        };
    for (n, raw) in text.lines().enumerate() {
        if n >= MAX_LINES {
            warnings.push("The file is too long; only its start is shown.".into());
            break;
        }
        let (code, comment) = match raw.split_once(';') {
            Some((c, m)) => (c.trim(), m.trim()),
            None => (raw.trim(), ""),
        };
        if !comment.is_empty() {
            if let Some(t) = comment.strip_prefix("TYPE:").or_else(|| comment.strip_prefix("FEATURE:")) {
                role = role_of(t);
            } else if let Some(w) = comment.strip_prefix("WIDTH:") {
                width = w.trim().parse::<f64>().ok().filter(|v| *v > 0.0 && *v < 10.0);
            } else if comment == "LAYER_CHANGE" {
                marked = true;
                new_layer = true;
                marked_z = None;
            } else if let Some(z) = comment.strip_prefix("Z:") {
                marked_z = z.trim().parse::<f64>().ok().filter(|v| v.is_finite());
            } else if let Some(h) = comment.strip_prefix("HEIGHT:") {
                height = h.trim().parse::<f64>().ok().filter(|v| *v > 0.0 && *v < 10.0);
            }
        }
        if code.is_empty() {
            continue;
        }
        let upper = code.to_ascii_uppercase();
        let cmd = upper.split_whitespace().next().unwrap_or("");
        match cmd {
            "G90" => abs_xyz = true,
            "G91" => abs_xyz = false,
            "M82" => abs_e = true,
            "M83" => abs_e = false,
            "G92" => {
                if let Some(v) = word(&upper, 'E') {
                    e = v;
                }
            }
            "M106" => fan = word(&upper, 'S').map_or(100.0, |v| (v / 2.55).clamp(0.0, 100.0)),
            "M107" => fan = 0.0,
            "M104" | "M109" => {
                if let Some(t) = word(&upper, 'S') {
                    temp = t;
                }
            }
            "M204" => {
                if let Some(a) = word(&upper, 'S').or_else(|| word(&upper, 'P')) {
                    accel = a;
                }
            }
            "M205" => {
                if let Some(j) = word(&upper, 'X') {
                    jerk = j;
                }
            }
            "SET_VELOCITY_LIMIT" => {
                if let Some(a) = param(&upper, "ACCEL") {
                    accel = a;
                }
                if let Some(j) = param(&upper, "SQUARE_CORNER_VELOCITY") {
                    jerk = j;
                }
            }
            c if c.starts_with('T') && c[1..].parse::<usize>().is_ok() => {
                tool = c[1..].parse::<usize>().unwrap_or(0).min(n_ext - 1);
            }
            "G0" | "G1" | "G2" | "G3" => {
                if let Some(f) = word(&upper, 'F') {
                    feed = (f / 60.0).max(0.1);
                }
                let get = |axis: char, cur: f64| match word(&upper, axis) {
                    Some(v) if abs_xyz => v,
                    Some(v) => cur + v,
                    None => cur,
                };
                let to = DVec3::new(get('X', pos.x), get('Y', pos.y), get('Z', pos.z));
                let de = match word(&upper, 'E') {
                    Some(v) if abs_e => {
                        let d = v - e;
                        e = v;
                        d
                    }
                    Some(v) => v,
                    None => 0.0,
                };
                // Arcs: unrolled into short lines for drawing and length.
                let mut pts: Vec<DVec3> = vec![to];
                if cmd == "G2" || cmd == "G3" {
                    let centre =
                        pos.truncate() + DVec2::new(word(&upper, 'I').unwrap_or(0.0), word(&upper, 'J').unwrap_or(0.0));
                    let (a, b) = (pos.truncate() - centre, to.truncate() - centre);
                    let ccw = cmd == "G3";
                    let mut sweep = a.angle_to(b);
                    if ccw && sweep <= 0.0 {
                        sweep += std::f64::consts::TAU;
                    }
                    if !ccw && sweep >= 0.0 {
                        sweep -= std::f64::consts::TAU;
                    }
                    let steps = ((sweep.abs() * a.length() / 0.5).ceil() as usize).clamp(1, 360);
                    pts = (1..=steps)
                        .map(|k| {
                            let t = k as f64 / steps as f64;
                            let (sn, cs) = (sweep * t).sin_cos();
                            let p = centre + DVec2::new(a.x * cs - a.y * sn, a.x * sn + a.y * cs);
                            p.extend(pos.z + (to.z - pos.z) * t)
                        })
                        .collect();
                }
                let xy_len: f64 = {
                    let mut l = 0.0;
                    let mut at = pos;
                    for p in &pts {
                        l += (p.truncate() - at.truncate()).length();
                        at = *p;
                    }
                    l
                };
                let extruding = de > 1e-9 && xy_len > 1e-6;
                if extruding {
                    // A new layer when printing starts at a new height.
                    let starts = if marked { new_layer } else { (pos.z - layer_z).abs() > 1e-4 };
                    if starts {
                        let z = if marked { marked_z.unwrap_or(pos.z) } else { pos.z };
                        let h = height.unwrap_or(if layer_z.is_finite() { z - layer_z } else { z }).max(0.01);
                        close_layer(&mut layers, &preview, layer_start, layer_z, h);
                        layer_start = (preview.segments.len(), preview.travels.len(), preview.retractions.len());
                        layer_z = z;
                        new_layer = false;
                    }
                    let fd = fil.get(tool).map_or(1.75, |f| f.diameter);
                    let volume = de * std::f64::consts::PI * (fd / 2.0).powi(2);
                    let h = height.unwrap_or(s.layer_height).max(0.01);
                    // The bead's width: as written, else from the volume laid down.
                    let w = width.unwrap_or_else(|| {
                        (volume / xy_len / h + h * (1.0 - std::f64::consts::FRAC_PI_4)).clamp(0.05, 5.0)
                    });
                    if let Some(mm) = stats.filament_mm.get_mut(tool) {
                        *mm += de;
                    }
                    if let Some(v) = stats.volume_mm3.get_mut(tool) {
                        *v += volume;
                    }
                    let area = bead_area(w, h);
                    let mut at = pos;
                    for p in &pts {
                        if preview.segments.len() < MAX_SEGMENTS {
                            preview.segments.push(Segment {
                                a: at.as_vec3().to_array(),
                                b: p.as_vec3().to_array(),
                                role,
                                width: w as f32,
                                height: h as f32,
                                speed: feed as f32,
                                fan: fan as f32,
                                temp: temp as f32,
                                extruder: tool as u8,
                                flow: (area * feed) as f32,
                            });
                        } else {
                            truncated = true;
                        }
                        at = *p;
                    }
                } else if xy_len > 1e-6 && preview.travels.len() < MAX_SEGMENTS {
                    preview.travels.push((pos.as_vec3().to_array(), to.as_vec3().to_array()));
                } else if de < -1e-9 {
                    preview.retractions.push(pos.as_vec3().to_array());
                }
                let d = to - pos;
                let kind = if extruding {
                    BlockKind::Extrude
                } else if xy_len < 1e-9 && d.z.abs() < 1e-9 {
                    BlockKind::Retract
                } else {
                    BlockKind::Travel
                };
                blocks.push(Block {
                    delta: [d.x, d.y, d.z, de],
                    length: (pts.len() > 1).then_some(xy_len),
                    feed,
                    accel,
                    kind,
                    jerk,
                });
                owners.push((extruding.then_some(role), layers.len()));
                pos = to;
            }
            _ => {}
        }
    }
    close_layer(&mut layers, &preview, layer_start, layer_z, height.unwrap_or(s.layer_height));
    if truncated {
        warnings.push("The file has more moves than the Preview draws; the top is cut off.".into());
    }
    let times = motion::plan(&blocks, &s.motion.limits, s.motion.flavor);
    for ((r, l), t) in owners.iter().zip(&times) {
        match r {
            Some(role) => {
                if let Some(slot) = stats.time_by_role.get_mut(role.index()) {
                    *slot += t;
                }
            }
            None => stats.travel_time += t,
        }
        // Moves before a layer's first extrusion count towards that layer.
        let index = (*l).min(layers.len().saturating_sub(1));
        if let Some(pl) = layers.get_mut(index) {
            pl.time += *t as f32;
        }
    }
    stats.total_time = times.iter().sum();
    stats.layers = layers.len();
    for (i, f) in fil.iter().enumerate() {
        let v = stats.volume_mm3.get(i).copied().unwrap_or(0.0);
        let g = v / 1000.0 * f.density;
        if let Some(slot) = stats.weight_g.get_mut(i) {
            *slot = g;
        }
        if let Some(slot) = stats.cost.get_mut(i) {
            *slot = g / 1000.0 * f.cost;
        }
    }
    preview.layers = layers;
    Viewed { preview, stats, warnings }
}

#[cfg(test)]
mod tests {
    use super::*;
    use CosmicSlicer_config::{Config, Schema};

    #[test]
    fn reads_moves_layers_and_features() {
        let schema = Schema::get();
        let s = Settings::read(schema, &Config::default());
        let fil = vec![Filament::read(schema, &Config::default(), "textured_plate_temp")];
        let g = "G90\nM83\nG1 Z0.2 F600\nG1 X10 Y10 F6000\n;TYPE:Outer wall\n;WIDTH:0.45\nG1 X20 Y10 E0.5 F1800\nG1 X20 Y20 E0.5\n\
                 G1 E-0.8\n;TYPE:Sparse infill\nG1 Z0.4\nG1 X10 Y20 E0.5\nG2 X10 Y10 I0 J-5 E0.5\n";
        let v = read(g, &s, &fil);
        assert_eq!(v.preview.layers.len(), 2);
        assert_eq!(v.preview.segments.first().map(|s| s.role), Some(Role::OuterWall));
        assert!(v.preview.segments.iter().any(|s| s.role == Role::SparseInfill));
        assert_eq!(v.preview.retractions.len(), 1);
        assert!((v.stats.filament_mm[0] - 2.0).abs() < 1e-9);
        assert!(v.stats.total_time > 0.0);
        assert!(v.stats.time_by_role[Role::OuterWall.index()] > 0.0);
    }

    #[test]
    fn garbage_is_harmless() {
        let schema = Schema::get();
        let s = Settings::read(schema, &Config::default());
        for g in ["", "G1 X\nG1 Xnan Y1e400 E\nT99\nM106 S-5\nG2 X1 I0 J0\n\u{0}\u{ffff}", "G91\nG1 X1 E1\n"] {
            let v = read(g, &s, &[]);
            assert!(v.stats.total_time.is_finite());
        }
        assert_eq!(role_of("External perimeter"), Role::OuterWall);
        assert_eq!(role_of("WALL-INNER"), Role::InnerWall);
        assert_eq!(role_of("something new"), Role::Custom);
    }
}
