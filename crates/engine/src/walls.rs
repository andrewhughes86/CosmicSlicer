//! Walls: loops inset from each island's outline, and the area left inside them for infill.
//!
//! Parts too thin for a wall loop get one line down their middle at their own width (thin
//! walls), and the narrow gaps a loop cannot reach between walls are filled the same way: what
//! OrcaSlicer's Arachne generator does with variable-width walls, and its classic generator with
//! thin-wall detection and gap fill.

use crate::geom::{self, Join, Op, Shape};
use crate::medial;
use crate::settings::Settings;
use crate::toolpath::{Path, Role};

/// One island's walls (outermost first) and the region inside them.
pub struct Walled {
    pub walls: Vec<Vec<Path>>,
    /// Lines filling narrow gaps between walls, printed after them.
    pub gaps: Vec<Path>,
    /// Where infill goes, already grown into the innermost wall by the overlap.
    pub infill_area: Vec<Shape>,
    /// The inner edge of the innermost wall, without overlap (for ironing and checks).
    pub inner_edge: Vec<Shape>,
}

struct TopSplit {
    /// The fill on top surfaces, where inner walls give way.
    top: Vec<Shape>,
    /// What inner walls still go round.
    rest: Vec<Shape>,
    /// Keeps the top's fill inside the outer wall.
    clip: Vec<Shape>,
}

/// OrcaSlicer's `split_top_surfaces`, for "only one wall on top". `orig` is the outer wall's
/// centre line.
fn split_top_surfaces(
    orig: &[Shape],
    upper: &[Shape],
    lower: &[Shape],
    s: &Settings,
    (wo, wi): (f64, f64),
    (so, si): (f64, f64),
) -> TopSplit {
    let n = s.wall_loops.max(1) as f64 - 1.0;
    let mut offset_top = 0.9 * (wo + si * n);
    let inner = 0.9 * if s.wall_loops <= 1 { 0.0 } else { si * n };
    offset_top = if offset_top > inner { offset_top - inner } else { 0.0 };
    let min_width = (so / 2.0 + 1e-5).max(s.walls.min_top_width);
    let grown_upper = geom::offset(upper, min_width, Join::Miter);
    let first_clip = geom::offset(orig, -so, Join::Miter);
    let bridge_offset = so.max(wi);
    let margin = (1.0f64).min(s.nozzle / 0.4);
    let checker =
        geom::offset(&geom::boolean(orig, lower, Op::Difference), 1.5 * bridge_offset + margin + si / 2.0, Join::Miter);
    let no_bridge = geom::boolean(orig, &checker, Op::Difference);
    let top = geom::boolean(&no_bridge, &grown_upper, Op::Difference);
    let temp_gap = geom::boolean(&top, &first_clip, Op::Difference);
    let grown_top = geom::offset(&top, offset_top + min_width - so / 2.0, Join::Miter);
    let inner_polygons = geom::boolean(orig, &grown_top, Op::Difference);
    let top = geom::boolean(&first_clip, &inner_polygons, Op::Difference);
    let clip = geom::offset(orig, so / 2.0 - s.widths.sparse / 2.0, Join::Miter);
    let rest = geom::boolean(&inner_polygons, orig, Op::Intersect);
    let rest = if s.speeds.gap > 0.0 { geom::boolean(&rest, &temp_gap, Op::Union) } else { rest };
    TopSplit { top, rest, clip }
}

/// The parts of `region` narrower than `width`, less features narrower than `min_feature`.
pub fn narrow_parts(region: &[Shape], width: f64, min_feature: f64) -> Vec<Shape> {
    if region.is_empty() {
        return Vec::new();
    }
    narrow_parts_eroded(region, &geom::offset(region, -width / 2.0, Join::Miter), width, min_feature)
}

