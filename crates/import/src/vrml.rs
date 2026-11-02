//! VRML (`.wrl`), 2.0/97 and 1.0, and the scene walk X3D shares.
//!
//! The text is parsed into generic nodes (type, fields, child nodes) without a schema, with
//! `DEF`/`USE` resolved; `PROTO`s and `ROUTE`s are skipped. The walk applies `Transform`s
//! (and VRML 1.0's `Separator` state, `Translation`, `Rotation`, `Scale`, `MatrixTransform`)
//! and turns `IndexedFaceSet`s, X3D's triangle and quad sets and the `Box`, `Sphere`,
//! `Cylinder` and `Cone` primitives into triangles. `Inline` files are not followed.

use std::collections::HashMap;
use std::rc::Rc;

use crate::geom::{Mat4, Quat, Vec3};

use crate::{ImportError, Imported, Soup};

/// Deepest node nesting.
const MAX_DEPTH: usize = 256;
/// Most node instances visited by the walk.
const MAX_VISITS: usize = 2_000_000;
/// Facets around a primitive's axis.
const SEGMENTS: usize = 48;

fn bad(what: impl std::fmt::Display) -> ImportError {
    ImportError::Malformed(format!("VRML: {what}"))
}

/// A field value.
#[derive(Debug, Clone)]
pub enum Val {
    Nums(Vec<f64>),
    Word(String),
    Nodes(Vec<Rc<VNode>>),
}

/// A node: its type and fields. Child nodes written without a field name (VRML 1.0, X3D
/// elements) are under the field name `""` or their X3D `containerField`.
#[derive(Debug, Default)]
pub struct VNode {
    pub kind: String,
    pub fields: Vec<(String, Val)>,
}

impl VNode {
    fn field(&self, name: &str) -> Option<&Val> {
        self.fields.iter().find(|(n, _)| n == name).map(|(_, v)| v)
    }

    fn nums(&self, name: &str) -> Option<&[f64]> {
        match self.field(name) {
            Some(Val::Nums(n)) => Some(n),
            _ => None,
        }
    }

    fn vec3(&self, name: &str, default: Vec3) -> Vec3 {
        match self.nums(name) {
            Some([x, y, z, ..]) => Vec3::new(*x as f32, *y as f32, *z as f32),
            _ => default,
        }
    }

    fn num(&self, name: &str, default: f64) -> f64 {
        self.nums(name).and_then(|n| n.first().copied()).unwrap_or(default)
    }

    fn flag(&self, name: &str, default: bool) -> bool {
        match self.field(name) {
            Some(Val::Word(w)) => w.eq_ignore_ascii_case("true"),
            _ => default,
        }
    }

    fn rotation(&self, name: &str) -> Quat {
        match self.nums(name) {
            Some([x, y, z, a, ..]) => {
                let axis = Vec3::new(*x as f32, *y as f32, *z as f32);
                if axis.length() < 1.0e-9 { Quat::IDENTITY } else { Quat::from_axis_angle(axis.normalize(), *a as f32) }
            }
            _ => Quat::IDENTITY,
        }
    }

    /// Child nodes in all fields, or only in `names` when given.
    fn nodes<'a>(&'a self, names: &'a [&'a str]) -> impl Iterator<Item = &'a Rc<VNode>> + 'a {
        self.fields.iter().filter(move |(n, _)| names.is_empty() || names.contains(&n.as_str())).flat_map(|(_, v)| {
            match v {
                Val::Nodes(n) => n.as_slice(),
                _ => &[],
            }
        })
    }

    fn points(&self) -> Vec<Vec3> {
        let coord = self.nodes(&["coord", ""]).find(|n| n.kind == "Coordinate" || n.kind == "CoordinateDouble");
        coord
            .and_then(|c| c.nums("point"))
            .map(|p| p.as_chunks::<3>().0.iter().map(|c| Vec3::new(c[0] as f32, c[1] as f32, c[2] as f32)).collect())
            .unwrap_or_default()
    }
}

// --- the VRML text ---

#[derive(Debug, Clone, PartialEq)]
enum Tok<'a> {
    Open,
    Close,
    ListOpen,
    ListClose,
    Str(String),
    Word(&'a str),
}

