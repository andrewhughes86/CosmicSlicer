//! The sidebar of Prepare and Preview, as in OrcaSlicer: Printer, Filament, the object list
//! and the Process settings.

use egui::{Align, Color32, Layout, RichText, Sense, Stroke, Ui, vec2};

use CosmicSlicer_config::{Preset, TabKind};

use crate::Action;
use crate::icons;
use crate::settings::{self, Target};
use crate::state::{BED_TYPES, Session, Sub};
use crate::theme::{Tokens, ink_on, parse_hex, to_hex};
use crate::ui_kit;
use CosmicSlicer_model::PartKind;

/// Sidebar state that is not part of the project.
#[derive(Default)]
pub struct Sidebar {
    pub process_page: usize,
    pub search: String,
    /// Ctrl+F: put the cursor in the settings search.
    pub focus_search: bool,
    /// 0 Global, 1 Objects.
    pub scope: usize,
    pub printer_open: bool,
    pub filament_open: bool,
    pub objects_open: bool,
    /// An object being renamed in the list, and the new name so far.
    pub renaming: Option<(u64, String)>,
    /// The volume or height range picked in the object list, for its own settings.
    pub sub: Option<(u64, Sub)>,
    /// Objects whose parts are folded away.
    pub folded: Vec<u64>,
}

/// The icon OrcaSlicer's object list shows for a volume of `kind`.
pub fn kind_icon(kind: PartKind) -> &'static str {
    match kind {
        PartKind::Normal => "menu_add_part",
        PartKind::Modifier => "menu_add_modifier",
        PartKind::Negative => "menu_add_negative",
        PartKind::SupportEnforcer => "menu_support_enforcer",
        PartKind::SupportBlocker => "menu_support_blocker",
    }
}

impl Sidebar {
    pub fn new() -> Self {
        Self { printer_open: true, filament_open: true, objects_open: true, ..Default::default() }
    }
}

const HEADER_HEIGHT: f32 = 34.0;

/// A section title bar: icon, title, and buttons drawn by `extra` at its right.
fn header(ui: &mut Ui, t: &Tokens, icon: &str, title: &str, open: &mut bool, extra: impl FnOnce(&mut Ui)) {
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), HEADER_HEIGHT), Sense::click());
    ui.painter().rect_filled(rect, 0.0, t.raised_bg);
    ui.painter().hline(rect.x_range(), rect.bottom() - 0.5, Stroke::new(1.0, t.border));
    let mut child = ui.new_child(
        egui::UiBuilder::new().max_rect(rect.shrink2(vec2(10.0, 0.0))).layout(Layout::left_to_right(Align::Center)),
    );
    let (r, _) = child.allocate_exact_size(vec2(20.0, 20.0), Sense::hover());
    icons::paint(&child, r, icon, t, true);
    child.label(RichText::new(crate::i18n::tr(title)).strong().size(14.0).color(t.text));
    child.with_layout(Layout::right_to_left(Align::Center), |ui| {
        let arrow = if *open { "⏷" } else { "⏵" };
        ui.label(RichText::new(arrow).color(t.text_muted));
        extra(ui);
    });
    if response.clicked() {
        *open = !*open;
    }
}

fn preset_label(p: &Preset, modified: bool) -> String {
    if modified { format!("{} (modified)", p.name) } else { p.name.clone() }
}

/// A preset dropdown grouped by vendor; returns the chosen index.
pub fn preset_combo(
    ui: &mut Ui,
    id: impl std::hash::Hash + std::fmt::Debug,
    list: &[Preset],
    current: usize,
    modified: bool,
    width: f32,
) -> Option<usize> {
    let all: Vec<usize> = (0..list.len()).collect();
    match preset_combo_filtered(ui, id, list, current, modified, width, &all, None) {
        Some(Pick::Preset(i)) => Some(i),
        _ => None,
    }
}

/// What a preset dropdown returned.
pub enum Pick {
    Preset(usize),
    /// The entry at the bottom ("Add/Remove printers…").
    Footer,
}

