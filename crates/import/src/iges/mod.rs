//! IGES 5.x: trimmed and bounded surfaces, B-rep solids and untrimmed surfaces.
//!
//! The fixed-column file (start, global, directory and parameter sections) is read with the
//! global section's delimiters and units. Faces come from trimmed surfaces (144), bounded
//! surfaces (143), manifold solid B-reps (186, through shells 514, faces 510, loops 508 and
//! edge lists 504) and surfaces that stand alone; subfigure instances (408) place their
//! definitions (308). Transformation matrices (124) apply throughout. Surfaces: B-spline
//! (128), planes (108, 190), cylinders, cones, spheres and tori (192–198), ruled (118),
//! revolution (120), tabulated cylinder (122) and offset (140). Curves: lines, arcs, ellipses,
//! B-splines, polylines, parametric splines and composites. Blanked entities are skipped.

use std::collections::{HashMap, HashSet};

use crate::geom::brep::{Axes, Curve, Surface, adaptive_points};
use crate::geom::nurbs::{NurbsCurve, NurbsSurface};
use crate::geom::{DMat4, DVec3, Vec3};

use crate::brep::{FaceJob, orient_outward, tessellate_faces, warnings};
use crate::{ImportError, Imported, Options, Soup, Units};

/// Deepest chain of references followed.
const MAX_DEPTH: usize = 24;
/// Most parameters in one entity.
const MAX_PARAMS: usize = 2_000_000;
/// Most subfigure instances placed.
const MAX_PLACED: usize = 100_000;

type Why = String;

/// A surface and its natural parameter box (u range, v range), when it has one.
type Bounded = (Surface, Option<[(f64, f64); 2]>);

fn bad(what: impl std::fmt::Display) -> ImportError {
    ImportError::Malformed(format!("IGES: {what}"))
}

#[derive(Clone, Debug, Default)]
struct Entry {
    kind: i64,
    param: usize,
    lines: usize,
    transform: usize,
    form: i64,
    blanked: bool,
    dependent: bool,
}

/// Splits a parameter record at `delim`, ending at `end`, reading Hollerith strings whole.
fn tokens(s: &str, delim: char, end: char) -> Vec<String> {
    let c: Vec<char> = s.chars().collect();
    let mut out = Vec::new();
    let mut i = 0usize;
    while i <= c.len() && out.len() < MAX_PARAMS {
        while c.get(i).is_some_and(|ch| *ch == ' ') {
            i += 1;
        }
        // nHxxxx: n characters, delimiters included.
        let digits = c.get(i..).map_or(0, |r| r.iter().take_while(|ch| ch.is_ascii_digit()).count());
        if digits > 0 && c.get(i + digits).is_some_and(|ch| *ch == 'H' || *ch == 'h') {
            let n: usize =
                c.get(i..i + digits).map(|d| d.iter().collect::<String>()).and_then(|d| d.parse().ok()).unwrap_or(0);
            let start = i + digits + 1;
            let stop = (start + n).min(c.len());
            out.push(c.get(start..stop).map(|t| t.iter().collect()).unwrap_or_default());
            i = stop;
            while c.get(i).is_some_and(|ch| *ch == ' ') {
                i += 1;
            }
            match c.get(i) {
                Some(ch) if *ch == end => break,
                Some(_) => i += 1,
                None => break,
            }
            continue;
        }
        let start = i;
        while c.get(i).is_some_and(|ch| *ch != delim && *ch != end) {
            i += 1;
        }
        out.push(c.get(start..i).map(|t| t.iter().collect::<String>().trim().to_owned()).unwrap_or_default());
        match c.get(i) {
            Some(ch) if *ch == end => break,
            Some(_) => i += 1,
            None => break,
        }
    }
    out
}

fn real(t: Option<&String>) -> Option<f64> {
    let t = t?.trim();
    if t.is_empty() {
        return Some(0.0);
    }
    t.replace(['D', 'd'], "E").parse::<f64>().ok().filter(|v| v.is_finite())
}

fn int(t: Option<&String>) -> Option<i64> {
    let t = t?.trim();
    if t.is_empty() {
        return Some(0);
    }
    t.parse::<i64>().ok().or_else(|| real(Some(&t.to_owned())).map(|v| v as i64))
}

struct Iges {
    de: HashMap<usize, Entry>,
    params: HashMap<usize, Vec<String>>,
    tol: f64,
    /// Kinds of geometry not understood, with counts.
    unknown: HashMap<String, usize>,
    edges: HashMap<(usize, usize), Vec<DVec3>>,
}

impl Iges {
    fn entry(&self, de: usize) -> Option<&Entry> {
        self.de.get(&de)
    }

    fn p(&self, de: usize) -> &[String] {
        self.params.get(&de).map_or(&[], Vec::as_slice)
    }

    fn f(&self, de: usize, i: usize) -> Result<f64, Why> {
        real(self.p(de).get(i)).ok_or_else(|| format!("entity {de}: parameter {i} is not a number"))
    }

    fn n(&self, de: usize, i: usize) -> Result<i64, Why> {
        int(self.p(de).get(i)).ok_or_else(|| format!("entity {de}: parameter {i} is not an integer"))
    }

