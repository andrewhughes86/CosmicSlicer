//! ACIS SAT (Standard ACIS Text) files, from the published SAT save file format.
//!
//! A three-line header (version, product strings, millimetres per unit) is followed by entity
//! records ending in `#`, numbered in order (or by an explicit `-n`). Faces are walked from their
//! loops, coedges, edges and vertices; each body's transform places them. Surfaces: planes,
//! cones and cylinders (`cone-surface`), spheres, tori and B-spline surfaces (`spline-surface`,
//! the exact spline or the approximation a procedural one carries). Curves: lines, circles and
//! ellipses, and B-spline curves (`intcurve-curve`). From release 7.0 every record carries an
//! entity id and topology and geometry records a pattern pointer; both are skipped. Subtypes
//! (`{ … }`) are numbered as they appear so `{ ref n }` finds its definition.

use std::collections::HashMap;

use crate::geom::brep::{Axes, Curve, Surface};
use crate::geom::nurbs::{NurbsCurve, NurbsSurface};
use crate::geom::{DMat4, DVec3, Vec3};

use crate::brep::{FaceJob, orient_outward, sample_edge, tessellate_faces, warnings};
use crate::{ImportError, Imported, Options, Soup, Units};

/// Most records read.
const MAX_RECORDS: usize = 5_000_000;
/// Most coedges followed around one loop, and loops on one face.
const MAX_LOOP: usize = 100_000;
/// Most control points in one spline.
const MAX_POLES: usize = 4_000_000;

fn bad(what: impl std::fmt::Display) -> ImportError {
    ImportError::Malformed(format!("SAT: {what}"))
}

#[derive(Clone, Debug, PartialEq)]
enum Tok {
    Ptr(i64),
    Num(f64, bool),
    Word(String),
    Str,
    Open,
    Close,
}

impl Tok {
    fn word(&self) -> Option<&str> {
        match self {
            Tok::Word(w) => Some(w),
            _ => None,
        }
    }
    fn num(&self) -> Option<f64> {
        match self {
            Tok::Num(n, _) => Some(*n),
            _ => None,
        }
    }
}

struct Record {
    /// The full save identifier, e.g. `cone-surface`.
    kind: String,
    toks: Vec<Tok>,
}

impl Record {
    /// The save identifier's last part (the base class: `surface`, `curve`, `edge`…).
    fn base(&self) -> &str {
        self.kind.rsplit('-').next().unwrap_or(&self.kind)
    }
    /// Its first part (the leaf class: `cone`, `plane`, `intcurve`…).
    fn leaf(&self) -> &str {
        self.kind.split('-').next().unwrap_or(&self.kind)
    }
}

/// Splits text into tokens: `@n` and counted header strings are read whole.
struct Lexer<'a> {
    c: &'a [u8],
    i: usize,
}

