//! Where loops start: a port of OrcaSlicer's `SeamPlacer`, so seams land where Orca puts them.
//!
//! Every outer wall point of an object is a candidate. Each gets a visibility from rays cast
//! off a decimated copy of the mesh (from a fixed random seed, so the result is repeatable), an
//! overhang and an angle penalty; the best of each loop is picked, then seams of neighbouring
//! layers are strung together and fitted with a B-spline. All of it runs once per object before
//! the G-code is written. Arithmetic is `f32` where Orca's is, because which of two near-equal
//! points wins depends on it.
//!
//! Orca's exact ties (a cube's four corners score the same) fall to the first point of its
//! polygon, which comes from Clipper; here the polygons start at their rear-most point, which
//! is what that amounts to on such shapes.

use std::collections::HashMap;

use glam::{DVec2, DVec3, Vec2, Vec3};
use rayon::prelude::*;

use crate::geom::{self, Join, Shape};
use crate::slicing::Placed;
use crate::toolpath::{LayerPlan, Path, Role};

const VISIBILITY_SAMPLES: usize = 30000;
const DECIMATION_TARGET: usize = 16000;
const SQR_RAYS: usize = 5;
const SHARP_ANGLE: f32 = 55.0 * std::f32::consts::PI / 180.0;
const ANGLE_ALIGNED: f32 = 0.6;
const ANGLE_NEAREST: f32 = 1.0;
const ENFORCER_STEP: f32 = 0.2;
/// Orca's rounded figure, kept: the depth of inner seams depends on it.
#[allow(clippy::approx_constant)]
const ORCA_SQRT_2: f32 = 1.4142;
const SCORE_TOLERANCE: f32 = 0.3;
const TOLERABLE_DIST_FACTOR: f32 = 4.0;
const MIN_STRING: usize = 6;
const MM_PER_SEGMENT: f32 = 4.0;
/// Scaled-integer resolution of Orca's polygons, in mm.
const NM: f64 = 1e-6;
/// No fit larger than this many curve parameters: the solve is cubic in them.
const MAX_PARAMETERS: usize = 400;

/// The `seam_position` setting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Setup {
    Nearest,
    Aligned,
    AlignedBack,
    Rear,
    Random,
}

impl Setup {
    pub fn of(value: &str) -> Setup {
        match value {
            "nearest" => Setup::Nearest,
            "aligned_back" => Setup::AlignedBack,
            "back" => Setup::Rear,
            "random" => Setup::Random,
            _ => Setup::Aligned,
        }
    }

    fn casts_rays(self) -> bool {
        matches!(self, Setup::Aligned | Setup::Nearest | Setup::AlignedBack)
    }

    fn aligns(self) -> bool {
        matches!(self, Setup::Aligned | Setup::Rear | Setup::AlignedBack)
    }
}

// ---- std's random numbers, as libstdc++ draws them ---------------------------------------

/// `std::mt19937_64`.
struct Mt64 {
    state: Vec<u64>,
    at: usize,
}

impl Mt64 {
    const N: usize = 312;
    const M: usize = 156;

    fn new(seed: u64) -> Mt64 {
        let mut state = Vec::with_capacity(Self::N);
        let mut last = seed;
        state.push(last);
        for i in 1..Self::N {
            last = 6364136223846793005u64.wrapping_mul(last ^ (last >> 62)).wrapping_add(i as u64);
            state.push(last);
        }
        Mt64 { state, at: Self::N }
    }

    fn next(&mut self) -> u64 {
        const UPPER: u64 = 0xFFFF_FFFF_8000_0000;
        const LOWER: u64 = 0x7FFF_FFFF;
        if self.at >= Self::N {
            for i in 0..Self::N {
                let word = |k: usize| self.state.get(k % Self::N).copied().unwrap_or(0);
                let x = (word(i) & UPPER) | (word(i + 1) & LOWER);
                let mixed = word(i + Self::M) ^ (x >> 1) ^ if x & 1 == 1 { 0xB502_6F5A_A966_19E9 } else { 0 };
                if let Some(slot) = self.state.get_mut(i) {
                    *slot = mixed;
                }
            }
            self.at = 0;
        }
        let mut y = self.state.get(self.at).copied().unwrap_or(0);
        self.at += 1;
        y ^= (y >> 29) & 0x5555_5555_5555_5555;
        y ^= (y << 17) & 0x71D6_7FFF_EDA6_0000;
        y ^= (y << 37) & 0xFFF7_EEE0_0000_0000;
        y ^ (y >> 43)
    }

    /// `std::uniform_real_distribution<double>` on [0, 1).
    fn unit(&mut self) -> f64 {
        let v = self.next() as f64 / 18446744073709551616.0;
        if v >= 1.0 { 1.0 - f64::EPSILON / 2.0 } else { v }
    }

    /// `std::uniform_int_distribution<size_t>{0, range - 1}` (Lemire's method).
    fn below(&mut self, range: u64) -> u64 {
        let mut wide = u128::from(self.next()) * u128::from(range);
        if (wide as u64) < range {
            let threshold = range.wrapping_neg() % range.max(1);
            while (wide as u64) < threshold {
                wide = u128::from(self.next()) * u128::from(range);
            }
        }
        (wide >> 64) as u64
    }
}

/// `std::shuffle`, which draws two positions at a time when the generator is wide enough.
fn shuffle(v: &mut [usize], g: &mut Mt64) {
    let n = v.len();
    if n < 2 {
        return;
    }
    let mut i = 1;
    if n.is_multiple_of(2) {
        v.swap(1, g.below(2) as usize);
        i = 2;
    }
    while i + 1 < n {
        let range = i as u64 + 1;
        let x = g.below(range * (range + 1));
        v.swap(i, (x / (range + 1)) as usize);
        v.swap(i + 1, (x % (range + 1)) as usize);
        i += 2;
    }
}

// ---- The mesh ----------------------------------------------------------------------------

/// A triangle set in single precision, as Orca keeps one.
#[derive(Clone, Default)]
struct Its {
    vertices: Vec<Vec3>,
    triangles: Vec<[u32; 3]>,
}

fn normalized(v: Vec3) -> Vec3 {
    let n = v.dot(v);
    if n > 0.0 { v / n.sqrt() } else { v }
}

impl Its {
    fn corners(&self, t: [u32; 3]) -> Option<[Vec3; 3]> {
        Some([
            *self.vertices.get(t[0] as usize)?,
            *self.vertices.get(t[1] as usize)?,
            *self.vertices.get(t[2] as usize)?,
        ])
    }

    fn face_normal(&self, t: [u32; 3]) -> Vec3 {
        self.corners(t).map_or(Vec3::ZERO, |[a, b, c]| normalized((b - a).cross(c - a)))
    }
}

/// Each triangle's neighbour across each of its edges, -1 for none.
fn face_neighbors(mesh: &Its) -> Vec<[i32; 3]> {
    let mut around: Vec<Vec<u32>> = vec![Vec::new(); mesh.vertices.len()];
    for (f, t) in mesh.triangles.iter().enumerate() {
        for v in t {
            if let Some(list) = around.get_mut(*v as usize) {
                list.push(f as u32);
            }
        }
    }
    let mut out = vec![[-1i32; 3]; mesh.triangles.len()];
    for (f, t) in mesh.triangles.iter().enumerate() {
        for edge in 0..3 {
            if out.get(f).is_none_or(|n| n[edge] != -1) {
                continue;
            }
            let (a, b) = (t[edge], t[(edge + 1) % 3]);
            for &other in around.get(a as usize).map_or(&[][..], |l| l.as_slice()) {
                if other as usize <= f {
                    continue;
                }
                let Some(o) = mesh.triangles.get(other as usize) else { continue };
                let Some(at) = o.iter().position(|v| *v == b) else { continue };
                if a != o[(at + 1) % 3] || out.get(other as usize).is_none_or(|n| n[at] != -1) {
                    continue;
                }
                if let Some(n) = out.get_mut(f) {
                    n[edge] = other as i32;
                }
                if let Some(n) = out.get_mut(other as usize) {
                    n[at] = f as i32;
                }
                break;
            }
        }
    }
    out
}

/// The lowest dot product of each vertex's (Nelson Max weighted) normal with its faces'.
fn min_vertex_dots(mesh: &Its) -> Vec<f32> {
    let n = mesh.vertices.len();
    let mut normals = vec![Vec3::ZERO; n];
    let mut counts = vec![0f32; n];
    let mut faces = Vec::with_capacity(mesh.triangles.len());
    for t in &mesh.triangles {
        let Some([a, b, c]) = mesh.corners(*t) else {
            faces.push(Vec3::ZERO);
            continue;
        };
        let normal = normalized((b - a).cross(c - a));
        faces.push(normal);
        let (e0, e1, e2) = ((a - b).length(), (b - c).length(), (c - a).length());
        for (v, weight) in t.iter().zip([e0 * e2, e0 * e1, e1 * e2]) {
            if let (Some(sum), Some(count)) = (normals.get_mut(*v as usize), counts.get_mut(*v as usize)) {
                *sum += normal * weight;
                *count += weight;
            }
        }
    }
    for (normal, count) in normals.iter_mut().zip(&counts) {
        *normal /= *count;
    }
    let mut dots = vec![1f32; n];
    for (t, face) in mesh.triangles.iter().zip(&faces) {
        for v in t {
            if let (Some(d), Some(normal)) = (dots.get_mut(*v as usize), normals.get(*v as usize)) {
                *d = d.min(face.dot(*normal));
            }
        }
    }
    dots
}

fn final_index(map: &mut [usize], from: usize) -> usize {
    let mut idx = from;
    let mut hops = 0;
    while let Some(&next) = map.get(idx) {
        if next == idx || hops > map.len() {
            break;
        }
        idx = next;
        hops += 1;
    }
    let mut at = from;
    while let Some(slot) = map.get_mut(at) {
        let next = *slot;
        if next == at || next == idx {
            break;
        }
        *slot = idx;
        at = next;
    }
    idx
}

