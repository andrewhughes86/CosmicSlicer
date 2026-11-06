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
    /// Walls: how much of each segment's line (from point `i` to the next) hangs past the
    /// layer below, in percent of its width (0 to 255). Empty when none does.
    pub overhang: Vec<u8>,
    /// Paths sharing a non-zero group print one after another in the order given (monotonic
    /// infill, overhang perimeters), not nearest first.
    pub group: u32,
    /// A fill line that prints the way it was drawn, whichever end is nearer (`can_reverse`).
    pub one_way: bool,
    /// Which segments lie on a fitted arc: neighbours with the same `id` print as one arc. An
    /// arc keeps its points, so seams and wipes see them. Empty when nothing was fitted.
    pub bends: Vec<Option<Bend>>,
    /// [`simplify`] has been through: the writer prints the points as they stand.
    pub fitted: bool,
}

/// The arc a segment lies on.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Bend {
    pub center: DVec2,
    /// In Orca's nanometres.
    pub radius: f64,
    pub ccw: bool,
    pub id: u32,
}

impl Bend {
    /// `Circle::get_closest_point`, on Orca's integers.
    fn closest(&self, p: DVec2) -> DVec2 {
        const NM: f64 = 1.0e6;
        let center = (self.center * NM).round();
        let v = ((p * NM).round() - center).normalize_or_zero();
        (center + (v * self.radius).trunc()) / NM
    }
}

/// The overhang degree (OrcaSlicer's: 10, 25, 50, 75 and 95 % of the line hanging start
/// degrees 1 to 5) of a share in percent.
pub fn degree_of(percent: u8) -> u8 {
    [10, 25, 50, 75, 95].iter().filter(|t| percent >= **t).count() as u8
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
        Path {
            role,
            points,
            closed,
            width,
            speed,
            flow,
            overhang: Vec::new(),
            group: 0,
            one_way: false,
            bends: Vec::new(),
            fitted: false,
        }
    }

    /// A new group id, unique in this run.
    pub fn new_group() -> u32 {
        static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(1);
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed).max(1)
    }

    /// The overhang degree of segment `i`: 0 (not) to 5 (95 % or more of the line).
    pub fn overhang_at(&self, i: usize) -> u8 {
        degree_of(self.hang_at(i))
    }

    /// How much of segment `i`'s line hangs, in percent of its width.
    pub fn hang_at(&self, i: usize) -> u8 {
        self.overhang.get(i).copied().unwrap_or(0)
    }

    /// Segment `i`'s arc, if it was fitted one.
    pub fn bend_at(&self, i: usize) -> Option<Bend> {
        self.bends.get(i).copied().flatten()
    }

    /// The arc both sides of point `i` lie on, if they do: splitting there clips it.
    fn arc_through(&self, i: usize) -> Option<Bend> {
        let n = self.points.len();
        let (before, after) = (self.bend_at((i + n - 1) % n)?, self.bend_at(i % n)?);
        (before.id == after.id).then_some(after)
    }

    /// Starts a loop at point `i`, as `ExtrusionLoop::split_at_vertex`: an arc through it is
    /// clipped into two, the point moving onto the circle.
    pub fn start_at_vertex(&mut self, i: usize) {
        if let Some(bend) = self.arc_through(i)
            && let Some(p) = self.points.get_mut(i)
        {
            *p = bend.closest(*p);
        }
        self.rotate(i);
    }

    /// Starts a loop at `p`, a new point on segment `i`, as `Polyline::split_at`: the segment
    /// becomes two straight lines and an arc it was part of ends each side.
    pub fn start_on_segment(&mut self, i: usize, p: DVec2) {
        let n = self.points.len();
        let next = (i + 1) % n;
        for end in [i, next] {
            if let Some(bend) = self.arc_through(end)
                && let Some(q) = self.points.get_mut(end)
            {
                *q = bend.closest(*q);
            }
        }
        self.points.insert(i + 1, p);
        if self.overhang.len() == n {
            self.overhang.insert(i + 1, self.overhang.get(i).copied().unwrap_or(0));
        }
        if self.bends.len() == n {
            if let Some(bend) = self.bends.get_mut(i) {
                *bend = None;
            }
            self.bends.insert(i + 1, None);
        }
        self.rotate(i + 1);
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
        if self.bends.len() == n {
            self.bends.rotate_left(i % n);
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
        // The same for arcs, which also turn the other way. An open path has one segment less.
        let count = if self.closed { n } else { n.saturating_sub(1) };
        if self.bends.len() == count && n >= 2 {
            let old = std::mem::take(&mut self.bends);
            self.bends = (0..count)
                .map(|j| {
                    let from = if j + 1 < n { n - 2 - j } else { n - 1 };
                    old.get(from).copied().flatten().map(|b| Bend { ccw: !b.ccw, ..b })
                })
                .collect();
        } else {
            self.bends.clear();
        }
    }

    pub fn start(&self) -> Option<DVec2> {
        self.points.first().copied()
    }

    pub fn end(&self) -> Option<DVec2> {
        if self.closed { self.points.first().copied() } else { self.points.last().copied() }
    }
}