    fn ptr(&self, de: usize, i: usize) -> Result<usize, Why> {
        Ok(self.n(de, i)?.unsigned_abs() as usize)
    }

    fn count(&self, de: usize, i: usize, per: usize) -> Result<usize, Why> {
        let n = self.n(de, i)?;
        if n < 0 || (n as usize).saturating_mul(per) > self.p(de).len() + 4 {
            return Err(format!("entity {de}: count {n} is more than it holds"));
        }
        Ok(n as usize)
    }

    fn v3(&self, de: usize, i: usize) -> Result<DVec3, Why> {
        Ok(DVec3::new(self.f(de, i)?, self.f(de, i + 1)?, self.f(de, i + 2)?))
    }

    /// A transformation matrix entity (124), with the ones it is itself placed by.
    fn matrix(&self, de: usize, depth: usize) -> DMat4 {
        if de == 0 || depth > MAX_DEPTH {
            return DMat4::IDENTITY;
        }
        let Some(e) = self.entry(de) else { return DMat4::IDENTITY };
        if e.kind != 124 {
            return DMat4::IDENTITY;
        }
        let r = |i: usize| self.f(de, i).unwrap_or(0.0);
        let m = DMat4::from_cols(
            DVec3::new(r(1), r(5), r(9)).extend(0.0),
            DVec3::new(r(2), r(6), r(10)).extend(0.0),
            DVec3::new(r(3), r(7), r(11)).extend(0.0),
            DVec3::new(r(4), r(8), r(12)).extend(1.0),
        );
        if !m.is_finite() || m.determinant().abs() < 1.0e-12 {
            return DMat4::IDENTITY;
        }
        self.matrix(e.transform, depth + 1) * m
    }

    fn placed(&self, de: usize) -> DMat4 {
        self.entry(de).map_or(DMat4::IDENTITY, |e| self.matrix(e.transform, 0))
    }

    fn note(&mut self, what: String) {
        *self.unknown.entry(what).or_insert(0) += 1;
    }

    fn point_entity(&self, de: usize) -> Result<DVec3, Why> {
        Ok(self.placed(de).transform_point3(self.v3(de, 1)?))
    }

    fn direction_entity(&self, de: usize) -> Result<DVec3, Why> {
        self.placed(de)
            .transform_vector3(self.v3(de, 1)?)
            .try_normalize()
            .ok_or_else(|| format!("entity {de}: zero direction"))
    }

