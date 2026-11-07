//! Infill patterns: every pattern in OrcaSlicer's menus has its own generator here.
//!
//! Most patterns build lines over the region's bounds in a frame turned to the infill
//! direction, then clip them to the region: parallel lines (rectilinear, grid, triangles,
//! tri-hexagon, the cubic family), waves (zig-zag, cross-hatch transitions, 3D honeycomb),
//! honeycomb columns, the gyroid and TPMS surfaces solved per layer, and plane-filling curves
//! (Hilbert, Archimedean chords, octagram spiral). Lightning grows across layers, so it is
//! planned in `lightning`; here it falls back to sparse triangles.

use std::collections::HashMap;
use std::f64::consts::{FRAC_PI_2, FRAC_PI_3, PI, TAU};

use glam::DVec2;

use crate::clipper::{ClipType, Path, Paths, Pt};
use crate::geom::{self, Contour, Join, Polyline, Shape};
use crate::rectilinear::Sweep;

/// A fill line: open, or a loop for concentric patterns.
#[derive(Clone, Debug, PartialEq)]
pub struct Line {
    pub points: Polyline,
    pub closed: bool,
    /// Printed in the order given (monotonic patterns), not nearest first.
    pub ordered: bool,
    /// Keeps the direction it was drawn in, whichever end is nearer.
    pub one_way: bool,
}

impl Line {
    fn open(points: Polyline) -> Line {
        Line { points, closed: false, ordered: false, one_way: false }
    }
}

/// OrcaSlicer's fill patterns.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// Parallel lines joined into zig-zags, turning 90° a layer when `alternate`.
    Rectilinear {
        alternate: bool,
    },
    /// Lines printed side by side in one sweep, joined into zig-zags along the edge.
    /// OrcaSlicer's "monotonic line" is the same without infill anchors (which ordered lines
    /// never get here).
    Monotonic,
    /// Monotonic, each line on its own, with no links between them.
    MonotonicLine,
    /// Parallel lines, each on its own.
    Line,
    ZigZag,
    CrossZag,
    LockedZag,
    Grid,
    Triangles,
    TriHexagon,
    Cubic,
    AdaptiveCubic,
    QuarterCubic,
    SupportCubic,
    Lightning,
    Honeycomb,
    Honeycomb3D,
    LateralHoneycomb,
    LateralLattice,
    CrossHatch,
    Gyroid,
    TpmsD,
    TpmsFk,
    Concentric,
    SpiralInset,
    Hilbert,
    Archimedean,
    Octagram,
}

/// The generator for an OrcaSlicer pattern name.
pub fn kind_of(pattern: &str) -> Kind {
    match pattern {
        "alignedrectilinear" => Kind::Rectilinear { alternate: false },
        "monotonic" => Kind::Monotonic,
        "monotonicline" => Kind::MonotonicLine,
        "line" => Kind::Line,
        "zigzag" => Kind::ZigZag,
        "crosszag" => Kind::CrossZag,
        "lockedzag" => Kind::LockedZag,
        "grid" | "rectilinear-grid" => Kind::Grid,
        "triangles" => Kind::Triangles,
        "tri-hexagon" => Kind::TriHexagon,
        "cubic" => Kind::Cubic,
        "adaptivecubic" => Kind::AdaptiveCubic,
        "quartercubic" => Kind::QuarterCubic,
        "supportcubic" => Kind::SupportCubic,
        "lightning" => Kind::Lightning,
        "honeycomb" => Kind::Honeycomb,
        "3dhoneycomb" => Kind::Honeycomb3D,
        "lateral-honeycomb" => Kind::LateralHoneycomb,
        "lateral-lattice" => Kind::LateralLattice,
        "crosshatch" => Kind::CrossHatch,
        "gyroid" => Kind::Gyroid,
        "tpmsd" => Kind::TpmsD,
        "tpmsfk" => Kind::TpmsFk,
        "concentric" => Kind::Concentric,
        "spiralinset" => Kind::SpiralInset,
        "hilbertcurve" => Kind::Hilbert,
        "archimedeanchords" => Kind::Archimedean,
        "octagramspiral" => Kind::Octagram,
        _ => Kind::Rectilinear { alternate: true },
    }
}

/// Locked zag's two densities: a skin round the edge and a skeleton inside.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Skin {
    /// mm.
    pub depth: f64,
    pub density: f64,
    pub skeleton: f64,
}

/// What a fill needs besides its region and pattern.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Params {
    pub width: f64,
    /// 0..1 of solid.
    pub density: f64,
    /// Radians.
    pub angle: f64,
    pub layer: usize,
    pub z: f64,
    /// Patterns may turn from layer to layer; off when a rotation template sets the angle.
    pub turn_by_layer: bool,
    /// Open line ends follow the edge this far (OrcaSlicer's infill anchors), mm.
    pub anchor: f64,
    /// Lines join along the edge when their ends are closer than this; under 0.05 they stay apart.
    pub anchor_max: f64,
    /// Lateral lattice angles from vertical, radians.
    pub lattice: [f64; 2],
    /// Cross zag's shift per layer, mm.
    pub shift_step: f64,
    pub skin: Skin,
    /// The layer height, when lines are spaced as beads side by side (OrcaSlicer's flow
    /// spacing: `width - height × (1 - π/4)` apart at full density); 0 spaces them a width apart.
    pub height: f64,
    /// Lines end half a line inside the region, as OrcaSlicer's object fills do (the walls
    /// leave the region reaching that far past where lines should end).
    pub inset: bool,
    /// Solid line fills spread their lines evenly over the region (OrcaSlicer's solid spacing
    /// adjustment; not for bridges, which keep their spacing).
    pub fit_solid: bool,
    /// A bridge: its lines link along the edge however far apart they end.
    pub bridge: bool,
    /// The bare lines, not joined up (what bridges above anchor on).
    pub bare: bool,
    /// Where the patterns' grid is fixed: OrcaSlicer fills in the object's own coordinates,
    /// centred on it, so lines sit the same way wherever the object is on the plate.
    pub origin: DVec2,
}

impl Params {
    pub fn new(width: f64, density: f64, angle: f64, layer: usize, z: f64) -> Params {
        Params {
            width,
            density,
            angle,
            layer,
            z,
            turn_by_layer: true,
            anchor: 0.0,
            anchor_max: 0.0,
            lattice: [-std::f64::consts::FRAC_PI_4, std::f64::consts::FRAC_PI_4],
            shift_step: 0.4,
            skin: Skin { depth: 2.0, density: 0.25, skeleton: 0.25 },
            origin: DVec2::ZERO,
            height: 0.0,
            fit_solid: false,
            bridge: false,
            bare: false,
            inset: false,
        }
    }
}

/// Most points one fill may generate before clipping; huge regions get coarser curves.
const MAX_POINTS: usize = 6_000_000;

/// Fills `area` with lines `width` wide at `density` (0..1) of solid.
///
/// `angle` is the base direction in radians, `layer` the layer index (for alternation),
/// `z` the height (for patterns that change with height).
pub fn fill(area: &[Shape], kind: Kind, width: f64, density: f64, angle: f64, layer: usize, z: f64) -> Vec<Line> {
    fill_with(area, kind, &Params::new(width, density, angle, layer, z))
}

/// Fills `area` with `kind` as `p` asks.
pub fn fill_with(area: &[Shape], kind: Kind, p: &Params) -> Vec<Line> {
    if !p.density.is_finite() {
        return Vec::new();
    }
    if p.origin != DVec2::ZERO && p.origin.is_finite() {
        // Filled where the origin is zero, then moved back.
        let o = p.origin;
        let moved: Vec<Shape> =
            area.iter().map(|sh| sh.iter().map(|c| c.iter().map(|q| *q - o).collect()).collect()).collect();
        let mut lines = fill_with(&moved, kind, &Params { origin: DVec2::ZERO, ..*p });
        for l in &mut lines {
            for q in &mut l.points {
                *q += o;
            }
        }
        return lines;
    }
    let (w, d) = (p.width, p.density.min(1.0));
    if area.is_empty() || !(d > 0.0 && w > 0.0 && w.is_finite() && p.angle.is_finite() && p.z.is_finite()) {
        return Vec::new();
    }
    if let Some(job) = rectilinear_job(kind, p) {
        return area
            .iter()
            .flat_map(|shape| crate::rectilinear::fill(&geom::paths_of(shape), &job).unwrap_or_default().lines)
            .map(|l| Line { points: geom::from_nm(&l), closed: false, ordered: job.monotonic, one_way: false })
            .filter(|l| l.points.len() >= 2)
            .collect();
    }
    let bead = if p.height > 0.0 && p.height.is_finite() { crate::settings::Settings::spacing(w, p.height) } else { w };
    let s = bead / d;
    let spacing = if p.height > 0.0 && p.height.is_finite() {
        f64::from(crate::walls::flow_spacing(w as f32, p.height as f32))
    } else {
        w
    };
    if let Some(sweeps) = sweeps_of(kind, p, spacing) {
        return crossing(area, kind, p, spacing, &sweeps);
    }
    // Loops already start half a line in.
    let shrunk;
    let area = if !p.inset || matches!(kind, Kind::Concentric | Kind::SpiralInset) {
        area
    } else {
        shrunk = geom::offset(area, -bead / 2.0, Join::Miter);
        shrunk.as_slice()
    };
    let turned = |a: f64| if p.turn_by_layer && p.layer % 2 == 1 { a + FRAC_PI_2 } else { a };
    let a = p.angle;
    // Solid fills sit half a line off the grid (OrcaSlicer's), so a square's lines are centred;
    // fitted ones are spread over the region instead, half a line in from its edge.
    let half = if d >= 0.9999 { s / 2.0 } else { 0.0 };
    // OrcaSlicer links rectilinear lines along the edge: up to three lines' spacing for solid
    // fills, however far for sparse ones.
    let link = if d > 0.8 && !p.bridge { 3.0 * bead } else { f64::INFINITY };
    let mut lines = match kind {
        Kind::Rectilinear { alternate } => parallel(area, s, if alternate { turned(a) } else { a }, half, w, link),
        Kind::Monotonic => parallel(area, s, turned(a), half, w, link),
        Kind::MonotonicLine => rows(area, s, turned(a), half, w).into_iter().map(Line::open).collect(),
        Kind::Line => rows(area, s, turned(a), half, w).into_iter().map(Line::open).collect(),
        Kind::Grid => directions(area, w, s * 2.0, &[(a, 0.0), (a + FRAC_PI_2, 0.0)]),
        Kind::Triangles | Kind::Lightning => {
            directions(area, w, s * 3.0, &[(a, 0.0), (a + FRAC_PI_3, 0.0), (a + 2.0 * FRAC_PI_3, 0.0)])
        }
        Kind::TriHexagon => {
            // One direction half a step over: triangles and hexagons instead of triangles.
            directions(area, w, s * 3.0, &[(a, 0.0), (a + FRAC_PI_3, 0.0), (a + 2.0 * FRAC_PI_3, s * 1.5)])
        }
        Kind::Cubic => cubic(area, p, s, 3, None),
        Kind::QuarterCubic => cubic(area, p, s, 4, None),
        // Finer near the walls, coarser inside: OrcaSlicer's adaptive cubic octree, flattened.
        Kind::AdaptiveCubic => cubic(area, p, s, 3, Some((2.0, 2.0))),
        Kind::SupportCubic => cubic(area, p, s, 3, Some((4.0, 1.0))),
        Kind::Honeycomb => honeycomb(area, p),
        Kind::Honeycomb3D => honeycomb_3d(area, p),
        Kind::LateralHoneycomb => lateral_honeycomb(area, p),
        Kind::LateralLattice => {
            let fam = |k: usize| {
                let tilt = p.lattice.get(k).copied().unwrap_or(0.0).clamp(-1.4, 1.4);
                let spacing = s * 2.0;
                parallel(area, spacing, a, (p.z * tilt.tan()).rem_euclid(spacing), w, 0.0)
            };
            let mut out = fam(0);
            out.extend(fam(1));
            out
        }
        Kind::CrossHatch => cross_hatch(area, p, s),
        Kind::ZigZag => waves(area, turned(a), s * std::f64::consts::SQRT_2, 0.0, false),
        Kind::CrossZag => cross_zag(area, a, w, d, p),
        Kind::LockedZag => {
            let depth = p.skin.depth.max(w);
            let skeleton = geom::offset(area, -depth, Join::Miter);
            let skin = geom::boolean(area, &skeleton, geom::Op::Difference);
            let mut out = cross_zag(&skin, a, w, p.skin.density.clamp(0.01, 1.0), p);
            out.extend(cross_zag(&skeleton, a, w, p.skin.skeleton.clamp(0.01, 1.0), p));
            out
        }
        Kind::Gyroid => gyroid(area, p, s),
        Kind::TpmsD => tpms_d(area, p, s),
        Kind::TpmsFk => tpms_fk(area, p, s),
        Kind::Concentric => concentric(area, s),
        Kind::SpiralInset => spiral_inset(area, s),
        Kind::Hilbert => hilbert(area, a, s),
        Kind::Archimedean => archimedean(area, s),
        Kind::Octagram => octagram(area, s),
    };
    if p.anchor > 0.0 && !(p.anchor_max >= 0.05 && matches!(kind, Kind::CrossHatch)) {
        // Never so long that neighbouring anchors meet.
        anchor_ends(&mut lines, area, p.anchor.min(s * 0.45), w * 0.25);
    }
    lines.retain(|l| l.points.len() >= 2);
    lines
}

