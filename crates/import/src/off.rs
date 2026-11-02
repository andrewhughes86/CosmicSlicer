//! OFF (Object File Format), ASCII: an `OFF` keyword (or a `COFF`/`NOFF`/`STOFF` variant),
//! the vertex, face and edge counts, the vertices, then each face as its corner count and
//! indices. Colours, normals and texture coordinates after the needed values are ignored.

use crate::geom::{Mesh, Vec3};

use crate::{ImportError, Soup, finite};

/// Most corners accepted on one face.
const MAX_FACE_CORNERS: usize = 4096;

pub fn read(bytes: &[u8]) -> Result<Mesh, ImportError> {
    let text = String::from_utf8_lossy(bytes);
    let syntax = |line: usize, message: String| ImportError::Syntax { format: "OFF", line, message };
    // Statements are lines without comments or blank lines.
    let mut lines = text
        .lines()
        .enumerate()
        .map(|(i, l)| (i + 1, l.split('#').next().unwrap_or_default().trim()))
        .filter(|(_, l)| !l.is_empty());
    let (line, first) = lines.next().ok_or_else(|| ImportError::Malformed("empty OFF file".into()))?;
    let mut words: Vec<&str> = first.split_whitespace().collect();
    let keyword = words.first().copied().unwrap_or_default();
    if !keyword.ends_with("OFF") {
        return Err(syntax(line, "the file does not start with OFF".into()));
    }
    if keyword.starts_with('4') || keyword.contains('n') {
        return Err(ImportError::Unsupported("OFF files of other dimensions than 3".into()));
    }
    if keyword.contains("OFF") && words.len() == 1 {
        let (line, counts) = lines.next().ok_or_else(|| syntax(line, "missing counts".into()))?;
        words = counts.split_whitespace().collect();
        words.insert(0, keyword);
        let _ = line;
    }
    let count = |k: usize| -> Result<usize, ImportError> {
        words.get(k).and_then(|w| w.parse().ok()).ok_or_else(|| syntax(line, "bad vertex or face count".into()))
    };
    let (nv, nf) = (count(1)?, count(2)?);
    if nv > bytes.len() || nf > bytes.len() {
        return Err(ImportError::Malformed(format!("OFF claims {nv} vertices and {nf} faces")));
    }
    let mut soup = Soup::new();
    for _ in 0..nv {
        let (line, l) = lines.next().ok_or_else(|| ImportError::Malformed("OFF ends before its vertices".into()))?;
        let mut w = l.split_whitespace();
        let mut p = [0.0f32; 3];
        for slot in &mut p {
            *slot = w.next().and_then(finite).ok_or_else(|| syntax(line, "a vertex needs three numbers".into()))?;
        }
        soup.vertex(Vec3::from(p))?;
    }
    let mut corners = Vec::new();
    for _ in 0..nf {
        let (line, l) = lines.next().ok_or_else(|| ImportError::Malformed("OFF ends before its faces".into()))?;
        let mut w = l.split_whitespace();
        let n: usize = w.next().and_then(|c| c.parse().ok()).ok_or_else(|| syntax(line, "bad corner count".into()))?;
        if n > MAX_FACE_CORNERS {
            return Err(syntax(line, format!("a face has more than {MAX_FACE_CORNERS} corners")));
        }
        corners.clear();
        for _ in 0..n {
            let i: u32 = w
                .next()
                .and_then(|c| c.parse().ok())
                .filter(|&i: &u32| (i as usize) < nv)
                .ok_or_else(|| syntax(line, "bad vertex index".into()))?;
            corners.push(i);
        }
        soup.polygon(&corners)?;
    }
    soup.into_mesh()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_counts_vertices_and_faces() -> Result<(), ImportError> {
        let m = read(b"OFF\n# a square\n4 1 0\n0 0 0\n10 0 0\n10 10 0\n0 10 0\n4 0 1 2 3 255 0 0\n")?;
        assert_eq!(m.triangles(), &[[0, 1, 2], [0, 2, 3]]);
        let m = read(b"COFF 3 1 0\n0 0 0 1 1 1 1\n1 0 0 1 1 1 1\n0 1 0 1 1 1 1\n3 0 1 2\n")?;
        assert_eq!(m.triangles().len(), 1);
        Ok(())
    }

    #[test]
    fn bad_files_are_errors() {
        for t in ["OFF\n3 1 0\n0 0 0\n1 0 0\n0 1 0\n3 0 1 3\n", "OFF\n3 1\n0 0 0\n", "PLY\n", "OFF\n99999999999 1 0\n"]
        {
            assert!(read(t.as_bytes()).is_err(), "{t}");
        }
    }
}
