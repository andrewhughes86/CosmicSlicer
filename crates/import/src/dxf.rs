//! DXF (ASCII): the faceted 3D entities.
//!
//! `3DFACE`s, polyface and polygon-mesh `POLYLINE`s and `MESH`es in the entities section,
//! and in blocks placed by `INSERT` (position, scale, rotation about Z; nested). The header's
//! `$INSUNITS` gives the unit. ACIS solids (`3DSOLID`, `BODY`, `REGION`, `SURFACE`) are
//! encrypted geometry and not read; the import says so.

use std::collections::HashMap;

use crate::geom::{Mat4, Vec3};

use crate::{ImportError, Imported, Soup, Units};

/// Deepest block nesting.
const MAX_DEPTH: usize = 32;
/// Most block instances placed.
const MAX_PLACED: usize = 1_000_000;
/// Most vertices along a polygon mesh side.
const MAX_GRID: i64 = 32_767;

fn bad(what: impl std::fmt::Display) -> ImportError {
    ImportError::Malformed(format!("DXF: {what}"))
}

#[derive(Debug, Default)]
struct Entity {
    kind: String,
    codes: Vec<(i32, String)>,
}

impl Entity {
    fn get(&self, code: i32) -> Option<&str> {
        self.codes.iter().find(|(c, _)| *c == code).map(|(_, v)| v.as_str())
    }

    fn num(&self, code: i32, default: f64) -> f64 {
        self.get(code).and_then(|v| crate::finite64(v.trim())).unwrap_or(default)
    }

    fn int(&self, code: i32) -> i64 {
        self.get(code).and_then(|v| v.trim().parse().ok()).unwrap_or(0)
    }

    fn point(&self, base: i32) -> Vec3 {
        Vec3::new(self.num(base, 0.0) as f32, self.num(base + 10, 0.0) as f32, self.num(base + 20, 0.0) as f32)
    }
}

/// The group-code pairs, grouped into entities at each code 0.
fn entities(text: &str) -> Result<Vec<Entity>, ImportError> {
    let mut lines = text.lines();
    let mut out: Vec<Entity> = Vec::new();
    let mut line = 0usize;
    while let Some(code) = lines.next() {
        line += 2;
        let value = lines.next().unwrap_or_default().trim().to_owned();
        let code: i32 = code.trim().parse().map_err(|_| ImportError::Syntax {
            format: "DXF",
            line: line - 1,
            message: format!("`{}` is not a group code", crate::xml::short(code.trim())),
        })?;
        if code == 0 {
            out.push(Entity { kind: value, codes: Vec::new() });
        } else if let Some(e) = out.last_mut() {
            e.codes.push((code, value));
        }
    }
    Ok(out)
}

#[derive(Default)]
struct Block {
    base: Vec3,
    entities: Vec<Entity>,
}

struct Reader {
    blocks: HashMap<String, Block>,
    soup: Soup,
    placed: usize,
    solids: usize,
}

impl Reader {
    /// Faces of `list` (which may hold POLYLINE … VERTEX … SEQEND runs), placed by `at`.
    fn add(&mut self, list: &[Entity], at: Mat4, depth: usize) -> Result<(), ImportError> {
        if depth > MAX_DEPTH {
            return Err(bad("blocks nest too deeply"));
        }
        let mut i = 0usize;
        while let Some(e) = list.get(i) {
            i += 1;
            match e.kind.as_str() {
                "3DFACE" => {
                    let p = [e.point(10), e.point(11), e.point(12), e.point(13)];
                    let quad = (p[3] - p[2]).length() > 0.0;
                    let tris: &[[u32; 3]] = if quad { &[[0, 1, 2], [0, 2, 3]] } else { &[[0, 1, 2]] };
                    self.soup.add(&p, tris, &at)?;
                }
                "POLYLINE" => {
                    let start = i;
                    while list.get(i).is_some_and(|v| v.kind == "VERTEX") {
                        i += 1;
                    }
                    let vertices = list.get(start..i).unwrap_or_default();
                    self.polyline(e, vertices, &at)?;
                }
                "MESH" => self.mesh(e, &at)?,
                "INSERT" => {
                    let name = e.get(2).unwrap_or_default().to_owned();
                    self.placed += 1;
                    if self.placed > MAX_PLACED {
                        return Err(bad("too many block references"));
                    }
                    let Some(block) = self.blocks.remove(&name) else { continue };
                    let scale = Vec3::new(e.num(41, 1.0) as f32, e.num(42, 1.0) as f32, e.num(43, 1.0) as f32);
                    let m = at
                        * Mat4::from_translation(e.point(10))
                        * Mat4::from_rotation_z((e.num(50, 0.0) as f32).to_radians())
                        * Mat4::from_scale(scale)
                        * Mat4::from_translation(-block.base);
                    // Taken out while it is placed, so a block holding itself ends.
                    let result = self.add(&block.entities, m, depth + 1);
                    self.blocks.insert(name, block);
                    result?;
                }
                "3DSOLID" | "BODY" | "REGION" | "SURFACE" => self.solids += 1,
                _ => {}
            }
        }
        Ok(())
    }

