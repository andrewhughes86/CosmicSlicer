//! 3DS (3D Studio), the binary chunk format.
//!
//! Chunks are a little-endian `u16` id and `u32` length (header included). The editor
//! chunk's objects hold triangle meshes: a vertex list (`0x4110`) and a face list
//! (`0x4120`), whose vertices are already in world space. Materials, lights, cameras and the
//! keyframer are skipped.

use crate::geom::{Mat4, Mesh, Vec3};

use crate::{ImportError, Soup};

/// Deepest chunk nesting.
const MAX_DEPTH: usize = 32;

fn bad(what: &str) -> ImportError {
    ImportError::Malformed(format!("3DS: {what}"))
}

fn u16_at(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(at..at + 2)?.try_into().ok()?))
}

fn u32_at(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

/// The chunks in `data`: (id, body).
fn chunks(data: &[u8]) -> Result<Vec<(u16, &[u8])>, ImportError> {
    let mut out = Vec::new();
    let mut at = 0usize;
    while at + 6 <= data.len() {
        let id = u16_at(data, at).ok_or_else(|| bad("chunk header"))?;
        let len = u32_at(data, at + 2).ok_or_else(|| bad("chunk header"))? as usize;
        if len < 6 {
            return Err(bad("chunk shorter than its header"));
        }
        // A last chunk that runs past the end is cut to what is there.
        let end = at.saturating_add(len).min(data.len());
        out.push((id, data.get(at + 6..end).unwrap_or_default()));
        at = end;
    }
    Ok(out)
}

fn walk(data: &[u8], soup: &mut Soup, depth: usize) -> Result<(), ImportError> {
    if depth > MAX_DEPTH {
        return Err(bad("chunks nest too deeply"));
    }
    for (id, body) in chunks(data)? {
        match id {
            // Main, editor: containers.
            0x4D4D | 0x3D3D => walk(body, soup, depth + 1)?,
            // Object: a name, then sub-chunks.
            0x4000 => {
                let name_end = body.iter().position(|&b| b == 0).map_or(body.len(), |p| p + 1);
                walk(body.get(name_end..).unwrap_or_default(), soup, depth + 1)?;
            }
            0x4100 => mesh(body, soup)?,
            _ => {}
        }
    }
    Ok(())
}

fn mesh(data: &[u8], soup: &mut Soup) -> Result<(), ImportError> {
    let mut positions = Vec::new();
    let mut tris = Vec::new();
    for (id, body) in chunks(data)? {
        match id {
            0x4110 => {
                let n = usize::from(u16_at(body, 0).ok_or_else(|| bad("vertex list"))?);
                for i in 0..n {
                    let f = |k: usize| {
                        body.get(2 + i * 12 + k * 4..6 + i * 12 + k * 4)
                            .and_then(|b| b.try_into().ok())
                            .map(f32::from_le_bytes)
                            .ok_or_else(|| bad("vertex list is cut short"))
                    };
                    positions.push(Vec3::new(f(0)?, f(1)?, f(2)?));
                }
            }
            0x4120 => {
                let n = usize::from(u16_at(body, 0).ok_or_else(|| bad("face list"))?);
                for i in 0..n {
                    let v = |k: usize| {
                        u16_at(body, 2 + i * 8 + k * 2).map(u32::from).ok_or_else(|| bad("face list is cut short"))
                    };
                    tris.push([v(0)?, v(1)?, v(2)?]);
                }
            }
            _ => {}
        }
    }
    soup.add(&positions, &tris, &Mat4::IDENTITY)
}

pub fn read(bytes: &[u8]) -> Result<Mesh, ImportError> {
    if u16_at(bytes, 0) != Some(0x4D4D) {
        return Err(bad("the file does not start with a main chunk"));
    }
    let mut soup = Soup::new();
    walk(bytes, &mut soup, 0)?;
    soup.into_mesh()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chunk(id: u16, body: &[u8]) -> Vec<u8> {
        let mut c = id.to_le_bytes().to_vec();
        c.extend_from_slice(&(body.len() as u32 + 6).to_le_bytes());
        c.extend_from_slice(body);
        c
    }

    fn file(index: u16) -> Vec<u8> {
        let mut verts = 3u16.to_le_bytes().to_vec();
        for v in [0.0f32, 0.0, 0.0, 4.0, 0.0, 0.0, 0.0, 4.0, 1.0] {
            verts.extend_from_slice(&v.to_le_bytes());
        }
        let mut faces = 1u16.to_le_bytes().to_vec();
        for i in [0u16, 1, index, 0] {
            faces.extend_from_slice(&i.to_le_bytes());
        }
        let mesh = [chunk(0x4110, &verts), chunk(0x4120, &faces)].concat();
        let object = [b"part\0".to_vec(), chunk(0x4100, &mesh)].concat();
        chunk(0x4D4D, &chunk(0x3D3D, &chunk(0x4000, &object)))
    }

    #[test]
    fn reads_object_meshes() -> Result<(), ImportError> {
        let m = read(&file(2))?;
        assert_eq!(m.triangles().len(), 1);
        assert_eq!(m.bounds().max, Vec3::new(4.0, 4.0, 1.0));
        Ok(())
    }

    #[test]
    fn bad_files_are_errors() {
        assert!(read(&file(7)).is_err());
        let f = file(2);
        for cut in 0..f.len() {
            let _ = read(&f[..cut]);
        }
    }
}
