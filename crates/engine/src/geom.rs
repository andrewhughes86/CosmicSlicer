//! Planar regions: shapes made of an outer boundary and holes, with offsets, booleans, line
//! clipping and point tests, through OrcaSlicer's Clipper ([`crate::clipper`]) and its
//! `ClipperUtils` wrappers, so regions come out with the same vertices in the same order.
//!
//! A [`Shape`] is its outer contour (counter-clockwise) followed by its holes (clockwise), with
//! no repeated closing point. Millimetres go in and out; Clipper works in Orca's 1 nm integers.
//! Everything runs under `catch_unwind`, and a failed call comes back empty, never as a crash.

use std::panic::{AssertUnwindSafe, catch_unwind};

use glam::DVec2;

use crate::clipper::{
    self, ClipType, Clipper, ClipperOffset, EndType, Failed, FillType, JoinType, Path, Paths, PolyTree, PolyType, Pt,
};

/// One closed contour.
pub type Contour = Vec<DVec2>;
/// An outer contour and its holes.
pub type Shape = Vec<Contour>;
/// An open line.
pub type Polyline = Vec<DVec2>;

/// Orca's `SCALING_FACTOR`: units per millimetre.
const SCALE: f64 = 1.0e6;
/// Orca's `SCALED_RESOLUTION`: how finely round joins are stepped, in Clipper's units.
const ROUND_TOLERANCE: f64 = 0.0125 * SCALE;
/// `ClipperSafetyOffset`.
const CLIPPER_SAFETY_OFFSET: f32 = 10.0;
/// `ClipperOffsetShortestEdgeFactor`.
const SHORTEST_EDGE_FACTOR: f64 = 0.005;
/// Past this, a coordinate is garbage (a thousand kilometres).
const FAR: f64 = 1.0e9;

fn usable(p: DVec2) -> bool {
    p.is_finite() && p.x.abs() < FAR && p.y.abs() < FAR
}

fn to_pt(p: DVec2) -> Pt {
    Pt::new((p.x * SCALE).round() as i64, (p.y * SCALE).round() as i64)
}

fn to_path(c: &[DVec2]) -> Path {
    c.iter().map(|p| to_pt(*p)).collect()
}

fn from_path(c: &[Pt]) -> Contour {
    c.iter().map(|p| DVec2::new(p.x as f64 / SCALE, p.y as f64 / SCALE)).collect()
}

/// The usable contours, each shape's outer first, as Orca's `ExPolygonsProvider` gives them.
fn to_paths(shapes: &[Shape]) -> Paths {
    shapes.iter().flatten().filter(|c| c.len() >= 3 && c.iter().all(|p| usable(*p))).map(|c| to_path(c)).collect()
}

/// `PolyTreeToExPolygons`: each outer with its holes, then what sits inside those holes.
fn to_shapes(tree: &PolyTree) -> Vec<Shape> {
    let mut out = Vec::new();
    let mut stack: Vec<usize> = tree.children.iter().rev().copied().collect();
    while let Some(n) = stack.pop() {
        let Some(node) = tree.nodes.get(n) else { continue };
        let mut shape = vec![from_path(&node.contour)];
        let mut nested = Vec::new();
        for hole in node.children.iter().filter_map(|h| tree.nodes.get(*h)) {
            shape.push(from_path(&hole.contour));
            nested.extend(hole.children.iter().copied());
        }
        out.push(shape);
        stack.extend(nested.into_iter().rev());
    }
    out
}

/// Runs Clipper, turning a failure or a panic into an empty result.
fn guarded<T: Default>(run: impl FnOnce() -> Result<T, Failed>) -> T {
    catch_unwind(AssertUnwindSafe(|| run().unwrap_or_default())).unwrap_or_default()
}

/// `clipper_do` into plain paths.
fn clip_paths(op: ClipType, subject: &[Path], clip: &[Path], fill: FillType) -> Result<Paths, Failed> {
    let mut c = Clipper::new();
    c.add_paths(subject, PolyType::Subject, true)?;
    c.add_paths(clip, PolyType::Clip, true)?;
    c.execute(op, fill, fill)
}

