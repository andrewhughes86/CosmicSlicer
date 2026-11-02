//! The canvas tools that work on an object's surface: painting (supports, seams, fuzzy skin,
//! colour), measuring, and placing brim ears.
//!
//! Painting marks whole triangles of the object's main mesh, as OrcaSlicer stores them, with
//! a circle or sphere brush, or a fill over connected triangles (all of them, those facing
//! within an angle of the clicked one, or the small unpainted gaps).

use std::collections::{HashMap, VecDeque};

use glam::{Mat4, Vec3};

use CosmicSlicer_model::threemf::PaintKind;
use CosmicSlicer_model::{Mesh, Object};

use super::gpu::FlatGeometry;
use crate::state::Session;

/// How the brush picks triangles.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Brush {
    /// Triangles facing the view within the radius.
    Circle,
    /// Every triangle within the radius, front or back.
    Sphere,
    /// Everything connected to the clicked triangle.
    Fill,
    /// Connected triangles facing within an angle of the clicked one.
    SmartFill,
}

impl Brush {
    pub const ALL: [(Brush, &'static str); 4] =
        [(Brush::Circle, "Circle"), (Brush::Sphere, "Sphere"), (Brush::Fill, "Fill"), (Brush::SmartFill, "Smart fill")];

    pub fn drags(self) -> bool {
        matches!(self, Brush::Circle | Brush::Sphere)
    }
}

/// A point picked on an object's surface.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SurfacePoint {
    pub id: u64,
    /// The main mesh's triangle, when the point is on it.
    pub tri: Option<usize>,
    pub point: Vec3,
    pub normal: Vec3,
}

/// The surface tools' settings and picks.
#[derive(Clone, Debug)]
pub struct ToolState {
    pub brush: Brush,
    /// Brush radius, mm.
    pub radius: f32,
    /// Smart fill's angle, degrees.
    pub smart_angle: f32,
    /// What painting puts down: 1 enforce, 2 block (the filament number for colour); 0 erases.
    pub state: u8,
    /// Gaps smaller than this (mm²) fill with "Fill gaps".
    pub gap_area: f32,
    /// The surface under the mouse.
    pub hover: Option<SurfacePoint>,
    /// Measure: up to two picks.
    pub measure: Vec<SurfacePoint>,
    /// Brim ears: the radius of the next one.
    pub ear_radius: f32,
    pub cut: CutState,
    pub text: TextState,
}

/// Where embossed text or an SVG goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EmbossMode {
    /// An object of its own.
    Object,
    /// Raised on the selected object's top.
    Emboss,
    /// Cut into the selected object's top.
    Engrave,
    /// A modifier over the selected object's top.
    Modifier,
}

/// The Text shape tool's settings.
#[derive(Clone, Debug)]
pub struct TextState {
    pub text: String,
    /// Index into the built-in fonts, or past them for a loaded one.
    pub font: usize,
    /// A font file loaded by the user: name and bytes.
    pub custom: Option<(String, std::sync::Arc<Vec<u8>>)>,
    /// Letter height (or SVG size), mm.
    pub height: f32,
    pub depth: f32,
    pub mode: EmbossMode,
}

impl Default for TextState {
    fn default() -> Self {
        Self { text: "Text".into(), font: 0, custom: None, height: 10.0, depth: 1.0, mode: EmbossMode::Object }
    }
}

impl TextState {
    /// The chosen font's bytes.
    pub fn font_bytes(&self) -> &[u8] {
        match (crate::emboss::FONTS.get(self.font), &self.custom) {
            (Some((_, bytes)), _) => bytes,
            (None, Some((_, bytes))) => bytes.as_slice(),
            (None, None) => crate::emboss::FONTS[0].1,
        }
    }
}

/// The Cut tool's plane and options.
#[derive(Clone, Debug, Default)]
pub struct CutState {
    /// The object the plane was set up for.
    pub object: Option<u64>,
    /// Height of the plane's centre above the object's bottom, mm.
    pub height: f32,
    /// Tilt about X and Y, degrees.
    pub tilt: [f32; 2],
    pub opts: crate::state::CutOptions,
    /// Clicks on the plane place connectors.
    pub placing: bool,
}

impl CutState {
    /// The plane for object `o`: a point on it (scene coordinates) and its normal.
    pub fn plane(&self, o: &Object) -> (Vec3, Vec3) {
        let b = o.bounds();
        let origin = Vec3::new(b.centre().x, b.centre().y, b.min.z + self.height);
        let turn =
            glam::Quat::from_euler(glam::EulerRot::XYZ, self.tilt[0].to_radians(), self.tilt[1].to_radians(), 0.0);
        (origin, turn * Vec3::Z)
    }