    /// A curve and its own parameter range, start to end, in model space.
    fn curve(&self, de: usize, depth: usize) -> Result<(Curve, (f64, f64)), Why> {
        if depth > MAX_DEPTH {
            return Err("curves nest too deeply".into());
        }
        let e = self.entry(de).ok_or_else(|| format!("missing entity {de}"))?;
        let m = self.matrix(e.transform, 0);
        let (c, range) = match e.kind {
            110 => {
                let (a, b) = (self.v3(de, 1)?, self.v3(de, 4)?);
                let d = b - a;
                let len = d.length();
                let dir = d.try_normalize().ok_or("zero-length line")?;
                (Curve::Line { origin: a, dir }, (0.0, len))
            }
            100 => {
                let z = self.f(de, 1)?;
                let c = DVec3::new(self.f(de, 2)?, self.f(de, 3)?, z);
                let s = DVec3::new(self.f(de, 4)?, self.f(de, 5)?, z);
                let t = DVec3::new(self.f(de, 6)?, self.f(de, 7)?, z);
                let r = (s - c).length();
                if r.is_nan() || r <= 0.0 {
                    return Err("zero-radius arc".into());
                }
                let frame = Axes::new(c, Some(DVec3::Z), Some(s - c));
                let l = frame.local(t);
                let mut end = l.y.atan2(l.x).rem_euclid(std::f64::consts::TAU);
                if end <= 1.0e-9 {
                    end = std::f64::consts::TAU;
                }
                (Curve::Circle { frame, radius: r }, (0.0, end))
            }
            104 => {
                if e.form != 1 {
                    return Err("parabolic and hyperbolic arcs".into());
                }
                let k: Vec<f64> = (1..=6).map(|i| self.f(de, i)).collect::<Result<_, _>>()?;
                let z = self.f(de, 7)?;
                let s = DVec3::new(self.f(de, 8)?, self.f(de, 9)?, z);
                let t = DVec3::new(self.f(de, 10)?, self.f(de, 11)?, z);
                let (frame, a, b) = ellipse(&k, z).ok_or("not an ellipse")?;
                let angle = |p: DVec3| {
                    let l = frame.local(p);
                    (l.y / b).atan2(l.x / a)
                };
                let t0 = angle(s);
                let mut t1 = angle(t);
                while t1 <= t0 + 1.0e-9 {
                    t1 += std::f64::consts::TAU;
                }
                (Curve::Ellipse { frame, a, b }, (t0, t1))
            }
            126 => {
                let k = self.n(de, 1)?;
                let deg = self.n(de, 2)?;
                if k < 0 || !(1..=25).contains(&deg) || k > 1_000_000 {
                    return Err("bad B-spline curve size".into());
                }
                let (k, deg) = (k as usize, deg as usize);
                let nk = k + deg + 2;
                let knots: Vec<f64> = (0..nk).map(|i| self.f(de, 7 + i)).collect::<Result<_, _>>()?;
                let w0 = 7 + nk;
                let weights: Vec<f64> = (0..=k).map(|i| self.f(de, w0 + i)).collect::<Result<_, _>>()?;
                let p0 = w0 + k + 1;
                let pts: Vec<DVec3> = (0..=k).map(|i| self.v3(de, p0 + 3 * i)).collect::<Result<_, _>>()?;
                let r0 = p0 + 3 * (k + 1);
                let c = NurbsCurve::new(deg, knots, &pts, Some(&weights)).map_err(|e| e.to_string())?;
                let (lo, hi) = c.domain();
                let v0 = self.f(de, r0).unwrap_or(lo).clamp(lo, hi);
                let v1 = self.f(de, r0 + 1).unwrap_or(hi).clamp(lo, hi);
                (Curve::Nurbs(c), if v1 > v0 { (v0, v1) } else { (lo, hi) })
            }
            106 => {
                let ip = self.n(de, 1)?;
                let n = self.count(de, 2, 2)?;
                let pts: Vec<DVec3> = match ip {
                    1 => {
                        let z = self.f(de, 3)?;
                        (0..n)
                            .map(|i| Ok(DVec3::new(self.f(de, 4 + 2 * i)?, self.f(de, 5 + 2 * i)?, z)))
                            .collect::<Result<_, Why>>()?
                    }
                    2 => (0..n).map(|i| self.v3(de, 3 + 3 * i)).collect::<Result<_, _>>()?,
                    3 => (0..n).map(|i| self.v3(de, 3 + 6 * i)).collect::<Result<_, _>>()?,
                    _ => return Err("copious data of an unknown kind".into()),
                };
                if pts.len() < 2 {
                    return Err("a polyline needs two points".into());
                }
                let last = (pts.len() - 1) as f64;
                (Curve::Polyline(pts), (0.0, last))
            }
            112 => {
                let pts = self.spline_points(de)?;
                let last = (pts.len().saturating_sub(1)) as f64;
                (Curve::Polyline(pts), (0.0, last))
            }
            102 => {
                let n = self.count(de, 1, 1)?;
                let mut pts: Vec<DVec3> = Vec::new();
                for i in 0..n {
                    let sub = self.ptr(de, 2 + i)?;
                    let mut p = self.curve_points(sub, depth + 1)?;
                    if pts.last().zip(p.first()).is_some_and(|(a, b)| (*a - *b).length() <= self.tol) {
                        p.remove(0);
                    }
                    pts.extend(p);
                }
                if pts.len() < 2 {
                    return Err("empty composite curve".into());
                }
                let last = (pts.len() - 1) as f64;
                // Already in model space: the composite's own matrix applies below.
                (Curve::Polyline(pts), (0.0, last))
            }
            k => return Err(format!("curve type {k}")),
        };
        Ok((if m == DMat4::IDENTITY { c } else { c.transformed(&m) }, range))
    }

    /// A parametric spline curve (112) evaluated along its segments.
    fn spline_points(&self, de: usize) -> Result<Vec<DVec3>, Why> {
        let n = self.count(de, 4, 12)?;
        let t: Vec<f64> = (0..=n).map(|i| self.f(de, 5 + i)).collect::<Result<_, _>>()?;
        let c0 = 6 + n;
        let mut pts = Vec::new();
        for s in 0..n {
            let base = c0 + 12 * s;
            let co: Vec<f64> = (0..12).map(|i| self.f(de, base + i)).collect::<Result<_, _>>()?;
            let len = t.get(s + 1).zip(t.get(s)).map_or(0.0, |(b, a)| b - a);
            let at = |x: f64| {
                let x = x * len;
                DVec3::new(
                    co[0] + x * (co[1] + x * (co[2] + x * co[3])),
                    co[4] + x * (co[5] + x * (co[6] + x * co[7])),
                    co[8] + x * (co[9] + x * (co[10] + x * co[11])),
                )
            };
            let seeds: Vec<f64> = (0..=4).map(|k| f64::from(k) / 4.0).collect();
            let mut seg = adaptive_points(&seeds, self.tol, &at);
            if !pts.is_empty() {
                seg.remove(0);
            }
            pts.extend(seg);
        }
        if pts.len() < 2 {
            return Err("empty parametric spline".into());
        }
        Ok(pts)
    }

    /// A curve's points from its start to its end, within the tolerance.
    fn curve_points(&self, de: usize, depth: usize) -> Result<Vec<DVec3>, Why> {
        let (c, (a, b)) = self.curve(de, depth)?;
        if let Curve::Line { .. } = c {
            return Ok(vec![c.point(a), c.point(b)]);
        }
        Ok(c.sample(a.min(b), a.max(b), self.tol).into_iter().map(|t| c.point(t)).collect())
    }