fn union_tree(paths: &[Path], fill: FillType) -> Result<PolyTree, Failed> {
    let mut c = Clipper::new();
    c.add_paths(paths, PolyType::Subject, true)?;
    c.execute_tree(ClipType::Union, fill, fill)
}

/// `clipper_do_polytree`: the operation into paths, then a union to find which loops are holes.
fn clip_tree(op: ClipType, subject: &[Path], clip: &[Path], fill: FillType) -> Result<PolyTree, Failed> {
    let out = clip_paths(op, subject, clip, fill)?;
    if out.is_empty() { Ok(PolyTree::default()) } else { union_tree(&out, fill) }
}

fn offsetter(join: JoinType, limit: f64, delta: f32) -> ClipperOffset {
    let mut co = ClipperOffset::new();
    if join == JoinType::Round {
        co.arc_tolerance = limit;
    } else {
        co.miter_limit = limit;
    }
    co.shortest_edge = (f64::from(delta) * SHORTEST_EDGE_FACTOR).abs();
    co
}

/// `raw_offset`: each path alone, holes shrinking when contours grow; not united.
fn raw_offset(paths: &[Path], delta: f32, join: JoinType, limit: f64) -> Result<Paths, Failed> {
    raw_offset_end(paths, delta, join, limit, EndType::ClosedPolygon)
}

fn raw_offset_end(paths: &[Path], delta: f32, join: JoinType, limit: f64, end: EndType) -> Result<Paths, Failed> {
    let mut out = Vec::with_capacity(paths.len());
    for path in paths {
        let mut co = offsetter(join, limit, delta);
        co.add_path(path, join, end);
        let ccw = end != EndType::ClosedPolygon || clipper::orientation(path);
        let mut this = co.execute(f64::from(if ccw { delta } else { -delta }))?;
        if !ccw {
            this.iter_mut().for_each(|p| p.reverse());
        }
        out.append(&mut this);
    }
    Ok(out)
}

/// `offset_expolygon_inner`; whether anything of the shape survived.
fn offset_shape(shape: &[Path], delta: f32, join: JoinType, limit: f64, out: &mut Paths) -> Result<bool, Failed> {
    let Some((contour, holes)) = shape.split_first() else { return Ok(false) };
    let mut co = offsetter(join, limit, delta);
    co.add_path(contour, join, EndType::ClosedPolygon);
    let mut contours = co.execute(f64::from(delta))?;
    if contours.is_empty() {
        return Ok(false);
    }
    let mut shifted = Vec::new();
    for hole in holes {
        let mut co = offsetter(join, limit, delta);
        co.add_path(hole, join, EndType::ClosedPolygon);
        shifted.append(&mut co.execute(f64::from(-delta))?);
    }
    if shifted.is_empty() {
        out.append(&mut contours);
    } else if delta < 0.0 {
        let mut left = clip_paths(ClipType::Difference, &contours, &shifted, FillType::NonZero)?;
        if left.is_empty() {
            return Ok(false);
        }
        out.append(&mut left);
    } else {
        out.append(&mut contours);
        shifted.iter_mut().for_each(|p| p.reverse());
        out.append(&mut shifted);
    }
    Ok(true)
}

/// `expolygons_offset_raw`: the shapes one at a time, and how many survived.
fn offset_shapes_raw(shapes: &[Shape], delta: f32, join: JoinType, limit: f64) -> Result<(Paths, usize), Failed> {
    let mut out = Vec::new();
    let mut kept = 0;
    for shape in shapes {
        let paths: Paths = shape.iter().filter(|c| c.iter().all(|p| usable(*p))).map(|c| to_path(c)).collect();
        kept += usize::from(offset_shape(&paths, delta, join, limit, &mut out)?);
    }
    Ok((out, kept))
}

