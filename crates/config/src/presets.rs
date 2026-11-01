//! Presets: named sets of values for a printer, a filament or a process, and the edits
//! made to the selected one.
//!
//! System presets come from OrcaSlicer's library ([`crate::library`]): the printers the user
//! installed in the setup wizard, and the processes and filaments made for them. Filaments and
//! processes list only when they are compatible with the selected printer, by OrcaSlicer's
//! `compatible_printers` and `compatible_printers_condition`.

use std::collections::{HashMap, HashSet};

use crate::Config;
use crate::expr::{self, ConfigVars};
use crate::library::{self, Library};
use crate::schema::{Schema, TabKind};

pub type PresetKind = TabKind;

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Preset {
    pub name: String,
    /// Shipped with the program (read-only) rather than saved by the user.
    pub system: bool,
    /// Group heading in the preset list ("System presets", a vendor, "User presets").
    pub vendor: String,
    pub config: Config,
    /// The system preset a user preset was made from ("" when detached or a system preset).
    #[serde(default)]
    pub inherits: String,
    /// Printer presets it is for; empty means any that meet `condition`.
    #[serde(default)]
    pub compatible_printers: Vec<String>,
    /// OrcaSlicer's `compatible_printers_condition`.
    #[serde(default)]
    pub condition: String,
}

impl Preset {
    fn system(name: &str, vendor: &str, values: &[(&str, &str)]) -> Self {
        let mut config = Config::default();
        for (k, v) in values {
            config.set(*k, *v);
        }
        Self {
            name: name.to_owned(),
            system: true,
            vendor: vendor.to_owned(),
            config,
            inherits: String::new(),
            compatible_printers: Vec::new(),
            condition: String::new(),
        }
    }

    fn from_library(p: library::SystemPreset, vendor_name: &str) -> Self {
        Self {
            name: p.name,
            system: true,
            vendor: vendor_name.to_owned(),
            config: p.config,
            inherits: String::new(),
            compatible_printers: p.compatible_printers,
            condition: p.condition,
        }
    }

    /// Whether this filament or process suits `printer`.
    pub fn suits(&self, printer: &Preset, schema: &Schema) -> bool {
        if !self.compatible_printers.is_empty() {
            let named = self
                .compatible_printers
                .iter()
                .any(|p| *p == printer.name || (!printer.inherits.is_empty() && *p == printer.inherits));
            if !named {
                return false;
            }
        }
        if self.condition.trim().is_empty() {
            return true;
        }
        let vars = ConfigVars { schema, config: &printer.config, extra: HashMap::new() };
        // A condition that cannot be evaluated does not match: never offer a filament or
        // process that may be wrong for the printer.
        expr::condition(&self.condition, &vars).unwrap_or(false)
    }
}

/// The selected preset of one kind and the unsaved edits over it.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Slot {
    pub preset: usize,
    pub edits: Config,
}

impl Slot {
    pub fn is_modified(&self) -> bool {
        !self.edits.values.is_empty()
    }
}

/// Every preset, the selections and the project's filament list.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct PresetBundle {
    pub printers: Vec<Preset>,
    pub filaments: Vec<Preset>,
    pub processes: Vec<Preset>,
    pub printer: Slot,
    pub process: Slot,
    /// One slot per filament in the project (extruder 1, 2, ...).
    pub project_filaments: Vec<Slot>,
}

impl Default for PresetBundle {
    fn default() -> Self {
        Self::builtin()
    }
}

/// Most filaments in a project, as in OrcaSlicer.
pub const MAX_FILAMENTS: usize = 64;