    /// A surface in model space and its natural parameter box, when it has one.
    fn surface(&self, de: usize, depth: usize) -> Result<Bounded, Why> {
        if depth > MAX_DEPTH {
            return Err("surfaces nest too deeply".into());
        }
        let e = self.entry(de).ok_or_else(|| format!("missing entity {de}"))?.clone();
        let m = self.matrix(e.transform, 0);
        // Location, axis and reference direction by parameter index (None: absent).
        let frame = |loc: usize, axis: Option<usize>, refd: Option<usize>| -> Result<Axes, Why> {
            let o = self.point_entity(self.ptr(de, loc)?)?;
            let dir = |i: Option<usize>| {
                i.and_then(|i| self.ptr(de, i).ok()).filter(|p| *p > 0).and_then(|p| self.direction_entity(p).ok())
            };
            Ok(Axes::new(o, dir(axis), dir(refd)))
        };
        let form1 = |i: usize| (e.form == 1).then_some(i);
        let (s, bx) = match e.kind {
            128 => {
                let (k1, k2, m1, m2) = (self.n(de, 1)?, self.n(de, 2)?, self.n(de, 3)?, self.n(de, 4)?);
                if k1 < 0
                    || k2 < 0
                    || !(1..=25).contains(&m1)
                    || !(1..=25).contains(&m2)
                    || (k1 + 1) * (k2 + 1) > 4_000_000
                {
                    return Err("bad B-spline surface size".into());
                }
                let (k1, k2, m1, m2) = (k1 as usize, k2 as usize, m1 as usize, m2 as usize);
                let (n1, n2) = (k1 + m1 + 2, k2 + m2 + 2);
                let s0 = 10;
                let ku: Vec<f64> = (0..n1).map(|i| self.f(de, s0 + i)).collect::<Result<_, _>>()?;
                let kv: Vec<f64> = (0..n2).map(|i| self.f(de, s0 + n1 + i)).collect::<Result<_, _>>()?;
                let w0 = s0 + n1 + n2;
                let count = (k1 + 1) * (k2 + 1);
                let p0 = w0 + count;
                let mut pts = vec![vec![DVec3::ZERO; k2 + 1]; k1 + 1];
                let mut ws = vec![vec![1.0; k2 + 1]; k1 + 1];
                for j in 0..=k2 {
                    for i in 0..=k1 {
                        let idx = j * (k1 + 1) + i;
                        pts[i][j] = self.v3(de, p0 + 3 * idx)?;
                        ws[i][j] = self.f(de, w0 + idx)?;
                    }
                }
                let r0 = p0 + 3 * count;
                let n = NurbsSurface::new(m1, m2, ku, kv, &pts, Some(&ws)).map_err(|e| e.to_string())?;
                let ((ul, uh), (vl, vh)) = n.domain();
                let range = |a: usize, lo: f64, hi: f64| {
                    let (x, y) =
                        (self.f(de, a).unwrap_or(lo).clamp(lo, hi), self.f(de, a + 1).unwrap_or(hi).clamp(lo, hi));
                    if y > x { (x, y) } else { (lo, hi) }
                };
                let bx = [range(r0, ul, uh), range(r0 + 2, vl, vh)];
                (Surface::Nurbs(n), Some(bx))
            }
            108 => {
                let n = DVec3::new(self.f(de, 1)?, self.f(de, 2)?, self.f(de, 3)?);
                let d = self.f(de, 4)?;
                let l2 = n.length_squared();
                if l2.is_nan() || l2 <= 0.0 {
                    return Err("plane without a normal".into());
                }
                (Surface::Plane(Axes::new(n * (d / l2), Some(n), None)), None)
            }
            190 => (Surface::Plane(frame(1, Some(2), form1(3))?), None),
            192 => (Surface::Cylinder { frame: frame(1, Some(2), form1(4))?, radius: self.f(de, 3)? }, None),
            194 => {
                let f = frame(1, Some(2), form1(5))?;
                (Surface::Cone { frame: f, radius: self.f(de, 3)?, tan: self.f(de, 4)?.to_radians().tan() }, None)
            }
            196 => (Surface::Sphere { frame: frame(1, form1(3), form1(4))?, radius: self.f(de, 2)? }, None),
            198 => (
                Surface::Torus { frame: frame(1, Some(2), form1(5))?, major: self.f(de, 3)?, minor: self.f(de, 4)? },
                None,
            ),
            118 => {
                let (a, ra) = self.curve(self.ptr(de, 1)?, depth + 1)?;
                let (b, rb) = self.curve(self.ptr(de, 2)?, depth + 1)?;
                let rb = if self.n(de, 3)? == 1 { (rb.1, rb.0) } else { rb };
                (Surface::Ruled { a: Box::new(a), ra, b: Box::new(b), rb }, Some([(0.0, 1.0), (0.0, 1.0)]))
            }
            120 => {
                let (axis, ar) = self.curve(self.ptr(de, 1)?, depth + 1)?;
                let Curve::Line { origin, dir } = axis else { return Err("revolution axis is not a line".into()) };
                let _ = ar;
                let (c, cr) = self.curve(self.ptr(de, 2)?, depth + 1)?;
                let (sa, ta) = (self.f(de, 3)?, self.f(de, 4)?);
                (Surface::Revolution { curve: Box::new(c), origin, axis: dir }, Some([cr, (sa, ta)]))
            }
            122 => {
                let (c, cr) = self.curve(self.ptr(de, 1)?, depth + 1)?;
                let end = self.v3(de, 2)?;
                let d = end - c.point(cr.0);
                let dir = d.try_normalize().ok_or("tabulated cylinder without a direction")?;
                (Surface::Extrusion { curve: Box::new(c), dir }, Some([cr, (0.0, d.length())]))
            }
            140 => {
                let d = self.f(de, 4)?;
                let (base, bx) = self.surface(self.ptr(de, 5)?, depth + 1)?;
                (Surface::Offset { base: Box::new(base), distance: d }, bx)
            }
            k => return Err(format!("surface type {k}")),
        };
        Ok((if m == DMat4::IDENTITY { s } else { s.transformed(&m) }, bx))
    }

