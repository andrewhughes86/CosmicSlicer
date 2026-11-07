//! OrcaSlicer's `FillRectilinear::fill_surface_by_lines`: the rectilinear, monotonic and
//! monotonic line fills of solid surfaces, bridges and line infill, in Orca's integers.
//!
//! The region turns so the lines are upright, straight vertical lines are cut by its contours
//! (each cut an exact rational number), and each cut learns its neighbours along the contour.
//! Rectilinear fills then walk that graph left to right; monotonic ones split it into regions and
//! order them with Orca's ant colony, down to its generator's seed, so lines land on the same
//! nanometre and in the same order.
//!
//! Orca's asserts and its `InfillFailedException` both come out as `None` here.

use std::collections::HashMap;

use crate::clipper::{Path, Pt};
use crate::connect::std_sort;
use crate::geom;
use crate::seam::Mt64;

/// `INFILL_OVERLAP_OVER_SPACING`.
const OVERLAP_OVER_SPACING: f32 = 0.45;
/// `EPSILON`.
const EPSILON: f64 = 1e-4;
/// `SCALED_EPSILON`, truncated.
const SCALED_EPSILON: i64 = 100;
/// `SCALING_FACTOR`.
const SCALING: f64 = 1e-6;
/// Most vertical lines one region may be cut by.
const MAX_LINES: i64 = 2_000_000;

/// What one fill needs: `Fill`'s and `FillParams`' members.
#[derive(Clone, Copy, Debug)]
pub struct Job {
    /// `Fill::spacing`, mm.
    pub spacing: f64,
    /// 0..1.
    pub density: f32,
    /// The first of `_infill_direction`'s pair.
    pub angle: f32,
    /// `FillParams::dont_connect`.
    pub dont_connect: bool,
    pub monotonic: bool,
    pub dont_adjust: bool,
}

/// `angle` for a layer: `_infill_direction`, with the fill's `base` angle and whether this
/// layer turns a quarter on from it.
pub fn direction(base: f64, turned: bool) -> f32 {
    let quarter = std::f64::consts::FRAC_PI_2 as f32;
    let mut out = base as f32;
    if turned {
        out += quarter;
    }
    out + quarter
}

/// The lines of a fill and the spacing they ended up at (`Fill::spacing`), mm.
#[derive(Clone, Debug, Default)]
pub struct Filled {
    pub lines: Vec<Path>,
    pub spacing: f64,
}

/// `Fill::_adjust_solid_spacing`.
fn adjust_solid_spacing(width: i64, distance: i64) -> i64 {
    let intervals = ((width as f64 - EPSILON) / distance as f64) as i64;
    let mut new = if intervals == 0 { distance } else { ((width as f64 - EPSILON) / intervals as f64) as i64 };
    if new as f64 / distance as f64 > 1.2 {
        new = (distance as f64 * 1.2 + 0.5).floor() as i64;
    }
    new
}

/// `coord_t(scale_(spacing) / density)`.
fn scaled_spacing(job: &Job) -> i64 {
    (job.spacing / SCALING / f64::from(job.density)) as i64
}

/// `align_to_grid`.
fn align_to_grid(coord: i64, spacing: i64) -> i64 {
    if coord < 0 { ((coord - spacing + 1) / spacing) * spacing } else { (coord / spacing) * spacing }
}

/// `MultiPoint::rotate`.
pub(crate) fn rotate(points: &mut [Pt], angle: f64) {
    let (s, c) = angle.sin_cos();
    for p in points {
        let (x, y) = (p.x as f64, p.y as f64);
        p.x = (c * x - s * y).round() as i64;
        p.y = (c * y + s * x).round() as i64;
    }
}

/// `MultiPoint::remove_duplicate_points`.
fn remove_duplicates(points: &mut Path) {
    points.dedup();
}

/// `is_stick`.
fn is_stick(p1: Pt, p2: Pt, p3: Pt) -> bool {
    let v1 = (p2.x - p1.x, p2.y - p1.y);
    let v2 = (p3.x - p2.x, p3.y - p2.y);
    let dir = i128::from(v1.0) * i128::from(v2.0) + i128::from(v1.1) * i128::from(v2.1);
    if dir > 0 {
        return false;
    }
    let (a, b) = ((v1.0 as f64, v1.1 as f64), (v2.0 as f64, v2.1 as f64));
    let l1 = a.0 * a.0 + a.1 * a.1;
    let l2 = b.0 * b.0 + b.1 * b.1;
    if dir == 0 {
        return l1 == 0.0 || l2 == 0.0;
    }
    let cross = a.0 * b.1 - b.0 * a.1;
    cross * cross / l1.max(l2) < EPSILON * EPSILON
}

/// `remove_sticks` of a polygon.
fn remove_sticks(poly: &mut Path) {
    let mut j = 1;
    for i in 1..poly.len().saturating_sub(1) {
        let (Some(&a), Some(&b), Some(&c)) = (poly.get(j - 1), poly.get(i), poly.get(i + 1)) else { break };
        if !is_stick(a, b, c) {
            if let Some(slot) = poly.get_mut(j) {
                *slot = b;
            }
            j += 1;
        }
    }
    j += 1;
    if j < poly.len() {
        if let Some(&last) = poly.last()
            && let Some(slot) = poly.get_mut(j - 1)
        {
            *slot = last;
        }
        poly.truncate(j);
    }
    while poly.len() >= 3 {
        let n = poly.len();
        let (Some(&a), Some(&b), Some(&c)) = (poly.get(n - 2), poly.get(n - 1), poly.first()) else { break };
        if !is_stick(a, b, c) {
            break;
        }
        poly.pop();
    }
}

/// `Polygon::area`, doubled and halved as Orca sums it.
fn area(poly: &[Pt]) -> f64 {
    let n = poly.len();
    if n < 3 {
        return 0.0;
    }
    let mut a = 0.0;
    let mut j = n - 1;
    for (i, p) in poly.iter().enumerate() {
        let q = poly.get(j).copied().unwrap_or_default();
        a += (q.x as f64 + p.x as f64) * (q.y as f64 - p.y as f64);
        j = i;
    }
    0.5 * a
}

/// `ExPolygonWithOffset`: the outer loops (lines reach them) and the inner ones (lines link
/// along them).
#[derive(Clone)]
struct Offsetted {
    /// The turned region, contour first.
    source: Vec<Path>,
    contours: Vec<Path>,
    outer: usize,
}

impl Offsetted {
    fn new(shape: &[Path], angle: f32, offset1: i64, offset2: i64) -> Offsetted {
        let mut source = shape.to_vec();
        if angle != 0.0 {
            for c in &mut source {
                rotate(c, f64::from(angle));
            }
        }
        // `remove_sticks` of an ExPolygon stops at the contour when that changed anything; a
        // change here is too rare to be worth the difference.
        for c in &mut source {
            remove_sticks(c);
        }
        source.retain(|c| c.len() >= 3);
        let mut outer = if offset1 == 0 { source.clone() } else { geom::offset_shape_loops(&source, offset1 as f32) };
        let mut inner =
            if offset2 < 0 { geom::shrink_polygons(&outer, (offset1 - offset2) as f32) } else { Vec::new() };
        let min_area = 0.01 * offset2 as f64 * offset2 as f64;
        for set in [&mut outer, &mut inner] {
            set.retain(|c| area(c).abs() >= min_area);
            for c in set.iter_mut() {
                remove_sticks(c);
            }
            set.retain(|c| c.len() >= 3);
            for c in set.iter_mut() {
                remove_duplicates(c);
            }
        }
        let n_outer = outer.len();
        outer.append(&mut inner);
        Offsetted { source, contours: outer, outer: n_outer }
    }

    /// A copy turned whole, without offsetting again.
    fn turned(&self, angle: f32) -> Offsetted {
        let mut out = self.clone();
        if angle != 0.0 {
            for c in out.source.iter_mut().chain(&mut out.contours) {
                rotate(c, f64::from(angle));
            }
        }
        out
    }

    fn contour(&self, i: usize) -> Option<&Path> {
        self.contours.get(i)
    }

    fn is_outer(&self, i: usize) -> bool {
        i < self.outer
    }