impl PresetBundle {
    pub fn builtin() -> Self {
        let printer = |name: &str, vendor: &str, w: u32, d: u32, h: u32, flavor: &str| {
            let area = format!("0x0,{w}x0,{w}x{d},0x{d}");
            Preset::system(
                name,
                vendor,
                &[
                    ("printable_area", &area),
                    ("printable_height", &h.to_string()),
                    ("nozzle_diameter", "0.4"),
                    ("gcode_flavor", flavor),
                    ("printer_model", name.trim_end_matches(" 0.4 nozzle")),
                ],
            )
        };
        let printers = vec![
            printer("Bambu Lab X1 Carbon 0.4 nozzle", "Bambu Lab", 256, 256, 250, "marlin"),
            printer("Creality Ender-3 V3 0.4 nozzle", "Creality", 220, 220, 250, "klipper"),
            printer("Prusa MK4 0.4 nozzle", "Prusa", 250, 210, 220, "marlin2"),
            printer("Voron 2.4 350 0.4 nozzle", "Voron", 350, 350, 340, "klipper"),
            printer("Generic Klipper Printer 0.4 nozzle", "Custom Printer", 235, 235, 250, "klipper"),
            printer("Generic Marlin Printer 0.4 nozzle", "Custom Printer", 220, 220, 250, "marlin2"),
        ];
        // (name, type, colour, nozzle °C, bed °C, max flow mm³/s, density g/cm³, cost per kg, fan %)
        let filament = |name: &str,
                        kind: &str,
                        colour: &str,
                        nozzle: &str,
                        bed: &str,
                        flow: &str,
                        density: &str,
                        cost: &str,
                        fan: &str| {
            Preset::system(
                name,
                "Generic",
                &[
                    ("filament_type", kind),
                    ("default_filament_colour", colour),
                    ("filament_colour", colour),
                    ("nozzle_temperature", nozzle),
                    ("nozzle_temperature_initial_layer", nozzle),
                    ("hot_plate_temp", bed),
                    ("hot_plate_temp_initial_layer", bed),
                    ("textured_plate_temp", bed),
                    ("textured_plate_temp_initial_layer", bed),
                    ("filament_max_volumetric_speed", flow),
                    ("filament_density", density),
                    ("filament_cost", cost),
                    ("fan_min_speed", fan),
                    ("cool_plate_temp", bed),
                    ("cool_plate_temp_initial_layer", bed),
                    ("eng_plate_temp", bed),
                    ("eng_plate_temp_initial_layer", bed),
                    ("textured_cool_plate_temp", bed),
                    ("textured_cool_plate_temp_initial_layer", bed),
                    ("supertack_plate_temp", bed),
                    ("supertack_plate_temp_initial_layer", bed),
                    ("filament_vendor", "Generic"),
                ],
            )
        };
        let filaments = vec![
            filament("Generic PLA", "PLA", "#D1D5DB", "220", "55", "12", "1.24", "20", "100"),
            filament("Generic PLA Silk", "PLA", "#C084FC", "225", "55", "7.5", "1.32", "25", "100"),
            filament("Generic PETG", "PETG", "#F59E0B", "250", "70", "10", "1.27", "25", "30"),
            filament("Generic ABS", "ABS", "#F1F5F9", "260", "100", "16", "1.04", "20", "10"),
            filament("Generic ASA", "ASA", "#94A3B8", "260", "100", "18", "1.07", "25", "10"),
            filament("Generic TPU", "TPU", "#34D399", "230", "35", "3.6", "1.21", "35", "100"),
            filament("Generic PA-CF", "PA-CF", "#334155", "290", "100", "8", "1.12", "60", "0"),
            filament("Generic PC", "PC", "#E2E8F0", "270", "110", "12", "1.20", "40", "0"),
        ];
        let process = |name: &str, layer: &str, walls: &str, top: &str, bottom: &str| {
            Preset::system(
                name,
                "System presets",
                &[
                    ("layer_height", layer),
                    ("wall_loops", walls),
                    ("top_shell_layers", top),
                    ("bottom_shell_layers", bottom),
                ],
            )
        };
        let processes = vec![
            process("0.08mm Extra Fine @Generic", "0.08", "2", "9", "7"),
            process("0.12mm Fine @Generic", "0.12", "2", "7", "5"),
            process("0.16mm Optimal @Generic", "0.16", "2", "6", "4"),
            process("0.20mm Standard @Generic", "0.2", "2", "5", "3"),
            process("0.24mm Draft @Generic", "0.24", "2", "4", "3"),
            process("0.28mm Extra Draft @Generic", "0.28", "2", "4", "3"),
        ];
        Self {
            printers,
            filaments,
            processes,
            printer: Slot { preset: 4, ..Default::default() },
            process: Slot { preset: 3, ..Default::default() },
            project_filaments: vec![Slot::default()],
        }
    }

