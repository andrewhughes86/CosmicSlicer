//! OrcaSlicer's Calibration menu: each test set up as Orca's Plater does (its model, cut to the
//! range asked for and scaled to the nozzle, the settings it needs, and the engine's
//! per-layer changes), its instructions, and the result written back into the presets.

use glam::{Mat4, Vec3};

use CosmicSlicer_config::{TabKind, first_number};
use CosmicSlicer_engine::calib::{Calib, Mode};
use CosmicSlicer_model::{Mesh, primitives};

use crate::state::Session;

/// Every test the Calibration menu offers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Test {
    Temperature,
    FlowPass1,
    FlowPass2,
    FlowYolo,
    FlowYoloFine,
    PaLine,
    PaPattern,
    PaTower,
    Retraction,
    MaxFlow,
    Vfa,
    ShaperFreq,
    ShaperDamp,
    Cornering,
}

impl Test {
    pub const ALL: [Test; 14] = [
        Test::Temperature,
        Test::FlowPass1,
        Test::FlowPass2,
        Test::FlowYolo,
        Test::FlowYoloFine,
        Test::PaLine,
        Test::PaPattern,
        Test::PaTower,
        Test::Retraction,
        Test::MaxFlow,
        Test::Vfa,
        Test::ShaperFreq,
        Test::ShaperDamp,
        Test::Cornering,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Test::Temperature => "Temperature",
            Test::FlowPass1 => "Flow rate: Pass 1",
            Test::FlowPass2 => "Flow rate: Pass 2",
            Test::FlowYolo => "Flow rate: YOLO (Recommended)",
            Test::FlowYoloFine => "Flow rate: YOLO (perfectionist version)",
            Test::PaLine => "Pressure advance: Line",
            Test::PaPattern => "Pressure advance: Pattern",
            Test::PaTower => "Pressure advance: Tower",
            Test::Retraction => "Retraction test",
            Test::MaxFlow => "Max flowrate",
            Test::Vfa => "VFA",
            Test::ShaperFreq => "Input shaping frequency",
            Test::ShaperDamp => "Input shaping damping/zeta factor",
            Test::Cornering => "Cornering",
        }
    }

    /// The menu's names for the tests (as `Action::Calibration` carries them).
    pub fn from_name(name: &str) -> Option<Test> {
        Test::ALL.into_iter().find(|t| t.label() == name)
    }

    fn flow(self) -> bool {
        matches!(self, Test::FlowPass1 | Test::FlowPass2 | Test::FlowYolo | Test::FlowYoloFine)
    }

    /// The range the test steps through: (start, end, step) and its unit, or `None` when it
    /// has none to choose.
    pub fn range_unit(self) -> Option<&'static str> {
        match self {
            Test::Temperature => Some("°C"),
            Test::PaLine | Test::PaPattern | Test::PaTower => Some(""),
            Test::Retraction => Some("mm"),
            Test::MaxFlow => Some("mm³/s"),
            Test::Vfa => Some("mm/s"),
            Test::ShaperFreq => Some("Hz"),
            Test::ShaperDamp => Some(""),
            Test::Cornering => Some(""),
            _ => None,
        }
    }

    /// What to look for once it is printed, and what to enter.
    pub fn instructions(self) -> &'static str {
        match self {
            Test::Temperature => {
                "Each block is printed 5 °C cooler than the one below; the temperatures are on the side. Pick the block with the best bridges, overhangs and surface, with no stringing, and enter its temperature."
            }
            Test::FlowPass1 | Test::FlowPass2 => {
                "Each block prints with the flow changed by the number on it. Pick the block whose top is smoothest (no gaps between lines, no ridges) and enter its number."
            }
            Test::FlowYolo | Test::FlowYoloFine => {
                "Each block adds the number on it to the flow ratio. Run a finger across the tops; pick the smoothest and enter its number."
            }
            Test::PaLine => {
                "Each line prints slow, fast, slow at the pressure advance written beside it (every other line is numbered). Pick the line most even in width through the speed changes and enter its value."
            }
            Test::PaPattern => {
                "Each corner pattern prints at the pressure advance above it. Pick the one with the sharpest corner and no gap or bulge before or after it, and enter its value."
            }
            Test::PaTower => {
                "Pressure advance rises with height. Measure the height where the corners look best and enter it; the value is worked out from it."
            }
            Test::Retraction => {
                "The retraction length grows with height (by the step every millimetre above the base). Measure the lowest height with no stringing between the towers and enter it."
            }
            Test::MaxFlow => {
                "The flow rises with height. Measure the height where the wall first goes bad (gaps, a dull or rough surface) and enter it; the volumetric speed is worked out from it."
            }
            Test::Vfa => {
                "The outer wall speed rises block by block (the speed is on each). Note the speeds with vertical fine artefacts and avoid them; nothing is saved."
            }
            Test::ShaperFreq => {
                "The input shaper frequency rises with height. Find the height with the least ringing on each side, work out the frequency for X and Y, and enter them."
            }
            Test::ShaperDamp => {
                "The damping ratio rises with height at the frequencies you set. Find the height with the least ringing and enter the damping ratio there."
            }
            Test::Cornering => {
                "Jerk (or junction deviation) rises with height. Find the highest point before the corners turn bad and enter the value there."
            }
        }
    }
}