/// The directions of the patterns Orca fills with `fill_surface_by_multilines`, for the sparse
/// infill of an object. `spacing` is the flow's.
fn sweeps_of(kind: Kind, p: &Params, spacing: f64) -> Option<Vec<Sweep>> {
    if !p.inset {
        return None;
    }
    let sweep = |angle: f32, shift: f32| Sweep { angle, shift };
    let (third, sixth) = ((2.0 * PI / 3.0) as f32, FRAC_PI_3 as f32);
    let quarter = FRAC_PI_2 as f32;
    let density = p.density.min(1.0) as f32;
    let spacing = f64::from(spacing as f32);
    Some(match kind {
        Kind::Grid => vec![sweep(0.0, 0.0), sweep(quarter, 0.0)],
        Kind::Triangles => vec![sweep(0.0, 0.0), sweep(sixth, 0.0), sweep(third, 0.0)],
        Kind::TriHexagon => {
            vec![sweep(0.0, 0.0), sweep(sixth, 0.0), sweep(third, (1.5 * spacing / f64::from(density)) as f32)]
        }
        Kind::Cubic => {
            let dx = (0.5f64.sqrt() * p.z) as f32;
            vec![sweep(0.0, dx), sweep(sixth, -dx), sweep(third, dx)]
        }
        Kind::QuarterCubic => {
            let scaled = spacing / 1.0e-6;
            let line = scaled as i64;
            let period = (scaled / f64::from(density)) as i64 * 4;
            if period <= 0 {
                return None;
            }
            let shift = |turn: f64| -> f32 {
                let root = f64::from(std::f32::consts::FRAC_1_SQRT_2);
                let s = (root * (p.z / 1.0e-6 + turn * period as f64 * 2.0)) as i64 % period;
                let s = s.min(period - s).min(period / 2 - line / 2).max(line / 2);
                (s as f64 * 1.0e-6) as f32
            };
            let (a, b) = (shift(0.0), shift(0.5));
            vec![sweep(0.0, a), sweep(0.0, -a), sweep(quarter, b), sweep(quarter, -b)]
        }
        Kind::LateralLattice => {
            let dx = |k: usize| (p.lattice.get(k).copied().unwrap_or(0.0).clamp(-1.4, 1.4).tan() * p.z) as f32;
            vec![sweep(quarter, dx(0)), sweep(quarter, dx(1))]
        }
        _ => return None,
    })
}

/// `fill_surface_by_multilines`: lines over each region in every direction, cut half a line
/// inside it and joined along its edge by `connect_infill`.
fn crossing(area: &[Shape], kind: Kind, p: &Params, spacing: f64, sweeps: &[Sweep]) -> Vec<Line> {
    let density = p.density.min(1.0) as f32;
    let spacing = f64::from(spacing as f32);
    let angle = crate::rectilinear::direction(p.angle, false);
    // Orca turns these lines round on odd layers, and a grid's stay that way when printed.
    let reversed = matches!(kind, Kind::Grid | Kind::LateralLattice) && p.layer % 2 == 1;
    let one_way = matches!(kind, Kind::Grid);
    let mut out = Vec::new();
    for shape in area {
        let source = geom::paths_of(shape);
        let Some(lines) = crate::rectilinear::crossing_lines(&source, spacing, density, angle, sweeps) else {
            continue;
        };
        let contracted = geom::offset_nm(std::slice::from_ref(shape), -((0.5 * spacing / 1.0e-6) as f32), Join::Miter);
        let edge: Paths =
            if contracted.is_empty() { source } else { contracted.iter().flat_map(geom::paths_of).collect() };
        let lines = geom::clip_open(&lines, &edge, ClipType::Intersection);
        let lines: Vec<Polyline> = lines.iter().map(|l| geom::from_nm(l)).collect();
        let joined = if p.anchor_max >= 0.05 {
            let boundary: Vec<Contour> = edge.iter().map(|c| geom::from_nm(c)).collect();
            crate::connect::connect_infill(lines, &boundary, spacing, p.anchor, p.anchor_max)
        } else {
            lines
        };
        out.extend(joined.into_iter().map(|mut points| {
            if reversed {
                points.reverse();
            }
            Line { one_way, ..Line::open(points) }
        }));
    }
    out.retain(|l| l.points.len() >= 2);
    out
}

/// What Orca's `fill_surface_by_lines` needs, for the object fills that go through it.
/// `spacing` is the flow's, in Orca's floats.
fn rectilinear_job(kind: Kind, p: &Params) -> Option<crate::rectilinear::Job> {
    let (monotonic, line) = match kind {
        Kind::Rectilinear { .. } => (false, false),
        Kind::Monotonic => (true, false),
        Kind::MonotonicLine => (true, true),
        _ => return None,
    };
    if !p.inset {
        return None;
    }
    let spacing = if p.height > 0.0 && p.height.is_finite() {
        f64::from(crate::walls::flow_spacing(p.width as f32, p.height as f32))
    } else {
        p.width
    };
    let alternate = !matches!(kind, Kind::Rectilinear { alternate: false });
    Some(crate::rectilinear::Job {
        spacing,
        density: p.density.min(1.0) as f32,
        angle: crate::rectilinear::direction(p.angle, alternate && p.turn_by_layer && p.layer % 2 == 1),
        dont_connect: line || (!p.fit_solid && !p.bridge && p.anchor_max < 0.05),
        monotonic,
        dont_adjust: !p.fit_solid && p.bridge,
    })
}

/// The patterns made of straight lines across the region.
pub fn is_line_pattern(kind: Kind) -> bool {
    matches!(kind, Kind::Rectilinear { .. } | Kind::Monotonic | Kind::MonotonicLine | Kind::Line)
}

/// The line width a solid fill of `area` (one region) prints at: Orca widens the lines by what
/// it spread them apart, so the fill stays solid.
pub fn solid_width(area: &[Shape], kind: Kind, p: &Params) -> f64 {
    let moved: Vec<Shape> =
        area.iter().map(|sh| sh.iter().map(|c| c.iter().map(|q| *q - p.origin).collect()).collect()).collect();
    match (rectilinear_job(kind, p), moved.first()) {
        (Some(job), Some(shape)) if p.fit_solid => crate::rectilinear::spacing_of(&geom::paths_of(shape), &job)
            // `Flow::with_spacing`, in its floats.
            .map_or(p.width, |fit| f64::from(p.width as f32 + (fit as f32 - job.spacing as f32))),
        _ => p.width,
    }
}

/// Turns `p` by `angle`.
fn turn(p: DVec2, (sin, cos): (f64, f64)) -> DVec2 {
    DVec2::new(p.x * cos - p.y * sin, p.x * sin + p.y * cos)
}

/// Builds lines in a frame turned by `angle` (so "along" is +x) over the region's bounds there,
/// clips them to the region and turns them back.
fn framed(area: &[Shape], angle: f64, make: impl FnOnce(DVec2, DVec2) -> Vec<Polyline>) -> Vec<Polyline> {
    let (sin, cos) = angle.sin_cos();
    let back = (sin, cos);
    let to = (-sin, cos);
    let turned: Vec<Shape> =
        area.iter().map(|sh| sh.iter().map(|c| c.iter().map(|q| turn(*q, to)).collect()).collect()).collect();
    let Some((lo, hi)) = geom::bounds(&turned) else { return Vec::new() };
    let raw = make(lo, hi);
    geom::clip_lines(&raw, &turned).into_iter().map(|l| l.into_iter().map(|q| turn(q, back)).collect()).collect()
}

/// Straight lines `spacing` apart at angle `a`, shifted by `phase`, clipped to the area.
fn rows(area: &[Shape], spacing: f64, a: f64, phase: f64, width: f64) -> Vec<Polyline> {
    let lines = geom::hatch_lines(area, spacing, a, phase, 100_000);
    geom::clip_lines(&lines, area).into_iter().filter(|l| geom::length(l) > width * 0.5).collect()
}

/// Pieces of lines along `a` as regions OrcaSlicer prints in one go: runs of rows where each
/// piece has exactly one piece next to it in the row before (a hole splits the rows into a
/// region each side of it). Each region's pieces run the same way, row by row.
fn regions(pieces: Vec<Polyline>, a: f64, spacing: f64, phase: f64) -> Vec<Vec<Polyline>> {
    let dir = DVec2::new(a.cos(), a.sin());
    let normal = dir.perp();
    // (row, from, to along the row, piece)
    let mut keyed: Vec<(i64, f64, f64, Polyline)> = pieces
        .into_iter()
        .filter_map(|mut l| {
            let (s, e) = (*l.first()?, *l.last()?);
            if (e - s).dot(dir) < 0.0 {
                l.reverse();
            }
            let (from, to) =
                l.iter().fold((f64::MAX, f64::MIN), |(lo, hi), q| (lo.min(q.dot(dir)), hi.max(q.dot(dir))));
            let row = ((l.first().copied().unwrap_or_default().dot(normal) - phase) / spacing).round();
            Some((row.clamp(-1.0e15, 1.0e15) as i64, from, to, l))
        })
        .collect();
    keyed.sort_by(|x, y| x.0.cmp(&y.0).then(x.1.total_cmp(&y.1)));
    let overlap = |p: &(i64, f64, f64), q: &(i64, f64, f64)| p.1 < q.2 && q.1 < p.2;
    // Open regions: their pieces, and the last piece's (row, from, to).
    let mut done: Vec<Vec<Polyline>> = Vec::new();
    let mut open: Vec<(Vec<Polyline>, (i64, f64, f64))> = Vec::new();
    let mut i = 0;
    while i < keyed.len() {
        let row = keyed.get(i).map_or(0, |k| k.0);
        let end = keyed.iter().skip(i).position(|k| k.0 != row).map_or(keyed.len(), |n| i + n);
        let here: Vec<(i64, f64, f64)> = keyed.get(i..end).unwrap_or(&[]).iter().map(|k| (k.0, k.1, k.2)).collect();
        let mut next: Vec<(Vec<Polyline>, (i64, f64, f64))> = Vec::new();
        let mut taken = vec![false; open.len()];
        for (j, piece) in here.iter().enumerate() {
            let Some(line) = keyed.get_mut(i + j).map(|k| std::mem::take(&mut k.3)) else { continue };
            // The one open region whose last piece is next to this one and to no other.
            let touching: Vec<usize> = (0..open.len())
                .filter(|&o| open.get(o).is_some_and(|r| r.1.0 == row - 1 && overlap(&r.1, piece)))
                .collect();
            let only = match touching.as_slice() {
                [o] => {
                    open.get(*o).is_some_and(|r| here.iter().filter(|h| overlap(&r.1, h)).count() == 1).then_some(*o)
                }
                _ => None,
            };
            match only.filter(|o| !taken.get(*o).copied().unwrap_or(true)) {
                Some(o) => {
                    if let Some(t) = taken.get_mut(o) {
                        *t = true;
                    }
                    if let Some(r) = open.get_mut(o) {
                        let mut lines = std::mem::take(&mut r.0);
                        lines.push(line);
                        next.push((lines, *piece));
                    }
                }
                None => next.push((vec![line], *piece)),
            }
        }
        for (r, t) in open.into_iter().zip(taken) {
            if !t {
                done.push(r.0);
            }
        }
        open = next;
        i = end;
    }
    done.extend(open.into_iter().map(|r| r.0));
    done.retain(|r| !r.is_empty());
    done
}

