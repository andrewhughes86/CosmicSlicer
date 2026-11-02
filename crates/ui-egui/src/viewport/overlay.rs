//! The 2D chrome over the canvas: OrcaSlicer's toolbar, the buttons beside each plate, tool
//! panels, object labels, the Preview legend and layer sliders, and notifications.

use egui::{Align2, Color32, FontId, Pos2, Rect, RichText, Sense, Stroke, Ui, pos2, vec2};
use glam::Vec3;

use super::{Tool, Viewport};
use crate::Action;
use crate::icons;
use crate::state::{Level, Page, Session};
use crate::theme::Tokens;
use crate::ui_kit::{self, Surface};
use CosmicSlicer_model::Transform;
use CosmicSlicer_model::threemf::PaintKind;

use crate::preview::{self, FEATURES, VIEW_TYPES};

pub fn show(vp: &mut Viewport, ui: &mut Ui, rect: Rect, session: &mut Session, t: &Tokens, actions: &mut Vec<Action>) {
    match session.page {
        Page::Prepare => {
            plate_chrome(vp, ui, rect, session, t, actions);
            labels(vp, ui, rect, session, t);
            toolbar(vp, ui, rect, session, t, actions);
            if let Some(tool) = vp.tool {
                tool_panel(vp, ui, rect, session, t, tool, actions);
            }
        }
        Page::Preview => {
            legend(vp, ui, rect, session, t);
            layer_slider(vp, ui, rect, t);
            move_slider(vp, ui, rect, t);
        }
        _ => {}
    }
    notifications(ui, rect, session, t);
}

fn to_screen(vp: &Viewport, rect: Rect, p: Vec3) -> Option<Pos2> {
    let s = vp.camera.project(p, vp.size)?;
    let pos = pos2(rect.left() + s.x, rect.top() + s.y);
    rect.expand(200.0).contains(pos).then_some(pos)
}

// ---- Prepare ----

fn toolbar(vp: &mut Viewport, ui: &mut Ui, rect: Rect, session: &mut Session, t: &Tokens, actions: &mut Vec<Action>) {
    // 21 buttons and 3 separators; they shrink to fit a narrow canvas.
    let side = ((rect.width() - 80.0) / 22.5).clamp(20.0, 34.0);
    let has_objects = !session.model.objects.is_empty();
    let has_selection = !session.selection.is_empty();
    egui::Area::new(egui::Id::new("canvas-toolbar"))
        .pivot(Align2::CENTER_TOP)
        .fixed_pos(pos2(rect.center().x, rect.top() + 8.0))
        .constrain_to(rect)
        .show(ui.ctx(), |ui| {
            ui_kit::frame(Surface::Overlay, t).inner_margin(egui::Margin::symmetric(6, 4)).show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 2.0;
                    if icons::button(ui, "toolbar_open", t, side, false, "Add (Ctrl+I)").clicked() {
                        actions.push(Action::Import);
                    }
                    if icons::button(ui, "toolbar_add_plate", t, side, false, "Add plate").clicked() {
                        session.add_plate();
                        vp.fit_pending = true;
                    }
                    if icons::button_enabled(ui, "toolbar_orient", t, side, false, has_objects, "Auto orient (Q)")
                        .clicked()
                    {
                        session.auto_orient();
                    }
                    if icons::button_enabled(
                        ui,
                        "toolbar_arrange",
                        t,
                        side,
                        false,
                        has_objects,
                        "Arrange all objects (A)",
                    )
                    .clicked()
                    {
                        session.arrange(false);
                    }
                    separator(ui, t, side);
                    if icons::button_enabled(ui, "split_objects", t, side, false, has_selection, "Split to objects")
                        .clicked()
                    {
                        session.split_selected();
                    }
                    if icons::button_enabled(ui, "split_parts", t, side, false, has_selection, "Split to parts")
                        .clicked()
                    {
                        session.split_to_parts();
                    }
                    separator(ui, t, side);
                    let vlh = vp.tool == Some(Tool::VariableLayerHeight);
                    if icons::button_enabled(
                        ui,
                        "toolbar_variable_layer_height",
                        t,
                        side,
                        vlh,
                        has_selection,
                        "Variable layer height",
                    )
                    .clicked()
                    {
                        vp.tool = if vlh { None } else { Some(Tool::VariableLayerHeight) };
                    }
                    separator(ui, t, side);
                    for (tool, icon, name, key) in Tool::GIZMOS {
                        let on = vp.tool == Some(tool);
                        let tip = format!("{name} ({key})");
                        if icons::button_enabled(ui, icon, t, side, on, has_objects, &tip).clicked() {
                            vp.tool = if on { None } else { Some(tool) };
                        }
                    }
                });
            });
        });
}

fn separator(ui: &mut Ui, t: &Tokens, side: f32) {
    let (r, _) = ui.allocate_exact_size(vec2(7.0, side), Sense::hover());
    ui.painter().vline(r.center().x, r.y_range().shrink(6.0), Stroke::new(1.0, t.border));
}

