//! 3MF (3D Manufacturing Format): a zip archive whose model part is XML.
//!
//! The model part is found through `_rels/.rels`. Its `<object>` meshes and `<components>`,
//! placed by the `<build>` items' transforms, make the model; components may live in other
//! model parts of the archive (the production extension's `path`). The model's `unit`
//! attribute is honoured. Materials, colours, textures and beam lattices are ignored.

use std::collections::HashMap;

use crate::geom::{Mat4, Vec3, Vec4};

use crate::xml::{Ev, stream};
use crate::zip::Zip;
use crate::{ImportError, Imported, Soup, Units};

/// Deepest nesting of components.
const MAX_DEPTH: usize = 32;
/// Most model parts read from one archive.
const MAX_PARTS: usize = 256;
/// Most object instances placed.
const MAX_PLACED: usize = 1_000_000;

#[derive(Default)]
struct Object {
    positions: Vec<Vec3>,
    triangles: Vec<[u32; 3]>,
    components: Vec<(String, Option<String>, Mat4)>,
}

#[derive(Default)]
struct Part {
    unit: Option<String>,
    objects: HashMap<String, Object>,
    build: Vec<(String, Option<String>, Mat4)>,
}

fn attr<'a>(attrs: &'a [(String, String)], key: &str) -> Option<&'a str> {
    attrs.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
}

/// A 3MF transform: twelve numbers, the rows of a 4×3 matrix applied to row vectors.
fn transform(text: Option<&str>) -> Result<Mat4, ImportError> {
    let Some(text) = text else { return Ok(Mat4::IDENTITY) };
    let m = crate::xml::floats(text)?;
    let [a, b, c, d, e, f, g, h, i, j, k, l] = m[..] else {
        return Err(ImportError::Malformed(format!("3MF transform `{}` needs 12 numbers", crate::xml::short(text))));
    };
    Ok(Mat4::from_cols(
        Vec4::new(a, b, c, 0.0),
        Vec4::new(d, e, f, 0.0),
        Vec4::new(g, h, i, 0.0),
        Vec4::new(j, k, l, 1.0),
    ))
}

fn parse_part(bytes: &[u8]) -> Result<Part, ImportError> {
    let mut part = Part::default();
    let mut current: Option<(String, Object)> = None;
    let num = |attrs: &[(String, String)], key: &str| -> Result<f32, ImportError> {
        attr(attrs, key)
            .and_then(crate::finite)
            .ok_or_else(|| ImportError::Malformed(format!("3MF vertex or triangle without a valid `{key}`")))
    };
    let index = |attrs: &[(String, String)], key: &str, n: usize| -> Result<u32, ImportError> {
        attr(attrs, key)
            .and_then(|v| v.parse::<u32>().ok())
            .filter(|&i| (i as usize) < n)
            .ok_or_else(|| ImportError::Malformed(format!("3MF triangle `{key}` is not a vertex of its object")))
    };
    stream(bytes, |ev| {
        match ev {
            Ev::Start("model", attrs) => part.unit = attr(attrs, "unit").map(str::to_owned),
            Ev::Start("object", attrs) => {
                current = Some((attr(attrs, "id").unwrap_or_default().to_owned(), Object::default()));
            }
            Ev::Start("vertex", attrs) => {
                if let Some((_, o)) = current.as_mut() {
                    if o.positions.len() >= 3 * crate::MAX_TRIANGLES {
                        return Err(ImportError::TooManyTriangles);
                    }
                    o.positions.push(Vec3::new(num(attrs, "x")?, num(attrs, "y")?, num(attrs, "z")?));
                }
            }
            Ev::Start("triangle", attrs) => {
                if let Some((_, o)) = current.as_mut() {
                    let n = o.positions.len();
                    if o.triangles.len() >= crate::MAX_TRIANGLES {
                        return Err(ImportError::TooManyTriangles);
                    }
                    o.triangles.push([index(attrs, "v1", n)?, index(attrs, "v2", n)?, index(attrs, "v3", n)?]);
                }
            }
            Ev::Start("component", attrs) => {
                if let Some((_, o)) = current.as_mut() {
                    o.components.push((
                        attr(attrs, "objectid").unwrap_or_default().to_owned(),
                        attr(attrs, "path").map(str::to_owned),
                        transform(attr(attrs, "transform"))?,
                    ));
                }
            }
            Ev::Start("item", attrs) => part.build.push((
                attr(attrs, "objectid").unwrap_or_default().to_owned(),
                attr(attrs, "path").map(str::to_owned),
                transform(attr(attrs, "transform"))?,
            )),
            Ev::End("object") => {
                if let Some((id, o)) = current.take() {
                    part.objects.insert(id, o);
                }
            }
            _ => {}
        }
        Ok(())
    })?;
    Ok(part)
}

/// The archive's main model part, from the package relationships.
fn main_part(zip: &Zip<'_>) -> String {
    let mut target = None;
    if let Ok(rels) = zip.read("_rels/.rels") {
        let _ = stream(&rels, |ev| {
            if let Ev::Start("Relationship", attrs) = ev
                && attr(attrs, "Type").is_some_and(|t| t.ends_with("/3dmodel"))
                && target.is_none()
            {
                target = attr(attrs, "Target").map(str::to_owned);
            }
            Ok(())
        });
    }
    target
        .or_else(|| zip.names().find(|n| n.to_ascii_lowercase().ends_with(".model")).map(str::to_owned))
        .unwrap_or_else(|| "3D/3dmodel.model".into())
}

