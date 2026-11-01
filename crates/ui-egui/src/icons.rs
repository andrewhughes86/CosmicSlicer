//! Icons: OrcaSlicer's SVG artwork (and CosmicSlicer's own), embedded at build time,
//! recoloured into the active theme and rasterised on demand.
//!
//! OrcaSlicer draws its icons in its teal (`#009688`) and greys. Teals and greens become the
//! theme accent; greys become the theme's ink, so the same file reads on dark and light.

use std::collections::HashMap;
use std::panic::{AssertUnwindSafe, catch_unwind};

use egui::{Color32, ColorImage, Rect, TextureHandle, TextureId, TextureOptions, Ui, pos2};
use resvg::{tiny_skia, usvg};

use crate::theme::Tokens;

include!(concat!(env!("OUT_DIR"), "/icons.rs"));

/// Largest raster made, in pixels.
const MAX_PX: u32 = 512;

pub fn source(name: &str) -> Option<&'static str> {
    ICONS.binary_search_by(|(n, _)| (*n).cmp(name)).ok().and_then(|i| ICONS.get(i)).map(|(_, svg)| *svg)
}

fn rgb_of(hex: &str) -> Option<Color32> {
    let full = match hex.len() {
        3 => hex.chars().flat_map(|c| [c, c]).collect::<String>(),
        6 => hex.to_owned(),
        _ => return None,
    };
    let v = u32::from_str_radix(&full, 16).ok()?;
    Some(Color32::from_rgb((v >> 16) as u8, (v >> 8) as u8, v as u8))
}

/// The theme colour for one of OrcaSlicer's literal icon colours.
fn remap(c: Color32, t: &Tokens) -> Color32 {
    let (r, g, b) = (f32::from(c.r()) / 255.0, f32::from(c.g()) / 255.0, f32::from(c.b()) / 255.0);
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let chroma = max - min;
    if chroma < 0.12 {
        // Greys: dark and mid greys are the ink, pale ones the muted ink.
        let l = (max + min) / 2.0;
        return if l < 0.62 || l > 0.97 { t.text } else { t.text_muted };
    }
    let hue = if max == r {
        60.0 * ((g - b) / chroma).rem_euclid(6.0)
    } else if max == g {
        60.0 * ((b - r) / chroma + 2.0)
    } else {
        60.0 * ((r - g) / chroma + 4.0)
    };
    // OrcaSlicer's teal and Bambu green.
    if (90.0..=200.0).contains(&hue) { t.accent } else { c }
}