/// "Plate N" under each plate, and its buttons up its right side.
fn plate_chrome(
    vp: &mut Viewport,
    ui: &mut Ui,
    rect: Rect,
    session: &mut Session,
    t: &Tokens,
    actions: &mut Vec<Action>,
) {
    let (lo, size) = session.bed();
    let plates = session.model.plates;
    for p in 0..plates {
        let origin = session.plate_origin(p);
        let current = p == session.plate;
        if let Some(at) = to_screen(vp, rect, origin + Vec3::new(lo.x, lo.y - size.y * 0.03, 0.0)) {
            let text = format!("Plate {}", p + 1);
            let galley = ui.painter().layout_no_wrap(text, FontId::proportional(13.0), t.text);
            let r = Rect::from_min_size(at, galley.size() + vec2(12.0, 6.0));
            ui.painter().rect_filled(r, 4.0, if current { t.accent } else { t.raised_bg.gamma_multiply(0.9) });
            ui.painter().galley(r.min + vec2(6.0, 3.0), galley, if current { t.on_accent } else { t.text });
        }
        let Some(corner) = to_screen(vp, rect, origin + Vec3::new(lo.x + size.x, lo.y + size.y, 0.0)) else { continue };
        let side = 24.0;
        let mut y = corner.y;
        let locked = session.locked.get(p).copied().unwrap_or(false);
        let mut buttons: Vec<(&str, &str)> = Vec::new();
        if plates > 1 {
            buttons.push(("plate_close", "Remove plate"));
        }
        buttons.push(("plate_arrange", "Arrange objects on current plate"));
        buttons.push(("plate_orient", "Auto orient objects on current plate"));
        buttons.push(if locked { ("plate_locked", "Unlock plate") } else { ("plate_unlocked", "Lock plate") });
        buttons.push(("plate_settings", "Customize current plate"));
        for (icon, tip) in buttons {
            let r = Rect::from_min_size(pos2(corner.x + 6.0, y), vec2(side, side));
            y += side + 3.0;
            if !rect.contains_rect(r) {
                continue;
            }
            let response = ui.interact(r, egui::Id::new(("plate-button", p, icon)), Sense::click());
            let fill = if response.hovered() { t.hover_bg } else { t.toolbar_bg.gamma_multiply(0.92) };
            ui.painter().rect_filled(r, 4.0, fill);
            ui.painter().rect_stroke(r, 4.0, Stroke::new(1.0, t.border), egui::StrokeKind::Inside);
            // OrcaSlicer draws these with their own background, in a light and a dark version.
            let variant = if t.dark { format!("{icon}_dark") } else { icon.to_owned() };
            icons::paint_original(ui, r.shrink(1.0), &variant, t);
            if response.on_hover_text(tip).clicked() {
                session.plate = p;
                match icon {
                    "plate_close" => {
                        session.delete_plate(p);
                        vp.fit_pending = true;
                    }
                    "plate_arrange" => session.arrange(false),
                    "plate_orient" => session.auto_orient(),
                    "plate_locked" | "plate_unlocked" => session.toggle_lock(p),
                    _ => actions.push(Action::PlateSettings(p)),
                }
            }
        }
    }
}

fn labels(vp: &Viewport, ui: &Ui, rect: Rect, session: &Session, t: &Tokens) {
    if !session.prefs.show_labels {
        return;
    }
    for o in session.model.objects.iter().filter(|o| o.visible) {
        let b = o.bounds();
        let Some(at) = to_screen(vp, rect, Vec3::new(b.centre().x, b.centre().y, b.max.z)) else { continue };
        let galley = ui.painter().layout_no_wrap(o.name.clone(), FontId::proportional(12.0), t.text);
        let r = Rect::from_center_size(at - vec2(0.0, 14.0), galley.size() + vec2(10.0, 4.0));
        ui.painter().rect_filled(r, 3.0, t.panel_bg.gamma_multiply(0.85));
        ui.painter().galley(r.min + vec2(5.0, 2.0), galley, t.text);
    }
}

fn tool_panel(
    vp: &mut Viewport,
    ui: &mut Ui,
    rect: Rect,
    session: &mut Session,
    t: &Tokens,
    tool: Tool,
    actions: &mut Vec<Action>,
) {
    let mut open = true;
    egui::Area::new(egui::Id::new("tool-panel"))
        .fixed_pos(pos2(rect.left() + 12.0, rect.top() + 64.0))
        .constrain_to(rect)
        .show(ui.ctx(), |ui| {
            ui_kit::frame(Surface::Overlay, t).inner_margin(egui::Margin::same(10)).show(ui, |ui| {
                ui.set_width(250.0);
                ui.horizontal(|ui| {
                    ui.label(RichText::new(tool.name()).strong().size(14.0));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui_kit::close_button(ui, t, "Close (Esc)").clicked() {
                            open = false;
                        }
                    });
                });
                ui.separator();
                let first = session.selection.first().copied();
                match (tool, first) {
                    (Tool::Move | Tool::Rotate | Tool::Scale, None) => {
                        ui.label(RichText::new("Select an object to edit.").color(t.text_muted));
                    }
                    (Tool::Move, Some(id)) => move_fields(ui, session, id),
                    (Tool::Rotate, Some(id)) => rotate_fields(ui, session, id),
                    (Tool::Scale, Some(id)) => scale_fields(ui, session, id, &mut vp.uniform_scale),
                    (Tool::LayOnFace, _) => {
                        ui.label("Click a face of an object to lay it on the plate.");
                    }
                    (tool, _) if tool.paint_kind().is_some() => {
                        if let Some(kind) = tool.paint_kind() {
                            paint_panel(ui, vp, session, t, kind);
                        }
                    }
                    (Tool::Measure, _) => measure_panel(ui, vp, t),
                    (Tool::Cut, None) => {
                        ui.label(RichText::new("Select an object to cut.").color(t.text_muted));
                    }
                    (Tool::Cut, Some(id)) => cut_panel(ui, vp, session, t, id),
                    (Tool::Text, first) => text_panel(ui, vp, session, t, first, actions),
                    (Tool::Boolean, _) => boolean_panel(ui, session, t),
                    (Tool::Assembly, _) => assembly_panel(ui, session, t),
                    (Tool::BrimEars, first) => ears_panel(ui, vp, session, t, first),
                    _ => {
                        // Only variable layer height is left: the layer-height step builds it.
                        ui.label(
                            RichText::new(format!(
                                "{} is built with variable and adaptive layer heights (roadmap session 13).",
                                tool.name()
                            ))
                            .color(t.text_muted),
                        );
                    }
                }
            });
        });
    if !open {
        vp.tool = None;
    }
}

