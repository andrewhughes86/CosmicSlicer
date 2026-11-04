//! Joining infill lines along the region's edge: a port of OrcaSlicer's `Fill::connect_infill`.
//!
//! Each line end becomes a point on the edge it touches. Arcs of edge between neighbouring line
//! ends are taken shortest first: an arc under `anchor_max` joins its two lines; a longer one
//! gives each line a hook of `anchor`, stopping short of other lines. Edge next to a line
//! already there is left alone.

use std::collections::HashMap;

use glam::DVec2;

use crate::geom::{Contour, Polyline};

/// OrcaSlicer's `SCALED_EPSILON`, in mm.
const EPS: f64 = 1e-4;
/// How far a line's end may be from the edge and still count as on it. Our clipper's integers
/// are coarser than Orca's nanometres.
const ON_EDGE: f64 = 2e-4;
const NONE: usize = usize::MAX;

/// `std::sort` as libstdc++ does it, so that equal keys land the same way.
pub fn std_sort<T: Clone>(v: &mut [T], less: &dyn Fn(&T, &T) -> bool) {
    const THRESHOLD: usize = 16;
    fn move_median<T>(v: &mut [T], result: usize, a: usize, b: usize, c: usize, less: &dyn Fn(&T, &T) -> bool) {
        let lt = |v: &[T], i: usize, j: usize| matches!((v.get(i), v.get(j)), (Some(x), Some(y)) if less(x, y));
        let pick = if lt(v, a, b) {
            if lt(v, b, c) {
                b
            } else if lt(v, a, c) {
                c
            } else {
                a
            }
        } else if lt(v, a, c) {
            a
        } else if lt(v, b, c) {
            c
        } else {
            b
        };
        v.swap(result, pick);
    }
    fn partition<T>(
        v: &mut [T],
        mut first: usize,
        mut last: usize,
        pivot: usize,
        less: &dyn Fn(&T, &T) -> bool,
    ) -> usize {
        let lt = |v: &[T], i: usize, j: usize| matches!((v.get(i), v.get(j)), (Some(x), Some(y)) if less(x, y));
        loop {
            while first < last && lt(v, first, pivot) {
                first += 1;
            }
            if last == 0 {
                return first;
            }
            last -= 1;
            while last > first && lt(v, pivot, last) {
                last -= 1;
            }
            if first >= last {
                return first;
            }
            v.swap(first, last);
            first += 1;
        }
    }
    fn introsort<T: Clone>(v: &mut [T], first: usize, mut last: usize, mut depth: u32, less: &dyn Fn(&T, &T) -> bool) {
        while last - first > THRESHOLD {
            if depth == 0 {
                // Only a contrived order gets here; any correct sort will do.
                if let Some(part) = v.get_mut(first..last) {
                    heap_sort(part, less);
                }
                return;
            }
            depth -= 1;
            let mid = first + (last - first) / 2;
            move_median(v, first, first + 1, mid, last - 1, less);
            let cut = partition(v, first + 1, last, first, less);
            introsort(v, cut, last, depth, less);
            last = cut;
        }
    }
    fn heap_sort<T: Clone>(v: &mut [T], less: &dyn Fn(&T, &T) -> bool) {
        let n = v.len();
        let sift = |v: &mut [T], mut at: usize, end: usize| {
            loop {
                let mut child = 2 * at + 1;
                if child >= end {
                    break;
                }
                if child + 1 < end && matches!((v.get(child), v.get(child + 1)), (Some(x), Some(y)) if less(x, y)) {
                    child += 1;
                }
                if !matches!((v.get(at), v.get(child)), (Some(x), Some(y)) if less(x, y)) {
                    break;
                }
                v.swap(at, child);
                at = child;
            }
        };
        for at in (0..n / 2).rev() {
            sift(v, at, n);
        }
        for end in (1..n).rev() {
            v.swap(0, end);
            sift(v, 0, end);
        }
    }
    fn insertion<T: Clone>(v: &mut [T], from: usize, to: usize, less: &dyn Fn(&T, &T) -> bool) {
        for i in from + 1..to {
            let Some(item) = v.get(i).cloned() else { continue };
            let mut j = i;
            while j > from && v.get(j - 1).is_some_and(|prev| less(&item, prev)) {
                v.swap(j, j - 1);
                j -= 1;
            }
        }
    }
    let n = v.len();
    if n < 2 {
        return;
    }
    introsort(v, 0, n, 2 * n.ilog2(), less);
    if n > THRESHOLD {
        insertion(v, 0, THRESHOLD, less);
        for i in THRESHOLD..n {
            let Some(item) = v.get(i).cloned() else { continue };
            let mut j = i;
            while j > 0 && v.get(j - 1).is_some_and(|prev| less(&item, prev)) {
                v.swap(j, j - 1);
                j -= 1;
            }
        }
    } else {
        insertion(v, 0, n, less);
    }
}

