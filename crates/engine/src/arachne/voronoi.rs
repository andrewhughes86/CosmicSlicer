//! Orca's `Geometry::VoronoiDiagram` and `VoronoiUtils` for Arachne: Boost's diagram of the
//! outline's segments (through `boostvoronoi`, a port of it), copied into plain arrays, with
//! the checks for the diagrams Boost gets wrong and the repair by turning the input.

use std::panic::{AssertUnwindSafe, catch_unwind};

use boostvoronoi::prelude::{Builder, SourceCategory};

use super::geo;
use crate::clipper::Pt;

pub const NONE: usize = usize::MAX;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Category {
    SinglePoint,
    SegmentStart,
    SegmentEnd,
    Segment,
}

#[derive(Clone, Copy, Debug)]
pub struct Cell {
    pub source: usize,
    pub category: Category,
    pub incident: usize,
}

impl Cell {
    pub fn contains_point(&self) -> bool {
        matches!(self.category, Category::SinglePoint | Category::SegmentStart | Category::SegmentEnd)
    }

    pub fn contains_segment(&self) -> bool {
        self.category == Category::Segment
    }

    pub fn is_degenerate(&self) -> bool {
        self.incident == NONE
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Edge {
    pub cell: usize,
    pub twin: usize,
    pub next: usize,
    pub v0: usize,
    pub primary: bool,
    pub linear: bool,
}

#[derive(Clone, Copy, Debug)]
pub struct Vertex {
    pub x: f64,
    pub y: f64,
    color: usize,
}

/// One input segment: `PolygonsSegmentIndex`, its polygon and its point.
#[derive(Clone, Copy, Debug)]
pub struct Segment {
    pub poly: usize,
    pub point: usize,
}

#[derive(Clone, Debug, Default)]
pub struct Diagram {
    pub cells: Vec<Cell>,
    pub edges: Vec<Edge>,
    pub vertices: Vec<Vertex>,
}

/// The input: the outline's polygons and one segment per point.
pub struct Input<'a> {
    pub polys: &'a [Vec<Pt>],
    pub segments: Vec<Segment>,
}

impl Input<'_> {
    fn point(&self, poly: usize, point: usize) -> Pt {
        self.polys.get(poly).and_then(|p| p.get(point)).copied().unwrap_or_default()
    }

    pub fn from(&self, s: usize) -> Pt {
        self.segments.get(s).map_or(Pt::default(), |s| self.point(s.poly, s.point))
    }

    pub fn to(&self, s: usize) -> Pt {
        self.segments.get(s).map_or(Pt::default(), |s| {
            let n = self.polys.get(s.poly).map_or(1, |p| p.len().max(1));
            self.point(s.poly, (s.point + 1) % n)
        })
    }

    /// `PolygonsPointIndex::prev`, `p` and `next` of a segment's start point, or with `end` of
    /// its end point (the next point of the polygon).
    pub fn corner(&self, s: usize, end: bool) -> (Pt, Pt, Pt) {
        let Some(seg) = self.segments.get(s) else { return Default::default() };
        let n = self.polys.get(seg.poly).map_or(1, |p| p.len().max(1));
        let at = (seg.point + usize::from(end)) % n;
        (self.point(seg.poly, (at + n - 1) % n), self.point(seg.poly, at), self.point(seg.poly, (at + 1) % n))
    }

    pub fn segment_count(&self) -> usize {
        self.segments.len()
    }
}

impl Diagram {
    pub fn edge(&self, e: usize) -> Option<&Edge> {
        self.edges.get(e)
    }

    pub fn v1(&self, e: usize) -> usize {
        self.edges.get(e).and_then(|e| self.edges.get(e.twin)).map_or(NONE, |t| t.v0)
    }

    pub fn is_infinite(&self, e: usize) -> bool {
        self.edges.get(e).is_none_or(|x| x.v0 == NONE) || self.v1(e) == NONE
    }

    /// `VoronoiUtils::to_point`: rounded to the nearest unit.
    pub fn to_point(&self, v: usize) -> Pt {
        self.vertices.get(v).map_or(Pt::default(), |v| Pt::new(v.x.round() as i64, v.y.round() as i64))
    }

    /// `is_in_range<coord_t>` of an edge's two ends.
    pub fn in_range(&self, e: usize) -> bool {
        let ok = |v: usize| self.vertices.get(v).is_some_and(|v| v.x.abs() < 9.2e18 && v.y.abs() < 9.2e18);
        self.edges.get(e).is_some_and(|x| ok(x.v0)) && ok(self.v1(e))
    }

