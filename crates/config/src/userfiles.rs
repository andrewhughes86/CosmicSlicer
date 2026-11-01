//! User presets as OrcaSlicer writes them: one JSON file per preset under
//! `user/default/{machine,filament,process}/`, holding the system preset it `inherits` from and
//! only the values that differ. Files from OrcaSlicer import the same way.

use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use crate::Config;
use crate::library::{self, Library};
use crate::presets::Preset;
use crate::schema::{OptionType, Schema, TabKind};

pub fn kind_dir(kind: TabKind) -> &'static str {
    match kind {
        TabKind::Printer => "machine",
        TabKind::Filament => "filament",
        TabKind::Process => "process",
    }
}

fn kind_of(type_name: &str) -> Option<TabKind> {
    match type_name {
        "machine" | "printer" => Some(TabKind::Printer),
        "filament" => Some(TabKind::Filament),
        "process" | "print" => Some(TabKind::Process),
        _ => None,
    }
}

/// A file name for a preset: its name with characters no file system accepts replaced.
pub fn file_name(name: &str) -> String {
    let safe: String =
        name.chars().map(|c| if "/\\:*?\"<>|".contains(c) || c.is_control() { '_' } else { c }).collect();
    format!("{}.json", safe.trim())
}

/// The system preset `name` of `kind`, wherever in the library it is.
fn find_parent(kind: TabKind, name: &str) -> Option<library::SystemPreset> {
    let lib = Library::get();
    if kind == TabKind::Printer
        && let Some(v) = lib.vendor_of_printer(name)
    {
        return lib.resolve(kind, name, &v.id);
    }
    // Filaments and processes: the common library first, then each vendor in turn.
    std::iter::once(library::COMMON_VENDOR.to_owned())
        .chain(lib.vendors.iter().map(|v| v.id.clone()))
        .find_map(|id| lib.resolve(kind, name, &id))
}

/// A value as OrcaSlicer writes it: lists as arrays, everything else as a string.
fn to_json_value(schema: &Schema, key: &str, value: &str) -> Value {
    match schema.option(key) {
        Some(def) if def.is_vector() && def.kind() != OptionType::String => {
            Value::Array(value.split(',').map(|v| Value::String(v.to_owned())).collect())
        }
        Some(def) if def.is_vector() => Value::Array(vec![Value::String(value.to_owned())]),
        _ => Value::String(value.to_owned()),
    }
}

fn from_json_value(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Array(items) => {
            let parts: Vec<String> = items.iter().map(from_json_value).collect();
            if parts.len() == 1 { parts.into_iter().next().unwrap_or_default() } else { parts.join(",") }
        }
        Value::Bool(b) => if *b { "1" } else { "0" }.into(),
        Value::Number(n) => n.to_string(),
        _ => String::new(),
    }
}

/// The JSON OrcaSlicer would write for user preset `p`.
pub fn to_json(schema: &Schema, kind: TabKind, p: &Preset) -> Value {
    let parent = (!p.inherits.is_empty()).then(|| find_parent(kind, &p.inherits)).flatten();
    let mut m = Map::new();
    m.insert("type".into(), Value::String(kind_dir(kind).into()));
    m.insert("name".into(), Value::String(p.name.clone()));
    m.insert("inherits".into(), Value::String(parent.as_ref().map_or_else(String::new, |q| q.name.clone())));
    m.insert("from".into(), Value::String("User".into()));
    m.insert("instantiation".into(), Value::String("true".into()));
    if !p.compatible_printers.is_empty() {
        m.insert(
            "compatible_printers".into(),
            Value::Array(p.compatible_printers.iter().cloned().map(Value::String).collect()),
        );
    }
    if !p.condition.is_empty() {
        m.insert("compatible_printers_condition".into(), Value::String(p.condition.clone()));
    }
    for (k, v) in &p.config.values {
        // Only what differs from the parent.
        if parent.as_ref().is_some_and(|q| q.config.values.get(k) == Some(v)) {
            continue;
        }
        m.insert(k.clone(), to_json_value(schema, k, v));
    }
    Value::Object(m)
}

/// Reads a preset file (CosmicSlicer's or OrcaSlicer's); `kind_hint` is used when the file
/// does not say what it is.
pub fn from_json(text: &str, kind_hint: Option<TabKind>) -> Result<(TabKind, Preset), String> {
    let v: Value =
        serde_json::from_str(text.trim_start_matches('\u{feff}')).map_err(|e| format!("not a preset file: {e}"))?;
    let m = v.as_object().ok_or("not a preset file")?;
    let kind = m
        .get("type")
        .and_then(Value::as_str)
        .and_then(kind_of)
        .or(kind_hint)
        .ok_or("the file does not say whether it is a printer, filament or process preset")?;
    let name = m
        .get("name")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .ok_or("the preset has no name")?;
    let inherits = m.get("inherits").and_then(Value::as_str).unwrap_or("").to_owned();
    let parent = (!inherits.is_empty()).then(|| find_parent(kind, &inherits)).flatten();
    let mut config = parent.as_ref().map(|p| p.config.clone()).unwrap_or_default();
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
        "version",
        "is_custom_defined",
        "base_id",
        "updated_time",
        "user_id",
    ];
    for (k, value) in m {
        if !META.contains(&k.as_str()) {
            config.values.insert(k.clone(), from_json_value(value));
        }
    }
    let list = |key: &str| -> Option<Vec<String>> {
        m.get(key).and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_str).map(str::to_owned).collect())
    };
    let preset = Preset {
        name: name.to_owned(),
        system: false,
        vendor: "User presets".into(),
        config,
        // A parent we do not have leaves the preset standing on its own values.
        inherits: parent.as_ref().map_or_else(String::new, |p| p.name.clone()),
        compatible_printers: list("compatible_printers")
            .or_else(|| parent.as_ref().map(|p| p.compatible_printers.clone()))
            .unwrap_or_default(),
        condition: m
            .get("compatible_printers_condition")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .or_else(|| parent.as_ref().map(|p| p.condition.clone()))
            .unwrap_or_default(),
    };
    Ok((kind, preset))
}