    /// The boundary of a surface's natural parameter box, as a 3D ring.
    fn natural_loop(&self, s: &Surface, bx: [(f64, f64); 2]) -> Vec<DVec3> {
        let [(u0, u1), (v0, v1)] = bx;
        let sides = [((u0, v0), (u1, v0)), ((u1, v0), (u1, v1)), ((u1, v1), (u0, v1)), ((u0, v1), (u0, v0))];
        let mut out = Vec::new();
        for ((a0, b0), (a1, b1)) in sides {
            let at = |t: f64| s.point(a0 + (a1 - a0) * t, b0 + (b1 - b0) * t);
            let seeds: Vec<f64> = (0..=8).map(|k| f64::from(k) / 8.0).collect();
            let mut pts = adaptive_points(&seeds, self.tol, &at);
            pts.pop();
            out.extend(pts);
        }
        out
    }

    /// A boundary curve: a curve on a surface (142) by its model-space curve, else by its
    /// parameter-space curve mapped through a B-spline surface.
    fn boundary(&self, de: usize, surface: &Surface) -> Result<Vec<DVec3>, Why> {
        let e = self.entry(de).ok_or_else(|| format!("missing entity {de}"))?;
        if e.kind != 142 {
            return self.curve_points(de, 0);
        }
        let model = self.ptr(de, 4).unwrap_or(0);
        if model > 0
            && let Ok(p) = self.curve_points(model, 0)
        {
            return Ok(p);
        }
        let pcurve = self.ptr(de, 3).unwrap_or(0);
        if pcurve == 0 || !matches!(surface, Surface::Nurbs(_) | Surface::Ruled { .. }) {
            return Err("a boundary given only in the parameters of an analytic surface".into());
        }
        let (c, (a, b)) = self.curve(pcurve, 0)?;
        let at = |t: f64| {
            let q = c.point(a + (b - a) * t);
            surface.point(q.x, q.y)
        };
        let seeds: Vec<f64> = (0..=32).map(|k| f64::from(k) / 32.0).collect();
        Ok(adaptive_points(&seeds, self.tol, &at))
    }

    fn trimmed(&self, de: usize) -> Result<FaceJob, Why> {
        let (surface, bx) = self.surface(self.ptr(de, 1)?, 0)?;
        let outer_given = self.n(de, 2)? != 0;
        let holes = self.count(de, 3, 1)?;
        let mut loops = Vec::new();
        if outer_given {
            loops.push(self.boundary(self.ptr(de, 4)?, &surface)?);
        } else {
            let bx = bx.ok_or("an untrimmed unbounded surface")?;
            loops.push(self.natural_loop(&surface, bx));
        }
        for i in 0..holes {
            if let Ok(h) = self.ptr(de, 5 + i).and_then(|p| self.boundary(p, &surface)) {
                loops.push(h);
            }
        }
        Ok(FaceJob { surface, loops, reversed: false, outer_first: true, side_unknown: true })
    }

    fn bounded(&self, de: usize) -> Result<FaceJob, Why> {
        let (surface, _) = self.surface(self.ptr(de, 2)?, 0)?;
        let n = self.count(de, 3, 1)?;
        let mut loops = Vec::new();
        for i in 0..n {
            let b = self.ptr(de, 4 + i)?;
            // Boundary (141): type, preference, surface, count, then (curve, sense, k, pcurves…).
            let m = self.count(b, 4, 3)?;
            let mut ring: Vec<DVec3> = Vec::new();
            let mut at = 5;
            for _ in 0..m {
                let curve = self.ptr(b, at)?;
                let sense = self.n(b, at + 1)?;
                let k = self.count(b, at + 2, 1)?;
                at += 3 + k;
                let mut p = self.curve_points(curve, 0)?;
                if sense == 2 {
                    p.reverse();
                }
                p.pop();
                ring.extend(p);
            }
            loops.push(ring);
        }
        Ok(FaceJob { surface, loops, reversed: false, outer_first: true, side_unknown: true })
    }

