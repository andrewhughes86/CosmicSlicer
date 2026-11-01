//! The options the engine reads, resolved from OrcaSlicer-style values into numbers.
//!
//! Everything is clamped to a sane range here, so the stages after it never see a zero
//! layer height, a negative width or a NaN speed, whatever a profile says.

use CosmicSlicer_config::{Config, Schema, first_number, is_true};

/// The printer, process and one object's overrides, merged.
#[derive(Clone, Debug)]
pub struct Settings {
    pub nozzle: f64,
    pub layer_height: f64,
    pub first_layer_height: f64,
    pub widths: Widths,
    pub wall_loops: usize,
    pub top_layers: usize,
    pub bottom_layers: usize,
    /// 0 to 1.
    pub density: f64,
    pub sparse_pattern: String,
    pub top_pattern: String,
    pub bottom_pattern: String,
    pub solid_pattern: String,
    /// Radians.
    pub infill_angle: f64,
    pub solid_angle: f64,
    /// Fractions of the inner wall width.
    pub infill_wall_overlap: f64,
    pub solid_wall_overlap: f64,
    pub speeds: Speeds,
    pub accel: Accel,
    pub retract: Retract,
    pub skirt_loops: usize,
    pub skirt_distance: f64,
    pub skirt_height: usize,
    pub brim_type: String,
    pub brim_width: f64,
    pub brim_gap: f64,
    pub support: Support,
    pub gcode: GcodeSettings,
    pub motion: Motion,
    pub seam: String,
    pub outer_first: bool,
    pub spiral: bool,
    pub bridge_flow: f64,
    pub only_one_wall_top: bool,
    pub ironing: bool,
    pub z_offset: f64,
    pub max_print_height: f64,
}

#[derive(Clone, Copy, Debug)]
pub struct Widths {
    pub outer: f64,
    pub inner: f64,
    pub top: f64,
    pub sparse: f64,
    pub solid: f64,
    pub support: f64,
    pub first_layer: f64,
}

/// mm/s.
#[derive(Clone, Copy, Debug)]
pub struct Speeds {
    pub outer: f64,
    pub inner: f64,
    pub sparse: f64,
    pub solid: f64,
    pub top: f64,
    pub travel: f64,
    pub first_layer: f64,
    pub first_layer_infill: f64,
    pub gap: f64,
    pub bridge: f64,
    pub support: f64,
    pub support_interface: f64,
    pub skirt: f64,
    pub travel_z: f64,
}

/// mm/s².
#[derive(Clone, Copy, Debug)]
pub struct Accel {
    pub default: f64,
    pub outer: f64,
    pub inner: f64,
    pub sparse: f64,
    pub solid: f64,
    pub top: f64,
    pub bridge: f64,
    pub travel: f64,
    pub first_layer: f64,
}

/// The firmware the G-code is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Flavor {
    Marlin,
    Marlin2,
    Klipper,
    RepRap,
    Repetier,
}

impl Flavor {
    pub fn from_name(s: &str) -> Flavor {
        match s {
            "klipper" => Flavor::Klipper,
            "marlin2" => Flavor::Marlin2,
            "reprapfirmware" | "reprap" => Flavor::RepRap,
            "repetier" => Flavor::Repetier,
            _ => Flavor::Marlin,
        }
    }
}

