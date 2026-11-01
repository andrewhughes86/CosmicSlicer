//! The Move, Rotate and Scale handles drawn on the selection, and picking them.

use std::f32::consts::TAU;

use glam::{Vec2, Vec3};

use super::camera::Camera;
use super::gpu::FlatGeometry;
use crate::theme::{Tokens, linear};
use CosmicSlicer_model::Aabb;

/// The canvas tools of OrcaSlicer's toolbar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tool {
    Move,
    Rotate,
    Scale,
    LayOnFace,
    Cut,
    Boolean,
    SupportPaint,
    SeamPaint,
    FuzzySkin,
    ColorPaint,
    Text,
    Measure,
    Assembly,
    BrimEars,
    VariableLayerHeight,
}

impl Tool {
    /// (tool, icon, name, shortcut) in toolbar order.
    pub const GIZMOS: [(Tool, &'static str, &'static str, &'static str); 14] = [
        (Tool::Move, "toolbar_move", "Move", "M"),
        (Tool::Rotate, "toolbar_rotate", "Rotate", "R"),
        (Tool::Scale, "toolbar_scale", "Scale", "S"),
        (Tool::LayOnFace, "toolbar_flatten", "Lay on face", "F"),
        (Tool::Cut, "toolbar_cut", "Cut", "C"),
        (Tool::Boolean, "toolbar_meshboolean", "Mesh boolean", "B"),
        (Tool::SupportPaint, "toolbar_support", "Support painting", "L"),
        (Tool::SeamPaint, "toolbar_seam", "Seam painting", "P"),
        (Tool::FuzzySkin, "toolbar_fuzzy_skin_paint", "Fuzzy skin painting", "H"),
        (Tool::ColorPaint, "mmu_segmentation", "Color painting", "N"),
        (Tool::Text, "toolbar_text", "Text shape", "T"),
        (Tool::Measure, "toolbar_measure", "Measure", "U"),
        (Tool::Assembly, "toolbar_assembly", "Assemble", "Y"),
        (Tool::BrimEars, "toolbar_brimears", "Brim ears", "E"),
    ];

    pub fn name(self) -> &'static str {
        if self == Tool::VariableLayerHeight {
            return "Variable layer height";
        }
        Self::GIZMOS.iter().find(|g| g.0 == self).map_or("", |g| g.2)
    }

    /// Tools with handles on the selection.
    pub fn has_handles(self) -> bool {
        matches!(self, Tool::Move | Tool::Rotate | Tool::Scale)
    }
}

/// A grabbable part of a gizmo.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Handle {
    /// Move along axis 0, 1, 2.
    Axis(usize),
    /// Rotate about axis 0, 1, 2.
    Ring(usize),
    /// Scale uniformly from a corner.
    Corner,
}

fn axis(i: usize) -> Vec3 {
    match i {
        0 => Vec3::X,
        1 => Vec3::Y,
        _ => Vec3::Z,
    }
}

fn axis_colour(t: &Tokens, i: usize) -> [f32; 4] {
    linear(match i {
        0 => t.axis_x,
        1 => t.axis_y,
        _ => t.axis_z,
    })
}

/// Handle sizes follow the selection, with a floor so tiny parts stay grabbable.
fn reach(b: &Aabb) -> f32 {
    (b.size().max_element() * 0.75).max(15.0)
}

fn ring_points(centre: Vec3, i: usize, r: f32) -> Vec<Vec3> {
    let (u, v) = match i {
        0 => (Vec3::Y, Vec3::Z),
        1 => (Vec3::X, Vec3::Z),
        _ => (Vec3::X, Vec3::Y),
    };
    (0..=64).map(|k| k as f32 / 64.0 * TAU).map(|a| centre + (u * a.cos() + v * a.sin()) * r).collect()
}

