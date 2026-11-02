//! Exact curves and surfaces of boundary-representation CAD models (STEP, IGES), in double
//! precision: evaluation, the parameter of a point, and sampling to a chord tolerance.
//!
//! Parameterisations follow ISO 10303-42, so a face's sense relative to its surface normal
//! (`∂S/∂u × ∂S/∂v`) means the same here as in the file.

use glam::{DMat3, DVec3};

use crate::geom::nurbs::{NurbsCurve, NurbsSurface};

/// Most samples on one curve.
const MAX_SAMPLES: usize = 50_000;
/// Smallest and largest angle step on round curves, radians.
const MIN_ANGLE: f64 = 1.0e-3;
const MAX_ANGLE: f64 = std::f64::consts::TAU / 24.0;

/// A right-handed orthonormal placement.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Axes {
    pub origin: DVec3,
    pub x: DVec3,
    pub y: DVec3,
    pub z: DVec3,
}

impl Axes {
    pub const WORLD: Self = Self { origin: DVec3::ZERO, x: DVec3::X, y: DVec3::Y, z: DVec3::Z };

    /// A placement at `origin` with `axis` as Z (default world Z) and `reference` projected
    /// square to it as X (default world X, or Z when the axis is along X).
    pub fn new(origin: DVec3, axis: Option<DVec3>, reference: Option<DVec3>) -> Self {
        let z = axis.and_then(|a| a.try_normalize()).unwrap_or(DVec3::Z);
        let pick = |r: DVec3| (r - z * r.dot(z)).try_normalize();
        let x = reference
            .and_then(pick)
            .or_else(|| pick(if z.x.abs() > 0.9 { DVec3::Z } else { DVec3::X }))
            .unwrap_or_else(|| z.any_orthonormal_vector());
        Self { origin, x, y: z.cross(x), z }
    }

    /// `p` in this frame's coordinates.
    pub fn local(&self, p: DVec3) -> DVec3 {
        let d = p - self.origin;
        DVec3::new(d.dot(self.x), d.dot(self.y), d.dot(self.z))
    }

    pub fn world(&self, l: DVec3) -> DVec3 {
        self.origin + self.x * l.x + self.y * l.y + self.z * l.z
    }

    pub fn matrix(&self) -> DMat3 {
        DMat3::from_cols(self.x, self.y, self.z)
    }
}

/// Golden-section search for the minimum of `f` on `[a, b]`.
fn golden(mut a: f64, mut b: f64, f: &impl Fn(f64) -> f64, iterations: usize) -> f64 {
    const R: f64 = 0.618_033_988_749_894_8;
    let mut c = b - R * (b - a);
    let mut d = a + R * (b - a);
    let (mut fc, mut fd) = (f(c), f(d));
    for _ in 0..iterations {
        if fc < fd {
            b = d;
            d = c;
            fd = fc;
            c = b - R * (b - a);
            fc = f(c);
        } else {
            a = c;
            c = d;
            fc = fd;
            d = a + R * (b - a);
            fd = f(d);
        }
    }
    (a + b) / 2.0
}

/// The angle step that keeps a circle of radius `r` within `tol` of its chords.
pub fn angle_step(r: f64, tol: f64) -> f64 {
    let s = if r > tol { 2.0 * (1.0 - tol / r).clamp(-1.0, 1.0).acos() } else { MAX_ANGLE };
    s.clamp(MIN_ANGLE, MAX_ANGLE)
}

#[derive(Clone, Debug)]
pub enum Curve {
    /// `origin + t·dir`, `dir` a unit vector.
    Line {
        origin: DVec3,
        dir: DVec3,
    },
    /// `t` is the angle from the frame's X.
    Circle {
        frame: Axes,
        radius: f64,
    },
    /// `a` along X, `b` along Y; `t` is the eccentric angle.
    Ellipse {
        frame: Axes,
        a: f64,
        b: f64,
    },
    Nurbs(NurbsCurve),
    /// Straight segments; `t` runs 0..n-1 through the points.
    Polyline(Vec<DVec3>),
}

impl Curve {
    pub fn point(&self, t: f64) -> DVec3 {
        match self {
            Self::Line { origin, dir } => *origin + *dir * t,
            Self::Circle { frame, radius } => frame.world(DVec3::new(radius * t.cos(), radius * t.sin(), 0.0)),
            Self::Ellipse { frame, a, b } => frame.world(DVec3::new(a * t.cos(), b * t.sin(), 0.0)),
            Self::Nurbs(n) => n.point(t),
            Self::Polyline(p) => {
                let last = p.len().saturating_sub(1);
                let t = if t.is_finite() { t.clamp(0.0, last as f64) } else { 0.0 };
                let i = (t.floor() as usize).min(last.saturating_sub(1));
                match (p.get(i), p.get(i + 1)) {
                    (Some(a), Some(b)) => a.lerp(*b, t - i as f64),
                    (Some(a), None) => *a,
                    _ => DVec3::ZERO,
                }
            }
        }
    }