/// The printer's motion limits (OrcaSlicer's "Motion ability"), for the G-code and the
/// time estimate. Axes are X, Y, Z, E.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Limits {
    pub max_speed: [f64; 4],
    pub max_accel: [f64; 4],
    pub accel_extruding: f64,
    pub accel_retracting: f64,
    pub accel_travel: f64,
    pub jerk: [f64; 4],
    /// Marlin 2 junction deviation (mm); 0 uses the jerk.
    pub junction_deviation: f64,
    pub min_extruding_rate: f64,
    pub min_travel_rate: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ZHop {
    Normal,
    /// Rising along the start of the travel.
    Slope,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArcFitting {
    Off,
    All,
    SparseInfill,
}

/// How the head moves, and what the G-code says about it.
#[derive(Clone, Debug)]
pub struct Motion {
    pub flavor: Flavor,
    pub limits: Limits,
    pub emit_limits: bool,
    /// Accelerations are set per feature (OrcaSlicer: default acceleration above 0).
    pub accel_control: bool,
    pub accel_to_decel: Option<f64>,
    /// Jerk per feature (mm/s); 0 leaves the firmware's.
    pub jerk_default: f64,
    pub jerk_outer: f64,
    pub jerk_inner: f64,
    pub jerk_infill: f64,
    pub jerk_top: f64,
    pub jerk_first_layer: f64,
    pub jerk_travel: f64,
    pub firmware_retraction: bool,
    pub z_hop: ZHop,
    /// Lift only between these heights (0 for "no limit" above).
    pub lift_above: f64,
    pub lift_below: f64,
    /// Degrees, for sloped lifts.
    pub travel_slope: f64,
    pub wipe_distance: f64,
    /// Share of the retraction done while wiping (0 to 1).
    pub retract_before_wipe: f64,
    pub restart_extra: f64,
    pub avoid_crossing_walls: bool,
    /// Longest detour avoiding walls may add (mm); 0 for no limit.
    pub max_detour: f64,
    /// Skip retraction on travels that stay inside the part.
    pub reduce_infill_retraction: bool,
    pub arc_fitting: ArcFitting,
    pub exclude_object: bool,
    pub label_objects: bool,
    /// e.g. "48x48/PNG,300x300/PNG".
    pub thumbnails: String,
    pub first_layer_travel_speed: f64,
}

#[derive(Clone, Copy, Debug)]
pub struct Retract {
    pub length: f64,
    pub speed: f64,
    pub deretract_speed: f64,
    pub z_hop: f64,
    pub min_travel: f64,
    pub on_layer_change: bool,
    pub wipe: bool,
    pub toolchange_length: f64,
}

#[derive(Clone, Debug)]
pub struct Support {
    pub enabled: bool,
    pub tree: bool,
    /// Overhangs steeper than this from vertical need support (degrees from horizontal as
    /// OrcaSlicer counts: 30 means faces within 30° of horizontal).
    pub threshold_angle: f64,
    pub spacing: f64,
    pub top_z_distance: f64,
    pub xy_distance: f64,
    pub interface_layers: usize,
    pub interface_spacing: f64,
    pub build_plate_only: bool,
}

#[derive(Clone, Debug)]
pub struct GcodeSettings {
    pub flavor: String,
    pub start: String,
    pub end: String,
    pub before_layer: String,
    pub layer_change: String,
    pub relative_e: bool,
    pub printer_model: String,
}

/// One filament's properties.
#[derive(Clone, Debug)]
pub struct Filament {
    pub diameter: f64,
    /// g/cm³.
    pub density: f64,
    /// Per kg.
    pub cost: f64,
    pub flow_ratio: f64,
    pub nozzle_temp: f64,
    pub nozzle_temp_first: f64,
    pub bed_temp: f64,
    pub bed_temp_first: f64,
    /// 0 to 100.
    pub fan_min: f64,
    pub fan_max: f64,
    pub fan_off_layers: usize,
    pub full_fan_layer: usize,
    pub overhang_fan: f64,
    pub slow_down_layer_time: f64,
    pub slow_down_min_speed: f64,
    pub max_volumetric: f64,
    pub kind: String,
    pub colour: String,
    /// Pressure advance (K) when the filament turns it on.
    pub pressure_advance: Option<f64>,
}

/// Reads one value with the schema default behind it.
pub struct Reader<'a> {
    pub schema: &'a Schema,
    pub config: &'a Config,
}