#[derive(Clone, Copy)]
struct Point {
    contour: usize,
    point: usize,
    param: f64,
    prev: usize,
    next: usize,
    free_prev: f64,
    free_next: f64,
    consumed: bool,
    prev_trimmed: bool,
    next_trimmed: bool,
}

impl Point {
    fn unconnected() -> Point {
        Point {
            contour: NONE,
            point: NONE,
            param: 0.0,
            prev: NONE,
            next: NONE,
            free_prev: f64::MAX,
            free_next: f64::MAX,
            consumed: false,
            prev_trimmed: false,
            next_trimmed: false,
        }
    }

    fn consume_prev(&mut self) {
        self.free_prev = 0.0;
        self.prev_trimmed = true;
        self.consumed = true;
    }

    fn consume_next(&mut self) {
        self.free_next = 0.0;
        self.next_trimmed = true;
        self.consumed = true;
    }

    fn trim_prev(&mut self, len: f64) {
        if len < self.free_prev {
            self.free_prev = len;
            self.prev_trimmed = true;
        }
    }

    fn trim_next(&mut self, len: f64) {
        if len < self.free_next {
            self.free_next = len;
            self.next_trimmed = true;
        }
    }
}

fn ccw(from: f64, to: f64, length: f64) -> f64 {
    let d = to - from;
    if d < 0.0 { d + length } else { d }
}

/// Buckets of segments of the contours, by the points along them.
struct Buckets {
    cell: f64,
    cells: HashMap<(i64, i64), Vec<(u32, u32)>>,
}

impl Buckets {
    fn key(&self, p: DVec2) -> (i64, i64) {
        ((p.x / self.cell).floor() as i64, (p.y / self.cell).floor() as i64)
    }

    fn new(contours: &[Contour], cell: f64) -> Buckets {
        let mut out = Buckets { cell: cell.max(1e-3), cells: HashMap::new() };
        for (c, contour) in contours.iter().enumerate() {
            let n = contour.len();
            for i in 0..n {
                let (Some(&a), Some(&b)) = (contour.get(i), contour.get((i + 1) % n)) else { continue };
                let steps = (a.distance(b) / (out.cell * 0.5)).ceil().min(1_000_000.0) as usize + 1;
                let mut last = None;
                for k in 0..=steps {
                    let key = out.key(a.lerp(b, k as f64 / steps as f64));
                    if last != Some(key) {
                        out.cells.entry(key).or_default().push((c as u32, i as u32));
                        last = Some(key);
                    }
                }
            }
        }
        out
    }

    fn around(&self, p: DVec2, reach: i64, into: &mut Vec<(u32, u32)>) {
        let (x, y) = self.key(p);
        for dx in -reach..=reach {
            for dy in -reach..=reach {
                if let Some(list) = self.cells.get(&(x + dx, y + dy)) {
                    into.extend_from_slice(list);
                }
            }
        }
    }
}

