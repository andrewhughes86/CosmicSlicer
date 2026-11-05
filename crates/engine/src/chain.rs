//! OrcaSlicer's `chain_segments_greedy_constrained_reversals`: the order pieces of a path print
//! in, keeping travels short. A shortest link is added as long as it makes no fork or loop, so
//! the tour grows from many fragments at once; this is not "always the nearest next".
//!
//! With its `KDTreeIndirect` and `MutablePriorityQueue`, as ties land by how those walk.

/// `KDTreeIndirect::npos`.
const NPOS: usize = usize::MAX;

/// A 2D `KDTreeIndirect` over `points`.
struct KdTree<'a> {
    points: &'a [[f64; 2]],
    nodes: Vec<usize>,
}

impl<'a> KdTree<'a> {
    fn new(points: &'a [[f64; 2]]) -> KdTree<'a> {
        let mut tree = KdTree { points, nodes: Vec::new() };
        if !points.is_empty() {
            let mut input: Vec<usize> = (0..points.len()).collect();
            tree.nodes = vec![NPOS; (points.len() + 1).next_power_of_two()];
            tree.build(&mut input, 0, 0, 0, points.len() - 1);
        }
        tree
    }

    fn coord(&self, i: usize, dimension: usize) -> f64 {
        self.points[i][dimension]
    }

    fn build(&mut self, input: &mut [usize], node: usize, dimension: usize, left: usize, right: usize) {
        if left > right {
            return;
        }
        if left == right {
            self.nodes[node] = input[left];
            return;
        }
        let center = (left + right) / 2;
        self.partition(input, dimension, left, right, center);
        self.nodes[node] = input[center];
        let next = 1 - dimension;
        if center > left {
            self.build(input, node * 2 + 1, next, left, center - 1);
        }
        self.build(input, node * 2 + 2, next, center + 1, right);
    }

    /// `partition_input`: quickselect, median of three.
    fn partition(&self, input: &mut [usize], dimension: usize, mut left: usize, mut right: usize, k: usize) {
        while left < right {
            let center = (left + right) / 2;
            let mut left_value = self.coord(input[left], dimension);
            let mut center_value = self.coord(input[center], dimension);
            let right_value = self.coord(input[right], dimension);
            if left_value > center_value {
                input.swap(left, center);
                std::mem::swap(&mut left_value, &mut center_value);
            }
            let mut right_value = right_value;
            if left_value > right_value {
                input.swap(left, right);
                right_value = left_value;
            }
            if center_value > right_value {
                input.swap(center, right);
                center_value = right_value;
            }
            let pivot = center_value;
            if right <= left + 2 {
                break;
            }
            let mut i = left;
            let mut j = right - 1;
            input.swap(center, j);
            loop {
                loop {
                    i += 1;
                    if self.coord(input[i], dimension) >= pivot {
                        break;
                    }
                }
                loop {
                    j -= 1;
                    if !(self.coord(input[j], dimension) > pivot && i < j) {
                        break;
                    }
                }
                if i >= j {
                    break;
                }
                input.swap(i, j);
            }
            input.swap(i, right - 1);
            match k.cmp(&i) {
                std::cmp::Ordering::Less => right = i - 1,
                std::cmp::Ordering::Equal => break,
                std::cmp::Ordering::Greater => left = i + 1,
            }
        }
    }

    /// `find_closest_point`: of equally close points, the last the walk reaches.
    fn closest(&self, point: [f64; 2], filter: &mut dyn FnMut(usize) -> bool) -> usize {
        let mut best = (NPOS, f64::MAX);
        if !self.nodes.is_empty() {
            self.visit(0, 0, point, filter, &mut best);
        }
        best.0
    }

    fn visit(
        &self,
        node: usize,
        dimension: usize,
        point: [f64; 2],
        filter: &mut dyn FnMut(usize) -> bool,
        best: &mut (usize, f64),
    ) {
        let Some(&idx) = self.nodes.get(node) else { return };
        if idx == NPOS {
            return;
        }
        if filter(idx) {
            let (dx, dy) = (point[0] - self.coord(idx, 0), point[1] - self.coord(idx, 1));
            let dist = dx * dx + dy * dy;
            if dist <= best.1 {
                *best = (idx, dist);
            }
        }
        let d = point[dimension] - self.coord(idx, dimension);
        let both = d * d < best.1 + EPSILON;
        let next = 1 - dimension;
        if both || d <= 0.0 {
            self.visit(node * 2 + 1, next, point, filter, best);
        }
        if both || d > 0.0 {
            self.visit(node * 2 + 2, next, point, filter, best);
        }
    }
}

/// `EPSILON`.
const EPSILON: f64 = 1e-4;

/// `EndPoint`.
struct End {
    pos: [f64; 2],
    chain: usize,
    edge_out: usize,
    distance: f64,
    heap_idx: usize,
}

/// `MutablePriorityQueue` of end points by `distance`.
#[derive(Default)]
struct Queue {
    heap: Vec<usize>,
}

impl Queue {
    fn less(ends: &[End], a: usize, b: usize) -> bool {
        ends[a].distance < ends[b].distance
    }

