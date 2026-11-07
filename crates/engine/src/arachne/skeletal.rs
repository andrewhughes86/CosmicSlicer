//! `SkeletalTrapezoidation`: the Voronoi diagram of the outline turned into a graph of
//! trapezoids, each joint knowing its distance to the outline; bead counts chosen along the
//! centre, transitions between counts placed and filtered, beadings propagated, and the
//! junctions of each bead joined into lines.

use std::collections::{HashMap, HashSet};

use super::beading::{Beading, Strategy};
use super::geo;
use super::graph::{EdgeType, Graph, List, NIL, WALK};
use super::voronoi::{self, Diagram, Input, NONE};
use super::{ExtrusionJunction, ExtrusionLine};
use crate::clipper::Pt;

#[derive(Clone, Copy, Debug)]
struct Mid {
    pos: i64,
    lower: i64,
    radius: i64,
}

#[derive(Clone, Copy, Debug)]
struct End {
    pos: i64,
    lower: i64,
    is_lower_end: bool,
}

#[derive(Clone, Debug)]
struct Propagation {
    beading: Beading,
    to_bottom: i64,
    from_top: i64,
    upward_only: bool,
}

impl Propagation {
    fn new(beading: Beading) -> Propagation {
        Propagation { beading, to_bottom: 0, from_top: 0, upward_only: false }
    }
}

/// `scaled<coord_t>(0.02)`.
const SNAP: i64 = 20_000;
/// `SKELETAL_TRAPEZOIDATION_BEAD_SEARCH_MAX`.
const BEAD_SEARCH_MAX: usize = 1000;
/// Deepest recursion along the graph before giving up on that branch.
const DEPTH: usize = 2000;

pub struct Skeletal<'a> {
    g: Graph,
    strategy: &'a Strategy,
    angle: f64,
    step: i64,
    filter_dist: i64,
    allowed_deviation: i64,
    propagation_dist: i64,
    mids: Vec<List<Mid>>,
    ends: Vec<List<End>>,
    beadings: Vec<Propagation>,
    junctions: Vec<Vec<ExtrusionJunction>>,
    pub out: Vec<Vec<ExtrusionLine>>,
}

/// A transition middle in some edge's list.
type MidRef = (u32, u32);