/// A preset dropdown showing only `shown` (user presets first, then by vendor), with an
/// optional `footer` entry.
#[allow(clippy::too_many_arguments)]
pub fn preset_combo_filtered(
    ui: &mut Ui,
    id: impl std::hash::Hash + std::fmt::Debug,
    list: &[Preset],
    current: usize,
    modified: bool,
    width: f32,
    shown: &[usize],
    footer: Option<&str>,
) -> Option<Pick> {
    let text = list.get(current).map(|p| preset_label(p, modified)).unwrap_or_default();
    let mut chosen = None;
    let mut order: Vec<usize> = shown.to_vec();
    order.sort_by_key(|&i| list.get(i).is_some_and(|p| p.system));
    egui::ComboBox::from_id_salt(id).selected_text(text).width(width).height(460.0).show_ui(ui, |ui| {
        let mut vendor = "";
        for i in order {
            let Some(p) = list.get(i) else { continue };
            if p.vendor != vendor {
                vendor = &p.vendor;
                ui.label(RichText::new(vendor).small().weak());
            }
            if ui.selectable_label(i == current, format!("   {}", p.name)).clicked() {
                chosen = Some(Pick::Preset(i));
            }
        }
        if let Some(label) = footer {
            ui.separator();
            if ui.selectable_label(false, format!("   {label}")).clicked() {
                chosen = Some(Pick::Footer);
            }
        }
    });
    match chosen {
        Some(Pick::Preset(i)) if i == current => None,
        other => other,
    }
}

/// A filament or process dropdown listing what suits the selected printer.
#[allow(clippy::too_many_arguments)]
fn preset_combo_compatible(
    ui: &mut Ui,
    id: impl std::hash::Hash + std::fmt::Debug,
    session: &Session,
    kind: TabKind,
    current: usize,
    modified: bool,
    width: f32,
    actions: &mut Vec<Action>,
) -> Option<usize> {
    let shown = session.presets.compatible(kind);
    let footer = (kind == TabKind::Filament).then_some("Add/Remove filaments…");
    match preset_combo_filtered(ui, id, session.presets.list(kind), current, modified, width, &shown, footer) {
        Some(Pick::Preset(i)) => Some(i),
        Some(Pick::Footer) => {
            actions.push(Action::Wizard(true));
            None
        }
        None => None,
    }
}

pub fn show(ui: &mut Ui, sb: &mut Sidebar, session: &mut Session, t: &Tokens, actions: &mut Vec<Action>) {
    ui.spacing_mut().item_spacing.y = 4.0;
    printer(ui, sb, session, t, actions);
    filaments(ui, sb, session, t, actions);
    objects(ui, sb, session, t, actions);
    process(ui, sb, session, t, actions);
}

fn body(ui: &mut Ui, add: impl FnOnce(&mut Ui)) {
    egui::Frame::new().inner_margin(egui::Margin::symmetric(10, 8)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        add(ui);
    });
}

