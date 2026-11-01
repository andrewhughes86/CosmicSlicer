//! OrcaSlicer's option definitions and the page / group layout of its settings tabs.

use std::collections::HashMap;
use std::sync::OnceLock;

use serde::Deserialize;

const SOURCE: &str = include_str!("../../../assets/settings/orca_settings.json");

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TabKind {
    Process,
    Filament,
    Printer,
}

impl TabKind {
    pub fn title(self) -> &'static str {
        match self {
            TabKind::Process => "Process",
            TabKind::Filament => "Filament",
            TabKind::Printer => "Printer",
        }
    }
}

/// Who sees an option: OrcaSlicer shows only `Simple` ones until Advanced is on.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    #[default]
    Simple,
    Advanced,
    Expert,
    Develop,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OptionType {
    Float,
    Int,
    Bool,
    String,
    Percent,
    FloatOrPercent,
    Enum,
    Point,
    Points,
    /// Anything else (point groups, generic enum lists), edited as text.
    Other,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct OptionDef {
    #[serde(rename = "type", default)]
    pub raw_type: String,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub full_label: String,
    #[serde(default)]
    pub tooltip: String,
    #[serde(default)]
    pub sidetext: String,
    pub min: Option<f64>,
    pub max: Option<f64>,
    #[serde(default)]
    pub mode: Mode,
    /// (value, label) pairs.
    #[serde(default, rename = "enum")]
    pub enum_values: Vec<(String, String)>,
    #[serde(default, deserialize_with = "default_value")]
    pub default: String,
    #[serde(default)]
    pub multiline: bool,
    #[serde(default)]
    pub height: Option<u32>,
    #[serde(default)]
    pub gui_type: Option<String>,
}

fn default_value<'de, D: serde::Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    Ok(match serde_json::Value::deserialize(d)? {
        serde_json::Value::Bool(b) => if b { "1" } else { "0" }.to_owned(),
        serde_json::Value::String(s) => s,
        serde_json::Value::Number(n) => n.to_string(),
        _ => String::new(),
    })
}

impl OptionDef {
    pub fn kind(&self) -> OptionType {
        // Open enums ("PLA", "PETG", … or anything typed) are chosen from their list.
        if self.gui_type.as_deref().is_some_and(|g| g.ends_with("enum_open")) && !self.enum_values.is_empty() {
            return OptionType::Enum;
        }
        let base = self.raw_type.trim_end_matches("Nullable");
        match base {
            "Float" | "Floats" => OptionType::Float,
            "Int" | "Ints" => OptionType::Int,
            "Bool" | "Bools" => OptionType::Bool,
            "String" | "Strings" => OptionType::String,
            "Percent" | "Percents" => OptionType::Percent,
            "FloatOrPercent" | "FloatsOrPercents" => OptionType::FloatOrPercent,
            "Enum" => OptionType::Enum,
            "Point" => OptionType::Point,
            "Points" => OptionType::Points,
            _ if !self.enum_values.is_empty() => OptionType::Enum,
            _ => OptionType::Other,
        }
    }

    /// Per-extruder or per-filament list values.
    pub fn is_vector(&self) -> bool {
        let base = self.raw_type.trim_end_matches("Nullable");
        base.ends_with('s') && base != "Points" && base != "FloatOrPercent"
    }

    /// The label shown in a settings row.
    pub fn display_label(&self) -> &str {
        if self.label.is_empty() { &self.full_label } else { &self.label }
    }

    /// The label used where the row's group is not shown (search, compare).
    pub fn long_label(&self) -> &str {
        if self.full_label.is_empty() { &self.label } else { &self.full_label }
    }

    pub fn enum_label(&self, value: &str) -> Option<&str> {
        self.enum_values.iter().find(|(v, _)| v == value).map(|(_, l)| l.as_str())
    }
}

/// One row in a group: a single option, or a labelled line of several.
#[derive(Clone, Debug, Deserialize)]
#[serde(untagged)]
pub enum Entry {
    Line {
        line: String,
        keys: Vec<String>,
    },
    Single {
        key: String,
        #[serde(default)]
        widget: bool,
    },
}

impl Entry {
    pub fn keys(&self) -> Vec<&str> {
        match self {
            Entry::Line { keys, .. } => keys.iter().map(String::as_str).collect(),
            Entry::Single { key, .. } => vec![key.as_str()],
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
pub struct Group {
    pub name: String,
    #[serde(default)]
    pub icon: String,
    pub options: Vec<Entry>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Page {
    pub name: String,
    #[serde(default)]
    pub icon: String,
    pub groups: Vec<Group>,
}

#[derive(Debug, Default, Deserialize)]
struct Tabs {
    #[serde(default)]
    process: Vec<Page>,
    #[serde(default)]
    filament: Vec<Page>,
    #[serde(default)]
    printer: Vec<Page>,
}

#[derive(Debug, Default, Deserialize)]
pub struct Schema {
    #[serde(default)]
    pub options: HashMap<String, OptionDef>,
    #[serde(default)]
    tabs: Tabs,
}

impl Schema {
    /// The embedded schema, parsed once. An unreadable schema yields an empty one (and the
    /// error), never a crash.
    pub fn get() -> &'static Schema {
        static SCHEMA: OnceLock<Schema> = OnceLock::new();
        SCHEMA.get_or_init(|| {
            serde_json::from_str(SOURCE).unwrap_or_else(|e| {
                eprintln!("settings schema is unreadable: {e}");
                Schema::default()
            })
        })
    }

    pub fn option(&self, key: &str) -> Option<&OptionDef> {
        self.options.get(key)
    }

    pub fn default_of(&self, key: &str) -> &str {
        self.options.get(key).map_or("", |o| o.default.as_str())
    }

    pub fn pages(&self, tab: TabKind) -> &[Page] {
        match tab {
            TabKind::Process => &self.tabs.process,
            TabKind::Filament => &self.tabs.filament,
            TabKind::Printer => &self.tabs.printer,
        }
    }

    /// Which tab shows `key`.
    pub fn tab_of(&self, key: &str) -> Option<TabKind> {
        [TabKind::Process, TabKind::Filament, TabKind::Printer].into_iter().find(|&t| {
            self.pages(t).iter().any(|p| p.groups.iter().any(|g| g.options.iter().any(|e| e.keys().contains(&key))))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_embedded_schema_has_orcas_tabs() {
        let s = Schema::get();
        assert!(s.options.len() > 500, "{}", s.options.len());
        let names: Vec<&str> = s.pages(TabKind::Process).iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["Quality", "Strength", "Speed", "Support", "Multimaterial", "Others"]);
        assert!(!s.pages(TabKind::Filament).is_empty());
        assert!(!s.pages(TabKind::Printer).is_empty());
        let lh = s.option("layer_height");
        assert!(lh.is_some_and(|o| o.kind() == OptionType::Float && o.sidetext == "mm"));
        let seam = s.option("seam_position");
        assert!(seam.is_some_and(|o| o.kind() == OptionType::Enum && o.default == "aligned"));
        assert_eq!(s.default_of("enable_support"), "0");
    }

    #[test]
    fn every_laid_out_option_is_defined() {
        let s = Schema::get();
        let missing: Vec<&str> = [TabKind::Process, TabKind::Filament, TabKind::Printer]
            .into_iter()
            .flat_map(|t| s.pages(t))
            .flat_map(|p| &p.groups)
            .flat_map(|g| &g.options)
            .flat_map(Entry::keys)
            .filter(|k| s.option(k).is_none())
            .collect();
        // OrcaSlicer lays out a few options that its definitions build in loops.
        assert!(missing.len() < 10, "{missing:?}");
    }
}