/// `its_short_edge_collpase`: random-order edge collapses, short edges first.
fn decimate(mesh: &Its, target: usize) -> Its {
    fn unlink(neighbors: &mut [[i32; 3]], at: i32, gone: i32, with: i32) {
        // As in Orca: face 0 never has its links updated.
        if at > 0
            && let Some(n) = neighbors.get_mut(at as usize)
            && let Some(slot) = n.iter_mut().find(|v| **v == gone)
        {
            *slot = with;
        }
    }
    fn remove_face(neighbors: &mut [[i32; 3]], removed: &mut [bool], face: i32, other: i32) {
        if face < 0 {
            return;
        }
        if let Some(flag) = removed.get_mut(face as usize) {
            *flag = true;
        }
        let Some(&n) = neighbors.get(face as usize) else { return };
        let a = if n[0] != other { n[0] } else { n[1] };
        let b = if n[2] != other { n[2] } else { n[1] };
        unlink(neighbors, a, face, b);
        unlink(neighbors, b, face, a);
    }

    let mut map: Vec<usize> = (0..mesh.vertices.len()).collect();
    let mut removed = vec![false; mesh.triangles.len()];
    let mut neighbors = face_neighbors(mesh);
    let dots = min_vertex_dots(mesh);
    let mut generator = Mt64::new(27644437);
    let mut faces: Vec<usize> = (0..mesh.triangles.len()).collect();
    let mut ratio = 1.0f32;
    let mut edge_len = 0.2f32;
    // Doubling the edge length each pass reaches any model's size well inside this.
    for _ in 0..256 {
        if faces.len() <= target {
            break;
        }
        edge_len = (f64::from(edge_len) * (f64::from(1.0f32) + 1.0 - f64::from(ratio))) as f32;
        let max_sq = edge_len * edge_len;
        shuffle(&mut faces, &mut generator);
        let mut allowed = faces.len() as i32 - target as i32;
        for &face in &faces {
            if removed.get(face).copied().unwrap_or(true) {
                continue;
            }
            let Some(&t) = mesh.triangles.get(face) else { continue };
            for edge in 0..3 {
                let mut keep = final_index(&mut map, t[edge] as usize);
                let mut drop = final_index(&mut map, t[(edge + 1) % 3] as usize);
                let (Some(&a), Some(&b)) = (mesh.vertices.get(keep), mesh.vertices.get(drop)) else { continue };
                if (a - b).length_squared() > max_sq {
                    continue;
                }
                if dots.get(drop) < dots.get(keep) {
                    std::mem::swap(&mut keep, &mut drop);
                }
                let merged = map.get(keep).copied().unwrap_or(keep);
                if let Some(slot) = map.get_mut(drop) {
                    *slot = merged;
                }
                let other = neighbors.get(face).map_or(-1, |n| n[edge]);
                remove_face(&mut neighbors, &mut removed, face as i32, other);
                remove_face(&mut neighbors, &mut removed, other, face as i32);
                allowed -= 2;
                break;
            }
            if allowed <= 0 {
                break;
            }
        }
        let before = faces.len();
        faces.retain(|f| !removed.get(*f).copied().unwrap_or(true));
        ratio = (before - faces.len()) as f32 / before.max(1) as f32;
    }

    let mut renumber: HashMap<usize, u32> = HashMap::new();
    let mut out = Its::default();
    for face in faces {
        let Some(t) = mesh.triangles.get(face) else { continue };
        let f = [
            final_index(&mut map, t[0] as usize),
            final_index(&mut map, t[1] as usize),
            final_index(&mut map, t[2] as usize),
        ];
        if f[0] == f[1] || f[1] == f[2] || f[2] == f[0] {
            continue;
        }
        let mut tri = [0u32; 3];
        for (slot, v) in tri.iter_mut().zip(f) {
            *slot = *renumber.entry(v).or_insert_with(|| {
                out.vertices.push(mesh.vertices.get(v).copied().unwrap_or(Vec3::ZERO));
                out.vertices.len() as u32 - 1
            });
        }
        out.triangles.push(tri);
    }
    out
}

// ---- A bounding volume tree, for rays at triangles and distances to lines ----------------

#[derive(Clone, Copy)]
struct Bounds {
    lo: DVec3,
    hi: DVec3,
}

impl Bounds {
    fn of(points: &[DVec3]) -> Bounds {
        let mut b = Bounds { lo: DVec3::INFINITY, hi: DVec3::NEG_INFINITY };
        for p in points {
            b.lo = b.lo.min(*p);
            b.hi = b.hi.max(*p);
        }
        b
    }

    fn join(a: Bounds, b: Bounds) -> Bounds {
        Bounds { lo: a.lo.min(b.lo), hi: a.hi.max(b.hi) }
    }

    fn hit_by(&self, origin: DVec3, inverse: DVec3, best: f64) -> bool {
        let (a, b) = ((self.lo - origin) * inverse, (self.hi - origin) * inverse);
        // Written so a NaN (a ray in the plane of a face) does not count against the box.
        let near = a.min(b).max_element().max(0.0);
        let far = a.max(b).min_element().min(best);
        near <= far || near.is_nan() || far.is_nan()
    }

    fn distance_sq(&self, p: DVec2) -> f64 {
        let dx = (self.lo.x - p.x).max(p.x - self.hi.x).max(0.0);
        let dy = (self.lo.y - p.y).max(p.y - self.hi.y).max(0.0);
        dx * dx + dy * dy
    }
}

struct Node {
    bounds: Bounds,
    /// Leaf: first of `items`; branch: the right child (the left is the next node).
    at: u32,
    /// Leaf: how many items; branch: 0.
    count: u32,
}

#[derive(Default)]
struct Tree {
    nodes: Vec<Node>,
    items: Vec<u32>,
}

impl Tree {
    fn new(boxes: &[Bounds]) -> Tree {
        let mut tree = Tree { nodes: Vec::with_capacity(boxes.len()), items: (0..boxes.len() as u32).collect() };
        let mut items = std::mem::take(&mut tree.items);
        if !items.is_empty() {
            tree.split(&mut items, 0, boxes);
        }
        tree.items = items;
        tree
    }

    fn split(&mut self, items: &mut [u32], first: u32, boxes: &[Bounds]) {
        let empty = Bounds { lo: DVec3::INFINITY, hi: DVec3::NEG_INFINITY };
        let of = |i: &u32| boxes.get(*i as usize).copied().unwrap_or(empty);
        let bounds = items.iter().map(of).fold(empty, Bounds::join);
        let node = self.nodes.len();
        self.nodes.push(Node { bounds, at: first, count: items.len() as u32 });
        if items.len() <= 4 {
            return;
        }
        let size = bounds.hi - bounds.lo;
        let axis = if size.x >= size.y && size.x >= size.z {
            0
        } else if size.y >= size.z {
            1
        } else {
            2
        };
        let key = |i: &u32| {
            let b = of(i);
            (b.lo + b.hi)[axis]
        };
        let middle = items.len() / 2;
        items.select_nth_unstable_by(middle, |a, b| key(a).total_cmp(&key(b)));
        let (left, right) = items.split_at_mut(middle);
        self.split(left, first, boxes);
        let right_node = self.nodes.len() as u32;
        self.split(right, first + middle as u32, boxes);
        if let Some(n) = self.nodes.get_mut(node) {
            n.at = right_node;
            n.count = 0;
        }
    }

    fn leaf(&self, n: &Node) -> &[u32] {
        self.items.get(n.at as usize..(n.at + n.count) as usize).unwrap_or(&[])
    }
}

/// Where a ray meets triangle `[a, b, c]`, as a distance along it.
fn ray_triangle(origin: DVec3, dir: DVec3, [a, b, c]: [DVec3; 3]) -> Option<f64> {
    let (e1, e2) = (b - a, c - a);
    let p = dir.cross(e2);
    let det = e1.dot(p);
    if det.abs() < 1e-14 {
        return None;
    }
    let inv = 1.0 / det;
    let s = origin - a;
    let u = s.dot(p) * inv;
    if !(0.0..=1.0).contains(&u) {
        return None;
    }
    let q = s.cross(e1);
    let v = dir.dot(q) * inv;
    if v < 0.0 || u + v > 1.0 {
        return None;
    }
    let t = e2.dot(q) * inv;
    (t > 0.0).then_some(t)
}

/// The triangles rays stop at.
struct Occluder {
    corners: Vec<[DVec3; 3]>,
    normals: Vec<Vec3>,
    tree: Tree,
}

impl Occluder {
    fn new(mesh: &Its) -> Occluder {
        let corners: Vec<[DVec3; 3]> = mesh
            .triangles
            .iter()
            .map(|t| mesh.corners(*t).map_or([DVec3::ZERO; 3], |c| c.map(|v| v.as_dvec3())))
            .collect();
        let normals = mesh.triangles.iter().map(|t| mesh.face_normal(*t)).collect();
        let boxes: Vec<Bounds> = corners.iter().map(|c| Bounds::of(c)).collect();
        Occluder { tree: Tree::new(&boxes), corners, normals }
    }

    /// The triangle a ray first hits.
    fn first_hit(&self, origin: DVec3, dir: DVec3, stack: &mut Vec<u32>) -> Option<u32> {
        let inverse = DVec3::ONE / dir;
        let mut best = (f64::INFINITY, None);
        stack.clear();
        stack.push(0);
        while let Some(at) = stack.pop() {
            let Some(n) = self.tree.nodes.get(at as usize) else { continue };
            if !n.bounds.hit_by(origin, inverse, best.0) {
                continue;
            }
            if n.count == 0 {
                stack.push(n.at);
                stack.push(at + 1);
                continue;
            }
            for item in self.tree.leaf(n) {
                if let Some(t) = self.corners.get(*item as usize).and_then(|c| ray_triangle(origin, dir, *c))
                    && t < best.0
                {
                    best = (t, Some(*item));
                }
            }
        }
        best.1
    }
}