    /// `bounding_box_src`: of the turned contour.
    fn bounds(&self) -> Option<(Pt, Pt)> {
        let c = self.source.first()?;
        let first = *c.first()?;
        Some(c.iter().fold((first, first), |(lo, hi), p| {
            (Pt::new(lo.x.min(p.x), lo.y.min(p.y)), Pt::new(hi.x.max(p.x), hi.y.max(p.y)))
        }))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Type {
    Unknown,
    OuterLow,
    OuterHigh,
    InnerLow,
    InnerHigh,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Link {
    Horizontal,
    Up,
    Down,
    Phony,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Quality {
    Invalid,
    Valid,
    TooLong,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Side {
    Left,
    Right,
}

/// `SegmentIntersection`: where a vertical line crosses a contour.
#[derive(Clone, Copy, Debug)]
struct Hit {
    contour: usize,
    segment: usize,
    /// y as the rational `pos_p / pos_q`.
    p: i128,
    q: i64,
    prev: i32,
    next: i32,
    kind: Type,
    prev_type: Link,
    next_type: Link,
    prev_quality: Quality,
    next_quality: Quality,
    consumed_up: bool,
    consumed_right: bool,
}

impl Default for Hit {
    fn default() -> Hit {
        Hit {
            contour: 0,
            segment: 0,
            p: 0,
            q: 1,
            prev: 0,
            next: 0,
            kind: Type::Unknown,
            prev_type: Link::Horizontal,
            next_type: Link::Horizontal,
            prev_quality: Quality::Valid,
            next_quality: Quality::Valid,
            consumed_up: false,
            consumed_right: false,
        }
    }
}

impl Hit {
    fn phony(kind: Type, pos: i64) -> Hit {
        Hit {
            contour: usize::MAX,
            segment: usize::MAX,
            p: i128::from(pos),
            kind,
            prev: -1,
            next: -1,
            prev_type: Link::Phony,
            next_type: Link::Phony,
            prev_quality: Quality::Invalid,
            next_quality: Quality::Invalid,
            ..Hit::default()
        }
    }

    fn pos(&self) -> i64 {
        let half = i128::from(self.q >> 1);
        let p = if self.p < 0 { self.p - half } else { self.p + half };
        (p / i128::from(self.q)) as i64
    }

    fn lower(&self, other: &Hit) -> bool {
        self.p * i128::from(other.q) < other.p * i128::from(self.q)
    }

    fn is_inner(&self) -> bool {
        matches!(self.kind, Type::InnerLow | Type::InnerHigh)
    }

    fn is_outer(&self) -> bool {
        matches!(self.kind, Type::OuterLow | Type::OuterHigh)
    }

    fn is_low(&self) -> bool {
        matches!(self.kind, Type::InnerLow | Type::OuterLow)
    }

    fn left_horizontal(&self) -> i32 {
        if self.prev_type == Link::Horizontal { self.prev } else { -1 }
    }

    fn right_horizontal(&self) -> i32 {
        if self.next_type == Link::Horizontal { self.next } else { -1 }
    }

    fn horizontal(&self, side: Side) -> i32 {
        if side == Side::Left { self.left_horizontal() } else { self.right_horizontal() }
    }

    fn horizontal_quality(&self, side: Side) -> Quality {
        if side == Side::Left { self.prev_quality } else { self.next_quality }
    }

    fn has_left_vertical(&self) -> bool {
        matches!(self.prev_type, Link::Up | Link::Down)
    }

    fn has_right_vertical(&self) -> bool {
        matches!(self.next_type, Link::Up | Link::Down)
    }

    fn has_vertical(&self, side: Side) -> bool {
        if side == Side::Left { self.has_left_vertical() } else { self.has_right_vertical() }
    }

    fn vertical_up(&self) -> i32 {
        if self.prev_type == Link::Up {
            self.prev
        } else if self.next_type == Link::Up {
            self.next
        } else {
            -1
        }
    }

    fn vertical_down(&self) -> i32 {
        if self.prev_type == Link::Down {
            self.prev
        } else if self.next_type == Link::Down {
            self.next
        } else {
            -1
        }
    }

    fn vertical_up_quality(&self) -> Quality {
        if self.prev_type == Link::Up { self.prev_quality } else { self.next_quality }
    }

    fn vertical_down_quality(&self) -> Quality {
        if self.prev_type == Link::Down { self.prev_quality } else { self.next_quality }
    }

    fn vertical_outside(&self) -> i32 {
        if self.is_low() { self.vertical_down() } else { self.vertical_up() }
    }

    fn vertical_outside_quality(&self) -> Quality {
        if self.is_low() { self.vertical_down_quality() } else { self.vertical_up_quality() }
    }
}

/// `SegmentedIntersectionLine`.
#[derive(Clone, Debug, Default)]
struct Vline {
    pos: i64,
    hits: Vec<Hit>,
}

fn at(segs: &[Vline], v: usize, i: i32) -> Option<&Hit> {
    segs.get(v)?.hits.get(usize::try_from(i).ok()?)
}

fn at_mut(segs: &mut [Vline], v: usize, i: i32) -> Option<&mut Hit> {
    segs.get_mut(v)?.hits.get_mut(usize::try_from(i).ok()?)
}

fn point(segs: &[Vline], v: usize, i: i32) -> Option<Pt> {
    Some(Pt::new(segs.get(v)?.pos, at(segs, v, i)?.pos()))
}

fn norm(a: Pt, b: Pt) -> f64 {
    let (x, y) = ((a.x - b.x) as f64, (a.y - b.y) as f64);
    (x * x + y * y).sqrt()
}

/// `segment_length`.
fn segment_length(poly: &[Pt], seg1: usize, p1: Pt, seg2: usize, p2: Pt) -> Option<f64> {
    let mut prev = p1;
    let mut len = 0.0;
    let mut add = |range: std::ops::Range<usize>| -> Option<()> {
        for i in range {
            let q = *poly.get(i)?;
            len += norm(prev, q);
            prev = q;
        }
        Some(())
    };
    if seg1 <= seg2 {
        add(seg1..seg2)?;
    } else {
        add(seg1..poly.len())?;
        add(0..seg2)?;
    }
    Some(len + norm(prev, p2))
}

/// `polygon_segment_append`.
fn append(out: &mut Path, poly: &[Pt], seg1: usize, seg2: usize) -> Option<()> {
    if seg1 < seg2 {
        out.extend_from_slice(poly.get(seg1..seg2)?);
    } else if seg1 > seg2 {
        out.extend_from_slice(poly.get(seg1..)?);
        out.extend_from_slice(poly.get(..seg2)?);
    }
    Some(())
}

/// `polygon_segment_append_reversed`.
fn append_reversed(out: &mut Path, poly: &[Pt], seg1: usize, seg2: usize) -> Option<()> {
    if seg1 >= seg2 {
        out.extend(poly.get(seg2..seg1)?.iter().rev());
    } else {
        out.extend(poly.get(..seg1)?.iter().rev());
        out.extend(poly.get(seg2..)?.iter().rev());
    }
    Some(())
}

/// `distance_of_segmens`.
fn segments_apart(poly: &[Pt], seg1: usize, seg2: usize, forward: bool) -> i64 {
    let mut d = seg2 as i64 - seg1 as i64;
    if !forward {
        d = -d;
    }
    if d < 0 {
        d += poly.len() as i64;
    }
    d
}

/// `intersection_on_prev_next_vertical_line_valid`.
fn neighbour_valid(segs: &[Vline], v: usize, i: i32, side: Side) -> Option<bool> {
    let this = at(segs, v, i)?;
    if this.has_vertical(side) {
        return Some(false);
    }
    let other_i = this.horizontal(side);
    if other_i == -1 {
        return Some(false);
    }
    let other_v = if side == Side::Right { v + 1 } else { v.checked_sub(1)? };
    let other = at(segs, other_v, other_i)?;
    let beside = at(segs, other_v, if other.is_low() { other_i - 1 } else { other_i + 1 })?;
    if beside.is_inner() {
        return Some(false);
    }
    if this.horizontal_quality(side) != Quality::Valid {
        return Some(false);
    }
    if if side == Side::Right { this.consumed_right } else { other.consumed_right } {
        return Some(false);
    }
    let up = if other.is_low() { other.consumed_up } else { at(segs, other_v, other_i - 1)?.consumed_up };
    Some(!up)
}

/// `measure_perimeter_horizontal_segment_length`.
fn measure_horizontal(o: &Offsetted, segs: &[Vline], v: usize, i: i32, i2: i32) -> Option<f64> {
    let (it, it2) = (at(segs, v, i)?, at(segs, v + 1, i2)?);
    let poly = o.contour(it.contour)?;
    let (p1, p2) = (point(segs, v, i)?, point(segs, v + 1, i2)?);
    if it.is_low() {
        segment_length(poly, it.segment, p1, it2.segment, p2)
    } else {
        segment_length(poly, it2.segment, p2, it.segment, p1)
    }
}

/// `emit_perimeter_prev_next_segment`.
fn emit_horizontal(o: &Offsetted, segs: &[Vline], v: usize, i: i32, i2: i32, out: &mut Path, next: bool) -> Option<()> {
    let other = if next { v + 1 } else { v.checked_sub(1)? };
    let (it, it2) = (at(segs, v, i)?, at(segs, other, i2)?);
    let poly = o.contour(it.contour)?;
    if it.is_low() == next {
        append(out, poly, it.segment, it2.segment)?;
    } else {
        append_reversed(out, poly, it.segment, it2.segment)?;
    }
    out.push(point(segs, other, i2)?);
    Some(())
}

/// `emit_perimeter_segment_on_vertical_line`.
fn emit_vertical(
    o: &Offsetted,
    segs: &[Vline],
    v: usize,
    i: i32,
    i2: i32,
    out: &mut Path,
    forward: bool,
) -> Option<()> {
    let (it, it2) = (at(segs, v, i)?, at(segs, v, i2)?);
    let poly = o.contour(it.contour)?;
    if forward {
        append(out, poly, it.segment, it2.segment)?;
    } else {
        append_reversed(out, poly, it.segment, it2.segment)?;
    }
    out.push(point(segs, v, i2)?);
    Some(())
}

/// `slice_region_by_vertical_lines`.
fn slice(o: &Offsetted, n_lines: usize, x0: i64, spacing: i64) -> Option<Vec<Vline>> {
    let mut segs: Vec<Vline> = (0..n_lines as i64).map(|i| Vline { pos: x0 + i * spacing, hits: Vec::new() }).collect();
    let last = segs.len() as i64 - 1;
    for (ic, contour) in o.contours.iter().enumerate() {
        let n = contour.len();
        if n < 2 {
            continue;
        }
        for is in 0..n {
            let ip = if is == 0 { n - 1 } else { is - 1 };
            let (p1, p2) = (*contour.get(ip)?, *contour.get(is)?);
            let (l, r) = (p1.x.min(p2.x), p1.x.max(p2.x));
            let mut il = (l - x0) / spacing;
            while il * spacing + x0 < l {
                il += 1;
            }
            let il = il.max(0);
            let mut ir = (r - x0 + spacing) / spacing;
            while ir * spacing + x0 > r {
                ir -= 1;
            }
            let ir = ir.min(last);
            for i in il..=ir {
                let line = segs.get_mut(usize::try_from(i).ok()?)?;
                let x = line.pos;
                let mut hit = Hit { contour: ic, segment: is, ..Hit::default() };
                if p1.x == x {
                    if p2.x == x {
                        continue;
                    }
                    let p0 = *contour.get(if ip == 0 { n - 1 } else { ip - 1 })?;
                    if i128::from(p0.x - p1.x) * i128::from(p2.x - p1.x) > 0 {
                        continue;
                    }
                    hit.p = i128::from(p1.y);
                } else if p2.x == x {
                    let p3 = *contour.get(if is + 1 == n { 0 } else { is + 1 })?;
                    if i128::from(p3.x - p2.x) * i128::from(p1.x - p2.x) > 0 {
                        continue;
                    }
                    hit.p = i128::from(p2.y);
                } else {
                    let (p, q) = if p2.x > p1.x { (x - p1.x, p2.x - p1.x) } else { (p1.x - x, p1.x - p2.x) };
                    hit.q = q;
                    hit.p = i128::from(p) * i128::from(p2.y - p1.y) + i128::from(p1.y) * i128::from(q);
                }
                line.hits.push(hit);
            }
        }
    }
    for line in &mut segs {
        std_sort(&mut line.hits, &|a: &Hit, b: &Hit| a.lower(b));
        classify(o, &mut line.hits)?;
    }
    // A broken cut is `InfillFailedException` in Orca.
    for line in &segs {
        let hits = &line.hits;
        if hits.len() % 2 != 0 {
            return None;
        }
        let mut i = 0;
        while i < hits.len() {
            if hits.get(i)?.kind != Type::OuterLow {
                return None;
            }
            let mut j = i + 1;
            if !matches!(hits.get(j)?.kind, Type::InnerLow | Type::OuterHigh) {
                return None;
            }
            while hits.get(j).is_some_and(Hit::is_inner) {
                j += 1;
            }
            if j & 1 != 1 || hits.get(j)?.kind != Type::OuterHigh {
                return None;
            }
            if i + 1 != j && hits.get(j - 1)?.kind != Type::InnerHigh {
                return None;
            }
            i = j + 1;
        }
    }
    Some(segs)
}

/// Gives a sorted line's cuts their types and drops the doubles where a vertex touches the line.
fn classify(o: &Offsetted, hits: &mut Vec<Hit>) -> Option<()> {
    let mut j = 0;
    for i in 0..hits.len() {
        let mut hit = *hits.get(i)?;
        let contour = o.contour(hit.contour)?;
        let n = contour.len();
        let prev = if hit.segment == 0 { n - 1 } else { hit.segment - 1 };
        let low = contour.get(hit.segment)?.x - contour.get(prev)?.x > 0;
        hit.kind = match (o.is_outer(hit.contour), low) {
            (true, true) => Type::OuterLow,
            (true, false) => Type::OuterHigh,
            (false, true) => Type::InnerLow,
            (false, false) => Type::InnerHigh,
        };
        if let Some(slot) = hits.get_mut(i) {
            *slot = hit;
        }
        let mut take = true;
        if j > 0 {
            let before = *hits.get(j - 1)?;
            if hit.contour == before.contour && hit.q == 1 && before.q == 1 {
                if hit.p == before.p {
                    take = false;
                } else if hit.kind == before.kind {
                    if !low && let Some(slot) = hits.get_mut(j - 1) {
                        *slot = hit;
                    }
                    take = false;
                }
            }
        }
        if take {
            if j < i
                && let Some(slot) = hits.get_mut(j)
            {
                *slot = hit;
            }
            j += 1;
        }
    }
    hits.truncate(j);
    Some(())
}

/// `connect_segment_intersections_by_contours`, with no limit on link lengths: Orca's
/// `link_max_length` is `3 * f->spacing` taken before `f->spacing` is set, so always 0.
fn connect(o: &Offsetted, segs: &mut [Vline], dont_connect: bool) -> Option<()> {
    for v in 0..segs.len() {
        for i in 0..segs.get(v)?.hits.len() {
            let link = links(o, segs, v, i as i32, dont_connect)?;
            let hit = at_mut(segs, v, i as i32)?;
            (hit.prev, hit.prev_type, hit.prev_quality) = link.0;
            (hit.next, hit.next_type, hit.next_quality) = link.1;
        }
        // Invalid is symmetric on vertical links.
        for i in 0..segs.get(v)?.hits.len() as i32 {
            let it = *at(segs, v, i)?;
            if it.has_left_vertical() && it.prev_quality == Quality::Invalid {
                at_mut(segs, v, it.prev)?.prev_quality = Quality::Invalid;
            }
            if it.has_right_vertical() && it.next_quality == Quality::Invalid {
                at_mut(segs, v, it.next)?.next_quality = Quality::Invalid;
            }
        }
    }
    Some(())
}

type Linked = (i32, Link, Quality);

/// One cut's two neighbours along its contour.
fn links(o: &Offsetted, segs: &[Vline], v: usize, i: i32, dont_connect: bool) -> Option<(Linked, Linked)> {
    let line = segs.get(v)?;
    let it = at(segs, v, i)?;
    let poly = o.contour(it.contour)?;
    let forward = it.is_low();
    let same_type = |h: &Hit| h.contour == it.contour && h.kind == it.kind;
    let (mut iprev, mut d_prev) = (-1, i64::from(i32::MAX));
    if let Some(left) = v.checked_sub(1).and_then(|l| segs.get(l)) {
        for (k, h) in left.hits.iter().enumerate().filter(|(_, h)| same_type(h)) {
            let d = segments_apart(poly, h.segment, it.segment, forward);
            if d < d_prev {
                (iprev, d_prev) = (k as i32, d);
            }
        }
    }
    let (mut inext, mut d_next) = (-1, i64::from(i32::MAX));
    if let Some(right) = segs.get(v + 1) {
        for (k, h) in right.hits.iter().enumerate().filter(|(_, h)| same_type(h)) {
            let d = segments_apart(poly, it.segment, h.segment, forward);
            if d < d_next {
                (inext, d_next) = (k as i32, d);
            }
        }
    }
    let (mut same_prev, mut same_next) = (false, false);
    for (k, h) in line.hits.iter().enumerate() {
        if k as i32 != i && h.contour == it.contour && h.kind != it.kind {
            let d = segments_apart(poly, h.segment, it.segment, forward);
            if d < d_prev {
                (iprev, d_prev, same_prev) = (k as i32, d, true);
            }
            let d = segments_apart(poly, it.segment, h.segment, forward);
            if d < d_next {
                (inext, d_next, same_next) = (k as i32, d, true);
            }
        }
    }
    let kind = |same: bool, other: i32| match (same, other < i) {
        (false, _) => Link::Horizontal,
        (true, true) => Link::Down,
        (true, false) => Link::Up,
    };
    // A vertical link only follows the contour past outer cuts.
    let clear = |other: i32| -> Option<Quality> {
        let (lo, hi) = (i.min(other), i.max(other));
        for k in lo + 1..hi {
            if at(segs, v, k)?.is_inner() {
                return Some(Quality::Invalid);
            }
        }
        Some(Quality::Valid)
    };
    let mut prev_quality = if same_prev { clear(iprev)? } else { Quality::Valid };
    let mut next_quality = if same_next { clear(inext)? } else { Quality::Valid };
    if same_prev && same_next && (iprev > i) == (inext > i) {
        prev_quality = Quality::Invalid;
        next_quality = Quality::Invalid;
    }
    if dont_connect {
        if prev_quality == Quality::Valid {
            prev_quality = Quality::TooLong;
        }
        if next_quality == Quality::Valid {
            next_quality = Quality::TooLong;
        }
    }
    Some(((iprev, kind(same_prev, iprev), prev_quality), (inext, kind(same_next, inext), next_quality)))
}

/// `pinch_contours_insert_phony_outer_intersections`.
fn pinch(segs: &mut [Vline]) -> Option<()> {
    for v in 1..segs.len() {
        let hits = &segs.get(v)?.hits;
        if hits.is_empty() {
            continue;
        }
        let end = hits.len() - 1;
        let mut after = Vec::new();
        let mut it = 1;
        while it < end {
            if hits.get(it)?.kind == Type::OuterHigh {
                it += 2;
                continue;
            }
            let hi = it + 1;
            let lo2 = it + 2;
            it = lo2;
            if hits.get(lo2)?.kind == Type::InnerLow {
                let up = hits.get(hi)?.vertical_up();
                let down = hits.get(lo2)?.vertical_down();
                if down + 1 != up {
                    after.push(hi);
                }
            }
        }
        if after.is_empty() {
            continue;
        }
        let mut map = Vec::with_capacity(hits.len());
        let mut out: Vec<Hit> = Vec::with_capacity(hits.len() + 2 * after.len());
        let mut i = 0;
        for &inset in &after {
            while i <= inset {
                map.push(out.len() as i32);
                out.push(*hits.get(i)?);
                i += 1;
            }
            let pos = (out.last()?.pos() + hits.get(i)?.pos()) / 2;
            out.push(Hit::phony(Type::OuterHigh, pos));
            out.push(Hit::phony(Type::OuterLow, pos));
        }
        for hit in hits.iter().skip(i) {
            map.push(out.len() as i32);
            out.push(*hit);
        }
        let mapped = |i: i32| usize::try_from(i).ok().and_then(|k| map.get(k).copied());
        for hit in &mut out {
            if hit.has_left_vertical() {
                hit.prev = mapped(hit.prev)?;
            }
            if hit.has_right_vertical() {
                hit.next = mapped(hit.next)?;
            }
        }
        segs.get_mut(v)?.hits = out;
        for hit in &mut segs.get_mut(v - 1)?.hits {
            if hit.right_horizontal() != -1 {
                hit.next = mapped(hit.next)?;
            }
        }
        if let Some(line) = segs.get_mut(v + 1) {
            for hit in &mut line.hits {
                if hit.left_horizontal() != -1 {
                    hit.prev = mapped(hit.prev)?;
                }
            }
        }
    }
    Some(())
}

/// `end_of_vertical_run_raw`.
fn end_of_run_raw(segs: &[Vline], v: usize, start: i32) -> Option<i32> {
    let mut it = start;
    loop {
        it += 1;
        if at(segs, v, it)?.kind == Type::OuterHigh {
            break;
        }
    }
    if at(segs, v, it - 1)?.is_inner() {
        it -= 1;
    }
    Some(it)
}

/// `end_of_vertical_run`.
fn end_of_run(segs: &[Vline], v: usize, start: i32) -> Option<i32> {
    let mut end = end_of_run_raw(segs, v, start)?;
    for _ in 0..=segs.get(v)?.hits.len() {
        let hit = at(segs, v, end)?;
        let up = hit.vertical_up();
        if up == -1 || hit.vertical_up_quality() != Quality::Valid {
            return Some(end);
        }
        end = end_of_run_raw(segs, v, up)?;
    }
    None
}

/// `traverse_graph_generate_polylines`.
fn traverse(o: &Offsetted, segs: &mut [Vline]) -> Option<Vec<Path>> {
    for line in segs.iter_mut() {
        for k in 0..line.hits.len().saturating_sub(1) {
            if line.hits.get(k)?.kind == Type::OuterLow && line.hits.get(k + 1)?.kind == Type::OuterHigh {
                line.hits.get_mut(k)?.consumed_up = true;
            }
        }
    }
    let total: usize = segs.iter().map(|l| l.hits.len()).sum();
    let mut out: Vec<Path> = Vec::new();
    let mut v = 0usize;
    let mut i: i32 = -1;
    // Every round consumes a vertical run, so this is only a guard.
    for _ in 0..4 * total + 16 {
        if i == -1 {
            let Some((nv, ni)) = next_start(segs)? else { return Some(out) };
            (v, i) = (nv, ni);
            out.push(vec![point(segs, v, i)?]);
        }
        let going_up = at(segs, v, i)?.is_low();
        let mut try_connect = false;
        if going_up {
            if at(segs, v, i)?.is_inner() {
                i -= 1;
            }
            loop {
                at_mut(segs, v, i)?.consumed_up = true;
                i += 1;
                if at(segs, v, i)?.kind == Type::OuterHigh {
                    break;
                }
            }
            if at(segs, v, i - 1)?.is_inner() {
                i -= 1;
                try_connect = true;
            }
        } else {
            if at(segs, v, i)?.is_inner() {
                at_mut(segs, v, i)?.consumed_up = true;
            }
            loop {
                i -= 1;
                at_mut(segs, v, i)?.consumed_up = true;
                if at(segs, v, i)?.kind == Type::OuterLow {
                    break;
                }
            }
            if at(segs, v, i + 1)?.is_inner() {
                i += 1;
                try_connect = true;
            }
        }
        if try_connect {
            let (i_prev, i_next) = {
                let it = at(segs, v, i)?;
                (it.left_horizontal(), it.right_horizontal())
            };
            let prev_valid = neighbour_valid(segs, v, i, Side::Left)?;
            let next_valid = neighbour_valid(segs, v, i, Side::Right)?;
            if i_prev != -1 {
                at_mut(segs, v.checked_sub(1)?, i_prev)?.consumed_right = true;
            }
            if i_next != -1 {
                at_mut(segs, v, i)?.consumed_right = true;
            }
            if prev_valid || next_valid {
                let mut take_next = next_valid;
                if prev_valid && next_valid {
                    let dist_prev = measure_horizontal(o, segs, v - 1, i_prev, i)?;
                    let dist_next = measure_horizontal(o, segs, v, i, i_next)?;
                    take_next = dist_next < dist_prev;
                }
                let current = out.last_mut()?;
                current.push(point(segs, v, i)?);
                emit_horizontal(o, segs, v, i, if take_next { i_next } else { i_prev }, current, take_next)?;
                if take_next {
                    v += 1;
                    i = i_next;
                } else {
                    v -= 1;
                    i = i_prev;
                }
                continue;
            }
            let it = *at(segs, v, i)?;
            let i_vertical = it.vertical_outside();
            let quality = if i_vertical == -1 || at(segs, v, i_vertical + if going_up { 0 } else { -1 })?.consumed_up {
                Quality::Invalid
            } else {
                it.vertical_outside_quality()
            };
            if quality != Quality::Invalid {
                out.last_mut()?.push(point(segs, v, i)?);
                if quality == Quality::Valid {
                    let forward = if going_up { it.prev_type == Link::Up } else { it.next_type == Link::Down };
                    emit_vertical(o, segs, v, i, i_vertical, out.last_mut()?, forward)?;
                } else {
                    out.push(vec![point(segs, v, i_vertical)?]);
                }
                let (from, to) = if going_up { (i, i_vertical) } else { (i_vertical, i) };
                for k in from..to {
                    at_mut(segs, v, k)?.consumed_up = true;
                }
                at_mut(segs, v, i)?.consumed_right = true;
                at_mut(segs, v, if going_up { i + 1 } else { i - 1 })?.consumed_right = true;
                i = i_vertical;
                continue;
            }
            i += if going_up { 1 } else { -1 };
        }
        // Finish this vertical line and look for a new start.
        let end = point(segs, v, i)?;
        let current = out.last_mut()?;
        current.push(end);
        remove_duplicates(current);
        if too_short(current) {
            out.pop();
        }
        i = -1;
    }
    None
}

/// The next unconsumed start of a path, left to right.
#[allow(clippy::type_complexity)]
fn next_start(segs: &[Vline]) -> Option<Option<(usize, i32)>> {
    for (v, line) in segs.iter().enumerate() {
        for (k, hit) in line.hits.iter().enumerate() {
            if hit.is_outer() {
                let consumed =
                    if hit.is_low() { hit.consumed_up } else { line.hits.get(k.checked_sub(1)?)?.consumed_up };
                if !consumed {
                    return Some(Some((v, k as i32)));
                }
            }
        }
    }
    Some(None)
}

fn too_short(line: &[Pt]) -> bool {
    line.len() <= 1
        || (line.len() == 2
            && matches!((line.first(), line.last()), (Some(a), Some(b))
                if (a.x - b.x).abs() < SCALED_EPSILON && (a.y - b.y).abs() < SCALED_EPSILON))
}

/// One side of a [`Region`].
#[derive(Clone, Copy, Debug, Default)]
struct Boundary {
    vline: usize,
    low: i32,
    high: i32,
}

/// `MonotonicRegion`.
#[derive(Clone, Debug, Default)]
struct Region {
    left: Boundary,
    right: Boundary,
    len1: f32,
    len2: f32,
    flips: bool,
    left_neighbours: Vec<usize>,
    right_neighbours: Vec<usize>,
}

impl Region {
    fn length(&self, flipped: bool) -> f32 {
        if flipped { self.len2 } else { self.len1 }
    }

    fn left_point(&self, flipped: bool) -> i32 {
        if flipped { self.left.high } else { self.left.low }
    }

    fn right_point(&self, flipped: bool) -> i32 {
        if flipped == self.flips { self.right.low } else { self.right.high }
    }
}

/// `vertical_run_bottom`.
fn run_bottom(segs: &[Vline], v: usize, start: i32) -> Option<i32> {
    let mut it = start;
    for _ in 0..=4 * segs.get(v)?.hits.len() {
        while at(segs, v, it)?.kind != Type::InnerLow {
            it -= 1;
        }
        if at(segs, v, it - 1)?.kind == Type::InnerHigh {
            it -= 1;
        } else {
            let hit = at(segs, v, it)?;
            let down = hit.vertical_down();
            if down == -1 || hit.vertical_down_quality() != Quality::Valid {
                return Some(it);
            }
            it = down;
        }
    }
    None
}

/// `vertical_run_top`.
fn run_top(segs: &[Vline], v: usize, start: i32) -> Option<i32> {
    let mut it = start;
    for _ in 0..=4 * segs.get(v)?.hits.len() {
        while at(segs, v, it)?.kind != Type::InnerHigh {
            it += 1;
        }
        if at(segs, v, it + 1)?.kind == Type::InnerLow {
            it += 1;
        } else {
            let hit = at(segs, v, it)?;
            let up = hit.vertical_up();
            if up == -1 || hit.vertical_up_quality() != Quality::Valid {
                return Some(it);
            }
            it = up;
        }
    }
    None
}

/// `overlap_bottom`; -1 for none.
fn overlap_bottom(segs: &[Vline], start: i32, end: i32, this: usize, other: usize, side: Side) -> Option<i32> {
    let mut it = start;
    let mut found = -1;
    for _ in 0..=4 * segs.get(this)?.hits.len() {
        let hit = at(segs, this, it)?;
        if hit.is_inner() {
            let i = hit.horizontal(side);
            if i != -1 {
                found = i;
                break;
            }
            if it == end {
                break;
            }
        }
        if hit.kind != Type::InnerHigh || at(segs, this, it + 1)?.kind == Type::InnerLow {
            it += 1;
        } else {
            let up = hit.vertical_up();
            if up == -1 || hit.vertical_up_quality() != Quality::Valid {
                break;
            }
            it = up;
        }
    }
    if found == -1 { Some(-1) } else { run_bottom(segs, other, found) }
}

/// `overlap_top`; -1 for none.
fn overlap_top(segs: &[Vline], start: i32, end: i32, this: usize, other: usize, side: Side) -> Option<i32> {
    let mut it = end;
    let mut found = -1;
    for _ in 0..=4 * segs.get(this)?.hits.len() {
        let hit = at(segs, this, it)?;
        if hit.is_inner() {
            let i = hit.horizontal(side);
            if i != -1 {
                found = i;
                break;
            }
            if it == start {
                break;
            }
        }
        if hit.kind != Type::InnerLow || at(segs, this, it - 1)?.kind == Type::InnerHigh {
            it -= 1;
        } else {
            let down = hit.vertical_down();
            if down == -1 || hit.vertical_down_quality() != Quality::Valid {
                break;
            }
            it = down;
        }
    }
    if found == -1 { Some(-1) } else { run_top(segs, other, found) }
}

/// `left_overlap` / `right_overlap`; (-1, -1) for none.
fn overlap(segs: &[Vline], (start, end): (i32, i32), this: usize, other: usize, side: Side) -> Option<(i32, i32)> {
    if start == -1 {
        return Some((-1, -1));
    }
    let first = overlap_bottom(segs, start, end, this, other, side)?;
    if first == -1 {
        return Some((-1, -1));
    }
    Some((first, overlap_top(segs, start, end, this, other, side)?))
}

/// `generate_montonous_regions`.
fn monotonic_regions(segs: &mut [Vline]) -> Option<Vec<Region>> {
    let mut regions = Vec::new();
    for seed in 0..segs.len() {
        let n = segs.get(seed)?.hits.len() as i32;
        let mut i_seed = 1;
        while i_seed + 1 < n {
            while i_seed < n && at(segs, seed, i_seed)?.kind != Type::InnerLow {
                i_seed += 1;
            }
            if i_seed == n {
                break;
            }
            let start = i_seed;
            let end = end_of_run(segs, seed, start)?;
            if !at(segs, seed, start)?.consumed_up {
                let mut v = seed;
                let mut left = (start, end);
                let mut region = Region::default();
                region.left = Boundary { vline: v, low: start, high: end };
                region.right = region.left;
                at_mut(segs, seed, start)?.consumed_up = true;
                let mut num_lines = 1;
                loop {
                    v += 1;
                    if v >= segs.len() {
                        break;
                    }
                    let right = overlap(segs, left, v - 1, v, Side::Right)?;
                    if right.0 == -1 || run_top(segs, v, right.0)? != right.1 {
                        break;
                    }
                    if overlap(segs, right, v, v - 1, Side::Left)? != left {
                        break;
                    }
                    region.right = Boundary { vline: v, low: right.0, high: right.1 };
                    at_mut(segs, v, right.0)?.consumed_up = true;
                    num_lines += 1;
                    left = right;
                }
                region.flips = num_lines & 1 != 0;
                regions.push(region);
            }
            i_seed = end + 1;
        }
    }
    Some(regions)
}

/// `Vec2f(x, y).norm()` of integers.
fn norm_f32(x: i64, y: i64) -> f32 {
    let (x, y) = (x as f32, y as f32);
    (x * x + y * y).sqrt()
}

/// `unscale<float>`.
fn unscale_f(v: f32) -> f32 {
    v * SCALING as f32
}

/// `montonous_region_path_length`.
fn region_length(region: &Region, dir: bool, o: &Offsetted, segs: &[Vline]) -> Option<f32> {
    let mut i_hit = region.left_point(dir);
    let mut total = 0.0f32;
    let mut no_perimeter = false;
    let mut last = (0.0f32, 0.0f32);
    for v in region.left.vline..=region.right.vline {
        let line = segs.get(v)?;
        let mut it = i_hit;
        let hit = at(segs, v, it)?;
        let going_up = hit.is_low();
        if no_perimeter {
            let y = at(segs, v, it + if going_up { -1 } else { 1 })?.pos() as f32;
            let (dx, dy) = (last.0 - line.pos as f32, last.1 - y);
            total += (dx * dx + dy * dy).sqrt();
        }
        let mut iright = hit.right_horizontal();
        (it, iright) = walk_run(segs, v, it, going_up, iright, |_, _| Some(()))?;
        if v == region.right.vline {
            return Some(unscale_f(total));
        }
        let hit = at(segs, v, it)?;
        let inext = hit.right_horizontal();
        i_hit = if going_up { run_top(segs, v + 1, iright)? } else { run_bottom(segs, v + 1, iright)? };
        if inext == i_hit && hit.next_quality == Quality::Valid {
            total += 0.5 * measure_horizontal(o, segs, v, it, inext)? as f32;
            no_perimeter = false;
        } else {
            let end = at(segs, v, it + if going_up { 1 } else { -1 })?;
            last = (line.pos as f32, end.pos() as f32);
            no_perimeter = true;
        }
    }
    None
}

/// Goes up or down one vertical run to its inner end, following contour links on this line.
/// `step` hears of each stop: where the run paused and where its contour link leads, or -1 at
/// the end. The run's end and the highest cut on the next line's side.
fn walk_run(
    segs: &[Vline],
    v: usize,
    start: i32,
    going_up: bool,
    mut iright: i32,
    mut step: impl FnMut(i32, i32) -> Option<()>,
) -> Option<(i32, i32)> {
    let mut it = start;
    for _ in 0..=segs.get(v)?.hits.len() {
        if going_up {
            loop {
                it += 1;
                let hit = at(segs, v, it)?;
                iright = iright.max(hit.right_horizontal());
                if hit.kind == Type::InnerHigh && at(segs, v, it + 1)?.kind == Type::OuterHigh {
                    break;
                }
            }
        } else {
            loop {
                it -= 1;
                let hit = at(segs, v, it)?;
                if hit.right_horizontal() != -1 {
                    iright = hit.right_horizontal();
                }
                if hit.kind == Type::InnerLow && at(segs, v, it - 1)?.kind == Type::OuterLow {
                    break;
                }
            }
        }
        let hit = at(segs, v, it)?;
        let (inext, quality) = if going_up {
            (hit.vertical_up(), hit.vertical_up_quality())
        } else {
            (hit.vertical_down(), hit.vertical_down_quality())
        };
        if inext == -1 || quality != Quality::Valid {
            step(it, -1)?;
            return Some((it, iright));
        }
        step(it, inext)?;
        it = inext;
    }
    None
}

/// `connect_monotonic_regions`.
fn connect_regions(regions: &mut [Region], o: &Offsetted, segs: &[Vline]) -> Option<()> {
    let mut starts: HashMap<(usize, i32), usize> = HashMap::with_capacity(regions.len());
    let mut ends: HashMap<(usize, i32), usize> = HashMap::with_capacity(regions.len());
    for (r, region) in regions.iter().enumerate() {
        starts.insert((region.left.vline, region.left.low), r);
        ends.insert((region.right.vline, region.right.low), r);
    }
    // Next door, cut by cut along the side.
    let scatter = |side_v: usize, range: (i32, i32), mut note: Box<dyn FnMut(i32) + '_>| -> Option<()> {
        let (mut begin, end) = range;
        if begin == -1 {
            return Some(());
        }
        for _ in 0..=segs.get(side_v)?.hits.len() {
            note(begin);
            let mut next = run_top(segs, side_v, begin)?;
            if next == end {
                return Some(());
            }
            while at(segs, side_v, next)?.kind != Type::InnerLow {
                next += 1;
            }
            begin = next;
        }
        None
    };
    for r in 0..regions.len() {
        let region = regions.get(r)?;
        let (left, right) = (region.left, region.right);
        if left.vline > 0 {
            let range = overlap(segs, (left.low, left.high), left.vline, left.vline - 1, Side::Left)?;
            let mut found = Vec::new();
            scatter(
                left.vline - 1,
                range,
                Box::new(|at_cut| {
                    if let Some(&e) = ends.get(&(left.vline - 1, at_cut)) {
                        found.push(e);
                    }
                }),
            )?;
            for e in found {
                regions.get_mut(e)?.right_neighbours.push(r);
            }
        }
        if right.vline + 1 < segs.len() {
            let range = overlap(segs, (right.low, right.high), right.vline, right.vline + 1, Side::Right)?;
            let mut found = Vec::new();
            scatter(
                right.vline + 1,
                range,
                Box::new(|at_cut| {
                    if let Some(&s) = starts.get(&(right.vline + 1, at_cut)) {
                        found.push(s);
                    }
                }),
            )?;
            for s in found {
                regions.get_mut(s)?.left_neighbours.push(r);
            }
        }
    }
    for region in regions.iter_mut() {
        region.left_neighbours.sort_unstable();
        region.right_neighbours.sort_unstable();
    }
    // A side may know a neighbour that does not know it.
    for r in 0..regions.len() {
        for k in 0..regions.get(r)?.left_neighbours.len() {
            let n = *regions.get(r)?.left_neighbours.get(k)?;
            let list = &mut regions.get_mut(n)?.right_neighbours;
            if let Err(at) = list.binary_search(&r) {
                list.insert(at, r);
            }
        }
        for k in 0..regions.get(r)?.right_neighbours.len() {
            let n = *regions.get(r)?.right_neighbours.get(k)?;
            let list = &mut regions.get_mut(n)?.left_neighbours;
            if let Err(at) = list.binary_search(&r) {
                list.insert(at, r);
            }
        }
    }
    for r in 0..regions.len() {
        let len1 = region_length(regions.get(r)?, false, o, segs)?;
        let len2 = region_length(regions.get(r)?, true, o, segs)?;
        let region = regions.get_mut(r)?;
        if len1 > len2 {
            (region.len1, region.len2) = (len1 - len2, 0.0);
        } else {
            (region.len1, region.len2) = (0.0, len2 - len1);
        }
    }
    Some(())
}

/// `AntPath`.
#[derive(Clone, Copy, Debug)]
struct AntPath {
    length: f32,
    visibility: f32,
    pheromone: f32,
}

/// `AntPathMatrix`, kept sparse: Orca's holds every pair of regions.
struct Ants<'a> {
    regions: &'a [Region],
    segs: &'a [Vline],
    paths: HashMap<(usize, bool, usize, bool), AntPath>,
    initial: f32,
}

type AntKey = (usize, bool, usize, bool);

impl Ants<'_> {
    fn update_initial(&mut self, pheromone: f32) {
        self.initial = pheromone;
        self.paths.values_mut().for_each(|p| p.pheromone = pheromone);
    }

    /// The path, measured the first time it is asked for. Orca's measure along the contour
    /// compares a region's right line with its own left one, so never runs: a straight line it is.
    fn path(&mut self, key: AntKey) -> Option<AntPath> {
        if let Some(p) = self.paths.get(&key) {
            return Some(*p);
        }
        let (from, flipped_from, to, flipped_to) = key;
        let (a, b) = (self.regions.get(from)?, self.regions.get(to)?);
        let (va, vb) = (a.right.vline, b.left.vline);
        let (i_from, i_to) = (a.right_point(flipped_from), b.left_point(flipped_to));
        let (pa, pb) = (point(self.segs, va, i_from)?, point(self.segs, vb, i_to)?);
        let length = unscale_f(norm_f32(pb.x - pa.x, pb.y - pa.y));
        let path = AntPath { length, visibility: 1.0 / (length + EPSILON as f32), pheromone: self.initial };
        self.paths.insert(key, path);
        Some(path)
    }
}

/// `MonotonicRegionLink`.
#[derive(Clone, Copy, Debug)]
struct RegionLink {
    region: usize,
    flipped: bool,
    next: Option<AntKey>,
}

/// `NextCandidate`.
#[derive(Clone, Copy, Debug)]
struct Candidate {
    region: usize,
    link: AntKey,
    probability: f32,
    dir: bool,
}

/// `chain_monotonic_regions`: the regions' order, by Orca's ant colony.
fn chain_regions(regions: &[Region], segs: &[Vline], rng: &mut Mt64) -> Option<Vec<RegionLink>> {
    const NUM_ROUNDS: usize = 25;
    const NO_CHANGE_EXIT: usize = 8;
    const EVAPORATION: f32 = 0.1;
    const DIVERSIFICATION: f32 = 0.1;
    const TAKE_BEST: f32 = 0.9;

    let mut unprocessed_initial = vec![1i32; regions.len()];
    let mut queue_initial = Vec::with_capacity(regions.len());
    for (r, region) in regions.iter().enumerate() {
        if region.left_neighbours.is_empty() {
            queue_initial.push(r);
        } else if let Some(u) = unprocessed_initial.get_mut(r) {
            *u += region.left_neighbours.len() as i32;
        }
    }
    let num_ants = regions.len().min(10);
    let mut ants = Ants { regions, segs, paths: HashMap::new(), initial: 0.5 };

    // A greedy path sets the first pheromone level.
    {
        let mut queue = queue_initial.clone();
        let mut unprocessed = unprocessed_initial.clone();
        let mut end = (queue.pop()?, false);
        *unprocessed.get_mut(end.0)? -= 1;
        let mut total = regions.get(end.0)?.length(false);
        loop {
            let region = regions.get(end.0)?;
            if queue.is_empty() && region.right_neighbours.is_empty() {
                break;
            }
            let dir = end.1;
            let mut best: Option<(usize, bool, f32)> = None;
            let consider = |ants: &mut Ants<'_>, next: usize, best: &mut Option<(usize, bool, f32)>| -> Option<()> {
                for flipped in [false, true] {
                    let v = ants.path((end.0, dir, next, flipped))?.visibility;
                    if v > best.map_or(0.0, |b| b.2) {
                        *best = Some((next, flipped, v));
                    }
                }
                Some(())
            };
            for &next in &region.right_neighbours {
                if *unprocessed.get(next)? == 2 {
                    consider(&mut ants, next, &mut best)?;
                }
            }
            let from_queue = best.is_none();
            if from_queue {
                for &next in &queue {
                    consider(&mut ants, next, &mut best)?;
                }
            }
            for &next in &region.right_neighbours {
                let u = unprocessed.get_mut(next)?;
                *u -= 1;
                if *u == 1 && best.map(|b| b.0) != Some(next) {
                    queue.push(next);
                }
            }
            let (next_region, next_dir, _) = best?;
            if from_queue {
                let k = queue.iter().position(|&q| q == next_region)?;
                queue.swap_remove(k);
            }
            total +=
                regions.get(next_region)?.length(next_dir) + ants.path((end.0, end.1, next_region, next_dir))?.length;
            end = (next_region, next_dir);
            *unprocessed.get_mut(next_region)? = 0;
        }
        ants.update_initial(0.1 / total);
    }

    let mut best_path: Vec<RegionLink> = Vec::new();
    let mut best_length = f32::MAX;
    let mut path: Vec<RegionLink> = Vec::with_capacity(regions.len());
    let mut candidates: Vec<Candidate> = Vec::new();
    let mut no_change = 0;
    let mut round = 0;
    while round < NUM_ROUNDS && no_change < NO_CHANGE_EXIT {
        round += 1;
        let mut improved = false;
        for _ in 0..num_ants {
            path.clear();
            let mut queue = queue_initial.clone();
            let mut unprocessed = unprocessed_initial.clone();
            let first = rng.below(queue.len() as u64) as usize;
            let region = *queue.get(first)?;
            path.push(RegionLink { region, flipped: rng.next() > u64::MAX / 2, next: None });
            queue.swap_remove(first);
            *unprocessed.get_mut(region)? -= 1;
            loop {
                let last = *path.last()?;
                let region = regions.get(last.region)?;
                if queue.is_empty() && region.right_neighbours.is_empty() {
                    break;
                }
                let dir = last.flipped;
                candidates.clear();
                let add = |ants: &mut Ants<'_>, c: &mut Vec<Candidate>, next: usize| -> Option<()> {
                    for flipped in [false, true] {
                        let link = (last.region, dir, next, flipped);
                        let p = ants.path(link)?;
                        // The other two of the four links are only made for the record.
                        ants.path((last.region, !dir, next, !flipped))?;
                        let visible = p.visibility * p.visibility;
                        c.push(Candidate { region: next, link, probability: p.pheromone * visible, dir: flipped });
                    }
                    Some(())
                };
                for &next in &region.right_neighbours {
                    let u = unprocessed.get_mut(next)?;
                    *u -= 1;
                    if *u == 1 {
                        add(&mut ants, &mut candidates, next)?;
                    }
                }
                let direct = candidates.len();
                if direct == 0 {
                    for &next in &queue {
                        add(&mut ants, &mut candidates, next)?;
                    }
                }
                let dice = rng.next() as f32 / u64::MAX as f32;
                let take = if dice < TAKE_BEST {
                    let mut best = 0;
                    for (k, c) in candidates.iter().enumerate() {
                        if candidates.get(best)?.probability < c.probability {
                            best = k;
                        }
                    }
                    best
                } else {
                    let total = candidates.iter().fold(0.0f32, |t, c| t + c.probability);
                    let mut threshold = rng.next() as f32 * total / u64::MAX as f32;
                    let mut take = candidates.len().checked_sub(1)?;
                    for (k, c) in candidates.iter().enumerate() {
                        threshold -= c.probability;
                        if f64::from(threshold) <= 0.0 {
                            take = k;
                            break;
                        }
                    }
                    take
                };
                let taken = *candidates.get(take)?;
                for c in candidates.get(..direct)? {
                    if queue.last() != Some(&c.region) && c.region != taken.region {
                        queue.push(c.region);
                    }
                }
                if take >= direct {
                    let k = queue.iter().position(|&q| q == taken.region)?;
                    queue.swap_remove(k);
                }
                path.last_mut()?.next = Some(taken.link);
                path.push(RegionLink { region: taken.region, flipped: taken.dir, next: None });
                *unprocessed.get_mut(taken.region)? = 0;
                let initial = ants.initial;
                if let Some(p) = ants.paths.get_mut(&taken.link) {
                    p.pheromone = (1.0 - DIVERSIFICATION) * p.pheromone + DIVERSIFICATION * initial;
                }
            }
            let tail = *path.last()?;
            let mut length = regions.get(tail.region)?.length(tail.flipped);
            for pair in path.windows(2) {
                let (Some(r), Some(n)) = (pair.first(), pair.get(1)) else { continue };
                length = length
                    + regions.get(r.region)?.length(r.flipped)
                    + ants.path((r.region, r.flipped, n.region, n.flipped))?.length;
            }
            if length < best_length {
                best_length = length;
                std::mem::swap(&mut best_path, &mut path);
                if length == 0.0 {
                    return Some(best_path);
                }
                improved = true;
            }
        }
        // Over the last ant's path, or the best but one when that was swapped out, as Orca's does.
        let total_cost = best_length + EPSILON as f32;
        for link in path.iter().take(path.len().saturating_sub(1)) {
            if let Some(p) = link.next.and_then(|k| ants.paths.get_mut(&k)) {
                p.pheromone = (1.0 - EVAPORATION) * p.pheromone + EVAPORATION / total_cost;
            }
        }
        no_change = if improved { 0 } else { no_change + 1 };
    }
    Some(best_path)
}