impl<'a> Skeletal<'a> {
    pub fn new(
        polys: &[Vec<Pt>],
        strategy: &'a Strategy,
        angle: f64,
        step: i64,
        filter_dist: i64,
        allowed_deviation: i64,
        propagation_dist: i64,
    ) -> Skeletal<'a> {
        let mut st = Skeletal {
            g: Graph::default(),
            strategy,
            angle,
            step,
            filter_dist,
            allowed_deviation,
            propagation_dist,
            mids: Vec::new(),
            ends: Vec::new(),
            beadings: Vec::new(),
            junctions: Vec::new(),
            out: Vec::new(),
        };
        st.construct(polys);
        st
    }

    // ---- Construction -------------------------------------------------------------------

    fn construct(&mut self, polys: &[Vec<Pt>]) {
        let mut segments = Vec::new();
        for (poly, points) in polys.iter().enumerate() {
            for point in 0..points.len() {
                segments.push(voronoi::Segment { poly, point });
            }
        }
        let input = Input { polys, segments };
        let Some(vd) = Diagram::construct(&input) else { return };
        let mut edge_map: HashMap<usize, u32> = HashMap::new();
        let mut node_map: HashMap<usize, u32> = HashMap::new();
        for cell in &vd.cells {
            if cell.incident == NONE {
                continue;
            }
            let (start_source, end_source, range) = if cell.contains_point() {
                let p = voronoi::source_point(cell, &input);
                let range = vd.point_cell_range(cell, &input, p);
                if !range.valid() {
                    continue;
                }
                (p, p, range)
            } else {
                let (from, to) = (input.from(cell.source), input.to(cell.source));
                (to, from, vd.segment_cell_range(cell, from, to))
            };
            if range.begin == NONE || range.end == NONE {
                continue;
            }
            let mut prev = NIL;
            let begin_v1 = vd.to_point(vd.v1(range.begin));
            self.transfer_edge(
                &vd,
                &input,
                &mut edge_map,
                &mut node_map,
                start_source,
                begin_v1,
                range.begin,
                &mut prev,
                start_source,
                end_source,
            );
            if let Some(&n) = vd.edge(range.begin).and_then(|e| node_map.get(&e.v0)) {
                self.g.set_r(n, 0);
            }
            if prev == NIL {
                continue;
            }
            self.g.make_rib(&mut prev, start_source, end_source);
            let mut e = vd.edge(range.begin).map_or(NONE, |x| x.next);
            let mut guard = 0;
            while e != range.end && e != NONE && guard < vd.edges.len() {
                let v0 = vd.edge(e).map_or(Pt::default(), |x| vd.to_point(x.v0));
                let v1 = vd.to_point(vd.v1(e));
                self.transfer_edge(
                    &vd,
                    &input,
                    &mut edge_map,
                    &mut node_map,
                    v0,
                    v1,
                    e,
                    &mut prev,
                    start_source,
                    end_source,
                );
                if prev == NIL {
                    break;
                }
                self.g.make_rib(&mut prev, start_source, end_source);
                e = vd.edge(e).map_or(NONE, |x| x.next);
                guard += 1;
            }
            let end_v0 = vd.edge(range.end).map_or(Pt::default(), |x| vd.to_point(x.v0));
            self.transfer_edge(
                &vd,
                &input,
                &mut edge_map,
                &mut node_map,
                end_v0,
                end_source,
                range.end,
                &mut prev,
                start_source,
                end_source,
            );
            let last_to = self.g.to(prev);
            self.g.set_r(last_to, 0);
        }
        self.separate_pointy_quad_end_nodes();
        self.g.collapse_small_edges(5);
        for e in self.g.edge_order.ids() {
            if self.g.prev(e) == NIL {
                let from = self.g.from(e);
                if let Some(n) = self.g.nm(from) {
                    n.incident = e;
                }
            }
        }
    }

    fn make_node(&mut self, node_map: &mut HashMap<usize, u32>, v: usize, p: Pt) -> u32 {
        if let Some(&n) = node_map.get(&v) {
            return n;
        }
        let n = self.g.new_node(p, true);
        node_map.insert(v, n);
        n
    }

    #[allow(clippy::too_many_arguments)]
    fn transfer_edge(
        &mut self,
        vd: &Diagram,
        input: &Input<'_>,
        edge_map: &mut HashMap<usize, u32>,
        node_map: &mut HashMap<usize, u32>,
        from: Pt,
        to: Pt,
        vd_edge: usize,
        prev: &mut u32,
        start_source: Pt,
        end_source: Pt,
    ) {
        let Some(edge) = vd.edge(vd_edge).copied() else { return };
        if let Some(&source_twin) = edge_map.get(&edge.twin) {
            let Some(&end_node) = node_map.get(&vd.v1(vd_edge)) else { return };
            let mut twin = source_twin;
            let mut guard = 0;
            loop {
                if twin == NIL || guard > WALK {
                    return;
                }
                guard += 1;
                let e = self.g.new_edge(EdgeType::Normal, true);
                let (tf, tt) = (self.g.from(twin), self.g.to(twin));
                if let Some(x) = self.g.em(e) {
                    x.from = tt;
                    x.to = tf;
                    x.twin = twin;
                }
                if let Some(x) = self.g.em(twin) {
                    x.twin = e;
                }
                if let Some(n) = self.g.nm(tt) {
                    n.incident = e;
                }
                if *prev != NIL {
                    if let Some(x) = self.g.em(e) {
                        x.prev = *prev;
                    }
                    if let Some(x) = self.g.em(*prev) {
                        x.next = e;
                    }
                }
                *prev = e;
                if tf == end_node {
                    return;
                }
                let tp = self.g.prev(twin);
                if tp == NIL || self.g.twin(tp) == NIL || self.g.prev(self.g.twin(tp)) == NIL {
                    return;
                }
                self.g.make_rib(prev, start_source, end_source);
                twin = self.g.prev(self.g.twin(tp));
            }
        }
        let points = self.discretize(vd, input, vd_edge);
        let mut v0 = if *prev != NIL { self.g.to(*prev) } else { self.make_node(node_map, edge.v0, from) };
        let count = points.len();
        for (i, &p1) in points.iter().enumerate().skip(1) {
            let v1 =
                if i + 1 < count { self.g.new_node(p1, true) } else { self.make_node(node_map, vd.v1(vd_edge), to) };
            let e = self.g.new_edge(EdgeType::Normal, true);
            if let Some(x) = self.g.em(e) {
                x.from = v0;
                x.to = v1;
            }
            if let Some(n) = self.g.nm(v0) {
                n.incident = e;
            }
            if *prev != NIL {
                if let Some(x) = self.g.em(e) {
                    x.prev = *prev;
                }
                if let Some(x) = self.g.em(*prev) {
                    x.next = e;
                }
            }
            *prev = e;
            v0 = v1;
            if i + 1 < count {
                self.g.make_rib(prev, start_source, end_source);
            }
        }
        edge_map.entry(vd_edge).or_insert(*prev);
    }

    fn discretize(&self, vd: &Diagram, input: &Input<'_>, e: usize) -> Vec<Pt> {
        let Some(edge) = vd.edge(e) else { return Vec::new() };
        let twin = vd.edge(edge.twin);
        let (Some(left), Some(right)) = (vd.cells.get(edge.cell), twin.and_then(|t| vd.cells.get(t.cell))) else {
            return Vec::new();
        };
        let start = vd.to_point(edge.v0);
        let end = vd.to_point(vd.v1(e));
        let (pl, pr) = (left.contains_point(), right.contains_point());
        if (!pl && !pr) || !edge.primary {
            return vec![start, end];
        }
        if pl != pr {
            let (point_cell, segment_cell) = if pl { (left, right) } else { (right, left) };
            let p = voronoi::source_point(point_cell, input);
            let (a, b) = (input.from(segment_cell.source), input.to(segment_cell.source));
            return voronoi::discretize_parabola(p, a, b, start, end, self.step, self.angle as f32);
        }
        let lp = voronoi::source_point(left, input);
        let rp = voronoi::source_point(right, input);
        let d = geo::norm(geo::sub(rp, lp));
        let middle = Pt::new((lp.x + rp.x) / 2, (lp.y + rp.y) / 2);
        let axis = geo::perp(geo::sub(rp, lp));
        let axis_len = geo::norm(axis);
        if axis_len == 0 {
            return vec![start, end];
        }
        let projected = |p: Pt| geo::dot(geo::sub(p, middle), axis) / axis_len;
        let (start_x, end_x) = (projected(start), projected(end));
        let bound = (0.5 / ((std::f64::consts::PI - self.angle) * 0.5).tan()) as f32;
        let mut ms_x = ((-d) as f32 * bound) as i64;
        let mut me_x = (d as f32 * bound) as i64;
        let at = |x: i64| geo::add(middle, Pt::new(axis.x * x / axis_len, axis.y * x / axis_len));
        let (mut marking_start, mut marking_end) = (at(ms_x), at(me_x));
        let mut direction = 1i64;
        if start_x > end_x {
            direction = -1;
            std::mem::swap(&mut marking_start, &mut marking_end);
            std::mem::swap(&mut ms_x, &mut me_x);
        }
        let ab = geo::sub(end, start);
        let ab_size = geo::norm(ab);
        let mut out = vec![start];
        let mut add_start = ms_x * direction > start_x * direction;
        let mut add_end = me_x * direction > start_x * direction;
        let mut steps = if self.step > 0 { (ab_size + self.step / 2) / self.step } else { 0 };
        if steps % 2 == 1 {
            steps += 1;
        }
        for k in 1..steps.min(1_000_000) {
            let here = geo::add(start, Pt::new(ab.x * k / steps, ab.y * k / steps));
            let x_here = projected(here);
            if add_start && ms_x * direction < x_here * direction {
                out.push(marking_start);
                add_start = false;
            }
            if add_end && me_x * direction < x_here * direction {
                out.push(marking_end);
                add_end = false;
            }
            out.push(here);
        }
        if add_end && me_x * direction < end_x * direction {
            out.push(marking_end);
        }
        out.push(end);
        out
    }

    fn separate_pointy_quad_end_nodes(&mut self) {
        let mut visited: HashSet<u32> = HashSet::new();
        for e in self.g.edge_order.ids() {
            if self.g.prev(e) != NIL {
                continue;
            }
            let from = self.g.from(e);
            if visited.insert(from) {
                continue;
            }
            let copy = self.g.n(from);
            self.g.nodes.push(copy);
            let new = (self.g.nodes.len() - 1) as u32;
            self.g.node_order.push_back(new);
            if let Some(n) = self.g.nm(new) {
                n.incident = e;
            }
            if let Some(x) = self.g.em(e) {
                x.from = new;
            }
            let twin = self.g.twin(e);
            if let Some(x) = self.g.em(twin) {
                x.to = new;
            }
        }
    }

    // ---- The steps of `generateToolpaths` ------------------------------------------------

    pub fn generate(&mut self) {
        self.update_is_central();
        // `filterCentral` keeps nothing: its condition asks for a local maximum that is not one.
        self.update_bead_count();
        self.filter_noncentral_regions();
        self.generate_transitioning_ribs();
        self.generate_extra_ribs();
        self.generate_segments();
    }

    fn update_is_central(&mut self) {
        let outer_filter = self.strategy.transition_thickness(0) / 2;
        let cap = (self.strategy.transitioning_angle() * 0.5).sin() as f32;
        for e in self.g.edge_order.ids() {
            let twin = self.g.twin(e);
            if twin == NIL {
                continue;
            }
            let td = self.g.e(twin).data;
            let central = if td.central_is_set() {
                td.is_central()
            } else if self.g.e(e).data.kind == EdgeType::ExtraVd {
                false
            } else {
                let (f, t) = (self.g.from(e), self.g.to(e));
                if self.g.r(f).max(self.g.r(t)) < outer_filter {
                    false
                } else {
                    let dr = (self.g.r(t) - self.g.r(f)).abs();
                    let dd = self.g.length(e);
                    (dr as f32) < dd as f32 * cap
                }
            };
            self.g.set_central(e, central);
        }
    }

    fn update_bead_count(&mut self) {
        for e in self.g.edge_order.ids() {
            if self.g.central(e) {
                let to = self.g.to(e);
                let n = self.strategy.optimal_bead_count(self.g.r(to) * 2);
                self.g.set_beads(to, n);
            }
        }
        for n in self.g.node_order.ids() {
            if !self.g.is_local_maximum(n, false) {
                continue;
            }
            if self.g.r(n) < 0 {
                let mut r = i64::MAX;
                for e in self.g.around(n) {
                    r = r.min(self.g.r(self.g.to(e)) + self.g.length(e));
                }
                self.g.set_r(n, r);
            }
            let beads = self.strategy.optimal_bead_count(self.g.r(n).saturating_mul(2));
            self.g.set_beads(n, beads);
        }
    }

    fn is_end_of_central(&self, e: u32) -> bool {
        if !self.g.central(e) {
            return false;
        }
        if self.g.next(e) == NIL {
            return true;
        }
        !self.g.fan(e).iter().any(|x| self.g.central(*x))
    }

    fn filter_noncentral_regions(&mut self) {
        for e in self.g.edge_order.ids() {
            if !self.is_end_of_central(e) {
                continue;
            }
            let beads = self.g.beads(self.g.to(e));
            self.filter_noncentral(e, beads, 0, 400_000, 0);
        }
    }

    fn filter_noncentral(&mut self, to_edge: u32, beads: i64, traveled: i64, max: i64, depth: usize) -> bool {
        if depth > DEPTH {
            return false;
        }
        let r = self.g.r(self.g.to(to_edge));
        let twin = self.g.twin(to_edge);
        let mut next = self.g.next(to_edge);
        let mut steps = 0;
        while next != NIL && next != twin && steps < WALK {
            let (nf, nt) = (self.g.from(next), self.g.to(next));
            if self.g.r(nt) >= r || geo::shorter_then(geo::sub(self.g.p(nt), self.g.p(nf)), 10_000) {
                break;
            }
            next = self.g.next(self.g.twin(next));
            steps += 1;
        }
        if next == twin || next == NIL {
            return false;
        }
        let length = self.g.length(next);
        let nt = self.g.to(next);
        let dissolve = if self.g.beads(nt) == beads {
            true
        } else if self.g.beads(nt) < 0 {
            self.filter_noncentral(next, beads, traveled + length, max, depth + 1)
        } else {
            traveled + length < max && (self.g.beads(nt) - beads).abs() == 1
        };
        if dissolve {
            self.g.set_central(next, true);
            let nt_twin = self.g.twin(next);
            self.g.set_central(nt_twin, true);
            let n = self.strategy.optimal_bead_count(self.g.r(nt) * 2);
            self.g.set_beads(nt, n);
            self.g.set_ratio(nt, 0.0);
        }
        dissolve
    }

    // ---- Transitions --------------------------------------------------------------------

    fn mids_of(&self, e: u32) -> Option<&List<Mid>> {
        self.mids.get(self.g.e(e).data.transitions as usize)
    }

    fn has_mids(&self, e: u32) -> bool {
        self.mids_of(e).is_some_and(|l| !l.is_empty())
    }

    fn ends_of(&self, e: u32) -> Option<&List<End>> {
        self.ends.get(self.g.e(e).data.ends as usize)
    }

    fn has_ends(&self, e: u32) -> bool {
        self.ends_of(e).is_some_and(|l| !l.is_empty())
    }

    fn generate_transitioning_ribs(&mut self) {
        self.generate_transition_mids();
        self.filter_transition_mids();
        self.generate_all_transition_ends();
        self.apply_transitions();
    }

    fn generate_transition_mids(&mut self) {
        for e in self.g.edge_order.ids() {
            if !self.g.central(e) {
                continue;
            }
            let (f, t) = (self.g.from(e), self.g.to(e));
            let (start_r, end_r) = (self.g.r(f), self.g.r(t));
            let (start_beads, end_beads) = (self.g.beads(f), self.g.beads(t));
            if start_r >= end_r || start_beads == end_beads {
                continue;
            }
            let size = self.g.length(e);
            for lower in start_beads..end_beads.min(start_beads.saturating_add(10_000)) {
                let mid_r = (self.strategy.transition_thickness(lower) / 2).min(end_r).max(start_r);
                let pos = size * (mid_r - start_r) / (end_r - start_r);
                if self.mids_of(e).is_none() {
                    self.mids.push(List::default());
                    let id = (self.mids.len() - 1) as u32;
                    if let Some(x) = self.g.em(e) {
                        x.data.transitions = id;
                    }
                }
                let list = self.g.e(e).data.transitions as usize;
                if let Some(l) = self.mids.get_mut(list) {
                    l.push_back(Mid { pos, lower, radius: mid_r });
                }
            }
        }
    }

    fn mid_list(&mut self, e: u32) -> Option<&mut List<Mid>> {
        let i = self.g.e(e).data.transitions as usize;
        self.mids.get_mut(i)
    }

    fn filter_transition_mids(&mut self) {
        for e in self.g.edge_order.ids() {
            if !self.has_mids(e) {
                continue;
            }
            let size = self.g.length(e);
            let Some(back) = self.mids_of(e).and_then(|l| l.back().copied()) else { continue };
            let refs = self.dissolve_nearby(e, back, size - back.pos, self.filter_dist, true, 0);
            let mut dissolve_back = !refs.is_empty();
            for (edge, item) in refs {
                let Some(b) = self.mids_of(e).and_then(|l| l.back().copied()) else { break };
                self.dissolve_bead_count_region(e, b.lower + 1, b.lower, 0);
                if let Some(l) = self.mid_list(edge) {
                    l.erase(item);
                }
            }
            let Some(back) = self.mids_of(e).and_then(|l| l.back().copied()) else { continue };
            let c = back.lower;
            let half = ((1.0 - f64::from(self.strategy.transition_anchor_pos(c)))
                * self.strategy.transitioning_length(c) as f64) as i64;
            dissolve_back |= self.filter_end_of_central_transition(e, size - back.pos, half, c, 0);
            if dissolve_back && let Some(l) = self.mid_list(e) {
                l.pop_back();
            }
            let Some(front) = self.mids_of(e).and_then(|l| l.front().copied()) else { continue };
            let twin = self.g.twin(e);
            let refs = self.dissolve_nearby(twin, front, front.pos, self.filter_dist, false, 0);
            let mut dissolve_front = !refs.is_empty();
            for (edge, item) in refs {
                let Some(f) = self.mids_of(e).and_then(|l| l.front().copied()) else { break };
                self.dissolve_bead_count_region(twin, f.lower, f.lower + 1, 0);
                if let Some(l) = self.mid_list(edge) {
                    l.erase(item);
                }
            }
            let Some(front) = self.mids_of(e).and_then(|l| l.front().copied()) else { continue };
            let c = front.lower;
            let half = (self.strategy.transition_anchor_pos(c) * self.strategy.transitioning_length(c) as f32) as i64;
            dissolve_front |= self.filter_end_of_central_transition(twin, front.pos, half, c + 1, 0);
            if dissolve_front && let Some(l) = self.mid_list(e) {
                l.pop_front();
            }
        }
    }

    fn dissolve_nearby(
        &self,
        start: u32,
        origin: Mid,
        traveled: i64,
        max: i64,
        going_up: bool,
        depth: usize,
    ) -> Vec<MidRef> {
        let mut out: Vec<MidRef> = Vec::new();
        if traveled > max || depth > DEPTH {
            return out;
        }
        let mut should = true;
        for e in self.g.fan(start) {
            if !self.g.central(e) {
                continue;
            }
            let size = self.g.length(e);
            let aligned = self.g.is_upward(e);
            let edge = if aligned { e } else { self.g.twin(e) };
            let mut seen = false;
            let here_r = self.g.r(self.g.from(e));
            let odd = (origin.lower % 2 != 0) == going_up;
            let deviation = (origin.radius - here_r).abs() * 2;
            let line_deviation = if odd { deviation } else { deviation / 2 };
            if line_deviation > self.allowed_deviation {
                should = false;
            }
            if should && let Some(list) = self.mids_of(edge) {
                for id in list.ids() {
                    let Some(t) = list.items.get(id as usize) else { continue };
                    let pos = if aligned { t.pos } else { size - t.pos };
                    if traveled + pos < max && t.lower == origin.lower {
                        out.push((edge, id));
                        seen = true;
                    }
                }
            }
            if should && !seen {
                let here = self.dissolve_nearby(e, origin, traveled + size, max, going_up, depth + 1);
                if here.is_empty() {
                    return Vec::new();
                }
                out.extend(here);
                should = !out.is_empty();
            }
        }
        if !should {
            out.clear();
        }
        out
    }

    fn dissolve_bead_count_region(&mut self, start: u32, from_beads: i64, to_beads: i64, depth: usize) {
        let to = self.g.to(start);
        if self.g.beads(to) != from_beads || depth > DEPTH {
            return;
        }
        self.g.set_beads(to, to_beads);
        for e in self.g.fan(start) {
            if self.g.central(e) {
                self.dissolve_bead_count_region(e, from_beads, to_beads, depth + 1);
            }
        }
    }

    fn filter_end_of_central_transition(
        &mut self,
        start: u32,
        traveled: i64,
        max: i64,
        replacing: i64,
        depth: usize,
    ) -> bool {
        if traveled > max || depth > DEPTH {
            return false;
        }
        let mut end_of_central = true;
        let mut dissolve = false;
        for e in self.g.fan(start) {
            if self.g.central(e) {
                let length = self.g.length(e);
                dissolve |= self.filter_end_of_central_transition(e, traveled + length, max, replacing, depth + 1);
                end_of_central = false;
            }
        }
        if end_of_central && traveled < max {
            dissolve = true;
        }
        if dissolve {
            let to = self.g.to(start);
            self.g.set_beads(to, replacing);
        }
        dissolve
    }

    fn generate_all_transition_ends(&mut self) {
        for e in self.g.edge_order.ids() {
            if !self.has_mids(e) {
                continue;
            }
            for mid in self.mids_of(e).map(List::values).unwrap_or_default() {
                self.generate_transition_ends(e, mid.pos, mid.lower);
            }
        }
    }

    fn generate_transition_ends(&mut self, e: u32, mid_pos: i64, lower: i64) {
        let size = self.g.length(e);
        let length = self.strategy.transitioning_length(lower);
        let mid_position = self.strategy.transition_anchor_pos(lower);
        let mid_rest = f64::from(mid_position);
        let twin = self.g.twin(e);
        {
            let start = size - mid_pos;
            let half = (mid_position * length as f32) as i64;
            self.generate_transition_end(twin, start, start + half, half, mid_rest, 0.0, lower, 0);
        }
        {
            let start = mid_pos;
            let half = ((1.0 - f64::from(mid_position)) * length as f64) as i64;
            // Orca calls it twice: once in a test that only logs, then again.
            self.generate_transition_end(e, start, start + half, half, mid_rest, 1.0, lower, 0);
            self.generate_transition_end(e, start, start + half, half, mid_rest, 1.0, lower, 0);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn generate_transition_end(
        &mut self,
        e: u32,
        start_pos: i64,
        end_pos: i64,
        half: i64,
        start_rest: f64,
        end_rest: f64,
        lower: i64,
        depth: usize,
    ) -> bool {
        let size = self.g.length(e);
        let going_up = end_rest > start_rest;
        if !self.g.central(e) || depth > DEPTH {
            return false;
        }
        if end_pos > size {
            let rest =
                (end_rest - (start_rest - end_rest) * (end_pos - size) as f64 / (start_pos - end_pos) as f64) as f32;
            let fan = self.g.fan(e);
            let central_count = fan.iter().filter(|x| self.g.central(**x)).count();
            let mut only_down = true;
            let mut recursed = false;
            for out in fan {
                if !self.g.central(out) {
                    continue;
                }
                if central_count > 1 && going_up && self.is_going_down(out, 0, end_pos - size + half, lower, 0) {
                    continue;
                }
                let down = self.generate_transition_end(
                    out,
                    0,
                    end_pos - size,
                    half,
                    f64::from(rest),
                    end_rest,
                    lower,
                    depth + 1,
                );
                only_down &= down;
                recursed = true;
            }
            if !going_up || (recursed && !only_down) {
                let to = self.g.to(e);
                self.g.set_ratio(to, rest);
                self.g.set_beads(to, lower);
            }
            return only_down;
        }
        let is_lower_end = end_rest == 0.0;
        let (upward, pos) = if self.g.is_upward(e) { (e, end_pos) } else { (self.g.twin(e), size - end_pos) };
        if self.ends_of(upward).is_none() {
            self.ends.push(List::default());
            let id = (self.ends.len() - 1) as u32;
            if let Some(x) = self.g.em(upward) {
                x.data.ends = id;
            }
        }
        let i = self.g.e(upward).data.ends as usize;
        if let Some(l) = self.ends.get_mut(i) {
            let end = End { pos, lower, is_lower_end };
            if l.front().is_none_or(|f| pos < f.pos) {
                l.push_front(end);
            } else {
                l.push_back(end);
            }
        }
        false
    }

    fn is_going_down(&self, out: u32, traveled: i64, max: i64, lower: i64, depth: usize) -> bool {
        let (f, t) = (self.g.from(out), self.g.to(out));
        if self.g.r(t) == 0 {
            return true;
        }
        if depth > DEPTH {
            return false;
        }
        let is_upward = self.g.r(t) >= self.g.r(f);
        let upward = if is_upward { out } else { self.g.twin(out) };
        if self.g.beads(t) > lower + 1 {
            return false;
        }
        let length = self.g.length(out);
        if let Some(list) = self.mids_of(upward).filter(|l| !l.is_empty()) {
            let mid = if is_upward { list.front() } else { list.back() };
            if let Some(mid) = mid
                && mid.lower == lower
                && ((is_upward && mid.pos + traveled < max) || (!is_upward && length - mid.pos + traveled < max))
            {
                return true;
            }
        }
        if traveled + length > max {
            return false;
        }
        let tn = self.g.n(t).data;
        if tn.beads <= lower && !(tn.beads == lower && tn.ratio > 0.0) {
            return true;
        }
        let mut only_down = true;
        let mut recursed = false;
        for next in self.g.fan(out) {
            if !self.g.central(next) {
                continue;
            }
            only_down &= self.is_going_down(next, traveled + length, max, lower, depth + 1);
            recursed = true;
        }
        recursed && only_down
    }

    fn apply_transitions(&mut self) {
        for e in self.g.edge_order.ids() {
            let twin = self.g.twin(e);
            if !self.has_ends(twin) {
                continue;
            }
            let length = self.g.length(e);
            let twin_list = self.g.e(twin).data.ends as usize;
            let twin_ends = self.ends.get(twin_list).map(List::values).unwrap_or_default();
            if !self.has_ends(e) {
                self.ends.push(List::default());
                let id = (self.ends.len() - 1) as u32;
                if let Some(x) = self.g.em(e) {
                    x.data.ends = id;
                }
            }
            let mine = self.g.e(e).data.ends as usize;
            if let Some(l) = self.ends.get_mut(mine) {
                for end in &twin_ends {
                    l.push_back(End { pos: length - end.pos, lower: end.lower, is_lower_end: end.is_lower_end });
                }
            }
            if let Some(l) = self.ends.get_mut(twin_list) {
                l.clear();
            }
        }
        let mut e = self.g.edge_order.first();
        let mut guard = 0;
        while e != NIL && guard < 10_000_000 {
            guard += 1;
            if self.has_ends(e) {
                let i = self.g.e(e).data.ends as usize;
                let sorted = self.ends.get_mut(i).map(|l| {
                    let mut ids = l.ids();
                    ids.sort_by_key(|id| l.items.get(*id as usize).map_or(0, |x| x.pos));
                    l.order.set(&ids);
                    l.values()
                });
                let (from, to) = (self.g.from(e), self.g.to(e));
                let (a, b) = (self.g.p(from), self.g.p(to));
                let ab = geo::sub(b, a);
                let size = geo::norm(ab);
                let mut last = e;
                for end in sorted.unwrap_or_default() {
                    let beads = if end.is_lower_end { end.lower } else { end.lower + 1 };
                    let close = if end.pos < size / 2 { from } else { to };
                    if (end.pos < SNAP || end.pos > size - SNAP) && self.g.beads(close) == beads {
                        self.g.set_ratio(close, 0.0);
                        continue;
                    }
                    let mid = geo::add(a, normal(ab, end.pos));
                    last = self.g.insert_node(last, mid, beads);
                }
            }
            e = self.g.edge_order.after(e);
        }
    }

    fn generate_extra_ribs(&mut self) {
        let mut e = self.g.edge_order.first();
        let mut guard = 0;
        while e != NIL && guard < 10_000_000 {
            guard += 1;
            let (from, to) = (self.g.from(e), self.g.to(e));
            let skip = !self.g.central(e)
                || geo::shorter_then(geo::sub(self.g.p(to), self.g.p(from)), self.step)
                || self.g.r(from) >= self.g.r(to);
            if !skip {
                let ribs = self.strategy.nonlinear_thicknesses();
                let (a, b) = (self.g.p(from), self.g.p(to));
                let ab = geo::sub(b, a);
                let size = geo::norm(ab);
                let (ar, br) = (self.g.r(from), self.g.r(to));
                let beads = self.g.beads(from).min(self.g.beads(to));
                let mut last = e;
                for rib in ribs {
                    if rib / 2 <= ar {
                        continue;
                    }
                    if rib / 2 >= br {
                        break;
                    }
                    let pos = size * (rib / 2 - ar) / (br - ar);
                    let close = if pos < size / 2 { from } else { to };
                    if (pos < SNAP || pos > size - SNAP) && self.g.beads(close) == beads {
                        self.g.set_ratio(close, 0.0);
                        continue;
                    }
                    let mid = geo::add(a, normal(ab, pos));
                    last = self.g.insert_node(last, mid, beads);
                }
            }
            e = self.g.edge_order.after(e);
        }
    }

    // ---- Beadings and lines -------------------------------------------------------------

    fn generate_segments(&mut self) {
        let mut mids: Vec<u32> = self
            .g
            .edge_order
            .ids()
            .into_iter()
            .filter(|&e| self.g.prev(e) != NIL && self.g.next(e) != NIL && self.g.is_upward(e))
            .collect();
        {
            let g = &self.g;
            let flat = |e: u32| g.r(g.from(e)) == g.r(g.to(e));
            let less = |a: &u32, b: &u32| -> bool {
                let (a, b) = (*a, *b);
                if g.r(g.to(a)) == g.r(g.to(b)) {
                    if flat(a) && flat(b) {
                        let up = |e: u32| {
                            let m = i64::MAX;
                            g.dist_to_go_up(e, 0).unwrap_or(m).min(g.dist_to_go_up(g.twin(e), 0).unwrap_or(m))
                                - g.length(e)
                        };
                        return up(a) < up(b);
                    } else if flat(a) {
                        return true;
                    } else if flat(b) {
                        return false;
                    }
                }
                g.r(g.to(a)) > g.r(g.to(b))
            };
            crate::connect::std_sort(&mut mids, &less);
        }
        for n in self.g.node_order.ids() {
            let d = self.g.n(n).data;
            if d.beads <= 0 {
                continue;
            }
            let beading = if d.ratio == 0.0 {
                self.strategy.compute(d.r * 2, d.beads)
            } else {
                let low = self.strategy.compute(d.r * 2, d.beads);
                let high = self.strategy.compute(d.r * 2, d.beads + 1);
                interpolate(&low, 1.0 - f64::from(d.ratio), &high)
            };
            self.beadings.push(Propagation::new(beading));
            let id = (self.beadings.len() - 1) as u32;
            if let Some(node) = self.g.nm(n) {
                node.data.beading = id;
            }
        }
        self.propagate_upward(&mids);
        self.propagate_downward(&mids);
        self.generate_junctions();
        self.connect_junctions();
        self.local_maxima_single_beads();
    }

    fn beading_of(&self, n: u32) -> Option<&Propagation> {
        self.beadings.get(self.g.n(n).data.beading as usize)
    }

    fn propagate_upward(&mut self, mids: &[u32]) {
        for &e in mids.iter().rev() {
            let (f, t) = (self.g.from(e), self.g.to(e));
            if self.g.beads(t) >= 0 {
                continue;
            }
            let Some(lower) = self.beading_of(f).cloned() else { continue };
            if self.beading_of(t).is_some() {
                continue;
            }
            let mut upper = lower;
            upper.to_bottom += self.g.length(e);
            upper.upward_only = true;
            self.beadings.push(upper);
            let id = (self.beadings.len() - 1) as u32;
            if let Some(n) = self.g.nm(t) {
                n.data.beading = id;
            }
        }
    }

    fn propagate_downward(&mut self, mids: &[u32]) {
        for &e in mids {
            if self.g.central(e) {
                continue;
            }
            let (f, t) = (self.g.from(e), self.g.to(e));
            if self.g.r(f) == self.g.r(t) && self.beading_of(f).is_some() && self.beading_of(t).is_none() {
                let twin = self.g.twin(e);
                self.propagate_down(twin);
            } else {
                self.propagate_down(e);
            }
        }
    }

    fn propagate_down(&mut self, e: u32) {
        let length = self.g.length(e);
        let (f, t) = (self.g.from(e), self.g.to(e));
        let Some(top_id) = self.get_or_create_beading(t) else { return };
        let Some(top) = self.beadings.get(top_id as usize).cloned() else { return };
        match self.beading_of(f).map(|_| self.g.n(f).data.beading) {
            None => {
                let mut down = top;
                down.from_top += length;
                self.beadings.push(down);
                let id = (self.beadings.len() - 1) as u32;
                if let Some(n) = self.g.nm(f) {
                    n.data.beading = id;
                }
            }
            Some(bottom_id) => {
                let Some(bottom) = self.beadings.get(bottom_id as usize).cloned() else { return };
                let total = top.from_top + length + bottom.to_bottom;
                let ratio = f64::from(bottom.to_bottom as f32 / total.min(self.propagation_dist) as f32).max(0.0);
                let replaced = if ratio >= 1.0 {
                    let mut b = top;
                    b.from_top += length;
                    b
                } else {
                    Propagation::new(interpolate_at(&top.beading, ratio, &bottom.beading, self.g.r(f)))
                };
                if let Some(slot) = self.beadings.get_mut(bottom_id as usize) {
                    *slot = replaced;
                }
            }
        }
    }

    fn get_or_create_beading(&mut self, n: u32) -> Option<u32> {
        if self.beading_of(n).is_none() {
            if self.g.beads(n) == -1 {
                if let Some(found) = self.nearest_beading(n, 100_000) {
                    return Some(found);
                }
                let mut dist = i64::MAX;
                let start = self.g.n(n).incident;
                let mut x = start;
                let mut steps = 0;
                while x != NIL && steps < WALK {
                    dist = dist.min(self.g.r(self.g.to(x)) + self.g.length(x));
                    x = self.g.next(self.g.twin(x));
                    steps += 1;
                    if x == start {
                        break;
                    }
                }
                let beads = self.strategy.optimal_bead_count(dist.saturating_mul(2));
                self.g.set_beads(n, beads);
            }
            let beading = self.strategy.compute(self.g.r(n) * 2, self.g.beads(n));
            self.beadings.push(Propagation::new(beading));
            let id = (self.beadings.len() - 1) as u32;
            if let Some(node) = self.g.nm(n) {
                node.data.beading = id;
            }
        }
        Some(self.g.n(n).data.beading)
    }

    /// `getNearestBeading`: a search outwards through the edges, nearest first, in
    /// `std::priority_queue`'s order.
    fn nearest_beading(&self, n: u32, max: i64) -> Option<u32> {
        let mut heap: Vec<(u32, i64)> = Vec::new();
        let less = |a: &(u32, i64), b: &(u32, i64)| a.1 > b.1;
        for e in self.g.around(n) {
            push_heap(&mut heap, (e, self.g.length(e)), &less);
        }
        for _ in 0..BEAD_SEARCH_MAX {
            let (e, dist) = pop_heap(&mut heap, &less)?;
            if dist > max {
                return None;
            }
            let to = self.g.to(e);
            if self.beading_of(to).is_some() {
                return Some(self.g.n(to).data.beading);
            }
            for further in self.g.fan(e) {
                push_heap(&mut heap, (further, dist + self.g.length(further)), &less);
            }
        }
        None
    }

    fn generate_junctions(&mut self) {
        for e in self.g.edge_order.ids() {
            let (f, t) = (self.g.from(e), self.g.to(e));
            if self.g.r(f) > self.g.r(t) {
                continue;
            }
            let (start_r, end_r) = (self.g.r(t), self.g.r(f));
            if (self.g.beads(f) == self.g.beads(t) && self.g.beads(f) >= 0) || end_r >= start_r {
                continue;
            }
            let Some(bid) = self.get_or_create_beading(t) else { continue };
            let Some(beading) = self.beadings.get(bid as usize).map(|b| b.beading.clone()) else { continue };
            let (a, b) = (self.g.p(t), self.g.p(f));
            let ab = geo::sub(b, a);
            let count = beading.toolpath_locations.len();
            let loc = |i: usize| beading.toolpath_locations.get(i).copied().unwrap_or(0);
            let mut j = (count.max(1) - 1) / 2;
            while j < count {
                if loc(j) <= start_r + 1 {
                    break;
                }
                j = j.wrapping_sub(1);
            }
            if j.wrapping_add(1) < count
                && loc(j.wrapping_add(1)) <= start_r + 5_000
                && beading.total_thickness < start_r + 5_000
            {
                j = j.wrapping_add(1);
            }
            let mut out = Vec::new();
            while j < count {
                let bead_r = loc(j);
                if bead_r < end_r {
                    break;
                }
                let mut junction = geo::add(
                    a,
                    Pt::new(
                        ab.x * (bead_r - start_r) / (end_r - start_r),
                        ab.y * (bead_r - start_r) / (end_r - start_r),
                    ),
                );
                if bead_r > start_r - 5_000 {
                    junction = a;
                }
                out.push(ExtrusionJunction {
                    p: junction,
                    w: beading.bead_widths.get(j).copied().unwrap_or(0),
                    perimeter_index: j,
                });
                j = j.wrapping_sub(1);
            }
            self.junctions.push(out);
            let id = (self.junctions.len() - 1) as u32;
            if let Some(x) = self.g.em(e) {
                x.data.junctions = id;
            }
        }
    }

    fn junctions_of(&mut self, e: u32) -> Vec<ExtrusionJunction> {
        let i = self.g.e(e).data.junctions as usize;
        match self.junctions.get(i) {
            Some(j) if !j.is_empty() => j.clone(),
            _ => {
                self.junctions.push(Vec::new());
                let id = (self.junctions.len() - 1) as u32;
                if let Some(x) = self.g.em(e) {
                    x.data.junctions = id;
                }
                Vec::new()
            }
        }
    }

    fn existing_junctions(&self, e: u32) -> Vec<ExtrusionJunction> {
        self.junctions.get(self.g.e(e).data.junctions as usize).cloned().unwrap_or_default()
    }

    /// `getQuadMaxRedgeTo`.
    fn quad_max_r_edge_to(&self, quad_start: u32) -> u32 {
        let mut max_r = -1;
        let mut ret = NIL;
        let mut e = quad_start;
        let mut steps = 0;
        while e != NIL && steps < WALK {
            let r = self.g.r(self.g.to(e));
            if r > max_r {
                max_r = r;
                ret = e;
            }
            e = self.g.next(e);
            steps += 1;
        }
        if ret != NIL && self.g.next(ret) == NIL && self.g.r(self.g.to(ret)) - 5_000 < self.g.r(self.g.from(ret)) {
            ret = self.g.prev(ret);
        }
        ret
    }

    fn connect_junctions(&mut self) {
        let mut unprocessed = DenseSet::default();
        for e in self.g.edge_order.ids() {
            if self.g.prev(e) == NIL {
                unprocessed.insert(e);
            }
        }
        let mut passed_odd: HashSet<u32> = HashSet::new();
        let mut guard = 0usize;
        while let Some(domain_start) = unprocessed.first() {
            let mut quad_start = domain_start;
            let mut new_domain = true;
            loop {
                guard += 1;
                if guard > 10_000_000 {
                    return;
                }
                let mut quad_end = quad_start;
                let mut steps = 0;
                while self.g.next(quad_end) != NIL && steps < WALK {
                    quad_end = self.g.next(quad_end);
                    steps += 1;
                }
                let to_peak = self.quad_max_r_edge_to(quad_start);
                let from_peak = self.g.next(to_peak);
                unprocessed.remove(quad_start);
                if to_peak == NIL || from_peak == NIL {
                    // Orca asserts these exist; without them there is nothing to join here.
                    let next = self.g.next_unconnected(quad_start);
                    if next == NIL || next == domain_start {
                        break;
                    }
                    quad_start = next;
                    continue;
                }
                let mut from_j = self.junctions_of(to_peak);
                let from_peak_twin = self.g.twin(from_peak);
                let mut to_j = self.junctions_of(from_peak_twin);
                let to_peak_prev = self.g.prev(to_peak);
                if to_peak_prev != NIL {
                    let prev_j = self.existing_junctions(to_peak_prev);
                    while let (Some(b), Some(f)) = (from_j.last(), prev_j.first()) {
                        if b.perimeter_index <= f.perimeter_index {
                            from_j.pop();
                        } else {
                            break;
                        }
                    }
                    from_j.extend(prev_j);
                }
                let from_peak_next = self.g.next(from_peak);
                if from_peak_next != NIL {
                    let next_j = self.existing_junctions(self.g.twin(from_peak_next));
                    while let (Some(b), Some(f)) = (to_j.last(), next_j.first()) {
                        if b.perimeter_index <= f.perimeter_index {
                            to_j.pop();
                        } else {
                            break;
                        }
                    }
                    to_j.extend(next_j);
                }
                let segments = from_j.len().min(to_j.len());
                let qs_to = self.g.to(quad_start);
                let qe_from = self.g.from(quad_end);
                let qs_d = self.g.n(qs_to).data;
                let qe_d = self.g.n(qe_from).data;
                for k in 0..segments {
                    let (Some(&from), Some(&to)) = (from_j.get(from_j.len() - 1 - k), to_j.get(to_j.len() - 1 - k))
                    else {
                        continue;
                    };
                    let last = k == segments - 1;
                    let from_odd = qs_d.beads > 0
                        && qs_d.beads % 2 == 1
                        && qs_d.ratio == 0.0
                        && last
                        && geo::shorter_then(geo::sub(from.p, self.g.p(qs_to)), 5_000);
                    let to_odd = qe_d.beads > 0
                        && qe_d.beads % 2 == 1
                        && qe_d.ratio == 0.0
                        && last
                        && geo::shorter_then(geo::sub(to.p, self.g.p(qe_from)), 5_000);
                    let odd = from_odd && to_odd;
                    let qs_next = self.g.next(quad_start);
                    if odd && passed_odd.contains(&self.g.twin(qs_next)) {
                        continue;
                    }
                    let from_3way = from_odd && self.g.is_multi_intersection(qs_to);
                    let to_3way = to_odd && self.g.is_multi_intersection(qe_from);
                    passed_odd.insert(qs_next);
                    self.add_segment(from, to, odd, new_domain, from_3way, to_3way);
                }
                new_domain = false;
                let next = self.g.next_unconnected(quad_start);
                if next == NIL || next == domain_start {
                    break;
                }
                quad_start = next;
            }
        }
    }

    fn add_segment(
        &mut self,
        from: ExtrusionJunction,
        to: ExtrusionJunction,
        odd: bool,
        force_new: bool,
        from_3way: bool,
        to_3way: bool,
    ) {
        if from == to {
            return;
        }
        let inset = from.perimeter_index;
        if inset > 100_000 {
            return;
        }
        if inset >= self.out.len() {
            self.out.resize(inset + 1, Vec::new());
        }
        let Some(lines) = self.out.get_mut(inset) else { return };
        let force = force_new
            || lines
                .last()
                .is_none_or(|l| l.is_odd != odd || l.junctions.last().is_none_or(|j| j.perimeter_index != inset));
        let tail = lines.last().and_then(|l| l.junctions.last()).copied();
        if let (false, Some(t)) = (force, tail) {
            if geo::shorter_then(geo::sub(t.p, from.p), 10_000) && (t.w - from.w).abs() < 10_000 && !from_3way {
                if let Some(l) = lines.last_mut() {
                    l.junctions.push(to);
                }
                return;
            }
            if geo::shorter_then(geo::sub(t.p, to.p), 10_000) && (t.w - to.w).abs() < 10_000 && !to_3way {
                if let Some(l) = lines.last_mut() {
                    l.junctions.push(from);
                }
                return;
            }
        }
        lines.push(ExtrusionLine { inset_idx: inset, is_odd: odd, is_closed: false, junctions: vec![from, to] });
    }

    fn local_maxima_single_beads(&mut self) {
        for n in self.g.node_order.ids() {
            let Some(beading) = self.beading_of(n).map(|b| b.beading.clone()) else { continue };
            if beading.bead_widths.len() % 2 == 1 && self.g.is_local_maximum(n, true) && !self.g.node_is_central(n) {
                let inset = beading.bead_widths.len() / 2;
                if inset >= self.out.len() {
                    self.out.resize(inset + 1, Vec::new());
                }
                let width = beading.bead_widths.get(inset).copied().unwrap_or(0);
                let r = width / 8;
                let p = self.g.p(n);
                let mut line =
                    ExtrusionLine { inset_idx: inset, is_odd: true, is_closed: false, junctions: Vec::new() };
                for k in 0..6 {
                    let a = (2.0 * std::f64::consts::PI / 6.0 * f64::from(k)) as f32;
                    let a = f64::from(a);
                    let off = geo::round_pt(r as f64 * a.cos(), r as f64 * a.sin());
                    line.junctions.push(ExtrusionJunction { p: geo::add(p, off), w: width, perimeter_index: inset });
                }
                if let Some(lines) = self.out.get_mut(inset) {
                    lines.push(line);
                }
            }
        }
    }
}

/// `normal`: `p` scaled to `len`.
fn normal(p: Pt, len: i64) -> Pt {
    let l = geo::norm(p);
    if l < 1 {
        return Pt::new(len, 0);
    }
    Pt::new(p.x * len / l, p.y * len / l)
}

/// `interpolate`: `ratio_left` of `left` and the rest of `right`.
fn interpolate(left: &Beading, ratio_left: f64, right: &Beading) -> Beading {
    let ratio_right = (1.0 - ratio_left) as f32;
    let mut ret = if left.total_thickness > right.total_thickness { left.clone() } else { right.clone() };
    let n = left.bead_widths.len().min(right.bead_widths.len());
    for i in 0..n {
        let (lw, rw) = (left.bead_widths.get(i).copied().unwrap_or(0), right.bead_widths.get(i).copied().unwrap_or(0));
        if let Some(w) = ret.bead_widths.get_mut(i) {
            *w = if lw == 0 || rw == 0 {
                0
            } else {
                (ratio_left * lw as f64 + f64::from(ratio_right * rw as f32)) as i64
            };
        }
        let (ll, rl) = (
            left.toolpath_locations.get(i).copied().unwrap_or(0),
            right.toolpath_locations.get(i).copied().unwrap_or(0),
        );
        if let Some(l) = ret.toolpath_locations.get_mut(i) {
            *l = (ratio_left * ll as f64 + f64::from(ratio_right * rl as f32)) as i64;
        }
    }
    ret
}

/// `interpolate` with a switching radius: keeps an inset from vanishing where the radius
/// still has room for it.
fn interpolate_at(left: &Beading, ratio_left: f64, right: &Beading, switching: i64) -> Beading {
    let ret = interpolate(left, ratio_left, right);
    let n = left.toolpath_locations.len() as i64;
    let mut next = n - 1;
    while next >= 0 {
        if switching > left.toolpath_locations.get(next as usize).copied().unwrap_or(0) {
            break;
        }
        next -= 1;
    }
    if next < 0 || next + 1 == n {
        return ret;
    }
    let i = next as usize;
    if ret.toolpath_locations.get(i).is_some_and(|l| *l > switching) {
        let (li, ri) = (
            left.toolpath_locations.get(i).copied().unwrap_or(0),
            right.toolpath_locations.get(i).copied().unwrap_or(0),
        );
        let ratio = (switching - ri) as f32 / (li - ri) as f32;
        let ratio = f64::from(ratio + 0.1).min(1.0) as f32;
        return interpolate(left, f64::from(ratio), right);
    }
    ret
}

/// `std::push_heap` with `less`.
fn push_heap<T: Copy>(heap: &mut Vec<T>, value: T, less: &dyn Fn(&T, &T) -> bool) {
    heap.push(value);
    let mut hole = heap.len() - 1;
    while hole > 0 {
        let parent = (hole - 1) / 2;
        match heap.get(parent) {
            Some(p) if less(p, &value) => {
                heap.swap(hole, parent);
                hole = parent;
            }
            _ => break,
        }
    }
}

/// `std::pop_heap` then `pop_back`: the top.
fn pop_heap<T: Copy>(heap: &mut Vec<T>, less: &dyn Fn(&T, &T) -> bool) -> Option<T> {
    let last = heap.pop()?;
    let Some(&top) = heap.first() else { return Some(last) };
    let n = heap.len();
    let lt = |h: &[T], i: usize, j: usize| matches!((h.get(i), h.get(j)), (Some(a), Some(b)) if less(a, b));
    let mut hole = 0;
    let mut child = 0;
    while child < n.saturating_sub(1) / 2 {
        child = 2 * (child + 1);
        if lt(heap, child, child - 1) {
            child -= 1;
        }
        heap.swap(hole, child);
        hole = child;
    }
    if n.is_multiple_of(2) && n >= 2 && child == (n - 2) / 2 {
        child = 2 * (child + 1);
        heap.swap(hole, child - 1);
        hole = child - 1;
    }
    if let Some(slot) = heap.get_mut(hole) {
        *slot = last;
    }
    while hole > 0 {
        let parent = (hole - 1) / 2;
        match heap.get(parent) {
            Some(p) if less(p, &last) => {
                heap.swap(hole, parent);
                hole = parent;
            }
            _ => break,
        }
    }
    Some(top)
}

/// `ankerl::unordered_dense::set`: insertion order, the last element moved into an erased
/// one's place.
#[derive(Default)]
struct DenseSet {
    items: Vec<u32>,
    at: HashMap<u32, usize>,
}

impl DenseSet {
    fn insert(&mut self, v: u32) {
        if self.at.contains_key(&v) {
            return;
        }
        self.at.insert(v, self.items.len());
        self.items.push(v);
    }

    fn remove(&mut self, v: u32) {
        let Some(i) = self.at.remove(&v) else { return };
        let Some(last) = self.items.pop() else { return };
        if last != v
            && let Some(slot) = self.items.get_mut(i)
        {
            *slot = last;
            self.at.insert(last, i);
        }
    }

    fn first(&self) -> Option<u32> {
        self.items.first().copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_heap_pops_the_nearest() {
        let less = |a: &(u32, i64), b: &(u32, i64)| a.1 > b.1;
        let mut heap = Vec::new();
        for (i, d) in [5, 1, 9, 3, 7, 2].iter().enumerate() {
            push_heap(&mut heap, (i as u32, *d), &less);
        }
        let mut out = Vec::new();
        while let Some((_, d)) = pop_heap(&mut heap, &less) {
            out.push(d);
        }
        assert_eq!(out, [1, 2, 3, 5, 7, 9]);
    }

    #[test]
    fn the_dense_set_swaps_the_last_in() {
        let mut s = DenseSet::default();
        for v in [4, 7, 9, 2] {
            s.insert(v);
        }
        s.remove(4);
        assert_eq!(s.first(), Some(2));
        s.remove(2);
        assert_eq!(s.first(), Some(9));
    }
}