/// [`narrow_parts`] given `eroded`, the region already offset in by half the width (the
/// walls have it: their centre line), which saves an offset.
fn narrow_parts_eroded(region: &[Shape], eroded: &[Shape], width: f64, min_feature: f64) -> Vec<Shape> {
    let opened = geom::offset(eroded, width / 2.0, Join::Miter);
    // The opening lies inside the region, so the narrow parts' total area is at most what it
    // removed: under the sliver threshold (with a margin for rounding), every piece would be
    // dropped below. Most regions end here, saving a boolean and two offsets (the bulk of
    // the walls' time).
    if geom::area(region) - geom::area(&opened) <= min_feature * min_feature * 0.5 {
        return Vec::new();
    }
    let thin = geom::boolean(region, &opened, Op::Difference);
    let thin = geom::offset(&geom::offset(&thin, -min_feature / 2.0, Join::Miter), min_feature / 2.0, Join::Miter);
    geom::without_slivers(thin, min_feature * min_feature)
}

/// Centre lines of narrow `parts`, as paths of near-constant width.
pub(crate) fn fill_narrow(
    parts: &[Shape],
    s: &Settings,
    max_w: f64,
    role: Role,
    speed: f64,
    flow: f64,
    min_len: f64,
) -> Vec<Path> {
    let mut out = Vec::new();
    // One part at a time keeps each raster small.
    for part in parts {
        let lines = medial::centre_lines(std::slice::from_ref(part), s.walls.min_bead, max_w, min_len);
        out.extend(medial::to_paths(&lines, role, speed, flow, min_len));
    }
    out
}

/// Orders a shape's loops for printing: the outer contour in the wall direction, holes the
/// other way.
fn oriented(shape: &Shape, ccw: bool) -> Vec<Vec<glam::DVec2>> {
    shape
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let want_positive = (i == 0) == ccw;
            let mut c = c.clone();
            if (geom::signed_area2(&c) > 0.0) != want_positive {
                c.reverse();
            }
            c
        })
        .collect()
}

/// OrcaSlicer's narrow outer loops: islands under `wo + spacing × (1 - this)` across get a
/// loop narrowed by half this share of the spacing...
const NARROW_OVERLAP: f64 = 0.22;
/// ...when their area is under that width times this many millimetres.
const NARROW_LOOP_LENGTH: f64 = 10.0;