/// `finish_polyline` of `polylines_from_paths`.
fn finish(out: &mut Vec<Path>) -> Option<()> {
    let line = out.last_mut()?;
    remove_duplicates(line);
    if too_short(line) {
        out.pop();
    } else if out.len() >= 2 {
        let line = out.last()?.clone();
        let n = out.len();
        let prev = out.get_mut(n - 2)?;
        let (front, back) = (*line.first()?, *prev.last()?);
        if (front.x - back.x).abs() < SCALED_EPSILON && (front.y - back.y).abs() < SCALED_EPSILON {
            *prev.last_mut()? = Pt::new((back.x + front.x) / 2, (back.y + front.y) / 2);
            prev.extend_from_slice(line.get(1..)?);
            out.pop();
        }
    }
    Some(())
}

/// `polylines_from_paths`.
fn polylines(path: &[RegionLink], regions: &[Region], o: &Offsetted, segs: &[Vline]) -> Option<Vec<Path>> {
    let mut out: Vec<Path> = Vec::new();
    let mut open = false;
    for (k, segment) in path.iter().enumerate() {
        let region = regions.get(segment.region)?;
        let dir = segment.flipped;
        let mut i_hit = region.left_point(dir);
        let first = region.left.vline;
        if open && k > 0 {
            let before = path.get(k - 1)?;
            let prev_region = regions.get(before.region)?;
            let v_prev = prev_region.right.vline;
            let i_prev = prev_region.right_point(before.flipped);
            let ip = at(segs, v_prev, i_prev)?;
            let mut extended = false;
            if v_prev + 1 == first && ip.right_horizontal() == i_hit && ip.next_quality == Quality::Valid {
                emit_horizontal(o, segs, v_prev, i_prev, i_hit, out.last_mut()?, true)?;
                extended = true;
            }
            if !extended {
                let outer = if ip.is_low() { i_prev - 1 } else { i_prev + 1 };
                *out.last_mut()?.last_mut()? = point(segs, v_prev, outer)?;
                finish(&mut out)?;
                open = false;
            }
        }
        for v in first..=region.right.vline {
            let mut it = i_hit;
            let hit = at(segs, v, it)?;
            let going_up = hit.is_low();
            if open {
                out.last_mut()?.push(point(segs, v, it)?);
            } else {
                out.push(vec![point(segs, v, it + if going_up { -1 } else { 1 })?]);
                open = true;
            }
            let iright = hit.right_horizontal();
            let line = out.last_mut()?;
            let (end, iright) = walk_run(segs, v, it, going_up, iright, |stop, next| {
                line.push(point(segs, v, stop)?);
                if next != -1 {
                    let h = at(segs, v, stop)?;
                    let forward = if going_up { h.prev_type == Link::Up } else { h.next_type == Link::Down };
                    emit_vertical(o, segs, v, stop, next, line, forward)?;
                }
                Some(())
            })?;
            it = end;
            if v == region.right.vline {
                break;
            }
            let hit = at(segs, v, it)?;
            let inext = hit.right_horizontal();
            i_hit = if going_up { run_top(segs, v + 1, iright)? } else { run_bottom(segs, v + 1, iright)? };
            if inext == i_hit && hit.next_quality == Quality::Valid {
                emit_horizontal(o, segs, v, it, inext, out.last_mut()?, true)?;
            } else {
                let outer = it + if going_up { 1 } else { -1 };
                *out.last_mut()?.last_mut()? = point(segs, v, outer)?;
                finish(&mut out)?;
                open = false;
            }
        }
    }
    if open {
        let tail = path.last()?;
        let region = regions.get(tail.region)?;
        let i = region.right_point(tail.flipped);
        let outer = if at(segs, region.right.vline, i)?.is_low() { i - 1 } else { i + 1 };
        *out.last_mut()?.last_mut()? = point(segs, region.right.vline, outer)?;
        finish(&mut out)?;
    }
    Some(out)
}

