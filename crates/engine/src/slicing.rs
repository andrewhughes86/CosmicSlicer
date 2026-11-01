//! Cutting a mesh into layers: where each layer's plane crosses the triangles, chained into
//! closed loops and filled where they wind.
//!
//! Crossing points are keyed by the mesh edge they lie on, so the two triangles sharing an
//! edge give the very same point and loops close exactly. Open meshes leave open chains;
//! their ends are joined to the nearest other end when the gap is small, as OrcaSlicer's
//! "close holes" does.

use std::collections::HashMap;

use glam::{DVec2, DVec3, Mat4};
use rayon::prelude::*;

use crate::geom::{self, Contour, Shape};
use CosmicSlicer_model::Mesh;

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

/// A mesh placed in the scene, ready to slice: world-space vertices.
pub struct Placed {
    pub vertices: Vec<DVec3>,
    pub triangles: Vec<[u32; 3]>,
}

impl Placed {
    pub fn new(mesh: &Mesh, matrix: &Mat4) -> Placed {
        let m = matrix.as_dmat4();
        let mirrored = m.determinant() < 0.0;
        let vertices = mesh.vertices.iter().map(|v| m.transform_point3(v.as_dvec3())).collect();
        // A mirrored placement turns the triangles inside out; turn them back.
        let triangles = mesh.triangles.iter().map(|t| if mirrored { [t[0], t[2], t[1]] } else { *t }).collect();
        Placed { vertices, triangles }
    }

    pub fn top(&self) -> f64 {
        self.vertices.iter().map(|v| v.z).fold(0.0, f64::max)
    }
}

/// The slices of `mesh` at every layer: one set of shapes per layer.
pub fn slice(mesh: &Placed, layers: &[LayerZ], cancelled: &(dyn Fn() -> bool + Sync)) -> Vec<Vec<Shape>> {
    let zs: Vec<f64> = layers.iter().map(LayerZ::slice_z).collect();
    // Triangles by the layers they span, so each layer only looks at its own.
    let mut buckets: Vec<Vec<u32>> = vec![Vec::new(); zs.len()];
    for (ti, t) in mesh.triangles.iter().enumerate() {
        let Some(corners) = corners(mesh, t) else { continue };
        let lo = corners.iter().map(|p| p.z).fold(f64::INFINITY, f64::min);
        let hi = corners.iter().map(|p| p.z).fold(f64::NEG_INFINITY, f64::max);
        let first = zs.partition_point(|z| *z < lo);
        let last = zs.partition_point(|z| *z <= hi);
        for b in buckets.iter_mut().take(last).skip(first) {
            b.push(ti as u32);
        }
    }
    zs.par_iter()
        .zip(buckets.par_iter())
        .map(|(z, tris)| if cancelled() { Vec::new() } else { slice_layer(mesh, tris, *z) })
        .collect()
}

fn corners(mesh: &Placed, t: &[u32; 3]) -> Option<[DVec3; 3]> {
    let v = |i: u32| mesh.vertices.get(i as usize).copied().filter(|p| p.is_finite());
    Some([v(t[0])?, v(t[1])?, v(t[2])?])
}

/// The point where the plane at `z` crosses edge `a`–`b`.
fn cross(a: DVec3, b: DVec3, z: f64) -> DVec2 {
    let t = ((z - a.z) / (b.z - a.z)).clamp(0.0, 1.0);
    let p = a + (b - a) * t;
    DVec2::new(p.x, p.y)
}

type EdgeKey = (u32, u32);

fn key(a: u32, b: u32) -> EdgeKey {
    if a < b { (a, b) } else { (b, a) }
}

/// One layer: crossing segments, chained into loops, then filled.
fn slice_layer(mesh: &Placed, tris: &[u32], z: f64) -> Vec<Shape> {
    // Each segment runs from the edge it enters by to the edge it leaves by, oriented so the
    // solid is on its left (outer loops counter-clockwise).
    let mut next: HashMap<EdgeKey, (EdgeKey, DVec2)> = HashMap::with_capacity(tris.len());
    let mut points: HashMap<EdgeKey, DVec2> = HashMap::with_capacity(tris.len());
    for &ti in tris {
        let Some(t) = mesh.triangles.get(ti as usize) else { continue };
        let Some(p) = corners(mesh, t) else { continue };
        // A vertex exactly on the plane counts as above it.
        let above = p.map(|q| q.z >= z);
        let n_above = above.iter().filter(|a| **a).count();
        if n_above == 0 || n_above == 3 {
            continue;
        }
        // The lone vertex on its side, and the two edges to it.
        let lone = (0..3).find(|&i| above[i] == (n_above == 1)).unwrap_or(0);
        let (i1, i2) = ((lone + 1) % 3, (lone + 2) % 3);
        let (ka, kb) = (key(t[lone], t[i1]), key(t[lone], t[i2]));
        let pa = cross(p[lone], p[i1], z);
        let pb = cross(p[lone], p[i2], z);
        // With the lone vertex above, going from its edge to i1 towards its edge to i2 keeps
        // the inside on the left for a counter-clockwise triangle; below, the other way.
        let (from, to, pf, pt) = if n_above == 1 { (ka, kb, pa, pb) } else { (kb, ka, pb, pa) };
        if pf.distance_squared(pt) < 1.0e-18 {
            continue;
        }
        points.insert(from, pf);
        points.insert(to, pt);
        next.insert(from, (to, pt));
    }
    let mut loops: Vec<Contour> = Vec::new();
    let mut open: Vec<Vec<DVec2>> = Vec::new();
    let mut starts: Vec<EdgeKey> = next.keys().copied().collect();
    starts.sort_unstable();
    for start in starts {
        if !next.contains_key(&start) {
            continue;
        }
        let mut chain = vec![points.get(&start).copied().unwrap_or_default()];
        let mut at = start;
        let mut closed = false;
        while let Some((to, p)) = next.remove(&at) {
            if to == start {
                closed = true;
                break;
            }
            chain.push(p);
            at = to;
            if chain.len() > 10_000_000 {
                break;
            }
        }
        if closed {
            loops.push(chain);
        } else if chain.len() >= 2 {
            open.push(chain);
        }
    }
    loops.extend(close_gaps(open, 1.0));
    let loops: Vec<Contour> = loops.into_iter().map(|l| geom::simplify_loop(&l, 0.0125)).collect();
    geom::union_loops(&loops)
}

/// Joins open chains end to start where the gap is under `max_gap`, closing what it can.
fn close_gaps(mut open: Vec<Vec<DVec2>>, max_gap: f64) -> Vec<Contour> {
    let mut out = Vec::new();
    while let Some(mut chain) = open.pop() {
        while let (Some(&first), Some(&last)) = (chain.first(), chain.last()) {
            if chain.len() >= 3 && first.distance(last) < max_gap {
                out.push(chain);
                break;
            }
            // The chain whose start is nearest this one's end.
            let best = open
                .iter()
                .enumerate()
                .filter_map(|(i, c)| c.first().map(|p| (i, p.distance(last))))
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
        let slices = slice(&placed, &layers, &never);
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
    fn a_torus_has_a_hole() {
        let mesh = primitives::torus(15.0, 5.0, 64, 24);
        let placed = Placed::new(&mesh, &Mat4::IDENTITY);
        let layers = layer_heights(placed.top(), 0.2, 0.2);
        let middle = slice(&placed, &layers, &never).into_iter().nth(layers.len() / 2).unwrap_or_default();
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
        let s = slice(&placed, &layers, &never);
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