/// Lines of a layer's outline, for signed distances: negative inside.
#[derive(Default)]
struct Outline {
    lines: Vec<[DVec2; 2]>,
    tree: Tree,
}

impl Outline {
    fn new(shapes: &[&Shape], centre: DVec2) -> Outline {
        let mut lines = Vec::new();
        for contour in shapes.iter().flat_map(|s| s.iter()) {
            let n = contour.len();
            for i in 0..n {
                if let (Some(a), Some(b)) = (contour.get(i), contour.get((i + 1) % n)) {
                    lines.push([*a - centre, *b - centre]);
                }
            }
        }
        let boxes: Vec<Bounds> = lines.iter().map(|[a, b]| Bounds::of(&[a.extend(0.0), b.extend(0.0)])).collect();
        Outline { tree: Tree::new(&boxes), lines }
    }

    fn distance_sq(&self, p: DVec2) -> f64 {
        let mut best = f64::INFINITY;
        let mut stack = vec![0u32];
        while let Some(at) = stack.pop() {
            let Some(n) = self.tree.nodes.get(at as usize) else { continue };
            if n.bounds.distance_sq(p) >= best {
                continue;
            }
            if n.count == 0 {
                stack.push(n.at);
                stack.push(at + 1);
                continue;
            }
            for item in self.tree.leaf(n) {
                if let Some([a, b]) = self.lines.get(*item as usize) {
                    best = best.min(foot(*a, *b, p).distance_squared(p));
                }
            }
        }
        best
    }

    /// Counts crossings of a line from `p` towards +X.
    fn inside(&self, p: DVec2) -> bool {
        let mut crossings = 0u32;
        let mut stack = vec![0u32];
        while let Some(at) = stack.pop() {
            let Some(n) = self.tree.nodes.get(at as usize) else { continue };
            if p.y < n.bounds.lo.y || p.y > n.bounds.hi.y || p.x > n.bounds.hi.x {
                continue;
            }
            if n.count == 0 {
                stack.push(n.at);
                stack.push(at + 1);
                continue;
            }
            for item in self.tree.leaf(n) {
                if let Some([a, b]) = self.lines.get(*item as usize)
                    && (a.y > p.y) != (b.y > p.y)
                    && p.x < a.x + (p.y - a.y) / (b.y - a.y) * (b.x - a.x)
                {
                    crossings += 1;
                }
            }
        }
        crossings % 2 == 1
    }

    fn signed(&self, p: DVec2) -> f64 {
        if self.lines.is_empty() {
            return 1e6;
        }
        let d = self.distance_sq(p).sqrt();
        if self.inside(p) { -d } else { d }
    }
}

/// The nearest point to `p` on the line from `a` to `b`.
fn foot(a: DVec2, b: DVec2, p: DVec2) -> DVec2 {
    let d = b - a;
    let len = d.length_squared();
    if len <= 0.0 {
        return a;
    }
    a + d * ((p - a).dot(d) / len).clamp(0.0, 1.0)
}

// ---- Visibility --------------------------------------------------------------------------

/// A 3D hash of points, for "everything within this radius".
struct Grid3 {
    cell: f32,
    cells: HashMap<(i32, i32, i32), Vec<u32>>,
}

impl Grid3 {
    fn new(points: &[Vec3], cell: f32) -> Grid3 {
        let mut grid = Grid3 { cell: cell.max(1e-3), cells: HashMap::new() };
        for (i, p) in points.iter().enumerate() {
            let key = grid.key(*p);
            grid.cells.entry(key).or_default().push(i as u32);
        }
        grid
    }

    fn key(&self, p: Vec3) -> (i32, i32, i32) {
        let k = p / self.cell;
        (k.x.floor() as i32, k.y.floor() as i32, k.z.floor() as i32)
    }

    /// Indices near `p`, in the cell's reach, sorted so the answer does not depend on the hash.
    fn near(&self, p: Vec3) -> Vec<u32> {
        let (x, y, z) = self.key(p);
        let mut out = Vec::new();
        for dx in -1..=1 {
            for dy in -1..=1 {
                for dz in -1..=1 {
                    if let Some(list) = self.cells.get(&(x + dx, y + dy, z + dz)) {
                        out.extend_from_slice(list);
                    }
                }
            }
        }
        out.sort_unstable();
        out
    }
}

/// How visible a model's surface is, from rays cast at sample points on it.
pub struct Visibility {
    positions: Vec<Vec3>,
    normals: Vec<Vec3>,
    visible: Vec<f32>,
    radius: f32,
    grid: Grid3,
}

/// `sample_its_uniform_parallel`: points spread over the surface by area, from a fixed seed.
fn sample_surface(mesh: &Its) -> (Vec<Vec3>, Vec<Vec3>, f32) {
    let mut keys: Vec<(f64, usize)> = Vec::with_capacity(mesh.triangles.len());
    let mut area_sum = 0f32;
    for (i, t) in mesh.triangles.iter().enumerate() {
        let area = mesh.corners(*t).map_or(0.0, |[a, b, c]| 0.5 * f64::from((b - a).cross(c - a).length()));
        area_sum = (f64::from(area_sum) + area) as f32;
        let key = f64::from(area_sum);
        match keys.last_mut() {
            Some(last) if last.0 == key => last.1 = i,
            _ => keys.push((key, i)),
        }
    }
    let mut random = Mt64::new(27644437);
    let draws: Vec<[f64; 3]> = (0..VISIBILITY_SAMPLES).map(|_| [random.unit(), random.unit(), random.unit()]).collect();
    let mut positions = Vec::with_capacity(draws.len());
    let mut normals = Vec::with_capacity(draws.len());
    for [x, y, z] in draws {
        let t_sample = x * f64::from(area_sum);
        let at = keys.partition_point(|k| k.0 <= t_sample);
        let Some(&(_, face)) = keys.get(at).or(keys.last()) else { break };
        let Some([a, b, c]) = mesh.triangles.get(face).and_then(|t| mesh.corners(*t)) else { break };
        let sq_u = y.sqrt();
        positions.push(a * (1.0 - sq_u) as f32 + b * (sq_u * (1.0 - z)) as f32 + c * (z * sq_u) as f32);
        normals.push(normalized((b - a).cross(c - b)));
    }
    (positions, normals, area_sum)
}

/// The rays over a surface point's hemisphere, with +Z out of the surface.
fn hemisphere() -> Vec<Vec3> {
    let step = 1.0f32 / SQR_RAYS as f32;
    let mut out = Vec::with_capacity(SQR_RAYS * SQR_RAYS);
    for x in 0..SQR_RAYS {
        let sx = (f64::from(x as f32 * step) + f64::from(step) / 2.0) as f32;
        for y in 0..SQR_RAYS {
            let sy = (f64::from(y as f32 * step) + f64::from(step) / 2.0) as f32;
            let term1 = 2.0 * std::f32::consts::PI * sx;
            let term2 = 2.0 * (sy - sy * sy).sqrt();
            out.push(Vec3::new(term1.cos() * term2, term1.sin() * term2, (1.0 - 2.0 * sy).abs()));
        }
    }
    out
}

impl Visibility {
    fn new(mesh: &Its, setup: Setup) -> Visibility {
        let simple = decimate(mesh, DECIMATION_TARGET);
        let (positions, normals, area) = sample_surface(&simple);
        let density = VISIBILITY_SAMPLES as f32 / area.max(1e-6);
        let search_area = 4.0 / (-(0.9f32).ln() * density);
        let radius = (f64::from(search_area) / std::f64::consts::PI).sqrt() as f32;
        let occluder = Occluder::new(&simple);
        let rays = hemisphere();
        let decrease = 1.0f32 / (SQR_RAYS * SQR_RAYS) as f32;
        let samples: Vec<(Vec3, Vec3)> = positions.iter().copied().zip(normals.iter().copied()).collect();
        let visible: Vec<f32> = samples
            .par_iter()
            .map_init(Vec::new, |stack, (centre, normal)| {
                let mut value = 1.0f32;
                if setup == Setup::AlignedBack {
                    value += ((normal.dot(Vec3::new(0.0, -1.0, 0.0)) + 1.2) * 0.5).clamp(0.0, 1.0);
                }
                let z = normalized(*normal);
                let tangent_seed = if z.x.abs() > 0.99 { Vec3::Y } else { Vec3::X };
                let y = normalized(z.cross(tangent_seed));
                let x = y.cross(z);
                let origin = (*centre + *normal * 0.01).as_dvec3();
                for ray in &rays {
                    let dir = x * ray.x + y * ray.y + z * ray.z;
                    if let Some(hit) = occluder.first_hit(origin, dir.as_dvec3(), stack)
                        && occluder.normals.get(hit as usize).is_some_and(|n| n.dot(dir) <= 0.0)
                    {
                        value -= decrease;
                    }
                }
                value
            })
            .collect();
        let grid = Grid3::new(&positions, radius);
        Visibility { positions, normals, visible, radius, grid }
    }

    /// `calculate_point_visibility`: samples nearby, weighted by how close and how in-plane.
    fn at(&self, p: Vec3) -> f32 {
        let r = self.radius;
        let mut total = 0f32;
        let mut weights = 0f32;
        for i in self.grid.near(p) {
            let i = i as usize;
            let (Some(sample), Some(normal), Some(visible)) =
                (self.positions.get(i), self.normals.get(i), self.visible.get(i))
            else {
                continue;
            };
            if (p - *sample).length_squared() >= r * r {
                continue;
            }
            let to_plane = (p - *sample).dot(*normal).abs();
            let weight = (r - to_plane) + (r - (p - *sample).length());
            total += weight * *visible;
            weights += weight;
        }
        if weights == 0.0 { 1.0 } else { total / weights }
    }
}

