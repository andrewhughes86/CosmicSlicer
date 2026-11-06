//! Orca's nozzle variants. A printer with different hot ends lists its per-nozzle values once
//! per variant (`Direct Drive Standard`, `Bowden High Flow`, ...); before slicing Orca keeps the
//! one each nozzle has fitted (`update_values_to_printer_extruders`), so the engine and custom
//! G-code see one value per nozzle. Doing it twice changes nothing.

use crate::{Config, Schema};

const PRINTER_ONE: [&str; 24] = [
    "nozzle_volume",
    "retraction_length",
    "z_hop",
    "travel_slope",
    "retract_lift_above",
    "retract_lift_below",
    "retract_lift_enforce",
    "z_hop_types",
    "retraction_speed",
    "deretraction_speed",
    "retraction_minimum_travel",
    "retract_when_changing_layer",
    "wipe",
    "wipe_distance",
    "retract_before_wipe",
    "retract_length_toolchange",
    "retract_restart_extra",
    "retract_restart_extra_toolchange",
    "long_retractions_when_cut",
    "retraction_distances_when_cut",
    "nozzle_type",
    "printer_extruder_id",
    "printer_extruder_variant",
    "nozzle_flush_dataset",
];

/// Normal and silent mode, two values per variant.
const PRINTER_TWO: [&str; 15] = [
    "machine_max_acceleration_x",
    "machine_max_acceleration_y",
    "machine_max_acceleration_z",
    "machine_max_acceleration_e",
    "machine_max_acceleration_extruding",
    "machine_max_acceleration_retracting",
    "machine_max_acceleration_travel",
    "machine_max_speed_x",
    "machine_max_speed_y",
    "machine_max_speed_z",
    "machine_max_speed_e",
    "machine_max_jerk_x",
    "machine_max_jerk_y",
    "machine_max_jerk_z",
    "machine_max_jerk_e",
];

const PROCESS: [&str; 2] = ["print_extruder_id", "print_extruder_variant"];

const FILAMENT: [&str; 37] = [
    "filament_flow_ratio",
    "filament_max_volumetric_speed",
    "filament_extruder_variant",
    "filament_retraction_length",
    "filament_z_hop",
    "filament_z_hop_types",
    "filament_retract_lift_above",
    "filament_retract_lift_below",
    "filament_retract_lift_enforce",
    "filament_retract_restart_extra",
    "filament_retraction_speed",
    "filament_deretraction_speed",
    "filament_retraction_minimum_travel",
    "filament_retract_when_changing_layer",
    "filament_wipe",
    "filament_wipe_distance",
    "filament_retract_before_wipe",
    "filament_long_retractions_when_cut",
    "filament_retraction_distances_when_cut",
    "long_retractions_when_ec",
    "retraction_distances_when_ec",
    "nozzle_temperature_initial_layer",
    "nozzle_temperature",
    "filament_flush_volumetric_speed",
    "filament_flush_temp",
    "filament_cooling_before_tower",
    "volumetric_speed_coefficients",
    "filament_adaptive_volumetric_speed",
    "filament_ironing_flow",
    "filament_ironing_spacing",
    "filament_ironing_inset",
    "filament_ironing_speed",
    "activate_air_filtration",
    "activate_air_filtration_during_print",
    "activate_air_filtration_on_completion",
    "during_print_exhaust_fan_speed",
    "complete_print_exhaust_fan_speed",
];

fn items(v: &str) -> Vec<&str> {
    v.split(',').map(str::trim).collect()
}

/// `ConfigOptionVector::get_at`: past the end is the first.
fn at<'a>(list: &[&'a str], i: usize) -> &'a str {
    list.get(i).or(list.first()).copied().unwrap_or("")
}

/// The variant each nozzle has fitted, as `get_extruder_variant_string` writes it.
fn fitted(schema: &Schema, printer: &Config) -> Vec<String> {
    let nozzles = items(printer.get(schema, "nozzle_diameter")).len();
    let types = items(printer.get(schema, "extruder_type"));
    let volumes = match printer.values.get("nozzle_volume_type") {
        Some(v) if !v.is_empty() => items(v),
        _ => items(printer.get(schema, "default_nozzle_volume_type")),
    };
    (0..nozzles).map(|n| format!("{} {}", at(&types, n), at(&volumes, n))).collect()
}

/// `get_index_for_extruder`: where nozzle `id` (1-based) finds `variant`.
fn index_of(variants: &[&str], ids: Option<&[&str]>, id: usize, variant: &str) -> Option<usize> {
    let wanted = id.to_string();
    (0..variants.len())
        .find(|&i| *variants.get(i).unwrap_or(&"") == variant && ids.is_none_or(|ids| at(ids, i) == wanted))
}

/// Keeps the entries at `picks` (each `stride` long) of every key of `keys` that `config` sets.
fn keep(config: &mut Config, keys: &[&str], picks: &[usize], stride: usize) {
    for key in keys {
        let Some(value) = config.values.get(*key) else { continue };
        let list = items(value);
        let kept: Vec<&str> =
            picks.iter().flat_map(|p| (0..stride).map(move |k| p * stride + k)).map(|i| at(&list, i)).collect();
        let joined = kept.join(",");
        config.values.insert((*key).to_owned(), joined);
    }
}

