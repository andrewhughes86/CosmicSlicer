//! The windows: the Printer / Filament / Process settings editors, saving and switching
//! presets, comparing them, Preferences, plate settings, bed shape, the printer connection,
//! keyboard shortcuts and About.

use egui::{Align, Layout, RichText, Sense, Stroke, vec2};

use CosmicSlicer_config::TabKind;

use crate::icons;
use CosmicSlicer_connect::{HostType, PhysicalPrinter};

use crate::pages::DeviceState;
use crate::settings::{self, Target};
use crate::sidebar::preset_combo;
use crate::state::{BED_TYPES, Level, Page, Session};
use crate::theme::{Theme, Tokens, parse_hex};
use crate::ui_kit::{self, Window};
use egui::Color32;

/// An open settings editor.
#[derive(Clone, Debug)]
pub struct Editor {
    pub kind: TabKind,
    pub filament: usize,
    pub page: usize,
    pub search: String,
}

/// A preset switch waiting on what to do with unsaved edits.
#[derive(Clone, Copy, Debug)]
pub struct PendingSwitch {
    pub kind: TabKind,
    pub filament: usize,
    pub index: usize,
}

#[derive(Default)]
pub struct Dialogs {
    pub editor: Option<Editor>,
    pub save_preset: Option<(TabKind, usize, String)>,
    pub switch: Option<PendingSwitch>,
    pub compare: Option<(TabKind, usize, usize)>,
    pub preferences: bool,
    /// "Jump to layer" (Shift+G in the Preview): the layer number being typed.
    pub go_to_layer: Option<String>,
    /// The layer asked for, for the app to show.
    pub jump_to_layer: Option<usize>,
    /// The Preferences window's page.
    pub preferences_page: usize,
    pub about: bool,
    pub shortcuts: bool,
    pub plate: Option<usize>,
    pub physical_printer: bool,
    pub flushing: bool,
    pub calibration: Option<crate::calibration::Dialog>,
    /// A calibration test was just set up: show it.
    pub calibration_ready: bool,
    pub rename: Option<(u64, String)>,
    /// Bed shape: (width, depth, origin x, origin y).
    pub bed_shape: Option<[f32; 4]>,
    pub print: bool,
    /// The Print dialog's "Export G-code" was clicked.
    pub export_requested: bool,
    /// The Print dialog's Send: (printer name, start printing).
    pub send_requested: Option<(String, bool)>,
    /// The Print dialog's "Upload only" is ticked.
    pub upload_only: bool,
    /// The setup wizard, while open.
    pub wizard: Option<crate::wizard::Wizard>,
    /// User preset being renamed: (kind, index, new name so far).
    pub rename_preset: Option<(TabKind, usize, String)>,
    /// User preset to delete once confirmed.
    pub delete_preset: Option<(TabKind, usize)>,
    /// "Export…" was clicked in a settings window.
    pub export_preset: Option<(TabKind, usize)>,
    /// A project left by a session that did not exit cleanly, and the answer to restoring it.
    pub restore: Option<crate::recovery::Orphan>,
    pub restore_choice: Option<bool>,
}

impl Dialogs {
    /// Closes the top-most dialog (Esc).
    pub fn close_one(&mut self) -> bool {
        if self.switch.take().is_some() || self.save_preset.take().is_some() || self.rename.take().is_some() {
            return true;
        }
        if self.bed_shape.take().is_some() || self.compare.take().is_some() || self.plate.take().is_some() {
            return true;
        }
        for flag in [
            &mut self.preferences,
            &mut self.about,
            &mut self.shortcuts,
            &mut self.physical_printer,
            &mut self.flushing,
            &mut self.print,
        ] {
            if *flag {
                *flag = false;
                return true;
            }
        }
        self.calibration.take().is_some() || self.editor.take().is_some() || self.wizard.take().is_some()
    }

    pub fn show(&mut self, ctx: &egui::Context, session: &mut Session, t: &Tokens, device: &mut DeviceState) {
        self.editor_window(ctx, session, t);
        self.save_window(ctx, session, t);
        self.switch_window(ctx, session, t);
        self.compare_window(ctx, session, t);
        self.preferences_window(ctx, session, t);
        self.plate_window(ctx, session, t);
        self.bed_shape_window(ctx, session, t);
        self.printer_window(ctx, session, t, device);
        self.small_windows(ctx, session, t);
        self.restore_window(ctx, t);
        self.preset_windows(ctx, session, t);
        units_window(ctx, session, t);
    }

