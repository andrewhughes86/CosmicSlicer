//! OrcaSlicer's `Algorithm::RegionExpansion`: surfaces grow into the zones beside them in waves
//! that follow each zone, so a surface reaches round a corner of the zone but never across a
//! gap in it. In Clipper's units, shapes as Orca's `ExPolygons` (contour first, then holes).
//!
//! Orca tags the pieces it clips with Clipper's Z coordinate to learn which source and which
//! zone each came from; here each source outline is clipped against each zone apart, which
//! gives the same pieces with their tags known.

use std::panic::{AssertUnwindSafe, catch_unwind};

use crate::clipper::{
    self, ClipType, Clipper, ClipperOffset, EndType, Failed, FillType, JoinType, Path, Paths, PolyType, Pt,
};
use crate::geom;

/// One shape in Clipper's units: contour first, then its holes.
pub type ExPoly = Paths;

/// `ClipperOffsetShortestEdgeFactor`.
const SHORTEST_EDGE_FACTOR: f64 = 0.005;

fn guarded<T: Default>(run: impl FnOnce() -> Result<T, Failed>) -> T {
    catch_unwind(AssertUnwindSafe(|| run().unwrap_or_default())).unwrap_or_default()
}

/// `RegionExpansionParameters`.
#[derive(Clone, Copy, Debug)]
pub struct Params {
    pub tiny: f32,
    initial_step: f32,
    other_step: f32,
    other_steps: usize,
    arc_tolerance: f64,
    shortest_edge: f64,
}

impl Params {
    /// `RegionExpansionParameters::build`, in Orca's floats.
    pub fn build(full: f32, step: f32, max_steps: usize) -> Params {
        let mut tiny = (0.25f32 * full).min((f64::from(0.05f32) / 1e-6) as f32);
        let mut nsteps = ((full - tiny) / step).ceil().max(0.0) as usize;
        if max_steps > 0 {
            nsteps = nsteps.min(max_steps);
        }
        // Orca asserts at least one step.
        nsteps = nsteps.max(1);
        let mut initial = (full - tiny) / nsteps as f32;
        if nsteps > 1 && 0.25 * f64::from(initial) < f64::from(tiny) {
            nsteps = ((f64::from(full - tiny) / (4.0 * f64::from(tiny))).floor().max(0.0) as usize).max(1);
            initial = (full - tiny) / nsteps as f32;
        }
        if 0.25 * f64::from(initial) < f64::from(tiny) || nsteps == 1 {
            tiny = 0.2f32 * full;
            initial = 0.8f32 * full;
        }
        Params {
            tiny,
            initial_step: initial,
            other_step: initial,
            other_steps: nsteps - 1,
            arc_tolerance: 0.1 / 1e-6,
            shortest_edge: f64::from(initial) * SHORTEST_EDGE_FACTOR,
        }
    }
}

/// `WaveSeed`: a piece of a source's slightly grown outline that lies in a zone.
#[derive(Clone, Debug)]
pub struct Seed {
    pub src: u32,
    pub boundary: u32,
    pub path: Path,
}

/// `RegionExpansion`.
#[derive(Clone, Debug)]
pub struct Expansion {
    pub polygon: Path,
    pub src: u32,
    pub boundary: u32,
}

/// `RegionExpansionEx`.
#[derive(Clone, Debug)]
pub struct ExpansionEx {
    pub expoly: ExPoly,
    pub src: u32,
    pub boundary: u32,
}

fn bounds(paths: &[Path]) -> Option<(Pt, Pt)> {
    let mut it = paths.iter().flatten();
    let first = *it.next()?;
    Some(it.fold((first, first), |(lo, hi), p| {
        (Pt::new(lo.x.min(p.x), lo.y.min(p.y)), Pt::new(hi.x.max(p.x), hi.y.max(p.y)))
    }))
}

fn overlap(a: Option<(Pt, Pt)>, b: Option<(Pt, Pt)>) -> bool {
    match (a, b) {
        (Some((alo, ahi)), Some((blo, bhi))) => alo.x <= bhi.x && blo.x <= ahi.x && alo.y <= bhi.y && blo.y <= ahi.y,
        _ => false,
    }
}

/// `merge_splits` for one outline: Clipper cuts a closed outline at its first point too, and
/// the two pieces either side of it are one.
fn merge_split(mut pieces: Paths, start: Pt) -> Paths {
    let ends_at = pieces.iter().position(|p| p.first() != p.last() && p.last() == Some(&start));
    let starts_at = pieces.iter().position(|p| p.first() != p.last() && p.first() == Some(&start));
    if let (Some(a), Some(b)) = (ends_at, starts_at)
        && a != b
    {
        let tail: Path = pieces.get(b).map(|p| p.iter().skip(1).copied().collect()).unwrap_or_default();
        if let Some(head) = pieces.get_mut(a) {
            head.extend(tail);
        }
        let (keep, gone) = (a.min(b), a.max(b));
        if keep != a {
            pieces.swap(keep, a);
        }
        pieces.remove(gone);
    }
    pieces
}