    /// Sets the plane up for object `o` the first time it is cut.
    pub fn follow(&mut self, o: &Object) {
        if self.object != Some(o.id) {
            self.object = Some(o.id);
            self.height = o.bounds().size().z / 2.0;
            self.tilt = [0.0, 0.0];
            self.opts.connectors.clear();
        }
    }

    /// Half the side of the square drawn for the plane.
    pub fn half_size(o: &Object) -> f32 {
        o.bounds().size().max_element() * 0.75
    }
}

/// The cut plane over `o`, its outline and the connectors.
pub fn cut_overlay(o: &Object, cs: &CutState, g: &mut FlatGeometry) {
    let (origin, n) = cs.plane(o);
    let s = CutState::half_size(o);
    let u = n.any_orthonormal_vector();
    let v = n.cross(u);
    let corners = [origin + (u + v) * s, origin + (v - u) * s, origin - (u + v) * s, origin + (u - v) * s];
    let fill = [0.95, 0.55, 0.15, 0.3];
    g.triangle(corners[0], corners[1], corners[2], fill);
    g.triangle(corners[0], corners[2], corners[3], fill);
    for k in 0..4 {
        g.line(corners[k], corners[(k + 1) % 4], [0.95, 0.55, 0.15, 1.0]);
    }
    for c in &cs.opts.connectors {
        ring(g, *c + n * 0.05, n, cs.opts.connector_radius, [1.0, 1.0, 1.0, 1.0]);
    }
}

/// Where the ray from `o` along `d` meets the cut plane over object `obj`, inside its square.
pub fn on_cut_plane(obj: &Object, cs: &CutState, o: Vec3, d: Vec3) -> Option<Vec3> {
    let (origin, n) = cs.plane(obj);
    let denom = d.dot(n);
    if denom.abs() < 1e-6 {
        return None;
    }
    let t = (origin - o).dot(n) / denom;
    let p = o + d * t;
    (t > 0.0 && (p - origin).length() <= CutState::half_size(obj) * 1.42).then_some(p)
}

impl Default for ToolState {
    fn default() -> Self {
        Self {
            brush: Brush::Circle,
            radius: 2.0,
            smart_angle: 30.0,
            state: 1,
            gap_area: 2.0,
            hover: None,
            measure: Vec::new(),
            ear_radius: 3.0,
            cut: CutState::default(),
            text: TextState::default(),
        }
    }
}

/// The triangles of `mesh` placed by `m`: corners, centroid and outward normal.
fn placed_triangle(mesh: &Mesh, m: &Mat4, t: &[u32; 3]) -> Option<([Vec3; 3], Vec3, Vec3)> {
    let c = mesh.corners(t)?.map(|p| m.transform_point3(p));
    let n = (c[1] - c[0]).cross(c[2] - c[0]).normalize_or_zero();
    Some((c, (c[0] + c[1] + c[2]) / 3.0, n))
}

/// How far `p` is from the nearest point of triangle `c`.
fn distance_to_triangle(p: Vec3, c: &[Vec3; 3]) -> f32 {
    let [a, b, d] = *c;
    let n = (b - a).cross(d - a);
    let len2 = n.length_squared();
    if len2 > 1e-12 {
        // Inside the triangle seen square-on: the distance to its plane.
        let q = p - n * ((p - a).dot(n) / len2);
        let inside = [(a, b), (b, d), (d, a)].iter().all(|(u, v)| (*v - *u).cross(q - *u).dot(n) >= 0.0);
        if inside {
            return (p - q).length();
        }
    }
    let seg = |u: Vec3, v: Vec3| {
        let e = v - u;
        let t = if e.length_squared() > 1e-12 { ((p - u).dot(e) / e.length_squared()).clamp(0.0, 1.0) } else { 0.0 };
        (p - (u + e * t)).length()
    };
    seg(a, b).min(seg(b, d)).min(seg(d, a))
}

/// Triangles that share an edge, by triangle.
fn neighbours(mesh: &Mesh) -> Vec<Vec<usize>> {
    let mut by_edge: HashMap<(u32, u32), Vec<usize>> = HashMap::new();
    for (i, t) in mesh.triangles.iter().enumerate() {
        for k in 0..3 {
            let (a, b) = (t[k], t[(k + 1) % 3]);
            by_edge.entry((a.min(b), a.max(b))).or_default().push(i);
        }
    }
    let mut out = vec![Vec::new(); mesh.triangles.len()];
    for list in by_edge.values() {
        for &a in list {
            for &b in list.iter().filter(|&&b| b != a) {
                if let Some(n) = out.get_mut(a) {
                    n.push(b);
                }
            }
        }
    }
    out
}

