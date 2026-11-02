//! The shapes of OrcaSlicer's "Add Primitive" menu.

use std::f32::consts::TAU;

use glam::Vec3;

use crate::Mesh;

/// Primitive shapes by menu name: (name, icon).
pub const ALL: &[(&str, &str)] = &[
    ("Cube", "menu_obj_cube"),
    ("Cylinder", "menu_obj_cylinder"),
    ("Sphere", "menu_obj_sphere"),
    ("Cone", "menu_obj_cone"),
    ("Disc", "menu_obj_disc"),
    ("Torus", "menu_obj_torus"),
];

/// The named primitive at OrcaSlicer's default sizes.
pub fn by_name(name: &str) -> Option<Mesh> {
    Some(match name.to_ascii_lowercase().as_str() {
        "cube" => cube(20.0),
        "cylinder" => cylinder(10.0, 20.0, 64),
        "sphere" => sphere(10.0, 48),
        "cone" => cone(10.0, 20.0, 64),
        "disc" => cylinder(25.0, 1.0, 96),
        "torus" => torus(15.0, 5.0, 64, 24),
        _ => return None,
    })
}

pub fn cube(size: f32) -> Mesh {
    let s = size;
    let vertices = vec![
        Vec3::new(0.0, 0.0, 0.0),
        Vec3::new(s, 0.0, 0.0),
        Vec3::new(s, s, 0.0),
        Vec3::new(0.0, s, 0.0),
        Vec3::new(0.0, 0.0, s),
        Vec3::new(s, 0.0, s),
        Vec3::new(s, s, s),
        Vec3::new(0.0, s, s),
    ];
    let triangles = vec![
        [0, 2, 1],
        [0, 3, 2],
        [4, 5, 6],
        [4, 6, 7],
        [0, 1, 5],
        [0, 5, 4],
        [1, 2, 6],
        [1, 6, 5],
        [2, 3, 7],
        [2, 7, 6],
        [3, 0, 4],
        [3, 4, 7],
    ];
    Mesh { vertices, triangles }
}

/// A solid of revolution about Z through `profile` points (radius, z), capped at both ends.
/// Rings of radius 0 (poles, a cone's tip) are one shared vertex, and a profile that ends where
/// it starts (the torus) closes onto its first ring, so the mesh is closed.
fn lathe(profile: &[(f32, f32)], segments: u32, capped: bool) -> Mesh {
    let segments = segments.max(3);
    let mut mesh = Mesh::default();
    let rings = profile.len() as u32;
    let closed = rings > 2 && profile.first() == profile.last();
    // Vertex of each segment and ring, segment by segment.
    let mut ids = vec![0u32; (segments * rings) as usize];
    let slot = |seg: u32, ring: u32| ((seg % segments) * rings + ring) as usize;
    for (ring, &(r, z)) in profile.iter().enumerate() {
        let ring = ring as u32;
        if closed && ring == rings - 1 {
            for seg in 0..segments {
                let first = ids.get(slot(seg, 0)).copied().unwrap_or(0);
                if let Some(id) = ids.get_mut(slot(seg, ring)) {
                    *id = first;
                }
            }
            continue;
        }
        let axis = (r == 0.0).then(|| {
            mesh.vertices.push(Vec3::new(0.0, 0.0, z));
            (mesh.vertices.len() - 1) as u32
        });
        for seg in 0..segments {
            let id = axis.unwrap_or_else(|| {
                let a = TAU * seg as f32 / segments as f32;
                mesh.vertices.push(Vec3::new(r * a.cos(), r * a.sin(), z));
                (mesh.vertices.len() - 1) as u32
            });
            if let Some(slot) = ids.get_mut(slot(seg, ring)) {
                *slot = id;
            }
        }
    }
    let at = |seg: u32, ring: u32| ids.get(slot(seg, ring)).copied().unwrap_or(0);
    for s in 0..segments {
        for r in 0..rings.saturating_sub(1) {
            let (a, b, c, d) = (at(s, r), at(s + 1, r), at(s + 1, r + 1), at(s, r + 1));
            mesh.triangles.push([a, b, c]);
            mesh.triangles.push([a, c, d]);
        }
    }
    let mut cap = |ring: u32, up: bool| {
        let Some(&(_, z)) = profile.get(ring as usize) else { return };
        let centre = mesh.vertices.len() as u32;
        mesh.vertices.push(Vec3::new(0.0, 0.0, z));
        for s in 0..segments {
            let (a, b) = (at(s, ring), at(s + 1, ring));
            mesh.triangles.push(if up { [centre, a, b] } else { [centre, b, a] });
        }
    };
    if capped && profile.first().is_some_and(|p| p.0 > 0.0) {
        cap(0, false);
    }
    if capped && profile.last().is_some_and(|p| p.0 > 0.0) {
        cap(rings - 1, true);
    }
    // Rings of radius 0 collapse to a point; drop the degenerate faces they leave.
    mesh.triangles.retain(|t| {
        let v = |i: u32| mesh.vertices.get(i as usize).copied().unwrap_or(Vec3::ZERO);
        t[0] != t[1]
            && t[1] != t[2]
            && t[0] != t[2]
            && (v(t[1]) - v(t[0])).cross(v(t[2]) - v(t[0])).length_squared() > 1e-12
    });
    mesh
}

