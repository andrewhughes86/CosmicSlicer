//! OrcaSlicer's top bar (the window's title bar: main menu, File, save / undo / redo, the
//! project name and the window buttons) and the tab bar under it (the pages, and the
//! Slice / Print buttons).

use egui::{Align2, Color32, FontId, Id, Rect, Response, RichText, Sense, Stroke, Ui, ViewportCommand, pos2, vec2};

use crate::Action;
use crate::icons;
use crate::menus;
use crate::state::{Page, Session};
use crate::theme::Tokens;
use crate::ui_kit::{self, Surface};

const TITLE_HEIGHT: f32 = 34.0;
const TAB_HEIGHT: f32 = 40.0;
const WINDOW_BUTTON: f32 = 44.0;

/// What the Slice and Print buttons do, as chosen in their dropdowns.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SliceChoices {
    pub slice_all: bool,
    /// 0 print plate, 1 print all, 2 export G-code file, 3 send.
    pub print: usize,
}

const PRINT_CHOICES: [&str; 4] = ["Print plate", "Print all", "Export G-code file", "Send"];

pub fn show(ui: &mut Ui, session: &mut Session, t: &Tokens, choices: &mut SliceChoices, actions: &mut Vec<Action>) {
    egui::Panel::top("title-bar")
        .resizable(false)
        .frame(ui_kit::frame(Surface::TopBar, t))
        .exact_size(TITLE_HEIGHT)
        .show(ui, |ui| {
            title_bar(ui, session, t, actions);
        });
    egui::Panel::top("tab-bar").resizable(false).frame(ui_kit::frame(Surface::TabBar, t)).exact_size(TAB_HEIGHT).show(
        ui,
        |ui| {
            tab_bar(ui, session, t, choices, actions);
        },
    );
}