impl Lexer<'_> {
    fn skip_ws(&mut self) {
        while self.c.get(self.i).is_some_and(|b| b.is_ascii_whitespace()) {
            self.i += 1;
        }
    }

    /// The next whitespace-delimited word, raw.
    fn raw(&mut self) -> Option<&str> {
        self.skip_ws();
        let start = self.i;
        while self.c.get(self.i).is_some_and(|b| !b.is_ascii_whitespace()) {
            self.i += 1;
        }
        (self.i > start).then(|| std::str::from_utf8(self.c.get(start..self.i).unwrap_or_default()).unwrap_or(""))
    }

    /// Skips `n` bytes after one space (the body of a counted string).
    fn skip_counted(&mut self, n: usize) {
        if self.c.get(self.i) == Some(&b' ') {
            self.i += 1;
        }
        self.i = (self.i + n).min(self.c.len());
    }

    /// A header string: `@n text` or `n text`.
    fn header_string(&mut self) {
        let Some(t) = self.raw() else { return };
        let n = t.trim_start_matches('@').parse::<usize>().unwrap_or(0);
        self.skip_counted(n);
    }

    /// The next record's tokens, up to its `#`; None at the end of the data.
    fn record(&mut self) -> Option<Vec<Tok>> {
        let mut out = Vec::new();
        loop {
            self.skip_ws();
            let b = *self.c.get(self.i)?;
            if b == b'#' {
                self.i += 1;
                return Some(out);
            }
            let start = self.i;
            while self.c.get(self.i).is_some_and(|b| !b.is_ascii_whitespace() && *b != b'#') {
                self.i += 1;
            }
            let t = std::str::from_utf8(self.c.get(start..self.i).unwrap_or_default()).unwrap_or("");
            if out.is_empty() && (t.starts_with("End-of-ACIS") || t.starts_with("Begin-of-ACIS")) {
                return None;
            }
            out.push(match t {
                "{" => Tok::Open,
                "}" => Tok::Close,
                _ if t.starts_with('$') => Tok::Ptr(t.get(1..).and_then(|v| v.parse().ok()).unwrap_or(-1)),
                _ if t.starts_with('@') => {
                    let n = t.get(1..).and_then(|v| v.parse::<usize>().ok()).unwrap_or(0);
                    self.skip_counted(n);
                    Tok::Str
                }
                _ => match t.parse::<f64>() {
                    Ok(v) if v.is_finite() => Tok::Num(v, !t.contains(['.', 'e', 'E'])),
                    _ => Tok::Word(t.to_owned()),
                },
            });
        }
    }
}

struct Sat {
    version: i64,
    records: HashMap<i64, Record>,
    /// Each subtype definition: (record, position of the token after its `{`).
    subtypes: Vec<(i64, usize)>,
    tol: f64,
    edges: HashMap<i64, Vec<DVec3>>,
    unknown: HashMap<String, usize>,
}

impl Sat {
    fn rec(&self, id: i64) -> Option<&Record> {
        if id < 0 { None } else { self.records.get(&id) }
    }

    /// A record's data, after the attribute pointer, and from release 7.0 the entity id and
    /// the pattern pointer.
    fn data(&self, id: i64) -> Option<(&Record, &[Tok])> {
        let r = self.rec(id)?;
        let mut i = 1usize.min(r.toks.len());
        if self.version >= 700 {
            while matches!(r.toks.get(i), Some(Tok::Num(_, true))) {
                i += 1;
            }
            if matches!(r.toks.get(i), Some(Tok::Ptr(_))) {
                i += 1;
            }
        }
        Some((r, r.toks.get(i..).unwrap_or_default()))
    }

    /// A topology record's pointers, in order.
    fn ptrs(&self, id: i64) -> Vec<i64> {
        self.data(id).map_or_else(Vec::new, |(_, d)| {
            d.iter().filter_map(|t| if let Tok::Ptr(p) = t { Some(*p) } else { None }).collect()
        })
    }

    fn ptr(&self, id: i64, k: usize) -> i64 {
        self.ptrs(id).get(k).copied().unwrap_or(-1)
    }

    /// Whether a record says `reversed` (older files: the integer 1).
    fn reversed(&self, id: i64) -> bool {
        let Some((_, d)) = self.data(id) else { return false };
        match d.iter().find_map(|t| t.word().filter(|w| *w == "forward" || *w == "reversed")) {
            Some(w) => w == "reversed",
            None => d.iter().any(|t| matches!(t, Tok::Num(v, true) if *v == 1.0)),
        }
    }

    fn point(&self, vertex: i64) -> Option<DVec3> {
        let p = self.ptr(vertex, 1);
        let (_, d) = self.data(p)?;
        let n: Vec<f64> = d.iter().filter_map(Tok::num).take(3).collect();
        (n.len() == 3).then(|| DVec3::new(n[0], n[1], n[2]))
    }

