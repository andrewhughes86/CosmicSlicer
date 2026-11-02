//! Supports: what each layer leaves hanging, then columns (grid or snug) or trees
//! ([`crate::tree`]) under it, with denser interface layers under the part and where support
//! stands on it; and the raft under everything.
//!
//! An area needs support where a layer reaches out past the one below by more than the
//! overhang angle allows, or where an enforcer asks for it; blockers take it away. Columns
//! carry each area down layer by layer until it lands on the plate or on the part, kept a gap
//! away from the part sideways, above and below.

use std::f64::consts::FRAC_PI_2;

use glam::DVec2;
use rayon::prelude::*;

use crate::geom::{self, Join, Op, Polyline, Shape};
use crate::infill::{self, Kind};
use crate::lightning;
use crate::settings::{Settings, SupportStyle};
use crate::toolpath::{Path, Role};
use crate::tree;

/// Support regions per layer.
#[derive(Default)]
pub struct SupportLayers {
    /// Columns.
    pub base: Vec<Vec<Shape>>,
    /// Tree branches.
    pub tree: Vec<Vec<Shape>>,
    /// Under the part.
    pub interface: Vec<Vec<Shape>>,
    /// Where support stands on the part.
    pub bottom: Vec<Vec<Shape>>,
    /// Brim round tree trunks, on the first layer.
    pub tree_brim: Vec<Shape>,
    /// Lightning base lines per layer, when that is the base pattern.
    pub lightning: Vec<Vec<Polyline>>,
}

impl SupportLayers {
    fn empty(n: usize) -> SupportLayers {
        let e = || vec![Vec::new(); n];
        SupportLayers {
            base: e(),
            tree: e(),
            interface: e(),
            bottom: e(),
            tree_brim: Vec::new(),
            lightning: vec![Vec::new(); n],
        }
    }

    /// Everything support covers on layer `i`.
    pub fn footprint(&self, i: usize) -> Vec<Shape> {
        [&self.base, &self.tree, &self.interface, &self.bottom]
            .iter()
            .filter_map(|l| l.get(i))
            .flatten()
            .cloned()
            .chain(if i == 0 { self.tree_brim.clone() } else { Vec::new() })
            .collect()
    }
}

/// One object's share of support generation.
#[derive(Clone, Copy)]
pub struct Input<'a> {
    /// The object's layers.
    pub slices: &'a [Vec<Shape>],
    /// The object's settings (its overrides on the plate's).
    pub settings: &'a Settings,
    /// Support enforcer volumes and painted facets, per layer.
    pub enforce: &'a [Vec<Shape>],
    pub block: &'a [Vec<Shape>],
}

impl Input<'_> {
    /// Whether this object gets any support.
    pub fn wants(&self) -> bool {
        let sp = &self.settings.support;
        sp.enforce_layers > 0 || (sp.enabled && (sp.auto || self.enforce.iter().any(|e| !e.is_empty())))
    }
}