    fn build(segments: &[[i64; 4]]) -> Option<Diagram> {
        let narrow: Option<Vec<[i32; 4]>> = segments
            .iter()
            .map(|s| {
                Some([
                    i32::try_from(s[0]).ok()?,
                    i32::try_from(s[1]).ok()?,
                    i32::try_from(s[2]).ok()?,
                    i32::try_from(s[3]).ok()?,
                ])
            })
            .collect();
        let narrow = narrow?;
        let d = catch_unwind(AssertUnwindSafe(|| {
            Builder::<i32>::default().with_segments(narrow.iter()).ok()?.build().ok()
        }))
        .ok()
        .flatten()?;
        let edge_index = |e: Option<boostvoronoi::prelude::EdgeIndex>| e.map_or(NONE, |e| e.usize());
        let mut out = Diagram::default();
        for c in d.cells() {
            let category = match c.source_category() {
                SourceCategory::SinglePoint => Category::SinglePoint,
                SourceCategory::SegmentStart => Category::SegmentStart,
                SourceCategory::SegmentEnd => Category::SegmentEnd,
                SourceCategory::Segment => Category::Segment,
            };
            out.cells.push(Cell {
                source: c.source_index().usize(),
                category,
                incident: edge_index(c.get_incident_edge()),
            });
        }
        for e in d.edges() {
            out.edges.push(Edge {
                cell: e.cell().map_or(NONE, |c| c.usize()),
                twin: e.twin().map_or(NONE, |t| t.usize()),
                next: e.next().map_or(NONE, |t| t.usize()),
                v0: e.vertex0().map_or(NONE, |v| v.usize()),
                primary: e.is_primary(),
                linear: e.is_linear(),
            });
        }
        for v in d.vertices() {
            out.vertices.push(Vertex { x: v.x(), y: v.y(), color: 0 });
        }
        Some(out)
    }

    /// `VoronoiDiagram::construct_voronoi` with repair: None when Boost could not build it.
    pub fn construct(input: &Input<'_>) -> Option<Diagram> {
        let segments: Vec<[i64; 4]> = (0..input.segment_count())
            .map(|s| {
                let (a, b) = (input.from(s), input.to(s));
                [a.x, a.y, b.x, b.y]
            })
            .collect();
        let d = Diagram::build(&segments)?;
        if d.known_issue(&segments).is_none() {
            return Some(d);
        }
        let mut last = d;
        for angle in [
            std::f64::consts::PI / 6.0,
            std::f64::consts::PI / 5.0,
            std::f64::consts::PI / 7.0,
            std::f64::consts::PI / 11.0,
        ] {
            let Some((fixed, issue)) = Diagram::rotated(&segments, angle) else { continue };
            last = fixed;
            if !issue {
                break;
            }
        }
        Some(last)
    }

    /// `try_to_repair_degenerated_voronoi_diagram_by_rotation`: built on the turned input, its
    /// vertices turned back, those on input ends put back exactly. Also whether the turned
    /// diagram still has a known issue.
    fn rotated(segments: &[[i64; 4]], angle: f64) -> Option<(Diagram, bool)> {
        let (sin, cos) = angle.sin_cos();
        let turn = |x: i64, y: i64| geo::rotated(Pt::new(x, y), cos, sin);
        let turned: Vec<[i64; 4]> = segments
            .iter()
            .map(|s| {
                let (a, b) = (turn(s[0], s[1]), turn(s[2], s[3]));
                [a.x, a.y, b.x, b.y]
            })
            .collect();
        let mut d = Diagram::build(&turned)?;
        let issue = d.known_issue(&turned).is_some();
        for c in 0..d.cells.len() {
            let Some(&cell) = d.cells.get(c) else { continue };
            if cell.is_degenerate() || !cell.contains_segment() {
                continue;
            }
            let Some(s) = turned.get(cell.source) else { continue };
            let range = d.segment_cell_range(&cell, Pt::new(s[0], s[1]), Pt::new(s[2], s[3]));
            if !range.valid() {
                continue;
            }
            let end_v1 = d.v1(range.end);
            if let Some(v) = d.vertices.get_mut(end_v1)
                && v.color == 0
            {
                v.color = (cell.source + 1) << 1;
            }
            let begin_v0 = d.edges.get(range.begin).map_or(NONE, |e| e.v0);
            if let Some(v) = d.vertices.get_mut(begin_v0)
                && v.color == 0
            {
                v.color = ((cell.source + 1) << 1) | 1;
            }
        }
        let (back_sin, back_cos) = (-angle).sin_cos();
        for v in &mut d.vertices {
            if v.color == 0 {
                let (x, y) = (v.x, v.y);
                v.x = back_cos * x - back_sin * y;
                v.y = back_cos * y + back_sin * x;
            } else if let Some(s) = segments.get((v.color >> 1).wrapping_sub(1)) {
                let (x, y) = if v.color & 1 == 1 { (s[2], s[3]) } else { (s[0], s[1]) };
                v.x = x as f64;
                v.y = y as f64;
            }
            v.color = 0;
        }
        Some((d, issue))
    }

