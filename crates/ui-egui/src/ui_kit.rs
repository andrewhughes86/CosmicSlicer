//! The outer UI containers. Feature modules declare a [`Surface`] and get their frame from
//! here; they never build raw egui frames or windows.

use egui::{CornerRadius, Frame, InnerResponse, Margin, Popup, Response, Stroke, Ui};

use crate::theme::Tokens;

/// Semantic role of a container; it decides fill, border and spacing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Surface {
    /// The title / menu bar across the top.
    TopBar,
    /// The page tabs and the slice buttons under it.
    TabBar,
    /// The settings sidebar at the left of Prepare and Preview.
    Sidebar,
    /// A full page (Home, Device, Project).
    Page,
    /// The 3D canvas.
    Canvas,
    /// Floating chrome over the canvas (toolbars, legends, gizmo panels).
    Overlay,
    /// Dropdown menus.
    Menu,
    /// Floating dialogs.
    Dialog,
    /// A group of content inside a page.
    Card,
}

pub fn frame(surface: Surface, t: &Tokens) -> Frame {
    match surface {
        Surface::TopBar => Frame::new().fill(t.app_bg).inner_margin(Margin::ZERO),
        Surface::TabBar => {
            Frame::new().fill(t.raised_bg).inner_margin(Margin::symmetric(8, 0)).stroke(Stroke::new(1.0, t.border))
        }
        Surface::Sidebar => Frame::new().fill(t.panel_bg).inner_margin(Margin::ZERO).stroke(Stroke::new(1.0, t.border)),
        Surface::Page => Frame::new().fill(t.app_bg).inner_margin(Margin::same(16)),
        Surface::Canvas => Frame::new().fill(t.viewport_bottom).inner_margin(Margin::ZERO),
        Surface::Overlay => Frame::new()
            .fill(t.toolbar_bg.gamma_multiply(0.94))
            .inner_margin(Margin::symmetric(8, 6))
            .corner_radius(CornerRadius::same(6))
            .stroke(Stroke::new(1.0, t.border)),
        // Windows glow in the accent colour all round, so they stand off the work behind.
        Surface::Dialog => Frame::new()
            .fill(t.panel_bg)
            .inner_margin(Margin::same(12))
            .corner_radius(CornerRadius::same(6))
            .stroke(Stroke::new(1.0, t.accent.gamma_multiply(0.55)))
            .shadow(egui::epaint::Shadow { offset: [0, 0], blur: 26, spread: 2, color: t.accent.gamma_multiply(0.38) }),
        Surface::Card => Frame::new()
            .fill(t.panel_bg)
            .inner_margin(Margin::symmetric(16, 14))
            .corner_radius(CornerRadius::same(8))
            .stroke(Stroke::new(1.0, t.border)),
        Surface::Menu => Frame::new()
            .fill(t.panel_bg)
            .inner_margin(Margin::same(4))
            .corner_radius(CornerRadius::same(4))
            .stroke(Stroke::new(1.0, t.border)),
    }
}

pub fn show<R>(surface: Surface, ui: &mut Ui, t: &Tokens, add: impl FnOnce(&mut Ui) -> R) -> InnerResponse<R> {
    frame(surface, t).show(ui, add)
}

/// A dropdown menu that opens below `button` when it is clicked.
pub fn menu_below<R>(button: &Response, t: &Tokens, add: impl FnOnce(&mut Ui) -> R) -> Option<InnerResponse<R>> {
    Popup::menu(button).frame(frame(Surface::Menu, t)).show(add)
}

/// The same menu, opened by right-clicking `response` instead.
pub fn context_menu<R>(response: &Response, t: &Tokens, add: impl FnOnce(&mut Ui) -> R) -> Option<InnerResponse<R>> {
    Popup::context_menu(response).frame(frame(Surface::Menu, t)).show(add)
}

/// A menu entry with a shortcut hint at the right.
pub fn menu_item(ui: &mut Ui, label: &str, shortcut: &str, enabled: bool) -> bool {
    let button = egui::Button::new(label).shortcut_text(shortcut);
    let clicked = ui.add_enabled(enabled, button).clicked();
    if clicked {
        ui.close();
    }
    clicked
}