    fn editor_window(&mut self, ctx: &egui::Context, session: &mut Session, t: &Tokens) {
        let Some(mut ed) = self.editor.clone() else { return };
        let mut open = true;
        let title = match ed.kind {
            TabKind::Printer => "Printer settings".to_owned(),
            TabKind::Filament => format!("Filament settings — filament {}", ed.filament + 1),
            TabKind::Process => "Process settings".to_owned(),
        };
        let mut save = false;
        let mut chosen = None;
        Window::sized(title, vec2(900.0, 680.0)).id(("editor", ed.kind.title())).show(ctx, t, &mut open, |ui| {
            let advanced = session.prefs.advanced;
            // Preset row: dropdown, save, compare, Advanced.
            ui.horizontal(|ui| {
                let (r, _) = ui.allocate_exact_size(vec2(22.0, 22.0), Sense::hover());
                icons::paint(
                    ui,
                    r,
                    match ed.kind {
                        TabKind::Printer => "printer",
                        TabKind::Filament => "filament",
                        TabKind::Process => "process",
                    },
                    t,
                    true,
                );
                let list = session.presets.list(ed.kind).to_vec();
                let shown = session.presets.compatible(ed.kind);
                if let Some(slot) = session.presets.slot(ed.kind, ed.filament).cloned()
                    && let Some(crate::sidebar::Pick::Preset(i)) = crate::sidebar::preset_combo_filtered(
                        ui,
                        ("editor-preset", ed.kind.title(), ed.filament),
                        &list,
                        slot.preset,
                        slot.is_modified(),
                        340.0,
                        &shown,
                        None,
                    )
                {
                    chosen = Some(i);
                }
                if icons::button(ui, "save", t, 24.0, false, "Save preset").clicked() {
                    save = true;
                }
                // User presets can be renamed, deleted and cut loose from their system parent.
                let index = session.presets.slot(ed.kind, ed.filament).map_or(0, |s| s.preset);
                let current = session.presets.list(ed.kind).get(index).cloned();
                let user = current.as_ref().is_some_and(|p| !p.system);
                if icons::button_enabled(ui, "edit", t, 24.0, false, user, "Rename").clicked()
                    && let Some(p) = &current
                {
                    self.rename_preset = Some((ed.kind, index, p.name.clone()));
                }
                if icons::button_enabled(ui, "delete", t, 24.0, false, user, "Delete").clicked() {
                    self.delete_preset = Some((ed.kind, index));
                }
                if let Some(p) = current.as_ref().filter(|p| !p.system && !p.inherits.is_empty())
                    && ui.small_button("Detach").on_hover_text(format!("Stop following \"{}\"", p.inherits)).clicked()
                {
                    session.presets.detach(ed.kind, index);
                }
                if ui.small_button("Export…").clicked() {
                    self.export_preset = Some((ed.kind, ed.filament));
                }
                if icons::button(ui, "compare", t, 24.0, false, "Compare presets").clicked() {
                    let current = session.presets.slot(ed.kind, ed.filament).map_or(0, |s| s.preset);
                    self.compare = Some((ed.kind, current, current));
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let mut adv = session.prefs.advanced;
                    ui_kit::switch(ui, t, &mut adv);
                    session.prefs.advanced = adv;
                    ui.label(RichText::new("Advanced").color(t.text_muted));
                });
            });
            ui.add_space(6.0);
            ui.separator();
            let pages = session.schema.pages(ed.kind);
            let height = ui.available_height();
            ui.horizontal_top(|ui| {
                // Page list down the left, as in OrcaSlicer's settings windows.
                ui.vertical(|ui| {
                    ui.set_width(190.0);
                    ui.add(egui::TextEdit::singleline(&mut ed.search).hint_text("Search").desired_width(184.0));
                    ui.add_space(6.0);
                    for (i, p) in pages.iter().enumerate() {
                        let on = ed.page == i && ed.search.is_empty();
                        let (r, response) = ui.allocate_exact_size(vec2(184.0, 32.0), Sense::click());
                        if on {
                            ui.painter().rect_filled(r, 4.0, t.accent.gamma_multiply(0.2));
                            ui.painter().vline(r.left() + 1.0, r.y_range().shrink(6.0), Stroke::new(3.0, t.accent));
                        } else if response.hovered() {
                            ui.painter().rect_filled(r, 4.0, t.hover_bg);
                        }
                        icons::paint(
                            ui,
                            egui::Rect::from_center_size(r.left_center() + vec2(18.0, 0.0), vec2(18.0, 18.0)),
                            &p.icon,
                            t,
                            true,
                        );
                        ui.painter().text(
                            r.left_center() + vec2(36.0, 0.0),
                            egui::Align2::LEFT_CENTER,
                            crate::i18n::tr(&p.name),
                            egui::FontId::proportional(13.5),
                            if on { t.accent } else { t.text },
                        );
                        if response.clicked() {
                            ed.page = i;
                            ed.search.clear();
                        }
                    }
                });
                ui.separator();
                ui.vertical(|ui| {
                    egui::ScrollArea::vertical()
                        .id_salt(("editor-scroll", ed.kind.title()))
                        .max_height(height - 10.0)
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width() - 12.0);
                            let target = Target { kind: ed.kind, filament: ed.filament, object: None, sub: None };
                            if !ed.search.trim().is_empty() {
                                let q = ed.search.clone();
                                settings::search_results(ui, session, t, ed.kind, &q, target, advanced);
                            } else if let Some(p) = pages.get(ed.page) {
                                settings::page(ui, session, t, p, target, advanced);
                            }
                        });
                });
            });
        });
        if let Some(index) = chosen {
            self.request_switch(session, ed.kind, ed.filament, index);
        }
        if save {
            self.open_save(session, ed.kind, ed.filament);
        }
        self.editor = open.then_some(ed);
        if ctx.data(|d| d.get_temp::<bool>(egui::Id::new("bed-shape-open"))).unwrap_or(false) {
            ctx.data_mut(|d| d.remove::<bool>(egui::Id::new("bed-shape-open")));
            let (lo, size) = session.bed();
            self.bed_shape = Some([size.x, size.y, -lo.x, -lo.y]);
        }
    }

    pub fn open_save(&mut self, session: &Session, kind: TabKind, filament: usize) {
        let name = session.presets.preset(kind, filament).map(|p| p.name.clone()).unwrap_or_default();
        let name = if session.presets.preset(kind, filament).is_some_and(|p| p.system) {
            format!("{name} - Copy")
        } else {
            name
        };
        self.save_preset = Some((kind, filament, name));
    }

    /// Switches preset, asking first when the current one has unsaved edits.
    pub fn request_switch(&mut self, session: &mut Session, kind: TabKind, filament: usize, index: usize) {
        if session.presets.slot(kind, filament).is_some_and(|s| s.is_modified()) {
            self.switch = Some(PendingSwitch { kind, filament, index });
        } else {
            session.presets.select(kind, filament, index);
            session.sliced.clear();
        }
    }

    fn save_window(&mut self, ctx: &egui::Context, session: &mut Session, t: &Tokens) {
        let Some((kind, filament, mut name)) = self.save_preset.clone() else { return };
        let mut open = true;
        let mut done = false;
        let mut cancel = false;
        Window::dialog(format!("Save {} preset", kind.title().to_lowercase())).show(ctx, t, &mut open, |ui| {
            ui.set_width(380.0);
            ui.label("Save the current settings as a user preset named:");
            let edit = ui.add(egui::TextEdit::singleline(&mut name).desired_width(f32::INFINITY));
            if edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                done = true;
            }
            let system = session.presets.list(kind).iter().any(|p| p.system && p.name == name.trim());
            let exists = session.presets.list(kind).iter().any(|p| !p.system && p.name == name.trim());
            if system {
                ui.label(RichText::new("That is the name of a system preset.").color(t.error));
            } else if exists {
                ui.label(RichText::new("A user preset of that name will be replaced.").color(t.warning));
            }
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.add_enabled(!system && !name.trim().is_empty(), egui::Button::new("Save")).clicked() {
                    done = true;
                }
                if ui.button("Cancel").clicked() {
                    cancel = true;
                }
            });
        });
        open &= !cancel;
        if done {
            match session.presets.save_as(kind, filament, &name) {
                Ok(()) => session.notify(Level::Info, format!("Saved preset \"{}\".", name.trim())),
                Err(e) => session.notify(Level::Error, e),
            }
            open = false;
        }
        self.save_preset = open.then_some((kind, filament, name));
    }

    fn switch_window(&mut self, ctx: &egui::Context, session: &mut Session, t: &Tokens) {
        let Some(sw) = self.switch else { return };
        let mut open = true;
        let mut choice = None;
        let mut cancel = false;
        Window::dialog("Unsaved changes").show(ctx, t, &mut open, |ui| {
            ui.set_width(440.0);
            let name = session.presets.preset(sw.kind, sw.filament).map(|p| p.name.clone()).unwrap_or_default();
            let n = session.presets.slot(sw.kind, sw.filament).map_or(0, |s| s.edits.values.len());
            ui.label(format!("\"{name}\" has {n} unsaved change(s). What should happen to them?"));
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                if ui.button("Transfer to the new preset").clicked() {
                    choice = Some(1);
                }
                if ui.button("Discard").clicked() {
                    choice = Some(0);
                }
                if ui.button("Save first…").clicked() {
                    choice = Some(2);
                }
                if ui.button("Cancel").clicked() {
                    cancel = true;
                }
            });
        });
        open &= !cancel;
        match choice {
            Some(0) => {
                session.presets.select(sw.kind, sw.filament, sw.index);
                open = false;
            }
            Some(1) => {
                let edits = session.presets.slot(sw.kind, sw.filament).map(|s| s.edits.clone()).unwrap_or_default();
                session.presets.select(sw.kind, sw.filament, sw.index);
                let schema = session.schema;
                for (k, v) in edits.values {
                    session.presets.set_value(schema, sw.kind, sw.filament, &k, v);
                }
                open = false;
            }
            Some(_) => {
                self.open_save(session, sw.kind, sw.filament);
                open = false;
            }
            None => {}
        }
        if !open {
            session.sliced.clear();
        }
        self.switch = if open { Some(sw) } else { None };
    }

    fn compare_window(&mut self, ctx: &egui::Context, session: &mut Session, t: &Tokens) {
        let Some((mut kind, mut a, mut b)) = self.compare else { return };
        let mut open = true;
        Window::sized("Compare presets", vec2(820.0, 560.0)).show(ctx, t, &mut open, |ui| {
            ui.horizontal(|ui| {
                for k in [TabKind::Printer, TabKind::Filament, TabKind::Process] {
                    if ui.selectable_label(kind == k, k.title()).clicked() && kind != k {
                        kind = k;
                        a = 0;
                        b = 0;
                    }
                }
            });
            let list = session.presets.list(kind).to_vec();
            ui.horizontal(|ui| {
                if let Some(i) = preset_combo(ui, "compare-a", &list, a, false, 360.0) {
                    a = i;
                }
                ui.label("↔");
                if let Some(i) = preset_combo(ui, "compare-b", &list, b, false, 360.0) {
                    b = i;
                }
            });
            ui.separator();
            let schema = session.schema;
            let (pa, pb) = (list.get(a), list.get(b));
            let mut keys: Vec<&String> = pa.iter().chain(pb.iter()).flat_map(|p| p.config.values.keys()).collect();
            keys.sort();
            keys.dedup();
            let diffs: Vec<(String, String, String)> = keys
                .into_iter()
                .filter_map(|k| {
                    let va = pa.map(|p| p.config.get(schema, k).to_owned()).unwrap_or_default();
                    let vb = pb.map(|p| p.config.get(schema, k).to_owned()).unwrap_or_default();
                    (va != vb).then(|| (schema.option(k).map_or(k.clone(), |d| d.long_label().to_owned()), va, vb))
                })
                .collect();
            egui::ScrollArea::vertical().show(ui, |ui| {
                if diffs.is_empty() {
                    ui.label(RichText::new("The presets are the same.").color(t.text_muted));
                }
                egui::Grid::new("compare-grid").num_columns(3).striped(true).spacing([24.0, 6.0]).show(ui, |ui| {
                    for (label, va, vb) in diffs {
                        ui.label(label);
                        ui.label(RichText::new(va).color(t.accent));
                        ui.label(RichText::new(vb).color(t.modified));
                        ui.end_row();
                    }
                });
            });
        });
        self.compare = open.then_some((kind, a, b));
    }

    fn preferences_window(&mut self, ctx: &egui::Context, session: &mut Session, t: &Tokens) {
        if !self.preferences {
            return;
        }
        let mut open = true;
        let mut shortcuts = false;
        let page = &mut self.preferences_page;
        Window::sized("Preferences", vec2(680.0, 560.0)).show(ctx, t, &mut open, |ui| {
            ui.horizontal_top(|ui| {
                ui.vertical(|ui| {
                    ui.set_width(130.0);
                    for (i, name) in PREFERENCE_PAGES.iter().enumerate() {
                        if ui.selectable_label(*page == i, crate::i18n::tr(name)).clicked() {
                            *page = i;
                        }
                    }
                });
                ui.separator();
                ui.vertical(|ui| {
                    egui::ScrollArea::vertical().id_salt("prefs-scroll").show(ui, |ui| {
                        preferences_page(ui, session, t, *page, &mut shortcuts);
                    });
                });
            });
        });
        if shortcuts {
            self.shortcuts = true;
        }
        self.preferences = open;
    }

    fn plate_window(&mut self, ctx: &egui::Context, session: &mut Session, t: &Tokens) {
        let Some(p) = self.plate else { return };
        let mut open = true;
        Window::dialog(format!("Plate {} settings", p + 1)).show(ctx, t, &mut open, |ui| {
            ui.set_width(420.0);
            let s = session.plate_settings_mut(p);
            egui::Grid::new("plate-settings").num_columns(2).spacing([20.0, 10.0]).show(ui, |ui| {
                ui.label("Plate name");
                ui.add(egui::TextEdit::singleline(&mut s.name).desired_width(240.0));
                ui.end_row();
                ui.label("Bed type");
                let current = s.bed_type.and_then(|i| BED_TYPES.get(i)).map_or("Same as Global Plate Type", |b| b.0);
                egui::ComboBox::from_id_salt("plate-bed").selected_text(current).width(240.0).show_ui(ui, |ui| {
                    ui.selectable_value(&mut s.bed_type, None, "Same as Global Plate Type");
                    for (i, (name, _)) in BED_TYPES.iter().enumerate() {
                        ui.selectable_value(&mut s.bed_type, Some(i), *name);
                    }
                });
                ui.end_row();
                ui.label("Print sequence");
                egui::ComboBox::from_id_salt("plate-seq")
                    .selected_text(["Same as Global Print Sequence", "By layer", "By object"][s.sequence.min(2)])
                    .width(240.0)
                    .show_ui(ui, |ui| {
                        for (i, n) in ["Same as Global Print Sequence", "By layer", "By object"].iter().enumerate() {
                            ui.selectable_value(&mut s.sequence, i, *n);
                        }
                    });
                ui.end_row();
                ui.label("Spiral vase");
                egui::ComboBox::from_id_salt("plate-spiral")
                    .selected_text(["Same as global", "Enable", "Disable"][s.spiral.min(2)])
                    .width(240.0)
                    .show_ui(ui, |ui| {
                        for (i, n) in ["Same as global", "Enable", "Disable"].iter().enumerate() {
                            ui.selectable_value(&mut s.spiral, i, *n);
                        }
                    });
                ui.end_row();
            });
            ui.add_space(8.0);
            if ui.button("OK").clicked() {
                self.plate = None;
            }
        });
        if !open {
            self.plate = None;
        }
        session.dirty = true;
    }

    fn bed_shape_window(&mut self, ctx: &egui::Context, session: &mut Session, t: &Tokens) {
        let Some(mut v) = self.bed_shape else { return };
        let mut open = true;
        let mut apply = false;
        let mut cancel = false;
        Window::dialog("Bed shape").show(ctx, t, &mut open, |ui| {
            ui.set_width(360.0);
            ui.label(RichText::new("Rectangular").strong());
            egui::Grid::new("bed-shape").num_columns(2).spacing([20.0, 8.0]).show(ui, |ui| {
                for (i, name) in ["Size X", "Size Y", "Origin X", "Origin Y"].iter().enumerate() {
                    ui.label(*name);
                    let range = if i < 2 { 10.0..=5000.0 } else { -5000.0..=5000.0 };
                    ui.add(egui::DragValue::new(&mut v[i]).range(range).suffix(" mm").speed(1.0));
                    ui.end_row();
                }
            });
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.button("OK").clicked() {
                    apply = true;
                }
                if ui.button("Cancel").clicked() {
                    cancel = true;
                }
            });
        });
        open &= !cancel;
        if apply {
            let [w, d, ox, oy] = v;
            let (x0, y0) = (-ox, -oy);
            let area = format!("{x0}x{y0},{}x{y0},{}x{},{x0}x{}", x0 + w, x0 + w, y0 + d, y0 + d);
            let schema = session.schema;
            session.presets.set_value(schema, TabKind::Printer, 0, "printable_area", area);
            open = false;
        }
        self.bed_shape = open.then_some(v);
    }

    /// OrcaSlicer's physical printer dialog: add or edit a printer's network connection,
    /// test it, and (for Bambu printers) find it on the network.
    fn printer_window(&mut self, ctx: &egui::Context, session: &mut Session, t: &Tokens, d: &mut DeviceState) {
        if !self.physical_printer {
            return;
        }
        if d.edit.is_none() {
            let n = session.prefs.printers.len() + 1;
            d.edit = Some((None, PhysicalPrinter { name: format!("Printer {n}"), ..Default::default() }));
            d.test_result = None;
        }
        // Finished background jobs.
        if let Some(r) = d.test.as_ref().and_then(|j| j.poll()) {
            d.test = None;
            d.test_result = Some(r.unwrap_or_else(|| Err("the test stopped unexpectedly".into())));
        }
        if let Some(r) = d.storages.as_ref().and_then(|j| j.poll()) {
            d.storages = None;
            match r {
                Some(Ok(list)) => d.storage_list = list,
                Some(Err(e)) => d.test_result = Some(Err(e)),
                None => {}
            }
        }
        if let Some(r) = d.discover.as_ref().and_then(|j| j.poll()) {
            d.discover = None;
            d.found = r.unwrap_or_else(|| Err("the search stopped unexpectedly".into()));
        }
        if d.test.is_some() || d.storages.is_some() || d.discover.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(200));
        }
        let mut open = true;
        let mut close = false;
        Window::dialog("Physical Printer").show(ctx, t, &mut open, |ui| {
            ui.set_width(500.0);
            let Some((index, p)) = d.edit.as_mut() else { return };
            // Which printer: one already set up, or a new one.
            let names: Vec<String> = session.prefs.printers.iter().map(|x| x.name.clone()).collect();
            if !names.is_empty() {
                ui.horizontal(|ui| {
                    ui.label("Edit");
                    let shown = index.and_then(|i| names.get(i).cloned()).unwrap_or_else(|| "New printer".into());
                    egui::ComboBox::from_id_salt("pp-which").selected_text(shown).width(260.0).show_ui(ui, |ui| {
                        for (i, n) in names.iter().enumerate() {
                            if ui.selectable_label(*index == Some(i), n).clicked()
                                && let Some(x) = session.prefs.printers.get(i)
                            {
                                *index = Some(i);
                                *p = x.clone();
                                d.test_result = None;
                                d.storage_list.clear();
                            }
                        }
                        if ui.selectable_label(index.is_none(), "New printer").clicked() {
                            *index = None;
                            *p = PhysicalPrinter { name: format!("Printer {}", names.len() + 1), ..Default::default() };
                            d.test_result = None;
                        }
                    });
                });
                ui.separator();
            }
            egui::Grid::new("host").num_columns(2).spacing([20.0, 10.0]).show(ui, |ui| {
                ui.label("Name");
                ui.add(egui::TextEdit::singleline(&mut p.name).desired_width(280.0));
                ui.end_row();
                ui.label("Host Type");
                egui::ComboBox::from_id_salt("host-type").selected_text(p.host.label()).width(280.0).show_ui(ui, |ui| {
                    for h in HostType::ALL {
                        ui.selectable_value(&mut p.host, h, h.label());
                    }
                });
                ui.end_row();
                let bambu = p.host == HostType::BambuLan;
                ui.label(if bambu { "IP address" } else { "Hostname, IP or URL" });
                ui.add(egui::TextEdit::singleline(&mut p.address).hint_text("192.168.1.50 or printer.local").desired_width(280.0));
                ui.end_row();
                if bambu {
                    ui.label("Serial number");
                    ui.add(egui::TextEdit::singleline(&mut p.serial).desired_width(280.0));
                    ui.end_row();
                    ui.label("Access code");
                    ui.add(egui::TextEdit::singleline(&mut p.access_code).password(true).desired_width(280.0));
                    ui.end_row();
                    ui.label("AMS");
                    ui.checkbox(&mut p.use_ams, "Print with the AMS (filament n from slot n)");
                    ui.end_row();
                } else if p.host == HostType::PrusaLink {
                    ui.label("User");
                    ui.add(egui::TextEdit::singleline(&mut p.user).hint_text("maker").desired_width(280.0));
                    ui.end_row();
                    ui.label("Password");
                    ui.add(egui::TextEdit::singleline(&mut p.password).password(true).desired_width(280.0));
                    ui.end_row();
                    ui.label("or API Key");
                    ui.add(egui::TextEdit::singleline(&mut p.api_key).password(true).desired_width(280.0));
                    ui.end_row();
                } else {
                    ui.label("API Key / Password");
                    ui.add(egui::TextEdit::singleline(&mut p.api_key).password(true).desired_width(280.0));
                    ui.end_row();
                }
                if p.host == HostType::Repetier {
                    ui.label("Printer");
                    ui.add(egui::TextEdit::singleline(&mut p.slug).hint_text("the printer's slug on the server").desired_width(280.0));
                    ui.end_row();
                }
                if p.host.has_storage() {
                    ui.label("Storage");
                    ui.horizontal(|ui| {
                        let shown = d.storage_list.iter().find(|s| s.id == p.storage).map_or_else(
                            || if p.storage.is_empty() { "Printer's default".to_owned() } else { p.storage.clone() },
                            |s| s.name.clone(),
                        );
                        egui::ComboBox::from_id_salt("pp-storage").selected_text(shown).width(190.0).show_ui(ui, |ui| {
                            ui.selectable_value(&mut p.storage, String::new(), "Printer's default");
                            for st in &d.storage_list {
                                ui.selectable_value(&mut p.storage, st.id.clone(), &st.name);
                            }
                        });
                        if ui.add_enabled(d.storages.is_none(), egui::Button::new("Refresh")).clicked() {
                            d.storages = crate::devices::storages(p);
                        }
                    });
                    ui.end_row();
                }
                if !bambu {
                    ui.label("HTTPS");
                    ui.checkbox(&mut p.verify_tls, "Check the certificate (off for printers that sign their own)");
                    ui.end_row();
                }
            });
            if let Some(why) = p.host.unsupported() {
                ui.colored_label(t.warning, format!("{}: {why}.", p.host.label()));
            }
            if p.host == HostType::BambuLan {
                ui.add_space(6.0);
                ui.label(
                    RichText::new(
                        "Turn on LAN mode on the printer (Settings › Network; newer firmware also needs \"LAN only\" or developer mode). The access code and IP address are on the same screen; the serial number is in Settings › Device.",
                    )
                    .small()
                    .color(t.text_muted),
                );
                ui.horizontal(|ui| {
                    if ui.add_enabled(d.discover.is_none(), egui::Button::new("Find printers")).clicked() {
                        d.discover = crate::devices::discover();
                        d.found = Ok(Vec::new());
                    }
                    if d.discover.is_some() {
                        ui.spinner();
                        ui.label("Listening for printers…");
                    }
                });
                match &d.found {
                    Ok(found) => {
                        for f in found {
                            let text = format!("{} ({}) at {} · {}", f.name, f.model, f.ip, f.serial);
                            if ui.button(text).on_hover_text("Use this printer").clicked() {
                                p.address = f.ip.clone();
                                p.serial = f.serial.clone();
                                if p.name.starts_with("Printer ") && !f.name.is_empty() {
                                    p.name = f.name.clone();
                                }
                            }
                        }
                    }
                    Err(e) => {
                        ui.colored_label(t.warning, e);
                    }
                }
            }
            ui.add_space(10.0);
            match &d.test_result {
                Some(Ok(text)) => {
                    ui.colored_label(t.success, format!("✔ {text}"));
                }
                Some(Err(e)) => {
                    ui.colored_label(t.error, e);
                }
                None => {}
            }
            ui.horizontal(|ui| {
                if ui.add_enabled(d.test.is_none(), egui::Button::new("Test")).clicked() {
                    d.test_result = None;
                    d.test = crate::devices::test(p);
                }
                if d.test.is_some() {
                    ui.spinner();
                }
                let name = p.name.trim().to_owned();
                let taken = session.prefs.printers.iter().enumerate().any(|(i, x)| x.name == name && Some(i) != *index);
                let can_save = !name.is_empty() && !taken && !p.address.trim().is_empty();
                if ui_kit::primary_button(ui, t, "Save").clicked() && can_save {
                    p.name = name.clone();
                    match index.and_then(|i| session.prefs.printers.get_mut(i)) {
                        Some(slot) => *slot = p.clone(),
                        None => {
                            session.prefs.printers.push(p.clone());
                            *index = Some(session.prefs.printers.len() - 1);
                        }
                    }
                    session.prefs.active_printer = name;
                    d.devices.prune(&session.prefs.printers);
                    close = true;
                }
                if let Some(i) = *index
                    && ui.button("Delete").clicked()
                {
                    if i < session.prefs.printers.len() {
                        session.prefs.printers.remove(i);
                    }
                    d.devices.prune(&session.prefs.printers);
                    close = true;
                }
                if ui.button("Close").clicked() {
                    close = true;
                }
                if taken {
                    ui.colored_label(t.warning, "Another printer has this name.");
                } else if p.address.trim().is_empty() {
                    ui.label(RichText::new("The address is needed.").color(t.text_muted));
                }
            });
        });
        if !open || close {
            self.physical_printer = false;
            d.edit = None;
            d.test_result = None;
        }
    }

    fn preset_windows(&mut self, ctx: &egui::Context, session: &mut Session, t: &Tokens) {
        if let Some((kind, index, mut name)) = self.rename_preset.clone() {
            let mut open = true;
            let mut done = false;
            Window::dialog("Rename preset").show(ctx, t, &mut open, |ui| {
                ui.set_width(380.0);
                let edit = ui.add(egui::TextEdit::singleline(&mut name).desired_width(f32::INFINITY));
                if edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    done = true;
                }
                if ui.button("Rename").clicked() {
                    done = true;
                }
            });
            if done {
                match session.presets.rename(kind, index, &name) {
                    Ok(()) => open = false,
                    Err(e) => session.notify(Level::Warning, e),
                }
            }
            self.rename_preset = open.then_some((kind, index, name));
        }
        if let Some((kind, index)) = self.delete_preset {
            let mut open = true;
            let name = session.presets.list(kind).get(index).map(|p| p.name.clone()).unwrap_or_default();
            let mut answer = None;
            Window::dialog("Delete preset").show(ctx, t, &mut open, |ui| {
                ui.set_width(380.0);
                ui.label(format!("Delete the user preset \"{name}\"? Its file is removed too."));
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button("Delete").clicked() {
                        answer = Some(true);
                    }
                    if ui.button("Cancel").clicked() {
                        answer = Some(false);
                    }
                });
            });
            if answer == Some(true)
                && let Err(e) = session.presets.delete(kind, index)
            {
                session.notify(Level::Warning, e);
            }
            if answer.is_some() || !open {
                self.delete_preset = None;
            }
        }
    }

    pub fn close_units(session: &mut Session) -> bool {
        session.units_prompt.take().is_some()
    }

    fn restore_window(&mut self, ctx: &egui::Context, t: &Tokens) {
        let Some(o) = self.restore.clone() else { return };
        if self.restore_choice.is_some() {
            return;
        }
        let mut open = true;
        Window::dialog("Restore project").show(ctx, t, &mut open, |ui| {
            ui.set_width(460.0);
            ui.label(RichText::new("CosmicSlicer closed unexpectedly last time.").strong());
            ui.add_space(4.0);
            ui.label(format!(
                "\"{}\" ({} object(s)) was autosaved {}. Restore it?",
                o.name,
                o.objects,
                crate::recovery::ago(o.age)
            ));
            if let Some(report) = &o.report {
                ui.add_space(4.0);
                ui.label(
                    RichText::new(format!("A crash report was saved to {}", report.display()))
                        .small()
                        .color(t.text_muted),
                );
            }
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                if ui_kit::primary_button(ui, t, "Restore").clicked() {
                    self.restore_choice = Some(true);
                }
                if ui.button("Discard").clicked() {
                    self.restore_choice = Some(false);
                }
            });
        });
        if !open {
            // Closing the prompt keeps the autosave for next time.
            self.restore = None;
        }
    }

    fn small_windows(&mut self, ctx: &egui::Context, session: &mut Session, t: &Tokens) {
        if self.about {
            let mut open = true;
            Window::dialog("About CosmicSlicer").show(ctx, t, &mut open, |ui| {
                ui.set_width(420.0);
                ui.vertical_centered(|ui| {
                    let (r, _) = ui.allocate_exact_size(vec2(72.0, 72.0), Sense::hover());
                    icons::paint_original(ui, r, "cosmic_logo", t);
                    ui.label(RichText::new("CosmicSlicer").size(22.0).strong());
                    ui.label(format!("Version {}", env!("CARGO_PKG_VERSION")));
                    ui.add_space(8.0);
                    ui.label("A slicer for FFF 3D printers, written in Rust.");
                    ui.label(RichText::new("Based on OrcaSlicer (by SoftFever and contributors), which is based on Bambu Studio, PrusaSlicer and Slic3r. Licensed under the GNU AGPL v3.").small().color(t.text_muted));
                });
            });
            self.about = open;
        }
        if self.shortcuts {
            let mut open = true;
            Window::sized("Keyboard Shortcuts", vec2(520.0, 560.0)).show(ctx, t, &mut open, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    for (section, rows) in crate::shortcuts::sections() {
                        ui.add_space(6.0);
                        ui.label(RichText::new(crate::i18n::tr(section)).strong());
                        egui::Grid::new(section).num_columns(2).spacing([30.0, 4.0]).show(ui, |ui| {
                            for (keys, what) in rows {
                                ui.label(RichText::new(keys).monospace().color(t.accent));
                                ui.label(crate::i18n::tr(what));
                                ui.end_row();
                            }
                        });
                    }
                    ui.add_space(6.0);
                    ui.label(RichText::new(crate::i18n::tr("Mouse")).strong());
                    ui.label(crate::i18n::tr("Left mouse: rotate (or as set in Preferences › Camera); right or middle mouse: pan; wheel: zoom; Shift while rotating with the gizmo: 15° steps."));
                });
            });
            self.shortcuts = open;
        }
        if let Some(mut text) = self.go_to_layer.take() {
            let mut open = true;
            let mut done = false;
            Window::dialog("Jump to layer").show(ctx, t, &mut open, |ui| {
                let edit = ui.add(egui::TextEdit::singleline(&mut text).desired_width(120.0));
                edit.request_focus();
                if edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    done = true;
                }
                if ui.button("OK").clicked() {
                    done = true;
                }
            });
            if done {
                self.jump_to_layer = text.trim().parse::<usize>().ok();
            } else if open {
                self.go_to_layer = Some(text);
            }
        }
        if self.flushing {
            let mut open = true;
            Window::dialog("Flushing volumes").show(ctx, t, &mut open, |ui| {
                let schema = session.schema;
                let n = session.presets.project_filaments.len();
                let colours: Vec<String> = (0..n).map(|i| session.presets.filament_colour(schema, i)).collect();
                let auto = |i: usize, j: usize| {
                    if i == j {
                        0.0
                    } else {
                        CosmicSlicer_engine::auto_flush(
                            colours.get(i).map_or("#808080", String::as_str),
                            colours.get(j).map_or("#808080", String::as_str),
                        )
                    }
                };
                let stored: Vec<f64> = session
                    .presets
                    .value(schema, TabKind::Process, 0, "flush_volumes_matrix")
                    .split(',')
                    .filter_map(|v| v.trim().parse::<f64>().ok())
                    .collect();
                // Until set by hand, the table follows the colours.
                let mut table: Vec<f64> = if stored.len() == n * n && stored.iter().any(|v| *v > 0.0) {
                    stored
                } else {
                    (0..n * n).map(|k| auto(k / n.max(1), k % n.max(1))).collect()
                };
                ui.label("Flushing volume (mm³) when changing from the row's filament to the column's.");
                ui.add_space(6.0);
                let mut changed = false;
                egui::Grid::new("flush").num_columns(n + 1).spacing([8.0, 6.0]).show(ui, |ui| {
                    ui.label("");
                    for j in 0..n {
                        let c = parse_hex(colours.get(j).map_or("", String::as_str)).unwrap_or(Color32::GRAY);
                        ui.label(
                            RichText::new(format!(" {} ", j + 1))
                                .strong()
                                .background_color(c)
                                .color(crate::theme::ink_on(c)),
                        );
                    }
                    ui.end_row();
                    for i in 0..n {
                        let c = parse_hex(colours.get(i).map_or("", String::as_str)).unwrap_or(Color32::GRAY);
                        ui.label(
                            RichText::new(format!(" {} ", i + 1))
                                .strong()
                                .background_color(c)
                                .color(crate::theme::ink_on(c)),
                        );
                        for j in 0..n {
                            if i == j {
                                ui.label("—");
                            } else if let Some(v) = table.get_mut(i * n + j) {
                                changed |= ui.add(egui::DragValue::new(v).range(0.0..=5000.0).speed(5.0)).changed();
                            }
                        }
                        ui.end_row();
                    }
                });
                if n < 2 {
                    ui.label(RichText::new("Add a second filament to set flushing volumes.").color(t.text_muted));
                }
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if ui.button("Auto-calculate from colours").clicked() {
                        table = (0..n * n).map(|k| auto(k / n.max(1), k % n.max(1))).collect();
                        changed = true;
                    }
                    let mut mult: f64 = session
                        .presets
                        .value(schema, TabKind::Process, 0, "flush_multiplier")
                        .trim()
                        .parse()
                        .unwrap_or(1.0);
                    ui.label("Multiplier");
                    if ui.add(egui::DragValue::new(&mut mult).range(0.0..=3.0).speed(0.01)).changed() {
                        session.presets.set_value(
                            schema,
                            TabKind::Process,
                            0,
                            "flush_multiplier",
                            format!("{mult:.2}"),
                        );
                        session.sliced.clear();
                    }
                });
                if changed {
                    let text = table.iter().map(|v| format!("{}", v.round())).collect::<Vec<_>>().join(",");
                    session.presets.set_value(schema, TabKind::Process, 0, "flush_volumes_matrix", text);
                    session.sliced.clear();
                }
                let mut tower = session.presets.value(schema, TabKind::Process, 0, "enable_prime_tower") == "1";
                if ui.checkbox(&mut tower, "Purge into a prime tower").changed() {
                    let v = if tower { "1" } else { "0" };
                    session.presets.set_value(schema, TabKind::Process, 0, "enable_prime_tower", v.to_owned());
                    session.sliced.clear();
                }
                ui.label(
                    RichText::new("The tower's place and size are in Process › Multimaterial.")
                        .small()
                        .color(t.text_muted),
                );
            });
            self.flushing = open;
        }
        if let Some(mut d) = self.calibration.take() {
            let (mut open, mut keep) = (true, true);
            Window::dialog(format!("{} calibration", d.test.label())).show(ctx, t, &mut open, |ui| {
                calibration_body(ui, session, t, &mut d, &mut keep, &mut self.calibration_ready);
            });
            self.calibration = (open && keep).then_some(d);
        }
        if let Some((id, mut name)) = self.rename.clone() {
            let mut open = true;
            let mut done = false;
            Window::dialog("Rename").show(ctx, t, &mut open, |ui| {
                ui.set_width(320.0);
                let edit = ui.add(egui::TextEdit::singleline(&mut name).desired_width(f32::INFINITY));
                edit.request_focus();
                if edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    done = true;
                }
                if ui.button("OK").clicked() {
                    done = true;
                }
            });
            if done {
                session.rename(id, name.clone());
                open = false;
            }
            self.rename = open.then_some((id, name));
        }
        if self.print {
            let mut open = true;
            Window::dialog("Print plate").show(ctx, t, &mut open, |ui| {
                ui.set_width(440.0);
                let printers: Vec<(String, HostType)> =
                    session.prefs.printers.iter().map(|p| (p.name.clone(), p.host)).collect();
                if printers.is_empty() {
                    ui.label("No printer is set up. Export the G-code to print from a card or USB stick, or set up the printer's network connection to send plates to it.");
                } else {
                    let current = session.active_printer().map(|p| p.name.clone()).unwrap_or_default();
                    ui.horizontal(|ui| {
                        ui.label("Printer");
                        egui::ComboBox::from_id_salt("print-to").selected_text(&current).width(260.0).show_ui(ui, |ui| {
                            for (name, host) in &printers {
                                if ui.selectable_label(*name == current, format!("{name}  ({})", host.label())).clicked() {
                                    session.prefs.active_printer = name.clone();
                                }
                            }
                        });
                    });
                    ui.checkbox(&mut self.upload_only, "Upload only (start it from the printer)");
                    let host = session.active_printer().map(|p| p.host);
                    if let Some(why) = host.and_then(HostType::unsupported) {
                        ui.colored_label(t.warning, why);
                    }
                    ui.label(
                        RichText::new("The plate is sliced first if it needs to be; the G-code goes to the printer as it is.")
                            .small()
                            .color(t.text_muted),
                    );
                }
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if let Some(p) = session.active_printer().filter(|p| p.host.unsupported().is_none()) {
                        let label = if self.upload_only { "Upload" } else { "Upload and print" };
                        if ui_kit::primary_button(ui, t, label).clicked() {
                            self.send_requested = Some((p.name.clone(), !self.upload_only));
                            self.print = false;
                        }
                    }
                    if ui.button("Export G-code…").clicked() {
                        self.export_requested = true;
                        self.print = false;
                    }
                    if ui.button("Set up a printer…").clicked() {
                        self.physical_printer = true;
                        self.print = false;
                    }
                });
            });
            if !open {
                self.print = false;
            }
        }
    }
}