/// Connected triangles from `seed` that `keep` accepts.
fn flood(mesh: &Mesh, seed: usize, keep: impl Fn(usize) -> bool) -> Vec<u32> {
    let nb = neighbours(mesh);
    let mut seen = vec![false; mesh.triangles.len()];
    let mut out = Vec::new();
    let mut queue = VecDeque::from([seed]);
    if let Some(s) = seen.get_mut(seed) {
        *s = true;
    }
    while let Some(t) = queue.pop_front() {
        out.push(t as u32);
        for &n in nb.get(t).map_or(&[][..], Vec::as_slice) {
            if !seen.get(n).copied().unwrap_or(true) && keep(n) {
                if let Some(s) = seen.get_mut(n) {
                    *s = true;
                }
                queue.push_back(n);
            }
        }
    }
    out
}

/// The triangles a brush stroke at `at` covers, looking along `view`.
pub fn brushed(o: &Object, ts: &ToolState, at: &SurfacePoint, view: Vec3) -> Vec<u32> {
    let m = o.matrix();
    let mesh = o.mesh.as_ref();
    match ts.brush {
        Brush::Circle | Brush::Sphere => mesh
            .triangles
            .iter()
            .enumerate()
            .filter_map(|(i, t)| {
                let (c, _, n) = placed_triangle(mesh, &m, t)?;
                let near = distance_to_triangle(at.point, &c) <= ts.radius;
                let facing = ts.brush == Brush::Sphere || n.dot(view) < 0.05;
                (near && facing).then_some(i as u32)
            })
            .collect(),
        Brush::Fill => at.tri.map(|seed| flood(mesh, seed, |_| true)).unwrap_or_default(),
        Brush::SmartFill => {
            let Some(seed) = at.tri else { return Vec::new() };
            let Some((_, _, n0)) = mesh.triangles.get(seed).and_then(|t| placed_triangle(mesh, &m, t)) else {
                return Vec::new();
            };
            let limit = ts.smart_angle.to_radians().cos();
            flood(mesh, seed, |i| {
                mesh.triangles
                    .get(i)
                    .and_then(|t| placed_triangle(mesh, &m, t))
                    .is_some_and(|(_, _, n)| n.dot(n0) >= limit)
            })
        }
    }
}

/// Unpainted patches smaller than `area` mm², each surrounded by paint: OrcaSlicer's gap fill.
pub fn gaps(o: &Object, kind: PaintKind, area: f32) -> Vec<u32> {
    let mesh = o.mesh.as_ref();
    let m = o.matrix();
    let states = o.paint.as_deref().map(|p| p.states(kind)).unwrap_or_default();
    if states.is_empty() {
        return Vec::new();
    }
    let painted = |i: usize| states.contains_key(&(i as u32));
    let nb = neighbours(mesh);
    let mut seen = vec![false; mesh.triangles.len()];
    let mut out = Vec::new();
    for start in 0..mesh.triangles.len() {
        if painted(start) || seen.get(start).copied().unwrap_or(true) {
            continue;
        }
        let patch = flood(mesh, start, |i| !painted(i));
        for &t in &patch {
            if let Some(s) = seen.get_mut(t as usize) {
                *s = true;
            }
        }
        let size: f32 = patch
            .iter()
            .filter_map(|&t| mesh.triangles.get(t as usize).and_then(|tri| placed_triangle(mesh, &m, tri)))
            .map(|(c, _, _)| (c[1] - c[0]).cross(c[2] - c[0]).length() / 2.0)
            .sum();
        let touches_paint = patch.iter().any(|&t| nb.get(t as usize).is_some_and(|n| n.iter().any(|&x| painted(x))));
        if size <= area && touches_paint {
            out.extend(patch);
        }
    }
    out
}

/// The colour a painted state shows in.
pub fn state_colour(session: &Session, kind: PaintKind, state: u8) -> [f32; 4] {
    match (kind, state) {
        (PaintKind::Color, f) => {
            let hex = session.presets.filament_colour(session.schema, usize::from(f).saturating_sub(1));
            let c = crate::theme::parse_hex(&hex).unwrap_or(egui::Color32::GRAY);
            let lin = egui::Rgba::from(c);
            [lin.r(), lin.g(), lin.b(), 1.0]
        }
        (PaintKind::Fuzzy, _) => [0.95, 0.55, 0.1, 0.9],
        (_, 1) => [0.15, 0.75, 0.3, 0.9],
        _ => [0.9, 0.2, 0.2, 0.9],
    }
}

