//! Cutting a mesh into layers: where each layer's plane crosses the triangles, chained into
//! closed loops and filled where they wind.
//!
//! Crossing points are keyed by the mesh edge they lie on, so the two triangles sharing an
//! edge give the very same point and loops close exactly. Open meshes leave open chains;
//! their ends are joined to the nearest other end when the gap is small, as OrcaSlicer's
//! "close holes" does.

use std::collections::HashMap;

use glam::{DMat4, DVec2, DVec3, Mat4, Vec3};
use rayon::prelude::*;

use crate::clipper::{Path, Pt};
use crate::geom::{self, Contour, Join, Shape};
use CosmicSlicer_model::Mesh;

/// Millimetres per unit of Orca's integers.
const NM_MM: f64 = 1.0e-6;

/// The heights layers are printed at.
#[derive(Clone, Debug, PartialEq)]
pub struct LayerZ {
    /// Top of the layer (where the nozzle is).
    pub print_z: f64,
    pub height: f64,
}

impl LayerZ {
    /// The height the mesh is cut at: the middle of the layer.
    pub fn slice_z(&self) -> f64 {
        self.print_z - self.height / 2.0
    }
}

/// Most layers sliced, so a hostile height cannot exhaust memory.
pub const MAX_LAYERS: usize = 200_000;

/// The layers up to `top`: the first `first` high, the rest `h`.
pub fn layer_heights(top: f64, first: f64, h: f64) -> Vec<LayerZ> {
    let mut out = Vec::new();
    if !(top.is_finite() && top > 0.0 && first > 0.0 && h > 0.0) {
        return out;
    }
    let mut z = first.min(top.max(first));
    out.push(LayerZ { print_z: z, height: first });
    while z + 1.0e-6 < top && out.len() < MAX_LAYERS {
        // The last layer may be thinner so the print stops at the model's top.
        let step = if top - z < h * 0.5 && top - z > 0.0 { h.max(top - z) } else { h };
        z += step;
        out.push(LayerZ { print_z: z, height: step });
    }
    out
}

/// The layers up to `top`: the first `first` high, then each as high as `height_at` says at
/// its middle (clamped to `lo..=hi`), the last one ending at the top.
pub fn layer_heights_by(top: f64, first: f64, lo: f64, hi: f64, height_at: &dyn Fn(f64) -> f64) -> Vec<LayerZ> {
    let mut out = Vec::new();
    if !(top.is_finite() && top > 0.0 && first > 0.0 && lo > 0.0 && hi >= lo) {
        return out;
    }
    let mut z = first.min(top.max(first));
    out.push(LayerZ { print_z: z, height: first });
    let pick = |at: f64| {
        let h = height_at(at);
        if h.is_finite() { h.max(lo).min(hi) } else { hi }
    };
    while z + 1.0e-6 < top && out.len() < MAX_LAYERS {
        // Look at the layer's middle: guess its height, then take the height asked there.
        let guess = pick(z);
        let mut h = pick(z + guess / 2.0);
        // Do not leave a sliver at the top: stretch or share the last bit.
        let left = top - z;
        if left < h + lo * 0.5 {
            h = if left <= hi { left.max(lo.min(left)) } else { left / 2.0 };
        }
        z += h;
        out.push(LayerZ { print_z: z, height: h });
    }
    out
}

/// A mesh placed in the scene, ready to slice: world-space vertices.
pub struct Placed {
    pub vertices: Vec<DVec3>,
    pub triangles: Vec<[u32; 3]>,
    /// What OrcaSlicer slices: floats around the object's centre, XY in nanometres.
    pub local: Vec<Vec3>,
    /// The object's centre in nanometres (`PrintObject::m_center_offset`), which puts slices
    /// back on the bed.
    pub shift: (i64, i64),
}