    /// The presets for the printers installed in the setup wizard (`installed`, printer preset
    /// names) and the filaments made visible there (`visible`, base names such as
    /// "Generic PLA"). `None` when none of the printers is in the library.
    pub fn from_library(installed: &[String], visible: &[String]) -> Option<PresetBundle> {
        let lib = Library::get();
        let mut printers = Vec::new();
        let mut vendors: Vec<String> = Vec::new();
        for name in installed {
            let Some(v) = lib.vendor_of_printer(name) else { continue };
            if let Some(p) = lib.resolve(TabKind::Printer, name, &v.id) {
                printers.push(Preset::from_library(p, Library::display_name(v)));
                if !vendors.contains(&v.id) {
                    vendors.push(v.id.clone());
                }
            }
        }
        if printers.is_empty() {
            return None;
        }
        let schema = Schema::get();
        let suits_any = |p: &Preset| printers.iter().any(|pr| p.suits(pr, schema));
        let mut processes = Vec::new();
        for id in &vendors {
            let display = lib.vendor_info(id).map_or(id.as_str(), Library::display_name).to_owned();
            for p in lib.resolve_all(TabKind::Process, id) {
                let preset = Preset::from_library(p, &display);
                if suits_any(&preset) {
                    processes.push(preset);
                }
            }
        }
        // Filaments: only the visible ones are resolved, so the bundle stays small.
        let visible: HashSet<&str> = visible.iter().map(String::as_str).collect();
        let mut filaments = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        let mut ids = vendors.clone();
        ids.push(library::COMMON_VENDOR.to_owned());
        for id in &ids {
            for name in lib.names(id, TabKind::Filament) {
                if !visible.contains(library::base_name(&name)) || !seen.insert(name.clone()) {
                    continue;
                }
                let Some(p) = lib.resolve(TabKind::Filament, &name, id) else { continue };
                let vendor = p.config.values.get("filament_vendor").cloned().unwrap_or_else(|| "Generic".into());
                let preset = Preset::from_library(p, &vendor);
                if suits_any(&preset) {
                    filaments.push(preset);
                }
            }
        }
        if processes.is_empty() || filaments.is_empty() {
            return None;
        }
        let order = |a: &Preset, b: &Preset| a.vendor.cmp(&b.vendor).then_with(|| a.name.cmp(&b.name));
        processes.sort_by(order);
        filaments.sort_by(order);
        let mut bundle = PresetBundle {
            printers,
            filaments,
            processes,
            printer: Slot::default(),
            process: Slot::default(),
            project_filaments: vec![Slot::default()],
        };
        // Start on the printer's own defaults, as OrcaSlicer does.
        let printer = bundle.current_printer().map(|p| p.config.clone()).unwrap_or_default();
        let wanted_process = printer.get(schema, "default_print_profile").to_owned();
        let wanted_filament =
            printer.get(schema, "default_filament_profile").split(',').next().unwrap_or("").trim().to_owned();
        let ok_process = bundle.compatible(TabKind::Process);
        let ok_filament = bundle.compatible(TabKind::Filament);
        let pick = |list: &[Preset], ok: &[usize], wanted: &str, fallback: &str| {
            ok.iter()
                .copied()
                .find(|&i| list.get(i).is_some_and(|p| p.name == wanted))
                .or_else(|| ok.iter().copied().find(|&i| list.get(i).is_some_and(|p| p.name.contains(fallback))))
                .or_else(|| ok.first().copied())
                .unwrap_or(0)
        };
        bundle.process.preset = pick(&bundle.processes, &ok_process, &wanted_process, "0.20mm Standard");
        let f = pick(&bundle.filaments, &ok_filament, &wanted_filament, "Generic PLA");
        bundle.project_filaments = vec![Slot { preset: f, ..Default::default() }];
        bundle.fix_selections();
        Some(bundle)
    }

