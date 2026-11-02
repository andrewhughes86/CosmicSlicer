//! Mesh booleans (OrcaSlicer's Mesh boolean tool): union, difference and intersection of two
//! closed meshes, as a new mesh.
//!
//! Every triangle of one mesh is cut along where it crosses the other: intersection points
//! are where an edge of one mesh passes through a triangle of the other, decided by exact
//! orientation tests (`robust`) and each computed once, keyed by that edge and triangle, so
//! the two triangles sharing the edge get the very same point and the pieces stitch together
//! by vertex, with no welding tolerance. Each cut triangle is retriangulated in its plane with
//! its cuts as constraints (`spade`). The pieces then fall into patches bounded by the cuts,
//! and each patch is inside or outside the other mesh by its generalised winding number (sums
//! of solid angles, right even for meshes with small holes); the operation keeps the patches
//! it wants.
//!
//! Exact ties (shared faces, edges or vertices) are what breaks mesh booleans: the second mesh
//! is shifted by a fixed 1e-7 of the models' size first, far below what an f32 mesh can tell
//! apart, so no point of one lies exactly on a face of the other.

use std::collections::HashMap;

use glam::{DVec2, DVec3, Vec3};
use spade::{ConstrainedDelaunayTriangulation, Point2, Triangulation};

use crate::Mesh;

/// The solid an object prints, on the plate: its main mesh with its parts added and its
/// negative volumes taken away (modifiers and support volumes do not print).
pub fn object_solid(o: &crate::Object) -> Result<Mesh, BooleanError> {
    let placed = |mesh: &Mesh, m: glam::Mat4| -> Mesh {
        let flip = m.determinant() < 0.0;
        Mesh {
            vertices: mesh.vertices.iter().map(|v| m.transform_point3(*v)).collect(),
            // A mirroring transform turns the faces inside out; turn them back.
            triangles: mesh.triangles.iter().map(|t| if flip { [t[0], t[2], t[1]] } else { *t }).collect(),
        }
    };
    let m = o.matrix();
    let mut solid = placed(&o.mesh, m);
    for kind in [crate::PartKind::Normal, crate::PartKind::Negative] {
        for p in o.parts.iter().filter(|p| p.kind == kind) {
            let part = placed(&p.mesh, m * p.matrix());
            let op = if kind == crate::PartKind::Normal { BoolOp::Union } else { BoolOp::Difference };
            solid = boolean(&solid, &part, op)?;
        }
    }
    Ok(solid)
}

/// Most shells merged in one mesh.
const MAX_SHELLS: usize = 512;

/// One solid from a mesh whose shells cross (models exported as separate overlapping pieces):
/// the shells unioned. Shells facing inwards (a hollow's cavity, negative volume) are kept as
/// they are, since a union would fill them in. Crossings within one shell are left. Returns
/// the mesh and how many shells were merged into others.
pub fn merge_shells(mesh: &Mesh) -> Result<(Mesh, usize), BooleanError> {
    let shells = mesh.split();
    if shells.len() < 2 {
        return Ok((mesh.clone(), 0));
    }
    if shells.len() > MAX_SHELLS {
        return Err(BooleanError::TooComplex);
    }
    let (solids, cavities): (Vec<Mesh>, Vec<Mesh>) = shells.into_iter().partition(|m| m.volume() >= 0.0);
    let mut merged = 0;
    let mut out: Vec<Mesh> = Vec::new();
    for shell in solids {
        let b = shell.bounds();
        // Into the first shell it may touch; on its own if none.
        match out.iter().position(|o| {
            let ob = o.bounds();
            ob.min.cmple(b.max).all() && b.min.cmple(ob.max).all()
        }) {
            Some(i) => {
                let Some(target) = out.get_mut(i) else { continue };
                *target = boolean(target, &shell, BoolOp::Union)?;
                merged += 1;
            }
            None => out.push(shell),
        }
    }
    let mut result = Mesh::default();
    for m in out.iter().chain(&cavities) {
        let base = result.vertices.len() as u32;
        result.vertices.extend_from_slice(&m.vertices);
        result.triangles.extend(m.triangles.iter().map(|t| t.map(|i| i + base)));
    }
    Ok((result, merged))
}

/// How the two meshes combine.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BoolOp {
    Union,
    /// The first less the second.
    Difference,
    Intersection,
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum BooleanError {
    #[error("a mesh has more than {MAX_TRIANGLES} triangles")]
    TooLarge,
    #[error("the meshes cross in too many places")]
    TooComplex,
    #[error("the boolean failed on these meshes")]
    Failed,
}

/// Most triangles in either input.
const MAX_TRIANGLES: usize = 2_000_000;
/// Most intersection points.
const MAX_POINTS: usize = 4_000_000;
/// Most triangle pairs looked at closely.
const MAX_PAIRS: usize = 50_000_000;