impl Placed {
    pub fn new(mesh: &Mesh, matrix: &Mat4) -> Placed {
        let m = matrix.as_dmat4();
        let mirrored = m.determinant() < 0.0;
        let vertices: Vec<DVec3> = mesh.vertices.iter().map(|v| m.transform_point3(v.as_dvec3())).collect();
        let (lo, hi) = vertices
            .iter()
            .filter(|v| v.is_finite())
            .fold((DVec2::splat(f64::INFINITY), DVec2::splat(f64::NEG_INFINITY)), |(lo, hi), v| {
                (lo.min(v.truncate()), hi.max(v.truncate()))
            });
        let centre = if lo.is_finite() && hi.is_finite() { (lo + hi) / 2.0 } else { DVec2::ZERO };
        let shift = ((centre.x / NM_MM).round() as i64, (centre.y / NM_MM).round() as i64);
        let to_local = DMat4::from_scale(DVec3::new(1.0 / NM_MM, 1.0 / NM_MM, 1.0))
            * DMat4::from_translation(DVec3::new(shift.0 as f64 * NM_MM, shift.1 as f64 * NM_MM, 0.0)).inverse()
            * m;
        let trafo = to_local.as_mat4();
        let (x, y, z, w) = (trafo.x_axis, trafo.y_axis, trafo.z_axis, trafo.w_axis);
        // `Transform3f` times a vertex, in floats.
        let local = mesh.vertices.iter().map(|v| (x * v.x + y * v.y + z * v.z + w).truncate()).collect();
        // A mirrored placement turns the triangles inside out; turn them back.
        let triangles = mesh.triangles.iter().map(|t| if mirrored { [t[0], t[2], t[1]] } else { *t }).collect();
        Placed { vertices, triangles, local, shift }
    }

    pub fn top(&self) -> f64 {
        self.vertices.iter().map(|v| v.z).fold(0.0, f64::max)
    }
}

/// The slices of `mesh` at every layer: one set of shapes per layer. `closing` is the "slice
/// gap closing radius".
pub fn slice(
    mesh: &Placed,
    layers: &[LayerZ],
    closing: f64,
    resolution: f64,
    cancelled: &(dyn Fn() -> bool + Sync),
) -> Vec<Vec<Shape>> {
    let zs: Vec<f32> = layers.iter().map(|l| l.slice_z() as f32).collect();
    // Triangles by the layers they span, so each layer only looks at its own.
    let mut buckets: Vec<Vec<u32>> = vec![Vec::new(); zs.len()];
    for (ti, t) in mesh.triangles.iter().enumerate() {
        let Some(p) = local_corners(mesh, t) else { continue };
        let lo = p[0].z.min(p[1].z.min(p[2].z));
        let hi = p[0].z.max(p[1].z.max(p[2].z));
        if lo == hi {
            continue;
        }
        let first = zs.partition_point(|z| *z < lo);
        let last = zs.partition_point(|z| *z <= hi);
        for b in buckets.iter_mut().take(last).skip(first) {
            b.push(ti as u32);
        }
    }
    zs.par_iter()
        .zip(buckets.par_iter())
        .map(|(z, tris)| if cancelled() { Vec::new() } else { slice_layer(mesh, tris, *z, closing, resolution) })
        .collect()
}

/// Where the triangles `tris` of `mesh` lie over each layer: each triangle cut to the slab
/// from half a layer under the layer's bottom to its top, seen from above. Painted support
/// uses this: an overhang's painted faces cover the area it needs held. Faces that stand
/// upright (walls) are lines from above; they become strips `pad` either side.
pub fn painted(mesh: &Placed, tris: &[u32], layers: &[LayerZ], pad: f64) -> Vec<Vec<Shape>> {
    let mut loops: Vec<Vec<Contour>> = vec![Vec::new(); layers.len()];
    let tops: Vec<f64> = layers.iter().map(|l| l.print_z).collect();
    for ti in tris {
        let Some(c) = mesh.triangles.get(*ti as usize).and_then(|t| corners(mesh, t)) else { continue };
        let lo = c.iter().map(|p| p.z).fold(f64::INFINITY, f64::min);
        let hi = c.iter().map(|p| p.z).fold(f64::NEG_INFINITY, f64::max);
        let first = tops.partition_point(|z| *z < lo);
        for (i, l) in layers.iter().enumerate().skip(first) {
            let bottom = l.print_z - l.height * 1.5;
            if bottom > hi {
                break;
            }
            let piece = clip_z(&clip_z(&c, bottom, true), l.print_z, false);
            if piece.len() >= 3 {
                let mut flat: Contour = piece.iter().map(|p| DVec2::new(p.x, p.y)).collect();
                if geom::signed_area2(&flat).abs() < 1e-6 {
                    flat = strip(&flat, pad);
                }
                if geom::signed_area2(&flat) < 0.0 {
                    flat.reverse();
                }
                if let Some(layer) = loops.get_mut(i) {
                    layer.push(flat);
                }
            }
        }
    }
    loops.iter().map(|l| if l.is_empty() { Vec::new() } else { geom::union_loops(l) }).collect()
}

