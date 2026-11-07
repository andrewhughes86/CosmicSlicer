//! The half-edge graph Arachne builds from the Voronoi diagram (`SkeletalTrapezoidationGraph`),
//! in arrays. Orca keeps its nodes and edges in `std::list`s, adding at the front and at the
//! back as it goes and walking them in that order; [`Order`] keeps the same order here.

use super::geo;
use crate::clipper::Pt;

pub const NIL: u32 = u32::MAX;

/// A `std::list`'s order over ids: push at either end, unlink, walk.
#[derive(Clone, Debug)]
pub struct Order {
    prev: Vec<u32>,
    next: Vec<u32>,
    live: Vec<bool>,
    head: u32,
    tail: u32,
    len: usize,
}

impl Default for Order {
    fn default() -> Order {
        Order { prev: Vec::new(), next: Vec::new(), live: Vec::new(), head: NIL, tail: NIL, len: 0 }
    }
}

impl Order {
    fn grow(&mut self, id: u32) {
        let need = id as usize + 1;
        if self.prev.len() < need {
            self.prev.resize(need, NIL);
            self.next.resize(need, NIL);
            self.live.resize(need, false);
        }
    }

    pub fn push_back(&mut self, id: u32) {
        self.grow(id);
        let i = id as usize;
        if self.live.get(i).copied().unwrap_or(true) {
            return;
        }
        if let Some(p) = self.prev.get_mut(i) {
            *p = self.tail;
        }
        if let Some(n) = self.next.get_mut(i) {
            *n = NIL;
        }
        match self.next.get_mut(self.tail as usize) {
            Some(n) if self.tail != NIL => *n = id,
            _ => self.head = id,
        }
        self.tail = id;
        if let Some(l) = self.live.get_mut(i) {
            *l = true;
        }
        self.len += 1;
    }

    pub fn push_front(&mut self, id: u32) {
        self.grow(id);
        let i = id as usize;
        if self.live.get(i).copied().unwrap_or(true) {
            return;
        }
        if let Some(n) = self.next.get_mut(i) {
            *n = self.head;
        }
        if let Some(p) = self.prev.get_mut(i) {
            *p = NIL;
        }
        match self.prev.get_mut(self.head as usize) {
            Some(p) if self.head != NIL => *p = id,
            _ => self.tail = id,
        }
        self.head = id;
        if let Some(l) = self.live.get_mut(i) {
            *l = true;
        }
        self.len += 1;
    }

    /// Unlinks `id`; the id after it, as `std::list::erase` returns.
    pub fn remove(&mut self, id: u32) -> u32 {
        let i = id as usize;
        if !self.live.get(i).copied().unwrap_or(false) {
            return NIL;
        }
        let (p, n) = (self.prev.get(i).copied().unwrap_or(NIL), self.next.get(i).copied().unwrap_or(NIL));
        match self.next.get_mut(p as usize) {
            Some(x) if p != NIL => *x = n,
            _ => self.head = n,
        }
        match self.prev.get_mut(n as usize) {
            Some(x) if n != NIL => *x = p,
            _ => self.tail = p,
        }
        if let Some(l) = self.live.get_mut(i) {
            *l = false;
        }
        self.len -= 1;
        n
    }

    pub fn first(&self) -> u32 {
        self.head
    }

    pub fn last(&self) -> u32 {
        self.tail
    }

    pub fn after(&self, id: u32) -> u32 {
        self.next.get(id as usize).copied().unwrap_or(NIL)
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn ids(&self) -> Vec<u32> {
        let mut out = Vec::with_capacity(self.len);
        let mut at = self.head;
        while at != NIL && out.len() <= self.len {
            out.push(at);
            at = self.after(at);
        }
        out
    }

    /// Relinks the live ids in the order given (`std::list::sort`).
    pub fn set(&mut self, ids: &[u32]) {
        for id in self.ids() {
            self.remove(id);
        }
        for &id in ids {
            self.push_back(id);
        }
    }
}

/// A `std::list<T>` of values.
#[derive(Clone, Debug)]
pub struct List<T> {
    pub items: Vec<T>,
    pub order: Order,
}

impl<T> Default for List<T> {
    fn default() -> List<T> {
        List { items: Vec::new(), order: Order::default() }
    }
}

impl<T: Clone> List<T> {
    fn add(&mut self, v: T) -> u32 {
        self.items.push(v);
        (self.items.len() - 1) as u32
    }

