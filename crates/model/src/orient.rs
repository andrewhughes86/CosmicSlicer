//! Auto orient, after OrcaSlicer's (itself after Tweaker-3): try the directions the model's
//! faces point in most, and put down the one that leaves the least overhang to support for
//! the most area resting on the plate, a little against tall and tipping orientations.

use std::collections::HashMap;

use glam::{Mat4, Vec3};

use crate::Mesh;

/// Face-direction groups tried, besides the six axes and the current one.
const CANDIDATES: usize = 24;
/// A new orientation must beat the current one by this much to be worth turning to.
const KEEP: f32 = 0.05;

/// One orientation's measure; lower `cost` is better.
#[derive(Clone, Copy, Debug)]
pub struct Score {
    pub down: Vec3,
    /// Area resting on the plate.
    pub bottom: f32,
    /// Area facing down steeper than the overhang angle (weighted by how steeply).
    pub overhang: f32,
    pub height: f32,
    pub cost: f32,
}

fn faces<'a>(solids: &'a [(&'a Mesh, Mat4)]) -> impl Iterator<Item = ([Vec3; 3], Vec3, f32)> + 'a {
    solids.iter().flat_map(|(mesh, m)| {
        mesh.triangles.iter().filter_map(move |t| {
            let c = mesh.corners(t)?.map(|p| m.transform_point3(p));
            let n = (c[1] - c[0]).cross(c[2] - c[0]);
            let a = n.length() / 2.0;
            (a.is_finite() && a > 1e-9).then(|| (c, n / (2.0 * a), a))
        })
    })
}

/// Scores putting `down` (a unit direction of the placed model) down on the plate.
/// `overhang_deg` is the steepest overhang printed without support, from the vertical.
pub fn score(solids: &[(&Mesh, Mat4)], down: Vec3, overhang_deg: f32) -> Score {
    let down = down.normalize_or(Vec3::NEG_Z);
    let (mut lo, mut hi, mut total) = (f32::INFINITY, f32::NEG_INFINITY, 0.0f32);
    for (c, _, a) in faces(solids) {
        for p in c {
            let d = p.dot(down);
            lo = lo.min(-d);
            hi = hi.max(-d);
        }
        total += a;
    }
    let height = (hi - lo).max(0.0);
    let limit = (90.0 - overhang_deg.clamp(0.0, 89.0)).to_radians().cos();
    let (mut bottom, mut overhang) = (0.0f32, 0.0f32);
    let touch = 0.05 + height * 1e-4;
    for (c, n, a) in faces(solids) {
        let facing = n.dot(down);
        if facing <= limit {
            continue;
        }
        let low = c.iter().map(|p| -p.dot(down)).fold(f32::INFINITY, f32::min);
        let high = c.iter().map(|p| -p.dot(down)).fold(f32::NEG_INFINITY, f32::max);
        if facing > 0.995 && high - lo < touch && low - lo < touch {
            bottom += a;
        } else {
            overhang += a * facing;
        }
    }
    let total = total.max(1e-6);
    let size = solids
        .iter()
        .fold(crate::Aabb::EMPTY, |b, (mesh, m)| b.union(&mesh.bounds_transformed(m)))
        .size()
        .length()
        .max(1e-6);
    let cost = overhang / total + 0.5 / (1.0 + 20.0 * bottom / total) + 0.1 * height / size;
    Score { down, bottom, overhang, height, cost }
}

/// The best direction to put down, if it is clearly better than how the model stands now.
pub fn best_down(solids: &[(&Mesh, Mat4)], overhang_deg: f32) -> Option<Score> {
    // The directions the most area faces, grouped a few degrees apart.
    let mut groups: HashMap<(i32, i32, i32), (Vec3, f32)> = HashMap::new();
    for (_, n, a) in faces(solids) {
        let key = ((n.x * 30.0).round() as i32, (n.y * 30.0).round() as i32, (n.z * 30.0).round() as i32);
        let e = groups.entry(key).or_insert((Vec3::ZERO, 0.0));
        e.0 += n * a;
        e.1 += a;
    }
    let mut ranked: Vec<(Vec3, f32)> = groups.into_values().filter(|(_, a)| *a > 0.0).collect();
    ranked.sort_by(|a, b| b.1.total_cmp(&a.1));
    let mut dirs: Vec<Vec3> = vec![Vec3::NEG_Z, Vec3::Z, Vec3::X, Vec3::NEG_X, Vec3::Y, Vec3::NEG_Y];
    dirs.extend(ranked.iter().take(CANDIDATES).map(|(n, _)| n.normalize_or_zero()).filter(|n| *n != Vec3::ZERO));
    let current = score(solids, Vec3::NEG_Z, overhang_deg);
    let best = dirs
        .iter()
        .skip(1)
        .map(|d| score(solids, *d, overhang_deg))
        .filter(|s| s.cost.is_finite())
        .min_by(|a, b| a.cost.total_cmp(&b.cost))?;
    (best.cost < current.cost - KEEP * current.cost.max(0.05) && best.down.angle_between(Vec3::NEG_Z) > 1e-3)
        .then_some(best)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::primitives;
    use glam::Quat;

    fn turned(mesh: &Mesh, q: Quat) -> Vec<(&Mesh, Mat4)> {
        vec![(mesh, Mat4::from_quat(q))]
    }

    #[test]
    fn a_cone_ends_up_on_its_base() {
        let cone = primitives::cone(10.0, 20.0, 48);
        // Upside down, point on the plate.
        let q = Quat::from_rotation_x(std::f32::consts::PI);
        let best = best_down(&turned(&cone, q), 30.0);
        assert!(best.is_some_and(|b| b.down.angle_between(Vec3::Z) < 0.05), "{best:?}");
        // Standing on its base already: left alone.
        assert!(best_down(&turned(&cone, Quat::IDENTITY), 30.0).is_none());
    }

    #[test]
    fn a_flat_plate_lies_down_and_a_cube_stays() {
        // A 40 x 4 x 30 slab standing on its thin edge.
        let mut slab = primitives::cube(1.0);
        for v in &mut slab.vertices {
            *v *= Vec3::new(40.0, 4.0, 30.0);
        }
        let best = best_down(&[(&slab, Mat4::IDENTITY)], 30.0);
        assert!(best.is_some_and(|b| b.down.y.abs() > 0.99), "{best:?}");
        assert!(best_down(&[(&primitives::cube(20.0), Mat4::IDENTITY)], 30.0).is_none());
        // A ball has no better side.
        assert!(best_down(&[(&primitives::sphere(10.0, 32), Mat4::IDENTITY)], 30.0).is_none());
    }
}
