//! STL, binary and ASCII.
//!
//! Binary: an 80-byte header, a little-endian `u32` triangle count, then 50 bytes per
//! triangle (normal, three vertices, attribute word). ASCII: `solid` … `facet` / `outer loop`
//! / three `vertex x y z` / `endloop` / `endfacet` … `endsolid`. Stored normals are ignored;
//! the winding defines the facet's side.

use crate::geom::{Mesh, Vec3};

use crate::{ImportError, MAX_TRIANGLES, finite};

const HEADER: usize = 80;
const RECORD: usize = 50;

pub fn read(bytes: &[u8]) -> Result<Mesh, ImportError> {
    if is_binary(bytes) { read_binary(bytes) } else { read_ascii(bytes) }
}

/// Binary when the size matches the triangle count exactly. Some exporters write binary
/// files whose header starts with "solid", so the size test decides, not the first word.
fn is_binary(bytes: &[u8]) -> bool {
    let exact_size = binary_count(bytes).is_some_and(|n| {
        (n as u64).checked_mul(RECORD as u64).and_then(|b| b.checked_add(HEADER as u64 + 4)) == Some(bytes.len() as u64)
    });
    exact_size || !looks_like_text(bytes)
}

fn binary_count(bytes: &[u8]) -> Option<u32> {
    let c = bytes.get(HEADER..HEADER + 4)?;
    Some(u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
}

fn looks_like_text(bytes: &[u8]) -> bool {
    let start = bytes.iter().position(|b| !b.is_ascii_whitespace()).unwrap_or(bytes.len());
    bytes.get(start..start + 5).is_some_and(|w| w.eq_ignore_ascii_case(b"solid"))
}

fn read_binary(bytes: &[u8]) -> Result<Mesh, ImportError> {
    let count = binary_count(bytes)
        .ok_or_else(|| ImportError::Malformed("binary STL is shorter than its 84-byte header".into()))?
        as usize;
    if count > MAX_TRIANGLES {
        return Err(ImportError::TooManyTriangles);
    }
    let body = bytes.get(HEADER + 4..).unwrap_or_default();
    let available = body.len() / RECORD;
    if available < count {
        return Err(ImportError::Malformed(format!(
            "binary STL declares {count} triangles but holds only {available}"
        )));
    }
    let f = |r: &[u8; RECORD], at: usize| -> f32 {
        r.get(at..at + 4).map_or(f32::NAN, |b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    };
    let soup = body.as_chunks::<RECORD>().0.iter().take(count).map(|r| {
        let v = |k: usize| Vec3::new(f(r, 12 + k * 12), f(r, 16 + k * 12), f(r, 20 + k * 12));
        [v(0), v(1), v(2)]
    });
    Ok(Mesh::from_soup(soup)?)
}

fn read_ascii(bytes: &[u8]) -> Result<Mesh, ImportError> {
    let text = std::str::from_utf8(bytes).map_err(|e| ImportError::Malformed(format!("ASCII STL is not text: {e}")))?;
    let syntax = |line: usize, message: String| ImportError::Syntax { format: "STL", line, message };
    let mut soup: Vec<[Vec3; 3]> = Vec::new();
    let mut loop_vertices: Option<Vec<Vec3>> = None;
    for (i, line) in text.lines().enumerate() {
        let line_no = i + 1;
        let mut words = line.split_whitespace();
        let Some(keyword) = words.next() else {
            continue;
        };
        match keyword.to_ascii_lowercase().as_str() {
            "outer" => loop_vertices = Some(Vec::with_capacity(3)),
            "vertex" => {
                let Some(vertices) = loop_vertices.as_mut() else {
                    return Err(syntax(line_no, "`vertex` outside `outer loop`".into()));
                };
                let mut c = [0.0f32; 3];
                for slot in &mut c {
                    *slot = words
                        .next()
                        .and_then(finite)
                        .ok_or_else(|| syntax(line_no, "`vertex` needs three finite numbers".into()))?;
                }
                if vertices.len() >= 3 {
                    return Err(syntax(line_no, "a facet has more than three vertices".into()));
                }
                vertices.push(Vec3::from(c));
            }
            "endloop" => match loop_vertices.take().as_deref() {
                Some(&[a, b, c]) => {
                    if soup.len() >= MAX_TRIANGLES {
                        return Err(ImportError::TooManyTriangles);
                    }
                    soup.push([a, b, c]);
                }
                Some(v) => return Err(syntax(line_no, format!("a facet has {} vertices, not 3", v.len()))),
                None => return Err(syntax(line_no, "`endloop` without `outer loop`".into())),
            },
            "solid" | "facet" | "endfacet" | "endsolid" => {}
            other => {
                let other: String = other.chars().take(32).collect();
                return Err(syntax(line_no, format!("unexpected `{other}`")));
            }
        }
    }
    Ok(Mesh::from_soup(soup)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ASCII: &str = "solid tri\n  facet normal 0 0 1\n    outer loop\n      vertex 0 0 0\n      vertex 10 0 0\n      vertex 0 10 0\n    endloop\n  endfacet\n  facet normal 0 0 1\n    outer loop\n      vertex 10 0 0\n      vertex 10 10 0\n      vertex 0 10 0\n    endloop\n  endfacet\nendsolid tri\n";

    fn binary(tris: &[[[f32; 3]; 3]], header: &[u8]) -> Vec<u8> {
        let mut b = vec![0u8; HEADER];
        b[..header.len()].copy_from_slice(header);
        b.extend_from_slice(&(tris.len() as u32).to_le_bytes());
        for t in tris {
            b.extend_from_slice(&[0u8; 12]);
            for v in t {
                for c in v {
                    b.extend_from_slice(&c.to_le_bytes());
                }
            }
            b.extend_from_slice(&[0u8; 2]);
        }
        b
    }

    #[test]
    fn reads_ascii() -> Result<(), ImportError> {
        let m = read(ASCII.as_bytes())?;
        assert_eq!(m.triangles().len(), 2);
        assert_eq!(m.positions().len(), 4);
        assert_eq!(m.bounds().max, Vec3::new(10.0, 10.0, 0.0));
        Ok(())
    }

    #[test]
    fn reads_binary_even_when_the_header_says_solid() -> Result<(), ImportError> {
        let tris = [[[0.0, 0.0, 0.0], [5.0, 0.0, 0.0], [0.0, 5.0, 2.0]]];
        let m = read(&binary(&tris, b"solid exported by a binary writer"))?;
        assert_eq!(m.triangles().len(), 1);
        assert_eq!(m.bounds().max, Vec3::new(5.0, 5.0, 2.0));
        Ok(())
    }

    #[test]
    fn truncated_binary_is_refused() {
        let mut b = binary(&[[[0.0; 3], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]]; 3], b"bin");
        b.truncate(b.len() - 60);
        assert!(matches!(read(&b), Err(ImportError::Malformed(m)) if m.contains("declares 3")));
    }

    #[test]
    fn huge_declared_count_is_refused_without_allocating() {
        let mut b = vec![0u8; HEADER];
        b.extend_from_slice(&u32::MAX.to_le_bytes());
        assert_eq!(read(&b), Err(ImportError::TooManyTriangles));
    }

    #[test]
    fn ascii_errors_name_the_line() {
        let bad = ASCII.replace("vertex 10 10 0", "vertex 10 nan 0");
        assert!(matches!(read(bad.as_bytes()), Err(ImportError::Syntax { line: 12, .. })));
        let four = ASCII.replacen("    endloop", "      vertex 1 1 1\n    endloop", 1);
        assert!(matches!(read(four.as_bytes()), Err(ImportError::Syntax { line: 7, .. })));
    }

    #[test]
    fn non_finite_binary_triangles_are_dropped() -> Result<(), ImportError> {
        let tris = [
            [[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
            [[f32::NAN, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
        ];
        assert_eq!(read(&binary(&tris, b"x"))?.triangles().len(), 1);
        Ok(())
    }
}