fn tokens(text: &str) -> Vec<Tok<'_>> {
    let b = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0usize;
    while let Some(&c) = b.get(i) {
        match c {
            b'#' => {
                while b.get(i).is_some_and(|&c| c != b'\n' && c != b'\r') {
                    i += 1;
                }
            }
            b'{' | b'}' | b'[' | b']' => {
                out.push(match c {
                    b'{' => Tok::Open,
                    b'}' => Tok::Close,
                    b'[' => Tok::ListOpen,
                    _ => Tok::ListClose,
                });
                i += 1;
            }
            b'"' => {
                let mut s = Vec::new();
                i += 1;
                while let Some(&c) = b.get(i) {
                    i += 1;
                    match c {
                        b'"' => break,
                        b'\\' => {
                            if let Some(&n) = b.get(i) {
                                s.push(n);
                                i += 1;
                            }
                        }
                        _ => s.push(c),
                    }
                }
                out.push(Tok::Str(String::from_utf8_lossy(&s).into_owned()));
            }
            _ if c.is_ascii_whitespace() || c == b',' => i += 1,
            _ => {
                let start = i;
                while b.get(i).is_some_and(|&c| !c.is_ascii_whitespace() && !b"{}[]\"#,".contains(&c)) {
                    i += 1;
                }
                out.push(Tok::Word(text.get(start..i).unwrap_or_default()));
            }
        }
    }
    out
}

fn number(w: &str) -> Option<f64> {
    if let Some(h) = w.strip_prefix("0x").or_else(|| w.strip_prefix("0X")) {
        return i64::from_str_radix(h, 16).ok().map(|v| v as f64);
    }
    crate::finite64(w)
}

struct Parser<'a> {
    toks: Vec<Tok<'a>>,
    at: usize,
    defs: HashMap<String, Rc<VNode>>,
}

