//! The presets the app offers: OrcaSlicer's library for the printers installed in the setup
//! wizard, the filaments made visible there, and the user's own presets, kept as
//! OrcaSlicer-style files in the data folder.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use CosmicSlicer_config::library::{self, Library};
use CosmicSlicer_config::{Preset, PresetBundle, Schema, Slot, TabKind, userfiles};

use crate::state::Prefs;

/// What a new installation starts with, before the setup wizard has run.
pub const DEFAULT_PRINTERS: &[&str] = &["MyKlipper 0.4 nozzle"];

/// The printers to offer: the installed ones, or the default.
pub fn installed(prefs: &Prefs) -> Vec<String> {
    if prefs.installed_printers.is_empty() {
        DEFAULT_PRINTERS.iter().map(|s| (*s).to_owned()).collect()
    } else {
        prefs.installed_printers.clone()
    }
}

/// The filaments a printer model suggests (its `default_materials`), as base names.
pub fn suggested_filaments(printers: &[String]) -> Vec<String> {
    let lib = Library::get();
    let mut out: Vec<String> = vec!["Generic PLA".into(), "Generic PETG".into()];
    for name in printers {
        let Some(v) = lib.vendor_of_printer(name) else { continue };
        let Some(model) = v.printers.iter().find(|p| p.name == *name).map(|p| p.model.clone()) else { continue };
        for m in v.models.iter().filter(|m| m.name == model) {
            for f in &m.default_materials {
                let base = library::base_name(f).to_owned();
                if !out.contains(&base) {
                    out.push(base);
                }
            }
        }
    }
    out
}

/// The filaments to offer: the visible ones, or the installed printers' suggestions.
pub fn visible(prefs: &Prefs) -> Vec<String> {
    if prefs.visible_filaments.is_empty() {
        suggested_filaments(&installed(prefs))
    } else {
        prefs.visible_filaments.clone()
    }
}

/// A fresh bundle for `prefs`; CosmicSlicer's built-in presets if the library cannot provide.
pub fn build(prefs: &Prefs) -> PresetBundle {
    PresetBundle::from_library(&installed(prefs), &visible(prefs)).unwrap_or_else(PresetBundle::builtin)
}

/// Rebuilds `bundle` for new `prefs` (after the wizard), keeping user presets and, where they
/// still exist, the selections.
pub fn rebuild(bundle: &PresetBundle, prefs: &Prefs) -> PresetBundle {
    let mut fresh = build(prefs);
    for kind in [TabKind::Printer, TabKind::Filament, TabKind::Process] {
        for p in bundle.list(kind).iter().filter(|p| !p.system) {
            fresh.add_user(kind, p.clone());
        }
    }
    let name =
        |kind: TabKind, slot: &Slot| bundle.list(kind).get(slot.preset).map(|p| p.name.clone()).unwrap_or_default();
    let printer = name(TabKind::Printer, &bundle.printer);
    let process = name(TabKind::Process, &bundle.process);
    let filaments: Vec<String> = bundle.project_filaments.iter().map(|s| name(TabKind::Filament, s)).collect();
    select_by_name(&mut fresh, &printer, &process, &filaments);
    fresh
}

/// Selects presets by name where they exist, then makes the selections compatible.
pub fn select_by_name(b: &mut PresetBundle, printer: &str, process: &str, filaments: &[String]) {
    let find = |list: &[Preset], n: &str| list.iter().position(|p| p.name == n);
    if let Some(i) = find(&b.printers, printer) {
        b.printer = Slot { preset: i, ..Default::default() };
    }
    if let Some(i) = find(&b.processes, process) {
        b.process = Slot { preset: i, ..Default::default() };
    }
    let picked: Vec<Slot> = filaments
        .iter()
        .filter_map(|n| find(&b.filaments, n))
        .take(CosmicSlicer_config::presets::MAX_FILAMENTS)
        .map(|preset| Slot { preset, ..Default::default() })
        .collect();
    if !picked.is_empty() {
        b.project_filaments = picked;
    }
    b.fix_selections();
}

/// The user's preset files, and keeping them in step with the bundle.
pub struct UserFiles {
    dir: PathBuf,
    /// Files this program wrote or read; only these are ever deleted.
    known: HashSet<PathBuf>,
}

