//! COLLADA (`.dae`), the XML interchange format.
//!
//! The scene's visual scene is walked: each `<node>`'s `matrix`, `translate`, `rotate` and
//! `scale` place its `<instance_geometry>` and `<instance_node>`s. A geometry's `<mesh>`
//! gives `<triangles>`, `<polylist>`, `<polygons>`, `<trifans>` and `<tristrips>` whose
//! `VERTEX` input points at the `POSITION` source. `<asset>`'s `unit` and `up_axis` are
//! honoured (the model is turned so its up is Z). Without a scene, every geometry is read.

use std::collections::HashMap;

use crate::geom::{Mat4, Vec3};

use crate::xml::{Node, floats, ints, tree};
use crate::{ImportError, Imported, Soup, Units};

/// Deepest node nesting.
const MAX_DEPTH: usize = 64;
/// Most node instances visited.
const MAX_VISITS: usize = 1_000_000;
/// Most inputs on one primitive.
const MAX_INPUTS: i64 = 64;

fn bad(what: impl std::fmt::Display) -> ImportError {
    ImportError::Malformed(format!("COLLADA: {what}"))
}

struct Doc<'a> {
    by_id: HashMap<&'a str, &'a Node>,
}

impl<'a> Doc<'a> {
    fn get(&self, url: &str) -> Option<&'a Node> {
        self.by_id.get(url.trim_start_matches('#')).copied()
    }

    /// A `<source>`'s points (its float array read with the accessor's stride).
    fn points(&self, source: &Node) -> Result<Vec<Vec3>, ImportError> {
        let array = source.child("float_array").ok_or_else(|| bad("source without a float_array"))?;
        let values = floats(&array.text)?;
        let accessor = source.child("technique_common").and_then(|t| t.child("accessor"));
        let stride = accessor.and_then(|a| a.attr("stride")).and_then(|s| s.parse::<usize>().ok()).unwrap_or(3).max(3);
        let offset = accessor.and_then(|a| a.attr("offset")).and_then(|s| s.parse::<usize>().ok()).unwrap_or(0);
        Ok(values
            .get(offset.min(values.len())..)
            .unwrap_or_default()
            .chunks(stride)
            .filter(|c| c.len() >= 3)
            .map(|c| Vec3::new(c[0], c[1], c[2]))
            .collect())
    }

    /// The positions a primitive's `VERTEX` input stands for, and its offset and stride.
    fn primitive_positions(&self, mesh: &Node, prim: &Node) -> Result<(Vec<Vec3>, usize, usize), ImportError> {
        let mut stride = 1i64;
        let mut found = None;
        for input in prim.children_named("input") {
            let offset: i64 = input.attr("offset").and_then(|o| o.parse().ok()).unwrap_or(0);
            if !(0..MAX_INPUTS).contains(&offset) {
                return Err(bad("input offset out of range"));
            }
            stride = stride.max(offset + 1);
            if input.attr("semantic") == Some("VERTEX") {
                found = Some((input.attr("source").unwrap_or_default(), offset as usize));
            }
        }
        let (url, offset) = found.ok_or_else(|| bad("primitive without a VERTEX input"))?;
        let vertices = self.get(url).or_else(|| mesh.child("vertices")).ok_or_else(|| bad("missing <vertices>"))?;
        let position = vertices
            .children_named("input")
            .find(|i| i.attr("semantic") == Some("POSITION"))
            .and_then(|i| i.attr("source"))
            .and_then(|s| self.get(s))
            .ok_or_else(|| bad("<vertices> without a POSITION source"))?;
        Ok((self.points(position)?, offset, stride as usize))
    }

    fn add_geometry(&self, soup: &mut Soup, geometry: &Node, at: &Mat4) -> Result<(), ImportError> {
        let Some(mesh) = geometry.child("mesh") else { return Ok(()) };
        for prim in &mesh.children {
            let kind = prim.name.as_str();
            if !matches!(kind, "triangles" | "polylist" | "polygons" | "trifans" | "tristrips") {
                continue;
            }
            let (positions, offset, stride) = self.primitive_positions(mesh, prim)?;
            // Each <p> lists `stride` indices per corner; the VERTEX one is at `offset`.
            let corners = |p: &Node| -> Result<Vec<u32>, ImportError> {
                ints(&p.text)?
                    .chunks(stride)
                    .filter_map(|c| c.get(offset).copied())
                    .map(|i| {
                        u32::try_from(i)
                            .ok()
                            .filter(|&i| (i as usize) < positions.len())
                            .ok_or_else(|| bad("index past the positions"))
                    })
                    .collect()
            };
            let mut tris: Vec<[u32; 3]> = Vec::new();
            let fan = |tris: &mut Vec<[u32; 3]>, c: &[u32]| {
                if let Some((&first, rest)) = c.split_first() {
                    tris.extend(rest.windows(2).map(|w| [first, w[0], w[1]]));
                }
            };
            match kind {
                "triangles" => {
                    for p in prim.children_named("p") {
                        tris.extend(corners(p)?.as_chunks::<3>().0.iter().copied());
                    }
                }
                "polylist" => {
                    let counts = prim.child("vcount").map(|v| ints(&v.text)).transpose()?.unwrap_or_default();
                    let all = prim.child("p").map(corners).transpose()?.unwrap_or_default();
                    let mut at = 0usize;
                    for n in counts {
                        let n = usize::try_from(n).map_err(|_| bad("negative vcount"))?;
                        let face = all.get(at..at + n).ok_or_else(|| bad("vcount runs past <p>"))?;
                        fan(&mut tris, face);
                        at += n;
                    }
                }
                "tristrips" => {
                    for p in prim.children_named("p") {
                        for (k, w) in corners(p)?.windows(3).enumerate() {
                            tris.push(if k % 2 == 0 { [w[0], w[1], w[2]] } else { [w[1], w[0], w[2]] });
                        }
                    }
                }
                _ => {
                    // polygons (holes in <ph> are ignored) and trifans: one fan per <p>.
                    for p in prim.children_named("p") {
                        fan(&mut tris, &corners(p)?);
                    }
                }
            }
            soup.add(&positions, &tris, at)?;
        }
        Ok(())
    }

    fn node_transform(node: &Node) -> Result<Mat4, ImportError> {
        let mut m = Mat4::IDENTITY;
        for t in &node.children {
            let v = || floats(&t.text);
            m *= match t.name.as_str() {
                "matrix" => {
                    let a = v()?;
                    let a: [f32; 16] = a.try_into().map_err(|_| bad("matrix needs 16 numbers"))?;
                    // Row-major in the file.
                    Mat4::from_cols_array(&a).transpose()
                }
                "translate" => match v()?[..] {
                    [x, y, z] => Mat4::from_translation(Vec3::new(x, y, z)),
                    _ => return Err(bad("translate needs 3 numbers")),
                },
                "scale" => match v()?[..] {
                    [x, y, z] => Mat4::from_scale(Vec3::new(x, y, z)),
                    _ => return Err(bad("scale needs 3 numbers")),
                },
                "rotate" => match v()?[..] {
                    [x, y, z, deg] => {
                        let axis = Vec3::new(x, y, z);
                        if axis.length() < 1.0e-9 {
                            Mat4::IDENTITY
                        } else {
                            Mat4::from_axis_angle(axis.normalize(), deg.to_radians())
                        }
                    }
                    _ => return Err(bad("rotate needs 4 numbers")),
                },
                _ => Mat4::IDENTITY,
            };
        }
        Ok(m)
    }

    fn visit(
        &self,
        soup: &mut Soup,
        node: &Node,
        parent: Mat4,
        depth: usize,
        visits: &mut usize,
    ) -> Result<(), ImportError> {
        *visits += 1;
        if depth > MAX_DEPTH || *visits > MAX_VISITS {
            return Err(bad("the node tree nests too deeply"));
        }
        let at = parent * Self::node_transform(node)?;
        for c in &node.children {
            match c.name.as_str() {
                "instance_geometry" => {
                    let g =
                        c.attr("url").and_then(|u| self.get(u)).ok_or_else(|| bad("instance of a missing geometry"))?;
                    self.add_geometry(soup, g, &at)?;
                }
                "instance_node" => {
                    let n = c.attr("url").and_then(|u| self.get(u)).ok_or_else(|| bad("instance of a missing node"))?;
                    self.visit(soup, n, at, depth + 1, visits)?;
                }
                "node" => self.visit(soup, c, at, depth + 1, visits)?,
                _ => {}
            }
        }
        Ok(())
    }
}