impl<'a> Parser<'a> {
    fn peek(&self, k: usize) -> Option<&Tok<'a>> {
        self.toks.get(self.at + k)
    }

    fn next(&mut self) -> Option<Tok<'a>> {
        let t = self.toks.get(self.at).cloned();
        self.at += 1;
        t
    }

    /// Skips a balanced `{…}` or `[…]` starting at the current token.
    fn skip_block(&mut self) {
        let mut depth = 0i64;
        while let Some(t) = self.next() {
            match t {
                Tok::Open | Tok::ListOpen => depth += 1,
                Tok::Close | Tok::ListClose => depth -= 1,
                _ => {}
            }
            if depth <= 0 {
                return;
            }
        }
    }

    /// A node at the current token: `Type {…}`, `DEF name Type {…}` or `USE name`.
    fn node(&mut self, depth: usize) -> Result<Option<Rc<VNode>>, ImportError> {
        if depth > MAX_DEPTH {
            return Err(bad("nodes nest too deeply"));
        }
        match self.next() {
            Some(Tok::Word("USE")) => match self.next() {
                Some(Tok::Word(name)) => Ok(self.defs.get(name).cloned()),
                _ => Err(bad("USE without a name")),
            },
            Some(Tok::Word("DEF")) => {
                let name = match self.next() {
                    Some(Tok::Word(n)) => n.to_owned(),
                    Some(Tok::Str(n)) => n,
                    _ => return Err(bad("DEF without a name")),
                };
                let node = self.node(depth)?;
                if let Some(n) = &node {
                    self.defs.insert(name, n.clone());
                }
                Ok(node)
            }
            Some(Tok::Word(kind)) if self.peek(0) == Some(&Tok::Open) => {
                self.at += 1;
                Ok(Some(Rc::new(self.body(kind.to_owned(), depth + 1)?)))
            }
            _ => Ok(None),
        }
    }

    fn starts_node(&self) -> bool {
        matches!(self.peek(0), Some(Tok::Word("DEF" | "USE")))
            || (matches!(self.peek(0), Some(Tok::Word(_))) && self.peek(1) == Some(&Tok::Open))
    }

    /// The fields of a node, after its `{`, through its `}`.
    fn body(&mut self, kind: String, depth: usize) -> Result<VNode, ImportError> {
        let mut node = VNode { kind, fields: Vec::new() };
        loop {
            match self.peek(0) {
                None => return Ok(node),
                Some(Tok::Close) => {
                    self.at += 1;
                    return Ok(node);
                }
                Some(Tok::Word("PROTO" | "EXTERNPROTO")) => {
                    // PROTO name [interface] {body}; EXTERNPROTO name [interface] "url"
                    self.at += 2;
                    if self.peek(0) == Some(&Tok::ListOpen) {
                        self.skip_block();
                    }
                    match self.peek(0) {
                        Some(Tok::Open | Tok::ListOpen) => self.skip_block(),
                        _ => self.at += 1,
                    }
                }
                Some(Tok::Word("ROUTE")) => self.at += 4,
                _ if self.starts_node() => {
                    if let Some(n) = self.node(depth)? {
                        node.fields.push((String::new(), Val::Nodes(vec![n])));
                    }
                }
                Some(Tok::Word(name)) => {
                    let name = (*name).to_owned();
                    self.at += 1;
                    let v = self.value(depth)?;
                    node.fields.push((name, v));
                }
                _ => self.at += 1,
            }
        }
    }

    /// A field value: a list, a node, a run of numbers or one word or string.
    fn value(&mut self, depth: usize) -> Result<Val, ImportError> {
        if self.starts_node() {
            return Ok(Val::Nodes(self.node(depth)?.into_iter().collect()));
        }
        match self.peek(0) {
            Some(Tok::ListOpen) => {
                self.at += 1;
                let mut nums = Vec::new();
                let mut nodes = Vec::new();
                let mut word = None;
                loop {
                    if self.starts_node() {
                        if let Some(n) = self.node(depth)? {
                            nodes.push(n);
                        }
                        continue;
                    }
                    match self.next() {
                        None | Some(Tok::ListClose) => break,
                        Some(Tok::Word(w)) => match number(w) {
                            Some(v) => nums.push(v),
                            None => word = Some(w.to_owned()),
                        },
                        Some(Tok::Str(s)) => word = Some(s),
                        Some(Tok::Open) => {
                            self.at -= 1;
                            self.skip_block();
                        }
                        _ => {}
                    }
                }
                Ok(if !nodes.is_empty() {
                    Val::Nodes(nodes)
                } else if nums.is_empty() && word.is_some() {
                    Val::Word(word.unwrap_or_default())
                } else {
                    Val::Nums(nums)
                })
            }
            Some(Tok::Word(w)) if number(w).is_some() => {
                let mut nums = Vec::new();
                while let Some(Tok::Word(w)) = self.peek(0) {
                    match number(w) {
                        Some(v) => nums.push(v),
                        None => break,
                    }
                    self.at += 1;
                }
                Ok(Val::Nums(nums))
            }
            Some(Tok::Word(_)) | Some(Tok::Str(_)) => match self.next() {
                Some(Tok::Word(w)) => Ok(Val::Word(w.to_owned())),
                Some(Tok::Str(s)) => Ok(Val::Word(s)),
                _ => Ok(Val::Word(String::new())),
            },
            Some(Tok::Open) => {
                self.skip_block();
                Ok(Val::Nums(Vec::new()))
            }
            _ => {
                self.at += 1;
                Ok(Val::Nums(Vec::new()))
            }
        }
    }
}

/// Parses VRML text into one root node holding the top-level nodes.
pub fn parse(text: &str) -> Result<VNode, ImportError> {
    let mut p = Parser { toks: tokens(text), at: 0, defs: HashMap::new() };
    p.body("Scene".into(), 0)
}

// --- the scene walk ---

#[derive(Clone)]
struct State {
    at: Mat4,
    /// VRML 1.0's current coordinates.
    coords: Rc<Vec<Vec3>>,
}

pub struct Walk {
    soup: Soup,
    visits: usize,
}

impl Walk {
    pub fn new() -> Self {
        Self { soup: Soup::new(), visits: 0 }
    }

    pub fn finish(self) -> Result<Imported, ImportError> {
        Ok(Imported::new(self.soup.into_mesh()?))
    }

    pub fn scene(&mut self, root: &VNode, at: Mat4) -> Result<(), ImportError> {
        let mut state = State { at, coords: Rc::new(Vec::new()) };
        self.children(root, &[], &mut state, 0)
    }

    fn children(&mut self, node: &VNode, names: &[&str], state: &mut State, depth: usize) -> Result<(), ImportError> {
        for child in node.nodes(names) {
            self.visit(child, state, depth + 1)?;
        }
        Ok(())
    }

