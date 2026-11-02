//! OrcaSlicer's calibration prints: values stepped up a tower layer by layer (temperature,
//! pressure advance, retraction, outer wall speed, input shaping, jerk / junction deviation),
//! and the pressure advance line and pattern tests, drawn here as toolpaths.

use glam::DVec2;

use crate::settings::{Flavor, Settings};
use crate::toolpath::{Island, LayerPlan, Path, Role};

/// Which calibration a job prints.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    PaLine,
    PaPattern,
    PaTower,
    TempTower,
    /// Max volumetric speed: the outer wall speeds up with height (mm/s per mm).
    VolSpeed,
    Vfa,
    Retraction,
    ShaperFreq,
    ShaperDamp,
    Cornering,
}

/// A calibration and its range, as OrcaSlicer's `Calib_Params`.
#[derive(Clone, Debug, PartialEq)]
pub struct Calib {
    pub mode: Mode,
    pub start: f64,
    pub end: f64,
    pub step: f64,
    /// Input shaping: frequency ranges (Hz) for X and Y, or the fixed frequencies for the
    /// damping test (`.0` used).
    pub freq_x: (f64, f64),
    pub freq_y: (f64, f64),
    /// Input shaper type ("" for the firmware's default).
    pub shaper: String,
    /// PA line: print the values beside the lines.
    pub numbers: bool,
}

impl Calib {
    pub fn new(mode: Mode, start: f64, end: f64, step: f64) -> Calib {
        Calib { mode, start, end, step, freq_x: (0.0, 0.0), freq_y: (0.0, 0.0), shaper: String::new(), numbers: true }
    }

    /// Whether the test is drawn here rather than sliced from a model.
    pub fn drawn(&self) -> bool {
        matches!(self.mode, Mode::PaLine | Mode::PaPattern)
    }
}

/// OrcaSlicer's `interpolate_value_across_layers`: from `start` on the first two layers to
/// `end` on the last, in equal bands of `step` when it is positive.
pub fn across(layer: usize, count: usize, start: f64, end: f64, step: f64) -> f64 {
    if layer <= 1 || count < 2 {
        return start;
    }
    let ratio = (layer as f64 / (count as f64 - 1.0)).clamp(0.0, 1.0);
    if step > 0.0 {
        let bands = ((end - start).abs() / step).round().max(0.0) as usize + 1;
        let band = ((ratio * bands as f64) as usize).min(bands - 1);
        let dir = if end >= start { 1.0 } else { -1.0 };
        return start + dir * band as f64 * step;
    }
    start + ratio * (end - start)
}

/// What a calibration changes on one layer.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LayerCalib {
    /// G-code at the start of the layer.
    pub gcode: Vec<String>,
    /// Outer wall speed for the layer, mm/s.
    pub outer_speed: Option<f64>,
    /// Retraction length for the layer, mm.
    pub retract_length: Option<f64>,
}

fn num(v: f64, places: usize) -> String {
    let s = format!("{v:.places$}");
    let s = if s.contains('.') { s.trim_end_matches('0').trim_end_matches('.').to_owned() } else { s };
    if s == "-0" { "0".into() } else { s }
}

/// The firmware's pressure advance command (OrcaSlicer's `set_pressure_advance`).
pub fn pressure_advance(flavor: Flavor, k: f64, tool: usize) -> String {
    let k = if k.is_finite() { k.clamp(0.0, 10.0) } else { 0.0 };
    match flavor {
        Flavor::Klipper => format!("SET_PRESSURE_ADVANCE ADVANCE={}", num(k, 4)),
        Flavor::RepRap => format!("M572 D{tool} S{}", num(k, 4)),
        Flavor::Repetier => format!("M233 X{} Y{}", num(k, 4), num(k, 4)),
        _ => format!("M900 K{}", num(k, 4)),
    }
}