fn printer(ui: &mut Ui, sb: &mut Sidebar, session: &mut Session, t: &Tokens, actions: &mut Vec<Action>) {
    let mut open = sb.printer_open;
    header(ui, t, "printer", "Printer", &mut open, |ui| {
        if icons::button(ui, "edit", t, 22.0, false, "Click to edit preset").clicked() {
            actions.push(Action::Settings(TabKind::Printer));
        }
        if icons::button(ui, "monitor_printer", t, 22.0, false, "Connection").clicked() {
            actions.push(Action::PhysicalPrinter);
        }
    });
    sb.printer_open = open;
    if !open {
        return;
    }
    body(ui, |ui| {
        ui.horizontal(|ui| {
            let (r, _) = ui.allocate_exact_size(vec2(58.0, 58.0), Sense::hover());
            ui.painter().rect_filled(r, 6.0, t.raised_bg);
            icons::paint(ui, r.shrink(6.0), "printer_thumbnail", t, true);
            ui.vertical(|ui| {
                let width = ui.available_width() - 4.0;
                let slot = session.presets.printer.clone();
                let all: Vec<usize> = (0..session.presets.printers.len()).collect();
                match preset_combo_filtered(
                    ui,
                    "printer-preset",
                    &session.presets.printers,
                    slot.preset,
                    slot.is_modified(),
                    width,
                    &all,
                    Some("Add/Remove printers…"),
                ) {
                    Some(Pick::Preset(i)) => actions.push(Action::SelectPreset(TabKind::Printer, 0, i)),
                    Some(Pick::Footer) => actions.push(Action::Wizard(false)),
                    None => {}
                }
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Plate type").color(t.text_muted));
                    let current = BED_TYPES.get(session.bed_type).map_or("", |b| b.0);
                    egui::ComboBox::from_id_salt("bed-type")
                        .selected_text(current)
                        .width(ui.available_width() - 4.0)
                        .show_ui(ui, |ui| {
                            for (i, (name, _)) in BED_TYPES.iter().enumerate() {
                                ui.selectable_value(&mut session.bed_type, i, *name);
                            }
                        });
                });
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Nozzle").color(t.text_muted));
                    // The installed nozzle sizes of this printer model; picking one switches to that
                    // printer preset, as in OrcaSlicer.
                    let schema = session.schema;
                    let variants = crate::profiles::nozzle_variants(&session.presets, schema);
                    let current = session.presets.printer.preset;
                    let nozzle = variants.iter().find(|(i, _)| *i == current).map_or_else(String::new, |v| v.1.clone());
                    let mut chosen = None;
                    egui::ComboBox::from_id_salt("nozzle").selected_text(format!("{nozzle} mm")).width(110.0).show_ui(
                        ui,
                        |ui| {
                            for (i, d) in &variants {
                                if ui.selectable_label(*i == current, format!("{d} mm")).clicked() {
                                    chosen = Some(*i);
                                }
                            }
                            ui.separator();
                            if ui.selectable_label(false, "Add nozzles…").clicked() {
                                chosen = Some(usize::MAX);
                            }
                        },
                    );
                    match chosen {
                        Some(usize::MAX) => actions.push(Action::Wizard(false)),
                        Some(i) if i != current => actions.push(Action::SelectPreset(TabKind::Printer, 0, i)),
                        _ => {}
                    }
                });
            });
        });
    });
}

fn filaments(ui: &mut Ui, sb: &mut Sidebar, session: &mut Session, t: &Tokens, actions: &mut Vec<Action>) {
    let mut open = sb.filament_open;
    let mut add = false;
    let mut remove = false;
    let count = session.presets.project_filaments.len();
    header(ui, t, "filament", "Filament", &mut open, |ui| {
        if icons::button(ui, "edit", t, 22.0, false, "Edit filament settings").clicked() {
            actions.push(Action::FilamentSettings(0));
        }
        if icons::button_enabled(ui, "delete_filament", t, 22.0, false, count > 1, "Remove last filament").clicked() {
            remove = true;
        }
        if icons::button(ui, "add_filament", t, 22.0, false, "Add one filament").clicked() {
            add = true;
        }
        if ui.small_button("Flushing volumes").clicked() {
            actions.push(Action::FlushingVolumes);
        }
    });
    sb.filament_open = open;
    if add {
        session.presets.add_filament();
    }
    if remove {
        session.presets.remove_filament();
        session.clamp_filaments();
    }
    if !open {
        return;
    }
    body(ui, |ui| {
        let n = session.presets.project_filaments.len();
        let col_w = (ui.available_width() - 8.0) / 2.0;
        egui::Grid::new("filament-grid").num_columns(2).spacing([8.0, 6.0]).show(ui, |ui| {
            for i in 0..n {
                ui.allocate_ui_with_layout(vec2(col_w, 26.0), Layout::left_to_right(Align::Center), |ui| {
                    filament_item(ui, session, t, i, col_w, actions);
                });
                if i % 2 == 1 {
                    ui.end_row();
                }
            }
        });
    });
}