    /// The period of a closed curve.
    pub fn period(&self) -> Option<f64> {
        matches!(self, Self::Circle { .. } | Self::Ellipse { .. }).then_some(std::f64::consts::TAU)
    }

    /// The parameter range; None for an unbounded line.
    pub fn domain(&self) -> Option<(f64, f64)> {
        match self {
            Self::Line { .. } => None,
            Self::Circle { .. } | Self::Ellipse { .. } => Some((0.0, std::f64::consts::TAU)),
            Self::Nurbs(n) => Some(n.domain()),
            Self::Polyline(p) => Some((0.0, p.len().saturating_sub(1) as f64)),
        }
    }

    /// Where the curve is nearest by `metric` (a squared distance from a curve point).
    pub fn closest_by(&self, metric: impl Fn(DVec3) -> f64, near: DVec3) -> f64 {
        let f = |t: f64| metric(self.point(t));
        match self {
            Self::Line { origin, dir } => {
                // Linear in t for every metric used (point and line distances): search
                // around the foot of `near`, widely enough for skewed metrics.
                let t0 = (near - *origin).dot(*dir);
                let span = (near - *origin).length().max(1.0) * 4.0;
                golden(t0 - span, t0 + span, &f, 80)
            }
            Self::Circle { .. } | Self::Ellipse { .. } | Self::Nurbs(_) | Self::Polyline(_) => {
                let (lo, hi) = self.domain().unwrap_or((0.0, 1.0));
                let samples: Vec<f64> = match self {
                    Self::Nurbs(n) => {
                        let b = n.breaks();
                        let per = (2 * n.degree() + 2).clamp(4, 16);
                        let mut s = Vec::new();
                        for w in b.windows(2) {
                            for k in 0..per {
                                s.push(w[0] + (w[1] - w[0]) * k as f64 / per as f64);
                            }
                            if s.len() > 4000 {
                                break;
                            }
                        }
                        s.push(hi);
                        s
                    }
                    Self::Polyline(p) => (0..p.len()).map(|i| i as f64).collect(),
                    _ => (0..=64).map(|k| lo + (hi - lo) * f64::from(k) / 64.0).collect(),
                };
                let (best, _) = samples
                    .iter()
                    .enumerate()
                    .map(|(i, &t)| (i, f(t)))
                    .fold((0, f64::INFINITY), |acc, (i, d)| if d < acc.1 { (i, d) } else { acc });
                let a = samples.get(best.saturating_sub(1)).copied().unwrap_or(lo);
                let b = samples.get(best + 1).copied().unwrap_or(hi);
                let t = golden(a, b, &f, 70);
                // A closed curve's ends are one point: keep the search's side.
                if f(t) <= f(samples.get(best).copied().unwrap_or(t)) {
                    t
                } else {
                    samples.get(best).copied().unwrap_or(t)
                }
            }
        }
    }

    /// The parameter of the curve point nearest `p`.
    pub fn closest(&self, p: DVec3) -> f64 {
        match self {
            Self::Line { origin, dir } => (p - *origin).dot(*dir),
            Self::Circle { frame, .. } => {
                let l = frame.local(p);
                l.y.atan2(l.x).rem_euclid(std::f64::consts::TAU)
            }
            _ => self.closest_by(|c| (c - p).length_squared(), p),
        }
    }