struct Reader<'a> {
    zip: Zip<'a>,
    parts: HashMap<String, Part>,
    placed: usize,
}

impl Reader<'_> {
    fn part(&mut self, name: &str) -> Result<&Part, ImportError> {
        let key = name.trim_start_matches('/').to_ascii_lowercase();
        if !self.parts.contains_key(&key) {
            if self.parts.len() >= MAX_PARTS {
                return Err(ImportError::Malformed("3MF has too many model parts".into()));
            }
            let part = parse_part(&self.zip.read(&key)?)?;
            self.parts.insert(key.clone(), part);
        }
        self.parts.get(&key).ok_or_else(|| ImportError::Malformed(format!("3MF part `{name}` is missing")))
    }

    fn place(&mut self, soup: &mut Soup, part: &str, id: &str, at: Mat4, depth: usize) -> Result<(), ImportError> {
        if depth > MAX_DEPTH {
            return Err(ImportError::Malformed("3MF components nest too deeply".into()));
        }
        self.placed += 1;
        if self.placed > MAX_PLACED {
            return Err(ImportError::Malformed("3MF places too many objects".into()));
        }
        let components = {
            let p = self.part(part)?;
            let o = p.objects.get(id).ok_or_else(|| ImportError::Malformed(format!("3MF has no object {id}")))?;
            soup.add(&o.positions, &o.triangles, &at)?;
            o.components.clone()
        };
        for (child, path, m) in components {
            let child_part = path.unwrap_or_else(|| part.to_owned());
            self.place(soup, &child_part, &child, at * m, depth + 1)?;
        }
        Ok(())
    }
}

pub fn read(bytes: &[u8]) -> Result<Imported, ImportError> {
    let zip = Zip::open(bytes)?;
    let main = main_part(&zip);
    let mut reader = Reader { zip, parts: HashMap::new(), placed: 0 };
    let (unit, build) = {
        let p = reader.part(&main)?;
        (p.unit.clone(), p.build.clone())
    };
    let mut soup = Soup::new();
    if build.is_empty() {
        // No build: every object that is not used as a component.
        let p = reader.part(&main)?;
        let used: Vec<&String> = p.objects.values().flat_map(|o| o.components.iter().map(|c| &c.0)).collect();
        let roots: Vec<String> = p.objects.keys().filter(|k| !used.contains(k)).cloned().collect();
        for id in roots {
            reader.place(&mut soup, &main, &id, Mat4::IDENTITY, 0)?;
        }
    }
    for (id, path, m) in build {
        let part = path.unwrap_or_else(|| main.clone());
        reader.place(&mut soup, &part, &id, m, 0)?;
    }
    let (scale, units) = match unit.as_deref().unwrap_or("millimeter") {
        "micron" => (0.001, Units::Millimetres),
        "millimeter" => (1.0, Units::Millimetres),
        "centimeter" => (1.0, Units::Centimetres),
        "inch" => (1.0, Units::Inches),
        "foot" => (12.0, Units::Inches),
        "meter" => (1.0, Units::Metres),
        other => return Err(ImportError::Malformed(format!("3MF unit `{}`", crate::xml::short(other)))),
    };
    let mut mesh = soup.into_mesh()?;
    if scale != 1.0 {
        mesh = mesh.scaled(scale)?;
    }
    Ok(Imported { mesh, units: Some(units), warnings: Vec::new() })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::zip::tests::stored_zip;

    const MODEL: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<model unit="inch" xmlns="http://schemas.microsoft.com/3dmanufacturing/core/2015/02">
 <resources>
  <object id="1" type="model"><mesh>
   <vertices><vertex x="0" y="0" z="0"/><vertex x="1" y="0" z="0"/><vertex x="0" y="1" z="0"/></vertices>
   <triangles><triangle v1="0" v2="1" v3="2"/></triangles>
  </mesh></object>
  <object id="2" type="model"><components>
   <component objectid="1"/><component objectid="1" transform="1 0 0 0 1 0 0 0 1 10 0 0"/>
  </components></object>
 </resources>
 <build><item objectid="2" transform="1 0 0 0 1 0 0 0 1 0 0 5"/></build>
</model>"#;

    const RELS: &str = r#"<Relationships><Relationship Target="/3D/3dmodel.model" Id="r0" Type="http://schemas.microsoft.com/3dmanufacturing/2013/01/3dmodel"/></Relationships>"#;

    #[test]
    fn components_and_build_transforms_place_meshes() -> Result<(), ImportError> {
        let z = stored_zip(&[("_rels/.rels", RELS.as_bytes()), ("3D/3dmodel.model", MODEL.as_bytes())]);
        let m = read(&z)?;
        assert_eq!(m.units, Some(Units::Inches));
        assert_eq!(m.mesh.triangles().len(), 2);
        assert_eq!(m.mesh.bounds().max, Vec3::new(11.0, 1.0, 5.0));
        Ok(())
    }

    #[test]
    fn bad_references_are_errors() {
        for (from, to) in
            [(r#"v3="2""#, r#"v3="3""#), (r#"objectid="2""#, r#"objectid="9""#), (r#"unit="inch""#, r#"unit="cubit""#)]
        {
            let model = MODEL.replace(from, to);
            let z = stored_zip(&[("3D/3dmodel.model", model.as_bytes())]);
            assert!(read(&z).is_err(), "{to}");
        }
        let looped = MODEL.replace(r#"<component objectid="1"/>"#, r#"<component objectid="2"/>"#);
        let z = stored_zip(&[("3D/3dmodel.model", looped.as_bytes())]);
        assert!(read(&z).is_err());
    }
}