impl Reader<'_> {
    pub fn text(&self, key: &str) -> String {
        self.config.get(self.schema, key).to_owned()
    }

    /// The first element of a list value, as text.
    pub fn first(&self, key: &str) -> String {
        self.config.get(self.schema, key).split(',').next().unwrap_or("").trim().to_owned()
    }

    pub fn num(&self, key: &str, fallback: f64) -> f64 {
        first_number(self.config.get(self.schema, key)).filter(|v| v.is_finite()).unwrap_or(fallback)
    }

    /// A number clamped to `lo..=hi`; `fallback` when unreadable.
    pub fn range(&self, key: &str, fallback: f64, lo: f64, hi: f64) -> f64 {
        self.num(key, fallback).clamp(lo, hi)
    }

    pub fn count(&self, key: &str, fallback: usize, hi: usize) -> usize {
        let v = self.num(key, fallback as f64);
        if v.is_finite() && v > 0.0 { (v.round() as usize).min(hi) } else { 0 }
    }

    pub fn flag(&self, key: &str) -> bool {
        is_true(self.config.get(self.schema, key))
    }

    /// A value that is millimetres or a percentage of `of`.
    pub fn mm_or_percent(&self, key: &str, of: f64) -> Option<f64> {
        let raw = self.first(key);
        let v = if let Some(p) = raw.strip_suffix('%') {
            p.trim().parse::<f64>().ok()? * of / 100.0
        } else {
            raw.parse::<f64>().ok()?
        };
        (v.is_finite() && v > 0.0).then_some(v)
    }

    /// A speed in mm/s; 0 or unreadable takes `fallback`.
    pub fn speed(&self, key: &str, fallback: f64) -> f64 {
        let v = self.num(key, fallback);
        if v.is_finite() && v > 0.0 { v.clamp(1.0, 2000.0) } else { fallback }
    }
}