/// The firmware's input shaper command for `axis` ('X', 'Y' or 'A' for both), `None` where
/// the firmware has none.
pub fn input_shaper(flavor: Flavor, axis: char, damp: f64, freq: f64, kind: &str) -> Option<String> {
    let (damp, freq) = (damp.clamp(0.0, 1.0), freq.clamp(0.0, 1000.0));
    let mut p = String::new();
    match flavor {
        Flavor::Klipper => {
            if !kind.is_empty() && kind != "Default" {
                p.push_str(&format!(" SHAPER_TYPE={kind}"));
            }
            let axes: &[char] = if axis == 'A' { &['X', 'Y'] } else { std::slice::from_ref(&axis) };
            for a in axes {
                if freq > 0.0 {
                    p.push_str(&format!(" SHAPER_FREQ_{a}={freq:.2}"));
                }
                if damp > 0.0 {
                    p.push_str(&format!(" DAMPING_RATIO_{a}={damp:.3}"));
                }
            }
            (!p.is_empty()).then(|| format!("SET_INPUT_SHAPER{p}"))
        }
        Flavor::RepRap | Flavor::Marlin2 => {
            if flavor == Flavor::Marlin2 && axis != 'A' {
                p.push_str(&format!(" {axis}"));
            }
            if flavor == Flavor::RepRap && !kind.is_empty() && kind != "Default" {
                p.push_str(&format!(" P\"{kind}\""));
            }
            if freq > 0.0 {
                p.push_str(&format!(" F{freq:.2}"));
            }
            if damp > 0.0 {
                p.push_str(&format!(" {}{damp:.3}", if flavor == Flavor::RepRap { 'S' } else { 'D' }));
            }
            (!p.is_empty()).then(|| format!("M593{p}"))
        }
        _ => None,
    }
}

/// The firmware's cornering command: Marlin 2's junction deviation, else jerk.
pub fn cornering(flavor: Flavor, value: f64, junction_deviation: bool) -> String {
    let v = if value.is_finite() { value.clamp(0.0, 1000.0) } else { 0.0 };
    match flavor {
        Flavor::Marlin2 if junction_deviation => format!("M205 J{}", num(v, 3)),
        Flavor::Klipper => format!("SET_VELOCITY_LIMIT SQUARE_CORNER_VELOCITY={}", num(v, 2)),
        Flavor::Repetier => format!("M207 X{}", num(v, 2)),
        Flavor::RepRap => format!("M566 X{} Y{}", num(v * 60.0, 0), num(v * 60.0, 0)),
        _ => format!("M205 X{} Y{}", num(v, 2), num(v, 2)),
    }
}

impl Calib {
    /// The changes for layer `layer` of `count`, its top at `z`.
    pub fn layer(&self, layer: usize, count: usize, z: f64, flavor: Flavor, tool: usize, jd: bool) -> LayerCalib {
        let mut out = LayerCalib::default();
        let (a, b, step) = (self.start, self.end, self.step);
        match self.mode {
            Mode::PaTower => {
                out.gcode.push(pressure_advance(flavor, across(layer, count, a, b, step), tool));
            }
            Mode::TempTower => {
                let t = across(layer, count, a, b, 5.0).clamp(0.0, 500.0);
                out.gcode.push(format!("M104 S{:.0} T{tool} ; temperature tower", t));
            }
            Mode::Vfa => out.outer_speed = Some(across(layer, count, a, b, step).round().max(1.0)),
            Mode::VolSpeed => out.outer_speed = Some((a + z.max(0.0) * step).round().max(1.0)),
            Mode::Retraction => {
                let length = (a + (z - 0.4).max(0.0).floor() * step).clamp(0.0, 50.0);
                out.retract_length = Some(length);
                out.gcode.push(format!("; Calib_Retraction_tower: Z_HEIGHT: {}, length:{}", num(z, 3), num(length, 3)));
            }
            Mode::ShaperFreq => {
                let line = if layer == 1 {
                    input_shaper(flavor, 'A', a, 0.0, &self.shaper)
                } else if self.freq_x == self.freq_y {
                    input_shaper(flavor, 'A', 0.0, across(layer, count, self.freq_x.0, self.freq_x.1, 0.0), "")
                } else {
                    let x = input_shaper(flavor, 'X', 0.0, across(layer, count, self.freq_x.0, self.freq_x.1, 0.0), "");
                    let y = input_shaper(flavor, 'Y', 0.0, across(layer, count, self.freq_y.0, self.freq_y.1, 0.0), "");
                    match (x, y) {
                        (Some(x), Some(y)) => Some(format!("{x}\n{y}")),
                        (x, y) => x.or(y),
                    }
                };
                out.gcode.extend(line);
            }
            Mode::ShaperDamp => {
                if layer == 1 {
                    out.gcode.extend(input_shaper(flavor, 'X', 0.0, self.freq_x.0, &self.shaper));
                    out.gcode.extend(input_shaper(flavor, 'Y', 0.0, self.freq_y.0, &self.shaper));
                } else {
                    out.gcode.extend(input_shaper(flavor, 'A', across(layer, count, a, b, 0.0), 0.0, ""));
                }
            }
            Mode::Cornering => out.gcode.push(cornering(flavor, across(layer, count, a, b, 0.0), jd)),
            Mode::PaLine | Mode::PaPattern => {}
        }
        out
    }
}