/// `a` combined with `b`. Never panics; the result may be empty (two meshes that do not meet,
/// intersected).
pub fn boolean(a: &Mesh, b: &Mesh, op: BoolOp) -> Result<Mesh, BooleanError> {
    if a.triangles.len() > MAX_TRIANGLES || b.triangles.len() > MAX_TRIANGLES {
        return Err(BooleanError::TooLarge);
    }
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run(a, b, op))).unwrap_or(Err(BooleanError::Failed))
}

/// A mesh's triangles with f64 corners, degenerate and broken ones dropped.
struct Side {
    pts: Vec<DVec3>,
    tris: Vec<[u32; 3]>,
}

impl Side {
    fn new(m: &Mesh, shift: DVec3) -> Side {
        let pts: Vec<DVec3> = m.vertices.iter().map(|v| v.as_dvec3() + shift).collect();
        let n = pts.len() as u32;
        let tris = m
            .triangles
            .iter()
            .copied()
            .filter(|t| t.iter().all(|i| *i < n) && t[0] != t[1] && t[1] != t[2] && t[0] != t[2])
            .filter(|t| t.iter().all(|i| pts.get(*i as usize).is_some_and(|p| p.is_finite())))
            .collect();
        Side { pts, tris }
    }

    fn corners(&self, t: usize) -> Option<[DVec3; 3]> {
        let tri = self.tris.get(t)?;
        Some([*self.pts.get(tri[0] as usize)?, *self.pts.get(tri[1] as usize)?, *self.pts.get(tri[2] as usize)?])
    }

    fn bounds(&self, t: usize) -> Option<(DVec3, DVec3)> {
        let c = self.corners(t)?;
        Some((c[0].min(c[1]).min(c[2]), c[0].max(c[1]).max(c[2])))
    }
}

fn c3(p: DVec3) -> robust::Coord3D<f64> {
    robust::Coord3D { x: p.x, y: p.y, z: p.z }
}

/// Exact sign of the volume of a-b-c-d (0 only for exactly coplanar points), and its value.
fn orient(a: DVec3, b: DVec3, c: DVec3, d: DVec3) -> f64 {
    robust::orient3d(c3(a), c3(b), c3(c), c3(d))
}

/// Where segment `a`–`b` passes through the inside of triangle `t` (not touching its edges or
/// plane at an end).
fn edge_through(a: DVec3, b: DVec3, t: [DVec3; 3]) -> Option<DVec3> {
    let sa = orient(t[0], t[1], t[2], a);
    let sb = orient(t[0], t[1], t[2], b);
    if !(sa > 0.0 && sb < 0.0 || sa < 0.0 && sb > 0.0) {
        return None;
    }
    let o = [orient(a, b, t[0], t[1]), orient(a, b, t[1], t[2]), orient(a, b, t[2], t[0])];
    if !(o.iter().all(|v| *v > 0.0) || o.iter().all(|v| *v < 0.0)) {
        return None;
    }
    let s = sa / (sa - sb);
    Some(a + (b - a) * s)
}

/// An intersection point: where an edge (two vertex indices, smaller first) of one side
/// passes through a triangle of the other.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct Crossing {
    /// The side the edge belongs to (0 for `a`, 1 for `b`).
    side: u8,
    edge: (u32, u32),
    tri: u32,
}

/// Every point of the result, numbered: `a`'s vertices, then `b`'s, then crossings and the
/// points the retriangulation adds.
struct Points {
    pos: Vec<DVec3>,
    crossings: HashMap<Crossing, u32>,
    /// For crossing points: the side and edge they lie on.
    on_edge: HashMap<u32, (u8, (u32, u32))>,
}

impl Points {
    fn add(&mut self, p: DVec3) -> Result<u32, BooleanError> {
        if self.pos.len() >= MAX_POINTS {
            return Err(BooleanError::TooComplex);
        }
        self.pos.push(p);
        Ok((self.pos.len() - 1) as u32)
    }

    /// The point where the edge `e` of `sides[side]` crosses triangle `tri` of the other side.
    fn crossing(
        &mut self,
        sides: [&Side; 2],
        side: usize,
        e: (u32, u32),
        tri: usize,
    ) -> Result<Option<u32>, BooleanError> {
        let e = (e.0.min(e.1), e.0.max(e.1));
        let key = Crossing { side: side as u8, edge: e, tri: tri as u32 };
        if let Some(&id) = self.crossings.get(&key) {
            return Ok(Some(id));
        }
        let (Some(here), Some(other)) = (sides.get(side), sides.get(1 - side)) else { return Ok(None) };
        let (Some(&a), Some(&b), Some(t)) =
            (here.pts.get(e.0 as usize), here.pts.get(e.1 as usize), other.corners(tri))
        else {
            return Ok(None);
        };
        let Some(p) = edge_through(a, b, t) else { return Ok(None) };
        let id = self.add(p)?;
        self.crossings.insert(key, id);
        self.on_edge.insert(id, (side as u8, e));
        Ok(Some(id))
    }
}

