//! AMF (Additive Manufacturing File Format): XML, plain or in a zip archive.
//!
//! Each `<object>`'s `<mesh>` holds `<vertices>` (`<coordinates>` `x`, `y`, `z`) and
//! `<volume>`s of `<triangle>`s (`v1`, `v2`, `v3`). When `<constellation>`s place objects
//! (`deltax`…, `rx`… in degrees), the outermost ones are built; otherwise every object is.
//! The `unit` attribute is honoured. Materials, colours and curved triangle edges are ignored.

use std::collections::HashMap;

use crate::geom::{Mat4, Vec3};

use crate::xml::{Ev, stream};
use crate::zip::Zip;
use crate::{ImportError, Imported, Soup, Units};

/// Deepest nesting of constellations.
const MAX_DEPTH: usize = 32;
/// Most object instances placed.
const MAX_PLACED: usize = 1_000_000;

#[derive(Default)]
struct Object {
    positions: Vec<Vec3>,
    triangles: Vec<[u32; 3]>,
}

#[derive(Default)]
struct Instance {
    id: String,
    delta: [f32; 3],
    rotation: [f32; 3],
}

#[derive(Default)]
struct Doc {
    unit: Option<String>,
    objects: HashMap<String, Object>,
    constellations: HashMap<String, Vec<Instance>>,
}

fn attr<'a>(attrs: &'a [(String, String)], key: &str) -> Option<&'a str> {
    attrs.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
}

fn parse(bytes: &[u8]) -> Result<Doc, ImportError> {
    let mut doc = Doc::default();
    let mut path: Vec<String> = Vec::new();
    let mut text = String::new();
    let mut object: Option<(String, Object)> = None;
    let mut constellation: Option<(String, Vec<Instance>)> = None;
    let mut point = [0.0f32; 3];
    let mut corner = [0u32; 3];
    let number = |t: &str| {
        crate::finite(t.trim())
            .ok_or_else(|| ImportError::Malformed(format!("AMF: `{}` is not a number", crate::xml::short(t.trim()))))
    };
    stream(bytes, |ev| {
        match ev {
            Ev::Start(name, attrs) => {
                text.clear();
                match name {
                    "amf" => doc.unit = attr(attrs, "unit").map(str::to_owned),
                    "object" => object = Some((attr(attrs, "id").unwrap_or_default().to_owned(), Object::default())),
                    "constellation" => {
                        constellation = Some((attr(attrs, "id").unwrap_or_default().to_owned(), Vec::new()));
                    }
                    "instance" => {
                        if let Some((_, c)) = constellation.as_mut() {
                            c.push(Instance {
                                id: attr(attrs, "objectid").unwrap_or_default().to_owned(),
                                ..Instance::default()
                            });
                        }
                    }
                    _ => {}
                }
                path.push(name.to_owned());
            }
            Ev::Text(t) => {
                if text.len() < 256 {
                    text.push_str(t);
                }
            }
            Ev::End(name) => {
                path.pop();
                let parent = path.last().map(String::as_str).unwrap_or_default();
                match (parent, name) {
                    ("coordinates", "x") => point[0] = number(&text)?,
                    ("coordinates", "y") => point[1] = number(&text)?,
                    ("coordinates", "z") => point[2] = number(&text)?,
                    ("vertices", "vertex") => {
                        if let Some((_, o)) = object.as_mut() {
                            if o.positions.len() >= 3 * crate::MAX_TRIANGLES {
                                return Err(ImportError::TooManyTriangles);
                            }
                            o.positions.push(Vec3::from(point));
                        }
                        point = [0.0; 3];
                    }
                    ("triangle", v @ ("v1" | "v2" | "v3")) => {
                        let k = usize::from(v.as_bytes().get(1).copied().unwrap_or(b'1').saturating_sub(b'1'));
                        let i = text.trim().parse::<u32>().map_err(|_| {
                            ImportError::Malformed(format!("AMF: bad vertex index `{}`", crate::xml::short(&text)))
                        })?;
                        if let Some(slot) = corner.get_mut(k) {
                            *slot = i;
                        }
                    }
                    ("volume", "triangle") => {
                        if let Some((_, o)) = object.as_mut() {
                            if corner.iter().any(|&i| i as usize >= o.positions.len()) {
                                return Err(ImportError::Malformed("AMF triangle refers to a missing vertex".into()));
                            }
                            if o.triangles.len() >= crate::MAX_TRIANGLES {
                                return Err(ImportError::TooManyTriangles);
                            }
                            o.triangles.push(corner);
                        }
                    }
                    ("instance", d @ ("deltax" | "deltay" | "deltaz" | "rx" | "ry" | "rz")) => {
                        let v = number(&text)?;
                        if let Some(inst) = constellation.as_mut().and_then(|(_, c)| c.last_mut()) {
                            let slot = match d {
                                "deltax" => &mut inst.delta[0],
                                "deltay" => &mut inst.delta[1],
                                "deltaz" => &mut inst.delta[2],
                                "rx" => &mut inst.rotation[0],
                                "ry" => &mut inst.rotation[1],
                                _ => &mut inst.rotation[2],
                            };
                            *slot = v;
                        }
                    }
                    (_, "object") => {
                        if let Some((id, o)) = object.take() {
                            doc.objects.insert(id, o);
                        }
                    }
                    (_, "constellation") => {
                        if let Some((id, c)) = constellation.take() {
                            doc.constellations.insert(id, c);
                        }
                    }
                    _ => {}
                }
                text.clear();
            }
        }
        Ok(())
    })?;
    Ok(doc)
}