fn filament_item(ui: &mut Ui, session: &mut Session, t: &Tokens, i: usize, width: f32, actions: &mut Vec<Action>) {
    let schema = session.schema;
    let colour_text = session.presets.filament_colour(schema, i);
    let mut colour = parse_hex(&colour_text).unwrap_or(Color32::GRAY);
    // The swatch: the filament's colour with its number; click to change the colour.
    let (r, response) = ui.allocate_exact_size(vec2(26.0, 24.0), Sense::click());
    ui.painter().rect_filled(r, 4.0, colour);
    ui.painter().rect_stroke(r, 4.0, Stroke::new(1.0, t.border), egui::StrokeKind::Inside);
    ui.painter().text(
        r.center(),
        egui::Align2::CENTER_CENTER,
        format!("{}", i + 1),
        egui::FontId::proportional(12.0),
        ink_on(colour),
    );
    let response = response.on_hover_text("Click to change the filament colour");
    egui::Popup::menu(&response).close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside).show(|ui| {
        if egui::color_picker::color_picker_color32(ui, &mut colour, egui::color_picker::Alpha::Opaque) {
            session.presets.set_value(schema, TabKind::Filament, i, "filament_colour", to_hex(colour));
        }
    });
    let Some(slot) = session.presets.project_filaments.get(i).cloned() else { return };
    let combo_w = (width - 26.0 - 30.0).max(60.0);
    if let Some(p) = preset_combo_compatible(
        ui,
        ("filament-preset", i),
        session,
        TabKind::Filament,
        slot.preset,
        slot.is_modified(),
        combo_w,
        actions,
    ) {
        actions.push(Action::SelectPreset(TabKind::Filament, i, p));
    }
    if icons::button(ui, "edit", t, 20.0, false, "Edit this filament").clicked() {
        actions.push(Action::FilamentSettings(i));
    }
}

fn objects(ui: &mut Ui, sb: &mut Sidebar, session: &mut Session, t: &Tokens, actions: &mut Vec<Action>) {
    let mut open = sb.objects_open;
    let count = session.model.objects.len();
    header(ui, t, "custom-gcode_object-info", &format!("Objects  ({count})"), &mut open, |_| {});
    sb.objects_open = open;
    if !open {
        return;
    }
    let row_h = 24.0;
    // Rows: each object, and its volumes and ranges unless folded.
    let rows: usize = session
        .model
        .objects
        .iter()
        .map(|o| 1 + if sb.folded.contains(&o.id) { 0 } else { o.parts.len() + o.ranges.len() })
        .sum();
    let height = ((rows.max(1)) as f32 * (row_h + 2.0) + 30.0).min(260.0);
    egui::Frame::new().inner_margin(egui::Margin::symmetric(6, 4)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal(|ui| {
            ui.add_space(28.0);
            ui.label(RichText::new("Name").small().color(t.text_muted));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.add_space(4.0);
                ui.label(RichText::new("Filament").small().color(t.text_muted));
                ui.add_space(16.0);
                ui.label(RichText::new("Plate").small().color(t.text_muted));
            });
        });
        egui::ScrollArea::vertical().id_salt("object-list").max_height(height - 24.0).auto_shrink([false, true]).show(
            ui,
            |ui| {
                if count == 0 {
                    ui.add_space(6.0);
                    ui.label(
                        RichText::new("No objects. Add a model with the Add button or drop files on the plate.")
                            .small()
                            .color(t.text_muted),
                    );
                }
                let ids: Vec<u64> = session.model.objects.iter().map(|o| o.id).collect();
                for (place, id) in ids.into_iter().enumerate() {
                    object_row(ui, sb, session, t, id, place, row_h, actions);
                    if !sb.folded.contains(&id) {
                        sub_rows(ui, sb, session, t, id, row_h);
                    }
                }
            },
        );
    });
}

