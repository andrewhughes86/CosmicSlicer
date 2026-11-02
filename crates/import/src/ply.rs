//! PLY (Stanford polygon format), ASCII and binary of either byte order.
//!
//! The header declares elements and their properties; the `vertex` element's `x`, `y`, `z`
//! and the `face` element's index list (`vertex_indices` or `vertex_index`) are used, other
//! elements and properties are read past. Polygons are fanned into triangles.

use crate::geom::{Mesh, Vec3};

use crate::{ImportError, Soup};

/// Most corners accepted on one face.
const MAX_FACE_CORNERS: usize = 4096;
/// Most elements or properties in a header.
const MAX_HEADER_ITEMS: usize = 1024;

#[derive(Clone, Copy, PartialEq)]
enum Ty {
    I8,
    U8,
    I16,
    U16,
    I32,
    U32,
    F32,
    F64,
}

impl Ty {
    fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "char" | "int8" => Self::I8,
            "uchar" | "uint8" => Self::U8,
            "short" | "int16" => Self::I16,
            "ushort" | "uint16" => Self::U16,
            "int" | "int32" => Self::I32,
            "uint" | "uint32" => Self::U32,
            "float" | "float32" => Self::F32,
            "double" | "float64" => Self::F64,
            _ => return None,
        })
    }

    fn size(self) -> usize {
        match self {
            Self::I8 | Self::U8 => 1,
            Self::I16 | Self::U16 => 2,
            Self::I32 | Self::U32 | Self::F32 => 4,
            Self::F64 => 8,
        }
    }
}

enum Prop {
    Scalar(Ty, String),
    List(Ty, Ty, String),
}

struct Element {
    name: String,
    count: usize,
    props: Vec<Prop>,
}

#[derive(Clone, Copy, PartialEq)]
enum Encoding {
    Ascii,
    Little,
    Big,
}

/// Values in the body, one at a time.
struct Body<'a> {
    bytes: &'a [u8],
    at: usize,
    encoding: Encoding,
}

impl Body<'_> {
    fn value(&mut self, ty: Ty) -> Result<f64, ImportError> {
        let short = || ImportError::Malformed("PLY data ends early".into());
        if self.encoding == Encoding::Ascii {
            let rest = self.bytes.get(self.at..).ok_or_else(short)?;
            let start = rest.iter().position(|b| !b.is_ascii_whitespace()).ok_or_else(short)?;
            let len = rest.get(start..).unwrap_or_default().iter().take_while(|b| !b.is_ascii_whitespace()).count();
            let word = std::str::from_utf8(rest.get(start..start + len).unwrap_or_default()).unwrap_or_default();
            self.at += start + len;
            return crate::finite64(word)
                .ok_or_else(|| ImportError::Malformed(format!("PLY: `{}` is not a number", crate::xml::short(word))));
        }
        let n = ty.size();
        let raw = self.bytes.get(self.at..self.at + n).ok_or_else(short)?;
        self.at += n;
        let mut b = [0u8; 8];
        b[..n].copy_from_slice(raw);
        if self.encoding == Encoding::Big {
            b[..n].reverse();
        }
        let v = match ty {
            Ty::I8 => f64::from(b[0] as i8),
            Ty::U8 => f64::from(b[0]),
            Ty::I16 => f64::from(i16::from_le_bytes([b[0], b[1]])),
            Ty::U16 => f64::from(u16::from_le_bytes([b[0], b[1]])),
            Ty::I32 => f64::from(i32::from_le_bytes([b[0], b[1], b[2], b[3]])),
            Ty::U32 => f64::from(u32::from_le_bytes([b[0], b[1], b[2], b[3]])),
            Ty::F32 => f64::from(f32::from_le_bytes([b[0], b[1], b[2], b[3]])),
            Ty::F64 => f64::from_le_bytes(b),
        };
        Ok(v)
    }
}