/// The calibration dialog's state.
#[derive(Clone, Debug, PartialEq)]
pub struct Dialog {
    pub test: Test,
    pub start: f64,
    pub end: f64,
    pub step: f64,
    pub freq_x: (f64, f64),
    pub freq_y: (f64, f64),
    pub damp: f64,
    pub shaper: String,
    /// Input shaping and cornering: 0 the ringing tower, 1 the fast tower, 2 (cornering) SCV.
    pub model: usize,
    pub numbers: bool,
    /// Scale the tower to the nozzle (temperature tower, VFA).
    pub resize: bool,
    /// The test is set up; the dialog asks for the result.
    pub printed: bool,
    /// What the user read off the print: a value, or a height.
    pub result: f64,
    pub result_y: f64,
}

impl Dialog {
    /// The dialog with OrcaSlicer's defaults for `test`.
    pub fn new(test: Test, session: &Session) -> Dialog {
        let kind = session.presets.value(session.schema, TabKind::Filament, 0, "filament_type").to_ascii_uppercase();
        let klipper = printer_text(session, "gcode_flavor") == "klipper";
        let (start, end, step) = match test {
            Test::Temperature => match kind.as_str() {
                "ABS" | "ASA" => (270.0, 230.0, 5.0),
                "PETG" => (250.0, 230.0, 5.0),
                "PCTG" => (280.0, 240.0, 5.0),
                "TPU" => (240.0, 210.0, 5.0),
                "PA-CF" | "PET-CF" => (320.0, 280.0, 5.0),
                "PC" => (280.0, 240.0, 5.0),
                _ => (230.0, 190.0, 5.0),
            },
            Test::PaLine => (0.0, 0.1, 0.002),
            Test::PaPattern => (0.0, 0.08, 0.005),
            Test::PaTower => (0.0, 0.1, 0.002),
            Test::Retraction => (0.0, 2.0, 0.1),
            Test::MaxFlow => (5.0, 20.0, 0.5),
            Test::Vfa => (40.0, 200.0, 10.0),
            Test::ShaperDamp => (0.0, 0.4, 0.0),
            Test::Cornering => {
                if !klipper && junction_deviation(session) {
                    (0.0, 0.25, 0.0)
                } else {
                    (1.0, 15.0, 0.0)
                }
            }
            _ => (0.0, 0.0, 0.0),
        };
        Dialog {
            test,
            start,
            end,
            step,
            freq_x: if test == Test::ShaperDamp { (30.0, 30.0) } else { (15.0, 110.0) },
            freq_y: if test == Test::ShaperDamp { (30.0, 30.0) } else { (15.0, 110.0) },
            damp: 0.15,
            shaper: String::new(),
            model: 0,
            numbers: true,
            resize: true,
            printed: false,
            result: if test == Test::Temperature { start } else { 0.0 },
            result_y: 0.0,
        }
    }
}

fn printer_text(session: &Session, key: &str) -> String {
    session.presets.value(session.schema, TabKind::Printer, 0, key)
}

fn number(session: &Session, kind: TabKind, key: &str, default: f64) -> f64 {
    first_number(&session.presets.value(session.schema, kind, 0, key)).filter(|v| v.is_finite()).unwrap_or(default)
}

fn junction_deviation(session: &Session) -> bool {
    printer_text(session, "gcode_flavor") == "marlin2"
        && number(session, TabKind::Printer, "machine_max_junction_deviation", 0.0) > 0.0
}

fn nozzle(session: &Session) -> f64 {
    let n = number(session, TabKind::Printer, "nozzle_diameter", 0.4);
    if n > 0.05 && n < 3.0 { n } else { 0.4 }
}

// ---- the models ----

fn unpack(z: &[u8]) -> Option<Mesh> {
    let bytes = miniz_oxide::inflate::decompress_to_vec_zlib_with_limit(z, 64 << 20).ok()?;
    CosmicSlicer_model::stl::parse(&bytes).ok()
}

