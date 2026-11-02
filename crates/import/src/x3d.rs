//! X3D, the XML encoding of VRML's successor (`.x3d`, gzipped `.x3dz`).
//!
//! Elements become the generic nodes VRML uses (attributes as fields, child elements under
//! their `containerField`), `DEF`/`USE` are resolved, and the `<Scene>` is walked by the
//! VRML walk.

use std::collections::HashMap;
use std::rc::Rc;

use crate::geom::Mat4;

use crate::vrml::{VNode, Val, Walk};
use crate::xml::{Node, tree};
use crate::{ImportError, Imported};

/// Deepest element nesting converted.
const MAX_DEPTH: usize = 256;

fn value(text: &str) -> Val {
    let words: Vec<&str> =
        text.split(|c: char| c.is_ascii_whitespace() || c == ',').filter(|w| !w.is_empty()).collect();
    let nums: Option<Vec<f64>> = words.iter().map(|w| crate::finite64(w)).collect();
    match nums {
        Some(n) if !n.is_empty() => Val::Nums(n),
        _ => Val::Word(text.trim().to_owned()),
    }
}

fn convert(e: &Node, defs: &mut HashMap<String, Rc<VNode>>, depth: usize) -> Result<Rc<VNode>, ImportError> {
    if depth > MAX_DEPTH {
        return Err(ImportError::Malformed("X3D nests too deeply".into()));
    }
    if let Some(name) = e.attr("USE") {
        return Ok(defs.get(name).cloned().unwrap_or_default());
    }
    let mut node = VNode { kind: e.name.clone(), fields: Vec::new() };
    for (k, v) in &e.attrs {
        if !matches!(k.as_str(), "DEF" | "USE" | "containerField") {
            node.fields.push((k.clone(), value(v)));
        }
    }
    for c in &e.children {
        let field = c.attr("containerField").unwrap_or_default().to_owned();
        let child = convert(c, defs, depth + 1)?;
        node.fields.push((field, Val::Nodes(vec![child])));
    }
    let node = Rc::new(node);
    if let Some(name) = e.attr("DEF") {
        defs.insert(name.to_owned(), node.clone());
    }
    Ok(node)
}

pub fn read(bytes: &[u8]) -> Result<Imported, ImportError> {
    let doc = tree(bytes)?;
    let scene = doc
        .child("X3D")
        .and_then(|x| x.child("Scene"))
        .ok_or_else(|| ImportError::Malformed("X3D has no <X3D><Scene>".into()))?;
    let mut defs = HashMap::new();
    let root = convert(scene, &mut defs, 0)?;
    let mut walk = Walk::new();
    walk.scene(&root, Mat4::IDENTITY)?;
    walk.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::Vec3;

    #[test]
    fn transforms_def_use_and_triangle_sets() -> Result<(), ImportError> {
        let doc = r#"<?xml version="1.0"?><X3D profile="Interchange" version="3.3"><Scene>
<Transform translation="0 0 5"><Shape><Appearance><Material/></Appearance>
 <IndexedFaceSet DEF="F" coordIndex="0 1 2 3 -1" solid="false"><Coordinate point="0 0 0, 10 0 0, 10 10 0, 0 10 0"/></IndexedFaceSet></Shape>
 <Transform translation="20 0 0"><Shape><IndexedFaceSet USE="F"/></Shape></Transform>
 <Shape><IndexedTriangleSet index="0 1 2"><Coordinate point="0 0 -5 1 0 -5 0 1 -5"/></IndexedTriangleSet></Shape>
</Transform></Scene></X3D>"#;
        let m = read(doc.as_bytes())?;
        assert_eq!(m.mesh.triangles().len(), 5);
        assert_eq!(m.mesh.bounds().max, Vec3::new(30.0, 10.0, 5.0));
        assert_eq!(m.mesh.bounds().min.z, 0.0);
        Ok(())
    }

    #[test]
    fn missing_scene_is_an_error() {
        assert!(read(b"<X3D/>").is_err());
        assert!(read(b"<X3D><Scene><IndexedFaceSet coordIndex='0 1 9'><Coordinate point='0 0 0 1 0 0 0 1 0'/></IndexedFaceSet></Scene></X3D>").is_err());
    }
}
