//! Plate thumbnails in the G-code, for printer screens: the objects drawn by a small software
//! rasteriser, encoded as PNG or QOI and written as base64 comment blocks the way PrusaSlicer
//! and OrcaSlicer do.

use glam::{DMat3, DVec3, Mat4};

use CosmicSlicer_model::Mesh;

/// One object to draw: its mesh, placement and colour.
pub struct Item<'a> {
    pub mesh: &'a Mesh,
    pub matrix: Mat4,
    pub colour: [u8; 3],
}

/// A requested thumbnail.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Spec {
    pub width: u32,
    pub height: u32,
    pub format: Format,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Png,
    Qoi,
}

/// Reads OrcaSlicer's `thumbnails` value ("48x48/PNG,300x300/PNG", or "48x48,300x300" with the
/// format given separately). Formats not supported fall back to PNG, noted in the warnings.
pub fn parse(spec: &str, default_format: &str, warnings: &mut Vec<String>) -> Vec<Spec> {
    let mut out = Vec::new();
    for item in spec.split([',', ';']).map(str::trim).filter(|s| !s.is_empty()) {
        let (size, fmt) = item.split_once('/').unwrap_or((item, default_format));
        let Some((w, h)) = size.split_once('x') else { continue };
        let (Ok(w), Ok(h)) = (w.trim().parse::<u32>(), h.trim().parse::<u32>()) else { continue };
        if w == 0 || h == 0 || w > 1024 || h > 1024 {
            continue;
        }
        let format = match fmt.trim().to_ascii_uppercase().as_str() {
            "QOI" => Format::Qoi,
            "PNG" | "" => Format::Png,
            other => {
                let note = format!("{other} thumbnails are written as PNG for now.");
                if !warnings.contains(&note) {
                    warnings.push(note);
                }
                Format::Png
            }
        };
        out.push(Spec { width: w, height: h, format });
    }
    out
}

/// Draws `items` into a `w` × `h` RGBA image (transparent background), from the front, a
/// little from the left and above, as OrcaSlicer's plate thumbnails are.
pub fn render(items: &[Item<'_>], w: u32, h: u32) -> Vec<u8> {
    let (w, h) = (w.clamp(1, 1024) as usize, h.clamp(1, 1024) as usize);
    let mut rgba = vec![0u8; w * h * 4];
    let mut depth = vec![f64::INFINITY; w * h];
    // View: turn 30° about Z, then tilt 35° down.
    let yaw = DMat3::from_rotation_z((-30f64).to_radians());
    let tilt = DMat3::from_rotation_x((-55f64).to_radians());
    let view = tilt * yaw;
    let mut tris: Vec<([DVec3; 3], [u8; 3])> = Vec::new();
    let (mut lo, mut hi) = (DVec3::splat(f64::INFINITY), DVec3::splat(f64::NEG_INFINITY));
    for item in items {
        let m = item.matrix.as_dmat4();
        for t in &item.mesh.triangles {
            let Some(c) = item.mesh.corners(t) else { continue };
            let p = c.map(|v| view * m.transform_point3(v.as_dvec3()));
            for q in &p {
                lo = lo.min(*q);
                hi = hi.max(*q);
            }
            tris.push((p, item.colour));
        }
    }
    if tris.is_empty() || !lo.is_finite() {
        return rgba;
    }
    let size = (hi - lo).truncate().max_element().max(1e-6);
    let scale = (w.min(h) as f64 * 0.9) / size;
    let centre = (lo + hi) / 2.0;
    let to_px = |p: DVec3| {
        DVec3::new((p.x - centre.x) * scale + w as f64 / 2.0, h as f64 / 2.0 - (p.y - centre.y) * scale, -p.z)
    };
    let light = DVec3::new(-0.3, 0.5, 0.8).normalize();
    for (p, colour) in tris {
        let n = (p[1] - p[0]).cross(p[2] - p[0]).normalize_or_zero();
        let shade = (0.35 + 0.65 * n.dot(light).abs()).clamp(0.0, 1.0);
        let px = p.map(to_px);
        let (minx, maxx) = (
            px.iter().map(|q| q.x).fold(f64::INFINITY, f64::min),
            px.iter().map(|q| q.x).fold(f64::NEG_INFINITY, f64::max),
        );
        let (miny, maxy) = (
            px.iter().map(|q| q.y).fold(f64::INFINITY, f64::min),
            px.iter().map(|q| q.y).fold(f64::NEG_INFINITY, f64::max),
        );
        let x0 = minx.floor().max(0.0) as usize;
        let x1 = (maxx.ceil() as i64).clamp(0, w as i64 - 1) as usize;
        let y0 = miny.floor().max(0.0) as usize;
        let y1 = (maxy.ceil() as i64).clamp(0, h as i64 - 1) as usize;
        let area = (px[1].x - px[0].x) * (px[2].y - px[0].y) - (px[2].x - px[0].x) * (px[1].y - px[0].y);
        if area.abs() < 1e-12 || x0 > x1 || y0 > y1 {
            continue;
        }
        for y in y0..=y1 {
            for x in x0..=x1 {
                let (fx, fy) = (x as f64 + 0.5, y as f64 + 0.5);
                let w0 = ((px[1].x - fx) * (px[2].y - fy) - (px[2].x - fx) * (px[1].y - fy)) / area;
                let w1 = ((px[2].x - fx) * (px[0].y - fy) - (px[0].x - fx) * (px[2].y - fy)) / area;
                let w2 = 1.0 - w0 - w1;
                if w0 < 0.0 || w1 < 0.0 || w2 < 0.0 {
                    continue;
                }
                let z = w0 * px[0].z + w1 * px[1].z + w2 * px[2].z;
                let i = y * w + x;
                if depth.get(i).is_some_and(|d| z < *d) {
                    if let Some(d) = depth.get_mut(i) {
                        *d = z;
                    }
                    if let Some(px) = rgba.get_mut(i * 4..i * 4 + 4) {
                        px.copy_from_slice(&[
                            (f64::from(colour[0]) * shade) as u8,
                            (f64::from(colour[1]) * shade) as u8,
                            (f64::from(colour[2]) * shade) as u8,
                            255,
                        ]);
                    }
                }
            }
        }
    }
    rgba
}

/// RGBA pixels as a PNG file.
pub fn png(rgba: &[u8], w: u32, h: u32) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, w, h);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        let mut writer = enc.write_header().ok()?;
        writer.write_image_data(rgba).ok()?;
    }
    Some(out)
}