/// One of OrcaSlicer's calibration models (packed by `tools/pack_calib_models.py`).
pub fn model(name: &str) -> Option<Mesh> {
    let z: &[u8] = match name {
        "temperature_tower" => include_bytes!("../../../assets/calib/temperature_tower.stl.z"),
        "tower_with_seam" => include_bytes!("../../../assets/calib/tower_with_seam.stl.z"),
        "retraction_tower" => include_bytes!("../../../assets/calib/retraction_tower.stl.z"),
        "SpeedTestStructure" => include_bytes!("../../../assets/calib/SpeedTestStructure.stl.z"),
        "vfa" => include_bytes!("../../../assets/calib/vfa.stl.z"),
        "ringing_tower" => include_bytes!("../../../assets/calib/ringing_tower.stl.z"),
        "fast_tower_test" => include_bytes!("../../../assets/calib/fast_tower_test.stl.z"),
        "SCV-V2" => include_bytes!("../../../assets/calib/SCV-V2.stl.z"),
        _ => return None,
    };
    unpack(z)
}

fn flow_project(test: Test) -> &'static [u8] {
    match test {
        Test::FlowPass2 => include_bytes!("../../../assets/calib/flowrate-test-pass2.3mf"),
        Test::FlowYolo => include_bytes!("../../../assets/calib/Orca-LinearFlow.3mf"),
        Test::FlowYoloFine => include_bytes!("../../../assets/calib/Orca-LinearFlow_fine.3mf"),
        _ => include_bytes!("../../../assets/calib/flowrate-test-pass1.3mf"),
    }
}

/// `mesh` with only what lies below (`keep_below`) or above height `z` (from its base).
fn cut_at(mesh: &Mesh, z: f32, keep_below: bool) -> Mesh {
    let b = mesh.bounds();
    if !b.is_valid() || z <= 0.0 || z >= b.size().z {
        return mesh.clone();
    }
    let h = CosmicSlicer_model::cut::cut(mesh, Vec3::new(0.0, 0.0, b.min.z + z), Vec3::Z);
    if keep_below { h.below } else { h.above }
}

fn scaled(mut mesh: Mesh, s: Vec3) -> Mesh {
    for v in &mut mesh.vertices {
        *v *= s;
    }
    mesh
}

// ---- setting up ----