pub fn read(bytes: &[u8]) -> Result<Imported, ImportError> {
    let doc = tree(bytes)?;
    let root = doc.child("COLLADA").ok_or_else(|| bad("no <COLLADA> root"))?;
    let mut by_id = HashMap::new();
    let mut stack = vec![root];
    while let Some(n) = stack.pop() {
        if let Some(id) = n.attr("id") {
            by_id.entry(id).or_insert(n);
        }
        stack.extend(n.children.iter());
    }
    let d = Doc { by_id };
    let asset = root.child("asset");
    let metres = asset
        .and_then(|a| a.child("unit"))
        .and_then(|u| u.attr("meter"))
        .and_then(crate::finite64)
        .filter(|m| *m > 0.0)
        .unwrap_or(1.0);
    let up = match asset.and_then(|a| a.child("up_axis")).map(|u| u.text.trim()) {
        Some("Z_UP") => Mat4::IDENTITY,
        Some("X_UP") => Mat4::from_rotation_y(-std::f32::consts::FRAC_PI_2),
        _ => Mat4::from_rotation_x(std::f32::consts::FRAC_PI_2),
    };
    let mut soup = Soup::new();
    let mut visits = 0usize;
    let scene = root
        .child("scene")
        .and_then(|s| s.child("instance_visual_scene"))
        .and_then(|i| i.attr("url"))
        .and_then(|u| d.get(u))
        .or_else(|| root.child("library_visual_scenes").and_then(|l| l.child("visual_scene")));
    match scene {
        Some(s) => {
            for n in s.children_named("node") {
                d.visit(&mut soup, n, up, 0, &mut visits)?;
            }
        }
        None => {
            for g in root.descendants("geometry") {
                d.add_geometry(&mut soup, g, &up)?;
            }
        }
    }
    let mm = metres * 1000.0;
    let (mesh, units) = match Units::from_mm(mm) {
        Some(u) => (soup.into_mesh()?, u),
        None => (soup.into_mesh()?.scaled(mm as f32)?, Units::Millimetres),
    };
    Ok(Imported { mesh, units: Some(units), warnings: Vec::new() })
}