pub fn cylinder(radius: f32, height: f32, segments: u32) -> Mesh {
    lathe(&[(radius, 0.0), (radius, height)], segments, true)
}

pub fn cone(radius: f32, height: f32, segments: u32) -> Mesh {
    lathe(&[(radius, 0.0), (0.0, height)], segments, true)
}

pub fn sphere(radius: f32, segments: u32) -> Mesh {
    let stacks = (segments / 2).max(2);
    let profile: Vec<(f32, f32)> = (0..=stacks)
        .map(|i| {
            let a = std::f32::consts::PI * i as f32 / stacks as f32;
            // Exactly on the axis at the poles (sin π is not 0 in f32).
            let r = if i == 0 || i == stacks { 0.0 } else { radius * a.sin() };
            (r, radius - radius * a.cos())
        })
        .collect();
    lathe(&profile, segments, true)
}

pub fn torus(major: f32, minor: f32, segments: u32, sides: u32) -> Mesh {
    let sides = sides.max(3);
    // The last ring repeats the first exactly (cos 2π is not 1 in f32), closing the tube.
    let mut profile: Vec<(f32, f32)> = (0..sides)
        .map(|i| {
            let a = TAU * i as f32 / sides as f32;
            (major - minor * a.cos(), minor - minor * a.sin())
        })
        .collect();
    if let Some(&first) = profile.first() {
        profile.push(first);
    }
    lathe(&profile, segments, false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_primitive_builds() {
        for (name, _) in ALL {
            let m = by_name(name);
            assert!(m.as_ref().is_some_and(|m| m.triangle_count() > 0), "{name}");
        }
    }

    #[test]
    fn every_primitive_is_closed() {
        // Each edge once each way: no holes, no seams.
        for (name, _) in ALL {
            let Some(m) = by_name(name) else { continue };
            let mut edges = std::collections::HashMap::new();
            for t in &m.triangles {
                for k in 0..3 {
                    *edges.entry((t[k], t[(k + 1) % 3])).or_insert(0) += 1;
                }
            }
            let open = edges.iter().filter(|((a, b), n)| **n != 1 || edges.get(&(*b, *a)) != Some(&1)).count();
            assert_eq!(open, 0, "{name}");
        }
    }

    #[test]
    fn closed_solids_have_the_right_volume() {
        let v = cylinder(10.0, 20.0, 256).volume();
        assert!((v - std::f32::consts::PI * 100.0 * 20.0).abs() / v < 0.01, "{v}");
        let v = sphere(10.0, 128).volume();
        assert!((v - 4.0 / 3.0 * std::f32::consts::PI * 1000.0).abs() / v < 0.01, "{v}");
    }
}