/// The triangles of `b` whose bounds meet each triangle of `a`'s.
fn candidate_pairs(a: &Side, b: &Side) -> Result<Vec<(usize, usize)>, BooleanError> {
    let mut lo = DVec3::splat(f64::INFINITY);
    let mut hi = DVec3::splat(f64::NEG_INFINITY);
    for t in 0..b.tris.len() {
        if let Some((l, h)) = b.bounds(t) {
            lo = lo.min(l);
            hi = hi.max(h);
        }
    }
    if !(lo.is_finite() && hi.is_finite()) {
        return Ok(Vec::new());
    }
    let n = ((b.tris.len() as f64).cbrt() * 1.5).clamp(1.0, 96.0) as usize;
    let size = (hi - lo).max(DVec3::splat(1e-9));
    let cell = |p: DVec3| -> [usize; 3] {
        let f = (p - lo) / size * n as f64;
        [f.x, f.y, f.z].map(|v| (v.floor().max(0.0) as usize).min(n - 1))
    };
    let mut grid: Vec<Vec<u32>> = vec![Vec::new(); n * n * n];
    // Triangles across many cells go in a list everyone checks.
    let mut big: Vec<u32> = Vec::new();
    for t in 0..b.tris.len() {
        let Some((l, h)) = b.bounds(t) else { continue };
        let (c0, c1) = (cell(l), cell(h));
        let count = (c1[0] - c0[0] + 1) * (c1[1] - c0[1] + 1) * (c1[2] - c0[2] + 1);
        if count > 64 {
            big.push(t as u32);
            continue;
        }
        for x in c0[0]..=c1[0] {
            for y in c0[1]..=c1[1] {
                for z in c0[2]..=c1[2] {
                    if let Some(c) = grid.get_mut((x * n + y) * n + z) {
                        c.push(t as u32);
                    }
                }
            }
        }
    }
    let overlaps = |a: (DVec3, DVec3), b: (DVec3, DVec3)| a.0.cmple(b.1).all() && b.0.cmple(a.1).all();
    let mut stamp = vec![usize::MAX; b.tris.len()];
    let mut pairs = Vec::new();
    for ta in 0..a.tris.len() {
        let Some(ba) = a.bounds(ta) else { continue };
        if !overlaps(ba, (lo, hi)) {
            continue;
        }
        let (c0, c1) = (cell(ba.0.max(lo)), cell(ba.1.min(hi)));
        let mut look = |tb: u32| {
            let Some(s) = stamp.get_mut(tb as usize) else { return };
            if *s == ta {
                return;
            }
            *s = ta;
            if b.bounds(tb as usize).is_some_and(|bb| overlaps(ba, bb)) {
                pairs.push((ta, tb as usize));
            }
        };
        for &tb in &big {
            look(tb);
        }
        for x in c0[0]..=c1[0] {
            for y in c0[1]..=c1[1] {
                for z in c0[2]..=c1[2] {
                    for &tb in grid.get((x * n + y) * n + z).map_or(&[][..], Vec::as_slice) {
                        look(tb);
                    }
                }
            }
        }
        if pairs.len() > MAX_PAIRS {
            return Err(BooleanError::TooComplex);
        }
    }
    Ok(pairs)
}

/// The cuts on each triangle of a side: segments between point ids.
type Cuts = HashMap<usize, Vec<(u32, u32)>>;

/// Finds where the two sides cross: the cuts on each side's triangles.
fn cuts(sides: [&Side; 2], points: &mut Points) -> Result<[Cuts; 2], BooleanError> {
    let [a, b] = sides;
    let mut out: [Cuts; 2] = [HashMap::new(), HashMap::new()];
    for (ta, tb) in candidate_pairs(a, b)? {
        let (Some(at), Some(bt)) = (a.tris.get(ta), b.tris.get(tb)) else { continue };
        let mut ends: Vec<u32> = Vec::new();
        for (side, own, other) in [(0usize, at, tb), (1usize, bt, ta)] {
            for k in 0..3 {
                let e = (own[k], own[(k + 1) % 3]);
                if let Some(id) = points.crossing(sides, side, e, other)? {
                    ends.push(id);
                }
            }
        }
        // In general position two triangles that cross do so along one segment.
        if let [p, q] = ends[..]
            && p != q
        {
            out[0].entry(ta).or_default().push((p, q));
            out[1].entry(tb).or_default().push((p, q));
        }
    }
    Ok(out)
}

/// One output triangle: point ids, and the side it came from.
#[derive(Clone, Copy)]
struct Piece {
    v: [u32; 3],
}