/// Where a line meets a segment thickened to round-ended `offset`, as distances along the line.
fn thick_collision(a: DVec2, b: DVec2, sa: DVec2, sb: DVec2, offset: f64) -> Option<(f64, f64)> {
    let v0 = b - a;
    let lv = v0.length_squared();
    let seg = sb - sa;
    let seg_l = seg.length();
    let offset2 = offset * offset;
    let to_seg = |p: DVec2| {
        let len = seg.length_squared();
        if len <= 0.0 {
            return p.distance_squared(sa);
        }
        (sa + seg * ((p - sa).dot(seg) / len).clamp(0.0, 1.0)).distance_squared(p)
    };
    if lv < EPS * EPS {
        let mid = (a + b) * 0.5;
        let hit = if seg_l > EPS { to_seg(mid) < offset2 } else { ((sa + sb) * 0.5).distance_squared(mid) < offset2 };
        return hit.then(|| (0.0, lv.sqrt()));
    }
    let (mut tmin, mut tmax) = (f64::MAX, -f64::MAX);
    let mut circle = |centre: DVec2, r2: f64| {
        let p0 = a - centre;
        let bq = p0.dot(v0);
        let d = bq * bq - lv * (p0.length_squared() - r2);
        if d >= 0.0 {
            let root = d.sqrt();
            let (t0, t1) = (((-bq - root) / lv).max(0.0), ((-bq + root) / lv).min(1.0));
            if t0 <= t1 {
                tmin = tmin.min(t0);
                tmax = tmax.max(t1);
            }
        }
    };
    if seg_l > EPS {
        circle(sa, offset2);
        circle(sb, offset2);
        let x = seg / seg_l;
        let y = x.perp();
        let p = a - sa;
        let (p, v) = (DVec2::new(p.dot(x), p.dot(y)), DVec2::new(v0.dot(x), v0.dot(y)));
        // Liang-Barsky against the box along the segment.
        let (mut t0, mut t1) = (0.0f64, 1.0f64);
        let mut clip = |d: f64, q: f64| -> bool {
            if d == 0.0 {
                return q >= 0.0;
            }
            let r = q / d;
            if d < 0.0 {
                t0 = t0.max(r);
            } else {
                t1 = t1.min(r);
            }
            true
        };
        let inside = clip(-v.x, p.x) && clip(v.x, seg_l - p.x) && clip(-v.y, p.y + offset) && clip(v.y, offset - p.y);
        if inside && t0 <= t1 {
            tmin = tmin.min(t0);
            tmax = tmax.max(t1);
        }
    } else {
        // Orca passes the radius here where the square is meant; kept.
        circle((sa + sb) * 0.5, offset);
    }
    (tmin <= tmax).then(|| (tmin * lv.sqrt(), tmax * lv.sqrt()))
}

struct Graph {
    boundary: Vec<Contour>,
    params: Vec<Vec<f64>>,
    points: Vec<Point>,
    on_contour: Vec<Vec<usize>>,
}

impl Graph {
    fn cp(&self, i: usize) -> Point {
        self.points.get(i).copied().unwrap_or_else(Point::unconnected)
    }

    fn contour_of(&self, i: usize) -> &[DVec2] {
        self.boundary.get(self.cp(i).contour).map_or(&[], |c| c.as_slice())
    }

    fn params_of(&self, i: usize) -> &[f64] {
        self.params.get(self.cp(i).contour).map_or(&[], |c| c.as_slice())
    }

    fn length_of(&self, i: usize) -> f64 {
        self.params_of(i).last().copied().unwrap_or(0.0)
    }

    fn with(&mut self, i: usize, f: impl FnOnce(&mut Point)) {
        if let Some(p) = self.points.get_mut(i) {
            f(p);
        }
    }

    fn position(&self, i: usize) -> Option<DVec2> {
        let p = self.cp(i);
        self.boundary.get(p.contour)?.get(p.point).copied()
    }

    fn could_connect_prev(&self, i: usize) -> bool {
        let p = self.cp(i);
        let o = self.cp(p.prev);
        !p.consumed && p.prev != i && !o.consumed && !p.prev_trimmed && !o.next_trimmed
    }

    fn could_connect_next(&self, i: usize) -> bool {
        let p = self.cp(i);
        let o = self.cp(p.next);
        !p.consumed && p.next != i && !o.consumed && !p.next_trimmed && !o.prev_trimmed
    }

    fn take_full(&self, line: &mut Polyline, contour: &[DVec2], from: usize, to: usize, clockwise: bool) {
        let n = contour.len();
        if n == 0 {
            return;
        }
        let step = |i: usize| if clockwise { (i + n - 1) % n } else { (i + 1) % n };
        let mut i = step(from);
        for _ in 0..n {
            line.extend(contour.get(i).copied());
            if i == to {
                break;
            }
            i = step(i);
        }
    }

