//! STL reader, binary and ASCII. Vertices are welded so the mesh is indexed.

use std::collections::HashMap;

use glam::Vec3;

use crate::{Mesh, ModelError, ModelResult};

/// Most triangles read from one file.
const MAX_TRIANGLES: usize = 50_000_000;

pub fn parse(bytes: &[u8]) -> ModelResult<Mesh> {
    let mut mesh = if is_binary(bytes) { binary(bytes)? } else { ascii(bytes)? };
    mesh.sanitise();
    Ok(mesh)
}

/// Binary when the size matches the triangle count in the header; some binary files
/// start with "solid", so the text prefix alone does not decide.
fn is_binary(bytes: &[u8]) -> bool {
    let Some(count) = bytes.get(80..84).and_then(|b| <[u8; 4]>::try_from(b).ok()).map(u32::from_le_bytes) else {
        return false;
    };
    let expected = 84u64 + u64::from(count) * 50;
    if expected == bytes.len() as u64 {
        return true;
    }
    !bytes.get(..5).is_some_and(|b| b.eq_ignore_ascii_case(b"solid"))
}

#[derive(Default)]
struct Welder {
    mesh: Mesh,
    index: HashMap<[u32; 3], u32>,
}

impl Welder {
    fn vertex(&mut self, v: Vec3) -> u32 {
        let key = [v.x.to_bits(), v.y.to_bits(), v.z.to_bits()];
        if let Some(&i) = self.index.get(&key) {
            return i;
        }
        let i = self.mesh.vertices.len() as u32;
        self.mesh.vertices.push(v);
        self.index.insert(key, i);
        i
    }

    fn triangle(&mut self, [a, b, c]: [Vec3; 3]) {
        if !(a.is_finite() && b.is_finite() && c.is_finite()) {
            return;
        }
        let t = [self.vertex(a), self.vertex(b), self.vertex(c)];
        if t[0] != t[1] && t[1] != t[2] && t[0] != t[2] {
            self.mesh.triangles.push(t);
        }
    }
}

fn binary(bytes: &[u8]) -> ModelResult<Mesh> {
    let body = bytes.get(84..).ok_or_else(|| ModelError::Format("the STL file is truncated".into()))?;
    let count = (body.len() / 50).min(MAX_TRIANGLES);
    let mut w = Welder::default();
    w.mesh.triangles.reserve(count);
    for rec in body.as_chunks::<50>().0.iter().take(count) {
        let f = |i: usize| {
            rec.get(i..i + 4).and_then(|b| <[u8; 4]>::try_from(b).ok()).map(f32::from_le_bytes).unwrap_or(f32::NAN)
        };
        let v = |o: usize| Vec3::new(f(o), f(o + 4), f(o + 8));
        w.triangle([v(12), v(24), v(36)]);
    }
    Ok(w.mesh)
}

fn ascii(bytes: &[u8]) -> ModelResult<Mesh> {
    let text = String::from_utf8_lossy(bytes);
    let mut w = Welder::default();
    let mut corners: Vec<Vec3> = Vec::with_capacity(3);
    for line in text.lines() {
        let mut parts = line.split_whitespace();
        match parts.next() {
            Some(word) if word.eq_ignore_ascii_case("vertex") => {
                let mut xyz = parts.map(|p| p.parse::<f32>().unwrap_or(f32::NAN));
                let v = Vec3::new(
                    xyz.next().unwrap_or(f32::NAN),
                    xyz.next().unwrap_or(f32::NAN),
                    xyz.next().unwrap_or(f32::NAN),
                );
                corners.push(v);
            }
            Some(word) if word.eq_ignore_ascii_case("endloop") => {
                if let [a, b, c] = corners[..] {
                    w.triangle([a, b, c]);
                }
                corners.clear();
                if w.mesh.triangles.len() >= MAX_TRIANGLES {
                    break;
                }
            }
            _ => {}
        }
    }
    if w.mesh.triangles.is_empty() && !text.trim_start().to_ascii_lowercase().starts_with("solid") {
        return Err(ModelError::Format("not an STL file".into()));
    }
    Ok(w.mesh)
}

/// Writes `meshes`, each placed by its matrix, as one binary STL.
pub fn write_binary(path: &std::path::Path, meshes: &[(&Mesh, glam::Mat4)]) -> std::io::Result<()> {
    use std::io::Write;
    let count: usize = meshes.iter().map(|(m, _)| m.triangles.len()).sum();
    let count = u32::try_from(count).map_err(|_| std::io::Error::other("too many triangles for STL"))?;
    let mut out = std::io::BufWriter::new(std::fs::File::create(path)?);
    let mut header = [0u8; 80];
    let tag = b"CosmicSlicer STL export";
    header[..tag.len()].copy_from_slice(tag);
    out.write_all(&header)?;
    out.write_all(&count.to_le_bytes())?;
    for (mesh, m) in meshes {
        // Mirrored placements turn the triangles inside out; keep them facing outwards.
        let flip = m.determinant() < 0.0;
        for t in &mesh.triangles {
            let Some(corners) = mesh.corners(t) else { continue };
            let [a, b, c] = corners.map(|p| m.transform_point3(p));
            let [b, c] = if flip { [c, b] } else { [b, c] };
            let n = (b - a).cross(c - a).normalize_or_zero();
            for v in [n, a, b, c] {
                for x in v.to_array() {
                    out.write_all(&x.to_le_bytes())?;
                }
            }
            out.write_all(&[0, 0])?;
        }
    }
    out.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_ascii() {
        let text = "solid t\nfacet normal 0 0 1\nouter loop\nvertex 0 0 0\nvertex 1 0 0\nvertex 0 1 0\nendloop\nendfacet\nfacet normal 0 0 1\nouter loop\nvertex 1 0 0\nvertex 1 1 0\nvertex 0 1 0\nendloop\nendfacet\nendsolid t\n";
        let m = parse(text.as_bytes());
        assert!(m.as_ref().is_ok_and(|m| m.triangles.len() == 2 && m.vertices.len() == 4), "{m:?}");
    }

    #[test]
    fn reads_binary_that_starts_with_solid() {
        let mut b = b"solid but binary".to_vec();
        b.resize(80, 0);
        b.extend_from_slice(&1u32.to_le_bytes());
        for v in [0.0f32, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0] {
            b.extend_from_slice(&v.to_le_bytes());
        }
        b.extend_from_slice(&[0, 0]);
        assert!(parse(&b).is_ok_and(|m| m.triangles.len() == 1));
    }

    #[test]
    fn garbage_is_an_error_not_a_crash() {
        assert!(parse(b"\x00\x01garbage").is_err());
        assert!(parse(b"hello world").is_err());
        assert!(parse(&[]).is_err());
    }
}