pub fn read(bytes: &[u8]) -> Result<Mesh, ImportError> {
    let syntax = |line: usize, message: String| ImportError::Syntax { format: "PLY", line, message };
    let end = bytes
        .windows(10)
        .position(|w| w == b"end_header")
        .ok_or_else(|| ImportError::Malformed("PLY has no end_header".into()))?;
    let header = String::from_utf8_lossy(bytes.get(..end).unwrap_or_default());
    // The body starts after the end_header line's line break.
    let mut body_at = end + 10;
    while let Some(&b) = bytes.get(body_at) {
        body_at += 1;
        if b == b'\n' {
            break;
        }
    }
    let mut encoding = None;
    let mut elements: Vec<Element> = Vec::new();
    for (i, line) in header.lines().enumerate() {
        let mut w = line.split_whitespace();
        match w.next() {
            Some("ply") | Some("comment") | Some("obj_info") | None => {}
            Some("format") => {
                encoding = Some(match w.next() {
                    Some("ascii") => Encoding::Ascii,
                    Some("binary_little_endian") => Encoding::Little,
                    Some("binary_big_endian") => Encoding::Big,
                    other => return Err(syntax(i + 1, format!("unknown format `{}`", other.unwrap_or_default()))),
                });
            }
            Some("element") => {
                let name = w.next().unwrap_or_default().to_owned();
                let count = w
                    .next()
                    .and_then(|c| c.parse::<usize>().ok())
                    .ok_or_else(|| syntax(i + 1, "bad element count".into()))?;
                if elements.len() >= MAX_HEADER_ITEMS {
                    return Err(syntax(i + 1, "too many elements".into()));
                }
                elements.push(Element { name, count, props: Vec::new() });
            }
            Some("property") => {
                let el = elements.last_mut().ok_or_else(|| syntax(i + 1, "property before any element".into()))?;
                if el.props.len() >= MAX_HEADER_ITEMS {
                    return Err(syntax(i + 1, "too many properties".into()));
                }
                let bad = || syntax(i + 1, format!("bad property `{line}`"));
                let first = w.next().ok_or_else(bad)?;
                if first == "list" {
                    let count = w.next().and_then(Ty::parse).ok_or_else(bad)?;
                    let item = w.next().and_then(Ty::parse).ok_or_else(bad)?;
                    el.props.push(Prop::List(count, item, w.next().unwrap_or_default().to_owned()));
                } else {
                    let ty = Ty::parse(first).ok_or_else(bad)?;
                    el.props.push(Prop::Scalar(ty, w.next().unwrap_or_default().to_owned()));
                }
            }
            Some(_) => {}
        }
    }
    let encoding = encoding.ok_or_else(|| ImportError::Malformed("PLY header has no format line".into()))?;
    let mut body = Body { bytes, at: body_at, encoding };
    let mut soup = Soup::new();
    let mut vertices = 0usize;
    let mut corners: Vec<u32> = Vec::new();
    for el in &elements {
        // Each value takes at least one byte, so a count beyond the bytes left is a lie.
        if el.count > bytes.len().saturating_sub(body.at) && !el.props.is_empty() {
            return Err(ImportError::Malformed(format!("PLY element `{}` claims {} items", el.name, el.count)));
        }
        let axis = |n: &str| el.props.iter().position(|p| matches!(p, Prop::Scalar(_, name) if name == n));
        let xyz = [axis("x"), axis("y"), axis("z")];
        let is_vertex = el.name == "vertex";
        let is_face = el.name == "face";
        if is_vertex && xyz.iter().any(Option::is_none) {
            return Err(ImportError::Malformed("PLY vertices have no x, y and z".into()));
        }
        for _ in 0..el.count {
            let mut p = [0.0f32; 3];
            for (k, prop) in el.props.iter().enumerate() {
                match prop {
                    Prop::Scalar(ty, _) => {
                        let v = body.value(*ty)?;
                        if is_vertex && let Some(slot) = xyz.iter().position(|a| *a == Some(k)) {
                            p[slot] = v as f32;
                        }
                    }
                    Prop::List(count_ty, item_ty, name) => {
                        let n = body.value(*count_ty)?;
                        if !(0.0..=MAX_FACE_CORNERS as f64).contains(&n) {
                            return Err(ImportError::Malformed(format!("PLY list of {n} items")));
                        }
                        let take = is_face && (name == "vertex_indices" || name == "vertex_index");
                        corners.clear();
                        for _ in 0..n as usize {
                            let v = body.value(*item_ty)?;
                            if take {
                                if !(0.0..vertices as f64).contains(&v) {
                                    return Err(ImportError::Malformed(format!("PLY face refers to vertex {v}")));
                                }
                                corners.push(v as u32);
                            }
                        }
                        if take {
                            soup.polygon(&corners)?;
                        }
                    }
                }
            }
            if is_vertex {
                soup.vertex(Vec3::from(p))?;
                vertices += 1;
            }
        }
    }
    soup.into_mesh()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SQUARE: &str = "ply\nformat ascii 1.0\ncomment a square\nelement vertex 4\nproperty float x\nproperty float y\nproperty float z\nproperty uchar red\nelement face 1\nproperty list uchar int vertex_indices\nend_header\n0 0 0 255\n10 0 0 0\n10 10 0 0\n0 10 0 0\n4 0 1 2 3\n";

    #[test]
    fn ascii_quads_become_triangles() -> Result<(), ImportError> {
        let m = read(SQUARE.as_bytes())?;
        assert_eq!(m.triangles(), &[[0, 1, 2], [0, 2, 3]]);
        assert_eq!(m.bounds().max, Vec3::new(10.0, 10.0, 0.0));
        Ok(())
    }

    #[test]
    fn binary_of_both_byte_orders() -> Result<(), ImportError> {
        for (name, big) in [("binary_little_endian", false), ("binary_big_endian", true)] {
            let mut b = format!("ply\nformat {name} 1.0\nelement vertex 3\nproperty float x\nproperty float y\nproperty double z\nelement face 1\nproperty list uchar uint vertex_index\nend_header\n").into_bytes();
            for p in [[0.0f32, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 5.0]] {
                for v in [p[0], p[1]] {
                    b.extend_from_slice(&if big { v.to_be_bytes() } else { v.to_le_bytes() });
                }
                let z = f64::from(p[2]);
                b.extend_from_slice(&if big { z.to_be_bytes() } else { z.to_le_bytes() });
            }
            b.push(3);
            for i in [0u32, 1, 2] {
                b.extend_from_slice(&if big { i.to_be_bytes() } else { i.to_le_bytes() });
            }
            let m = read(&b)?;
            assert_eq!(m.triangles().len(), 1, "{name}");
            assert_eq!(m.bounds().max.z, 5.0, "{name}");
        }
        Ok(())
    }

    #[test]
    fn lies_are_errors() {
        let bad = SQUARE.replace("4 0 1 2 3", "4 0 1 2 9");
        assert!(read(bad.as_bytes()).is_err());
        let bad = SQUARE.replace("element vertex 4", "element vertex 4000000000");
        assert!(read(bad.as_bytes()).is_err());
        assert!(read(b"ply\nformat ascii 1.0\nelement vertex 1\nproperty float x\nend_header\n1\n").is_err());
    }
}