/// Support for every object. `all[i]` is everything printed on layer `i` (every object), which
/// support keeps clear of; `heights` are the layer heights; `s` the plate's settings.
pub fn generate(
    objects: &[Input],
    all: &[Vec<Shape>],
    heights: &[f64],
    s: &Settings,
    cancelled: &(dyn Fn() -> bool + Sync),
) -> SupportLayers {
    let n = all.len();
    let mut out = SupportLayers::empty(n);
    if n < 2 {
        return out;
    }
    let mut sources = 0;
    for o in objects.iter().filter(|o| o.wants()) {
        if cancelled() {
            return out;
        }
        let os = o.settings;
        let need: Vec<Vec<Shape>> =
            (0..n)
                .into_par_iter()
                .map(|i| {
                    if cancelled() {
                        Vec::new()
                    } else {
                        need(o, i, heights.get(i).copied().unwrap_or(os.layer_height))
                    }
                })
                .collect();
        let (columns_need, trees_need): (Vec<Vec<Shape>>, Vec<Vec<Shape>>) = match os.support.style {
            SupportStyle::Grid | SupportStyle::Snug => (need, Vec::new()),
            SupportStyle::TreeHybrid => {
                // Broad, flat overhangs get columns; the rest trees.
                let r = os.support.tree.branch_distance;
                need.into_par_iter()
                    .map(|a| {
                        if a.is_empty() {
                            return (Vec::new(), Vec::new());
                        }
                        let broad = geom::offset(&geom::offset(&a, -r, Join::Round), r, Join::Round);
                        let rest = if broad.is_empty() { a } else { geom::boolean(&a, &broad, Op::Difference) };
                        (broad, rest)
                    })
                    .unzip()
            }
            _ => (Vec::new(), need),
        };
        if columns_need.iter().any(|a| !a.is_empty()) {
            columns(&columns_need, all, os, &mut out, cancelled);
            sources += 1;
        }
        if trees_need.iter().any(|a| !a.is_empty()) {
            let t = tree::grow(&trees_need, all, heights, os, cancelled);
            for (dst, src) in [(&mut out.tree, t.area), (&mut out.interface, t.interface)] {
                for (d, sh) in dst.iter_mut().zip(src) {
                    d.extend(sh);
                }
            }
            out.tree_brim.extend(t.brim);
            sources += 1;
        }
    }
    if sources > 1 {
        for layers in [&mut out.base, &mut out.tree, &mut out.interface, &mut out.bottom] {
            layers.par_iter_mut().filter(|l| !l.is_empty()).for_each(|l| *l = geom::union(l));
        }
        // Interfaces win over the base and branches they overlap.
        let (iface, bottom) = (&out.interface, &out.bottom);
        for layers in [&mut out.base, &mut out.tree] {
            layers.par_iter_mut().enumerate().filter(|(_, l)| !l.is_empty()).for_each(|(i, l)| {
                let top: Vec<Shape> = iface.get(i).into_iter().chain(bottom.get(i)).flatten().cloned().collect();
                if !top.is_empty() {
                    *l = geom::boolean(l, &top, Op::Difference);
                }
            });
        }
        out.tree_brim = geom::union(&out.tree_brim);
    }
    if s.support.base_pattern == "lightning" && out.base.iter().any(|b| !b.is_empty()) {
        out.lightning = lightning_base(&out, heights, s, cancelled);
    }
    out
}

/// What layer `i` of one object leaves hanging that support must hold.
fn need(o: &Input, i: usize, h: f64) -> Vec<Shape> {
    let s = o.settings;
    let sp = &s.support;
    let w = s.widths.support;
    let (Some(here), Some(below)) = (o.slices.get(i), i.checked_sub(1).and_then(|j| o.slices.get(j))) else {
        return Vec::new();
    };
    if here.is_empty() {
        return Vec::new();
    }
    // Whatever does not rest on the layer below at all.
    let bare = || geom::boolean(here, &geom::offset(below, w * 0.1, Join::Round), Op::Difference);
    let mut out = Vec::new();
    if sp.enabled && sp.auto {
        // Faces closer to horizontal than the threshold need support: the allowed reach a layer.
        let held = geom::offset(below, reach(s, h), Join::Round);
        let mut hang = geom::boolean(here, &held, Op::Difference);
        if sp.critical_only {
            hang = critical(hang, here, below);
        }
        // Tiny overhangs bridge on their own.
        out = geom::without_slivers(hang, if sp.remove_small { w * w * 6.0 } else { w * w });
    }
    if i <= sp.enforce_layers {
        out = geom::boolean(&out, &bare(), Op::Union);
    }
    if sp.enabled
        && let Some(e) = o.enforce.get(i).filter(|e| !e.is_empty())
    {
        let forced = geom::boolean(&bare(), &geom::offset(e, w, Join::Round), Op::Intersect);
        out = geom::boolean(&out, &forced, Op::Union);
    }
    if let Some(b) = o.block.get(i).filter(|b| !b.is_empty())
        && !out.is_empty()
    {
        out = geom::boolean(&out, &geom::offset(b, w, Join::Round), Op::Difference);
    }
    out
}