/// Replaces the project with the test `d` asks for. Errors say why it cannot be printed.
pub fn set_up(session: &mut Session, d: &Dialog) -> Result<(), String> {
    let flavor = printer_text(session, "gcode_flavor");
    let shaping = matches!(d.test, Test::ShaperFreq | Test::ShaperDamp);
    if shaping && !matches!(flavor.as_str(), "klipper" | "reprapfirmware" | "marlin2") {
        return Err("Input shaping can be set from G-code only on Klipper, RepRapFirmware and Marlin 2 (set the G-code flavor in the printer settings).".into());
    }
    if let Some(_unit) = d.test.range_unit()
        && !matches!(d.test, Test::ShaperFreq)
        && !(d.start.is_finite() && d.end.is_finite() && d.step.is_finite())
    {
        return Err("Enter numbers for the range.".into());
    }
    let needs_step =
        matches!(d.test, Test::PaLine | Test::PaPattern | Test::PaTower | Test::Retraction | Test::MaxFlow | Test::Vfa);
    if needs_step && (d.step <= 0.0 || d.end <= d.start) {
        return Err("The end must be above the start, and the step above zero.".into());
    }
    if needs_step && (d.end - d.start) / d.step > 500.0 {
        return Err("That range has too many steps; take a larger step.".into());
    }
    if d.test == Test::Temperature && (d.start > 350.0 || d.end < 150.0 || d.start <= d.end) {
        return Err("The tower runs from a hotter start down to a cooler end, between 350 and 150 °C.".into());
    }
    session.new_project();
    session.project.name = d.test.label().replace(':', "").to_owned();
    let schema = session.schema;
    let set = |s: &mut Session, kind: TabKind, key: &str, v: String| s.presets.set_value(schema, kind, 0, key, v);
    let n = nozzle(session);
    let mut calib = None;
    let mut overrides: Vec<(&str, String)> = Vec::new();
    let mut mesh = None;
    match d.test {
        Test::Temperature => {
            let tower = model("temperature_tower").ok_or("the tower model is missing")?;
            // Blocks of 10 mm, 5 °C apart, from 500 °C at the bottom.
            let below = cut_at(&tower, (((500.0 - d.end) / 5.0).round() + 1.0) as f32 * 10.0 - 1e-3, true);
            let mut m = cut_at(&below, ((500.0 - d.start) / 5.0).round() as f32 * 10.0 + 1e-3, false);
            if d.resize && (n - 0.4).abs() > 1e-3 {
                m = scaled(m, Vec3::splat((n / 0.4) as f32));
                overrides.push(("layer_height", format!("{}", n / 2.0)));
                set(session, TabKind::Process, "initial_layer_print_height", format!("{}", n / 2.0));
            }
            mesh = Some(m);
            let t = format!("{:.0}", d.start);
            set(session, TabKind::Filament, "nozzle_temperature", t.clone());
            set(session, TabKind::Filament, "nozzle_temperature_initial_layer", t);
            overrides.extend([
                ("brim_type", "outer_only".into()),
                ("brim_width", "5".into()),
                ("brim_object_gap", "0".into()),
                ("seam_slope_type", "none".into()),
            ]);
            calib = Some(Calib::new(Mode::TempTower, d.start, d.end, 5.0));
        }
        Test::PaLine | Test::PaPattern => {
            let count = ((d.end - d.start) / d.step + 1.0).max(1.0);
            let (_, bed) = session.bed();
            let size = if d.test == Test::PaLine {
                Vec3::new(110.0, (count as f32 * 3.5 + 8.0).min(bed.y - 10.0), 0.2)
            } else {
                Vec3::new((count as f32 * 4.5 + 40.0).min(bed.x - 10.0), 60.0, 0.2)
            };
            // A flat placeholder: where the test is drawn.
            mesh = Some(scaled(primitives::cube(1.0), size));
            let mut c =
                Calib::new(if d.test == Test::PaLine { Mode::PaLine } else { Mode::PaPattern }, d.start, d.end, d.step);
            c.numbers = d.numbers;
            calib = Some(c);
        }
        Test::PaTower => {
            let tower = model("tower_with_seam").ok_or("the tower model is missing")?;
            mesh = Some(cut_at(&tower, ((d.end - d.start) / d.step).ceil() as f32 + 1.0, true));
            set(session, TabKind::Filament, "slow_down_layer_time", "1".into());
            overrides.extend([
                ("seam_position", "back".into()),
                ("wall_loops", "2".into()),
                ("top_shell_layers", "0".into()),
                ("bottom_shell_layers", "0".into()),
                ("sparse_infill_density", "0%".into()),
                ("brim_type", "brim_ears".into()),
                ("brim_object_gap", "0".into()),
                ("brim_width", "6".into()),
                ("seam_slope_type", "none".into()),
            ]);
            calib = Some(Calib::new(Mode::PaTower, d.start, d.end, d.step));
        }
        Test::Retraction => {
            let tower = model("retraction_tower").ok_or("the tower model is missing")?;
            let lh = if n <= 0.1 {
                0.05
            } else if n <= 0.2 {
                0.1
            } else {
                0.2
            };
            mesh = Some(cut_at(&tower, (1.4 + (d.end - d.start) / d.step - 1e-3) as f32, true));
            raise_max_layer(session, lh);
            set(session, TabKind::Printer, "use_firmware_retraction", "0".into());
            set(session, TabKind::Process, "initial_layer_print_height", format!("{lh}"));
            overrides.extend([
                ("wall_loops", "2".into()),
                ("top_shell_layers", "0".into()),
                ("bottom_shell_layers", "3".into()),
                ("sparse_infill_density", "0%".into()),
                ("layer_height", format!("{lh}")),
                ("seam_position", "aligned".into()),
                ("wall_sequence", "inner wall/outer wall".into()),
                ("seam_slope_type", "none".into()),
            ]);
            calib = Some(Calib::new(Mode::Retraction, d.start, d.end, d.step));
        }
        Test::MaxFlow => {
            let mut m = model("SpeedTestStructure").ok_or("the test model is missing")?;
            let (_, bed) = session.bed();
            let wide = m.bounds().size().x;
            if wide > bed.x - 10.0 {
                m = scaled(m, Vec3::new((bed.x - 10.0) / wide, 1.0, 1.0));
            }
            let (lw, lh) = (n * 1.75, n * 0.8);
            m = cut_at(&m, ((d.end - d.start + 1.0) / d.step) as f32, true);
            mesh = Some(m);
            raise_max_layer(session, lh);
            let mvs = number(session, TabKind::Filament, "filament_max_volumetric_speed", 0.0).max(200.0);
            set(session, TabKind::Filament, "filament_max_volumetric_speed", format!("{mvs}"));
            set(session, TabKind::Filament, "slow_down_layer_time", "0".into());
            set(session, TabKind::Process, "spiral_mode", "1".into());
            overrides.extend([
                ("enable_overhang_speed", "0".into()),
                ("wall_loops", "1".into()),
                ("top_shell_layers", "0".into()),
                ("bottom_shell_layers", "0".into()),
                ("sparse_infill_density", "0%".into()),
                ("outer_wall_line_width", format!("{lw}")),
                ("layer_height", format!("{lh}")),
                ("brim_type", "outer_and_inner".into()),
                ("brim_width", "5".into()),
                ("brim_object_gap", "0".into()),
            ]);
            // mm³/s to mm/s for this line.
            let flow = number(session, TabKind::Filament, "filament_flow_ratio", 1.0).clamp(0.1, 3.0);
            let per_mm = (lw - lh * (1.0 - std::f64::consts::FRAC_PI_4)) * lh * flow;
            calib = Some(Calib::new(Mode::VolSpeed, d.start / per_mm, d.end / per_mm, d.step / per_mm));
        }
        Test::Vfa => {
            let tower = model("vfa").ok_or("the tower model is missing")?;
            let lh = n / 2.0;
            let mut m = cut_at(&tower, (5.0 * ((d.end - d.start) / d.step + 1.0) - 1e-3) as f32, true);
            if d.resize {
                let xy = (n / 0.4) as f32;
                let z = (25.0 * lh / 5.0) as f32;
                if (xy - 1.0).abs() > 1e-3 || (z - 1.0).abs() > 1e-3 {
                    m = scaled(m, Vec3::new(xy, xy, z));
                }
                set(session, TabKind::Process, "initial_layer_print_height", format!("{lh}"));
                overrides.push(("layer_height", format!("{lh}")));
            }
            mesh = Some(m);
            set(session, TabKind::Filament, "slow_down_layer_time", "0".into());
            for (k, v) in [
                ("enable_overhang_speed", "0"),
                ("wall_loops", "1"),
                ("top_shell_layers", "0"),
                ("bottom_shell_layers", "1"),
                ("sparse_infill_density", "0%"),
                ("detect_thin_wall", "0"),
                ("spiral_mode", "1"),
            ] {
                set(session, TabKind::Process, k, v.into());
            }
            overrides.extend([
                ("brim_type", "outer_only".into()),
                ("brim_width", "3".into()),
                ("brim_object_gap", "0".into()),
            ]);
            calib = Some(Calib::new(Mode::Vfa, d.start, d.end, d.step));
        }
        Test::ShaperFreq | Test::ShaperDamp | Test::Cornering => {
            let name = match d.model {
                1 => "fast_tower_test",
                2 if d.test == Test::Cornering => "SCV-V2",
                _ => "ringing_tower",
            };
            mesh = Some(model(name).ok_or("the tower model is missing")?);
            let speed = number(session, TabKind::Printer, "machine_max_speed_x", 200.0)
                .min(number(session, TabKind::Printer, "machine_max_speed_y", 200.0))
                .clamp(20.0, 2000.0);
            let accel =
                number(session, TabKind::Printer, "machine_max_acceleration_extruding", 5000.0).clamp(100.0, 100_000.0);
            for key in ["slow_down_layer_time", "slow_down_min_speed"] {
                set(session, TabKind::Filament, key, "0".into());
            }
            for (k, v) in [
                ("enable_overhang_speed", "0".to_owned()),
                ("wall_loops", "1".into()),
                ("top_shell_layers", "0".into()),
                ("bottom_shell_layers", "1".into()),
                ("sparse_infill_density", "0%".into()),
                ("detect_thin_wall", "0".into()),
                ("spiral_mode", "1".into()),
                ("bottom_surface_pattern", "zig-zag".into()),
                ("outer_wall_speed", format!("{speed}")),
                ("default_acceleration", format!("{accel}")),
                ("outer_wall_acceleration", format!("{accel}")),
            ] {
                set(session, TabKind::Process, k, v);
            }
            if d.test != Test::Cornering {
                set(session, TabKind::Process, "layer_height", "0.2".into());
            }
            overrides.extend([
                ("brim_type", "outer_only".into()),
                ("brim_width", "3".into()),
                ("brim_object_gap", "0".into()),
            ]);
            let mut c = match d.test {
                Test::ShaperFreq => Calib::new(Mode::ShaperFreq, d.damp, d.damp, 0.0),
                Test::ShaperDamp => Calib::new(Mode::ShaperDamp, d.start, d.end, 0.0),
                _ => {
                    if junction_deviation(session) {
                        set(session, TabKind::Printer, "machine_max_junction_deviation", format!("{}", d.end));
                        set(session, TabKind::Process, "default_junction_deviation", "0".into());
                    } else {
                        set(session, TabKind::Process, "default_jerk", "0".into());
                    }
                    let mvs = number(session, TabKind::Filament, "filament_max_volumetric_speed", 0.0).max(200.0);
                    set(session, TabKind::Filament, "filament_max_volumetric_speed", format!("{mvs}"));
                    Calib::new(Mode::Cornering, d.start, d.end, 0.0)
                }
            };
            c.freq_x = d.freq_x;
            c.freq_y = d.freq_y;
            c.shaper = d.shaper.clone();
            calib = Some(c);
        }
        t if t.flow() => return set_up_flow(session, d),
        _ => {}
    }
    let mesh = mesh.ok_or("no model for this test")?;
    let at = plate_centre(session);
    let id = session.model.add(d.test.label(), mesh, at, 0);
    if let Some(o) = session.model.get_mut(id) {
        o.overrides =
            overrides.into_iter().filter(|(k, _)| schema.option(k).is_some()).map(|(k, v)| (k.to_owned(), v)).collect();
    }
    session.selection = vec![id];
    session.calib = calib;
    session.dirty = true;
    Ok(())
}