    fn visit(&mut self, n: &VNode, state: &mut State, depth: usize) -> Result<(), ImportError> {
        self.visits += 1;
        if depth > MAX_DEPTH || self.visits > MAX_VISITS {
            return Err(bad("the scene nests too deeply"));
        }
        match n.kind.as_str() {
            "Transform" | "MatrixTransform" | "Translation" | "Rotation" | "Scale" => {
                let m = transform(n);
                let has_children = n.nodes(&["children", ""]).next().is_some();
                if n.kind == "Transform" && has_children {
                    let mut inner = State { at: state.at * m, coords: state.coords.clone() };
                    self.children(n, &["children", ""], &mut inner, depth)?;
                } else {
                    state.at *= m;
                }
            }
            "Separator" | "TransformSeparator" => {
                let mut inner = state.clone();
                self.children(n, &[], &mut inner, depth)?;
            }
            "Switch" => {
                let which = n.num("whichChoice", -1.0);
                if which >= 0.0
                    && let Some(child) = n.nodes(&["choice", "children", ""]).nth(which as usize)
                {
                    let mut inner = state.clone();
                    self.visit(child, &mut inner, depth + 1)?;
                }
            }
            "LOD" => {
                if let Some(child) = n.nodes(&["level", "children", ""]).next() {
                    let mut inner = state.clone();
                    self.visit(child, &mut inner, depth + 1)?;
                }
            }
            "Coordinate3" => {
                if let Some(p) = n.nums("point") {
                    state.coords = Rc::new(
                        p.as_chunks::<3>().0.iter().map(|c| Vec3::new(c[0] as f32, c[1] as f32, c[2] as f32)).collect(),
                    );
                }
            }
            "IndexedFaceSet" => {
                let mut points = n.points();
                if points.is_empty() && n.nodes(&["coord"]).next().is_none() {
                    points = state.coords.as_ref().clone();
                }
                let ccw = n.flag("ccw", true);
                let index = n.nums("coordIndex").unwrap_or_default();
                self.faces(&points, index, ccw, &state.at)?;
            }
            "IndexedTriangleSet" | "TriangleSet" | "IndexedQuadSet" | "QuadSet" => {
                let points = n.points();
                let ccw = n.flag("ccw", true);
                let k = if n.kind.contains("Quad") { 4 } else { 3 };
                let order: Vec<f64> = match n.nums("index") {
                    Some(i) => i.to_vec(),
                    None => (0..points.len()).map(|i| i as f64).collect(),
                };
                let mut with_ends = Vec::with_capacity(order.len() + order.len() / k);
                for c in order.chunks_exact(k) {
                    with_ends.extend_from_slice(c);
                    with_ends.push(-1.0);
                }
                self.faces(&points, &with_ends, ccw, &state.at)?;
            }
            "Box" | "Cube" => {
                let size = match n.nums("size") {
                    Some([x, y, z, ..]) => Vec3::new(*x as f32, *y as f32, *z as f32),
                    _ => Vec3::new(n.num("width", 2.0) as f32, n.num("height", 2.0) as f32, n.num("depth", 2.0) as f32),
                };
                self.cuboid(size / 2.0, &state.at)?;
            }
            "Sphere" => self.revolve(&sphere_profile(n.num("radius", 1.0) as f32), &state.at)?,
            "Cylinder" => {
                let (r, h) = (n.num("radius", 1.0) as f32, n.num("height", 2.0) as f32 / 2.0);
                self.revolve(&[(0.0, -h), (r, -h), (r, h), (0.0, h)], &state.at)?;
            }
            "Cone" => {
                let (r, h) = (n.num("bottomRadius", 1.0) as f32, n.num("height", 2.0) as f32 / 2.0);
                self.revolve(&[(0.0, -h), (r, -h), (0.0, h)], &state.at)?;
            }
            "Inline" | "Script" | "Appearance" | "Material" | "ImageTexture" => {}
            _ => {
                // Group, Anchor, Collision, Billboard, StaticGroup, Shape and anything else
                // that may hold nodes: visit them with the state VRML 1.0 would share.
                let mut inner = state.clone();
                self.children(n, &[], &mut inner, depth)?;
            }
        }
        Ok(())
    }

