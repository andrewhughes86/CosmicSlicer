//! Fuzzy skin: walls re-traced through points a set distance apart, each pushed in or out
//! along the wall's normal by noise, for a rough, grippy surface (OrcaSlicer's fuzzy skin in
//! displacement mode). Classic noise is random per point; the others are coherent in 3D, so
//! the texture lines up from layer to layer.

use glam::{DVec2, DVec3};

use crate::settings::{Fuzzy, Noise};
use crate::toolpath::Path;

/// A small, fast hash to [0, 1).
fn hash(mut x: u64) -> f64 {
    x ^= x >> 33;
    x = x.wrapping_mul(0xff51_afd7_ed55_8ccd);
    x ^= x >> 33;
    x = x.wrapping_mul(0xc4ce_b9fe_1a85_ec53);
    x ^= x >> 33;
    (x >> 11) as f64 / (1u64 << 53) as f64
}

fn lattice(i: i64, j: i64, k: i64, salt: u64) -> u64 {
    (i as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15)
        ^ (j as u64).wrapping_mul(0xC2B2_AE3D_27D4_EB4F)
        ^ (k as u64).wrapping_mul(0x1656_67B1_9E37_79F9)
        ^ salt
}

/// Smooth value noise in -1..1.
fn value_noise(p: DVec3) -> f64 {
    let f = p.floor();
    let t = p - f;
    let s = t * t * (DVec3::splat(3.0) - 2.0 * t);
    let (i, j, k) = (f.x as i64, f.y as i64, f.z as i64);
    let v = |a: i64, b: i64, c: i64| hash(lattice(i + a, j + b, k + c, 7)) * 2.0 - 1.0;
    let lerp = |a: f64, b: f64, t: f64| a + (b - a) * t;
    let x00 = lerp(v(0, 0, 0), v(1, 0, 0), s.x);
    let x10 = lerp(v(0, 1, 0), v(1, 1, 0), s.x);
    let x01 = lerp(v(0, 0, 1), v(1, 0, 1), s.x);
    let x11 = lerp(v(0, 1, 1), v(1, 1, 1), s.x);
    lerp(lerp(x00, x10, s.y), lerp(x01, x11, s.y), s.z)
}

/// Octaves of noise added up, each finer and weaker by `persistence`.
fn fractal(p: DVec3, octaves: usize, persistence: f64, shape: impl Fn(f64) -> f64) -> f64 {
    let (mut sum, mut amp, mut freq, mut norm) = (0.0, 1.0, 1.0, 0.0);
    for _ in 0..octaves.clamp(1, 10) {
        sum += shape(value_noise(p * freq)) * amp;
        norm += amp;
        amp *= persistence;
        freq *= 2.0;
    }
    if norm > 0.0 { sum / norm } else { 0.0 }
}

/// Distance to the nearest of random points in unit cells, mapped to -1..1.
fn voronoi(p: DVec3) -> f64 {
    let f = p.floor();
    let mut best = f64::INFINITY;
    for a in -1..=1 {
        for b in -1..=1 {
            for c in -1..=1 {
                let cell = f + DVec3::new(a as f64, b as f64, c as f64);
                let key = lattice(cell.x as i64, cell.y as i64, cell.z as i64, 11);
                let q = cell + DVec3::new(hash(key), hash(key ^ 1), hash(key ^ 2));
                best = best.min(p.distance(q));
            }
        }
    }
    (best.min(1.0) * 2.0 - 1.0).clamp(-1.0, 1.0)
}

/// The push at a point, -1..1, for noise `f.noise`. `along` is the distance round the loop and
/// `length` its length (for ripples), `layer` the layer index.
fn push(f: &Fuzzy, p: DVec3, along: f64, length: f64, layer: usize, k: u64) -> f64 {
    let q = p / f.scale.max(0.01);
    match f.noise {
        Noise::Classic => hash(k) * 2.0 - 1.0,
        Noise::Perlin => fractal(q, f.octaves, f.persistence, |v| v),
        Noise::Billow => fractal(q, f.octaves, f.persistence, |v| v.abs() * 2.0 - 1.0),
        Noise::Ridged => fractal(q, f.octaves, f.persistence, |v| 1.0 - v.abs() * 2.0),
        Noise::Voronoi => voronoi(q),
        Noise::Ripple => {
            // Waves round the loop, shifted every few layers.
            let shift = (layer / f.ripple_layers.max(1)) as f64 * f.ripple_offset;
            let t = along / length.max(1e-9) * f.ripples_per_layer as f64 + shift;
            (t * std::f64::consts::TAU).sin()
        }
    }
}