/// A check-marked menu entry.
pub fn menu_check(ui: &mut Ui, label: &str, shortcut: &str, checked: bool) -> bool {
    let text = format!("{}  {label}", if checked { "✔" } else { "   " });
    menu_item(ui, &text, shortcut, true)
}

/// A floating, titled dialog window framed as [`Surface::Dialog`].
pub struct Window {
    title: String,
    size: Option<egui::Vec2>,
    id: egui::Id,
}

impl Window {
    pub fn dialog(title: impl Into<String>) -> Self {
        let title = title.into();
        Self { id: egui::Id::new(("dialog", title.clone())), title, size: None }
    }

    /// A centred, resizable window (settings editors, preferences).
    pub fn sized(title: impl Into<String>, size: egui::Vec2) -> Self {
        let title = title.into();
        Self { id: egui::Id::new(("dialog", title.clone())), title, size: Some(size) }
    }

    /// Keeps the window's place when its title changes (a preset's name in the title).
    pub fn id(mut self, id: impl std::hash::Hash + std::fmt::Debug) -> Self {
        self.id = egui::Id::new(("dialog", id));
        self
    }

    /// Shows the window while `open` is true; the title bar's close button clears it.
    pub fn show<R>(
        self,
        ctx: &egui::Context,
        t: &Tokens,
        open: &mut bool,
        add: impl FnOnce(&mut Ui) -> R,
    ) -> Option<R> {
        let title = self.title.clone();
        let mut close = false;
        let shown = egui::Window::new(self.title)
            .id(self.id)
            .open(open)
            .title_bar(false)
            .collapsible(false)
            .resizable(self.size.is_some())
            .max_size(ctx.content_rect().size() * 0.95)
            .default_size(self.size.unwrap_or(egui::vec2(360.0, 200.0)))
            .pivot(egui::Align2::CENTER_CENTER)
            .default_pos(ctx.content_rect().center())
            .frame(frame(Surface::Dialog, t).inner_margin(Margin::ZERO))
            .show(ctx, |ui| {
                let spacing = ui.spacing().item_spacing;
                ui.spacing_mut().item_spacing.y = 0.0;
                let top = ui.cursor().min;
                ui.add_space(TITLE_HEIGHT);
                let inner = Frame::new()
                    .inner_margin(Margin::same(12))
                    .show(ui, |ui| {
                        ui.spacing_mut().item_spacing = spacing;
                        add(ui)
                    })
                    .inner;
                let bar = egui::Rect::from_min_max(top, egui::pos2(ui.min_rect().right(), top.y + TITLE_HEIGHT));
                close = title_bar(ui, t, bar, &title);
                inner
            })
            .and_then(|r| r.inner);
        if close {
            *open = false;
        }
        shown
    }
}

/// Height of a window's title bar.
const TITLE_HEIGHT: f32 = 28.0;

/// Paints a window's title bar into `bar`; true when its close button was clicked.
fn title_bar(ui: &Ui, t: &Tokens, bar: egui::Rect, title: &str) -> bool {
    let painter = ui.painter();
    painter.rect_filled(bar, CornerRadius { nw: 6, ne: 6, sw: 0, se: 0 }, t.raised_bg);
    painter.hline(bar.x_range(), bar.bottom() - 0.5, Stroke::new(1.0, t.border));
    painter.text(
        egui::pos2(bar.left() + 12.0, bar.center().y),
        egui::Align2::LEFT_CENTER,
        title,
        egui::FontId::proportional(13.5),
        t.text,
    );
    let button = egui::Rect::from_min_max(egui::pos2(bar.right() - 34.0, bar.top()), bar.right_bottom());
    let response = ui.interact(button, ui.id().with("window-close"), egui::Sense::click());
    let ink = if response.hovered() {
        painter.rect_filled(button.shrink2(egui::vec2(4.0, 4.0)), 3.0, t.error);
        Stroke::new(1.2, egui::Color32::WHITE)
    } else {
        Stroke::new(1.2, t.text_muted)
    };
    let (c, h) = (button.center(), 4.0);
    painter.line_segment([c + egui::vec2(-h, -h), c + egui::vec2(h, h)], ink);
    painter.line_segment([c + egui::vec2(-h, h), c + egui::vec2(h, -h)], ink);
    response.on_hover_text("Close").clicked()
}