/// A rectangle `pad` either side of (and past the ends of) the longest span of `pts`.
fn strip(pts: &[DVec2], pad: f64) -> Contour {
    let mut best = (0.0, DVec2::ZERO, DVec2::ZERO);
    for (i, a) in pts.iter().enumerate() {
        for b in pts.iter().skip(i + 1) {
            let d = a.distance(*b);
            if d > best.0 {
                best = (d, *a, *b);
            }
        }
    }
    let (len, a, b) = best;
    if len < 1e-9 || pad <= 0.0 {
        return Vec::new();
    }
    let dir = (b - a) / len;
    let side = dir.perp() * pad;
    let (a, b) = (a - dir * pad, b + dir * pad);
    vec![a - side, b - side, b + side, a + side]
}

/// The part of polygon `poly` above (`keep_above`) or below the plane at `z`.
fn clip_z(poly: &[DVec3], z: f64, keep_above: bool) -> Vec<DVec3> {
    let inside = |p: &DVec3| if keep_above { p.z >= z } else { p.z <= z };
    let mut out = Vec::new();
    for (k, a) in poly.iter().enumerate() {
        let Some(b) = poly.get((k + 1) % poly.len()) else { continue };
        if inside(a) {
            out.push(*a);
        }
        if inside(a) != inside(b) {
            let t = ((z - a.z) / (b.z - a.z)).clamp(0.0, 1.0);
            out.push(*a + (*b - *a) * t);
        }
    }
    out
}

fn corners(mesh: &Placed, t: &[u32; 3]) -> Option<[DVec3; 3]> {
    let v = |i: u32| mesh.vertices.get(i as usize).copied().filter(|p| p.is_finite());
    Some([v(t[0])?, v(t[1])?, v(t[2])?])
}

fn local_corners(mesh: &Placed, t: &[u32; 3]) -> Option<[Vec3; 3]> {
    let v = |i: u32| mesh.local.get(i as usize).copied().filter(|p| p.is_finite() && p.abs().max_element() < 1.0e15);
    Some([v(t[0])?, v(t[1])?, v(t[2])?])
}

/// Where a line's end lies: on a mesh vertex, or on an edge, by its two vertices.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Node {
    Vertex(u32),
    Edge(u32, u32),
}

fn edge_node(a: u32, b: u32) -> Node {
    if a < b { Node::Edge(a, b) } else { Node::Edge(b, a) }
}

struct Line {
    a: Pt,
    b: Pt,
    from: Node,
    to: Node,
}

/// `slice_facet`: the line a triangle leaves at height `z`, the solid on its left. `v` holds the
/// vertices of `ids`, and `lowest` is which of them is lowest.
fn slice_facet(z: f32, v: &[Vec3; 3], ids: &[u32; 3], lowest: usize) -> Option<Line> {
    let mut points: [(Pt, Node); 3] = [(Pt::default(), Node::Vertex(0)); 3];
    let mut count = 0;
    let mut on_layer: Option<usize> = None;
    let at = |p: Vec3| Pt::new(p.x as i64, p.y as i64);
    for j in 0..3 {
        let (k, l) = ((lowest + j) % 3, (lowest + j + 1) % 3);
        let (mut a, mut b, mut a_id, mut b_id) = (v[k], v[l], ids[k], ids[l]);
        if a.z == z && b.z == z {
            // An edge in the plane is the line of the triangle under it.
            if !(v[0].z < z || v[1].z < z || v[2].z < z) {
                return None;
            }
            return Some(Line { a: at(b), b: at(a), from: Node::Vertex(b_id), to: Node::Vertex(a_id) });
        }
        let same =
            |on: Option<usize>, points: &[(Pt, Node); 3], id: u32| on.is_some_and(|i| points[i].1 == Node::Vertex(id));
        if a.z == z {
            if !same(on_layer, &points, a_id) {
                on_layer = Some(count);
                points[count] = (at(a), Node::Vertex(a_id));
                count += 1;
            }
        } else if b.z == z {
            if !same(on_layer, &points, b_id) {
                on_layer = Some(count);
                points[count] = (at(b), Node::Vertex(b_id));
                count += 1;
            }
        } else if (a.z < z && b.z > z) || (b.z < z && a.z > z) {
            // In the same order from both sides of the edge.
            if a_id > b_id {
                std::mem::swap(&mut a_id, &mut b_id);
                std::mem::swap(&mut a, &mut b);
            }
            let t = (f64::from(z) - f64::from(b.z)) / (f64::from(a.z) - f64::from(b.z));
            if t <= 0.0 {
                if !same(on_layer, &points, a_id) {
                    on_layer = Some(count);
                    points[count] = (at(a), Node::Vertex(a_id));
                    count += 1;
                }
            } else if t >= 1.0 {
                if !same(on_layer, &points, b_id) {
                    on_layer = Some(count);
                    points[count] = (at(b), Node::Vertex(b_id));
                    count += 1;
                }
            } else {
                let cross =
                    |pa: f32, pb: f32| (f64::from(pb) + (f64::from(pa) - f64::from(pb)) * t + 0.5).floor() as i64;
                points[count] = (Pt::new(cross(a.x, b.x), cross(a.y, b.y)), edge_node(a_id, b_id));
                count += 1;
            }
        }
        if count > 2 {
            return None;
        }
    }
    (count == 2).then(|| Line { a: points[1].0, b: points[0].0, from: points[1].1, to: points[0].1 })
}