impl UserFiles {
    /// Reads `<data>/user/default` into `bundle`; returns the problems found.
    pub fn load(data: &Path, bundle: &mut PresetBundle) -> (UserFiles, Vec<String>) {
        let dir = data.join("user").join("default");
        let (presets, errors) = userfiles::load_dir(&dir);
        let mut known = HashSet::new();
        for (kind, p) in presets {
            known.insert(userfiles::path_of(&dir, kind, &p.name));
            bundle.add_user(kind, p);
        }
        (UserFiles { dir, known }, errors)
    }

    /// Writes changed user presets and removes the files of deleted ones.
    pub fn sync(&mut self, bundle: &PresetBundle) -> Vec<String> {
        let schema = Schema::get();
        let mut errors = Vec::new();
        let mut now: HashSet<PathBuf> = HashSet::new();
        for kind in [TabKind::Printer, TabKind::Filament, TabKind::Process] {
            for p in bundle.list(kind).iter().filter(|p| !p.system) {
                let path = userfiles::path_of(&self.dir, kind, &p.name);
                let text = userfiles::to_text(schema, kind, p);
                if std::fs::read_to_string(&path).ok().as_deref() != Some(text.as_str())
                    && let Err(e) = crate::store::write_atomic(&path, text.as_bytes())
                {
                    errors.push(format!("Could not save preset \"{}\": {e}", p.name));
                }
                now.insert(path);
            }
        }
        for gone in self.known.difference(&now) {
            let _ = std::fs::remove_file(gone);
        }
        self.known = now;
        errors
    }
}

/// Printers of the same model as `printer` (its nozzle variants), as (index, nozzle).
pub fn nozzle_variants(b: &PresetBundle, schema: &Schema) -> Vec<(usize, String)> {
    let Some(current) = b.current_printer() else { return Vec::new() };
    let model = current.config.get(schema, "printer_model").to_owned();
    let nozzle = |p: &Preset| p.config.get(schema, "nozzle_diameter").split(',').next().unwrap_or("").trim().to_owned();
    if model.is_empty() {
        return vec![(b.printer.preset, nozzle(current))];
    }
    b.printers
        .iter()
        .enumerate()
        .filter(|(_, p)| p.config.get(schema, "printer_model") == model)
        .map(|(i, p)| (i, nozzle(p)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fresh_install_offers_a_klipper_printer_and_generic_filaments() {
        let b = build(&Prefs::default());
        assert!(b.printers.iter().any(|p| p.name == "MyKlipper 0.4 nozzle"));
        assert!(
            b.current_printer()
                .is_some_and(|p| p.config.values.get("gcode_flavor").map(String::as_str) == Some("klipper"))
        );
        assert!(b.preset(TabKind::Filament, 0).is_some_and(|f| f.name.contains("PLA")));
    }

    #[test]
    fn rebuilding_keeps_user_presets_and_selections() {
        let mut prefs = Prefs::default();
        let mut b = build(&prefs);
        let schema = Schema::get();
        b.set_value(schema, TabKind::Process, 0, "wall_loops", "4".into());
        assert!(b.save_as(TabKind::Process, 0, "Strong walls").is_ok());
        prefs.installed_printers = vec!["MyKlipper 0.4 nozzle".into(), "Bambu Lab X1 Carbon 0.4 nozzle".into()];
        let fresh = rebuild(&b, &prefs);
        assert_eq!(fresh.printers.len(), 2);
        assert_eq!(fresh.preset(TabKind::Process, 0).map(|p| p.name.as_str()), Some("Strong walls"));
    }

    #[test]
    fn user_files_follow_the_bundle() {
        let dir = crate::store::tests::TempDir::new("userfiles");
        let mut b = build(&Prefs::default());
        let (mut files, errors) = UserFiles::load(&dir.0, &mut b);
        assert!(errors.is_empty());
        let schema = Schema::get();
        b.set_value(schema, TabKind::Filament, 0, "nozzle_temperature", "212".into());
        assert!(b.save_as(TabKind::Filament, 0, "My PLA").is_ok());
        assert!(files.sync(&b).is_empty());
        let path = dir.0.join("user/default/filament/My PLA.json");
        assert!(path.exists());
        // A second start reads it back.
        let mut again = build(&Prefs::default());
        let _ = UserFiles::load(&dir.0, &mut again);
        assert!(again.filaments.iter().any(
            |p| p.name == "My PLA" && p.config.values.get("nozzle_temperature").map(String::as_str) == Some("212")
        ));
        // Deleting it removes the file.
        let i = b.filaments.iter().position(|p| p.name == "My PLA").unwrap_or(0);
        assert!(b.delete(TabKind::Filament, i).is_ok());
        assert!(files.sync(&b).is_empty());
        assert!(!path.exists());
    }
}