    /// The tokens of subtype `n`, after its `{`.
    fn subtype(&self, n: usize) -> Option<&[Tok]> {
        let (rid, at) = *self.subtypes.get(n)?;
        self.records.get(&rid)?.toks.get(at..)
    }

    /// The tokens inside the first `{ … }` of `d`, following a `ref n` to its definition.
    fn inside<'a>(&'a self, d: &'a [Tok]) -> Option<&'a [Tok]> {
        let open = d.iter().position(|t| *t == Tok::Open)?;
        let body = d.get(open + 1..)?;
        if body.first().and_then(Tok::word) == Some("ref") {
            let n = body.get(1).and_then(Tok::num)?;
            return self.subtype(n as usize);
        }
        Some(body)
    }

    fn note(&mut self, what: &str) {
        *self.unknown.entry(what.to_owned()).or_default() += 1;
    }

    /// A curve and whether it runs against its spline's parameter.
    fn curve(&mut self, id: i64) -> Option<(Curve, bool)> {
        let (r, d) = self.data(id)?;
        let leaf = r.leaf().to_owned();
        let nums: Vec<f64> = d.iter().filter_map(Tok::num).collect();
        let v = |k: usize| {
            DVec3::new(
                nums.get(k).copied().unwrap_or(0.0),
                nums.get(k + 1).copied().unwrap_or(0.0),
                nums.get(k + 2).copied().unwrap_or(0.0),
            )
        };
        match leaf.as_str() {
            "straight" if nums.len() >= 6 => Some((Curve::Line { origin: v(0), dir: v(3).try_normalize()? }, false)),
            "ellipse" if nums.len() >= 10 => {
                let (c, n, major, ratio) = (v(0), v(3), v(6), nums.get(9).copied().unwrap_or(1.0));
                let frame = Axes::new(c, Some(n), Some(major));
                let a = major.length();
                Some(if (ratio.abs() - 1.0).abs() < 1.0e-9 {
                    (Curve::Circle { frame, radius: a }, false)
                } else {
                    (Curve::Ellipse { frame, a, b: a * ratio.abs() }, false)
                })
            }
            "intcurve" => {
                let reversed = d.iter().find_map(Tok::word) == Some("reversed");
                let body = self.inside(d)?;
                let start = body.iter().position(|t| matches!(t.word(), Some("nubs" | "nurbs" | "nullbs")))?;
                let curve = bs3_curve(body.get(start..)?)?;
                Some((Curve::Nurbs(curve), reversed))
            }
            other => {
                let what = format!("{other} curves");
                self.note(&what);
                None
            }
        }
    }

    /// An edge's points from its start vertex to its end vertex.
    fn edge(&mut self, id: i64) -> Option<Vec<DVec3>> {
        if let Some(e) = self.edges.get(&id) {
            return Some(e.clone());
        }
        let p = self.ptrs(id);
        let (v1, v2, curve) = (p.first().copied()?, p.get(1).copied()?, p.get(3).copied().unwrap_or(-1));
        let (a, b) = (self.point(v1)?, self.point(v2)?);
        let forward = !self.reversed(id);
        let pts = match self.curve(curve) {
            Some((c, against)) => sample_edge(&c, a, b, forward != against, self.tol),
            None if a.distance(b) <= self.tol * 1.0e-3 => vec![a],
            None => vec![a, b],
        };
        self.edges.insert(id, pts.clone());
        Some(pts)
    }

    /// A face's boundary loops as point rings.
    fn loops(&mut self, face: i64) -> Vec<Vec<DVec3>> {
        let mut out = Vec::new();
        let mut lp = self.ptr(face, 1);
        let mut seen = 0usize;
        while lp >= 0 && seen < MAX_LOOP {
            seen += 1;
            let first = self.ptr(lp, 1);
            let mut ring: Vec<DVec3> = Vec::new();
            let mut co = first;
            let mut steps = 0usize;
            while co >= 0 && steps < MAX_LOOP {
                steps += 1;
                let edge = self.ptr(co, 3);
                if let Some(mut pts) = self.edge(edge) {
                    if self.reversed(co) {
                        pts.reverse();
                    }
                    if pts.len() > 1 {
                        pts.pop();
                    }
                    ring.extend(pts);
                }
                co = self.ptr(co, 0);
                if co == first {
                    break;
                }
            }
            if ring.len() >= 3 {
                out.push(ring);
            }
            lp = self.ptr(lp, 0);
        }
        out
    }