fn title_bar(ui: &mut Ui, session: &mut Session, t: &Tokens, actions: &mut Vec<Action>) {
    let (strip, background) = ui.allocate_exact_size(vec2(ui.available_width(), TITLE_HEIGHT), Sense::click_and_drag());
    let right = window_controls(ui, strip, t, &background);
    let mut x = strip.left() + 8.0;
    let side = TITLE_HEIGHT - 8.0;
    let button = |ui: &mut Ui, x: &mut f32, icon: &str, enabled: bool, tip: &str, id: &str| -> Response {
        let r = Rect::from_min_size(pos2(*x, strip.top() + 4.0), vec2(side, side));
        *x += side + 4.0;
        let response =
            ui.interact(r, Id::new(("title-button", id)), if enabled { Sense::click() } else { Sense::hover() });
        if response.hovered() && enabled {
            ui.painter().rect_filled(r, 4.0, t.hover_bg);
        }
        icons::paint(ui, r.shrink(4.0), icon, t, enabled);
        response.on_hover_text(tip)
    };
    // The logo opens the main menu, as in OrcaSlicer.
    let logo = button(ui, &mut x, "cosmic_logo", true, "Menu", "logo");
    ui_kit::menu_below(&logo, t, |ui| {
        ui.set_min_width(200.0);
        ui.menu_button(crate::i18n::tr("Edit"), |ui| {
            ui.set_min_width(240.0);
            menus::edit_menu(ui, session, actions);
        });
        ui.menu_button(crate::i18n::tr("Window"), |ui| {
            ui.set_min_width(240.0);
            menus::window_menu(ui, session, actions);
        });
        ui.menu_button(crate::i18n::tr("View"), |ui| {
            ui.set_min_width(240.0);
            menus::view_menu(ui, session, actions);
        });
        ui.menu_button(crate::i18n::tr("Calibration"), |ui| {
            ui.set_min_width(220.0);
            menus::calibration_menu(ui, actions);
        });
        ui.menu_button(crate::i18n::tr("Help"), |ui| {
            ui.set_min_width(220.0);
            menus::help_menu(ui, actions);
        });
        ui.separator();
        if ui_kit::menu_item(ui, "Preferences", &menus::ctrl("P"), true) {
            actions.push(Action::Preferences);
        }
    });
    // "File ▾"
    let file_rect = Rect::from_min_size(pos2(x, strip.top() + 4.0), vec2(58.0, side));
    let file = ui.interact(file_rect, Id::new("title-file"), Sense::click());
    if file.hovered() || egui::Popup::is_id_open(ui.ctx(), egui::Popup::default_response_id(&file)) {
        ui.painter().rect_filled(file_rect, 4.0, t.hover_bg);
    }
    ui.painter().text(
        file_rect.left_center() + vec2(10.0, 0.0),
        Align2::LEFT_CENTER,
        crate::i18n::tr("File"),
        FontId::proportional(13.5),
        t.text,
    );
    icons::paint(
        ui,
        Rect::from_center_size(file_rect.right_center() - vec2(12.0, 0.0), vec2(12.0, 12.0)),
        "topbar_dropdown",
        t,
        true,
    );
    ui_kit::menu_below(&file, t, |ui| {
        ui.set_min_width(260.0);
        menus::file_menu(ui, session, actions);
    });
    x = file_rect.right() + 8.0;
    if button(ui, &mut x, "topbar_save", true, &format!("Save ({})", menus::ctrl("S")), "save").clicked() {
        actions.push(Action::SaveProject);
    }
    if button(ui, &mut x, "topbar_undo", session.can_undo(), &format!("Undo ({})", menus::ctrl("Z")), "undo").clicked()
    {
        session.undo();
    }
    if button(ui, &mut x, "topbar_redo", session.can_redo(), &format!("Redo ({})", menus::ctrl("Y")), "redo").clicked()
    {
        session.redo();
    }
    x += 4.0;
    ui.painter().vline(x, strip.y_range().shrink(9.0), Stroke::new(1.0, t.border));
    x += 8.0;
    let calib = Rect::from_min_size(pos2(x, strip.top() + 4.0), vec2(96.0, side));
    let calib_r = ui.interact(calib, Id::new("title-calibration"), Sense::click());
    if calib_r.hovered() || egui::Popup::is_id_open(ui.ctx(), egui::Popup::default_response_id(&calib_r)) {
        ui.painter().rect_filled(calib, 4.0, t.hover_bg);
    }
    ui.painter().text(
        calib.left_center() + vec2(10.0, 0.0),
        Align2::LEFT_CENTER,
        crate::i18n::tr("Calibration"),
        FontId::proportional(13.5),
        t.text,
    );
    ui_kit::menu_below(&calib_r, t, |ui| {
        ui.set_min_width(220.0);
        menus::calibration_menu(ui, actions);
    });

    // The project name in the middle.
    let title = format!("{} — CosmicSlicer", session.title());
    let galley = ui.painter().layout_no_wrap(title, FontId::proportional(13.0), t.text_muted);
    // Centred when there is room, else just right of the menus (`clamp` would panic when a
    // long name leaves no room).
    let lo = calib.right() + galley.size().x / 2.0 + 20.0;
    let hi = right - galley.size().x / 2.0 - 20.0;
    let centre = if hi >= lo { strip.center().x.clamp(lo, hi) } else { lo };
    ui.painter().galley(
        pos2(centre - galley.size().x / 2.0, strip.center().y - galley.size().y / 2.0),
        galley,
        t.text_muted,
    );
}