    fn set(&mut self, ends: &mut [End], at: usize, item: usize) {
        self.heap[at] = item;
        ends[item].heap_idx = at;
    }

    fn push(&mut self, ends: &mut [End], item: usize) {
        let idx = self.heap.len();
        self.heap.push(item);
        ends[item].heap_idx = idx;
        self.up(ends, 0, idx);
    }

    fn top(&self) -> Option<usize> {
        self.heap.first().copied()
    }

    fn pop(&mut self, ends: &mut [End]) {
        if self.heap.len() > 1 {
            let last = self.heap[self.heap.len() - 1];
            self.set(ends, 0, last);
            self.heap.pop();
            self.down(ends, 0, self.heap.len() - 1);
        } else {
            self.heap.clear();
        }
    }

    fn remove(&mut self, ends: &mut [End], idx: usize) {
        if idx + 1 == self.heap.len() {
            self.heap.pop();
            return;
        }
        let last = self.heap[self.heap.len() - 1];
        self.set(ends, idx, last);
        self.heap.pop();
        self.down(ends, idx, self.heap.len() - 1);
        self.up(ends, 0, idx);
    }

    fn update(&mut self, ends: &mut [End], idx: usize) {
        let item = self.heap[idx];
        self.remove(ends, idx);
        self.push(ends, item);
    }

    /// `update_heap_up`: goes on to `top` even past a parent in order, as Orca's does.
    fn up(&mut self, ends: &mut [End], top: usize, bottom: usize) {
        let mut child = bottom;
        loop {
            if child == 0 {
                break;
            }
            let parent = (child - 1) >> 1;
            if parent < top {
                break;
            }
            let (p, c) = (self.heap[parent], self.heap[child]);
            if !Self::less(ends, p, c) {
                self.set(ends, child, p);
                self.set(ends, parent, c);
            }
            child = parent;
        }
    }

    fn down(&mut self, ends: &mut [End], top: usize, bottom: usize) {
        let mut parent = top;
        loop {
            let mut child = (parent << 1) + 1;
            if child > bottom {
                break;
            }
            let second = child + 1;
            if second <= bottom && !Self::less(ends, self.heap[child], self.heap[second]) {
                child = second;
            }
            let (p, c) = (self.heap[parent], self.heap[child]);
            if Self::less(ends, p, c) {
                return;
            }
            self.set(ends, child, p);
            self.set(ends, parent, c);
            parent = child;
        }
    }
}

/// `EquivalentChains`.
struct Equivalent {
    last: usize,
    with: Vec<usize>,
}

impl Equivalent {
    fn next(&mut self) -> usize {
        self.last += 1;
        self.with.push(self.last);
        self.last
    }

    fn get(&mut self, mut id: usize) -> usize {
        if id != 0 {
            let mut last = id;
            loop {
                let lower = self.with[last];
                if lower == last {
                    self.with[id] = lower;
                    id = lower;
                    break;
                }
                last = lower;
            }
        }
        id
    }