#[allow(clippy::too_many_arguments)]
fn object_row(
    ui: &mut Ui,
    sb: &mut Sidebar,
    session: &mut Session,
    t: &Tokens,
    id: u64,
    place: usize,
    h: f32,
    actions: &mut Vec<Action>,
) {
    let Some(o) = session.model.get(id) else { return };
    let (name, plate, filament, visible) = (o.name.clone(), o.plate, o.filament, o.visible);
    let has_subs = !o.parts.is_empty() || !o.ranges.is_empty();
    let fits = session.fits(id);
    let selected = session.selection.contains(&id) && sb.sub.is_none_or(|(sid, _)| sid != id);
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), h), Sense::click_and_drag());
    // Drag an object onto another to move it there in the list.
    response.dnd_set_drag_payload(id);
    if let Some(moved) = response.dnd_release_payload::<u64>()
        && *moved != id
    {
        session.reorder(*moved, place);
    }
    if response.dnd_hover_payload::<u64>().is_some() {
        ui.painter().hline(rect.x_range(), rect.top(), Stroke::new(2.0, t.accent));
    }
    if selected {
        ui.painter().rect_filled(rect, 3.0, t.accent.gamma_multiply(0.22));
    } else if response.hovered() {
        ui.painter().rect_filled(rect, 3.0, t.hover_bg);
    }
    let mut child = ui.new_child(
        egui::UiBuilder::new().max_rect(rect.shrink2(vec2(4.0, 0.0))).layout(Layout::left_to_right(Align::Center)),
    );
    if has_subs {
        let folded = sb.folded.contains(&id);
        let (r, fold) = child.allocate_exact_size(vec2(14.0, 18.0), Sense::click());
        // A small triangle: right when folded, down when open.
        let c = r.center();
        let pts = if folded {
            vec![c + vec2(-3.0, -5.0), c + vec2(4.0, 0.0), c + vec2(-3.0, 5.0)]
        } else {
            vec![c + vec2(-5.0, -3.0), c + vec2(5.0, -3.0), c + vec2(0.0, 4.0)]
        };
        let colour = if fold.hovered() { t.accent } else { t.text_muted };
        child.painter().add(egui::Shape::convex_polygon(pts, colour, Stroke::NONE));
        if fold.clicked() {
            if folded {
                sb.folded.retain(|f| *f != id);
            } else {
                sb.folded.push(id);
            }
        }
    } else {
        child.add_space(18.0);
    }
    let eye = if visible { "im_visible" } else { "im_hidden" };
    if icons::button(&mut child, eye, t, 20.0, false, if visible { "Hide" } else { "Show" }).clicked() {
        session.toggle_visible(id);
    }
    let renaming = sb.renaming.as_ref().is_some_and(|(r, _)| *r == id);
    if renaming {
        let mut done = None;
        if let Some((_, text)) = sb.renaming.as_mut() {
            let edit = child.add(egui::TextEdit::singleline(text).desired_width(150.0));
            edit.request_focus();
            if edit.lost_focus() {
                done = Some(text.clone());
            }
        }
        if let Some(name) = done {
            session.rename(id, name);
            sb.renaming = None;
        }
    } else {
        let colour = if fits { t.text } else { t.warning };
        // Instances of one object, as OrcaSlicer lists them.
        let instances = session.model.instances_of(id);
        let shown = match instances.iter().position(|i| *i == id) {
            Some(k) if instances.len() > 1 => format!("{name} (instance {} of {})", k + 1, instances.len()),
            _ => name.clone(),
        };
        let label = child.add(egui::Label::new(RichText::new(&shown).color(colour)).truncate().sense(Sense::click()));
        if label.double_clicked() {
            sb.renaming = Some((id, name.clone()));
        }
        if !fits {
            label.on_hover_text("Outside the printable area of its plate");
        }
    }
    child.with_layout(Layout::right_to_left(Align::Center), |ui| {
        let colour = parse_hex(&session.presets.filament_colour(session.schema, filament.saturating_sub(1)))
            .unwrap_or(Color32::GRAY);
        let (r, sw) = ui.allocate_exact_size(vec2(22.0, 18.0), Sense::click());
        ui.painter().rect_filled(r, 3.0, colour);
        ui.painter().text(
            r.center(),
            egui::Align2::CENTER_CENTER,
            filament.to_string(),
            egui::FontId::proportional(11.0),
            ink_on(colour),
        );
        let n = session.presets.project_filaments.len();
        ui_kit::menu_below(&sw, t, |ui| {
            for f in 1..=n {
                if ui.selectable_label(f == filament, format!("Filament {f}")).clicked() {
                    session.set_filament(id, f);
                    ui.close();
                }
            }
        });
        ui.add_space(22.0);
        ui.label(RichText::new(format!("{}", plate + 1)).color(t.text_muted));
    });
    if response.clicked() {
        let multi = ui.input(|i| i.modifiers.ctrl || i.modifiers.command || i.modifiers.shift);
        if multi {
            if let Some(i) = session.selection.iter().position(|s| *s == id) {
                session.selection.remove(i);
            } else {
                session.selection.push(id);
            }
        } else {
            session.selection = vec![id];
            session.plate = plate;
        }
        sb.sub = None;
    }
    if response.secondary_clicked() && !selected {
        session.selection = vec![id];
    }
    ui_kit::context_menu(&response, t, |ui| {
        ui.set_min_width(220.0);
        crate::menus::object_menu(ui, session, id, actions);
    });
}