fn plate_centre(session: &Session) -> Vec3 {
    let (lo, size) = session.bed();
    session.plate_origin(0) + (lo + size / 2.0).extend(0.0)
}

fn raise_max_layer(session: &mut Session, lh: f64) {
    if number(session, TabKind::Printer, "max_layer_height", 0.0) < lh {
        let schema = session.schema;
        session.presets.set_value(schema, TabKind::Printer, 0, "max_layer_height", format!("{lh}"));
    }
}

/// The flow tests: OrcaSlicer's blocks, each with its flow change (from its name).
fn set_up_flow(session: &mut Session, d: &Dialog) -> Result<(), String> {
    let project = CosmicSlicer_model::threemf::read(flow_project(d.test)).map_err(|e| e.to_string())?;
    let schema = session.schema;
    let n = nozzle(session);
    let first = number(session, TabKind::Process, "initial_layer_print_height", 0.2);
    let lh = n / 2.0;
    let first = first.max(lh);
    let z = ((first + 9.0 * lh) / 2.0) as f32;
    let xy = if n / 0.6 > 1.2 { (n / 0.6) as f32 } else { 1.0 };
    let linear = matches!(d.test, Test::FlowYolo | Test::FlowYoloFine);
    let current = number(session, TabKind::Filament, "filament_flow_ratio", 1.0).clamp(0.1, 3.0);
    let width = n * 1.2;
    let mut blocks: Vec<(String, Mesh)> = Vec::new();
    for o in &project.objects {
        let Some(part) = o.parts.first() else { continue };
        let Some(inst) = o.instances.first() else { continue };
        let mut mesh = part.mesh.clone();
        let m = Mat4::from_scale(Vec3::new(xy, xy, z)) * inst.matrix;
        for v in &mut mesh.vertices {
            *v = m.transform_point3(*v);
        }
        blocks.push((o.name.clone(), mesh));
    }
    if blocks.is_empty() {
        return Err("the flow test has no blocks".into());
    }
    let all = blocks.iter().fold(CosmicSlicer_model::Aabb::EMPTY, |b, (_, m)| b.union(&m.bounds()));
    let centre = plate_centre(session);
    let mut ids = Vec::new();
    for (name, mesh) in blocks {
        let b = mesh.bounds();
        let at = centre + Vec3::new(b.centre().x - all.centre().x, b.centre().y - all.centre().y, 0.0);
        let id = session.model.add(name.clone(), mesh, at, 0);
        // "flowrate_m5" is −5.
        let modifier = name
            .strip_prefix("flowrate_")
            .map(|s| s.replacen('m', "-", 1))
            .and_then(|s| s.parse::<f64>().ok())
            .unwrap_or(0.0);
        let ratio = if linear { (current + modifier) / current } else { 1.0 + modifier / 100.0 };
        let overrides: Vec<(&str, String)> = vec![
            ("wall_loops", "1".into()),
            ("only_one_wall_top", "1".into()),
            ("sparse_infill_density", "35%".into()),
            ("min_width_top_surface", "100%".into()),
            ("bottom_shell_layers", "2".into()),
            ("top_shell_layers", "5".into()),
            ("top_shell_thickness", "0".into()),
            ("bottom_shell_thickness", "0".into()),
            ("detect_thin_wall", "1".into()),
            ("sparse_infill_pattern", "zig-zag".into()),
            ("top_surface_line_width", format!("{width}")),
            ("internal_solid_infill_line_width", format!("{width}")),
            ("top_surface_pattern", "monotonic".into()),
            ("infill_direction", "45".into()),
            ("solid_infill_direction", "135".into()),
            ("ironing_type", "no ironing".into()),
            ("seam_slope_type", "none".into()),
            ("print_flow_ratio", format!("{ratio:.4}")),
        ];
        if let Some(o) = session.model.get_mut(id) {
            o.overrides = overrides
                .into_iter()
                .filter(|(k, _)| schema.option(k).is_some())
                .map(|(k, v)| (k.to_owned(), v))
                .collect();
        }
        ids.push(id);
    }
    session.presets.set_value(schema, TabKind::Process, 0, "layer_height", format!("{lh}"));
    session.presets.set_value(schema, TabKind::Process, 0, "initial_layer_print_height", format!("{first}"));
    session.selection = ids;
    session.calib = None;
    session.dirty = true;
    Ok(())
}