/// The triangles of `side` (offset: the id of its first vertex, `index` 0 for `a`, 1 for `b`),
/// cut along `cuts`; the edges that lie on cuts are added to `cut_edges`.
fn pieces(
    side: &Side,
    index: u8,
    offset: u32,
    cuts: &Cuts,
    points: &mut Points,
    cut_edges: &mut std::collections::HashSet<(u32, u32)>,
) -> Result<Vec<Piece>, BooleanError> {
    let mut out = Vec::with_capacity(side.tris.len() + cuts.len() * 8);
    for (t, tri) in side.tris.iter().enumerate() {
        let ids = tri.map(|i| i + offset);
        let Some(segments) = cuts.get(&t) else {
            out.push(Piece { v: ids });
            continue;
        };
        let Some(c) = side.corners(t) else { continue };
        let (e1, e2) = (c[1] - c[0], c[2] - c[0]);
        let normal = e1.cross(e2);
        let (d11, d12, d22) = (e1.dot(e1), e1.dot(e2), e2.dot(e2));
        let det = d11 * d22 - d12 * d12;
        if det.is_nan() || det <= 0.0 {
            continue;
        }
        // The triangle's own parameters: corners at (0, 0), (1, 0) and (0, 1). Points on an
        // edge are put exactly on it (the diagonal by Sterbenz: u + v is exactly 1), so the
        // triangulation sees them in line with the corners, as its neighbour does.
        let along = |p: DVec3, a: DVec3, b: DVec3| ((p - a).dot(b - a) / (b - a).length_squared()).clamp(0.0, 1.0);
        let param = |id: u32, p: DVec3| -> DVec2 {
            let edge = points.on_edge.get(&id).filter(|(s, _)| *s == index).map(|(_, e)| *e);
            let is = |a: u32, b: u32| edge == Some((a.min(b), a.max(b)));
            if is(tri[0], tri[1]) {
                DVec2::new(along(p, c[0], c[1]), 0.0)
            } else if is(tri[0], tri[2]) {
                DVec2::new(0.0, along(p, c[0], c[2]))
            } else if is(tri[1], tri[2]) {
                let v = along(p, c[1], c[2]);
                if v >= 0.5 {
                    DVec2::new(1.0 - v, v)
                } else {
                    let u = 1.0 - v;
                    DVec2::new(u, 1.0 - u)
                }
            } else {
                let w = p - c[0];
                let (b1, b2) = (w.dot(e1), w.dot(e2));
                DVec2::new((d22 * b1 - d12 * b2) / det, (d11 * b2 - d12 * b1) / det)
            }
        };
        let mut cdt: ConstrainedDelaunayTriangulation<Point2<f64>> = ConstrainedDelaunayTriangulation::new();
        // Vertex handle index -> point id.
        let mut id_of: Vec<u32> = Vec::new();
        let mut handle_of: HashMap<u32, spade::handles::FixedVertexHandle> = HashMap::new();
        let mut insert = |cdt: &mut ConstrainedDelaunayTriangulation<Point2<f64>>,
                          id_of: &mut Vec<u32>,
                          id: u32,
                          q: DVec2|
         -> Option<spade::handles::FixedVertexHandle> {
            if let Some(h) = handle_of.get(&id) {
                return Some(*h);
            }
            let h = cdt.insert(Point2::new(q.x, q.y)).ok()?;
            if h.index() == id_of.len() {
                id_of.push(id);
            }
            handle_of.insert(id, h);
            Some(h)
        };
        for (k, &id) in ids.iter().enumerate() {
            let q = [DVec2::ZERO, DVec2::X, DVec2::Y][k];
            insert(&mut cdt, &mut id_of, id, q);
        }
        let mut segs = Vec::with_capacity(segments.len());
        for &(p, q) in segments {
            let (Some(&pp), Some(&pq)) = (points.pos.get(p as usize), points.pos.get(q as usize)) else { continue };
            let (fp, fq) = (param(p, pp), param(q, pq));
            let (Some(hp), Some(hq)) = (insert(&mut cdt, &mut id_of, p, fp), insert(&mut cdt, &mut id_of, q, fq))
            else {
                continue;
            };
            segs.push((hp, hq));
        }
        for (hp, hq) in segs {
            if hp != hq {
                cdt.add_constraint_and_split(hp, hq, |v| v);
            }
        }
        // Points the constraints added where they cross, back on the triangle.
        while id_of.len() < cdt.num_vertices() {
            let Some(v) = cdt.vertices().nth(id_of.len()) else { break };
            let q = v.position();
            let id = points.add(c[0] + e1 * q.x + e2 * q.y)?;
            id_of.push(id);
        }
        for f in cdt.inner_faces() {
            let v = f.vertices().map(|h| id_of.get(h.fix().index()).copied().unwrap_or(u32::MAX));
            if v.contains(&u32::MAX) || v[0] == v[1] || v[1] == v[2] || v[0] == v[2] {
                continue;
            }
            // Counter-clockwise in the parameters is the triangle's own way round.
            let _ = normal;
            out.push(Piece { v });
        }
        for e in cdt.undirected_edges() {
            if e.is_constraint_edge() {
                let [p, q] = e.vertices().map(|h| id_of.get(h.fix().index()).copied().unwrap_or(u32::MAX));
                cut_edges.insert((p.min(q), p.max(q)));
            }
        }
    }
    Ok(out)
}