/// One layer: the triangles' lines chained into loops, gaps closed, simplified (`slice_mesh_ex`).
fn slice_layer(mesh: &Placed, tris: &[u32], z: f32, closing: f64, resolution: f64) -> Vec<Shape> {
    let mut lines: Vec<Line> = Vec::with_capacity(tris.len());
    for &ti in tris {
        let Some(t) = mesh.triangles.get(ti as usize) else { continue };
        let Some(v) = local_corners(mesh, t) else { continue };
        let lo = v[0].z.min(v[1].z.min(v[2].z));
        let lowest = if v[1].z == lo {
            1
        } else if v[2].z == lo {
            2
        } else {
            0
        };
        lines.extend(slice_facet(z, &v, t, lowest));
    }
    let mut by_start: HashMap<Node, Vec<u32>> = HashMap::with_capacity(lines.len());
    for (i, l) in lines.iter().enumerate().rev() {
        by_start.entry(l.from).or_default().push(i as u32);
    }
    let mut used = vec![false; lines.len()];
    let (mut loops, mut open): (Vec<Path>, Vec<Path>) = (Vec::new(), Vec::new());
    for start in 0..lines.len() {
        if used.get(start).copied().unwrap_or(true) {
            continue;
        }
        let mut at = start;
        let mut chain: Path = Vec::new();
        let mut closed = false;
        while let Some(line) = lines.get(at) {
            if let Some(u) = used.get_mut(at) {
                *u = true;
            }
            chain.push(line.a);
            if lines.get(start).is_some_and(|s| s.from == line.to) {
                closed = true;
                break;
            }
            let Some(list) = by_start.get_mut(&line.to) else {
                chain.push(line.b);
                break;
            };
            let mut next = None;
            while let Some(i) = list.pop() {
                if !used.get(i as usize).copied().unwrap_or(true) {
                    next = Some(i as usize);
                    break;
                }
            }
            match next {
                Some(i) => at = i,
                None => {
                    chain.push(line.b);
                    break;
                }
            }
        }
        if closed {
            loops.push(chain);
        } else if chain.len() >= 2 {
            open.push(chain);
        }
    }
    loops.extend(close_gaps(open, 1_000_000.0));
    for l in &mut loops {
        for p in l.iter_mut() {
            *p = Pt::new(p.x + mesh.shift.0, p.y + mesh.shift.1);
        }
    }
    let solid = geom::union_paths(&loops);
    let closed = if closing > 0.0 {
        let radius = closing / 1.0e-6;
        geom::offset2_nm(&solid, radius as f32, -radius as f32, Join::Miter)
    } else {
        solid
    };
    if resolution <= 0.0 {
        return closed;
    }
    closed.iter().flat_map(|shape| geom::union_paths(&geom::simplify_p(shape, resolution / 1.0e-6))).collect()
}