/// How far a layer may reach past the one below before it needs support.
fn reach(s: &Settings, h: f64) -> f64 {
    let sp = &s.support;
    let w = s.widths.support;
    let r = if sp.threshold_angle > 0.0 {
        (h / sp.threshold_angle.to_radians().tan()).min(w * 4.0)
    } else {
        w * (1.0 - sp.threshold_overlap)
    };
    r.max(0.0)
}

/// OrcaSlicer's "critical regions only": parts that start in mid-air (sharp tails) and broad
/// overhangs; slopes that print on their own are left.
fn critical(hang: Vec<Shape>, here: &[Shape], below: &[Shape]) -> Vec<Shape> {
    if hang.is_empty() {
        return hang;
    }
    let mut keep: Vec<Shape> = here
        .iter()
        .filter(|isl| geom::boolean(std::slice::from_ref(*isl), below, Op::Intersect).is_empty())
        .cloned()
        .collect();
    keep.extend(geom::offset(&geom::offset(&hang, -2.5, Join::Round), 2.5, Join::Round));
    geom::boolean(&hang, &keep, Op::Intersect)
}

/// Columns under `need`, added to `out`.
fn columns(
    need: &[Vec<Shape>],
    all: &[Vec<Shape>],
    s: &Settings,
    out: &mut SupportLayers,
    cancelled: &(dyn Fn() -> bool + Sync),
) {
    let sp = &s.support;
    let w = s.widths.support;
    let h = s.layer_height;
    let n = all.len();
    let gap_top = (sp.top_z_distance / h).round() as usize;
    let gap_bottom = (sp.bottom_z_distance / h).round() as usize;
    let snug = sp.style == SupportStyle::Snug;
    // What each layer of a slope leaves hanging is a strip the reach away from the next
    // one's; grown by the reach, the strips join into one column. Grid then closes the
    // columns, merging neighbours into blocks; snug follows the outline.
    let grow = |fresh: Vec<Shape>, at: usize| -> Vec<Shape> {
        let grown = geom::offset(&fresh, reach(s, h) + w * 0.1, Join::Round);
        // Only under the part: the gaps lie inside this layer or the next.
        let parts: Vec<Shape> = all.get(at).into_iter().chain(all.get(at + 1)).flatten().cloned().collect();
        let grown = geom::boolean(&grown, &parts, Op::Intersect);
        let grown = if sp.expansion.abs() > 1e-6 { geom::offset(&grown, sp.expansion, Join::Round) } else { grown };
        if snug {
            grown
        } else {
            geom::offset(&geom::offset(&grown, sp.spacing, Join::Miter), -sp.spacing, Join::Miter)
        }
    };
    // On the build plate only: support may not stand on anything printed below.
    let shadow: Vec<Vec<Shape>> = if sp.build_plate_only {
        let mut acc: Vec<Shape> = Vec::new();
        all.iter()
            .map(|layer| {
                if !layer.is_empty() {
                    acc = geom::boolean(&acc, layer, Op::Union);
                }
                acc.clone()
            })
            .collect()
    } else {
        Vec::new()
    };

    // Each layer's fresh overhang grown into columns: independent of the support above, so
    // made in parallel; only the carrying down below runs layer by layer.
    let fresh: Vec<Vec<Shape>> = (0..n)
        .into_par_iter()
        .map(|i| {
            let at = i + 1 + gap_top;
            let f = need.get(at).cloned().unwrap_or_default();
            if f.is_empty() || i + 1 >= n || cancelled() {
                return Vec::new();
            }
            let f = grow(f, at);
            match shadow.get(i).filter(|sh| !sh.is_empty()) {
                Some(sh) => geom::boolean(&f, &geom::offset(sh, sp.xy_distance, Join::Round), Op::Difference),
                None => f,
            }
        })
        .collect();
    let mut carried: Vec<Vec<Shape>> = vec![Vec::new(); n];
    let mut carry: Vec<Shape> = Vec::new();
    for i in (0..n.saturating_sub(1)).rev() {
        if cancelled() {
            return;
        }
        if let Some(f) = fresh.get(i).filter(|f| !f.is_empty()) {
            carry = geom::boolean(&carry, f, Op::Union);
        }
        let Some(here) = all.get(i) else { continue };
        // Support ends where it lands on the part.
        carry = geom::without_slivers(geom::boolean(&carry, here, Op::Difference), w * w * 4.0);
        if let Some(c) = carried.get_mut(i) {
            c.clone_from(&carry);
        }
    }
    // Each layer's column: clear of the part, smoothed, and its top interface under the part
    // on the layers just above (past the gap).
    let reach_up = sp.interface_layers.max(1);
    let (mut region, top): (Vec<Vec<Shape>>, Vec<Vec<Shape>>) = carried
        .into_par_iter()
        .enumerate()
        .map(|(i, carry)| {
            let Some(here) = all.get(i).filter(|_| !carry.is_empty()) else { return (Vec::new(), Vec::new()) };
            let gap = if i == 0 { sp.first_layer_gap } else { sp.xy_distance };
            let clear = geom::offset(here, gap + w / 2.0, Join::Round);
            let r = geom::boolean(&carry, &clear, Op::Difference);
            // Smooth the column outlines a little.
            let k = if snug { w / 2.0 } else { w };
            let r = geom::offset(&geom::offset(&r, -k, Join::Round), k, Join::Round);
            let t = if sp.interface_layers > 0 {
                let near: Vec<Shape> =
                    (i + 1 + gap_top..=i + gap_top + reach_up).filter_map(|j| all.get(j)).flatten().cloned().collect();
                geom::boolean(&r, &near, Op::Intersect)
            } else {
                Vec::new()
            };
            (r, t)
        })
        .unzip();

    // Standing on the part: a gap over it, then the bottom interface layers.
    let union_of = |layers: std::ops::RangeInclusive<usize>| -> Vec<Shape> {
        let parts: Vec<Shape> = layers.filter_map(|j| all.get(j)).flatten().cloned().collect();
        if parts.is_empty() { parts } else { geom::union(&parts) }
    };
    let bottom: Vec<Vec<Shape>> = region
        .par_iter_mut()
        .enumerate()
        .map(|(i, r)| {
            if r.is_empty() || i == 0 {
                return Vec::new();
            }
            if gap_bottom > 0 {
                let under = union_of(i.saturating_sub(gap_bottom)..=i - 1);
                if !under.is_empty() {
                    *r = geom::boolean(r, &under, Op::Difference);
                }
            }
            let (Some(hi), true) = (i.checked_sub(gap_bottom + 1), sp.bottom_interface_layers > 0) else {
                return Vec::new();
            };
            let lo = hi.saturating_sub(sp.bottom_interface_layers - 1);
            let under = union_of(lo..=hi);
            if under.is_empty() { Vec::new() } else { geom::boolean(r, &under, Op::Intersect) }
        })
        .collect();

    for i in 0..n {
        let r = region.get(i).cloned().unwrap_or_default();
        if r.is_empty() {
            continue;
        }
        let t = top.get(i).map(|t| geom::boolean(t, &r, Op::Intersect)).unwrap_or_default();
        let b = bottom.get(i).map(|b| geom::boolean(b, &t, Op::Difference)).unwrap_or_default();
        let solid: Vec<Shape> = t.iter().chain(&b).cloned().collect();
        let base = if solid.is_empty() { r } else { geom::boolean(&r, &solid, Op::Difference) };
        for (dst, src) in [(&mut out.base, base), (&mut out.interface, t), (&mut out.bottom, b)] {
            if let Some(d) = dst.get_mut(i) {
                d.extend(geom::without_slivers(src, w * w * 2.0));
            }
        }
    }
}

