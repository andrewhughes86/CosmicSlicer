//! The setup wizard, as OrcaSlicer's: choose printers (vendor, model, nozzles), then the
//! filaments to show.

use std::collections::BTreeSet;

use egui::{RichText, Sense, Ui, vec2};

use CosmicSlicer_config::TabKind;
use CosmicSlicer_config::library::{self, COMMON_VENDOR, Library};

use crate::state::Prefs;
use crate::theme::Tokens;
use crate::ui_kit::{self, Window};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WizardPage {
    Printers,
    Filaments,
}

pub struct Wizard {
    pub page: WizardPage,
    vendor: String,
    search: String,
    filament_search: String,
    pub printers: BTreeSet<String>,
    pub filaments: BTreeSet<String>,
    /// Filament base names offered, refreshed when the printers change.
    offered: Vec<String>,
    offered_for: BTreeSet<String>,
}

/// What the wizard ended with.
pub enum Outcome {
    Finished { printers: Vec<String>, filaments: Vec<String> },
    Cancelled,
}

impl Wizard {
    pub fn new(prefs: &Prefs, page: WizardPage) -> Wizard {
        let printers: BTreeSet<String> = crate::profiles::installed(prefs).into_iter().collect();
        let filaments: BTreeSet<String> = crate::profiles::visible(prefs).into_iter().collect();
        let vendor = printers
            .iter()
            .next()
            .and_then(|p| Library::get().vendor_of_printer(p))
            .map_or_else(|| "BBL".to_owned(), |v| v.id.clone());
        Wizard {
            page,
            vendor,
            search: String::new(),
            filament_search: String::new(),
            printers,
            filaments,
            offered: Vec::new(),
            offered_for: BTreeSet::new(),
        }
    }