fn process(ui: &mut Ui, sb: &mut Sidebar, session: &mut Session, t: &Tokens, actions: &mut Vec<Action>) {
    let mut always_open = true;
    let mut advanced = session.prefs.advanced;
    let mut scope = sb.scope;
    header(ui, t, "process", "Process", &mut always_open, |ui| {
        if icons::button(ui, "compare", t, 22.0, false, "Compare presets").clicked() {
            actions.push(Action::ComparePresets);
        }
        if icons::button(ui, "save", t, 22.0, false, "Save current process preset").clicked() {
            actions.push(Action::SavePreset(TabKind::Process, 0));
        }
        ui_kit::switch(ui, t, &mut advanced).on_hover_text("Show all settings (Advanced)");
        ui.label(RichText::new("Advanced").small().color(t.text_muted));
        ui.add_space(6.0);
        ui_kit::segmented(ui, t, &["Global", "Objects"], &mut scope);
    });
    session.prefs.advanced = advanced;
    sb.scope = scope;
    egui::Frame::new().inner_margin(egui::Margin::symmetric(10, 6)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        let slot = session.presets.process.clone();
        ui.horizontal(|ui| {
            let w = ui.available_width() - 30.0;
            if let Some(i) = preset_combo_compatible(
                ui,
                "process-preset",
                session,
                TabKind::Process,
                slot.preset,
                slot.is_modified(),
                w,
                actions,
            ) {
                actions.push(Action::SelectPreset(TabKind::Process, 0, i));
            }
            if icons::button(ui, "edit", t, 22.0, false, "Edit process settings").clicked() {
                actions.push(Action::Settings(TabKind::Process));
            }
        });
        ui.add_space(4.0);
        // The page tabs: icon over name.
        let pages = session.schema.pages(TabKind::Process);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            let w = (ui.available_width() / pages.len().max(1) as f32 - 2.0).max(40.0);
            for (i, p) in pages.iter().enumerate() {
                let on = sb.process_page == i && sb.search.is_empty();
                let (r, response) = ui.allocate_exact_size(vec2(w, 44.0), Sense::click());
                if on {
                    ui.painter().rect_filled(r, 4.0, t.accent.gamma_multiply(0.18));
                    ui.painter().hline(r.x_range().shrink(4.0), r.bottom() - 1.0, Stroke::new(2.0, t.accent));
                } else if response.hovered() {
                    ui.painter().rect_filled(r, 4.0, t.hover_bg);
                }
                let icon_r = egui::Rect::from_center_size(r.center() - vec2(0.0, 8.0), vec2(20.0, 20.0));
                icons::paint(ui, icon_r, &p.icon, t, true);
                ui.painter().text(
                    r.center() + vec2(0.0, 12.0),
                    egui::Align2::CENTER_CENTER,
                    crate::i18n::tr(&p.name),
                    egui::FontId::proportional(11.0),
                    if on { t.accent } else { t.text },
                );
                if response.on_hover_text(&p.name).clicked() {
                    sb.process_page = i;
                    sb.search.clear();
                }
            }
        });
        ui.add_space(2.0);
        ui.horizontal(|ui| {
            let (r, _) = ui.allocate_exact_size(vec2(18.0, 18.0), Sense::hover());
            icons::paint(ui, r, "search", t, true);
            let field = ui.add(
                egui::TextEdit::singleline(&mut sb.search)
                    .hint_text(crate::i18n::tr("Search settings"))
                    .desired_width(f32::INFINITY),
            );
            if std::mem::take(&mut sb.focus_search) {
                field.request_focus();
            }
        });
    });
    ui.painter().hline(ui.available_rect_before_wrap().x_range(), ui.cursor().top(), Stroke::new(1.0, t.border));
    let target = if sb.scope == 1 {
        match session.selection.first() {
            Some(id) => Target {
                kind: TabKind::Process,
                filament: 0,
                object: Some(*id),
                sub: sb.sub.filter(|(sid, _)| sid == id).map(|(_, s)| s),
            },
            None => {
                body(ui, |ui| {
                    ui.label(
                        RichText::new("Select an object to give it its own process settings.").color(t.text_muted),
                    );
                });
                return;
            }
        }
    } else {
        Target::new(TabKind::Process)
    };
    egui::ScrollArea::vertical().id_salt("process-settings").auto_shrink([false, false]).show(ui, |ui| {
        egui::Frame::new().inner_margin(egui::Margin { left: 8, right: 12, top: 0, bottom: 12 }).show(ui, |ui| {
            ui.set_width(ui.available_width());
            if let Some(id) = target.object {
                object_overrides(ui, session, t, id, target.sub);
            }
            if !sb.search.trim().is_empty() {
                let q = sb.search.clone();
                settings::search_results(ui, session, t, TabKind::Process, &q, target, advanced);
            } else if let Some(p) = session.schema.pages(TabKind::Process).get(sb.process_page) {
                settings::page(ui, session, t, p, target, advanced);
            }
        });
    });
}

