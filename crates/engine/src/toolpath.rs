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
    InternalBridge,
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
    pub const COUNT: usize = 18;

    pub const ALL: [Role; Role::COUNT] = [
        Role::InnerWall,
        Role::OuterWall,
        Role::OverhangWall,
        Role::SparseInfill,
        Role::SolidInfill,
        Role::TopSurface,
        Role::BottomSurface,
        Role::Bridge,
        Role::InternalBridge,
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
            Role::InternalBridge => "Internal Bridge",
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

    pub fn is_bridge(self) -> bool {
        matches!(self, Role::Bridge | Role::InternalBridge)
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
    /// Walls: how far each segment (from point `i` to the next) overhangs the layer below,
    /// 0 (not) to 5 (95 % or more of the line width). Empty when none does.
    pub overhang: Vec<u8>,
    /// Paths sharing a non-zero group print one after another in the order given (monotonic
    /// infill, overhang perimeters), not nearest first.
    pub group: u32,
}

impl Path {
    pub fn length(&self) -> f64 {
        let open: f64 = self.points.windows(2).map(|w| w[0].distance(w[1])).sum();
        match (self.closed, self.points.first(), self.points.last()) {
            (true, Some(a), Some(b)) => open + a.distance(*b),
            _ => open,
        }
    }

    /// A path with no overhang marks.
    pub fn new(role: Role, points: Vec<DVec2>, closed: bool, width: f64, speed: f64, flow: f64) -> Path {
        Path { role, points, closed, width, speed, flow, overhang: Vec::new(), group: 0 }
    }

    /// A new group id, unique in this run.
    pub fn new_group() -> u32 {
        static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(1);
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed).max(1)
    }

    /// The overhang degree of segment `i`.
    pub fn overhang_at(&self, i: usize) -> u8 {
        self.overhang.get(i).copied().unwrap_or(0)
    }

    /// Starts a loop at point `i`.
    pub fn rotate(&mut self, i: usize) {
        let n = self.points.len();
        if n == 0 {
            return;
        }
        self.points.rotate_left(i % n);
        if self.overhang.len() == n {
            self.overhang.rotate_left(i % n);
        }
    }

    /// Runs the other way round, keeping each segment's overhang mark.
    pub fn reverse(&mut self) {
        let n = self.points.len();
        self.points.reverse();
        if self.overhang.len() == n && n >= 2 {
            // Segment j now runs from old point n-1-j to n-2-j: old segment n-2-j; a loop's
            // closing segment stays last.
            let old = std::mem::take(&mut self.overhang);
            self.overhang = (0..n)
                .map(|j| {
                    if j + 1 < n {
                        old.get(n - 2 - j).copied().unwrap_or(0)
                    } else {
                        old.get(n - 1).copied().unwrap_or(0)
                    }
                })
                .collect();
        } else {
            self.overhang.clear();
        }
    }

    pub fn start(&self) -> Option<DVec2> {
        self.points.first().copied()
    }

    pub fn end(&self) -> Option<DVec2> {
        if self.closed { self.points.first().copied() } else { self.points.last().copied() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reversing_keeps_overhang_marks_on_their_segments() {
        let mut p = Path::new(Role::OuterWall, vec![DVec2::ZERO, DVec2::X, DVec2::ONE, DVec2::Y], true, 0.4, 1.0, 1.0);
        // Only the segment from X to ONE overhangs.
        p.overhang = vec![0, 3, 0, 0];
        p.reverse();
        let i = p.points.iter().position(|q| *q == DVec2::ONE).unwrap_or(9);
        assert_eq!(p.points.get(i + 1), Some(&DVec2::X));
        assert_eq!(p.overhang_at(i), 3);
        assert_eq!(p.overhang.iter().filter(|d| **d > 0).count(), 1);
        p.rotate(i);
        assert_eq!(p.overhang_at(0), 3);
    }
}

/// One connected part of one object on one layer: its walls (outermost first) and fill.
#[derive(Clone, Debug, Default)]
pub struct Island {
    /// The island's outline, for routing travel inside it.
    pub outline: crate::geom::Shape,
    /// Walls by depth: `walls[0]` are the outer walls.
    pub walls: Vec<Vec<Path>>,
    pub fills: Vec<Path>,
    /// Painted seams on this layer: where seams should go, and where they must not.
    pub seam_enforce: Vec<crate::geom::Shape>,
    pub seam_block: Vec<crate::geom::Shape>,
    /// The filament (0-based) when not the object's: a painted colour region.
    pub extruder: Option<usize>,
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
    /// Printing one object at a time: this layer starts the next object, so the nozzle lifts
    /// clear of what is printed before it comes down.
    pub new_object: bool,
}