    /// Shows the wizard; `Some` once it is finished or cancelled.
    pub fn show(&mut self, ctx: &egui::Context, t: &Tokens) -> Option<Outcome> {
        let mut open = true;
        let mut outcome = None;
        Window::sized("Setup Wizard", vec2(980.0, 680.0)).show(ctx, t, &mut open, |ui| {
            ui.horizontal(|ui| {
                for (page, label) in [(WizardPage::Printers, "1  Printers"), (WizardPage::Filaments, "2  Filaments")] {
                    let on = self.page == page;
                    if ui.selectable_label(on, RichText::new(label).strong().size(14.0)).clicked() {
                        self.page = page;
                    }
                }
            });
            ui.separator();
            let footer = 40.0;
            let height = (ui.available_height() - footer).max(200.0);
            ui.allocate_ui(vec2(ui.available_width(), height), |ui| match self.page {
                WizardPage::Printers => self.printers_page(ui, t),
                WizardPage::Filaments => self.filaments_page(ui, t),
            });
            ui.separator();
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(format!(
                        "{} printer(s), {} filament(s) selected",
                        self.printers.len(),
                        self.filaments.len()
                    ))
                    .color(t.text_muted),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let can_finish = !self.printers.is_empty() && !self.filaments.is_empty();
                    if self.page == WizardPage::Printers {
                        if ui_kit::primary_button(ui, t, "Next").clicked() {
                            self.page = WizardPage::Filaments;
                        }
                    } else {
                        let finish = ui.add_enabled(can_finish, egui::Button::new(RichText::new("Finish").strong()));
                        if finish.clicked() {
                            outcome = Some(Outcome::Finished {
                                printers: self.printers.iter().cloned().collect(),
                                filaments: self.filaments.iter().cloned().collect(),
                            });
                        }
                        if ui.button("Back").clicked() {
                            self.page = WizardPage::Printers;
                        }
                    }
                    if ui.button("Cancel").clicked() {
                        outcome = Some(Outcome::Cancelled);
                    }
                });
            });
        });
        if !open && outcome.is_none() {
            outcome = Some(Outcome::Cancelled);
        }
        outcome
    }

    fn printers_page(&mut self, ui: &mut Ui, t: &Tokens) {
        let lib = Library::get();
        ui.horizontal(|ui| {
            ui.label("Search printers");
            ui.add(
                egui::TextEdit::singleline(&mut self.search)
                    .hint_text("e.g. X1 Carbon, Ender-3, Voron")
                    .desired_width(320.0),
            );
        });
        ui.add_space(6.0);
        let query = self.search.trim().to_lowercase();
        let height = ui.available_height();
        ui.horizontal_top(|ui| {
            // Vendors down the left.
            ui.vertical(|ui| {
                ui.set_width(200.0);
                egui::ScrollArea::vertical().id_salt("wizard-vendors").max_height(height).show(ui, |ui| {
                    for v in lib.vendors.iter().filter(|v| v.id != COMMON_VENDOR && !v.printers.is_empty()) {
                        let chosen = v.printers.iter().filter(|p| self.printers.contains(&p.name)).count();
                        let label = if chosen > 0 {
                            format!("{}  ({chosen})", Library::display_name(v))
                        } else {
                            Library::display_name(v).to_owned()
                        };
                        if ui.selectable_label(self.vendor == v.id && query.is_empty(), label).clicked() {
                            self.vendor = v.id.clone();
                            self.search.clear();
                        }
                    }
                });
            });
            ui.separator();
            // Models with their nozzles.
            egui::ScrollArea::vertical().id_salt("wizard-models").max_height(height).show(ui, |ui| {
                ui.set_width(ui.available_width());
                let vendor = self.vendor.clone();
                for v in
                    lib.vendors.iter().filter(|v| if query.is_empty() { v.id == vendor } else { v.id != COMMON_VENDOR })
                {
                    let models: Vec<&library::ModelInfo> = v
                        .models
                        .iter()
                        .filter(|m| query.is_empty() || m.name.to_lowercase().contains(&query))
                        .filter(|m| v.printers.iter().any(|p| p.model == m.name))
                        .collect();
                    if models.is_empty() {
                        continue;
                    }
                    ui.label(RichText::new(Library::display_name(v)).strong().size(15.0));
                    ui.add_space(4.0);
                    let cols = ((ui.available_width() + 10.0) / 230.0).floor().max(1.0) as usize;
                    egui::Grid::new(("wizard-grid", &v.id)).spacing([10.0, 10.0]).show(ui, |ui| {
                        for (i, m) in models.iter().enumerate() {
                            self.model_card(ui, t, v, m);
                            if (i + 1) % cols == 0 {
                                ui.end_row();
                            }
                        }
                    });
                    ui.add_space(12.0);
                }
            });
        });
    }

    fn model_card(&mut self, ui: &mut Ui, t: &Tokens, v: &library::VendorInfo, m: &library::ModelInfo) {
        let variants: Vec<&library::PrinterInfo> = v.printers.iter().filter(|p| p.model == m.name).collect();
        let any = variants.iter().any(|p| self.printers.contains(&p.name));
        ui_kit::frame(ui_kit::Surface::Card, t)
            .stroke(egui::Stroke::new(1.0, if any { t.accent } else { t.border }))
            .inner_margin(egui::Margin::same(10))
            .show(ui, |ui| {
                ui.set_width(200.0);
                // Grid cells lay out sideways; a card stacks its name over its nozzles.
                ui.vertical(|ui| {
                    ui.horizontal(|ui| {
                        let (r, _) = ui.allocate_exact_size(vec2(28.0, 28.0), Sense::hover());
                        crate::icons::paint(ui, r, "printer_thumbnail", t, true);
                        ui.add(egui::Label::new(RichText::new(&m.name).strong()).wrap());
                    });
                    ui.horizontal_wrapped(|ui| {
                        for p in variants {
                            let label = if p.variant.is_empty() { p.name.clone() } else { format!("{} mm", p.variant) };
                            let mut on = self.printers.contains(&p.name);
                            if ui.checkbox(&mut on, label).on_hover_text(&p.name).changed() {
                                if on {
                                    self.printers.insert(p.name.clone());
                                } else {
                                    self.printers.remove(&p.name);
                                }
                            }
                        }
                    });
                });
            });
    }

    fn filaments_page(&mut self, ui: &mut Ui, t: &Tokens) {
        if self.offered_for != self.printers {
            self.offered = offered_filaments(&self.printers);
            self.offered_for = self.printers.clone();
        }
        ui.horizontal(|ui| {
            ui.label("Search filaments");
            ui.add(
                egui::TextEdit::singleline(&mut self.filament_search)
                    .hint_text("e.g. PLA, PETG, Polymaker")
                    .desired_width(320.0),
            );
            if ui.button("Select shown").clicked() {
                let q = self.filament_search.trim().to_lowercase();
                for f in self.offered.iter().filter(|f| f.to_lowercase().contains(&q)) {
                    self.filaments.insert(f.clone());
                }
            }
            if ui.button("Clear shown").clicked() {
                let q = self.filament_search.trim().to_lowercase();
                for f in self.offered.iter().filter(|f| f.to_lowercase().contains(&q)) {
                    self.filaments.remove(f);
                }
            }
        });
        ui.label(
            RichText::new("Filaments made for the chosen printers, and OrcaSlicer's filament library.")
                .small()
                .color(t.text_muted),
        );
        ui.add_space(6.0);
        let q = self.filament_search.trim().to_lowercase();
        let shown: Vec<String> = self.offered.iter().filter(|f| f.to_lowercase().contains(&q)).cloned().collect();
        egui::ScrollArea::vertical().id_salt("wizard-filaments").show(ui, |ui| {
            ui.set_width(ui.available_width());
            let cols = ((ui.available_width() + 10.0) / 300.0).floor().max(1.0) as usize;
            let mut brand = String::new();
            let mut col = 0;
            egui::Grid::new("wizard-filament-grid").spacing([16.0, 4.0]).show(ui, |ui| {
                for f in &shown {
                    let b = f.split_whitespace().next().unwrap_or("").to_owned();
                    if b != brand {
                        if col != 0 {
                            ui.end_row();
                        }
                        ui.label(RichText::new(&b).strong().color(t.accent));
                        ui.end_row();
                        brand = b;
                        col = 0;
                    }
                    let mut on = self.filaments.contains(f);
                    if ui.checkbox(&mut on, f.as_str()).changed() {
                        if on {
                            self.filaments.insert(f.clone());
                        } else {
                            self.filaments.remove(f);
                        }
                    }
                    col += 1;
                    if col == cols {
                        ui.end_row();
                        col = 0;
                    }
                }
            });
        });
    }
}

/// The filament base names for `printers`' vendors and the common library.
fn offered_filaments(printers: &BTreeSet<String>) -> Vec<String> {
    let lib = Library::get();
    let mut vendors: Vec<String> =
        printers.iter().filter_map(|p| lib.vendor_of_printer(p).map(|v| v.id.clone())).collect();
    vendors.push(COMMON_VENDOR.to_owned());
    vendors.sort();
    vendors.dedup();
    let mut names: BTreeSet<String> = BTreeSet::new();
    for id in vendors {
        for n in lib.names(&id, TabKind::Filament) {
            names.insert(library::base_name(&n).to_owned());
        }
    }
    names.into_iter().collect()
}