fn paint_panel(ui: &mut Ui, vp: &mut Viewport, session: &mut Session, t: &Tokens, kind: PaintKind) {
    use super::tools::Brush;
    let ts = &mut vp.tools;
    ui.label(RichText::new("Brush").weak());
    ui.horizontal_wrapped(|ui| {
        for (b, name) in Brush::ALL {
            if ui.selectable_label(ts.brush == b, name).clicked() {
                ts.brush = b;
            }
        }
    });
    if ts.brush.drags() {
        ui.add(egui::Slider::new(&mut ts.radius, 0.2..=20.0).text("Radius (mm)").logarithmic(true));
    }
    if ts.brush == Brush::SmartFill {
        ui.add(egui::Slider::new(&mut ts.smart_angle, 0.0..=90.0).text("Angle (°)"));
    }
    ui.add_space(4.0);
    ui.label(RichText::new("Paint").weak());
    ui.horizontal_wrapped(|ui| match kind {
        PaintKind::Color => {
            for f in 1..=session.presets.project_filaments.len().min(16) {
                let colour = super::tools::state_colour(session, kind, f as u8);
                let c = egui::Color32::from(egui::Rgba::from_rgb(colour[0], colour[1], colour[2]));
                let on = ts.state == f as u8;
                let b =
                    egui::Button::new(RichText::new(f.to_string()).color(crate::theme::ink_on(c))).fill(c).selected(on);
                if ui.add(b).clicked() {
                    ts.state = f as u8;
                }
            }
            if ui.selectable_label(ts.state == 0, "Erase").clicked() {
                ts.state = 0;
            }
        }
        PaintKind::Fuzzy => {
            if ui.selectable_label(ts.state != 0, "Fuzzy skin").clicked() {
                ts.state = 1;
            }
            if ui.selectable_label(ts.state == 0, "Erase").clicked() {
                ts.state = 0;
            }
        }
        _ => {
            for (state, name) in [(1u8, "Enforce"), (2, "Block"), (0, "Erase")] {
                if ui.selectable_label(ts.state == state, name).clicked() {
                    ts.state = state;
                }
            }
        }
    });
    // Kinds take different states; keep the one picked valid for this kind.
    if kind == PaintKind::Fuzzy && ts.state > 1 {
        ts.state = 1;
    }
    if kind != PaintKind::Color && ts.state > 2 {
        ts.state = 1;
    }
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        let first = session.selection.first().copied();
        if ui.add_enabled(first.is_some() && ts.state != 0, egui::Button::new("Fill gaps")).clicked()
            && let Some(id) = first
            && let Some(o) = session.model.get(id)
        {
            let tris = super::tools::gaps(o, kind, ts.gap_area);
            if !tris.is_empty() {
                session.checkpoint();
                session.paint(id, kind, &tris, ts.state);
            }
        }
        ui.add(egui::DragValue::new(&mut ts.gap_area).range(0.01..=1000.0).speed(0.1).suffix(" mm²"));
    });
    if ui.button("Clear all").clicked() {
        session.clear_paint(kind);
    }
    let hint = match kind {
        PaintKind::Support => "Supports are added where painted to enforce and left out where blocked.",
        PaintKind::Seam => "Seams go where painted to enforce and stay out of blocked areas.",
        PaintKind::Fuzzy => "Painted walls print with fuzzy skin, by the object's fuzzy skin settings.",
        PaintKind::Color => "Painted areas print in the filament painted.",
    };
    ui.label(RichText::new(hint).weak().small());
    ui.label(
        RichText::new("Drag on the object to paint; drag off it to turn the view, right-drag to pan.")
            .weak()
            .small()
            .color(t.text_muted),
    );
}

