//! Outlines for the Text shape tool: lines of text in a font, or the shapes of an SVG file,
//! as closed loops in millimetres (Y up), ready to extrude.

use glam::Vec2;
use skrifa::MetadataProvider;
use skrifa::instance::{LocationRef, Size};
use skrifa::outline::{DrawSettings, OutlinePen};

/// Points per curve when flattening.
const CURVE_STEPS: usize = 8;
/// Most outline points made, so a hostile font or SVG cannot exhaust memory.
const MAX_POINTS: usize = 1_000_000;

/// Collects a glyph's or path's outline as loops, flattening curves.
#[derive(Default)]
struct Loops {
    loops: Vec<Vec<Vec2>>,
    current: Vec<Vec2>,
    /// Placement: scale, then offset.
    scale: f32,
    offset: Vec2,
    points: usize,
}

impl Loops {
    fn at(&self, x: f32, y: f32) -> Vec2 {
        Vec2::new(x, y) * self.scale + self.offset
    }

    fn push(&mut self, p: Vec2) {
        if self.points < MAX_POINTS && p.is_finite() && self.current.last() != Some(&p) {
            self.current.push(p);
            self.points += 1;
        }
    }

    fn last(&self) -> Vec2 {
        self.current.last().copied().unwrap_or(self.offset)
    }

    fn finish(&mut self) {
        let mut l = std::mem::take(&mut self.current);
        if l.len() > 1 && l.first() == l.last() {
            l.pop();
        }
        if l.len() >= 3 {
            self.loops.push(l);
        }
    }
}

impl OutlinePen for Loops {
    fn move_to(&mut self, x: f32, y: f32) {
        self.finish();
        let p = self.at(x, y);
        self.push(p);
    }

    fn line_to(&mut self, x: f32, y: f32) {
        let p = self.at(x, y);
        self.push(p);
    }

    fn quad_to(&mut self, cx0: f32, cy0: f32, x: f32, y: f32) {
        let (p0, c, p1) = (self.last(), self.at(cx0, cy0), self.at(x, y));
        for k in 1..=CURVE_STEPS {
            let t = k as f32 / CURVE_STEPS as f32;
            let u = 1.0 - t;
            self.push(p0 * u * u + c * 2.0 * u * t + p1 * t * t);
        }
    }

    fn curve_to(&mut self, cx0: f32, cy0: f32, cx1: f32, cy1: f32, x: f32, y: f32) {
        let (p0, c0, c1, p1) = (self.last(), self.at(cx0, cy0), self.at(cx1, cy1), self.at(x, y));
        for k in 1..=CURVE_STEPS {
            let t = k as f32 / CURVE_STEPS as f32;
            let u = 1.0 - t;
            self.push(p0 * u * u * u + c0 * 3.0 * u * u * t + c1 * 3.0 * u * t * t + p1 * t * t * t);
        }
    }

    fn close(&mut self) {
        self.finish();
    }
}

/// The fonts that come with CosmicSlicer.
pub const FONTS: [(&str, &[u8]); 2] =
    [("Sans", epaint_default_fonts::UBUNTU_LIGHT), ("Mono", epaint_default_fonts::HACK_REGULAR)];

/// `text` in `font` (a TrueType or OpenType file), each line `height` mm tall, lines below one
/// another, the first line's baseline at Y 0.
pub fn text(font: &[u8], text: &str, height: f32) -> Result<Vec<Vec<Vec2>>, String> {
    let font = skrifa::FontRef::new(font).map_err(|e| format!("the font could not be read: {e}"))?;
    if !(height.is_finite() && height > 0.0) {
        return Err("the text needs a height".into());
    }
    let size = Size::new(height);
    let charmap = font.charmap();
    let metrics = font.glyph_metrics(size, LocationRef::default());
    let glyphs = font.outline_glyphs();
    let mut pen = Loops { scale: 1.0, ..Default::default() };
    for (line_no, line) in text.lines().enumerate() {
        let mut x = 0.0;
        let y = -(line_no as f32) * height * 1.25;
        for ch in line.chars() {
            let Some(gid) = charmap.map(ch) else { continue };
            pen.offset = Vec2::new(x, y);
            if let Some(g) = glyphs.get(gid) {
                // A glyph that will not draw is left out; the rest of the text still makes it.
                let _ = g.draw(DrawSettings::unhinted(size, LocationRef::default()), &mut pen);
                pen.finish();
            }
            x += metrics.advance_width(gid).unwrap_or(height * 0.5);
        }
    }
    if pen.loops.is_empty() {
        return Err("the text has nothing to emboss".into());
    }
    Ok(pen.loops)
}