/// Walls for one island at layer height `h`. `loops` overrides the wall count (top layers
/// with "only one wall on top", spiral vase). `last_layer` is the object's top layer, whose
/// infill overlaps the walls as much as the first layer's. `around` is the layers above and
/// below, for "only one wall on top": on top surfaces inner walls give way to the top's fill.
pub fn walls(
    island: &Shape,
    s: &Settings,
    h: f64,
    first_layer: bool,
    last_layer: bool,
    loops: usize,
    speed_scale: f64,
    around: Option<(&[Shape], &[Shape])>,
) -> Walled {
    let wo = if first_layer { s.widths.first_layer } else { s.widths.outer };
    let wi = if first_layer { s.widths.first_layer } else { s.widths.inner };
    let (so, si) = (Settings::spacing(wo, h), Settings::spacing(wi, h));
    let (outer_speed, inner_speed) = if first_layer {
        (s.speeds.first_layer, s.speeds.first_layer)
    } else {
        (s.speeds.outer * speed_scale, s.speeds.inner * speed_scale)
    };
    let gap_speed = if first_layer { s.speeds.first_layer } else { s.speeds.gap * speed_scale };
    let loops = if first_layer && s.walls.one_wall_first_layer { loops.min(1) } else { loops };
    let source = std::slice::from_ref(island);
    // OrcaSlicer's narrow loops (classic walls without thin-wall detection): an island too
    // narrow for an outer wall at its usual spacing, and not long, gets one a little thinner,
    // kept however small (pin tips).
    let narrow_loop = !s.walls.thin_walls && !s.walls.arachne && {
        let m = so * (1.0 - NARROW_OVERLAP);
        geom::offset(&geom::offset(source, -(wo + m) / 2.0, Join::Miter), m / 2.0, Join::Miter).is_empty()
            && geom::area(source) < (wo + m) * NARROW_LOOP_LENGTH
    };
    let wo = if narrow_loop { wo - 0.5 * NARROW_OVERLAP * so } else { wo };
    let mut walls: Vec<Vec<Path>> = Vec::new();
    let mut gaps: Vec<Path> = Vec::new();
    let min_feature = s.walls.min_feature;
    // Distance from the outline to each wall's centre line.
    let mut centre = wo / 2.0;
    let mut inner_edge_at = 0.0;
    let mut complete = true;
    // What inner walls are offset from, and how far that already is from the outline.
    let mut inner_from: Vec<Shape> = source.to_vec();
    let mut inner_base = 0.0;
    let mut top_fills: Vec<Shape> = Vec::new();
    let mut fill_clip: Vec<Shape> = Vec::new();
    for k in 0..loops {
        if k == 1 {
            // "Precise outer wall": the first inner wall sits a full width in, so the outer
            // wall is not squashed outwards and the part keeps its size (OrcaSlicer's Arachne
            // lays its beads at their spacing, classic at their width).
            let outer = if s.walls.precise_outer { wo } else { (wo + so) / 2.0 };
            centre = match (s.walls.precise_outer, s.walls.arachne) {
                (true, false) => wo + wi / 2.0,
                _ => outer + si / 2.0,
            };
        } else if k > 1 {
            centre += si;
        }
        let width = if k == 0 { wo } else { wi };
        // What this wall has to cover: from its outer edge inwards.
        let edge = (centre - width / 2.0).max(0.0);
        let available = if edge > 0.0 {
            geom::offset(&inner_from, -(edge - inner_base).max(0.0), Join::Miter)
        } else {
            source.to_vec()
        };
        // The centre line: `available` offset in by half the width.
        let centred = geom::offset(&inner_from, -(centre - inner_base), Join::Miter);
        let rings = geom::without_slivers(centred.clone(), if narrow_loop { 0.0 } else { wo * wo * 0.25 });
        // Where this wall cannot reach: thin walls at the outside, gaps further in.
        let narrow = if rings.is_empty() {
            available.clone()
        } else {
            narrow_parts_eroded(&available, &centred, width, min_feature)
        };
        if !narrow.is_empty() {
            if k == 0 {
                if s.walls.thin_walls {
                    let thin = fill_narrow(&narrow, s, wo * 1.5, Role::OuterWall, outer_speed, 1.0, wo);
                    if !thin.is_empty() {
                        walls.push(Vec::new());
                        if let Some(w0) = walls.first_mut() {
                            w0.extend(thin);
                        }
                    }
                }
            } else {
                // Arachne prints these as inner walls of varying width; classic as gap fill.
                let (role, speed, flow) = if s.walls.arachne {
                    (Role::InnerWall, inner_speed, 1.0)
                } else {
                    (Role::GapFill, gap_speed, s.walls.gap_flow)
                };
                gaps.extend(fill_narrow(&narrow, s, wi * 1.5, role, speed, flow, s.walls.filter_gap.max(wi)));
            }
        }
        if rings.is_empty() {
            complete = false;
            break;
        }
        let role = if k == 0 { Role::OuterWall } else { Role::InnerWall };
        let speed = if k == 0 { outer_speed } else { inner_speed };
        let paths: Vec<Path> = rings
            .iter()
            .flat_map(|shape| oriented(shape, s.walls.ccw))
            .map(|c| geom::simplify_loop(&c, 0.01))
            .filter(|c| c.len() >= 3)
            .map(|points| Path::new(role, points, true, width, speed, 1.0))
            .collect();
        // Infill starts half the wall's spacing in from its centre (OrcaSlicer's).
        let spacing = if k == 0 { so } else { si };
        inner_edge_at = centre + spacing / 2.0;
        // Thin walls found at this depth went in first; the loops join them.
        if let Some(level) = walls.get_mut(k) {
            level.splice(0..0, paths);
        } else {
            walls.push(paths);
        }
        if k == 0
            && k + 1 < loops
            && s.only_one_wall_top
            && let Some((upper, lower)) = around
        {
            let split = split_top_surfaces(&centred, upper, lower, s, (wo, wi), (so, si));
            top_fills = split.top;
            fill_clip = split.clip;
            inner_from = split.rest;
            inner_base = centre;
        }
    }
    let has_loops = walls.iter().flatten().any(|p| p.closed);
    let (inner_edge, infill_area) = if !has_loops {
        // No room for walls (or none wanted): the whole island is infill unless walls were
        // wanted, when thin walls took what there was.
        (vec![island.clone()], if loops == 0 { vec![island.clone()] } else { Vec::new() })
    } else if !complete {
        let edge = geom::offset(source, -inner_edge_at, Join::Miter);
        (edge, Vec::new())
    } else {
        let s_last = if walls.len() == 1 { so } else { si };
        let edge = geom::offset(&inner_from, -(inner_edge_at - inner_base), Join::Miter);
        // OrcaSlicer's overlap: classic walls the share of half the last wall's spacing and
        // half a solid line's (top and bottom surfaces' share on the first layer), Arachne the
        // share of the last wall's spacing.
        let share = if first_layer || last_layer { s.solid_wall_overlap } else { s.infill_wall_overlap };
        let overlap = if s.walls.arachne {
            share * s_last
        } else {
            let solid = if first_layer { s.widths.first_layer } else { s.widths.solid };
            share * (s_last + Settings::spacing(solid, h)) / 2.0
        };
        // Strips inside the last wall too narrow for infill are gap filled instead.
        // (OrcaSlicer's classic walls look one loop further: what is under two spacings
        // across there is gap fill, not infill.)
        let gap_width = if s.walls.arachne { wi } else { 2.0 * si };
        let narrow = narrow_parts(&edge, gap_width, min_feature);
        let mut area = geom::offset(&inner_from, -(inner_edge_at - overlap - inner_base), Join::Miter);
        if !narrow.is_empty() {
            let (role, speed, flow) = if s.walls.arachne {
                (Role::InnerWall, inner_speed, 1.0)
            } else {
                (Role::GapFill, gap_speed, s.walls.gap_flow)
            };
            gaps.extend(fill_narrow(&narrow, s, wi * 1.5, role, speed, flow, s.walls.filter_gap.max(wi)));
            area = geom::boolean(&area, &geom::offset(&narrow, wi * 0.25, Join::Miter), Op::Difference);
        }
        // The top's own fill joins what is left inside the walls.
        if !top_fills.is_empty() {
            let share = if first_layer || last_layer { 0.0 } else { s.solid_wall_overlap };
            let reach = share * (s_last + Settings::spacing(s.widths.solid, h)) / 2.0;
            let top = geom::boolean(&fill_clip, &geom::offset(&top_fills, so / 2.0, Join::Miter), Op::Intersect);
            area = geom::boolean(&area, &geom::offset(&top, reach, Join::Miter), Op::Union);
        }
        (edge, area)
    };
    Walled { walls, gaps, infill_area: geom::without_slivers(infill_area, wi * wi), inner_edge }
}

