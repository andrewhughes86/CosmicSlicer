//! Print, filament and printer settings.
//!
//! The option definitions and the settings-tab layout come from OrcaSlicer
//! (`assets/settings/orca_settings.json`, extracted from its `PrintConfig.cpp` and `Tab.cpp`
//! by `tools/extract_orca_settings.py`). Values are kept as OrcaSlicer serialises them
//! (vectors comma-separated, booleans `1`/`0`) so its profiles load unchanged later.

#![allow(non_snake_case)]

pub mod expr;
pub mod library;
pub mod presets;
pub mod schema;
pub mod userfiles;
pub mod variants;

pub use presets::{Preset, PresetBundle, PresetKind, Slot};
pub use schema::{Entry, Group, Mode, OptionDef, OptionType, Page, Schema, TabKind};

use std::collections::BTreeMap;

/// Option values by key; keys not present take the schema default.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Config {
    pub values: BTreeMap<String, String>,
}

impl Config {
    pub fn get<'a>(&'a self, schema: &'a Schema, key: &str) -> &'a str {
        self.values.get(key).map(String::as_str).unwrap_or_else(|| schema.default_of(key))
    }

    pub fn set(&mut self, key: impl Into<String>, value: impl Into<String>) {
        self.values.insert(key.into(), value.into());
    }

    /// The first number in a value ("0.2", "60,60", "15%"); `None` when there is none.
    pub fn number(&self, schema: &Schema, key: &str) -> Option<f64> {
        first_number(self.get(schema, key))
    }

    pub fn bool(&self, schema: &Schema, key: &str) -> bool {
        is_true(self.get(schema, key))
    }

    /// Layered lookup: `self` over `base`.
    pub fn overlaid(&self, base: &Config) -> Config {
        let mut values = base.values.clone();
        values.extend(self.values.iter().map(|(k, v)| (k.clone(), v.clone())));
        Config { values }
    }
}

pub fn first_number(value: &str) -> Option<f64> {
    let first = value.split([',', ';']).next()?.trim().trim_end_matches('%');
    first.parse::<f64>().ok().filter(|v| v.is_finite())
}

pub fn is_true(value: &str) -> bool {
    matches!(value.split(',').next().map(str::trim), Some("1" | "true"))
}

/// The bed outline from a `printable_area` value ("0x0,235x0,235x235,0x235").
pub fn parse_points(value: &str) -> Vec<[f64; 2]> {
    value
        .split(',')
        .filter_map(|p| {
            let (x, y) = p.trim().split_once('x')?;
            let (x, y) = (x.trim().parse::<f64>().ok()?, y.trim().parse::<f64>().ok()?);
            (x.is_finite() && y.is_finite()).then_some([x, y])
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_and_points() {
        assert_eq!(first_number("60,60"), Some(60.0));
        assert_eq!(first_number("15%"), Some(15.0));
        assert_eq!(first_number("nan"), None);
        assert_eq!(parse_points("0x0,235x0,235x235,0x235").len(), 4);
        assert_eq!(parse_points("junk,1x"), Vec::<[f64; 2]>::new());
    }
}