/// The two offsets of `fill_surface_by_lines`, as `ExPolygonWithOffset`'s integer arguments.
fn offsets(spacing: f64) -> (i64, i64) {
    let inner = 0.5 - f64::from(OVERLAP_OVER_SPACING);
    (((-(inner * spacing) / SCALING) as f32) as i64, ((-(0.5f32 as f64) * spacing / SCALING) as f32) as i64)
}

/// `fill_surface_by_lines` for one expolygon (`shape`, contour first, in Orca's integers).
pub fn fill(shape: &[Path], job: &Job) -> Option<Filled> {
    if !(job.spacing > 0.0 && job.spacing.is_finite() && job.density > 0.0 && job.angle.is_finite()) {
        return None;
    }
    let mut spacing = job.spacing;
    let mut line_spacing = scaled_spacing(job);
    if line_spacing <= 0 {
        return None;
    }
    let (offset1, offset2) = offsets(job.spacing);
    let o = Offsetted::new(shape, -job.angle, offset1, offset2);
    if o.contours.len() == o.outer {
        return Some(Filled { lines: Vec::new(), spacing });
    }
    let (mut lo, hi) = o.bounds()?;
    let full = job.density > 0.9999;
    if full && !job.dont_adjust {
        line_spacing = adjust_solid_spacing(hi.x - lo.x, line_spacing);
        spacing = line_spacing as f64 * SCALING;
    } else {
        // Aligned on the object's centre, the point everything here is measured from.
        lo.x = lo.x.min(align_to_grid(lo.x, line_spacing));
    }
    let n_lines = (hi.x - lo.x + line_spacing - 1) / line_spacing;
    if !(0..=MAX_LINES).contains(&n_lines) {
        return None;
    }
    let mut x0 = lo.x;
    if full {
        x0 += (line_spacing + SCALED_EPSILON) / 2;
    }
    let mut segs = slice(&o, n_lines as usize, x0, line_spacing)?;
    connect(&o, &mut segs, job.dont_connect)?;
    let mut lines = if job.monotonic {
        pinch(&mut segs)?;
        let mut regions = monotonic_regions(&mut segs)?;
        connect_regions(&mut regions, &o, &segs)?;
        if regions.is_empty() {
            Vec::new()
        } else {
            // Orca's generator starts afresh for each fill, at its default seed.
            let mut rng = Mt64::new(5489);
            let path = chain_regions(&regions, &segs, &mut rng)?;
            polylines(&path, &regions, &o, &segs)?
        }
    } else {
        traverse(&o, &mut segs)?
    };
    for line in &mut lines {
        rotate(line, f64::from(job.angle));
        remove_duplicates(line);
    }
    Some(Filled { lines, spacing })
}