/// The "Quite OK Image" format some printer screens read.
fn qoi(rgba: &[u8], w: u32, h: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity(rgba.len() / 2 + 22);
    out.extend_from_slice(b"qoif");
    out.extend_from_slice(&w.to_be_bytes());
    out.extend_from_slice(&h.to_be_bytes());
    out.extend_from_slice(&[4, 0]);
    let mut seen = [[0u8; 4]; 64];
    let mut prev = [0u8, 0, 0, 255];
    let mut run = 0u8;
    let px_count = rgba.len() / 4;
    for i in 0..px_count {
        let Some(p) = rgba.get(i * 4..i * 4 + 4) else { break };
        let px = [p[0], p[1], p[2], p[3]];
        if px == prev {
            run += 1;
            if run == 62 || i == px_count - 1 {
                out.push(0xC0 | (run - 1));
                run = 0;
            }
            continue;
        }
        if run > 0 {
            out.push(0xC0 | (run - 1));
            run = 0;
        }
        let hash =
            (usize::from(px[0]) * 3 + usize::from(px[1]) * 5 + usize::from(px[2]) * 7 + usize::from(px[3]) * 11) % 64;
        if seen[hash] == px {
            out.push(hash as u8);
        } else {
            seen[hash] = px;
            if px[3] == prev[3] {
                out.extend_from_slice(&[0xFE, px[0], px[1], px[2]]);
            } else {
                out.extend_from_slice(&[0xFF, px[0], px[1], px[2], px[3]]);
            }
        }
        prev = px;
    }
    out.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 1]);
    out
}

fn base64(bytes: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk.first().copied().unwrap_or(0),
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for k in 0..4 {
            if k <= chunk.len() {
                out.push(char::from(T[((n >> (18 - 6 * k)) & 63) as usize]));
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// The thumbnail comment blocks for the top of the G-code.
pub fn blocks(items: &[Item<'_>], specs: &[Spec]) -> String {
    if specs.is_empty() || items.is_empty() {
        return String::new();
    }
    let mut out = String::from("; THUMBNAIL_BLOCK_START\n");
    for spec in specs {
        let rgba = render(items, spec.width, spec.height);
        let (data, tag) = match spec.format {
            Format::Png => (png(&rgba, spec.width, spec.height).unwrap_or_default(), "thumbnail"),
            Format::Qoi => (qoi(&rgba, spec.width, spec.height), "thumbnail_QOI"),
        };
        if data.is_empty() {
            continue;
        }
        let text = base64(&data);
        out.push_str(&format!(";\n; {tag} begin {}x{} {}\n", spec.width, spec.height, text.len()));
        for line in text.as_bytes().chunks(78) {
            out.push_str("; ");
            out.push_str(std::str::from_utf8(line).unwrap_or(""));
            out.push('\n');
        }
        out.push_str(&format!("; {tag} end\n"));
    }
    out.push_str("; THUMBNAIL_BLOCK_END\n\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use CosmicSlicer_model::primitives;

    #[test]
    fn specs_parse_like_orca() {
        let mut w = Vec::new();
        let s = parse("48x48/PNG, 300x300/QOI,96x96/JPG,bad,0x5", "PNG", &mut w);
        assert_eq!(s.len(), 3);
        assert_eq!(s[1], Spec { width: 300, height: 300, format: Format::Qoi });
        assert_eq!(w.len(), 1);
        assert_eq!(parse("48x48,300x300", "QOI", &mut w)[0].format, Format::Qoi);
    }

    #[test]
    fn a_cube_draws_and_encodes() {
        let cube = primitives::cube(20.0);
        let items = [Item { mesh: &cube, matrix: Mat4::IDENTITY, colour: [240, 120, 60] }];
        let img = render(&items, 64, 64);
        let painted = img.chunks(4).filter(|p| p[3] == 255).count();
        assert!(painted > 64 * 64 / 4, "{painted}");
        // The corners stay transparent.
        assert_eq!(img[3], 0);
        let text = blocks(
            &items,
            &[Spec { width: 64, height: 64, format: Format::Png }, Spec { width: 32, height: 32, format: Format::Qoi }],
        );
        assert!(text.contains("; thumbnail begin 64x64 "));
        assert!(text.contains("; thumbnail_QOI begin 32x32 "));
        assert!(text.lines().all(|l| l.len() <= 80));
        assert_eq!(base64(b"Man"), "TWFu");
        assert_eq!(base64(b"Ma"), "TWE=");
    }
}