    /// A face's surface, and whether its normal runs opposite to ours for that kind.
    fn surface(&mut self, id: i64, loops: &[Vec<DVec3>]) -> Option<(Surface, bool)> {
        let (r, d) = self.data(id)?;
        let leaf = r.leaf().to_owned();
        let nums: Vec<f64> = d.iter().filter_map(Tok::num).collect();
        let num = |k: usize| nums.get(k).copied().unwrap_or(0.0);
        let v = |k: usize| DVec3::new(num(k), num(k + 1), num(k + 2));
        match leaf.as_str() {
            "plane" if nums.len() >= 9 => Some((Surface::Plane(Axes::new(v(0), Some(v(3)), Some(v(6)))), false)),
            "cone" if nums.len() >= 10 => {
                let (c, n, major) = (v(0), v(3), v(6));
                let frame = Axes::new(c, Some(n), Some(major));
                let radius = major.length();
                // After the base ellipse's ratio and its (possibly infinite) interval: sine and
                // cosine of the half angle.
                let (sin, cos) = cone_angle(d, self.version)?;
                if sin.abs() < 1.0e-12 {
                    return Some((Surface::Cylinder { frame, radius }, cos < 0.0));
                }
                // The radius's growth along the axis, from the boundary where it has one.
                let (mut num_, mut den) = (0.0, 0.0);
                for p in loops.iter().flatten() {
                    let l = frame.local(*p);
                    let r = (l.x * l.x + l.y * l.y).sqrt();
                    num_ += l.z * (r - radius);
                    den += l.z * l.z;
                }
                let tan = if den > 1.0e-12 { num_ / den } else { sin / cos };
                Some((Surface::Cone { frame, radius, tan }, cos < 0.0))
            }
            "sphere" if nums.len() >= 10 => {
                let (c, r, u, pole) = (v(0), num(3), v(4), v(7));
                Some((Surface::Sphere { frame: Axes::new(c, Some(pole), Some(u)), radius: r.abs() }, r < 0.0))
            }
            "torus" if nums.len() >= 11 => {
                let (c, n, major, minor, u) = (v(0), v(3), num(6), num(7), v(8));
                Some((Surface::Torus { frame: Axes::new(c, Some(n), Some(u)), major, minor: minor.abs() }, minor < 0.0))
            }
            "spline" => {
                let reversed = d.iter().find_map(Tok::word) == Some("reversed");
                let body = self.inside(d)?;
                let start = body.iter().position(|t| matches!(t.word(), Some("nubs" | "nurbs" | "nullbs")))?;
                let s = bs3_surface(body.get(start..)?, loops)?;
                Some((Surface::Nurbs(s), reversed))
            }
            other => {
                let what = format!("{other} surfaces");
                self.note(&what);
                None
            }
        }
    }

    /// The body transform that places `face`: face → shell → lump → body → transform.
    fn placement(&self, face: i64) -> DMat4 {
        let shell = self.ptr(face, 2);
        let lump = self.ptr(shell, 4);
        let body = self.ptr(lump, 2);
        let t = self.ptr(body, 2);
        self.transform(t).unwrap_or(DMat4::IDENTITY)
    }