#[cfg(test)]
mod tests {
    use super::*;

    const DOC: &str = r##"<?xml version="1.0"?>
<COLLADA xmlns="http://www.collada.org/2005/11/COLLADASchema" version="1.4.1">
 <asset><unit meter="0.001" name="millimeter"/><up_axis>Z_UP</up_axis></asset>
 <library_geometries><geometry id="g"><mesh>
  <source id="pos"><float_array id="pa" count="12">0 0 0 10 0 0 10 10 0 0 10 0</float_array>
   <technique_common><accessor source="#pa" count="4" stride="3"/></technique_common></source>
  <source id="nrm"><float_array id="na" count="3">0 0 1</float_array></source>
  <vertices id="v"><input semantic="POSITION" source="#pos"/></vertices>
  <polylist count="1"><input semantic="VERTEX" source="#v" offset="0"/><input semantic="NORMAL" source="#nrm" offset="1"/>
   <vcount>4</vcount><p>0 0 1 0 2 0 3 0</p></polylist>
 </mesh></geometry></library_geometries>
 <library_visual_scenes><visual_scene id="s">
  <node><translate>0 0 5</translate><instance_geometry url="#g"/>
   <node><matrix>1 0 0 20 0 1 0 0 0 0 1 0 0 0 0 1</matrix><instance_geometry url="#g"/></node></node>
 </visual_scene></library_visual_scenes>
 <scene><instance_visual_scene url="#s"/></scene>
</COLLADA>"##;

    #[test]
    fn nodes_place_polylists() -> Result<(), ImportError> {
        let m = read(DOC.as_bytes())?;
        assert_eq!(m.units, Some(Units::Millimetres));
        assert_eq!(m.mesh.triangles().len(), 4);
        assert_eq!(m.mesh.bounds().max, Vec3::new(30.0, 10.0, 5.0));
        Ok(())
    }

    #[test]
    fn y_up_turns_to_z_up() -> Result<(), ImportError> {
        let m = read(DOC.replace("Z_UP", "Y_UP").as_bytes())?;
        // The square in the XY plane stands up in XZ.
        assert!((m.mesh.bounds().max.z - 10.0).abs() < 1e-4, "{:?}", m.mesh.bounds());
        Ok(())
    }

    #[test]
    fn bad_indices_are_errors() {
        assert!(read(DOC.replace("3 0</p>", "9 0</p>").as_bytes()).is_err());
        assert!(read(DOC.replace("<vcount>4", "<vcount>9").as_bytes()).is_err());
        assert!(read(DOC.replace("url=\"#g\"", "url=\"#nothing\"").as_bytes()).is_err());
    }
}