/// `expand_paths`.
fn expand_tree(paths: &[Path], delta: f32, join: JoinType, limit: f64) -> Result<PolyTree, Failed> {
    union_tree(&raw_offset(paths, delta, join, limit)?, FillType::NonZero)
}

/// `shrink_paths`.
fn shrink_tree(paths: &[Path], delta: f32, join: JoinType, limit: f64) -> Result<PolyTree, Failed> {
    let raw = raw_offset(paths, -delta, join, limit)?;
    if raw.is_empty() {
        return Ok(PolyTree::default());
    }
    let mut c = Clipper::new();
    c.add_paths(&raw, PolyType::Subject, true)?;
    let rect = clipper::outer_rect(c.bounds());
    c.add_path(&rect, PolyType::Subject, true)?;
    c.reverse_solution(true);
    let mut tree = c.execute_tree(ClipType::Union, FillType::Negative, FillType::Negative)?;
    tree.remove_outermost();
    Ok(tree)
}

/// `offset_paths`.
fn offset_tree(paths: &[Path], delta: f32, join: JoinType, limit: f64) -> Result<PolyTree, Failed> {
    if delta > 0.0 { expand_tree(paths, delta, join, limit) } else { shrink_tree(paths, -delta, join, limit) }
}

fn scaled(distance: f64) -> f32 {
    (distance / 1.0e-6) as f32
}

fn clipper_join(join: Join) -> (JoinType, f64) {
    match join {
        Join::Miter => (JoinType::Miter, 3.0),
        Join::Round => (JoinType::Round, ROUND_TOLERANCE),
    }
}

/// Twice the signed area of a loop: positive when counter-clockwise.
pub fn signed_area2(pts: &[DVec2]) -> f64 {
    let n = pts.len();
    (0..n)
        .map(|i| {
            let (a, b) = (pts.get(i).copied().unwrap_or_default(), pts.get((i + 1) % n).copied().unwrap_or_default());
            a.perp_dot(b)
        })
        .sum()
}

/// An axis-aligned rectangle as a shape.
pub fn rectangle(min: DVec2, max: DVec2) -> Shape {
    vec![vec![min, DVec2::new(max.x, min.y), max, DVec2::new(min.x, max.y)]]
}

/// How offset corners are joined.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Join {
    /// Sharp corners (walls).
    Miter,
    /// Rounded corners (skirts, brims, supports).
    Round,
}

/// Offsets shapes by `distance`: outwards when positive, inwards when negative. Insetting can
/// split shapes or remove them. Orca's `offset_ex`.
pub fn offset(shapes: &[Shape], distance: f64, join: Join) -> Vec<Shape> {
    if !distance.is_finite() {
        return Vec::new();
    }
    offset_nm(shapes, scaled(distance), join)
}

/// `offset_ex` with Orca's own float delta, in Clipper's units.
pub fn offset_nm(shapes: &[Shape], delta: f32, join: Join) -> Vec<Shape> {
    let (join, limit) = clipper_join(join);
    guarded(|| {
        let (out, _) = offset_shapes_raw(shapes, delta, join, limit)?;
        Ok(to_shapes(&union_tree(&out, FillType::NonZero)?))
    })
}

/// Offsets twice, the second time over the first's loops all together: Orca's `offset2_ex`.
pub fn offset2(shapes: &[Shape], first: f64, second: f64, join: Join) -> Vec<Shape> {
    if !first.is_finite() || !second.is_finite() {
        return Vec::new();
    }
    offset2_nm(shapes, scaled(first), scaled(second), join)
}

/// `offset2_ex` with Orca's own float deltas, in Clipper's units.
pub fn offset2_nm(shapes: &[Shape], first: f32, second: f32, join: Join) -> Vec<Shape> {
    let (join, limit) = clipper_join(join);
    guarded(|| {
        let out = offset_shapes(shapes, first, join, limit)?;
        Ok(to_shapes(&offset_tree(&out, second, join, limit)?))
    })
}