    fn polyline(&mut self, e: &Entity, vertices: &[Entity], at: &Mat4) -> Result<(), ImportError> {
        let flags = e.int(70);
        if flags & 64 != 0 {
            // Polyface mesh: position vertices (flag 64|128), then face records (128 only).
            let positions: Vec<Vec3> = vertices.iter().filter(|v| v.int(70) & 64 != 0).map(|v| v.point(10)).collect();
            let mut tris = Vec::new();
            for f in vertices.iter().filter(|v| v.int(70) & 192 == 128) {
                let corners: Vec<u32> = [71, 72, 73, 74]
                    .iter()
                    .map(|&c| f.int(c).unsigned_abs())
                    .filter(|&k| k > 0)
                    .map(|k| u32::try_from(k - 1).map_err(|_| bad("face index out of range")))
                    .collect::<Result<_, _>>()?;
                if let Some((&first, rest)) = corners.split_first() {
                    tris.extend(rest.windows(2).map(|w| [first, w[0], w[1]]));
                }
            }
            self.soup.add(&positions, &tris, at)
        } else if flags & 16 != 0 {
            // Polygon mesh: an M × N grid, optionally closed in either direction.
            let (m, n) = (e.int(71), e.int(72));
            if !(2..=MAX_GRID).contains(&m) || !(2..=MAX_GRID).contains(&n) {
                return Err(bad(format!("polygon mesh of {m} × {n}")));
            }
            let (m, n) = (m as usize, n as usize);
            let positions: Vec<Vec3> = vertices.iter().map(|v| v.point(10)).collect();
            if positions.len() < m * n {
                return Err(bad("polygon mesh has too few vertices"));
            }
            let (m_closed, n_closed) = (flags & 1 != 0, flags & 32 != 0);
            let mut tris = Vec::new();
            let rows = if m_closed { m } else { m - 1 };
            let cols = if n_closed { n } else { n - 1 };
            for r in 0..rows {
                for c in 0..cols {
                    let at = |r: usize, c: usize| ((r % m) * n + (c % n)) as u32;
                    let (a, b, cc, d) = (at(r, c), at(r, c + 1), at(r + 1, c + 1), at(r + 1, c));
                    tris.push([a, b, cc]);
                    tris.push([a, cc, d]);
                }
            }
            self.soup.add(&positions, &tris, at)
        } else {
            Ok(())
        }
    }

    fn mesh(&mut self, e: &Entity, at: &Mat4) -> Result<(), ImportError> {
        // Vertex count (92), the vertices (10/20/30 each), then the face list (93): a corner
        // count followed by indices (90), repeated.
        let mut positions = Vec::new();
        let mut list: Vec<i64> = Vec::new();
        let mut in_faces = false;
        let mut pending = [0.0f32; 3];
        for (code, v) in &e.codes {
            match code {
                93 => in_faces = true,
                10 => pending[0] = crate::finite(v).unwrap_or(0.0),
                20 => pending[1] = crate::finite(v).unwrap_or(0.0),
                30 => {
                    pending[2] = crate::finite(v).unwrap_or(0.0);
                    positions.push(Vec3::from(pending));
                }
                90 if in_faces => list.push(v.parse().map_err(|_| bad("bad MESH face entry"))?),
                94 => in_faces = false,
                _ => {}
            }
        }
        let mut tris = Vec::new();
        let mut k = 0usize;
        while let Some(&n) = list.get(k) {
            let n = usize::try_from(n).map_err(|_| bad("bad MESH face size"))?;
            let face = list.get(k + 1..k + 1 + n).ok_or_else(|| bad("MESH face list is cut short"))?;
            let face: Vec<u32> =
                face.iter().map(|&i| u32::try_from(i).map_err(|_| bad("bad MESH index"))).collect::<Result<_, _>>()?;
            if let Some((&first, rest)) = face.split_first() {
                tris.extend(rest.windows(2).map(|w| [first, w[0], w[1]]));
            }
            k += 1 + n;
        }
        self.soup.add(&positions, &tris, at)
    }
}