/// Minimise, maximise and close; the empty bar drags the window. Returns the left edge.
fn window_controls(ui: &Ui, strip: Rect, t: &Tokens, background: &Response) -> f32 {
    let ctx = ui.ctx();
    let maximized = ctx.input(|i| i.viewport().maximized.unwrap_or(false));
    if background.double_clicked() {
        ctx.send_viewport_cmd(ViewportCommand::Maximized(!maximized));
    } else if background.drag_started() {
        ctx.send_viewport_cmd(ViewportCommand::StartDrag);
    }
    let painter = ui.painter_at(strip);
    let ink = Stroke::new(1.2, t.text);
    let mut x = strip.right();
    for button in ["close", "maximize", "minimize"] {
        let rect = Rect::from_min_max(pos2(x - WINDOW_BUTTON, strip.top()), pos2(x, strip.bottom()));
        x = rect.left();
        let response = ui.interact(rect, Id::new(("window-control", button)), Sense::click());
        if response.hovered() {
            painter.rect_filled(rect, 0.0, if button == "close" { t.error } else { t.hover_bg });
        }
        let c = rect.center();
        let h = 5.0;
        match button {
            "close" => {
                let ink = if response.hovered() { Stroke::new(1.2, Color32::WHITE) } else { ink };
                painter.line_segment([c + vec2(-h, -h), c + vec2(h, h)], ink);
                painter.line_segment([c + vec2(-h, h), c + vec2(h, -h)], ink);
            }
            "maximize" if maximized => {
                painter.rect_stroke(
                    Rect::from_min_max(c + vec2(-h, -h + 2.0), c + vec2(h - 2.0, h)),
                    0.0,
                    ink,
                    egui::StrokeKind::Middle,
                );
                painter.line_segment([c + vec2(-h + 2.0, -h), c + vec2(h, -h)], ink);
                painter.line_segment([c + vec2(h, -h), c + vec2(h, h - 2.0)], ink);
            }
            "maximize" => {
                painter.rect_stroke(
                    Rect::from_center_size(c, vec2(2.0 * h, 2.0 * h)),
                    0.0,
                    ink,
                    egui::StrokeKind::Middle,
                );
            }
            _ => {
                painter.line_segment([c + vec2(-h, 0.0), c + vec2(h, 0.0)], ink);
            }
        }
        let response = response.on_hover_text(match button {
            "close" => "Close",
            "maximize" if maximized => "Restore",
            "maximize" => "Maximize",
            _ => "Minimize",
        });
        if response.clicked() {
            ctx.send_viewport_cmd(match button {
                "close" => ViewportCommand::Close,
                "maximize" => ViewportCommand::Maximized(!maximized),
                _ => ViewportCommand::Minimized(true),
            });
        }
    }
    x
}

/// Lets the window, which has no system frame, be resized from its edges and corners.
pub fn resize_edges(ctx: &egui::Context) {
    use egui::{CursorIcon, ResizeDirection};
    const GRIP: f32 = 5.0;
    let (maximized, pointer, pressed, screen) = ctx.input(|i| {
        (
            i.viewport().maximized.unwrap_or(false) || i.viewport().fullscreen.unwrap_or(false),
            i.pointer.hover_pos(),
            i.pointer.primary_pressed(),
            i.content_rect(),
        )
    });
    let Some(p) = pointer.filter(|_| !maximized) else { return };
    let (w, e) = (p.x - screen.left() < GRIP, screen.right() - p.x < GRIP);
    let (n, s) = (p.y - screen.top() < GRIP, screen.bottom() - p.y < GRIP);
    let (direction, cursor) = match (n, s, w, e) {
        (true, _, true, _) => (ResizeDirection::NorthWest, CursorIcon::ResizeNorthWest),
        (true, _, _, true) => (ResizeDirection::NorthEast, CursorIcon::ResizeNorthEast),
        (_, true, true, _) => (ResizeDirection::SouthWest, CursorIcon::ResizeSouthWest),
        (_, true, _, true) => (ResizeDirection::SouthEast, CursorIcon::ResizeSouthEast),
        (true, ..) => (ResizeDirection::North, CursorIcon::ResizeNorth),
        (_, true, ..) => (ResizeDirection::South, CursorIcon::ResizeSouth),
        (_, _, true, _) => (ResizeDirection::West, CursorIcon::ResizeWest),
        (_, _, _, true) => (ResizeDirection::East, CursorIcon::ResizeEast),
        _ => return,
    };
    ctx.set_cursor_icon(cursor);
    if pressed {
        ctx.send_viewport_cmd(ViewportCommand::BeginResize(direction));
    }
}

