//! Arc fitting: runs of points on a circle become one G2/G3 move (OrcaSlicer's "Arc
//! fitting"), so curves print smoothly and the G-code is smaller.

use glam::DVec2;

use crate::clipper::Pt;

/// One move of a fitted path.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Piece {
    Line(DVec2),
    /// To `end` round `center`, counter-clockwise when `ccw` (G3) else clockwise (G2).
    Arc {
        end: DVec2,
        center: DVec2,
        ccw: bool,
    },
}

/// OrcaSlicer's numbers, in its nanometre units.
const SCALE: f64 = 1e6;
const MAX_RADIUS: f64 = 2000.0 * SCALE;
const PARALLEL_AREA: f64 = 1e-4 * SCALE * SCALE;
const SCALED_EPSILON: f64 = 1e-4 * SCALE;
const ZERO_TOLERANCE: f64 = 0.000005;
const LENGTH_TOLERANCE: f64 = 0.05;
const EPSILON: f64 = 1e-4;
/// Past this many points a run closes, so a huge smooth curve stays quick to fit.
const MAX_POINTS: usize = 4000;
use std::f64::consts::PI;

#[derive(Clone, Copy)]
struct Circle {
    center: DVec2,
    radius: f64,
}

#[derive(Clone, Copy)]
struct Arc {
    circle: Circle,
    ccw: bool,
    start_theta: f64,
    end_theta: f64,
}

fn through(a: DVec2, b: DVec2, c: DVec2) -> Option<Circle> {
    if ((a.y - b.y) * (a.x - c.x) - (a.y - c.y) * (a.x - b.x)).abs() <= PARALLEL_AREA {
        return None;
    }
    let d = a.x * (b.y - c.y) - a.y * (b.x - c.x) + b.x * c.y - c.x * b.y;
    if d.abs() < SCALED_EPSILON {
        return None;
    }
    let (a2, b2, c2) = (a.length_squared(), b.length_squared(), c.length_squared());
    let e = a2 * (c.y - b.y) + b2 * (a.y - c.y) + c2 * (b.y - a.y);
    let f = a2 * (b.x - c.x) + b2 * (c.x - a.x) + c2 * (a.x - b.x);
    let centre = DVec2::new(-e / (2.0 * d), -f / (2.0 * d));
    let radius = centre.distance(a);
    (radius <= MAX_RADIUS).then(|| Circle { center: centre.round(), radius })
}

/// The foot of `c` on the segment, strictly inside it.
fn perpendicular(p: DVec2, q: DVec2, c: DVec2) -> Option<DVec2> {
    let d = q - p;
    let t = (c - p).dot(d) / d.length_squared();
    let inside = t.is_finite() && t > ZERO_TOLERANCE && t < 1.0 - ZERO_TOLERANCE;
    inside.then(|| (p + d * t).round())
}

impl Circle {
    fn polar(&self, p: DVec2) -> f64 {
        let a = (p.y - self.center.y).atan2(p.x - self.center.x);
        if a < 0.0 { 2.0 * PI + a } else { a }
    }

    fn off(&self, p: DVec2, tolerance: f64) -> bool {
        (p.distance(self.center) - self.radius).abs() > tolerance
    }

    fn over_deviation(&self, pts: &[DVec2], tolerance: f64) -> bool {
        pts.windows(2).enumerate().any(|(i, w)| {
            (i != 0 && self.off(w[0], tolerance))
                || perpendicular(w[0], w[1], self.center).is_some_and(|q| self.off(q, tolerance))
        })
    }

    /// The sum of squared deviations, or `None` where a point is out of tolerance.
    fn deviation(&self, pts: &[DVec2], tolerance: f64) -> Option<f64> {
        let mut total = 0.0;
        for (i, w) in pts.windows(2).enumerate() {
            if i != 0 {
                let d = (w[0].distance(self.center) - self.radius).abs();
                if d > tolerance {
                    return None;
                }
                total += d * d;
            }
            if let Some(q) = perpendicular(w[0], w[1], self.center) {
                let d = (q.distance(self.center) - self.radius).abs();
                if d > tolerance {
                    return None;
                }
                total += d * d;
            }
        }
        Some(total)
    }
}

fn best_circle(pts: &[DVec2], tolerance: f64) -> Option<Circle> {
    let n = pts.len();
    let middle = n / 2;
    let (&first, &last) = (pts.first()?, pts.last()?);
    if n == 3 {
        return through(first, *pts.get(middle)?, last).filter(|c| !c.over_deviation(pts, tolerance));
    }
    let mid = if n.is_multiple_of(2) {
        ((*pts.get(middle)? + *pts.get(middle - 1)?) / 2.0).trunc()
    } else {
        ((*pts.get(middle - 1)? + *pts.get(middle + 1)?) / 2.0).trunc()
    };
    if let Some(c) = through(first, mid, last).filter(|c| !c.over_deviation(pts, tolerance)) {
        return Some(c);
    }
    let mut best: Option<(f64, Circle)> = None;
    for i in (1..n - 1).filter(|i| *i != middle) {
        let Some(c) = pts.get(i).and_then(|p| through(first, *p, last)) else { continue };
        if let Some(d) = c.deviation(pts, tolerance)
            && best.is_none_or(|b| d < b.0)
        {
            best = Some((d, c));
        }
    }
    best.map(|b| b.1)
}

