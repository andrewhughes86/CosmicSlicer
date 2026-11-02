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

use crate::geom::{self, Join, Polyline, Shape};

/// A fill line: open, or a loop for concentric patterns.
#[derive(Clone, Debug, PartialEq)]
pub struct Line {
    pub points: Polyline,
    pub closed: bool,
    /// Printed in the order given (monotonic patterns), not nearest first.
    pub ordered: bool,
}

impl Line {
    fn open(points: Polyline) -> Line {
        Line { points, closed: false, ordered: false }
    }
}

/// OrcaSlicer's fill patterns.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// Parallel lines joined into zig-zags, turning 90° a layer when `alternate`.
    Rectilinear {
        alternate: bool,
    },
    /// Lines printed side by side in one sweep, joined into zig-zags or each on its own.
    Monotonic {
        joined: bool,
    },
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
        "monotonic" => Kind::Monotonic { joined: true },
        "monotonicline" => Kind::Monotonic { joined: false },
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
    /// Lateral lattice angles from vertical, radians.
    pub lattice: [f64; 2],
    /// Cross zag's shift per layer, mm.
    pub shift_step: f64,
    pub skin: Skin,
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
            lattice: [-std::f64::consts::FRAC_PI_4, std::f64::consts::FRAC_PI_4],
            shift_step: 0.4,
            skin: Skin { depth: 2.0, density: 0.25, skeleton: 0.25 },
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
    let (w, d) = (p.width, p.density.min(1.0));
    if area.is_empty() || !(d > 0.0 && w > 0.0 && w.is_finite() && p.angle.is_finite() && p.z.is_finite()) {
        return Vec::new();
    }
    let s = w / d;
    let turned = |a: f64| if p.turn_by_layer && p.layer % 2 == 1 { a + FRAC_PI_2 } else { a };
    let a = p.angle;
    let mut lines = match kind {
        Kind::Rectilinear { alternate } => parallel(area, s, if alternate { turned(a) } else { a }, 0.0, w),
        Kind::Monotonic { joined } => monotonic(area, s, turned(a), w, joined),
        Kind::Line => rows(area, s, turned(a), 0.0, w).into_iter().map(Line::open).collect(),
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
                parallel(area, spacing, a, (p.z * tilt.tan()).rem_euclid(spacing), w)
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
    if p.anchor > 0.0 {
        // Never so long that neighbouring anchors meet.
        anchor_ends(&mut lines, area, p.anchor.min(s * 0.45), w * 0.25);
    }
    lines.retain(|l| l.points.len() >= 2);
    lines
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
fn regions(pieces: Vec<Polyline>, a: f64, spacing: f64) -> Vec<Vec<Polyline>> {
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
            let row = (l.first().copied().unwrap_or_default().dot(normal) / spacing).round();
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

/// Pieces of lines along `a`, joined zig-zag within each region.
fn join_rows(pieces: Vec<Polyline>, a: f64, spacing: f64, area: &[Shape], max_step: f64) -> Vec<Line> {
    regions(pieces, a, spacing).into_iter().flat_map(|r| join_zigzag(r, area, max_step)).collect()
}

/// Lines `spacing` apart at angle `a`, clipped and joined zig-zag where they meet.
fn parallel(area: &[Shape], spacing: f64, a: f64, phase: f64, width: f64) -> Vec<Line> {
    join_rows(rows(area, spacing, a, phase, width), a, spacing, area, spacing * 1.9)
}

/// Parallel lines in several directions, each `(angle, phase)`.
fn directions(area: &[Shape], width: f64, spacing: f64, dirs: &[(f64, f64)]) -> Vec<Line> {
    dirs.iter().flat_map(|&(a, phase)| parallel(area, spacing, a, phase.rem_euclid(spacing), width)).collect()
}

/// Monotonic: every line runs the same way, printed in order across the region.
fn monotonic(area: &[Shape], spacing: f64, a: f64, width: f64, joined: bool) -> Vec<Line> {
    let pieces = rows(area, spacing, a, 0.0, width);
    let mut out = if joined {
        join_rows(pieces, a, spacing, area, spacing * 1.9)
    } else {
        regions(pieces, a, spacing).into_iter().flatten().map(Line::open).collect()
    };
    for l in &mut out {
        l.ordered = true;
    }
    out
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
        out.extend(parallel(&near, spacing, a, phase.rem_euclid(spacing), p.width));
        if !far.is_empty() {
            out.extend(parallel(&far, spacing * coarse, a, phase, p.width));
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
    join_rows(lines, a, spacing, area, spacing * 1.9)
}

/// Cross zag: zig-zag waves along the infill direction, shifted along by `shift_step` each
/// layer, so each layer's lines cross the ones below.
fn cross_zag(area: &[Shape], a: f64, w: f64, d: f64, p: &Params) -> Vec<Line> {
    let spacing = w / d * std::f64::consts::SQRT_2;
    let shift = (p.layer as f64 * p.shift_step).rem_euclid(spacing);
    waves(area, a, spacing, shift, false)
}

/// Cross hatch: straight lines that keep their direction for a while, then bend through
/// zig-zags into the other direction, so layers bond without abrupt crossings.
fn cross_hatch(area: &[Shape], p: &Params, s: f64) -> Vec<Line> {
    // Each layer runs one way, so its lines carry the whole density.
    let grid = s;
    let (repeat, transition) = (grid * 2.0, grid * 0.8);
    let period = repeat + transition;
    let cycle = (p.z / period).floor();
    let phase = p.z - cycle * period;
    let a = p.angle + if (cycle as i64).rem_euclid(2) == 1 { FRAC_PI_2 } else { 0.0 };
    if phase < repeat {
        parallel(area, grid, a, 0.0, p.width)
    } else {
        // Waves growing to 45°, the rows spread as they lengthen so the density holds; at the
        // end neighbouring rows touch.
        let t = ((phase - repeat) / transition).clamp(0.0, 1.0);
        let spacing = grid * (1.0 + t * t).sqrt();
        wave_rows(area, a, spacing, spacing * 2.0, t * spacing / 2.0, 0.0, true)
    }
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
    join_rows(lines, p.angle, wid / 2.0, area, wid)
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

/// Joins each line to the next when the step between their ends is short and stays inside
/// `area`, flipping every other line, so neighbouring rows print as one zig-zag.
fn join_zigzag(lines: Vec<Polyline>, area: &[Shape], max_step: f64) -> Vec<Line> {
    // Line ends lie on the boundary, so the step between them runs along it: test against the
    // area grown by a little.
    let area = &geom::offset(area, max_step * 0.25, Join::Miter);
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
            let step = start.distance(end);
            if step < max_step && geom::contains(area, (start + end) / 2.0) {
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

/// Loops inset `spacing` apart until the area runs out.
fn concentric(area: &[Shape], spacing: f64) -> Vec<Line> {
    concentric_loops(area, spacing).into_iter().map(|points| Line { points, closed: true, ordered: false }).collect()
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
                assert!(lines.iter().flat_map(|l| &l.points).all(|p| p.x >= -1e-6 && p.x <= 50.0 + 1e-6), "{pattern}");
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
    fn monotonic_lines_run_one_way_in_order() {
        let lines = fill(&square(10.0), kind_of("monotonicline"), 0.42, 1.0, 0.0, 0, 0.0);
        assert!(lines.len() > 20 && lines.iter().all(|l| l.ordered));
        assert!(lines.iter().all(|l| l.points[l.points.len() - 1].x > l.points[0].x));
        assert!(lines.windows(2).all(|w| w[1].points[0].y > w[0].points[0].y));
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