// ---- the drawn tests ----

/// An open path of the test.
fn line(role: Role, points: Vec<DVec2>, width: f64, speed: f64) -> Path {
    Path::new(role, points, false, width, speed.max(1.0), 1.0)
}

/// An island printing `paths` with pressure advance `k`.
fn island(paths: Vec<Path>, k: Option<f64>) -> Island {
    Island { fills: paths, pressure_advance: k, ..Default::default() }
}

/// OrcaSlicer's seven-segment digits, `len` mm segments: the strokes of `c` with its top
/// left at `at`, written left to right.
fn digit(c: char, at: DVec2, len: f64, gap: f64) -> Vec<Vec<DVec2>> {
    //  0---1
    //  |   |
    //  3---2
    //  |   |
    //  4---5
    let p = |x: f64, y: f64| at + DVec2::new(x * len, -y * len);
    let (p0, p1, p2, p3, p4, p5) = (p(0.0, 0.0), p(1.0, 0.0), p(1.0, 1.0), p(0.0, 1.0), p(0.0, 2.0), p(1.0, 2.0));
    let (p0_5, p4_5) = (p(0.5, 0.0), p(0.5, 2.0));
    match c {
        '0' => vec![vec![p0, p1, p5, p4, p0 - DVec2::new(0.0, gap)]],
        '1' => vec![vec![p0_5, p4_5]],
        '2' => vec![vec![p0, p1, p2, p3, p4, p5]],
        '3' => vec![vec![p0, p1, p5, p4], vec![p2 - DVec2::new(gap, 0.0), p3]],
        '4' => vec![vec![p0, p3, p2], vec![p1, p5]],
        '5' => vec![vec![p1, p0, p3, p2, p5, p4]],
        '6' => vec![vec![p1, p0, p4, p5, p2, p3]],
        '7' => vec![vec![p0, p1, p5]],
        '8' => vec![vec![p2, p3, p4, p5, p1, p0, p3]],
        '9' => vec![vec![p5, p1, p0, p3, p2]],
        '.' => vec![vec![p4_5, p4_5 + DVec2::new(0.0, len / 2.0)]],
        _ => Vec::new(),
    }
}

/// `value` written as OrcaSlicer labels its tests (at most five characters).
pub fn label(value: f64) -> String {
    let s = num(value, 4);
    let s = s.strip_prefix("0.").map_or(s.clone(), |r| format!(".{r}"));
    s.chars().take(5).collect()
}

/// `value`'s digits as paths from `at` (top left), 3 mm apart.
fn number(value: f64, at: DVec2, width: f64, speed: f64) -> Vec<Path> {
    let mut out = Vec::new();
    for (i, c) in label(value).chars().enumerate() {
        let start = at + DVec2::new(i as f64 * 3.0, 0.0);
        for stroke in digit(c, start, 2.0, width / 2.0) {
            out.push(line(Role::OuterWall, stroke, width, speed));
        }
    }
    out
}

/// The values the test steps through, at most `cap`.
fn values(c: &Calib, cap: usize) -> Vec<f64> {
    let step = if c.step.is_finite() && c.step > 0.0 { c.step } else { 0.002 };
    let n = (((c.end - c.start) / step + 1e-6).floor().max(0.0) as usize + 1).min(cap);
    (0..n).map(|i| c.start + i as f64 * step).collect()
}