/// Generalised winding number of `side` at `p`: about 1 inside a closed mesh, 0 outside.
fn winding(side: &Side, p: DVec3) -> f64 {
    let mut total = 0.0;
    for t in 0..side.tris.len() {
        let Some(c) = side.corners(t) else { continue };
        let (a, b, cc) = (c[0] - p, c[1] - p, c[2] - p);
        let (la, lb, lc) = (a.length(), b.length(), c[2].distance(p));
        let num = a.dot(b.cross(cc));
        let den = la * lb * lc + a.dot(b) * lc + b.dot(cc) * la + cc.dot(a) * lb;
        total += 2.0 * num.atan2(den);
    }
    total / (4.0 * std::f64::consts::PI)
}

/// Patches of `pieces`: pieces joined across edges that are not cuts.
fn patches(pieces: &[Piece], cut_edges: &std::collections::HashSet<(u32, u32)>) -> Vec<usize> {
    let mut parent: Vec<usize> = (0..pieces.len()).collect();
    fn find(parent: &mut [usize], mut i: usize) -> usize {
        while let Some(&p) = parent.get(i) {
            if p == i {
                break;
            }
            let gp = parent.get(p).copied().unwrap_or(p);
            if let Some(slot) = parent.get_mut(i) {
                *slot = gp;
            }
            i = gp;
        }
        i
    }
    let mut first: HashMap<(u32, u32), usize> = HashMap::new();
    for (i, p) in pieces.iter().enumerate() {
        for k in 0..3 {
            let (a, b) = (p.v[k], p.v[(k + 1) % 3]);
            let e = (a.min(b), a.max(b));
            if cut_edges.contains(&e) {
                continue;
            }
            match first.get(&e) {
                Some(&j) => {
                    let (ri, rj) = (find(&mut parent, i), find(&mut parent, j));
                    if let Some(slot) = parent.get_mut(ri) {
                        *slot = rj;
                    }
                }
                None => {
                    first.insert(e, i);
                }
            }
        }
    }
    (0..pieces.len()).map(|i| find(&mut parent, i)).collect()
}

/// Whether each piece lies inside `other`, decided once per patch at its largest piece.
fn inside(pieces: &[Piece], patch: &[usize], points: &Points, other: &Side) -> Vec<bool> {
    let pos = |i: u32| points.pos.get(i as usize).copied().unwrap_or(DVec3::ZERO);
    let mut best: HashMap<usize, (f64, usize)> = HashMap::new();
    for (i, p) in pieces.iter().enumerate() {
        let area = (pos(p.v[1]) - pos(p.v[0])).cross(pos(p.v[2]) - pos(p.v[0])).length();
        let root = patch.get(i).copied().unwrap_or(i);
        let e = best.entry(root).or_insert((-1.0, i));
        if area > e.0 {
            *e = (area, i);
        }
    }
    let verdict: HashMap<usize, bool> = best
        .into_iter()
        .map(|(root, (_, i))| {
            let p = pieces.get(i).map_or(DVec3::ZERO, |p| (pos(p.v[0]) + pos(p.v[1]) + pos(p.v[2])) / 3.0);
            (root, winding(other, p) > 0.5)
        })
        .collect();
    patch.iter().map(|r| verdict.get(r).copied().unwrap_or(false)).collect()
}