/// OrcaSlicer's `SPARSE_INFILL_RESOLUTION`.
const SPARSE_RESOLUTION: f64 = 0.04;

/// `LayerRegion::simplify_path`, done once a path is finished and before seams or printing
/// order touch it: arcs fitted along the path as it is stored and what stays straight
/// simplified, each stretch that hangs alike on its own. The dropped points are gone for good,
/// so seams only see what is left. Without arcs, a plain simplification.
pub fn simplify(path: &mut Path, resolution: f64, arcs: bool) {
    let n = path.points.len();
    if n < 2 || path.fitted {
        return;
    }
    let tolerance = if path.role == Role::SparseInfill && arcs { SPARSE_RESOLUTION } else { resolution };
    let segments = if path.closed { n } else { n - 1 };
    let at = |i: usize| path.points.get(i % n).copied().unwrap_or_default();
    let mut points = vec![at(0)];
    let mut bends = Vec::with_capacity(segments);
    let mut marks = Vec::with_capacity(segments);
    let mut id = 0;
    let mut from = 0;
    while from < segments {
        let degree = path.overhang_at(from);
        let mut to = from + 1;
        while to < segments && path.overhang_at(to) == degree {
            to += 1;
        }
        let run: Vec<DVec2> = (from..=to).map(at).collect();
        let mark = path.hang_at(from);
        let fitted = if arcs {
            crate::arcs::fit_runs(&run, tolerance)
        } else {
            vec![crate::arcs::Run { points: crate::geom::simplify(&run, tolerance), arc: None }]
        };
        for stretch in fitted {
            id += 1;
            let bend = stretch.arc.map(|a| Bend { center: a.center, radius: a.radius, ccw: a.ccw, id });
            for &end in stretch.points.iter().skip(1) {
                points.push(end);
                bends.push(bend);
                marks.push(mark);
            }
        }
        from = to;
    }
    if path.closed {
        points.pop();
    }
    // A loop needs its points; one reduced to nothing keeps what it had.
    if points.len() < if path.closed { 3 } else { 2 } {
        return;
    }
    path.points = points;
    path.overhang = if path.overhang.is_empty() { Vec::new() } else { marks };
    path.bends = if bends.iter().any(Option::is_some) { bends } else { Vec::new() };
    path.fitted = true;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reversing_keeps_overhang_marks_on_their_segments() {
        let mut p = Path::new(Role::OuterWall, vec![DVec2::ZERO, DVec2::X, DVec2::ONE, DVec2::Y], true, 0.4, 1.0, 1.0);
        // Only the segment from X to ONE overhangs.
        p.overhang = vec![0, 60, 0, 0];
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
    /// Pressure advance to set before printing it (the calibration tests).
    pub pressure_advance: Option<f64>,
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
