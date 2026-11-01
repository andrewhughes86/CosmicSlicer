//! Per-object layer planning: walls, then each island's fill split into top surface, bottom
//! surface or bridge, internal solid and sparse infill, from the layers above and below.

use rayon::prelude::*;

use crate::geom::{self, Join, Op, Shape};
use crate::infill::{self, Kind};
use crate::settings::Settings;
use crate::slicing::LayerZ;
use crate::toolpath::{Island, Path, Role};
use crate::walls;

/// What OrcaSlicer calls the regions of a layer's fill.
struct Regions {
    top: Vec<Shape>,
    bottom: Vec<Shape>,
    solid: Vec<Shape>,
    sparse: Vec<Shape>,
}

/// The intersection of `slices[from..to]`; empty when any is missing (past the top or bottom),
/// which makes everything there solid.
fn common(slices: &[Vec<Shape>], from: isize, to: isize) -> Vec<Shape> {
    let mut acc: Option<Vec<Shape>> = None;
    for k in from..to {
        let Some(s) = (k >= 0).then(|| slices.get(k as usize)).flatten() else { return Vec::new() };
        acc = Some(match acc {
            None => s.clone(),
            Some(a) => geom::boolean(&a, s, Op::Intersect),
        });
        if acc.as_ref().is_some_and(Vec::is_empty) {
            return Vec::new();
        }
    }
    acc.unwrap_or_default()
}

fn classify(area: &[Shape], slices: &[Vec<Shape>], i: usize, s: &Settings, w: f64) -> Regions {
    let i = i as isize;
    let (top_n, bottom_n) = (s.top_layers as isize, s.bottom_layers as isize);
    let sliver = w * w * 2.0;
    let above_one = if top_n > 0 { common(slices, i + 1, i + 2) } else { area.to_vec() };
    let below_one = if bottom_n > 0 { common(slices, i - 1, i) } else { area.to_vec() };
    let top = geom::without_slivers(geom::boolean(area, &above_one, Op::Difference), sliver);
    let bottom_all = geom::boolean(area, &below_one, Op::Difference);
    let bottom = geom::without_slivers(geom::boolean(&bottom_all, &top, Op::Difference), sliver);
    let solid_all = if s.density >= 0.999 {
        area.to_vec()
    } else {
        let above = if top_n > 0 { common(slices, i + 1, i + 1 + top_n) } else { area.to_vec() };
        let below = if bottom_n > 0 { common(slices, i - bottom_n, i) } else { area.to_vec() };
        let a = geom::boolean(area, &above, Op::Difference);
        let b = geom::boolean(area, &below, Op::Difference);
        geom::boolean(&a, &b, Op::Union)
    };
    // Solid shells grow a little into the sparse area so they anchor on it.
    let solid_all = geom::boolean(&geom::offset(&solid_all, w, Join::Miter), area, Op::Intersect);
    let solid = geom::boolean(&geom::boolean(&solid_all, &top, Op::Difference), &bottom, Op::Difference);
    let solid = geom::without_slivers(solid, sliver);
    let sparse = geom::without_slivers(geom::boolean(area, &solid_all, Op::Difference), sliver);
    Regions { top, bottom, solid, sparse }
}

fn paths_of(lines: Vec<infill::Line>, role: Role, width: f64, speed: f64, flow: f64) -> impl Iterator<Item = Path> {
    lines.into_iter().filter(|l| l.points.len() >= 2).map(move |l| Path {
        role,
        points: l.points,
        closed: l.closed,
        width,
        speed,
        flow,
    })
}