/// `expolygons_offset`: united only when something outwards might now overlap.
fn offset_shapes(shapes: &[Shape], delta: f32, join: JoinType, limit: f64) -> Result<Paths, Failed> {
    let (out, kept) = offset_shapes_raw(shapes, delta, join, limit)?;
    if kept > 1 && delta > 0.0 { clip_paths(ClipType::Union, &out, &[], FillType::NonZero) } else { Ok(out) }
}

/// Orca's `offset` of expolygons, to plain loops, in Clipper's units.
pub fn offset_loops(shapes: &[Shape], delta: f32) -> Paths {
    guarded(|| offset_shapes(shapes, delta, JoinType::Miter, 3.0))
}

/// Orca's `offset` of one expolygon (contour first), in Clipper's units.
pub fn offset_shape_loops(shape: &[Path], delta: f32) -> Paths {
    guarded(|| {
        let mut out = Vec::new();
        offset_shape(shape, delta, JoinType::Miter, 3.0, &mut out)?;
        Ok(out)
    })
}

/// Orca's `shrink` of polygons, by a positive `delta`, in Clipper's units.
pub fn shrink_polygons(paths: &[Path], delta: f32) -> Paths {
    guarded(|| {
        let raw = raw_offset(paths, -delta, JoinType::Miter, 3.0)?;
        if raw.is_empty() {
            return Ok(Vec::new());
        }
        let mut c = Clipper::new();
        c.add_paths(&raw, PolyType::Subject, true)?;
        let rect = clipper::outer_rect(c.bounds());
        c.add_path(&rect, PolyType::Subject, true)?;
        c.reverse_solution(true);
        let mut out = c.execute(ClipType::Union, FillType::Negative, FillType::Negative)?;
        if !out.is_empty() {
            out.remove(0);
        }
        Ok(out)
    })
}

/// Orca's `offset` of polygons, to plain loops, in Clipper's units.
pub fn offset_polygons(paths: &[Path], delta: f32) -> Paths {
    guarded(|| {
        let raw = raw_offset(paths, delta, JoinType::Miter, 3.0)?;
        clip_paths(ClipType::Union, &raw, &[], FillType::NonZero)
    })
}

/// A line's outline: Orca's `offset` of a polyline, square joins and butt ends.
pub fn offset_line(line: &[DVec2], delta: f32) -> Paths {
    let path = [to_path(line)];
    guarded(|| raw_offset_end(&path, delta, JoinType::Square, 0.0, EndType::OpenButt))
}

/// `diff_ex`; `safety` is its `ApplySafetyOffset`, growing `clip` by 10 units first.
pub fn diff(subject: &[Path], clip: &[Path], safety: bool) -> Vec<Shape> {
    guarded(|| {
        let grown;
        let clip = if safety {
            grown = raw_offset(clip, CLIPPER_SAFETY_OFFSET, JoinType::Miter, 3.0)?;
            grown.as_slice()
        } else {
            clip
        };
        Ok(to_shapes(&clip_tree(ClipType::Difference, subject, clip, FillType::NonZero)?))
    })
}

/// `union_ex` of plain loops.
pub fn union_paths(paths: &[Path]) -> Vec<Shape> {
    guarded(|| Ok(to_shapes(&clip_tree(ClipType::Union, paths, &[], FillType::NonZero)?)))
}

/// All the shapes' contours in Clipper's units, outers before their holes.
pub fn shape_paths(shapes: &[Shape]) -> Paths {
    to_paths(shapes)
}

