//! The build plates: their surface, grid, outline and the origin axes.

use glam::{Vec2, Vec3};

use super::gpu::FlatGeometry;
use crate::state::Session;
use crate::theme::{Tokens, blend, linear};

/// Builds every plate (Prepare) or just the current one (Preview).
pub fn build(session: &Session, t: &Tokens, only_current: bool) -> FlatGeometry {
    let mut g = FlatGeometry::default();
    let outline = session.bed_outline();
    let (lo, size) = session.bed();
    for p in 0..session.model.plates {
        if only_current && p != session.plate {
            continue;
        }
        let origin = session.plate_origin(p).truncate();
        let current = p == session.plate;
        let fill = if current { t.bed } else { t.bed_inactive };
        plate(&mut g, &outline, origin, fill, t, current, session.prefs.show_grid, lo, size);
    }
    // The origin axes of the current plate, as OrcaSlicer draws them.
    let o = session.plate_origin(session.plate) + lo.extend(0.0) + Vec3::new(0.0, 0.0, 0.02);
    let len = (size.min_element() * 0.12).clamp(10.0, 40.0);
    g.line(o, o + Vec3::X * len, linear(t.axis_x));
    g.line(o, o + Vec3::Y * len, linear(t.axis_y));
    g.line(o, o + Vec3::Z * len, linear(t.axis_z));
    g
}

#[allow(clippy::too_many_arguments)]
fn plate(
    g: &mut FlatGeometry,
    outline: &[Vec2],
    origin: Vec2,
    fill: egui::Color32,
    t: &Tokens,
    current: bool,
    grid: bool,
    lo: Vec2,
    size: Vec2,
) {
    let at = |p: Vec2, z: f32| (origin + p).extend(z);
    // The surface: a fan over the outline (bed shapes are convex).
    if let Some((&first, rest)) = outline.split_first() {
        for pair in rest.windows(2) {
            if let [b, c] = pair {
                g.triangle(at(first, 0.0), at(*b, 0.0), at(*c, 0.0), linear(fill));
            }
        }
    }
    // A thin slab edge under the surface gives the plate some depth.
    let edge = linear(blend(fill, t.viewport_bottom, 0.5));
    for (i, a) in outline.iter().enumerate() {
        let b = outline.get((i + 1) % outline.len()).copied().unwrap_or(*a);
        g.triangle(at(*a, 0.0), at(b, 0.0), at(b, -1.5), edge);
        g.triangle(at(*a, 0.0), at(b, -1.5), at(*a, -1.5), edge);
    }
    if grid {
        let minor = linear(blend(fill, t.bed_grid, if current { 0.55 } else { 0.3 }));
        let major = linear(blend(fill, t.bed_grid, if current { 1.0 } else { 0.5 }));
        let step = 10.0;
        let z = 0.01;
        let mut x = (lo.x / step).ceil() * step;
        while x <= lo.x + size.x + 1e-3 {
            let c = if (x / 50.0).round() * 50.0 == x { major } else { minor };
            g.line(at(Vec2::new(x, lo.y), z), at(Vec2::new(x, lo.y + size.y), z), c);
            x += step;
        }
        let mut y = (lo.y / step).ceil() * step;
        while y <= lo.y + size.y + 1e-3 {
            let c = if (y / 50.0).round() * 50.0 == y { major } else { minor };
            g.line(at(Vec2::new(lo.x, y), z), at(Vec2::new(lo.x + size.x, y), z), c);
            y += step;
        }
    }
    let border = linear(if current { t.bed_edge } else { blend(t.bed_edge, fill, 0.5) });
    for (i, a) in outline.iter().enumerate() {
        let b = outline.get((i + 1) % outline.len()).copied().unwrap_or(*a);
        g.line(at(*a, 0.02), at(b, 0.02), border);
    }
}
