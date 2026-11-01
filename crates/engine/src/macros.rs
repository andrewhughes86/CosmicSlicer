//! The values custom G-code can use, and rendering it with OrcaSlicer's macro language.
//!
//! Option values come from the merged printer, process and filament settings; the slicer adds
//! what only it knows (temperatures for the plate type, the first layer's bounds, layer
//! numbers, extruder numbers). A template that uses something unknown, or is malformed, is an
//! error that stops the slice: wrong G-code must never reach a printer.

use std::cell::RefCell;
use std::collections::HashMap;

use CosmicSlicer_config::expr::{self, ConfigVars, Value, Vars};
use CosmicSlicer_config::{Config, Schema};

use crate::settings::{Filament, Settings};

/// Everything a template can read, for one slice.
pub struct Macros<'a> {
    vars: ConfigVars<'a>,
    /// `{global x = …}` values, shared by every template of the slice.
    globals: RefCell<HashMap<String, Value>>,
}

/// Per-call additions, e.g. `layer_num`.
struct Layered<'a> {
    base: &'a ConfigVars<'a>,
    layer: &'a HashMap<String, Value>,
}

impl Vars for Layered<'_> {
    fn var(&self, name: &str) -> Option<Value> {
        self.layer.get(name).cloned().or_else(|| self.base.var(name))
    }
}

fn nums(v: impl IntoIterator<Item = f64>) -> Value {
    Value::Vec(v.into_iter().map(Value::Num).collect())
}

/// What the slicer knows about the plate, for the template variables.
pub struct PlateFacts {
    pub plate_name: String,
    /// The plate type's name as OrcaSlicer shows it ("Textured PEI Plate").
    pub bed_type_name: String,
    pub first_layer_min: [f64; 2],
    pub first_layer_max: [f64; 2],
    pub bed_min: [f64; 2],
    pub bed_max: [f64; 2],
    pub layer_count: usize,
    pub max_z: f64,
    /// Extruders (0-based) that print something.
    pub used_extruders: Vec<usize>,
    pub initial_extruder: usize,
}