fn run(a: &Mesh, b: &Mesh, op: BoolOp) -> Result<Mesh, BooleanError> {
    let ba = a.bounds();
    let bb = b.bounds();
    let size = ba.union(&bb).size().as_dvec3().length().max(1e-3);
    // Off any exact tie with `a` (see the module notes).
    let shift = DVec3::new(0.723_606_8, 0.525_731_1, 0.447_213_6) * size * 1e-7;
    let sa = Side::new(a, DVec3::ZERO);
    let sb = Side::new(b, shift);
    let offset_b = sa.pts.len() as u32;
    let mut points = Points {
        pos: sa.pts.iter().chain(&sb.pts).copied().collect(),
        crossings: HashMap::new(),
        on_edge: HashMap::new(),
    };
    // Crossing ids refer to `points`; the sides keep their own vertex numbering, so `b`'s
    // edges are looked up in `sb` and offset only when they become pieces.
    let [cuts_a, cuts_b] = cuts([&sa, &sb], &mut points)?;
    let mut cut_edges = std::collections::HashSet::new();
    let pa = pieces(&sa, 0, 0, &cuts_a, &mut points, &mut cut_edges)?;
    let pb = pieces(&sb, 1, offset_b, &cuts_b, &mut points, &mut cut_edges)?;
    let patch_a = patches(&pa, &cut_edges);
    let patch_b = patches(&pb, &cut_edges);
    let in_b = inside(&pa, &patch_a, &points, &sb);
    let in_a = inside(&pb, &patch_b, &points, &sa);
    let (keep_a_inside, keep_b_inside, flip_b) = match op {
        BoolOp::Union => (false, false, false),
        BoolOp::Difference => (false, true, true),
        BoolOp::Intersection => (true, true, false),
    };
    let mut tris: Vec<[u32; 3]> = Vec::new();
    for (p, inside) in pa.iter().zip(&in_b) {
        if *inside == keep_a_inside {
            tris.push(p.v);
        }
    }
    for (p, inside) in pb.iter().zip(&in_a) {
        if *inside == keep_b_inside {
            tris.push(if flip_b { [p.v[0], p.v[2], p.v[1]] } else { p.v });
        }
    }
    // Only the points used, renumbered.
    let mut remap: Vec<u32> = vec![u32::MAX; points.pos.len()];
    let mut vertices: Vec<Vec3> = Vec::new();
    for t in &mut tris {
        for i in t.iter_mut() {
            let Some(slot) = remap.get_mut(*i as usize) else { return Err(BooleanError::Failed) };
            if *slot == u32::MAX {
                *slot = vertices.len() as u32;
                vertices.push(points.pos.get(*i as usize).copied().unwrap_or(DVec3::ZERO).as_vec3());
            }
            *i = *slot;
        }
    }
    Ok(Mesh { vertices, triangles: tris })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::primitives;

    fn moved(mut m: Mesh, by: Vec3) -> Mesh {
        for v in &mut m.vertices {
            *v += by;
        }
        m
    }

    /// `m` with vertices at the same place merged (the UV sphere repeats its seam and poles).
    fn welded(m: Mesh) -> Mesh {
        let mut at: HashMap<[i64; 3], u32> = HashMap::new();
        let mut vertices = Vec::new();
        let map: Vec<u32> = m
            .vertices
            .iter()
            .map(|v| {
                let key = [v.x, v.y, v.z].map(|c| (f64::from(c) * 1e5).round() as i64);
                *at.entry(key).or_insert_with(|| {
                    vertices.push(*v);
                    (vertices.len() - 1) as u32
                })
            })
            .collect();
        let triangles = m
            .triangles
            .iter()
            .map(|t| t.map(|i| map.get(i as usize).copied().unwrap_or(0)))
            .filter(|t| t[0] != t[1] && t[1] != t[2] && t[0] != t[2])
            .collect();
        Mesh { vertices, triangles }
    }

    pub(super) fn volume(m: &Mesh) -> f64 {
        let at = |i: u32| m.vertices.get(i as usize).map_or(DVec3::ZERO, |v| v.as_dvec3());
        m.triangles.iter().map(|t| at(t[0]).dot(at(t[1]).cross(at(t[2])))).sum::<f64>() / 6.0
    }

    /// Edges not shared by exactly two triangles running opposite ways.
    pub(super) fn open_edges(m: &Mesh) -> usize {
        let mut directed: HashMap<(u32, u32), i32> = HashMap::new();
        for t in &m.triangles {
            for k in 0..3 {
                let (a, b) = (t[k], t[(k + 1) % 3]);
                *directed.entry((a, b)).or_insert(0) += 1;
            }
        }
        directed.iter().filter(|((a, b), n)| **n != 1 || directed.get(&(*b, *a)) != Some(&1)).count()
    }

    fn check(m: &Result<Mesh, BooleanError>, expect: f64, what: &str) {
        assert!(m.is_ok(), "{what}: {m:?}");
        let Ok(m) = m else { return };
        let v = volume(m);
        assert!((v - expect).abs() <= expect.abs() * 2e-3 + 1e-3, "{what}: volume {v}, expected {expect}");
        assert_eq!(open_edges(m), 0, "{what}: open edges");
    }

    #[test]
    fn two_cubes_overlapping_at_a_corner() {
        let a = moved(primitives::cube(20.0), Vec3::ZERO);
        let b = moved(primitives::cube(20.0), Vec3::splat(10.0));
        check(&boolean(&a, &b, BoolOp::Union), 15_000.0, "union");
        check(&boolean(&a, &b, BoolOp::Difference), 7_000.0, "difference");
        check(&boolean(&a, &b, BoolOp::Intersection), 1_000.0, "intersection");
    }

    #[test]
    fn faces_in_the_same_plane() {
        // Side by side, overlapping by half: their top, bottom, front and back faces are coplanar.
        let a = primitives::cube(20.0);
        let b = moved(primitives::cube(20.0), Vec3::new(10.0, 0.0, 0.0));
        check(&boolean(&a, &b, BoolOp::Union), 12_000.0, "union");
        check(&boolean(&a, &b, BoolOp::Difference), 4_000.0, "difference");
        check(&boolean(&a, &b, BoolOp::Intersection), 4_000.0, "intersection");
    }

    #[test]
    fn a_mesh_with_itself() {
        let a = primitives::cube(20.0);
        check(&boolean(&a, &a, BoolOp::Union), 8_000.0, "union");
        check(&boolean(&a, &a, BoolOp::Intersection), 8_000.0, "intersection");
        let d = boolean(&a, &a, BoolOp::Difference);
        assert!(d.as_ref().is_ok_and(|m| volume(m).abs() < 1.0), "{:?}", d.as_ref().map(volume));
    }

    #[test]
    fn crossing_shells_merge_and_cavities_stay() {
        // Two crossing cubes in one mesh, and a cavity inside the first (facing inwards).
        let mut cavity = moved(primitives::cube(4.0), Vec3::new(-8.0, -8.0, 2.0));
        for t in &mut cavity.triangles {
            t.swap(1, 2);
        }
        let mut m = primitives::cube(20.0);
        for part in [moved(primitives::cube(20.0), Vec3::new(10.0, 0.0, 0.0)), cavity] {
            let base = m.vertices.len() as u32;
            m.vertices.extend(part.vertices);
            m.triangles.extend(part.triangles.iter().map(|t| t.map(|i| i + base)));
        }
        let r = merge_shells(&m);
        assert!(r.is_ok(), "{r:?}");
        let Ok((fixed, merged)) = r else { return };
        assert_eq!(merged, 1);
        assert_eq!(open_edges(&fixed), 0);
        let v = volume(&fixed);
        assert!((v - (12_000.0 - 64.0)).abs() < 1.0, "{v}");
    }

    #[test]
    fn meshes_that_do_not_meet() {
        let a = primitives::cube(10.0);
        let b = moved(primitives::cube(10.0), Vec3::new(30.0, 0.0, 0.0));
        check(&boolean(&a, &b, BoolOp::Union), 2_000.0, "union");
        check(&boolean(&a, &b, BoolOp::Difference), 1_000.0, "difference");
        assert!(boolean(&a, &b, BoolOp::Intersection).is_ok_and(|m| m.triangles.is_empty()));
    }

    #[test]
    fn curved_meshes() {
        // A sphere with a cylinder through it: the volumes of the polygonal meshes add up.
        let s = welded(primitives::sphere(10.0, 48));
        assert_eq!(open_edges(&s), 0);
        let c = moved(primitives::cylinder(4.0, 30.0, 32), Vec3::new(0.0, 0.0, -15.0));
        let (vs, vc) = (volume(&s), volume(&c));
        let i = boolean(&s, &c, BoolOp::Intersection);
        assert!(i.is_ok(), "{i:?}");
        let Ok(im) = &i else { return };
        let vi = volume(im);
        assert!(vi > 0.0 && vi < vc, "{vi}");
        assert_eq!(open_edges(im), 0);
        check(&boolean(&s, &c, BoolOp::Union), vs + vc - vi, "union");
        check(&boolean(&s, &c, BoolOp::Difference), vs - vi, "difference");
    }
}