fn cut_panel(ui: &mut Ui, vp: &mut Viewport, session: &mut Session, t: &Tokens, id: u64) {
    use crate::state::Connector;
    let Some(o) = session.model.get(id) else { return };
    let cs = &mut vp.tools.cut;
    cs.follow(o);
    let height = o.bounds().size().z;
    ui.add(egui::Slider::new(&mut cs.height, 0.0..=height.max(0.1)).text("Height (mm)"));
    ui.add(egui::Slider::new(&mut cs.tilt[0], -90.0..=90.0).text("Tilt X (°)"));
    ui.add(egui::Slider::new(&mut cs.tilt[1], -90.0..=90.0).text("Tilt Y (°)"));
    ui.separator();
    let opts = &mut cs.opts;
    ui.checkbox(&mut opts.to_parts, "Keep both as parts of one object");
    ui.add_enabled_ui(!opts.to_parts, |ui| {
        ui.checkbox(&mut opts.keep_above, "Keep the top piece");
        ui.checkbox(&mut opts.keep_below, "Keep the bottom piece");
        ui.checkbox(&mut opts.flip_above, "Turn the top piece over (cut face down)");
    });
    ui.separator();
    ui.horizontal(|ui| {
        ui.label("Connectors");
        for (kind, name) in [(Connector::Plug, "Plug"), (Connector::Dowel, "Dowel")] {
            if ui.selectable_label(opts.connector == kind, name).clicked() {
                opts.connector = kind;
            }
        }
    });
    ui.add(egui::Slider::new(&mut opts.connector_radius, 0.5..=20.0).text("Radius (mm)"));
    ui.add(egui::Slider::new(&mut opts.connector_depth, 0.5..=50.0).text("Depth (mm)"));
    ui.add(egui::Slider::new(&mut opts.tolerance, 0.0..=1.0).text("Clearance (mm)"));
    ui.horizontal(|ui| {
        ui.toggle_value(&mut cs.placing, "Place on the plane");
        ui.label(RichText::new(format!("{} placed", cs.opts.connectors.len())).color(t.text_muted));
        if ui.add_enabled(!cs.opts.connectors.is_empty(), egui::Button::new("Clear")).clicked() {
            cs.opts.connectors.clear();
        }
    });
    ui.separator();
    let any = cs.opts.to_parts || cs.opts.keep_above || cs.opts.keep_below;
    if any && ui_kit::primary_button(ui, t, "Cut").clicked() {
        let (origin, normal) = cs.plane(o);
        let opts = cs.opts.clone();
        cs.object = None;
        cs.placing = false;
        session.cut(id, origin, normal, &opts);
    }
}

fn text_panel(
    ui: &mut Ui,
    vp: &mut Viewport,
    session: &mut Session,
    t: &Tokens,
    first: Option<u64>,
    actions: &mut Vec<Action>,
) {
    use super::tools::EmbossMode;
    let ts = &mut vp.tools.text;
    ui.add(egui::TextEdit::multiline(&mut ts.text).desired_rows(2).desired_width(f32::INFINITY));
    ui.horizontal_wrapped(|ui| {
        ui.label("Font");
        for (i, (name, _)) in crate::emboss::FONTS.iter().enumerate() {
            if ui.selectable_label(ts.font == i, *name).clicked() {
                ts.font = i;
            }
        }
        if let Some((name, _)) = &ts.custom {
            let i = crate::emboss::FONTS.len();
            if ui.selectable_label(ts.font == i, name.as_str()).clicked() {
                ts.font = i;
            }
        }
        if ui.small_button("Load font…").clicked() {
            actions.push(Action::PickFont);
        }
    });
    ui.add(egui::Slider::new(&mut ts.height, 1.0..=200.0).text("Height (mm)").logarithmic(true));
    ui.add(egui::Slider::new(&mut ts.depth, 0.2..=20.0).text("Depth (mm)"));
    ui.horizontal_wrapped(|ui| {
        for (mode, name, needs) in [
            (EmbossMode::Object, "New object", false),
            (EmbossMode::Emboss, "Emboss", true),
            (EmbossMode::Engrave, "Engrave", true),
            (EmbossMode::Modifier, "Modifier", true),
        ] {
            let ok = !needs || first.is_some();
            if ui.add_enabled(ok, egui::Button::selectable(ts.mode == mode, name)).clicked() {
                ts.mode = mode;
            }
        }
    });
    if first.is_none() && ts.mode != EmbossMode::Object {
        ts.mode = EmbossMode::Object;
    }
    ui.horizontal(|ui| {
        if ui_kit::primary_button(ui, t, "Add text").clicked() {
            match crate::emboss::text(ts.font_bytes(), &ts.text, ts.height) {
                Ok(loops) => {
                    let name = ts.text.lines().next().unwrap_or("Text").chars().take(24).collect::<String>();
                    let (depth, mode) = (ts.depth, ts.mode);
                    session.add_emboss(&name, &loops, depth, mode, first);
                }
                Err(e) => session.notify(Level::Warning, e),
            }
        }
        if ui.button("Emboss an SVG…").clicked() {
            actions.push(Action::EmbossSvg);
        }
    });
    ui.label(
        RichText::new(
            "Emboss, Engrave and Modifier go on the top of the selected object; move them in its parts list.",
        )
        .weak()
        .small(),
    );
}

fn measure_panel(ui: &mut Ui, vp: &mut Viewport, t: &Tokens) {
    match vp.tools.measure.as_slice() {
        [] => {
            ui.label("Click a point or face on an object.");
        }
        [a] => {
            ui.label("Click a second point or face.");
            ui.label(
                RichText::new(format!("First: {:.2}, {:.2}, {:.2} mm", a.point.x, a.point.y, a.point.z))
                    .small()
                    .color(t.text_muted),
            );
        }
        [a, b, ..] => {
            for line in super::tools::measure_text(a, b) {
                ui.label(line);
            }
        }
    }
    if ui.add_enabled(!vp.tools.measure.is_empty(), egui::Button::new("Clear")).clicked() {
        vp.tools.measure.clear();
    }
}

