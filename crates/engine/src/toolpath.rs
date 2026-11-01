//! What gets printed: extrusion paths by role, grouped by layer and island.

use glam::DVec2;

/// What an extrusion is for: OrcaSlicer's "line types".
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Role {
    InnerWall,
    OuterWall,
    OverhangWall,
    SparseInfill,
    SolidInfill,
    TopSurface,
    BottomSurface,
    Bridge,
    GapFill,
    Ironing,
    Skirt,
    Brim,
    Support,
    SupportInterface,
    SupportTransition,
    PrimeTower,
    Custom,
}

impl Role {
    pub const ALL: [Role; 17] = [
        Role::InnerWall,
        Role::OuterWall,
        Role::OverhangWall,
        Role::SparseInfill,
        Role::SolidInfill,
        Role::TopSurface,
        Role::BottomSurface,
        Role::Bridge,
        Role::GapFill,
        Role::Ironing,
        Role::Skirt,
        Role::Brim,
        Role::Support,
        Role::SupportInterface,
        Role::SupportTransition,
        Role::PrimeTower,
        Role::Custom,
    ];

    /// The name in OrcaSlicer's legend and in `;TYPE:` comments.
    pub fn name(self) -> &'static str {
        match self {
            Role::InnerWall => "Inner wall",
            Role::OuterWall => "Outer wall",
            Role::OverhangWall => "Overhang wall",
            Role::SparseInfill => "Sparse infill",
            Role::SolidInfill => "Internal solid infill",
            Role::TopSurface => "Top surface",
            Role::BottomSurface => "Bottom surface",
            Role::Bridge => "Bridge",
            Role::GapFill => "Gap infill",
            Role::Ironing => "Ironing",
            Role::Skirt => "Skirt",
            Role::Brim => "Brim",
            Role::Support => "Support",
            Role::SupportInterface => "Support interface",
            Role::SupportTransition => "Support transition",
            Role::PrimeTower => "Prime tower",
            Role::Custom => "Custom",
        }
    }

    pub fn index(self) -> usize {
        Role::ALL.iter().position(|r| *r == self).unwrap_or(0)
    }

    pub fn from_index(i: usize) -> Role {
        Role::ALL.get(i).copied().unwrap_or(Role::Custom)
    }

    /// Walls and surfaces whose look matters slow the print down for cooling last.
    pub fn is_wall(self) -> bool {
        matches!(self, Role::InnerWall | Role::OuterWall | Role::OverhangWall)
    }
}

/// One extrusion: a line or a loop at one width and speed.
#[derive(Clone, Debug, PartialEq)]
pub struct Path {
    pub role: Role,
    pub points: Vec<DVec2>,
    /// A loop: the last point joins the first.
    pub closed: bool,
    pub width: f64,
    /// mm/s before cooling slow-downs and the volumetric limit.
    pub speed: f64,
    /// Extrusion multiplier on top of the filament's flow ratio (bridges).
    pub flow: f64,
}

impl Path {
    pub fn length(&self) -> f64 {
        let open: f64 = self.points.windows(2).map(|w| w[0].distance(w[1])).sum();
        match (self.closed, self.points.first(), self.points.last()) {
            (true, Some(a), Some(b)) => open + a.distance(*b),
            _ => open,
        }
    }

    pub fn start(&self) -> Option<DVec2> {
        self.points.first().copied()
    }

    pub fn end(&self) -> Option<DVec2> {
        if self.closed { self.points.first().copied() } else { self.points.last().copied() }
    }
}

/// One connected part of one object on one layer: its walls (outermost first) and fill.
#[derive(Clone, Debug, Default)]
pub struct Island {
    /// Walls by depth: `walls[0]` are the outer walls.
    pub walls: Vec<Vec<Path>>,
    pub fills: Vec<Path>,
}

/// Everything printed on one layer.
#[derive(Clone, Debug, Default)]
pub struct LayerPlan {
    pub print_z: f64,
    pub height: f64,
    /// Skirt and brim, printed first.
    pub skirt: Vec<Path>,
    pub support: Vec<Path>,
    /// Per object (index into the job's objects): its islands.
    pub objects: Vec<(usize, Vec<Island>)>,
}