    /// Polygons from a coordinate index list with `-1` ending each face.
    fn faces(&mut self, points: &[Vec3], index: &[f64], ccw: bool, at: &Mat4) -> Result<(), ImportError> {
        let mut tris = Vec::new();
        let mut face: Vec<u32> = Vec::new();
        for &i in index.iter().chain([-1.0].iter()) {
            if i < 0.0 {
                if let Some((&first, rest)) = face.split_first() {
                    for w in rest.windows(2) {
                        tris.push(if ccw { [first, w[0], w[1]] } else { [first, w[1], w[0]] });
                    }
                }
                face.clear();
            } else if (i as usize) < points.len() && i.fract() == 0.0 {
                face.push(i as u32);
            } else {
                return Err(bad(format!("coordIndex {i} is not one of the {} points", points.len())));
            }
        }
        self.soup.add(points, &tris, at)
    }

    fn cuboid(&mut self, h: Vec3, at: &Mat4) -> Result<(), ImportError> {
        let p: Vec<Vec3> = (0..8)
            .map(|i| {
                Vec3::new(
                    if i & 1 == 0 { -h.x } else { h.x },
                    if i & 2 == 0 { -h.y } else { h.y },
                    if i & 4 == 0 { -h.z } else { h.z },
                )
            })
            .collect();
        let quads = [[0, 2, 3, 1], [4, 5, 7, 6], [0, 1, 5, 4], [2, 6, 7, 3], [0, 4, 6, 2], [1, 3, 7, 5]];
        let tris: Vec<[u32; 3]> = quads.iter().flat_map(|q| [[q[0], q[1], q[2]], [q[0], q[2], q[3]]]).collect();
        self.soup.add(&p, &tris, at)
    }

    /// A solid of revolution about Y from a profile of (radius, y) running bottom to top.
    fn revolve(&mut self, profile: &[(f32, f32)], at: &Mat4) -> Result<(), ImportError> {
        let mut p = Vec::new();
        for &(r, y) in profile {
            for k in 0..SEGMENTS {
                let a = std::f32::consts::TAU * k as f32 / SEGMENTS as f32;
                p.push(Vec3::new(r * a.sin(), y, r * a.cos()));
            }
        }
        let mut tris = Vec::new();
        for ring in 0..profile.len().saturating_sub(1) {
            for k in 0..SEGMENTS {
                let k1 = (k + 1) % SEGMENTS;
                let (a, b) = ((ring * SEGMENTS + k) as u32, (ring * SEGMENTS + k1) as u32);
                let (c, d) = (a + SEGMENTS as u32, b + SEGMENTS as u32);
                tris.push([a, b, d]);
                tris.push([a, d, c]);
            }
        }
        // Rings of radius 0 collapse; the mesh builder drops the degenerate triangles.
        let mut welded = Soup::new();
        welded.add(&p, &tris, at)?;
        let mesh = welded.into_welded_mesh()?;
        self.soup.add(mesh.positions(), mesh.triangles(), &Mat4::IDENTITY)
    }
}

impl Default for Walk {
    fn default() -> Self {
        Self::new()
    }
}

fn sphere_profile(r: f32) -> Vec<(f32, f32)> {
    let n = SEGMENTS / 2;
    (0..=n)
        .map(|i| {
            let a = std::f32::consts::PI * (i as f32 / n as f32) - std::f32::consts::FRAC_PI_2;
            // The poles exactly on the axis, so their rings weld to one point.
            if i == 0 || i == n { (0.0, r * a.sin().signum()) } else { (r * a.cos(), r * a.sin()) }
        })
        .collect()
}