    /// The selected printer.
    pub fn current_printer(&self) -> Option<&Preset> {
        self.printers.get(self.printer.preset)
    }

    /// Whether preset `index` of `kind` suits the selected printer (printers always do).
    pub fn is_compatible(&self, kind: PresetKind, index: usize) -> bool {
        if kind == TabKind::Printer {
            return true;
        }
        let (Some(printer), Some(p)) = (self.current_printer(), self.list(kind).get(index)) else { return false };
        p.suits(printer, Schema::get())
    }

    /// Indices of the presets of `kind` that suit the selected printer.
    pub fn compatible(&self, kind: PresetKind) -> Vec<usize> {
        (0..self.list(kind).len()).filter(|&i| self.is_compatible(kind, i)).collect()
    }

    /// After a printer change: a process and filaments that suit it. The printer's own default
    /// process wins; a filament switches to the same filament made for this printer when there
    /// is one ("Bambu PLA Basic @BBL X1C" → "@BBL P1P").
    pub fn fix_selections(&mut self) {
        let schema = Schema::get();
        let default_process = self
            .current_printer()
            .map(|p| p.config.get(schema, "default_print_profile").to_owned())
            .unwrap_or_default();
        if !self.is_compatible(TabKind::Process, self.process.preset) {
            let ok = self.compatible(TabKind::Process);
            let pick = ok
                .iter()
                .copied()
                .find(|&i| self.processes.get(i).is_some_and(|p| p.name == default_process))
                .or_else(|| {
                    ok.iter()
                        .copied()
                        .find(|&i| self.processes.get(i).is_some_and(|p| p.name.contains("0.20mm Standard")))
                })
                .or_else(|| ok.first().copied());
            if let Some(i) = pick {
                self.process = Slot { preset: i, edits: Config::default() };
            }
        }
        let ok = self.compatible(TabKind::Filament);
        for f in 0..self.project_filaments.len() {
            let Some(slot) = self.project_filaments.get(f) else { continue };
            if self.is_compatible(TabKind::Filament, slot.preset) {
                continue;
            }
            let base =
                self.filaments.get(slot.preset).map(|p| library::base_name(&p.name).to_owned()).unwrap_or_default();
            let pick = ok
                .iter()
                .copied()
                .find(|&i| self.filaments.get(i).is_some_and(|p| library::base_name(&p.name) == base))
                .or_else(|| {
                    ok.iter().copied().find(|&i| self.filaments.get(i).is_some_and(|p| p.name.contains("Generic PLA")))
                })
                .or_else(|| ok.first().copied());
            if let (Some(i), Some(slot)) = (pick, self.project_filaments.get_mut(f)) {
                *slot = Slot { preset: i, edits: Config::default() };
            }
        }
    }

    /// Adds (or replaces, by name) a user preset; returns its index.
    pub fn add_user(&mut self, kind: PresetKind, preset: Preset) -> usize {
        let list = self.list_mut(kind);
        match list.iter().position(|p| p.name == preset.name && !p.system) {
            Some(i) => {
                if let Some(slot) = list.get_mut(i) {
                    *slot = preset;
                }
                i
            }
            None => {
                list.push(preset);
                list.len() - 1
            }
        }
    }

    /// Renames user preset `index`; system presets cannot be renamed.
    pub fn rename(&mut self, kind: PresetKind, index: usize, name: &str) -> Result<(), String> {
        let name = name.trim();
        if name.is_empty() {
            return Err("the preset needs a name".into());
        }
        if self.list(kind).iter().enumerate().any(|(i, p)| i != index && p.name == name) {
            return Err(format!("there is already a preset called \"{name}\""));
        }
        match self.list_mut(kind).get_mut(index) {
            Some(p) if !p.system => {
                p.name = name.to_owned();
                Ok(())
            }
            Some(_) => Err("system presets cannot be renamed".into()),
            None => Err("no such preset".into()),
        }
    }