impl<'a> Macros<'a> {
    /// `table` is the merged config: printer and process values, filament options as lists
    /// across the extruders.
    pub fn new(
        schema: &'a Schema,
        table: &'a Config,
        s: &Settings,
        fil: &[Filament],
        facts: &PlateFacts,
    ) -> Macros<'a> {
        let n = fil.len().max(1);
        let e = facts.initial_extruder as f64;
        let mut x: HashMap<String, Value> = HashMap::new();
        let mut put = |k: &str, v: Value| {
            x.insert(k.to_owned(), v);
        };
        let first_temps = nums(fil.iter().map(|f| f.nozzle_temp_first));
        let temps = nums(fil.iter().map(|f| f.nozzle_temp));
        let bed_first = nums(fil.iter().map(|f| f.bed_temp_first));
        let bed = nums(fil.iter().map(|f| f.bed_temp));
        let bed_first_max = fil.iter().map(|f| f.bed_temp_first).fold(0.0, f64::max);
        put("nozzle_temperature_initial_layer", first_temps.clone());
        put("first_layer_temperature", first_temps);
        put("nozzle_temperature", temps.clone());
        put("temperature", temps);
        put("bed_temperature_initial_layer", bed_first.clone());
        put("first_layer_bed_temperature", bed_first);
        put("bed_temperature", bed);
        put("bed_temperature_initial_layer_single", Value::Num(bed_first_max));
        put("bed_temperature_initial_layer_vector", Value::Str(format!("{bed_first_max:.0}")));
        put("curr_bed_type", Value::Str(facts.bed_type_name.clone()));
        put("plate_name", Value::Str(facts.plate_name.clone()));
        for (k, v) in [
            ("initial_extruder", e),
            ("initial_tool", e),
            ("current_extruder", e),
            ("next_extruder", e),
            ("previous_extruder", -1.0),
            ("initial_no_support_extruder", e),
            ("initial_no_support_filament_id", e),
            ("initial_no_support_hotend", 0.0),
            ("initial_filament_id", e),
            ("current_filament_id", e),
            ("next_filament_id", e),
            ("initial_nozzle_id", 0.0),
            ("current_nozzle_id", 0.0),
            ("next_nozzle_id", 0.0),
            ("current_hotend", 0.0),
            ("most_used_physical_extruder_id", 0.0),
            ("toolchange_count", 0.0),
            ("extruded_volume_total", 0.0),
            ("extruded_weight_total", 0.0),
            ("total_layer_count", facts.layer_count as f64),
            ("layer_num", 0.0),
            ("layer_z", s.first_layer_height),
            ("previous_layer_z", 0.0),
            ("max_layer_z", facts.max_z),
            ("max_print_height", s.max_print_height),
            ("first_layer_height", s.first_layer_height),
            ("timelapse_type", 0.0),
        ] {
            put(k, Value::Num(v));
        }
        let pt = |p: [f64; 2]| nums(p);
        put("first_layer_print_min", pt(facts.first_layer_min));
        put("first_layer_print_max", pt(facts.first_layer_max));
        put(
            "first_layer_print_size",
            pt([
                facts.first_layer_max[0] - facts.first_layer_min[0],
                facts.first_layer_max[1] - facts.first_layer_min[1],
            ]),
        );
        put(
            "first_layer_center_no_wipe_tower",
            pt([
                (facts.first_layer_min[0] + facts.first_layer_max[0]) / 2.0,
                (facts.first_layer_min[1] + facts.first_layer_max[1]) / 2.0,
            ]),
        );
        put("adaptive_bed_mesh_min", pt(facts.first_layer_min));
        put("adaptive_bed_mesh_max", pt(facts.first_layer_max));
        put("print_bed_min", pt(facts.bed_min));
        put("print_bed_max", pt(facts.bed_max));
        put("print_bed_size", pt([facts.bed_max[0] - facts.bed_min[0], facts.bed_max[1] - facts.bed_min[1]]));
        put("bed_mesh_probe_count", nums([5.0, 5.0]));
        // One entry per extruder the printer has, so templates that walk every tool see the unused ones.
        // Templates written for up to 16 tools check every slot; the ones not there are unused.
        let tools = table.get(schema, "nozzle_diameter").split(',').count().max(n).max(16);
        let used: Vec<Value> = (0..tools).map(|i| Value::Bool(facts.used_extruders.contains(&i))).collect();
        put("is_extruder_used", Value::Vec(used));
        put("filament_map", nums((0..n).map(|_| 1.0)));
        put("filament_extruder_id", nums((0..n).map(|_| 1.0)));
        put("first_filaments", nums([e]));
        put("first_non_support_filaments", nums([e]));
        // The outer wall's volumetric speed, which Bambu's start G-code uses for its purge line.
        let w = s.widths.outer;
        let area = crate::settings::bead_area(w, s.layer_height);
        let max_vol = fil.get(facts.initial_extruder).map_or(1000.0, |f| f.max_volumetric);
        put("outer_wall_volumetric_speed", Value::Num((s.speeds.outer * area).min(max_vol)));
        put("filament_max_volumetric_speed", nums(fil.iter().map(|f| f.max_volumetric)));
        put("flush_volumetric_speeds", nums(fil.iter().map(|f| f.max_volumetric)));
        put("flush_temperatures", nums(fil.iter().map(|f| f.nozzle_temp)));
        put("nozzle_diameter_at_nozzle_id", nums([s.nozzle]));
        put("relative_e_axis", Value::Bool(s.gcode.relative_e));
        put("input_filename", Value::Str(String::new()));
        put("input_filename_base", Value::Str(facts.plate_name.clone()));
        for k in [
            "in_head_wrap_detect_zone",
            "long_retraction_when_cut",
            "has_timelapse_safe_pos",
            "timelapse_inline_photo",
            "clear_to_x0",
            "is_prime_tower_interface",
            "wipe_tower_wait_pos_valid",
            "wipe_tower_center_pos_valid",
            "wipe_avoid_perimeter",
        ] {
            put(k, Value::Bool(false));
        }
        for k in [
            "current_extruder_id",
            "curr_physical_extruder_id",
            "next_hotend",
            "imex_mode_index",
            "timelapse_pos_x",
            "timelapse_pos_y",
            "retraction_distance_when_cut",
            "curr_accumulated_mass",
            "previous_layer_mass",
            "curr_layer_mass",
        ] {
            put(k, Value::Num(0.0));
        }
        put("imex_mode", Value::Str("normal".into()));
        put("imex_mode_gcode", Value::Str(String::new()));
        let chamber = table
            .get(schema, "chamber_temperature")
            .split(',')
            .filter_map(|v| v.trim().parse::<f64>().ok())
            .fold(0.0, f64::max);
        put("overall_chamber_temperature", Value::Num(chamber));
        put(
            "curr_y_acceleration_limit",
            Value::Num(
                CosmicSlicer_config::first_number(table.get(schema, "machine_max_acceleration_y")).unwrap_or(5000.0),
            ),
        );
        // Older names still found in profiles.
        put("retract_length", nums([s.retract.length]));
        put("farthest_point_timelapse_enabled", Value::Bool(false));
        put("max_print_z", Value::Num(facts.max_z));
        let tpu = facts
            .used_extruders
            .iter()
            .any(|&i| fil.get(i).is_some_and(|f| f.kind.to_ascii_uppercase().contains("TPU")));
        put("has_tpu_in_first_layer", Value::Bool(tpu));
        put("first_non_support_hotend", Value::Num(0.0));
        put("hold_chamber_temp_for_flat_print", Value::Bool(false));
        put("long_retraction_when_ec", Value::Bool(false));
        let vitrification = table
            .get(schema, "temperature_vitrification")
            .split(',')
            .filter_map(|v| v.trim().parse::<f64>().ok())
            .fold(f64::INFINITY, f64::min);
        put("min_vitrification_temperature", Value::Num(if vitrification.is_finite() { vitrification } else { 0.0 }));
        // OrcaSlicer's writable state: where the head is, how far each extruder is retracted.
        put("zhop", Value::Num(0.0));
        put("position", nums([0.0, 0.0, 0.0]));
        put("e_position", nums((0..n).map(|_| 0.0)));
        put("e_retracted", nums((0..n).map(|_| 0.0)));
        put("e_restart_extra", nums((0..n).map(|_| 0.0)));
        Macros { vars: ConfigVars { schema, config: table, extra: x }, globals: RefCell::new(HashMap::new()) }
    }

    /// Renders `template` (named `what` in errors), with `layer` values on top.
    pub fn render(&self, what: &str, template: &str, layer: &[(&str, Value)]) -> Result<String, String> {
        if !template.contains(['{', '[']) {
            return Ok(template.to_owned());
        }
        let layer: HashMap<String, Value> = layer.iter().map(|(k, v)| ((*k).to_owned(), v.clone())).collect();
        let vars = Layered { base: &self.vars, layer: &layer };
        let mut globals = self.globals.borrow_mut();
        expr::render(template, &vars, &mut globals).map_err(|e| format!("{what}: {e}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts() -> PlateFacts {
        PlateFacts {
            plate_name: "Plate 1".into(),
            bed_type_name: "Textured PEI Plate".into(),
            first_layer_min: [100.0, 100.0],
            first_layer_max: [120.0, 120.0],
            bed_min: [0.0, 0.0],
            bed_max: [220.0, 220.0],
            layer_count: 100,
            max_z: 20.0,
            used_extruders: vec![0],
            initial_extruder: 0,
        }
    }

    #[test]
    fn templates_render_with_slicer_values() {
        let schema = Schema::get();
        let table = Config::default();
        let s = Settings::read(schema, &table);
        let fil = vec![Filament::read(schema, &table, "textured_plate_temp")];
        let m = Macros::new(schema, &table, &s, &fil, &facts());
        let out = m.render(
            "start",
            "M190 S[bed_temperature_initial_layer_single]\n{if curr_bed_type==\"Textured PEI Plate\"}G29.1 Z{-0.04}{endif}\nG1 X{first_layer_print_min[0]}",
            &[],
        );
        assert_eq!(out.as_deref(), Ok("M190 S45\nG29.1 Z-0.04\nG1 X100"));
        let layer = m.render(
            "layer",
            "; layer {layer_num} at {layer_z}",
            &[("layer_num", Value::Num(3.0)), ("layer_z", Value::Num(0.8))],
        );
        assert_eq!(layer.as_deref(), Ok("; layer 3 at 0.8"));
        assert!(m.render("start", "{no_such_variable}", &[]).is_err_and(|e| e.contains("no_such_variable")));
    }
}

#[cfg(test)]
mod library_survey {
    use super::*;
    use CosmicSlicer_config::TabKind;
    use CosmicSlicer_config::library::{COMMON_VENDOR, Library};

    /// Every printer in OrcaSlicer's library: its start, end and layer G-code must render.
    #[test]
    fn every_library_printer_renders_its_gcode() {
        let lib = Library::get();
        let schema = Schema::get();
        let pla =
            lib.resolve(TabKind::Filament, "Generic PLA @System", COMMON_VENDOR).map(|p| p.config).unwrap_or_default();
        let mut failures: Vec<String> = Vec::new();
        let mut unknown: HashMap<String, usize> = HashMap::new();
        let mut total = 0;
        for v in &lib.vendors {
            for p in &v.printers {
                let Some(printer) = lib.resolve(TabKind::Printer, &p.name, &v.id) else { continue };
                total += 1;
                let table = pla.overlaid(&printer.config);
                let s = Settings::read(schema, &table);
                let fil = vec![Filament::read(schema, &pla, "textured_plate_temp")];
                let m = Macros::new(schema, &table, &s, &fil, &tests_facts());
                for key in
                    ["machine_start_gcode", "machine_end_gcode", "layer_change_gcode", "before_layer_change_gcode"]
                {
                    let template = table.values.get(key).cloned().unwrap_or_default();
                    let layer = [
                        ("layer_num", Value::Num(5.0)),
                        ("layer_z", Value::Num(1.2)),
                        ("previous_layer_z", Value::Num(1.0)),
                    ];
                    let rendered = m.render(key, &template, &layer);
                    if let Ok(text) = &rendered {
                        // No template syntax may survive into the G-code (comments aside).
                        let leftover = text
                            .lines()
                            .map(|l| l.split(';').next().unwrap_or(""))
                            .any(|code| code.contains(['{', '}']));
                        if leftover {
                            failures.push(format!("{} / {}: {key}: template text left in the G-code", v.id, p.name));
                        }
                    }
                    if let Err(e) = rendered {
                        if let Some(name) = e.split("unknown variable \"").nth(1).and_then(|r| r.split('"').next()) {
                            *unknown.entry(name.to_owned()).or_default() += 1;
                        }
                        failures.push(format!("{} / {}: {e}", v.id, p.name));
                    }
                }
            }
        }
        let mut u: Vec<(String, usize)> = unknown.into_iter().collect();
        u.sort_by_key(|x| std::cmp::Reverse(x.1));
        println!("{} printers, {} failing templates; unknown: {:?}", total, failures.len(), u);
        for f in failures.iter().filter(|f| !f.contains("unknown variable")).take(40) {
            println!("{f}");
        }
        assert!(total > 1000);
        assert!(failures.is_empty(), "{} templates fail; first: {:?}", failures.len(), failures.first());
    }

    fn tests_facts() -> PlateFacts {
        PlateFacts {
            plate_name: "Plate 1".into(),
            bed_type_name: "Textured PEI Plate".into(),
            first_layer_min: [100.0, 100.0],
            first_layer_max: [120.0, 120.0],
            bed_min: [0.0, 0.0],
            bed_max: [256.0, 256.0],
            layer_count: 100,
            max_z: 20.0,
            used_extruders: vec![0],
            initial_extruder: 0,
        }
    }
}