/// A transform node's matrix (VRML 2.0 / X3D `Transform`, VRML 1.0's transform nodes).
fn transform(n: &VNode) -> Mat4 {
    match n.kind.as_str() {
        "MatrixTransform" => match n.nums("matrix") {
            Some(m) if m.len() >= 16 => {
                let a: Vec<f32> = m.iter().take(16).map(|v| *v as f32).collect();
                <[f32; 16]>::try_from(a).map(|a| Mat4::from_cols_array(&a)).unwrap_or(Mat4::IDENTITY)
            }
            _ => Mat4::IDENTITY,
        },
        "Translation" => Mat4::from_translation(n.vec3("translation", Vec3::ZERO)),
        "Rotation" => Mat4::from_quat(n.rotation("rotation")),
        "Scale" => Mat4::from_scale(n.vec3("scaleFactor", Vec3::ONE)),
        _ => {
            let t = n.vec3("translation", Vec3::ZERO);
            let c = n.vec3("center", Vec3::ZERO);
            let r = n.rotation("rotation");
            let s = n.vec3("scale", n.vec3("scaleFactor", Vec3::ONE));
            let so = n.rotation("scaleOrientation");
            Mat4::from_translation(t + c)
                * Mat4::from_quat(r)
                * Mat4::from_quat(so)
                * Mat4::from_scale(s)
                * Mat4::from_quat(so.inverse())
                * Mat4::from_translation(-c)
        }
    }
}

pub fn read(bytes: &[u8]) -> Result<Imported, ImportError> {
    let text = String::from_utf8_lossy(bytes);
    if !text.trim_start().starts_with("#VRML") {
        return Err(bad("the file does not start with #VRML"));
    }
    let root = parse(&text)?;
    let mut walk = Walk::new();
    walk.scene(&root, Mat4::IDENTITY)?;
    walk.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vrml2_transforms_def_use_and_face_sets() -> Result<(), ImportError> {
        let text = "#VRML V2.0 utf8\n# comment\nPROTO Foo [ field SFFloat a 1 ] { Group {} }\n\
            DEF T Transform { translation 0 0 5 children [\n\
              Shape { appearance Appearance { material Material { diffuseColor 1 0 0 } }\n\
                geometry DEF F IndexedFaceSet { solid FALSE coord Coordinate { point [ 0 0 0, 10 0 0, 10 10 0, 0 10 0 ] }\n\
                  coordIndex [ 0, 1, 2, 3, -1 ] } }\n\
              Transform { translation 20 0 0 children Shape { geometry USE F } }\n\
            ] }\nROUTE a.b TO c.d\n";
        let m = read(text.as_bytes())?;
        assert_eq!(m.mesh.triangles().len(), 4);
        assert_eq!(m.mesh.bounds().max, Vec3::new(30.0, 10.0, 5.0));
        Ok(())
    }

    #[test]
    fn vrml1_separators_keep_state() -> Result<(), ImportError> {
        let text = "#VRML V1.0 ascii\nSeparator { Translation { translation 0 0 3 }\n\
            Coordinate3 { point [ 0 0 0, 1 0 0, 0 1 0 ] }\nIndexedFaceSet { coordIndex [ 0, 1, 2, -1 ] }\n\
            Separator { Translation { translation 5 0 0 } IndexedFaceSet { coordIndex [ 0, 1, 2, -1 ] } }\n\
            Cube { width 2 height 2 depth 2 } }\n";
        let m = read(text.as_bytes())?;
        assert_eq!(m.mesh.triangles().len(), 2 + 12);
        assert_eq!(m.mesh.bounds().max, Vec3::new(6.0, 1.0, 4.0));
        Ok(())
    }

    #[test]
    fn round_primitives_are_closed() -> Result<(), ImportError> {
        for shape in ["Sphere { radius 3 }", "Cylinder { radius 1 height 5 }", "Cone { }"] {
            let text = format!("#VRML V2.0 utf8\nShape {{ geometry {shape} }}\n");
            let m = read(text.as_bytes())?;
            let mut edges = std::collections::HashMap::new();
            for t in m.mesh.triangles() {
                for k in 0..3 {
                    let (a, b) = (t[k], t[(k + 1) % 3]);
                    *edges.entry((a.min(b), a.max(b))).or_insert(0) += 1;
                }
            }
            assert!(!edges.is_empty() && edges.values().all(|&n| n == 2), "{shape} is not closed");
        }
        Ok(())
    }

    #[test]
    fn garbage_does_not_panic() {
        for t in [
            "#VRML V2.0 utf8\n{{{{]]]",
            "#VRML V2.0 utf8\nDEF",
            "#VRML V2.0 utf8\nUSE x",
            "#VRML V2.0 utf8\nIndexedFaceSet { coordIndex [ 5 -1 ] coord Coordinate { point [ 1 2 3 ] } }",
        ] {
            let _ = read(t.as_bytes());
        }
        assert!(read(b"not vrml").is_err());
    }
}
