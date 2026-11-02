//! glTF 2.0, as `.gltf` (JSON with buffers in data URIs or files beside it) or `.glb`
//! (binary container with the JSON and one buffer).
//!
//! The default scene's node tree is walked with each node's matrix or translation, rotation
//! and scale; every triangle primitive's `POSITION` accessor (with its `indices`, if any)
//! becomes triangles. Strips and fans are unrolled; points and lines are skipped. glTF is in
//! metres with Y up; the model is turned so its up is CosmicSlicer's Z. Compressed meshes
//! (Draco, meshopt) are not supported yet.

use crate::geom::{Mat4, Quat, Vec3};
use serde_json::Value;

use crate::{ImportError, Imported, Sibling, Soup, Units};

/// Deepest node nesting.
const MAX_DEPTH: usize = 64;
/// Most node instances visited.
const MAX_VISITS: usize = 1_000_000;

fn bad(what: impl std::fmt::Display) -> ImportError {
    ImportError::Malformed(format!("glTF: {what}"))
}

/// Standard base64, ignoring whitespace and padding.
pub(crate) fn base64(text: &str) -> Result<Vec<u8>, ImportError> {
    let mut out = Vec::with_capacity(text.len() * 3 / 4);
    let (mut acc, mut bits) = (0u32, 0u32);
    for c in text.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            b'=' | b' ' | b'\n' | b'\r' | b'\t' => continue,
            _ => return Err(bad("bad base64 data")),
        };
        acc = (acc << 6) | u32::from(v);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Ok(out)
}

fn index(v: &Value, key: &str) -> Option<usize> {
    v.get(key).and_then(Value::as_u64).and_then(|i| usize::try_from(i).ok())
}

struct Gltf<'a> {
    json: Value,
    buffers: Vec<Vec<u8>>,
    sibling: Sibling<'a>,
}

