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
    /// The printer's layer height limits.
    pub min_layer_height: f64,
    pub max_layer_height: f64,
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
    /// Mouse ears at corners sharper than this, degrees.
    pub brim_ears_max_angle: f64,
    /// Outlines are simplified by this before looking for corners, mm.
    pub brim_ears_detection_length: f64,
    pub support: Support,
    pub raft: Raft,
    pub multi: MultiMaterial,
    pub gcode: GcodeSettings,
    pub motion: Motion,
    pub seam: String,
    pub outer_first: bool,
    pub spiral: bool,
    /// "Slice gap closing radius", mm.
    pub slice_closing: f64,
    /// How far slices are simplified, mm.
    pub slice_resolution: f64,
    /// How far simplified paths may stray, mm.
    pub resolution: f64,
    pub bridge_flow: f64,
    pub only_one_wall_top: bool,
    pub walls: Walls,
    pub bridges: Bridges,
    pub seams: Seams,
    /// Overhangs steeper than this (degrees from vertical) are filled out into cones.
    pub printable_overhang: Option<f64>,
    /// A Bambu Lab printer: OrcaSlicer writes those files' comments its own way.
    pub bbl: bool,
    /// A bed-slinger (OrcaSlicer's "i3" structure): timelapse pictures differ.
    pub i3: bool,
    /// Object names and label comments in the G-code.
    pub label_objects: bool,
    /// Outlines and holes grown (positive) or shrunk on every layer, mm.
    pub xy_contour: f64,
    pub xy_hole: f64,
    /// Elephant foot compensation: the first layer shrunk by this much, less on each layer
    /// up to `elephant_layers`.
    pub elephant_foot: f64,
    pub elephant_layers: usize,
    pub infill: InfillOpts,
    pub ironing: Ironing,
    pub fuzzy: Fuzzy,
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

/// A speed that may be a percentage of another; `Off` keeps the other.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum RelSpeed {
    Off,
    Mm(f64),
    Percent(f64),
}

impl RelSpeed {
    pub fn of(self, base: f64) -> f64 {
        match self {
            RelSpeed::Off => base,
            RelSpeed::Mm(v) => v,
            RelSpeed::Percent(p) => (base * p / 100.0).clamp(1.0, 2000.0),
        }
    }
}

/// One step of a rotation template ("0,30,60", "+5#5"): an angle offset (radians) or a turn
/// by it, held for `layers` layers.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TemplateStep {
    pub angle: f64,
    pub relative: bool,
    pub layers: usize,
}

/// Reads an OrcaSlicer rotation template: comma-separated degrees, `+`/`-` for a turn rather
/// than an angle, `#n` to hold a step for n layers. Unreadable steps are skipped.
pub fn parse_template(text: &str) -> Vec<TemplateStep> {
    text.split(',')
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .take(1000)
        .filter_map(|t| {
            let (value, layers) = match t.split_once('#') {
                Some((v, n)) => (v.trim(), n.trim().parse::<usize>().ok()?.clamp(1, 10_000)),
                None => (t, 1),
            };
            let relative = value.starts_with('+') || value.starts_with('-');
            let angle = value.parse::<f64>().ok().filter(|v| v.is_finite())?;
            Some(TemplateStep { angle: angle.to_radians(), relative, layers })
        })
        .collect()
}

/// The angle offset a template gives layer `layer` (0 with no template).
pub fn template_angle(steps: &[TemplateStep], layer: usize) -> f64 {
    let cycle: usize = steps.iter().map(|s| s.layers).sum();
    if cycle == 0 {
        return 0.0;
    }
    // Turns add up over every layer so far, angles set it outright.
    let mut angle = 0.0;
    let mut seen = 0usize;
    'outer: loop {
        for st in steps {
            if seen > layer {
                break 'outer;
            }
            angle = if st.relative { angle + st.angle } else { st.angle };
            seen += st.layers;
        }
        if seen > layer || seen == 0 {
            break;
        }
    }
    angle
}

/// How hard OrcaSlicer's "ensure vertical shell thickness" works.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VerticalShell {
    None,
    Critical,
    Moderate,
    All,
}

/// Infill and surface options beyond patterns and densities.
#[derive(Clone, Debug)]
pub struct InfillOpts {
    /// mm; 0 for no anchors.
    pub anchor: f64,
    /// mm: line ends closer than this along the edge join up.
    pub anchor_max: f64,
    /// Lateral lattice angles from vertical, radians.
    pub lattice: [f64; 2],
    pub shift_step: f64,
    pub skin: crate::infill::Skin,
    pub sparse_template: Vec<TemplateStep>,
    pub solid_template: Vec<TemplateStep>,
    /// Sparse regions smaller than this (mm²) are printed solid.
    pub min_sparse_area: f64,
    pub vertical_shell: VerticalShell,
    pub top_density: f64,
    pub bottom_density: f64,
    pub top_flow: f64,
    pub bottom_flow: f64,
    pub solid_flow: f64,
    pub sparse_flow: f64,
    /// Sparse infill printed every few layers at up to this height, mm.
    pub combination: Option<f64>,
    pub extra_overhang_perimeters: bool,
    /// Lightning branches lean in by this much a layer, radians.
    pub lightning_prune: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IroningKind {
    None,
    /// Every top surface.
    Top,
    /// Only the object's topmost layer.
    Topmost,
    /// Every solid surface.
    Solid,
}

#[derive(Clone, Copy, Debug)]
pub struct Ironing {
    pub kind: IroningKind,
    pub concentric: bool,
    pub spacing: f64,
    pub inset: f64,
    /// Radians: from the top surface's direction, or outright when `fixed`.
    pub angle: f64,
    pub fixed: bool,
    /// Of a full line at the ironing spacing.
    pub flow: f64,
    pub speed: f64,
}

/// Which walls get fuzzy skin.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FuzzyWhere {
    None,
    /// Outer walls of the outside only.
    External,
    /// Outer walls of holes only.
    Holes,
    /// Every outer wall.
    All,
    /// Every wall.
    AllWalls,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Noise {
    Classic,
    Perlin,
    Billow,
    Ridged,
    Voronoi,
    Ripple,
}

#[derive(Clone, Copy, Debug)]
pub struct Fuzzy {
    pub on: FuzzyWhere,
    /// mm either side.
    pub thickness: f64,
    pub point_distance: f64,
    pub first_layer: bool,
    pub noise: Noise,
    /// Feature size of coherent noise, mm.
    pub scale: f64,
    pub octaves: usize,
    pub persistence: f64,
    pub ripples_per_layer: usize,
    /// Fraction of a ripple each layer moves along.
    pub ripple_offset: f64,
    pub ripple_layers: usize,
}

/// Where gap fill goes in solid infill (OrcaSlicer's `gap_fill_target`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GapFill {
    Everywhere,
    TopBottom,
    Nowhere,
}