    fn take_part(&self, line: &mut Polyline, at: (usize, usize), to: usize, length_to_take: f64, clockwise: bool) {
        let (Some(contour), Some(params)) = (self.boundary.get(at.0), self.params.get(at.0)) else { return };
        let n = contour.len();
        let start = at.1;
        let (Some(&length), Some(&p0)) = (params.last(), params.get(start)) else { return };
        if n == 0 {
            return;
        }
        let step = |i: usize| if clockwise { (i + n - 1) % n } else { (i + 1) % n };
        let mut i = step(start);
        let mut before = start;
        let mut taken = 0.0;
        for _ in 0..n {
            let here = params.get(i).copied().unwrap_or(0.0);
            let l = if clockwise { ccw(here, p0, length) } else { ccw(p0, here, length) };
            if l >= length_to_take {
                let t = (length_to_take - taken) / (l - taken);
                if let (Some(a), Some(b)) = (contour.get(before), contour.get(i)) {
                    line.push(a.lerp(*b, t));
                }
                return;
            }
            line.extend(contour.get(i).copied());
            if i == to {
                return;
            }
            before = i;
            taken = l;
            i = step(i);
        }
    }

    /// Joins the end of `first` to the start of `second` along the edge.
    fn take(&mut self, first: &mut Polyline, second: &[DVec2], start: usize, end: usize, clockwise: bool) {
        let contour = self.contour_of(start).to_vec();
        self.take_full(first, &contour, self.cp(start).point, self.cp(end).point, clockwise);
        first.extend(second.iter().skip(1).copied());
        let (mut start, mut end) = (start, end);
        if clockwise {
            std::mem::swap(&mut start, &mut end);
        }
        if self.cp(start).next != end {
            let mut cp = self.cp(start).next;
            for _ in 0..self.points.len() {
                if self.cp(cp).next == end {
                    break;
                }
                self.with(cp, |p| {
                    p.consume_prev();
                    p.consume_next();
                });
                cp = self.cp(cp).next;
            }
        }
        self.with(start, Point::consume_next);
        self.with(end, Point::consume_prev);
    }

    fn take_limited(&mut self, line: &mut Polyline, start: usize, end: usize, clockwise: bool, max: f64, half: f64) {
        let s = self.cp(start);
        let free = if clockwise { s.free_prev } else { s.free_next };
        if s.consumed || free <= EPS {
            return;
        }
        let Some(at) = self.position(start) else { return };
        let add_at_start = line.first().is_some_and(|p| p.distance(at) < 1e-6);
        if add_at_start {
            line.reverse();
        }
        let length = self.length_of(start);
        let mut to_go = max;
        self.with(start, |p| p.consumed = true);
        if start == end {
            to_go = (to_go.min(length - half)).max(0.0);
            to_go = to_go.min(free);
            self.with(start, |p| {
                p.consume_prev();
                p.consume_next();
            });
            if to_go > EPS {
                self.take_part(line, (s.contour, s.point), s.point, to_go, clockwise);
            }
        } else {
            let mut cp = start;
            for _ in 0..self.points.len() {
                if cp == end {
                    break;
                }
                let here = self.cp(cp);
                let other = if clockwise { here.prev } else { here.next };
                let there = self.cp(other);
                let l =
                    if clockwise { ccw(there.param, here.param, length) } else { ccw(here.param, there.param, length) };
                to_go = to_go.min(if clockwise { here.free_prev } else { here.free_next });
                to_go = (to_go.min(l - half)).max(0.0);
                self.with(cp, |p| if clockwise { p.consume_prev() } else { p.consume_next() });
                if l >= to_go {
                    if to_go > EPS {
                        self.with(other, |p| {
                            if clockwise {
                                p.trim_next(l - to_go);
                            } else {
                                p.trim_prev(l - to_go);
                            }
                        });
                        self.take_part(line, (here.contour, here.point), there.point, to_go, clockwise);
                    }
                    break;
                }
                self.with(other, |p| if clockwise { p.trim_next(0.0) } else { p.trim_prev(0.0) });
                let contour = self.contour_of(cp).to_vec();
                self.take_full(line, &contour, here.point, there.point, clockwise);
                to_go -= l;
                cp = other;
            }
        }
        if add_at_start {
            line.reverse();
        }
    }