/// One direction of a pattern of crossing lines: Orca's `SweepParams`.
#[derive(Clone, Copy, Debug)]
pub struct Sweep {
    pub angle: f32,
    pub shift: f32,
}

/// `make_fill_lines`: straight lines over the turned region, each kept when it clears the
/// region's sides, turned back.
fn make_fill_lines(
    o: &Offsetted,
    angle: f32,
    margin: i64,
    spacing: i64,
    shift: i64,
    out: &mut Vec<Path>,
) -> Option<()> {
    let (mut lo, hi) = o.bounds()?;
    let (x_min, x_max) = (lo.x + margin, hi.x - margin);
    let shift = shift % spacing;
    let refpt = -(if shift >= 0 { shift } else { spacing + shift });
    lo.x = lo.x.min(refpt + align_to_grid(lo.x - refpt, spacing));
    let n_lines = (hi.x - lo.x + spacing - 1) / spacing;
    if !(0..=MAX_LINES).contains(&n_lines) {
        return None;
    }
    for line in slice(o, n_lines as usize, lo.x, spacing)? {
        if line.pos < x_min {
            continue;
        }
        if line.pos > x_max {
            break;
        }
        let mut i = 0;
        while let Some(low) = line.hits.get(i) {
            i += 1;
            if low.kind != Type::OuterLow {
                continue;
            }
            let Some(high) = line.hits.get(i).filter(|h| h.kind == Type::OuterHigh) else { continue };
            let mut l = vec![Pt::new(line.pos, low.pos()), Pt::new(line.pos, high.pos())];
            if angle != 0.0 {
                rotate(&mut l, f64::from(angle));
            }
            out.push(l);
            i += 1;
        }
    }
    Some(())
}

