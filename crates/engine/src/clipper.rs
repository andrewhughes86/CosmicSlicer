//! Clipper 6.2.6 as OrcaSlicer carries it (`deps_src/clipper/clipper.cpp`): the polygon boolean
//! and offset engine under every region in a slice. It is a line-by-line port, down to the
//! floating-point `Round`, the unstable sorts and the order points join an output loop, because
//! which vertex a loop starts on and which loop comes first reach the G-code. Keep it so.
//!
//! Orca's pointers are indices here: `edges`, `pts` and `recs` are arenas and [`NIL`] is its null.
//! Its exceptions are `Err(Failed)`. All coordinates are 64-bit (1 unit = 1 nm in the engine), and
//! `use_lines` is on, so open paths work, into a [`PolyTree`] only.

// A port keeps its source's branches, so that the two can be read side by side.
#![allow(clippy::if_same_then_else, clippy::needless_late_init, clippy::collapsible_else_if, clippy::manual_clamp)]

use crate::connect::std_sort;

const PI: f64 = std::f64::consts::PI;
const TWO_PI: f64 = PI * 2.0;
const DEF_ARC_TOLERANCE: f64 = 0.25;
const HORIZONTAL: f64 = -1.0e40;
const TOLERANCE: f64 = 1.0e-20;
const UNASSIGNED: i32 = -1;
const SKIP: i32 = -2;
const LO_RANGE: i64 = 0x3FFF_FFFF;
const HI_RANGE: i64 = 0x3FFF_FFFF_FFFF_FFFF;
/// The null pointer of the arenas.
const NIL: usize = usize::MAX;

/// Clipper gave up (a coordinate out of range or a sweep it could not finish).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Failed;

/// A point in integer units.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Pt {
    pub x: i64,
    pub y: i64,
}

impl Pt {
    pub const fn new(x: i64, y: i64) -> Self {
        Self { x, y }
    }
}