/// An object's volumes and height ranges, under its row.
fn sub_rows(ui: &mut Ui, sb: &mut Sidebar, session: &mut Session, t: &Tokens, id: u64, h: f32) {
    let Some(o) = session.model.get(id) else { return };
    let volumes: Vec<(String, PartKind, usize)> =
        o.parts.iter().map(|v| (v.name.clone(), v.kind, v.overrides.len())).collect();
    let ranges: Vec<(f32, f32, usize)> = o.ranges.iter().map(|r| (r.lo, r.hi, r.overrides.len())).collect();
    let rows =
        volumes.into_iter().enumerate().map(|(i, (name, kind, n))| (Sub::Volume(i), kind_icon(kind), name, n)).chain(
            ranges
                .into_iter()
                .enumerate()
                .map(|(i, (lo, hi, n))| (Sub::Range(i), "param_layer_height", format!("Height {lo:.2}–{hi:.2} mm"), n)),
        );
    for (sub, icon, label, n) in rows.collect::<Vec<_>>() {
        let on = sb.sub == Some((id, sub)) && session.selection.contains(&id);
        let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), h - 2.0), Sense::click());
        if on {
            ui.painter().rect_filled(rect, 3.0, t.accent.gamma_multiply(0.22));
        } else if response.hovered() {
            ui.painter().rect_filled(rect, 3.0, t.hover_bg);
        }
        let mut row = ui.new_child(
            egui::UiBuilder::new().max_rect(rect.shrink2(vec2(4.0, 0.0))).layout(Layout::left_to_right(Align::Center)),
        );
        row.add_space(34.0);
        let (r, _) = row.allocate_exact_size(vec2(16.0, 16.0), Sense::hover());
        icons::paint(&row, r, icon, t, true);
        row.add(egui::Label::new(RichText::new(label).small()).truncate());
        if n > 0 {
            row.label(RichText::new(format!("· {n} setting(s)")).small().color(t.text_muted));
        }
        if response.clicked() {
            session.selection = vec![id];
            sb.sub = Some((id, sub));
            // Its settings show in the Process panel's Objects mode.
            sb.scope = 1;
        }
        ui_kit::context_menu(&response, t, |ui| {
            ui.set_min_width(200.0);
            match sub {
                Sub::Volume(i) => {
                    ui.menu_button("Change type", |ui| {
                        for kind in PartKind::ALL {
                            if ui_kit::menu_item(ui, kind.label(), "", true) {
                                session.set_volume_kind(id, i, kind);
                                ui.close();
                            }
                        }
                    });
                    if ui_kit::menu_item(ui, "Delete", "", true) {
                        session.remove_volume(id, i);
                        sb.sub = None;
                        ui.close();
                    }
                }
                Sub::Range(i) => {
                    if ui_kit::menu_item(ui, "Delete", "", true) {
                        session.remove_range(id, i);
                        sb.sub = None;
                        ui.close();
                    }
                }
            }
        });
    }
}