impl Settings {
    pub fn read(schema: &Schema, config: &Config) -> Settings {
        let r = Reader { schema, config };
        let nozzle = r.range("nozzle_diameter", 0.4, 0.05, 3.0);
        let layer_height = r.range("layer_height", 0.2, 0.01, nozzle * 0.95);
        let first_layer_height = r.range("initial_layer_print_height", layer_height, 0.01, nozzle * 1.2);
        // OrcaSlicer's "0" widths mean automatic, from the nozzle.
        let default_width = r.mm_or_percent("line_width", nozzle);
        let width = |key: &str, auto: f64| {
            r.mm_or_percent(key, nozzle).or(default_width).unwrap_or(nozzle * auto).clamp(nozzle * 0.5, nozzle * 3.0)
        };
        let widths = Widths {
            outer: width("outer_wall_line_width", 1.05),
            inner: width("inner_wall_line_width", 1.125),
            top: width("top_surface_line_width", 1.05),
            sparse: width("sparse_infill_line_width", 1.125),
            solid: width("internal_solid_infill_line_width", 1.05),
            support: width("support_line_width", 1.05),
            first_layer: width("initial_layer_line_width", 1.25),
        };
        let shells = |layers_key: &str, thickness_key: &str| {
            let layers = r.count(layers_key, 3, 1000);
            let thick = r.range(thickness_key, 0.0, 0.0, 1000.0);
            if layers == 0 { 0 } else { layers.max((thick / layer_height).ceil() as usize) }
        };
        let first_layer_speed = r.speed("initial_layer_speed", 30.0);
        let speeds = Speeds {
            outer: r.speed("outer_wall_speed", 60.0),
            inner: r.speed("inner_wall_speed", 60.0),
            sparse: r.speed("sparse_infill_speed", 100.0),
            solid: r.speed("internal_solid_infill_speed", 100.0),
            top: r.speed("top_surface_speed", 100.0),
            travel: r.speed("travel_speed", 120.0),
            first_layer: first_layer_speed,
            first_layer_infill: r.speed("initial_layer_infill_speed", 60.0),
            gap: r.speed("gap_infill_speed", 30.0),
            bridge: r.speed("bridge_speed", 25.0),
            support: r.speed("support_speed", 80.0),
            support_interface: r.speed("support_interface_speed", 80.0),
            skirt: r.speed("skirt_speed", first_layer_speed),
            travel_z: r.speed("travel_speed_z", 10.0),
        };
        let accel = |key: &str, fallback: f64| {
            let v = r.num(key, fallback);
            if v.is_finite() && v > 0.0 { v.clamp(10.0, 100_000.0) } else { fallback }
        };
        let default_accel = accel("default_acceleration", 500.0);
        let support_type = r.text("support_type");
        Settings {
            nozzle,
            layer_height,
            first_layer_height,
            widths,
            wall_loops: r.count("wall_loops", 2, 100),
            top_layers: shells("top_shell_layers", "top_shell_thickness"),
            bottom_layers: shells("bottom_shell_layers", "bottom_shell_thickness"),
            density: r.range("sparse_infill_density", 20.0, 0.0, 100.0) / 100.0,
            sparse_pattern: r.text("sparse_infill_pattern"),
            top_pattern: r.text("top_surface_pattern"),
            bottom_pattern: r.text("bottom_surface_pattern"),
            solid_pattern: r.text("internal_solid_infill_pattern"),
            infill_angle: r.num("infill_direction", 45.0).to_radians(),
            solid_angle: r.num("solid_infill_direction", 45.0).to_radians(),
            infill_wall_overlap: r.range("infill_wall_overlap", 15.0, 0.0, 100.0) / 100.0,
            solid_wall_overlap: r.range("top_bottom_infill_wall_overlap", 25.0, 0.0, 100.0) / 100.0,
            speeds,
            accel: {
                // Some accelerations are a share of another (OrcaSlicer's "%" values).
                let pct = |key: &str, of: f64| r.mm_or_percent(key, of).map_or(of, |v| v.clamp(10.0, 100_000.0));
                let outer = accel("outer_wall_acceleration", default_accel);
                Accel {
                    default: default_accel,
                    outer,
                    inner: accel("inner_wall_acceleration", default_accel),
                    sparse: pct("sparse_infill_acceleration", default_accel),
                    solid: pct("internal_solid_infill_acceleration", default_accel),
                    top: accel("top_surface_acceleration", default_accel),
                    bridge: pct("bridge_acceleration", outer),
                    travel: accel("travel_acceleration", default_accel),
                    first_layer: accel("initial_layer_acceleration", default_accel),
                }
            },
            motion: {
                let axis = |prefix: &str, fallback: [f64; 4], hi: f64| -> [f64; 4] {
                    let mut out = fallback;
                    for (i, a) in ["x", "y", "z", "e"].iter().enumerate() {
                        let v = r.num(&format!("{prefix}_{a}"), fallback[i]);
                        out[i] = if v.is_finite() && v > 0.0 { v.min(hi) } else { fallback[i] };
                    }
                    out
                };
                let positive = |key: &str, fallback: f64| {
                    let v = r.num(key, fallback);
                    if v.is_finite() && v > 0.0 { v } else { fallback }
                };
                let accel_extruding = positive("machine_max_acceleration_extruding", 1500.0);
                let limits = Limits {
                    max_speed: axis("machine_max_speed", [500.0, 500.0, 12.0, 120.0], 10_000.0),
                    max_accel: axis("machine_max_acceleration", [10_000.0, 10_000.0, 500.0, 5000.0], 1.0e6),
                    accel_extruding,
                    accel_retracting: positive("machine_max_acceleration_retracting", accel_extruding),
                    accel_travel: positive("machine_max_acceleration_travel", accel_extruding),
                    jerk: axis("machine_max_jerk", [9.0, 9.0, 0.4, 2.5], 1000.0),
                    junction_deviation: r.range("machine_max_junction_deviation", 0.0, 0.0, 1.0),
                    min_extruding_rate: r.range("machine_min_extruding_rate", 0.0, 0.0, 1000.0),
                    min_travel_rate: r.range("machine_min_travel_rate", 0.0, 0.0, 1000.0),
                };
                let jerk = |key: &str| r.range(key, 0.0, 0.0, 1000.0);
                let travel = r.speed("travel_speed", 120.0);
                Motion {
                    flavor: Flavor::from_name(&r.text("gcode_flavor")),
                    limits,
                    emit_limits: r.flag("emit_machine_limits_to_gcode"),
                    accel_control: r.num("default_acceleration", 0.0) > 0.0,
                    accel_to_decel: r
                        .flag("accel_to_decel_enable")
                        .then(|| r.range("accel_to_decel_factor", 50.0, 1.0, 100.0) / 100.0),
                    jerk_default: jerk("default_jerk"),
                    jerk_outer: jerk("outer_wall_jerk"),
                    jerk_inner: jerk("inner_wall_jerk"),
                    jerk_infill: jerk("infill_jerk"),
                    jerk_top: jerk("top_surface_jerk"),
                    jerk_first_layer: jerk("initial_layer_jerk"),
                    jerk_travel: jerk("travel_jerk"),
                    firmware_retraction: r.flag("use_firmware_retraction"),
                    z_hop: if r.first("z_hop_types") == "Normal Lift" { ZHop::Normal } else { ZHop::Slope },
                    lift_above: r.range("retract_lift_above", 0.0, 0.0, 10_000.0),
                    lift_below: r.range("retract_lift_below", 0.0, 0.0, 10_000.0),
                    travel_slope: r.range("travel_slope", 3.0, 0.5, 90.0),
                    wipe_distance: r.range("wipe_distance", 1.0, 0.0, 50.0),
                    retract_before_wipe: r.range("retract_before_wipe", 100.0, 0.0, 100.0) / 100.0,
                    restart_extra: r.range("retract_restart_extra", 0.0, -10.0, 10.0),
                    avoid_crossing_walls: r.flag("reduce_crossing_wall"),
                    max_detour: r.mm_or_percent("max_travel_detour_distance", 100.0).unwrap_or(0.0),
                    reduce_infill_retraction: r.flag("reduce_infill_retraction"),
                    arc_fitting: match r.text("enable_arc_fitting").as_str() {
                        "enabled" | "1" => ArcFitting::All,
                        "sparse_infill" => ArcFitting::SparseInfill,
                        _ => ArcFitting::Off,
                    },
                    exclude_object: r.flag("exclude_object"),
                    label_objects: r.flag("gcode_label_objects"),
                    thumbnails: r.text("thumbnails"),
                    first_layer_travel_speed: r
                        .mm_or_percent("initial_layer_travel_speed", travel)
                        .unwrap_or(travel)
                        .clamp(1.0, 2000.0),
                }
            },
            retract: Retract {
                length: r.range("retraction_length", 0.8, 0.0, 50.0),
                speed: r.speed("retraction_speed", 30.0),
                deretract_speed: r.speed("deretraction_speed", r.speed("retraction_speed", 30.0)),
                z_hop: r.range("z_hop", 0.0, 0.0, 10.0),
                min_travel: r.range("retraction_minimum_travel", 2.0, 0.0, 1000.0),
                on_layer_change: r.flag("retract_when_changing_layer"),
                wipe: r.flag("wipe"),
                toolchange_length: r.range("retract_length_toolchange", 10.0, 0.0, 200.0),
            },
            skirt_loops: r.count("skirt_loops", 1, 50),
            skirt_distance: r.range("skirt_distance", 2.0, 0.0, 100.0),
            skirt_height: r.count("skirt_height", 1, 10_000),
            brim_type: r.text("brim_type"),
            brim_width: r.range("brim_width", 0.0, 0.0, 100.0),
            brim_gap: r.range("brim_object_gap", 0.0, 0.0, 10.0),
            support: Support {
                enabled: r.flag("enable_support"),
                tree: support_type.starts_with("tree"),
                threshold_angle: r.range("support_threshold_angle", 30.0, 0.0, 90.0),
                spacing: r.range("support_base_pattern_spacing", 2.5, nozzle, 50.0),
                top_z_distance: r.range("support_top_z_distance", 0.2, 0.0, 5.0),
                xy_distance: r.range("support_object_xy_distance", 0.35, 0.0, 10.0),
                interface_layers: r.count("support_interface_top_layers", 3, 20),
                interface_spacing: r.range("support_interface_spacing", 0.5, 0.0, 10.0),
                build_plate_only: r.flag("support_on_build_plate_only"),
            },
            gcode: GcodeSettings {
                flavor: r.text("gcode_flavor"),
                start: r.text("machine_start_gcode"),
                end: r.text("machine_end_gcode"),
                before_layer: r.text("before_layer_change_gcode"),
                layer_change: r.text("layer_change_gcode"),
                relative_e: r.flag("use_relative_e_distances"),
                printer_model: r.text("printer_model"),
            },
            seam: r.text("seam_position"),
            outer_first: r.text("wall_sequence").starts_with("outer"),
            spiral: r.flag("spiral_mode"),
            bridge_flow: r.range("bridge_flow", 1.0, 0.1, 2.0),
            only_one_wall_top: r.flag("only_one_wall_top"),
            ironing: !r.text("ironing_type").starts_with("no"),
            z_offset: r.range("z_offset", 0.0, -5.0, 5.0),
            max_print_height: r.range("printable_height", 250.0, 1.0, 10_000.0),
        }
    }