/// Wall generation (OrcaSlicer's Quality → Walls and Overhangs).
#[derive(Clone, Copy, Debug)]
pub struct Walls {
    /// Arachne: thin parts and the gaps between walls get variable-width lines.
    pub arachne: bool,
    /// Parts too thin for a wall get one line down the middle.
    pub thin_walls: bool,
    /// mm.
    pub min_bead: f64,
    pub min_feature: f64,
    pub gap_fill: GapFill,
    /// Narrow internal solid infill is printed concentric.
    pub narrow_solid: bool,
    /// Gap fill lines shorter than this are left out, mm.
    pub filter_gap: f64,
    pub gap_flow: f64,
    pub detect_overhang: bool,
    pub overhang_speed: bool,
    /// For overhangs of 10–25 %, 25–50 %, 50–75 % and 75–100 % of the line width.
    pub overhang_speeds: [RelSpeed; 4],
    pub overhang_flow: f64,
    /// Outer loops counter-clockwise (holes the other way).
    pub ccw: bool,
    pub precise_outer: bool,
    pub one_wall_first_layer: bool,
    /// "Only one wall on top" leaves out top surfaces narrower than this, mm.
    pub min_top_width: f64,
}

#[derive(Clone, Copy, Debug)]
pub struct Bridges {
    /// Line width, mm; 0 takes the solid infill's.
    pub width: f64,
    /// Degrees; `None` finds the best direction.
    pub angle: Option<f64>,
    pub internal_angle: Option<f64>,
    pub density: f64,
    pub internal_density: f64,
    pub thick: bool,
    pub thick_internal: bool,
    pub internal_speed: f64,
    pub internal_flow: f64,
}

/// Which loops get a scarf seam.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scarf {
    None,
    /// Outer contours only.
    External,
    /// Every outer wall loop, holes too.
    All,
}

#[derive(Clone, Copy, Debug)]
pub struct Seams {
    /// mm the loop stops short of its start.
    pub gap: f64,
    pub staggered_inner: bool,
    pub scarf: Scarf,
    /// Only where the seam is on a smooth stretch (corner angle over `scarf_angle`).
    pub scarf_conditional: bool,
    pub scarf_angle: f64,
    /// mm above the layer below where the scarf starts.
    pub scarf_start_height: f64,
    pub scarf_entire_loop: bool,
    pub scarf_length: f64,
    pub scarf_steps: usize,
    pub scarf_inner: bool,
    pub scarf_speed: RelSpeed,
    pub scarf_flow: f64,
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
    /// A helix on the spot.
    Spiral,
    /// OrcaSlicer's "Auto": a slope, a helix at layer changes.
    Auto,
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
    /// Degrees, for sloped lifts.
    pub travel_slope: f64,
    /// Wipes at the speed of what was just printed, else at `wipe_speed`.
    pub role_based_wipe: bool,
    /// Of the travel speed.
    pub wipe_speed: RelSpeed,
    /// After an outer loop, a small move inwards hides the seam.
    pub wipe_on_loops: bool,
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
    pub z_hop_type: ZHop,
    pub lift_above: f64,
    pub lift_below: f64,
    pub min_travel: f64,
    pub on_layer_change: bool,
    pub wipe: bool,
    pub wipe_distance: f64,
    /// 0..1 of the length, taken back standing before the wipe.
    pub before_wipe: f64,
    pub restart_extra: f64,
    pub toolchange_length: f64,
}

impl Retract {
    /// These settings with a filament's own (OrcaSlicer's `filament_extruder_override_keys`).
    pub fn with(mut self, o: &RetractOverride) -> Retract {
        let RetractOverride {
            length,
            speed,
            deretract_speed,
            z_hop,
            z_hop_type,
            lift_above,
            lift_below,
            min_travel,
            on_layer_change,
            wipe,
            wipe_distance,
            before_wipe,
            restart_extra,
        } = *o;
        self.length = length.unwrap_or(self.length);
        self.speed = speed.unwrap_or(self.speed);
        self.deretract_speed = deretract_speed.unwrap_or(self.deretract_speed);
        self.z_hop = z_hop.unwrap_or(self.z_hop);
        self.z_hop_type = z_hop_type.unwrap_or(self.z_hop_type);
        self.lift_above = lift_above.unwrap_or(self.lift_above);
        self.lift_below = lift_below.unwrap_or(self.lift_below);
        self.min_travel = min_travel.unwrap_or(self.min_travel);
        self.on_layer_change = on_layer_change.unwrap_or(self.on_layer_change);
        self.wipe = wipe.unwrap_or(self.wipe);
        self.wipe_distance = wipe_distance.unwrap_or(self.wipe_distance);
        self.before_wipe = before_wipe.unwrap_or(self.before_wipe);
        self.restart_extra = restart_extra.unwrap_or(self.restart_extra);
        self
    }
}