/// OrcaSlicer's pressure advance line test, laid out from `at` (front left) within `room`
/// (width, depth): slow, fast, slow lines one above the other, each at the next PA value,
/// with an anchor line, and the values on a pad beside them.
pub fn pa_line(c: &Calib, s: &Settings, at: DVec2, room: DVec2, fast: f64, slow: f64) -> Vec<LayerPlan> {
    let nozzle = s.nozzle;
    let width = if nozzle < 0.51 { nozzle * 1.5 } else { nozzle * 1.05 };
    let h = s.first_layer_height;
    let space = 3.5;
    let ks = values(c, ((room.y - 10.0) / space).max(1.0) as usize);
    let n = ks.len();
    let (short, long) = (20.0, 40.0 + (room.x - 120.0).clamp(-20.0, 0.0));
    let mut islands = Vec::new();
    let x0 = at.x;
    let y0 = at.y;
    // The anchor down the left, at no PA.
    islands.push(island(
        vec![line(Role::Skirt, vec![DVec2::new(x0, y0 + n as f64 * space), DVec2::new(x0, y0)], width, slow)],
        Some(0.0),
    ));
    for (i, k) in ks.iter().enumerate() {
        let y = y0 + i as f64 * space;
        let p = |x: f64| DVec2::new(x, y);
        islands.push(island(
            vec![
                line(Role::Skirt, vec![p(x0), p(x0 + short)], width, slow),
                line(Role::OuterWall, vec![p(x0 + short), p(x0 + short + long)], width, fast),
                line(Role::Skirt, vec![p(x0 + short + long), p(x0 + 2.0 * short + long)], width, slow),
            ],
            Some(*k),
        ));
    }
    let right = x0 + 2.0 * short + long;
    islands.push(island(
        vec![line(Role::Skirt, vec![DVec2::new(right, y0), DVec2::new(right, y0 + n as f64 * space)], width, slow)],
        Some(0.0),
    ));
    let mut plans = vec![LayerPlan { print_z: h, height: h, objects: vec![(0, islands)], ..Default::default() }];
    if c.numbers {
        // A filled pad beside the lines, and the values on it a layer up.
        let pad_x = right + width;
        let pad = (0..((n as f64 + 1.0) * space / (nozzle * 0.9)) as usize)
            .map(|k| {
                let y = y0 - space + k as f64 * nozzle * 0.9;
                let (a, b) = (DVec2::new(pad_x, y), DVec2::new(pad_x + 24.0, y));
                line(Role::BottomSurface, if k % 2 == 0 { vec![a, b] } else { vec![b, a] }, nozzle, fast)
            })
            .collect();
        if let Some(first) = plans.first_mut()
            && let Some((_, isl)) = first.objects.first_mut()
        {
            isl.push(island(pad, Some(0.0)));
        }
        let mut digits = Vec::new();
        for (i, k) in ks.iter().enumerate().step_by(2) {
            let top_left = DVec2::new(pad_x + 3.0 + width, y0 + i as f64 * space + space / 2.0 + 2.0);
            digits.extend(number(*k, top_left, nozzle, 60.0));
        }
        plans.push(LayerPlan {
            print_z: 2.0 * h,
            height: h,
            objects: vec![(0, vec![island(digits, Some(0.0))])],
            ..Default::default()
        });
    }
    plans
}

/// OrcaSlicer's pressure advance pattern: for each PA value, nested 90° corners (as many as
/// the wall loops), four layers tall, side by side, the values printed above every other one.
pub fn pa_pattern(c: &Calib, s: &Settings, at: DVec2, room: DVec2) -> Vec<LayerPlan> {
    let width = s.widths.outer.max(s.nozzle);
    let walls = s.wall_loops.clamp(1, 6);
    let side = 30.0f64;
    let half = std::f64::consts::FRAC_PI_4;
    let spacing = width - s.layer_height * (1.0 - std::f64::consts::FRAC_PI_4);
    let along = spacing / half.sin();
    let one = walls as f64 * along + 2.0 + width;
    let ks = values(c, ((room.x - side) / one).max(1.0) as usize);
    let speed = s.speeds.outer;
    let first_speed = s.speeds.first_layer.min(speed);
    let mut plans = Vec::new();
    let mut z = 0.0;
    for li in 0..4 {
        let h = if li == 0 { s.first_layer_height } else { s.layer_height };
        z += h;
        let mut islands = Vec::new();
        let mut x = at.x;
        for k in &ks {
            let mut paths = Vec::new();
            for w in 0..walls {
                let x0 = x + w as f64 * along;
                let tip = DVec2::new(x0 + half.cos() * side, at.y + half.sin() * side);
                let pts = vec![DVec2::new(x0, at.y), tip, DVec2::new(x0, at.y + 2.0 * half.sin() * side)];
                paths.push(line(Role::OuterWall, pts, width, if li == 0 { first_speed } else { speed }));
            }
            islands.push(island(paths, Some(*k)));
            x += one;
        }
        if li == 3 {
            let top = at.y + 2.0 * half.sin() * side + 9.0;
            let mut digits = Vec::new();
            for (i, k) in ks.iter().enumerate().step_by(2) {
                digits.extend(number(*k, DVec2::new(at.x + i as f64 * one, top), width, first_speed));
            }
            islands.push(island(digits, ks.first().copied()));
        }
        plans.push(LayerPlan { print_z: z, height: h, objects: vec![(0, islands)], ..Default::default() });
    }
    // A tab under the numbers on the first layer, so they have something to stand on.
    let top = at.y + 2.0 * half.sin() * side + 1.0;
    let length = (ks.len() as f64 * one).max(20.0);
    let rows = (9.0 / (width * 0.9)) as usize;
    let tab: Vec<Path> = (0..rows)
        .map(|k| {
            let y = top + k as f64 * width * 0.9;
            let (a, b) = (DVec2::new(at.x, y), DVec2::new(at.x + length, y));
            line(Role::BottomSurface, if k % 2 == 0 { vec![a, b] } else { vec![b, a] }, width, first_speed)
        })
        .collect();
    for li in 0..3 {
        if let Some((_, isl)) = plans.get_mut(li).and_then(|p| p.objects.first_mut()) {
            isl.push(island(tab.clone(), ks.first().copied()));
        }
    }
    plans
}