/// Makes a closed wall fuzzy at height `z` on layer `layer`. The wall's points are replaced, so
/// any overhang marks are dropped (they are worked out after).
pub fn apply(path: &mut Path, f: &Fuzzy, z: f64, layer: usize) {
    let n = path.points.len();
    if !path.closed || n < 3 || f.thickness <= 0.0 {
        return;
    }
    let length = path.length();
    let dist = f.point_distance.max(0.05);
    if !(length.is_finite() && length > dist * 3.0) || length / dist > 1_000_000.0 {
        return;
    }
    // Outward is to the right of a counter-clockwise loop.
    let ccw = crate::geom::signed_area2(&path.points) > 0.0;
    let mut out = Vec::with_capacity((length / dist) as usize + n);
    let mut along = 0.0;
    let mut next = 0.0;
    let mut count = 0u64;
    let seed = (layer as u64).wrapping_mul(0x2545_F491_4F6C_DD1D);
    for i in 0..n {
        let (Some(&a), Some(&b)) = (path.points.get(i), path.points.get((i + 1) % n)) else { continue };
        let seg = a.distance(b);
        if seg < 1e-9 {
            continue;
        }
        let dir = (b - a) / seg;
        let normal = if ccw { DVec2::new(dir.y, -dir.x) } else { DVec2::new(-dir.y, dir.x) };
        while next < along + seg {
            let t = next - along;
            let p = a + dir * t;
            let k = push(f, p.extend(z), next, length, layer, seed ^ count);
            out.push(p + normal * (k * f.thickness));
            count += 1;
            // Classic spacing varies a little, as OrcaSlicer's does, so it does not pattern.
            let jitter = if f.noise == Noise::Classic { 0.5 + hash(seed ^ count ^ 0xABCD) } else { 1.0 };
            next += dist * jitter;
        }
        along += seg;
    }
    if out.len() >= 3 {
        path.points = out;
        path.overhang.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::toolpath::Role;

    fn fuzzy(noise: Noise) -> Fuzzy {
        Fuzzy {
            on: crate::settings::FuzzyWhere::External,
            thickness: 0.3,
            point_distance: 0.5,
            first_layer: false,
            noise,
            scale: 1.0,
            octaves: 4,
            persistence: 0.5,
            ripples_per_layer: 15,
            ripple_offset: 0.5,
            ripple_layers: 1,
        }
    }

    fn square() -> Path {
        let pts = crate::geom::rectangle(DVec2::ZERO, DVec2::splat(10.0)).remove(0);
        Path::new(Role::OuterWall, pts, true, 0.42, 60.0, 1.0)
    }

    #[test]
    fn walls_get_rough_within_the_thickness() {
        for noise in [Noise::Classic, Noise::Perlin, Noise::Billow, Noise::Ridged, Noise::Voronoi, Noise::Ripple] {
            let mut p = square();
            apply(&mut p, &fuzzy(noise), 1.0, 5);
            // About 40 mm / 0.5 mm points, each within 0.3 mm of the square's edge.
            assert!(p.points.len() > 40, "{noise:?}: {}", p.points.len());
            let off = |q: &DVec2| q.x.abs().min((q.x - 10.0).abs()).min(q.y.abs()).min((q.y - 10.0).abs());
            assert!(p.points.iter().all(|q| off(q) <= 0.3 + 1e-9), "{noise:?}");
            assert!(p.points.iter().any(|q| off(q) > 0.05), "{noise:?}");
        }
    }

    #[test]
    fn coherent_noise_lines_up_between_layers() {
        let (mut a, mut b) = (square(), square());
        apply(&mut a, &fuzzy(Noise::Perlin), 1.0, 5);
        apply(&mut b, &fuzzy(Noise::Perlin), 1.02, 6);
        let close = a.points.iter().zip(&b.points).filter(|(p, q)| p.distance(**q) < 0.05).count();
        assert!(close as f64 > a.points.len() as f64 * 0.8, "{close} of {}", a.points.len());
    }
}