    /// Parameters from `t0` to `t1` (`t1 >= t0`) whose chords stay within `tol` of the curve.
    pub fn sample(&self, t0: f64, t1: f64, tol: f64) -> Vec<f64> {
        let span = t1 - t0;
        if !span.is_finite() || span <= 0.0 {
            return vec![t0, t1];
        }
        let even = |n: usize| -> Vec<f64> {
            let n = n.clamp(1, MAX_SAMPLES);
            (0..=n).map(|k| t0 + span * k as f64 / n as f64).collect()
        };
        match self {
            Self::Line { .. } => vec![t0, t1],
            Self::Circle { radius, .. } => even((span / angle_step(*radius, tol)).ceil() as usize),
            Self::Ellipse { a, b, .. } => {
                // The tightest bend has radius min²/max; the widest arc length per angle is max.
                let (lo, hi) = (a.min(*b), a.max(*b));
                let r = if hi > 0.0 { lo * lo / hi } else { 0.0 };
                let step = angle_step(r.max(tol), tol) * (r / hi.max(1e-300)).clamp(0.05, 1.0).sqrt();
                even((span / step.max(MIN_ANGLE)).ceil() as usize)
            }
            Self::Polyline(p) => {
                let mut t = vec![t0];
                let first = t0.floor() as i64 + 1;
                for i in first..(t1.ceil() as i64) {
                    if (i as f64) > t0 && (i as f64) < t1 && (i as usize) < p.len() {
                        t.push(i as f64);
                    }
                }
                t.push(t1);
                t
            }
            Self::Nurbs(n) => {
                let mut seeds = vec![t0];
                let per = (2 * n.degree()).clamp(2, 8);
                for w in n.breaks().windows(2) {
                    let (a, b) = (w[0].max(t0), w[1].min(t1));
                    if b <= a {
                        continue;
                    }
                    for k in 1..=per {
                        seeds.push(a + (b - a) * k as f64 / per as f64);
                    }
                }
                if seeds.last().is_none_or(|l| *l < t1) {
                    seeds.push(t1);
                }
                seeds.dedup_by(|a, b| (*a - *b).abs() <= 1e-15 * span);
                adaptive(&seeds, tol, &|t| self.point(t))
            }
        }
    }
}

/// Refines `seeds` (increasing parameters) until every chord stays within `tol` of `point`.
pub(crate) fn adaptive(seeds: &[f64], tol: f64, point: &impl Fn(f64) -> DVec3) -> Vec<f64> {
    let mut out = Vec::with_capacity(seeds.len() * 2);
    let mut stack: Vec<(f64, DVec3, f64, DVec3, u32)> = Vec::new();
    let Some(&first) = seeds.first() else { return out };
    out.push(first);
    for w in seeds.windows(2) {
        stack.push((w[0], point(w[0]), w[1], point(w[1]), 0));
        while let Some((a, pa, b, pb, depth)) = stack.pop() {
            let m = (a + b) / 2.0;
            let pm = point(m);
            // Check the middle and the quarter points against the chord.
            let off = |t: f64, p: DVec3| {
                let s = (t - a) / (b - a);
                (p - pa.lerp(pb, s)).length()
            };
            let q1 = a + (b - a) * 0.25;
            let q3 = a + (b - a) * 0.75;
            let bad = off(m, pm) > tol || off(q1, point(q1)) > tol || off(q3, point(q3)) > tol;
            if bad && depth < 20 && out.len() + stack.len() < MAX_SAMPLES {
                // Right half first on the stack so the left comes out first.
                stack.push((m, pm, b, pb, depth + 1));
                stack.push((a, pa, m, pm, depth + 1));
            } else {
                out.push(b);
            }
        }
    }
    out
}

/// Points along `point` at parameters refined from `seeds` until every chord stays within `tol`.
pub fn adaptive_points(seeds: &[f64], tol: f64, point: &impl Fn(f64) -> DVec3) -> Vec<DVec3> {
    adaptive(seeds, tol, point).into_iter().map(point).collect()
}

/// A line along which a surface collapses to one point: `S(u, value)` for every `u` when
/// `along_u`, else `S(value, v)` for every `v`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pole {
    pub along_u: bool,
    pub value: f64,
    pub point: DVec3,
}

#[derive(Clone, Debug)]
pub enum Surface {
    Plane(Axes),
    /// `u` angle, `v` along the axis.
    Cylinder {
        frame: Axes,
        radius: f64,
    },
    /// Radius `radius + v·tan` at height `v`; `u` angle.
    Cone {
        frame: Axes,
        radius: f64,
        tan: f64,
    },
    /// `u` longitude, `v` latitude.
    Sphere {
        frame: Axes,
        radius: f64,
    },
    /// `u` around the axis, `v` around the tube.
    Torus {
        frame: Axes,
        major: f64,
        minor: f64,
    },
    /// The `curve` (parameter `u`) turned by `v` about the axis through `origin`.
    Revolution {
        curve: Box<Curve>,
        origin: DVec3,
        axis: DVec3,
    },
    /// The `curve` (parameter `u`) moved `v` along the unit `dir`.
    Extrusion {
        curve: Box<Curve>,
        dir: DVec3,
    },
    Nurbs(NurbsSurface),
    /// `base` moved `distance` along its unit normal.
    Offset {
        base: Box<Surface>,
        distance: f64,
    },
    /// Straight lines from curve `a` to curve `b`: `u` in 0..1 runs along both (over their
    /// ranges `ra`, `rb`), `v` in 0..1 across.
    Ruled {
        a: Box<Curve>,
        ra: (f64, f64),
        b: Box<Curve>,
        rb: (f64, f64),
    },
}