/// The uncut lines of `fill_surface_by_multilines` for one expolygon: `spacing` and `density`
/// as Orca's `Fill` has them, `angle` from `direction`. None is `InfillFailedException`.
pub fn crossing_lines(shape: &[Path], spacing: f64, density: f32, angle: f32, sweeps: &[Sweep]) -> Option<Vec<Path>> {
    if !(spacing > 0.0 && spacing.is_finite() && density > 0.0 && angle.is_finite()) || sweeps.is_empty() {
        return None;
    }
    let density = density / sweeps.len() as f32;
    let scaled = spacing / SCALING;
    let base = Offsetted::new(shape, 0.0, ((0.5 * spacing / SCALING) as f32) as i64, 0);
    if base.contours.is_empty() {
        return Some(Vec::new());
    }
    let line_spacing = (scaled / f64::from(density)) as i64;
    if line_spacing <= 0 {
        return None;
    }
    let mut lines = Vec::new();
    for sweep in sweeps {
        let turned = angle + sweep.angle;
        let shift = (f64::from(sweep.shift) / SCALING) as i64;
        make_fill_lines(
            &base.turned(-turned),
            turned,
            scaled as i64 + SCALED_EPSILON,
            line_spacing,
            shift,
            &mut lines,
        )?;
    }
    Some(lines)
}