/// Notes a field's change, and whether it starts an undo step (a drag starting, or a typed
/// change).
fn track(r: egui::Response, changed: &mut bool, undo: &mut bool) {
    *changed |= r.changed();
    *undo |= r.drag_started() || (r.changed() && !r.dragged());
}

/// The selected object's own settings, in Objects mode.
fn object_overrides(ui: &mut Ui, session: &mut Session, t: &Tokens, id: u64, sub: Option<Sub>) {
    let Some(o) = session.model.get(id) else { return };
    let name = o.name.clone();
    ui.add_space(6.0);
    match sub {
        Some(Sub::Volume(i)) => {
            let Some(v) = o.parts.get(i) else { return };
            let (vname, kind, n, mut tr) = (v.name.clone(), v.kind, v.overrides.len(), v.transform);
            ui.label(
                RichText::new(format!("{vname} ({}): {n} setting(s) of its own", kind.label().to_lowercase())).strong(),
            );
            // Where it sits in the object.
            let (mut changed, mut undo) = (false, false);
            ui.horizontal(|ui| {
                ui.label(RichText::new("Position").small().color(t.text_muted));
                for (axis, v) in ["X", "Y", "Z"].iter().zip([&mut tr.offset.x, &mut tr.offset.y, &mut tr.offset.z]) {
                    ui.label(RichText::new(*axis).small());
                    track(
                        ui.add(egui::DragValue::new(v).speed(0.1).max_decimals(2).suffix(" mm")),
                        &mut changed,
                        &mut undo,
                    );
                }
            });
            ui.horizontal(|ui| {
                ui.label(RichText::new("Scale").small().color(t.text_muted));
                for (axis, v) in ["X", "Y", "Z"].iter().zip([&mut tr.scale.x, &mut tr.scale.y, &mut tr.scale.z]) {
                    ui.label(RichText::new(*axis).small());
                    track(
                        ui.add(egui::DragValue::new(v).speed(0.01).range(0.01..=100.0).max_decimals(3)),
                        &mut changed,
                        &mut undo,
                    );
                }
            });
            if undo {
                session.checkpoint();
            }
            if changed {
                session.set_volume_transform(id, i, tr);
            }
        }
        Some(Sub::Range(i)) => {
            let Some(r) = o.ranges.get(i) else { return };
            let (mut lo, mut hi, n) = (r.lo, r.hi, r.overrides.len());
            ui.label(RichText::new(format!("{name}, height range: {n} setting(s) of its own")).strong());
            let (mut changed, mut undo) = (false, false);
            ui.horizontal(|ui| {
                ui.label(RichText::new("From").small().color(t.text_muted));
                track(
                    ui.add(egui::DragValue::new(&mut lo).speed(0.05).range(0.0..=10_000.0).suffix(" mm")),
                    &mut changed,
                    &mut undo,
                );
                ui.label(RichText::new("to").small().color(t.text_muted));
                track(
                    ui.add(egui::DragValue::new(&mut hi).speed(0.05).range(0.0..=10_000.0).suffix(" mm")),
                    &mut changed,
                    &mut undo,
                );
            });
            if undo {
                session.checkpoint();
            }
            if changed {
                session.set_range(id, i, lo, hi);
            }
        }
        None => {
            let n = o.overrides.len();
            ui.label(RichText::new(format!("{name}: {n} setting(s) of its own")).strong());
        }
    }
    ui.label(
        RichText::new("Changes below apply to this object only; a reset arrow returns a setting to the global value.")
            .small()
            .color(t.text_muted),
    );
}
