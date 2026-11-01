//! Colouring the sliced toolpaths for the Preview legend's view types.

#[cfg(test)]
use CosmicSlicer_engine::Role;
use CosmicSlicer_engine::Sliced;

/// OrcaSlicer's preview legend: feature, colour (same order as `Role::ALL`).
pub const FEATURES: &[(&str, [u8; 3])] = &[
    ("Inner wall", [255, 230, 77]),
    ("Outer wall", [255, 125, 56]),
    ("Overhang wall", [31, 31, 255]),
    ("Sparse infill", [176, 48, 41]),
    ("Internal solid infill", [150, 84, 204]),
    ("Top surface", [240, 64, 64]),
    ("Bottom surface", [102, 92, 199]),
    ("Bridge", [77, 128, 186]),
    ("Gap infill", [255, 255, 255]),
    ("Ironing", [255, 140, 105]),
    ("Skirt", [0, 135, 110]),
    ("Brim", [0, 59, 110]),
    ("Support", [0, 255, 0]),
    ("Support interface", [0, 128, 0]),
    ("Support transition", [0, 64, 0]),
    ("Prime tower", [179, 227, 171]),
    ("Custom", [94, 209, 148]),
];

/// The legend's "view type" choices, as in OrcaSlicer.
pub const VIEW_TYPES: &[&str] = &[
    "Line Type",
    "Layer Height",
    "Line Width",
    "Speed",
    "Fan Speed",
    "Temperature",
    "Volumetric flow rate",
    "Layer Time (linear)",
    "Layer Time (logarithmic)",
    "Tool",
    "Color Print",
];

/// OrcaSlicer's range gradient, low to high.
pub const GRADIENT: [[u8; 3]; 11] = [
    [11, 44, 122],
    [19, 89, 133],
    [28, 136, 145],
    [4, 214, 15],
    [170, 242, 0],
    [252, 249, 3],
    [245, 206, 10],
    [227, 136, 32],
    [209, 104, 48],
    [194, 82, 60],
    [148, 38, 22],
];

pub fn gradient(t: f32) -> [u8; 3] {
    let t = if t.is_finite() { t.clamp(0.0, 1.0) } else { 0.0 } * (GRADIENT.len() - 1) as f32;
    let i = (t.floor() as usize).min(GRADIENT.len() - 2);
    let f = t - i as f32;
    let (a, b) = (GRADIENT[i], GRADIENT[i + 1]);
    [0, 1, 2].map(|k| (f32::from(a[k]) + (f32::from(b[k]) - f32::from(a[k])) * f).round() as u8)
}

/// What a gradient legend shows: low and high values and their unit.
#[derive(Clone, Debug, PartialEq)]
pub struct Range {
    pub min: f32,
    pub max: f32,
    pub unit: &'static str,
    /// Log scale (layer time).
    pub log: bool,
}

fn hex(s: &str) -> [u8; 3] {
    let h = s.trim().trim_start_matches('#');
    let v = h.get(..6).and_then(|x| u32::from_str_radix(x, 16).ok()).unwrap_or(0x22D3EE);
    [(v >> 16) as u8, (v >> 8) as u8, v as u8]
}

/// A distinct colour per extruder for the "Tool" view.
const TOOLS: [[u8; 3]; 8] = [
    [34, 211, 238],
    [251, 146, 60],
    [167, 139, 250],
    [52, 211, 153],
    [248, 113, 113],
    [250, 204, 21],
    [96, 165, 250],
    [244, 114, 182],
];

/// Colours for every segment in `view`, hiding `hidden` roles; and the range for a gradient.
pub fn colours(sliced: &Sliced, view: usize, hidden: &[bool]) -> (Vec<[u8; 4]>, Option<Range>) {
    let segs = &sliced.preview.segments;
    // Which layer each segment is on, for layer times.
    let mut layer_time = vec![0.0f32; segs.len()];
    for l in &sliced.preview.layers {
        for t in layer_time.get_mut(l.segments.clone()).into_iter().flatten() {
            *t = l.time;
        }
    }
    let value = |i: usize| -> f32 {
        let Some(s) = segs.get(i) else { return 0.0 };
        match view {
            1 => s.height,
            2 => s.width,
            3 => s.speed,
            4 => s.fan,
            5 => s.temp,
            6 => s.flow,
            7 | 8 => layer_time.get(i).copied().unwrap_or(0.0),
            _ => 0.0,
        }
    };
    let range = match view {
        1..=8 => {
            let (mut lo, mut hi) = (f32::INFINITY, f32::NEG_INFINITY);
            for (i, s) in segs.iter().enumerate() {
                if hidden.get(s.role.index()).copied().unwrap_or(false) {
                    continue;
                }
                let v = value(i);
                lo = lo.min(v);
                hi = hi.max(v);
            }
            if !lo.is_finite() {
                lo = 0.0;
                hi = 0.0;
            }
            let unit = match view {
                1 | 2 => "mm",
                3 => "mm/s",
                4 => "%",
                5 => "°C",
                6 => "mm³/s",
                _ => "s",
            };
            Some(Range { min: lo, max: hi, unit, log: view == 8 })
        }
        _ => None,
    };
    let colours = segs
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let alpha = if hidden.get(s.role.index()).copied().unwrap_or(false) { 0 } else { 255 };
            let rgb = match (view, &range) {
                (0, _) => FEATURES.get(s.role.index()).map_or([128, 128, 128], |f| f.1),
                (9, _) => TOOLS[usize::from(s.extruder) % TOOLS.len()],
                (10, _) => hex(sliced.colours.get(usize::from(s.extruder)).map_or("#22D3EE", String::as_str)),
                (_, Some(r)) => {
                    let v = value(i);
                    let t = if r.max - r.min < 1e-6 {
                        0.5
                    } else if r.log {
                        ((v.max(0.01)).ln() - r.min.max(0.01).ln()) / (r.max.max(0.01).ln() - r.min.max(0.01).ln())
                    } else {
                        (v - r.min) / (r.max - r.min)
                    };
                    gradient(t)
                }
                _ => [128, 128, 128],
            };
            [rgb[0], rgb[1], rgb[2], alpha]
        })
        .collect();
    (colours, range)
}

/// "1h 2m 3s".
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn features_match_roles() {
        for (i, (name, _)) in FEATURES.iter().enumerate() {
            assert_eq!(*name, Role::from_index(i).name());
        }
    }

    #[test]
    fn gradient_ends() {
        assert_eq!(gradient(0.0), GRADIENT[0]);
        assert_eq!(gradient(1.0), GRADIENT[10]);
        assert_eq!(gradient(f32::NAN), GRADIENT[0]);
    }
}
