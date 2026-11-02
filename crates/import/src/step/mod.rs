//! STEP (ISO 10303 AP203, AP214, AP242) boundary-representation models.
//!
//! Solids (`MANIFOLD_SOLID_BREP`, `BREP_WITH_VOIDS`, `FACETED_BREP`) and surface models
//! (`SHELL_BASED_SURFACE_MODEL`) are found through their shape representations and placed
//! by the assembly structure (transformed representation relationships, mapped items). Their
//! faces' exact surfaces (planes, cylinders, cones, spheres, tori, surfaces of revolution and
//! extrusion, offset and B-spline surfaces) are tessellated to the tolerance, every edge
//! sampled once so neighbouring faces meet exactly. AP242 tessellated shapes (triangulated
//! faces and surface sets) are read as they stand when a shape has no exact geometry. The
//! global length and angle units are honoured. Wireframes are not read.

pub mod p21;

use std::collections::{HashMap, HashSet};

use crate::geom::brep::{Axes, Curve, Surface};
use crate::geom::nurbs::{NurbsCurve, NurbsSurface};
use crate::geom::{DMat4, DVec3, Vec3};
use p21::{Instance, P};

use crate::brep::{FaceJob, tessellate_faces, warnings};
use crate::{ImportError, Imported, Options, Soup, Units};

/// Deepest assembly nesting followed.
const MAX_DEPTH: usize = 32;
/// Most solid instances placed.
const MAX_INSTANCES: usize = 100_000;
/// Most expanded knots in one B-spline direction.
const MAX_KNOTS: usize = 200_000;
/// Deepest nesting of curve and surface definitions.
const MAX_GEOMETRY_DEPTH: usize = 16;

type Map = HashMap<u64, Instance>;

/// Why a curve or surface was not understood (kept short; counted for the warnings).
type Why = String;

struct Step<'a> {
    m: &'a Map,
    tol: f64,
    /// Radians per angle unit.
    angle: f64,
    edges: HashMap<u64, Vec<DVec3>>,
    /// Kinds of geometry that were not understood, with counts.
    unknown: HashMap<String, usize>,
}

fn frame_matrix(f: &Axes) -> DMat4 {
    DMat4::from_cols(f.x.extend(0.0), f.y.extend(0.0), f.z.extend(0.0), f.origin.extend(1.0))
}