/// Lightning lines inside the columns: only what holds up the interface and column tops.
fn lightning_base(
    sl: &SupportLayers,
    heights: &[f64],
    s: &Settings,
    cancelled: &(dyn Fn() -> bool + Sync),
) -> Vec<Vec<Polyline>> {
    let w = s.widths.support;
    let inside: Vec<Vec<Shape>> = sl.base.par_iter().map(|b| geom::offset(b, -w, Join::Round)).collect();
    let demand: Vec<Vec<Shape>> = (0..inside.len())
        .into_par_iter()
        .map(|i| {
            let (Some(here), up) = (inside.get(i), sl.base.get(i + 1)) else { return Vec::new() };
            if here.is_empty() {
                return Vec::new();
            }
            let held: Vec<Shape> = sl.interface.get(i + 1).into_iter().flatten().cloned().collect();
            let mut d = if held.is_empty() { Vec::new() } else { geom::boolean(here, &held, Op::Intersect) };
            // Column tops.
            let tops = match up {
                Some(u) if !u.is_empty() => geom::boolean(here, u, Op::Difference),
                _ => here.clone(),
            };
            d.extend(tops);
            d
        })
        .collect();
    lightning::grow(&inside, &demand, heights, s.support.spacing, 45f64.to_radians(), w, cancelled)
}