    /// Trims the edge where a stretch of infill line runs close along it.
    fn trim_by(&mut self, buckets: &Buckets, a: DVec2, b: DVec2, radius: f64) {
        let mut near: Vec<(u32, u32)> = Vec::new();
        let steps = (a.distance(b) / buckets.cell).ceil().min(1_000_000.0) as usize + 1;
        let reach = (radius / buckets.cell).ceil() as i64 + 1;
        for k in 0..=steps {
            buckets.around(a.lerp(b, k as f64 / steps as f64), reach, &mut near);
        }
        near.sort_unstable();
        near.dedup();
        for (c, s) in near {
            let (c, s) = (c as usize, s as usize);
            let Some(list) = self.on_contour.get(c).cloned().filter(|l| !l.is_empty()) else { continue };
            let (Some(contour), Some(params)) = (self.boundary.get(c), self.params.get(c)) else { continue };
            let n = contour.len();
            let (Some(&p1), Some(&p2)) = (contour.get(s), contour.get((s + 1) % n)) else { continue };
            let Some((from, to)) = thick_collision(p1, p2, a, b, radius) else { continue };
            let length = params.last().copied().unwrap_or(0.0);
            let (Some(&q1), Some(&q2)) = (params.get(s), params.get(s + 1)) else { continue };
            let overlap1 = q2.min(q1 + from);
            let overlap2 = q2.min(q1 + to);
            let (mut low, high);
            if let [only] = list.as_slice() {
                low = *only;
                high = *only;
            } else {
                let lower = |value: f64| list.partition_point(|i| self.cp(*i).param < value);
                let first = list.first().copied().unwrap_or(NONE);
                low = list.get(lower(overlap1)).copied().unwrap_or(first);
                high = list.get(lower(overlap2)).copied().unwrap_or(first);
                if self.cp(low).param != overlap1 {
                    low = self.cp(low).prev;
                }
            }
            if low == NONE {
                continue;
            }
            if self.cp(low).next != high {
                let mut ip = self.cp(low).next;
                for _ in 0..self.points.len() {
                    if ip == high || ip == NONE {
                        break;
                    }
                    self.with(ip, |p| {
                        p.consume_prev();
                        p.consume_next();
                    });
                    ip = self.cp(ip).next;
                }
            }
            let trim = ccw(self.cp(low).param, overlap1, length);
            self.with(low, |p| p.trim_next(trim));
            let trim = ccw(overlap2, self.cp(high).param, length);
            self.with(high, |p| p.trim_prev(trim));
        }
    }
}

/// The start and end of `line` once `clip` is cut off both ends: the segment and point of each.
fn clipped(line: &[DVec2], clip: f64) -> Option<((usize, f64, DVec2), (usize, f64, DVec2))> {
    let mut left = clip;
    let mut start = None;
    for (i, w) in line.windows(2).enumerate() {
        let l = w[0].distance(w[1]);
        if l > left {
            let t = left / l;
            start = Some((i, t, w[0].lerp(w[1], t)));
            break;
        }
        left -= l;
    }
    let mut left = clip;
    let mut end = None;
    for (i, w) in line.windows(2).enumerate().rev() {
        let l = w[0].distance(w[1]);
        if l > left {
            let t = left / l;
            end = Some((i, 1.0 - t, w[1].lerp(w[0], t)));
            break;
        }
        left -= l;
    }
    Some((start?, end?))
}