impl Gltf<'_> {
    fn list(&self, key: &str) -> &[Value] {
        self.json.get(key).and_then(Value::as_array).map_or(&[], Vec::as_slice)
    }

    fn item(&self, key: &str, i: usize) -> Result<&Value, ImportError> {
        self.list(key).get(i).ok_or_else(|| bad(format!("{key}[{i}] does not exist")))
    }

    /// An accessor's values as f64, `width` per element.
    fn accessor(&self, i: usize) -> Result<(Vec<f64>, usize), ImportError> {
        let a = self.item("accessors", i)?;
        if a.get("sparse").is_some() {
            return Err(ImportError::Unsupported("glTF sparse accessors".into()));
        }
        let count = index(a, "count").ok_or_else(|| bad("accessor without a count"))?;
        let width = match a.get("type").and_then(Value::as_str) {
            Some("SCALAR") => 1,
            Some("VEC2") => 2,
            Some("VEC3") => 3,
            Some("VEC4") => 4,
            _ => return Err(bad("unknown accessor type")),
        };
        let (size, ty) = match a.get("componentType").and_then(Value::as_u64) {
            Some(5120) => (1, 0),
            Some(5121) => (1, 1),
            Some(5122) => (2, 2),
            Some(5123) => (2, 3),
            Some(5125) => (4, 4),
            Some(5126) => (4, 5),
            _ => return Err(bad("unknown accessor component type")),
        };
        let Some(view_index) = index(a, "bufferView") else {
            // No view: all zeros, by the specification.
            return Ok((vec![0.0; count.min(crate::MAX_TRIANGLES * 3) * width], width));
        };
        let view = self.item("bufferViews", view_index)?;
        let buffer = self
            .buffers
            .get(index(view, "buffer").unwrap_or(usize::MAX))
            .ok_or_else(|| bad("buffer view names no buffer"))?;
        let start = index(view, "byteOffset").unwrap_or(0).saturating_add(index(a, "byteOffset").unwrap_or(0));
        let stride = index(view, "byteStride").filter(|&s| s > 0).unwrap_or(size * width);
        let element = size * width;
        let needed = if count == 0 { 0 } else { (count - 1).saturating_mul(stride).saturating_add(element) };
        let data =
            buffer.get(start..start.saturating_add(needed)).ok_or_else(|| bad("accessor runs past its buffer"))?;
        let mut out = Vec::with_capacity(count * width);
        for e in 0..count {
            for c in 0..width {
                let at = e * stride + c * size;
                let b = data.get(at..at + size).ok_or_else(|| bad("accessor runs past its buffer"))?;
                out.push(match ty {
                    0 => f64::from(b[0] as i8),
                    1 => f64::from(b[0]),
                    2 => f64::from(i16::from_le_bytes([b[0], b[1]])),
                    3 => f64::from(u16::from_le_bytes([b[0], b[1]])),
                    4 => f64::from(u32::from_le_bytes([b[0], b[1], b[2], b[3]])),
                    _ => f64::from(f32::from_le_bytes([b[0], b[1], b[2], b[3]])),
                });
            }
        }
        Ok((out, width))
    }

    fn add_mesh(&self, soup: &mut Soup, mesh: usize, at: &Mat4) -> Result<(), ImportError> {
        for prim in
            self.item("meshes", mesh)?.get("primitives").and_then(Value::as_array).map_or(&[][..], Vec::as_slice)
        {
            if prim.get("extensions").is_some_and(|e| e.get("KHR_draco_mesh_compression").is_some()) {
                return Err(ImportError::Unsupported("Draco-compressed glTF meshes".into()));
            }
            let mode = prim.get("mode").and_then(Value::as_u64).unwrap_or(4);
            if !(4..=6).contains(&mode) {
                continue;
            }
            let Some(pos) = prim.get("attributes").and_then(|a| index(a, "POSITION")) else { continue };
            let (p, width) = self.accessor(pos)?;
            if width != 3 {
                return Err(bad("POSITION is not VEC3"));
            }
            let positions: Vec<Vec3> =
                p.as_chunks::<3>().0.iter().map(|c| Vec3::new(c[0] as f32, c[1] as f32, c[2] as f32)).collect();
            let order: Vec<u32> = match index(prim, "indices") {
                Some(i) => {
                    let (idx, _) = self.accessor(i)?;
                    idx.iter()
                        .map(|&v| {
                            if (0.0..positions.len() as f64).contains(&v) {
                                Ok(v as u32)
                            } else {
                                Err(bad("index past the vertices"))
                            }
                        })
                        .collect::<Result<_, _>>()?
                }
                None => (0..u32::try_from(positions.len()).map_err(|_| bad("too many vertices"))?).collect(),
            };
            let mut tris: Vec<[u32; 3]> = Vec::new();
            match mode {
                4 => tris.extend(order.as_chunks::<3>().0.iter().copied()),
                5 => {
                    for (k, w) in order.windows(3).enumerate() {
                        tris.push(if k % 2 == 0 { [w[0], w[1], w[2]] } else { [w[1], w[0], w[2]] });
                    }
                }
                _ => {
                    if let Some((&first, rest)) = order.split_first() {
                        tris.extend(rest.windows(2).map(|w| [first, w[0], w[1]]));
                    }
                }
            }
            soup.add(&positions, &tris, at)?;
        }
        Ok(())
    }

    fn node_matrix(node: &Value) -> Mat4 {
        let floats = |key: &str| -> Option<Vec<f32>> {
            node.get(key)?.as_array()?.iter().map(|v| v.as_f64().map(|f| f as f32).filter(|f| f.is_finite())).collect()
        };
        if let Some(m) = floats("matrix").and_then(|m| <[f32; 16]>::try_from(m).ok()) {
            return Mat4::from_cols_array(&m);
        }
        let t = floats("translation").and_then(|v| <[f32; 3]>::try_from(v).ok()).map_or(Vec3::ZERO, Vec3::from);
        let s = floats("scale").and_then(|v| <[f32; 3]>::try_from(v).ok()).map_or(Vec3::ONE, Vec3::from);
        let r = floats("rotation")
            .and_then(|v| <[f32; 4]>::try_from(v).ok())
            .map(Quat::from_array)
            .filter(|q| q.length() > 1.0e-6)
            .map_or(Quat::IDENTITY, Quat::normalize);
        Mat4::from_scale_rotation_translation(s, r, t)
    }

    fn visit(
        &self,
        soup: &mut Soup,
        node: usize,
        parent: Mat4,
        depth: usize,
        visits: &mut usize,
    ) -> Result<(), ImportError> {
        *visits += 1;
        if depth > MAX_DEPTH || *visits > MAX_VISITS {
            return Err(bad("the node tree nests too deeply"));
        }
        let n = self.item("nodes", node)?;
        let at = parent * Self::node_matrix(n);
        if let Some(mesh) = index(n, "mesh") {
            self.add_mesh(soup, mesh, &at)?;
        }
        for child in n.get("children").and_then(Value::as_array).map_or(&[][..], Vec::as_slice) {
            let child = child.as_u64().and_then(|c| usize::try_from(c).ok()).ok_or_else(|| bad("bad child index"))?;
            self.visit(soup, child, at, depth + 1, visits)?;
        }
        Ok(())
    }

    fn load_buffers(&mut self, bin: Option<&[u8]>) -> Result<(), ImportError> {
        let mut buffers = Vec::new();
        for (i, b) in self.list("buffers").iter().enumerate() {
            let data = match b.get("uri").and_then(Value::as_str) {
                Some(uri) if uri.starts_with("data:") => {
                    let payload =
                        uri.split_once(";base64,").map(|(_, d)| d).ok_or_else(|| bad("data URI is not base64"))?;
                    base64(payload)?
                }
                Some(uri) => {
                    let name = percent_decode(uri);
                    (self.sibling)(&name).ok_or_else(|| {
                        ImportError::Malformed(format!(
                            "glTF buffer `{}` was not found next to the file",
                            crate::xml::short(&name)
                        ))
                    })?
                }
                None if i == 0 => bin.ok_or_else(|| bad("buffer 0 has no data"))?.to_vec(),
                None => return Err(bad(format!("buffer {i} has no data"))),
            };
            buffers.push(data);
        }
        self.buffers = buffers;
        Ok(())
    }
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while let Some(&c) = b.get(i) {
        if c == b'%'
            && let Some(h) = b
                .get(i + 1..i + 3)
                .and_then(|h| std::str::from_utf8(h).ok())
                .and_then(|h| u8::from_str_radix(h, 16).ok())
        {
            out.push(h);
            i += 3;
        } else {
            out.push(c);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The JSON and binary chunks of a `.glb`.
fn split_glb(bytes: &[u8]) -> Result<(&[u8], Option<&[u8]>), ImportError> {
    let u32_at = |at: usize| -> Result<usize, ImportError> {
        bytes
            .get(at..at + 4)
            .and_then(|b| b.try_into().ok())
            .map(|b| u32::from_le_bytes(b) as usize)
            .ok_or_else(|| bad("binary container is cut short"))
    };
    if u32_at(4)? != 2 {
        return Err(ImportError::Unsupported("glTF versions other than 2".into()));
    }
    let (mut json, mut bin) = (None, None);
    let mut at = 12usize;
    while at + 8 <= bytes.len() {
        let (len, kind) = (u32_at(at)?, u32_at(at + 4)?);
        let chunk = bytes.get(at + 8..at + 8 + len).ok_or_else(|| bad("chunk runs past the end"))?;
        match kind {
            0x4E4F_534A if json.is_none() => json = Some(chunk),
            0x004E_4942 if bin.is_none() => bin = Some(chunk),
            _ => {}
        }
        at += 8 + len.div_ceil(4) * 4;
    }
    Ok((json.ok_or_else(|| bad("no JSON chunk"))?, bin))
}

pub fn read(bytes: &[u8], sibling: Sibling<'_>) -> Result<Imported, ImportError> {
    let (json, bin) = if bytes.starts_with(b"glTF") { split_glb(bytes)? } else { (bytes, None) };
    let json: Value = serde_json::from_slice(json).map_err(bad)?;
    if json.get("extensionsRequired").and_then(Value::as_array).is_some_and(|r| {
        r.iter().any(|e| matches!(e.as_str(), Some("KHR_draco_mesh_compression" | "EXT_meshopt_compression")))
    }) {
        return Err(ImportError::Unsupported("compressed glTF meshes (Draco, meshopt)".into()));
    }
    let mut g = Gltf { json, buffers: Vec::new(), sibling };
    g.load_buffers(bin)?;
    let mut soup = Soup::new();
    let mut visits = 0usize;
    let y_up = Mat4::from_rotation_x(std::f32::consts::FRAC_PI_2);
    let scene = index(&g.json, "scene").unwrap_or(0);
    match g.list("scenes").get(scene) {
        Some(s) => {
            for n in s.get("nodes").and_then(Value::as_array).map_or(&[][..], Vec::as_slice) {
                let n = n.as_u64().and_then(|n| usize::try_from(n).ok()).ok_or_else(|| bad("bad scene node"))?;
                g.visit(&mut soup, n, y_up, 0, &mut visits)?;
            }
        }
        None => {
            for m in 0..g.list("meshes").len() {
                g.add_mesh(&mut soup, m, &y_up)?;
            }
        }
    }
    Ok(Imported { mesh: soup.into_mesh()?, units: Some(Units::Metres), warnings: Vec::new() })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn triangle_gltf(uri: &str) -> String {
        format!(
            r#"{{"asset":{{"version":"2.0"}},"scene":0,"scenes":[{{"nodes":[0]}}],
            "nodes":[{{"children":[1],"translation":[0,0,2]}},{{"mesh":0,"scale":[2,2,2]}}],
            "meshes":[{{"primitives":[{{"attributes":{{"POSITION":0}},"indices":1}}]}}],
            "buffers":[{{"byteLength":42,"uri":"{uri}"}}],
            "bufferViews":[{{"buffer":0,"byteOffset":0,"byteLength":36}},{{"buffer":0,"byteOffset":36,"byteLength":6}}],
            "accessors":[{{"bufferView":0,"componentType":5126,"count":3,"type":"VEC3"}},
                         {{"bufferView":1,"componentType":5123,"count":3,"type":"SCALAR"}}]}}"#
        )
    }

    fn triangle_buffer() -> Vec<u8> {
        let mut b = Vec::new();
        for v in [0.0f32, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0] {
            b.extend_from_slice(&v.to_le_bytes());
        }
        for i in [0u16, 1, 2] {
            b.extend_from_slice(&i.to_le_bytes());
        }
        b
    }

    fn encode(data: &[u8]) -> String {
        const A: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut s = String::new();
        for c in data.chunks(3) {
            let n = (u32::from(c[0]) << 16)
                | (u32::from(*c.get(1).unwrap_or(&0)) << 8)
                | u32::from(*c.get(2).unwrap_or(&0));
            for k in 0..=c.len() {
                s.push(char::from(A[((n >> (18 - 6 * k)) & 63) as usize]));
            }
        }
        s
    }

    #[test]
    fn data_uris_files_and_glb_all_read() -> Result<(), ImportError> {
        let buf = triangle_buffer();
        let embedded = triangle_gltf(&format!("data:application/octet-stream;base64,{}", encode(&buf)));
        let m = read(embedded.as_bytes(), &|_| None)?;
        assert_eq!(m.units, Some(Units::Metres));
        assert_eq!(m.mesh.triangles().len(), 1);
        // Y up becomes Z up: glTF's Y is Z, its Z is -Y.
        let b = m.mesh.bounds();
        assert!(
            (b.max - Vec3::new(2.0, -2.0, 2.0)).length() < 1e-5 && (b.min - Vec3::new(0.0, -2.0, 0.0)).length() < 1e-5,
            "{b:?}"
        );

        let external = triangle_gltf("tri%20angle.bin");
        let found = read(external.as_bytes(), &|n| (n == "tri angle.bin").then(|| buf.clone()))?;
        assert_eq!(found.mesh.triangles().len(), 1);
        assert!(read(external.as_bytes(), &|_| None).is_err());

        let json = triangle_gltf("x").replace(r#","uri":"x""#, "");
        let mut json = json.into_bytes();
        while !json.len().is_multiple_of(4) {
            json.push(b' ');
        }
        let mut glb = b"glTF".to_vec();
        glb.extend_from_slice(&2u32.to_le_bytes());
        glb.extend_from_slice(&0u32.to_le_bytes());
        glb.extend_from_slice(&(json.len() as u32).to_le_bytes());
        glb.extend_from_slice(&0x4E4F_534Au32.to_le_bytes());
        glb.extend_from_slice(&json);
        glb.extend_from_slice(&(buf.len() as u32).to_le_bytes());
        glb.extend_from_slice(&0x004E_4942u32.to_le_bytes());
        glb.extend_from_slice(&buf);
        assert_eq!(read(&glb, &|_| None)?.mesh.triangles().len(), 1);
        Ok(())
    }

    #[test]
    fn bad_references_are_errors() {
        let buf = triangle_buffer();
        let good = triangle_gltf(&format!("data:application/octet-stream;base64,{}", encode(&buf)));
        for (from, to) in [
            (r#""count":3,"type":"VEC3""#, r#""count":30,"type":"VEC3""#),
            (r#""mesh":0"#, r#""mesh":4"#),
            (r#""children":[1]"#, r#""children":[0]"#),
        ] {
            assert!(read(good.replace(from, to).as_bytes(), &|_| None).is_err(), "{to}");
        }
    }
}