    /// An edge of an edge list (504) from its start vertex to its end vertex.
    fn edge(&mut self, list: usize, index: usize) -> Result<Vec<DVec3>, Why> {
        if let Some(e) = self.edges.get(&(list, index)) {
            return Ok(e.clone());
        }
        let base = 2 + 5 * index.checked_sub(1).ok_or("edge index 0")?;
        let curve = self.ptr(list, base)?;
        let vertex = |s: &Self, vl: usize, vi: usize| -> Result<DVec3, Why> {
            let i = vi.checked_sub(1).ok_or("vertex index 0")?;
            Ok(s.placed(vl).transform_point3(s.v3(vl, 2 + 3 * i)?))
        };
        let a = vertex(self, self.ptr(list, base + 1)?, self.ptr(list, base + 2)?)?;
        let b = vertex(self, self.ptr(list, base + 3)?, self.ptr(list, base + 4)?)?;
        let mut pts = match self.curve_points(curve, 0) {
            Ok(p) => p,
            Err(why) => {
                self.note(why);
                vec![a, b]
            }
        };
        let (Some(&first), Some(&last)) = (pts.first(), pts.last()) else { return Ok(vec![a, b]) };
        if (first - a).length() + (last - b).length() > (first - b).length() + (last - a).length() {
            pts.reverse();
        }
        if let Some(p) = pts.first_mut() {
            *p = a;
        }
        if let Some(p) = pts.last_mut() {
            *p = b;
        }
        self.edges.insert((list, index), pts.clone());
        Ok(pts)
    }

    /// A B-rep face (510), turned over when `flip`.
    fn brep_face(&mut self, de: usize, flip: bool) -> Result<FaceJob, Why> {
        let (surface, _) = self.surface(self.ptr(de, 1)?, 0)?;
        let n = self.count(de, 2, 1)?;
        let outer = self.n(de, 3)? == 1;
        let mut loops = Vec::new();
        for i in 0..n {
            let lp = self.ptr(de, 4 + i)?;
            let m = self.count(lp, 1, 5)?;
            let mut ring: Vec<DVec3> = Vec::new();
            let mut at = 2;
            for _ in 0..m {
                let kind = self.n(lp, at)?;
                let list = self.ptr(lp, at + 1)?;
                let index = self.ptr(lp, at + 2)?;
                let forward = self.n(lp, at + 3)? == 1;
                let k = self.count(lp, at + 4, 2)?;
                at += 5 + 2 * k;
                if kind != 0 {
                    continue;
                }
                let mut p = self.edge(list, index)?;
                if !forward {
                    p.reverse();
                }
                p.pop();
                ring.extend(p);
            }
            if ring.len() >= 3 {
                loops.push(ring);
            }
        }
        Ok(FaceJob { surface, loops, reversed: flip, outer_first: outer, side_unknown: false })
    }

    /// The faces of a shell (514), each turned over as its flag and `flip` say.
    fn shell(&mut self, de: usize, flip: bool, jobs: &mut Vec<FaceJob>, failed: &mut usize) {
        let Ok(n) = self.count(de, 1, 2) else { return };
        for i in 0..n {
            let (Ok(face), Ok(of)) = (self.ptr(de, 2 + 2 * i), self.n(de, 3 + 2 * i)) else { continue };
            match self.brep_face(face, flip != (of != 1)) {
                Ok(j) => jobs.push(j),
                Err(why) => {
                    self.note(why);
                    *failed += 1;
                }
            }
        }
    }

    /// Faces of one entity that makes geometry by itself.
    fn faces_of(&mut self, de: usize, jobs: &mut Vec<FaceJob>, failed: &mut usize) {
        let Some(e) = self.entry(de).cloned() else { return };
        let job = match e.kind {
            144 => self.trimmed(de),
            143 => self.bounded(de),
            186 => {
                let Ok(shell) = self.ptr(de, 1) else { return };
                let sof = self.n(de, 2).unwrap_or(1) == 1;
                self.shell(shell, !sof, jobs, failed);
                if let Ok(n) = self.count(de, 3, 2) {
                    for i in 0..n {
                        if let (Ok(v), Ok(vof)) = (self.ptr(de, 4 + 2 * i), self.n(de, 5 + 2 * i)) {
                            self.shell(v, vof != 1, jobs, failed);
                        }
                    }
                }
                return;
            }
            514 => {
                self.shell(de, false, jobs, failed);
                return;
            }
            128 | 118 | 120 | 122 | 140 => self.surface(de, 0).and_then(|(s, bx)| {
                let bx = bx.ok_or("an unbounded surface")?;
                let ring = self.natural_loop(&s, bx);
                Ok(FaceJob { surface: s, loops: vec![ring], reversed: false, outer_first: true, side_unknown: true })
            }),
            _ => return,
        };
        match job {
            Ok(j) => jobs.push(j),
            Err(why) => {
                self.note(why);
                *failed += 1;
            }
        }
    }
}