    /// `detect_known_issues`, without the CGAL planarity test.
    fn known_issue(&self, segments: &[[i64; 4]]) -> Option<&'static str> {
        for (i, e) in self.edges.iter().enumerate() {
            if self.is_infinite(i) {
                continue;
            }
            let finite = |v: usize| self.vertices.get(v).is_some_and(|v| v.x.is_finite() && v.y.is_finite());
            if !finite(e.v0) || !finite(self.v1(i)) {
                return Some("finite edge with non-finite vertex");
            }
            let point_cell = |c: usize| self.cells.get(c).is_some_and(Cell::contains_point);
            let twin_cell = self.edges.get(e.twin).map_or(NONE, |t| t.cell);
            if !e.linear && !point_cell(e.cell) && !point_cell(twin_cell) {
                return Some("parabolic edge without focus");
            }
        }
        for cell in &self.cells {
            if cell.is_degenerate() || !cell.contains_segment() {
                continue;
            }
            let Some(s) = segments.get(cell.source) else { return Some("missing vertex") };
            let (from, to) = (Pt::new(s[0], s[1]), Pt::new(s[2], s[3]));
            let range = self.segment_cell_range(cell, from, to);
            if !range.valid() {
                return Some("missing vertex");
            }
            let vec = ((to.x - from.x) as f64, (to.y - from.y) as f64);
            let mut e = range.begin;
            let mut guard = 0;
            while e != range.end {
                if self.is_infinite(e) {
                    return Some("missing vertex");
                }
                let v1 = self.vertices.get(self.v1(e))?;
                let rel = (v1.x - from.x as f64, v1.y - from.y as f64);
                if vec.0 * rel.1 - vec.1 * rel.0 < 0.0 {
                    return Some("edge crossing the input");
                }
                e = self.edges.get(e).map_or(NONE, |x| x.next);
                guard += 1;
                if e == NONE || guard > self.edges.len() {
                    return Some("missing vertex");
                }
            }
        }
        None
    }

    /// `compute_segment_cell_range`.
    pub fn segment_cell_range(&self, cell: &Cell, from: Pt, to: Pt) -> Range {
        let mut range = Range { begin: NONE, end: NONE };
        let (mut seen_start, mut after_start, mut end_before_start) = (false, false, false);
        let mut e = cell.incident;
        let mut guard = 0;
        loop {
            if !self.is_infinite(e) {
                let v0 = self.edges.get(e).map_or(Pt::default(), |x| self.to_point(x.v0));
                let v1 = self.to_point(self.v1(e));
                if v0 == to && !after_start {
                    range.begin = e;
                    seen_start = true;
                } else if seen_start {
                    after_start = true;
                }
                if v1 == from && (range.end == NONE || end_before_start) {
                    end_before_start = !after_start;
                    range.end = e;
                }
            }
            e = self.edges.get(e).map_or(NONE, |x| x.next);
            guard += 1;
            if e == cell.incident || e == NONE || guard > self.edges.len() {
                break;
            }
        }
        range
    }

    /// `compute_point_cell_range`.
    pub fn point_cell_range(&self, cell: &Cell, input: &Input<'_>, source: Pt) -> Range {
        let mut range = Range { begin: NONE, end: NONE };
        let first = cell.incident;
        if self.is_infinite(first) || !self.in_range(first) {
            return range;
        }
        // The point's index: a segment's start, or the next one's for its end.
        let corner = match cell.category {
            Category::SegmentStart => input.corner(cell.source, false),
            Category::SegmentEnd => input.corner(cell.source, true),
            _ => return range,
        };
        let v0 = self.edges.get(first).map_or(Pt::default(), |x| self.to_point(x.v0));
        let v1 = self.to_point(self.v1(first));
        let query = if v0 == source { v1 } else { v0 };
        if !geo::inside_corner(corner.0, corner.1, corner.2, query) {
            return range;
        }
        let mut e = first;
        let mut guard = 0;
        loop {
            if self.to_point(self.v1(e)) == source {
                range.begin = self.edges.get(e).map_or(NONE, |x| x.next);
                range.end = e;
            }
            e = self.edges.get(e).map_or(NONE, |x| x.next);
            guard += 1;
            if e == first || e == NONE || guard > self.edges.len() {
                break;
            }
        }
        range
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Range {
    pub begin: usize,
    pub end: usize,
}

impl Range {
    pub fn valid(&self) -> bool {
        self.begin != NONE && self.end != NONE && self.begin != self.end
    }
}

/// `VoronoiUtils::get_source_point`.
pub fn source_point(cell: &Cell, input: &Input<'_>) -> Pt {
    match cell.category {
        Category::SegmentStart => input.from(cell.source),
        Category::SegmentEnd => input.to(cell.source),
        _ => Pt::default(),
    }
}

/// `VoronoiUtils::discretize_parabola`: the parabola between point `p` and segment `a`–`b`
/// from `start` to `end`, in steps, with marks where it turns past the transitioning angle.
pub fn discretize_parabola(p: Pt, a: Pt, b: Pt, start: Pt, end: Pt, step: i64, angle: f32) -> Vec<Pt> {
    let ab = geo::sub(b, a);
    let ab_size = geo::norm(ab);
    if ab_size == 0 || step <= 0 {
        return vec![start, end];
    }
    let sx = geo::dot(geo::sub(start, a), ab) / ab_size;
    let ex = geo::dot(geo::sub(end, a), ab) / ab_size;
    let sxex = ex - sx;
    let px = geo::dot(geo::sub(p, a), ab) / ab_size;
    let (_, pxx) = geo::infinite_sq(p, a, b);
    let ppxx = geo::sub(pxx, p);
    let d = geo::norm(ppxx);
    let rot = geo::perp(ppxx);
    let len = ((rot.x as f64).powi(2) + (rot.y as f64).powi(2)).sqrt();
    let (cos, sin) = if len > 0.0 { (rot.x as f64 / len, rot.y as f64 / len) } else { (rot.x as f64, rot.y as f64) };
    if d == 0 {
        return vec![start, end];
    }
    let bound = (f64::from(angle) * 0.5).atan();
    let mut msx = (-bound * d as f64) as i64;
    let mut mex = (bound * d as f64) as i64;
    let h = msx.wrapping_mul(msx) / (2 * d) + d / 2;
    let mut marking_start = geo::add(geo::rotated(Pt::new(msx, h), cos, sin), pxx);
    let mut marking_end = geo::add(geo::rotated(Pt::new(mex, h), cos, sin), pxx);
    let dir: i64 = if sx > ex { -1 } else { 1 };
    if dir < 0 {
        std::mem::swap(&mut marking_start, &mut marking_end);
        std::mem::swap(&mut msx, &mut mex);
    }
    let (lo, hi) = ((sx - px) * dir, (ex - px) * dir);
    let mut add_start = msx * dir > lo && msx * dir < hi;
    let mut add_end = mex * dir > lo && mex * dir < hi;
    let apex = geo::add(geo::rotated(Pt::new(0, d / 2), cos, sin), pxx);
    let mut add_apex = lo < 0 && hi > 0;
    let steps = ((ex - sx).abs() as f64 / step as f64).round() as i64;
    let mut out = vec![start];
    for k in 1..steps.min(1_000_000) {
        let x = sx + sxex * k / steps - px;
        let y = x.wrapping_mul(x) / (2 * d) + d / 2;
        if add_start && msx * dir < x * dir {
            out.push(marking_start);
            add_start = false;
        }
        if add_apex && x * dir > 0 {
            out.push(apex);
            add_apex = false;
        }
        if add_end && mex * dir < x * dir {
            out.push(marking_end);
            add_end = false;
        }
        out.push(geo::add(geo::rotated(Pt::new(x, y), cos, sin), pxx));
    }
    if add_apex {
        out.push(apex);
    }
    if add_end {
        out.push(marking_end);
    }
    out.push(end);
    out
}