    /// A transform record: a 3×3 matrix applied to row vectors, a translation and a scale.
    fn transform(&self, id: i64) -> Option<DMat4> {
        let r = self.rec(id)?;
        let nums: Vec<f64> = r.toks.iter().skip(1).filter_map(Tok::num).collect();
        let m = nums.get(nums.len().checked_sub(13)?..)?;
        let row = |i: usize| DVec3::new(m[i * 3], m[i * 3 + 1], m[i * 3 + 2]);
        let s = if m[12].abs() > 1.0e-15 { m[12] } else { 1.0 };
        let (x, y, z) = (row(0) * s, row(1) * s, row(2) * s);
        let t = DVec3::new(m[9], m[10], m[11]);
        Some(DMat4::from_cols(x.extend(0.0), y.extend(0.0), z.extend(0.0), t.extend(1.0)))
    }
}

/// The cone's sine and cosine: after the base ellipse (centre, normal, major axis, ratio) comes,
/// from release 1.6, the ellipse's interval (each end `I` or `F value`), then the two.
fn cone_angle(d: &[Tok], version: i64) -> Option<(f64, f64)> {
    let mut i = 0usize;
    let mut seen = 0usize;
    while seen < 10 {
        if d.get(i)?.num().is_some() {
            seen += 1;
        }
        i += 1;
    }
    if version >= 106 {
        for _ in 0..2 {
            i += match d.get(i)? {
                Tok::Word(w) if w == "F" => 2,
                _ => 1,
            };
        }
    }
    Some((d.get(i)?.num()?, d.get(i + 1)?.num()?))
}

/// Knots written as (value, multiplicity) pairs, ACIS-style (end multiplicity the degree), as a
/// full clamped knot vector, and the number of control points it implies.
fn knots(pairs: &[(f64, usize)], degree: usize) -> Option<(Vec<f64>, usize)> {
    let total: usize = pairs.iter().map(|(_, m)| *m).sum();
    let first = pairs.first()?;
    let last = pairs.last()?;
    let mut k = Vec::with_capacity(total + 2);
    // ACIS leaves out one knot at each end; a file may also write them in full.
    let short = first.1 <= degree;
    if short {
        k.push(first.0);
    }
    for (v, m) in pairs {
        for _ in 0..*m {
            k.push(*v);
        }
    }
    if short {
        k.push(last.0);
    }
    let poles = k.len().checked_sub(degree + 1)?;
    (poles > degree && poles <= MAX_POLES).then_some((k, poles))
}

/// Reads `count` (value, multiplicity) pairs from `t` at `*i`.
fn pairs(t: &[Tok], i: &mut usize, count: usize) -> Option<Vec<(f64, usize)>> {
    let mut out = Vec::with_capacity(count.min(10_000));
    for _ in 0..count {
        let v = t.get(*i)?.num()?;
        let m = t.get(*i + 1)?.num()?;
        if !(1.0..=64.0).contains(&m) {
            return None;
        }
        out.push((v, m as usize));
        *i += 2;
    }
    Some(out)
}

fn small(t: &Tok) -> Option<usize> {
    t.num().filter(|n| (0.0..=1.0e7).contains(n) && n.fract() == 0.0).map(|n| n as usize)
}

/// Reads `n` control points (with weights when `rational`) from `t` at `*i`.
fn poles(t: &[Tok], i: &mut usize, n: usize, rational: bool) -> Option<(Vec<DVec3>, Vec<f64>)> {
    let mut pts = Vec::with_capacity(n.min(MAX_POLES));
    let mut w = Vec::with_capacity(if rational { n.min(MAX_POLES) } else { 0 });
    for _ in 0..n {
        let x = t.get(*i)?.num()?;
        let y = t.get(*i + 1)?.num()?;
        let z = t.get(*i + 2)?.num()?;
        *i += 3;
        pts.push(DVec3::new(x, y, z));
        if rational {
            w.push(t.get(*i)?.num()?);
            *i += 1;
        }
    }
    Some((pts, w))
}