/// A model's surface visibility, shared by the instances of an object.
pub fn visibility(parts: &[&Placed], value: &str) -> Option<Visibility> {
    let setup = Setup::of(value);
    setup.casts_rays().then(|| Visibility::new(&centred(parts, centre_of(parts), bottom_of(parts)), setup))
}

fn centre_of(parts: &[&Placed]) -> DVec2 {
    let (lo, hi) = parts
        .iter()
        .flat_map(|p| p.vertices.iter())
        .fold((DVec2::splat(f64::INFINITY), DVec2::splat(f64::NEG_INFINITY)), |(lo, hi), v| {
            (lo.min(v.truncate()), hi.max(v.truncate()))
        });
    if lo.is_finite() && hi.is_finite() { (lo + hi) / 2.0 } else { DVec2::ZERO }
}

fn bottom_of(parts: &[&Placed]) -> f64 {
    let z = parts.iter().flat_map(|p| p.vertices.iter()).fold(f64::INFINITY, |z, v| z.min(v.z));
    if z.is_finite() { z } else { 0.0 }
}

/// The parts as one triangle set around the object's centre, standing at height 0.
fn centred(parts: &[&Placed], centre: DVec2, bottom: f64) -> Its {
    let mut out = Its::default();
    for part in parts {
        let base = out.vertices.len() as u32;
        let shift = centre.extend(bottom);
        out.vertices.extend(part.vertices.iter().map(|v| (*v - shift).as_vec3()));
        out.triangles.extend(part.triangles.iter().map(|t| t.map(|v| v + base)));
    }
    out
}

// ---- Candidates --------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
enum Kind {
    Blocked,
    Neutral,
    Enforced,
}

struct Perimeter {
    start: usize,
    end: usize,
    seam: usize,
    finalized: bool,
    final_position: Vec3,
}

struct Candidate {
    position: Vec3,
    perimeter: u32,
    width: f32,
    visibility: f32,
    overhang: f32,
    embedded: f32,
    angle: f32,
    kind: Kind,
    central: bool,
}

#[derive(Default)]
struct Layer {
    points: Vec<Candidate>,
    perimeters: Vec<Perimeter>,
    cells: HashMap<(i32, i32), Vec<u32>>,
}

const CELL: f32 = 2.0;

impl Layer {
    fn index(&mut self) {
        for (i, p) in self.points.iter().enumerate() {
            let key = ((p.position.x / CELL).floor() as i32, (p.position.y / CELL).floor() as i32);
            self.cells.entry(key).or_default().push(i as u32);
        }
    }

    fn near(&self, p: Vec3, radius: f32) -> Vec<u32> {
        let reach = (radius / CELL).ceil().min(64.0) as i32;
        let (cx, cy) = ((p.x / CELL).floor() as i32, (p.y / CELL).floor() as i32);
        let mut out = Vec::new();
        for x in cx - reach..=cx + reach {
            for y in cy - reach..=cy + reach {
                for i in self.cells.get(&(x, y)).map_or(&[][..], |l| l.as_slice()) {
                    if self.points.get(*i as usize).is_some_and(|c| (c.position - p).length_squared() < radius * radius)
                    {
                        out.push(*i);
                    }
                }
            }
        }
        out.sort_unstable();
        out
    }

    fn closest(&self, p: Vec3) -> Option<usize> {
        let (cx, cy) = ((p.x / CELL).floor() as i32, (p.y / CELL).floor() as i32);
        let mut best: Option<(f32, usize)> = None;
        // Rings of cells outwards; the point list itself when it is far off the layer.
        for ring in 0..16 {
            for x in cx - ring..=cx + ring {
                for y in cy - ring..=cy + ring {
                    if (x - cx).abs().max((y - cy).abs()) != ring {
                        continue;
                    }
                    for i in self.cells.get(&(x, y)).map_or(&[][..], |l| l.as_slice()) {
                        let Some(c) = self.points.get(*i as usize) else { continue };
                        let d = (c.position - p).length_squared();
                        if best.is_none_or(|b| d < b.0) {
                            best = Some((d, *i as usize));
                        }
                    }
                }
            }
            if best.is_some_and(|b| b.0.sqrt() <= ring as f32 * CELL) {
                return best.map(|b| b.1);
            }
        }
        self.points
            .iter()
            .enumerate()
            .map(|(i, c)| ((c.position - p).length_squared(), i))
            .min_by(|a, b| a.0.total_cmp(&b.0))
            .map(|b| b.1)
    }

    fn perimeter_of(&self, point: usize) -> Option<&Perimeter> {
        self.perimeters.get(self.points.get(point)?.perimeter as usize)
    }
}

/// `calculate_polygon_angles_at_vertices`: each vertex's turn over arms at least `arm` long.
fn polygon_angles(polygon: &[DVec2], lengths: &[f32], arm: f32) -> Vec<f32> {
    let n = polygon.len();
    let mut out = vec![0f32; n];
    if n < 2 {
        return out;
    }
    let length = |i: usize| lengths.get(i).copied().unwrap_or(0.0);
    let total: f32 = lengths.iter().sum();
    if total.is_nan() || total <= arm {
        return out;
    }
    let (mut prev, mut next) = (0usize, 0usize);
    let (mut to_prev, mut to_next) = (0f32, 0f32);
    while to_prev < arm {
        prev = (prev + n - 1) % n;
        to_prev += length(prev);
    }
    for (current, slot) in out.iter_mut().enumerate() {
        while to_prev - length(prev) > arm {
            to_prev -= length(prev);
            prev = (prev + 1) % n;
        }
        // Rounding can leave the sum short of the arm where the loop is barely longer.
        let mut steps = 0;
        while to_next < arm && steps <= n {
            to_next += length(next);
            next = (next + 1) % n;
            steps += 1;
        }
        if let (Some(a), Some(b), Some(c)) = (polygon.get(prev), polygon.get(current), polygon.get(next)) {
            let (u, v) = (*b - *a, *c - *b);
            *slot = u.perp_dot(v).atan2(u.dot(v)) as f32;
        }
        to_prev += length(current);
        to_next -= length(current);
    }
    out
}

/// One outer wall loop to make candidates from. Its points have Orca's resolution; the painted
/// regions are grown by the line width, as Orca looks that far for painted facets.
struct Loop<'a> {
    points: Vec<DVec2>,
    width: f32,
    enforce: &'a [Shape],
    block: &'a [Shape],
}

/// `process_perimeter_polygon`.
fn add_perimeter(layer: &mut Layer, source: &Loop<'_>, z: f32, arm: f32, centre: DVec2) {
    let mut polygon = source.points.clone();
    if polygon.len() < 3 {
        return;
    }
    // Tie-break: the polygon starts at its rear-most point, right-most of those.
    let first = polygon
        .iter()
        .enumerate()
        .max_by(|a, b| (a.1.y, a.1.x).partial_cmp(&(b.1.y, b.1.x)).unwrap_or(std::cmp::Ordering::Equal))
        .map_or(0, |(i, _)| i);
    polygon.rotate_left(first);
    if let Some(p) = polygon.first().copied() {
        // A loop's polygon closes on its first point.
        polygon.push(p);
    }
    let was_clockwise = geom::signed_area2(&polygon) < 0.0;
    if was_clockwise {
        polygon.reverse();
    }
    let n = polygon.len();
    let mut lengths: Vec<f32> = polygon.windows(2).map(|w| w[0].distance(w[1]) as f32).collect();
    let closing = polygon.first().zip(polygon.last()).map_or(0.0, |(a, b)| a.distance(*b));
    lengths.push(closing.max(0.1) as f32);
    let angles = polygon_angles(&polygon, &lengths, arm);

    let perimeter = layer.perimeters.len() as u32;
    let start = layer.points.len();
    let mut some_enforced = false;
    let painted = !source.enforce.is_empty() || !source.block.is_empty();
    let mut push = |layer: &mut Layer, position: Vec3, angle: f32| {
        let at = position.truncate().as_dvec2() + centre;
        let mut kind = Kind::Neutral;
        if painted {
            if geom::contains(source.enforce, at) {
                kind = Kind::Enforced;
            }
            if geom::contains(source.block, at) {
                kind = Kind::Blocked;
            }
        }
        some_enforced |= kind == Kind::Enforced;
        layer.points.push(Candidate {
            position,
            perimeter,
            width: source.width,
            visibility: 0.0,
            overhang: 0.0,
            embedded: 0.0,
            angle,
            kind,
            central: false,
        });
    };
    let points: Vec<Vec3> = polygon.iter().map(|p| p.as_vec2().extend(z)).collect();
    for i in 0..n {
        let Some(&position) = points.get(i) else { continue };
        let angle = angles.get(i).copied().unwrap_or(0.0);
        push(layer, position, if was_clockwise { -angle } else { angle });
        if source.enforce.is_empty() {
            continue;
        }
        let next = points.get(i + 1).or(points.first()).copied().unwrap_or(position);
        let distance = (position - next).length();
        let to_next = normalized(next - position);
        let at = |d: f32| position + to_next * d;
        let mut step = ENFORCER_STEP;
        let reaches = (0..=(distance / ENFORCER_STEP).min(4096.0) as usize)
            .any(|k| geom::contains(source.enforce, at(k as f32 * ENFORCER_STEP).truncate().as_dvec2() + centre));
        if reaches {
            while step < distance {
                push(layer, at(step), 0.0);
                step += ENFORCER_STEP;
            }
        }
    }
    let end = layer.points.len();
    layer.perimeters.push(Perimeter { start, end, seam: start, finalized: false, final_position: Vec3::ZERO });
    if some_enforced {
        mark_central_enforcer(layer, start, end);
    }
}

fn grow(shapes: &[Shape], by: f64) -> Vec<Shape> {
    if shapes.is_empty() { Vec::new() } else { geom::offset(shapes, by, Join::Round) }
}