/// `create_boundary_infill_graph`.
fn graph(infill: &[Polyline], boundary: &[Contour], spacing: f64) -> Graph {
    let buckets = Buckets::new(boundary, 2.0);
    let mut hits: Vec<(usize, usize, f64, usize)> = Vec::with_capacity(infill.len() * 2);
    let mut near = Vec::new();
    for (l, line) in infill.iter().enumerate() {
        for (end, p) in [line.first(), line.last()].into_iter().enumerate() {
            let Some(&p) = p else { continue };
            near.clear();
            buckets.around(p, 1, &mut near);
            let mut best: Option<(f64, usize, usize, f64)> = None;
            for &(c, s) in &near {
                let (c, s) = (c as usize, s as usize);
                let Some(contour) = boundary.get(c) else { continue };
                let (Some(&a), Some(&b)) = (contour.get(s), contour.get((s + 1) % contour.len())) else { continue };
                let v = b - a;
                let len = v.length_squared();
                let t = if len > 0.0 { ((p - a).dot(v) / len).clamp(0.0, 1.0) } else { 0.0 };
                let d = (a + v * t).distance(p);
                if d <= ON_EDGE && best.is_none_or(|x| d < x.0) {
                    best = Some((d, c, s, t));
                }
            }
            if let Some((_, c, s, t)) = best {
                hits.push((c, s, t, l * 2 + end));
            }
        }
    }
    std_sort(&mut hits, &|a, b| a.0 < b.0 || (a.0 == b.0 && (a.1 < b.1 || (a.1 == b.1 && a.2 < b.2))));
    let mut out = Graph {
        boundary: vec![Vec::new(); boundary.len()],
        params: vec![Vec::new(); boundary.len()],
        points: vec![Point::unconnected(); infill.len() * 2],
        on_contour: vec![Vec::new(); boundary.len()],
    };
    let mut hit = hits.iter().peekable();
    for (c, source) in boundary.iter().enumerate() {
        let mut dst: Contour = Vec::with_capacity(source.len());
        let mut list: Vec<usize> = Vec::new();
        let (mut first, mut prev) = (NONE, NONE);
        let same = |a: Option<&DVec2>, b: DVec2| a.is_some_and(|p| p.distance(b) < 1e-6);
        for (i, ipt) in source.iter().enumerate() {
            if !same(dst.last(), *ipt) {
                dst.push(*ipt);
            }
            while let Some(&&(hc, hs, _, which)) = hit.peek() {
                if hc != c || hs != i {
                    break;
                }
                hit.next();
                let line = infill.get(which / 2);
                let pt = line.and_then(|l| if which & 1 == 1 { l.last() } else { l.first() }).copied().unwrap_or(*ipt);
                let mut joint = 0;
                if i + 1 < source.len() || !same(dst.first(), pt) {
                    if !same(dst.last(), pt) {
                        dst.push(pt);
                    }
                    joint = dst.len() - 1;
                }
                let at = which;
                if let Some(p) = out.points.get_mut(at) {
                    *p = Point { contour: c, point: joint, ..Point::unconnected() };
                }
                if prev != NONE {
                    out.with(prev, |p| p.next = at);
                    out.with(at, |p| p.prev = prev);
                } else {
                    first = at;
                }
                list.push(at);
                prev = at;
            }
            if first != NONE {
                let (head, tail) = (first, prev);
                out.with(tail, |p| p.next = head);
                out.with(head, |p| p.prev = tail);
            }
        }
        let mut params = vec![0f64; dst.len() + 1];
        for i in 1..dst.len() {
            let last = params.get(i - 1).copied().unwrap_or(0.0);
            let step = dst.get(i).zip(dst.get(i - 1)).map_or(0.0, |(a, b)| a.distance(*b));
            if let Some(slot) = params.get_mut(i) {
                *slot = last + step;
            }
        }
        let closing = dst.first().zip(dst.last()).map_or(0.0, |(a, b)| a.distance(*b));
        let before = dst.len().checked_sub(1).and_then(|k| params.get(k)).copied().unwrap_or(0.0);
        if let Some(slot) = params.last_mut() {
            *slot = before + closing;
        }
        let length = params.last().copied().unwrap_or(0.0);
        for ip in &list {
            let point = out.cp(*ip).point;
            let param = params.get(point).copied().unwrap_or(0.0);
            out.with(*ip, |p| p.param = param);
        }
        for ip in &list {
            let this = out.cp(*ip);
            let (before, after) = if this.next == *ip {
                (length, length)
            } else {
                (ccw(out.cp(this.prev).param, this.param, length), ccw(this.param, out.cp(this.next).param, length))
            };
            out.with(*ip, |p| {
                p.free_prev = before;
                p.free_next = after;
            });
        }
        if let (Some(a), Some(b), Some(d)) = (out.boundary.get_mut(c), out.params.get_mut(c), out.on_contour.get_mut(c))
        {
            *a = dst;
            *b = params;
            *d = list;
        }
    }
    let grid = Buckets::new(&out.boundary, (0.8 * spacing).max(2.0));
    for line in infill {
        let Some(((s0, t0, p0), (s1, t1, p1))) = clipped(line, 1.7 * spacing) else { continue };
        if !(s0 < s1 || (s0 == s1 && t0 < t1)) {
            continue;
        }
        for i in s0..=s1 {
            let a = if i == s0 { p0 } else { line.get(i).copied().unwrap_or(p0) };
            let b = if i == s1 { p1 } else { line.get(i + 1).copied().unwrap_or(p1) };
            out.trim_by(&grid, a, b, 0.8 * spacing);
        }
    }
    out
}