impl Surface {
    pub fn point(&self, u: f64, v: f64) -> DVec3 {
        match self {
            Self::Plane(f) => f.world(DVec3::new(u, v, 0.0)),
            Self::Cylinder { frame, radius } => frame.world(DVec3::new(radius * u.cos(), radius * u.sin(), v)),
            Self::Cone { frame, radius, tan } => {
                let r = radius + v * tan;
                frame.world(DVec3::new(r * u.cos(), r * u.sin(), v))
            }
            Self::Sphere { frame, radius } => {
                frame.world(DVec3::new(radius * v.cos() * u.cos(), radius * v.cos() * u.sin(), radius * v.sin()))
            }
            Self::Torus { frame, major, minor } => {
                let r = major + minor * v.cos();
                frame.world(DVec3::new(r * u.cos(), r * u.sin(), minor * v.sin()))
            }
            Self::Revolution { curve, origin, axis } => {
                let q = curve.point(u) - *origin;
                // Rodrigues' rotation by v about the axis.
                *origin + q * v.cos() + axis.cross(q) * v.sin() + *axis * axis.dot(q) * (1.0 - v.cos())
            }
            Self::Extrusion { curve, dir } => curve.point(u) + *dir * v,
            Self::Nurbs(n) => n.point(u, v),
            Self::Offset { base, distance } => base.point(u, v) + base.normal(u, v) * *distance,
            Self::Ruled { a, ra, b, rb } => {
                let pa = a.point(ra.0 + (ra.1 - ra.0) * u);
                let pb = b.point(rb.0 + (rb.1 - rb.0) * u);
                pa.lerp(pb, v)
            }
        }
    }

    /// A parameter step for numeric derivatives at `x`.
    fn step(x: f64) -> f64 {
        1.0e-6 * x.abs().max(1.0)
    }

    /// `∂S/∂u` and `∂S/∂v`, by central differences.
    pub fn derivatives(&self, u: f64, v: f64) -> (DVec3, DVec3) {
        let (hu, hv) = (Self::step(u), Self::step(v));
        let du = (self.point(u + hu, v) - self.point(u - hu, v)) / (2.0 * hu);
        let dv = (self.point(u, v + hv) - self.point(u, v - hv)) / (2.0 * hv);
        (du, dv)
    }

    /// The unit normal `∂S/∂u × ∂S/∂v`; at a collapsed point, the normal a little inside.
    pub fn normal(&self, u: f64, v: f64) -> DVec3 {
        let (du, dv) = self.derivatives(u, v);
        if let Some(n) = du.cross(dv).try_normalize() {
            return n;
        }
        for (a, b) in [(1.0, 1.0), (-1.0, 1.0), (1.0, -1.0), (-1.0, -1.0)] {
            let (du, dv) = self.derivatives(u + a * 1.0e-4, v + b * 1.0e-4);
            if let Some(n) = du.cross(dv).try_normalize() {
                return n;
            }
        }
        DVec3::Z
    }

    /// The period in u and in v, for surfaces that close on themselves analytically.
    pub fn periods(&self) -> [Option<f64>; 2] {
        const T: Option<f64> = Some(std::f64::consts::TAU);
        match self {
            Self::Cylinder { .. } | Self::Cone { .. } | Self::Sphere { .. } => [T, None],
            Self::Torus { .. } => [T, T],
            Self::Revolution { .. } => [None, T],
            Self::Extrusion { curve, .. } => [curve.period(), None],
            Self::Offset { base, .. } => base.periods(),
            Self::Plane(_) | Self::Nurbs(_) | Self::Ruled { .. } => [None, None],
        }
    }