/// The spacing `fill` would settle on, without filling: what the flow's width follows.
pub fn spacing_of(shape: &[Path], job: &Job) -> Option<f64> {
    let line_spacing = scaled_spacing(job);
    if line_spacing <= 0 {
        return None;
    }
    if job.density <= 0.9999 || job.dont_adjust {
        return Some(job.spacing);
    }
    let mut contour = shape.first()?.clone();
    if job.angle != 0.0 {
        rotate(&mut contour, f64::from(-job.angle));
    }
    let (lo, hi) = contour.iter().fold((i64::MAX, i64::MIN), |(lo, hi), p| (lo.min(p.x), hi.max(p.x)));
    (lo <= hi).then(|| adjust_solid_spacing(hi - lo, line_spacing) as f64 * SCALING)
}

#[cfg(test)]
mod crossing_tests {
    use super::*;

    #[test]
    fn a_grid_crosses_and_stays_in_bounds() {
        let side = 20_000_000;
        let square = vec![Pt::new(0, 0), Pt::new(side, 0), Pt::new(side, side), Pt::new(0, side)];
        let sweeps = [Sweep { angle: 0.0, shift: 0.0 }, Sweep { angle: std::f32::consts::FRAC_PI_2, shift: 0.0 }];
        let lines = crossing_lines(&[square], 0.45, 0.2, direction(0.0, false), &sweeps).unwrap_or_default();
        assert!(lines.len() >= 8, "{} lines", lines.len());
        let reach = side + 300_000;
        assert!(lines.iter().flatten().all(|p| p.x >= -300_000 && p.x <= reach && p.y >= -300_000 && p.y <= reach));
    }