/// Picks the middle of the longest enforced patch, or a sharp corner in it.
fn mark_central_enforcer(layer: &mut Layer, start: usize, end: usize) {
    let size = end - start;
    if size == 0 {
        return;
    }
    let next = |i: usize| start + (i - start + 1) % size;
    let enforced = |layer: &Layer, i: usize| layer.points.get(i).is_some_and(|p| p.kind == Kind::Enforced);
    let mut marks: Vec<usize> = Vec::new();
    for i in start..end {
        if enforced(layer, i) != enforced(layer, next(i)) {
            marks.push(next(i));
        }
    }
    let Some(&first) = marks.first() else { return };
    let start_on_second = !enforced(layer, first);
    if start_on_second {
        marks.push(first);
    }
    let patch_len = |(a, b): (usize, usize)| if b < a { a + (size - b) } else { b - a };
    let mut longest = (0usize, 0usize);
    let mut i = usize::from(start_on_second);
    while let (Some(&a), Some(&b)) = (marks.get(i), marks.get(i + 1)) {
        if patch_len(longest) < patch_len((a, b)) {
            longest = (a, b);
        }
        i += 2;
    }
    let mut viable = Vec::new();
    let mut sharp = Vec::new();
    let mut at = longest.0;
    for _ in 0..size {
        if at == longest.1 {
            break;
        }
        viable.push(at);
        if layer.points.get(at).is_some_and(|p| p.angle.abs() > SHARP_ANGLE) {
            sharp.push(at);
        }
        at = next(at);
    }
    let list = if sharp.is_empty() { &viable } else { &sharp };
    if let Some(p) = list.get(list.len() / 2).and_then(|i| layer.points.get_mut(*i)) {
        p.central = true;
    }
}

// ---- Scoring -----------------------------------------------------------------------------

fn gauss(value: f32, mean_x: f32, mean_value: f32, falloff: f32) -> f32 {
    let shifted = value - mean_x;
    let exponent = 1.0 / (falloff * shifted * shifted + 1.0);
    mean_value * (exponent.exp() - 1.0) / (1f32.exp() - 1.0)
}

fn angle_penalty(ccw: f32) -> f32 {
    gauss(ccw, 0.0, 1.0, 3.0) + 1.0 / (2.0 + (-ccw).exp())
}

#[derive(Clone, Copy)]
struct Comparator {
    setup: Setup,
    importance: f32,
}

impl Comparator {
    fn new(setup: Setup) -> Comparator {
        Comparator { setup, importance: if setup == Setup::Nearest { ANGLE_NEAREST } else { ANGLE_ALIGNED } }
    }

    fn enforcer_first(&self) -> bool {
        matches!(self.setup, Setup::Aligned | Setup::AlignedBack)
    }

    fn better(&self, a: &Candidate, b: &Candidate, preferred: Vec2) -> bool {
        if self.enforcer_first() && a.central != b.central {
            return a.central;
        }
        if a.kind != b.kind {
            return a.kind > b.kind;
        }
        if a.overhang > 0.0 || b.overhang > 0.0 {
            return a.overhang < b.overhang;
        }
        if a.embedded < -0.5 && b.embedded > -0.5 {
            return true;
        }
        if b.embedded < -0.5 && a.embedded > -0.5 {
            return false;
        }
        if self.setup == Setup::Rear && a.position.y != b.position.y {
            return a.position.y > b.position.y;
        }
        let distance = |c: &Candidate| {
            if self.setup == Setup::Nearest {
                1.0 - gauss((c.position.truncate() - preferred).length(), 0.0, 1.0, 0.005)
            } else {
                0.0
            }
        };
        let penalty =
            |c: &Candidate| c.overhang + c.visibility + self.importance * angle_penalty(c.angle) + distance(c);
        penalty(a) < penalty(b)
    }

    fn not_much_worse(&self, a: &Candidate, b: &Candidate) -> bool {
        if self.enforcer_first() && a.central != b.central {
            return a.central;
        }
        if a.kind == Kind::Enforced {
            return true;
        }
        if a.kind == Kind::Blocked {
            return false;
        }
        if a.kind != b.kind {
            return a.kind > b.kind;
        }
        if (a.overhang > 0.0 || b.overhang > 0.0) && (a.overhang - b.overhang).abs() > 0.1 * a.width {
            return a.overhang < b.overhang;
        }
        if a.embedded < -0.5 && b.embedded > -0.5 {
            return true;
        }
        if b.embedded < -0.5 && a.embedded > -0.5 {
            return false;
        }
        match self.setup {
            Setup::Random => return true,
            Setup::Rear => return a.position.y + SCORE_TOLERANCE * 5.0 > b.position.y,
            _ => {}
        }
        let penalty = |c: &Candidate| c.overhang + c.visibility + self.importance * angle_penalty(c.angle);
        let (pa, pb) = (penalty(a), penalty(b));
        pa <= pb || pa - pb < SCORE_TOLERANCE
    }

    fn similar(&self, a: &Candidate, b: &Candidate) -> bool {
        self.not_much_worse(a, b) && self.not_much_worse(b, a)
    }
}

fn pick_seam(layer: &mut Layer, perimeter: usize, cmp: Comparator) {
    let Some(p) = layer.perimeters.get(perimeter) else { return };
    let (start, end) = (p.start, p.end);
    let mut seam = start;
    for i in start..end {
        if let (Some(a), Some(b)) = (layer.points.get(i), layer.points.get(seam))
            && cmp.better(a, b, Vec2::ZERO)
        {
            seam = i;
        }
    }
    if let Some(p) = layer.perimeters.get_mut(perimeter) {
        p.seam = seam;
    }
}

fn pick_random(layer: &mut Layer, perimeter: usize) {
    let cmp = Comparator::new(Setup::Random);
    let Some(p) = layer.perimeters.get(perimeter) else { return };
    let (start, end) = (p.start, p.end);
    let Some(first) = layer.points.get(start) else { return };
    let mut example = start;
    let seed = first.position;
    let mut rand = (seed.dot(Vec3::new(12.9898, 78.233, 133.3333)).sin() * 43758.547).abs();
    rand -= rand.trunc();
    let edge = |i: usize| {
        let at = |k: usize| layer.points.get(k).map_or(Vec3::ZERO, |c| c.position);
        at(if i + 1 == end { start } else { i + 1 }) - at(i)
    };
    let mut viables: Vec<(usize, f32, Vec3)> = Vec::new();
    for i in start..end {
        let (Some(a), Some(b)) = (layer.points.get(i), layer.points.get(example)) else { continue };
        if cmp.similar(a, b) {
            let e = edge(i);
            viables.push((i, e.length(), e));
        } else if cmp.not_much_worse(b, a) {
            // Worse than the example: not a choice.
        } else {
            example = i;
            viables.clear();
            let e = edge(i);
            viables.push((i, e.length(), e));
        }
    }
    let total: f32 = viables.iter().map(|v| v.1).sum();
    let mut picked = total * rand;
    let mut at = 0;
    while let Some(v) = viables.get(at) {
        if picked - v.1 > 0.0 && at + 1 < viables.len() {
            picked -= v.1;
            at += 1;
        } else {
            break;
        }
    }
    let Some(&(index, _, e)) = viables.get(at) else { return };
    let position = layer.points.get(index).map_or(Vec3::ZERO, |c| c.position) + normalized(e) * picked;
    if let Some(p) = layer.perimeters.get_mut(perimeter) {
        p.seam = index;
        p.final_position = position;
        p.finalized = true;
    }
}

// ---- Least squares and the fitted curve ----------------------------------------------------

/// Eigen's `FullPivHouseholderQR`, for solving one tall system with two right-hand sides.
struct Qr {
    rows: usize,
    cols: usize,
    m: Vec<f32>,
    tau: Vec<f32>,
    row_swaps: Vec<usize>,
    col_swaps: Vec<usize>,
    rank: usize,
}

fn house(column: &mut [f32]) -> (f32, f32) {
    let Some((head, tail)) = column.split_first_mut() else { return (0.0, 0.0) };
    let tail_sq: f32 = tail.iter().map(|v| v * v).sum();
    let c0 = *head;
    if tail_sq <= f32::MIN_POSITIVE {
        tail.fill(0.0);
        return (0.0, c0);
    }
    let mut beta = (c0 * c0 + tail_sq).sqrt();
    if c0 >= 0.0 {
        beta = -beta;
    }
    let scale = c0 - beta;
    for v in tail.iter_mut() {
        *v /= scale;
    }
    ((beta - c0) / beta, beta)
}

impl Qr {
    fn at(&self, r: usize, c: usize) -> f32 {
        self.m.get(c * self.rows + r).copied().unwrap_or(0.0)
    }

    fn new(mut m: Vec<f32>, rows: usize, cols: usize) -> Qr {
        let size = rows.min(cols);
        let mut tau = vec![0f32; size];
        let mut row_swaps = vec![0usize; size];
        let mut col_swaps = vec![0usize; size];
        let mut pivots = vec![0f32; size];
        let mut nonzero = size;
        for k in 0..size {
            let (mut big, mut br, mut bc) = (-1f32, k, k);
            for c in k..cols {
                for r in k..rows {
                    let v = m.get(c * rows + r).copied().unwrap_or(0.0).abs();
                    if v > big {
                        (big, br, bc) = (v, r, c);
                    }
                }
            }
            if let Some(s) = row_swaps.get_mut(k) {
                *s = br;
            }
            if let Some(s) = col_swaps.get_mut(k) {
                *s = bc;
            }
            if big <= 0.0 {
                nonzero = k;
                for j in k..size {
                    if let Some(s) = row_swaps.get_mut(j) {
                        *s = j;
                    }
                    if let Some(s) = col_swaps.get_mut(j) {
                        *s = j;
                    }
                }
                break;
            }
            if br != k {
                for c in k..cols {
                    m.swap(c * rows + k, c * rows + br);
                }
            }
            if bc != k {
                for r in 0..rows {
                    m.swap(k * rows + r, bc * rows + r);
                }
            }
            let (t, beta) = match m.get_mut(k * rows + k..(k + 1) * rows) {
                Some(column) => house(column),
                None => (0.0, 0.0),
            };
            if let Some(s) = m.get_mut(k * rows + k) {
                *s = beta;
            }
            if let Some(s) = tau.get_mut(k) {
                *s = t;
            }
            if let Some(s) = pivots.get_mut(k) {
                *s = beta;
            }
            let essential: Vec<f32> = m.get(k * rows + k + 1..(k + 1) * rows).map_or(Vec::new(), <[f32]>::to_vec);
            for c in k + 1..cols {
                if let Some(column) = m.get_mut(c * rows + k..(c + 1) * rows) {
                    apply_house(column, &essential, t);
                }
            }
        }
        let biggest = pivots.iter().fold(0f32, |a, b| a.max(b.abs()));
        let threshold = biggest * f32::EPSILON * size as f32;
        let rank = pivots.iter().take(nonzero).filter(|p| p.abs() > threshold).count();
        Qr { rows, cols, m, tau, row_swaps, col_swaps, rank }
    }