/// Joins `infill`, in this order, along the contours of `boundary`, which its ends touch.
pub fn connect_infill(
    mut infill: Vec<Polyline>,
    boundary: &[Contour],
    spacing: f64,
    anchor: f64,
    anchor_max: f64,
) -> Vec<Polyline> {
    if infill.is_empty() {
        return infill;
    }
    let mut g = graph(&infill, boundary, spacing);
    let half = 0.5 * spacing;
    let mut merged: Vec<usize> = (0..infill.len()).collect();
    fn root(merged: &mut [usize], from: usize) -> usize {
        let mut last = from;
        for _ in 0..merged.len() {
            let lower = merged.get(last).copied().unwrap_or(last);
            if lower == last {
                break;
            }
            last = lower;
        }
        if let Some(slot) = merged.get_mut(from) {
            *slot = last;
        }
        last
    }
    let mut arcs: Vec<(usize, f64)> = Vec::with_capacity(g.points.len());
    for i in 0..g.points.len() {
        let p = g.cp(i);
        if p.contour != NONE && p.next != i && g.could_connect_next(i) {
            arcs.push((i, ccw(p.param, g.cp(p.next).param, g.length_of(i))));
        }
    }
    std_sort(&mut arcs, &|a, b| a.1 < b.1);
    let same = |a: Option<&DVec2>, b: Option<DVec2>| matches!((a, b), (Some(x), Some(y)) if x.distance(y) < 1e-6);
    for (cp1, length) in arcs {
        let cp2 = g.cp(cp1).next;
        if g.cp(cp1).consumed || g.cp(cp2).consumed {
            continue;
        }
        let (i1, i2) = (root(&mut merged, cp1 / 2), root(&mut merged, cp2 / 2));
        if i1 == i2 {
            continue;
        }
        let (Some(mut one), Some(mut two)) = (infill.get(i1).cloned(), infill.get(i2).cloned()) else { continue };
        if length < anchor_max {
            if same(one.first(), g.position(cp1)) {
                one.reverse();
            }
            if same(two.last(), g.position(cp2)) {
                two.reverse();
            }
            g.take(&mut one, &two, cp1, cp2, false);
            if i2 < i1 {
                if let Some(slot) = infill.get_mut(i2) {
                    *slot = one;
                }
                if let Some(slot) = infill.get_mut(i1) {
                    slot.clear();
                }
                let to = merged.get(i2).copied().unwrap_or(i2);
                if let Some(slot) = merged.get_mut(i1) {
                    *slot = to;
                }
            } else {
                if let Some(slot) = infill.get_mut(i1) {
                    *slot = one;
                }
                if let Some(slot) = infill.get_mut(i2) {
                    slot.clear();
                }
                let to = merged.get(i1).copied().unwrap_or(i1);
                if let Some(slot) = merged.get_mut(i2) {
                    *slot = to;
                }
            }
        } else if anchor > EPS {
            g.take_limited(&mut one, cp1, cp2, false, anchor, half);
            g.take_limited(&mut two, cp2, cp1, true, anchor, half);
            if let Some(slot) = infill.get_mut(i1) {
                *slot = one;
            }
            if let Some(slot) = infill.get_mut(i2) {
                *slot = two;
            }
        }
    }
    // Whatever is still open: the arcs each side if free, else a hook.
    for point in 0..g.points.len() {
        let p = g.cp(point);
        if p.consumed || p.contour == NONE {
            continue;
        }
        let length = g.length_of(point);
        let lprev = if g.could_connect_prev(point) { ccw(g.cp(p.prev).param, p.param, length) } else { f64::MAX };
        let lnext = if g.could_connect_next(point) { ccw(p.param, g.cp(p.next).param, length) } else { f64::MAX };
        let line = root(&mut merged, point / 2);
        let mut connected = false;
        for l in [lprev.min(lnext), lprev.max(lnext)] {
            if l == f64::MAX || l > anchor_max {
                break;
            }
            let reversed = l == lprev;
            let cp2 = if reversed { p.prev } else { p.next };
            let other = root(&mut merged, cp2 / 2);
            if line == other {
                continue;
            }
            let (Some(mut one), Some(mut two)) = (infill.get(line).cloned(), infill.get(other).cloned()) else {
                continue;
            };
            if same(one.first(), g.position(point)) {
                one.reverse();
            }
            if same(two.last(), g.position(cp2)) {
                two.reverse();
            }
            g.take(&mut one, &two, point, cp2, reversed);
            let (keep, gone) = if line < other { (line, other) } else { (other, line) };
            if let Some(slot) = merged.get_mut(gone) {
                *slot = keep;
            }
            if let Some(slot) = infill.get_mut(keep) {
                *slot = one;
            }
            if let Some(slot) = infill.get_mut(gone) {
                slot.clear();
            }
            connected = true;
            break;
        }
        if !connected && anchor > EPS {
            let here = g.cp(point);
            if here.free_prev.max(here.free_next) > EPS {
                let Some(mut one) = infill.get(line).cloned() else { continue };
                if here.free_prev > here.free_next {
                    g.take_limited(&mut one, point, here.prev, true, anchor, half);
                } else {
                    g.take_limited(&mut one, point, here.next, false, anchor, half);
                }
                if let Some(slot) = infill.get_mut(line) {
                    *slot = one;
                }
            }
        }
    }
    infill.retain(|l| !l.is_empty());
    infill
}