/// `update_values_to_printer_extruders` for a whole job: `filament_map` gives each filament's
/// nozzle, 1-based, and what it does not list prints with nozzle 1.
pub fn for_nozzles(schema: &Schema, printer: &mut Config, process: &mut Config, filaments: &mut [Config]) {
    let fitted = fitted(schema, printer);
    let listed = printer.get(schema, "extruder_variant_list").to_owned();
    let mut different = items(&listed);
    different.retain(|v| !v.is_empty());
    different.sort_unstable();
    different.dedup();
    if fitted.len() < 2 && different.len() < 2 {
        return;
    }
    let picks = |config: &Config, id_key: &str, variant_key: &str| -> Vec<usize> {
        let (ids, variants) = (config.get(schema, id_key).to_owned(), config.get(schema, variant_key).to_owned());
        let (ids, variants) = (items(&ids), items(&variants));
        // Nothing fits in a half-made config: Orca takes the first.
        let index = |(n, v): (usize, &String)| index_of(&variants, Some(&ids), n + 1, v).unwrap_or(0);
        fitted.iter().enumerate().map(index).collect()
    };
    let chosen = picks(printer, "printer_extruder_id", "printer_extruder_variant");
    keep(printer, &PRINTER_ONE, &chosen, 1);
    keep(printer, &PRINTER_TWO, &chosen, 2);
    let chosen = picks(process, "print_extruder_id", "print_extruder_variant");
    keep(process, &PROCESS, &chosen, 1);

    let map = process.get(schema, "filament_map").to_owned();
    let map = items(&map);
    for (k, filament) in filaments.iter_mut().enumerate() {
        let id = map.get(k).and_then(|v| v.parse::<usize>().ok()).unwrap_or(1);
        let Some(variant) = id.checked_sub(1).and_then(|n| fitted.get(n)) else { continue };
        let variants = filament.get(schema, "filament_extruder_variant").to_owned();
        let index = index_of(&items(&variants), None, id, variant).unwrap_or(0);
        keep(filament, &FILAMENT, &[index], 1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn x2d() -> (Config, Config, Config) {
        let mut printer = Config::default();
        printer.set("nozzle_diameter", "0.4,0.4");
        printer.set("extruder_type", "Direct Drive,Bowden");
        printer.set("nozzle_volume_type", "Standard");
        printer.set("extruder_variant_list", "Direct Drive Standard,Direct Drive High Flow,Bowden Standard");
        printer.set("printer_extruder_id", "1,1,2");
        printer.set("printer_extruder_variant", "Direct Drive Standard,Direct Drive High Flow,Bowden Standard");
        printer.set("retraction_length", "0.8,0.6,2");
        printer.set("machine_max_speed_x", "500,200,400,100,300,50");
        let mut process = Config::default();
        process.set("filament_map", "1,2");
        let mut filament = Config::default();
        filament.set("filament_extruder_variant", "Direct Drive High Flow,Direct Drive Standard,Bowden Standard");
        filament.set("nozzle_temperature", "230,220,240");
        (printer, process, filament)
    }

    #[test]
    fn each_nozzle_keeps_the_variant_it_has() {
        let (mut printer, mut process, filament) = x2d();
        let mut filaments = [filament.clone(), filament];
        for_nozzles(Schema::get(), &mut printer, &mut process, &mut filaments);
        let value = |c: &Config, k: &str| c.values.get(k).cloned().unwrap_or_default();
        assert_eq!(value(&printer, "retraction_length"), "0.8,2");
        assert_eq!(value(&printer, "machine_max_speed_x"), "500,200,300,50");
        assert_eq!(value(&filaments[0], "nozzle_temperature"), "220");
        assert_eq!(value(&filaments[1], "nozzle_temperature"), "240");
    }

    #[test]
    fn twice_changes_nothing() {
        let (mut printer, mut process, filament) = x2d();
        let mut filaments = [filament];
        let schema = Schema::get();
        for_nozzles(schema, &mut printer, &mut process, &mut filaments);
        let once = (printer.clone(), filaments[0].clone());
        for_nozzles(schema, &mut printer, &mut process, &mut filaments);
        assert_eq!(once.0.values, printer.values);
        assert_eq!(once.1.values, filaments[0].values);
    }

    #[test]
    fn one_plain_nozzle_is_left_alone() {
        let mut printer = Config::default();
        printer.set("nozzle_diameter", "0.4");
        printer.set("retraction_length", "0.8,0.6");
        let before = printer.clone();
        for_nozzles(Schema::get(), &mut printer, &mut Config::default(), &mut []);
        assert_eq!(before.values, printer.values);
    }

    #[test]
    fn garbage_does_not_crash() {
        let mut printer = Config::default();
        printer.set("nozzle_diameter", ",,,");
        printer.set("extruder_type", "");
        printer.set("printer_extruder_variant", ",");
        printer.set("retraction_length", "");
        let mut process = Config::default();
        process.set("filament_map", "0,x,99");
        let mut filaments = [Config::default(), Config::default(), Config::default()];
        for_nozzles(Schema::get(), &mut printer, &mut process, &mut filaments);
    }
}