    /// Deletes user preset `index`; selections move to a compatible preset.
    pub fn delete(&mut self, kind: PresetKind, index: usize) -> Result<Preset, String> {
        if self.list(kind).get(index).is_none_or(|p| p.system) {
            return Err("system presets cannot be deleted".into());
        }
        if kind == TabKind::Printer
            && self.printers.iter().filter(|p| p.system).count() == 0
            && self.printers.len() == 1
        {
            return Err("the last printer cannot be deleted".into());
        }
        let removed = self.list_mut(kind).remove(index);
        let shift = |slot: &mut Slot| {
            if slot.preset == index {
                *slot = Slot::default();
            } else if slot.preset > index {
                slot.preset -= 1;
            }
        };
        match kind {
            TabKind::Printer => shift(&mut self.printer),
            TabKind::Process => shift(&mut self.process),
            TabKind::Filament => self.project_filaments.iter_mut().for_each(shift),
        }
        self.fix_selections();
        Ok(removed)
    }

    /// Cuts a user preset loose from its system parent.
    pub fn detach(&mut self, kind: PresetKind, index: usize) {
        if let Some(p) = self.list_mut(kind).get_mut(index).filter(|p| !p.system) {
            p.inherits.clear();
        }
    }

    pub fn list(&self, kind: PresetKind) -> &[Preset] {
        match kind {
            TabKind::Printer => &self.printers,
            TabKind::Filament => &self.filaments,
            TabKind::Process => &self.processes,
        }
    }

    fn list_mut(&mut self, kind: PresetKind) -> &mut Vec<Preset> {
        match kind {
            TabKind::Printer => &mut self.printers,
            TabKind::Filament => &mut self.filaments,
            TabKind::Process => &mut self.processes,
        }
    }

    /// The slot for `kind`; filaments use `filament` (0-based project filament).
    pub fn slot(&self, kind: PresetKind, filament: usize) -> Option<&Slot> {
        match kind {
            TabKind::Printer => Some(&self.printer),
            TabKind::Process => Some(&self.process),
            TabKind::Filament => self.project_filaments.get(filament),
        }
    }

    pub fn slot_mut(&mut self, kind: PresetKind, filament: usize) -> Option<&mut Slot> {
        match kind {
            TabKind::Printer => Some(&mut self.printer),
            TabKind::Process => Some(&mut self.process),
            TabKind::Filament => self.project_filaments.get_mut(filament),
        }
    }

    pub fn preset(&self, kind: PresetKind, filament: usize) -> Option<&Preset> {
        self.list(kind).get(self.slot(kind, filament)?.preset)
    }

    /// The selected preset's values with the edits over them.
    pub fn effective(&self, kind: PresetKind, filament: usize) -> Config {
        let (Some(slot), Some(preset)) = (self.slot(kind, filament), self.preset(kind, filament)) else {
            return Config::default();
        };
        slot.edits.overlaid(&preset.config)
    }

    /// One value as the UI shows it: edit, else preset, else default.
    pub fn value(&self, schema: &Schema, kind: PresetKind, filament: usize, key: &str) -> String {
        let Some(slot) = self.slot(kind, filament) else { return schema.default_of(key).to_owned() };
        if let Some(v) = slot.edits.values.get(key) {
            return v.clone();
        }
        self.preset(kind, filament)
            .map_or_else(|| schema.default_of(key).to_owned(), |p| p.config.get(schema, key).to_owned())
    }

    /// Edits `key`; setting it back to the preset's value drops the edit.
    pub fn set_value(&mut self, schema: &Schema, kind: PresetKind, filament: usize, key: &str, value: String) {
        let base = self.preset(kind, filament).map(|p| p.config.get(schema, key).to_owned());
        let Some(slot) = self.slot_mut(kind, filament) else { return };
        if base.as_deref() == Some(value.as_str()) {
            slot.edits.values.remove(key);
        } else {
            slot.edits.set(key, value);
        }
    }