/// Orca's `_douglas_peucker`, in Clipper's units.
pub fn douglas_peucker(pts: &[Pt], tolerance: f64) -> Path {
    let squared = tolerance * tolerance;
    let at = |i: usize| pts.get(i).copied().unwrap_or_default();
    let distance = |p: Pt, a: Pt, b: Pt| {
        let v = ((b.x - a.x) as f64, (b.y - a.y) as f64);
        let va = ((p.x - a.x) as f64, (p.y - a.y) as f64);
        let l2 = v.0 * v.0 + v.1 * v.1;
        let square = |x: f64, y: f64| x * x + y * y;
        if l2 == 0.0 {
            return square(va.0, va.1);
        }
        let t = (va.0 * v.0 + va.1 * v.1) / l2;
        if t <= 0.0 {
            square(va.0, va.1)
        } else if t >= 1.0 {
            square((p.x - b.x) as f64, (p.y - b.y) as f64)
        } else {
            square(t * v.0 - va.0, t * v.1 - va.1)
        }
    };
    let Some(last) = pts.len().checked_sub(1) else { return Vec::new() };
    let (mut anchor, mut floater) = (0, last);
    let mut out = vec![at(0)];
    if anchor == floater {
        return out;
    }
    let mut stack = vec![floater];
    loop {
        let mut furthest = (0.0, anchor);
        for i in anchor + 1..floater {
            let d = distance(at(i), at(anchor), at(floater));
            if d > furthest.0 {
                furthest = (d, i);
            }
        }
        if furthest.0 <= squared {
            out.push(at(floater));
            anchor = floater;
            stack.pop();
            let Some(&next) = stack.last() else { break };
            floater = next;
        } else {
            floater = furthest.1;
            stack.push(floater);
        }
    }
    out
}

/// `ExPolygon::simplify_p`: each contour simplified, then Clipper's strictly simple union.
pub fn simplify_p(shape: &Shape, tolerance: f64) -> Paths {
    let pp: Paths = paths_of(shape)
        .into_iter()
        .map(|c| {
            let mut closed = c;
            if let Some(&first) = closed.first() {
                closed.push(first);
            }
            let mut p = douglas_peucker(&closed, tolerance);
            p.pop();
            p
        })
        .collect();
    guarded(|| {
        let mut c = Clipper::new();
        c.strictly_simple(true);
        c.add_paths(&pp, PolyType::Subject, true)?;
        c.execute(ClipType::Union, FillType::NonZero, FillType::NonZero)
    })
}

/// Grows then shrinks: Orca's `closing_ex`, with both distances positive.
pub fn closing(shapes: &[Shape], grow: f64, shrink: f64, join: Join) -> Vec<Shape> {
    if !grow.is_finite() || !shrink.is_finite() {
        return Vec::new();
    }
    let (join, limit) = clipper_join(join);
    guarded(|| {
        let grown = raw_offset(&to_paths(shapes), scaled(grow), join, limit)?;
        let mut c = Clipper::new();
        c.add_paths(&grown, PolyType::Subject, true)?;
        let united = c.execute(ClipType::Union, FillType::NonZero, FillType::NonZero)?;
        Ok(to_shapes(&shrink_tree(&united, scaled(shrink), join, limit)?))
    })
}

/// How two sets of shapes combine.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op {
    Union,
    Difference,
    Intersect,
}

/// `a` combined with `b`.
pub fn boolean(a: &[Shape], b: &[Shape], op: Op) -> Vec<Shape> {
    let kind = match op {
        Op::Union => ClipType::Union,
        Op::Difference => ClipType::Difference,
        Op::Intersect => ClipType::Intersection,
    };
    let (pa, pb) = (to_paths(a), to_paths(b));
    guarded(|| Ok(to_shapes(&clip_tree(kind, &pa, &pb, FillType::NonZero)?)))
}

/// The shapes merged where they overlap.
pub fn union(shapes: &[Shape]) -> Vec<Shape> {
    let paths = to_paths(shapes);
    guarded(|| Ok(to_shapes(&clip_tree(ClipType::Union, &paths, &[], FillType::NonZero)?)))
}