// ---- the result ----

/// A preset value to set: which preset, the option, its value.
pub type Setting = (TabKind, &'static str, String);

/// What entering `d.result` saves, as (preset kind, key, value) and a note; `None` when the
/// test has nothing to save.
pub fn result(session: &Session, d: &Dialog) -> Option<(Vec<Setting>, String)> {
    let r = d.result;
    if !r.is_finite() {
        return None;
    }
    let fmt = |v: f64, places: usize| {
        let s = format!("{v:.places$}");
        if s.contains('.') { s.trim_end_matches('0').trim_end_matches('.').to_owned() } else { s }
    };
    let current = number(session, TabKind::Filament, "filament_flow_ratio", 1.0);
    Some(match d.test {
        Test::Temperature => {
            let t = fmt(r.round(), 0);
            (
                vec![
                    (TabKind::Filament, "nozzle_temperature", t.clone()),
                    (TabKind::Filament, "nozzle_temperature_initial_layer", t.clone()),
                ],
                format!("Nozzle temperature {t} °C"),
            )
        }
        Test::FlowPass1 | Test::FlowPass2 => {
            let v = (current * (100.0 + r) / 100.0).clamp(0.1, 3.0);
            (vec![(TabKind::Filament, "filament_flow_ratio", fmt(v, 4))], format!("Flow ratio {}", fmt(v, 4)))
        }
        Test::FlowYolo | Test::FlowYoloFine => {
            let v = (current + r).clamp(0.1, 3.0);
            (vec![(TabKind::Filament, "filament_flow_ratio", fmt(v, 4))], format!("Flow ratio {}", fmt(v, 4)))
        }
        Test::PaLine | Test::PaPattern | Test::PaTower => {
            let k = if d.test == Test::PaTower { d.start + r.max(0.0) * d.step } else { r }.clamp(0.0, 2.0);
            (
                vec![
                    (TabKind::Filament, "pressure_advance", fmt(k, 4)),
                    (TabKind::Filament, "enable_pressure_advance", "1".into()),
                ],
                format!("Pressure advance {}", fmt(k, 4)),
            )
        }
        Test::Retraction => {
            let l = (d.start + (r - 0.4).max(0.0).floor() * d.step).clamp(0.0, 50.0);
            (vec![(TabKind::Printer, "retraction_length", fmt(l, 2))], format!("Retraction length {} mm", fmt(l, 2)))
        }
        Test::MaxFlow => {
            let v = (d.start + r.max(0.0) * d.step).clamp(0.5, 200.0);
            (
                vec![(TabKind::Filament, "filament_max_volumetric_speed", fmt(v, 1))],
                format!("Max volumetric speed {} mm³/s", fmt(v, 1)),
            )
        }
        Test::ShaperFreq => (
            vec![
                (TabKind::Printer, "input_shaping_freq_x", fmt(r, 1)),
                (TabKind::Printer, "input_shaping_freq_y", fmt(d.result_y, 1)),
            ],
            format!("Input shaper {} Hz (X), {} Hz (Y); also set them in the firmware", fmt(r, 1), fmt(d.result_y, 1)),
        ),
        Test::ShaperDamp => (
            vec![
                (TabKind::Printer, "input_shaping_damp_x", fmt(r, 3)),
                (TabKind::Printer, "input_shaping_damp_y", fmt(r, 3)),
            ],
            format!("Damping ratio {}; also set it in the firmware", fmt(r, 3)),
        ),
        Test::Cornering if junction_deviation(session) => (
            vec![(TabKind::Printer, "machine_max_junction_deviation", fmt(r, 3))],
            format!("Junction deviation {} mm", fmt(r, 3)),
        ),
        Test::Cornering => (vec![(TabKind::Process, "default_jerk", fmt(r, 2))], format!("Jerk {} mm/s", fmt(r, 2))),
        Test::Vfa => return None,
    })
}

/// Writes the result into the presets (as unsaved edits, to save as a preset); returns the note.
pub fn save(session: &mut Session, d: &Dialog) -> Option<String> {
    let (values, note) = result(session, d)?;
    let schema = session.schema;
    for (kind, key, value) in values {
        if schema.option(key).is_some() {
            session.presets.set_value(schema, kind, 0, key, value);
        }
    }
    Some(note)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_model_unpacks() {
        for name in [
            "temperature_tower",
            "tower_with_seam",
            "retraction_tower",
            "SpeedTestStructure",
            "vfa",
            "ringing_tower",
            "fast_tower_test",
            "SCV-V2",
        ] {
            let m = model(name);
            assert!(m.as_ref().is_some_and(|m| m.triangle_count() > 0), "{name}");
        }
        for t in [Test::FlowPass1, Test::FlowPass2, Test::FlowYolo, Test::FlowYoloFine] {
            let p = CosmicSlicer_model::threemf::read(flow_project(t));
            assert!(p.is_ok_and(|p| p.objects.len() >= 9), "{t:?}");
        }
    }

    #[test]
    fn every_test_sets_up_and_its_result_saves() {
        for t in Test::ALL {
            let mut s = Session::new();
            let schema = s.schema;
            s.presets.set_value(schema, TabKind::Printer, 0, "gcode_flavor", "klipper".into());
            let mut d = Dialog::new(t, &s);
            assert!(set_up(&mut s, &d).is_ok(), "{t:?}: {:?}", set_up(&mut s, &d));
            assert!(!s.model.objects.is_empty(), "{t:?}");
            for o in &s.model.objects {
                assert!(s.fits(o.id), "{t:?}: {:?}", o.bounds());
            }
            d.result = match t {
                Test::Temperature => 215.0,
                Test::FlowPass1 => 5.0,
                Test::PaTower => 10.0,
                Test::Retraction => 5.4,
                _ => 0.5,
            };
            let note = save(&mut s, &d);
            assert_eq!(note.is_some(), t != Test::Vfa, "{t:?}");
            if t == Test::Temperature {
                assert_eq!(s.presets.value(schema, TabKind::Filament, 0, "nozzle_temperature"), "215");
            }
            if t == Test::Retraction {
                assert_eq!(s.presets.value(schema, TabKind::Printer, 0, "retraction_length"), "0.5");
            }
        }
    }

    #[test]
    fn the_temperature_tower_is_cut_to_the_range() {
        let mut s = Session::new();
        let d = Dialog { start: 230.0, end: 190.0, ..Dialog::new(Test::Temperature, &s) };
        assert!(set_up(&mut s, &d).is_ok());
        // Nine blocks of 10 mm.
        let h = s.model.objects.first().map(|o| o.bounds().size().z);
        assert!(h.is_some_and(|h| (h - 90.0).abs() < 0.5), "{h:?}");
        assert!(s.calib.as_ref().is_some_and(|c| c.mode == Mode::TempTower));
        // A bad range is refused before anything changes.
        let bad = Dialog { start: 190.0, end: 230.0, ..d };
        assert!(set_up(&mut s, &bad).is_err());
    }

    #[test]
    fn flow_blocks_get_their_ratios() {
        let mut s = Session::new();
        let d = Dialog::new(Test::FlowPass1, &s);
        assert!(set_up(&mut s, &d).is_ok());
        let ratio = |name: &str| {
            s.model
                .objects
                .iter()
                .find(|o| o.name == name)
                .and_then(|o| o.overrides.iter().find(|(k, _)| k == "print_flow_ratio").map(|(_, v)| v.clone()))
        };
        assert_eq!(ratio("flowrate_m10").as_deref(), Some("0.9000"));
        assert_eq!(ratio("flowrate_15").as_deref(), Some("1.1500"));
    }

    #[test]
    fn every_test_slices_with_its_changes() {
        for t in Test::ALL {
            let mut s = Session::new();
            let schema = s.schema;
            s.presets.set_value(schema, TabKind::Printer, 0, "gcode_flavor", "klipper".into());
            let mut d = Dialog::new(t, &s);
            // Short towers keep the test quick.
            match t {
                Test::Temperature => (d.start, d.end) = (220.0, 210.0),
                Test::PaTower => d.end = 0.02,
                Test::Retraction => d.end = 0.5,
                Test::MaxFlow => d.end = 8.0,
                Test::Vfa => d.end = 60.0,
                _ => {}
            }
            assert!(set_up(&mut s, &d).is_ok(), "{t:?}");
            let job = crate::slicer::job(&s, 0);
            let out = CosmicSlicer_engine::slice(&job, &CosmicSlicer_engine::Progress::default());
            assert!(out.is_ok(), "{t:?}: {:?}", out.as_ref().err());
            let Ok(out) = out else { continue };
            let g = &out.gcode;
            let has = |needle: &str| g.lines().any(|l| l.contains(needle));
            let expected = match t {
                Test::Temperature => has("; temperature tower"),
                Test::PaLine | Test::PaPattern | Test::PaTower => has("SET_PRESSURE_ADVANCE"),
                Test::Retraction => has("Calib_Retraction_tower"),
                Test::ShaperFreq | Test::ShaperDamp => has("SET_INPUT_SHAPER"),
                Test::Cornering => has("SQUARE_CORNER_VELOCITY"),
                _ => out.stats.layers > 3,
            };
            assert!(expected, "{t:?}: the G-code lacks the test's changes");
        }
    }
}