/// Splits solid infill into what fills well with lines and the narrow rest, which fills better
/// concentrically: OrcaSlicer's `split_solid_surface` ("detect narrow internal solid infill",
/// after PrusaSlicer's anti-vibration filter), in its integers. For line patterns, the lines
/// (`angle` as the region sets it, `odd` turning them a quarter more) that stay after opening
/// by two and three spacings, less runs of short ones, fill with lines; other patterns only
/// lose what an opening by one spacing removes. `width` and `height` are the solid flow's,
/// `origin` the object's centre Orca slices round. Returns (lines, narrow).
pub fn split_narrow_solid(
    area: &[Shape],
    width: f64,
    height: f64,
    angle: f64,
    odd: bool,
    origin: DVec2,
    kind: Kind,
) -> (Vec<Shape>, Vec<Shape>) {
    let spacing = f64::from(crate::walls::flow_spacing(width as f32, height as f32));
    let sp = (spacing / 1e-6) as i64;
    if area.is_empty() || !spacing.is_finite() || sp <= 0 {
        return (area.to_vec(), Vec::new());
    }
    let shift = |shapes: Vec<Shape>, by: DVec2| -> Vec<Shape> {
        shapes.into_iter().map(|sh| sh.into_iter().map(|c| c.into_iter().map(|q| q + by).collect()).collect()).collect()
    };
    let moved = shift(area.to_vec(), -origin);
    let lines = matches!(kind, Kind::Rectilinear { .. } | Kind::Monotonic | Kind::MonotonicLine);
    let (normal, narrow) = if lines {
        // `AlignedRectilinear` keeps one direction.
        let odd = odd && !matches!(kind, Kind::Rectilinear { alternate: false });
        split_lines(&moved, sp, angle, odd)
    } else {
        split_core(&moved, sp)
    };
    if narrow.is_empty() {
        return (area.to_vec(), Vec::new());
    }
    (shift(normal, origin), shift(narrow, origin))
}

/// Patterns other than lines: the parts an opening by a spacing keeps fill as set.
fn split_core(area: &[Shape], sp: i64) -> (Vec<Shape>, Vec<Shape>) {
    let (mut normal, mut narrow) = (Vec::new(), Vec::new());
    for shape in area {
        let filled = geom::paths_of(shape);
        let opened = geom::offset_polygons(&geom::shrink_polygons(&filled, sp as f32), sp as f32);
        let inner = geom::intersect_paths(&filled, &opened);
        if inner.is_empty() {
            narrow.push(shape.clone());
            continue;
        }
        let inner = geom::union_paths(&inner);
        let one = std::slice::from_ref(shape);
        narrow.extend(geom::boolean(one, &inner, geom::Op::Difference));
        normal.extend(geom::boolean(one, &inner, geom::Op::Intersect));
    }
    (normal, narrow)
}

/// Line patterns: Orca's sections of upright lines, filtered and traced round.
fn split_lines(area: &[Shape], sp: i64, angle: f64, odd: bool) -> (Vec<Shape>, Vec<Shape>) {
    // `params.angle + float(M_PI / 2.)` adds in floats; the odd layers' quarter in doubles.
    let quarter = FRAC_PI_2 as f32;
    let mut base = f64::from(angle as f32 + quarter);
    if odd {
        base += f64::from(quarter);
    }
    let aligning = -base + PI;
    let mut normal_paths: Paths = Vec::new();
    for shape in area {
        let mut filled = geom::paths_of(shape);
        filled.iter_mut().for_each(|p| crate::rectilinear::rotate(p, aligning));
        let Some((lo, hi)) = path_bounds(&filled) else { continue };
        let opened = geom::offset_polygons(&geom::shrink_polygons(&filled, (2 * sp) as f32), (3 * sp) as f32);
        let inner = geom::intersect_paths(&filled, &opened);
        let inner = geom::shrink_polygons(&inner, (sp as f64 * 0.5) as f32);
        let walls: Vec<(Pt, Pt)> = inner
            .iter()
            .flat_map(|c| (0..c.len()).filter_map(|i| Some((*c.get(i)?, *c.get((i + 1) % c.len())?))))
            .collect();
        let rows = ((hi.x - lo.x + sp - 1) / sp).clamp(0, 100_000) as usize;
        let mut sections: Vec<Vec<Section>> = Vec::with_capacity(rows);
        for row in 0..rows {
            let x = (lo.x as f64 + row as f64 * sp as f64) as i64;
            let (a, b) = (Pt::new(x, lo.y), Pt::new(x, hi.y));
            let mut hits: Vec<(f64, Pt)> = walls
                .iter()
                .filter_map(|&(c, d)| line_intersection(a, b, c, d))
                .map(|p| {
                    let (dx, dy) = ((p.x - a.x) as f64, (p.y - a.y) as f64);
                    (dx * dx + dy * dy, p)
                })
                .collect();
            hits.sort_by(|p, q| p.0.total_cmp(&q.0));
            let mut section = Vec::new();
            for pair in hits.windows(2) {
                let [(_, p), (_, q)] = pair else { continue };
                let mid = Pt::new((p.x + q.x) / 2, (p.y + q.y) / 2);
                if outside(&walls, mid) < 0 && (p.y - q.y).abs() > sp {
                    section.push(Section { x: p.x, low: p.y, high: q.y });
                }
            }
            sections.push(section);
        }
        let kept = filter_vibrating_lines(&sections);
        for mut traced in trace_sections(&kept, sp) {
            crate::rectilinear::rotate(&mut traced, -aligning);
            normal_paths.push(traced);
        }
    }
    // `union_safety_offset_ex`.
    let mut normal = geom::expand_polygons(&normal_paths, 10.0);
    let narrow = geom::boolean(area, &normal, geom::Op::Difference);
    // Bits under a line wide that touch the normal infill join it.
    let mut kept = Vec::new();
    for piece in narrow {
        let one = std::slice::from_ref(&piece);
        if geom::offset_nm(one, (-sp as f64 * 0.5) as f32, Join::Miter).is_empty() {
            let grown = geom::offset_nm(one, (sp as f64 * 0.3) as f32, Join::Miter);
            if !geom::boolean(&normal, &grown, geom::Op::Intersect).is_empty() {
                normal.push(piece);
                continue;
            }
        }
        kept.push(piece);
    }
    if kept.is_empty() {
        return (Vec::new(), Vec::new());
    }
    let normal =
        geom::boolean(&geom::offset_nm(&normal, (sp as f64 * 0.5) as f32, Join::Miter), area, geom::Op::Intersect);
    (normal, kept)
}

/// `get_extents` of polygons.
fn path_bounds(paths: &[Path]) -> Option<(Pt, Pt)> {
    let mut it = paths.iter().flatten();
    let first = *it.next()?;
    Some(it.fold((first, first), |(lo, hi), p| {
        (Pt::new(lo.x.min(p.x), lo.y.min(p.y)), Pt::new(hi.x.max(p.x), hi.y.max(p.y)))
    }))
}

/// `line_alg::intersection` of `a`–`b` with `c`–`d`, the point truncated to integers.
fn line_intersection(a: Pt, b: Pt, c: Pt, d: Pt) -> Option<Pt> {
    let v1 = ((b.x - a.x) as f64, (b.y - a.y) as f64);
    let v2 = ((d.x - c.x) as f64, (d.y - c.y) as f64);
    let denom = v1.0 * v2.1 - v1.1 * v2.0;
    if denom.abs() < 1e-4 {
        return None;
    }
    let v12 = ((a.x - c.x) as f64, (a.y - c.y) as f64);
    let t1 = (v2.0 * v12.1 - v2.1 * v12.0) / denom;
    let t2 = (v1.0 * v12.1 - v1.1 * v12.0) / denom;
    if (0.0..=1.0).contains(&t1) && (0.0..=1.0).contains(&t2) {
        Some(Pt::new((a.x as f64 + t1 * v1.0) as i64, (a.y as f64 + t1 * v1.1) as i64))
    } else {
        None
    }
}

/// `coordinate_aligned_ray_hit_count`: walls passed on each side of `p` along x (or y when
/// `along_y`); `None` when `p` lies on one.
fn ray_hits(walls: &[(Pt, Pt)], p: Pt, along_y: bool) -> Option<(i32, i32)> {
    let (mut above, mut below) = (0, 0);
    for &(a, b) in walls {
        let ((pc, ac, bc), (po, ao, bo)) =
            if along_y { ((p.y, a.y, b.y), (p.x, a.x, b.x)) } else { ((p.x, a.x, b.x), (p.y, a.y, b.y)) };
        if po < ao.min(bo) || po >= ao.max(bo) {
            continue;
        }
        if pc > ac.max(bc) {
            above += 1;
        } else if pc < ac.min(bc) {
            below += 1;
        } else {
            let t = (po - ao) as f64 / (bo - ao) as f64;
            let val = ac as f64 + t * (bc - ac) as f64;
            let pc = pc as f64;
            if pc > val {
                above += 1;
            } else if pc < val {
                below += 1;
            } else {
                return None;
            }
        }
    }
    Some((above, below))
}

/// `point_outside_closed_contours`: -1 inside, 1 outside, 0 on a wall or unclear.
fn outside(walls: &[(Pt, Pt)], p: Pt) -> i32 {
    if walls.is_empty() {
        return 1;
    }
    let parity = |hits: Option<(i32, i32)>| match hits {
        None => Some(0),
        Some((a, b)) if a % 2 == 1 && b % 2 == 1 => Some(-1),
        Some((a, b)) if a % 2 == 0 && b % 2 == 0 => Some(1),
        _ => None,
    };
    parity(ray_hits(walls, p, false)).or_else(|| parity(ray_hits(walls, p, true))).unwrap_or(0)
}

/// One line of a solid fill in the upright frame: at `x`, from `low` to `high`.
#[derive(Clone, Copy, Debug)]
struct Section {
    x: i64,
    low: i64,
    high: i64,
}

impl Section {
    fn length(&self) -> f64 {
        (self.high - self.low).abs() as f64
    }
}

#[derive(Clone, Default)]
struct LineState {
    min_skips_taken: i32,
    total_short_lines: i32,
    initial_touches_long: bool,
    initialized: bool,
}

struct LineNode {
    section: Section,
    previous: Vec<(usize, usize)>,
    next: Vec<(usize, usize)>,
    removed: bool,
    state: LineState,
}

/// `_MAX_LINE_LENGTH_TO_FILTER`: `scaled<coord_t>(4.)`.
fn short_line() -> f64 {
    ((4.0 / 1e-6) as i64) as f64
}
const MAX_SKIPS: i32 = 2;
const MIN_DEPTH: i32 = 5;