    pub fn is_edited(&self, kind: PresetKind, filament: usize, key: &str) -> bool {
        self.slot(kind, filament).is_some_and(|s| s.edits.values.contains_key(key))
    }

    pub fn revert(&mut self, kind: PresetKind, filament: usize, key: &str) {
        if let Some(slot) = self.slot_mut(kind, filament) {
            slot.edits.values.remove(key);
        }
    }

    /// Selects preset `index`, dropping unsaved edits.
    pub fn select(&mut self, kind: PresetKind, filament: usize, index: usize) {
        let len = self.list(kind).len();
        if let Some(slot) = self.slot_mut(kind, filament)
            && index < len
        {
            *slot = Slot { preset: index, edits: Default::default() };
        }
        if kind == TabKind::Printer {
            self.fix_selections();
        }
    }

    /// Saves the edits of `kind` as a user preset called `name` (replacing a user preset of
    /// that name) and selects it.
    pub fn save_as(&mut self, kind: PresetKind, filament: usize, name: &str) -> Result<(), String> {
        let name = name.trim();
        if name.is_empty() {
            return Err("the preset needs a name".into());
        }
        if self.list(kind).iter().any(|p| p.system && p.name == name) {
            return Err(format!("\"{name}\" is a system preset; choose another name"));
        }
        let config = self.effective(kind, filament);
        let parent = self.preset(kind, filament).cloned();
        let inherits =
            parent.as_ref().map_or_else(String::new, |p| if p.system { p.name.clone() } else { p.inherits.clone() });
        let list = self.list_mut(kind);
        let preset = Preset {
            name: name.to_owned(),
            system: false,
            vendor: "User presets".into(),
            config,
            inherits,
            compatible_printers: parent.as_ref().map(|p| p.compatible_printers.clone()).unwrap_or_default(),
            condition: parent.map(|p| p.condition).unwrap_or_default(),
        };
        let index = match list.iter().position(|p| p.name == name) {
            Some(i) => {
                if let Some(slot) = list.get_mut(i) {
                    *slot = preset;
                }
                i
            }
            None => {
                list.push(preset);
                list.len() - 1
            }
        };
        self.select(kind, filament, index);
        Ok(())
    }

    pub fn add_filament(&mut self) {
        if self.project_filaments.len() < MAX_FILAMENTS {
            // The next compatible filament, round the list.
            let ok = self.compatible(TabKind::Filament);
            let pool = if ok.is_empty() { (0..self.filaments.len()).collect() } else { ok };
            let next = pool.get(self.project_filaments.len() % pool.len().max(1)).copied().unwrap_or(0);
            self.project_filaments.push(Slot { preset: next, ..Default::default() });
        }
    }

    pub fn remove_filament(&mut self) {
        if self.project_filaments.len() > 1 {
            self.project_filaments.pop();
        }
    }