/// The centre, axes and semi-axes of the ellipse `A x² + B xy + C y² + D x + E y + F = 0` at
/// height `z`.
fn ellipse(k: &[f64], z: f64) -> Option<(Axes, f64, f64)> {
    let [a, b, c, d, e, f] = <[f64; 6]>::try_from(k).ok()?;
    let det = 4.0 * a * c - b * b;
    if det.is_nan() || det <= 0.0 {
        return None;
    }
    let cx = (b * e - 2.0 * c * d) / det;
    let cy = (b * d - 2.0 * a * e) / det;
    let f0 = a * cx * cx + b * cx * cy + c * cy * cy + d * cx + e * cy + f;
    let theta = 0.5 * b.atan2(a - c);
    let (s, co) = theta.sin_cos();
    let l1 = a * co * co + b * co * s + c * s * s;
    let l2 = a * s * s - b * co * s + c * co * co;
    let (ra, rb) = ((-f0 / l1).sqrt(), (-f0 / l2).sqrt());
    if ra.is_nan() || rb.is_nan() || ra <= 0.0 || rb <= 0.0 {
        return None;
    }
    Some((Axes::new(DVec3::new(cx, cy, z), Some(DVec3::Z), Some(DVec3::new(co, s, 0.0))), ra, rb))
}