/// What a filament overrides of the printer's retraction: `nil` leaves the printer's value.
#[derive(Clone, Copy, Debug, Default)]
pub struct RetractOverride {
    pub length: Option<f64>,
    pub speed: Option<f64>,
    pub deretract_speed: Option<f64>,
    pub z_hop: Option<f64>,
    pub z_hop_type: Option<ZHop>,
    pub lift_above: Option<f64>,
    pub lift_below: Option<f64>,
    pub min_travel: Option<f64>,
    pub on_layer_change: Option<bool>,
    pub wipe: Option<bool>,
    pub wipe_distance: Option<f64>,
    pub before_wipe: Option<f64>,
    pub restart_extra: Option<f64>,
}

impl RetractOverride {
    fn read(r: &Reader<'_>) -> RetractOverride {
        // The first variant's value; missing or `nil` for none.
        let text = |key: &str| Some(r.first(key)).filter(|v| !v.is_empty() && v != "nil");
        let num = |key: &str, lo: f64, hi: f64| {
            let v = text(key)?;
            let v: f64 = v.trim_end_matches('%').trim().parse().ok()?;
            v.is_finite().then(|| v.clamp(lo, hi))
        };
        let flag = |key: &str| text(key).map(|v| is_true(&v));
        RetractOverride {
            length: num("filament_retraction_length", 0.0, 50.0),
            speed: num("filament_retraction_speed", 1.0, 1000.0),
            deretract_speed: num("filament_deretraction_speed", 1.0, 1000.0),
            z_hop: num("filament_z_hop", 0.0, 10.0),
            z_hop_type: text("filament_z_hop_types").map(|v| ZHop::of(&v)),
            lift_above: num("filament_retract_lift_above", 0.0, 10_000.0),
            lift_below: num("filament_retract_lift_below", 0.0, 10_000.0),
            min_travel: num("filament_retraction_minimum_travel", 0.0, 1000.0),
            on_layer_change: flag("filament_retract_when_changing_layer"),
            wipe: flag("filament_wipe"),
            wipe_distance: num("filament_wipe_distance", 0.0, 50.0),
            before_wipe: num("filament_retract_before_wipe", 0.0, 100.0).map(|v| v / 100.0),
            restart_extra: num("filament_retract_restart_extra", -10.0, 10.0),
        }
    }
}

impl ZHop {
    /// From `z_hop_types`.
    pub fn of(text: &str) -> ZHop {
        match text {
            "Normal Lift" => ZHop::Normal,
            "Slope Lift" => ZHop::Slope,
            "Spiral Lift" => ZHop::Spiral,
            _ => ZHop::Auto,
        }
    }
}

/// How support areas are shaped (OrcaSlicer's `support_style`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SupportStyle {
    /// Columns merged into blocks: sturdy, easy to print.
    Grid,
    /// Columns that follow the overhang's outline.
    Snug,
    /// Branches, smooth and thin (OrcaSlicer's organic trees).
    Organic,
    TreeSlim,
    TreeStrong,
    /// Trees under small overhangs, columns under large flat ones.
    TreeHybrid,
}

impl SupportStyle {
    pub fn is_tree(self) -> bool {
        !matches!(self, SupportStyle::Grid | SupportStyle::Snug)
    }
}

#[derive(Clone, Debug)]
pub struct Tree {
    /// Most a branch leans from vertical, radians.
    pub branch_angle: f64,
    /// Tips under an overhang sit this far apart, mm.
    pub branch_distance: f64,
    pub tip_diameter: f64,
    pub branch_diameter: f64,
    /// How fast branches thicken going down, radians.
    pub diameter_angle: f64,
    /// Loops round each branch.
    pub walls: usize,
    pub with_infill: bool,
    /// Brim round the trunks on the plate, mm (0 for none).
    pub brim: f64,
}

#[derive(Clone, Debug)]
pub struct Support {
    pub enabled: bool,
    /// Generated wherever overhangs need it; off for "manual", where only enforcers count.
    pub auto: bool,
    pub style: SupportStyle,
    /// Overhangs steeper than this from vertical need support (degrees from horizontal as
    /// OrcaSlicer counts: 30 means faces within 30° of horizontal); 0 uses `threshold_overlap`.
    pub threshold_angle: f64,
    /// With no angle: a line needs support when less than this share of it rests on the
    /// layer below (0..1).
    pub threshold_overlap: f64,
    pub critical_only: bool,
    pub remove_small: bool,
    /// The first this many layers get support under any overhang, even with support off.
    pub enforce_layers: usize,
    /// Base pattern: "rectilinear", "rectilinear-grid", "honeycomb", "lightning" or "hollow".
    pub base_pattern: String,
    pub spacing: f64,
    /// Base lines' direction, radians.
    pub angle: f64,
    pub expansion: f64,
    pub top_z_distance: f64,
    pub bottom_z_distance: f64,
    pub xy_distance: f64,
    /// Gap round the part on the first layer only.
    pub first_layer_gap: f64,
    pub interface_layers: usize,
    pub bottom_interface_layers: usize,
    pub interface_spacing: f64,
    pub bottom_interface_spacing: f64,
    /// "rectilinear", "concentric", "spiralinset", "rectilinear_interlaced" or "grid".
    pub interface_pattern: String,
    pub build_plate_only: bool,
    pub flow: f64,
    pub interface_flow: f64,
    pub tree: Tree,
}