fn node(nodes: &[Vec<LineNode>], (s, i): (usize, usize)) -> Option<&LineNode> {
    nodes.get(s).and_then(|l| l.get(i))
}

fn removable(nodes: &[Vec<LineNode>], at: (usize, usize)) -> bool {
    let Some(n) = node(nodes, at) else { return false };
    let initial_touching = n.previous.iter().any(|p| node(nodes, *p).is_some_and(|q| q.state.initial_touches_long));
    let has_next = n.next.iter().any(|p| node(nodes, *p).is_some_and(|q| !q.removed));
    n.section.length() < short_line() && (n.state.total_short_lines > MIN_DEPTH || (!initial_touching && !has_next))
}

/// Filters out runs of short lines, which shake the printer (from OrcaSlicer's
/// `filter_vibrating_extrusions`, keeping its rules exactly).
fn filter_vibrating_lines(sections: &[Vec<Section>]) -> Vec<Vec<Section>> {
    let overlapping = |a: &Section, b: &Section| {
        (b.low <= a.low && a.low <= b.high)
            || (b.low <= a.high && a.high <= b.high)
            || (a.low <= b.low && b.low <= a.high)
            || (a.low <= b.high && b.high <= a.high)
    };
    let beside = |s: usize, line: &Section, other: usize| -> Vec<(usize, usize)> {
        sections.get(other).map_or(Vec::new(), |list| {
            list.iter()
                .enumerate()
                .filter(|(_, o)| s != other && overlapping(line, o))
                .map(|(k, _)| (other, k))
                .collect()
        })
    };
    let mut nodes: Vec<Vec<LineNode>> = sections
        .iter()
        .enumerate()
        .map(|(s, list)| {
            list.iter()
                .map(|line| LineNode {
                    section: *line,
                    previous: s.checked_sub(1).map_or(Vec::new(), |p| beside(s, line, p)),
                    next: beside(s, line, s + 1),
                    removed: false,
                    state: LineState::default(),
                })
                .collect()
        })
        .collect();
    let count = nodes.len();
    for initial in 0..count {
        for i in 0..nodes.get(initial).map_or(0, Vec::len) {
            let Some(n) = node(&nodes, (initial, i)) else { continue };
            if n.removed || n.section.length() >= short_line() {
                continue;
            }
            let touches = n
                .previous
                .iter()
                .any(|p| node(&nodes, *p).is_some_and(|q| !q.removed && q.section.length() >= short_line()));
            if let Some(n) = nodes.get_mut(initial).and_then(|l| l.get_mut(i)) {
                n.state = LineState {
                    min_skips_taken: 0,
                    total_short_lines: 1,
                    initial_touches_long: touches,
                    initialized: true,
                };
            }
        }
        for propagate in initial..count {
            for n in nodes.get_mut(propagate + 1).into_iter().flatten() {
                n.state = LineState::default();
            }
            for i in 0..nodes.get(propagate).map_or(0, Vec::len) {
                let at = (propagate, i);
                let Some(n) = node(&nodes, at) else { continue };
                if n.removed || !n.state.initialized {
                    continue;
                }
                let (state, next) = (n.state.clone(), n.next.clone());
                for neighbour in next {
                    let Some(m) = node(&nodes, neighbour) else { continue };
                    if m.removed {
                        continue;
                    }
                    let short = m.section.length() < short_line();
                    let skip_allowed = state.min_skips_taken < MAX_SKIPS;
                    if !short && !skip_allowed {
                        continue;
                    }
                    let total = state.total_short_lines + i32::from(short);
                    let skips = state.min_skips_taken + i32::from(!short);
                    if let Some(m) = nodes.get_mut(neighbour.0).and_then(|l| l.get_mut(neighbour.1)) {
                        if m.state.initialized {
                            m.state.min_skips_taken = m.state.min_skips_taken.max(total).min(skips);
                            if m.state.initial_touches_long {
                                m.state.initial_touches_long = state.initial_touches_long;
                            }
                        } else {
                            m.state = LineState {
                                min_skips_taken: skips,
                                total_short_lines: total,
                                initial_touches_long: state.initial_touches_long,
                                initialized: true,
                            };
                        }
                    }
                }
                if removable(&nodes, at) {
                    // Its removal may free the lines before it too.
                    let mut queue: std::collections::VecDeque<(usize, usize)> = std::collections::VecDeque::new();
                    if let Some(n) = nodes.get_mut(at.0).and_then(|l| l.get_mut(at.1)) {
                        n.removed = true;
                        queue.extend(n.previous.iter().copied());
                    }
                    while let Some(check) = queue.pop_front() {
                        let alive = node(&nodes, check).is_some_and(|n| !n.removed);
                        if alive
                            && removable(&nodes, check)
                            && let Some(n) = nodes.get_mut(check.0).and_then(|l| l.get_mut(check.1))
                        {
                            n.removed = true;
                            queue.extend(n.previous.iter().copied());
                        }
                    }
                }
            }
        }
    }
    nodes.into_iter().map(|l| l.into_iter().filter(|n| !n.removed).map(|n| n.section).collect()).collect()
}

/// Polygons round runs of neighbouring lines, each widened half a spacing at both ends.
fn trace_sections(sections: &[Vec<Section>], sp: i64) -> Vec<Path> {
    struct Traced {
        lows: Vec<Pt>,
        highs: Vec<Pt>,
    }
    let limit = 2.0 * sp as f64;
    let limit2 = limit * limit;
    let widen = (0.5 * sp as f64).round() as i64;
    let half = sp / 2;
    let near = |p: Pt, q: Pt| {
        let (dx, dy) = ((p.x - q.x) as f64, (p.y - q.y) as f64);
        dx * dx + dy * dy < limit2
    };
    let left = |p: Pt| Pt::new(p.x - half, p.y);
    let right = |p: Pt| Pt::new(p.x + half, p.y);
    let close = |t: Traced, out: &mut Vec<Path>| {
        let mut c = t.lows;
        c.extend(t.highs.into_iter().rev());
        out.push(c);
    };
    let mut out: Vec<Path> = Vec::new();
    let mut open: Vec<Traced> = Vec::new();
    for section in sections {
        let lines: Vec<(Pt, Pt)> =
            section.iter().map(|l| (Pt::new(l.x, l.low - widen), Pt::new(l.x, l.high + widen))).collect();
        let mut used = vec![false; lines.len()];
        let mut still: Vec<Traced> = Vec::with_capacity(open.len());
        for mut t in open.drain(..) {
            let (Some(&low), Some(&high)) = (t.lows.last(), t.highs.last()) else { continue };
            let first = lines.partition_point(|l| l.1.y <= low.y);
            let end = lines.partition_point(|l| l.0.y <= high.y);
            // As in Orca, a free line in reach continues the polygon with the first in reach.
            let free = (first..end).any(|k| !used.get(k).copied().unwrap_or(true));
            match lines.get(first).filter(|_| free) {
                Some(&(a, b)) => {
                    if near(low, a) {
                        t.lows.push(a);
                    } else {
                        t.lows.extend([right(low), left(a), a]);
                    }
                    if near(high, b) {
                        t.highs.push(b);
                    } else {
                        t.highs.extend([right(high), left(b), b]);
                    }
                    if let Some(u) = used.get_mut(first) {
                        *u = true;
                    }
                    still.push(t);
                }
                None => {
                    t.lows.push(right(low));
                    t.highs.push(right(high));
                    close(t, &mut out);
                }
            }
        }
        open = still;
        for (line, was_used) in lines.iter().zip(&used) {
            if !*was_used {
                open.push(Traced { lows: vec![left(line.0), line.0], highs: vec![left(line.1), line.1] });
            }
        }
    }
    for t in open {
        close(t, &mut out);
    }
    out
}

/// Pieces of lines along `a`, joined zig-zag within each region.
fn join_rows(
    pieces: Vec<Polyline>,
    a: f64,
    spacing: f64,
    phase: f64,
    area: &[Shape],
    max_step: f64,
    link: f64,
) -> Vec<Line> {
    let around = Around::new(area, max_step, link);
    regions(pieces, a, spacing, phase).into_iter().flat_map(|r| join_zigzag(r, &around, max_step, link)).collect()
}

/// Lines `spacing` apart at angle `a`, clipped and joined zig-zag where they meet.
fn parallel(area: &[Shape], spacing: f64, a: f64, phase: f64, width: f64, link: f64) -> Vec<Line> {
    join_rows(rows(area, spacing, a, phase, width), a, spacing, phase, area, spacing * 1.9, link)
}

/// Parallel lines in several directions, each `(angle, phase)`.
fn directions(area: &[Shape], width: f64, spacing: f64, dirs: &[(f64, f64)]) -> Vec<Line> {
    dirs.iter().flat_map(|&(a, phase)| parallel(area, spacing, a, phase.rem_euclid(spacing), width, 0.0)).collect()
}

/// Cubic: `dirs` directions over 180°, each sliding with height so the lines build tilted
/// planes. `graded` = (spacing factor, band in steps): coarser lines deeper than the band
/// from the edge, on a grid that keeps every other (or fourth) line of the fine one.
fn cubic(area: &[Shape], p: &Params, s: f64, dirs: usize, graded: Option<(f64, f64)>) -> Vec<Line> {
    let spacing = s * dirs as f64;
    let mut out = Vec::new();
    let (near, far, coarse) = match graded {
        Some((factor, band)) => {
            let far = geom::offset(area, -band * spacing, Join::Miter);
            let near = geom::boolean(area, &far, geom::Op::Difference);
            (near, far, factor.max(1.0))
        }
        None => (area.to_vec(), Vec::new(), 1.0),
    };
    for k in 0..dirs {
        let a = p.angle + PI * k as f64 / dirs as f64;
        let sign = if k % 2 == 0 { 1.0 } else { -1.0 };
        let slide = p.z * sign * std::f64::consts::FRAC_1_SQRT_2;
        let phase = slide.rem_euclid(spacing * coarse);
        out.extend(parallel(&near, spacing, a, phase.rem_euclid(spacing), p.width, 0.0));
        if !far.is_empty() {
            out.extend(parallel(&far, spacing * coarse, a, phase, p.width, 0.0));
        }
    }
    out
}

/// Rows of zig-zag waves along `a`: rows `spacing` apart, a period of `spacing`, a quarter
/// period high, shifted by `shift` along the rows.
fn waves(area: &[Shape], a: f64, spacing: f64, shift: f64, mirror: bool) -> Vec<Line> {
    wave_rows(area, a, spacing, spacing, spacing / 4.0, shift, mirror)
}

fn wave_rows(area: &[Shape], a: f64, spacing: f64, period: f64, amp: f64, shift: f64, mirror: bool) -> Vec<Line> {
    if !(spacing > 1e-3 && period > 1e-3) {
        return Vec::new();
    }
    let lines = framed(area, a, |lo, hi| {
        let (m0, m1) = ((lo.y / spacing).floor() as i64 - 1, (hi.y / spacing).ceil() as i64 + 1);
        let half = period / 2.0;
        let (k0, k1) = (((lo.x - shift) / half).floor() as i64 - 1, ((hi.x - shift) / half).ceil() as i64 + 1);
        if ((m1 - m0) as f64) * ((k1 - k0) as f64) > MAX_POINTS as f64 {
            return Vec::new();
        }
        (m0..=m1)
            .map(|m| {
                (k0..=k1)
                    .map(|k| {
                        let up = (k + if mirror { m } else { 0 }).rem_euclid(2) == 0;
                        DVec2::new(k as f64 * half + shift, m as f64 * spacing + if up { amp } else { -amp })
                    })
                    .collect()
            })
            .collect()
    });
    join_rows(lines, a, spacing, 0.0, area, spacing * 1.9, 0.0)
}