    pub fn push_back(&mut self, v: T) {
        let id = self.add(v);
        self.order.push_back(id);
    }

    pub fn push_front(&mut self, v: T) {
        let id = self.add(v);
        self.order.push_front(id);
    }

    pub fn front(&self) -> Option<&T> {
        self.items.get(self.order.first() as usize)
    }

    pub fn back(&self) -> Option<&T> {
        self.items.get(self.order.last() as usize)
    }

    pub fn pop_front(&mut self) {
        self.order.remove(self.order.first());
    }

    pub fn pop_back(&mut self) {
        self.order.remove(self.order.last());
    }

    pub fn erase(&mut self, id: u32) {
        self.order.remove(id);
    }

    pub fn is_empty(&self) -> bool {
        self.order.is_empty()
    }

    pub fn clear(&mut self) {
        self.order.set(&[]);
    }

    pub fn ids(&self) -> Vec<u32> {
        self.order.ids()
    }

    pub fn values(&self) -> Vec<T> {
        self.ids().iter().filter_map(|i| self.items.get(*i as usize).cloned()).collect()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EdgeType {
    Normal,
    ExtraVd,
    TransitionEnd,
}

#[derive(Clone, Copy, Debug)]
pub struct EdgeData {
    pub kind: EdgeType,
    /// -1 unknown, 0 no, 1 yes.
    central: i8,
    pub transitions: u32,
    pub ends: u32,
    pub junctions: u32,
}

impl EdgeData {
    pub fn new(kind: EdgeType) -> EdgeData {
        EdgeData { kind, central: -1, transitions: NIL, ends: NIL, junctions: NIL }
    }

    pub fn is_central(&self) -> bool {
        self.central == 1
    }

    pub fn central_is_set(&self) -> bool {
        self.central >= 0
    }

    pub fn set_central(&mut self, b: bool) {
        self.central = i8::from(b);
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Edge {
    pub data: EdgeData,
    pub twin: u32,
    pub next: u32,
    pub prev: u32,
    pub from: u32,
    pub to: u32,
}

#[derive(Clone, Copy, Debug)]
pub struct Joint {
    pub r: i64,
    pub beads: i64,
    pub ratio: f32,
    pub beading: u32,
}

impl Default for Joint {
    fn default() -> Joint {
        Joint { r: -1, beads: -1, ratio: 0.0, beading: NIL }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Node {
    pub data: Joint,
    pub p: Pt,
    pub incident: u32,
}

/// Most steps a walk round a node or along a fan takes: a broken graph must not spin.
pub const WALK: usize = 100_000;

#[derive(Clone, Debug, Default)]
pub struct Graph {
    pub edges: Vec<Edge>,
    pub nodes: Vec<Node>,
    pub edge_order: Order,
    pub node_order: Order,
}

impl Graph {
    pub fn e(&self, e: u32) -> Edge {
        self.edges.get(e as usize).copied().unwrap_or(Edge {
            data: EdgeData::new(EdgeType::Normal),
            twin: NIL,
            next: NIL,
            prev: NIL,
            from: NIL,
            to: NIL,
        })
    }

    pub fn em(&mut self, e: u32) -> Option<&mut Edge> {
        self.edges.get_mut(e as usize)
    }

    pub fn n(&self, n: u32) -> Node {
        self.nodes.get(n as usize).copied().unwrap_or(Node { data: Joint::default(), p: Pt::default(), incident: NIL })
    }

    pub fn nm(&mut self, n: u32) -> Option<&mut Node> {
        self.nodes.get_mut(n as usize)
    }

    pub fn twin(&self, e: u32) -> u32 {
        self.e(e).twin
    }
    pub fn next(&self, e: u32) -> u32 {
        self.e(e).next
    }
    pub fn prev(&self, e: u32) -> u32 {
        self.e(e).prev
    }
    pub fn from(&self, e: u32) -> u32 {
        self.e(e).from
    }
    pub fn to(&self, e: u32) -> u32 {
        self.e(e).to
    }
    pub fn r(&self, n: u32) -> i64 {
        self.n(n).data.r
    }
    pub fn beads(&self, n: u32) -> i64 {
        self.n(n).data.beads
    }
    pub fn p(&self, n: u32) -> Pt {
        self.n(n).p
    }
    pub fn central(&self, e: u32) -> bool {
        self.e(e).data.is_central()
    }
    pub fn length(&self, e: u32) -> i64 {
        geo::norm(geo::sub(self.p(self.to(e)), self.p(self.from(e))))
    }

    pub fn new_node(&mut self, p: Pt, front: bool) -> u32 {
        self.nodes.push(Node { data: Joint::default(), p, incident: NIL });
        let id = (self.nodes.len() - 1) as u32;
        if front {
            self.node_order.push_front(id);
        } else {
            self.node_order.push_back(id);
        }
        id
    }

    pub fn new_edge(&mut self, kind: EdgeType, front: bool) -> u32 {
        self.edges.push(Edge { data: EdgeData::new(kind), twin: NIL, next: NIL, prev: NIL, from: NIL, to: NIL });
        let id = (self.edges.len() - 1) as u32;
        if front {
            self.edge_order.push_front(id);
        } else {
            self.edge_order.push_back(id);
        }
        id
    }

    pub fn set_r(&mut self, n: u32, r: i64) {
        if let Some(node) = self.nm(n) {
            node.data.r = r;
        }
    }

    pub fn set_beads(&mut self, n: u32, b: i64) {
        if let Some(node) = self.nm(n) {
            node.data.beads = b;
        }
    }

    pub fn set_ratio(&mut self, n: u32, ratio: f32) {
        if let Some(node) = self.nm(n) {
            node.data.ratio = ratio;
        }
    }

    pub fn set_central(&mut self, e: u32, b: bool) {
        if let Some(edge) = self.em(e) {
            edge.data.set_central(b);
        }
    }

    /// The edges leaving `e`'s end other than its twin: `for (x = e->next; x && x != e->twin;
    /// x = x->twin->next)`.
    pub fn fan(&self, e: u32) -> Vec<u32> {
        let mut out = Vec::new();
        let twin = self.twin(e);
        let mut x = self.next(e);
        while x != NIL && x != twin && out.len() < WALK {
            out.push(x);
            x = self.next(self.twin(x));
        }
        out
    }

    /// The edges leaving node `n`: `do { … } while (x = x->twin->next, x != incident)`, with
    /// Orca's early stop where it meets an edge without a successor (`None` then).
    pub fn around(&self, n: u32) -> Vec<u32> {
        let start = self.n(n).incident;
        let mut out = Vec::new();
        let mut x = start;
        loop {
            if x == NIL {
                break;
            }
            out.push(x);
            x = self.next(self.twin(x));
            if x == start || out.len() >= WALK {
                break;
            }
        }
        out
    }

    /// `STHalfEdge::canGoUp`.
    pub fn can_go_up(&self, e: u32, strict: bool, depth: usize) -> bool {
        let (rf, rt) = (self.r(self.from(e)), self.r(self.to(e)));
        if rt > rf {
            return true;
        }
        if rt < rf || strict || depth > 1000 {
            return false;
        }
        let twin = self.twin(e);
        let mut x = self.next(e);
        let mut steps = 0;
        while x != twin && steps < WALK {
            if x == NIL {
                return false;
            }
            if self.can_go_up(x, false, depth + 1) {
                return true;
            }
            let t = self.twin(x);
            if t == NIL {
                return false;
            }
            if self.next(t) == NIL {
                return true;
            }
            x = self.next(t);
            steps += 1;
        }
        false
    }

    /// `STHalfEdge::distToGoUp`.
    pub fn dist_to_go_up(&self, e: u32, depth: usize) -> Option<i64> {
        let (rf, rt) = (self.r(self.from(e)), self.r(self.to(e)));
        if rt > rf {
            return Some(0);
        }
        if rt < rf || depth > 1000 {
            return None;
        }
        let mut ret: Option<i64> = None;
        let twin = self.twin(e);
        let mut x = self.next(e);
        let mut steps = 0;
        while x != twin && steps < WALK {
            if x == NIL {
                return None;
            }
            if let Some(d) = self.dist_to_go_up(x, depth + 1) {
                ret = Some(ret.map_or(d, |r| r.min(d)));
            }
            let t = self.twin(x);
            if t == NIL {
                return None;
            }
            if self.next(t) == NIL {
                return Some(0);
            }
            x = self.next(t);
            steps += 1;
        }
        ret.map(|r| r + self.length(e))
    }

    /// `STHalfEdge::isUpward`.
    pub fn is_upward(&self, e: u32) -> bool {
        let (rf, rt) = (self.r(self.from(e)), self.r(self.to(e)));
        if rt > rf {
            return true;
        }
        if rt < rf {
            return false;
        }
        match (self.dist_to_go_up(e, 0), self.dist_to_go_up(self.twin(e), 0)) {
            (Some(f), Some(b)) => f < b,
            (Some(_), None) => true,
            (None, Some(_)) => false,
            (None, None) => geo::less(self.p(self.to(e)), self.p(self.from(e))),
        }
    }

    /// `STHalfEdge::getNextUnconnected`.
    pub fn next_unconnected(&self, e: u32) -> u32 {
        let mut result = e;
        let mut steps = 0;
        while self.next(result) != NIL {
            result = self.next(result);
            if result == e || steps > WALK {
                return NIL;
            }
            steps += 1;
        }
        self.twin(result)
    }

    /// `STHalfEdgeNode::isMultiIntersection`.
    pub fn is_multi_intersection(&self, n: u32) -> bool {
        let start = self.n(n).incident;
        let mut x = start;
        let mut count = 0;
        let mut steps = 0;
        loop {
            if x == NIL {
                return false;
            }
            if self.central(x) {
                count += 1;
            }
            x = self.next(self.twin(x));
            steps += 1;
            if x == start || steps > WALK {
                break;
            }
        }
        count > 2
    }

    /// `STHalfEdgeNode::isCentral`.
    pub fn node_is_central(&self, n: u32) -> bool {
        let start = self.n(n).incident;
        let mut x = start;
        let mut steps = 0;
        loop {
            if x == NIL {
                return false;
            }
            if self.central(x) {
                return true;
            }
            if self.twin(x) == NIL {
                return false;
            }
            x = self.next(self.twin(x));
            steps += 1;
            if x == start || steps > WALK {
                return false;
            }
        }
    }

    /// `STHalfEdgeNode::isLocalMaximum`.
    pub fn is_local_maximum(&self, n: u32, strict: bool) -> bool {
        if self.r(n) == 0 {
            return false;
        }
        let start = self.n(n).incident;
        let mut x = start;
        let mut steps = 0;
        loop {
            if x == NIL {
                return false;
            }
            if self.can_go_up(x, strict, 0) {
                return false;
            }
            let t = self.twin(x);
            if t == NIL || self.next(t) == NIL {
                return false;
            }
            x = self.next(t);
            steps += 1;
            if x == start || steps > WALK {
                return true;
            }
        }
    }

    /// `SkeletalTrapezoidationGraph::collapseSmallEdges`.
    pub fn collapse_small_edges(&mut self, snap: i64) {
        let close = |g: &Graph, a: u32, b: u32| geo::shorter_then(geo::sub(g.p(a), g.p(b)), snap);
        let mut it = self.edge_order.first();
        let mut guard = 0;
        while it != NIL && guard < self.edges.len() * 4 + 16 {
            guard += 1;
            if self.prev(it) != NIL {
                it = self.edge_order.after(it);
                continue;
            }
            let quad_start = it;
            let mut quad_end = quad_start;
            let mut steps = 0;
            while self.next(quad_end) != NIL && steps < WALK {
                quad_end = self.next(quad_end);
                steps += 1;
            }
            let quad_mid = if self.next(quad_start) == quad_end { NIL } else { self.next(quad_start) };
            let mut updated = false;
            let remove = |g: &mut Graph, e: u32, it: &mut u32, updated: &mut bool| {
                if e == *it {
                    *it = g.edge_order.remove(e);
                    *updated = true;
                } else {
                    g.edge_order.remove(e);
                }
            };
            if quad_mid != NIL && close(self, self.from(quad_mid), self.to(quad_mid)) {
                let mid_twin = self.twin(quad_mid);
                if mid_twin == NIL {
                    // Orca `continue`s here without moving on, which would spin.
                    it = self.edge_order.after(it);
                    continue;
                }
                let keep = self.from(quad_mid);
                let mut x = quad_end;
                let mut count = 0;
                while x != NIL && x != mid_twin {
                    if let Some(edge) = self.em(x) {
                        edge.from = keep;
                    }
                    let t = self.twin(x);
                    if let Some(edge) = self.em(t) {
                        edge.to = keep;
                    }
                    count += 1;
                    if count > 1000 {
                        break;
                    }
                    x = self.next(t);
                }
                if self.n(keep).incident == quad_mid {
                    let replacement =
                        if self.next(mid_twin) != NIL { self.next(mid_twin) } else { self.twin(self.prev(quad_mid)) };
                    if let Some(node) = self.nm(keep) {
                        node.incident = replacement;
                    }
                }
                self.node_order.remove(self.to(quad_mid));
                let (mp, mn) = (self.prev(quad_mid), self.next(quad_mid));
                if let Some(e) = self.em(mp) {
                    e.next = mn;
                }
                if let Some(e) = self.em(mn) {
                    e.prev = mp;
                }
                let (tp, tn) = (self.prev(mid_twin), self.next(mid_twin));
                if let Some(e) = self.em(tn) {
                    e.prev = tp;
                }
                if let Some(e) = self.em(tp) {
                    e.next = tn;
                }
                remove(self, mid_twin, &mut it, &mut updated);
                remove(self, quad_mid, &mut it, &mut updated);
            }
            if close(self, self.from(quad_start), self.to(quad_end))
                && close(self, self.to(quad_start), self.from(quad_end))
            {
                let (start_twin, end_twin) = (self.twin(quad_start), self.twin(quad_end));
                let end_to = self.to(quad_end);
                if let Some(e) = self.em(start_twin) {
                    e.to = end_to;
                }
                if let Some(n) = self.nm(end_to) {
                    n.incident = end_twin;
                }
                let end_from = self.from(quad_end);
                if self.n(end_from).incident == quad_end {
                    let replacement =
                        if self.next(end_twin) != NIL { self.next(end_twin) } else { self.twin(self.prev(quad_end)) };
                    if let Some(n) = self.nm(end_from) {
                        n.incident = replacement;
                    }
                }
                self.node_order.remove(self.from(quad_start));
                if let Some(e) = self.em(start_twin) {
                    e.twin = end_twin;
                }
                if let Some(e) = self.em(end_twin) {
                    e.twin = start_twin;
                }
                remove(self, quad_start, &mut it, &mut updated);
                remove(self, quad_end, &mut it, &mut updated);
            }
            if !updated {
                it = self.edge_order.after(it);
            }
        }
    }

    /// `makeRib`: from the end of `prev` straight to the source line, and back.
    pub fn make_rib(&mut self, prev: &mut u32, start: Pt, end: Pt) {
        let at = self.to(*prev);
        let here = self.p(at);
        let (_, foot) = geo::infinite_sq(here, start, end);
        self.set_r(at, geo::norm(geo::sub(here, foot)));
        let node = self.new_node(foot, true);
        self.set_r(node, 0);
        let forth = self.new_edge(EdgeType::ExtraVd, true);
        let back = self.new_edge(EdgeType::ExtraVd, true);
        if let Some(e) = self.em(*prev) {
            e.next = forth;
        }
        if let Some(e) = self.em(forth) {
            e.prev = *prev;
            e.from = at;
            e.to = node;
            e.twin = back;
        }
        if let Some(e) = self.em(back) {
            e.twin = forth;
            e.from = node;
            e.to = at;
        }
        if let Some(n) = self.nm(node) {
            n.incident = back;
        }
        *prev = back;
    }

    /// `getSource`: the segment from the start of `e`'s cell to its end.
    fn source(&self, e: u32) -> (Pt, Pt) {
        let mut a = e;
        let mut steps = 0;
        while self.prev(a) != NIL && steps < WALK {
            a = self.prev(a);
            steps += 1;
        }
        let mut b = e;
        steps = 0;
        while self.next(b) != NIL && steps < WALK {
            b = self.next(b);
            steps += 1;
        }
        (self.p(self.from(a)), self.p(self.to(b)))
    }

    /// `insertRib`.
    fn insert_rib(&mut self, edge: u32, mid: u32) -> (u32, u32) {
        let (before, after) = (self.prev(edge), self.next(edge));
        let (node_before, node_after) = (self.from(edge), self.to(edge));
        let p = self.p(mid);
        let (a, b) = self.source(edge);
        let (_, px) = geo::segment_sq(p, a, b);
        let dist = geo::norm(geo::sub(p, px));
        self.set_r(mid, dist);
        self.set_ratio(mid, 0.0);
        let source = self.new_node(px, false);
        self.set_r(source, 0);
        let first = edge;
        let second = self.new_edge(EdgeType::Normal, false);
        let outward = self.new_edge(EdgeType::TransitionEnd, false);
        let inward = self.new_edge(EdgeType::TransitionEnd, false);
        if let Some(e) = self.em(before) {
            e.next = first;
        }
        if let Some(e) = self.em(first) {
            e.next = outward;
            e.prev = before;
            e.to = mid;
            e.from = node_before;
            e.data.set_central(true);
            e.twin = NIL;
        }
        if let Some(e) = self.em(outward) {
            e.next = NIL;
            e.prev = first;
            e.to = source;
            e.from = mid;
            e.data.set_central(false);
            e.twin = inward;
        }
        if let Some(e) = self.em(inward) {
            e.next = second;
            e.prev = NIL;
            e.to = mid;
            e.from = source;
            e.data.set_central(false);
            e.twin = outward;
        }
        if let Some(e) = self.em(second) {
            e.next = after;
            e.prev = inward;
            e.to = node_after;
            e.from = mid;
            e.data.set_central(true);
            e.twin = NIL;
        }
        if let Some(e) = self.em(after) {
            e.prev = second;
        }
        if let Some(n) = self.nm(node_before) {
            n.incident = first;
        }
        if let Some(n) = self.nm(mid) {
            n.incident = outward;
        }
        if let Some(n) = self.nm(source) {
            n.incident = inward;
        }
        if after != NIL
            && let Some(n) = self.nm(node_after)
        {
            n.incident = after;
        }
        (first, second)
    }

    /// `insertNode`: splits `edge` and its twin at `mid` with ribs to the source.
    pub fn insert_node(&mut self, edge: u32, mid: Pt, beads: i64) -> u32 {
        let node = self.new_node(mid, false);
        let twin = self.twin(edge);
        if let Some(e) = self.em(edge) {
            e.twin = NIL;
        }
        if let Some(e) = self.em(twin) {
            e.twin = NIL;
        }
        let (first_input, last_input) = self.insert_rib(edge, node);
        let (first_twin, last_twin) = self.insert_rib(twin, node);
        if let Some(e) = self.em(first_input) {
            e.twin = last_twin;
        }
        if let Some(e) = self.em(last_twin) {
            e.twin = first_input;
        }
        if let Some(e) = self.em(last_input) {
            e.twin = first_twin;
        }
        if let Some(e) = self.em(first_twin) {
            e.twin = last_input;
        }
        self.set_beads(node, beads);
        last_input
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_order_is_a_list() {
        let mut o = Order::default();
        o.push_back(0);
        o.push_front(1);
        o.push_back(2);
        o.push_front(3);
        assert_eq!(o.ids(), [3, 1, 0, 2]);
        assert_eq!(o.remove(1), 0);
        assert_eq!(o.ids(), [3, 0, 2]);
        o.remove(3);
        o.remove(2);
        assert_eq!(o.ids(), [0]);
        assert_eq!((o.first(), o.last()), (0, 0));
        o.remove(0);
        assert!(o.is_empty());
        assert_eq!(o.remove(0), NIL);
    }

    #[test]
    fn lists_pop_and_sort() {
        let mut l: List<i32> = List::default();
        for v in [5, 1, 4] {
            l.push_back(v);
        }
        l.push_front(9);
        assert_eq!(l.values(), [9, 5, 1, 4]);
        l.pop_front();
        l.pop_back();
        assert_eq!(l.values(), [5, 1]);
        let mut ids = l.ids();
        ids.sort_by_key(|i| l.items.get(*i as usize).copied().unwrap_or(0));
        l.order.set(&ids);
        assert_eq!(l.values(), [1, 5]);
    }
}