#[derive(Clone, Debug)]
pub struct Raft {
    pub layers: usize,
    /// Gap between the raft's top and the part, mm.
    pub contact_distance: f64,
    pub expansion: f64,
    pub first_layer_expansion: f64,
    /// 0..1.
    pub first_layer_density: f64,
}

#[derive(Clone, Debug)]
pub struct GcodeSettings {
    pub flavor: String,
    pub start: String,
    pub end: String,
    pub before_layer: String,
    pub timelapse: String,
    pub layer_change: String,
    /// Bambu Lab's auxiliary part fan (M106 P2).
    pub auxiliary_fan: bool,
    pub relative_e: bool,
    pub printer_model: String,
    /// Run at each filament change (OrcaSlicer's `change_filament_gcode`).
    pub change_filament: String,
    /// A pause from the layer slider.
    pub pause: String,
    /// Custom G-code from the layer slider uses its own text; this is the printer's template.
    pub template_custom: String,
}

/// Several filaments: the prime tower, flushing, standby temperatures, print order.
#[derive(Clone, Debug)]
pub struct MultiMaterial {
    pub tower: bool,
    /// The tower's front-left corner, printer coordinates.
    pub tower_at: [f64; 2],
    pub tower_width: f64,
    pub tower_brim: f64,
    /// Least purged at a change, mm³.
    pub prime_volume: f64,
    /// Flushing volumes from filament i to j at `i * n + j`, mm³ (empty: from the colours).
    pub flush: Vec<f64>,
    pub flush_multiplier: f64,
    /// Turn idle hotends down by `standby_delta` (multi-hotend printers).
    pub ooze_prevention: bool,
    pub standby_delta: f64,
    /// Support and its interface print with these filaments (1-based; 0 keeps the current).
    pub support_filament: usize,
    pub support_interface_filament: usize,
    /// How deep painted colours reach in from the surface, mm (0: all the way).
    pub segment_width: f64,
    /// Print each object to the top before the next.
    pub by_object: bool,
    pub clearance_radius: f64,
    pub clearance_height: f64,
    /// The printer's nozzles (`nozzle_diameter`'s length); two change how timelapse pictures go.
    pub nozzles: usize,
}

/// One filament's properties.
#[derive(Clone, Debug)]
pub struct Filament {
    pub retract: RetractOverride,
    /// Custom G-code for when this filament starts and finishes.
    pub start_gcode: String,
    pub end_gcode: String,
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
    /// Walls overhanging by this degree (0 = every outer wall, 1–5 for 10 %…95 %) or more get
    /// the overhang fan; `None` when the filament turns it off.
    pub overhang_fan_from: Option<u8>,
    /// Internal bridges' fan; `None` keeps the overhang fan.
    pub internal_bridge_fan: Option<f64>,
    /// Whether short layers are slowed down to `slow_down_layer_time`.
    pub slow_down: bool,
    pub slow_down_layer_time: f64,
    pub slow_down_min_speed: f64,
    /// Up to this layer time the fan runs between its maximum and minimum speed.
    pub fan_cooling_layer_time: f64,
    /// The fan keeps its minimum speed on long layers (else it stops).
    pub keep_fan_on: bool,
    /// Layer cooling leaves outer walls at their speed.
    pub keep_outer_wall_speed: bool,
    /// Support interfaces' and ironing's own fan speeds.
    pub support_interface_fan: Option<f64>,
    pub ironing_fan: Option<f64>,
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

    /// A speed in mm/s or a percentage of another; 0 or unreadable is `Off`.
    pub fn rel_speed(&self, key: &str) -> RelSpeed {
        let raw = self.first(key);
        let (v, pct) = match raw.strip_suffix('%') {
            Some(p) => (p.trim().parse::<f64>().ok(), true),
            None => (raw.parse::<f64>().ok(), false),
        };
        match v {
            Some(v) if v.is_finite() && v > 0.0 && pct => RelSpeed::Percent(v.min(1000.0)),
            Some(v) if v.is_finite() && v > 0.0 => RelSpeed::Mm(v.clamp(1.0, 2000.0)),
            _ => RelSpeed::Off,
        }
    }