/// "The object seems to be in inches": convert the imported objects to millimetres, or keep
/// them as they are.
fn units_window(ctx: &egui::Context, session: &mut Session, t: &Tokens) {
    let Some(ids) = session.units_prompt.clone() else { return };
    let mut open = true;
    let mut answer = None;
    Window::dialog("Object dimensions").show(ctx, t, &mut open, |ui| {
        ui.set_width(440.0);
        let size = ids.iter().filter_map(|id| session.model.get(*id)).fold(glam::Vec3::ZERO, |a, o| a.max(o.size()));
        ui.label(format!(
            "The imported object is only {:.2} × {:.2} × {:.2} mm. It seems to have been drawn in inches.",
            size.x, size.y, size.z
        ));
        ui.add_space(4.0);
        ui.label(RichText::new("Converting scales it by 25.4.").color(t.text_muted));
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            if ui_kit::primary_button(ui, t, "Convert to millimetres").clicked() {
                answer = Some(true);
            }
            if ui.button("Keep as it is").clicked() {
                answer = Some(false);
            }
        });
    });
    if answer == Some(true) {
        session.scale_objects(&ids, 25.4);
    }
    if answer.is_some() || !open {
        session.units_prompt = None;
    }
}

/// The calibration dialog: the test's range, then (once set up) what to read off the print
/// and the result to save.
fn calibration_body(
    ui: &mut egui::Ui,
    session: &mut Session,
    t: &Tokens,
    d: &mut crate::calibration::Dialog,
    open: &mut bool,
    ready: &mut bool,
) {
    use crate::calibration::{self, Test};
    ui.set_width(440.0);
    let drag = |ui: &mut egui::Ui, v: &mut f64, unit: &str, speed: f64| {
        ui.add(egui::DragValue::new(v).speed(speed).max_decimals(4).suffix(if unit.is_empty() {
            String::new()
        } else {
            format!(" {unit}")
        }));
    };
    if !d.printed {
        ui.label("The current project is replaced by the test print, with the settings it needs changed (as edits you can see in the settings).");
        ui.add_space(6.0);
        let unit = d.test.range_unit().unwrap_or("");
        let fine =
            if matches!(d.test, Test::PaLine | Test::PaPattern | Test::PaTower | Test::ShaperDamp | Test::Cornering) {
                0.001
            } else {
                0.1
            };
        egui::Grid::new("calib-range").num_columns(2).spacing([16.0, 8.0]).show(ui, |ui| match d.test {
            Test::ShaperFreq => {
                ui.label("Frequency X (from, to)");
                ui.horizontal(|ui| {
                    drag(ui, &mut d.freq_x.0, "Hz", 1.0);
                    drag(ui, &mut d.freq_x.1, "Hz", 1.0);
                });
                ui.end_row();
                ui.label("Frequency Y (from, to)");
                ui.horizontal(|ui| {
                    drag(ui, &mut d.freq_y.0, "Hz", 1.0);
                    drag(ui, &mut d.freq_y.1, "Hz", 1.0);
                });
                ui.end_row();
                ui.label("Damping ratio");
                drag(ui, &mut d.damp, "", 0.005);
                ui.end_row();
                ui.label("Shaper type");
                ui.add(
                    egui::TextEdit::singleline(&mut d.shaper)
                        .hint_text("firmware default (e.g. mzv)")
                        .desired_width(160.0),
                );
                ui.end_row();
            }
            Test::ShaperDamp => {
                ui.label("Frequency X / Y");
                ui.horizontal(|ui| {
                    drag(ui, &mut d.freq_x.0, "Hz", 1.0);
                    drag(ui, &mut d.freq_y.0, "Hz", 1.0);
                });
                ui.end_row();
                ui.label("Damping (from, to)");
                ui.horizontal(|ui| {
                    drag(ui, &mut d.start, "", 0.005);
                    drag(ui, &mut d.end, "", 0.005);
                });
                ui.end_row();
                ui.label("Shaper type");
                ui.add(
                    egui::TextEdit::singleline(&mut d.shaper)
                        .hint_text("firmware default (e.g. mzv)")
                        .desired_width(160.0),
                );
                ui.end_row();
            }
            _ if d.test.range_unit().is_some() => {
                ui.label("Start");
                drag(ui, &mut d.start, unit, fine);
                ui.end_row();
                ui.label("End");
                drag(ui, &mut d.end, unit, fine);
                ui.end_row();
                if !matches!(d.test, Test::Temperature | Test::Cornering) {
                    ui.label("Step");
                    drag(ui, &mut d.step, unit, fine / 10.0);
                    ui.end_row();
                }
            }
            _ => {}
        });
        match d.test {
            Test::PaLine => {
                ui.checkbox(&mut d.numbers, "Print numbers");
            }
            Test::Temperature | Test::Vfa => {
                ui.checkbox(&mut d.resize, "Scale the tower to the nozzle");
            }
            Test::ShaperFreq | Test::ShaperDamp | Test::Cornering => {
                ui.horizontal(|ui| {
                    ui.label("Model");
                    ui.selectable_value(&mut d.model, 0, "Ringing tower");
                    ui.selectable_value(&mut d.model, 1, "Fast tower");
                    if d.test == Test::Cornering {
                        ui.selectable_value(&mut d.model, 2, "SCV-V2");
                    }
                });
            }
            _ => {}
        }
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if ui_kit::primary_button(ui, t, "Set up the test").clicked() {
                match calibration::set_up(session, d) {
                    Ok(()) => {
                        d.printed = true;
                        *ready = true;
                        session.notify(
                            Level::Info,
                            format!("{} is on the plate; slice and print it, then come back here.", d.test.label()),
                        );
                    }
                    Err(e) => session.notify(Level::Error, e),
                }
            }
            if ui.button("Cancel").clicked() {
                *open = false;
            }
        });
        return;
    }
    ui.label(d.test.instructions());
    ui.add_space(6.0);
    let height = matches!(d.test, Test::PaTower | Test::Retraction | Test::MaxFlow);
    if d.test != Test::Vfa {
        ui.horizontal(|ui| {
            ui.label(match d.test {
                _ if height => "Height (mm)",
                Test::ShaperFreq => "Frequency X / Y (Hz)",
                _ => "Value",
            });
            drag(ui, &mut d.result, "", if d.test == Test::Temperature { 1.0 } else { 0.001 });
            if d.test == Test::ShaperFreq {
                drag(ui, &mut d.result_y, "", 0.5);
            }
        });
    }
    let saved = calibration::result(session, d);
    if let Some((_, note)) = &saved {
        ui.label(RichText::new(format!("Saves: {note}")).color(t.text_muted));
    }
    ui.add_space(8.0);
    ui.horizontal(|ui| {
        if saved.is_some() && ui_kit::primary_button(ui, t, "Save to presets").clicked() {
            if let Some(note) = calibration::save(session, d) {
                session.notify(Level::Info, format!("{note} — set in the presets; save the preset to keep it."));
            }
            *open = false;
        }
        if ui.button("Back").clicked() {
            d.printed = false;
        }
        if ui.button("Close").clicked() {
            *open = false;
        }
    });
}