/// Cross zag: zig-zag waves along the infill direction, shifted along by `shift_step` each
/// layer, so each layer's lines cross the ones below.
fn cross_zag(area: &[Shape], a: f64, w: f64, d: f64, p: &Params) -> Vec<Line> {
    let spacing = w / d * std::f64::consts::SQRT_2;
    let shift = (p.layer as f64 * p.shift_step).rem_euclid(spacing);
    waves(area, a, spacing, shift, false)
}

/// Cross hatch, as OrcaSlicer (Bambu Lab) draws it: straight lines one way for a stretch of
/// height, then a few transition layers of waves that turn them, then straight lines the
/// other way. At low density the straight stretches are shorter and the lines a little
/// further apart. The grid is fixed four lines at a time from the object's origin.
fn cross_hatch(area: &[Shape], p: &Params, s: f64) -> Vec<Line> {
    let density = p.density.min(1.0);
    area.iter().flat_map(|shape| cross_hatch_one(shape, p, s * density, density)).collect()
}

/// `FillCrossHatch::_fill_surface_single`, in Orca's integers: the region turned into the
/// pattern's frame, the pattern over its bounds, clipped by Clipper (whose order the connecting
/// sees) and turned back.
fn cross_hatch_one(shape: &Shape, p: &Params, spacing: f64, density: f64) -> Vec<Line> {
    let rotate = |q: Pt, angle: f64| {
        let (sin, cos) = angle.sin_cos();
        let (x, y) = (q.x as f64, q.y as f64);
        Pt::new((cos * x - sin * y).round() as i64, (cos * y + sin * x).round() as i64)
    };
    let angle = f64::from(p.angle as f32);
    let turning = angle.abs() >= 1.0e-4;
    let turned: Paths = geom::paths_of(shape)
        .into_iter()
        .map(|c| if turning { c.into_iter().map(|q| rotate(q, -angle)).collect() } else { c })
        .collect();
    let Some(contour) = turned.first() else { return Vec::new() };
    let lo = Pt::new(contour.iter().map(|q| q.x).min().unwrap_or(0), contour.iter().map(|q| q.y).min().unwrap_or(0));
    let hi = Pt::new(contour.iter().map(|q| q.x).max().unwrap_or(0), contour.iter().map(|q| q.y).max().unwrap_or(0));

    let thin = density as f32;
    let spacing = f64::from(spacing as f32);
    let mut grid = (spacing / 1.0e-6 / f64::from(thin)) as i64;
    if thin < 0.999 {
        grid = (grid as f64 * 1.08) as i64;
    }
    if grid <= 0 {
        return Vec::new();
    }
    let module = grid * 4;
    let lo = Pt::new(lo.x.min(lo.x.div_euclid(module) * module), lo.y.min(lo.y.div_euclid(module) * module));
    let size = (hi.x - lo.x, hi.y - lo.y);
    // A pattern of a hundred thousand rows is no pattern.
    if size.0 / grid > 100_000 || size.1 / grid > 100_000 {
        return Vec::new();
    }

    let repeat_ratio = if thin < 0.3 { (1.0 - f64::from((-5.0 * thin).exp())).clamp(0.2, 1.0) } else { 1.0 };
    let mut lines = hatch_layers(p.z / 1.0e-6, repeat_ratio, grid as f64, size.0 as f64, size.1 as f64);
    for line in &mut lines {
        for q in line.iter_mut() {
            *q = Pt::new(q.x + lo.x, q.y + lo.y);
        }
    }
    let least = 0.8 * spacing / 1.0e-6;
    let nm_length = |l: &Path| {
        l.windows(2)
            .map(|w| match w {
                [a, b] => ((b.x - a.x) as f64).hypot((b.y - a.y) as f64),
                _ => 0.0,
            })
            .sum::<f64>()
    };
    let lines: Paths = geom::clip_open(&lines, &turned, ClipType::Intersection)
        .into_iter()
        .filter(|l| nm_length(l) >= least)
        .collect();

    let back = |l: &Polyline| -> Polyline {
        if turning {
            geom::from_nm(&geom::to_nm(l).into_iter().map(|q| rotate(q, angle)).collect::<Path>())
        } else {
            l.clone()
        }
    };
    let lines: Vec<Polyline> = lines.iter().map(|l| geom::from_nm(l)).collect();
    if p.anchor_max >= 0.05 {
        let boundary: Vec<Contour> = turned.iter().map(|c| geom::from_nm(c)).collect();
        return crate::connect::connect_infill(lines, &boundary, spacing, p.anchor, p.anchor_max)
            .iter()
            .map(|l| Line::open(back(l)))
            .collect();
    }
    lines.iter().map(|l| Line::open(back(l))).collect()
}

/// `generate_infill_layers`: straight lines, or the wave that turns them through 90 degrees.
fn hatch_layers(z: f64, repeat_ratio: f64, grid: f64, width: f64, height: f64) -> Paths {
    let trans = grid * 0.4;
    let repeat = grid * repeat_ratio;
    let z = z + repeat / 2.0 + trans;
    let period = trans + repeat;
    let trans_z = z - (z / period).floor() * period - repeat;
    let phase = (z % (period * 2.0) - (period - 1.0)) as i32;
    let direction = if phase <= 0 { -1 } else { 1 };
    if trans_z < 0.0 {
        return hatch_repeat(direction, grid, width, height);
    }
    let progress = (trans_z % trans) / trans;
    if progress < 0.5 {
        hatch_transform((progress + 0.1) * 2.0, direction, grid, width, height)
    } else {
        hatch_transform((1.1 - progress) * 2.0, -direction, grid, width, height)
    }
}

fn nm(x: f64) -> i64 {
    x.round() as i64
}

/// `generate_repeat_pattern`.
fn hatch_repeat(direction: i32, grid: f64, width: f64, height: f64) -> Paths {
    let (w, h) = if direction < 0 { (height, width) } else { (width, height) };
    let n = (h / grid) as i64 + 1;
    (0..n)
        .map(|i| {
            let y = nm(grid * i as f64);
            let line = vec![Pt::new(0, y), Pt::new(nm(w), y)];
            if direction < 0 { line.into_iter().map(|q| Pt::new(q.y, q.x)).collect() } else { line }
        })
        .collect()
}

/// `generate_transform_pattern`: rows of a four-point wave whose bends grow with `progress`,
/// odd and even rows half a cycle apart.
fn hatch_transform(progress: f64, direction: i32, grid: f64, width: f64, height: f64) -> Paths {
    let grid = grid * 2.0;
    let (w, h) = if direction < 0 { (height, width) } else { (width, height) };
    let off = progress * 1.0 / 8.0 * grid;
    let cycle = [
        Pt::new(nm(0.25 * grid - off), nm(off)),
        Pt::new(nm(0.25 * grid + off), nm(off)),
        Pt::new(nm(0.75 * grid - off), nm(-off)),
        Pt::new(nm(0.75 * grid + off), nm(-off)),
    ];
    let cycles = (w / grid + 2.0) as i64;
    let mut odd: Path = Vec::with_capacity(cycles.max(0) as usize * 4);
    for i in 0..cycles {
        let shift = nm(i as f64 * grid);
        odd.extend(cycle.iter().map(|q| Pt::new(q.x + shift, q.y)));
    }
    let rows = (h / grid + 2.0) as i64;
    let mut out: Paths = Vec::with_capacity(rows.max(0) as usize * 2);
    for i in 0..rows {
        let shift = nm(grid * i as f64);
        out.push(odd.iter().map(|q| Pt::new(q.x, q.y + shift)).collect());
    }
    for i in 0..rows {
        let (sx, sy) = (nm(-0.5 * grid), nm((i as f64 + 0.5) * grid));
        out.push(odd.iter().map(|q| Pt::new(q.x + sx, q.y + sy)).collect());
    }
    if direction < 0 {
        for line in &mut out {
            for q in line.iter_mut() {
                *q = Pt::new(q.y, q.x);
            }
        }
    }
    out
}

/// Honeycomb as OrcaSlicer (Slic3r) draws it: columns of zig-zags up one side of a row of
/// hexagons and down the other, the shared walls laid side by side; turned 60° a layer about
/// a hexagon centre so the walls stay over each other.
fn honeycomb(area: &[Shape], p: &Params) -> Vec<Line> {
    let side = 8.0 * p.width / (3.0 * 3f64.sqrt() * p.density.min(1.0));
    let wid = 3f64.sqrt() * side;
    let o = (p.width / 2.0).min(wid / 8.0);
    let turns = if p.turn_by_layer { (p.layer % 3) as f64 } else { 0.0 };
    let a = p.angle + turns * FRAC_PI_3;
    let lines = framed(area, a, |lo, hi| {
        let (j0, j1) = ((lo.y / (1.5 * side)).floor() as i64 - 1, (hi.y / (1.5 * side)).ceil() as i64 + 1);
        let (k0, k1) = ((lo.x / wid).floor() as i64 - 1, (hi.x / wid).ceil() as i64 + 1);
        if ((j1 - j0) as f64) * ((k1 - k0) as f64) * 4.0 > MAX_POINTS as f64 {
            return Vec::new();
        }
        (k0..=k1)
            .map(|k| {
                let kw = k as f64 * wid;
                let mut col: Polyline = Vec::new();
                for j in j0..=j1 {
                    let cy = 1.5 * side * j as f64;
                    let x = if j.rem_euclid(2) == 0 { kw + wid / 2.0 + o } else { kw + wid - o };
                    col.push(DVec2::new(x, cy - side / 2.0));
                    col.push(DVec2::new(x, cy + side / 2.0));
                }
                for j in (j0..=j1).rev() {
                    let cy = 1.5 * side * j as f64;
                    let x = if j.rem_euclid(2) == 0 { kw + wid / 2.0 - o } else { kw + o };
                    col.push(DVec2::new(x, cy + side / 2.0));
                    col.push(DVec2::new(x, cy - side / 2.0));
                }
                col
            })
            .collect()
    });
    lines.into_iter().filter(|l| geom::length(l) > p.width).map(Line::open).collect()
}

/// 3D honeycomb (truncated octahedra): rows of zig-zags whose height grows and shrinks with z
/// until neighbouring rows touch, swapping direction every cell.
fn honeycomb_3d(area: &[Shape], p: &Params) -> Vec<Line> {
    let cell = 1.15 * p.width / p.density.min(1.0);
    let t = p.z / cell;
    let frac = t - t.floor();
    let amp = cell / 2.0 * (1.0 - (2.0 * frac - 1.0).abs());
    let a = p.angle + if (t.floor() as i64).rem_euclid(2) == 1 { FRAC_PI_2 } else { 0.0 };
    wave_rows(area, a, cell, cell * 2.0, amp, 0.0, true)
}

/// Lateral honeycomb: hexagonal prisms lying along the infill direction; each layer cuts
/// their walls into lines along it, which move as the walls slope.
fn lateral_honeycomb(area: &[Shape], p: &Params) -> Vec<Line> {
    let wid = 2.0 * p.width / p.density.min(1.0);
    let side = wid / 3f64.sqrt();
    let j = (p.z / (1.5 * side)).round();
    let t = p.z - 1.5 * side * j;
    let row_shift = if (j as i64).rem_euclid(2) == 1 { wid / 2.0 } else { 0.0 };
    // Where walls cross this height, about each hexagon centre of the row.
    // Upright walls are laid twice side by side, as honeycomb's are, so every height has
    // about two lines a cell.
    let offsets: Vec<f64> = if t.abs() <= side / 2.0 {
        vec![wid / 2.0 - p.width / 2.0, wid / 2.0 + p.width / 2.0]
    } else {
        let f = ((t.abs() - side / 2.0) / (side / 2.0)).clamp(0.0, 1.0);
        let r = wid / 2.0 * (1.0 - f);
        if r < p.width * 0.5 { vec![0.0] } else { vec![-r, r] }
    };
    let lines = framed(area, p.angle, |lo, hi| {
        let (i0, i1) = ((lo.y / wid).floor() as i64 - 1, (hi.y / wid).ceil() as i64 + 1);
        if (i1 - i0) as usize > MAX_POINTS {
            return Vec::new();
        }
        (i0..=i1)
            .flat_map(|i| {
                let cx = i as f64 * wid + row_shift;
                offsets.iter().map(move |o| vec![DVec2::new(lo.x - 1.0, cx + o), DVec2::new(hi.x + 1.0, cx + o)])
            })
            .collect()
    });
    join_rows(lines, p.angle, wid / 2.0, 0.0, area, wid, 0.0)
}