    /// An angle in degrees where 0 means automatic.
    pub fn angle_or_auto(&self, key: &str) -> Option<f64> {
        let v = self.num(key, 0.0);
        (v.is_finite() && v.abs() > 1e-6).then(|| v.rem_euclid(360.0))
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
            // 0: as fast as travel (OrcaSlicer's).
            travel_z: r.range("travel_speed_z", 0.0, 0.0, 2000.0),
        };
        let accel = |key: &str, fallback: f64| {
            let v = r.num(key, fallback);
            if v.is_finite() && v > 0.0 { v.clamp(10.0, 100_000.0) } else { fallback }
        };
        let default_accel = accel("default_acceleration", 500.0);
        let support_type = r.text("support_type");
        let sparse_w = widths.sparse;
        // Percentages are of the infill's line spacing.
        let sparse_spacing = Settings::spacing(sparse_w, r.range("layer_height", 0.2, 0.01, 10.0));
        let (anchor, anchor_max) = match (
            r.mm_or_percent("infill_anchor", sparse_spacing),
            r.mm_or_percent("infill_anchor_max", sparse_spacing),
        ) {
            (Some(a), Some(m)) => (a.min(m).min(1000.0), m.min(1000.0)),
            _ => (0.0, 0.0),
        };
        let infill = InfillOpts {
            anchor,
            anchor_max,
            lattice: [
                r.range("lateral_lattice_angle_1", -45.0, -80.0, 80.0).to_radians(),
                r.range("lateral_lattice_angle_2", 45.0, -80.0, 80.0).to_radians(),
            ],
            shift_step: r.range("infill_shift_step", 0.4, 0.0, 100.0),
            skin: crate::infill::Skin {
                depth: r.range("skin_infill_depth", 2.0, 0.0, 100.0),
                density: r.range("skin_infill_density", 25.0, 1.0, 100.0) / 100.0,
                skeleton: r.range("skeleton_infill_density", 25.0, 1.0, 100.0) / 100.0,
            },
            sparse_template: parse_template(&r.text("sparse_infill_rotate_template")),
            solid_template: parse_template(&r.text("solid_infill_rotate_template")),
            min_sparse_area: r.range("minimum_sparse_infill_area", 15.0, 0.0, 10_000.0),
            vertical_shell: match r.text("ensure_vertical_shell_thickness").as_str() {
                "none" => VerticalShell::None,
                "ensure_critical_only" => VerticalShell::Critical,
                "ensure_moderate" => VerticalShell::Moderate,
                _ => VerticalShell::All,
            },
            top_density: r.range("top_surface_density", 100.0, 10.0, 100.0) / 100.0,
            bottom_density: r.range("bottom_surface_density", 100.0, 10.0, 100.0) / 100.0,
            top_flow: r.range("top_solid_infill_flow_ratio", 1.0, 0.1, 2.0),
            bottom_flow: r.range("bottom_solid_infill_flow_ratio", 1.0, 0.1, 2.0),
            solid_flow: r.range("internal_solid_infill_flow_ratio", 1.0, 0.1, 2.0),
            sparse_flow: r.range("sparse_infill_flow_ratio", 1.0, 0.1, 2.0),
            combination: r
                .flag("infill_combination")
                .then(|| r.mm_or_percent("infill_combination_max_layer_height", nozzle).unwrap_or(nozzle))
                .map(|h| h.min(nozzle)),
            extra_overhang_perimeters: r.flag("extra_perimeters_on_overhangs"),
            lightning_prune: r.range("lightning_prune_angle", 45.0, 0.0, 85.0).to_radians(),
        };
        let ironing = Ironing {
            kind: match r.text("ironing_type").as_str() {
                "top" => IroningKind::Top,
                "topmost" => IroningKind::Topmost,
                "solid" => IroningKind::Solid,
                _ => IroningKind::None,
            },
            concentric: r.text("ironing_pattern") == "concentric",
            spacing: r.range("ironing_spacing", 0.1, 0.01, 2.0),
            inset: r.range("ironing_inset", 0.0, 0.0, 10.0),
            angle: r.num("ironing_angle", 0.0).to_radians(),
            fixed: r.flag("ironing_angle_fixed"),
            flow: r.range("ironing_flow", 10.0, 0.0, 100.0) / 100.0,
            speed: r.speed("ironing_speed", 20.0),
        };
        let fuzzy = Fuzzy {
            on: match r.text("fuzzy_skin").as_str() {
                "external" => FuzzyWhere::External,
                "hole" => FuzzyWhere::Holes,
                "all" => FuzzyWhere::All,
                "allwalls" => FuzzyWhere::AllWalls,
                _ => FuzzyWhere::None,
            },
            thickness: r.range("fuzzy_skin_thickness", 0.2, 0.0, 5.0),
            point_distance: r.range("fuzzy_skin_point_distance", 0.3, 0.05, 50.0),
            first_layer: r.flag("fuzzy_skin_first_layer"),
            noise: match r.text("fuzzy_skin_noise_type").as_str() {
                "perlin" => Noise::Perlin,
                "billow" => Noise::Billow,
                "ridgedmulti" => Noise::Ridged,
                "voronoi" => Noise::Voronoi,
                "ripple" => Noise::Ripple,
                _ => Noise::Classic,
            },
            scale: r.range("fuzzy_skin_scale", 1.0, 0.01, 1000.0),
            octaves: r.count("fuzzy_skin_octaves", 4, 10).max(1),
            persistence: r.range("fuzzy_skin_persistence", 0.5, 0.01, 1.0),
            ripples_per_layer: r.count("fuzzy_skin_ripples_per_layer", 15, 10_000).max(1),
            ripple_offset: r.range("fuzzy_skin_ripple_offset", 50.0, 0.0, 100.0) / 100.0,
            ripple_layers: r.count("fuzzy_skin_layers_between_ripple_offset", 1, 10_000).max(1),
        };
        let bridge_speed = speeds.bridge;
        let walls = Walls {
            arachne: r.text("wall_generator") != "classic",
            thin_walls: r.flag("detect_thin_wall") || r.text("wall_generator") != "classic",
            min_bead: r
                .mm_or_percent("min_bead_width", nozzle)
                .unwrap_or(nozzle * 0.85)
                .clamp(nozzle * 0.1, nozzle * 2.0),
            min_feature: r
                .mm_or_percent("min_feature_size", nozzle)
                .unwrap_or(nozzle * 0.25)
                .clamp(nozzle * 0.05, nozzle * 2.0),
            gap_fill: match r.text("gap_fill_target").as_str() {
                "everywhere" => GapFill::Everywhere,
                "topbottom" => GapFill::TopBottom,
                _ => GapFill::Nowhere,
            },
            narrow_solid: r.flag("detect_narrow_internal_solid_infill"),
            filter_gap: r.range("filter_out_gap_fill", 0.0, 0.0, 100.0),
            gap_flow: r.range("gap_fill_flow_ratio", 1.0, 0.1, 2.0),
            detect_overhang: r.flag("detect_overhang_wall"),
            overhang_speed: r.flag("enable_overhang_speed"),
            overhang_speeds: [
                r.rel_speed("overhang_1_4_speed"),
                r.rel_speed("overhang_2_4_speed"),
                r.rel_speed("overhang_3_4_speed"),
                r.rel_speed("overhang_4_4_speed"),
            ],
            overhang_flow: r.range("overhang_flow_ratio", 1.0, 0.1, 2.0),
            ccw: r.text("wall_direction") != "cw",
            precise_outer: r.flag("precise_outer_wall"),
            one_wall_first_layer: r.flag("only_one_wall_first_layer"),
            min_top_width: r.mm_or_percent("min_width_top_surface", widths.inner).unwrap_or(0.0),
        };
        let bridges = Bridges {
            width: r.mm_or_percent("bridge_line_width", nozzle).unwrap_or(0.0).clamp(0.0, 10.0),
            angle: r.angle_or_auto("bridge_angle"),
            internal_angle: r.angle_or_auto("internal_bridge_angle"),
            density: r.range("bridge_density", 100.0, 10.0, 100.0) / 100.0,
            internal_density: r.range("internal_bridge_density", 100.0, 10.0, 100.0) / 100.0,
            thick: r.flag("thick_bridges"),
            thick_internal: r.flag("thick_internal_bridges"),
            internal_speed: r.rel_speed("internal_bridge_speed").of(bridge_speed),
            internal_flow: r.range("internal_bridge_flow", 1.0, 0.1, 2.0),
        };
        let seams = Seams {
            gap: r.mm_or_percent("seam_gap", nozzle).unwrap_or(0.0).min(nozzle * 5.0),
            staggered_inner: r.flag("staggered_inner_seams"),
            scarf: match r.text("seam_slope_type").as_str() {
                "external" => Scarf::External,
                "all" => Scarf::All,
                _ => Scarf::None,
            },
            scarf_conditional: r.flag("seam_slope_conditional"),
            scarf_angle: r.range("scarf_angle_threshold", 155.0, 0.0, 180.0),
            scarf_start_height: r
                .mm_or_percent("seam_slope_start_height", layer_height)
                .unwrap_or(0.0)
                .min(layer_height),
            scarf_entire_loop: r.flag("seam_slope_entire_loop"),
            scarf_length: r.range("seam_slope_min_length", 20.0, 0.0, 1000.0),
            scarf_steps: r.count("seam_slope_steps", 10, 100).max(1),
            scarf_inner: r.flag("seam_slope_inner_walls"),
            scarf_speed: r.rel_speed("scarf_joint_speed"),
            scarf_flow: r.range("scarf_joint_flow_ratio", 1.0, 0.1, 2.0),
        };
        Settings {
            nozzle,
            layer_height,
            first_layer_height,
            min_layer_height: r.range("min_layer_height", 0.07, 0.01, nozzle),
            max_layer_height: {
                let v = r.num("max_layer_height", 0.0);
                if v.is_finite() && v > 0.01 { v.min(nozzle) } else { nozzle * 0.75 }
            },
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
                    travel_slope: r.range("travel_slope", 3.0, 0.5, 90.0),
                    role_based_wipe: r.flag("role_based_wipe_speed"),
                    wipe_speed: r.rel_speed("wipe_speed"),
                    wipe_on_loops: r.flag("wipe_on_loops"),
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
                z_hop_type: ZHop::of(&r.first("z_hop_types")),
                lift_above: r.range("retract_lift_above", 0.0, 0.0, 10_000.0),
                lift_below: r.range("retract_lift_below", 0.0, 0.0, 10_000.0),
                min_travel: r.range("retraction_minimum_travel", 2.0, 0.0, 1000.0),
                on_layer_change: r.flag("retract_when_changing_layer"),
                wipe: r.flag("wipe"),
                wipe_distance: r.range("wipe_distance", 1.0, 0.0, 50.0),
                before_wipe: r.range("retract_before_wipe", 100.0, 0.0, 100.0) / 100.0,
                restart_extra: r.range("retract_restart_extra", 0.0, -10.0, 10.0),
                toolchange_length: r.range("retract_length_toolchange", 10.0, 0.0, 200.0),
            },
            skirt_loops: r.count("skirt_loops", 1, 50),
            skirt_distance: r.range("skirt_distance", 2.0, 0.0, 100.0),
            skirt_height: r.count("skirt_height", 1, 10_000),
            brim_type: r.text("brim_type"),
            brim_width: r.range("brim_width", 0.0, 0.0, 100.0),
            brim_gap: r.range("brim_object_gap", 0.0, 0.0, 10.0),
            brim_ears_max_angle: r.range("brim_ears_max_angle", 125.0, 0.0, 180.0),
            brim_ears_detection_length: r.range("brim_ears_detection_length", 1.0, 0.0, 100.0),
            support: {
                let style = match (support_type.starts_with("tree"), r.text("support_style").as_str()) {
                    (false, "snug") => SupportStyle::Snug,
                    (false, _) => SupportStyle::Grid,
                    (true, "tree_slim") => SupportStyle::TreeSlim,
                    (true, "tree_strong") => SupportStyle::TreeStrong,
                    (true, "tree_hybrid") => SupportStyle::TreeHybrid,
                    (true, _) => SupportStyle::Organic,
                };
                let organic = style == SupportStyle::Organic;
                let pick = |plain: &str, org: &str| if organic { org.to_owned() } else { plain.to_owned() };
                let top_layers = r.count("support_interface_top_layers", 3, 20);
                let bottom = r.num("support_interface_bottom_layers", 0.0);
                Support {
                    enabled: r.flag("enable_support"),
                    auto: !support_type.ends_with("(manual)"),
                    style,
                    threshold_angle: r.range("support_threshold_angle", 30.0, 0.0, 90.0),
                    threshold_overlap: r.mm_or_percent("support_threshold_overlap", 1.0).unwrap_or(0.5).min(1.0),
                    critical_only: r.flag("support_critical_regions_only"),
                    remove_small: r.flag("support_remove_small_overhang"),
                    enforce_layers: r.count("enforce_support_layers", 0, 5000),
                    base_pattern: match r.text("support_base_pattern").as_str() {
                        p @ ("rectilinear" | "rectilinear-grid" | "honeycomb" | "lightning" | "hollow") => p.to_owned(),
                        _ if style.is_tree() => "hollow".into(),
                        _ => "rectilinear".into(),
                    },
                    spacing: r.range("support_base_pattern_spacing", 2.5, nozzle, 50.0),
                    angle: r.range("support_angle", 0.0, 0.0, 359.0).to_radians(),
                    expansion: r.range("support_expansion", 0.0, -10.0, 20.0),
                    top_z_distance: r.range("support_top_z_distance", 0.2, 0.0, 5.0),
                    bottom_z_distance: r.range("support_bottom_z_distance", 0.2, 0.0, 5.0),
                    xy_distance: r.range("support_object_xy_distance", 0.35, 0.0, 10.0),
                    first_layer_gap: r.range("support_object_first_layer_gap", 0.2, 0.0, 10.0),
                    interface_layers: top_layers,
                    bottom_interface_layers: if bottom < 0.0 { top_layers } else { (bottom.round() as usize).min(20) },
                    interface_spacing: r.range("support_interface_spacing", 0.5, 0.0, 10.0),
                    bottom_interface_spacing: r.range("support_bottom_interface_spacing", 0.5, 0.0, 10.0),
                    interface_pattern: match r.text("support_interface_pattern").as_str() {
                        p @ ("concentric" | "spiralinset" | "rectilinear_interlaced" | "grid") => p.to_owned(),
                        _ => "rectilinear".into(),
                    },
                    build_plate_only: r.flag("support_on_build_plate_only"),
                    flow: r.range("support_flow_ratio", 1.0, 0.1, 2.0),
                    interface_flow: r.range("support_interface_flow_ratio", 1.0, 0.1, 2.0),
                    tree: Tree {
                        branch_angle: r
                            .range(
                                &pick("tree_support_branch_angle", "tree_support_branch_angle_organic"),
                                40.0,
                                0.0,
                                60.0,
                            )
                            .to_radians(),
                        branch_distance: r.range(
                            &pick("tree_support_branch_distance", "tree_support_branch_distance_organic"),
                            5.0,
                            1.0,
                            10.0,
                        ),
                        tip_diameter: r.range("tree_support_tip_diameter", 0.8, nozzle, 10.0),
                        branch_diameter: r.range(
                            &pick("tree_support_branch_diameter", "tree_support_branch_diameter_organic"),
                            5.0,
                            1.0,
                            10.0,
                        ),
                        diameter_angle: r.range("tree_support_branch_diameter_angle", 5.0, 0.0, 15.0).to_radians(),
                        walls: match (r.count("tree_support_wall_count", 0, 2), style) {
                            (0, SupportStyle::TreeStrong) => 2,
                            (0, _) => 1,
                            (n, _) => n,
                        },
                        with_infill: r.flag("tree_support_with_infill"),
                        brim: if r.flag("tree_support_auto_brim") {
                            3.0
                        } else {
                            r.range("tree_support_brim_width", 3.0, 0.0, 20.0)
                        },
                    },
                }
            },
            raft: Raft {
                layers: r.count("raft_layers", 0, 100),
                contact_distance: r.range("raft_contact_distance", 0.1, 0.0, 5.0),
                expansion: r.range("raft_expansion", 1.5, 0.0, 50.0),
                first_layer_expansion: r.range("raft_first_layer_expansion", 2.0, 0.0, 50.0),
                first_layer_density: r.range("raft_first_layer_density", 90.0, 10.0, 100.0) / 100.0,
            },
            gcode: GcodeSettings {
                flavor: r.text("gcode_flavor"),
                start: r.text("machine_start_gcode"),
                end: r.text("machine_end_gcode"),
                auxiliary_fan: r.flag("auxiliary_fan"),
                before_layer: r.text("before_layer_change_gcode"),
                timelapse: r.text("time_lapse_gcode"),
                layer_change: r.text("layer_change_gcode"),
                relative_e: r.flag("use_relative_e_distances"),
                printer_model: r.text("printer_model"),
                change_filament: r.text("change_filament_gcode"),
                pause: r.text("machine_pause_gcode"),
                template_custom: r.text("template_custom_gcode"),
            },
            multi: MultiMaterial {
                tower: r.flag("enable_prime_tower"),
                tower_at: [
                    r.range("wipe_tower_x", 15.0, -1000.0, 10_000.0),
                    r.range("wipe_tower_y", 220.0, -1000.0, 10_000.0),
                ],
                tower_width: r.range("prime_tower_width", 60.0, 5.0, 300.0),
                tower_brim: r.range("prime_tower_brim_width", 3.0, 0.0, 50.0),
                prime_volume: r.range("prime_volume", 45.0, 0.0, 2000.0),
                flush: r
                    .text("flush_volumes_matrix")
                    .split(',')
                    .filter_map(|v| v.trim().parse::<f64>().ok())
                    .map(|v| if v.is_finite() { v.clamp(0.0, 5000.0) } else { 0.0 })
                    .collect(),
                flush_multiplier: r.range("flush_multiplier", 1.0, 0.0, 10.0),
                ooze_prevention: r.flag("ooze_prevention"),
                standby_delta: r.range("standby_temperature_delta", -5.0, -300.0, 300.0),
                support_filament: r.count("support_filament", 0, 64),
                support_interface_filament: r.count("support_interface_filament", 0, 64),
                segment_width: r.range("mmu_segmented_region_max_width", 0.0, 0.0, 1000.0),
                by_object: r.text("print_sequence") == "by object",
                clearance_radius: r.range("extruder_clearance_radius", 40.0, 0.0, 1000.0),
                clearance_height: r.range("extruder_clearance_height_to_rod", 40.0, 0.0, 1000.0),
                nozzles: r.text("nozzle_diameter").split(',').count().clamp(1, 64),
            },
            seam: r.text("seam_position"),
            outer_first: r.text("wall_sequence").starts_with("outer"),
            spiral: r.flag("spiral_mode"),
            slice_closing: r.range("slice_closing_radius", 0.049, 0.0, 2.0),
            slice_resolution: if r.range("resolution", 0.012, 0.0, 1.0) <= 0.001 { 0.0 } else { 0.0025 },
            resolution: r.range("resolution", 0.012, 0.0, 1.0),
            bridge_flow: r.range("bridge_flow", 1.0, 0.1, 2.0),
            only_one_wall_top: r.flag("only_one_wall_top"),
            walls,
            bridges,
            seams,
            bbl: r.text("printer_model").starts_with("Bambu"),
            i3: r.text("printer_structure") == "i3",
            label_objects: r.flag("gcode_label_objects"),
            xy_contour: r.range("xy_contour_compensation", 0.0, -10.0, 10.0),
            xy_hole: r.range("xy_hole_compensation", 0.0, -10.0, 10.0),
            elephant_foot: r.range("elefant_foot_compensation", 0.0, 0.0, 5.0),
            elephant_layers: r.count("elefant_foot_compensation_layers", 1, 1000),
            printable_overhang: r
                .flag("make_overhang_printable")
                .then(|| r.range("make_overhang_printable_angle", 55.0, 0.0, 90.0))
                .filter(|a| *a < 89.9),
            infill,
            ironing,
            fuzzy,
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
            retract: RetractOverride::read(&r),
            start_gcode: r.text("filament_start_gcode"),
            end_gcode: r.text("filament_end_gcode"),
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
            overhang_fan_from: r.flag("enable_overhang_bridge_fan").then(|| {
                match r.first("overhang_fan_threshold").trim_end_matches('%') {
                    "0" => 0,
                    "10" => 1,
                    "25" => 2,
                    "50" => 3,
                    "75" => 4,
                    _ => 5,
                }
            }),
            internal_bridge_fan: {
                let v = r.num("internal_bridge_fan_speed", -1.0);
                (v >= 0.0).then(|| v.min(100.0))
            },
            slow_down: r.flag("slow_down_for_layer_cooling"),
            slow_down_layer_time: r.range("slow_down_layer_time", 5.0, 0.0, 1000.0),
            fan_cooling_layer_time: r.range("fan_cooling_layer_time", 60.0, 0.0, 10_000.0),
            keep_fan_on: r.flag("reduce_fan_stop_start_freq"),
            keep_outer_wall_speed: r.flag("dont_slow_down_outer_wall"),
            support_interface_fan: {
                let v = r.num("support_material_interface_fan_speed", -1.0);
                (v >= 0.0).then(|| v.min(100.0))
            },
            ironing_fan: {
                let v = r.num("ironing_fan_speed", -1.0);
                (v >= 0.0).then(|| v.min(100.0))
            },
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
        // Single precision, as OrcaSlicer's `Flow` holds it: its last digit shows in E.
        let (w, h) = (f64::from(w as f32), f64::from(h as f32));
        f64::from((h * (w - h * (1.0 - 0.25 * std::f64::consts::PI))) as f32)
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
    fn rotation_templates_read_like_orca() {
        let t = parse_template("0, 30,60");
        let deg = |l: usize| template_angle(&t, l).to_degrees().round();
        assert_eq!([deg(0), deg(1), deg(2), deg(3)], [0.0, 30.0, 60.0, 0.0]);
        let turns = parse_template("+5#2");
        let deg = |l: usize| template_angle(&turns, l).to_degrees().round();
        assert_eq!([deg(0), deg(1), deg(2), deg(3), deg(4)], [5.0, 5.0, 10.0, 10.0, 15.0]);
        assert!(parse_template("x, ,#").is_empty());
        assert_eq!(template_angle(&[], 7), 0.0);
    }

    #[test]
    fn bead_area_matches_orca() {
        // 0.45 × 0.2: (0.45 − 0.2)·0.2 + π·0.1² ≈ 0.0814.
        assert!((bead_area(0.45, 0.2) - 0.081_415_9).abs() < 1e-6);
    }
}