fn tab_bar(ui: &mut Ui, session: &mut Session, t: &Tokens, choices: &mut SliceChoices, actions: &mut Vec<Action>) {
    let strip = ui.available_rect_before_wrap();
    let mut pages: Vec<(Page, &str, &str)> = vec![
        (Page::Home, "", "tab_home_active"),
        (Page::Prepare, "Prepare", "tab_3d_active"),
        (Page::Preview, "Preview", "tab_preview_active"),
        (Page::Device, "Device", "tab_monitor_active"),
    ];
    if session.prefs.multi_device {
        pages.push((Page::MultiDevice, "Multi-device", "tab_multi_active"));
    }
    pages.push((Page::Project, "Project", "tab_auxiliary_active"));
    let mut x = strip.left();
    let font = FontId::proportional(14.0);
    for (page, label, icon) in pages {
        let label = crate::i18n::tr(label);
        let label = label.as_str();
        let text_w = if label.is_empty() {
            0.0
        } else {
            ui.painter().layout_no_wrap(label.to_owned(), font.clone(), t.text).size().x + 8.0
        };
        let r = Rect::from_min_size(pos2(x, strip.top()), vec2(text_w + 46.0, TAB_HEIGHT));
        x = r.right() + 2.0;
        let response = ui.interact(r, Id::new(("page-tab", label, icon)), Sense::click());
        let on = session.page == page;
        if on {
            ui.painter().rect_filled(r.shrink2(vec2(0.0, 5.0)), 6.0, t.accent.gamma_multiply(0.18));
            ui.painter().hline(r.x_range().shrink(8.0), r.bottom() - 2.0, Stroke::new(3.0, t.accent));
        } else if response.hovered() {
            ui.painter().rect_filled(r.shrink2(vec2(0.0, 5.0)), 6.0, t.hover_bg);
        }
        let ink = if on { t.accent } else { t.text };
        let icon_r = Rect::from_center_size(pos2(r.left() + 23.0, r.center().y), vec2(20.0, 20.0));
        tinted_icon(ui, icon_r, icon, t, ink);
        if !label.is_empty() {
            ui.painter().text(pos2(r.left() + 38.0, r.center().y), Align2::LEFT_CENTER, label, font.clone(), ink);
        }
        let tip = if label.is_empty() { "Home" } else { label };
        if response.on_hover_text(tip).clicked() {
            if page == Page::Preview && session.page != Page::Preview && session.sliced.is_empty() {
                actions.push(Action::Slice);
            }
            session.page = page;
        }
    }

    // Slice and Print, at the right.
    let right = strip.right() - 6.0;
    let h = 30.0;
    let y = strip.center().y - h / 2.0;
    let print_label = PRINT_CHOICES.get(choices.print).copied().unwrap_or("Print plate");
    let slice_label = if choices.slice_all { "Slice all" } else { "Slice plate" };
    // As wide as the label needs (translations run longer), at least Orca's 150.
    let fit = |label: &str| {
        let font = egui::FontId::proportional(14.0);
        let w = ui.painter().layout_no_wrap(crate::i18n::tr(label), font, t.text).size().x;
        (w + 56.0).clamp(150.0, 320.0)
    };
    let (print_w, slice_w) = (fit(print_label), fit(slice_label));
    let print_r = Rect::from_min_size(pos2(right - print_w, y), vec2(print_w, h));
    let slice_r = Rect::from_min_size(pos2(print_r.left() - slice_w - 10.0, y), vec2(slice_w, h));
    let can_slice = !session.model.objects.is_empty();
    let slice_action = if choices.slice_all { Action::SliceAll } else { Action::Slice };
    if let Some(dropdown) = split_button(ui, t, slice_r, slice_label, "slice", true, can_slice, actions, slice_action) {
        ui_kit::menu_below(&dropdown, t, |ui| {
            if ui.selectable_label(choices.slice_all, "Slice all").clicked() {
                choices.slice_all = true;
                ui.close();
            }
            if ui.selectable_label(!choices.slice_all, "Slice plate").clicked() {
                choices.slice_all = false;
                ui.close();
            }
        });
    }
    let print_action = match choices.print {
        2 => Action::ExportGcode,
        _ => Action::Print,
    };
    if let Some(dropdown) = split_button(ui, t, print_r, print_label, "print", false, can_slice, actions, print_action)
    {
        ui_kit::menu_below(&dropdown, t, |ui| {
            for (i, name) in PRINT_CHOICES.iter().enumerate() {
                if ui.selectable_label(choices.print == i, *name).clicked() {
                    choices.print = i;
                    ui.close();
                }
            }
        });
    }
}