#[cfg(test)]
mod tests {
    use super::*;
    use CosmicSlicer_config::{Config, Schema};
    use glam::DVec2;

    fn settings(generator: &str) -> Settings {
        let mut c = Config::default();
        c.set("wall_generator", generator);
        Settings::read(Schema::get(), &c)
    }

    #[test]
    fn pin_tips_get_a_thinner_loop() {
        // A 0.62 mm square (a pin's tip) is too narrow for a 0.42 mm outer wall with its
        // spacing; classic walls give it one loop of a thinner line, as OrcaSlicer does.
        let mut s = settings("classic");
        s.walls.thin_walls = false;
        let tip = geom::rectangle(DVec2::ZERO, DVec2::splat(0.62));
        let w = walls(&tip, &s, 0.2, false, false, 2, 1.0, None);
        let loops: Vec<&Path> = w.walls.iter().flatten().collect();
        assert_eq!(loops.len(), 1);
        assert!(loops[0].width < s.widths.outer && loops[0].width > 0.3, "{}", loops[0].width);
        // A 2 mm square keeps the usual width.
        let w = walls(&geom::rectangle(DVec2::ZERO, DVec2::splat(2.0)), &s, 0.2, false, false, 2, 1.0, None);
        assert!(w.walls.iter().flatten().all(|p| p.width >= s.widths.outer.min(s.widths.inner) - 1e-9));
    }

