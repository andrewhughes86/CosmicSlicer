//! Theme tokens: the only place UI colours are defined.
//!
//! The palettes are the Cosmic family's (CosmicDMS, CosmicCut): deep slate with electric
//! blue / cyan accents, and a light variant for bright rooms. [`apply`] maps them onto
//! `egui::Visuals`; the viewport reads the scene colours directly.

use egui::{Color32, CornerRadius, Rgba, Stroke, Visuals};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Theme {
    #[default]
    Dark,
    Light,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Tokens {
    pub dark: bool,
    /// Window background behind every panel; also the top bar.
    pub app_bg: Color32,
    /// Panel and tab body surfaces.
    pub panel_bg: Color32,
    /// Raised surfaces: tab bar, inputs, section headers, cards.
    pub raised_bg: Color32,
    pub hover_bg: Color32,
    pub border: Color32,
    pub text: Color32,
    pub text_muted: Color32,
    /// Primary accent: selection, focus, active tab.
    pub accent: Color32,
    /// Secondary accent for emphasis (strong buttons, links).
    pub accent_strong: Color32,
    /// Text drawn on an accent fill.
    pub on_accent: Color32,
    pub success: Color32,
    pub warning: Color32,
    pub error: Color32,
    /// A setting changed from its preset.
    pub modified: Color32,
    pub viewport_top: Color32,
    pub viewport_bottom: Color32,
    /// The build plate surface, its grid and its outline.
    pub bed: Color32,
    pub bed_grid: Color32,
    pub bed_edge: Color32,
    /// A plate that is not the current one.
    pub bed_inactive: Color32,
    pub axis_x: Color32,
    pub axis_y: Color32,
    pub axis_z: Color32,
    /// Selected and hovered objects.
    pub selected: Color32,
    pub hovered: Color32,
    /// Objects outside the printable volume.
    pub outside: Color32,
    /// Floating canvas toolbars.
    pub toolbar_bg: Color32,
}

const fn hex(rgb: u32) -> Color32 {
    Color32::from_rgb((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8)
}

/// Modern dark (default): deep slate with electric blue / cyan accents.
pub const DARK: Tokens = Tokens {
    dark: true,
    app_bg: hex(0x0A101C),
    panel_bg: hex(0x0F1726),
    raised_bg: hex(0x162133),
    hover_bg: hex(0x213049),
    border: hex(0x2A3A55),
    text: hex(0xE6ECF5),
    text_muted: hex(0x98A6BC),
    accent: hex(0x22D3EE),
    accent_strong: hex(0x3B82F6),
    on_accent: hex(0x04121A),
    success: hex(0x34D399),
    warning: hex(0xFBBF24),
    error: hex(0xF87171),
    modified: hex(0xFB923C),
    viewport_top: hex(0x1C2840),
    viewport_bottom: hex(0x080D17),
    bed: hex(0x1A2436),
    bed_grid: hex(0x2C3B55),
    bed_edge: hex(0x55657F),
    bed_inactive: hex(0x141C2B),
    axis_x: hex(0xF0525B),
    axis_y: hex(0x3DD68C),
    axis_z: hex(0x4C8DFF),
    selected: hex(0x22D3EE),
    hovered: hex(0x7DD3FC),
    outside: hex(0xF87171),
    toolbar_bg: hex(0x162133),
};

/// Light: off-white surfaces, white panels, charcoal type for bright rooms.
pub const LIGHT: Tokens = Tokens {
    dark: false,
    app_bg: hex(0xE9EAE6),
    panel_bg: hex(0xFFFFFF),
    raised_bg: hex(0xF6F6F3),
    hover_bg: hex(0xE4E7EB),
    border: hex(0xC5CBD3),
    text: hex(0x1C2025),
    text_muted: hex(0x505A66),
    accent: hex(0x0369A1),
    accent_strong: hex(0x1D4ED8),
    on_accent: hex(0xFFFFFF),
    success: hex(0x047857),
    warning: hex(0xA16207),
    error: hex(0xB91C1C),
    modified: hex(0xC2410C),
    viewport_top: hex(0xFAFBFC),
    viewport_bottom: hex(0xD5DBE3),
    bed: hex(0xDCE1E8),
    bed_grid: hex(0xB4BDC9),
    bed_edge: hex(0x8A96A8),
    bed_inactive: hex(0xE8EBF0),
    axis_x: hex(0xD12F3A),
    axis_y: hex(0x16924A),
    axis_z: hex(0x2563EB),
    selected: hex(0x0284C7),
    hovered: hex(0x38BDF8),
    outside: hex(0xDC2626),
    toolbar_bg: hex(0xFFFFFF),
};

impl Tokens {
    pub fn for_theme(theme: Theme) -> &'static Tokens {
        match theme {
            Theme::Dark => &DARK,
            Theme::Light => &LIGHT,
        }
    }
}

/// `a` turned `t` (0 to 1) of the way to `b`.
pub fn blend(a: Color32, b: Color32, t: f32) -> Color32 {
    let t = t.clamp(0.0, 1.0);
    let mix = |x: u8, y: u8| (f32::from(x) + (f32::from(y) - f32::from(x)) * t).round() as u8;
    Color32::from_rgb(mix(a.r(), b.r()), mix(a.g(), b.g()), mix(a.b(), b.b()))
}

/// sRGB colour to linear RGBA for the GPU.
pub fn linear(c: Color32) -> [f32; 4] {
    Rgba::from(c).to_array()
}

/// Parses "#RRGGBB" (as OrcaSlicer stores filament colours).
pub fn parse_hex(s: &str) -> Option<Color32> {
    let h = s.trim().trim_start_matches('#');
    if h.len() < 6 {
        return None;
    }
    let v = u32::from_str_radix(h.get(..6)?, 16).ok()?;
    Some(hex(v))
}

pub fn to_hex(c: Color32) -> String {
    format!("#{:02X}{:02X}{:02X}", c.r(), c.g(), c.b())
}

/// Black or white, whichever reads on `bg`.
pub fn ink_on(bg: Color32) -> Color32 {
    let [r, g, b, _] = linear(bg);
    if 0.2126 * r + 0.7152 * g + 0.0722 * b > 0.25 { Color32::BLACK } else { Color32::WHITE }
}

/// Installs `tokens` as the egui visuals.
pub fn apply(ctx: &egui::Context, tokens: &Tokens) {
    ctx.set_visuals(visuals(tokens));
    ctx.all_styles_mut(|s| {
        s.spacing.item_spacing = egui::vec2(6.0, 5.0);
        s.spacing.button_padding = egui::vec2(8.0, 3.0);
        s.spacing.interact_size.y = 24.0;
        s.spacing.combo_height = 320.0;
    });
}

pub fn visuals(t: &Tokens) -> Visuals {
    let mut v = if t.dark { Visuals::dark() } else { Visuals::light() };
    let radius = CornerRadius::same(4);
    v.override_text_color = Some(t.text);
    v.weak_text_color = Some(t.text_muted);
    v.hyperlink_color = t.accent_strong;
    v.faint_bg_color = t.raised_bg;
    v.extreme_bg_color = t.app_bg;
    v.text_edit_bg_color = Some(t.app_bg);
    v.code_bg_color = t.raised_bg;
    v.warn_fg_color = t.warning;
    v.error_fg_color = t.error;
    v.panel_fill = t.panel_bg;
    v.window_fill = t.panel_bg;
    v.window_stroke = Stroke::new(1.0, t.border);
    v.window_corner_radius = CornerRadius::same(6);
    v.menu_corner_radius = radius;
    v.selection.bg_fill = t.accent.gamma_multiply(if t.dark { 0.45 } else { 0.25 });
    v.selection.stroke = Stroke::new(1.0, t.accent);
    v.slider_trailing_fill = true;

    let w = &mut v.widgets;
    w.noninteractive.bg_fill = t.panel_bg;
    w.noninteractive.weak_bg_fill = t.panel_bg;
    w.noninteractive.bg_stroke = Stroke::new(1.0, t.border);
    w.noninteractive.fg_stroke = Stroke::new(1.0, t.text);
    w.noninteractive.corner_radius = radius;

    w.inactive.bg_fill = t.raised_bg;
    w.inactive.weak_bg_fill = t.raised_bg;
    w.inactive.bg_stroke = Stroke::new(1.0, t.border);
    w.inactive.fg_stroke = Stroke::new(1.0, t.text);
    w.inactive.corner_radius = radius;

    w.hovered.bg_fill = t.hover_bg;
    w.hovered.weak_bg_fill = t.hover_bg;
    w.hovered.bg_stroke = Stroke::new(1.0, t.accent);
    w.hovered.fg_stroke = Stroke::new(1.5, t.text);
    w.hovered.corner_radius = radius;

    w.active.bg_fill = t.accent_strong.gamma_multiply(0.45);
    w.active.weak_bg_fill = t.accent_strong.gamma_multiply(0.45);
    w.active.bg_stroke = Stroke::new(1.0, t.accent);
    w.active.fg_stroke = Stroke::new(2.0, t.text);
    w.active.corner_radius = radius;

    w.open.bg_fill = t.hover_bg;
    w.open.weak_bg_fill = t.hover_bg;
    w.open.bg_stroke = Stroke::new(1.0, t.accent);
    w.open.fg_stroke = Stroke::new(1.0, t.text);
    w.open.corner_radius = radius;
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    fn luminance(c: Color32) -> f32 {
        let [r, g, b, _] = linear(c);
        0.2126 * r + 0.7152 * g + 0.0722 * b
    }

    fn contrast(a: Color32, b: Color32) -> f32 {
        let (la, lb) = (luminance(a), luminance(b));
        (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
    }

    #[test]
    fn text_meets_wcag_aa_on_every_surface() {
        for t in [&DARK, &LIGHT] {
            for bg in [t.app_bg, t.panel_bg, t.raised_bg, t.hover_bg] {
                assert!(contrast(t.text, bg) >= 7.0, "text on {bg:?}: {}", contrast(t.text, bg));
                assert!(contrast(t.text_muted, bg) >= 4.5, "muted on {bg:?}: {}", contrast(t.text_muted, bg));
                assert!(contrast(t.modified, bg) >= 3.0, "modified on {bg:?}: {}", contrast(t.modified, bg));
            }
            assert!(contrast(t.on_accent, t.accent) >= 4.5, "on_accent: {}", contrast(t.on_accent, t.accent));
        }
    }

    #[test]
    fn hex_round_trip() {
        assert_eq!(parse_hex("#22D3EE").map(to_hex).as_deref(), Some("#22D3EE"));
        assert_eq!(parse_hex("nope"), None);
        assert_eq!(parse_hex("#12"), None);
    }
}