/// A small × that closes a panel.
pub fn close_button(ui: &mut Ui, t: &Tokens, tooltip: &str) -> Response {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(18.0, 18.0), egui::Sense::click());
    if response.hovered() {
        ui.painter().rect_filled(rect, 3.0, t.hover_bg);
    }
    let ink = Stroke::new(1.3, if response.hovered() { t.text } else { t.text_muted });
    let (c, h) = (rect.center(), 4.0);
    ui.painter().line_segment([c + egui::vec2(-h, -h), c + egui::vec2(h, h)], ink);
    ui.painter().line_segment([c + egui::vec2(-h, h), c + egui::vec2(h, -h)], ink);
    response.on_hover_text(tooltip)
}

/// An accent-filled button (Slice, OK).
pub fn primary_button(ui: &mut Ui, t: &Tokens, label: &str) -> Response {
    ui.add(
        egui::Button::new(egui::RichText::new(label).color(t.on_accent).strong())
            .fill(t.accent)
            .stroke(Stroke::NONE)
            .corner_radius(CornerRadius::same(4)),
    )
}

/// An on/off switch like OrcaSlicer's "Advanced" toggle.
pub fn switch(ui: &mut Ui, t: &Tokens, on: &mut bool) -> Response {
    let size = egui::vec2(30.0, 16.0);
    let (rect, mut response) = ui.allocate_exact_size(size, egui::Sense::click());
    if response.clicked() {
        *on = !*on;
        response.mark_changed();
    }
    let how_on = ui.ctx().animate_bool_responsive(response.id, *on);
    let fill = crate::theme::blend(t.border, t.accent, how_on);
    ui.painter().rect_filled(rect, rect.height() / 2.0, fill);
    let r = rect.height() / 2.0 - 2.0;
    let x = egui::lerp(rect.left() + r + 2.0..=rect.right() - r - 2.0, how_on);
    ui.painter().circle_filled(egui::pos2(x, rect.center().y), r, if *on { t.on_accent } else { t.text });
    response
}

/// Two or more choices side by side, one selected (OrcaSlicer's "Global | Objects").
pub fn segmented(ui: &mut Ui, t: &Tokens, options: &[&str], selected: &mut usize) -> bool {
    let mut changed = false;
    let font = egui::FontId::proportional(12.0);
    let widths: Vec<f32> = options
        .iter()
        .map(|o| ui.painter().layout_no_wrap((*o).to_owned(), font.clone(), t.text).size().x + 16.0)
        .collect();
    let total: f32 = widths.iter().sum();
    let (rect, _) = ui.allocate_exact_size(egui::vec2(total, 20.0), egui::Sense::hover());
    ui.painter().rect_filled(rect, 10.0, t.app_bg);
    ui.painter().rect_stroke(rect, 10.0, Stroke::new(1.0, t.border), egui::StrokeKind::Inside);
    let mut x = rect.left();
    for (i, (option, w)) in options.iter().zip(&widths).enumerate() {
        let r = egui::Rect::from_min_size(egui::pos2(x, rect.top()), egui::vec2(*w, rect.height()));
        let response = ui.interact(r, ui.id().with(("segmented", option)), egui::Sense::click());
        let on = *selected == i;
        if on {
            ui.painter().rect_filled(r.shrink(2.0), 8.0, t.accent);
        } else if response.hovered() {
            ui.painter().rect_filled(r.shrink(2.0), 8.0, t.hover_bg);
        }
        ui.painter().text(
            r.center(),
            egui::Align2::CENTER_CENTER,
            *option,
            font.clone(),
            if on { t.on_accent } else { t.text },
        );
        if response.clicked() && !on {
            *selected = i;
            changed = true;
        }
        x += w;
    }
    changed
}