    /// The parameter box, where the surface has one (`None` for an unbounded direction).
    pub fn domain(&self) -> [Option<(f64, f64)>; 2] {
        use std::f64::consts::{FRAC_PI_2, TAU};
        match self {
            Self::Plane(_) => [None, None],
            Self::Cylinder { .. } | Self::Cone { .. } => [Some((0.0, TAU)), None],
            Self::Sphere { .. } => [Some((0.0, TAU)), Some((-FRAC_PI_2, FRAC_PI_2))],
            Self::Torus { .. } => [Some((0.0, TAU)), Some((0.0, TAU))],
            Self::Revolution { curve, .. } => [curve.domain(), Some((0.0, TAU))],
            Self::Extrusion { curve, .. } => [curve.domain(), None],
            Self::Nurbs(n) => {
                let (u, v) = n.domain();
                [Some(u), Some(v)]
            }
            Self::Offset { base, .. } => base.domain(),
            Self::Ruled { .. } => [Some((0.0, 1.0)), Some((0.0, 1.0))],
        }
    }

    /// Lines along which the surface collapses to a point.
    pub fn poles(&self) -> Vec<Pole> {
        use std::f64::consts::FRAC_PI_2;
        match self {
            Self::Sphere { frame, radius } => vec![
                Pole { along_u: true, value: -FRAC_PI_2, point: frame.origin - frame.z * *radius },
                Pole { along_u: true, value: FRAC_PI_2, point: frame.origin + frame.z * *radius },
            ],
            Self::Cone { frame, radius, tan } if tan.abs() > 1.0e-12 => {
                let v = -radius / tan;
                vec![Pole { along_u: true, value: v, point: frame.origin + frame.z * v }]
            }
            Self::Revolution { curve, origin, axis } => {
                let mut out = Vec::new();
                if let Some((a, b)) = curve.domain() {
                    for t in [a, b] {
                        let p = curve.point(t);
                        let q = p - *origin;
                        let size = q.length().max(1.0);
                        if (q - *axis * q.dot(*axis)).length() <= 1.0e-9 * size {
                            out.push(Pole { along_u: false, value: t, point: p });
                        }
                    }
                }
                out
            }
            Self::Nurbs(_) | Self::Offset { .. } | Self::Ruled { .. } => {
                let mut out = Vec::new();
                let [Some((u0, u1)), Some((v0, v1))] = self.domain() else { return out };
                let size = self.size_hint();
                for (along_u, value) in [(true, v0), (true, v1), (false, u0), (false, u1)] {
                    let at = |k: f64| {
                        if along_u {
                            self.point(u0 + (u1 - u0) * k, value)
                        } else {
                            self.point(value, v0 + (v1 - v0) * k)
                        }
                    };
                    let p0 = at(0.0);
                    if [0.25, 0.5, 0.75, 1.0].iter().all(|&k| (at(k) - p0).length() <= 1.0e-9 * size.max(1.0)) {
                        out.push(Pole { along_u, value, point: p0 });
                    }
                }
                out
            }
            _ => Vec::new(),
        }
    }

    /// A length typical of the surface, for relative tolerances.
    pub fn size_hint(&self) -> f64 {
        match self {
            Self::Plane(f) => f.origin.length().max(1.0),
            Self::Cylinder { radius, .. } | Self::Sphere { radius, .. } | Self::Cone { radius, .. } => {
                radius.abs().max(1.0)
            }
            Self::Torus { major, minor, .. } => (major.abs() + minor.abs()).max(1.0),
            Self::Nurbs(n) => {
                let mut lo = DVec3::splat(f64::INFINITY);
                let mut hi = DVec3::splat(f64::NEG_INFINITY);
                for p in n.control_points() {
                    lo = lo.min(p);
                    hi = hi.max(p);
                }
                (hi - lo).length().max(1.0)
            }
            Self::Revolution { curve, origin, .. } => {
                let (a, b) = curve.domain().unwrap_or((0.0, 1.0));
                ((curve.point(a) - *origin).length() + (curve.point(b) - curve.point(a)).length()).max(1.0)
            }
            Self::Extrusion { curve, .. } => {
                let (a, b) = curve.domain().unwrap_or((0.0, 1.0));
                (curve.point(a).length() + (curve.point(b) - curve.point(a)).length()).max(1.0)
            }
            Self::Offset { base, distance } => base.size_hint() + distance.abs(),
            Self::Ruled { .. } => {
                let p = [self.point(0.0, 0.0), self.point(1.0, 0.0), self.point(0.0, 1.0), self.point(1.0, 1.0)];
                ((p[0] - p[3]).length() + (p[1] - p[2]).length()).max(1.0)
            }
        }
    }