    #[test]
    fn two_walls_round_a_square() {
        let s = settings("arachne");
        let sq = geom::rectangle(DVec2::ZERO, DVec2::splat(20.0));
        let w = walls(&sq, &s, 0.2, false, false, 2, 1.0, None);
        assert_eq!(w.walls.len(), 2);
        assert_eq!(w.walls[0][0].role, Role::OuterWall);
        // The outer wall's centre is half a line in: a 19.58 mm square.
        let outer = w.walls[0][0].length();
        assert!((outer - 4.0 * (20.0 - 0.42)).abs() < 0.05, "{outer}");
        let a = geom::area(&w.infill_area);
        assert!(a > 15.0 * 15.0 && a < 19.0 * 19.0, "{a}");
        // A plain square leaves no gaps.
        assert!(w.gaps.is_empty(), "{:?}", w.gaps.len());
        // Counter-clockwise by default.
        assert!(geom::signed_area2(&w.walls[0][0].points) > 0.0);
    }

    #[test]
    fn walls_turn_the_way_asked() {
        let mut c = Config::default();
        c.set("wall_direction", "cw");
        let s = Settings::read(Schema::get(), &c);
        let ring = vec![
            vec![DVec2::ZERO, DVec2::new(20.0, 0.0), DVec2::new(20.0, 20.0), DVec2::new(0.0, 20.0)],
            vec![DVec2::new(5.0, 5.0), DVec2::new(5.0, 15.0), DVec2::new(15.0, 15.0), DVec2::new(15.0, 5.0)],
        ];
        let w = walls(&ring, &s, 0.2, false, false, 1, 1.0, None);
        let areas: Vec<f64> = w.walls[0].iter().map(|p| geom::signed_area2(&p.points)).collect();
        // The outside clockwise, the hole counter-clockwise.
        assert!(areas.iter().any(|a| *a < 0.0 && a.abs() > 300.0), "{areas:?}");
        assert!(areas.iter().any(|a| *a > 0.0 && a.abs() < 300.0), "{areas:?}");
    }

    #[test]
    fn a_thin_fin_gets_a_thin_wall() {
        // A body with a 0.3 mm fin: too thin for a loop.
        let s = settings("arachne");
        let fin = geom::rectangle(DVec2::ZERO, DVec2::new(20.0, 0.3));
        let w = walls(&fin, &s, 0.2, false, false, 2, 1.0, None);
        let thin: Vec<&Path> = w.walls.iter().flatten().filter(|p| !p.closed).collect();
        assert!(!thin.is_empty());
        let len: f64 = thin.iter().map(|p| p.length()).sum();
        assert!(len > 18.0, "{len}");
        assert!(thin.iter().all(|p| p.role == Role::OuterWall && p.width < 0.42), "{:?}", thin[0].width);
        // Classic without thin-wall detection drops it, as OrcaSlicer does.
        let none = walls(&fin, &settings("classic"), 0.2, false, false, 2, 1.0, None);
        assert!(none.walls.iter().flatten().next().is_none());
    }

    #[test]
    fn a_gap_between_walls_is_filled() {
        // 1.9 mm wide: two walls a side leave a strip in the middle no wall fits.
        let s = settings("classic");
        let bar = geom::rectangle(DVec2::ZERO, DVec2::new(30.0, 1.9));
        let w = walls(&bar, &s, 0.2, false, false, 2, 1.0, None);
        assert_eq!(w.walls.len(), 2);
        let gap: f64 = w.gaps.iter().filter(|p| p.role == Role::GapFill).map(Path::length).sum();
        assert!(gap > 20.0, "{gap}");
        assert!(w.gaps.iter().all(|p| p.width >= s.walls.min_bead - 1e-9 && p.width < 0.42 * 1.5 + 1e-9));
    }

    #[test]
    fn a_thin_island_gets_what_fits() {
        let s = settings("classic");
        let sliver = geom::rectangle(DVec2::ZERO, DVec2::new(20.0, 0.6));
        let w = walls(&sliver, &s, 0.2, false, false, 3, 1.0, None);
        assert!(w.walls.len() <= 1);
        let none = geom::rectangle(DVec2::ZERO, DVec2::new(20.0, 0.1));
        assert!(walls(&none, &s, 0.2, false, false, 3, 1.0, None).walls.is_empty());
    }
}