/// How much room the pattern takes for `c` with settings `s` (width, depth), for placing it.
pub fn pa_pattern_size(c: &Calib, s: &Settings) -> DVec2 {
    let width = s.widths.outer.max(s.nozzle);
    let walls = s.wall_loops.clamp(1, 6);
    let spacing = width - s.layer_height * (1.0 - std::f64::consts::FRAC_PI_4);
    let one = walls as f64 * spacing / std::f64::consts::FRAC_PI_4.sin() + 2.0 + width;
    let n = values(c, 200).len() as f64;
    DVec2::new(n * one + 30.0, 2.0 * std::f64::consts::FRAC_PI_4.sin() * 30.0 + 12.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_step_across_layers_like_orca() {
        // 10 layers from 200 to 220 in steps of 5: five bands.
        let v: Vec<f64> = (0..10).map(|l| across(l, 10, 200.0, 220.0, 5.0)).collect();
        assert_eq!(v.first(), Some(&200.0));
        assert_eq!(v.last(), Some(&220.0));
        assert!(v.windows(2).all(|w| w[1] >= w[0]));
        // Downwards too, and smooth without a step.
        assert_eq!(across(9, 10, 240.0, 200.0, 5.0), 200.0);
        assert!((across(5, 11, 0.0, 1.0, 0.0) - 0.5).abs() < 1e-9);
        assert_eq!(across(3, 1, 7.0, 9.0, 1.0), 7.0);
    }

    #[test]
    fn firmware_commands() {
        assert_eq!(pressure_advance(Flavor::Klipper, 0.045, 0), "SET_PRESSURE_ADVANCE ADVANCE=0.045");
        assert_eq!(pressure_advance(Flavor::Marlin2, 0.1, 0), "M900 K0.1");
        assert_eq!(pressure_advance(Flavor::RepRap, 0.1, 1), "M572 D1 S0.1");
        assert_eq!(
            input_shaper(Flavor::Klipper, 'A', 0.0, 40.0, "").as_deref(),
            Some("SET_INPUT_SHAPER SHAPER_FREQ_X=40.00 SHAPER_FREQ_Y=40.00")
        );
        assert_eq!(input_shaper(Flavor::Marlin2, 'X', 0.1, 0.0, "").as_deref(), Some("M593 X D0.100"));
        assert!(input_shaper(Flavor::Marlin, 'A', 0.1, 30.0, "").is_none());
        assert_eq!(cornering(Flavor::Marlin2, 0.08, true), "M205 J0.08");
        assert_eq!(cornering(Flavor::Klipper, 5.0, false), "SET_VELOCITY_LIMIT SQUARE_CORNER_VELOCITY=5");
        assert_eq!(label(0.0450), ".045");
        assert_eq!(label(215.0), "215");
    }

    #[test]
    fn retraction_and_speed_change_with_height() {
        let c = Calib::new(Mode::Retraction, 0.0, 2.0, 0.1);
        assert_eq!(c.layer(5, 100, 1.0, Flavor::Klipper, 0, false).retract_length, Some(0.0));
        assert_eq!(c.layer(20, 100, 4.4, Flavor::Klipper, 0, false).retract_length, Some(0.4));
        let v = Calib::new(Mode::VolSpeed, 10.0, 0.0, 2.5);
        assert_eq!(v.layer(10, 100, 4.0, Flavor::Klipper, 0, false).outer_speed, Some(20.0));
        let t = Calib::new(Mode::TempTower, 230.0, 190.0, 5.0);
        assert_eq!(
            t.layer(0, 100, 0.2, Flavor::Marlin2, 0, false).gcode,
            vec!["M104 S230 T0 ; temperature tower".to_owned()]
        );
    }

    #[test]
    fn digits_draw() {
        for c in "0123456789.".chars() {
            assert!(!digit(c, DVec2::ZERO, 2.0, 0.2).is_empty(), "{c}");
        }
        assert!(digit('x', DVec2::ZERO, 2.0, 0.2).is_empty());
    }
}
