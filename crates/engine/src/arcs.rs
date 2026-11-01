//! Arc fitting: runs of points on a circle become one G2/G3 move (OrcaSlicer's "Arc
//! fitting"), so curves print smoothly and the G-code is smaller.

use glam::DVec2;

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

const MIN_POINTS: usize = 4;
const MAX_WINDOW: usize = 400;
const MIN_RADIUS: f64 = 0.5;
const MAX_RADIUS: f64 = 1000.0;

fn circle(a: DVec2, b: DVec2, c: DVec2) -> Option<(DVec2, f64)> {
    let d = 2.0 * (a.x * (b.y - c.y) + b.x * (c.y - a.y) + c.x * (a.y - b.y));
    if d.abs() < 1e-12 {
        return None;
    }
    let (a2, b2, c2) = (a.length_squared(), b.length_squared(), c.length_squared());
    let x = (a2 * (b.y - c.y) + b2 * (c.y - a.y) + c2 * (a.y - b.y)) / d;
    let y = (a2 * (c.x - b.x) + b2 * (a.x - c.x) + c2 * (b.x - a.x)) / d;
    let centre = DVec2::new(x, y);
    let r = centre.distance(a);
    (r.is_finite() && (MIN_RADIUS..=MAX_RADIUS).contains(&r)).then_some((centre, r))
}

/// Whether points `pts` lie on one arc within `tol`, turning one way under a full circle.
fn fits(pts: &[DVec2], tol: f64) -> Option<(DVec2, bool)> {
    let (first, last) = (*pts.first()?, *pts.last()?);
    let mid = *pts.get(pts.len() / 2)?;
    let (centre, r) = circle(first, mid, last)?;
    let ccw = (mid - first).perp_dot(last - mid) > 0.0;
    let mut swept = 0.0;
    for w in pts.windows(2) {
        let (p, q) = (w[0], w[1]);
        if (p.distance(centre) - r).abs() > tol {
            return None;
        }
        // The chord's sag must be within tolerance too.
        let sag = r - (r * r - (p.distance(q) / 2.0).powi(2)).max(0.0).sqrt();
        if sag > tol {
            return None;
        }
        let turn = (p - centre).perp_dot(q - centre);
        if (turn > 0.0) != ccw {
            return None;
        }
        swept += (p - centre).angle_to(q - centre).abs();
    }
    (swept < std::f64::consts::TAU * 0.95).then_some((centre, ccw))
}

/// Fits arcs to the path after `pts[0]` (the current position), within `tol` mm.
pub fn fit(pts: &[DVec2], tol: f64) -> Vec<Piece> {
    let mut out = Vec::new();
    let mut i = 0;
    while i + 1 < pts.len() {
        let mut best: Option<(usize, DVec2, bool)> = None;
        let mut j = i + MIN_POINTS - 1;
        while j < pts.len() && j - i <= MAX_WINDOW {
            match pts.get(i..=j).and_then(|w| fits(w, tol)) {
                Some((c, ccw)) => best = Some((j, c, ccw)),
                None if best.is_some() => break,
                None => {}
            }
            j += 1;
        }
        match best {
            Some((j, center, ccw)) => {
                if let Some(&end) = pts.get(j) {
                    out.push(Piece::Arc { end, center, ccw });
                }
                i = j;
            }
            None => {
                if let Some(&p) = pts.get(i + 1) {
                    out.push(Piece::Line(p));
                }
                i += 1;
            }
        }
    }
    out
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
