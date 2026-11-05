//! OrcaSlicer's `TimelapsePosPicker`: where a Bambu Lab printer with two nozzles parks for the
//! timelapse picture, clear of the object and of the camera's view of it.
//!
//! Printing by layer only; printing by object Orca also weighs the rod and each object's height,
//! and here it gets no safe position, which its G-code templates handle.

use glam::DVec2;

use crate::geom::{self, Join, Op, Shape};

/// `candidate_point_segment`: 5 mm in Orca's units.
const SEGMENT: f64 = 5.0e6;
/// `MAX_CANDIDATE_SIZE`.
const MAX_CANDIDATES: usize = 5;
/// `FILTER_THRESHOLD`, mm.
const FILTER: f64 = 5.0;
const CAMERA_WEIGHT: f64 = 1.0 / 3.0;

/// One object on the plate: its footprint and height, mm.
#[derive(Clone, Copy, Debug)]
pub struct Instance {
    pub min: DVec2,
    pub max: DVec2,
    pub height: f64,
}

/// What the printer offers: per nozzle (in Orca's logical order) where it can reach.
#[derive(Clone, Debug, Default)]
pub struct Picker {
    printable: Vec<Vec<Shape>>,
    clearance: f64,
    objects: Vec<Instance>,
    /// The nozzle that lifts out of the way, and by how much: pictures with the other one reach
    /// lower than the head that just printed.
    liftable: Option<(usize, f64)>,
}

fn rectangle(min: DVec2, max: DVec2) -> Vec<DVec2> {
    vec![min, DVec2::new(max.x, min.y), max, DVec2::new(min.x, max.y)]
}

/// `Point(double, double)`: to the nearest whole number, ties to even.
fn whole(v: f64) -> i64 {
    v.round_ties_even() as i64
}

impl Picker {
    /// `construct_printable_area_by_printer`; no wipe tower, which only sits on plates of
    /// several filaments (then Orca's area also leaves out the tower).
    pub fn new(
        bed: Vec<DVec2>,
        exclude: &[DVec2],
        extruders: &[Vec<DVec2>],
        heights: &[f64],
        clearance: f64,
        objects: Vec<Instance>,
    ) -> Picker {
        let whole_bed: Vec<Shape> = vec![vec![bed.clone()]];
        let free = if exclude.len() >= 3 {
            geom::boolean(&whole_bed, &[vec![exclude.to_vec()]], Op::Difference)
        } else {
            whole_bed
        };
        let printable = extruders.iter().map(|a| geom::boolean(&free, &[vec![a.clone()]], Op::Intersect)).collect();
        let liftable = match heights {
            [a, b, ..] => Some((usize::from(a >= b), (a - b).abs())),
            _ => None,
        };
        Picker { printable, clearance, objects, liftable }
    }

    /// The whole-millimetre position for a picture taken by `extruder` at `print_z` with
    /// `current` the nozzle now printing; (0, 0) for none safe (`DefaultTimelapsePos`).
    pub fn pick(&self, print_z: f64, extruder: usize, current: usize) -> (i64, i64) {
        let radius = self.clearance / 2.0;
        let gap = match self.liftable {
            Some((lifts, gap)) if current != extruder && extruder != lifts => -gap as f32,
            _ => 0.0,
        };
        let (z, low) = (print_z as f32, print_z as f32 + gap.min(0.0));
        if low <= 0.0 {
            return (0, 0);
        }
        // `range_intersect` compares in whole millimetres.
        let (low, high) = (low as i64, z as i64);
        let slices: Vec<Shape> = self
            .objects
            .iter()
            .filter(|o| low <= o.height as i64 && high >= 0)
            .map(|o| vec![rectangle(o.min - radius, o.max + radius)])
            .collect();
        let camera_reach = (self.clearance as i64 / 2) as f32 as f64 * std::f64::consts::SQRT_2;
        let camera: Vec<Shape> = self
            .objects
            .iter()
            .map(|o| {
                let (lo, hi) = (o.min - camera_reach, o.max + camera_reach);
                vec![vec![
                    DVec2::ZERO,
                    DVec2::new(hi.x, lo.y).max(DVec2::ZERO),
                    hi.max(DVec2::ZERO),
                    DVec2::new(lo.x, hi.y).max(DVec2::ZERO),
                ]]
            })
            .collect();
        let blocked = geom::boolean(&geom::union(&slices), &geom::union(&camera), Op::Union);
        let Some(reach) = self.printable.get(extruder) else { return (0, 0) };
        let safe = geom::boolean(reach, &blocked, Op::Difference);
        let safe = geom::offset2(&safe, -FILTER, FILTER, Join::Miter);
        let (mm_x, mm_y) = self.centre();
        let centre = (((mm_x / 1e-6) as i64), ((mm_y / 1e-6) as i64));
        let at = pick_internal(centre, &safe);
        (whole(at.0 as f64 * 1e-6), whole(at.1 as f64 * 1e-6))
    }

    /// `get_objects_center`, from footprints in whole millimetres; mm.
    fn centre(&self) -> (f64, f64) {
        if self.objects.is_empty() {
            return (0.0, 0.0);
        }
        let mut sum = (0.0, 0.0);
        for o in &self.objects {
            sum.0 += f64::from((whole(o.min.x) + whole(o.max.x)) as f32 / 2.0);
            sum.1 += f64::from((whole(o.min.y) + whole(o.max.y)) as f32 / 2.0);
        }
        let n = self.objects.len() as f64;
        (sum.0 / n, sum.1 / n)
    }
}