/// The painted triangles of `o`, just off its surface, and the brush at `hover`.
pub fn paint_overlay(session: &Session, o: &Object, kind: PaintKind, ts: &ToolState, g: &mut FlatGeometry) {
    let m = o.matrix();
    if let Some(paint) = o.paint.as_deref() {
        for (t, state) in paint.states(kind) {
            let Some((c, _, n)) = o.mesh.triangles.get(t as usize).and_then(|tri| placed_triangle(&o.mesh, &m, tri))
            else {
                continue;
            };
            let lift = n * 0.03;
            g.triangle(c[0] + lift, c[1] + lift, c[2] + lift, state_colour(session, kind, state));
        }
    }
    if let Some(h) = ts.hover.filter(|h| h.id == o.id && ts.brush.drags()) {
        ring(g, h.point + h.normal * 0.05, h.normal, ts.radius, [1.0, 1.0, 1.0, 0.9]);
    }
}

/// A circle of radius `r` about `centre`, square to `normal`.
pub fn ring(g: &mut FlatGeometry, centre: Vec3, normal: Vec3, r: f32, colour: [f32; 4]) {
    let n = normal.normalize_or(Vec3::Z);
    let u = n.any_orthonormal_vector();
    let v = n.cross(u);
    let at = |k: usize| {
        let a = std::f32::consts::TAU * k as f32 / 48.0;
        centre + (u * a.cos() + v * a.sin()) * r
    };
    for k in 0..48 {
        g.line(at(k), at(k + 1), colour);
    }
}

/// What two measured points say: their distance, and for two faces the angle between them and
/// (when parallel) the distance between their planes.
pub fn measure_text(a: &SurfacePoint, b: &SurfacePoint) -> Vec<String> {
    let d = b.point - a.point;
    let mut out =
        vec![format!("Distance: {:.3} mm", d.length()), format!("ΔX {:.3}  ΔY {:.3}  ΔZ {:.3} mm", d.x, d.y, d.z)];
    let cos = a.normal.dot(b.normal).clamp(-1.0, 1.0);
    let angle = cos.acos().to_degrees();
    out.push(format!("Angle between faces: {angle:.2}°"));
    if cos.abs() > 0.999 {
        out.push(format!("Between the parallel faces: {:.3} mm", d.dot(a.normal).abs()));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use CosmicSlicer_model::{Model, primitives};

    fn cube() -> Object {
        let mut m = Model::new();
        m.add("c", primitives::cube(10.0), Vec3::new(50.0, 50.0, 0.0), 0);
        m.objects.remove(0)
    }

    #[test]
    fn brushes_pick_what_they_should() {
        let o = cube();
        let top_tri = o
            .mesh
            .triangles
            .iter()
            .position(|t| o.mesh.corners(t).is_some_and(|c| c.iter().all(|p| p.z > 9.0)))
            .unwrap_or(0);
        let at = SurfacePoint { id: o.id, tri: Some(top_tri), point: Vec3::new(50.0, 50.0, 10.0), normal: Vec3::Z };
        let mut ts = ToolState { radius: 1.0, ..Default::default() };
        // A small circle on the top's middle: the top's two triangles share that point.
        let small = brushed(&o, &ts, &at, -Vec3::Z);
        assert!(!small.is_empty() && small.len() <= 2, "{small:?}");
        ts.brush = Brush::Fill;
        assert_eq!(brushed(&o, &ts, &at, -Vec3::Z).len(), 12);
        ts.brush = Brush::SmartFill;
        assert_eq!(brushed(&o, &ts, &at, -Vec3::Z).len(), 2);
    }

    #[test]
    fn small_gaps_fill() {
        let mut o = cube();
        let mut paint = CosmicSlicer_model::Paint::default();
        // Everything but one triangle.
        let all: Vec<u32> = (1..12).collect();
        paint.set(PaintKind::Support, &all, 1);
        o.paint = Some(std::sync::Arc::new(paint));
        assert_eq!(gaps(&o, PaintKind::Support, 100.0), vec![0]);
        assert!(gaps(&o, PaintKind::Support, 1.0).is_empty());
    }

    #[test]
    fn measuring_two_faces() {
        let a = SurfacePoint { id: 1, tri: None, point: Vec3::ZERO, normal: Vec3::Z };
        let b = SurfacePoint { id: 1, tri: None, point: Vec3::new(3.0, 0.0, 4.0), normal: -Vec3::Z };
        let t = measure_text(&a, &b);
        assert!(t[0].contains("5.000"));
        assert!(t.iter().any(|l| l.contains("180.00")));
        assert!(t.iter().any(|l| l.contains("parallel faces: 4.000")));
    }
}