/// A `bs3_curve`: `nubs|nurbs degree closure knot-count (value multiplicity)… poles…`.
fn bs3_curve(t: &[Tok]) -> Option<NurbsCurve> {
    let rational = t.first()?.word()? == "nurbs";
    let degree = small(t.get(1)?)?;
    let mut i = 2;
    if t.get(i)?.word().is_some() {
        i += 1;
    }
    let n = small(t.get(i)?)?;
    i += 1;
    let (k, count) = knots(&pairs(t, &mut i, n)?, degree)?;
    let (pts, w) = poles(t, &mut i, count, rational)?;
    NurbsCurve::new(degree, k, &pts, rational.then_some(w.as_slice())).ok()
}

/// A `bs3_surface`: `nubs|nurbs du dv closure-u closure-v [singularity-u singularity-v] ku kv
/// knots… poles…`. The order of the poles (u or v outermost) is taken from whichever fits the
/// face's boundary better.
fn bs3_surface(t: &[Tok], loops: &[Vec<DVec3>]) -> Option<NurbsSurface> {
    let rational = t.first()?.word()? == "nurbs";
    let du = small(t.get(1)?)?;
    let dv = small(t.get(2)?)?;
    let mut i = 3;
    while t.get(i).and_then(Tok::word).is_some() {
        i += 1;
    }
    let nku = small(t.get(i)?)?;
    let nkv = small(t.get(i + 1)?)?;
    i += 2;
    let (ku, nu) = knots(&pairs(t, &mut i, nku)?, du)?;
    let (kv, nv) = knots(&pairs(t, &mut i, nkv)?, dv)?;
    if nu.checked_mul(nv)? > MAX_POLES {
        return None;
    }
    let (pts, w) = poles(t, &mut i, nu * nv, rational)?;
    let grid = |v_outer: bool| -> Option<NurbsSurface> {
        let at = |a: usize, b: usize| if v_outer { b * nu + a } else { a * nv + b };
        let rows: Vec<Vec<DVec3>> =
            (0..nu).map(|a| (0..nv).map(|b| pts.get(at(a, b)).copied().unwrap_or_default()).collect()).collect();
        let weights: Option<Vec<Vec<f64>>> = rational
            .then(|| (0..nu).map(|a| (0..nv).map(|b| w.get(at(a, b)).copied().unwrap_or(1.0)).collect()).collect());
        NurbsSurface::new(du, dv, ku.clone(), kv.clone(), &rows, weights.as_deref()).ok()
    };
    let fit = |s: &NurbsSurface| -> f64 {
        let surf = Surface::Nurbs(s.clone());
        let pts: Vec<DVec3> = loops.iter().flatten().step_by(3).take(60).copied().collect();
        pts.iter()
            .map(|p| {
                let (u, v) = surf.invert(*p, None);
                surf.point(u, v).distance(*p)
            })
            .sum::<f64>()
            / pts.len().max(1) as f64
    };
    match (grid(false), grid(true)) {
        (Some(a), Some(b)) if loops.iter().any(|l| !l.is_empty()) => Some(if fit(&b) < fit(&a) { b } else { a }),
        (Some(a), _) => Some(a),
        (None, b) => b,
    }
}