/// Curves of an implicit surface solved along one axis: for each `u` sample, the `v` values
/// (in surface units) where it is zero, as continuous branches repeating every `v_period`.
fn branches(
    lo: f64,
    hi: f64,
    v_lo: f64,
    v_hi: f64,
    v_period: f64,
    steps_per_period: f64,
    solve: impl Fn(f64) -> Vec<f64>,
) -> Vec<Vec<DVec2>> {
    let du = TAU / steps_per_period;
    let n_u = ((hi - lo) / du).ceil() as usize + 3;
    let m0 = (v_lo / v_period).floor() as i64 - 2;
    let m1 = (v_hi / v_period).ceil() as i64 + 2;
    let samples: Vec<(f64, Vec<f64>)> = (0..n_u).map(|i| lo - du + i as f64 * du).map(|u| (u, solve(u))).collect();
    let n_b = samples.first().map_or(0, |s| s.1.len());
    let mut out = Vec::new();
    for b in 0..n_b {
        // One branch, unwrapped so it does not jump a whole period between samples.
        let mut branch: Vec<(f64, f64)> = Vec::with_capacity(n_u);
        let mut prev: Option<f64> = None;
        for (u, vs) in &samples {
            let Some(&raw) = vs.get(b) else { continue };
            let v = match prev {
                Some(pv) => raw + v_period * ((pv - raw) / v_period).round(),
                None => raw,
            };
            prev = Some(v);
            branch.push((*u, v));
        }
        let first = branch.first().map_or(0.0, |x| x.1);
        for m in m0..=m1 {
            // Copies spread from the branch's start so they cover the bounds.
            let shift = m as f64 * v_period - (first / v_period).floor() * v_period;
            out.push(branch.iter().map(|&(u, v)| DVec2::new(u, v + shift)).collect());
        }
    }
    out
}

/// Fills with zero-set curves solved per layer, scaled from surface units (period 2π) to
/// `period` mm, then simplified and ordered.
fn surface(
    area: &[Shape],
    p: &Params,
    period: f64,
    make: impl FnOnce(DVec2, DVec2, f64) -> Vec<Vec<DVec2>>,
) -> Vec<Line> {
    let k = TAU / period;
    let z = p.z * k;
    let lines = framed(area, p.angle, |lo, hi| {
        let cells = ((hi - lo).x * k / TAU + 4.0) * ((hi - lo).y * k / TAU + 4.0);
        if cells * 100.0 > MAX_POINTS as f64 {
            return Vec::new();
        }
        make(lo * k, hi * k, z)
            .into_iter()
            .map(|c| geom::simplify(&c.into_iter().map(|q| q / k).collect::<Vec<_>>(), 0.02))
            .collect()
    });
    let mut pieces: Vec<Polyline> = lines.into_iter().filter(|l| geom::length(l) > p.width).collect();
    pieces.sort_by(|a, b| {
        let (pa, pb) = (a.first().copied().unwrap_or_default(), b.first().copied().unwrap_or_default());
        (pa.y, pa.x).partial_cmp(&(pb.y, pb.x)).unwrap_or(std::cmp::Ordering::Equal)
    });
    pieces.into_iter().map(Line::open).collect()
}

/// The gyroid, sin x cos y + sin y cos z + sin z cos x = 0, solved per layer for y along x (or
/// x along y where the cut runs the other way): waves, no grid to search.
fn gyroid(area: &[Shape], p: &Params, s: f64) -> Vec<Line> {
    surface(area, p, 2.41 * s, |lo, hi, z| {
        let (sz, cz) = z.sin_cos();
        if cz.abs() >= sz.abs() {
            // a cos y + b sin y = c with a = sin x, b = cos z, c = -sin z cos x.
            branches(lo.x, hi.x, lo.y, hi.y, TAU, 24.0, |x| {
                let (a, b, c) = (x.sin(), cz, -sz * x.cos());
                let r = a.hypot(b).max(1e-12);
                let (phi, delta) = (b.atan2(a), (c / r).clamp(-1.0, 1.0).acos());
                vec![phi + delta, phi - delta]
            })
        } else {
            // a cos x + b sin x = c with a = sin z, b = cos y, c = -sin y cos z.
            branches(lo.y, hi.y, lo.x, hi.x, TAU, 24.0, |y| {
                let (a, b, c) = (sz, y.cos(), -y.sin() * cz);
                let r = a.hypot(b).max(1e-12);
                let (phi, delta) = (b.atan2(a), (c / r).clamp(-1.0, 1.0).acos());
                vec![phi + delta, phi - delta]
            })
            .into_iter()
            .map(|c| c.into_iter().map(|q| DVec2::new(q.y, q.x)).collect())
            .collect()
        }
    })
}

/// Schwarz Diamond: sin x cos(y − z) + cos x sin(y + z) = 0, traced on a grid (its curves
/// cross where both terms vanish, which per-layer solving cannot follow).
fn tpms_d(area: &[Shape], p: &Params, s: f64) -> Vec<Line> {
    let period = 3.2 * s;
    let k = TAU / period;
    let z = p.z * k;
    let field = move |q: DVec2| {
        let (x, y) = (q.x * k, q.y * k);
        x.sin() * (y - z).cos() + x.cos() * (y + z).sin()
    };
    traced(area, p, period, field)
}

/// Fischer–Koch S, traced on a grid (it has no closed form per layer).
fn tpms_fk(area: &[Shape], p: &Params, s: f64) -> Vec<Line> {
    let period = 4.1 * s;
    let k = TAU / period;
    let z = p.z * k;
    let (c2z, sz, cz) = ((2.0 * z).cos(), z.sin(), z.cos());
    let field = move |q: DVec2| {
        let (x, y) = (q.x * k, q.y * k);
        (2.0 * x).cos() * y.sin() * cz + x.cos() * (2.0 * y).cos() * sz + x.sin() * y.cos() * c2z
    };
    traced(area, p, period, field)
}

fn traced(area: &[Shape], p: &Params, period: f64, field: impl Fn(DVec2) -> f64) -> Vec<Line> {
    let cell = (period / 12.0).clamp(p.width * 0.5, p.width * 3.0);
    let lines = framed(area, p.angle, |lo, hi| iso_lines(lo, hi, cell, field));
    lines.into_iter().filter(|l| geom::length(l) > p.width).map(Line::open).collect()
}

/// A marching-squares grid edge: (i, j, 0) runs along x from node (i, j), (i, j, 1) along y.
type GridEdge = (usize, usize, u8);

/// Where `f` is zero over the bounds, traced on a grid of `cell`.
fn iso_lines(lo: DVec2, hi: DVec2, cell: f64, f: impl Fn(DVec2) -> f64) -> Vec<Polyline> {
    let (nx, ny) = (((hi.x - lo.x) / cell).ceil() as usize + 3, ((hi.y - lo.y) / cell).ceil() as usize + 3);
    if nx.saturating_mul(ny) > MAX_POINTS {
        return Vec::new();
    }
    let at = |i: usize, j: usize| lo + DVec2::new((i as f64 - 1.0) * cell, (j as f64 - 1.0) * cell);
    let values: Vec<f64> = (0..ny).flat_map(|j| (0..nx).map(move |i| (i, j))).map(|(i, j)| f(at(i, j))).collect();
    let v = |i: usize, j: usize| values.get(j * nx + i).copied().unwrap_or(0.0);
    let point = |e: GridEdge| {
        let (i, j, d) = e;
        let (a, b, pa, pb) = if d == 0 {
            (v(i, j), v(i + 1, j), at(i, j), at(i + 1, j))
        } else {
            (v(i, j), v(i, j + 1), at(i, j), at(i, j + 1))
        };
        let t = if (b - a).abs() > 1e-12 { (a / (a - b)).clamp(0.0, 1.0) } else { 0.5 };
        pa + (pb - pa) * t
    };
    let mut links: HashMap<GridEdge, Vec<GridEdge>> = HashMap::new();
    for j in 0..ny - 1 {
        for i in 0..nx - 1 {
            let corners = [v(i, j), v(i + 1, j), v(i + 1, j + 1), v(i, j + 1)];
            let edges = [(i, j, 0u8), (i + 1, j, 1u8), (i, j + 1, 0u8), (i, j, 1u8)];
            let crossed: Vec<GridEdge> =
                (0..4).filter(|&e| (corners[e] >= 0.0) != (corners[(e + 1) % 4] >= 0.0)).map(|e| edges[e]).collect();
            let pairs: &[(usize, usize)] = match crossed.len() {
                2 => &[(0, 1)],
                4 => &[(0, 1), (2, 3)],
                _ => &[],
            };
            for &(a, b) in pairs {
                let (Some(&ea), Some(&eb)) = (crossed.get(a), crossed.get(b)) else { continue };
                links.entry(ea).or_default().push(eb);
                links.entry(eb).or_default().push(ea);
            }
        }
    }
    let mut keys: Vec<GridEdge> = links.keys().copied().collect();
    keys.sort_unstable();
    // Line ends (one link) first, then whatever loops remain.
    keys.sort_by_key(|k| links.get(k).map_or(0, Vec::len));
    let mut lines = Vec::new();
    for start in keys {
        if links.get(&start).is_none_or(Vec::is_empty) {
            continue;
        }
        let mut line = vec![point(start)];
        let mut at_key = start;
        while let Some(next) = links.get_mut(&at_key).and_then(Vec::pop) {
            if let Some(back) = links.get_mut(&next)
                && let Some(pos) = back.iter().position(|e| *e == at_key)
            {
                back.swap_remove(pos);
            }
            line.push(point(next));
            at_key = next;
            if line.len() > MAX_POINTS {
                break;
            }
        }
        if line.len() >= 2 {
            lines.push(geom::simplify(&line, 0.03));
        }
    }
    lines
}

/// Joins each line to the next, flipping every other line, so neighbouring rows print as one
/// zig-zag: along the area's edge between their ends when that is at most `link` long
/// (OrcaSlicer's link along the perimeter), else straight across when the step is under
/// `max_step` and stays inside.
fn join_zigzag(lines: Vec<Polyline>, around: &Around<'_>, max_step: f64, link: f64) -> Vec<Line> {
    let mut out: Vec<Line> = Vec::new();
    let mut current: Option<Polyline> = None;
    for mut l in lines {
        if let Some(cur) = current.as_mut() {
            let end = cur.last().copied().unwrap_or_default();
            // Try the nearer end of the next line.
            let (Some(&s), Some(&e)) = (l.first(), l.last()) else { continue };
            if e.distance(end) < s.distance(end) {
                l.reverse();
            }
            let start = l.first().copied().unwrap_or_default();
            if let Some(path) = around.edges.as_ref().and_then(|ed| ed.along(end, start, link)) {
                cur.extend(path);
                cur.extend(l);
                continue;
            }
            let step = start.distance(end);
            if step < max_step && geom::contains(around.grown(), (start + end) / 2.0) {
                cur.extend(l);
                continue;
            }
            if let Some(done) = current.take() {
                out.push(Line::open(done));
            }
        }
        current = Some(l);
    }
    if let Some(done) = current {
        out.push(Line::open(done));
    }
    out
}