/// Pattern names to generators for the interface.
fn interface_kind(pattern: &str) -> Kind {
    match pattern {
        "concentric" => Kind::Concentric,
        "spiralinset" => Kind::SpiralInset,
        "grid" => Kind::Grid,
        "rectilinear_interlaced" => Kind::Rectilinear { alternate: true },
        _ => Kind::Rectilinear { alternate: false },
    }
}

fn path(role: Role, points: Vec<DVec2>, closed: bool, width: f64, speed: f64, flow: f64) -> Path {
    Path::new(role, points, closed, width, speed, flow)
}

/// The support lines for one layer.
pub fn paths(sl: &SupportLayers, i: usize, s: &Settings, first_layer: bool, z: f64) -> Vec<Path> {
    let sp = &s.support;
    let w = if first_layer { s.widths.first_layer } else { s.widths.support };
    let speed = if first_layer { s.speeds.first_layer } else { s.speeds.support };
    let iface_speed = if first_layer { s.speeds.first_layer } else { s.speeds.support_interface };
    let mut out = Vec::new();
    let base_density = {
        let d = (w / sp.spacing).clamp(0.05, 1.0);
        // On the plate, denser support sticks better.
        if first_layer { (d * 2.0).min(1.0) } else { d }
    };
    if let Some(base) = sl.base.get(i).filter(|b| !b.is_empty()) {
        // A loop round each column keeps the lines standing.
        for c in geom::offset(base, -w / 2.0, Join::Round).iter().flatten() {
            out.push(path(Role::Support, c.clone(), true, w, speed, sp.flow));
        }
        let inside = geom::offset(base, -w, Join::Round);
        let kind = match (sp.base_pattern.as_str(), first_layer) {
            (_, true) | ("rectilinear", _) => Some(Kind::Rectilinear { alternate: false }),
            ("rectilinear-grid", _) => Some(Kind::Grid),
            ("honeycomb", _) => Some(Kind::Honeycomb),
            _ => None,
        };
        if let Some(kind) = kind {
            for l in infill::fill(&inside, kind, w, base_density, sp.angle, i, z) {
                out.push(path(Role::Support, l.points, l.closed, w, speed, sp.flow));
            }
        } else if sp.base_pattern == "lightning" {
            for l in sl.lightning.get(i).into_iter().flatten() {
                out.push(path(Role::Support, l.clone(), false, w, speed, sp.flow));
            }
        }
    }
    if let Some(t) = sl.tree.get(i).filter(|t| !t.is_empty()) {
        let walls = sp.tree.walls.max(1);
        let mut ring = geom::offset(t, -w / 2.0, Join::Round);
        for _ in 0..walls {
            if ring.is_empty() {
                break;
            }
            for c in ring.iter().flatten() {
                out.push(path(Role::Support, geom::simplify_loop(c, 0.01), true, w, speed, sp.flow));
            }
            ring = geom::offset(&ring, -w, Join::Round);
        }
        if (sp.tree.with_infill || first_layer) && !ring.is_empty() {
            let inner = geom::offset(&ring, w / 2.0, Join::Round);
            let kind = Kind::Rectilinear { alternate: false };
            for l in infill::fill(&inner, kind, w, base_density, sp.angle, i, z) {
                out.push(path(Role::Support, l.points, l.closed, w, speed, sp.flow));
            }
        }
    }
    if first_layer && !sl.tree_brim.is_empty() {
        for l in infill::fill(&sl.tree_brim, Kind::Concentric, w, 1.0, 0.0, i, z) {
            out.push(path(Role::Support, l.points, l.closed, w, speed, sp.flow));
        }
    }
    let kind = interface_kind(&sp.interface_pattern);
    for (layers, spacing) in [(&sl.interface, sp.interface_spacing), (&sl.bottom, sp.bottom_interface_spacing)] {
        if let Some(area) = layers.get(i).filter(|f| !f.is_empty()) {
            let density = (w / (spacing + w)).clamp(0.1, 1.0);
            for l in infill::fill(area, kind, w, density, sp.angle + FRAC_PI_2, i, z) {
                out.push(path(Role::SupportInterface, l.points, l.closed, w, iface_speed, sp.interface_flow));
            }
        }
    }
    out
}