#[cfg(test)]
mod fuzz_tests {
    //! Random pairs of primitives, turned, scaled and placed at random, half of them on a coarse
    //! grid with right-angle turns so faces, edges and corners often coincide exactly. Every
    //! result must be closed, and the volumes must agree: union + intersection = both, and
    //! difference = first − intersection.

    use super::tests::{open_edges, volume};
    use super::*;
    use crate::primitives;
    use CosmicSlicer_import::fuzz::{self, Rng};
    use glam::{Mat4, Quat};

    fn transformed(m: &Mesh, t: Mat4) -> Mesh {
        Mesh { vertices: m.vertices.iter().map(|v| t.transform_point3(*v)).collect(), triangles: m.triangles.clone() }
    }

    fn placement(rng: &mut Rng, snapped: bool) -> Mat4 {
        if snapped {
            let turn = Quat::from_rotation_z(std::f32::consts::FRAC_PI_2 * rng.below(4) as f32)
                * Quat::from_rotation_x(std::f32::consts::FRAC_PI_2 * rng.below(4) as f32);
            let at = Vec3::new(rng.below(5) as f32, rng.below(5) as f32, rng.below(5) as f32) * 5.0 - 10.0;
            Mat4::from_rotation_translation(turn, at)
        } else {
            let axis = Vec3::new(rng.unit() as f32, rng.unit() as f32, rng.unit() as f32).normalize_or(Vec3::Z);
            let turn = Quat::from_axis_angle(axis, rng.unit() as f32 * std::f32::consts::PI);
            let scale = Vec3::splat(0.5 + rng.unit().abs() as f32);
            let at = Vec3::new(rng.unit() as f32, rng.unit() as f32, rng.unit() as f32) * 15.0;
            Mat4::from_scale_rotation_translation(scale, turn, at)
        }
    }