    fn solve(&self, rhs: &[f32]) -> Vec<f32> {
        let mut c = rhs.to_vec();
        let mut out = vec![0f32; self.cols];
        for k in 0..self.rank {
            let swap = self.row_swaps.get(k).copied().unwrap_or(k);
            if swap != k && swap < c.len() && k < c.len() {
                c.swap(k, swap);
            }
            let essential: Vec<f32> =
                self.m.get(k * self.rows + k + 1..(k + 1) * self.rows).map_or(Vec::new(), <[f32]>::to_vec);
            if let Some(tail) = c.get_mut(k..) {
                apply_house(tail, &essential, self.tau.get(k).copied().unwrap_or(0.0));
            }
        }
        let mut x = vec![0f32; self.rank];
        for i in (0..self.rank).rev() {
            let mut v = c.get(i).copied().unwrap_or(0.0);
            for j in i + 1..self.rank {
                v -= self.at(i, j) * x.get(j).copied().unwrap_or(0.0);
            }
            let d = self.at(i, i);
            if let Some(slot) = x.get_mut(i) {
                *slot = if d != 0.0 { v / d } else { 0.0 };
            }
        }
        // The columns were swapped one after another, so walk the swaps back.
        out.iter_mut().zip(&x).for_each(|(o, v)| *o = *v);
        for k in (0..self.rank).rev() {
            let swap = self.col_swaps.get(k).copied().unwrap_or(k);
            if swap != k && swap < out.len() {
                out.swap(k, swap);
            }
        }
        out
    }
}

fn apply_house(column: &mut [f32], essential: &[f32], tau: f32) {
    let Some((head, tail)) = column.split_first_mut() else { return };
    let w = *head + tail.iter().zip(essential).map(|(a, b)| a * b).sum::<f32>();
    *head -= tau * w;
    for (a, b) in tail.iter_mut().zip(essential) {
        *a -= tau * w * b;
    }
}

fn bspline_kernel(x: f32) -> f32 {
    const A: [f32; 4] = [(1.0 / 6.0) as f32, (-3.0 / 6.0) as f32, (3.0 / 6.0) as f32, (-1.0 / 6.0) as f32];
    const B: [f32; 4] = [(4.0 / 6.0) as f32, 0.0, (-6.0 / 6.0) as f32, (3.0 / 6.0) as f32];
    let x = x.abs();
    if x >= 2.0 {
        return 0.0;
    }
    let cubic = |k: [f32; 4], x: f32| {
        let (x2, x3) = (x * x, x * x * x);
        k[0] + k[1] * x + k[2] * x2 + k[3] * x3
    };
    if x <= 1.0 { cubic(B, x) } else { cubic(A, x - 1.0) }
}

/// A cubic B-spline through weighted points, by height.
struct Spline {
    coefficients: Vec<Vec2>,
    start: f32,
    segment: f32,
}

impl Spline {
    fn fit(observations: &[Vec2], at: &[f32], weights: &[f32], segments: usize) -> Option<Spline> {
        let (&start, &end) = (at.first()?, at.last()?);
        let segment = (end - start) / segments as f32;
        if !segment.is_finite() || segment <= 0.0 {
            return None;
        }
        let parameters = segments + 1;
        if parameters > MAX_PARAMETERS {
            return None;
        }
        let rows = observations.len();
        let root: Vec<f32> = weights.iter().map(|w| w.sqrt()).collect();
        let mut t = vec![0f32; rows * parameters];
        for (i, point) in at.iter().enumerate() {
            let middle = ((*point - start) / segment).floor() as i32;
            let first = middle - 2 + 1;
            for s in first..first + 4 {
                let distance = (start + s as f32 * segment - *point) / segment;
                let p = s.clamp(0, parameters as i32 - 1) as usize;
                if let Some(cell) = t.get_mut(p * rows + i) {
                    *cell += bspline_kernel(distance) * root.get(i).copied().unwrap_or(0.0);
                }
            }
        }
        let qr = Qr::new(t, rows, parameters);
        let side =
            |f: fn(&Vec2) -> f32| -> Vec<f32> { observations.iter().zip(&root).map(|(o, w)| f(o) * *w).collect() };
        let (x, y) = (qr.solve(&side(|v| v.x)), qr.solve(&side(|v| v.y)));
        Some(Spline { coefficients: x.iter().zip(&y).map(|(a, b)| Vec2::new(*a, *b)).collect(), start, segment })
    }

    fn at(&self, point: f32) -> Vec2 {
        let last = self.coefficients.len() as i32 - 1;
        let middle = ((point - self.start) / self.segment).floor() as i32;
        let first = middle - 2 + 1;
        let mut out = Vec2::ZERO;
        for s in first..first + 4 {
            let distance = (self.start + s as f32 * self.segment - point) / self.segment;
            let p = s.min(last).max(0) as usize;
            out += self.coefficients.get(p).copied().unwrap_or(Vec2::ZERO) * bspline_kernel(distance);
        }
        out
    }
}

// ---- Strings of seams ----------------------------------------------------------------------

type Seam = (usize, usize);

/// `std::stable_sort` by a comparator that may not be a total order (`sort_by` can panic).
fn merge_sort(items: &mut Vec<Seam>, before: &dyn Fn(&Seam, &Seam) -> bool) {
    let n = items.len();
    if n < 2 {
        return;
    }
    let mut right = items.split_off(n / 2);
    merge_sort(items, before);
    merge_sort(&mut right, before);
    let left = std::mem::replace(items, Vec::with_capacity(n));
    let (mut a, mut b) = (left.into_iter().peekable(), right.into_iter().peekable());
    loop {
        let take_right = match (a.peek(), b.peek()) {
            (Some(x), Some(y)) => before(y, x),
            (None, Some(_)) => true,
            (Some(_), None) => false,
            (None, None) => break,
        };
        items.extend(if take_right { b.next() } else { a.next() });
    }
}

/// All the seams of one object.
pub struct ObjectSeams {
    setup: Setup,
    centre: DVec2,
    layers: Vec<Layer>,
    /// Plan index to this object's layer.
    layer_of: HashMap<usize, usize>,
}

impl ObjectSeams {
    fn point(&self, (l, i): Seam) -> Option<&Candidate> {
        self.layers.get(l)?.points.get(i)
    }

    fn perimeter(&self, (l, i): Seam) -> Option<&Perimeter> {
        self.layers.get(l)?.perimeter_of(i)
    }

    fn next_seam(&self, projected: Vec3, layer_index: usize, max_distance: f32, cmp: Comparator) -> Option<Seam> {
        let layer = self.layers.get(layer_index)?;
        let near = layer.near(projected, max_distance);
        let &start = near.first()?;
        let (mut best, mut nearest) = (start as usize, start as usize);
        let finalized = |i: usize| layer.perimeter_of(i).is_none_or(|p| p.finalized);
        let pos = |i: usize| layer.points.get(i).map_or(Vec3::ZERO, |c| c.position);
        for &i in &near {
            let i = i as usize;
            if finalized(i) {
                continue;
            }
            let (Some(p), Some(b)) = (layer.points.get(i), layer.points.get(best)) else { continue };
            if cmp.better(p, b, projected.truncate()) || finalized(best) {
                best = i;
            }
            if (pos(i) - projected).length_squared() < (pos(nearest) - projected).length_squared() || finalized(nearest)
            {
                nearest = i;
            }
        }
        if finalized(nearest) {
            return None;
        }
        let seam_index = layer.perimeter_of(nearest)?.seam;
        let seam = layer.points.get(seam_index)?;
        if seam.central && (seam.position - projected).length_squared() < (3.0 * max_distance).powi(2) {
            return Some((layer_index, seam_index));
        }
        if cmp.not_much_worse(layer.points.get(nearest)?, seam) {
            return Some((layer_index, nearest));
        }
        if cmp.not_much_worse(layer.points.get(best)?, seam) {
            return Some((layer_index, best));
        }
        None
    }

    fn seam_string(&self, start: Seam, heights: &[f32], cmp: Comparator) -> Vec<Seam> {
        let origin = start.0 as i64;
        let mut next = origin + 1;
        let mut step = 1;
        let mut previous = start;
        let mut string = vec![start];
        let max_distance = TOLERABLE_DIST_FACTOR * self.point(start).map_or(0.0, |c| c.width);
        while next >= 0 {
            if next >= self.layers.len() as i64 {
                step = -1;
                previous = start;
                next = origin - 1;
                if next < 0 {
                    break;
                }
            }
            let Some(position) = self.point(previous).map(|c| c.position) else { break };
            let z = heights.get(next as usize).copied().unwrap_or(position.z);
            let projected = position.truncate().extend(z);
            match self.next_seam(projected, next as usize, max_distance, cmp) {
                Some(found) => {
                    string.push(found);
                    previous = found;
                }
                None if step == 1 => {
                    step = -1;
                    previous = start;
                    next = origin - 1;
                    if next < 0 {
                        break;
                    }
                    continue;
                }
                None => break,
            }
            next += step;
        }
        string
    }

