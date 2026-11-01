//! Wavefront OBJ reader: vertices and faces (polygons are fanned into triangles).

use glam::Vec3;

use crate::{Mesh, ModelError, ModelResult};

const MAX_VERTICES: usize = 50_000_000;

pub fn parse(bytes: &[u8]) -> ModelResult<Mesh> {
    let text = String::from_utf8_lossy(bytes);
    let mut mesh = Mesh::default();
    for line in text.lines() {
        let mut parts = line.split_whitespace();
        match parts.next() {
            Some("v") if mesh.vertices.len() < MAX_VERTICES => {
                let mut xyz = parts.map(|p| p.parse::<f32>().unwrap_or(f32::NAN));
                mesh.vertices.push(Vec3::new(
                    xyz.next().unwrap_or(f32::NAN),
                    xyz.next().unwrap_or(f32::NAN),
                    xyz.next().unwrap_or(f32::NAN),
                ));
            }
            Some("f") => {
                let n = mesh.vertices.len() as i64;
                // "7", "7/1", "7//3", "-1": the first number, 1-based or relative.
                let idx: Vec<u32> = parts
                    .filter_map(|p| p.split('/').next()?.parse::<i64>().ok())
                    .filter_map(|i| {
                        let i = if i < 0 { n + i } else { i - 1 };
                        (0..n).contains(&i).then_some(i as u32)
                    })
                    .collect();
                if let Some((&first, rest)) = idx.split_first() {
                    for pair in rest.windows(2) {
                        if let [b, c] = pair {
                            mesh.triangles.push([first, *b, *c]);
                        }
                    }
                }
            }
            _ => {}
        }
    }
    if mesh.vertices.is_empty() {
        return Err(ModelError::Format("the OBJ file has no vertices".into()));
    }
    mesh.sanitise();
    Ok(mesh)
}

#[cfg(test)]
mod tests {
    #[test]
    fn fans_a_quad() {
        let m = super::parse(b"v 0 0 0\nv 1 0 0\nv 1 1 0\nv 0 1 0\nf 1 2 3 4\nf 1/1 2/2 9/9\n");
        assert!(m.is_ok_and(|m| m.triangles.len() == 2));
    }
}