    fn check(a: &Mesh, b: &Mesh) -> Result<(), String> {
        let (va, vb) = (volume(a), volume(b));
        let tol = (va.abs() + vb.abs()) * 2e-3 + 1e-3;
        let mut v = [0.0; 3];
        for (k, op) in [BoolOp::Union, BoolOp::Intersection, BoolOp::Difference].into_iter().enumerate() {
            let m = boolean(a, b, op).map_err(|e| format!("{op:?}: {e}"))?;
            let open = open_edges(&m);
            if open > 0 {
                return Err(format!("{op:?}: {open} open edges"));
            }
            if let Some(slot) = v.get_mut(k) {
                *slot = volume(&m);
            }
        }
        let [u, i, d] = v;
        if (u + i - va - vb).abs() > tol {
            return Err(format!("union {u} + intersection {i} != {va} + {vb}"));
        }
        if (d - (va - i)).abs() > tol {
            return Err(format!("difference {d} != {va} - {i}"));
        }
        if i < -tol || i > va.min(vb) + tol {
            return Err(format!("intersection {i} outside 0..{}", va.min(vb)));
        }
        Ok(())
    }

    #[test]
    fn random_pairs_combine_closed_with_matching_volumes() {
        let names = ["Cube", "Cylinder", "Sphere", "Cone", "Disc", "Torus"];
        let seed = fuzz::seed_for("boolean", 0);
        let mut rng = Rng::new(seed);
        let mut failures = Vec::new();
        for case in 0..(fuzz::iterations() / 4).max(1) {
            let (na, nb) = (names[rng.below(names.len())], names[rng.below(names.len())]);
            let snapped = rng.chance(2);
            let (ta, tb) = (placement(&mut rng, snapped), placement(&mut rng, snapped));
            // Now and then the very same placement: every face shared.
            let tb = if rng.chance(8) { ta } else { tb };
            let (Some(ma), Some(mb)) = (primitives::by_name(na), primitives::by_name(nb)) else { continue };
            let (a, b) = (transformed(&ma, ta), transformed(&mb, tb));
            let input = format!("{na} at {ta:?}\n{nb} at {tb:?}\n");
            if let Some(f) =
                fuzz::guard(&format!("boolean-case{case}-seed{seed:#x}"), input.as_bytes(), &mut || check(&a, &b))
            {
                failures.push(format!("{f}: {na} and {nb}{}", if snapped { ", snapped" } else { "" }));
                if failures.len() > 5 {
                    break;
                }
            }
        }
        assert!(failures.is_empty(), "{failures:#?}");
    }
}

#[cfg(test)]
mod file_tests {
    use super::tests::{open_edges, volume};
    use super::*;
    use crate::primitives;

    /// `BOOLEAN_FILE=x.stl cargo test -p CosmicSlicer-model boolean_a_file -- --ignored --nocapture`:
    /// a model with a cylinder through its middle, timed.
    #[test]
    #[ignore = "needs BOOLEAN_FILE"]
    fn boolean_a_file() {
        let Ok(path) = std::env::var("BOOLEAN_FILE") else { return };
        let file = crate::load(std::path::Path::new(&path));
        let Some(a) = file.ok().and_then(|f| f.objects.into_iter().next()).map(|o| o.mesh) else { return };
        let b = a.bounds();
        let r = b.size().min_element() * 0.15;
        let mut c = primitives::cylinder(r, b.size().z * 2.0, 64);
        for v in &mut c.vertices {
            *v += Vec3::new(b.centre().x, b.centre().y, b.min.z - b.size().z * 0.5);
        }
        println!("{path}: {} triangles, {} open edges, volume {:.1}", a.triangles.len(), open_edges(&a), volume(&a));
        for op in [BoolOp::Union, BoolOp::Difference, BoolOp::Intersection] {
            let t = std::time::Instant::now();
            let m = boolean(&a, &c, op);
            let secs = t.elapsed().as_secs_f64();
            match m {
                Ok(m) => println!(
                    "{op:?}: {secs:.2} s, {} triangles, {} open edges, volume {:.1}",
                    m.triangles.len(),
                    open_edges(&m),
                    volume(&m)
                ),
                Err(e) => println!("{op:?}: {secs:.2} s, {e}"),
            }
        }
    }
}