impl<'a> Step<'a> {
    fn inst(&self, id: u64) -> Option<&'a Instance> {
        self.m.get(&id)
    }

    /// The first record of a simple instance.
    fn simple(&self, id: u64) -> Option<(&'a str, &'a [P])> {
        self.inst(id).and_then(|i| i.parts.first()).map(|(n, a)| (n.as_str(), a.as_slice()))
    }

    fn note(&mut self, what: impl Into<String>) {
        *self.unknown.entry(what.into()).or_insert(0) += 1;
    }

    fn point(&self, p: &P) -> Option<DVec3> {
        let (kind, a) = self.simple(p.reference()?)?;
        if kind != "CARTESIAN_POINT" {
            return None;
        }
        let c: Vec<f64> = a.get(1)?.list().iter().filter_map(P::num).collect();
        let v = DVec3::new(*c.first()?, *c.get(1)?, c.get(2).copied().unwrap_or(0.0));
        v.is_finite().then_some(v)
    }

    fn direction(&self, p: &P) -> Option<DVec3> {
        let (kind, a) = self.simple(p.reference()?)?;
        if kind != "DIRECTION" {
            return None;
        }
        let c: Vec<f64> = a.get(1)?.list().iter().filter_map(P::num).collect();
        DVec3::new(*c.first()?, *c.get(1)?, c.get(2).copied().unwrap_or(0.0)).try_normalize()
    }

    /// A VECTOR's unit direction and magnitude.
    fn vector(&self, p: &P) -> Option<(DVec3, f64)> {
        let (kind, a) = self.simple(p.reference()?)?;
        if kind != "VECTOR" {
            return None;
        }
        Some((self.direction(a.get(1)?)?, a.get(2)?.num()?))
    }

    fn placement(&self, p: &P) -> Option<Axes> {
        let (kind, a) = self.simple(p.reference()?)?;
        match kind {
            "AXIS2_PLACEMENT_3D" => {
                let origin = self.point(a.get(1)?)?;
                Some(Axes::new(
                    origin,
                    a.get(2).and_then(|d| self.direction(d)),
                    a.get(3).and_then(|d| self.direction(d)),
                ))
            }
            "AXIS2_PLACEMENT_2D" => {
                let origin = self.point(a.get(1)?)?;
                Some(Axes::new(origin, None, a.get(2).and_then(|d| self.direction(d))))
            }
            _ => None,
        }
    }

    /// A transformation operator or placement as a matrix.
    fn transform_item(&self, p: &P) -> Option<DMat4> {
        if let Some(f) = self.placement(p) {
            return Some(frame_matrix(&f));
        }
        let inst = self.inst(p.reference()?)?;
        let a = inst
            .part("CARTESIAN_TRANSFORMATION_OPERATOR_3D")
            .or_else(|| inst.part("CARTESIAN_TRANSFORMATION_OPERATOR"))?;
        // (name, description, axis1, axis2, local_origin, scale[, axis3]) in a simple instance.
        let off = if a.len() >= 6 { 2 } else { 0 };
        let x = a.get(off).and_then(|d| self.direction(d)).unwrap_or(DVec3::X);
        let y = a.get(off + 1).and_then(|d| self.direction(d)).unwrap_or(DVec3::Y);
        let o = a.get(off + 2).and_then(|d| self.point(d)).unwrap_or(DVec3::ZERO);
        let s = a.get(off + 3).and_then(P::num).unwrap_or(1.0);
        let z = a.get(off + 4).and_then(|d| self.direction(d)).unwrap_or_else(|| x.cross(y));
        Some(DMat4::from_cols((x * s).extend(0.0), (y * s).extend(0.0), (z * s).extend(0.0), o.extend(1.0)))
    }

    fn nums(p: Option<&P>) -> Vec<f64> {
        p.map(|l| l.list().iter().filter_map(P::num).collect()).unwrap_or_default()
    }

    fn expand_knots(mults: &[f64], knots: &[f64]) -> Result<Vec<f64>, Why> {
        let mut out = Vec::new();
        for (m, k) in mults.iter().zip(knots) {
            if m.is_nan() || *m < 0.0 || out.len() + *m as usize > MAX_KNOTS {
                return Err("bad knot multiplicities".into());
            }
            out.extend(std::iter::repeat_n(*k, *m as usize));
        }
        Ok(out)
    }

    /// Knots for B-splines given by form (Bézier, uniform, quasi-uniform) rather than values.
    fn implied_knots(inst: &Instance, degree: usize, count: usize) -> Option<Vec<f64>> {
        let n = count + degree + 1;
        if inst.is("BEZIER_CURVE") || inst.is("BEZIER_SURFACE") {
            let mut k = vec![0.0; degree + 1];
            k.extend(vec![1.0; count.saturating_sub(degree) + degree]);
            k.truncate(n);
            Some(k)
        } else if inst.is("UNIFORM_CURVE") || inst.is("UNIFORM_SURFACE") {
            Some((0..n).map(|i| i as f64 - degree as f64).collect())
        } else if inst.is("QUASI_UNIFORM_CURVE") || inst.is("QUASI_UNIFORM_SURFACE") {
            let inner = count.saturating_sub(degree);
            let mut k = vec![0.0; degree + 1];
            for i in 1..inner {
                k.push(i as f64);
            }
            k.extend(vec![inner as f64; degree + 1]);
            Some(k)
        } else {
            None
        }
    }

    fn bspline_curve(&self, inst: &Instance) -> Result<NurbsCurve, Why> {
        let simple = inst.parts.len() == 1;
        let (degree, pts, knots) = if simple {
            let (name, a) = inst.parts.first().map(|(n, a)| (n.as_str(), a.as_slice())).ok_or("empty")?;
            let degree = a.get(1).and_then(P::num).ok_or("no degree")? as usize;
            let pts: Vec<DVec3> =
                a.get(2).map(|l| l.list().iter().filter_map(|p| self.point(p)).collect()).unwrap_or_default();
            let knots = if name == "B_SPLINE_CURVE_WITH_KNOTS" {
                Self::expand_knots(&Self::nums(a.get(6)), &Self::nums(a.get(7)))?
            } else {
                Self::implied_knots(inst, degree, pts.len()).ok_or("unknown B-spline form")?
            };
            (degree, pts, knots)
        } else {
            let b = inst.part("B_SPLINE_CURVE").ok_or("no B_SPLINE_CURVE record")?;
            let degree = b.first().and_then(P::num).ok_or("no degree")? as usize;
            let pts: Vec<DVec3> =
                b.get(1).map(|l| l.list().iter().filter_map(|p| self.point(p)).collect()).unwrap_or_default();
            let knots = match inst.part("B_SPLINE_CURVE_WITH_KNOTS") {
                Some(k) => Self::expand_knots(&Self::nums(k.first()), &Self::nums(k.get(1)))?,
                None => Self::implied_knots(inst, degree, pts.len()).ok_or("unknown B-spline form")?,
            };
            (degree, pts, knots)
        };
        let weights = inst.part("RATIONAL_B_SPLINE_CURVE").map(|w| Self::nums(w.first()));
        NurbsCurve::new(degree, knots, &pts, weights.as_deref()).map_err(|e| e.to_string())
    }

    fn bspline_surface(&self, inst: &Instance) -> Result<NurbsSurface, Why> {
        let simple = inst.parts.len() == 1;
        let grid = |p: Option<&P>| -> Vec<Vec<DVec3>> {
            p.map(|rows| rows.list().iter().map(|r| r.list().iter().filter_map(|q| self.point(q)).collect()).collect())
                .unwrap_or_default()
        };
        let (du, dv, pts, ku, kv) = if simple {
            let (name, a) = inst.parts.first().map(|(n, a)| (n.as_str(), a.as_slice())).ok_or("empty")?;
            let du = a.get(1).and_then(P::num).ok_or("no degree")? as usize;
            let dv = a.get(2).and_then(P::num).ok_or("no degree")? as usize;
            let pts = grid(a.get(3));
            let (nu, nv) = (pts.len(), pts.first().map_or(0, Vec::len));
            if name == "B_SPLINE_SURFACE_WITH_KNOTS" {
                let ku = Self::expand_knots(&Self::nums(a.get(8)), &Self::nums(a.get(10)))?;
                let kv = Self::expand_knots(&Self::nums(a.get(9)), &Self::nums(a.get(11)))?;
                (du, dv, pts, ku, kv)
            } else {
                let ku = Self::implied_knots(inst, du, nu).ok_or("unknown B-spline form")?;
                let kv = Self::implied_knots(inst, dv, nv).ok_or("unknown B-spline form")?;
                (du, dv, pts, ku, kv)
            }
        } else {
            let b = inst.part("B_SPLINE_SURFACE").ok_or("no B_SPLINE_SURFACE record")?;
            let du = b.first().and_then(P::num).ok_or("no degree")? as usize;
            let dv = b.get(1).and_then(P::num).ok_or("no degree")? as usize;
            let pts = grid(b.get(2));
            let (nu, nv) = (pts.len(), pts.first().map_or(0, Vec::len));
            match inst.part("B_SPLINE_SURFACE_WITH_KNOTS") {
                Some(k) => {
                    let ku = Self::expand_knots(&Self::nums(k.first()), &Self::nums(k.get(2)))?;
                    let kv = Self::expand_knots(&Self::nums(k.get(1)), &Self::nums(k.get(3)))?;
                    (du, dv, pts, ku, kv)
                }
                None => {
                    let ku = Self::implied_knots(inst, du, nu).ok_or("unknown B-spline form")?;
                    let kv = Self::implied_knots(inst, dv, nv).ok_or("unknown B-spline form")?;
                    (du, dv, pts, ku, kv)
                }
            }
        };
        let weights: Option<Vec<Vec<f64>>> = inst.part("RATIONAL_B_SPLINE_SURFACE").map(|w| {
            w.first().map(|rows| rows.list().iter().map(|r| Self::nums(Some(r))).collect()).unwrap_or_default()
        });
        NurbsSurface::new(du, dv, ku, kv, &pts, weights.as_deref()).map_err(|e| e.to_string())
    }

    /// Points along a trimmed or composite curve, for use as a polyline.
    fn curve_points(&mut self, id: u64, depth: usize) -> Result<Vec<DVec3>, Why> {
        if depth > MAX_GEOMETRY_DEPTH {
            return Err("curves nest too deeply".into());
        }
        let (kind, a) = self.simple(id).ok_or("missing curve")?;
        match kind {
            "COMPOSITE_CURVE" | "COMPOSITE_CURVE_ON_SURFACE" => {
                let mut pts: Vec<DVec3> = Vec::new();
                for seg in a.get(1).map(P::list).unwrap_or_default() {
                    let Some((_, s)) = seg.reference().and_then(|r| self.simple(r)) else { continue };
                    let same = s.get(1).and_then(P::flag).unwrap_or(true);
                    let parent = s.get(2).and_then(P::reference).ok_or("segment without a curve")?;
                    let mut p = self.curve_points(parent, depth + 1)?;
                    if !same {
                        p.reverse();
                    }
                    if pts.last().zip(p.first()).is_some_and(|(a, b)| (*a - *b).length() <= self.tol) {
                        p.remove(0);
                    }
                    pts.extend(p);
                }
                Ok(pts)
            }
            "TRIMMED_CURVE" => {
                let basis = a.get(1).and_then(P::reference).ok_or("trimmed curve without a basis")?;
                let curve = self.curve(basis, depth + 1)?;
                let trim = |t: Option<&P>, s: &Self| -> Option<f64> {
                    let l = t?.list();
                    if let Some(p) = l.iter().find_map(|v| s.point(v)) {
                        return Some(curve.closest(p));
                    }
                    l.iter()
                        .find_map(|v| match v {
                            P::Typed(n, x) if n == "PARAMETER_VALUE" => x.first().and_then(P::num),
                            _ => None,
                        })
                        .map(|t| if curve.period().is_some() { t * s.angle } else { t })
                };
                let (t1, t2) = (trim(a.get(2), self).ok_or("bad trim")?, trim(a.get(3), self).ok_or("bad trim")?);
                let sense = a.get(4).and_then(P::flag).unwrap_or(true);
                let (mut lo, mut hi) = if sense { (t1, t2) } else { (t2, t1) };
                if let Some(per) = curve.period() {
                    while hi <= lo {
                        hi += per;
                    }
                } else if hi < lo {
                    std::mem::swap(&mut lo, &mut hi);
                }
                let mut pts: Vec<DVec3> = curve.sample(lo, hi, self.tol).into_iter().map(|t| curve.point(t)).collect();
                if !sense {
                    pts.reverse();
                }
                Ok(pts)
            }
            _ => {
                let curve = self.curve(id, depth + 1)?;
                let (lo, hi) = curve.domain().ok_or("an unbounded curve in a composite")?;
                Ok(curve.sample(lo, hi, self.tol).into_iter().map(|t| curve.point(t)).collect())
            }
        }
    }

    fn curve(&mut self, id: u64, depth: usize) -> Result<Curve, Why> {
        if depth > MAX_GEOMETRY_DEPTH {
            return Err("curves nest too deeply".into());
        }
        let inst = self.inst(id).ok_or("missing curve")?;
        if inst.is("B_SPLINE_CURVE")
            || inst.is("B_SPLINE_CURVE_WITH_KNOTS")
            || inst.is("BEZIER_CURVE")
            || inst.is("UNIFORM_CURVE")
            || inst.is("QUASI_UNIFORM_CURVE")
        {
            return self.bspline_curve(inst).map(Curve::Nurbs);
        }
        let (kind, a) = self.simple(id).ok_or("missing curve")?;
        match kind {
            "LINE" => {
                let origin = a.get(1).and_then(|p| self.point(p)).ok_or("bad line")?;
                let (dir, _) = a.get(2).and_then(|v| self.vector(v)).ok_or("bad line")?;
                Ok(Curve::Line { origin, dir })
            }
            "CIRCLE" => {
                let frame = a.get(1).and_then(|p| self.placement(p)).ok_or("bad circle")?;
                let radius = a.get(2).and_then(P::num).filter(|r| *r > 0.0).ok_or("bad circle")?;
                Ok(Curve::Circle { frame, radius })
            }
            "ELLIPSE" => {
                let frame = a.get(1).and_then(|p| self.placement(p)).ok_or("bad ellipse")?;
                let ra = a.get(2).and_then(P::num).filter(|r| *r > 0.0).ok_or("bad ellipse")?;
                let rb = a.get(3).and_then(P::num).filter(|r| *r > 0.0).ok_or("bad ellipse")?;
                Ok(Curve::Ellipse { frame, a: ra, b: rb })
            }
            "POLYLINE" => {
                let pts: Vec<DVec3> =
                    a.get(1).map(|l| l.list().iter().filter_map(|p| self.point(p)).collect()).unwrap_or_default();
                if pts.len() < 2 {
                    return Err("bad polyline".into());
                }
                Ok(Curve::Polyline(pts))
            }
            "SURFACE_CURVE" | "SEAM_CURVE" | "INTERSECTION_CURVE" | "BOUNDED_SURFACE_CURVE" => {
                self.curve(a.get(1).and_then(P::reference).ok_or("surface curve without a 3D curve")?, depth + 1)
            }
            "TRIMMED_CURVE" => {
                self.curve(a.get(1).and_then(P::reference).ok_or("trimmed curve without a basis")?, depth + 1)
            }
            "COMPOSITE_CURVE" | "COMPOSITE_CURVE_ON_SURFACE" => {
                let pts = self.curve_points(id, depth + 1)?;
                if pts.len() < 2 {
                    return Err("empty composite curve".into());
                }
                Ok(Curve::Polyline(pts))
            }
            other => Err(other.to_owned()),
        }
    }

    fn surface(&mut self, id: u64, depth: usize) -> Result<Surface, Why> {
        if depth > MAX_GEOMETRY_DEPTH {
            return Err("surfaces nest too deeply".into());
        }
        let inst = self.inst(id).ok_or("missing surface")?;
        if inst.is("B_SPLINE_SURFACE")
            || inst.is("B_SPLINE_SURFACE_WITH_KNOTS")
            || inst.is("BEZIER_SURFACE")
            || inst.is("UNIFORM_SURFACE")
            || inst.is("QUASI_UNIFORM_SURFACE")
        {
            return self.bspline_surface(inst).map(Surface::Nurbs);
        }
        let (kind, a) = self.simple(id).ok_or("missing surface")?;
        let frame = || a.get(1).and_then(|p| self.placement(p)).ok_or_else(|| format!("bad {kind}"));
        let num = |i: usize| a.get(i).and_then(P::num).ok_or_else(|| format!("bad {kind}"));
        match kind {
            "PLANE" => Ok(Surface::Plane(frame()?)),
            "CYLINDRICAL_SURFACE" => Ok(Surface::Cylinder { frame: frame()?, radius: num(2)? }),
            "CONICAL_SURFACE" => {
                let semi = num(3)? * self.angle;
                Ok(Surface::Cone { frame: frame()?, radius: num(2)?, tan: semi.tan() })
            }
            "SPHERICAL_SURFACE" => Ok(Surface::Sphere { frame: frame()?, radius: num(2)? }),
            "TOROIDAL_SURFACE" | "DEGENERATE_TOROIDAL_SURFACE" => {
                Ok(Surface::Torus { frame: frame()?, major: num(2)?, minor: num(3)? })
            }
            "SURFACE_OF_REVOLUTION" => {
                let curve =
                    self.curve(a.get(1).and_then(P::reference).ok_or("revolution without a curve")?, depth + 1)?;
                let (_, ax) = self
                    .simple(a.get(2).and_then(P::reference).ok_or("revolution without an axis")?)
                    .ok_or("bad axis")?;
                let origin = ax.get(1).and_then(|p| self.point(p)).ok_or("bad axis")?;
                let axis = ax.get(2).and_then(|d| self.direction(d)).unwrap_or(DVec3::Z);
                Ok(Surface::Revolution { curve: Box::new(curve), origin, axis })
            }
            "SURFACE_OF_LINEAR_EXTRUSION" => {
                let curve =
                    self.curve(a.get(1).and_then(P::reference).ok_or("extrusion without a curve")?, depth + 1)?;
                let (dir, _) = a.get(2).and_then(|v| self.vector(v)).ok_or("bad extrusion vector")?;
                Ok(Surface::Extrusion { curve: Box::new(curve), dir })
            }
            "OFFSET_SURFACE" => {
                let base = self.surface(a.get(1).and_then(P::reference).ok_or("offset without a basis")?, depth + 1)?;
                Ok(Surface::Offset { base: Box::new(base), distance: num(2)? })
            }
            "RECTANGULAR_TRIMMED_SURFACE" | "CURVE_BOUNDED_SURFACE" => {
                self.surface(a.get(1).and_then(P::reference).ok_or("trimmed surface without a basis")?, depth + 1)
            }
            other => Err(other.to_owned()),
        }
    }

    fn vertex(&self, p: Option<&P>) -> Option<DVec3> {
        let (kind, a) = self.simple(p?.reference()?)?;
        (kind == "VERTEX_POINT").then(|| self.point(a.get(1)?)).flatten()
    }

    /// An edge's points from its start vertex to its end vertex, sampled once and reused.
    fn edge(&mut self, id: u64) -> Option<Vec<DVec3>> {
        if let Some(e) = self.edges.get(&id) {
            return Some(e.clone());
        }
        let (kind, a) = self.simple(id)?;
        if kind != "EDGE_CURVE" {
            return None;
        }
        let (p1, p2) = (self.vertex(a.get(1))?, self.vertex(a.get(2))?);
        let same = a.get(4).and_then(P::flag).unwrap_or(true);
        let pts = match a.get(3).and_then(P::reference).map(|g| self.curve(g, 0)) {
            Some(Ok(curve)) => self.sample_edge(&curve, p1, p2, same),
            Some(Err(why)) => {
                self.note(format!("edge curve {why}"));
                vec![p1, p2]
            }
            None => vec![p1, p2],
        };
        self.edges.insert(id, pts.clone());
        Some(pts)
    }

    fn sample_edge(&self, curve: &Curve, p1: DVec3, p2: DVec3, same: bool) -> Vec<DVec3> {
        crate::brep::sample_edge(curve, p1, p2, same, self.tol)
    }

    /// A face's boundary loops as point rings.
    fn loops(&mut self, bounds: &[P]) -> Vec<Vec<DVec3>> {
        let mut out = Vec::new();
        for b in bounds {
            let Some((_, fb)) = b.reference().and_then(|r| self.simple(r)) else { continue };
            let orient = fb.get(2).and_then(P::flag).unwrap_or(true);
            let Some(lid) = fb.get(1).and_then(P::reference) else { continue };
            let Some((kind, la)) = self.simple(lid) else { continue };
            let mut ring: Vec<DVec3> = Vec::new();
            match kind {
                "EDGE_LOOP" => {
                    for oe in la.get(1).map(P::list).unwrap_or_default() {
                        let Some((_, o)) = oe.reference().and_then(|r| self.simple(r)) else { continue };
                        let forward = o.get(4).and_then(P::flag).unwrap_or(true);
                        let Some(mut pts) = o.get(3).and_then(P::reference).and_then(|e| self.edge(e)) else {
                            continue;
                        };
                        if !forward {
                            pts.reverse();
                        }
                        pts.pop();
                        ring.extend(pts);
                    }
                }
                "POLY_LOOP" => {
                    ring =
                        la.get(1).map(|l| l.list().iter().filter_map(|p| self.point(p)).collect()).unwrap_or_default()
                }
                _ => continue,
            }
            if !orient {
                ring.reverse();
            }
            if ring.len() >= 3 {
                out.push(ring);
            }
        }
        out
    }

    fn face(&mut self, id: u64, flip: bool) -> Option<FaceJob> {
        let (kind, a) = self.simple(id)?;
        let bounds = a.get(1).map(P::list).unwrap_or_default();
        let loops = self.loops(bounds);
        if loops.is_empty() && kind == "FACE" {
            return None;
        }
        let (surface, same) = match kind {
            "ADVANCED_FACE" | "FACE_SURFACE" => {
                let same = a.get(3).and_then(P::flag).unwrap_or(true);
                match a.get(2).and_then(P::reference).map(|s| self.surface(s, 0)) {
                    Some(Ok(s)) => (s, same),
                    Some(Err(why)) => {
                        self.note(format!("surface {why}"));
                        return None;
                    }
                    None => return None,
                }
            }
            // A faceted face: its plane follows from the polygon.
            "FACE" => (Surface::Plane(planar_frame(&loops)?), true),
            _ => return None,
        };
        Some(FaceJob { surface, loops, reversed: !same != flip, outer_first: false, side_unknown: false })
    }

    /// The triangles of a tessellated shape (AP242): solids and shells of triangulated
    /// faces, and triangulated surface sets, by indices into a coordinate list.
    fn tessellated(&self, id: u64, out: &mut Vec<[DVec3; 3]>, depth: usize) {
        if depth > MAX_GEOMETRY_DEPTH || out.len() > crate::MAX_TRIANGLES {
            return;
        }
        let Some((kind, a)) = self.simple(id) else { return };
        let refs =
            |p: Option<&P>| p.map(|l| l.list().iter().filter_map(P::reference).collect::<Vec<_>>()).unwrap_or_default();
        // (coordinates, index list, triangles, strips, fans) argument positions.
        let layout = match kind {
            "TESSELLATED_SOLID" | "TESSELLATED_SHELL" => {
                for f in refs(a.get(1)) {
                    self.tessellated(f, out, depth + 1);
                }
                return;
            }
            "TRIANGULATED_FACE" => (1, 5, Some(6), None, None),
            "COMPLEX_TRIANGULATED_FACE" => (1, 5, None, Some(6), Some(7)),
            "TRIANGULATED_SURFACE_SET" => (1, 4, Some(5), None, None),
            "COMPLEX_TRIANGULATED_SURFACE_SET" => (1, 4, None, Some(5), Some(6)),
            _ => return,
        };
        let Some(coords) = a.get(layout.0).and_then(P::reference).and_then(|c| self.simple(c)) else { return };
        let points: Vec<DVec3> = coords
            .1
            .get(2)
            .map(|l| {
                l.list()
                    .iter()
                    .map(|p| {
                        let c: Vec<f64> = p.list().iter().filter_map(P::num).collect();
                        DVec3::new(
                            c.first().copied().unwrap_or(0.0),
                            c.get(1).copied().unwrap_or(0.0),
                            c.get(2).copied().unwrap_or(0.0),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();
        let index: Vec<usize> = Self::nums(a.get(layout.1)).into_iter().map(|v| v as usize).collect();
        // 1-based, through the index list when there is one.
        let at = |i: f64| -> Option<DVec3> {
            if i.is_nan() || i < 1.0 {
                return None;
            }
            let i = i as usize;
            let j = if index.is_empty() { i } else { *index.get(i - 1)? };
            points.get(j.checked_sub(1)?).copied()
        };
        let mut push = |a: f64, b: f64, c: f64| {
            if a != b
                && b != c
                && a != c
                && let (Some(p), Some(q), Some(r)) = (at(a), at(b), at(c))
                && out.len() <= crate::MAX_TRIANGLES
            {
                out.push([p, q, r]);
            }
        };
        if let Some(t) = layout.2 {
            for tri in a.get(t).map(P::list).unwrap_or_default() {
                let v = Self::nums(Some(tri));
                if let [x, y, z] = v[..] {
                    push(x, y, z);
                }
            }
        }
        if let Some(t) = layout.3 {
            for strip in a.get(t).map(P::list).unwrap_or_default() {
                let v = Self::nums(Some(strip));
                for (k, w) in v.windows(3).enumerate() {
                    if k % 2 == 0 { push(w[0], w[1], w[2]) } else { push(w[1], w[0], w[2]) }
                }
            }
        }
        if let Some(t) = layout.4 {
            for fan in a.get(t).map(P::list).unwrap_or_default() {
                let v = Self::nums(Some(fan));
                if let Some((&first, rest)) = v.split_first() {
                    for w in rest.windows(2) {
                        push(first, w[0], w[1]);
                    }
                }
            }
        }
    }

    /// The faces of a solid or surface model, with whether each is turned inside out.
    fn shell_faces(&self, id: u64, flip: bool, out: &mut Vec<(u64, bool)>, depth: usize) {
        if depth > MAX_GEOMETRY_DEPTH {
            return;
        }
        let Some((kind, a)) = self.simple(id) else { return };
        let refs =
            |p: Option<&P>| p.map(|l| l.list().iter().filter_map(P::reference).collect::<Vec<_>>()).unwrap_or_default();
        match kind {
            "MANIFOLD_SOLID_BREP" | "FACETED_BREP" => {
                if let Some(s) = a.get(1).and_then(P::reference) {
                    self.shell_faces(s, flip, out, depth + 1);
                }
            }
            "BREP_WITH_VOIDS" => {
                if let Some(s) = a.get(1).and_then(P::reference) {
                    self.shell_faces(s, flip, out, depth + 1);
                }
                for v in refs(a.get(2)) {
                    self.shell_faces(v, flip, out, depth + 1);
                }
            }
            "SHELL_BASED_SURFACE_MODEL" => {
                for s in refs(a.get(1)) {
                    self.shell_faces(s, flip, out, depth + 1);
                }
            }
            "CLOSED_SHELL" | "OPEN_SHELL" | "CONNECTED_FACE_SET" => {
                out.extend(refs(a.get(1)).into_iter().map(|f| (f, flip)))
            }
            "ORIENTED_CLOSED_SHELL" | "ORIENTED_OPEN_SHELL" => {
                let forward = a.get(3).and_then(P::flag).unwrap_or(true);
                if let Some(s) = a.get(2).and_then(P::reference) {
                    self.shell_faces(s, flip != !forward, out, depth + 1);
                }
            }
            "ADVANCED_FACE" | "FACE_SURFACE" | "FACE" => out.push((id, flip)),
            _ => {}
        }
    }
}

/// The plane of a polygon, by Newell's method.
fn planar_frame(loops: &[Vec<DVec3>]) -> Option<Axes> {
    let l = loops.first()?;
    let mut n = DVec3::ZERO;
    for (i, p) in l.iter().enumerate() {
        let q = l.get((i + 1) % l.len()).copied().unwrap_or(*p);
        n += DVec3::new((p.y - q.y) * (p.z + q.z), (p.z - q.z) * (p.x + q.x), (p.x - q.x) * (p.y + q.y));
    }
    Some(Axes::new(*l.first()?, Some(n.try_normalize()?), None))
}

const SOLIDS: [&str; 9] = [
    "MANIFOLD_SOLID_BREP",
    "BREP_WITH_VOIDS",
    "FACETED_BREP",
    "SHELL_BASED_SURFACE_MODEL",
    "ORIENTED_CLOSED_SHELL",
    "TESSELLATED_SOLID",
    "TESSELLATED_SHELL",
    "TRIANGULATED_SURFACE_SET",
    "COMPLEX_TRIANGULATED_SURFACE_SET",
];

/// Shapes given as triangles already (AP242 tessellated geometry).
const TESSELLATED: [&str; 4] =
    ["TESSELLATED_SOLID", "TESSELLATED_SHELL", "TRIANGULATED_SURFACE_SET", "COMPLEX_TRIANGULATED_SURFACE_SET"];

/// The length unit (in mm) and angle unit (in radians) of the file's first unit context.
fn units(m: &Map) -> (f64, f64) {
    let si = |a: &[P]| -> f64 {
        match a.first().and_then(P::enumeration) {
            Some("MILLI") => 1.0e-3,
            Some("CENTI") => 1.0e-2,
            Some("DECI") => 1.0e-1,
            Some("KILO") => 1.0e3,
            Some("MICRO") => 1.0e-6,
            Some("NANO") => 1.0e-9,
            _ => 1.0,
        }
    };
    // Metres (or radians) per unit, following conversion-based units down to an SI one.
    fn factor(m: &Map, id: u64, si: &dyn Fn(&[P]) -> f64, depth: usize) -> Option<f64> {
        if depth > 8 {
            return None;
        }
        let inst = m.get(&id)?;
        if let Some(a) = inst.part("SI_UNIT") {
            return Some(si(a));
        }
        let c = inst.part("CONVERSION_BASED_UNIT")?;
        let (_, mw) = m.get(&c.get(1)?.reference()?)?.parts.first()?;
        let value = mw.first()?.num()?;
        Some(value * factor(m, mw.get(1)?.reference()?, si, depth + 1)?)
    }
    let mut ids: Vec<&u64> = m.keys().collect();
    ids.sort();
    for id in ids {
        let Some(units) = m.get(id).and_then(|i| i.part("GLOBAL_UNIT_ASSIGNED_CONTEXT")) else { continue };
        let mut length = None;
        let mut angle = None;
        for u in units.first().map(P::list).unwrap_or_default() {
            let Some(uid) = u.reference() else { continue };
            let Some(inst) = m.get(&uid) else { continue };
            if inst.is("LENGTH_UNIT") {
                length = factor(m, uid, &si, 0).map(|f| f * 1000.0);
            } else if inst.is("PLANE_ANGLE_UNIT") {
                angle = factor(m, uid, &si, 0);
            }
        }
        if length.is_some() || angle.is_some() {
            return (
                length.filter(|l| *l > 0.0 && l.is_finite()).unwrap_or(1.0),
                angle.filter(|a| *a > 0.0 && a.is_finite()).unwrap_or(1.0),
            );
        }
    }
    (1.0, 1.0)
}

/// Union-find over representation ids.
struct Groups(HashMap<u64, u64>);

impl Groups {
    fn find(&mut self, x: u64) -> u64 {
        let mut r = x;
        let mut steps = 0;
        while let Some(&p) = self.0.get(&r) {
            if p == r || steps > 10_000 {
                break;
            }
            r = p;
            steps += 1;
        }
        self.0.insert(x, r);
        r
    }

    fn join(&mut self, a: u64, b: u64) {
        let (ra, rb) = (self.find(a), self.find(b));
        if ra != rb {
            self.0.insert(ra, rb);
        }
    }
}

/// The solids to draw and where: (solid id, placement), through the assembly structure.
fn placed_solids(st: &Step<'_>, skipped_surfaces: &mut usize) -> Vec<(u64, DMat4)> {
    let m = st.m;
    let is_rep =
        |i: &Instance| i.parts.iter().any(|(n, _)| n.ends_with("SHAPE_REPRESENTATION") && !n.contains("RELATIONSHIP"));
    let items_of = |i: &Instance| -> Vec<u64> {
        i.parts
            .iter()
            .find(|(_, a)| a.len() >= 3 && matches!(a.get(1), Some(P::List(_))))
            .map(|(_, a)| a.get(1).map(P::list).unwrap_or_default().iter().filter_map(P::reference).collect())
            .unwrap_or_default()
    };
    let mut groups = Groups(HashMap::new());
    let mut reps: Vec<u64> = m.iter().filter(|(_, i)| is_rep(i)).map(|(id, _)| *id).collect();
    reps.sort_unstable();
    // Representations of one product share coordinates; join them.
    let mut transformed: Vec<(u64, u64, DMat4)> = Vec::new();
    let mut rel_ids: Vec<&u64> = m.keys().collect();
    rel_ids.sort();
    for id in &rel_ids {
        let Some(inst) = m.get(id) else { continue };
        let rr = inst
            .part("REPRESENTATION_RELATIONSHIP")
            .or_else(|| inst.part("SHAPE_REPRESENTATION_RELATIONSHIP"))
            .or_else(|| inst.part("REPRESENTATION_RELATIONSHIP_WITH_TRANSFORMATION"));
        let Some(rr) = rr else { continue };
        let (Some(r1), Some(r2)) = (rr.get(2).and_then(P::reference), rr.get(3).and_then(P::reference)) else {
            continue;
        };
        let op = inst
            .part("REPRESENTATION_RELATIONSHIP_WITH_TRANSFORMATION")
            .and_then(|t| if t.len() >= 5 { t.get(4) } else { t.first() })
            .and_then(P::reference);
        match op {
            Some(op) => {
                let t = match st.simple(op) {
                    Some(("ITEM_DEFINED_TRANSFORMATION", a)) => {
                        let f1 = a.get(2).and_then(|p| st.transform_item(p)).unwrap_or(DMat4::IDENTITY);
                        let f2 = a.get(3).and_then(|p| st.transform_item(p)).unwrap_or(DMat4::IDENTITY);
                        f2 * f1.inverse()
                    }
                    _ => st.transform_item(&P::Ref(op)).unwrap_or(DMat4::IDENTITY),
                };
                transformed.push((r1, r2, t));
            }
            None => groups.join(r1, r2),
        }
    }
    // Which side of a transformed relationship is the component, from the assembly usage.
    let mut pd_reps: HashMap<u64, Vec<u64>> = HashMap::new();
    for inst in m.values() {
        if let Some(a) = inst.part("SHAPE_DEFINITION_REPRESENTATION")
            && let (Some(pds), Some(rep)) = (a.first().and_then(P::reference), a.get(1).and_then(P::reference))
            && let Some(pd) = st.simple(pds).and_then(|(_, p)| p.get(2)).and_then(P::reference)
        {
            pd_reps.entry(pd).or_default().push(rep);
        }
    }
    let mut parent_child: HashMap<(u64, u64), bool> = HashMap::new();
    for inst in m.values() {
        let Some(a) = inst.part("CONTEXT_DEPENDENT_SHAPE_REPRESENTATION") else { continue };
        let (Some(rel), Some(pds)) = (a.first().and_then(P::reference), a.get(1).and_then(P::reference)) else {
            continue;
        };
        let Some(nauo) = st.simple(pds).and_then(|(_, p)| p.get(2)).and_then(P::reference) else { continue };
        let Some((_, na)) = st.simple(nauo) else { continue };
        let (Some(parent), Some(child)) = (na.get(3).and_then(P::reference), na.get(4).and_then(P::reference)) else {
            continue;
        };
        let Some(rr) = m.get(&rel).and_then(|i| {
            i.part("REPRESENTATION_RELATIONSHIP").or_else(|| i.part("REPRESENTATION_RELATIONSHIP_WITH_TRANSFORMATION"))
        }) else {
            continue;
        };
        let Some(r1) = rr.get(2).and_then(P::reference) else { continue };
        let g1 = groups.find(r1);
        let in_parent = pd_reps.get(&parent).is_some_and(|rs| rs.iter().any(|r| groups.find(*r) == g1));
        let in_child = pd_reps.get(&child).is_some_and(|rs| rs.iter().any(|r| groups.find(*r) == g1));
        if in_parent && !in_child {
            parent_child.insert((r1, rr.get(3).and_then(P::reference).unwrap_or(0)), true);
        }
    }
    // Edges parent group → child group, with the child's placement in the parent.
    let mut children: HashMap<u64, Vec<(u64, DMat4)>> = HashMap::new();
    let mut is_child: HashSet<u64> = HashSet::new();
    for (r1, r2, t) in transformed {
        let swapped = parent_child.contains_key(&(r1, r2));
        // Normally rep_1 is the component placed in rep_2.
        let (parent, child, t) = if swapped { (r1, r2, t.inverse()) } else { (r2, r1, t) };
        let (gp, gc) = (groups.find(parent), groups.find(child));
        if gp != gc {
            children.entry(gp).or_default().push((gc, t));
            is_child.insert(gc);
        }
    }
    // Items per group; mapped items are children too.
    let mut items: HashMap<u64, Vec<u64>> = HashMap::new();
    for &r in &reps {
        let g = groups.find(r);
        let Some(inst) = m.get(&r) else { continue };
        for it in items_of(inst) {
            if let Some(("MAPPED_ITEM", a)) = st.simple(it) {
                let Some((_, map)) = a.get(1).and_then(P::reference).and_then(|id| st.simple(id)) else { continue };
                let (Some(origin), Some(rep)) = (map.first(), map.get(1).and_then(P::reference)) else { continue };
                let from = st.transform_item(origin).unwrap_or(DMat4::IDENTITY);
                let to = a.get(2).and_then(|p| st.transform_item(p)).unwrap_or(DMat4::IDENTITY);
                let gc = groups.find(rep);
                if gc != g {
                    children.entry(g).or_default().push((gc, to * from.inverse()));
                    is_child.insert(gc);
                }
            } else {
                items.entry(g).or_default().push(it);
            }
        }
    }
    let mut roots: Vec<u64> = reps.iter().map(|&r| groups.find(r)).filter(|g| !is_child.contains(g)).collect();
    roots.sort_unstable();
    roots.dedup();
    let mut out = Vec::new();
    // A solid reachable two ways (a product's shape and a PMI representation, say) at the
    // same place is drawn once.
    let mut seen: HashSet<(u64, [i64; 12])> = HashSet::new();
    let key = |t: &DMat4| {
        let a = t.to_cols_array();
        let mut k = [0i64; 12];
        for (slot, i) in k.iter_mut().zip([0, 1, 2, 4, 5, 6, 8, 9, 10, 12, 13, 14]) {
            *slot = (a.get(i).copied().unwrap_or(0.0) * 1.0e6).round() as i64;
        }
        k
    };
    let mut stack: Vec<(u64, DMat4, usize)> = roots.into_iter().map(|g| (g, DMat4::IDENTITY, 0)).collect();
    while let Some((g, t, depth)) = stack.pop() {
        if out.len() >= MAX_INSTANCES || depth > MAX_DEPTH {
            continue;
        }
        let here = items.get(&g).map(Vec::as_slice).unwrap_or_default();
        // Surface bodies beside a solid are construction geometry (extracts, splits).
        let has_solid = here.iter().any(|&it| {
            st.simple(it).is_some_and(|(k, _)| matches!(k, "MANIFOLD_SOLID_BREP" | "BREP_WITH_VOIDS" | "FACETED_BREP"))
        });
        for &it in here {
            let Some((k, _)) = st.simple(it) else { continue };
            if !SOLIDS.contains(&k) {
                continue;
            }
            if has_solid && k == "SHELL_BASED_SURFACE_MODEL" {
                *skipped_surfaces += 1;
                continue;
            }
            if seen.insert((it, key(&t))) {
                out.push((it, t));
            }
        }
        for (c, ct) in children.get(&g).map(Vec::as_slice).unwrap_or_default() {
            stack.push((*c, t * *ct, depth + 1));
        }
    }
    // A tessellated copy of a shape that also has exact geometry is left out.
    let tess = |id: u64| st.simple(id).is_some_and(|(k, _)| TESSELLATED.contains(&k));
    if out.iter().any(|(id, _)| !tess(*id)) {
        out.retain(|(id, _)| !tess(*id));
    }
    // Likewise surface bodies in a file that has solids (supplemental or PMI geometry).
    let surface = |id: u64| st.simple(id).is_some_and(|(k, _)| k == "SHELL_BASED_SURFACE_MODEL");
    if out.iter().any(|(id, _)| !surface(*id)) {
        let before = out.len();
        out.retain(|(id, _)| !surface(*id));
        *skipped_surfaces += before - out.len();
    }
    out
}

pub fn read(bytes: &[u8], options: Options) -> Result<Imported, ImportError> {
    let m = p21::parse(bytes)?;
    let (mm, angle) = units(&m);
    let mut st = Step { m: &m, tol: options.tolerance / mm, angle, edges: HashMap::new(), unknown: HashMap::new() };
    let mut skipped_surfaces = 0usize;
    let mut placed = placed_solids(&st, &mut skipped_surfaces);
    if placed.is_empty() {
        // No usable product structure: every solid, else every face, where it stands.
        let mut ids: Vec<u64> = m
            .iter()
            .filter(|(_, i)| {
                SOLIDS.contains(&i.kind()) && i.kind() != "ORIENTED_CLOSED_SHELL" && i.kind() != "TESSELLATED_SHELL"
            })
            .map(|(id, _)| *id)
            .collect();
        if ids.is_empty() {
            ids = m
                .iter()
                .filter(|(_, i)| matches!(i.kind(), "ADVANCED_FACE" | "FACE_SURFACE"))
                .map(|(id, _)| *id)
                .collect();
        }
        ids.sort_unstable();
        placed = ids.into_iter().map(|id| (id, DMat4::IDENTITY)).collect();
    }
    if placed.is_empty() {
        return Err(ImportError::Malformed("the STEP file holds no solids or surfaces".into()));
    }
    // Each distinct solid's faces, tessellated once.
    let mut solids: Vec<u64> = placed.iter().map(|(id, _)| *id).collect();
    solids.sort_unstable();
    solids.dedup();
    let mut jobs: Vec<FaceJob> = Vec::new();
    let mut ranges: HashMap<u64, (usize, usize)> = HashMap::new();
    let mut skipped = 0usize;
    let mut meshes: HashMap<u64, Vec<[DVec3; 3]>> = HashMap::new();
    for &s in &solids {
        if st.simple(s).is_some_and(|(k, _)| TESSELLATED.contains(&k)) {
            let mut tris = Vec::new();
            st.tessellated(s, &mut tris, 0);
            meshes.insert(s, tris);
            continue;
        }
        let mut faces = Vec::new();
        st.shell_faces(s, false, &mut faces, 0);
        let start = jobs.len();
        let mut seen = HashSet::new();
        for (f, flip) in faces {
            if !seen.insert(f) {
                continue;
            }
            match st.face(f, flip) {
                Some(job) => jobs.push(job),
                None => skipped += 1,
            }
        }
        ranges.insert(s, (start, jobs.len()));
    }
    let t = tessellate_faces(&jobs, st.tol);
    let mut soup = Soup::new();
    for (s, at) in &placed {
        let given = meshes.get(s).map(std::slice::from_ref).unwrap_or_default();
        let (a, b) = ranges.get(s).copied().unwrap_or((0, 0));
        for face in t.faces.get(a..b).unwrap_or_default().iter().chain(given) {
            for tri in face {
                let p = tri.map(|q| {
                    let w = at.transform_point3(q);
                    Vec3::new(w.x as f32, w.y as f32, w.z as f32)
                });
                soup.push(p)?;
            }
        }
    }
    let mut warn = warnings(&t, jobs.len() + skipped);
    if skipped > 0 {
        warn.push(format!("{skipped} faces were skipped (unsupported or damaged geometry)"));
    }
    if skipped_surfaces > 0 {
        warn.push(format!("{skipped_surfaces} surface bodies beside the solid were left out as construction geometry"));
    }
    let mut unknown: Vec<(String, usize)> = st.unknown.into_iter().collect();
    unknown.sort();
    for (what, n) in unknown.into_iter().take(8) {
        warn.push(format!("not supported yet: {what} ({n}×)"));
    }
    let eps = soup.extent() * 1.0e-8;
    let mut mesh = soup.into_mesh_welded_within(eps)?;
    let units = match Units::from_mm(mm) {
        Some(u) => Some(u),
        None => {
            mesh = mesh.scaled(mm as f32)?;
            Some(Units::Millimetres)
        }
    };
    Ok(Imported { mesh, units, warnings: warn })
}

#[cfg(test)]
mod tests;