    /// Colour of project filament `index` as "#RRGGBB".
    pub fn filament_colour(&self, schema: &Schema, index: usize) -> String {
        self.value(schema, TabKind::Filament, index, "filament_colour")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edits_track_the_preset() {
        let s = Schema::get();
        let mut b = PresetBundle::builtin();
        assert_eq!(b.value(s, TabKind::Process, 0, "layer_height"), "0.2");
        b.set_value(s, TabKind::Process, 0, "layer_height", "0.3".into());
        assert!(b.process.is_modified());
        b.set_value(s, TabKind::Process, 0, "layer_height", "0.2".into());
        assert!(!b.process.is_modified());
        b.set_value(s, TabKind::Process, 0, "wall_loops", "4".into());
        assert!(b.save_as(TabKind::Process, 0, "0.20mm Standard @Generic").is_err());
        assert!(b.save_as(TabKind::Process, 0, "Strong").is_ok());
        assert_eq!(b.preset(TabKind::Process, 0).map(|p| p.name.as_str()), Some("Strong"));
        assert_eq!(b.value(s, TabKind::Process, 0, "wall_loops"), "4");
        assert!(!b.process.is_modified());
    }

    fn names(b: &PresetBundle, kind: TabKind) -> Vec<String> {
        b.compatible(kind).into_iter().filter_map(|i| b.list(kind).get(i).map(|p| p.name.clone())).collect()
    }

    #[test]
    fn library_bundles_filter_by_printer() {
        let installed = vec![
            "Bambu Lab X1 Carbon 0.4 nozzle".to_owned(),
            "Bambu Lab A1 0.4 nozzle".to_owned(),
            "MyKlipper 0.4 nozzle".to_owned(),
        ];
        let visible = vec!["Bambu PLA Basic".to_owned(), "Generic PLA".to_owned(), "Generic PETG".to_owned()];
        let b = PresetBundle::from_library(&installed, &visible);
        assert!(b.is_some());
        let Some(mut b) = b else { return };
        assert_eq!(b.printers.len(), 3);
        // The X1C is selected first: its own filaments and processes only.
        let fil = names(&b, TabKind::Filament);
        assert!(fil.iter().any(|n| n == "Bambu PLA Basic @BBL X1C"), "{fil:?}");
        assert!(!fil.iter().any(|n| n.contains("@BBL A1")), "{fil:?}");
        let proc_names = names(&b, TabKind::Process);
        assert!(!proc_names.is_empty() && proc_names.iter().all(|n| !n.contains("@BBL A1")), "{proc_names:?}");
        let selected_process = b.preset(TabKind::Process, 0).map(|p| p.name.clone()).unwrap_or_default();
        assert!(selected_process.contains("X1C"), "{selected_process}");
        // Pick Bambu PLA Basic, then switch to the A1: the A1's version of it is chosen.
        let x1c_pla = b.filaments.iter().position(|p| p.name == "Bambu PLA Basic @BBL X1C");
        assert!(x1c_pla.is_some());
        b.select(TabKind::Filament, 0, x1c_pla.unwrap_or(0));
        let a1 = b.printers.iter().position(|p| p.name == "Bambu Lab A1 0.4 nozzle").unwrap_or(0);
        b.select(TabKind::Printer, 0, a1);
        let now = b.preset(TabKind::Filament, 0).map(|p| p.name.clone()).unwrap_or_default();
        assert_eq!(now, "Bambu PLA Basic @BBL A1");
        assert!(b.preset(TabKind::Process, 0).is_some_and(|p| p.name.contains("A1")));
        // A generic Klipper printer: generic filaments, no Bambu ones.
        let k = b.printers.iter().position(|p| p.name == "MyKlipper 0.4 nozzle").unwrap_or(0);
        b.select(TabKind::Printer, 0, k);
        let fil = names(&b, TabKind::Filament);
        assert!(fil.iter().any(|n| n.starts_with("Generic PLA")) && !fil.iter().any(|n| n.contains("@BBL")), "{fil:?}");
    }

    #[test]
    fn conditions_pick_prusa_filaments() {
        let b = PresetBundle::from_library(
            &["Prusa MK4S 0.4 nozzle".to_owned()],
            &["Prusament PLA".to_owned(), "Generic PLA".to_owned()],
        );
        assert!(b.is_some());
        let Some(b) = b else { return };
        let fil = names(&b, TabKind::Filament);
        assert!(fil.iter().any(|n| n.starts_with("Prusament PLA")), "{fil:?}");
        assert!(!fil.iter().any(|n| n.contains("CORE One") || n.contains("MINI")), "{fil:?}");
    }

    #[test]
    fn unknown_printers_give_no_bundle() {
        assert!(PresetBundle::from_library(&["Nope".to_owned()], &[]).is_none());
    }

    #[test]
    fn filament_list_bounds() {
        let mut b = PresetBundle::builtin();
        b.remove_filament();
        assert_eq!(b.project_filaments.len(), 1);
        for _ in 0..100 {
            b.add_filament();
        }
        assert_eq!(b.project_filaments.len(), MAX_FILAMENTS);
        assert!(b.slot(TabKind::Filament, 99).is_none());
        assert_eq!(
            b.value(Schema::get(), TabKind::Filament, 99, "filament_type"),
            Schema::get().default_of("filament_type")
        );
    }
}