/// `wave_seeds`, sorted by zone and then source.
pub fn wave_seeds(src: &[ExPoly], boundary: &[ExPoly], tiny: f32) -> Vec<Seed> {
    if src.is_empty() || boundary.is_empty() {
        return Vec::new();
    }
    let boxes: Vec<Option<(Pt, Pt)>> = boundary.iter().map(|b| bounds(b.get(..1).unwrap_or(&[]))).collect();
    let mut out: Vec<Seed> = Vec::new();
    for (si, ex) in src.iter().enumerate() {
        for (ci, contour) in ex.iter().enumerate() {
            // `expolygons_to_zpaths_expanded_opened`.
            let mut co = ClipperOffset::new();
            co.shortest_edge = f64::from(tiny) * SHORTEST_EDGE_FACTOR;
            co.add_path(contour, JoinType::Square, EndType::ClosedPolygon);
            let delta = f64::from(if ci == 0 { tiny } else { -tiny });
            let loops = guarded(|| co.execute(delta));
            for mut outline in loops {
                let Some(&start) = outline.first() else { continue };
                outline.push(start);
                let reach = bounds(std::slice::from_ref(&outline));
                for (bi, zone) in boundary.iter().enumerate() {
                    if !overlap(reach, boxes.get(bi).copied().flatten()) {
                        continue;
                    }
                    let pieces = geom::clip_open(std::slice::from_ref(&outline), zone, ClipType::Intersection);
                    for path in merge_split(pieces, start) {
                        if path.len() >= 2 {
                            out.push(Seed { src: si as u32, boundary: bi as u32, path });
                        }
                    }
                }
            }
        }
    }
    crate::connect::std_sort(&mut out, &|l: &Seed, r: &Seed| {
        l.boundary < r.boundary || (l.boundary == r.boundary && l.src < r.src)
    });
    out
}

fn offsetter(p: &Params) -> ClipperOffset {
    let mut co = ClipperOffset::new();
    co.arc_tolerance = p.arc_tolerance;
    co.shortest_edge = p.shortest_edge;
    co
}

fn wavefront_initial(seed: &[Path], p: &Params) -> Paths {
    let mut out = Vec::new();
    for path in seed {
        let mut co = offsetter(p);
        let end = if path.first() == path.last() { EndType::ClosedLine } else { EndType::OpenRound };
        co.add_path(path, JoinType::Round, end);
        out.extend(guarded(|| co.execute(f64::from(p.initial_step))));
    }
    out
}

fn wavefront_step(polygons: &[Path], p: &Params) -> Paths {
    let mut out = Vec::new();
    for polygon in polygons {
        let mut co = offsetter(p);
        co.add_path(polygon, JoinType::Round, EndType::ClosedPolygon);
        let ccw = clipper::orientation(polygon);
        let step = f64::from(p.other_step);
        let mut this = guarded(|| co.execute(if ccw { step } else { -step }));
        if !ccw {
            this.iter_mut().for_each(|q| q.reverse());
        }
        out.extend(this);
    }
    out
}

fn wavefront_clip(front: &[Path], clip: &[Path]) -> Paths {
    guarded(|| {
        let mut c = Clipper::new();
        c.add_paths(front, PolyType::Subject, true)?;
        c.add_paths(clip, PolyType::Clip, true)?;
        c.execute(ClipType::Intersection, FillType::Positive, FillType::Positive)
    })
}

/// `propagate_wave_from_boundary`. Orca first cuts the zone to the seeds' reach, which the
/// waves never leave, so the whole zone clips the same.
fn propagate_from(seed: &[Path], boundary: &ExPoly, p: &Params) -> Paths {
    let mut polygons = wavefront_clip(&wavefront_initial(seed, p), boundary);
    for _ in 0..p.other_steps {
        if polygons.is_empty() {
            break;
        }
        polygons = wavefront_clip(&wavefront_step(&polygons, p), boundary);
    }
    polygons
}

/// `propagate_waves` from sorted seeds.
pub fn propagate_waves(seeds: &[Seed], boundary: &[ExPoly], p: &Params) -> Vec<Expansion> {
    let mut out = Vec::new();
    let mut i = 0;
    while let Some(first) = seeds.get(i) {
        let mut paths: Paths = Vec::new();
        let mut j = i;
        while let Some(s) = seeds.get(j).filter(|s| s.boundary == first.boundary && s.src == first.src) {
            paths.push(s.path.clone());
            j += 1;
        }
        if let Some(zone) = boundary.get(first.boundary as usize) {
            for polygon in propagate_from(&paths, zone, p) {
                out.push(Expansion { polygon, src: first.src, boundary: first.boundary });
            }
        }
        i = j;
    }
    out
}