/// Every user preset in `dir` (the `user/default` folder).
pub fn load_dir(dir: &Path) -> (Vec<(TabKind, Preset)>, Vec<String>) {
    let mut out = Vec::new();
    let mut errors = Vec::new();
    for kind in [TabKind::Printer, TabKind::Filament, TabKind::Process] {
        let Ok(entries) = std::fs::read_dir(dir.join(kind_dir(kind))) else { continue };
        let mut paths: Vec<PathBuf> =
            entries.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e == "json")).collect();
        paths.sort();
        for path in paths {
            let text = match std::fs::read_to_string(&path) {
                Ok(t) => t,
                Err(e) => {
                    errors.push(format!("{}: {e}", path.display()));
                    continue;
                }
            };
            match from_json(&text, Some(kind)) {
                Ok((k, p)) if k == kind => out.push((k, p)),
                Ok(_) => errors.push(format!("{}: in the wrong folder", path.display())),
                Err(e) => errors.push(format!("{}: {e}", path.display())),
            }
        }
    }
    (out, errors)
}

/// Where user preset `name` of `kind` is stored.
pub fn path_of(dir: &Path, kind: TabKind, name: &str) -> PathBuf {
    dir.join(kind_dir(kind)).join(file_name(name))
}

/// The preset's JSON text.
pub fn to_text(schema: &Schema, kind: TabKind, p: &Preset) -> String {
    serde_json::to_string_pretty(&to_json(schema, kind, p)).unwrap_or_default()
}

/// Values of a user preset that differ from its system parent (for "Compare").
pub fn changed_from_parent(kind: TabKind, p: &Preset) -> Config {
    let parent = (!p.inherits.is_empty()).then(|| find_parent(kind, &p.inherits)).flatten();
    let mut c = Config::default();
    for (k, v) in &p.config.values {
        if parent.as_ref().is_none_or(|q| q.config.values.get(k) != Some(v)) {
            c.set(k.clone(), v.clone());
        }
    }
    c
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_user_preset_round_trips_as_a_diff() {
        let schema = Schema::get();
        let lib = Library::get();
        let parent = lib.resolve(TabKind::Filament, "Generic PLA @System", library::COMMON_VENDOR);
        assert!(parent.is_some());
        let Some(parent) = parent else { return };
        let mut config = parent.config.clone();
        config.set("nozzle_temperature", "215,215");
        config.set("filament_cost", "22.5");
        let p = Preset {
            name: "My PLA".into(),
            system: false,
            vendor: "User presets".into(),
            config,
            inherits: parent.name.clone(),
            compatible_printers: Vec::new(),
            condition: String::new(),
        };
        let json = to_json(schema, TabKind::Filament, &p);
        let obj = json.as_object();
        assert!(obj.is_some());
        let Some(obj) = obj else { return };
        // Only the changes, OrcaSlicer style.
        assert!(obj.len() < 12, "{obj:?}");
        assert_eq!(obj.get("inherits").and_then(Value::as_str), Some("Generic PLA @System"));
        assert_eq!(obj.get("nozzle_temperature"), Some(&serde_json::json!(["215", "215"])));
        let back = from_json(&serde_json::to_string(&json).unwrap_or_default(), None);
        assert!(back.is_ok(), "{:?}", back.err());
        let Ok((kind, q)) = back else { return };
        assert_eq!(kind, TabKind::Filament);
        assert_eq!(q.config, p.config);
    }

    #[test]
    fn an_orca_user_file_imports() {
        // As OrcaSlicer saves one.
        let text = r#"{
            "type": "machine", "name": "My X1C", "inherits": "Bambu Lab X1 Carbon 0.4 nozzle",
            "from": "User", "instantiation": "true",
            "printable_height": "245", "machine_start_gcode": "G28\nM104 S[nozzle_temperature_initial_layer]"
        }"#;
        let r = from_json(text, None);
        assert!(r.is_ok(), "{:?}", r.err());
        let Ok((kind, p)) = r else { return };
        assert_eq!(kind, TabKind::Printer);
        assert_eq!(p.config.values.get("printable_height").map(String::as_str), Some("245"));
        assert_eq!(p.config.values.get("printer_model").map(String::as_str), Some("Bambu Lab X1 Carbon"));
        assert!(from_json("{}", None).is_err());
        assert!(from_json("not json", None).is_err());
        assert_eq!(file_name("a/b:c"), "a_b_c.json");
    }
}
