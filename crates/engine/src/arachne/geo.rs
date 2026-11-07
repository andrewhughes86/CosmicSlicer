//! The integer geometry Arachne leans on, as Orca's `Point`, `Line` and Eigen compute it:
//! squared norms in 64 bits, square roots through doubles and truncated back, projections in
//! doubles cast back by truncation.

use crate::clipper::Pt;

pub fn sub(a: Pt, b: Pt) -> Pt {
    Pt::new(a.x.wrapping_sub(b.x), a.y.wrapping_sub(b.y))
}

pub fn add(a: Pt, b: Pt) -> Pt {
    Pt::new(a.x.wrapping_add(b.x), a.y.wrapping_add(b.y))
}

/// `cast<int64_t>().squaredNorm()`.
pub fn sq(p: Pt) -> i64 {
    p.x.wrapping_mul(p.x).wrapping_add(p.y.wrapping_mul(p.y))
}

/// `cast<int64_t>().norm()`: the square root of the integer square, truncated.
pub fn norm(p: Pt) -> i64 {
    (sq(p) as f64).sqrt() as i64
}

pub fn dot(a: Pt, b: Pt) -> i64 {
    a.x.wrapping_mul(b.x).wrapping_add(a.y.wrapping_mul(b.y))
}

pub fn cross(a: Pt, b: Pt) -> i64 {
    a.x.wrapping_mul(b.y).wrapping_sub(a.y.wrapping_mul(b.x))
}

/// `perp`: a quarter turn left.
pub fn perp(p: Pt) -> Pt {
    Pt::new(p.y.wrapping_neg(), p.x)
}

/// `shorter_then`.
pub fn shorter_then(p: Pt, len: i64) -> bool {
    if p.x > len || p.x < -len || p.y > len || p.y < -len {
        return false;
    }
    sq(p) <= len.wrapping_mul(len)
}

/// `Point::operator<`.
pub fn less(a: Pt, b: Pt) -> bool {
    a.x < b.x || (a.x == b.x && a.y < b.y)
}

/// `Point(double, double)`.
pub fn round_pt(x: f64, y: f64) -> Pt {
    Pt::new(x.round() as i64, y.round() as i64)
}

/// `Point::rotated(cos, sin)`.
pub fn rotated(p: Pt, cos: f64, sin: f64) -> Pt {
    let (x, y) = (p.x as f64, p.y as f64);
    Pt::new((cos * x - sin * y).round() as i64, (cos * y + sin * x).round() as i64)
}

/// `line_alg::distance_to_infinite_squared` with the foot point, cast back by truncation.
pub fn infinite_sq(p: Pt, a: Pt, b: Pt) -> (f64, Pt) {
    let v = ((b.x - a.x) as f64, (b.y - a.y) as f64);
    let va = ((p.x - a.x) as f64, (p.y - a.y) as f64);
    let l2 = v.0 * v.0 + v.1 * v.1;
    if l2 == 0.0 {
        return (va.0 * va.0 + va.1 * va.1, a);
    }
    let t = (va.0 * v.0 + va.1 * v.1) / l2;
    let foot = Pt::new((a.x as f64 + t * v.0) as i64, (a.y as f64 + t * v.1) as i64);
    let (dx, dy) = (t * v.0 - va.0, t * v.1 - va.1);
    (dx * dx + dy * dy, foot)
}

/// `line_alg::distance_to_squared`: to the segment.
pub fn segment_sq(p: Pt, a: Pt, b: Pt) -> (f64, Pt) {
    let v = ((b.x - a.x) as f64, (b.y - a.y) as f64);
    let va = ((p.x - a.x) as f64, (p.y - a.y) as f64);
    let l2 = v.0 * v.0 + v.1 * v.1;
    if l2 == 0.0 {
        return (va.0 * va.0 + va.1 * va.1, a);
    }
    let t = (va.0 * v.0 + va.1 * v.1) / l2;
    if t <= 0.0 {
        return (va.0 * va.0 + va.1 * va.1, a);
    }
    if t >= 1.0 {
        let vb = ((p.x - b.x) as f64, (p.y - b.y) as f64);
        return (vb.0 * vb.0 + vb.1 * vb.1, b);
    }
    let foot = Pt::new((a.x as f64 + t * v.0) as i64, (a.y as f64 + t * v.1) as i64);
    let (dx, dy) = (t * v.0 - va.0, t * v.1 - va.1);
    (dx * dx + dy * dy, foot)
}

/// `Line::intersection_infinite`.
pub fn intersection_infinite(a1: Pt, b1: Pt, a2: Pt, b2: Pt) -> Option<Pt> {
    let v12 = ((a2.x - a1.x) as f64, (a2.y - a1.y) as f64);
    let v1 = ((b1.x - a1.x) as f64, (b1.y - a1.y) as f64);
    let v2 = ((b2.x - a2.x) as f64, (b2.y - a2.y) as f64);
    let denom = v1.0 * v2.1 - v1.1 * v2.0;
    if denom.abs() < 1e-4 {
        return None;
    }
    let t1 = (v12.0 * v2.1 - v12.1 * v2.0) / denom;
    let (x, y) = (a1.x as f64 + t1 * v1.0, a1.y as f64 + t1 * v1.1);
    let limit = i64::MAX as f64;
    if !(x.abs() <= limit && y.abs() <= limit) {
        return None;
    }
    Some(Pt::new(x as i64, y as i64))
}

/// `Geometry::is_point_inside_polygon_corner`.
pub fn inside_corner(a: Pt, b: Pt, c: Pt, q: Pt) -> bool {
    let unit = |p: Pt| {
        let (x, y) = ((p.x - b.x) as f64, (p.y - b.y) as f64);
        let n = (x * x + y * y).sqrt();
        if n > 0.0 { (x / n, y / n) } else { (x, y) }
    };
    let (ba, bc, bq) = (unit(a), unit(c), unit(q));
    let normal = (-bq.1, bq.0);
    let pa = ba.0 * normal.0 + ba.1 * normal.1;
    let pc = bc.0 * normal.0 + bc.1 * normal.1;
    if (pa > 0.0 && pc <= 0.0) || (pa <= 0.0 && pc > 0.0) {
        return pa > 0.0;
    }
    let qa = ba.0 * bq.0 + ba.1 * bq.1;
    let qc = bc.0 * bq.0 + bc.1 * bq.1;
    (pa > 0.0 && qc < qa) || (pa <= 0.0 && qc >= qa)
}

/// `LinearAlg2D::pointIsLeftOfLine`.
pub fn left_of(p: Pt, a: Pt, b: Pt) -> i64 {
    (b.x - a.x).wrapping_mul(p.y - a.y).wrapping_sub((b.y - a.y).wrapping_mul(p.x - a.x))
}

/// `LinearAlg2D::getAngleLeft`, in [0, 2π).
pub fn angle_left(a: Pt, b: Pt, c: Pt) -> f32 {
    let (ba, bc) = (sub(a, b), sub(c, b));
    let dott = dot(ba, bc);
    let det = cross(ba, bc);
    if det == 0 {
        let pointy = (ba.x != 0 && (ba.x > 0) == (bc.x > 0)) || (ba.x == 0 && (ba.y > 0) == (bc.y > 0));
        return if pointy { 0.0 } else { std::f32::consts::PI };
    }
    let angle = -((det as f64).atan2(dott as f64)) as f32;
    if angle >= 0.0 { angle } else { (std::f64::consts::PI * 2.0 + f64::from(angle)) as f32 }
}

/// `ClipperLib::Area`.
pub fn area(poly: &[Pt]) -> f64 {
    crate::clipper::area(poly)
}