/// The islands of one object on every layer.
pub fn object(
    slices: &[Vec<Shape>],
    layers: &[LayerZ],
    s: &Settings,
    cancelled: &(dyn Fn() -> bool + Sync),
) -> Vec<Vec<Island>> {
    // Spiral vase: one wall, solid bottom, nothing else.
    let mut s = s.clone();
    if s.spiral {
        s.wall_loops = 1;
        s.top_layers = 0;
        s.density = 0.0;
    }
    let s = &s;
    (0..slices.len())
        .into_par_iter()
        .map(|i| {
            if cancelled() {
                return Vec::new();
            }
            let (Some(slice), Some(layer)) = (slices.get(i), layers.get(i)) else { return Vec::new() };
            let h = layer.height;
            let first = i == 0;
            // "Only one wall on top": top layers (nothing above) get just the outer wall.
            let top_layer = slices.get(i + 1).is_none_or(Vec::is_empty);
            let loops = if s.only_one_wall_top && top_layer { s.wall_loops.min(1) } else { s.wall_loops };
            slice
                .iter()
                .map(|island| {
                    let walled = walls::walls(island, s, h, first, loops, 1.0);
                    let mut out = Island { walls: walled.walls, fills: Vec::new() };
                    if walled.infill_area.is_empty() {
                        return out;
                    }
                    let solid_w = if first { s.widths.first_layer } else { s.widths.solid };
                    let r = classify(&walled.infill_area, slices, i, s, solid_w);
                    let z = layer.print_z;
                    let solid_angle = s.solid_angle;
                    let (infill_speed, solid_speed, top_speed) = if first {
                        (s.speeds.first_layer_infill, s.speeds.first_layer_infill, s.speeds.first_layer_infill)
                    } else {
                        (s.speeds.sparse, s.speeds.solid, s.speeds.top)
                    };
                    let top_w = if first { s.widths.first_layer } else { s.widths.top };
                    out.fills.extend(paths_of(
                        infill::fill(&r.top, infill::kind_of(&s.top_pattern), top_w, 1.0, solid_angle, i, z),
                        Role::TopSurface,
                        top_w,
                        top_speed,
                        1.0,
                    ));
                    if s.ironing && !first {
                        let ironed = geom::offset(&r.top, -top_w / 2.0, Join::Miter);
                        let lines = infill::fill(
                            &ironed,
                            Kind::Lines { dirs: 1, alternate: false, cubic: false },
                            0.1,
                            1.0,
                            solid_angle + std::f64::consts::FRAC_PI_4,
                            i,
                            z,
                        );
                        out.fills.extend(paths_of(lines, Role::Ironing, top_w, 30.0, 0.1 * 0.1 / top_w.max(0.1)));
                    }
                    if first {
                        out.fills.extend(paths_of(
                            infill::fill(
                                &r.bottom,
                                infill::kind_of(&s.bottom_pattern),
                                solid_w,
                                1.0,
                                solid_angle,
                                i,
                                z,
                            ),
                            Role::BottomSurface,
                            solid_w,
                            solid_speed,
                            1.0,
                        ));
                    } else {
                        // Over air: a bridge, at its own speed and flow, across the gap.
                        out.fills.extend(paths_of(
                            infill::fill(
                                &r.bottom,
                                Kind::Lines { dirs: 1, alternate: false, cubic: false },
                                solid_w,
                                1.0,
                                bridge_angle(&r.bottom),
                                i,
                                z,
                            ),
                            Role::Bridge,
                            solid_w,
                            s.speeds.bridge,
                            s.bridge_flow,
                        ));
                    }
                    out.fills.extend(paths_of(
                        infill::fill(&r.solid, infill::kind_of(&s.solid_pattern), solid_w, 1.0, solid_angle, i, z),
                        Role::SolidInfill,
                        solid_w,
                        solid_speed,
                        1.0,
                    ));
                    let sparse_w = if first { s.widths.first_layer } else { s.widths.sparse };
                    out.fills.extend(paths_of(
                        infill::fill(
                            &r.sparse,
                            infill::kind_of(&s.sparse_pattern),
                            sparse_w,
                            s.density,
                            s.infill_angle,
                            i,
                            z,
                        ),
                        Role::SparseInfill,
                        sparse_w,
                        infill_speed,
                        1.0,
                    ));
                    out
                })
                .collect()
        })
        .collect()
}

/// Bridges run across the narrow way of the gap: along the long side of its bounds.
fn bridge_angle(area: &[Shape]) -> f64 {
    match geom::bounds(area) {
        Some((lo, hi)) if (hi.x - lo.x) < (hi.y - lo.y) => 0.0,
        Some(_) => std::f64::consts::FRAC_PI_2,
        None => 0.0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::slicing;
    use CosmicSlicer_config::{Config, Schema};
    use CosmicSlicer_model::primitives;
    use glam::{Mat4, Vec3};

    fn never() -> bool {
        false
    }

    #[test]
    fn a_cube_has_shells_and_sparse_infill() {
        let s = Settings::read(Schema::get(), &Config::default());
        let placed = slicing::Placed::new(&primitives::cube(20.0), &Mat4::from_translation(Vec3::new(10.0, 10.0, 0.0)));
        let layers = slicing::layer_heights(placed.top(), s.first_layer_height, s.layer_height);
        let slices = slicing::slice(&placed, &layers, &never);
        let plan = object(&slices, &layers, &s, &never);
        let roles =
            |i: usize| -> Vec<Role> { plan[i].iter().flat_map(|isl| isl.fills.iter().map(|p| p.role)).collect() };
        assert!(roles(0).contains(&Role::BottomSurface));
        assert!(roles(1).contains(&Role::SolidInfill));
        assert!(roles(50).contains(&Role::SparseInfill) && !roles(50).contains(&Role::SolidInfill));
        assert!(roles(99).contains(&Role::TopSurface));
        assert!(plan[50][0].walls.len() == 2);
    }
}