fn ears_panel(ui: &mut Ui, vp: &mut Viewport, session: &mut Session, t: &Tokens, first: Option<u64>) {
    ui.add(egui::Slider::new(&mut vp.tools.ear_radius, 0.5..=20.0).text("Ear radius (mm)"));
    ui.label("Click an object to put an ear on the plate under that point; Shift-click removes the nearest.");
    let Some(id) = first else { return };
    let count = session.model.get(id).map_or(0, |o| o.brim_ears.len());
    ui.label(RichText::new(format!("{count} ear(s) on this object")).color(t.text_muted));
    let painted =
        session.model.get(id).is_some_and(|o| o.overrides.iter().any(|(k, v)| k == "brim_type" && v == "painted"));
    if !painted {
        ui.label(RichText::new("Ears print when the object's brim type is Painted.").small().color(t.warning));
        if ui.button("Use painted brim for this object").clicked() {
            session.set_override(id, "brim_type", "painted");
        }
    }
    if ui.add_enabled(count > 0, egui::Button::new("Remove all ears")).clicked() {
        session.clear_brim_ears(id);
    }
}

fn field(ui: &mut Ui, label: &str, value: &mut f32, suffix: &str, speed: f64) -> bool {
    ui.horizontal(|ui| {
        ui.add_sized([70.0, 20.0], egui::Label::new(label));
        ui.add_sized([150.0, 22.0], egui::DragValue::new(value).speed(speed).max_decimals(3).suffix(suffix)).changed()
    })
    .inner
}

fn move_fields(ui: &mut Ui, session: &mut Session, id: u64) {
    let Some(o) = session.model.get(id) else { return };
    let b = o.bounds();
    let origin = session.plate_origin(o.plate);
    let mut pos = b.centre() - origin;
    pos.z = b.min.z;
    ui.label(RichText::new("World coordinates (plate)").weak());
    let mut changed = false;
    changed |= field(ui, "X", &mut pos.x, " mm", 0.5);
    changed |= field(ui, "Y", &mut pos.y, " mm", 0.5);
    changed |= field(ui, "Z", &mut pos.z, " mm", 0.5);
    if changed {
        let mut t = o.transform;
        let now = b.centre() - origin;
        t.offset += Vec3::new(pos.x - now.x, pos.y - now.y, pos.z - b.min.z);
        session.set_transform(id, t);
    }
    if ui.button("Drop to bed").clicked() {
        session.drop_to_bed();
    }
}

fn rotate_fields(ui: &mut Ui, session: &mut Session, id: u64) {
    let Some(o) = session.model.get(id) else { return };
    let mut r = o.transform.rotation;
    let mut changed = false;
    changed |= field(ui, "X", &mut r.x, "°", 1.0);
    changed |= field(ui, "Y", &mut r.y, "°", 1.0);
    changed |= field(ui, "Z", &mut r.z, "°", 1.0);
    let mut t = o.transform;
    if changed {
        t.rotation = r;
    }
    if ui.button("Reset rotation").clicked() {
        t.rotation = Vec3::ZERO;
        changed = true;
    }
    if changed {
        session.set_transform(id, t);
        if let Some(o) = session.model.get_mut(id) {
            o.drop_to_bed();
        }
    }
    if ui.button("Drop to bed").clicked() {
        session.drop_to_bed();
    }
    ui.label(RichText::new("Drag a ring to turn; hold Shift for 15° steps.").weak().small());
}

fn scale_fields(ui: &mut Ui, session: &mut Session, id: u64, uniform: &mut bool) {
    let Some(o) = session.model.get(id) else { return };
    let t0: Transform = o.transform;
    let base = o.mesh.bounds().size();
    let mut pct = t0.scale.abs() * 100.0;
    let mut size = base * t0.scale.abs();
    let mut new_scale = None;
    ui.label(RichText::new("Scale").weak());
    for i in 0..3 {
        let name = ["X", "Y", "Z"][i];
        if field(ui, name, &mut pct[i], " %", 0.5) && pct[i] > 0.01 {
            let f = pct[i] / (t0.scale[i].abs() * 100.0);
            new_scale = Some(if *uniform { t0.scale * f } else { with_axis(t0.scale, i, t0.scale[i] * f) });
        }
    }
    ui.label(RichText::new("Size").weak());
    for i in 0..3 {
        let name = ["X", "Y", "Z"][i];
        if field(ui, name, &mut size[i], " mm", 0.5) && size[i] > 0.001 && base[i] > 0.0 {
            let f = size[i] / (base[i] * t0.scale[i].abs());
            new_scale = Some(if *uniform { t0.scale * f } else { with_axis(t0.scale, i, t0.scale[i] * f) });
        }
    }
    ui.checkbox(uniform, "Uniform scale");
    ui.horizontal(|ui| {
        if ui.button("Reset scale").clicked() {
            new_scale = Some(Vec3::new(t0.scale.x.signum(), t0.scale.y.signum(), t0.scale.z.signum()));
        }
        if ui.button("Scale to fit").on_hover_text("As large as the plate and printer height allow").clicked() {
            session.scale_to_fit(id);
        }
        if ui.button("Drop to bed").clicked() {
            session.drop_to_bed();
        }
    });
    if let Some(s) = new_scale {
        let mut t = t0;
        t.scale = s;
        session.set_transform(id, t);
        if let Some(o) = session.model.get_mut(id) {
            o.drop_to_bed();
        }
    }
}