/// The Preferences window's pages, as OrcaSlicer groups them.
const PREFERENCE_PAGES: [&str; 6] = ["General", "Project", "Behavior", "Camera", "Keyboard", "Developer"];

fn pref_title(ui: &mut egui::Ui, title: &str) {
    ui.add_space(6.0);
    ui.label(RichText::new(crate::i18n::tr(title)).strong().size(14.0));
    ui.add_space(2.0);
}

/// A row: label (with tooltip), then the control.
fn pref_row(ui: &mut egui::Ui, label: &str, tip: &str, add: impl FnOnce(&mut egui::Ui)) {
    ui.horizontal(|ui| {
        let r = ui
            .allocate_ui_with_layout(vec2(230.0, 22.0), Layout::left_to_right(Align::Center), |ui| {
                ui.set_width(230.0);
                ui.add(egui::Label::new(crate::i18n::tr(label)).truncate())
            })
            .inner;
        if !tip.is_empty() {
            r.on_hover_text(crate::i18n::tr(tip));
        }
        add(ui);
    });
}

fn preferences_page(ui: &mut egui::Ui, session: &mut Session, t: &Tokens, page: usize, shortcuts: &mut bool) {
    let p = &mut session.prefs;
    match page {
        0 => {
            pref_title(ui, "Settings");
            pref_row(ui, "Language", "", |ui| {
                let langs = crate::i18n::languages();
                let shown = if p.language.is_empty() {
                    "System".to_owned()
                } else {
                    langs.iter().find(|(c, _)| *c == p.language).map_or(p.language.clone(), |(_, n)| n.clone())
                };
                egui::ComboBox::from_id_salt("pref-lang").selected_text(shown).width(200.0).show_ui(ui, |ui| {
                    ui.selectable_value(&mut p.language, String::new(), "System");
                    for (code, name) in &langs {
                        ui.selectable_value(&mut p.language, code.clone(), name);
                    }
                });
            });
            pref_row(ui, "Units", "", |ui| {
                ui.selectable_value(&mut p.imperial, false, crate::i18n::tr("Metric"));
                ui.selectable_value(&mut p.imperial, true, crate::i18n::tr("Imperial"));
            });
            pref_row(ui, "Default page", "Set the page opened on startup.", |ui| {
                ui.selectable_value(&mut p.default_page, Page::Home, crate::i18n::tr("Home"));
                ui.selectable_value(&mut p.default_page, Page::Prepare, crate::i18n::tr("Prepare"));
            });
            pref_row(ui, "Enable dark Mode", "", |ui| {
                let mut dark = p.theme == Theme::Dark;
                if ui.checkbox(&mut dark, "").changed() {
                    p.theme = if dark { Theme::Dark } else { Theme::Light };
                }
            });
            pref_row(ui, "Show all settings (Advanced)", "", |ui| {
                ui.checkbox(&mut p.advanced, "");
            });
            pref_title(ui, "Features");
            pref_row(
                ui,
                "Multi device management",
                "With this option enabled, you can send a task to multiple devices at the same time and manage multiple devices.",
                |ui| {
                    ui.checkbox(&mut p.multi_device, "");
                },
            );
        }
        1 => {
            pref_title(ui, "Project");
            pref_row(
                ui,
                "Load behavior",
                "Should printer/filament/process settings be loaded when opening a 3MF file?",
                |ui| {
                    let names = ["Load All", "Ask When Relevant", "Always Ask", "Load Geometry Only"];
                    egui::ComboBox::from_id_salt("pref-load")
                        .selected_text(crate::i18n::tr(names.get(p.load_behaviour).copied().unwrap_or("Load All")))
                        .width(200.0)
                        .show_ui(ui, |ui| {
                            for (i, n) in names.iter().enumerate() {
                                ui.selectable_value(&mut p.load_behaviour, i, crate::i18n::tr(n));
                            }
                        });
                },
            );
            pref_row(
                ui,
                "Auto backup",
                "Backup your project periodically to help with restoring from an occasional crash.",
                |ui| {
                    let mut on = p.backup_seconds > 0;
                    if ui.checkbox(&mut on, "").changed() {
                        p.backup_seconds = if on { 60 } else { 0 };
                    }
                    if on {
                        ui.add(egui::DragValue::new(&mut p.backup_seconds).range(10..=3600).suffix(" s"));
                    }
                },
            );
            pref_row(ui, "Maximum recent files", "Maximum count of recent files", |ui| {
                ui.add(egui::DragValue::new(&mut p.max_recent).range(1..=100));
            });
            pref_row(ui, "Add STL/STEP files to recent files list", "", |ui| {
                ui.checkbox(&mut p.recent_models, "");
            });
            pref_row(ui, "Recent files", "", |ui| {
                ui.label(format!("{}", p.recent.len()));
                if ui.button(crate::i18n::tr("Clear")).clicked() {
                    p.recent.clear();
                }
            });
        }
        2 => {
            pref_title(ui, "Behavior");
            pref_row(ui, "Auto arrange plate after cloning", "", |ui| {
                ui.checkbox(&mut p.arrange_after_clone, "");
            });
            pref_row(ui, "Arrange: spacing", "", |ui| {
                ui.add(egui::DragValue::new(&mut p.arrange_spacing).range(0.0..=100.0).speed(0.1).suffix(" mm"));
            });
            pref_row(ui, "Auto rotate for arrangement", "", |ui| {
                ui.checkbox(&mut p.arrange_rotate, "");
            });
            pref_title(ui, "Slicing");
            pref_row(
                ui,
                "Remember last print action",
                "If enabled, OrcaSlicer will remember the last selected option in the print button's dropdown (e.g. Print, Export plate sliced file, Export G-code file) and use it as the default on next startup.",
                |ui| {
                    ui.checkbox(&mut p.remember_print_action, "");
                },
            );
        }
        3 => {
            pref_title(ui, "Camera");
            pref_row(
                ui,
                "Camera style",
                "Select camera navigation style.\nDefault: LMB+move for rotation, RMB/MMB+move for panning.\nTouchpad: Alt+move for rotation, Shift+move for panning.",
                |ui| {
                    ui.selectable_value(&mut p.camera_style, 0, crate::i18n::tr("Default"));
                    ui.selectable_value(&mut p.camera_style, 1, crate::i18n::tr("Touchpad"));
                },
            );
            pref_row(
                ui,
                "Orbit speed multiplier",
                "Multiplies the orbit speed for finer or coarser camera movement.",
                |ui| {
                    ui.add(egui::Slider::new(&mut p.orbit_speed, 0.1..=5.0));
                },
            );
            pref_row(
                ui,
                "Zoom to mouse position",
                "Zoom in towards the mouse pointer's position in the 3D view, rather than the 2D window center.",
                |ui| {
                    ui.checkbox(&mut p.zoom_to_mouse, "");
                },
            );
            pref_row(ui, "Reverse mouse zoom", "If enabled, reverses the direction of zoom with mouse wheel.", |ui| {
                ui.checkbox(&mut p.reverse_zoom, "");
            });
            pref_row(ui, "Use perspective view", "", |ui| {
                ui.checkbox(&mut p.perspective, "");
            });
            for (label, value, id) in [
                ("Left Mouse Drag", &mut p.left_drag, "pref-left"),
                ("Middle Mouse Drag", &mut p.middle_drag, "pref-mid"),
                ("Right Mouse Drag", &mut p.right_drag, "pref-right"),
            ] {
                pref_row(ui, label, "Set the action that dragging the left mouse button should perform.", |ui| {
                    let names = ["None", "Pan", "Rotate"];
                    egui::ComboBox::from_id_salt(id)
                        .selected_text(crate::i18n::tr(names.get(*value).copied().unwrap_or("None")))
                        .show_ui(ui, |ui| {
                            for (i, n) in names.iter().enumerate() {
                                ui.selectable_value(value, i, crate::i18n::tr(n));
                            }
                        });
                });
            }
            pref_row(ui, "Show gridlines", "", |ui| {
                ui.checkbox(&mut p.show_grid, "");
            });
            pref_row(ui, "Show object labels", "", |ui| {
                ui.checkbox(&mut p.show_labels, "");
            });
            pref_title(ui, "3Dconnexion");
            pref_row(ui, "Use a 3D mouse", "", |ui| {
                ui.checkbox(&mut p.space_mouse, "");
            });
            pref_row(ui, "3D mouse speed", "", |ui| {
                ui.add(egui::Slider::new(&mut p.space_mouse_speed, 0.1..=5.0));
            });
            ui.label(RichText::new(crate::space_mouse::support_note()).small().color(t.text_muted));
        }
        4 => {
            pref_title(ui, "Keyboard");
            pref_row(ui, "Keyboard shortcuts", "", |ui| {
                if ui.button(crate::i18n::tr("Show keyboard shortcuts list")).clicked() {
                    *shortcuts = true;
                }
            });
        }
        _ => {
            pref_title(ui, "Log Level");
            pref_row(ui, "Log Level", "", |ui| {
                for level in ["error", "warn", "info", "debug"] {
                    if ui.selectable_value(&mut p.log_level, level.to_owned(), level).changed() {
                        log::set_max_level(match level {
                            "error" => log::LevelFilter::Error,
                            "info" => log::LevelFilter::Info,
                            "debug" => log::LevelFilter::Debug,
                            _ => log::LevelFilter::Warn,
                        });
                    }
                }
            });
            let data = crate::store::data_dir().map_or_else(|| "—".to_owned(), |d| d.display().to_string());
            pref_row(ui, "Data folder", "Settings, user presets, autosaves and crash reports.", |ui| {
                ui.label(RichText::new(data).small());
            });
        }
    }
}