/// Replaces every literal colour in `svg` with its theme colour.
pub fn colourise(svg: &str, t: &Tokens) -> String {
    let mut out = String::with_capacity(svg.len());
    let mut rest = svg;
    while let Some(i) = rest.find('#') {
        out.push_str(&rest[..i]);
        let after = &rest[i + 1..];
        let len = after.chars().take_while(char::is_ascii_hexdigit).count();
        let candidate = after.get(..len).unwrap_or_default();
        match (len == 3 || len == 6).then(|| rgb_of(candidate)).flatten() {
            Some(c) => {
                let m = remap(c, t);
                out.push_str(&format!("#{:02x}{:02x}{:02x}", m.r(), m.g(), m.b()));
                rest = after.get(len..).unwrap_or_default();
            }
            None => {
                out.push('#');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// Renders `svg` (already coloured) to fit a `px` × `px` image, centred.
pub fn rasterise(svg: &str, px: u32) -> Result<ColorImage, String> {
    let px = px.clamp(1, MAX_PX);
    catch_unwind(AssertUnwindSafe(|| {
        let tree = usvg::Tree::from_str(svg, &usvg::Options::default()).map_err(|e| e.to_string())?;
        let mut pixmap = tiny_skia::Pixmap::new(px, px).ok_or("could not allocate the icon raster")?;
        let size = tree.size();
        let scale = px as f32 / size.width().max(size.height()).max(1.0);
        let dx = (px as f32 - size.width() * scale) / 2.0;
        let dy = (px as f32 - size.height() * scale) / 2.0;
        resvg::render(&tree, tiny_skia::Transform::from_row(scale, 0.0, 0.0, scale, dx, dy), &mut pixmap.as_mut());
        Ok(ColorImage::from_rgba_premultiplied([px as usize, px as usize], pixmap.data()))
    }))
    .unwrap_or_else(|_| Err("the SVG renderer failed".into()))
}

/// Rasterised icons per (name, ink, pixel size, recolour). Failures are remembered so a bad
/// file is reported once and drawn as a placeholder.
#[derive(Clone, Default)]
pub struct IconCache {
    /// Keyed by the ink colour too, so themes and tinted variants never collide.
    textures: HashMap<(String, [u8; 4], u32, bool), Option<TextureHandle>>,
}

impl IconCache {
    pub fn get(&mut self, ctx: &egui::Context, name: &str, t: &Tokens, px: u32, recolour: bool) -> Option<TextureId> {
        let px = px.clamp(1, MAX_PX);
        let key = (name.to_owned(), t.text.to_array(), px, recolour);
        if let Some(entry) = self.textures.get(&key) {
            return entry.as_ref().map(TextureHandle::id);
        }
        let handle = source(name).and_then(|svg| {
            let svg = if recolour { colourise(svg, t) } else { svg.to_owned() };
            match rasterise(&svg, px) {
                Ok(image) => Some(ctx.load_texture(format!("icon-{name}-{px}"), image, TextureOptions::LINEAR)),
                Err(e) => {
                    log::warn!("icon {name}: {e}");
                    None
                }
            }
        });
        let id = handle.as_ref().map(TextureHandle::id);
        self.textures.insert(key, handle);
        id
    }
}

fn texture(ui: &Ui, name: &str, t: &Tokens, side: f32, recolour: bool) -> Option<TextureId> {
    let ctx = ui.ctx();
    let px = (side * ctx.pixels_per_point()).round() as u32;
    let cache_id = egui::Id::new("icon-cache");
    let mut cache = ctx.data_mut(|d| std::mem::take(d.get_temp_mut_or_default::<IconCache>(cache_id)));
    let tex = cache.get(ctx, name, t, px, recolour);
    ctx.data_mut(|d| d.insert_temp(cache_id, cache));
    tex
}

/// Draws icon `name` into `rect`, recoloured into the theme; dimmed when disabled.
pub fn paint(ui: &Ui, rect: Rect, name: &str, t: &Tokens, enabled: bool) {
    paint_with(ui, rect, name, t, enabled, true);
}

/// Draws icon `name` in its own colours (logos, plate pictures).
pub fn paint_original(ui: &Ui, rect: Rect, name: &str, t: &Tokens) {
    paint_with(ui, rect, name, t, true, false);
}

fn paint_with(ui: &Ui, rect: Rect, name: &str, t: &Tokens, enabled: bool, recolour: bool) {
    let side = rect.width().max(rect.height());
    let square = Rect::from_center_size(rect.center(), egui::vec2(side, side));
    match texture(ui, name, t, side, recolour) {
        Some(id) => {
            let tint = if enabled { Color32::WHITE } else { Color32::WHITE.gamma_multiply(0.35) };
            ui.painter().image(id, square, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), tint);
        }
        None => {
            let stroke = egui::Stroke::new(1.0, t.text_muted.gamma_multiply(0.5));
            ui.painter().rect_stroke(rect.shrink(rect.width() * 0.15), 3.0, stroke, egui::StrokeKind::Inside);
        }
    }
}

/// A square icon button with a hover and a checked state; returns the response.
pub fn button(ui: &mut Ui, name: &str, t: &Tokens, side: f32, checked: bool, tooltip: &str) -> egui::Response {
    button_enabled(ui, name, t, side, checked, true, tooltip)
}

pub fn button_enabled(
    ui: &mut Ui,
    name: &str,
    t: &Tokens,
    side: f32,
    checked: bool,
    enabled: bool,
    tooltip: &str,
) -> egui::Response {
    let sense = if enabled { egui::Sense::click() } else { egui::Sense::hover() };
    let (rect, response) = ui.allocate_exact_size(egui::vec2(side, side), sense);
    if checked {
        ui.painter().rect_filled(rect, 4.0, t.accent.gamma_multiply(0.22));
        ui.painter().rect_stroke(rect, 4.0, egui::Stroke::new(1.0, t.accent), egui::StrokeKind::Inside);
    } else if response.hovered() && enabled {
        ui.painter().rect_filled(rect, 4.0, t.hover_bg);
    }
    paint(ui, rect.shrink(side * 0.12), name, t, enabled);
    if tooltip.is_empty() { response } else { response.on_hover_text(tooltip) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::{DARK, LIGHT};

    #[test]
    fn teal_becomes_the_accent_and_greys_the_ink() {
        let svg = r##"<path style="fill:#009688"/><path fill="#2b3436"/><path fill="#fff"/><path fill="#ff0000"/>"##;
        let d = colourise(svg, &DARK);
        assert!(d.contains("#22d3ee"), "{d}");
        assert!(d.contains("#e6ecf5"), "{d}");
        assert!(d.contains("#ff0000"), "{d}");
        assert!(!colourise(svg, &LIGHT).contains("#009688"));
        // Not colours: ids and fragment links survive.
        assert_eq!(colourise("url(#grad1)", &DARK), "url(#grad1)");
    }

    #[test]
    fn every_icon_renders() {
        assert!(ICONS.len() > 100);
        let mut failed = Vec::new();
        for (name, svg) in ICONS {
            if rasterise(&colourise(svg, &DARK), 24).is_err() {
                failed.push(*name);
            }
        }
        assert!(failed.is_empty(), "{failed:?}");
    }

    #[test]
    fn the_icons_the_ui_names_exist() {
        let used = [
            "toolbar_open",
            "toolbar_add_plate",
            "toolbar_orient",
            "toolbar_arrange",
            "split_objects",
            "split_parts",
            "toolbar_variable_layer_height",
            "toolbar_move",
            "toolbar_rotate",
            "toolbar_scale",
            "toolbar_flatten",
            "toolbar_cut",
            "toolbar_meshboolean",
            "toolbar_support",
            "toolbar_seam",
            "toolbar_fuzzy_skin_paint",
            "mmu_segmentation",
            "toolbar_text",
            "toolbar_measure",
            "toolbar_assembly",
            "toolbar_brimears",
            "topbar_save",
            "topbar_undo",
            "topbar_redo",
            "topbar_file",
            "topbar_dropdown",
            "tab_home_active",
            "tab_3d_active",
            "tab_preview_active",
            "tab_monitor_active",
            "tab_multi_active",
            "tab_auxiliary_active",
            "param_layer_height",
            "custom-gcode_quality",
            "edit",
            "add",
            "delete",
            "search",
            "cosmic_logo",
            "plate_arrange",
            "plate_orient",
            "plate_locked",
            "plate_unlocked",
            "plate_settings",
            "plate_close",
            "printer_thumbnail",
            "compare",
            "save",
            "im_visible",
            "im_hidden",
        ];
        let missing: Vec<&str> = used.into_iter().filter(|n| source(n).is_none()).collect();
        assert!(missing.is_empty(), "{missing:?}");
    }
}
