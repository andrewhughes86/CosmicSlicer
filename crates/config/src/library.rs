//! OrcaSlicer's profile library: every vendor's printers, filaments and processes, embedded
//! from `assets/profiles/orca_profiles.bin` (made by `tools/pack_orca_profiles.py`).
//!
//! The index (vendors, printer models, printers) is read at start-up; a vendor's profiles are
//! unpacked only when one of them is used. Profiles are resolved as OrcaSlicer does: the
//! parent named by `inherits` first, then the profiles named by `include`, then the profile's
//! own values.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use serde::Deserialize;

use crate::Config;
use crate::schema::TabKind;

const PACK: &[u8] = include_bytes!("../../../assets/profiles/orca_profiles.bin");
const MAGIC: &[u8] = b"CSPROF1\n";
/// The vendor whose filaments every printer may use.
pub const COMMON_VENDOR: &str = "OrcaFilamentLibrary";
/// Deepest `inherits` chain followed (OrcaSlicer's are under ten).
const MAX_DEPTH: usize = 32;

#[derive(Clone, Debug, Deserialize)]
pub struct ModelInfo {
    pub name: String,
    #[serde(default)]
    pub family: String,
    #[serde(default)]
    pub nozzles: Vec<String>,
    #[serde(default)]
    pub default_materials: Vec<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct PrinterInfo {
    pub name: String,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub variant: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct VendorInfo {
    /// Folder name, e.g. `BBL`.
    pub id: String,
    /// Display name, e.g. `Bambu Lab`.
    pub name: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub models: Vec<ModelInfo>,
    #[serde(default)]
    pub printers: Vec<PrinterInfo>,
    offset: usize,
    length: usize,
}

#[derive(Deserialize)]
struct Index {
    vendors: Vec<VendorInfo>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(untagged)]
enum Raw {
    Text(String),
    List(Vec<String>),
}

type RawProfile = HashMap<String, Raw>;

#[derive(Default, Deserialize)]
struct VendorData {
    #[serde(default)]
    machine: HashMap<String, RawProfile>,
    #[serde(default)]
    process: HashMap<String, RawProfile>,
    #[serde(default)]
    filament: HashMap<String, RawProfile>,
}

impl VendorData {
    fn of(&self, kind: TabKind) -> &HashMap<String, RawProfile> {
        match kind {
            TabKind::Printer => &self.machine,
            TabKind::Process => &self.process,
            TabKind::Filament => &self.filament,
        }
    }
}

/// A profile with its whole `inherits` chain applied.
#[derive(Clone, Debug, PartialEq)]
pub struct SystemPreset {
    pub name: String,
    pub kind: TabKind,
    /// The vendor's folder id.
    pub vendor: String,
    pub config: Config,
    /// Printer preset names it is meant for (empty: any, subject to the condition).
    pub compatible_printers: Vec<String>,
    pub condition: String,
    /// Shown in lists (OrcaSlicer's `instantiation`); bases like `fdm_filament_pla` are not.
    pub instantiated: bool,
}

/// Keys that describe a profile rather than set an option.
const META: &[&str] = &[
    "type",
    "name",
    "inherits",
    "from",
    "setting_id",
    "instantiation",
    "include",
    "compatible_printers",
    "compatible_printers_condition",
    "compatible_prints",
    "compatible_prints_condition",
    "renamed_from",
    "version",
    "description",
    "upward_compatible_machine",
    "is_custom_defined",
];

pub struct Library {
    pub vendors: Vec<VendorInfo>,
    data_start: usize,
    cache: Mutex<HashMap<String, Arc<VendorData>>>,
}

impl Library {
    /// The embedded library, read once. A damaged pack gives an empty library (and a log
    /// line), never a crash; CosmicSlicer then falls back to its built-in presets.
    pub fn get() -> &'static Library {
        static LIB: OnceLock<Library> = OnceLock::new();
        LIB.get_or_init(|| {
            Library::read(PACK).unwrap_or_else(|e| {
                eprintln!("the profile library is unreadable: {e}");
                Library { vendors: Vec::new(), data_start: 0, cache: Mutex::default() }
            })
        })
    }

    fn read(pack: &[u8]) -> Result<Library, String> {
        let head = pack.get(..MAGIC.len()).ok_or("too short")?;
        if head != MAGIC {
            return Err("not a profile pack".into());
        }
        let len_bytes = pack.get(MAGIC.len()..MAGIC.len() + 4).ok_or("too short")?;
        let len = u32::from_le_bytes([len_bytes[0], len_bytes[1], len_bytes[2], len_bytes[3]]) as usize;
        let start = MAGIC.len() + 4;
        let index_bytes = pack.get(start..start + len).ok_or("index cut short")?;
        let index: Index = serde_json::from_slice(index_bytes).map_err(|e| e.to_string())?;
        Ok(Library { vendors: index.vendors, data_start: start + len, cache: Mutex::default() })
    }

    /// The name to show for a vendor (OrcaSlicer's index calls Bambu Lab "Bambulab").
    pub fn display_name(v: &VendorInfo) -> &str {
        match v.id.as_str() {
            "BBL" => "Bambu Lab",
            "OrcaFilamentLibrary" => "Orca Filament Library",
            _ => &v.name,
        }
    }

    pub fn vendor_info(&self, id: &str) -> Option<&VendorInfo> {
        self.vendors.iter().find(|v| v.id == id)
    }

    /// The vendor that has printer preset `name`.
    pub fn vendor_of_printer(&self, name: &str) -> Option<&VendorInfo> {
        self.vendors.iter().find(|v| v.printers.iter().any(|p| p.name == name))
    }

    fn data(&self, id: &str) -> Option<Arc<VendorData>> {
        if let Some(d) = self.cache.lock().ok()?.get(id) {
            return Some(Arc::clone(d));
        }
        let v = self.vendor_info(id)?;
        let start = self.data_start.checked_add(v.offset)?;
        let blob = PACK.get(start..start.checked_add(v.length)?)?;
        let json = miniz_oxide::inflate::decompress_to_vec_zlib_with_limit(blob, 256 << 20).ok()?;
        let data: VendorData = serde_json::from_slice(&json).ok()?;
        let data = Arc::new(data);
        if let Ok(mut c) = self.cache.lock() {
            c.insert(id.to_owned(), Arc::clone(&data));
        }
        Some(data)
    }

    /// Names of the instantiated (listable) profiles of `kind` from vendor `id`.
    pub fn names(&self, id: &str, kind: TabKind) -> Vec<String> {
        let Some(d) = self.data(id) else { return Vec::new() };
        let mut names: Vec<String> = d
            .of(kind)
            .iter()
            .filter(|(_, p)| !matches!(p.get("instantiation"), Some(Raw::Text(t)) if t == "false"))
            .map(|(n, _)| n.clone())
            .collect();
        names.sort();
        names
    }

    fn raw(&self, kind: TabKind, name: &str, vendor: &str) -> Option<(String, RawProfile)> {
        // The vendor's own first, then the common filament library, as OrcaSlicer looks up.
        for id in [vendor, COMMON_VENDOR] {
            if let Some(d) = self.data(id)
                && let Some(p) = d.of(kind).get(name)
            {
                return Some((id.to_owned(), p.clone()));
            }
        }
        None
    }

    /// `name` with its parents and includes applied.
    pub fn resolve(&self, kind: TabKind, name: &str, vendor: &str) -> Option<SystemPreset> {
        self.resolve_depth(kind, name, vendor, 0)
    }

    fn resolve_depth(&self, kind: TabKind, name: &str, vendor: &str, depth: usize) -> Option<SystemPreset> {
        if depth > MAX_DEPTH {
            return None;
        }
        let (found_in, raw) = self.raw(kind, name, vendor)?;
        let text = |key: &str| match raw.get(key) {
            Some(Raw::Text(t)) => Some(t.clone()),
            Some(Raw::List(l)) => Some(l.join(",")),
            None => None,
        };
        let list = |key: &str| match raw.get(key) {
            Some(Raw::List(l)) => Some(l.clone()),
            Some(Raw::Text(t)) if !t.is_empty() => Some(t.split(';').map(str::to_owned).collect()),
            Some(Raw::Text(_)) => Some(Vec::new()),
            None => None,
        };
        let parent =
            text("inherits").filter(|p| !p.is_empty()).and_then(|p| self.resolve_depth(kind, &p, vendor, depth + 1));
        let mut config = parent.as_ref().map(|p| p.config.clone()).unwrap_or_default();
        for include in list("include").unwrap_or_default() {
            if let Some(inc) = self.resolve_depth(kind, &include, vendor, depth + 1) {
                config.values.extend(inc.config.values);
            }
        }
        for (k, v) in &raw {
            if META.contains(&k.as_str()) {
                continue;
            }
            let value = match v {
                Raw::Text(t) => t.clone(),
                // A list of point lists, which Orca writes with '#' between them.
                Raw::List(l) if k == "extruder_printable_area" => l.join("#"),
                Raw::List(l) => l.join(","),
            };
            config.values.insert(k.clone(), value);
        }
        Some(SystemPreset {
            name: name.to_owned(),
            kind,
            vendor: found_in,
            config,
            compatible_printers: list("compatible_printers")
                .or_else(|| parent.as_ref().map(|p| p.compatible_printers.clone()))
                .unwrap_or_default(),
            condition: text("compatible_printers_condition")
                .or_else(|| parent.as_ref().map(|p| p.condition.clone()))
                .unwrap_or_default(),
            instantiated: text("instantiation").is_none_or(|t| t != "false"),
        })
    }

    /// Every instantiated profile of `kind` from `vendor` (and the common library).
    pub fn resolve_all(&self, kind: TabKind, vendor: &str) -> Vec<SystemPreset> {
        let mut out = Vec::new();
        let mut ids = vec![vendor.to_owned()];
        if kind == TabKind::Filament && vendor != COMMON_VENDOR {
            ids.push(COMMON_VENDOR.to_owned());
        }
        for id in ids {
            for name in self.names(&id, kind) {
                if let Some(p) = self.resolve(kind, &name, &id) {
                    out.push(p);
                }
            }
        }
        out
    }
}

/// The name a filament shows under in the setup wizard: `Bambu PLA Basic @BBL X1C` →
/// `Bambu PLA Basic`.
pub fn base_name(name: &str) -> &str {
    name.split(" @").next().unwrap_or(name).trim()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_library_has_orcas_vendors() {
        let lib = Library::get();
        assert!(lib.vendors.len() > 60, "{}", lib.vendors.len());
        let printers: usize = lib.vendors.iter().map(|v| v.printers.len()).sum();
        assert!(printers > 1000, "{printers}");
        assert!(lib.vendor_info("BBL").is_some_and(|v| Library::display_name(v) == "Bambu Lab"));
        assert!(lib.vendor_of_printer("Bambu Lab X1 Carbon 0.4 nozzle").is_some_and(|v| v.id == "BBL"));
    }

    #[test]
    fn inheritance_and_includes_resolve() {
        let lib = Library::get();
        let x1c = lib.resolve(TabKind::Printer, "Bambu Lab X1 Carbon 0.4 nozzle", "BBL");
        assert!(x1c.is_some());
        let Some(x1c) = x1c else { return };
        // From the printer itself.
        assert_eq!(x1c.config.values.get("printer_model").map(String::as_str), Some("Bambu Lab X1 Carbon"));
        // From its included start G-code template.
        assert!(x1c.config.values.get("machine_start_gcode").is_some_and(|g| g.len() > 200));
        // From the common bases it inherits.
        assert!(x1c.config.values.contains_key("printable_area"));
        assert!(!x1c.config.values.contains_key("inherits"));

        let klipper = lib.resolve(TabKind::Printer, "MyKlipper 0.4 nozzle", "Custom");
        assert!(klipper.is_some_and(|k| k.config.values.get("gcode_flavor").map(String::as_str) == Some("klipper")));

        let pla = lib.resolve(TabKind::Filament, "Generic PLA @System", COMMON_VENDOR);
        assert!(pla.is_some_and(|p| p.config.values.get("filament_type").map(String::as_str) == Some("PLA")));
    }

    #[test]
    fn missing_profiles_are_none() {
        let lib = Library::get();
        assert!(lib.resolve(TabKind::Printer, "No Such Printer", "BBL").is_none());
        assert!(lib.names("NoVendor", TabKind::Printer).is_empty());
        assert_eq!(base_name("Bambu PLA Basic @BBL X1C"), "Bambu PLA Basic");
    }
}