fn arc_of(c: Circle, start: DVec2, mid: DVec2, end: DVec2, approximate: f64) -> Option<Arc> {
    let (s, m, e) = (c.polar(start), c.polar(mid), c.polar(end));
    let mut angle = 0.0;
    let mut ccw = None;
    if e > s {
        if s < m && m < e {
            ccw = Some(true);
            angle = e - s;
        } else if (0.0 <= m && m < s) || (e < m && m < 2.0 * PI) {
            ccw = Some(false);
            angle = s + (2.0 * PI - e);
        }
    } else if s > e {
        if (s < m && m < 2.0 * PI) || (0.0 < m && m < e) {
            ccw = Some(true);
            angle = e + (2.0 * PI - s);
        } else if e < m && m < s {
            ccw = Some(false);
            angle = s - e;
        }
    }
    let mut ccw = ccw?;
    if angle.abs() < EPSILON {
        return None;
    }
    if ((c.radius * angle - approximate) / approximate).abs() >= LENGTH_TOLERANCE {
        // Points very close together can read the wrong way round: try the rest of the circle.
        let rest = c.radius * (angle - 2.0 * PI).abs();
        if ((rest - approximate) / approximate).abs() >= LENGTH_TOLERANCE {
            return None;
        }
        ccw = !ccw;
    }
    Some(Arc { circle: c, ccw, start_theta: s, end_theta: e })
}

fn ray_crosses(origin: DVec2, dir: DVec2, a: DVec2, b: DVec2) -> bool {
    let (v1, v2, v3) = (origin - a, b - a, DVec2::new(-dir.y, dir.x));
    let dot = v2.dot(v3);
    if dot.abs() < SCALED_EPSILON {
        return false;
    }
    let t1 = (v2.x * v1.y - v2.y * v1.x) / dot;
    let t2 = v1.dot(v3) / dot;
    t1 >= 0.0 && (0.0..=1.0).contains(&t2)
}

/// `are_points_within_slice`, which looks at the last two points: earlier ones passed already.
fn within_slice(arc: &Arc, pts: &[DVec2]) -> bool {
    let n = pts.len();
    let (Some(&first), Some(&last)) = (pts.first(), pts.last()) else { return false };
    let r = arc.circle.radius;
    let (start_norm, end_norm) = ((first - arc.circle.center) / r, (last - arc.circle.center) / r);
    let cross_zero = if arc.ccw { arc.start_theta > arc.end_theta } else { arc.start_theta < arc.end_theta };
    let mut crossed = false;
    let mut previous = arc.start_theta;
    for index in n.saturating_sub(2)..n {
        let Some(&p) = pts.get(index) else { return false };
        let polar = if index + 1 < n { arc.circle.polar(p) } else { arc.end_theta };
        let (low, high) = if arc.ccw { (arc.start_theta, arc.end_theta) } else { (arc.end_theta, arc.start_theta) };
        if index + 1 < n {
            let inside =
                if cross_zero { polar > high.max(low) || polar < high.min(low) } else { low < polar && polar < high };
            if !inside {
                return false;
            }
        }
        if if arc.ccw { previous > polar } else { previous < polar } {
            if !cross_zero || crossed {
                return false;
            }
            crossed = true;
        }
        let Some(&before) = index.checked_sub(1).and_then(|i| pts.get(i)) else { return false };
        if (index != 1 && ray_crosses(arc.circle.center, start_norm, before, p))
            || (index + 1 != n && ray_crosses(arc.circle.center, end_norm, before, p))
        {
            return false;
        }
        previous = polar;
    }
    cross_zero == crossed
}

fn try_arc(pts: &[DVec2], tolerance: f64) -> Option<Arc> {
    if pts.len() > MAX_POINTS {
        return None;
    }
    let circle = best_circle(pts, tolerance)?;
    let length: f64 = pts.windows(2).map(|w| w[0].distance(w[1])).sum();
    let mid = *pts.get((pts.len() - 2) / 2 + 1)?;
    let arc = arc_of(circle, *pts.first()?, mid, *pts.last()?, length)?;
    within_slice(&arc, pts).then_some(arc)
}