    fn align(&mut self, heights: &[f32], cmp: Comparator) {
        let mut seams: Vec<Seam> = Vec::new();
        for (l, layer) in self.layers.iter().enumerate() {
            seams.extend(layer.perimeters.iter().map(|p| (l, p.seam)));
        }
        {
            let this = &*self;
            merge_sort(&mut seams, &|a, b| match (this.point(*a), this.point(*b)) {
                (Some(x), Some(y)) => cmp.better(x, y, Vec2::ZERO),
                _ => false,
            });
        }
        let mut global = 0;
        // Each pass finalizes at least one perimeter or moves on, but a bad fit must not spin.
        let mut budget = seams.len().saturating_mul(2).saturating_add(16);
        while let Some(&seam) = seams.get(global) {
            global += 1;
            if budget == 0 {
                break;
            }
            budget -= 1;
            if self.perimeter(seam).is_none_or(|p| p.finalized) {
                continue;
            }
            let mut string = self.seam_string(seam, heights, cmp);
            let step = 1 + string.len() / 20;
            let mut alternative = 0;
            while alternative < string.len() {
                let Some(&at) = string.get(alternative) else { break };
                let seam_of = self.perimeter(at).map_or(at.1, |p| p.seam);
                let other = self.seam_string((at.0, seam_of), heights, cmp);
                if other.len() > string.len() {
                    string = other;
                }
                alternative += step;
            }
            if string.len() < MIN_STRING {
                continue;
            }
            string.sort_by_key(|s| s.0);
            global -= 1;
            self.fit_string(&string, cmp);
        }
    }

    fn fit_string(&mut self, string: &[Seam], cmp: Comparator) {
        let angle_3d = |a: Vec3, b: Vec3| normalized(a).dot(normalized(b)).acos().abs();
        let angle_weight = |angle: f32| 1.0 / (0.1 + angle_penalty(angle));
        let position =
            |this: &Self, i: usize| string.get(i).and_then(|s| this.point(*s)).map_or(Vec3::ZERO, |c| c.position);
        let mut observations = Vec::with_capacity(string.len());
        let mut at = Vec::with_capacity(string.len());
        let mut weights = Vec::with_capacity(string.len());
        let mut total = 0f32;
        let mut last = position(self, 0);
        for (index, seam) in string.iter().enumerate() {
            let Some(current) = self.point(*seam) else { continue };
            let mut layer_angle = 0.0;
            if index > 0 && index + 1 < string.len() {
                layer_angle = angle_3d(
                    current.position - position(self, index - 1),
                    position(self, index + 1) - current.position,
                );
            }
            observations.push(current.position.truncate());
            at.push(current.position.z);
            let mut weight = angle_weight(current.angle);
            let mut curling = if f64::from(layer_angle) > 2.0 * f64::from(current.angle.abs()) { -0.8 } else { 1.0 };
            if current.kind == Kind::Enforced {
                curling = 1.0;
                weight += 3.0;
            }
            weights.push(weight);
            total += curling * (last - current.position).length();
            last = current.position;
        }
        if cmp.setup == Setup::Rear {
            total *= 0.3;
        }
        let segments = ((total.max(0.0) / MM_PER_SEGMENT) as usize).max(1);
        let curve = Spline::fit(&observations, &at, &weights, segments);
        for seam in string {
            let Some(point) = self.point(*seam) else { continue };
            let mut t = (point.angle.abs() / SHARP_ANGLE).powf(3.0).min(1.0);
            if point.kind == Kind::Enforced {
                t = t.max(0.4);
            }
            let current = point.position;
            let fitted = curve.as_ref().map_or(current.truncate(), |c| c.at(current.z)).extend(current.z);
            let position = current * t + fitted * (1.0 - t);
            let perimeter = point.perimeter as usize;
            if let Some(p) = self.layers.get_mut(seam.0).and_then(|l| l.perimeters.get_mut(perimeter)) {
                p.seam = seam.1;
                p.final_position = position;
                p.finalized = true;
            }
        }
    }

    /// The seam chosen for `path` on plan layer `plan`: starts the loop there. False when this
    /// layer has no candidates, so the caller must pick some other way.
    pub fn place(&self, plan: usize, path: &mut Path, last_pos: DVec2, staggered: bool) -> bool {
        let Some(layer) = self.layer_of.get(&plan).and_then(|l| self.layers.get(*l)) else { return false };
        let n = path.points.len();
        let Some(z) = layer.points.first().map(|c| c.position.z) else { return false };
        let local = |p: DVec2| (p - self.centre).as_vec2().extend(z);
        // The loop's perimeter: walk along it until two points in a row agree (Arachne's
        // T junctions can land the first on the wrong one).
        let mut closest = 0usize;
        let mut known: Option<u32> = None;
        let mut at = 0usize;
        for _ in 0..n {
            let Some(found) = path.points.get(at).and_then(|p| layer.closest(local(*p))) else { return false };
            closest = found;
            let perimeter = layer.points.get(found).map(|c| c.perimeter);
            if known != perimeter {
                known = perimeter;
                at = (at + 1) % n;
            } else {
                break;
            }
        }
        let Some(perimeter) = layer.perimeter_of(closest) else { return false };
        let (seam_position, seam_index) = if perimeter.finalized {
            (perimeter.final_position, perimeter.seam)
        } else {
            let index = if self.setup == Setup::Nearest {
                nearest_seam(layer, perimeter, (last_pos - self.centre).as_vec2())
            } else {
                perimeter.seam
            };
            (layer.points.get(index).map_or(Vec3::ZERO, |c| c.position), index)
        };
        let to_world = |p: Vec3| ((p.truncate().as_dvec2() + self.centre) / NM).round() * NM;
        let mut seam_point = to_world(seam_position);
        let Some(candidate) = layer.points.get(seam_index) else { return false };
        if path.role == Role::InnerWall {
            let (mut segment, mut projected) = closest_on_loop(&path.points, seam_point);
            let mut depth = seam_point.distance(projected) as f32;
            let beta = (candidate.angle / 2.0).cos();
            let on_point = (seam_position - candidate.position).length_squared() < depth;
            if on_point && candidate.angle < -1e-4 {
                let neighbour = |i: usize| layer.points.get(i).map_or(Vec3::ZERO, |c| c.position);
                let previous = if seam_index == perimeter.start { perimeter.end - 1 } else { seam_index - 1 };
                let next = if seam_index + 1 == perimeter.end { perimeter.start } else { seam_index + 1 };
                let to_middle = (normalized(candidate.position - neighbour(previous)).truncate()
                    + normalized(candidate.position - neighbour(next)).truncate())
                    * 0.5;
                depth = ORCA_SQRT_2 * depth / beta;
                let final_position = candidate.position.truncate() + to_middle * depth;
                (segment, projected) = closest_on_loop(&path.points, to_world(final_position.extend(0.0)));
            } else {
                depth = depth * beta / ORCA_SQRT_2;
            }
            seam_point = projected;
            if staggered && path.length() > 0.0 {
                seam_point = stagger(&path.points, segment, projected, depth.max(path.width as f32));
            }
        }
        start_at(path, seam_point);
        true
    }
}

fn nearest_seam(layer: &Layer, perimeter: &Perimeter, preferred: Vec2) -> usize {
    let cmp = Comparator::new(Setup::Nearest);
    let mut seam = perimeter.start;
    for i in perimeter.start..perimeter.end {
        if let (Some(a), Some(b)) = (layer.points.get(i), layer.points.get(seam))
            && cmp.better(a, b, preferred)
        {
            seam = i;
        }
    }
    seam
}

/// The nearest point of a loop to `p`: the segment it is on and the point.
fn closest_on_loop(points: &[DVec2], p: DVec2) -> (usize, DVec2) {
    let n = points.len();
    let mut best = (f64::INFINITY, 0, p);
    for i in 0..n {
        if let (Some(a), Some(b)) = (points.get(i), points.get((i + 1) % n)) {
            let f = foot(*a, *b, p);
            let d = f.distance_squared(p);
            if d < best.0 {
                best = (d, i, f);
            }
        }
    }
    (best.1, best.2)
}

/// `depth` mm further along the loop from `from` on `segment`.
fn stagger(points: &[DVec2], segment: usize, from: DVec2, mut depth: f32) -> DVec2 {
    let n = points.len();
    let (mut index, mut at) = (segment, from);
    for _ in 0..n * 2 {
        if depth <= 0.0 {
            break;
        }
        index = (index + 1) % n;
        let Some(next) = points.get(index).copied() else { break };
        let distance = at.distance(next) as f32;
        if distance > depth {
            at += (next - at) * f64::from(depth / distance);
        } else {
            at = next;
        }
        depth -= distance;
    }
    at
}

/// Starts a loop at `p`: on the vertex there, or on a new one on the nearest segment.
fn start_at(path: &mut Path, p: DVec2) {
    const SAME_VERTEX: f64 = 0.0015;
    if let Some(i) = path.points.iter().position(|v| v.distance(p) <= SAME_VERTEX) {
        path.rotate(i);
        return;
    }
    let (segment, on) = closest_on_loop(&path.points, p);
    let n = path.points.len();
    let (a, b) = (path.points.get(segment).copied(), path.points.get((segment + 1) % n).copied());
    if let (Some(a), Some(b)) = (a, b) {
        const SNAP: f64 = 0.001;
        if a.distance(p) < b.distance(p) && a.distance(p) < SNAP {
            path.rotate(segment);
            return;
        }
        if b.distance(p) < SNAP {
            path.rotate(segment + 1);
            return;
        }
    }
    path.points.insert(segment + 1, on);
    if path.overhang.len() == n {
        let mark = path.overhang.get(segment).copied().unwrap_or(0);
        path.overhang.insert(segment + 1, mark);
    }
    path.rotate(segment + 1);
}