    /// The parameters of the surface point nearest `p`, starting from `hint` when given.
    pub fn invert(&self, p: DVec3, hint: Option<(f64, f64)>) -> (f64, f64) {
        use std::f64::consts::TAU;
        match self {
            Self::Plane(f) => {
                let l = f.local(p);
                (l.x, l.y)
            }
            Self::Cylinder { frame, .. } => {
                let l = frame.local(p);
                (l.y.atan2(l.x).rem_euclid(TAU), l.z)
            }
            Self::Cone { frame, tan, .. } => {
                let l = frame.local(p);
                let u = l.y.atan2(l.x).rem_euclid(TAU);
                // Nearest point on the generator line at angle u, in its half-plane.
                let rho = (l.x * l.x + l.y * l.y).sqrt();
                let (radius, tan) = (self.cone_radius(), *tan);
                let v = (l.z + (rho - radius) * tan) / (1.0 + tan * tan);
                (u, v)
            }
            Self::Sphere { frame, .. } => {
                let l = frame.local(p);
                let rho = (l.x * l.x + l.y * l.y).sqrt();
                (l.y.atan2(l.x).rem_euclid(TAU), l.z.atan2(rho))
            }
            Self::Torus { frame, major, .. } => {
                let l = frame.local(p);
                let u = l.y.atan2(l.x).rem_euclid(TAU);
                let rho = (l.x * l.x + l.y * l.y).sqrt();
                (u, l.z.atan2(rho - major).rem_euclid(TAU))
            }
            Self::Revolution { curve, origin, axis } => {
                let q = p - *origin;
                let h = q.dot(*axis);
                let radial = q - *axis * h;
                let rho = radial.length();
                let meridian = |c: DVec3| {
                    let qc = c - *origin;
                    let hc = qc.dot(*axis);
                    let rc = (qc - *axis * hc).length();
                    (h - hc).powi(2) + (rho - rc).powi(2)
                };
                let u = curve.closest_by(meridian, p);
                let qc = curve.point(u) - *origin;
                let rc = qc - *axis * qc.dot(*axis);
                let v = if rc.length() > 1.0e-12 && rho > 1.0e-12 {
                    let (a, b) = (rc / rc.length(), radial / rho);
                    axis.dot(a.cross(b)).atan2(a.dot(b)).rem_euclid(TAU)
                } else {
                    hint.map_or(0.0, |h| h.1)
                };
                (u, v)
            }
            Self::Extrusion { curve, dir } => {
                let perp = |c: DVec3| {
                    let d = p - c;
                    (d - *dir * d.dot(*dir)).length_squared()
                };
                let u = curve.closest_by(perp, p);
                (u, (p - curve.point(u)).dot(*dir))
            }
            Self::Nurbs(_) | Self::Offset { .. } | Self::Ruled { .. } => self.invert_numeric(p, hint),
        }
    }

    fn cone_radius(&self) -> f64 {
        match self {
            Self::Cone { radius, .. } => *radius,
            _ => 0.0,
        }
    }

    /// Gauss-Newton from `start`, kept inside the parameter box.
    fn newton(&self, p: DVec3, start: (f64, f64)) -> ((f64, f64), f64) {
        let dom = self.domain();
        let clamp = |x: f64, d: Option<(f64, f64)>| d.map_or(x, |(a, b)| x.clamp(a, b));
        let (mut u, mut v) = start;
        let mut dist = (self.point(u, v) - p).length();
        for _ in 0..40 {
            let s = self.point(u, v);
            let (su, sv) = self.derivatives(u, v);
            let r = s - p;
            let (a, b, c) = (su.dot(su), su.dot(sv), sv.dot(sv));
            let (g0, g1) = (su.dot(r), sv.dot(r));
            let det = a * c - b * b;
            if det.is_nan() || det.abs() <= 1.0e-300 {
                break;
            }
            let du = (c * g0 - b * g1) / det;
            let dv = (a * g1 - b * g0) / det;
            // Halve the step until it does not move away.
            let mut k = 1.0;
            let mut moved = false;
            for _ in 0..12 {
                let (nu, nv) = (clamp(u - du * k, dom[0]), clamp(v - dv * k, dom[1]));
                let nd = (self.point(nu, nv) - p).length();
                if nd <= dist {
                    let tiny = (nu - u).abs() + (nv - v).abs() <= 1.0e-14 * (1.0 + u.abs() + v.abs());
                    u = nu;
                    v = nv;
                    dist = nd;
                    moved = !tiny;
                    break;
                }
                k *= 0.5;
            }
            if !moved {
                break;
            }
        }
        ((u, v), dist)
    }