/// One layer of the raft.
pub struct RaftLayer {
    pub area: Vec<Shape>,
    pub height: f64,
    /// The dense top layers the part sits on.
    pub interface: bool,
}

/// The raft under `footprint` (everything on the part's first layer, support included),
/// bottom up.
pub fn raft(footprint: &[Shape], s: &Settings) -> Vec<RaftLayer> {
    let r = &s.raft;
    if r.layers == 0 || footprint.is_empty() {
        return Vec::new();
    }
    let area = geom::offset(&geom::union(footprint), r.expansion, Join::Round);
    // The outsides only: the raft fills holes in the part.
    let area: Vec<Shape> = area.into_iter().filter_map(|sh| sh.first().map(|o| vec![o.clone()])).collect();
    let interface = s.support.interface_layers.max(1).min(r.layers - 1);
    (0..r.layers)
        .map(|k| RaftLayer {
            area: if k == 0 { geom::offset(&area, r.first_layer_expansion, Join::Round) } else { area.clone() },
            height: if k == 0 { s.first_layer_height } else { s.layer_height },
            interface: k >= r.layers - interface,
        })
        .collect()
}

/// The lines of raft layer `k`.
pub fn raft_paths(layer: &RaftLayer, k: usize, s: &Settings, z: f64) -> Vec<Path> {
    let first = k == 0;
    let sp = &s.support;
    let w = if first { s.widths.first_layer } else { s.widths.support };
    let speed = if first {
        s.speeds.first_layer
    } else if layer.interface {
        s.speeds.support_interface
    } else {
        s.speeds.support
    };
    let (role, density, flow) = if first {
        (Role::Support, s.raft.first_layer_density, sp.flow)
    } else if layer.interface {
        (Role::SupportInterface, (w / (sp.interface_spacing + w)).clamp(0.1, 1.0), sp.interface_flow)
    } else {
        (Role::Support, (w / sp.spacing).clamp(0.05, 1.0), sp.flow)
    };
    let mut out = Vec::new();
    for c in geom::offset(&layer.area, -w / 2.0, Join::Round).iter().flatten() {
        out.push(path(role, geom::simplify_loop(c, 0.01), true, w, speed, flow));
    }
    let inside = geom::offset(&layer.area, -w, Join::Round);
    // Each layer crosses the one under it.
    let angle = sp.angle + if k % 2 == 1 { FRAC_PI_2 } else { 0.0 };
    for l in infill::fill(&inside, Kind::Rectilinear { alternate: false }, w, density, angle, k, z) {
        out.push(path(role, l.points, l.closed, w, speed, flow));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use CosmicSlicer_config::{Config, Schema};

    fn settings(pairs: &[(&str, &str)]) -> Settings {
        let mut c = Config::default();
        c.set("enable_support", "1");
        for (k, v) in pairs {
            c.set((*k).to_owned(), (*v).to_owned());
        }
        Settings::read(Schema::get(), &c)
    }

    fn square(x: f64, y: f64, size: f64) -> Shape {
        geom::rectangle(DVec2::new(x, y), DVec2::new(x + size, y + size))
    }

    /// A T: a 6 mm post under a 20 mm slab, 0.2 mm layers; the slab from layer 30.
    fn t_shape() -> Vec<Vec<Shape>> {
        (0..40).map(|i| if i < 30 { vec![square(7.0, 7.0, 6.0)] } else { vec![square(0.0, 0.0, 20.0)] }).collect()
    }

    fn run(slices: &[Vec<Shape>], s: &Settings, enforce: &[Vec<Shape>], block: &[Vec<Shape>]) -> SupportLayers {
        let heights = vec![0.2; slices.len()];
        let input = Input { slices, settings: s, enforce, block };
        generate(&[input], slices, &heights, s, &|| false)
    }

    fn total(layers: &[Vec<Shape>]) -> f64 {
        layers.iter().map(|l| geom::area(l)).sum()
    }

    #[test]
    fn columns_hold_the_t_and_keep_clear() {
        let slices = t_shape();
        let s = settings(&[]);
        let sl = run(&slices, &s, &[], &[]);
        // Under the slab, down to the plate, never inside the post.
        assert!(!sl.base.first().is_none_or(Vec::is_empty));
        assert!(!sl.interface.get(28).is_none_or(Vec::is_empty));
        // The top gap: nothing on the layer just under the slab.
        assert!(sl.footprint(29).is_empty());
        for (i, part) in slices.iter().enumerate().take(29) {
            let overlap = geom::boolean(&sl.footprint(i), part, Op::Intersect);
            assert!(geom::area(&overlap) < 1e-3, "layer {i}");
        }
    }

    #[test]
    fn snug_supports_less_than_grid_and_trees_less_still() {
        // Two separate ledges: grid merges their columns, snug does not.
        let slices: Vec<Vec<Shape>> = (0..40)
            .map(|i| {
                if i < 30 {
                    vec![square(20.0, 0.0, 2.0)]
                } else {
                    vec![square(0.0, 0.0, 4.0), square(7.0, 0.0, 4.0), square(20.0, 0.0, 2.0)]
                }
            })
            .collect();
        let grid = total(&run(&slices, &settings(&[("support_style", "grid")]), &[], &[]).base);
        let snug = total(&run(&slices, &settings(&[("support_style", "snug")]), &[], &[]).base);
        assert!(snug > 0.0 && snug < grid, "snug {snug} grid {grid}");
        let t = run(&slices, &settings(&[("support_type", "tree(auto)")]), &[], &[]);
        assert!(total(&t.tree) > 0.0 && total(&t.tree) < snug);
        assert!(total(&t.base) == 0.0);
    }

    #[test]
    fn trees_reach_the_plate_round_the_post() {
        let slices = t_shape();
        for style in ["organic", "tree_slim", "tree_strong", "tree_hybrid"] {
            let s = settings(&[("support_type", "tree(auto)"), ("support_style", style)]);
            let sl = run(&slices, &s, &[], &[]);
            let on_plate = sl.footprint(0);
            assert!(!on_plate.is_empty(), "{style}");
            for (i, part) in slices.iter().enumerate().take(29) {
                let overlap = geom::boolean(&sl.footprint(i), part, Op::Intersect);
                assert!(geom::area(&overlap) < 1e-3, "{style} layer {i}");
            }
        }
    }

    #[test]
    fn build_plate_only_drops_support_on_the_part() {
        // A ledge over a wide base: its support would stand on the base.
        let slices: Vec<Vec<Shape>> = (0..40)
            .map(|i| {
                if i < 10 {
                    vec![square(0.0, 0.0, 20.0)]
                } else if i < 30 {
                    vec![square(0.0, 0.0, 4.0)]
                } else {
                    vec![square(0.0, 0.0, 15.0)]
                }
            })
            .collect();
        let normal = run(&slices, &settings(&[]), &[], &[]);
        assert!(total(&normal.base) > 0.0);
        // Standing on the base: a bottom gap, so nothing on layer 10.
        assert!(normal.footprint(10).is_empty() && !normal.footprint(11).is_empty());
        let plate = run(&slices, &settings(&[("support_on_build_plate_only", "1")]), &[], &[]);
        assert!(total(&plate.base) + total(&plate.interface) < 1e-6);
        let trees =
            run(&slices, &settings(&[("support_type", "tree(auto)"), ("support_on_build_plate_only", "1")]), &[], &[]);
        assert!(total(&trees.tree) < total(&run(&slices, &settings(&[("support_type", "tree(auto)")]), &[], &[]).tree));
    }

    #[test]
    fn bottom_interfaces_where_support_stands_on_the_part() {
        let slices: Vec<Vec<Shape>> = (0..40)
            .map(|i| {
                if i < 10 {
                    vec![square(0.0, 0.0, 20.0)]
                } else if i < 30 {
                    vec![square(0.0, 0.0, 4.0)]
                } else {
                    vec![square(0.0, 0.0, 15.0)]
                }
            })
            .collect();
        let none = run(&slices, &settings(&[]), &[], &[]);
        assert!(total(&none.bottom) == 0.0);
        let two = run(&slices, &settings(&[("support_interface_bottom_layers", "2")]), &[], &[]);
        assert!(!two.bottom.get(11).is_none_or(Vec::is_empty) && !two.bottom.get(12).is_none_or(Vec::is_empty));
        assert!(two.bottom.get(13).is_none_or(Vec::is_empty));
    }

    #[test]
    fn enforcers_and_blockers() {
        let slices = t_shape();
        let n = slices.len();
        let everywhere: Vec<Vec<Shape>> = vec![vec![square(-5.0, -5.0, 30.0)]; n];
        let blocked = run(&slices, &settings(&[]), &[], &everywhere);
        assert!(total(&blocked.base) + total(&blocked.interface) < 1e-6);
        // Manual: nothing without an enforcer, support where one is.
        let manual = settings(&[("support_type", "normal(manual)")]);
        assert!(total(&run(&slices, &manual, &[], &[]).base) < 1e-6);
        let half: Vec<Vec<Shape>> = vec![vec![square(-5.0, -5.0, 12.0)]; n];
        let enforced = run(&slices, &manual, &half, &[]);
        assert!(total(&enforced.base) > 0.0);
        assert!(total(&enforced.base) < total(&run(&slices, &settings(&[]), &[], &[]).base));
    }

    #[test]
    fn every_pattern_draws_lines() {
        let slices = t_shape();
        for base in ["rectilinear", "rectilinear-grid", "honeycomb", "lightning", "hollow"] {
            for iface in ["rectilinear", "concentric", "spiralinset", "rectilinear_interlaced", "grid"] {
                let s = settings(&[("support_base_pattern", base), ("support_interface_pattern", iface)]);
                let sl = run(&slices, &s, &[], &[]);
                let p = paths(&sl, 10, &s, false, 2.2);
                assert!(p.iter().any(|p| p.role == Role::Support), "{base}");
                let top = paths(&sl, 28, &s, false, 5.8);
                assert!(top.iter().any(|p| p.role == Role::SupportInterface), "{iface}");
            }
        }
    }

    #[test]
    fn a_raft_grows_past_the_part() {
        let s = settings(&[("raft_layers", "4")]);
        let r = raft(&[square(0.0, 0.0, 10.0)], &s);
        assert_eq!(r.len(), 4);
        assert!(r.iter().filter(|l| l.interface).count() == 3);
        assert!(geom::area(&r[0].area) > geom::area(&r[1].area) && geom::area(&r[1].area) > 100.0);
        assert!(r.iter().enumerate().all(|(k, l)| !raft_paths(l, k, &s, 0.2).is_empty()));
    }
}