    fn merge(&mut self, a: usize, b: usize) -> usize {
        let id = self.get(a).min(self.get(b));
        self.with[a] = id;
        self.with[b] = id;
        id
    }
}

/// `chain_segments_closest_point`, the last resort when a piece that may not turn would have to.
fn closest_point(ends: &mut [End], tree: &KdTree<'_>, reversible: &[bool], first: usize) -> Vec<(usize, bool)> {
    let segments = ends.len() / 2;
    for e in ends.iter_mut() {
        e.chain = 0;
    }
    let mut out = Vec::with_capacity(segments);
    out.push((first / 2, first & 1 != 0));
    ends[first].chain = 1;
    let mut this = first ^ 1;
    for _ in 1..segments {
        ends[this].chain = 1;
        let chains: Vec<usize> = ends.iter().map(|e| e.chain).collect();
        let next = tree.closest(ends[this].pos, &mut |idx| {
            (idx ^ this) > 1 && chains[idx] == 0 && (idx & 1 == 0 || reversible[idx >> 1])
        });
        if next == NPOS {
            break;
        }
        ends[next].chain = 1;
        out.push((next / 2, next & 1 != 0));
        this = next ^ 1;
    }
    out
}

/// The order to print pieces in, from `start`: (piece, reversed). `pieces` are each's first and
/// last point, `reversible` which may print backwards. Every piece comes out once.
pub fn chain(pieces: &[([f64; 2], [f64; 2])], reversible: &[bool], start: [f64; 2]) -> Vec<(usize, bool)> {
    let n = pieces.len();
    let may_turn = |i: usize| reversible.get(i).copied().unwrap_or(false);
    if n == 0 {
        return Vec::new();
    }
    if n == 1 {
        let d = |p: [f64; 2]| (p[0] - start[0]).powi(2) + (p[1] - start[1]).powi(2);
        return vec![(0, may_turn(0) && d(pieces[0].1) < d(pieces[0].0))];
    }
    let turns: Vec<bool> = (0..n).map(may_turn).collect();
    let points: Vec<[f64; 2]> = pieces.iter().flat_map(|p| [p.0, p.1]).collect();
    let mut ends: Vec<End> =
        points.iter().map(|&pos| End { pos, chain: 0, edge_out: NPOS, distance: f64::MAX, heap_idx: NPOS }).collect();
    let dist2 = |a: [f64; 2], b: [f64; 2]| (b[0] - a[0]).powi(2) + (b[1] - a[1]).powi(2);
    let tree = KdTree::new(&points);
    let mut equivalent = Equivalent { last: 0, with: vec![0] };
    let in_order: Vec<(usize, bool)> = (0..n).map(|i| (i, false)).collect();

    let first = tree.closest(start, &mut |idx| idx & 1 == 0 || turns[idx >> 1]);
    if first == NPOS {
        return in_order;
    }
    ends[first].distance = 0.0;
    ends[first].chain = equivalent.next();
    for i in 0..ends.len() {
        if i != first {
            let next = tree.closest(ends[i].pos, &mut |idx| idx != first && (idx ^ i) > 1);
            if next == NPOS {
                return in_order;
            }
            ends[i].edge_out = next;
            ends[i].distance = dist2(ends[i].pos, ends[next].pos);
        }
    }
    let mut queue = Queue::default();
    for i in 0..ends.len() {
        if i != first {
            queue.push(&mut ends, i);
        }
    }

    // (n - 1) shortest links that make neither fork nor loop.
    let mut left = n as i64 - 2;
    let mut guard = 8 * (n as u64 + 2) * (n as u64 + 2);
    loop {
        if guard == 0 {
            return in_order;
        }
        guard -= 1;
        let Some(one) = queue.top() else { return in_order };
        let two = ends[one].edge_out;
        if two == NPOS {
            return in_order;
        }
        let mut valid = true;
        let (mut chain1, mut chain2) = (0, 0);
        if ends[two].chain > 0 {
            valid = false;
        } else {
            chain1 = equivalent.get(ends[one ^ 1].chain);
            chain2 = equivalent.get(ends[two ^ 1].chain);
            if chain1 == chain2 && chain1 != 0 {
                valid = false;
            }
        }
        if valid {
            queue.pop(&mut ends);
            let at = ends[two].heap_idx;
            queue.remove(&mut ends, at);
            ends[two].edge_out = one;
            ends[two].distance = ends[one].distance;
            let id = match (chain1, chain2) {
                (0, 0) => equivalent.next(),
                (0, b) => b,
                (a, 0) => a,
                (a, b) if a == b => a,
                (a, b) => equivalent.merge(a, b),
            };
            ends[one].chain = id;
            ends[two].chain = id;
            if left == 0 {
                let Some(last) = queue.top() else { return in_order };
                ends[last].edge_out = NPOS;
                queue.pop(&mut ends);
                break;
            }
            left -= 1;
        } else {
            ends[one].edge_out = NPOS;
            let chains: Vec<usize> = ends.iter().map(|e| e.chain).collect();
            let this = equivalent.get(chains[one ^ 1]);
            let next = tree.closest(ends[one].pos, &mut |idx| {
                if (idx ^ one) <= 1 || chains[idx] != 0 {
                    return false;
                }
                let other = equivalent.get(chains[idx ^ 1]);
                this != other || this == 0
            });
            if next == NPOS {
                return in_order;
            }
            ends[one].edge_out = next;
            ends[one].distance = dist2(ends[one].pos, ends[next].pos);
            let at = ends[one].heap_idx;
            queue.update(&mut ends, at);
        }
    }

    // Link the pairs into one path.
    let mut out = Vec::with_capacity(n);
    let mut at = first;
    let mut failed = false;
    while at != NPOS && out.len() < n {
        let (segment, reversed) = (at >> 1, at & 1 != 0);
        if reversed && !turns[segment] {
            failed = true;
            break;
        }
        out.push((segment, reversed));
        at = ends[at ^ 1].edge_out;
    }
    if failed {
        out = closest_point(&mut ends, &tree, &turns, first);
    }
    if out.len() != n {
        return in_order;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(x: f64) -> ([f64; 2], [f64; 2]) {
        ([x, 0.0], [x, 10.0])
    }

    #[test]
    fn parallel_lines_zig_zag_from_the_start() {
        let pieces: Vec<_> = [0.0, 3.0, 1.0, 2.0].map(line).to_vec();
        let order = chain(&pieces, &[true; 4], [-1.0, 0.0]);
        assert_eq!(order, [(0, false), (2, true), (3, false), (1, true)]);
    }

    #[test]
    fn a_piece_that_may_not_turn_keeps_its_way() {
        let pieces: Vec<_> = [0.0, 1.0, 2.0].map(line).to_vec();
        let order = chain(&pieces, &[false; 3], [-1.0, 10.0]);
        assert_eq!(order.len(), 3);
        assert!(order.iter().all(|o| !o.1));
    }

    #[test]
    fn everything_comes_out_once_whatever_the_points() {
        let pieces: Vec<_> = (0..200)
            .map(|i| {
                let a = f64::from(i * 37 % 11);
                ([a, f64::from(i * 13 % 7)], [a, f64::from(i * 13 % 7)])
            })
            .collect();
        let mut order: Vec<usize> = chain(&pieces, &[true; 200], [0.0, 0.0]).iter().map(|o| o.0).collect();
        order.sort_unstable();
        assert_eq!(order, (0..200).collect::<Vec<_>>());
    }

    #[test]
    fn nothing_and_one_are_simple() {
        assert!(chain(&[], &[], [0.0, 0.0]).is_empty());
        assert_eq!(chain(&[line(0.0)], &[true], [0.0, 11.0]), [(0, true)]);
        assert_eq!(chain(&[line(0.0)], &[false], [0.0, 11.0]), [(0, false)]);
    }
}
