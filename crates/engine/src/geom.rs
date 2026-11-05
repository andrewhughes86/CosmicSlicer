//! Planar regions: shapes made of an outer boundary and holes, with offsets, booleans, line
//! clipping and point tests through the `i_overlay` engine.
//!
//! A [`Shape`] is its outer contour (counter-clockwise) followed by its holes (clockwise), with
//! no repeated closing point. Every third-party call runs under `catch_unwind`; a panic there
//! comes back as an empty result, never as a crash. The engine works in 32-bit integers,
//! scaled to each call's bounds (about 1e-7 mm on a 250 mm bed): twice as fast as 64-bit.

use std::panic::{AssertUnwindSafe, catch_unwind};

use glam::DVec2;
use i_overlay::core::fill_rule::FillRule;
use i_overlay::core::overlay_rule::OverlayRule;
use i_overlay::float::clip::FloatClip;
use i_overlay::float::single::SingleFloatOverlay;
use i_overlay::mesh::float::outline::offset::OutlineOffset;
use i_overlay::mesh::float::style::{LineJoin, OutlineStyle};
use i_overlay::string::clip::ClipRule;

/// One closed contour.
pub type Contour = Vec<DVec2>;
/// An outer contour and its holes.
pub type Shape = Vec<Contour>;
/// An open line.
pub type Polyline = Vec<DVec2>;

type Raw = Vec<Vec<Vec<[f64; 2]>>>;

fn to_raw(shapes: &[Shape]) -> Raw {
    shapes
        .iter()
        .map(|s| {
            s.iter()
                .filter(|c| c.len() >= 3 && c.iter().all(|p| p.is_finite()))
                .map(|c| c.iter().map(|p| [p.x, p.y]).collect())
                .collect::<Vec<_>>()
        })
        .filter(|s: &Vec<Vec<[f64; 2]>>| !s.is_empty())
        .collect()
}

fn from_raw(raw: Raw) -> Vec<Shape> {
    raw.into_iter()
        .map(|s| {
            s.into_iter()
                .map(|c| c.into_iter().map(|[x, y]| DVec2::new(x, y)).collect::<Contour>())
                .filter(|c| c.len() >= 3)
                .collect::<Shape>()
        })
        .filter(|s: &Shape| !s.is_empty())
        .collect()
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
/// split shapes or remove them.
pub fn offset(shapes: &[Shape], distance: f64, join: Join) -> Vec<Shape> {
    if !distance.is_finite() {
        return Vec::new();
    }
    if distance.abs() < 1.0e-9 {
        return shapes.to_vec();
    }
    let raw = to_raw(shapes);
    if raw.is_empty() {
        return Vec::new();
    }
    let join = match join {
        // Corners sharper than about 30° are cut, as Slic3r's miter limit does.
        Join::Miter => LineJoin::Miter(0.5),
        Join::Round => LineJoin::Round(0.25),
    };
    let style = OutlineStyle::new(distance).line_join(join);
    let out = catch_unwind(AssertUnwindSafe(|| raw.outline_as::<i32>(&style))).unwrap_or_default();
    from_raw(out)
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
    let (ra, rb) = (to_raw(a), to_raw(b));
    if ra.is_empty() {
        return if op == Op::Union { union(b) } else { Vec::new() };
    }
    if rb.is_empty() {
        return match op {
            Op::Intersect => Vec::new(),
            _ => union(a),
        };
    }
    let rule = match op {
        Op::Union => OverlayRule::Union,
        Op::Difference => OverlayRule::Difference,
        Op::Intersect => OverlayRule::Intersect,
    };
    let out = catch_unwind(AssertUnwindSafe(|| ra.overlay_as::<i32>(&rb, rule, FillRule::NonZero))).unwrap_or_default();
    from_raw(out)
}

/// The shapes merged where they overlap.
pub fn union(shapes: &[Shape]) -> Vec<Shape> {
    let raw = to_raw(shapes);
    if raw.is_empty() {
        return Vec::new();
    }
    let empty: Raw = Vec::new();
    let out = catch_unwind(AssertUnwindSafe(|| raw.overlay_as::<i32>(&empty, OverlayRule::Subject, FillRule::NonZero)))
        .unwrap_or_default();
    from_raw(out)
}

/// Shapes from closed loops in any orientation, filled where they wind (a mesh's slice).
pub fn union_loops(loops: &[Contour]) -> Vec<Shape> {
    let raw: Vec<Vec<[f64; 2]>> = loops
        .iter()
        .filter(|p| p.len() >= 3 && p.iter().all(|q| q.is_finite()) && signed_area2(p).abs() > 1.0e-10)
        .map(|p| p.iter().map(|q| [q.x, q.y]).collect())
        .collect();
    if raw.is_empty() {
        return Vec::new();
    }
    let empty: Vec<Vec<[f64; 2]>> = Vec::new();
    let out = catch_unwind(AssertUnwindSafe(|| raw.overlay_as::<i32>(&empty, OverlayRule::Subject, FillRule::NonZero)))
        .unwrap_or_default();
    from_raw(out)
}

/// The parts of `lines` that lie inside `shapes`.
pub fn clip_lines(lines: &[Polyline], shapes: &[Shape]) -> Vec<Polyline> {
    clip(lines, shapes, false)
}

/// The parts of `lines` outside `shapes`.
pub fn clip_lines_outside(lines: &[Polyline], shapes: &[Shape]) -> Vec<Polyline> {
    clip(lines, shapes, true)
}

fn clip(lines: &[Polyline], shapes: &[Shape], invert: bool) -> Vec<Polyline> {
    let paths: Vec<Vec<[f64; 2]>> = lines
        .iter()
        .filter(|l| l.len() >= 2 && l.iter().all(|p| p.is_finite()))
        .map(|l| l.iter().map(|p| [p.x, p.y]).collect())
        .collect();
    let raw = to_raw(shapes);
    if paths.is_empty() || raw.is_empty() {
        return Vec::new();
    }
    let rule = ClipRule { invert, boundary_included: false };
    let out =
        catch_unwind(AssertUnwindSafe(|| paths.clip_by_as::<i32>(&raw, FillRule::NonZero, rule))).unwrap_or_default();
    out.into_iter()
        .map(|l| l.into_iter().map(|[x, y]| DVec2::new(x, y)).collect::<Polyline>())
        .filter(|l| l.len() >= 2)
        .collect()
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