/// Every seam of object `object` on the plans.
pub fn build(
    plans: &[LayerPlan],
    object: usize,
    parts: &[&Placed],
    visibility: Option<&Visibility>,
    value: &str,
    nozzle: f64,
) -> ObjectSeams {
    let setup = Setup::of(value);
    let centre = centre_of(parts);
    let on_layers: Vec<(usize, &LayerPlan)> =
        plans.iter().enumerate().filter(|(_, p)| p.objects.iter().any(|(o, _)| *o == object)).collect();
    let base = on_layers.first().map_or(0.0, |(_, p)| p.print_z - p.height);
    let heights: Vec<f32> = on_layers.iter().map(|(_, p)| (p.print_z - p.height / 2.0 - base) as f32).collect();
    let mut seams = ObjectSeams {
        setup,
        centre,
        layers: Vec::with_capacity(on_layers.len()),
        layer_of: on_layers.iter().enumerate().map(|(l, (p, _))| (*p, l)).collect(),
    };
    let arm = nozzle as f32;
    let mut outlines: Vec<Outline> = Vec::with_capacity(on_layers.len());
    let mut regions: Vec<usize> = Vec::with_capacity(on_layers.len());
    for ((_, plan), &z) in on_layers.iter().zip(&heights) {
        let islands: Vec<&crate::toolpath::Island> =
            plan.objects.iter().filter(|(o, _)| *o == object).flat_map(|(_, list)| list).collect();
        let mut layer = Layer::default();
        for island in &islands {
            let outer = island.walls.first().map_or(&[][..], |w| w.as_slice());
            let width = outer.first().map_or(0.0, |p| p.width);
            let (enforce, block) = (grow(&island.seam_enforce, width), grow(&island.seam_block, width));
            for path in outer.iter().filter(|p| p.closed) {
                let source = Loop {
                    points: path.points.iter().map(|p| ((*p - centre) / NM).round() * NM).collect(),
                    width: path.width as f32,
                    enforce: &enforce,
                    block: &block,
                };
                add_perimeter(&mut layer, &source, z, arm, centre);
            }
        }
        layer.index();
        seams.layers.push(layer);
        let shapes: Vec<&Shape> = islands.iter().map(|i| &i.outline).collect();
        outlines.push(Outline::new(&shapes, centre));
        let mut used: Vec<Option<usize>> = islands.iter().map(|i| i.extruder).collect();
        used.sort_unstable();
        used.dedup();
        regions.push(used.len());
    }
    if let Some(v) = visibility {
        seams.layers.par_iter_mut().for_each(|layer| {
            layer.points.iter_mut().for_each(|c| c.visibility = v.at(c.position));
        });
    }
    let heights_of: Vec<f64> = on_layers.iter().map(|(_, p)| p.height).collect();
    let empty = Outline::default();
    seams.layers.par_iter_mut().enumerate().for_each(|(i, layer)| {
        let below = i.checked_sub(1).and_then(|k| outlines.get(k));
        let here = outlines.get(i).unwrap_or(&empty);
        let height = heights_of.get(i).copied().unwrap_or(0.0);
        let several = regions.get(i).copied().unwrap_or(0) > 1;
        let tan45 = f64::from(std::f32::consts::FRAC_PI_4.tan());
        for c in &mut layer.points {
            let p = c.position.truncate().as_dvec2();
            if let Some(below) = below {
                let d = below.signed(p) + f64::from(0.65 * c.width) - tan45 * height;
                c.overhang = (d as f32).max(0.0);
            }
            if several {
                c.embedded = (here.signed(p) + f64::from(0.65 * c.width)) as f32;
            }
        }
    });
    if setup != Setup::Nearest {
        let cmp = Comparator::new(setup);
        seams.layers.par_iter_mut().for_each(|layer| {
            for perimeter in 0..layer.perimeters.len() {
                if setup == Setup::Random {
                    pick_random(layer, perimeter);
                } else {
                    pick_seam(layer, perimeter, cmp);
                }
            }
        });
    }
    if setup.aligns() {
        seams.align(&heights, Comparator::new(setup));
    }
    seams
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_generator_is_mt19937_64() {
        let mut g = Mt64::new(5489);
        let mut v = 0;
        for _ in 0..10000 {
            v = g.next();
        }
        // The C++ standard gives this as the 10000th value.
        assert_eq!(v, 9981545732273789042);
    }

    fn cube() -> Its {
        let c = |x: f32, y: f32, z: f32| Vec3::new(x, y, z);
        Its {
            vertices: vec![
                c(0., 0., 0.),
                c(20., 0., 0.),
                c(20., 20., 0.),
                c(0., 20., 0.),
                c(0., 0., 20.),
                c(20., 0., 20.),
                c(20., 20., 20.),
                c(0., 20., 20.),
            ],
            triangles: vec![
                [0, 2, 1],
                [0, 3, 2],
                [4, 5, 6],
                [4, 6, 7],
                [0, 1, 5],
                [0, 5, 4],
                [1, 2, 6],
                [1, 6, 5],
                [2, 3, 7],
                [2, 7, 6],
                [3, 0, 4],
                [3, 4, 7],
            ],
        }
    }

    #[test]
    fn nothing_hides_the_outside_of_a_cube() {
        let v = Visibility::new(&cube(), Setup::Aligned);
        assert_eq!(v.positions.len(), VISIBILITY_SAMPLES);
        assert!(v.visible.iter().all(|x| *x == 1.0));
    }

    #[test]
    fn the_inside_of_a_box_is_hidden() {
        let mut m = cube();
        // Turned inside out, every ray from a face hits another face from behind it.
        m.triangles.iter_mut().for_each(|t| t.swap(1, 2));
        let v = Visibility::new(&m, Setup::Aligned);
        let mean = v.visible.iter().sum::<f32>() / v.visible.len() as f32;
        assert!(mean < 0.05, "{mean}");
    }

    #[test]
    fn decimating_reaches_the_target_and_stays_on_the_surface() {
        // A grid of little squares.
        let (n, mut m) = (60u32, Its::default());
        for y in 0..=n {
            for x in 0..=n {
                m.vertices.push(Vec3::new(x as f32 * 0.5, y as f32 * 0.5, 0.0));
            }
        }
        for y in 0..n {
            for x in 0..n {
                let a = y * (n + 1) + x;
                m.triangles.push([a, a + 1, a + n + 2]);
                m.triangles.push([a, a + n + 2, a + n + 1]);
            }
        }
        let out = decimate(&m, 1000);
        assert!(out.triangles.len() <= 1000 && out.triangles.len() > 500, "{}", out.triangles.len());
        assert!(out.vertices.iter().all(|v| v.z == 0.0 && v.x >= 0.0 && v.x <= 30.0));
    }

    #[test]
    fn the_fit_goes_through_a_straight_line() {
        let at: Vec<f32> = (0..50).map(|i| i as f32 * 0.2).collect();
        let points: Vec<Vec2> = at.iter().map(|z| Vec2::new(1.0 + z * 0.5, 2.0 - z)).collect();
        let s = Spline::fit(&points, &at, &[1.0; 50], 3);
        assert!(s.is_some());
        let Some(s) = s else { return };
        // With no spare parameters at the ends the curve only settles inside them, as in Orca.
        for z in [2.0f32, 4.0, 7.5] {
            assert!((s.at(z) - Vec2::new(1.0 + z * 0.5, 2.0 - z)).length() < 0.06, "{z}: {:?}", s.at(z));
        }
    }

    #[test]
    fn angles_read_the_turn_at_each_corner() {
        let pts: Vec<DVec2> =
            [(0., 0.), (20., 0.), (20., 20.), (0., 20.), (0., 0.)].iter().map(|(x, y)| DVec2::new(*x, *y)).collect();
        let mut lengths: Vec<f32> = pts.windows(2).map(|w| w[0].distance(w[1]) as f32).collect();
        lengths.push(0.1);
        let a = polygon_angles(&pts, &lengths, 0.4);
        for v in a.iter().take(4) {
            assert!((*v - std::f32::consts::FRAC_PI_2).abs() < 1e-5, "{a:?}");
        }
    }

    #[test]
    fn splitting_a_loop_keeps_its_overhang_marks() {
        let mut p = Path::new(
            Role::OuterWall,
            vec![DVec2::ZERO, DVec2::new(10.0, 0.0), DVec2::new(10.0, 10.0), DVec2::new(0.0, 10.0)],
            true,
            0.4,
            1.0,
            1.0,
        );
        p.overhang = vec![0, 90, 0, 0];
        start_at(&mut p, DVec2::new(10.0, 4.0));
        assert_eq!(p.points.first().copied(), Some(DVec2::new(10.0, 4.0)));
        assert_eq!(p.points.len(), 5);
        assert_eq!(p.overhang.len(), 5);
        assert_eq!(p.hang_at(0), 90);
    }

    #[test]
    fn outlines_tell_inside_from_outside() {
        let shape: Shape =
            vec![vec![DVec2::new(0.0, 0.0), DVec2::new(10.0, 0.0), DVec2::new(10.0, 10.0), DVec2::new(0.0, 10.0)]];
        let o = Outline::new(&[&shape], DVec2::ZERO);
        assert!((o.signed(DVec2::new(1.0, 5.0)) + 1.0).abs() < 1e-9);
        assert!((o.signed(DVec2::new(12.0, 5.0)) - 2.0).abs() < 1e-9);
    }

    #[test]
    fn sorting_with_a_bad_comparator_does_not_panic() {
        let mut v: Vec<Seam> = (0..1000).map(|i| (i, i * 7919 % 1000)).collect();
        merge_sort(&mut v, &|a, b| (a.0 ^ b.1).is_multiple_of(3));
        assert_eq!(v.len(), 1000);
    }
}