/// `propagate_waves_ex`: each source's waves in each zone, united.
pub fn propagate_waves_ex(seeds: &[Seed], boundary: &[ExPoly], p: &Params) -> Vec<ExpansionEx> {
    let expanded = propagate_waves(seeds, boundary, p);
    let mut out = Vec::new();
    let mut i = 0;
    while let Some(first) = expanded.get(i) {
        let mut j = i;
        let mut acc: Paths = Vec::new();
        while let Some(e) = expanded.get(j).filter(|e| e.boundary == first.boundary && e.src == first.src) {
            acc.push(e.polygon.clone());
            j += 1;
        }
        if acc.len() == 1 {
            out.push(ExpansionEx { expoly: acc, src: first.src, boundary: first.boundary });
        } else {
            for shape in geom::union_paths(&acc) {
                out.push(ExpansionEx { expoly: geom::paths_of(&shape), src: first.src, boundary: first.boundary });
            }
        }
        i = j;
    }
    out
}

/// `ExPolygon::contains`, a point on the outline counting as inside.
pub fn contains(ex: &ExPoly, p: Pt) -> bool {
    let Some((contour, holes)) = ex.split_first() else { return false };
    clipper::point_in_polygon(p, contour) != 0 && !holes.iter().any(|h| clipper::point_in_polygon(p, h) == 1)
}

/// `merge_expansions_into_expolygons`: each source with its waves.
pub fn merge_expansions(src: Vec<ExPoly>, mut expanded: Vec<Expansion>) -> Vec<ExPoly> {
    crate::connect::std_sort(&mut expanded, &|l: &Expansion, r: &Expansion| l.src < r.src);
    let n = src.len();
    let mut src: Vec<Option<ExPoly>> = src.into_iter().map(Some).collect();
    let mut out: Vec<ExPoly> = Vec::with_capacity(n);
    let mut last = 0usize;
    let mut i = 0;
    while let Some(first) = expanded.get(i) {
        let id = first.src as usize;
        while last < id.min(n) {
            out.extend(src.get_mut(last).and_then(Option::take));
            last += 1;
        }
        let mut acc: Paths = Vec::new();
        while let Some(e) = expanded.get(i).filter(|e| e.src as usize == id) {
            acc.push(e.polygon.clone());
            i += 1;
        }
        let Some(own) = src.get_mut(last).and_then(Option::take) else {
            last += 1;
            continue;
        };
        last += 1;
        let Some(&sample) = own.first().and_then(|c| c.first()) else { continue };
        acc.extend(own);
        // `union_safety_offset_ex`.
        let merged = geom::shapes_nm(&geom::expand_polygons(&acc, 10.0));
        if merged.len() > 1 {
            if let Some(m) = merged.into_iter().find(|m| contains(m, sample)) {
                out.push(m);
            }
        } else {
            out.extend(merged);
        }
    }
    while last < n {
        out.extend(src.get_mut(last).and_then(Option::take));
        last += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn square(x0: i64, y0: i64, x1: i64, y1: i64) -> ExPoly {
        vec![vec![Pt::new(x0, y0), Pt::new(x1, y0), Pt::new(x1, y1), Pt::new(x0, y1)]]
    }

    #[test]
    fn params_follow_orcas_steps() {
        let p = Params::build(900_000.0, 100_000.0, 5);
        assert!(p.other_steps < 5 && p.tiny > 0.0);
        let total = p.tiny + p.initial_step * (p.other_steps + 1) as f32;
        assert!((total - 900_000.0).abs() < 1.0, "{p:?}");
    }

    #[test]
    fn a_surface_grows_into_the_zone_it_touches() {
        // A 2 mm square beside a 10 mm zone: the wave goes in about a millimetre.
        let src = vec![square(0, 0, 2_000_000, 2_000_000)];
        let zone = vec![square(2_000_000, -4_000_000, 12_000_000, 6_000_000)];
        let p = Params::build(1_000_000.0, 100_000.0, 5);
        let seeds = wave_seeds(&src, &zone, p.tiny);
        assert!(!seeds.is_empty());
        assert!(seeds.iter().all(|s| s.src == 0 && s.boundary == 0));
        let grown = propagate_waves(&seeds, &zone, &p);
        let reach = grown.iter().flat_map(|e| e.polygon.iter()).map(|q| q.x).max().unwrap_or(0);
        assert!((2_900_000..=3_100_000).contains(&reach), "{reach}");
        let merged = merge_expansions(src, grown);
        assert_eq!(merged.len(), 1);
    }

    #[test]
    fn a_surface_apart_from_the_zone_stays_put() {
        let src = vec![square(0, 0, 1_000_000, 1_000_000)];
        let zone = vec![square(5_000_000, 0, 6_000_000, 1_000_000)];
        let p = Params::build(1_000_000.0, 100_000.0, 5);
        assert!(wave_seeds(&src, &zone, p.tiny).is_empty());
    }
}