    #[test]
    fn nonsense_fails_quietly() {
        let square = vec![Pt::new(0, 0), Pt::new(1000, 0), Pt::new(1000, 1000)];
        assert!(
            crossing_lines(std::slice::from_ref(&square), 0.0, 0.2, 0.0, &[Sweep { angle: 0.0, shift: 0.0 }]).is_none()
        );
        assert!(crossing_lines(&[square], 0.45, 0.2, f32::NAN, &[Sweep { angle: 0.0, shift: 0.0 }]).is_none());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn square(size: i64) -> Vec<Path> {
        vec![vec![Pt::new(0, 0), Pt::new(size, 0), Pt::new(size, size), Pt::new(0, size)]]
    }

    fn job(monotonic: bool) -> Job {
        Job {
            spacing: 0.4,
            density: 1.0,
            angle: std::f64::consts::FRAC_PI_2 as f32,
            dont_connect: false,
            monotonic,
            dont_adjust: false,
        }
    }

    #[test]
    fn a_square_fills_with_one_zig_zag() {
        for monotonic in [false, true] {
            let out = fill(&square(10_000_000), &job(monotonic)).unwrap_or_default();
            assert_eq!(out.lines.len(), 1, "monotonic {monotonic}");
            let spread = out.spacing / 0.4;
            assert!((1.0..1.2).contains(&spread), "{spread}");
        }
    }

    #[test]
    fn unconnected_lines_stay_apart_and_in_order() {
        let mut j = job(true);
        j.dont_connect = true;
        let out = fill(&square(10_000_000), &j).unwrap_or_default();
        assert!(out.lines.len() > 20, "{}", out.lines.len());
        assert!(out.lines.iter().all(|l| l.len() == 2));
    }

    #[test]
    fn a_hole_makes_monotonic_regions_round_it() {
        let mut shape = square(10_000_000);
        shape.push(vec![
            Pt::new(4_000_000, 4_000_000),
            Pt::new(4_000_000, 6_000_000),
            Pt::new(6_000_000, 6_000_000),
            Pt::new(6_000_000, 4_000_000),
        ]);
        let out = fill(&shape, &job(true)).unwrap_or_default();
        assert!(out.lines.len() >= 2, "{}", out.lines.len());
        let hit = out
            .lines
            .iter()
            .flatten()
            .any(|p| p.x > 4_300_000 && p.x < 5_700_000 && p.y > 4_300_000 && p.y < 5_700_000);
        assert!(!hit, "a line crosses the hole");
    }

    #[test]
    fn nonsense_fills_nothing() {
        let mut j = job(false);
        j.density = 0.0;
        assert!(fill(&square(1_000_000), &j).is_none());
        j = job(false);
        j.spacing = f64::NAN;
        assert!(fill(&square(1_000_000), &j).is_none());
        assert!(fill(&[], &job(true)).is_none_or(|f| f.lines.is_empty()));
    }
}