/// One stretch of a fitted path: its points, the previous stretch's last first, and the arc
/// through them when it is one. An arc keeps its points (simplified), as Orca's do for wipes.
#[derive(Clone, Debug, PartialEq)]
pub struct Run {
    pub points: Vec<DVec2>,
    pub arc: Option<RunArc>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RunArc {
    /// In millimetres.
    pub center: DVec2,
    /// In nanometres.
    pub radius: f64,
    pub ccw: bool,
}

/// Fits arcs to the path after `pts[0]` (the current position), within `tol` mm, and simplifies
/// what is left straight: OrcaSlicer's `ArcFitter::do_arc_fitting_and_simplify`.
pub fn fit(pts: &[DVec2], tol: f64) -> Vec<Piece> {
    let mut out = Vec::new();
    for run in fit_runs(pts, tol) {
        match run.arc {
            Some(a) => {
                if let Some(&end) = run.points.last() {
                    out.push(Piece::Arc { end, center: a.center, ccw: a.ccw });
                }
            }
            None => out.extend(run.points.iter().skip(1).copied().map(Piece::Line)),
        }
    }
    out
}

/// [`fit`], as stretches with their points.
pub fn fit_runs(pts: &[DVec2], tol: f64) -> Vec<Run> {
    let nm: Vec<DVec2> = pts.iter().map(|p| (*p * SCALE).round()).collect();
    let tolerance = tol * SCALE;
    let n = nm.len();
    if n < 2 {
        return Vec::new();
    }
    // (from, to, arc): an index range of the points.
    let mut runs: Vec<(usize, usize, Option<Arc>)> = Vec::new();
    let line_to = |runs: &mut Vec<(usize, usize, Option<Arc>)>, from: usize, to: usize| match runs.last_mut() {
        Some(r) if r.2.is_none() => r.1 = to,
        _ => runs.push((from, to, None)),
    };
    let mut front = 0;
    let mut last_arc: Option<Arc> = None;
    let mut back = 0;
    if n >= 3 {
        let mut i = 0;
        while i < n {
            back = i;
            if back - front >= 2 {
                match nm.get(front..=back).and_then(|w| try_arc(w, tolerance)) {
                    Some(arc) => {
                        last_arc = Some(arc);
                        if back + 1 == n {
                            runs.push((front, back, last_arc));
                            front = back;
                        }
                    }
                    None => {
                        if back - front > 2 {
                            runs.push((front, back - 1, last_arc));
                        } else {
                            line_to(&mut runs, front, front + 1);
                        }
                        front = back - 1;
                    }
                }
            }
            i += 1;
        }
    } else {
        back = n - 1;
    }
    if front != back {
        line_to(&mut runs, front, back);
    }
    let simplified = |from: usize, to: usize| -> Vec<DVec2> {
        let part: Vec<Pt> =
            nm.get(from..=to).unwrap_or_default().iter().map(|p| Pt::new(p.x as i64, p.y as i64)).collect();
        crate::geom::from_nm(&crate::geom::douglas_peucker(&part, tolerance))
    };
    runs.into_iter()
        .map(|(from, to, arc)| Run {
            points: simplified(from, to),
            arc: arc.map(|a| RunArc { center: a.circle.center / SCALE, radius: a.circle.radius, ccw: a.ccw }),
        })
        .collect()
}

/// Length of an arc from `from` to `end`.
pub fn arc_length(from: DVec2, end: DVec2, center: DVec2, ccw: bool) -> f64 {
    let (a, b) = (from - center, end - center);
    let mut angle = a.angle_to(b);
    if ccw && angle < 0.0 {
        angle += std::f64::consts::TAU;
    }
    if !ccw && angle > 0.0 {
        angle -= std::f64::consts::TAU;
    }
    angle.abs() * a.length()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_polygonal_circle_becomes_arcs() {
        let pts: Vec<DVec2> = (0..=60)
            .map(|k| {
                let a = k as f64 / 60.0 * std::f64::consts::PI * 1.5;
                DVec2::new(10.0 * a.cos(), 10.0 * a.sin())
            })
            .collect();
        let pieces = fit(&pts, 0.02);
        assert!(pieces.len() <= 3, "{}", pieces.len());
        assert!(matches!(pieces.first(), Some(Piece::Arc { ccw: true, .. })));
        // Arc lengths add up to the curve's.
        let mut at = pts[0];
        let mut total = 0.0;
        for p in &pieces {
            match *p {
                Piece::Arc { end, center, ccw } => {
                    total += arc_length(at, end, center, ccw);
                    at = end;
                }
                Piece::Line(q) => {
                    total += at.distance(q);
                    at = q;
                }
            }
        }
        let expected = 10.0 * std::f64::consts::PI * 1.5;
        assert!((total - expected).abs() < 0.1, "{total} vs {expected}");
    }

    #[test]
    fn straight_lines_stay_lines() {
        let pts: Vec<DVec2> = (0..10).map(|k| DVec2::new(k as f64, 0.0)).collect();
        assert!(fit(&pts, 0.02).iter().all(|p| matches!(p, Piece::Line(_))));
        let corner = vec![DVec2::ZERO, DVec2::X, DVec2::new(1.0, 1.0), DVec2::Y, DVec2::ZERO];
        assert_eq!(fit(&corner, 0.02).len(), 4);
    }
}