/// Joins open chains end to start where the gap is under `max_gap`, closing what it can.
fn close_gaps(mut open: Vec<Path>, max_gap: f64) -> Vec<Path> {
    let gap = |a: Pt, b: Pt| ((a.x - b.x) as f64).hypot((a.y - b.y) as f64);
    let mut out = Vec::new();
    while let Some(mut chain) = open.pop() {
        while let (Some(&first), Some(&last)) = (chain.first(), chain.last()) {
            if chain.len() >= 3 && gap(first, last) < max_gap {
                out.push(chain);
                break;
            }
            // The chain whose start is nearest this one's end.
            let best = open
                .iter()
                .enumerate()
                .filter_map(|(i, c)| c.first().map(|p| (i, gap(*p, last))))
                .min_by(|a, b| a.1.total_cmp(&b.1));
            match best {
                Some((i, d)) if d < max_gap => {
                    let next = open.swap_remove(i);
                    chain.extend(next);
                }
                _ => break,
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use CosmicSlicer_model::primitives;
    use glam::Vec3;

    fn never() -> bool {
        false
    }

    #[test]
    fn a_cube_slices_into_squares() {
        let mesh = primitives::cube(20.0);
        let placed = Placed::new(&mesh, &Mat4::from_translation(Vec3::new(10.0, 10.0, 0.0)));
        let layers = layer_heights(placed.top(), 0.2, 0.2);
        assert_eq!(layers.len(), 100);
        let slices = slice(&placed, &layers, 0.049, 0.0025, &never);
        // Chained in the mesh's own winding, the outline runs counter-clockwise.
        assert!(
            slices.iter().all(|s| s.first().and_then(|sh| sh.first()).is_some_and(|c| geom::signed_area2(c) > 0.0))
        );
        for s in &slices {
            assert_eq!(s.len(), 1);
            assert!((geom::area(s) - 400.0).abs() < 1e-6, "{}", geom::area(s));
        }
    }

    #[test]
    fn vertices_on_the_plane_keep_the_loop_whole() {
        // A 10 mm block with a 5 mm column on it; the ledge between them lies exactly at
        // 0.5 mm, where the third layer (0.4 to 0.6) is cut. Vertices on the plane once broke
        // the loops there into scraps.
        let square = |half: f32, z: f32| {
            [(-half, -half), (half, -half), (half, half), (-half, half)].map(|(x, y)| Vec3::new(x, y, z))
        };
        let mut vertices = Vec::new();
        for (half, z) in [(5.0, 0.0), (5.0, 0.5), (2.5, 0.5), (2.5, 10.0)] {
            vertices.extend(square(half, z));
        }
        // Rings 0 (bottom), 1 (ledge, outside), 2 (ledge, inside), 3 (top).
        let mut triangles = vec![[0, 2, 1], [0, 3, 2], [12, 13, 14], [12, 14, 15]];
        for (lower, upper) in [(0u32, 1u32), (1, 2), (2, 3)] {
            for k in 0..4u32 {
                let (a, b) = (lower * 4 + k, lower * 4 + (k + 1) % 4);
                let (c, d) = (upper * 4 + (k + 1) % 4, upper * 4 + k);
                triangles.push([a, b, c]);
                triangles.push([a, c, d]);
            }
        }
        let mesh = Mesh { vertices, triangles };
        let placed = Placed::new(&mesh, &glam::Mat4::IDENTITY);
        // Lines ending on the ledge's vertices chain through them (Benchy's hull at 6.5 mm).
        let layers = layer_heights(placed.top(), 0.2, 0.2);
        let out = slice(&placed, &layers, 0.049, 0.0025, &|| false);
        for (l, s) in layers.iter().zip(&out) {
            let a = geom::area(s);
            assert_eq!(s.len(), 1, "z {}: {a}", l.print_z);
            let want: &[f64] = if l.slice_z() < 0.49 {
                &[100.0]
            } else if l.slice_z() > 0.51 {
                &[25.0]
            } else {
                &[100.0, 25.0]
            };
            assert!(want.iter().any(|w| (a - w).abs() < 1e-3), "z {}: {a}", l.print_z);
        }
    }

    #[test]
    fn a_torus_has_a_hole() {
        let mesh = primitives::torus(15.0, 5.0, 64, 24);
        let placed = Placed::new(&mesh, &Mat4::IDENTITY);
        let layers = layer_heights(placed.top(), 0.2, 0.2);
        let middle =
            slice(&placed, &layers, 0.049, 0.0025, &never).into_iter().nth(layers.len() / 2).unwrap_or_default();
        assert_eq!(middle.len(), 1);
        assert_eq!(middle[0].len(), 2, "outer and hole");
        let a = geom::area(&middle);
        let expected = std::f64::consts::PI * (20.0f64.powi(2) - 10.0f64.powi(2));
        assert!((a - expected).abs() / expected < 0.02, "{a} vs {expected}");
    }

    #[test]
    fn mirrored_and_inside_out_meshes_still_fill() {
        let mesh = primitives::cylinder(10.0, 10.0, 64);
        let placed = Placed::new(&mesh, &Mat4::from_scale(Vec3::new(-1.0, 1.0, 1.0)));
        let layers = layer_heights(placed.top(), 0.2, 0.2);
        let s = slice(&placed, &layers, 0.049, 0.0025, &never);
        assert!(s.iter().all(|l| geom::area(l) > 300.0));
    }

    #[test]
    fn layer_heights_end_at_the_top() {
        let l = layer_heights(10.05, 0.3, 0.2);
        assert!((l.first().map_or(0.0, |l| l.print_z) - 0.3).abs() < 1e-9);
        let top = l.last().map_or(0.0, |l| l.print_z);
        assert!((top - 10.05).abs() < 0.11, "{top}");
        assert!(layer_heights(f64::NAN, 0.2, 0.2).is_empty());
        assert!(layer_heights(1e12, 0.2, 0.2).len() <= MAX_LAYERS);
    }
}