fn place(doc: &Doc, soup: &mut Soup, id: &str, at: Mat4, depth: usize, placed: &mut usize) -> Result<(), ImportError> {
    *placed += 1;
    if depth > MAX_DEPTH || *placed > MAX_PLACED {
        return Err(ImportError::Malformed("AMF constellations nest too deeply".into()));
    }
    if let Some(o) = doc.objects.get(id) {
        return soup.add(&o.positions, &o.triangles, &at);
    }
    let c = doc.constellations.get(id).ok_or_else(|| ImportError::Malformed(format!("AMF has no object {id}")))?;
    for inst in c {
        let [rx, ry, rz] = inst.rotation.map(f32::to_radians);
        let m = Mat4::from_translation(Vec3::from(inst.delta))
            * Mat4::from_rotation_z(rz)
            * Mat4::from_rotation_y(ry)
            * Mat4::from_rotation_x(rx);
        place(doc, soup, &inst.id, at * m, depth + 1, placed)?;
    }
    Ok(())
}

pub fn read(bytes: &[u8]) -> Result<Imported, ImportError> {
    let unzipped;
    let bytes = if Zip::is_zip(bytes) {
        let zip = Zip::open(bytes)?;
        let name = zip
            .names()
            .find(|n| n.to_ascii_lowercase().ends_with(".amf"))
            .or_else(|| zip.names().next())
            .map(str::to_owned)
            .ok_or_else(|| ImportError::Malformed("the AMF archive is empty".into()))?;
        unzipped = zip.read(&name)?;
        unzipped.as_slice()
    } else {
        bytes
    };
    let doc = parse(bytes)?;
    let mut soup = Soup::new();
    let mut placed = 0usize;
    if doc.constellations.is_empty() {
        for o in doc.objects.values() {
            soup.add(&o.positions, &o.triangles, &Mat4::IDENTITY)?;
        }
    } else {
        let used: Vec<&String> = doc.constellations.values().flatten().map(|i| &i.id).collect();
        let mut roots: Vec<&String> = doc.constellations.keys().filter(|k| !used.contains(k)).collect();
        roots.sort();
        for id in roots {
            place(&doc, &mut soup, id, Mat4::IDENTITY, 0, &mut placed)?;
        }
    }
    let (scale, units) = match doc.unit.as_deref().unwrap_or("millimeter") {
        "micron" => (0.001, Units::Millimetres),
        "millimeter" => (1.0, Units::Millimetres),
        "meter" => (1.0, Units::Metres),
        "inch" => (1.0, Units::Inches),
        "feet" | "foot" => (12.0, Units::Inches),
        other => return Err(ImportError::Malformed(format!("AMF unit `{}`", crate::xml::short(other)))),
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

    const DOC: &str = r#"<?xml version="1.0"?><amf unit="inch"><object id="0"><mesh><vertices>
<vertex><coordinates><x>0</x><y>0</y><z>0</z></coordinates></vertex>
<vertex><coordinates><x>1</x><y>0</y><z>0</z></coordinates></vertex>
<vertex><coordinates><x>0</x><y>1</y><z>0</z></coordinates></vertex>
</vertices><volume><triangle><v1>0</v1><v2>1</v2><v3>2</v3></triangle></volume></mesh></object>
<constellation id="5"><instance objectid="0"><deltax>10</deltax><rz>90</rz></instance><instance objectid="0"/></constellation>
</amf>"#;

    #[test]
    fn constellations_place_objects() -> Result<(), ImportError> {
        let m = read(DOC.as_bytes())?;
        assert_eq!(m.units, Some(Units::Inches));
        assert_eq!(m.mesh.triangles().len(), 2);
        let b = m.mesh.bounds();
        assert!((b.min.x - 0.0).abs() < 1e-5 && (b.max.x - 10.0).abs() < 1e-5, "{b:?}");
        let plain = DOC.split("<constellation").next().unwrap_or_default().to_owned() + "</amf>";
        assert_eq!(read(plain.as_bytes())?.mesh.triangles().len(), 1);
        Ok(())
    }

    #[test]
    fn bad_indices_are_errors() {
        assert!(read(DOC.replace("<v3>2</v3>", "<v3>7</v3>").as_bytes()).is_err());
        assert!(read(DOC.replace("<x>1</x>", "<x>one</x>").as_bytes()).is_err());
    }
}