    fn invert_numeric(&self, p: DVec3, hint: Option<(f64, f64)>) -> (f64, f64) {
        let size = self.size_hint();
        if let Some(h) = hint {
            let (uv, d) = self.newton(p, h);
            if d <= 1.0e-7 * size {
                return uv;
            }
        }
        let ((u0, u1), (v0, v1)) = match self.domain() {
            [Some(u), Some(v)] => (u, v),
            _ => ((0.0, 1.0), (0.0, 1.0)),
        };
        // Seed on a grid that follows the knots, then polish the best few.
        let (us, vs) = match self {
            Self::Nurbs(n) => {
                let (bu, bv) = n.breaks();
                let (du, dv) = n.degrees();
                (subdivide(&bu, (du + 1).clamp(2, 6), 48), subdivide(&bv, (dv + 1).clamp(2, 6), 48))
            }
            _ => (
                (0..=16).map(|k| u0 + (u1 - u0) * f64::from(k) / 16.0).collect(),
                (0..=16).map(|k| v0 + (v1 - v0) * f64::from(k) / 16.0).collect(),
            ),
        };
        let mut seeds: Vec<(f64, (f64, f64))> = Vec::with_capacity(us.len() * vs.len());
        for &u in &us {
            for &v in &vs {
                seeds.push(((self.point(u, v) - p).length_squared(), (u, v)));
            }
        }
        seeds.sort_by(|a, b| a.0.total_cmp(&b.0));
        let mut best = (hint.unwrap_or((u0, v0)), f64::INFINITY);
        if let Some(h) = hint {
            best = self.newton(p, h);
        }
        for &(_, s) in seeds.iter().take(4) {
            let r = self.newton(p, s);
            if r.1 < best.1 {
                best = r;
            }
        }
        best.0
    }
}

impl Axes {
    /// This frame moved by the affine `m` (its axes re-orthonormalised).
    pub fn transformed(&self, m: &glam::DMat4) -> Self {
        Self::new(m.transform_point3(self.origin), Some(m.transform_vector3(self.z)), Some(m.transform_vector3(self.x)))
    }
}

impl Curve {
    /// The curve moved by the rigid (or uniformly scaled) `m`.
    pub fn transformed(&self, m: &glam::DMat4) -> Self {
        let s = m.transform_vector3(DVec3::X).length();
        match self {
            Self::Line { origin, dir } => Self::Line {
                origin: m.transform_point3(*origin),
                dir: m.transform_vector3(*dir).try_normalize().unwrap_or(*dir),
            },
            Self::Circle { frame, radius } => Self::Circle { frame: frame.transformed(m), radius: radius * s },
            Self::Ellipse { frame, a, b } => Self::Ellipse { frame: frame.transformed(m), a: a * s, b: b * s },
            Self::Nurbs(n) => Self::Nurbs(n.map_points(|p| m.transform_point3(p))),
            Self::Polyline(p) => Self::Polyline(p.iter().map(|q| m.transform_point3(*q)).collect()),
        }
    }
}

impl Surface {
    /// The surface moved by the rigid (or uniformly scaled) `m`.
    pub fn transformed(&self, m: &glam::DMat4) -> Self {
        let s = m.transform_vector3(DVec3::X).length();
        match self {
            Self::Plane(f) => Self::Plane(f.transformed(m)),
            Self::Cylinder { frame, radius } => Self::Cylinder { frame: frame.transformed(m), radius: radius * s },
            Self::Cone { frame, radius, tan } => {
                Self::Cone { frame: frame.transformed(m), radius: radius * s, tan: *tan }
            }
            Self::Sphere { frame, radius } => Self::Sphere { frame: frame.transformed(m), radius: radius * s },
            Self::Torus { frame, major, minor } => {
                Self::Torus { frame: frame.transformed(m), major: major * s, minor: minor * s }
            }
            Self::Revolution { curve, origin, axis } => Self::Revolution {
                curve: Box::new(curve.transformed(m)),
                origin: m.transform_point3(*origin),
                axis: m.transform_vector3(*axis).try_normalize().unwrap_or(*axis),
            },
            Self::Extrusion { curve, dir } => Self::Extrusion {
                curve: Box::new(curve.transformed(m)),
                dir: m.transform_vector3(*dir).try_normalize().unwrap_or(*dir),
            },
            Self::Nurbs(n) => Self::Nurbs(n.map_points(|p| m.transform_point3(p))),
            Self::Offset { base, distance } => {
                Self::Offset { base: Box::new(base.transformed(m)), distance: distance * s }
            }
            Self::Ruled { a, ra, b, rb } => {
                Self::Ruled { a: Box::new(a.transformed(m)), ra: *ra, b: Box::new(b.transformed(m)), rb: *rb }
            }
        }
    }
}

