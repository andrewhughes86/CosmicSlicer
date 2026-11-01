//! Supports: columns under overhangs, filled with lines, with denser interface layers under
//! the part.
//!
//! An area needs support where a layer reaches out past the one below by more than the
//! overhang angle allows. Each such area is carried down layer by layer until it lands on the
//! plate or on the part, kept a gap away from the part sideways and underneath.

use std::f64::consts::FRAC_PI_2;

use crate::geom::{self, Join, Op, Shape};
use crate::infill;
use crate::settings::Settings;
use crate::toolpath::{Path, Role};

/// Support regions per layer: (base, interface).
pub struct SupportLayers {
    pub base: Vec<Vec<Shape>>,
    pub interface: Vec<Vec<Shape>>,
}

/// `slices[i]` is everything printed on layer `i` (all objects).
pub fn generate(slices: &[Vec<Shape>], s: &Settings, cancelled: &dyn Fn() -> bool) -> SupportLayers {
    let n = slices.len();
    let empty = || vec![Vec::new(); n];
    if !s.support.enabled || n < 2 {
        return SupportLayers { base: empty(), interface: empty() };
    }
    let w = s.widths.support;
    let h = s.layer_height;
    // Faces closer to horizontal than the threshold need support: the allowed reach per layer.
    let angle = s.support.threshold_angle.max(1.0).to_radians();
    let reach = (h / angle.tan()).clamp(w * 0.25, w * 4.0);
    // What each layer leaves unsupported.
    let need: Vec<Vec<Shape>> = (0..n)
        .map(|i| {
            if i == 0 || cancelled() {
                return Vec::new();
            }
            let (Some(here), Some(below)) = (slices.get(i), slices.get(i - 1)) else { return Vec::new() };
            let held = geom::offset(below, reach, Join::Round);
            let out = geom::boolean(here, &held, Op::Difference);
            // Tiny overhangs bridge on their own.
            geom::without_slivers(out, w * w * 6.0)
        })
        .collect();
    let gap_layers = (s.support.top_z_distance / h).round() as usize;
    let mut base = empty();
    let mut interface = empty();
    let mut carry: Vec<Shape> = Vec::new();
    // Interfaces: the layers just under each overhang.
    let mut recent: Vec<Vec<Shape>> = Vec::new();
    for i in (0..n.saturating_sub(1)).rev() {
        if cancelled() {
            break;
        }
        let above = i + 1 + gap_layers;
        let fresh = need.get(above).cloned().unwrap_or_default();
        if !fresh.is_empty() {
            carry = geom::boolean(&carry, &fresh, Op::Union);
        }
        recent.insert(0, fresh);
        recent.truncate(s.support.interface_layers.max(1));
        let Some(here) = slices.get(i) else { continue };
        // Support ends where it lands on the part (OrcaSlicer leaves a gap under it too).
        carry = geom::boolean(&carry, here, Op::Difference);
        if s.support.build_plate_only {
            // Columns that touched the part never reach the plate: drop what lies over it.
            let over_part = geom::boolean(&carry, &geom::offset(here, w, Join::Round), Op::Intersect);
            if !over_part.is_empty() {
                carry = geom::boolean(&carry, &over_part, Op::Difference);
            }
        }
        carry = geom::without_slivers(carry, w * w * 4.0);
        if carry.is_empty() {
            continue;
        }
        let clear = geom::offset(here, s.support.xy_distance + w / 2.0, Join::Round);
        let region = geom::boolean(&carry, &clear, Op::Difference);
        // Smooth the column outlines a little.
        let region = geom::offset(&geom::offset(&region, -w, Join::Round), w, Join::Round);
        let iface_area: Vec<Shape> = if s.support.interface_layers > 0 {
            let near: Vec<Shape> = recent.iter().flatten().cloned().collect();
            geom::boolean(&region, &near, Op::Intersect)
        } else {
            Vec::new()
        };
        let base_area =
            if iface_area.is_empty() { region } else { geom::boolean(&region, &iface_area, Op::Difference) };
        if let Some(b) = base.get_mut(i) {
            *b = geom::without_slivers(base_area, w * w * 2.0);
        }
        if let Some(f) = interface.get_mut(i) {
            *f = geom::without_slivers(iface_area, w * w * 2.0);
        }
    }
    SupportLayers { base, interface }
}

/// The support lines for one layer.
pub fn paths(sl: &SupportLayers, i: usize, s: &Settings, first_layer: bool, z: f64) -> Vec<Path> {
    let w = if first_layer { s.widths.first_layer } else { s.widths.support };
    let mut out = Vec::new();
    if let Some(base) = sl.base.get(i).filter(|b| !b.is_empty()) {
        // A loop round each column keeps the lines standing.
        for c in geom::offset(base, -w / 2.0, Join::Round).iter().flatten() {
            out.push(Path {
                role: Role::Support,
                points: c.clone(),
                closed: true,
                width: w,
                speed: if first_layer { s.speeds.first_layer } else { s.speeds.support },
                flow: 1.0,
            });
        }
        let inside = geom::offset(base, -w, Join::Round);
        let density = (w / s.support.spacing).clamp(0.05, 1.0);
        // On the plate, solid support sticks better.
        let density = if first_layer { (density * 2.0).min(1.0) } else { density };
        let kind = infill::Kind::Lines { dirs: 1, alternate: false, cubic: false };
        for l in infill::fill(&inside, kind, w, density, 0.0, i, z) {
            out.push(Path {
                role: Role::Support,
                points: l.points,
                closed: l.closed,
                width: w,
                speed: if first_layer { s.speeds.first_layer } else { s.speeds.support },
                flow: 1.0,
            });
        }
    }
    if let Some(iface) = sl.interface.get(i).filter(|f| !f.is_empty()) {
        let density = (w / (s.support.interface_spacing + w)).clamp(0.1, 1.0);
        let kind = infill::Kind::Lines { dirs: 1, alternate: false, cubic: false };
        for l in infill::fill(iface, kind, w, density, FRAC_PI_2, i, z) {
            out.push(Path {
                role: Role::SupportInterface,
                points: l.points,
                closed: l.closed,
                width: w,
                speed: s.speeds.support_interface,
                flow: 1.0,
            });
        }
    }
    out
}