/// The filled shapes of an SVG file, scaled so the drawing's larger side is `size` mm, Y up.
pub fn svg(bytes: &[u8], size: f32) -> Result<Vec<Vec<Vec2>>, String> {
    let tree = resvg::usvg::Tree::from_data(bytes, &resvg::usvg::Options::default())
        .map_err(|e| format!("the SVG could not be read: {e}"))?;
    let mut pen = Loops { scale: 1.0, ..Default::default() };
    collect(tree.root(), &mut pen);
    pen.finish();
    let mut loops = pen.loops;
    // Scale to size, SVG's Y down turned up.
    let (mut lo, mut hi) = (Vec2::splat(f32::INFINITY), Vec2::splat(f32::NEG_INFINITY));
    for p in loops.iter().flatten() {
        lo = lo.min(*p);
        hi = hi.max(*p);
    }
    let extent = (hi - lo).max_element();
    if loops.is_empty() || !(extent.is_finite() && extent > 0.0) {
        return Err("the SVG has no filled shapes".into());
    }
    let k = size / extent;
    for l in &mut loops {
        for p in l.iter_mut() {
            *p = Vec2::new((p.x - lo.x) * k, (hi.y - p.y) * k);
        }
    }
    Ok(loops)
}

fn collect(group: &resvg::usvg::Group, pen: &mut Loops) {
    use resvg::usvg::Node;
    use resvg::usvg::tiny_skia_path::PathSegment;
    for node in group.children() {
        match node {
            Node::Group(g) => collect(g, pen),
            Node::Path(p) if p.fill().is_some() => {
                let t = p.abs_transform();
                let map = |pt: resvg::usvg::tiny_skia_path::Point| {
                    let mut q = pt;
                    t.map_point(&mut q);
                    (q.x, q.y)
                };
                for seg in p.data().segments() {
                    match seg {
                        PathSegment::MoveTo(a) => {
                            let (x, y) = map(a);
                            pen.move_to(x, y);
                        }
                        PathSegment::LineTo(a) => {
                            let (x, y) = map(a);
                            pen.line_to(x, y);
                        }
                        PathSegment::QuadTo(c, a) => {
                            let ((cx, cy), (x, y)) = (map(c), map(a));
                            pen.quad_to(cx, cy, x, y);
                        }
                        PathSegment::CubicTo(c0, c1, a) => {
                            let ((c0x, c0y), (c1x, c1y), (x, y)) = (map(c0), map(c1), map(a));
                            pen.curve_to(c0x, c0y, c1x, c1y, x, y);
                        }
                        PathSegment::Close => pen.close(),
                    }
                }
                pen.finish();
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_becomes_loops_and_a_solid() {
        let loops = text(FONTS[0].1, "Ab\no", 10.0);
        assert!(loops.as_ref().is_ok_and(|l| l.len() >= 4), "{:?}", loops.as_ref().map(Vec::len));
        let Ok(loops) = loops else { return };
        let mesh = CosmicSlicer_model::shape2d::extrude(&loops, 2.0);
        assert!(mesh.volume() > 20.0, "{}", mesh.volume());
        assert!(text(FONTS[0].1, "   ", 10.0).is_err());
        assert!(text(b"not a font", "A", 10.0).is_err());
    }

    #[test]
    fn svg_shapes_scale_to_size() {
        let svg_data = br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 100 50">
            <rect x="0" y="0" width="100" height="50" fill="black"/>
            <circle cx="25" cy="25" r="10" fill="white"/></svg>"#;
        let loops = svg(svg_data, 40.0);
        assert!(loops.is_ok(), "{loops:?}");
        let Ok(loops) = loops else { return };
        let max = loops.iter().flatten().fold(Vec2::ZERO, |a, p| a.max(*p));
        assert!((max.x - 40.0).abs() < 1e-3 && (max.y - 20.0).abs() < 1e-3, "{max:?}");
        assert!(svg(b"<svg", 10.0).is_err());
    }
}