pub fn read(bytes: &[u8], options: Options) -> Result<Imported, ImportError> {
    if bytes.starts_with(b"ACIS BinaryFile") {
        return Err(ImportError::Unsupported("binary ACIS (.sab) files; save the model as .sat".into()));
    }
    let mut lx = Lexer { c: bytes, i: 0 };
    let version =
        lx.raw().and_then(|t| t.parse::<i64>().ok()).ok_or_else(|| bad("no version number; this is not a SAT file"))?;
    if !(100..=1_000_000).contains(&version) {
        return Err(bad(format!("version {version}")));
    }
    for _ in 0..3 {
        lx.raw();
    }
    for _ in 0..3 {
        lx.header_string();
    }
    let mm_per_unit = lx.raw().and_then(|t| t.parse::<f64>().ok()).filter(|v| v.is_finite()).unwrap_or(1.0);
    lx.raw();
    lx.raw();
    let mut warn = Vec::new();
    let mm_per_unit = if mm_per_unit > 0.0 {
        mm_per_unit
    } else {
        warn.push("the file does not say its units; read as millimetres".to_owned());
        1.0
    };

    let mut records = HashMap::new();
    let mut subtypes = Vec::new();
    let mut next = 0i64;
    while let Some(mut toks) = lx.record() {
        if records.len() >= MAX_RECORDS {
            return Err(ImportError::TooLarge);
        }
        // An explicit sequence number, `-n`, before the identifier.
        let id = match (toks.first(), toks.get(1)) {
            (Some(Tok::Num(v, true)), Some(Tok::Word(_))) if *v <= 0.0 => {
                let n = -*v as i64;
                toks.remove(0);
                n
            }
            _ => next,
        };
        next = id + 1;
        let Some(Tok::Word(kind)) = toks.first().cloned() else { continue };
        toks.remove(0);
        for (k, t) in toks.iter().enumerate() {
            if *t == Tok::Open && toks.get(k + 1).and_then(Tok::word) != Some("ref") {
                subtypes.push((id, k + 1));
            }
        }
        records.insert(id, Record { kind, toks });
    }
    if records.is_empty() {
        return Err(bad("no entity records"));
    }
    let mut sat = Sat {
        version,
        records,
        subtypes,
        tol: options.tolerance / mm_per_unit,
        edges: HashMap::new(),
        unknown: HashMap::new(),
    };

    let mut faces: Vec<i64> = sat.records.iter().filter(|(_, r)| r.base() == "face").map(|(id, _)| *id).collect();
    faces.sort_unstable();
    let mut jobs = Vec::new();
    let mut placed = Vec::new();
    let mut failed = 0usize;
    for f in faces {
        let loops = sat.loops(f);
        let surface = sat.ptr(f, 4);
        let Some((surface, against)) = sat.surface(surface, &loops) else {
            failed += 1;
            continue;
        };
        if loops.is_empty() && !matches!(surface, Surface::Sphere { .. } | Surface::Torus { .. }) {
            failed += 1;
            continue;
        }
        jobs.push(FaceJob {
            surface,
            loops,
            reversed: sat.reversed(f) != against,
            outer_first: false,
            side_unknown: true,
        });
        placed.push(sat.placement(f));
    }
    if jobs.is_empty() {
        let wire = sat.records.values().any(|r| r.base() == "curve");
        return Err(if failed > 0 {
            ImportError::Unsupported(format!(
                "this ACIS geometry ({})",
                sat.unknown.keys().take(3).cloned().collect::<Vec<_>>().join(", ")
            ))
        } else if wire {
            ImportError::Unsupported("ACIS wire bodies (curves without faces)".into())
        } else {
            bad("no faces")
        });
    }
    // Tessellate in the bodies' own coordinates, as finely as their scale needs.
    let scale =
        placed.iter().map(|m| m.x_axis.truncate().length()).fold(1.0f64, |a, s| if s > 1.0e-12 { a.min(s) } else { a });
    let mut t = tessellate_faces(&jobs, sat.tol / scale.max(1.0e-12));
    let unknown: Vec<bool> = jobs.iter().map(|j| j.side_unknown).collect();
    orient_outward(&mut t.faces, &unknown);
    let mut soup = Soup::new();
    for (face, at) in t.faces.iter().zip(&placed) {
        for tri in face {
            let p = tri.map(|q| {
                let w = at.transform_point3(q);
                Vec3::new(w.x as f32, w.y as f32, w.z as f32)
            });
            soup.push(p)?;
        }
    }
    warn.extend(warnings(&t, jobs.len() + failed));
    if failed > 0 {
        warn.push(format!("{failed} faces were skipped (unsupported or damaged geometry)"));
    }
    let mut unknown: Vec<(String, usize)> = sat.unknown.into_iter().collect();
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
