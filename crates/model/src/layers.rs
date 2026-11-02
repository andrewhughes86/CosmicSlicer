//! Variable layer heights: a profile of layer height against height above the plate, made
//! from the model's surfaces (adaptive) or drawn, and smoothed.
//!
//! A profile is a list of (height, layer height) points in order; between them the layer
//! height changes in a straight line.

use glam::Mat4;

use crate::Mesh;

/// Points a profile keeps, so a hostile model cannot make it huge.
const MAX_POINTS: usize = 20_000;

/// The layer height at `z` (straight between points; the end points' beyond them).
pub fn at(profile: &[(f32, f32)], z: f32) -> Option<f32> {
    let first = profile.first()?;
    if z <= first.0 {
        return Some(first.1);
    }
    for pair in profile.windows(2) {
        let (Some(a), Some(b)) = (pair.first(), pair.get(1)) else { continue };
        if z <= b.0 {
            let t = if b.0 - a.0 > 1e-6 { (z - a.0) / (b.0 - a.0) } else { 1.0 };
            return Some(a.1 + (b.1 - a.1) * t);
        }
    }
    profile.last().map(|p| p.1)
}

/// A profile from the surfaces of `mesh` placed by `m`: fine layers where surfaces lie close to
/// flat (the stair steps show most), thick ones up walls. `quality` 0 to 1 (finer).
pub fn adaptive(mesh: &Mesh, m: &Mat4, quality: f32, min: f32, max: f32) -> Vec<(f32, f32)> {
    let b = mesh.bounds_transformed(m);
    let top = b.max.z;
    if !(top.is_finite() && top > 0.0 && max >= min && min > 0.0) {
        return Vec::new();
    }
    // The stair step allowed on a slope, from the quality.
    let q = quality.clamp(0.0, 1.0);
    let cusp = min + (max - min) * (1.0 - q) * 0.5;
    let step = 0.25f32.max(top / MAX_POINTS as f32);
    let n = (top / step).ceil() as usize + 1;
    let mut want = vec![max; n];
    for t in &mesh.triangles {
        let Some(c) = mesh.corners(t) else { continue };
        let c = c.map(|p| m.transform_point3(p));
        let normal = (c[1] - c[0]).cross(c[2] - c[0]).normalize_or_zero();
        let nz = normal.z.abs();
        // Flat tops and bottoms take whatever lands on them; walls take thick layers.
        if !(0.01..0.999).contains(&nz) {
            continue;
        }
        let h = (cusp / nz).clamp(min, max);
        let lo = c.iter().map(|p| p.z).fold(f32::INFINITY, f32::min).max(0.0);
        let hi = c.iter().map(|p| p.z).fold(f32::NEG_INFINITY, f32::max).min(top);
        let (i0, i1) = ((lo / step).floor() as usize, ((hi / step).ceil() as usize).min(n - 1));
        for w in want.iter_mut().take(i1 + 1).skip(i0) {
            *w = w.min(h);
        }
    }
    let mut out: Vec<(f32, f32)> = want.iter().enumerate().map(|(i, h)| (i as f32 * step, *h)).collect();
    simplify(&mut out);
    out
}

/// The profile averaged over `radius` mm either side; `keep_min` keeps the thinnest layers
/// where they were (so fine detail is not lost to the average).
pub fn smooth(profile: &[(f32, f32)], radius: f32, keep_min: bool) -> Vec<(f32, f32)> {
    let (Some(first), Some(last)) = (profile.first(), profile.last()) else { return Vec::new() };
    let top = last.0;
    let step = 0.25f32.max((top - first.0) / MAX_POINTS as f32);
    let n = ((top - first.0) / step).ceil().max(0.0) as usize + 1;
    let r = radius.max(step);
    let samples: Vec<(f32, f32)> =
        (0..n).map(|i| first.0 + i as f32 * step).filter_map(|z| at(profile, z).map(|h| (z, h))).collect();
    let mut out: Vec<(f32, f32)> = samples
        .iter()
        .map(|&(z, h)| {
            let near: Vec<f32> = samples.iter().filter(|(y, _)| (y - z).abs() <= r).map(|p| p.1).collect();
            let avg = near.iter().sum::<f32>() / near.len().max(1) as f32;
            (z, if keep_min { avg.min(h) } else { avg })
        })
        .collect();
    simplify(&mut out);
    out
}

/// Sets the layer height to `height` within `radius` of `z` (drawing on the profile), from a
/// flat `base` when there is no profile yet; `top` is the object's height.
pub fn draw(profile: &[(f32, f32)], base: f32, top: f32, z: f32, radius: f32, height: f32) -> Vec<(f32, f32)> {
    let step = 0.25f32.max(top / MAX_POINTS as f32);
    let n = (top / step).ceil().max(0.0) as usize + 1;
    let mut out: Vec<(f32, f32)> = (0..n)
        .map(|i| {
            let y = i as f32 * step;
            let old = at(profile, y).unwrap_or(base);
            let d = (y - z).abs();
            // Full strength in the middle, fading to nothing at the radius.
            let k = if d >= radius { 0.0 } else { 0.5 + 0.5 * (std::f32::consts::PI * d / radius).cos() };
            (y, old + (height - old) * k)
        })
        .collect();
    simplify(&mut out);
    out
}

/// Drops points that lie on the straight line between their neighbours.
fn simplify(p: &mut Vec<(f32, f32)>) {
    if p.len() < 3 {
        return;
    }
    let mut out = Vec::with_capacity(p.len());
    out.extend(p.first().copied());
    for i in 1..p.len() - 1 {
        let (Some(&a), Some(&b), Some(&c)) = (out.last(), p.get(i), p.get(i + 1)) else { continue };
        let t = if c.0 - a.0 > 1e-6 { (b.0 - a.0) / (c.0 - a.0) } else { 0.0 };
        if (a.1 + (c.1 - a.1) * t - b.1).abs() > 1e-4 {
            out.push(b);
        }
    }
    out.extend(p.last().copied());
    *p = out;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::primitives;

    #[test]
    fn a_sphere_gets_fine_layers_at_its_poles_and_thick_at_its_waist() {
        let s = primitives::sphere(10.0, 64);
        let p = adaptive(&s, &Mat4::IDENTITY, 0.5, 0.08, 0.3);
        let (pole, waist) = (at(&p, 19.5), at(&p, 10.0));
        assert!(pole.is_some_and(|h| h < 0.15), "{pole:?}");
        assert!(waist.is_some_and(|h| h > 0.25), "{waist:?}");
        // A cube's walls are upright: thick all the way.
        let c = adaptive(&primitives::cube(10.0), &Mat4::IDENTITY, 0.5, 0.08, 0.3);
        assert!(c.iter().all(|(_, h)| (*h - 0.3).abs() < 1e-6), "{c:?}");
    }

    #[test]
    fn drawing_and_smoothing() {
        let p = draw(&[], 0.2, 20.0, 10.0, 2.0, 0.1);
        assert!(at(&p, 10.0).is_some_and(|h| (h - 0.1).abs() < 1e-4));
        assert!(at(&p, 5.0).is_some_and(|h| (h - 0.2).abs() < 1e-4));
        let s = smooth(&p, 4.0, false);
        let mid = at(&s, 10.0).unwrap_or(0.0);
        assert!(mid > 0.1 && mid < 0.2, "{mid}");
        // Keeping the minimum leaves the thin layers thin.
        let k = smooth(&p, 4.0, true);
        assert!(at(&k, 10.0).is_some_and(|h| h <= 0.1001));
    }
}