/// The handles of `tool` around `b`, lit where `hot`.
pub fn build(tool: Tool, b: &Aabb, t: &Tokens, hot: Option<Handle>, g: &mut FlatGeometry) {
    let c = b.centre();
    let r = reach(b);
    let lit = |h: Handle, colour: [f32; 4]| if hot == Some(h) { linear(t.warning) } else { colour };
    // Corner brackets mark the selection in every tool, as in OrcaSlicer.
    let bracket = linear(t.selected);
    let s = b.size() * 0.18;
    for corner in 0..8 {
        let p = Vec3::new(
            if corner & 1 == 0 { b.min.x } else { b.max.x },
            if corner & 2 == 0 { b.min.y } else { b.max.y },
            if corner & 4 == 0 { b.min.z } else { b.max.z },
        );
        let dir = Vec3::new(
            if corner & 1 == 0 { 1.0 } else { -1.0 },
            if corner & 2 == 0 { 1.0 } else { -1.0 },
            if corner & 4 == 0 { 1.0 } else { -1.0 },
        );
        for k in 0..3 {
            g.line(p, p + axis(k) * dir * s, bracket);
        }
    }
    match tool {
        Tool::Move => {
            for i in 0..3 {
                let colour = lit(Handle::Axis(i), axis_colour(t, i));
                let end = c + axis(i) * r;
                g.line(c, end, colour);
                // Arrow head: a small cone of lines.
                let (u, v) = (axis((i + 1) % 3), axis((i + 2) % 3));
                for k in 0..8 {
                    let a = k as f32 / 8.0 * TAU;
                    let base = end - axis(i) * r * 0.12 + (u * a.cos() + v * a.sin()) * r * 0.04;
                    g.line(base, end, colour);
                }
            }
        }
        Tool::Rotate => {
            for i in 0..3 {
                let colour = lit(Handle::Ring(i), axis_colour(t, i));
                let pts = ring_points(c, i, r);
                for pair in pts.windows(2) {
                    if let [a, b] = pair {
                        g.line(*a, *b, colour);
                    }
                }
            }
        }
        Tool::Scale => {
            let colour = lit(Handle::Corner, linear(t.accent));
            for k in 0..4 {
                let p = Vec3::new(
                    if k & 1 == 0 { b.min.x } else { b.max.x },
                    if k & 2 == 0 { b.min.y } else { b.max.y },
                    b.max.z,
                );
                let q = Vec3::new(
                    if k & 1 == 0 { b.max.x } else { b.min.x },
                    if k & 2 == 0 { b.min.y } else { b.max.y },
                    b.max.z,
                );
                g.line(p, q, colour);
                g.line(p, Vec3::new(p.x, p.y, b.min.z), colour);
            }
            let corner = b.max;
            let d = r * 0.06;
            for k in 0..3 {
                g.line(corner - axis(k) * d, corner + axis(k) * d, colour);
            }
        }
        _ => {}
    }
}

/// Distance from `p` to segment `a`–`b`.
fn segment_distance(p: Vec2, a: Vec2, b: Vec2) -> f32 {
    let ab = b - a;
    let t = if ab.length_squared() > 0.0 { ((p - a).dot(ab) / ab.length_squared()).clamp(0.0, 1.0) } else { 0.0 };
    (a + ab * t - p).length()
}

/// The handle under the mouse at `mouse` (canvas points), within a few points.
pub fn pick(tool: Tool, b: &Aabb, camera: &Camera, size: Vec2, mouse: Vec2) -> Option<Handle> {
    const TOLERANCE: f32 = 7.0;
    let c = b.centre();
    let r = reach(b);
    let project = |p: Vec3| camera.project(p, size);
    let mut best: Option<(f32, Handle)> = None;
    let mut consider = |d: f32, h: Handle| {
        if d < TOLERANCE && best.is_none_or(|(bd, _)| d < bd) {
            best = Some((d, h));
        }
    };
    match tool {
        Tool::Move => {
            for i in 0..3 {
                if let (Some(a), Some(e)) = (project(c), project(c + axis(i) * r)) {
                    consider(segment_distance(mouse, a, e), Handle::Axis(i));
                }
            }
        }
        Tool::Rotate => {
            for i in 0..3 {
                let pts: Vec<Vec2> = ring_points(c, i, r).into_iter().filter_map(project).collect();
                for pair in pts.windows(2) {
                    if let [a, e] = pair {
                        consider(segment_distance(mouse, *a, *e), Handle::Ring(i));
                    }
                }
            }
        }
        Tool::Scale => {
            if let Some(p) = project(b.max) {
                consider((p - mouse).length() * 0.6, Handle::Corner);
            }
        }
        _ => {}
    }
    best.map(|(_, h)| h)
}

/// Where along axis `i` through `origin` the mouse ray passes closest.
pub fn axis_param(origin: Vec3, i: usize, ray_o: Vec3, ray_d: Vec3) -> Option<f32> {
    let a = axis(i);
    let w = origin - ray_o;
    let (b, d, e) = (a.dot(ray_d), a.dot(w), ray_d.dot(w));
    let denom = 1.0 - b * b;
    if denom.abs() < 1e-5 {
        return None;
    }
    let t = (b * e - d) / denom;
    t.is_finite().then_some(t)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn closest_point_on_an_axis() {
        // A ray straight down at x = 5 meets the X axis at 5.
        let t = axis_param(Vec3::ZERO, 0, Vec3::new(5.0, 0.0, 10.0), Vec3::NEG_Z);
        assert!(t.is_some_and(|t| (t - 5.0).abs() < 1e-4), "{t:?}");
        assert!(axis_param(Vec3::ZERO, 2, Vec3::new(0.0, 0.0, 10.0), Vec3::NEG_Z).is_none());
    }

    #[test]
    fn handles_are_found_under_the_mouse() {
        let cam = Camera::default();
        let b = Aabb { min: Vec3::new(100.0, 100.0, 0.0), max: Vec3::new(120.0, 120.0, 20.0) };
        let size = Vec2::new(1200.0, 800.0);
        let tip = cam.project(b.centre() + Vec3::X * reach(&b) * 0.8, size);
        assert!(tip.is_some_and(|tip| pick(Tool::Move, &b, &cam, size, tip) == Some(Handle::Axis(0))));
        assert_eq!(pick(Tool::Move, &b, &cam, size, Vec2::new(5.0, 5.0)), None);
    }
}
