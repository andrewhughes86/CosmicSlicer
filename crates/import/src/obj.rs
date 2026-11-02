//! Wavefront OBJ geometry: `v` vertices and `f` faces.
//!
//! Face corners may be `v`, `v/vt`, `v//vn` or `v/vt/vn`; only the vertex index is used.
//! Indices are 1-based, negative ones count back from the latest vertex. Polygons are
//! fanned into triangles. Groups, materials, texture coordinates, normals, lines and
//! free-form surfaces are skipped.

use crate::geom::{Mesh, Vec3};

use crate::{ImportError, MAX_TRIANGLES, finite};

/// Most corners accepted on one face.
const MAX_FACE_CORNERS: usize = 4096;

pub fn read(bytes: &[u8]) -> Result<Mesh, ImportError> {
    // OBJ is ASCII in practice; tolerate stray non-UTF-8 bytes in comments and names.
    let text = String::from_utf8_lossy(bytes);
    let syntax = |line: usize, message: String| ImportError::Syntax { format: "OBJ", line, message };
    let mut positions: Vec<Vec3> = Vec::new();
    let mut triangles: Vec<[u32; 3]> = Vec::new();
    let mut corners: Vec<u32> = Vec::new();
    let mut lines = text.lines().enumerate();
    while let Some((i, raw)) = lines.next() {
        let line_no = i + 1;
        // A trailing backslash continues the statement on the next line.
        let mut statement = raw.split('#').next().unwrap_or_default().to_owned();
        while statement.trim_end().ends_with('\\') {
            let trimmed = statement.trim_end();
            statement.truncate(trimmed.len() - 1);
            match lines.next() {
                Some((_, more)) => {
                    statement.push(' ');
                    statement.push_str(more.split('#').next().unwrap_or_default());
                }
                None => break,
            }
        }
        let mut words = statement.split_whitespace();
        match words.next() {
            Some("v") => {
                let mut c = [0.0f32; 3];
                for slot in &mut c {
                    *slot = words
                        .next()
                        .and_then(finite)
                        .ok_or_else(|| syntax(line_no, "`v` needs three finite numbers".into()))?;
                }
                if positions.len() >= u32::MAX as usize {
                    return Err(ImportError::Malformed("too many vertices".into()));
                }
                positions.push(Vec3::from(c));
            }
            Some("f") => {
                corners.clear();
                for word in words {
                    if corners.len() >= MAX_FACE_CORNERS {
                        return Err(syntax(line_no, format!("a face has more than {MAX_FACE_CORNERS} corners")));
                    }
                    let index = word.split('/').next().unwrap_or_default();
                    corners.push(resolve(index, positions.len()).ok_or_else(|| {
                        let word: String = word.chars().take(32).collect();
                        syntax(line_no, format!("bad vertex reference `{word}`"))
                    })?);
                }
                let Some((&first, rest)) = corners.split_first() else {
                    return Err(syntax(line_no, "`f` has no corners".into()));
                };
                if rest.len() < 2 {
                    return Err(syntax(line_no, "a face needs at least three corners".into()));
                }
                for pair in rest.windows(2) {
                    if let [b, c] = *pair
                        && first != b
                        && b != c
                        && first != c
                    {
                        if triangles.len() >= MAX_TRIANGLES {
                            return Err(ImportError::TooManyTriangles);
                        }
                        triangles.push([first, b, c]);
                    }
                }
            }
            _ => {}
        }
    }
    Ok(Mesh::new(positions, triangles)?)
}

/// 1-based or negative OBJ index to a 0-based index into the vertices read so far.
fn resolve(token: &str, count: usize) -> Option<u32> {
    let i: i64 = token.parse().ok()?;
    let count = i64::try_from(count).ok()?;
    let zero_based = if (1..=count).contains(&i) {
        i - 1
    } else if (-count..=-1).contains(&i) {
        count + i
    } else {
        return None;
    };
    u32::try_from(zero_based).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_quads_and_slash_forms() -> Result<(), ImportError> {
        let text = "# square\no plate\nv 0 0 0\nv 10 0 0\nv 10 10 0\nv 0 10 0\nvt 0 0\nvn 0 0 1\nusemtl steel\nf 1/1/1 2/1/1 3//1 4\n";
        let m = read(text.as_bytes())?;
        assert_eq!(m.triangles(), &[[0, 1, 2], [0, 2, 3]]);
        assert_eq!(m.bounds().max, Vec3::new(10.0, 10.0, 0.0));
        Ok(())
    }

    #[test]
    fn negative_indices_count_back() -> Result<(), ImportError> {
        let m = read(b"v 0 0 0\nv 1 0 0\nv 0 1 0\nf -3 -2 -1\n")?;
        assert_eq!(m.triangles(), &[[0, 1, 2]]);
        Ok(())
    }

    #[test]
    fn continuation_lines_join() -> Result<(), ImportError> {
        let m = read(b"v 0 0 0\nv 1 0 0\nv 0 1 0\nf 1 2 \\\n 3\n")?;
        assert_eq!(m.triangles().len(), 1);
        Ok(())
    }

    #[test]
    fn bad_references_name_the_line() {
        for face in ["f 1 2 4", "f 0 1 2", "f -4 1 2", "f 1 2", "f a b c", "f 1 2 99999999999999999999"] {
            let text = format!("v 0 0 0\nv 1 0 0\nv 0 1 0\n{face}\n");
            assert!(matches!(read(text.as_bytes()), Err(ImportError::Syntax { line: 4, .. })), "{face}");
        }
    }

    #[test]
    fn faces_may_only_refer_to_earlier_vertices() {
        assert!(matches!(read(b"f 1 2 3\nv 0 0 0\nv 1 0 0\nv 0 1 0\n"), Err(ImportError::Syntax { line: 1, .. })));
    }

    #[test]
    fn non_finite_vertices_are_refused() {
        assert!(matches!(read(b"v inf 0 0\n"), Err(ImportError::Syntax { line: 1, .. })));
        assert!(matches!(read(b"v 1e39 0 0\n"), Err(ImportError::Syntax { line: 1, .. })));
    }
}