pub fn read(bytes: &[u8], options: Options) -> Result<Imported, ImportError> {
    let text = String::from_utf8_lossy(bytes);
    let mut g = String::new();
    let mut d: Vec<String> = Vec::new();
    let mut p: Vec<String> = Vec::new();
    for line in text.lines() {
        let line: String = line.trim_end_matches(['\r', '\n']).chars().collect();
        let chars: Vec<char> = line.chars().collect();
        if chars.len() < 73 {
            continue;
        }
        let body: String = chars.get(..72).map(|c| c.iter().collect()).unwrap_or_default();
        match chars.get(72) {
            Some('G') => g.push_str(&body),
            Some('D') => d.push(body),
            Some('P') => p.push(body),
            Some('C') => return Err(ImportError::Unsupported("compressed IGES".into())),
            _ => {}
        }
    }
    if d.is_empty() {
        return Err(bad("no directory section; this is not an IGES file"));
    }
    // Global section: delimiters first (as Hollerith "1H,"), then the fields.
    let gt = g.trim_start();
    let pd = if gt.starts_with("1H") { gt.chars().nth(2).unwrap_or(',') } else { ',' };
    let after = if gt.starts_with("1H") {
        gt.get(3..).unwrap_or_default().trim_start_matches(pd)
    } else {
        gt.trim_start_matches(pd)
    };
    let rd = if after.starts_with("1H") { after.chars().nth(2).unwrap_or(';') } else { ';' };
    let global = tokens(gt, pd, rd);
    let field = |i: usize| global.get(i).cloned().unwrap_or_default();
    let scale = real(Some(&field(12))).filter(|s| *s > 0.0).unwrap_or(1.0);
    let flag = int(Some(&field(13))).unwrap_or(2);
    let unit_name = field(14).to_ascii_uppercase();
    let mm_per_unit = match flag {
        1 => 25.4,
        2 => 1.0,
        4 => 304.8,
        5 => 1_609_344.0,
        6 => 1000.0,
        7 => 1_000_000.0,
        8 => 0.0254,
        9 => 0.001,
        10 => 10.0,
        11 => 0.0000254,
        _ => match unit_name.trim_matches(|c: char| !c.is_ascii_alphanumeric()) {
            "IN" | "INCH" => 25.4,
            "FT" => 304.8,
            "M" => 1000.0,
            "CM" => 10.0,
            "UM" => 0.001,
            _ => 1.0,
        },
    } / scale;

    let mut de: HashMap<usize, Entry> = HashMap::new();
    for (k, pair) in d.chunks(2).enumerate() {
        let [l1, l2] = pair else { continue };
        let fields = |l: &str| -> Vec<String> {
            let c: Vec<char> = l.chars().collect();
            (0..9)
                .map(|i| {
                    c.get(i * 8..(i * 8 + 8).min(c.len())).map(|f| f.iter().collect::<String>()).unwrap_or_default()
                })
                .collect()
        };
        let (a, b) = (fields(l1), fields(l2));
        let num = |v: &[String], i: usize| v.get(i).and_then(|t| t.trim().parse::<i64>().ok()).unwrap_or(0);
        let status: Vec<char> = a.get(8).map(|s| s.replace(' ', "0").chars().collect()).unwrap_or_default();
        let pair_at = |i: usize| -> u32 {
            status
                .get(i..i + 2)
                .map(|c| c.iter().filter_map(|d| d.to_digit(10)).fold(0, |acc, x| acc * 10 + x))
                .unwrap_or(0)
        };
        de.insert(
            2 * k + 1,
            Entry {
                kind: num(&a, 0),
                param: num(&a, 1).max(0) as usize,
                lines: num(&b, 3).max(0) as usize,
                transform: num(&a, 6).unsigned_abs() as usize,
                form: num(&b, 4),
                blanked: pair_at(0) == 1,
                dependent: pair_at(2) & 1 == 1,
            },
        );
    }
    let mut params: HashMap<usize, Vec<String>> = HashMap::new();
    for (&n, e) in &de {
        if e.param == 0 {
            continue;
        }
        let lines = p.get(e.param - 1..(e.param - 1 + e.lines.max(1)).min(p.len())).unwrap_or_default();
        let joined: String = lines.iter().map(|l| l.chars().take(64).collect::<String>()).collect();
        params.insert(n, tokens(&joined, pd, rd));
    }
    let mut ig =
        Iges { de, params, tol: options.tolerance / mm_per_unit, unknown: HashMap::new(), edges: HashMap::new() };

    // Independent, visible geometry; subfigure instances place their definitions.
    let mut order: Vec<usize> = ig.de.keys().copied().collect();
    order.sort_unstable();
    let mut placements: Vec<(usize, DMat4)> = Vec::new();
    let mut referenced: HashSet<usize> = HashSet::new();
    for &n in &order {
        if let Some(e) = ig.entry(n)
            && e.kind == 308
            && let Ok(count) = ig.count(n, 3, 1)
        {
            for i in 0..count {
                if let Ok(c) = ig.ptr(n, 4 + i) {
                    referenced.insert(c);
                }
            }
        }
    }
    for &n in &order {
        let Some(e) = ig.entry(n) else { continue };
        if e.blanked || referenced.contains(&n) {
            continue;
        }
        if e.kind == 408 {
            let (Ok(def), Ok(t)) = (ig.ptr(n, 1), ig.v3(n, 2)) else { continue };
            let s = ig.f(n, 5).ok().filter(|s| *s > 0.0).unwrap_or(1.0);
            let at = ig.placed(n) * DMat4::from_translation(t) * DMat4::from_scale(DVec3::splat(s));
            let mut stack = vec![(def, at, 0usize)];
            while let Some((def, at, depth)) = stack.pop() {
                if depth > MAX_DEPTH || placements.len() >= MAX_PLACED {
                    break;
                }
                let Ok(count) = ig.count(def, 3, 1) else { continue };
                for i in 0..count {
                    let Ok(c) = ig.ptr(def, 4 + i) else { continue };
                    match ig.entry(c).map(|e| e.kind) {
                        Some(408) => {
                            if let (Ok(d2), Ok(t2)) = (ig.ptr(c, 1), ig.v3(c, 2)) {
                                let s2 = ig.f(c, 5).ok().filter(|s| *s > 0.0).unwrap_or(1.0);
                                stack.push((
                                    d2,
                                    at * ig.placed(c)
                                        * DMat4::from_translation(t2)
                                        * DMat4::from_scale(DVec3::splat(s2)),
                                    depth + 1,
                                ));
                            }
                        }
                        Some(_) => placements.push((c, at)),
                        None => {}
                    }
                }
            }
        } else if !e.dependent {
            placements.push((n, DMat4::IDENTITY));
        }
    }
    let mut entities: Vec<usize> = placements.iter().map(|(n, _)| *n).collect();
    entities.sort_unstable();
    entities.dedup();
    let mut jobs = Vec::new();
    let mut ranges: HashMap<usize, (usize, usize)> = HashMap::new();
    let mut failed = 0usize;
    for &n in &entities {
        let start = jobs.len();
        ig.faces_of(n, &mut jobs, &mut failed);
        ranges.insert(n, (start, jobs.len()));
    }
    if jobs.is_empty() {
        let wire = ig.de.values().any(|e| matches!(e.kind, 100 | 102 | 104 | 110 | 112 | 126));
        return Err(if failed > 0 {
            ImportError::Unsupported(format!(
                "this IGES geometry ({})",
                ig.unknown.keys().take(3).cloned().collect::<Vec<_>>().join(", ")
            ))
        } else if wire {
            ImportError::Unsupported("IGES wireframes (curves without surfaces)".into())
        } else {
            bad("no surfaces or solids")
        });
    }
    let mut t = tessellate_faces(&jobs, ig.tol);
    // Trimmed and bounded surfaces say nothing about their outside; B-rep faces do.
    let unknown: Vec<bool> = jobs.iter().map(|j| j.side_unknown).collect();
    orient_outward(&mut t.faces, &unknown);
    let mut soup = Soup::new();
    for (n, at) in &placements {
        let Some(&(a, b)) = ranges.get(n) else { continue };
        for face in t.faces.get(a..b).unwrap_or_default() {
            for tri in face {
                let p = tri.map(|q| {
                    let w = at.transform_point3(q);
                    Vec3::new(w.x as f32, w.y as f32, w.z as f32)
                });
                soup.push(p)?;
            }
        }
    }
    let mut warn = warnings(&t, jobs.len() + failed);
    if failed > 0 {
        warn.push(format!("{failed} surfaces were skipped (unsupported or damaged geometry)"));
    }
    let mut unknown: Vec<(String, usize)> = ig.unknown.into_iter().collect();
    unknown.sort();
    for (what, n) in unknown.into_iter().take(8) {
        warn.push(format!("not supported yet: {what} ({n}×)"));
    }
    let eps = soup.extent() * 1.0e-8;
    let mut mesh = soup.into_mesh_welded_within(eps)?;
    let units = match Units::from_mm(mm_per_unit) {
        Some(u) => u,
        None => {
            mesh = mesh.scaled(mm_per_unit as f32)?;
            Units::Millimetres
        }
    };
    Ok(Imported { mesh, units: Some(units), warnings: warn })
}

#[cfg(test)]
mod tests;