/// `breaks` with `per` points in each span, at most `cap` in all.
fn subdivide(breaks: &[f64], per: usize, cap: usize) -> Vec<f64> {
    let spans = breaks.len().saturating_sub(1).max(1);
    let per = per.min((cap / spans).max(1));
    let mut out = Vec::new();
    for w in breaks.windows(2) {
        for k in 0..per {
            out.push(w[0] + (w[1] - w[0]) * k as f64 / per as f64);
        }
    }
    if let Some(&l) = breaks.last() {
        out.push(l);
    }
    if out.len() > cap {
        let step = out.len().div_ceil(cap);
        out = out.into_iter().step_by(step).collect();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn surfaces() -> Vec<Surface> {
        let f = Axes::new(DVec3::new(1.0, 2.0, 3.0), Some(DVec3::new(0.3, -0.2, 1.0)), Some(DVec3::X));
        vec![
            Surface::Plane(f),
            Surface::Cylinder { frame: f, radius: 4.0 },
            Surface::Cone { frame: f, radius: 4.0, tan: 0.3 },
            Surface::Sphere { frame: f, radius: 5.0 },
            Surface::Torus { frame: f, major: 10.0, minor: 2.0 },
            Surface::Revolution {
                curve: Box::new(Curve::Line { origin: DVec3::new(3.0, 0.0, 0.0), dir: DVec3::new(0.6, 0.0, 0.8) }),
                origin: DVec3::ZERO,
                axis: DVec3::Z,
            },
            Surface::Extrusion {
                curve: Box::new(Curve::Circle { frame: f, radius: 3.0 }),
                dir: DVec3::new(0.0, 0.6, 0.8),
            },
            Surface::Offset { base: Box::new(Surface::Sphere { frame: f, radius: 5.0 }), distance: 1.0 },
        ]
    }

    #[test]
    fn inversion_finds_the_parameters_of_surface_points() {
        for s in surfaces() {
            for (u, v) in [(0.3, 0.4), (1.2, -0.5), (2.5, 0.9), (5.5, 0.2)] {
                let p = s.point(u, v);
                let (iu, iv) = s.invert(p, None);
                let q = s.point(iu, iv);
                assert!((p - q).length() < 1e-6, "{s:?} at ({u}, {v}): {p} vs {q}");
            }
        }
    }

    #[test]
    fn analytic_normals_point_outward() {
        let f = Axes::WORLD;
        let n = Surface::Sphere { frame: f, radius: 2.0 }.normal(0.0, 0.0);
        assert!((n - DVec3::X).length() < 1e-6);
        let n = Surface::Cylinder { frame: f, radius: 2.0 }.normal(0.0, 1.0);
        assert!((n - DVec3::X).length() < 1e-6);
        let n = Surface::Torus { frame: f, major: 5.0, minor: 1.0 }.normal(0.0, 0.0);
        assert!((n - DVec3::X).length() < 1e-6);
    }

    #[test]
    fn circle_samples_meet_the_tolerance() {
        let c = Curve::Circle { frame: Axes::WORLD, radius: 10.0 };
        let t = c.sample(0.0, std::f64::consts::PI, 0.01);
        for w in t.windows(2) {
            let mid = c.point((w[0] + w[1]) / 2.0);
            let chord = (c.point(w[0]) + c.point(w[1])) / 2.0;
            assert!((mid - chord).length() <= 0.0101);
        }
        assert!((c.closest(DVec3::new(0.0, -3.0, 1.0)) - 1.5 * std::f64::consts::PI).abs() < 1e-12);
    }

    #[test]
    fn curve_closest_points() -> Result<(), crate::geom::nurbs::NurbsError> {
        let n = NurbsCurve::new(
            3,
            vec![0., 0., 0., 0., 1., 2., 2., 2., 2.],
            &[
                DVec3::ZERO,
                DVec3::new(1., 2., 0.),
                DVec3::new(3., -1., 0.),
                DVec3::new(4., 1., 0.),
                DVec3::new(6., 0., 0.),
            ],
            None,
        )?;
        let c = Curve::Nurbs(n);
        for t in [0.0, 0.3, 1.1, 1.9, 2.0] {
            let p = c.point(t);
            assert!((c.point(c.closest(p)) - p).length() < 1e-7, "{t}");
        }
        let e = Curve::Ellipse { frame: Axes::WORLD, a: 5.0, b: 2.0 };
        let p = e.point(2.0);
        assert!((e.closest(p) - 2.0).abs() < 1e-6);
        Ok(())
    }
}
