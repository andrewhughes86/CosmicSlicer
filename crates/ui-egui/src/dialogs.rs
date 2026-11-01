//! The windows: the Printer / Filament / Process settings editors, saving and switching
//! presets, comparing them, Preferences, plate settings, bed shape, the printer connection,
//! keyboard shortcuts and About.

use egui::{Align, Layout, RichText, Sense, Stroke, vec2};

use CosmicSlicer_config::TabKind;

use crate::icons;
use crate::pages::{DeviceState, HOST_TYPES};
use crate::settings::{self, Target};
use crate::sidebar::preset_combo;
use crate::state::{BED_TYPES, Level, Page, Session};
use crate::theme::{Theme, Tokens};
use crate::ui_kit::{self, Window};

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
    pub about: bool,
    pub shortcuts: bool,
    pub plate: Option<usize>,
    pub physical_printer: bool,
    pub flushing: bool,
    pub calibration: Option<&'static str>,
    pub rename: Option<(u64, String)>,
    /// Bed shape: (width, depth, origin x, origin y).
    pub bed_shape: Option<[f32; 4]>,
    pub print: bool,
    /// The Print dialog's "Export G-code" was clicked.
    pub export_requested: bool,
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
                            &p.name,
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
                            let target = Target { kind: ed.kind, filament: ed.filament, object: None };
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
            session.sliced = None;
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
            session.sliced = None;
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
        Window::sized("Preferences", vec2(560.0, 520.0)).show(ctx, t, &mut open, |ui| {
            let p = &mut session.prefs;
            ui.label(RichText::new("General").strong().size(14.0));
            egui::Grid::new("prefs-general").num_columns(2).spacing([24.0, 10.0]).show(ui, |ui| {
                ui.label("Theme");
                ui.horizontal(|ui| {
                    ui.selectable_value(&mut p.theme, Theme::Dark, "Dark");
                    ui.selectable_value(&mut p.theme, Theme::Light, "Light");
                });
                ui.end_row();
                ui.label("Units");
                ui.horizontal(|ui| {
                    ui.selectable_value(&mut p.imperial, false, "Metric");
                    ui.selectable_value(&mut p.imperial, true, "Imperial");
                });
                ui.end_row();
                ui.label("Show the Multi-device tab");
                ui.checkbox(&mut p.multi_device, "");
                ui.end_row();
                ui.label("Show all settings (Advanced)");
                ui.checkbox(&mut p.advanced, "");
                ui.end_row();
            });
            ui.add_space(12.0);
            ui.label(RichText::new("3D view").strong().size(14.0));
            egui::Grid::new("prefs-view").num_columns(2).spacing([24.0, 10.0]).show(ui, |ui| {
                ui.label("Zoom to mouse position");
                ui.checkbox(&mut p.zoom_to_mouse, "");
                ui.end_row();
                ui.label("Use perspective view");
                ui.checkbox(&mut p.perspective, "");
                ui.end_row();
                ui.label("Show gridlines");
                ui.checkbox(&mut p.show_grid, "");
                ui.end_row();
                ui.label("Show object labels");
                ui.checkbox(&mut p.show_labels, "");
                ui.end_row();
            });
            ui.add_space(12.0);
            ui.label(RichText::new("Files").strong().size(14.0));
            ui.horizontal(|ui| {
                ui.label(format!("{} recent file(s)", p.recent.len()));
                if ui.button("Clear").clicked() {
                    p.recent.clear();
                }
            });
        });
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

    fn printer_window(&mut self, ctx: &egui::Context, session: &mut Session, t: &Tokens, d: &mut DeviceState) {
        if !self.physical_printer {
            return;
        }
        let mut open = true;
        Window::dialog("Physical Printer").show(ctx, t, &mut open, |ui| {
            ui.set_width(460.0);
            egui::Grid::new("host").num_columns(2).spacing([20.0, 10.0]).show(ui, |ui| {
                ui.label("Host Type");
                egui::ComboBox::from_id_salt("host-type")
                    .selected_text(HOST_TYPES.get(d.host_type).copied().unwrap_or(""))
                    .width(260.0)
                    .show_ui(ui, |ui| {
                        for (i, h) in HOST_TYPES.iter().enumerate() {
                            ui.selectable_value(&mut d.host_type, i, *h);
                        }
                    });
                ui.end_row();
                ui.label("Hostname, IP or URL");
                ui.add(
                    egui::TextEdit::singleline(&mut d.host)
                        .hint_text("192.168.1.50 or printer.local")
                        .desired_width(260.0),
                );
                ui.end_row();
                ui.label("API Key / Password");
                ui.add(egui::TextEdit::singleline(&mut d.api_key).password(true).desired_width(260.0));
                ui.end_row();
            });
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                if ui.button("Test").clicked() {
                    session.notify(Level::Warning, "Printer connections arrive in a later step; nothing was sent.");
                }
                if ui.button("OK").clicked() {
                    self.physical_printer = false;
                }
            });
        });
        if !open {
            self.physical_printer = false;
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
                    for (section, rows) in SHORTCUTS {
                        ui.add_space(6.0);
                        ui.label(RichText::new(*section).strong());
                        egui::Grid::new(*section).num_columns(2).spacing([30.0, 4.0]).show(ui, |ui| {
                            for (key, what) in *rows {
                                ui.label(RichText::new(crate::menus::ctrl_text(key)).monospace().color(t.accent));
                                ui.label(*what);
                                ui.end_row();
                            }
                        });
                    }
                });
            });
            self.shortcuts = open;
        }
        if self.flushing {
            let mut open = true;
            Window::dialog("Flushing volumes").show(ctx, t, &mut open, |ui| {
                let n = session.presets.project_filaments.len();
                ui.label("Flushing volume (mm³) when changing from the row's filament to the column's.");
                ui.add_space(6.0);
                egui::Grid::new("flush").num_columns(n + 1).spacing([8.0, 6.0]).show(ui, |ui| {
                    ui.label("");
                    for j in 0..n {
                        ui.label(RichText::new(format!("{}", j + 1)).strong());
                    }
                    ui.end_row();
                    for i in 0..n {
                        ui.label(RichText::new(format!("{}", i + 1)).strong());
                        for j in 0..n {
                            ui.label(if i == j { "0".to_owned() } else { "280".to_owned() });
                        }
                        ui.end_row();
                    }
                });
                if n < 2 {
                    ui.label(RichText::new("Add a second filament to set flushing volumes.").color(t.text_muted));
                }
            });
            self.flushing = open;
        }
        if let Some(name) = self.calibration {
            let mut open = true;
            Window::dialog(format!("{name} calibration")).show(ctx, t, &mut open, |ui| {
                ui.set_width(420.0);
                ui.label(format!(
                    "The {} calibration builds a test print and slices it. It arrives with the slicing engine.",
                    name.to_lowercase()
                ));
            });
            if !open {
                self.calibration = None;
            }
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
                ui.set_width(420.0);
                ui.label("No printer is connected. Export the G-code to print from a card or USB stick, or set up the printer's connection to send plates to it.");
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui_kit::primary_button(ui, t, "Export G-code…").clicked() {
                        self.export_requested = true;
                        self.print = false;
                    }
                    if ui.button("Set up a printer…").clicked() {
                        self.physical_printer = true;
                        self.print = false;
                    }
                    if ui.button("Go to Device").clicked() {
                        session.page = Page::Device;
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

/// OrcaSlicer's keyboard shortcuts that CosmicSlicer has.
const SHORTCUTS: &[(&str, &[(&str, &str)])] = &[
    (
        "Global",
        &[
            ("Ctrl+N", "New project"),
            ("Ctrl+O", "Open project"),
            ("Ctrl+S", "Save project"),
            ("Ctrl+I", "Import model"),
            ("Ctrl+R", "Slice plate"),
            ("Ctrl+P", "Preferences"),
            ("Ctrl+1 … 5", "Home, Prepare, Preview, Device, Project"),
            ("Tab", "Switch between Prepare and Preview"),
            ("Shift+Tab", "Collapse / expand the sidebar"),
            ("?", "Show keyboard shortcuts"),
        ],
    ),
    (
        "Plater",
        &[
            ("Ctrl+A", "Select all objects"),
            ("Esc", "Deselect all / close the tool"),
            ("Del", "Delete selected"),
            ("Ctrl+Del", "Delete all"),
            ("Ctrl+C / Ctrl+V", "Copy / paste"),
            ("Ctrl+K", "Clone selected"),
            ("Ctrl+Z / Ctrl+Y", "Undo / redo"),
            ("A", "Arrange"),
            ("Q", "Auto orient"),
            ("M / R / S / F", "Move / Rotate / Scale / Lay on face"),
            ("0 … 6", "Default, top, bottom, front, rear, left, right view"),
            ("Z", "Zoom to fit"),
            ("K", "Perspective / orthographic view"),
            ("E", "Show object labels"),
            ("Arrow keys", "Move the selection 1 mm (Shift: 10 mm)"),
        ],
    ),
    ("Preview", &[("Up / Down", "Show one more / one fewer layer")]),
];