/// Shapes from closed loops in any orientation, filled where they wind (a mesh's slice).
pub fn union_loops(loops: &[Contour]) -> Vec<Shape> {
    let paths: Paths = loops
        .iter()
        .filter(|p| p.len() >= 3 && p.iter().all(|q| usable(*q)) && signed_area2(p).abs() > 1.0e-10)
        .map(|p| to_path(p))
        .collect();
    guarded(|| Ok(to_shapes(&clip_tree(ClipType::Union, &paths, &[], FillType::NonZero)?)))
}

/// The parts of `lines` that lie inside `shapes`.
pub fn clip_lines(lines: &[Polyline], shapes: &[Shape]) -> Vec<Polyline> {
    clip(lines, shapes, ClipType::Intersection)
}

/// The parts of `lines` outside `shapes`.
pub fn clip_lines_outside(lines: &[Polyline], shapes: &[Shape]) -> Vec<Polyline> {
    clip(lines, shapes, ClipType::Difference)
}

/// Orca's `_clipper_pl_open`. As there, a line may come back reversed.
fn clip(lines: &[Polyline], shapes: &[Shape], op: ClipType) -> Vec<Polyline> {
    let paths: Paths =
        lines.iter().filter(|l| l.len() >= 2 && l.iter().all(|p| usable(*p))).map(|l| to_path(l)).collect();
    clip_open(&paths, &to_paths(shapes), op).iter().map(|l| from_path(l)).collect()
}

/// Open paths clipped by closed ones, in Clipper's units and order.
pub fn clip_open(lines: &[Path], clip: &[Path], op: ClipType) -> Paths {
    if lines.is_empty() {
        return Vec::new();
    }
    let tree = guarded(|| {
        let mut c = Clipper::new();
        c.add_paths(lines, PolyType::Subject, false)?;
        c.add_paths(clip, PolyType::Clip, true)?;
        c.execute_tree(op, FillType::NonZero, FillType::NonZero)
    });
    tree.children
        .iter()
        .filter_map(|n| tree.nodes.get(*n))
        .filter(|n| n.open && n.contour.len() >= 2)
        .map(|n| n.contour.clone())
        .collect()
}

/// Shapes back from plain loops of Clipper's units, which must be outers and holes in order.
pub fn loops_to_contours(paths: &[Path]) -> Vec<Contour> {
    paths.iter().map(|p| from_path(p)).collect()
}

/// A shape's contours in Clipper's units, outer first.
pub fn paths_of(shape: &Shape) -> Paths {
    to_paths(std::slice::from_ref(shape))
}

/// A line in Clipper's units.
pub fn to_nm(line: &[DVec2]) -> Path {
    to_path(line)
}

/// A line back from Clipper's units.
pub fn from_nm(line: &[Pt]) -> Polyline {
    from_path(line)
}

/// The area the shapes cover.
pub fn area(shapes: &[Shape]) -> f64 {
    shapes.iter().flatten().map(|c| signed_area2(c) / 2.0).sum()
}

pub fn shape_area(shape: &Shape) -> f64 {
    shape.iter().map(|c| signed_area2(c) / 2.0).sum()
}

/// Drops shapes smaller than `min_area` (slivers left by offsets and booleans).
pub fn without_slivers(shapes: Vec<Shape>, min_area: f64) -> Vec<Shape> {
    shapes.into_iter().filter(|s| shape_area(s) > min_area).collect()
}

fn in_contour(c: &[DVec2], p: DVec2) -> bool {
    let n = c.len();
    let mut inside = false;
    for i in 0..n {
        let (Some(a), Some(b)) = (c.get(i), c.get((i + n - 1) % n)) else { continue };
        if (a.y > p.y) != (b.y > p.y) && p.x < (b.x - a.x) * (p.y - a.y) / (b.y - a.y) + a.x {
            inside = !inside;
        }
    }
    inside
}

/// Whether `p` is inside the shapes (inside an outer contour and outside its holes).
pub fn contains(shapes: &[Shape], p: DVec2) -> bool {
    shapes.iter().any(|s| {
        let mut it = s.iter();
        it.next().is_some_and(|outer| in_contour(outer, p)) && !it.any(|hole| in_contour(hole, p))
    })
}