fn boolean_panel(ui: &mut Ui, session: &mut Session, t: &Tokens) {
    let n = session.selection.len();
    ui.label("Select two or more objects; the first selected is kept.");
    ui.label(RichText::new(format!("{n} selected")).color(t.text_muted));
    let ok = n >= 2;
    if ui
        .add_enabled(ok, egui::Button::new("Union"))
        .on_hover_text("Join them into one object of several parts")
        .clicked()
    {
        session.assemble();
    }
    if ui
        .add_enabled(ok, egui::Button::new("Difference"))
        .on_hover_text("Cut the others out of the first: they become its negative volumes")
        .clicked()
    {
        session.subtract();
    }
    ui.add_enabled(false, egui::Button::new("Intersection"))
        .on_disabled_hover_text("Intersection needs true mesh booleans, planned for the hardening step (session 18)");
    ui.label(
        RichText::new(
            "Union and Difference work as the slicer slices parts and negative volumes; the meshes stay as they are.",
        )
        .weak()
        .small(),
    );
}

fn assembly_panel(ui: &mut Ui, session: &mut Session, t: &Tokens) {
    let n = session.selection.len();
    ui.label(RichText::new(format!("{n} selected")).color(t.text_muted));
    if ui
        .add_enabled(n >= 2, egui::Button::new("Assemble"))
        .on_hover_text("Join the selected objects into one object of several parts")
        .clicked()
    {
        session.assemble();
    }
    if ui.add_enabled(n >= 1, egui::Button::new("Split to parts")).clicked() {
        session.split_to_parts();
    }
    if ui.add_enabled(n >= 1, egui::Button::new("Split to objects")).clicked() {
        session.split_selected();
    }
}

fn with_axis(v: Vec3, i: usize, x: f32) -> Vec3 {
    let mut v = v;
    v[i] = x;
    v
}

// ---- Preview ----

fn legend(vp: &mut Viewport, ui: &mut Ui, rect: Rect, session: &Session, t: &Tokens) {
    if vp.hidden_features.len() != FEATURES.len() {
        vp.hidden_features = vec![false; FEATURES.len()];
    }
    let result = session.shown_result();
    let viewing = session.viewer.as_ref().map(|(name, _)| name.clone());
    egui::Area::new(egui::Id::new("preview-legend"))
        .fixed_pos(pos2(rect.left() + 10.0, rect.top() + 10.0))
        .constrain_to(rect)
        .show(ui.ctx(), |ui| {
            ui_kit::frame(Surface::Overlay, t).inner_margin(egui::Margin::same(10)).show(ui, |ui| {
                ui.set_width(330.0);
                ui.set_max_height(rect.height() - 40.0);
                if let Some(name) = &viewing {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(format!("G-code: {name}")).strong());
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.small_button("Close").on_hover_text("Back to the plate's own slice").clicked() {
                                vp.close_viewer = true;
                            }
                        });
                    });
                    ui.separator();
                }
                let current = VIEW_TYPES.get(vp.view_type).copied().unwrap_or("Line Type");
                egui::ComboBox::from_id_salt("legend-view-type").selected_text(current).width(200.0).show_ui(
                    ui,
                    |ui| {
                        for (i, name) in VIEW_TYPES.iter().enumerate() {
                            ui.selectable_value(&mut vp.view_type, i, *name);
                        }
                    },
                );
                ui.add_space(6.0);
                egui::ScrollArea::vertical().max_height((rect.height() - 170.0).max(120.0)).show(ui, |ui| {
                    if vp.view_type == 0 {
                        feature_table(vp, ui, t, result.as_deref());
                    } else if let (Some(range), Some(_)) = (&vp.legend_range, &result) {
                        gradient(ui, t, range);
                    } else if vp.view_type == 9 || vp.view_type == 10 {
                        let n = session.presets.project_filaments.len();
                        for i in 0..n {
                            ui.horizontal(|ui| {
                                let (sw, _) = ui.allocate_exact_size(vec2(12.0, 12.0), Sense::hover());
                                let c = crate::theme::parse_hex(&session.presets.filament_colour(session.schema, i))
                                    .unwrap_or(Color32::GRAY);
                                ui.painter().rect_filled(sw, 2.0, c);
                                ui.label(format!("Filament {}", i + 1));
                            });
                        }
                    } else {
                        ui.label(RichText::new("Shown once the plate is sliced.").color(t.text_muted));
                    }
                });
                ui.separator();
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new("Options").strong());
                    ui.checkbox(&mut vp.show_travel, "Travel");
                    ui.checkbox(&mut vp.show_retractions, "Retractions");
                    ui.checkbox(&mut vp.show_seams, "Seams");
                });
                ui.separator();
                ui.label(RichText::new("Estimation").strong());
                egui::Grid::new("legend-estimate").num_columns(2).spacing([16.0, 2.0]).show(ui, |ui| {
                    let dash = || "—".to_owned();
                    let stats = result.as_ref().map(|r| &r.stats);
                    let row = |ui: &mut Ui, name: &str, value: String| {
                        ui.label(RichText::new(name).weak());
                        ui.label(value);
                        ui.end_row();
                    };
                    row(
                        ui,
                        "Total filament",
                        stats.map_or_else(dash, |s| {
                            let mm: f64 = s.filament_mm.iter().sum();
                            let g: f64 = s.weight_g.iter().sum();
                            format!("{:.2} m  ·  {:.2} g", mm / 1000.0, g)
                        }),
                    );
                    row(ui, "Cost", stats.map_or_else(dash, |s| format!("{:.2}", s.cost.iter().sum::<f64>())));
                    row(ui, "Total time", stats.map_or_else(dash, |s| preview::hms(s.total_time)));
                    row(
                        ui,
                        "Model printing time",
                        stats.map_or_else(dash, |s| preview::hms(s.total_time - s.travel_time)),
                    );
                    row(ui, "Layers", format!("{}", vp.layers.count));
                });
            });
        });
}