pub type Path = Vec<Pt>;
pub type Paths = Vec<Path>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClipType {
    Intersection,
    Union,
    Difference,
    Xor,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PolyType {
    Subject,
    Clip,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FillType {
    EvenOdd,
    NonZero,
    Positive,
    Negative,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JoinType {
    Square,
    Round,
    Miter,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EndType {
    ClosedPolygon,
    ClosedLine,
    OpenButt,
    OpenSquare,
    OpenRound,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Side {
    Left,
    Right,
}

/// A node of a [`PolyTree`]: its contour and children, as indices into `nodes`.
#[derive(Clone, Debug, Default)]
pub struct PolyNode {
    pub contour: Path,
    pub children: Vec<usize>,
    pub open: bool,
}

/// Clipper's output as a tree of outer loops and holes.
#[derive(Clone, Debug, Default)]
pub struct PolyTree {
    pub nodes: Vec<PolyNode>,
    /// The top-level nodes.
    pub children: Vec<usize>,
}

impl PolyTree {
    /// Removes the rectangle a negative offset wraps around its result.
    pub fn remove_outermost(&mut self) {
        let only = match self.children.as_slice() {
            [one] => self.nodes.get(*one).filter(|n| !n.children.is_empty()),
            _ => None,
        };
        match only {
            Some(outer) => self.children = outer.children.clone(),
            None => *self = Self::default(),
        }
    }
}

#[inline]
fn fround(a: f64) -> f64 {
    if a == 0.49999999999999994 { 0.0 } else { (a + 0.5).floor() }
}

#[inline]
fn round(v: f64) -> i64 {
    fround(v) as i64
}

#[inline]
fn near_zero(v: f64) -> bool {
    v > -TOLERANCE && v < TOLERANCE
}

/// Signed area of a path: positive when counter-clockwise.
pub fn area(poly: &[Pt]) -> f64 {
    let size = poly.len();
    if size < 3 {
        return 0.0;
    }
    let mut a = 0.0;
    let mut j = size - 1;
    for i in 0..size {
        a += (poly[j].x as f64 + poly[i].x as f64) * (poly[j].y as f64 - poly[i].y as f64);
        j = i;
    }
    -a * 0.5
}

/// Whether the path runs counter-clockwise.
pub fn orientation(poly: &[Pt]) -> bool {
    area(poly) >= 0.0
}

/// 0 outside, 1 inside, -1 on the boundary.
pub fn point_in_polygon(pt: Pt, path: &[Pt]) -> i32 {
    let mut result = 0;
    let cnt = path.len();
    if cnt < 3 {
        return 0;
    }
    let mut ip = path[0];
    for i in 1..=cnt {
        let ip_next = if i == cnt { path[0] } else { path[i] };
        if ip_next.y == pt.y && (ip_next.x == pt.x || (ip.y == pt.y && ((ip_next.x > pt.x) == (ip.x < pt.x)))) {
            return -1;
        }
        if (ip.y < pt.y) != (ip_next.y < pt.y) {
            let d =
                || (ip.x - pt.x) as f64 * (ip_next.y - pt.y) as f64 - (ip_next.x - pt.x) as f64 * (ip.y - pt.y) as f64;
            if ip.x >= pt.x {
                if ip_next.x > pt.x {
                    result = 1 - result;
                } else {
                    let d = d();
                    if d == 0.0 {
                        return -1;
                    }
                    if (d > 0.0) == (ip_next.y > ip.y) {
                        result = 1 - result;
                    }
                }
            } else if ip_next.x > pt.x {
                let d = d();
                if d == 0.0 {
                    return -1;
                }
                if (d > 0.0) == (ip_next.y > ip.y) {
                    result = 1 - result;
                }
            }
        }
        ip = ip_next;
    }
    result
}

#[inline]
fn slopes_equal(dx1: i64, dy1: i64, dx2: i64, dy2: i64, full: bool) -> bool {
    if full {
        i128::from(dy1) * i128::from(dx2) == i128::from(dx1) * i128::from(dy2)
    } else {
        dy1.wrapping_mul(dx2) == dx1.wrapping_mul(dy2)
    }
}

#[inline]
fn slopes3(a: Pt, b: Pt, c: Pt, full: bool) -> bool {
    slopes_equal(a.x - b.x, a.y - b.y, b.x - c.x, b.y - c.y, full)
}

#[inline]
fn slopes4(a: Pt, b: Pt, c: Pt, d: Pt, full: bool) -> bool {
    slopes_equal(a.x - b.x, a.y - b.y, c.x - d.x, c.y - d.y, full)
}

#[inline]
fn get_dx(a: Pt, b: Pt) -> f64 {
    if a.y == b.y { HORIZONTAL } else { (b.x - a.x) as f64 / (b.y - a.y) as f64 }
}

fn pt2_between(a: Pt, b: Pt, c: Pt) -> bool {
    if a == c || a == b || c == b {
        false
    } else if a.x != c.x {
        (b.x > a.x) == (b.x < c.x)
    } else {
        (b.y > a.y) == (b.y < c.y)
    }
}

fn horz_overlap(mut a: i64, mut b: i64, mut c: i64, mut d: i64) -> bool {
    if a > b {
        std::mem::swap(&mut a, &mut b);
    }
    if c > d {
        std::mem::swap(&mut c, &mut d);
    }
    a < d && c < b
}

fn range_test(p: Pt, full: &mut bool) -> Result<(), Failed> {
    let over = |limit: i64| p.x > limit || p.y > limit || -p.x > limit || -p.y > limit;
    if *full {
        if over(HI_RANGE) {
            return Err(Failed);
        }
    } else if over(LO_RANGE) {
        *full = true;
        return range_test(p, full);
    }
    Ok(())
}

#[derive(Clone, Copy)]
struct Edge {
    bot: Pt,
    curr: Pt,
    top: Pt,
    dx: f64,
    poly: PolyType,
    side: Side,
    wind_delta: i32,
    wind_cnt: i32,
    wind_cnt2: i32,
    out_idx: i32,
    next: usize,
    prev: usize,
    next_lml: usize,
    next_ael: usize,
    prev_ael: usize,
    next_sel: usize,
    prev_sel: usize,
}

const BLANK: Edge = Edge {
    bot: Pt::new(0, 0),
    curr: Pt::new(0, 0),
    top: Pt::new(0, 0),
    dx: 0.0,
    poly: PolyType::Subject,
    side: Side::Left,
    wind_delta: 0,
    wind_cnt: 0,
    wind_cnt2: 0,
    out_idx: 0,
    next: NIL,
    prev: NIL,
    next_lml: NIL,
    next_ael: NIL,
    prev_ael: NIL,
    next_sel: NIL,
    prev_sel: NIL,
};

#[derive(Clone, Copy)]
struct OutPt {
    idx: i32,
    pt: Pt,
    next: usize,
    prev: usize,
}

#[derive(Clone, Copy)]
struct OutRec {
    idx: i32,
    is_hole: bool,
    is_open: bool,
    first_left: usize,
    node: usize,
    pts: usize,
    bottom: usize,
}

#[derive(Clone, Copy)]
struct Minimum {
    y: i64,
    left: usize,
    right: usize,
}

#[derive(Clone, Copy)]
struct Join {
    out1: usize,
    out2: usize,
    off: Pt,
}

#[derive(Clone, Copy)]
struct Intersect {
    e1: usize,
    e2: usize,
    pt: Pt,
}

/// The rectangle around the local minima.
#[derive(Clone, Copy, Debug, Default)]
pub struct Rect {
    pub left: i64,
    pub top: i64,
    pub right: i64,
    pub bottom: i64,
}

/// The sweep. Add paths, then run one of the `execute` calls once.
pub struct Clipper {
    edges: Vec<Edge>,
    minima: Vec<Minimum>,
    full: bool,
    has_open: bool,
    preserve_collinear: bool,
    reverse_output: bool,
    strict_simple: bool,
    using_tree: bool,
    pts: Vec<OutPt>,
    recs: Vec<OutRec>,
    joins: Vec<Join>,
    ghosts: Vec<Join>,
    intersects: Vec<Intersect>,
    clip_type: ClipType,
    scanbeam: std::collections::BinaryHeap<i64>,
    maxima: Vec<i64>,
    active: usize,
    sorted: usize,
    subj_fill: FillType,
    clip_fill: FillType,
}

impl Default for Clipper {
    fn default() -> Self {
        Self::new()
    }
}

impl Clipper {
    pub fn new() -> Self {
        Self {
            edges: Vec::new(),
            minima: Vec::new(),
            full: false,
            has_open: false,
            preserve_collinear: false,
            reverse_output: false,
            strict_simple: false,
            using_tree: false,
            pts: Vec::new(),
            recs: Vec::new(),
            joins: Vec::new(),
            ghosts: Vec::new(),
            intersects: Vec::new(),
            clip_type: ClipType::Union,
            scanbeam: std::collections::BinaryHeap::new(),
            maxima: Vec::new(),
            active: NIL,
            sorted: NIL,
            subj_fill: FillType::EvenOdd,
            clip_fill: FillType::EvenOdd,
        }
    }

    /// Output loops wind the other way round.
    pub fn reverse_solution(&mut self, on: bool) {
        self.reverse_output = on;
    }

    /// Splits loops at touching vertices (expensive).
    pub fn strictly_simple(&mut self, on: bool) {
        self.strict_simple = on;
    }

    // ---- building the local minima ------------------------------------------------------

    #[inline]
    fn is_horizontal(&self, e: usize) -> bool {
        self.edges[e].dx == HORIZONTAL
    }

    fn init_edge(&mut self, e: usize, next: usize, prev: usize, pt: Pt) {
        self.edges[e] = Edge { next, prev, curr: pt, out_idx: UNASSIGNED, ..BLANK };
    }

    fn init_edge2(&mut self, e: usize, poly: PolyType) {
        let next = self.edges[self.edges[e].next].curr;
        let ed = &mut self.edges[e];
        if ed.curr.y >= next.y {
            ed.bot = ed.curr;
            ed.top = next;
        } else {
            ed.top = ed.curr;
            ed.bot = next;
        }
        let dy = ed.top.y - ed.bot.y;
        ed.dx = if dy == 0 { HORIZONTAL } else { (ed.top.x - ed.bot.x) as f64 / dy as f64 };
        ed.poly = poly;
    }

    fn remove_edge(&mut self, e: usize) -> usize {
        let (prev, next) = (self.edges[e].prev, self.edges[e].next);
        self.edges[prev].next = next;
        self.edges[next].prev = prev;
        self.edges[e].prev = NIL;
        next
    }

    fn reverse_horizontal(&mut self, e: usize) {
        let ed = &mut self.edges[e];
        std::mem::swap(&mut ed.top.x, &mut ed.bot.x);
    }

    fn find_next_loc_min(&self, mut e: usize) -> usize {
        loop {
            while self.edges[e].bot != self.edges[self.edges[e].prev].bot || self.edges[e].curr == self.edges[e].top {
                e = self.edges[e].next;
            }
            if !self.is_horizontal(e) && !self.is_horizontal(self.edges[e].prev) {
                break;
            }
            while self.is_horizontal(self.edges[e].prev) {
                e = self.edges[e].prev;
            }
            let e2 = e;
            while self.is_horizontal(e) {
                e = self.edges[e].next;
            }
            if self.edges[e].top.y == self.edges[self.edges[e].prev].bot.y {
                continue;
            }
            if self.edges[self.edges[e2].prev].bot.x < self.edges[e].bot.x {
                e = e2;
            }
            break;
        }
        e
    }

    fn process_bound(&mut self, mut e: usize, next_is_forward: bool) -> usize {
        let mut result = e;
        if self.edges[e].out_idx == SKIP {
            if next_is_forward {
                while self.edges[e].top.y == self.edges[self.edges[e].next].bot.y {
                    e = self.edges[e].next;
                }
                while e != result && self.is_horizontal(e) {
                    e = self.edges[e].prev;
                }
            } else {
                while self.edges[e].top.y == self.edges[self.edges[e].prev].bot.y {
                    e = self.edges[e].prev;
                }
                while e != result && self.is_horizontal(e) {
                    e = self.edges[e].next;
                }
            }
            if e == result {
                result = if next_is_forward { self.edges[e].next } else { self.edges[e].prev };
            } else {
                e = if next_is_forward { self.edges[result].next } else { self.edges[result].prev };
                let min = Minimum { y: self.edges[e].bot.y, left: NIL, right: e };
                self.edges[e].wind_delta = 0;
                result = self.process_bound(e, next_is_forward);
                self.minima.push(min);
            }
            return result;
        }

        if self.is_horizontal(e) {
            let start = if next_is_forward { self.edges[e].prev } else { self.edges[e].next };
            if self.is_horizontal(start) {
                if self.edges[start].bot.x != self.edges[e].bot.x && self.edges[start].top.x != self.edges[e].bot.x {
                    self.reverse_horizontal(e);
                }
            } else if self.edges[start].bot.x != self.edges[e].bot.x {
                self.reverse_horizontal(e);
            }
        }

        let start = e;
        if next_is_forward {
            while self.edges[result].top.y == self.edges[self.edges[result].next].bot.y
                && self.edges[self.edges[result].next].out_idx != SKIP
            {
                result = self.edges[result].next;
            }
            if self.is_horizontal(result) && self.edges[self.edges[result].next].out_idx != SKIP {
                let mut horz = result;
                while self.is_horizontal(self.edges[horz].prev) {
                    horz = self.edges[horz].prev;
                }
                if self.edges[self.edges[horz].prev].top.x > self.edges[self.edges[result].next].top.x {
                    result = self.edges[horz].prev;
                }
            }
            while e != result {
                self.edges[e].next_lml = self.edges[e].next;
                if self.is_horizontal(e) && e != start && self.edges[e].bot.x != self.edges[self.edges[e].prev].top.x {
                    self.reverse_horizontal(e);
                }
                e = self.edges[e].next;
            }
            if self.is_horizontal(e) && e != start && self.edges[e].bot.x != self.edges[self.edges[e].prev].top.x {
                self.reverse_horizontal(e);
            }
            result = self.edges[result].next;
        } else {
            while self.edges[result].top.y == self.edges[self.edges[result].prev].bot.y
                && self.edges[self.edges[result].prev].out_idx != SKIP
            {
                result = self.edges[result].prev;
            }
            if self.is_horizontal(result) && self.edges[self.edges[result].prev].out_idx != SKIP {
                let mut horz = result;
                while self.is_horizontal(self.edges[horz].next) {
                    horz = self.edges[horz].next;
                }
                if self.edges[self.edges[horz].next].top.x >= self.edges[self.edges[result].prev].top.x {
                    result = self.edges[horz].next;
                }
            }
            while e != result {
                self.edges[e].next_lml = self.edges[e].prev;
                if self.is_horizontal(e) && e != start && self.edges[e].bot.x != self.edges[self.edges[e].next].top.x {
                    self.reverse_horizontal(e);
                }
                e = self.edges[e].prev;
            }
            if self.is_horizontal(e) && e != start && self.edges[e].bot.x != self.edges[self.edges[e].next].top.x {
                self.reverse_horizontal(e);
            }
            result = self.edges[result].prev;
        }
        result
    }

    /// Adds a path; `false` when it has no area or length left to add.
    pub fn add_path(&mut self, pg: &[Pt], poly: PolyType, closed: bool) -> Result<bool, Failed> {
        if pg.is_empty() {
            return Ok(false);
        }
        let mut high = pg.len() - 1;
        if closed {
            while high > 0 && pg[high] == pg[0] {
                high -= 1;
            }
        }
        while high > 0 && pg[high] == pg[high - 1] {
            high -= 1;
        }
        if (closed && high < 2) || (!closed && high < 1) {
            return Ok(false);
        }
        if !closed && poly == PolyType::Clip {
            return Err(Failed);
        }
        let base = self.edges.len();
        self.edges.resize(base + high + 1, BLANK);
        let added = self.add_internal(pg, high, poly, closed, base);
        if !matches!(added, Ok(true)) {
            self.edges.truncate(base);
        }
        added
    }

    fn add_internal(&mut self, pg: &[Pt], high: usize, poly: PolyType, closed: bool, b: usize) -> Result<bool, Failed> {
        // 1. Basic (first) edge initialization.
        let mut full = self.full;
        range_test(pg[0], &mut full)?;
        range_test(pg[high], &mut full)?;
        self.init_edge(b, b + 1, b + high, pg[0]);
        self.init_edge(b + high, b, b + high - 1, pg[high]);
        for i in (1..high).rev() {
            range_test(pg[i], &mut full)?;
            self.init_edge(b + i, b + i + 1, b + i - 1, pg[i]);
        }
        self.full = full;
        let mut e_start = b;

        // 2. Remove duplicate vertices, and (when closed) collinear edges.
        let mut e = e_start;
        let mut stop = e_start;
        loop {
            let (next, prev) = (self.edges[e].next, self.edges[e].prev);
            if self.edges[e].curr == self.edges[next].curr && (closed || next != e_start) {
                if e == next {
                    break;
                }
                if e == e_start {
                    e_start = next;
                }
                e = self.remove_edge(e);
                stop = e;
                continue;
            }
            if prev == next {
                break;
            } else if closed
                && slopes3(self.edges[prev].curr, self.edges[e].curr, self.edges[next].curr, self.full)
                && (!self.preserve_collinear
                    || !pt2_between(self.edges[prev].curr, self.edges[e].curr, self.edges[next].curr))
            {
                if e == e_start {
                    e_start = next;
                }
                e = self.remove_edge(e);
                e = self.edges[e].prev;
                stop = e;
                continue;
            }
            e = self.edges[e].next;
            if e == stop || (!closed && self.edges[e].next == e_start) {
                break;
            }
        }

        if (!closed && e == self.edges[e].next) || (closed && self.edges[e].prev == self.edges[e].next) {
            return Ok(false);
        }

        if !closed {
            self.has_open = true;
            let last = self.edges[e_start].prev;
            self.edges[last].out_idx = SKIP;
        }

        // 3. Second stage of edge initialization.
        let mut flat = true;
        e = e_start;
        loop {
            self.init_edge2(e, poly);
            e = self.edges[e].next;
            if flat && self.edges[e].curr.y != self.edges[e_start].curr.y {
                flat = false;
            }
            if e == e_start {
                break;
            }
        }

        // 4. Add edge bounds to the local minima list.
        if flat {
            if closed {
                return Ok(false);
            }
            let prev = self.edges[e].prev;
            self.edges[prev].out_idx = SKIP;
            let min = Minimum { y: self.edges[e].bot.y, left: NIL, right: e };
            self.edges[e].side = Side::Right;
            self.edges[e].wind_delta = 0;
            loop {
                if self.edges[e].bot.x != self.edges[self.edges[e].prev].top.x {
                    self.reverse_horizontal(e);
                }
                if self.edges[self.edges[e].next].out_idx == SKIP {
                    break;
                }
                self.edges[e].next_lml = self.edges[e].next;
                e = self.edges[e].next;
            }
            self.minima.push(min);
            return Ok(true);
        }

        let mut e_min = NIL;
        {
            let prev = self.edges[e].prev;
            if self.edges[prev].bot == self.edges[prev].top {
                e = self.edges[e].next;
            }
        }
        loop {
            e = self.find_next_loc_min(e);
            if e == e_min {
                break;
            } else if e_min == NIL {
                e_min = e;
            }
            let prev = self.edges[e].prev;
            let mut min = Minimum { y: self.edges[e].bot.y, left: NIL, right: NIL };
            let left_forward;
            if self.edges[e].dx < self.edges[prev].dx {
                min.left = prev;
                min.right = e;
                left_forward = false;
            } else {
                min.left = e;
                min.right = prev;
                left_forward = true;
            }
            let delta = if !closed {
                0
            } else if self.edges[min.left].next == min.right {
                -1
            } else {
                1
            };
            self.edges[min.left].wind_delta = delta;
            self.edges[min.right].wind_delta = -delta;

            e = self.process_bound(min.left, left_forward);
            if self.edges[e].out_idx == SKIP {
                e = self.process_bound(e, left_forward);
            }
            let mut e2 = self.process_bound(min.right, !left_forward);
            if self.edges[e2].out_idx == SKIP {
                e2 = self.process_bound(e2, !left_forward);
            }
            if self.edges[min.left].out_idx == SKIP {
                min.left = NIL;
            } else if self.edges[min.right].out_idx == SKIP {
                min.right = NIL;
            }
            self.minima.push(min);
            if !left_forward {
                e = e2;
            }
        }
        Ok(true)
    }

    /// Adds several paths; `false` when none went in.
    pub fn add_paths(&mut self, paths: &[Path], poly: PolyType, closed: bool) -> Result<bool, Failed> {
        let mut any = false;
        for p in paths {
            any |= self.add_path(p, poly, closed)?;
        }
        Ok(any)
    }

    /// The bounds of the paths added so far (closed paths only).
    pub fn bounds(&self) -> Rect {
        let Some(first) = self.minima.first() else { return Rect::default() };
        let Some(start) = self.edges.get(first.left) else { return Rect::default() };
        let mut r = Rect { left: start.bot.x, top: start.bot.y, right: start.bot.x, bottom: start.bot.y };
        for lm in &self.minima {
            let Some(left) = self.edges.get(lm.left) else { continue };
            r.bottom = r.bottom.max(left.bot.y);
            let mut e = lm.left;
            loop {
                let bottom = e;
                while self.edges[e].next_lml != NIL {
                    r.left = r.left.min(self.edges[e].bot.x);
                    r.right = r.right.max(self.edges[e].bot.x);
                    e = self.edges[e].next_lml;
                }
                r.left = r.left.min(self.edges[e].bot.x).min(self.edges[e].top.x);
                r.right = r.right.max(self.edges[e].bot.x).max(self.edges[e].top.x);
                r.top = r.top.min(self.edges[e].top.y);
                if bottom == lm.left {
                    e = lm.right;
                } else {
                    break;
                }
            }
        }
        r
    }

    // ---- the sweep ----------------------------------------------------------------------

    fn reset(&mut self) {
        if self.minima.is_empty() {
            return;
        }
        std_sort(&mut self.minima, &|a: &Minimum, b: &Minimum| a.y < b.y);
        for i in 0..self.minima.len() {
            let lm = self.minima[i];
            if lm.left != NIL {
                let e = &mut self.edges[lm.left];
                e.curr = e.bot;
                e.side = Side::Left;
                e.out_idx = UNASSIGNED;
            }
            if lm.right != NIL {
                let e = &mut self.edges[lm.right];
                e.curr = e.bot;
                e.side = Side::Right;
                e.out_idx = UNASSIGNED;
            }
        }
        self.scanbeam.clear();
        self.maxima.clear();
        self.active = NIL;
        self.sorted = NIL;
        for lm in self.minima.iter().rev() {
            self.scanbeam.push(lm.y);
        }
    }

    /// Runs into plain loops. Open paths need [`Self::execute_tree`].
    pub fn execute(&mut self, clip: ClipType, subj: FillType, clip_fill: FillType) -> Result<Paths, Failed> {
        if self.has_open {
            return Err(Failed);
        }
        self.subj_fill = subj;
        self.clip_fill = clip_fill;
        self.clip_type = clip;
        self.using_tree = false;
        self.execute_internal()?;
        Ok(self.build_result())
    }

    /// Runs into a tree of loops and holes, with any open paths at its top.
    pub fn execute_tree(&mut self, clip: ClipType, subj: FillType, clip_fill: FillType) -> Result<PolyTree, Failed> {
        self.subj_fill = subj;
        self.clip_fill = clip_fill;
        self.clip_type = clip;
        self.using_tree = true;
        self.execute_internal()?;
        Ok(self.build_result_tree())
    }

    fn pop_beam(&mut self) -> Option<i64> {
        let y = self.scanbeam.pop()?;
        while self.scanbeam.peek() == Some(&y) {
            self.scanbeam.pop();
        }
        Some(y)
    }

    fn execute_internal(&mut self) -> Result<(), Failed> {
        self.reset();
        if self.minima.is_empty() {
            return Ok(());
        }
        let Some(mut bot) = self.pop_beam() else { return Err(Failed) };
        loop {
            self.insert_local_minima(bot);
            self.process_horizontals()?;
            self.ghosts.clear();
            let Some(top) = self.pop_beam() else { break };
            self.process_intersections(top)?;
            self.process_top(top)?;
            bot = top;
            if self.scanbeam.is_empty() && self.minima.is_empty() {
                break;
            }
        }

        for i in 0..self.recs.len() {
            let r = self.recs[i];
            if r.pts != NIL && !r.is_open && (r.is_hole ^ self.reverse_output) == (self.area_of(r.pts) > 0.0) {
                self.reverse_links(r.pts);
            }
        }

        self.join_common_edges();

        for i in 0..self.recs.len() {
            if self.recs[i].pts != NIL {
                if self.recs[i].is_open {
                    self.fixup_polyline(i);
                } else {
                    self.fixup_polygon(i);
                }
            }
        }
        if self.strict_simple {
            self.do_simple_polygons();
        }
        self.joins.clear();
        self.ghosts.clear();
        Ok(())
    }

    fn area_of(&self, op: usize) -> f64 {
        if op == NIL {
            return 0.0;
        }
        let mut a = 0.0;
        let mut p = op;
        loop {
            let (cur, prev) = (self.pts[p], self.pts[self.pts[p].prev]);
            a += (prev.pt.x + cur.pt.x) as f64 * (prev.pt.y - cur.pt.y) as f64;
            p = cur.next;
            if p == op {
                break;
            }
        }
        a * 0.5
    }

    fn reverse_links(&mut self, pp: usize) {
        if pp == NIL {
            return;
        }
        let mut p = pp;
        loop {
            let next = self.pts[p].next;
            self.pts[p].next = self.pts[p].prev;
            self.pts[p].prev = next;
            p = next;
            if p == pp {
                break;
            }
        }
    }

    fn is_even_odd(&self, e: &Edge) -> bool {
        match e.poly {
            PolyType::Subject => self.subj_fill == FillType::EvenOdd,
            PolyType::Clip => self.clip_fill == FillType::EvenOdd,
        }
    }

    fn is_even_odd_alt(&self, e: &Edge) -> bool {
        match e.poly {
            PolyType::Subject => self.clip_fill == FillType::EvenOdd,
            PolyType::Clip => self.subj_fill == FillType::EvenOdd,
        }
    }

    fn set_winding_count(&mut self, edge: usize) {
        let mut ed = self.edges[edge];
        let mut e = ed.prev_ael;
        while e != NIL && (self.edges[e].poly != ed.poly || self.edges[e].wind_delta == 0) {
            e = self.edges[e].prev_ael;
        }
        if e == NIL {
            if ed.wind_delta == 0 {
                let pft = if ed.poly == PolyType::Subject { self.subj_fill } else { self.clip_fill };
                ed.wind_cnt = if pft == FillType::Negative { -1 } else { 1 };
            } else {
                ed.wind_cnt = ed.wind_delta;
            }
            ed.wind_cnt2 = 0;
            e = self.active;
        } else if ed.wind_delta == 0 && self.clip_type != ClipType::Union {
            ed.wind_cnt = 1;
            ed.wind_cnt2 = self.edges[e].wind_cnt2;
            e = self.edges[e].next_ael;
        } else if self.is_even_odd(&ed) {
            if ed.wind_delta == 0 {
                let mut inside = true;
                let mut e2 = self.edges[e].prev_ael;
                while e2 != NIL {
                    if self.edges[e2].poly == self.edges[e].poly && self.edges[e2].wind_delta != 0 {
                        inside = !inside;
                    }
                    e2 = self.edges[e2].prev_ael;
                }
                ed.wind_cnt = if inside { 0 } else { 1 };
            } else {
                ed.wind_cnt = ed.wind_delta;
            }
            ed.wind_cnt2 = self.edges[e].wind_cnt2;
            e = self.edges[e].next_ael;
        } else {
            let p = self.edges[e];
            if p.wind_cnt * p.wind_delta < 0 {
                if p.wind_cnt.abs() > 1 {
                    if p.wind_delta * ed.wind_delta < 0 {
                        ed.wind_cnt = p.wind_cnt;
                    } else {
                        ed.wind_cnt = p.wind_cnt + ed.wind_delta;
                    }
                } else {
                    ed.wind_cnt = if ed.wind_delta == 0 { 1 } else { ed.wind_delta };
                }
            } else if ed.wind_delta == 0 {
                ed.wind_cnt = if p.wind_cnt < 0 { p.wind_cnt - 1 } else { p.wind_cnt + 1 };
            } else if p.wind_delta * ed.wind_delta < 0 {
                ed.wind_cnt = p.wind_cnt;
            } else {
                ed.wind_cnt = p.wind_cnt + ed.wind_delta;
            }
            ed.wind_cnt2 = p.wind_cnt2;
            e = p.next_ael;
        }

        if self.is_even_odd_alt(&ed) {
            while e != edge {
                if self.edges[e].wind_delta != 0 {
                    ed.wind_cnt2 = if ed.wind_cnt2 == 0 { 1 } else { 0 };
                }
                e = self.edges[e].next_ael;
            }
        } else {
            while e != edge {
                ed.wind_cnt2 += self.edges[e].wind_delta;
                e = self.edges[e].next_ael;
            }
        }
        self.edges[edge].wind_cnt = ed.wind_cnt;
        self.edges[edge].wind_cnt2 = ed.wind_cnt2;
    }

    fn is_contributing(&self, edge: usize) -> bool {
        let ed = &self.edges[edge];
        let (pft, pft2) = if ed.poly == PolyType::Subject {
            (self.subj_fill, self.clip_fill)
        } else {
            (self.clip_fill, self.subj_fill)
        };
        match pft {
            FillType::EvenOdd => {
                if ed.wind_delta == 0 && ed.wind_cnt != 1 {
                    return false;
                }
            }
            FillType::NonZero => {
                if ed.wind_cnt.abs() != 1 {
                    return false;
                }
            }
            FillType::Positive => {
                if ed.wind_cnt != 1 {
                    return false;
                }
            }
            FillType::Negative => {
                if ed.wind_cnt != -1 {
                    return false;
                }
            }
        }
        let outside = || match pft2 {
            FillType::EvenOdd | FillType::NonZero => ed.wind_cnt2 == 0,
            FillType::Positive => ed.wind_cnt2 <= 0,
            FillType::Negative => ed.wind_cnt2 >= 0,
        };
        let inside = || match pft2 {
            FillType::EvenOdd | FillType::NonZero => ed.wind_cnt2 != 0,
            FillType::Positive => ed.wind_cnt2 > 0,
            FillType::Negative => ed.wind_cnt2 < 0,
        };
        match self.clip_type {
            ClipType::Intersection => inside(),
            ClipType::Union => outside(),
            ClipType::Difference => {
                if ed.poly == PolyType::Subject {
                    outside()
                } else {
                    inside()
                }
            }
            ClipType::Xor => {
                if ed.wind_delta == 0 {
                    outside()
                } else {
                    true
                }
            }
        }
    }

    fn top_x(&self, e: usize, y: i64) -> i64 {
        let ed = &self.edges[e];
        if y == ed.top.y { ed.top.x } else { ed.bot.x + round(ed.dx * (y - ed.bot.y) as f64) }
    }

    fn add_local_min_poly(&mut self, e1: usize, e2: usize, pt: Pt) -> usize {
        let result;
        let e;
        let prev_e;
        if self.is_horizontal(e2) || self.edges[e1].dx > self.edges[e2].dx {
            result = self.add_out_pt(e1, pt);
            self.edges[e2].out_idx = self.edges[e1].out_idx;
            self.edges[e1].side = Side::Left;
            self.edges[e2].side = Side::Right;
            e = e1;
            prev_e = if self.edges[e].prev_ael == e2 { self.edges[e2].prev_ael } else { self.edges[e].prev_ael };
        } else {
            result = self.add_out_pt(e2, pt);
            self.edges[e1].out_idx = self.edges[e2].out_idx;
            self.edges[e1].side = Side::Right;
            self.edges[e2].side = Side::Left;
            e = e2;
            prev_e = if self.edges[e].prev_ael == e1 { self.edges[e1].prev_ael } else { self.edges[e].prev_ael };
        }
        if prev_e != NIL
            && self.edges[prev_e].out_idx >= 0
            && self.edges[prev_e].top.y < pt.y
            && self.edges[e].top.y < pt.y
        {
            let x_prev = self.top_x(prev_e, pt.y);
            let x_e = self.top_x(e, pt.y);
            if x_prev == x_e
                && self.edges[e].wind_delta != 0
                && self.edges[prev_e].wind_delta != 0
                && slopes4(
                    Pt::new(x_prev, pt.y),
                    self.edges[prev_e].top,
                    Pt::new(x_e, pt.y),
                    self.edges[e].top,
                    self.full,
                )
            {
                let out = self.add_out_pt(prev_e, pt);
                self.joins.push(Join { out1: result, out2: out, off: self.edges[e].top });
            }
        }
        result
    }

    fn add_local_max_poly(&mut self, e1: usize, e2: usize, pt: Pt) {
        self.add_out_pt(e1, pt);
        if self.edges[e2].wind_delta == 0 {
            self.add_out_pt(e2, pt);
        }
        let (o1, o2) = (self.edges[e1].out_idx, self.edges[e2].out_idx);
        if o1 == o2 {
            self.edges[e1].out_idx = UNASSIGNED;
            self.edges[e2].out_idx = UNASSIGNED;
        } else if o1 < o2 {
            self.append_polygon(e1, e2);
        } else {
            self.append_polygon(e2, e1);
        }
    }

    fn add_edge_to_sel(&mut self, edge: usize) {
        if self.sorted == NIL {
            self.sorted = edge;
            self.edges[edge].prev_sel = NIL;
            self.edges[edge].next_sel = NIL;
        } else {
            self.edges[edge].next_sel = self.sorted;
            self.edges[edge].prev_sel = NIL;
            let s = self.sorted;
            self.edges[s].prev_sel = edge;
            self.sorted = edge;
        }
    }

    fn copy_ael_to_sel(&mut self) {
        let mut e = self.active;
        self.sorted = e;
        while e != NIL {
            self.edges[e].prev_sel = self.edges[e].prev_ael;
            self.edges[e].next_sel = self.edges[e].next_ael;
            e = self.edges[e].next_ael;
        }
    }

    fn insert_local_minima(&mut self, bot_y: i64) {
        while let Some(&lm) = self.minima.last() {
            if lm.y != bot_y {
                break;
            }
            let (lb, rb) = (lm.left, lm.right);
            self.minima.pop();

            let mut op1 = NIL;
            if lb == NIL {
                self.insert_edge(rb, NIL);
                self.set_winding_count(rb);
                if self.is_contributing(rb) {
                    op1 = self.add_out_pt(rb, self.edges[rb].bot);
                }
            } else if rb == NIL {
                self.insert_edge(lb, NIL);
                self.set_winding_count(lb);
                if self.is_contributing(lb) {
                    op1 = self.add_out_pt(lb, self.edges[lb].bot);
                }
                self.scanbeam.push(self.edges[lb].top.y);
            } else {
                self.insert_edge(lb, NIL);
                self.insert_edge(rb, lb);
                self.set_winding_count(lb);
                self.edges[rb].wind_cnt = self.edges[lb].wind_cnt;
                self.edges[rb].wind_cnt2 = self.edges[lb].wind_cnt2;
                if self.is_contributing(lb) {
                    op1 = self.add_local_min_poly(lb, rb, self.edges[lb].bot);
                }
                self.scanbeam.push(self.edges[lb].top.y);
            }

            if rb != NIL {
                if self.is_horizontal(rb) {
                    self.add_edge_to_sel(rb);
                    let next = self.edges[rb].next_lml;
                    if next != NIL {
                        self.scanbeam.push(self.edges[next].top.y);
                    }
                } else {
                    self.scanbeam.push(self.edges[rb].top.y);
                }
            }

            if lb == NIL || rb == NIL {
                continue;
            }

            if op1 != NIL && self.is_horizontal(rb) && !self.ghosts.is_empty() && self.edges[rb].wind_delta != 0 {
                for i in 0..self.ghosts.len() {
                    let jr = self.ghosts[i];
                    if horz_overlap(self.pts[jr.out1].pt.x, jr.off.x, self.edges[rb].bot.x, self.edges[rb].top.x) {
                        self.joins.push(Join { out1: jr.out1, out2: op1, off: jr.off });
                    }
                }
            }

            let lp = self.edges[lb].prev_ael;
            if self.edges[lb].out_idx >= 0
                && lp != NIL
                && self.edges[lp].curr.x == self.edges[lb].bot.x
                && self.edges[lp].out_idx >= 0
                && slopes4(self.edges[lp].bot, self.edges[lp].top, self.edges[lb].curr, self.edges[lb].top, self.full)
                && self.edges[lb].wind_delta != 0
                && self.edges[lp].wind_delta != 0
            {
                let op2 = self.add_out_pt(lp, self.edges[lb].bot);
                self.joins.push(Join { out1: op1, out2: op2, off: self.edges[lb].top });
            }

            if self.edges[lb].next_ael != rb {
                let rp = self.edges[rb].prev_ael;
                if self.edges[rb].out_idx >= 0
                    && self.edges[rp].out_idx >= 0
                    && slopes4(
                        self.edges[rp].curr,
                        self.edges[rp].top,
                        self.edges[rb].curr,
                        self.edges[rb].top,
                        self.full,
                    )
                    && self.edges[rb].wind_delta != 0
                    && self.edges[rp].wind_delta != 0
                {
                    let op2 = self.add_out_pt(rp, self.edges[rb].bot);
                    self.joins.push(Join { out1: op1, out2: op2, off: self.edges[rb].top });
                }
                let mut e = self.edges[lb].next_ael;
                if e != NIL {
                    while e != rb {
                        self.intersect_edges(rb, e, self.edges[lb].curr);
                        e = self.edges[e].next_ael;
                    }
                }
            }
        }
    }

    fn delete_from_ael(&mut self, e: usize) {
        let (prev, next) = (self.edges[e].prev_ael, self.edges[e].next_ael);
        if prev == NIL && next == NIL && e != self.active {
            return;
        }
        if prev != NIL {
            self.edges[prev].next_ael = next;
        } else {
            self.active = next;
        }
        if next != NIL {
            self.edges[next].prev_ael = prev;
        }
        self.edges[e].next_ael = NIL;
        self.edges[e].prev_ael = NIL;
    }

    fn delete_from_sel(&mut self, e: usize) {
        let (prev, next) = (self.edges[e].prev_sel, self.edges[e].next_sel);
        if prev == NIL && next == NIL && e != self.sorted {
            return;
        }
        if prev != NIL {
            self.edges[prev].next_sel = next;
        } else {
            self.sorted = next;
        }
        if next != NIL {
            self.edges[next].prev_sel = prev;
        }
        self.edges[e].next_sel = NIL;
        self.edges[e].prev_sel = NIL;
    }

    fn swap_sides_and_idx(&mut self, e1: usize, e2: usize) {
        let (s1, s2) = (self.edges[e1].side, self.edges[e2].side);
        self.edges[e1].side = s2;
        self.edges[e2].side = s1;
        let (o1, o2) = (self.edges[e1].out_idx, self.edges[e2].out_idx);
        self.edges[e1].out_idx = o2;
        self.edges[e2].out_idx = o1;
    }

    fn swap_sides(&mut self, e1: usize, e2: usize) {
        let (s1, s2) = (self.edges[e1].side, self.edges[e2].side);
        self.edges[e1].side = s2;
        self.edges[e2].side = s1;
    }

    fn intersect_edges(&mut self, e1: usize, e2: usize, pt: Pt) {
        let e1c = self.edges[e1].out_idx >= 0;
        let e2c = self.edges[e2].out_idx >= 0;
        let (d1, d2) = (self.edges[e1].wind_delta, self.edges[e2].wind_delta);

        // Open paths.
        if d1 == 0 || d2 == 0 {
            if d1 == 0 && d2 == 0 {
                return;
            } else if self.edges[e1].poly == self.edges[e2].poly && d1 != d2 && self.clip_type == ClipType::Union {
                if d1 == 0 {
                    if e2c {
                        self.add_out_pt(e1, pt);
                        if e1c {
                            self.edges[e1].out_idx = UNASSIGNED;
                        }
                    }
                } else if e1c {
                    self.add_out_pt(e2, pt);
                    if e2c {
                        self.edges[e2].out_idx = UNASSIGNED;
                    }
                }
            } else if self.edges[e1].poly != self.edges[e2].poly {
                if d1 == 0
                    && self.edges[e2].wind_cnt.abs() == 1
                    && (self.clip_type != ClipType::Union || self.edges[e2].wind_cnt2 == 0)
                {
                    self.add_out_pt(e1, pt);
                    if e1c {
                        self.edges[e1].out_idx = UNASSIGNED;
                    }
                } else if d2 == 0
                    && self.edges[e1].wind_cnt.abs() == 1
                    && (self.clip_type != ClipType::Union || self.edges[e1].wind_cnt2 == 0)
                {
                    self.add_out_pt(e2, pt);
                    if e2c {
                        self.edges[e2].out_idx = UNASSIGNED;
                    }
                }
            }
            return;
        }

        // Winding counts: e1 is to the right of e2 above the intersection.
        if self.edges[e1].poly == self.edges[e2].poly {
            if self.is_even_odd(&self.edges[e1]) {
                let old = self.edges[e1].wind_cnt;
                self.edges[e1].wind_cnt = self.edges[e2].wind_cnt;
                self.edges[e2].wind_cnt = old;
            } else {
                if self.edges[e1].wind_cnt + d2 == 0 {
                    self.edges[e1].wind_cnt = -self.edges[e1].wind_cnt;
                } else {
                    self.edges[e1].wind_cnt += d2;
                }
                if self.edges[e2].wind_cnt - d1 == 0 {
                    self.edges[e2].wind_cnt = -self.edges[e2].wind_cnt;
                } else {
                    self.edges[e2].wind_cnt -= d1;
                }
            }
        } else {
            if !self.is_even_odd(&self.edges[e2]) {
                self.edges[e1].wind_cnt2 += d2;
            } else {
                self.edges[e1].wind_cnt2 = if self.edges[e1].wind_cnt2 == 0 { 1 } else { 0 };
            }
            if !self.is_even_odd(&self.edges[e1]) {
                self.edges[e2].wind_cnt2 -= d1;
            } else {
                self.edges[e2].wind_cnt2 = if self.edges[e2].wind_cnt2 == 0 { 1 } else { 0 };
            }
        }

        let (e1_fill, e1_fill2) = if self.edges[e1].poly == PolyType::Subject {
            (self.subj_fill, self.clip_fill)
        } else {
            (self.clip_fill, self.subj_fill)
        };
        let (e2_fill, e2_fill2) = if self.edges[e2].poly == PolyType::Subject {
            (self.subj_fill, self.clip_fill)
        } else {
            (self.clip_fill, self.subj_fill)
        };
        let wc = |fill: FillType, n: i32| match fill {
            FillType::Positive => n,
            FillType::Negative => -n,
            _ => n.abs(),
        };
        let e1_wc = wc(e1_fill, self.edges[e1].wind_cnt);
        let e2_wc = wc(e2_fill, self.edges[e2].wind_cnt);

        if e1c && e2c {
            if (e1_wc != 0 && e1_wc != 1)
                || (e2_wc != 0 && e2_wc != 1)
                || (self.edges[e1].poly != self.edges[e2].poly && self.clip_type != ClipType::Xor)
            {
                self.add_local_max_poly(e1, e2, pt);
            } else {
                self.add_out_pt(e1, pt);
                self.add_out_pt(e2, pt);
                self.swap_sides_and_idx(e1, e2);
            }
        } else if e1c {
            if e2_wc == 0 || e2_wc == 1 {
                self.add_out_pt(e1, pt);
                self.swap_sides_and_idx(e1, e2);
            }
        } else if e2c {
            if e1_wc == 0 || e1_wc == 1 {
                self.add_out_pt(e2, pt);
                self.swap_sides_and_idx(e1, e2);
            }
        } else if (e1_wc == 0 || e1_wc == 1) && (e2_wc == 0 || e2_wc == 1) {
            let e1_wc2 = wc(e1_fill2, self.edges[e1].wind_cnt2);
            let e2_wc2 = wc(e2_fill2, self.edges[e2].wind_cnt2);
            if self.edges[e1].poly != self.edges[e2].poly {
                self.add_local_min_poly(e1, e2, pt);
            } else if e1_wc == 1 && e2_wc == 1 {
                let add = match self.clip_type {
                    ClipType::Intersection => e1_wc2 > 0 && e2_wc2 > 0,
                    ClipType::Union => e1_wc2 <= 0 && e2_wc2 <= 0,
                    ClipType::Difference => {
                        (self.edges[e1].poly == PolyType::Clip && e1_wc2 > 0 && e2_wc2 > 0)
                            || (self.edges[e1].poly == PolyType::Subject && e1_wc2 <= 0 && e2_wc2 <= 0)
                    }
                    ClipType::Xor => true,
                };
                if add {
                    self.add_local_min_poly(e1, e2, pt);
                }
            } else {
                self.swap_sides(e1, e2);
            }
        }
    }

    fn set_hole_state(&mut self, e: usize, rec: usize) {
        let mut e2 = self.edges[e].prev_ael;
        let mut tmp = NIL;
        while e2 != NIL {
            if self.edges[e2].out_idx >= 0 && self.edges[e2].wind_delta != 0 {
                if tmp == NIL {
                    tmp = e2;
                } else if self.edges[tmp].out_idx == self.edges[e2].out_idx {
                    tmp = NIL;
                }
            }
            e2 = self.edges[e2].prev_ael;
        }
        if tmp == NIL {
            self.recs[rec].first_left = NIL;
            self.recs[rec].is_hole = false;
        } else {
            let left = self.edges[tmp].out_idx as usize;
            self.recs[rec].first_left = left;
            self.recs[rec].is_hole = !self.recs[left].is_hole;
        }
    }

    fn first_is_bottom_pt(&self, b1: usize, b2: usize) -> bool {
        let abs_dx = |from: usize, forward: bool| {
            let at = self.pts[from].pt;
            let step = |p: usize| if forward { self.pts[p].next } else { self.pts[p].prev };
            let mut p = step(from);
            while self.pts[p].pt == at && p != from {
                p = step(p);
            }
            get_dx(at, self.pts[p].pt).abs()
        };
        let dx1p = abs_dx(b1, false);
        let dx1n = abs_dx(b1, true);
        let dx2p = abs_dx(b2, false);
        let dx2n = abs_dx(b2, true);
        if dx1p.max(dx1n) == dx2p.max(dx2n) && dx1p.min(dx1n) == dx2p.min(dx2n) {
            self.area_of(b1) > 0.0
        } else {
            (dx1p >= dx2p && dx1p >= dx2n) || (dx1n >= dx2p && dx1n >= dx2n)
        }
    }

    fn get_bottom_pt(&self, mut pp: usize) -> usize {
        let mut dups = NIL;
        let mut p = self.pts[pp].next;
        while p != pp {
            let (a, b) = (self.pts[p].pt, self.pts[pp].pt);
            if a.y > b.y {
                pp = p;
                dups = NIL;
            } else if a.y == b.y && a.x <= b.x {
                if a.x < b.x {
                    dups = NIL;
                    pp = p;
                } else if self.pts[p].next != pp && self.pts[p].prev != pp {
                    dups = p;
                }
            }
            p = self.pts[p].next;
        }
        if dups != NIL {
            while dups != p {
                if !self.first_is_bottom_pt(p, dups) {
                    pp = dups;
                }
                dups = self.pts[dups].next;
                while self.pts[dups].pt != self.pts[pp].pt {
                    dups = self.pts[dups].next;
                }
            }
        }
        pp
    }

    fn get_lowermost_rec(&mut self, r1: usize, r2: usize) -> usize {
        if self.recs[r1].bottom == NIL {
            self.recs[r1].bottom = self.get_bottom_pt(self.recs[r1].pts);
        }
        if self.recs[r2].bottom == NIL {
            self.recs[r2].bottom = self.get_bottom_pt(self.recs[r2].pts);
        }
        let (o1, o2) = (self.recs[r1].bottom, self.recs[r2].bottom);
        let (p1, p2) = (self.pts[o1].pt, self.pts[o2].pt);
        if p1.y > p2.y {
            r1
        } else if p1.y < p2.y {
            r2
        } else if p1.x < p2.x {
            r1
        } else if p1.x > p2.x {
            r2
        } else if self.pts[o1].next == o1 {
            r2
        } else if self.pts[o2].next == o2 {
            r1
        } else if self.first_is_bottom_pt(o1, o2) {
            r1
        } else {
            r2
        }
    }

    fn rec1_right_of_rec2(&self, mut r1: usize, r2: usize) -> bool {
        loop {
            r1 = self.recs[r1].first_left;
            if r1 == r2 {
                return true;
            }
            if r1 == NIL {
                return false;
            }
        }
    }

    fn get_out_rec(&self, idx: i32) -> usize {
        let mut r = idx as usize;
        while r != self.recs[r].idx as usize {
            r = self.recs[r].idx as usize;
        }
        r
    }

    fn append_polygon(&mut self, e1: usize, e2: usize) {
        let r1 = self.edges[e1].out_idx as usize;
        let r2 = self.edges[e2].out_idx as usize;
        let hole_state = if self.rec1_right_of_rec2(r1, r2) {
            r2
        } else if self.rec1_right_of_rec2(r2, r1) {
            r1
        } else {
            self.get_lowermost_rec(r1, r2)
        };

        let p1_lft = self.recs[r1].pts;
        let p1_rt = self.pts[p1_lft].prev;
        let p2_lft = self.recs[r2].pts;
        let p2_rt = self.pts[p2_lft].prev;

        if self.edges[e1].side == Side::Left {
            if self.edges[e2].side == Side::Left {
                self.reverse_links(p2_lft);
                self.pts[p2_lft].next = p1_lft;
                self.pts[p1_lft].prev = p2_lft;
                self.pts[p1_rt].next = p2_rt;
                self.pts[p2_rt].prev = p1_rt;
                self.recs[r1].pts = p2_rt;
            } else {
                self.pts[p2_rt].next = p1_lft;
                self.pts[p1_lft].prev = p2_rt;
                self.pts[p2_lft].prev = p1_rt;
                self.pts[p1_rt].next = p2_lft;
                self.recs[r1].pts = p2_lft;
            }
        } else if self.edges[e2].side == Side::Right {
            self.reverse_links(p2_lft);
            self.pts[p1_rt].next = p2_rt;
            self.pts[p2_rt].prev = p1_rt;
            self.pts[p2_lft].next = p1_lft;
            self.pts[p1_lft].prev = p2_lft;
        } else {
            self.pts[p1_rt].next = p2_lft;
            self.pts[p2_lft].prev = p1_rt;
            self.pts[p1_lft].prev = p2_rt;
            self.pts[p2_rt].next = p1_lft;
        }

        self.recs[r1].bottom = NIL;
        if hole_state == r2 {
            if self.recs[r2].first_left != r1 {
                self.recs[r1].first_left = self.recs[r2].first_left;
            }
            self.recs[r1].is_hole = self.recs[r2].is_hole;
        }
        self.recs[r2].pts = NIL;
        self.recs[r2].bottom = NIL;
        self.recs[r2].first_left = r1;

        let ok = self.edges[e1].out_idx;
        let obsolete = self.edges[e2].out_idx;
        self.edges[e1].out_idx = UNASSIGNED;
        self.edges[e2].out_idx = UNASSIGNED;

        let side = self.edges[e1].side;
        let mut e = self.active;
        while e != NIL {
            if self.edges[e].out_idx == obsolete {
                self.edges[e].out_idx = ok;
                self.edges[e].side = side;
                break;
            }
            e = self.edges[e].next_ael;
        }
        self.recs[r2].idx = self.recs[r1].idx;
    }

    fn create_out_rec(&mut self) -> usize {
        let idx = self.recs.len();
        self.recs.push(OutRec {
            idx: idx as i32,
            is_hole: false,
            is_open: false,
            first_left: NIL,
            node: NIL,
            pts: NIL,
            bottom: NIL,
        });
        idx
    }

    fn new_pt(&mut self, idx: i32, pt: Pt, next: usize, prev: usize) -> usize {
        let at = self.pts.len();
        let (next, prev) = (if next == NIL { at } else { next }, if prev == NIL { at } else { prev });
        self.pts.push(OutPt { idx, pt, next, prev });
        at
    }

    fn add_out_pt(&mut self, e: usize, pt: Pt) -> usize {
        if self.edges[e].out_idx < 0 {
            let rec = self.create_out_rec();
            self.recs[rec].is_open = self.edges[e].wind_delta == 0;
            let new_op = self.new_pt(rec as i32, pt, NIL, NIL);
            self.recs[rec].pts = new_op;
            if !self.recs[rec].is_open {
                self.set_hole_state(e, rec);
            }
            self.edges[e].out_idx = rec as i32;
            new_op
        } else {
            let rec = self.edges[e].out_idx as usize;
            let op = self.recs[rec].pts;
            let to_front = self.edges[e].side == Side::Left;
            if to_front && pt == self.pts[op].pt {
                return op;
            } else if !to_front && pt == self.pts[self.pts[op].prev].pt {
                return self.pts[op].prev;
            }
            let prev = self.pts[op].prev;
            let new_op = self.new_pt(rec as i32, pt, op, prev);
            self.pts[prev].next = new_op;
            self.pts[op].prev = new_op;
            if to_front {
                self.recs[rec].pts = new_op;
            }
            new_op
        }
    }

    fn get_last_out_pt(&self, e: usize) -> usize {
        let rec = self.edges[e].out_idx as usize;
        if self.edges[e].side == Side::Left { self.recs[rec].pts } else { self.pts[self.recs[rec].pts].prev }
    }

    fn process_horizontals(&mut self) -> Result<(), Failed> {
        let mut h = self.sorted;
        while h != NIL {
            self.delete_from_sel(h);
            self.process_horizontal(h)?;
            h = self.sorted;
        }
        Ok(())
    }

    fn is_maxima(&self, e: usize, y: i64) -> bool {
        e != NIL && self.edges[e].top.y == y && self.edges[e].next_lml == NIL
    }

    fn is_intermediate(&self, e: usize, y: i64) -> bool {
        self.edges[e].top.y == y && self.edges[e].next_lml != NIL
    }

    fn maxima_pair(&self, e: usize) -> usize {
        let ed = &self.edges[e];
        let (n, p) = (&self.edges[ed.next], &self.edges[ed.prev]);
        if n.top == ed.top && n.next_lml == NIL {
            ed.next
        } else if p.top == ed.top && p.next_lml == NIL {
            ed.prev
        } else {
            NIL
        }
    }

    fn maxima_pair_ex(&self, e: usize) -> usize {
        let r = self.maxima_pair(e);
        if r != NIL
            && (self.edges[r].out_idx == SKIP
                || (self.edges[r].next_ael == self.edges[r].prev_ael && !self.is_horizontal(r)))
        {
            return NIL;
        }
        r
    }

    fn swap_in_ael(&mut self, a: usize, b: usize) {
        if self.edges[a].next_ael == self.edges[a].prev_ael || self.edges[b].next_ael == self.edges[b].prev_ael {
            return;
        }
        if self.edges[a].next_ael == b {
            let next = self.edges[b].next_ael;
            if next != NIL {
                self.edges[next].prev_ael = a;
            }
            let prev = self.edges[a].prev_ael;
            if prev != NIL {
                self.edges[prev].next_ael = b;
            }
            self.edges[b].prev_ael = prev;
            self.edges[b].next_ael = a;
            self.edges[a].prev_ael = b;
            self.edges[a].next_ael = next;
        } else if self.edges[b].next_ael == a {
            let next = self.edges[a].next_ael;
            if next != NIL {
                self.edges[next].prev_ael = b;
            }
            let prev = self.edges[b].prev_ael;
            if prev != NIL {
                self.edges[prev].next_ael = a;
            }
            self.edges[a].prev_ael = prev;
            self.edges[a].next_ael = b;
            self.edges[b].prev_ael = a;
            self.edges[b].next_ael = next;
        } else {
            let next = self.edges[a].next_ael;
            let prev = self.edges[a].prev_ael;
            self.edges[a].next_ael = self.edges[b].next_ael;
            let n = self.edges[a].next_ael;
            if n != NIL {
                self.edges[n].prev_ael = a;
            }
            self.edges[a].prev_ael = self.edges[b].prev_ael;
            let p = self.edges[a].prev_ael;
            if p != NIL {
                self.edges[p].next_ael = a;
            }
            self.edges[b].next_ael = next;
            if next != NIL {
                self.edges[next].prev_ael = b;
            }
            self.edges[b].prev_ael = prev;
            if prev != NIL {
                self.edges[prev].next_ael = b;
            }
        }
        if self.edges[a].prev_ael == NIL {
            self.active = a;
        } else if self.edges[b].prev_ael == NIL {
            self.active = b;
        }
    }

    fn swap_in_sel(&mut self, a: usize, b: usize) {
        if self.edges[a].next_sel == NIL && self.edges[a].prev_sel == NIL {
            return;
        }
        if self.edges[b].next_sel == NIL && self.edges[b].prev_sel == NIL {
            return;
        }
        if self.edges[a].next_sel == b {
            let next = self.edges[b].next_sel;
            if next != NIL {
                self.edges[next].prev_sel = a;
            }
            let prev = self.edges[a].prev_sel;
            if prev != NIL {
                self.edges[prev].next_sel = b;
            }
            self.edges[b].prev_sel = prev;
            self.edges[b].next_sel = a;
            self.edges[a].prev_sel = b;
            self.edges[a].next_sel = next;
        } else if self.edges[b].next_sel == a {
            let next = self.edges[a].next_sel;
            if next != NIL {
                self.edges[next].prev_sel = b;
            }
            let prev = self.edges[b].prev_sel;
            if prev != NIL {
                self.edges[prev].next_sel = a;
            }
            self.edges[a].prev_sel = prev;
            self.edges[a].next_sel = b;
            self.edges[b].prev_sel = a;
            self.edges[b].next_sel = next;
        } else {
            let next = self.edges[a].next_sel;
            let prev = self.edges[a].prev_sel;
            self.edges[a].next_sel = self.edges[b].next_sel;
            let n = self.edges[a].next_sel;
            if n != NIL {
                self.edges[n].prev_sel = a;
            }
            self.edges[a].prev_sel = self.edges[b].prev_sel;
            let p = self.edges[a].prev_sel;
            if p != NIL {
                self.edges[p].next_sel = a;
            }
            self.edges[b].next_sel = next;
            if next != NIL {
                self.edges[next].prev_sel = b;
            }
            self.edges[b].prev_sel = prev;
            if prev != NIL {
                self.edges[prev].next_sel = b;
            }
        }
        if self.edges[a].prev_sel == NIL {
            self.sorted = a;
        } else if self.edges[b].prev_sel == NIL {
            self.sorted = b;
        }
    }

    /// (left to right, left, right)
    fn horz_direction(&self, e: usize) -> (bool, i64, i64) {
        let ed = &self.edges[e];
        if ed.bot.x < ed.top.x { (true, ed.bot.x, ed.top.x) } else { (false, ed.top.x, ed.bot.x) }
    }

    fn join_overlapping_horz(&mut self, horz: usize, op1: usize) {
        let mut next = self.sorted;
        while next != NIL {
            if self.edges[next].out_idx >= 0
                && horz_overlap(
                    self.edges[horz].bot.x,
                    self.edges[horz].top.x,
                    self.edges[next].bot.x,
                    self.edges[next].top.x,
                )
            {
                let op2 = self.get_last_out_pt(next);
                self.joins.push(Join { out1: op2, out2: op1, off: self.edges[next].top });
            }
            next = self.edges[next].next_sel;
        }
    }

    fn process_horizontal(&mut self, mut horz: usize) -> Result<(), Failed> {
        let is_open = self.edges[horz].wind_delta == 0;
        let (mut ltr, mut horz_left, mut horz_right) = self.horz_direction(horz);

        let mut last = horz;
        while self.edges[last].next_lml != NIL && self.is_horizontal(self.edges[last].next_lml) {
            last = self.edges[last].next_lml;
        }
        let max_pair = if self.edges[last].next_lml == NIL { self.maxima_pair(last) } else { NIL };

        // Cursors into `maxima`, from the left when going right and from the right otherwise.
        let total = self.maxima.len();
        let mut max_it = 0;
        let mut max_rit = 0;
        if total > 0 {
            if ltr {
                while max_it < total && self.maxima[max_it] <= self.edges[horz].bot.x {
                    max_it += 1;
                }
                if max_it < total && self.maxima[max_it] >= self.edges[last].top.x {
                    max_it = total;
                }
            } else {
                while max_rit < total && self.maxima[total - 1 - max_rit] > self.edges[horz].bot.x {
                    max_rit += 1;
                }
                if max_rit < total && self.maxima[total - 1 - max_rit] <= self.edges[last].top.x {
                    max_rit = total;
                }
            }
        }

        let mut op1 = NIL;
        loop {
            let is_last = horz == last;
            let mut e = if ltr { self.edges[horz].next_ael } else { self.edges[horz].prev_ael };
            while e != NIL {
                if total > 0 {
                    if ltr {
                        while max_it < total && self.maxima[max_it] < self.edges[e].curr.x {
                            if self.edges[horz].out_idx >= 0 && !is_open {
                                self.add_out_pt(horz, Pt::new(self.maxima[max_it], self.edges[horz].bot.y));
                            }
                            max_it += 1;
                        }
                    } else {
                        while max_rit < total && self.maxima[total - 1 - max_rit] > self.edges[e].curr.x {
                            if self.edges[horz].out_idx >= 0 && !is_open {
                                self.add_out_pt(
                                    horz,
                                    Pt::new(self.maxima[total - 1 - max_rit], self.edges[horz].bot.y),
                                );
                            }
                            max_rit += 1;
                        }
                    }
                }

                if (ltr && self.edges[e].curr.x > horz_right) || (!ltr && self.edges[e].curr.x < horz_left) {
                    break;
                }

                let lml = self.edges[horz].next_lml;
                if self.edges[e].curr.x == self.edges[horz].top.x && lml != NIL && self.edges[e].dx < self.edges[lml].dx
                {
                    break;
                }

                if self.edges[horz].out_idx >= 0 && !is_open {
                    op1 = self.add_out_pt(horz, self.edges[e].curr);
                    self.join_overlapping_horz(horz, op1);
                    self.ghosts.push(Join { out1: op1, out2: NIL, off: self.edges[horz].bot });
                }

                if e == max_pair && is_last {
                    if self.edges[horz].out_idx >= 0 {
                        self.add_local_max_poly(horz, max_pair, self.edges[horz].top);
                    }
                    self.delete_from_ael(horz);
                    self.delete_from_ael(max_pair);
                    return Ok(());
                }

                let pt = Pt::new(self.edges[e].curr.x, self.edges[horz].curr.y);
                if ltr {
                    self.intersect_edges(horz, e, pt);
                } else {
                    self.intersect_edges(e, horz, pt);
                }
                let next = if ltr { self.edges[e].next_ael } else { self.edges[e].prev_ael };
                self.swap_in_ael(horz, e);
                e = next;
            }

            let lml = self.edges[horz].next_lml;
            if lml == NIL || !self.is_horizontal(lml) {
                break;
            }
            horz = self.update_edge_into_ael(horz)?;
            if self.edges[horz].out_idx >= 0 {
                self.add_out_pt(horz, self.edges[horz].bot);
            }
            (ltr, horz_left, horz_right) = self.horz_direction(horz);
        }

        if self.edges[horz].out_idx >= 0 && op1 == NIL {
            op1 = self.get_last_out_pt(horz);
            self.join_overlapping_horz(horz, op1);
            self.ghosts.push(Join { out1: op1, out2: NIL, off: self.edges[horz].top });
        }

        if self.edges[horz].next_lml != NIL {
            if self.edges[horz].out_idx >= 0 {
                op1 = self.add_out_pt(horz, self.edges[horz].top);
                horz = self.update_edge_into_ael(horz)?;
                if self.edges[horz].wind_delta == 0 {
                    return Ok(());
                }
                self.join_collinear_neighbour(horz, op1);
            } else {
                self.update_edge_into_ael(horz)?;
            }
        } else {
            if self.edges[horz].out_idx >= 0 {
                self.add_out_pt(horz, self.edges[horz].top);
            }
            self.delete_from_ael(horz);
        }
        Ok(())
    }

    /// After promoting `e`, a neighbour running along it from the same point joins its loop.
    fn join_collinear_neighbour(&mut self, e: usize, op: usize) {
        let ed = self.edges[e];
        let along = |c: &Self, n: usize| {
            n != NIL
                && c.edges[n].curr == ed.bot
                && c.edges[n].wind_delta != 0
                && c.edges[n].out_idx >= 0
                && c.edges[n].curr.y > c.edges[n].top.y
                && slopes4(ed.top, ed.bot, c.edges[n].top, c.edges[n].bot, c.full)
        };
        for n in [ed.prev_ael, ed.next_ael] {
            if along(self, n) {
                let op2 = self.add_out_pt(n, ed.bot);
                self.joins.push(Join { out1: op, out2: op2, off: ed.top });
                return;
            }
        }
    }

    fn update_edge_into_ael(&mut self, e: usize) -> Result<usize, Failed> {
        let next = self.edges[e].next_lml;
        if next == NIL {
            return Err(Failed);
        }
        let old = self.edges[e];
        let (prev_ael, next_ael) = (old.prev_ael, old.next_ael);
        if prev_ael != NIL {
            self.edges[prev_ael].next_ael = next;
        } else {
            self.active = next;
        }
        if next_ael != NIL {
            self.edges[next_ael].prev_ael = next;
        }
        let n = &mut self.edges[next];
        n.out_idx = old.out_idx;
        n.side = old.side;
        n.wind_delta = old.wind_delta;
        n.wind_cnt = old.wind_cnt;
        n.wind_cnt2 = old.wind_cnt2;
        n.curr = n.bot;
        n.prev_ael = prev_ael;
        n.next_ael = next_ael;
        if !self.is_horizontal(next) {
            self.scanbeam.push(self.edges[next].top.y);
        }
        Ok(next)
    }

    fn process_intersections(&mut self, top_y: i64) -> Result<(), Failed> {
        if self.active == NIL {
            return Ok(());
        }
        self.build_intersect_list(top_y);
        let n = self.intersects.len();
        if n == 0 {
            return Ok(());
        }
        if n == 1 || self.fixup_intersection_order() {
            for i in 0..n {
                let node = self.intersects[i];
                self.intersect_edges(node.e1, node.e2, node.pt);
                self.swap_in_ael(node.e1, node.e2);
            }
            self.intersects.clear();
        } else {
            return Err(Failed);
        }
        self.sorted = NIL;
        Ok(())
    }

    fn intersect_point(&self, e1: usize, e2: usize) -> Pt {
        let (a, b) = (&self.edges[e1], &self.edges[e2]);
        let mut ip = Pt::default();
        if a.dx == b.dx {
            ip.y = a.curr.y;
            ip.x = self.top_x(e1, ip.y);
            return ip;
        }
        let mut y;
        if a.dx == 0.0 {
            ip.x = a.bot.x;
            y = if b.dx == HORIZONTAL {
                b.bot.y
            } else {
                round(ip.x as f64 / b.dx + b.bot.y as f64 - (b.bot.x as f64 / b.dx))
            };
        } else if b.dx == 0.0 {
            ip.x = b.bot.x;
            y = if a.dx == HORIZONTAL {
                a.bot.y
            } else {
                round(ip.x as f64 / a.dx + a.bot.y as f64 - (a.bot.x as f64 / a.dx))
            };
        } else {
            let b1 = a.bot.x as f64 - a.bot.y as f64 * a.dx;
            let b2 = b.bot.x as f64 - b.bot.y as f64 * b.dx;
            let q = (b2 - b1) / (a.dx - b.dx);
            y = round(q);
            ip.x = if a.dx.abs() < b.dx.abs() { round(a.dx * q + b1) } else { round(b.dx * q + b2) };
        }
        ip.y = y;
        if y < a.top.y || y < b.top.y {
            ip.y = if a.top.y > b.top.y { a.top.y } else { b.top.y };
            y = ip.y;
            ip.x = self.top_x(if a.dx.abs() < b.dx.abs() { e1 } else { e2 }, ip.y);
        }
        if y > a.curr.y {
            ip.y = a.curr.y;
            ip.x = self.top_x(if a.dx.abs() > b.dx.abs() { e2 } else { e1 }, ip.y);
        }
        ip
    }

    fn build_intersect_list(&mut self, top_y: i64) {
        if self.active == NIL {
            return;
        }
        let mut e = self.active;
        self.sorted = e;
        while e != NIL {
            self.edges[e].prev_sel = self.edges[e].prev_ael;
            self.edges[e].next_sel = self.edges[e].next_ael;
            self.edges[e].curr.x = self.top_x(e, top_y);
            e = self.edges[e].next_ael;
        }
        loop {
            let mut modified = false;
            e = self.sorted;
            while self.edges[e].next_sel != NIL {
                let next = self.edges[e].next_sel;
                if self.edges[e].curr.x > self.edges[next].curr.x {
                    let mut pt = self.intersect_point(e, next);
                    if pt.y < top_y {
                        pt = Pt::new(self.top_x(e, top_y), top_y);
                    }
                    self.intersects.push(Intersect { e1: e, e2: next, pt });
                    self.swap_in_sel(e, next);
                    modified = true;
                } else {
                    e = next;
                }
            }
            let prev = self.edges[e].prev_sel;
            if prev != NIL {
                self.edges[prev].next_sel = NIL;
            } else {
                break;
            }
            if !modified {
                break;
            }
        }
        self.sorted = NIL;
    }

    fn edges_adjacent(&self, n: &Intersect) -> bool {
        self.edges[n.e1].next_sel == n.e2 || self.edges[n.e1].prev_sel == n.e2
    }

    fn fixup_intersection_order(&mut self) -> bool {
        self.copy_ael_to_sel();
        std_sort(&mut self.intersects, &|a: &Intersect, b: &Intersect| b.pt.y < a.pt.y);
        let cnt = self.intersects.len();
        for i in 0..cnt {
            if !self.edges_adjacent(&self.intersects[i]) {
                let mut j = i + 1;
                while j < cnt && !self.edges_adjacent(&self.intersects[j]) {
                    j += 1;
                }
                if j == cnt {
                    return false;
                }
                self.intersects.swap(i, j);
            }
            let n = self.intersects[i];
            self.swap_in_sel(n.e1, n.e2);
        }
        true
    }

    fn do_maxima(&mut self, e: usize) -> Result<(), Failed> {
        let max_pair = self.maxima_pair_ex(e);
        if max_pair == NIL {
            if self.edges[e].out_idx >= 0 {
                self.add_out_pt(e, self.edges[e].top);
            }
            self.delete_from_ael(e);
            return Ok(());
        }
        let mut next = self.edges[e].next_ael;
        while next != NIL && next != max_pair {
            self.intersect_edges(e, next, self.edges[e].top);
            self.swap_in_ael(e, next);
            next = self.edges[e].next_ael;
        }
        let (a, b) = (self.edges[e].out_idx, self.edges[max_pair].out_idx);
        if a == UNASSIGNED && b == UNASSIGNED {
            self.delete_from_ael(e);
            self.delete_from_ael(max_pair);
        } else if a >= 0 && b >= 0 {
            self.add_local_max_poly(e, max_pair, self.edges[e].top);
            self.delete_from_ael(e);
            self.delete_from_ael(max_pair);
        } else if self.edges[e].wind_delta == 0 {
            if self.edges[e].out_idx >= 0 {
                self.add_out_pt(e, self.edges[e].top);
                self.edges[e].out_idx = UNASSIGNED;
            }
            self.delete_from_ael(e);
            if self.edges[max_pair].out_idx >= 0 {
                self.add_out_pt(max_pair, self.edges[e].top);
                self.edges[max_pair].out_idx = UNASSIGNED;
            }
            self.delete_from_ael(max_pair);
        } else {
            return Err(Failed);
        }
        Ok(())
    }

    fn process_top(&mut self, top_y: i64) -> Result<(), Failed> {
        let mut e = self.active;
        while e != NIL {
            let mut is_max = self.is_maxima(e, top_y);
            if is_max {
                let pair = self.maxima_pair_ex(e);
                is_max = pair == NIL || !self.is_horizontal(pair);
            }
            if is_max {
                if self.strict_simple {
                    self.maxima.push(self.edges[e].top.x);
                }
                let prev = self.edges[e].prev_ael;
                self.do_maxima(e)?;
                e = if prev == NIL { self.active } else { self.edges[prev].next_ael };
            } else {
                if self.is_intermediate(e, top_y) && self.is_horizontal(self.edges[e].next_lml) {
                    e = self.update_edge_into_ael(e)?;
                    if self.edges[e].out_idx >= 0 {
                        self.add_out_pt(e, self.edges[e].bot);
                    }
                    self.add_edge_to_sel(e);
                } else {
                    self.edges[e].curr.x = self.top_x(e, top_y);
                    self.edges[e].curr.y = top_y;
                }
                if self.strict_simple {
                    let prev = self.edges[e].prev_ael;
                    if self.edges[e].out_idx >= 0
                        && self.edges[e].wind_delta != 0
                        && prev != NIL
                        && self.edges[prev].out_idx >= 0
                        && self.edges[prev].curr.x == self.edges[e].curr.x
                        && self.edges[prev].wind_delta != 0
                    {
                        let pt = self.edges[e].curr;
                        let op = self.add_out_pt(prev, pt);
                        let op2 = self.add_out_pt(e, pt);
                        self.joins.push(Join { out1: op, out2: op2, off: pt });
                    }
                }
                e = self.edges[e].next_ael;
            }
        }

        self.maxima.sort_unstable();
        self.process_horizontals()?;
        self.maxima.clear();

        e = self.active;
        while e != NIL {
            if self.is_intermediate(e, top_y) {
                let mut op = NIL;
                if self.edges[e].out_idx >= 0 {
                    op = self.add_out_pt(e, self.edges[e].top);
                }
                e = self.update_edge_into_ael(e)?;
                let ed = self.edges[e];
                let along = |c: &Self, n: usize| {
                    n != NIL
                        && c.edges[n].curr == ed.bot
                        && op != NIL
                        && c.edges[n].out_idx >= 0
                        && c.edges[n].curr.y > c.edges[n].top.y
                        && slopes4(ed.curr, ed.top, c.edges[n].curr, c.edges[n].top, c.full)
                        && ed.wind_delta != 0
                        && c.edges[n].wind_delta != 0
                };
                if along(self, ed.prev_ael) {
                    let op2 = self.add_out_pt(ed.prev_ael, ed.bot);
                    self.joins.push(Join { out1: op, out2: op2, off: ed.top });
                } else if along(self, ed.next_ael) {
                    let op2 = self.add_out_pt(ed.next_ael, ed.bot);
                    self.joins.push(Join { out1: op, out2: op2, off: ed.top });
                }
            }
            e = self.edges[e].next_ael;
        }
        Ok(())
    }

    fn fixup_polyline(&mut self, rec: usize) {
        let mut pp = self.recs[rec].pts;
        let mut last = self.pts[pp].prev;
        while pp != last {
            pp = self.pts[pp].next;
            if self.pts[pp].pt == self.pts[self.pts[pp].prev].pt {
                if pp == last {
                    last = self.pts[pp].prev;
                }
                let tmp = self.pts[pp].prev;
                let next = self.pts[pp].next;
                self.pts[tmp].next = next;
                self.pts[next].prev = tmp;
                pp = tmp;
            }
        }
        if pp == self.pts[pp].prev {
            self.recs[rec].pts = NIL;
        }
    }

    fn fixup_polygon(&mut self, rec: usize) {
        let mut last_ok = NIL;
        self.recs[rec].bottom = NIL;
        let mut pp = self.recs[rec].pts;
        let preserve = self.preserve_collinear || self.strict_simple;
        loop {
            let (prev, next) = (self.pts[pp].prev, self.pts[pp].next);
            if prev == pp || prev == next {
                self.recs[rec].pts = NIL;
                return;
            }
            let (a, b, c) = (self.pts[prev].pt, self.pts[pp].pt, self.pts[next].pt);
            if b == c || b == a || (slopes3(a, b, c, self.full) && (!preserve || !pt2_between(a, b, c))) {
                last_ok = NIL;
                self.pts[prev].next = next;
                self.pts[next].prev = prev;
                pp = prev;
            } else if pp == last_ok {
                break;
            } else {
                if last_ok == NIL {
                    last_ok = pp;
                }
                pp = next;
            }
        }
        self.recs[rec].pts = pp;
    }

    fn point_count(&self, pts: usize) -> usize {
        if pts == NIL {
            return 0;
        }
        let mut n = 0;
        let mut p = pts;
        loop {
            n += 1;
            p = self.pts[p].next;
            if p == pts {
                return n;
            }
        }
    }

    fn contour(&self, pts: usize, cnt: usize) -> Path {
        let mut out = Vec::with_capacity(cnt);
        let mut p = self.pts[pts].prev;
        for _ in 0..cnt {
            out.push(self.pts[p].pt);
            p = self.pts[p].prev;
        }
        out
    }

    fn build_result(&self) -> Paths {
        let mut polys = Vec::with_capacity(self.recs.len());
        for r in &self.recs {
            if r.pts == NIL {
                continue;
            }
            let cnt = self.point_count(self.pts[r.pts].prev);
            if cnt < 2 {
                continue;
            }
            polys.push(self.contour(r.pts, cnt));
        }
        polys
    }

    fn build_result_tree(&mut self) -> PolyTree {
        let mut tree = PolyTree::default();
        tree.nodes.reserve(self.recs.len());
        for i in 0..self.recs.len() {
            let r = self.recs[i];
            let cnt = self.point_count(r.pts);
            if (r.is_open && cnt < 2) || (!r.is_open && cnt < 3) {
                continue;
            }
            if r.first_left != NIL
                && (r.is_hole == self.recs[r.first_left].is_hole || self.recs[r.first_left].pts == NIL)
            {
                let mut left = r.first_left;
                while left != NIL && (self.recs[left].is_hole == r.is_hole || self.recs[left].pts == NIL) {
                    left = self.recs[left].first_left;
                }
                self.recs[i].first_left = left;
            }
            self.recs[i].node = tree.nodes.len();
            tree.nodes.push(PolyNode { contour: self.contour(r.pts, cnt), children: Vec::new(), open: false });
        }
        tree.children.reserve(self.recs.len());
        for i in 0..self.recs.len() {
            let r = self.recs[i];
            if r.node == NIL {
                continue;
            }
            if r.is_open {
                tree.nodes[r.node].open = true;
                tree.children.push(r.node);
            } else if r.first_left != NIL && self.recs[r.first_left].node != NIL {
                let parent = self.recs[r.first_left].node;
                tree.nodes[parent].children.push(r.node);
            } else {
                tree.children.push(r.node);
            }
        }
        tree
    }

    fn e2_inserts_before_e1(&self, e1: usize, e2: usize) -> bool {
        let (a, b) = (&self.edges[e1], &self.edges[e2]);
        if b.curr.x == a.curr.x {
            if b.top.y > a.top.y { b.top.x < self.top_x(e1, b.top.y) } else { a.top.x > self.top_x(e2, a.top.y) }
        } else {
            b.curr.x < a.curr.x
        }
    }

    fn insert_edge(&mut self, edge: usize, mut start: usize) {
        if self.active == NIL {
            self.edges[edge].prev_ael = NIL;
            self.edges[edge].next_ael = NIL;
            self.active = edge;
        } else if start == NIL && self.e2_inserts_before_e1(self.active, edge) {
            self.edges[edge].prev_ael = NIL;
            self.edges[edge].next_ael = self.active;
            let a = self.active;
            self.edges[a].prev_ael = edge;
            self.active = edge;
        } else {
            if start == NIL {
                start = self.active;
            }
            while self.edges[start].next_ael != NIL && !self.e2_inserts_before_e1(self.edges[start].next_ael, edge) {
                start = self.edges[start].next_ael;
            }
            let next = self.edges[start].next_ael;
            self.edges[edge].next_ael = next;
            if next != NIL {
                self.edges[next].prev_ael = edge;
            }
            self.edges[edge].prev_ael = start;
            self.edges[start].next_ael = edge;
        }
    }

    fn dup_out_pt(&mut self, op: usize, after: bool) -> usize {
        let (idx, pt) = (self.pts[op].idx, self.pts[op].pt);
        if after {
            let next = self.pts[op].next;
            let r = self.new_pt(idx, pt, next, op);
            self.pts[next].prev = r;
            self.pts[op].next = r;
            r
        } else {
            let prev = self.pts[op].prev;
            let r = self.new_pt(idx, pt, op, prev);
            self.pts[prev].next = r;
            self.pts[op].prev = r;
            r
        }
    }

    /// Walks to where a horizontal join splits one side and duplicates the point there.
    fn join_horz_side(&mut self, mut op: usize, ltr: bool, pt: Pt, discard_left: bool) -> (usize, usize) {
        let after;
        if ltr {
            loop {
                let n = self.pts[self.pts[op].next].pt;
                if n.x <= pt.x && n.x >= self.pts[op].pt.x && n.y == pt.y {
                    op = self.pts[op].next;
                } else {
                    break;
                }
            }
            if discard_left && self.pts[op].pt.x != pt.x {
                op = self.pts[op].next;
            }
            after = !discard_left;
        } else {
            loop {
                let n = self.pts[self.pts[op].next].pt;
                if n.x >= pt.x && n.x <= self.pts[op].pt.x && n.y == pt.y {
                    op = self.pts[op].next;
                } else {
                    break;
                }
            }
            if !discard_left && self.pts[op].pt.x != pt.x {
                op = self.pts[op].next;
            }
            after = discard_left;
        }
        let mut opb = self.dup_out_pt(op, after);
        if self.pts[opb].pt != pt {
            op = opb;
            self.pts[op].pt = pt;
            opb = self.dup_out_pt(op, after);
        }
        (op, opb)
    }

    fn join_horz(&mut self, op1: usize, op1b: usize, op2: usize, op2b: usize, pt: Pt, discard_left: bool) -> bool {
        let dir1 = self.pts[op1].pt.x <= self.pts[op1b].pt.x;
        let dir2 = self.pts[op2].pt.x <= self.pts[op2b].pt.x;
        if dir1 == dir2 {
            return false;
        }
        let (op1, op1b) = self.join_horz_side(op1, dir1, pt, discard_left);
        let (op2, op2b) = self.join_horz_side(op2, dir2, pt, discard_left);
        if dir1 == discard_left {
            self.pts[op1].prev = op2;
            self.pts[op2].next = op1;
            self.pts[op1b].next = op2b;
            self.pts[op2b].prev = op1b;
        } else {
            self.pts[op1].next = op2;
            self.pts[op2].prev = op1;
            self.pts[op1b].prev = op2b;
            self.pts[op2b].next = op1b;
        }
        true
    }

    fn split_at_join(&mut self, j: &mut Join, op1: usize, op2: usize, reverse1: bool) -> bool {
        let (op1b, op2b);
        if reverse1 {
            op1b = self.dup_out_pt(op1, false);
            op2b = self.dup_out_pt(op2, true);
            self.pts[op1].prev = op2;
            self.pts[op2].next = op1;
            self.pts[op1b].next = op2b;
            self.pts[op2b].prev = op1b;
        } else {
            op1b = self.dup_out_pt(op1, true);
            op2b = self.dup_out_pt(op2, false);
            self.pts[op1].next = op2;
            self.pts[op2].prev = op1;
            self.pts[op1b].prev = op2b;
            self.pts[op2b].next = op1b;
        }
        j.out1 = op1;
        j.out2 = op1b;
        true
    }

    fn join_points(&mut self, j: &mut Join, rec1: usize, rec2: usize) -> bool {
        let mut op1 = j.out1;
        let mut op2 = j.out2;
        let horizontal = self.pts[j.out1].pt.y == j.off.y;

        if horizontal && j.off == self.pts[j.out1].pt && j.off == self.pts[j.out2].pt {
            // Strictly simple join.
            if rec1 != rec2 {
                return false;
            }
            let mut op1b = self.pts[op1].next;
            while op1b != op1 && self.pts[op1b].pt == j.off {
                op1b = self.pts[op1b].next;
            }
            let reverse1 = self.pts[op1b].pt.y > j.off.y;
            let mut op2b = self.pts[op2].next;
            while op2b != op2 && self.pts[op2b].pt == j.off {
                op2b = self.pts[op2b].next;
            }
            let reverse2 = self.pts[op2b].pt.y > j.off.y;
            if reverse1 == reverse2 {
                return false;
            }
            self.split_at_join(j, op1, op2, reverse1)
        } else if horizontal {
            let mut op1b = op1;
            while self.pts[self.pts[op1].prev].pt.y == self.pts[op1].pt.y
                && self.pts[op1].prev != op1b
                && self.pts[op1].prev != op2
            {
                op1 = self.pts[op1].prev;
            }
            while self.pts[self.pts[op1b].next].pt.y == self.pts[op1b].pt.y
                && self.pts[op1b].next != op1
                && self.pts[op1b].next != op2
            {
                op1b = self.pts[op1b].next;
            }
            if self.pts[op1b].next == op1 || self.pts[op1b].next == op2 {
                return false;
            }
            let mut op2b = op2;
            while self.pts[self.pts[op2].prev].pt.y == self.pts[op2].pt.y
                && self.pts[op2].prev != op2b
                && self.pts[op2].prev != op1b
            {
                op2 = self.pts[op2].prev;
            }
            while self.pts[self.pts[op2b].next].pt.y == self.pts[op2b].pt.y
                && self.pts[op2b].next != op2
                && self.pts[op2b].next != op1
            {
                op2b = self.pts[op2b].next;
            }
            if self.pts[op2b].next == op2 || self.pts[op2b].next == op1 {
                return false;
            }
            let (a1, a2, b1, b2) = (self.pts[op1].pt.x, self.pts[op1b].pt.x, self.pts[op2].pt.x, self.pts[op2b].pt.x);
            let (left, right) = if a1 < a2 {
                if b1 < b2 { (a1.max(b1), a2.min(b2)) } else { (a1.max(b2), a2.min(b1)) }
            } else if b1 < b2 {
                (a2.max(b1), a1.min(b2))
            } else {
                (a2.max(b2), a1.min(b1))
            };
            if left >= right {
                return false;
            }
            let (pt, discard_left);
            if a1 >= left && a1 <= right {
                pt = self.pts[op1].pt;
                discard_left = a1 > a2;
            } else if b1 >= left && b1 <= right {
                pt = self.pts[op2].pt;
                discard_left = b1 > b2;
            } else if a2 >= left && a2 <= right {
                pt = self.pts[op1b].pt;
                discard_left = a2 > a1;
            } else {
                pt = self.pts[op2b].pt;
                discard_left = b2 > b1;
            }
            j.out1 = op1;
            j.out2 = op2;
            self.join_horz(op1, op1b, op2, op2b, pt, discard_left)
        } else {
            let at1 = self.pts[op1].pt;
            let mut op1b = self.pts[op1].next;
            while self.pts[op1b].pt == at1 && op1b != op1 {
                op1b = self.pts[op1b].next;
            }
            let reverse1 = self.pts[op1b].pt.y > at1.y || !slopes3(at1, self.pts[op1b].pt, j.off, self.full);
            if reverse1 {
                op1b = self.pts[op1].prev;
                while self.pts[op1b].pt == at1 && op1b != op1 {
                    op1b = self.pts[op1b].prev;
                }
                if self.pts[op1b].pt.y > at1.y || !slopes3(at1, self.pts[op1b].pt, j.off, self.full) {
                    return false;
                }
            }
            let at2 = self.pts[op2].pt;
            let mut op2b = self.pts[op2].next;
            while self.pts[op2b].pt == at2 && op2b != op2 {
                op2b = self.pts[op2b].next;
            }
            let reverse2 = self.pts[op2b].pt.y > at2.y || !slopes3(at2, self.pts[op2b].pt, j.off, self.full);
            if reverse2 {
                op2b = self.pts[op2].prev;
                while self.pts[op2b].pt == at2 && op2b != op2 {
                    op2b = self.pts[op2b].prev;
                }
                if self.pts[op2b].pt.y > at2.y || !slopes3(at2, self.pts[op2b].pt, j.off, self.full) {
                    return false;
                }
            }
            if op1b == op1 || op2b == op2 || op1b == op2b || (rec1 == rec2 && reverse1 == reverse2) {
                return false;
            }
            self.split_at_join(j, op1, op2, reverse1)
        }
    }

    fn parse_first_left(&self, mut left: usize) -> usize {
        while left != NIL && self.recs[left].pts == NIL {
            left = self.recs[left].first_left;
        }
        left
    }

    fn point_in_out_polygon(&self, pt: Pt, op: usize) -> i32 {
        let mut result = 0;
        let start = op;
        let mut op = op;
        loop {
            let cur = self.pts[op].pt;
            let next = self.pts[self.pts[op].next].pt;
            if next.y == pt.y && (next.x == pt.x || (cur.y == pt.y && ((next.x > pt.x) == (cur.x < pt.x)))) {
                return -1;
            }
            if (cur.y < pt.y) != (next.y < pt.y) {
                let d =
                    || (cur.x - pt.x) as f64 * (next.y - pt.y) as f64 - (next.x - pt.x) as f64 * (cur.y - pt.y) as f64;
                if cur.x >= pt.x {
                    if next.x > pt.x {
                        result = 1 - result;
                    } else {
                        let d = d();
                        if d == 0.0 {
                            return -1;
                        }
                        if (d > 0.0) == (next.y > cur.y) {
                            result = 1 - result;
                        }
                    }
                } else if next.x > pt.x {
                    let d = d();
                    if d == 0.0 {
                        return -1;
                    }
                    if (d > 0.0) == (next.y > cur.y) {
                        result = 1 - result;
                    }
                }
            }
            op = self.pts[op].next;
            if op == start {
                return result;
            }
        }
    }

    fn poly2_contains_poly1(&self, out1: usize, out2: usize) -> bool {
        let mut op = out1;
        loop {
            let res = self.point_in_out_polygon(self.pts[op].pt, out2);
            if res >= 0 {
                return res > 0;
            }
            op = self.pts[op].next;
            if op == out1 {
                return true;
            }
        }
    }

    fn fixup_first_lefts1(&mut self, old: usize, new: usize) {
        for i in 0..self.recs.len() {
            let r = self.recs[i];
            let left = self.parse_first_left(r.first_left);
            if r.pts != NIL && left == old && self.poly2_contains_poly1(r.pts, self.recs[new].pts) {
                self.recs[i].first_left = new;
            }
        }
    }

    fn fixup_first_lefts2(&mut self, inner: usize, outer: usize) {
        let orfl = self.recs[outer].first_left;
        for i in 0..self.recs.len() {
            let r = self.recs[i];
            if r.pts == NIL || i == outer || i == inner {
                continue;
            }
            let left = self.parse_first_left(r.first_left);
            if left != orfl && left != inner && left != outer {
                continue;
            }
            if self.poly2_contains_poly1(r.pts, self.recs[inner].pts) {
                self.recs[i].first_left = inner;
            } else if self.poly2_contains_poly1(r.pts, self.recs[outer].pts) {
                self.recs[i].first_left = outer;
            } else if r.first_left == inner || r.first_left == outer {
                self.recs[i].first_left = orfl;
            }
        }
    }

    fn fixup_first_lefts3(&mut self, old: usize, new: usize) {
        for i in 0..self.recs.len() {
            let r = self.recs[i];
            let left = self.parse_first_left(r.first_left);
            if r.pts != NIL && left == old {
                self.recs[i].first_left = new;
            }
        }
    }

    fn update_out_pt_idxs(&mut self, rec: usize) {
        let start = self.recs[rec].pts;
        let idx = self.recs[rec].idx;
        let mut op = start;
        loop {
            self.pts[op].idx = idx;
            op = self.pts[op].prev;
            if op == start {
                break;
            }
        }
    }

    fn join_common_edges(&mut self) {
        for k in 0..self.joins.len() {
            let mut join = self.joins[k];
            let r1 = self.get_out_rec(self.pts[join.out1].idx);
            let mut r2 = self.get_out_rec(self.pts[join.out2].idx);
            if self.recs[r1].pts == NIL || self.recs[r2].pts == NIL {
                continue;
            }
            if self.recs[r1].is_open || self.recs[r2].is_open {
                continue;
            }
            let hole_state = if r1 == r2 {
                r1
            } else if self.rec1_right_of_rec2(r1, r2) {
                r2
            } else if self.rec1_right_of_rec2(r2, r1) {
                r1
            } else {
                self.get_lowermost_rec(r1, r2)
            };

            let joined = self.join_points(&mut join, r1, r2);
            self.joins[k] = join;
            if !joined {
                continue;
            }

            if r1 == r2 {
                // A polygon split in two.
                self.recs[r1].pts = join.out1;
                self.recs[r1].bottom = NIL;
                r2 = self.create_out_rec();
                self.recs[r2].pts = join.out2;
                self.update_out_pt_idxs(r2);

                if self.poly2_contains_poly1(self.recs[r2].pts, self.recs[r1].pts) {
                    self.recs[r2].is_hole = !self.recs[r1].is_hole;
                    self.recs[r2].first_left = r1;
                    if self.using_tree {
                        self.fixup_first_lefts2(r2, r1);
                    }
                    if (self.recs[r2].is_hole ^ self.reverse_output) == (self.area_of(self.recs[r2].pts) > 0.0) {
                        self.reverse_links(self.recs[r2].pts);
                    }
                } else if self.poly2_contains_poly1(self.recs[r1].pts, self.recs[r2].pts) {
                    self.recs[r2].is_hole = self.recs[r1].is_hole;
                    self.recs[r1].is_hole = !self.recs[r2].is_hole;
                    self.recs[r2].first_left = self.recs[r1].first_left;
                    self.recs[r1].first_left = r2;
                    if self.using_tree {
                        self.fixup_first_lefts2(r1, r2);
                    }
                    if (self.recs[r1].is_hole ^ self.reverse_output) == (self.area_of(self.recs[r1].pts) > 0.0) {
                        self.reverse_links(self.recs[r1].pts);
                    }
                } else {
                    self.recs[r2].is_hole = self.recs[r1].is_hole;
                    self.recs[r2].first_left = self.recs[r1].first_left;
                    if self.using_tree {
                        self.fixup_first_lefts1(r1, r2);
                    }
                }
            } else {
                // Two polygons joined.
                self.recs[r2].pts = NIL;
                self.recs[r2].bottom = NIL;
                self.recs[r2].idx = self.recs[r1].idx;
                self.recs[r1].is_hole = self.recs[hole_state].is_hole;
                if hole_state == r2 {
                    self.recs[r1].first_left = self.recs[r2].first_left;
                }
                self.recs[r2].first_left = r1;
                if self.using_tree {
                    self.fixup_first_lefts3(r2, r1);
                }
            }
        }
    }

    fn do_simple_polygons(&mut self) {
        let mut i = 0;
        while i < self.recs.len() {
            let rec = i;
            i += 1;
            let mut op = self.recs[rec].pts;
            if op == NIL || self.recs[rec].is_open {
                continue;
            }
            loop {
                let mut op2 = self.pts[op].next;
                while op2 != self.recs[rec].pts {
                    if self.pts[op].pt == self.pts[op2].pt && self.pts[op2].next != op && self.pts[op2].prev != op {
                        let op3 = self.pts[op].prev;
                        let op4 = self.pts[op2].prev;
                        self.pts[op].prev = op4;
                        self.pts[op4].next = op;
                        self.pts[op2].prev = op3;
                        self.pts[op3].next = op2;

                        self.recs[rec].pts = op;
                        let rec2 = self.create_out_rec();
                        self.recs[rec2].pts = op2;
                        self.update_out_pt_idxs(rec2);
                        if self.poly2_contains_poly1(self.recs[rec2].pts, self.recs[rec].pts) {
                            self.recs[rec2].is_hole = !self.recs[rec].is_hole;
                            self.recs[rec2].first_left = rec;
                            if self.using_tree {
                                self.fixup_first_lefts2(rec2, rec);
                            }
                        } else if self.poly2_contains_poly1(self.recs[rec].pts, self.recs[rec2].pts) {
                            self.recs[rec2].is_hole = self.recs[rec].is_hole;
                            self.recs[rec].is_hole = !self.recs[rec2].is_hole;
                            self.recs[rec2].first_left = self.recs[rec].first_left;
                            self.recs[rec].first_left = rec2;
                            if self.using_tree {
                                self.fixup_first_lefts2(rec, rec2);
                            }
                        } else {
                            self.recs[rec2].is_hole = self.recs[rec].is_hole;
                            self.recs[rec2].first_left = self.recs[rec].first_left;
                            if self.using_tree {
                                self.fixup_first_lefts1(rec, rec2);
                            }
                        }
                        op2 = op;
                    }
                    op2 = self.pts[op2].next;
                }
                op = self.pts[op].next;
                if op == self.recs[rec].pts {
                    break;
                }
            }
        }
    }
}

// ---- ClipperOffset ----------------------------------------------------------------------

#[derive(Clone, Copy, Default)]
struct Normal {
    x: f64,
    y: f64,
}

fn unit_normal(a: Pt, b: Pt) -> Normal {
    if a == b {
        return Normal::default();
    }
    let mut dx = (b.x - a.x) as f64;
    let mut dy = (b.y - a.y) as f64;
    let f = 1.0 / (dx * dx + dy * dy).sqrt();
    dx *= f;
    dy *= f;
    Normal { x: dy, y: -dx }
}

struct OffsetNode {
    contour: Path,
    join: JoinType,
    end: EndType,
}

/// Offsets paths. `shortest_edge` is Orca's addition: points nearer than it are dropped first.
pub struct ClipperOffset {
    pub miter_limit: f64,
    pub arc_tolerance: f64,
    pub shortest_edge: f64,
    nodes: Vec<OffsetNode>,
    lowest: Option<(usize, usize)>,
    dest_polys: Paths,
    src: Path,
    dest: Path,
    normals: Vec<Normal>,
    delta: f64,
    sin_a: f64,
    sin: f64,
    cos: f64,
    miter_lim: f64,
    steps_per_rad: f64,
}

impl Default for ClipperOffset {
    fn default() -> Self {
        Self::new()
    }
}

impl ClipperOffset {
    pub fn new() -> Self {
        Self {
            miter_limit: 2.0,
            arc_tolerance: 0.25,
            shortest_edge: 0.0,
            nodes: Vec::new(),
            lowest: None,
            dest_polys: Vec::new(),
            src: Vec::new(),
            dest: Vec::new(),
            normals: Vec::new(),
            delta: 0.0,
            sin_a: 0.0,
            sin: 0.0,
            cos: 0.0,
            miter_lim: 0.0,
            steps_per_rad: 0.0,
        }
    }

    pub fn add_path(&mut self, path: &[Pt], join: JoinType, end: EndType) {
        if path.is_empty() {
            return;
        }
        let mut high = path.len() - 1;
        let has_shortest = self.shortest_edge > 0.0;
        let shortest2 = if has_shortest { self.shortest_edge * self.shortest_edge } else { 0.0 };
        let same = |a: Pt, b: Pt| {
            if has_shortest {
                let dx = (a.x - b.x) as f64;
                let dy = (a.y - b.y) as f64;
                dx * dx + dy * dy < shortest2
            } else {
                a == b
            }
        };
        if matches!(end, EndType::ClosedLine | EndType::ClosedPolygon) {
            while high > 0 && same(path[high], path[0]) {
                high -= 1;
            }
        }
        let mut contour = Vec::with_capacity(high + 1);
        contour.push(path[0]);
        let (mut j, mut k) = (0, 0);
        for &p in path.iter().take(high + 1).skip(1) {
            if same(p, contour[j]) {
                continue;
            }
            j += 1;
            contour.push(p);
            if p.y > contour[k].y || (p.y == contour[k].y && p.x < contour[k].x) {
                k = j;
            }
        }
        if end == EndType::ClosedPolygon && j < 2 {
            return;
        }
        self.nodes.push(OffsetNode { contour, join, end });
        if end != EndType::ClosedPolygon {
            return;
        }
        let at = self.nodes.len() - 1;
        let low = self.nodes[at].contour[k];
        match self.lowest {
            None => self.lowest = Some((at, k)),
            Some((n, i)) => {
                let ip = self.nodes[n].contour[i];
                if low.y > ip.y || (low.y == ip.y && low.x < ip.x) {
                    self.lowest = Some((at, k));
                }
            }
        }
    }

    fn fix_orientations(&mut self) {
        let wrong = self.lowest.is_some_and(|(n, _)| !orientation(&self.nodes[n].contour));
        for node in &mut self.nodes {
            let reverse = if wrong {
                node.end == EndType::ClosedPolygon || (node.end == EndType::ClosedLine && orientation(&node.contour))
            } else {
                node.end == EndType::ClosedLine && !orientation(&node.contour)
            };
            if reverse {
                node.contour.reverse();
            }
        }
    }

    /// The offset as plain loops.
    pub fn execute(&mut self, delta: f64) -> Result<Paths, Failed> {
        self.fix_orientations();
        self.do_offset(delta);
        let mut c = Clipper::new();
        c.add_paths(&self.dest_polys, PolyType::Subject, true)?;
        if delta > 0.0 {
            c.execute(ClipType::Union, FillType::Positive, FillType::Positive)
        } else {
            let r = c.bounds();
            c.add_path(&outer_rect(r), PolyType::Subject, true)?;
            c.reverse_solution(true);
            let mut out = c.execute(ClipType::Union, FillType::Negative, FillType::Negative)?;
            if !out.is_empty() {
                out.remove(0);
            }
            Ok(out)
        }
    }

    fn do_offset(&mut self, delta: f64) {
        self.dest_polys.clear();
        self.delta = delta;
        if near_zero(delta) {
            for node in &self.nodes {
                if node.end == EndType::ClosedPolygon {
                    self.dest_polys.push(node.contour.clone());
                }
            }
            return;
        }
        self.miter_lim = if self.miter_limit > 2.0 { 2.0 / (self.miter_limit * self.miter_limit) } else { 0.5 };
        let y = if self.arc_tolerance <= 0.0 {
            DEF_ARC_TOLERANCE
        } else if self.arc_tolerance > delta.abs() * DEF_ARC_TOLERANCE {
            delta.abs() * DEF_ARC_TOLERANCE
        } else {
            self.arc_tolerance
        };
        let mut steps = PI / (1.0 - y / delta.abs()).acos();
        if steps > delta.abs() * PI {
            steps = delta.abs() * PI;
        }
        self.sin = (TWO_PI / steps).sin();
        self.cos = (TWO_PI / steps).cos();
        self.steps_per_rad = steps / TWO_PI;
        if delta < 0.0 {
            self.sin = -self.sin;
        }

        for n in 0..self.nodes.len() {
            let (join, end) = (self.nodes[n].join, self.nodes[n].end);
            self.src = self.nodes[n].contour.clone();
            let len = self.src.len();
            if len == 0 || (delta <= 0.0 && (len < 3 || end != EndType::ClosedPolygon)) {
                continue;
            }
            self.dest.clear();
            if len == 1 {
                let c = self.src[0];
                if join == JoinType::Round {
                    let (mut x, mut y) = (1.0, 0.0);
                    let mut j = 1;
                    while (j as f64) <= steps {
                        self.dest.push(Pt::new(round(c.x as f64 + x * delta), round(c.y as f64 + y * delta)));
                        let x2 = x;
                        x = x * self.cos - self.sin * y;
                        y = x2 * self.sin + y * self.cos;
                        j += 1;
                    }
                } else {
                    let (mut x, mut y) = (-1.0, -1.0);
                    for _ in 0..4 {
                        self.dest.push(Pt::new(round(c.x as f64 + x * delta), round(c.y as f64 + y * delta)));
                        if x < 0.0 {
                            x = 1.0;
                        } else if y < 0.0 {
                            y = 1.0;
                        } else {
                            x = -1.0;
                        }
                    }
                }
                self.dest_polys.push(std::mem::take(&mut self.dest));
                continue;
            }

            self.normals.clear();
            for j in 0..len - 1 {
                self.normals.push(unit_normal(self.src[j], self.src[j + 1]));
            }
            if matches!(end, EndType::ClosedLine | EndType::ClosedPolygon) {
                self.normals.push(unit_normal(self.src[len - 1], self.src[0]));
            } else {
                self.normals.push(self.normals[len - 2]);
            }

            match end {
                EndType::ClosedPolygon => {
                    let mut k = len - 1;
                    for j in 0..len {
                        self.offset_point(j, &mut k, join);
                    }
                    self.dest_polys.push(std::mem::take(&mut self.dest));
                }
                EndType::ClosedLine => {
                    let mut k = len - 1;
                    for j in 0..len {
                        self.offset_point(j, &mut k, join);
                    }
                    self.dest_polys.push(std::mem::take(&mut self.dest));
                    let last = self.normals[len - 1];
                    for j in (1..len).rev() {
                        self.normals[j] = Normal { x: -self.normals[j - 1].x, y: -self.normals[j - 1].y };
                    }
                    self.normals[0] = Normal { x: -last.x, y: -last.y };
                    k = 0;
                    for j in (0..len).rev() {
                        self.offset_point(j, &mut k, join);
                    }
                    self.dest_polys.push(std::mem::take(&mut self.dest));
                }
                _ => {
                    let mut k = 0;
                    for j in 1..len - 1 {
                        self.offset_point(j, &mut k, join);
                    }
                    if end == EndType::OpenButt {
                        let j = len - 1;
                        let (s, m) = (self.src[j], self.normals[j]);
                        self.dest.push(Pt::new(round(s.x as f64 + m.x * delta), round(s.y as f64 + m.y * delta)));
                        self.dest.push(Pt::new(round(s.x as f64 - m.x * delta), round(s.y as f64 - m.y * delta)));
                    } else {
                        let j = len - 1;
                        k = len - 2;
                        self.sin_a = 0.0;
                        self.normals[j] = Normal { x: -self.normals[j].x, y: -self.normals[j].y };
                        if end == EndType::OpenSquare {
                            self.do_square(j, k);
                        } else {
                            self.do_round(j, k);
                        }
                    }

                    for j in (1..len).rev() {
                        self.normals[j] = Normal { x: -self.normals[j - 1].x, y: -self.normals[j - 1].y };
                    }
                    self.normals[0] = Normal { x: -self.normals[1].x, y: -self.normals[1].y };

                    k = len - 1;
                    for j in (1..k).rev() {
                        self.offset_point(j, &mut k, join);
                    }

                    if end == EndType::OpenButt {
                        let (s, m) = (self.src[0], self.normals[0]);
                        self.dest.push(Pt::new(round(s.x as f64 - m.x * delta), round(s.y as f64 - m.y * delta)));
                        self.dest.push(Pt::new(round(s.x as f64 + m.x * delta), round(s.y as f64 + m.y * delta)));
                    } else {
                        self.sin_a = 0.0;
                        if end == EndType::OpenSquare {
                            self.do_square(0, 1);
                        } else {
                            self.do_round(0, 1);
                        }
                    }
                    self.dest_polys.push(std::mem::take(&mut self.dest));
                }
            }
        }
    }

    fn offset_point(&mut self, j: usize, k: &mut usize, join: JoinType) {
        let (nk, nj, s, delta) = (self.normals[*k], self.normals[j], self.src[j], self.delta);
        self.sin_a = nk.x * nj.y - nj.x * nk.y;
        if (self.sin_a * delta).abs() < 1.0 {
            let cos_a = nk.x * nj.x + nj.y * nk.y;
            if cos_a > 0.0 {
                self.dest.push(Pt::new(round(s.x as f64 + nk.x * delta), round(s.y as f64 + nk.y * delta)));
                return;
            }
        } else if self.sin_a > 1.0 {
            self.sin_a = 1.0;
        } else if self.sin_a < -1.0 {
            self.sin_a = -1.0;
        }

        if self.sin_a * delta < 0.0 {
            self.dest.push(Pt::new(round(s.x as f64 + nk.x * delta), round(s.y as f64 + nk.y * delta)));
            self.dest.push(s);
            self.dest.push(Pt::new(round(s.x as f64 + nj.x * delta), round(s.y as f64 + nj.y * delta)));
        } else {
            match join {
                JoinType::Miter => {
                    let r = 1.0 + (nj.x * nk.x + nj.y * nk.y);
                    if r >= self.miter_lim {
                        let q = delta / r;
                        self.dest.push(Pt::new(
                            round(s.x as f64 + (nk.x + nj.x) * q),
                            round(s.y as f64 + (nk.y + nj.y) * q),
                        ));
                    } else {
                        self.do_square(j, *k);
                    }
                }
                JoinType::Square => self.do_square(j, *k),
                JoinType::Round => self.do_round(j, *k),
            }
        }
        *k = j;
    }

    fn do_square(&mut self, j: usize, k: usize) {
        let (nk, nj, s, delta) = (self.normals[k], self.normals[j], self.src[j], self.delta);
        let dx = (self.sin_a.atan2(nk.x * nj.x + nk.y * nj.y) / 4.0).tan();
        self.dest.push(Pt::new(
            round(s.x as f64 + delta * (nk.x - nk.y * dx)),
            round(s.y as f64 + delta * (nk.y + nk.x * dx)),
        ));
        self.dest.push(Pt::new(
            round(s.x as f64 + delta * (nj.x + nj.y * dx)),
            round(s.y as f64 + delta * (nj.y - nj.x * dx)),
        ));
    }

    fn do_round(&mut self, j: usize, k: usize) {
        let (nk, nj, s, delta) = (self.normals[k], self.normals[j], self.src[j], self.delta);
        let a = self.sin_a.atan2(nk.x * nj.x + nk.y * nj.y);
        // Clipper keeps this in an int; a saturated cast is only reached on absurd offsets.
        let steps = i32::try_from(round(self.steps_per_rad * a.abs())).unwrap_or(i32::MAX).max(1);
        let (mut x, mut y) = (nk.x, nk.y);
        for _ in 0..steps {
            self.dest.push(Pt::new(round(s.x as f64 + x * delta), round(s.y as f64 + y * delta)));
            let x2 = x;
            x = x * self.cos - self.sin * y;
            y = x2 * self.sin + y * self.cos;
        }
        self.dest.push(Pt::new(round(s.x as f64 + nj.x * delta), round(s.y as f64 + nj.y * delta)));
    }
}

/// The rectangle a negative offset wraps round its loops, 10 units out.
pub fn outer_rect(r: Rect) -> Path {
    vec![
        Pt::new(r.left - 10, r.bottom + 10),
        Pt::new(r.right + 10, r.bottom + 10),
        Pt::new(r.right + 10, r.top - 10),
        Pt::new(r.left - 10, r.top - 10),
    ]
}