/// What joining lines needs of their area, made once per fill: its edges, and the area grown a
/// little (line ends lie on the boundary, so a straight step between them runs along it), made
/// only if a straight step is tried.
struct Around<'a> {
    area: &'a [Shape],
    grow: f64,
    grown: std::cell::OnceCell<Vec<Shape>>,
    edges: Option<Edges<'a>>,
}

impl<'a> Around<'a> {
    fn new(area: &'a [Shape], max_step: f64, link: f64) -> Around<'a> {
        Around {
            area,
            grow: max_step * 0.25,
            grown: std::cell::OnceCell::new(),
            edges: (link > 0.0).then(|| Edges::new(area, max_step)),
        }
    }

    fn grown(&self) -> &[Shape] {
        self.grown.get_or_init(|| geom::offset(self.area, self.grow, Join::Miter))
    }
}

/// An area's contours, bucketed on a grid to find which edge a point lies on.
struct Edges<'a> {
    contours: Vec<&'a Vec<DVec2>>,
    cell: f64,
    grid: HashMap<(i64, i64), Vec<(usize, usize)>>,
}

impl<'a> Edges<'a> {
    fn new(area: &'a [Shape], step: f64) -> Edges<'a> {
        let contours: Vec<&Vec<DVec2>> = area.iter().flatten().filter(|c| c.len() >= 3).collect();
        let extent = geom::bounds(area).map_or(1.0, |(lo, hi)| (hi - lo).max_element());
        let cell = step.max(extent / 1024.0).max(1e-3);
        let mut grid: HashMap<(i64, i64), Vec<(usize, usize)>> = HashMap::new();
        for (ci, c) in contours.iter().enumerate() {
            let n = c.len();
            for i in 0..n {
                let (Some(&a), Some(&b)) = (c.get(i), c.get((i + 1) % n)) else { continue };
                // The cells along the segment (and their neighbours catch the rest).
                let steps = ((a.distance(b) / (cell / 2.0)).ceil() as usize).clamp(1, 100_000);
                let mut last = None;
                for k in 0..=steps {
                    let q = a.lerp(b, k as f64 / steps as f64);
                    let key = ((q.x / cell).floor() as i64, (q.y / cell).floor() as i64);
                    if last != Some(key) {
                        grid.entry(key).or_default().push((ci, i));
                        last = Some(key);
                    }
                }
            }
        }
        Edges { contours, cell, grid }
    }

    /// The contour and segment `p` lies on, and how far along the segment.
    fn locate(&self, p: DVec2) -> Option<(usize, usize, f64)> {
        let (cx, cy) = ((p.x / self.cell).floor() as i64, (p.y / self.cell).floor() as i64);
        let mut best: Option<(f64, usize, usize, f64)> = None;
        for x in cx - 1..=cx + 1 {
            for y in cy - 1..=cy + 1 {
                for &(ci, i) in self.grid.get(&(x, y)).into_iter().flatten() {
                    let Some(c) = self.contours.get(ci) else { continue };
                    let (Some(&a), Some(&b)) = (c.get(i), c.get((i + 1) % c.len())) else { continue };
                    let d = geom::point_segment_distance(p, a, b);
                    if d < 1e-3 && best.is_none_or(|bst| d < bst.0) {
                        let t = (p - a).dot(b - a) / (b - a).length_squared().max(1e-18);
                        best = Some((d, ci, i, t));
                    }
                }
            }
        }
        best.map(|(_, ci, i, t)| (ci, i, t))
    }

    /// The points along the edge from `a` to `b` (both on the same contour), the shorter way
    /// round, when that is at most `limit` long.
    fn along(&self, a: DVec2, b: DVec2, limit: f64) -> Option<Vec<DVec2>> {
        let (ca, ia, ta) = self.locate(a)?;
        let (cb, ib, tb) = self.locate(b)?;
        if ca != cb {
            return None;
        }
        let c = self.contours.get(ca)?;
        let n = c.len();
        let walk = |forward: bool| -> Option<(f64, Vec<DVec2>)> {
            if ia == ib && (tb >= ta) == forward {
                let d = a.distance(b);
                return (d <= limit).then(|| (d, Vec::new()));
            }
            let mut pts = Vec::new();
            let mut at = a;
            let mut length = 0.0;
            let mut k = if forward { (ia + 1) % n } else { ia };
            let stop = if forward { ib } else { (ib + 1) % n };
            for _ in 0..n {
                let q = *c.get(k)?;
                length += at.distance(q);
                if length > limit {
                    return None;
                }
                pts.push(q);
                at = q;
                if k == stop {
                    let total = length + at.distance(b);
                    return (total <= limit).then_some((total, pts));
                }
                k = if forward { (k + 1) % n } else { (k + n - 1) % n };
            }
            None
        };
        match (walk(true), walk(false)) {
            (Some(f), Some(r)) => Some(if f.0 <= r.0 { f.1 } else { r.1 }),
            (Some(f), None) => Some(f.1),
            (None, Some(r)) => Some(r.1),
            (None, None) => None,
        }
    }
}

/// Loops inset `spacing` apart until the area runs out.
fn concentric(area: &[Shape], spacing: f64) -> Vec<Line> {
    concentric_loops(area, spacing)
        .into_iter()
        .map(|points| Line { points, closed: true, ordered: false, one_way: false })
        .collect()
}

fn concentric_loops(area: &[Shape], spacing: f64) -> Vec<Polyline> {
    let mut out = Vec::new();
    let mut ring = geom::offset(area, -spacing / 2.0, Join::Miter);
    let mut guard = 0;
    while !ring.is_empty() && guard < 10_000 {
        for c in ring.iter().flatten() {
            out.push(geom::simplify_loop(c, 0.01));
        }
        ring = geom::offset(&ring, -spacing, Join::Miter);
        guard += 1;
    }
    out
}

/// Spiral inset: the concentric loops, outside in, each opened where the last one ended and
/// joined to it, so the region fills in one continuous line where it can.
fn spiral_inset(area: &[Shape], spacing: f64) -> Vec<Line> {
    let mut out: Vec<Line> = Vec::new();
    let mut cur: Polyline = Vec::new();
    for mut ring in concentric_loops(area, spacing) {
        if ring.len() < 3 {
            continue;
        }
        if let Some(end) = cur.last().copied() {
            let i = ring
                .iter()
                .enumerate()
                .min_by(|a, b| a.1.distance_squared(end).total_cmp(&b.1.distance_squared(end)))
                .map_or(0, |x| x.0);
            ring.rotate_left(i);
            let near = ring.first().is_some_and(|p| p.distance(end) < spacing * 2.0);
            if !near {
                out.push(Line::open(std::mem::take(&mut cur)));
            }
        }
        // Round the loop and back to its start, then on in to the next.
        if let Some(&first) = ring.first() {
            cur.extend(ring);
            cur.push(first);
        }
    }
    if cur.len() >= 2 {
        out.push(Line::open(cur));
    }
    out
}

/// A Hilbert curve on a grid of `step`, in tiles fixed to the plate so layers line up.
fn hilbert(area: &[Shape], a: f64, step: f64) -> Vec<Line> {
    const ORDER: u32 = 8;
    let side = 1u64 << ORDER;
    let tile = step * side as f64;
    let lines = framed(area, a, |lo, hi| {
        let (tx0, tx1) = ((lo.x / tile).floor() as i64, (hi.x / tile).floor() as i64);
        let (ty0, ty1) = ((lo.y / tile).floor() as i64, (hi.y / tile).floor() as i64);
        let tiles = ((tx1 - tx0 + 1) * (ty1 - ty0 + 1)).max(0) as u64;
        if tiles.saturating_mul(side * side) > MAX_POINTS as u64 {
            return Vec::new();
        }
        let curve: Vec<DVec2> = (0..side * side)
            .map(|d| {
                let (x, y) = hilbert_point(side, d);
                DVec2::new((x as f64 + 0.5) * step, (y as f64 + 0.5) * step)
            })
            .collect();
        let mut out = Vec::new();
        for ty in ty0..=ty1 {
            for tx in tx0..=tx1 {
                let origin = DVec2::new(tx as f64 * tile, ty as f64 * tile);
                // Skip tiles outside the bounds' rows (cheap) — clipping does the rest.
                out.push(geom::simplify(&curve.iter().map(|p| *p + origin).collect::<Vec<_>>(), 1e-6));
            }
        }
        out
    });
    lines.into_iter().filter(|l| geom::length(l) > step).map(Line::open).collect()
}

/// Point `d` along a Hilbert curve filling a `side` × `side` grid.
fn hilbert_point(side: u64, d: u64) -> (u64, u64) {
    let (mut x, mut y, mut t) = (0u64, 0u64, d);
    let mut s = 1u64;
    while s < side {
        let rx = 1 & (t / 2);
        let ry = 1 & (t ^ rx);
        if ry == 0 {
            if rx == 1 {
                x = s - 1 - x;
                y = s - 1 - y;
            }
            std::mem::swap(&mut x, &mut y);
        }
        x += s * rx;
        y += s * ry;
        t /= 4;
        s *= 2;
    }
    (x, y)
}

/// The radius from the bounds' centre that covers them.
fn centre_and_reach(area: &[Shape]) -> Option<(DVec2, f64)> {
    let (lo, hi) = geom::bounds(area)?;
    Some(((lo + hi) / 2.0, (hi - lo).length() / 2.0 + 1.0))
}

/// Archimedean chords: a spiral from the region's centre, `step` between turns.
fn archimedean(area: &[Shape], step: f64) -> Vec<Line> {
    let Some((c, reach)) = centre_and_reach(area) else { return Vec::new() };
    let turns = reach / step;
    // About half a millimetre a chord, coarser on huge regions.
    let total = std::f64::consts::PI * reach * reach / step;
    let chord = (total / MAX_POINTS as f64).max(0.5);
    let mut pts = Vec::new();
    let mut theta: f64 = 0.0;
    while theta < turns * TAU {
        let r = step * theta / TAU;
        pts.push(c + DVec2::new(r * theta.cos(), r * theta.sin()));
        theta += (chord / r.max(step)).min(0.5);
    }
    let lines = geom::clip_lines(&[pts], area);
    lines.into_iter().filter(|l| geom::length(l) > step).map(Line::open).collect()
}

/// Octagram spiral: an eight-pointed star wound out from the centre, `step` between turns
/// (Slic3r's construction).
fn octagram(area: &[Shape], step: f64) -> Vec<Line> {
    let Some((c, reach)) = centre_and_reach(area) else { return Vec::new() };
    let rmax = reach / step * std::f64::consts::SQRT_2 + 1.5;
    if rmax * 16.0 > MAX_POINTS as f64 {
        return Vec::new();
    }
    let inc = std::f64::consts::SQRT_2;
    let mut pts = vec![c];
    let mut r = 0.0;
    while r < rmax {
        r += inc;
        let rx = r / std::f64::consts::SQRT_2;
        let r2 = r + rx;
        for (x, y) in [
            (r, 0.0),
            (r2, rx),
            (rx, rx),
            (rx, r2),
            (0.0, r),
            (-rx, r2),
            (-rx, rx),
            (-r2, rx),
            (-r, 0.0),
            (-r2, -rx),
            (-rx, -rx),
            (-rx, -r2),
            (0.0, -r),
            (rx, -r2),
            (rx, -rx),
            (r2 + inc, -rx),
        ] {
            pts.push(c + DVec2::new(x, y) * step);
        }
    }
    let lines = geom::clip_lines(&[pts], area);
    lines.into_iter().filter(|l| geom::length(l) > step).map(Line::open).collect()
}

/// Infill anchors: open line ends on the region's edge carry on along it for `len`, so the
/// line holds to the walls (OrcaSlicer's `infill_anchor`). Ends follow the edge to their left
/// as they leave, so neighbouring anchors run the same way and never meet.
fn anchor_ends(lines: &mut [Line], area: &[Shape], len: f64, tol: f64) {
    let contours: Vec<&Vec<DVec2>> = area.iter().flatten().filter(|c| c.len() >= 3).collect();
    if contours.is_empty() || len.is_nan() || len <= 0.0 {
        return;
    }
    for l in lines.iter_mut().filter(|l| !l.closed && !l.ordered && l.points.len() >= 2) {
        let n = l.points.len();
        let (Some(&s0), Some(&s1), Some(&e0), Some(&e1)) =
            (l.points.first(), l.points.get(1), l.points.get(n - 1), l.points.get(n - 2))
        else {
            continue;
        };
        if let Some(mut a) = edge_walk(&contours, e0, (e0 - e1).normalize_or_zero(), len, tol) {
            l.points.append(&mut a);
        }
        if let Some(a) = edge_walk(&contours, s0, (s0 - s1).normalize_or_zero(), len, tol) {
            l.points.splice(0..0, a.into_iter().rev());
        }
    }
}

/// From `p` on a contour (within `tol`), `len` along it to the left of `out`.
fn edge_walk(contours: &[&Vec<DVec2>], p: DVec2, out: DVec2, len: f64, tol: f64) -> Option<Vec<DVec2>> {
    let mut best: Option<(f64, usize, usize)> = None;
    for (ci, c) in contours.iter().enumerate() {
        for i in 0..c.len() {
            let (Some(&a), Some(&b)) = (c.get(i), c.get((i + 1) % c.len())) else { continue };
            let d = geom::point_segment_distance(p, a, b);
            if d < tol && best.is_none_or(|x| d < x.0) {
                best = Some((d, ci, i));
            }
        }
    }
    let (_, ci, i) = best?;
    let c = contours.get(ci)?;
    let n = c.len();
    let (a, b) = (*c.get(i)?, *c.get((i + 1) % n)?);
    let forward = (b - a).dot(out.perp()) > 0.0;
    let mut pts = Vec::new();
    let mut left = len;
    let mut at = p;
    let mut k = if forward { (i + 1) % n } else { i };
    for _ in 0..n {
        let q = *c.get(k)?;
        let d = at.distance(q);
        if d >= left {
            if d > 1e-9 {
                pts.push(at + (q - at) * (left / d));
            }
            break;
        }
        left -= d;
        pts.push(q);
        at = q;
        k = if forward { (k + 1) % n } else { (k + n - 1) % n };
    }
    (!pts.is_empty()).then_some(pts)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn square(size: f64) -> Vec<Shape> {
        vec![geom::rectangle(DVec2::ZERO, DVec2::splat(size))]
    }

    fn total(lines: &[Line]) -> f64 {
        lines
            .iter()
            .map(|l| {
                let close = match (l.closed, l.points.first(), l.points.last()) {
                    (true, Some(a), Some(b)) => a.distance(*b),
                    _ => 0.0,
                };
                geom::length(&l.points) + close
            })
            .sum()
    }

    const SPARSE: [&str; 26] = [
        "rectilinear",
        "alignedrectilinear",
        "zigzag",
        "crosszag",
        "lockedzag",
        "line",
        "grid",
        "triangles",
        "tri-hexagon",
        "cubic",
        "adaptivecubic",
        "quartercubic",
        "supportcubic",
        "lightning",
        "honeycomb",
        "3dhoneycomb",
        "lateral-honeycomb",
        "lateral-lattice",
        "crosshatch",
        "tpmsd",
        "tpmsfk",
        "gyroid",
        "concentric",
        "hilbertcurve",
        "archimedeanchords",
        "octagramspiral",
    ];

    #[test]
    fn density_sets_how_much_line_there_is() {
        let area = square(50.0);
        for pattern in SPARSE {
            // Lock zag's densities are its own; graded cubics are sparser inside by design.
            if matches!(pattern, "lockedzag" | "adaptivecubic" | "supportcubic") {
                continue;
            }
            // Solid would be area / width of line; 20% of that, give or take: on average over
            // heights, and not far off on any one layer (TPMS sections vary with height).
            let expected = 2500.0 / 0.45 * 0.2;
            let mut sum = 0.0;
            let heights = [1.0, 5.3, 12.7, 20.1];
            for z in heights {
                let lines = fill(&area, kind_of(pattern), 0.45, 0.2, 0.785, 3, z);
                let got = total(&lines);
                sum += got;
                assert!((got - expected).abs() / expected < 0.45, "{pattern} at {z}: {got} vs {expected}");
                assert!(lines.iter().flat_map(|l| &l.points).all(|p| p.x >= -2e-6 && p.x <= 50.0 + 2e-6), "{pattern}");
            }
            let mean = sum / heights.len() as f64;
            assert!((mean - expected).abs() / expected < 0.25, "{pattern}: {mean} vs {expected}");
        }
    }

    /// `cargo test -p CosmicSlicer-engine density_report -- --ignored --nocapture` prints each
    /// pattern's line length against the asked density, over heights.
    #[test]
    #[ignore]
    fn density_report() {
        let area = square(50.0);
        for pattern in SPARSE {
            let ratios: Vec<String> = (0..12)
                .map(|k| {
                    let z = 0.2 + k as f64 * 1.37;
                    let got = total(&fill(&area, kind_of(pattern), 0.45, 0.2, 0.785, k, z));
                    format!("{:.2}", got / (2500.0 / 0.45 * 0.2))
                })
                .collect();
            println!("{pattern:20} {}", ratios.join(" "));
        }
    }

    #[test]
    fn every_pattern_name_has_its_own_generator() {
        let kinds: std::collections::HashSet<String> = SPARSE.iter().map(|p| format!("{:?}", kind_of(p))).collect();
        assert_eq!(kinds.len(), SPARSE.len());
        for p in ["monotonic", "monotonicline", "spiralinset"] {
            assert!(!kinds.contains(&format!("{:?}", kind_of(p))), "{p}");
        }
    }

    #[test]
    fn graded_cubics_are_sparser_deep_inside() {
        let area = square(80.0);
        let plain = total(&fill(&area, kind_of("cubic"), 0.45, 0.2, 0.0, 0, 3.0));
        let adaptive = total(&fill(&area, kind_of("adaptivecubic"), 0.45, 0.2, 0.0, 0, 3.0));
        let support = total(&fill(&area, kind_of("supportcubic"), 0.45, 0.2, 0.0, 0, 3.0));
        assert!(support < adaptive && adaptive < plain, "{support} {adaptive} {plain}");
    }

    #[test]
    fn solid_patterns_cover_the_area() {
        let area = square(20.0);
        for pattern in [
            "monotonic",
            "monotonicline",
            "rectilinear",
            "concentric",
            "spiralinset",
            "hilbertcurve",
            "archimedeanchords",
            "octagramspiral",
        ] {
            let got = total(&fill(&area, kind_of(pattern), 0.42, 1.0, 0.0, 0, 0.0));
            let expected = 400.0 / 0.42;
            assert!((got - expected).abs() / expected < 0.2, "{pattern}: {got} vs {expected}");
        }
    }

    #[test]
    fn solid_rectilinear_zigzags() {
        let lines = fill(&square(20.0), kind_of("rectilinear"), 0.42, 1.0, 0.0, 0, 0.0);
        // Joined into one zig-zag rather than ~48 separate lines.
        assert!(lines.len() <= 3, "{}", lines.len());
        assert!(total(&lines) > 900.0);
    }

    #[test]
    fn solid_lines_round_a_hole_join_along_the_edge() {
        // A 20 mm square with a 10 mm hole, filled solid at 45°: rows split by the hole join
        // along the edges into a few zig-zags (OrcaSlicer's links along the perimeter), not
        // one line per row.
        let mut ring = geom::rectangle(DVec2::ZERO, DVec2::splat(20.0));
        let mut hole = geom::rectangle(DVec2::splat(5.0), DVec2::splat(15.0)).remove(0);
        hole.reverse();
        ring.push(hole);
        let mut p = Params::new(0.42, 1.0, std::f64::consts::FRAC_PI_4, 0, 0.2);
        p.height = 0.2;
        let lines = fill_with(&[ring], kind_of("monotonic"), &p);
        assert!(lines.len() <= 8, "{} lines", lines.len());
        assert!(total(&lines) > 300.0 / 0.38 * 0.9, "{}", total(&lines));
    }

    #[test]
    fn monotonic_lines_sweep_across_in_order() {
        // OrcaSlicer's monotonic lines: each on its own, across the square in order, turn about.
        let mut p = Params::new(0.42, 1.0, 0.0, 0, 0.0);
        p.inset = true;
        p.fit_solid = true;
        let lines = fill_with(&square(10.0), kind_of("monotonicline"), &p);
        assert!(lines.len() > 20 && lines.iter().all(|l| l.ordered && l.points.len() == 2), "{}", lines.len());
        let ys: Vec<f64> = lines.iter().flat_map(|l| l.points.iter().map(|p| p.y)).collect();
        assert!(ys.windows(2).all(|w| w[1] >= w[0] - 1e-9));
        assert!(lines.windows(2).all(|w| (w[0].points[0].x - w[1].points[0].x).abs() > 5.0));
    }

    #[test]
    #[ignore = "heavy: a 300 mm gyroid; run with --ignored --release"]
    fn the_gyroid_is_fast_and_smooth() {
        let area = square(300.0);
        let started = std::time::Instant::now();
        let lines = fill(&area, Kind::Gyroid, 0.45, 0.15, 0.785, 0, 17.3);
        assert!(started.elapsed().as_secs_f64() < 2.0);
        let expected = 90_000.0 / 0.45 * 0.15;
        assert!((total(&lines) - expected).abs() / expected < 0.3);
        // Waves are long: a few hundred lines, not many thousands of fragments.
        assert!(lines.len() < 3000, "{}", lines.len());
    }

    #[test]
    fn honeycomb_walls_stack_from_layer_to_layer() {
        // Turned 60° a layer about a hexagon centre: the same hexagons.
        let area = square(30.0);
        let pts = |layer: usize| -> Vec<DVec2> {
            fill(&area, Kind::Honeycomb, 0.45, 0.2, 0.0, layer, 0.0).into_iter().flat_map(|l| l.points).collect()
        };
        let (a, b) = (pts(0), pts(1));
        let near = b.iter().filter(|q| a.iter().any(|p| p.distance(**q) < 0.5)).count();
        assert!(near as f64 > b.len() as f64 * 0.5, "{near} of {}", b.len());
    }

    #[test]
    fn anchors_follow_the_edge() {
        let mut p = Params::new(0.45, 0.2, 0.0, 0, 0.0);
        let plain = total(&fill_with(&square(30.0), Kind::Line, &p));
        p.anchor = 1.8;
        let anchored = fill_with(&square(30.0), Kind::Line, &p);
        let gained = total(&anchored) - plain;
        // Two anchors a line, each capped at 0.45 of the 2.25 mm spacing.
        let each = 0.45 * 0.45 / 0.2;
        assert!((gained - anchored.len() as f64 * 2.0 * each).abs() < 1.0, "{gained} over {} lines", anchored.len());
        assert!(anchored.iter().flat_map(|l| &l.points).all(|q| q.x > -1e-6 && q.x < 30.0 + 1e-6));
    }

    #[test]
    fn hilbert_points_visit_every_cell() {
        let mut seen = std::collections::HashSet::new();
        for d in 0..64 {
            seen.insert(hilbert_point(8, d));
        }
        assert_eq!(seen.len(), 64);
    }

    #[test]
    fn nothing_from_nothing() {
        assert!(fill(&[], kind_of("grid"), 0.45, 0.2, 0.0, 0, 0.0).is_empty());
        assert!(fill(&square(10.0), kind_of("grid"), 0.45, 0.0, 0.0, 0, 0.0).is_empty());
        assert!(fill(&square(10.0), kind_of("gyroid"), 0.45, f64::NAN, 0.0, 0, 0.0).is_empty());
    }
}