/// The "Line Type" table: colour, name, time, share, and a display toggle per feature.
fn feature_table(vp: &mut Viewport, ui: &mut Ui, t: &Tokens, result: Option<&CosmicSlicer_engine::Sliced>) {
    let total: f64 = result.map_or(0.0, |r| r.stats.time_by_role.iter().sum());
    egui::Grid::new("legend-rows").num_columns(4).spacing([10.0, 3.0]).show(ui, |ui| {
        ui.label(RichText::new("Line Type").weak());
        ui.label(RichText::new("Time").weak());
        ui.label(RichText::new("%").weak());
        ui.label(RichText::new("Display").weak());
        ui.end_row();
        for (i, (name, [r, g, b])) in FEATURES.iter().enumerate() {
            let time = result.and_then(|r| r.stats.time_by_role.get(i).copied()).unwrap_or(0.0);
            // Once sliced, only the features the plate has are listed, as in OrcaSlicer.
            if result.is_some() && time <= 0.0 {
                continue;
            }
            let hidden = vp.hidden_features.get(i).copied().unwrap_or(false);
            ui.horizontal(|ui| {
                let (sw, _) = ui.allocate_exact_size(vec2(12.0, 12.0), Sense::hover());
                let c = Color32::from_rgb(*r, *g, *b);
                ui.painter().rect_filled(sw, 2.0, if hidden { c.gamma_multiply(0.25) } else { c });
                ui.label(RichText::new(*name).color(if hidden { t.text_muted } else { t.text }));
            });
            if result.is_some() {
                ui.label(preview::hms(time));
                ui.label(format!("{:.1}%", if total > 0.0 { time / total * 100.0 } else { 0.0 }));
            } else {
                ui.label(RichText::new("—").weak());
                ui.label(RichText::new("—").weak());
            }
            let icon = if hidden { "im_hidden" } else { "im_visible" };
            if icons::button(ui, icon, t, 18.0, false, "").clicked()
                && let Some(h) = vp.hidden_features.get_mut(i)
            {
                *h = !*h;
            }
            ui.end_row();
        }
    });
}

fn gradient(ui: &mut Ui, t: &Tokens, range: &preview::Range) {
    let n = preview::GRADIENT.len();
    for i in (0..n).rev() {
        let f = i as f32 / (n - 1) as f32;
        let v = if range.log {
            let (a, b) = (range.min.max(0.01).ln(), range.max.max(0.01).ln());
            (a + (b - a) * f).exp()
        } else {
            range.min + (range.max - range.min) * f
        };
        let [r, g, b] = preview::GRADIENT[i];
        ui.horizontal(|ui| {
            let (sw, _) = ui.allocate_exact_size(vec2(24.0, 12.0), Sense::hover());
            ui.painter().rect_filled(sw, 0.0, Color32::from_rgb(r, g, b));
            ui.label(RichText::new(format!("{v:.2} {}", range.unit)).color(t.text));
        });
    }
}

/// OrcaSlicer's vertical layer slider: two handles, top and bottom of the shown layers.
fn layer_slider(vp: &mut Viewport, ui: &mut Ui, rect: Rect, t: &Tokens) {
    let count = vp.layers.count;
    if count == 0 {
        return;
    }
    let track = Rect::from_min_max(
        pos2(rect.right() - 46.0, rect.top() + 70.0),
        pos2(rect.right() - 40.0, rect.bottom() - 90.0),
    );
    if track.height() < 60.0 {
        return;
    }
    let painter = ui.painter();
    painter.rect_filled(track, 3.0, t.border);
    let y_of = |n: usize| {
        let f = if count <= 1 { 1.0 } else { (n.saturating_sub(1)) as f32 / (count - 1) as f32 };
        track.bottom() - f * track.height()
    };
    let n_of = |y: f32| {
        let f = ((track.bottom() - y) / track.height()).clamp(0.0, 1.0);
        1 + (f * (count.saturating_sub(1)) as f32).round() as usize
    };
    let (top, bottom) = (vp.layers.top.max(1), vp.layers.bottom.max(1));
    let fill = Rect::from_x_y_ranges(track.x_range(), y_of(top)..=y_of(bottom));
    ui.painter().rect_filled(fill, 3.0, t.accent);
    for (which, n) in [(0usize, top), (1, bottom)] {
        let c = pos2(track.center().x, y_of(n));
        let hit = Rect::from_center_size(c, vec2(26.0, 18.0));
        let response = ui.interact(hit, egui::Id::new(("layer-handle", which)), Sense::drag());
        if response.dragged()
            && let Some(p) = response.interact_pointer_pos()
        {
            let n = n_of(p.y);
            if which == 0 {
                vp.layers.top = n.max(vp.layers.bottom);
            } else {
                vp.layers.bottom = n.min(vp.layers.top);
            }
        }
        let hot = response.hovered() || response.dragged();
        ui.painter().circle_filled(c, if hot { 8.0 } else { 7.0 }, t.accent);
        ui.painter().circle_stroke(c, 7.0, Stroke::new(2.0, t.on_accent));
        if which == 0 || bottom > 1 {
            let label = format!("{n}\n{:.2}", vp.layers.z_of(n));
            let galley = ui.painter().layout_no_wrap(label, FontId::proportional(11.5), t.text);
            let r = Rect::from_min_size(
                pos2(c.x - 18.0 - galley.size().x - 10.0, c.y - galley.size().y / 2.0 - 3.0),
                galley.size() + vec2(10.0, 6.0),
            );
            ui.painter().rect_filled(r, 4.0, t.toolbar_bg);
            ui.painter().rect_stroke(
                r,
                4.0,
                Stroke::new(1.0, if hot { t.accent } else { t.border }),
                egui::StrokeKind::Inside,
            );
            ui.painter().galley(r.min + vec2(5.0, 3.0), galley, t.text);
        }
    }
    // The wheel over the slider steps the top layer, as in OrcaSlicer.
    let area = track.expand2(vec2(30.0, 10.0));
    if ui.rect_contains_pointer(area) {
        let scroll: f32 = ui.input(|i| {
            i.raw
                .events
                .iter()
                .filter_map(|e| if let egui::Event::MouseWheel { delta, .. } = e { Some(delta.y) } else { None })
                .sum()
        });
        if scroll > 0.0 {
            vp.layers.top = (vp.layers.top + 1).min(count);
        } else if scroll < 0.0 {
            vp.layers.top = vp.layers.top.saturating_sub(1).max(vp.layers.bottom);
        }
    }
}