    /// Centre-to-centre distance of neighbouring lines of width `w` at height `h`: OrcaSlicer's
    /// rounded-rectangle bead overlaps its neighbour by `h·(1 − π/4)`.
    pub fn spacing(w: f64, h: f64) -> f64 {
        (w - h * (1.0 - std::f64::consts::FRAC_PI_4)).max(w * 0.5)
    }
}

impl Filament {
    /// `bed_key` is the plate type's temperature option, e.g. `textured_plate_temp`.
    pub fn read(schema: &Schema, config: &Config, bed_key: &str) -> Filament {
        let r = Reader { schema, config };
        let nozzle_temp = r.range("nozzle_temperature", 210.0, 0.0, 500.0);
        let bed_temp = r.range(bed_key, 60.0, 0.0, 200.0);
        Filament {
            diameter: r.range("filament_diameter", 1.75, 0.5, 5.0),
            density: r.range("filament_density", 1.24, 0.0, 30.0),
            cost: r.range("filament_cost", 0.0, 0.0, 1.0e6),
            flow_ratio: r.range("filament_flow_ratio", 1.0, 0.1, 3.0),
            nozzle_temp,
            nozzle_temp_first: r.range("nozzle_temperature_initial_layer", nozzle_temp, 0.0, 500.0),
            bed_temp,
            bed_temp_first: r.range(&format!("{bed_key}_initial_layer"), bed_temp, 0.0, 200.0),
            fan_min: r.range("fan_min_speed", 20.0, 0.0, 100.0),
            fan_max: r.range("fan_max_speed", 100.0, 0.0, 100.0),
            fan_off_layers: r.count("close_fan_the_first_x_layers", 1, 10_000),
            full_fan_layer: r.count("full_fan_speed_layer", 0, 10_000),
            overhang_fan: r.range("overhang_fan_speed", 100.0, 0.0, 100.0),
            slow_down_layer_time: r.range("slow_down_layer_time", 5.0, 0.0, 1000.0),
            slow_down_min_speed: r.range("slow_down_min_speed", 10.0, 1.0, 1000.0),
            max_volumetric: {
                let v = r.num("filament_max_volumetric_speed", 0.0);
                if v.is_finite() && v > 0.1 { v.min(1000.0) } else { 1000.0 }
            },
            kind: r.first("filament_type"),
            colour: r.first("filament_colour"),
            pressure_advance: r.flag("enable_pressure_advance").then(|| r.range("pressure_advance", 0.0, 0.0, 2.0)),
        }
    }