#[cfg(test)]
mod tests {
    use super::*;

    fn square(s: f64) -> Contour {
        vec![DVec2::ZERO, DVec2::new(s, 0.0), DVec2::new(s, s), DVec2::new(0.0, s)]
    }

    #[test]
    fn sorting_gives_ascending_keys_at_any_size() {
        for n in [0usize, 1, 2, 16, 17, 100, 1000] {
            let mut v: Vec<u64> = (0..n as u64).map(|i| i.wrapping_mul(2654435761) % 977).collect();
            std_sort(&mut v, &|a, b| a < b);
            assert!(v.windows(2).all(|w| w[0] <= w[1]), "{n}");
        }
    }

    #[test]
    fn sorting_survives_an_inconsistent_order() {
        let mut v: Vec<u64> = (0..1000).collect();
        std_sort(&mut v, &|a, b| (a ^ b) % 3 == 1);
        assert_eq!(v.len(), 1000);
    }

    #[test]
    fn near_neighbours_join_along_the_edge() {
        // Three lines across a square, 2 mm apart.
        let lines: Vec<Polyline> =
            [2.0, 4.0, 6.0].iter().map(|y| vec![DVec2::new(0.0, *y), DVec2::new(10.0, *y)]).collect();
        let out = connect_infill(lines, &[square(10.0)], 0.4, 1.0, 20.0);
        assert_eq!(out.len(), 1);
        let n = out.first().map_or(0, Vec::len);
        assert!(n >= 6, "{n}");
    }

    #[test]
    fn far_neighbours_only_get_hooks() {
        let lines: Vec<Polyline> =
            [1.0, 39.0].iter().map(|y| vec![DVec2::new(0.0, *y), DVec2::new(40.0, *y)]).collect();
        let out = connect_infill(lines, &[square(40.0)], 0.4, 1.5, 5.0);
        assert_eq!(out.len(), 2);
        let total: f64 = out.iter().map(|l| crate::geom::length(l)).sum();
        assert!(total > 80.0 && total < 80.0 + 4.0 * 1.5 + 0.01, "{total}");
    }

    #[test]
    fn lines_that_touch_no_edge_stay_as_they_are() {
        let lines: Vec<Polyline> = vec![vec![DVec2::new(3.0, 3.0), DVec2::new(5.0, 3.0)]];
        let out = connect_infill(lines.clone(), &[square(10.0)], 0.4, 1.0, 20.0);
        assert_eq!(out, lines);
    }
}