/// A button with a dropdown arrow part; returns the arrow's response for its menu.
#[allow(clippy::too_many_arguments)]
fn split_button(
    ui: &mut Ui,
    t: &Tokens,
    r: Rect,
    label: &str,
    id: &str,
    primary: bool,
    enabled: bool,
    actions: &mut Vec<Action>,
    action: Action,
) -> Option<Response> {
    let arrow_w = 28.0;
    let main = Rect::from_min_max(r.min, pos2(r.right() - arrow_w, r.bottom()));
    let arrow = Rect::from_min_max(pos2(main.right(), r.top()), r.max);
    let main_resp =
        ui.interact(main, Id::new(("split-main", id)), if enabled { Sense::click() } else { Sense::hover() });
    let arrow_resp = ui.interact(arrow, Id::new(("split-arrow", id)), Sense::click());
    let (fill, ink) = if primary { (t.accent, t.on_accent) } else { (t.raised_bg, t.text) };
    let fill = if !enabled { fill.gamma_multiply(0.45) } else { fill };
    ui.painter().rect_filled(r, 15.0, fill);
    if !primary {
        ui.painter().rect_stroke(r, 15.0, Stroke::new(1.0, t.accent), egui::StrokeKind::Inside);
    }
    if enabled && main_resp.hovered() {
        ui.painter().rect_filled(
            main,
            egui::CornerRadius { nw: 15, sw: 15, ne: 0, se: 0 },
            Color32::WHITE.gamma_multiply(0.12),
        );
    }
    if arrow_resp.hovered() {
        ui.painter().rect_filled(
            arrow,
            egui::CornerRadius { nw: 0, sw: 0, ne: 15, se: 15 },
            Color32::WHITE.gamma_multiply(0.12),
        );
    }
    ui.painter().vline(arrow.left(), arrow.y_range().shrink(7.0), Stroke::new(1.0, ink.gamma_multiply(0.5)));
    ui.painter().text(
        main.center(),
        Align2::CENTER_CENTER,
        crate::i18n::tr(label),
        FontId::proportional(14.0),
        if enabled { ink } else { ink.gamma_multiply(0.6) },
    );
    let c = arrow.center();
    ui.painter().add(egui::Shape::convex_polygon(
        vec![c + vec2(-4.0, -2.0), c + vec2(4.0, -2.0), c + vec2(0.0, 3.0)],
        ink,
        Stroke::NONE,
    ));
    if main_resp.clicked() {
        actions.push(action);
    }
    if !enabled {
        main_resp.on_hover_text(RichText::new("Add an object first"));
    }
    Some(arrow_resp)
}

/// Draws a white OrcaSlicer tab icon tinted with `ink`.
fn tinted_icon(ui: &Ui, r: Rect, name: &str, t: &Tokens, ink: Color32) {
    // The tab icons are white line art; drawing them through the icon cache recolours
    // white to the text colour, and the tint pulls that towards the accent when active.
    let tint = if ink == t.text { Color32::WHITE } else { ink };
    let ctx = ui.ctx();
    let px = (r.width() * ctx.pixels_per_point()).round() as u32;
    let cache_id = Id::new("icon-cache");
    let mut cache = ctx.data_mut(|d| std::mem::take(d.get_temp_mut_or_default::<icons::IconCache>(cache_id)));
    // Tinting multiplies, so render the icon in white for an exact accent.
    let white = Tokens { text: Color32::WHITE, text_muted: Color32::WHITE, ..*t };
    let tex = cache.get(ctx, name, if tint == Color32::WHITE { t } else { &white }, px, true);
    ctx.data_mut(|d| d.insert_temp(cache_id, cache));
    match tex {
        Some(id) => {
            ui.painter().image(id, r, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), tint);
        }
        None => {
            ui.painter().circle_stroke(r.center(), r.width() / 3.0, Stroke::new(1.5, ink));
        }
    }
}