pub fn read(bytes: &[u8]) -> Result<Imported, ImportError> {
    if bytes.starts_with(b"AutoCAD Binary DXF") {
        return Err(ImportError::Unsupported("binary DXF; save it as ASCII DXF".into()));
    }
    let text = String::from_utf8_lossy(bytes);
    let all = entities(&text)?;
    let mut section = String::new();
    let mut units = None;
    let mut blocks: HashMap<String, Block> = HashMap::new();
    let mut block: Option<(String, Block)> = None;
    let mut model = Vec::new();
    for e in all {
        match e.kind.as_str() {
            "SECTION" => {
                section = e.get(2).unwrap_or_default().to_owned();
                // The header is `9 $NAME` / value pairs inside the section record.
                let mut name = "";
                for (code, v) in &e.codes {
                    if *code == 9 {
                        name = v.as_str();
                    } else if name == "$INSUNITS" && *code == 70 {
                        units = v.trim().parse::<i64>().ok();
                    }
                }
            }
            "ENDSEC" => section.clear(),
            "BLOCK" => {
                block =
                    Some((e.get(2).unwrap_or_default().to_owned(), Block { base: e.point(10), entities: Vec::new() }))
            }
            "ENDBLK" => {
                if let Some((name, b)) = block.take() {
                    blocks.insert(name, b);
                }
            }
            _ if section == "HEADER" => {}
            _ => match block.as_mut() {
                Some((_, b)) => b.entities.push(e),
                None if section == "ENTITIES" => model.push(e),
                None => {}
            },
        }
    }
    let mut r = Reader { blocks, soup: Soup::new(), placed: 0, solids: 0 };
    r.add(&model, Mat4::IDENTITY, 0)?;
    let mut warnings = Vec::new();
    if r.solids > 0 {
        warnings.push(format!(
            "{} ACIS solid(s) skipped: DXF solids are encrypted geometry; export the model as STEP instead",
            r.solids
        ));
    }
    if r.soup.triangle_count() == 0 && r.solids > 0 {
        return Err(ImportError::Unsupported("DXF 3D solids (ACIS); export the model as STEP instead".into()));
    }
    let (scale, units) = match units {
        Some(1) => (1.0, Some(Units::Inches)),
        Some(2) => (12.0, Some(Units::Inches)),
        Some(4) => (1.0, Some(Units::Millimetres)),
        Some(5) => (1.0, Some(Units::Centimetres)),
        Some(6) => (1.0, Some(Units::Metres)),
        _ => (1.0, None),
    };
    let mut mesh = r.soup.into_mesh()?;
    if scale != 1.0 {
        mesh = mesh.scaled(scale)?;
    }
    Ok(Imported { mesh, units, warnings })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dxf(body: &str) -> String {
        format!("0\nSECTION\n2\nHEADER\n9\n$INSUNITS\n70\n1\n0\nENDSEC\n{body}0\nEOF\n")
    }

    const FACE: &str =
        "0\n3DFACE\n8\n0\n10\n0\n20\n0\n30\n0\n11\n1\n21\n0\n31\n0\n12\n1\n22\n1\n32\n0\n13\n0\n23\n1\n33\n0\n";

    #[test]
    fn faces_blocks_and_units() -> Result<(), ImportError> {
        let body = format!(
            "0\nSECTION\n2\nBLOCKS\n0\nBLOCK\n2\nSQ\n10\n0\n20\n0\n30\n0\n{FACE}0\nENDBLK\n0\nENDSEC\n\
             0\nSECTION\n2\nENTITIES\n{FACE}0\nINSERT\n2\nSQ\n10\n10\n20\n0\n30\n0\n41\n2\n42\n2\n43\n2\n0\n3DSOLID\n0\nENDSEC\n"
        );
        let m = read(dxf(&body).as_bytes())?;
        assert_eq!(m.units, Some(Units::Inches));
        assert_eq!(m.mesh.triangles().len(), 4);
        assert_eq!(m.mesh.bounds().max, Vec3::new(12.0, 2.0, 0.0));
        assert_eq!(m.warnings.len(), 1);
        Ok(())
    }

    #[test]
    fn polyface_meshes() -> Result<(), ImportError> {
        let v = |x: i32, y: i32| format!("0\nVERTEX\n10\n{x}\n20\n{y}\n30\n0\n70\n192\n");
        let body = format!(
            "0\nSECTION\n2\nENTITIES\n0\nPOLYLINE\n70\n64\n{}{}{}{}0\nVERTEX\n70\n128\n71\n1\n72\n2\n73\n-3\n74\n4\n0\nSEQEND\n0\nENDSEC\n",
            v(0, 0),
            v(5, 0),
            v(5, 5),
            v(0, 5)
        );
        let m = read(dxf(&body).as_bytes())?;
        assert_eq!(m.mesh.triangles().len(), 2);
        let broken = body.replace("74\n4\n", "74\n9\n");
        assert!(read(dxf(&broken).as_bytes()).is_err());
        Ok(())
    }

    #[test]
    fn self_inserting_blocks_end() {
        let body = "0\nSECTION\n2\nBLOCKS\n0\nBLOCK\n2\nA\n0\nINSERT\n2\nA\n0\nENDBLK\n0\nENDSEC\n0\nSECTION\n2\nENTITIES\n0\nINSERT\n2\nA\n0\nENDSEC\n";
        assert!(read(dxf(body).as_bytes()).map_or(true, |m| m.mesh.is_empty()));
        let _ = read(b"0\nSECTION\n2\nENTITIES\n0\nPOLYLINE\n70\n16\n71\n99999\n72\n2\n");
    }
}