/// `pick_pos_internal`, in Orca's units: `curr` when already safe, else the best point along the
/// safe areas' edges, weighing the way there against a clear view from the camera in the corner.
fn pick_internal(curr: (i64, i64), safe: &[Shape]) -> (i64, i64) {
    if geom::contains(safe, DVec2::new(curr.0 as f64 * 1e-6, curr.1 as f64 * 1e-6)) {
        return curr;
    }
    let penalty = |c: (i64, i64)| -> f64 {
        ((curr.0 - c.0).abs() + (curr.1 - c.1).abs()) as f64 - CAMERA_WEIGHT * (c.0.abs() + c.1.abs()) as f64
    };
    let mut heap: Vec<(f64, (i64, i64))> = Vec::new();
    for shape in safe {
        for poly in geom::paths_of(shape) {
            let n = poly.len();
            for (i, p) in poly.iter().enumerate() {
                let Some(q) = poly.get((i + 1) % n) else { continue };
                let (p, step) = ((p.x, p.y), (q.x - p.x, q.y - p.y));
                let length = (step.0.abs() + step.1.abs()) as f64;
                let mut best = (f64::MAX, (0, 0));
                if length < SEGMENT {
                    best = (penalty(p), p);
                } else {
                    let steps = (length / SEGMENT) as i64;
                    let dir = ((step.0 as f64 * SEGMENT / length) as i64, (step.1 as f64 * SEGMENT / length) as i64);
                    for k in 0..=steps {
                        let c = (p.0 + dir.0 * k, p.1 + dir.1 * k);
                        let d = penalty(c);
                        if d < best.0 {
                            best = (d, c);
                        }
                    }
                }
                push_heap(&mut heap, best);
                if heap.len() > MAX_CANDIDATES {
                    pop_heap(&mut heap);
                }
            }
        }
    }
    // The last left in the heap is the lowest, and the first Orca tries.
    let mut last = (0, 0);
    while let Some(top) = pop_heap(&mut heap) {
        last = top.1;
    }
    last
}

type Candidate = (f64, (i64, i64));

fn get(heap: &[Candidate], i: usize) -> f64 {
    heap.get(i).map_or(f64::MIN, |c| c.0)
}

/// `std::push_heap`, for `priority_queue`.
fn push_heap(heap: &mut Vec<Candidate>, value: Candidate) {
    heap.push(value);
    let mut hole = heap.len() - 1;
    while hole > 0 {
        let parent = (hole - 1) / 2;
        if get(heap, parent) >= value.0 {
            break;
        }
        heap.swap(hole, parent);
        hole = parent;
    }
}

/// `std::pop_heap`: the hole sinks to a leaf along the larger children, then the old last rises.
fn pop_heap(heap: &mut Vec<Candidate>) -> Option<Candidate> {
    let last = heap.pop()?;
    let Some(&top) = heap.first() else { return Some(last) };
    let n = heap.len();
    let mut hole = 0;
    let mut child = 0;
    while child < n.saturating_sub(1) / 2 {
        child = 2 * (child + 1);
        if get(heap, child) < get(heap, child - 1) {
            child -= 1;
        }
        heap.swap(hole, child);
        hole = child;
    }
    if n.is_multiple_of(2) && child == (n - 2) / 2 {
        child = 2 * (child + 1);
        heap.swap(hole, child - 1);
        hole = child - 1;
    }
    if let Some(slot) = heap.get_mut(hole) {
        *slot = last;
    }
    while hole > 0 {
        let parent = (hole - 1) / 2;
        if get(heap, parent) >= last.0 {
            break;
        }
        heap.swap(hole, parent);
        hole = parent;
    }
    Some(top)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn x2d(objects: Vec<Instance>) -> Picker {
        let v = |x: f64| DVec2::new(x, 0.0);
        let area = |x: f64| vec![v(x), v(256.0), DVec2::new(256.0, 256.0), DVec2::new(x, 256.0)];
        Picker::new(area(0.0), &[], &[area(0.0), area(20.5)], &[261.0, 256.0], 60.0, objects)
    }

    #[test]
    fn the_x2d_parks_as_orca_does_for_a_cube() {
        let cube = Instance { min: DVec2::new(128.25, 118.0), max: DVec2::new(148.25, 138.0), height: 20.0 };
        assert_eq!(x2d(vec![cube]).pick(0.4, 0, 0), (191, 130));
    }

    #[test]
    fn nothing_on_the_plate_is_safe_anywhere() {
        let big = Instance { min: DVec2::new(0.0, 0.0), max: DVec2::new(256.0, 256.0), height: 20.0 };
        assert_eq!(x2d(vec![big]).pick(0.4, 0, 0), (0, 0));
    }

    #[test]
    fn the_heap_gives_the_largest_first() {
        let mut heap = Vec::new();
        for v in [3.0, 9.0, 1.0, 7.0, 5.0, 8.0, 2.0] {
            push_heap(&mut heap, (v, (0, 0)));
        }
        let mut out = Vec::new();
        while let Some(t) = pop_heap(&mut heap) {
            out.push(t.0);
        }
        assert_eq!(out, [9.0, 8.0, 7.0, 5.0, 3.0, 2.0, 1.0]);
    }
}
