//! Presets: named sets of values for a printer, a filament or a process, and the edits
//! made to the selected one.

use crate::Config;
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
}

impl Preset {
    fn system(name: &str, vendor: &str, values: &[(&str, &str)]) -> Self {
        let mut config = Config::default();
        for (k, v) in values {
            config.set(*k, *v);
        }
        Self { name: name.to_owned(), system: true, vendor: vendor.to_owned(), config }
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
        let list = self.list_mut(kind);
        let preset = Preset { name: name.to_owned(), system: false, vendor: "User presets".into(), config };
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
            let next = self.project_filaments.len() % self.filaments.len().max(1);
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