/// The horizontal slider through the moves of the top layer.
fn move_slider(vp: &mut Viewport, ui: &mut Ui, rect: Rect, t: &Tokens) {
    if vp.layers.count == 0 {
        return;
    }
    let width = (rect.width() * 0.55).clamp(200.0, 900.0);
    let track = Rect::from_center_size(pos2(rect.center().x, rect.bottom() - 34.0), vec2(width, 6.0));
    ui.painter().rect_filled(track, 3.0, t.border);
    let x = egui::lerp(track.x_range(), vp.moves.clamp(0.0, 1.0));
    let filled = Rect::from_min_max(track.min, pos2(x, track.max.y));
    ui.painter().rect_filled(filled, 3.0, t.accent);
    let c = pos2(x, track.center().y);
    let response =
        ui.interact(Rect::from_center_size(c, vec2(18.0, 24.0)), egui::Id::new("move-handle"), Sense::drag());
    if response.dragged()
        && let Some(p) = response.interact_pointer_pos()
    {
        vp.moves = ((p.x - track.left()) / track.width()).clamp(0.0, 1.0);
    }
    ui.painter().circle_filled(c, 7.0, t.accent);
    ui.painter().circle_stroke(c, 7.0, Stroke::new(2.0, t.on_accent));
    response.on_hover_text("Moves of the top layer");
}

// ---- notifications ----

fn notifications(ui: &mut Ui, rect: Rect, session: &mut Session, t: &Tokens) {
    // Information fades after a while; warnings and errors stay until closed.
    let now = session.now;
    session.notices.retain(|n| n.level != Level::Info || now - n.at < 10.0);
    if let Some((fraction, stage)) = session.slicing.clone() {
        let mut cancel = false;
        egui::Area::new(egui::Id::new("slicing-progress"))
            .pivot(Align2::RIGHT_BOTTOM)
            .fixed_pos(pos2(rect.right() - 70.0, rect.bottom() - 60.0 - 56.0 * session.notices.len() as f32))
            .constrain_to(rect)
            .show(ui.ctx(), |ui| {
                ui_kit::frame(Surface::Overlay, t).show(ui, |ui| {
                    ui.set_width(400.0);
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(&stage).strong());
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.button("Cancel").clicked() {
                                cancel = true;
                            }
                        });
                    });
                    ui.add(egui::ProgressBar::new(fraction).show_percentage().desired_width(390.0));
                });
            });
        if cancel {
            session.cancel_slice = true;
        }
    }
    if session.notices.is_empty() {
        return;
    }
    let mut close = None;
    egui::Area::new(egui::Id::new("notifications"))
        .pivot(Align2::RIGHT_BOTTOM)
        .fixed_pos(pos2(rect.right() - 70.0, rect.bottom() - 60.0))
        .constrain_to(rect)
        .show(ui.ctx(), |ui| {
            ui.set_max_width(420.0);
            for (i, n) in session.notices.iter().enumerate() {
                let colour = match n.level {
                    Level::Info => t.accent,
                    Level::Warning => t.warning,
                    Level::Error => t.error,
                };
                ui_kit::frame(Surface::Overlay, t).show(ui, |ui| {
                    ui.set_width(400.0);
                    ui.horizontal(|ui| {
                        let (bar, _) = ui.allocate_exact_size(vec2(4.0, 30.0), Sense::hover());
                        ui.painter().rect_filled(bar, 2.0, colour);
                        ui.add(egui::Label::new(RichText::new(&n.text).color(t.text)).wrap());
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui_kit::close_button(ui, t, "Close").clicked() {
                                close = Some(i);
                            }
                        });
                    });
                });
                ui.add_space(4.0);
            }
        });
    if let Some(i) = close
        && i < session.notices.len()
    {
        session.notices.remove(i);
    }
}