/// Bounds of the shapes as (min, max); `None` when empty.
pub fn bounds(shapes: &[Shape]) -> Option<(DVec2, DVec2)> {
    let mut lo = DVec2::splat(f64::INFINITY);
    let mut hi = DVec2::splat(f64::NEG_INFINITY);
    for p in shapes.iter().flatten().flatten() {
        lo = lo.min(*p);
        hi = hi.max(*p);
    }
    (lo.is_finite() && hi.is_finite()).then_some((lo, hi))
}

/// Parallel lines across the bounds of `shapes`, `spacing` apart at `angle` (radians from +X),
/// shifted by `phase` along their normal; not yet clipped.
pub fn hatch_lines(shapes: &[Shape], spacing: f64, angle: f64, phase: f64, max_lines: usize) -> Vec<Polyline> {
    let Some((lo, hi)) = bounds(shapes) else { return Vec::new() };
    if !(spacing.is_finite() && spacing > 1.0e-3) {
        return Vec::new();
    }
    let centre = (lo + hi) / 2.0;
    let radius = (hi - lo).length() / 2.0 + spacing;
    let dir = DVec2::new(angle.cos(), angle.sin());
    let normal = dir.perp();
    // Lines sit on a grid fixed to the plate, so they line up from layer to layer.
    let first = ((centre.dot(normal) - radius - phase) / spacing).floor();
    let count = ((2.0 * radius / spacing).ceil() as usize + 2).min(max_lines);
    (0..count)
        .map(|i| {
            let d = (first + i as f64) * spacing + phase;
            let foot = normal * d + dir * (centre.dot(dir));
            vec![foot - dir * radius, foot + dir * radius]
        })
        .collect()
}

/// The convex hull of `points`, counter-clockwise (Andrew's monotone chain).
pub fn convex_hull(points: &[DVec2]) -> Contour {
    let mut pts: Vec<DVec2> = points.iter().copied().filter(|p| p.is_finite()).collect();
    pts.sort_by(|a, b| a.x.total_cmp(&b.x).then(a.y.total_cmp(&b.y)));
    pts.dedup();
    if pts.len() < 3 {
        return pts;
    }
    let cross = |o: DVec2, a: DVec2, b: DVec2| (a - o).perp_dot(b - o);
    let mut lower: Vec<DVec2> = Vec::new();
    for &p in &pts {
        while lower.len() >= 2 && cross(lower[lower.len() - 2], lower[lower.len() - 1], p) <= 0.0 {
            lower.pop();
        }
        lower.push(p);
    }
    let mut upper: Vec<DVec2> = Vec::new();
    for &p in pts.iter().rev() {
        while upper.len() >= 2 && cross(upper[upper.len() - 2], upper[upper.len() - 1], p) <= 0.0 {
            upper.pop();
        }
        upper.push(p);
    }
    lower.pop();
    upper.pop();
    lower.extend(upper);
    lower
}

/// Length of an open line.
pub fn length(line: &[DVec2]) -> f64 {
    line.windows(2)
        .map(|w| w.first().copied().unwrap_or_default().distance(w.get(1).copied().unwrap_or_default()))
        .sum()
}

/// Drops points closer than `tolerance` to the line through their neighbours
/// (Douglas–Peucker), keeping the ends.
pub fn simplify(line: &[DVec2], tolerance: f64) -> Polyline {
    if line.len() < 3 {
        return line.to_vec();
    }
    let mut keep = vec![false; line.len()];
    keep[0] = true;
    keep[line.len() - 1] = true;
    let mut stack = vec![(0usize, line.len() - 1)];
    while let Some((a, b)) = stack.pop() {
        let (Some(&pa), Some(&pb)) = (line.get(a), line.get(b)) else { continue };
        let mut worst = (0.0f64, 0usize);
        for (i, p) in line.iter().enumerate().take(b).skip(a + 1) {
            let d = point_segment_distance(*p, pa, pb);
            if d > worst.0 {
                worst = (d, i);
            }
        }
        if worst.0 > tolerance {
            if let Some(k) = keep.get_mut(worst.1) {
                *k = true;
            }
            stack.push((a, worst.1));
            stack.push((worst.1, b));
        }
    }
    line.iter().zip(keep).filter(|(_, k)| *k).map(|(p, _)| *p).collect()
}