    /// Cross-section of the filament in mm².
    pub fn area(&self) -> f64 {
        std::f64::consts::PI * (self.diameter / 2.0).powi(2)
    }
}

/// Cross-section of a bead `w` wide and `h` high (OrcaSlicer's rounded rectangle), mm².
pub fn bead_area(w: f64, h: f64) -> f64 {
    if w <= h {
        // A round bead: the line is narrower than it is tall.
        std::f64::consts::PI * (w / 2.0).powi(2)
    } else {
        (w - h) * h + std::f64::consts::PI * (h / 2.0).powi(2)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_sane() {
        let s = Settings::read(Schema::get(), &Config::default());
        assert!((s.layer_height - 0.2).abs() < 1e-9);
        assert!((s.widths.outer - 0.42).abs() < 1e-9);
        assert_eq!(s.wall_loops, 2);
        assert!(s.top_layers >= 3 && s.bottom_layers >= 3);
        assert!((s.density - 0.2).abs() < 1e-9);
        assert!(s.retract.length > 0.0);
    }

    #[test]
    fn hostile_values_are_clamped() {
        let mut c = Config::default();
        c.set("layer_height", "0");
        c.set("wall_loops", "-5");
        c.set("outer_wall_speed", "nan");
        c.set("line_width", "500%");
        let s = Settings::read(Schema::get(), &c);
        assert!(s.layer_height >= 0.01);
        assert_eq!(s.wall_loops, 0);
        assert!(s.speeds.outer > 0.0 && s.speeds.outer.is_finite());
        assert!(s.widths.inner <= s.nozzle * 3.0);
    }

    #[test]
    fn bead_area_matches_orca() {
        // 0.45 × 0.2: (0.45 − 0.2)·0.2 + π·0.1² ≈ 0.0814.
        assert!((bead_area(0.45, 0.2) - 0.081_415_9).abs() < 1e-6);
    }
}
