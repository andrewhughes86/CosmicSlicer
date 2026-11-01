//! Walls: loops inset from each island's outline, and the area left inside them for infill.

use crate::geom::{self, Join, Shape};
use crate::settings::Settings;
use crate::toolpath::{Path, Role};

/// One island's walls (outermost first) and the region inside them.
pub struct Walled {
    pub walls: Vec<Vec<Path>>,
    /// Where infill goes, already grown into the innermost wall by the overlap.
    pub infill_area: Vec<Shape>,
    /// The inner edge of the innermost wall, without overlap (for ironing and checks).
    pub inner_edge: Vec<Shape>,
}

/// Walls for one island at layer height `h`. `loops` overrides the wall count (top layers
/// with "only one wall on top", spiral vase).
pub fn walls(island: &Shape, s: &Settings, h: f64, first_layer: bool, loops: usize, speed_scale: f64) -> Walled {
    let wo = if first_layer { s.widths.first_layer } else { s.widths.outer };
    let wi = if first_layer { s.widths.first_layer } else { s.widths.inner };
    let (so, si) = (Settings::spacing(wo, h), Settings::spacing(wi, h));
    let (outer_speed, inner_speed) = if first_layer {
        (s.speeds.first_layer, s.speeds.first_layer)
    } else {
        (s.speeds.outer * speed_scale, s.speeds.inner * speed_scale)
    };
    let source = std::slice::from_ref(island);
    let mut walls: Vec<Vec<Path>> = Vec::new();
    // Distance from the outline to each wall's centre line.
    let mut centre = wo / 2.0;
    let mut inner_edge_at = 0.0;
    for k in 0..loops {
        if k == 1 {
            centre = wo / 2.0 + (so + si) / 2.0;
        } else if k > 1 {
            centre += si;
        }
        let rings = geom::offset(source, -centre, Join::Miter);
        let rings = geom::without_slivers(rings, wo * wo * 0.25);
        if rings.is_empty() {
            break;
        }
        let (role, width, speed) =
            if k == 0 { (Role::OuterWall, wo, outer_speed) } else { (Role::InnerWall, wi, inner_speed) };
        let paths: Vec<Path> = rings
            .iter()
            .flatten()
            .map(|c| geom::simplify_loop(c, 0.01))
            .filter(|c| c.len() >= 3)
            .map(|points| Path { role, points, closed: true, width, speed, flow: 1.0 })
            .collect();
        inner_edge_at = centre + width / 2.0;
        walls.push(paths);
    }
    let (inner_edge, infill_area) = if walls.is_empty() {
        // No room for walls (or none wanted): the whole island is infill.
        (vec![island.clone()], if loops == 0 { vec![island.clone()] } else { Vec::new() })
    } else {
        let w_last = if walls.len() == 1 { wo } else { wi };
        let edge = geom::offset(source, -inner_edge_at, Join::Miter);
        let area = geom::offset(source, -(inner_edge_at - s.infill_wall_overlap * w_last), Join::Miter);
        (edge, area)
    };
    Walled { walls, infill_area: geom::without_slivers(infill_area, wi * wi), inner_edge }
}

#[cfg(test)]
mod tests {
    use super::*;
    use CosmicSlicer_config::{Config, Schema};
    use glam::DVec2;

    #[test]
    fn two_walls_round_a_square() {
        let s = Settings::read(Schema::get(), &Config::default());
        let sq = geom::rectangle(DVec2::ZERO, DVec2::splat(20.0));
        let w = walls(&sq, &s, 0.2, false, 2, 1.0);
        assert_eq!(w.walls.len(), 2);
        assert_eq!(w.walls[0][0].role, Role::OuterWall);
        // The outer wall's centre is half a line in: a 19.58 mm square.
        let outer = w.walls[0][0].length();
        assert!((outer - 4.0 * (20.0 - 0.42)).abs() < 0.05, "{outer}");
        let a = geom::area(&w.infill_area);
        assert!(a > 15.0 * 15.0 && a < 19.0 * 19.0, "{a}");
    }

    #[test]
    fn a_thin_island_gets_what_fits() {
        let s = Settings::read(Schema::get(), &Config::default());
        let sliver = geom::rectangle(DVec2::ZERO, DVec2::new(20.0, 0.6));
        let w = walls(&sliver, &s, 0.2, false, 3, 1.0);
        assert!(w.walls.len() <= 1);
        let none = geom::rectangle(DVec2::ZERO, DVec2::new(20.0, 0.1));
        assert!(walls(&none, &s, 0.2, false, 3, 1.0).walls.is_empty());
    }
}