/// A closed loop simplified the same way.
pub fn simplify_loop(contour: &[DVec2], tolerance: f64) -> Contour {
    if contour.len() < 4 {
        return contour.to_vec();
    }
    let mut open = contour.to_vec();
    if let Some(&first) = contour.first() {
        open.push(first);
    }
    let mut out = simplify(&open, tolerance);
    out.pop();
    if out.len() >= 3 { out } else { contour.to_vec() }
}

pub fn point_segment_distance(p: DVec2, a: DVec2, b: DVec2) -> f64 {
    let ab = b - a;
    let t = if ab.length_squared() > 0.0 { ((p - a).dot(ab) / ab.length_squared()).clamp(0.0, 1.0) } else { 0.0 };
    (a + ab * t).distance(p)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn square(c: DVec2, h: f64) -> Shape {
        rectangle(c - DVec2::splat(h), c + DVec2::splat(h))
    }

    #[test]
    fn booleans_and_offsets_keep_islands() {
        let ring = boolean(&[square(DVec2::ZERO, 50.0)], &[square(DVec2::ZERO, 10.0)], Op::Difference);
        assert_eq!(ring.len(), 1);
        assert_eq!(ring[0].len(), 2, "outer and hole");
        assert!((area(&ring) - (10_000.0 - 400.0)).abs() < 1.0e-6);
        let inset = offset(&ring, -5.0, Join::Miter);
        assert!(contains(&inset, DVec2::new(40.0, 0.0)));
        assert!(!contains(&inset, DVec2::new(47.0, 0.0)));
        assert!(!contains(&inset, DVec2::new(13.0, 0.0)));
        // Miter joins keep square corners square.
        assert!((area(&offset(&[square(DVec2::ZERO, 10.0)], -1.0, Join::Miter)) - 324.0).abs() < 1.0e-3);
        assert!(offset(&ring, -30.0, Join::Miter).is_empty());
    }

    #[test]
    fn lines_clip_to_regions() {
        let ring = boolean(&[square(DVec2::ZERO, 50.0)], &[square(DVec2::ZERO, 10.0)], Op::Difference);
        let lines = hatch_lines(&ring, 10.0, 0.0, 0.0, 1000);
        let clipped = clip_lines(&lines, &ring);
        let total: f64 = clipped.iter().map(|l| length(l)).sum();
        // Rows at y = -40..40 every 10: 9 rows of 100, less the hole (20) on the rows inside it.
        assert!(total > 800.0 && total < 900.0, "{total}");
        assert!(clipped.iter().flatten().all(|p| p.x.abs() <= 50.0 + 1e-6));
    }

    #[test]
    fn simplify_keeps_corners() {
        let line = vec![DVec2::ZERO, DVec2::new(1.0, 0.001), DVec2::new(2.0, 0.0), DVec2::new(2.0, 2.0)];
        assert_eq!(simplify(&line, 0.01).len(), 3);
    }

    #[test]
    fn hostile_input_gives_nothing() {
        let bad: Shape = vec![vec![DVec2::NAN, DVec2::X, DVec2::Y]];
        assert!(offset(std::slice::from_ref(&bad), 1.0, Join::Round).is_empty());
        assert!(boolean(std::slice::from_ref(&bad), std::slice::from_ref(&bad), Op::Union).is_empty());
        assert!(offset(&[square(DVec2::ZERO, 1.0)], f64::INFINITY, Join::Miter).is_empty());
        assert!(hatch_lines(&[square(DVec2::ZERO, 1.0)], 0.0, 0.0, 0.0, 10).is_empty());
    }
}
