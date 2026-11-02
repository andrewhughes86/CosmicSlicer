//! How long moves take: a motion planner like the firmware's (Marlin's and Klipper's), with
//! trapezoid speed profiles, look-ahead across corners, per-axis speed and acceleration limits,
//! and corner speeds from jerk or junction deviation.

use crate::settings::{Flavor, Limits};

/// What kind of move a block is, for its acceleration limit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlockKind {
    Extrude,
    Travel,
    /// Extruder only (retract, unretract).
    Retract,
}

/// One straight (or, for arcs, unrolled) move.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Block {
    /// X, Y, Z, E change.
    pub delta: [f64; 4],
    /// Path length for arcs (otherwise worked out from `delta`).
    pub length: Option<f64>,
    /// Requested speed, mm/s.
    pub feed: f64,
    /// The acceleration the G-code set for this kind of move, mm/s².
    pub accel: f64,
    pub kind: BlockKind,
    /// Corner speed the G-code set (jerk or Klipper's square corner velocity), mm/s; 0 for the
    /// machine's.
    pub jerk: f64,
}

/// Times (seconds) of each block. Takes the blocks as they come, so callers need not hold a
/// copy of every move (millions on a tall print).
pub fn plan(blocks: impl IntoIterator<Item = Block>, limits: &Limits, flavor: Flavor) -> Vec<f64> {
    struct P {
        len: f64,
        dir: [f64; 4],
        nominal: f64,
        accel: f64,
        max_entry: f64,
        jerk: f64,
    }
    let blocks = blocks.into_iter();
    let mut ps: Vec<P> = Vec::with_capacity(blocks.size_hint().0);
    for b in blocks {
        let d = b.delta;
        let xyz = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
        let len = b.length.unwrap_or(if xyz > 1e-9 { xyz } else { d[3].abs() }).max(0.0);
        if !(len.is_finite()) || len < 1e-9 {
            ps.push(P { len: 0.0, dir: [0.0; 4], nominal: 0.0, accel: 1.0, max_entry: 0.0, jerk: b.jerk });
            continue;
        }
        let dir = [d[0] / len, d[1] / len, d[2] / len, d[3] / len];
        let min_rate = if b.kind == BlockKind::Extrude { limits.min_extruding_rate } else { limits.min_travel_rate };
        let mut v = b.feed.max(min_rate).max(0.1);
        let kind_accel = match b.kind {
            BlockKind::Extrude => limits.accel_extruding,
            BlockKind::Travel => limits.accel_travel,
            BlockKind::Retract => limits.accel_retracting,
        };
        let mut a = if b.accel > 0.0 { b.accel.min(kind_accel) } else { kind_accel };
        // Each axis caps the speed and acceleration along the move.
        for ((d, max_v), max_a) in dir.iter().zip(limits.max_speed).zip(limits.max_accel) {
            let share = d.abs();
            if share > 1e-9 {
                v = v.min(max_v / share);
                a = a.min(max_a / share);
            }
        }
        ps.push(P { len, dir, nominal: v, accel: a.max(1.0), max_entry: 0.0, jerk: b.jerk });
    }
    let n = ps.len();
    // Corner speeds between consecutive moves.
    let mut prev: Option<usize> = None;
    for i in 0..n {
        if ps[i].len == 0.0 {
            continue;
        }
        let entry = match prev {
            None => 0.0,
            Some(j) => {
                let jerk = if ps[i].jerk > 0.0 { ps[i].jerk } else { 0.0 };
                junction(&ps[j].dir, &ps[i].dir, ps[j].nominal, ps[i].nominal, ps[i].accel, limits, flavor, jerk)
            }
        };
        ps[i].max_entry = entry.min(ps[i].nominal);
        prev = Some(i);
    }
    // Look-ahead: slow down early enough for what comes, speed up only as fast as allowed.
    let mut entry: Vec<f64> = ps.iter().map(|p| p.max_entry).collect();
    let mut exit = vec![0.0f64; n];
    let mut next_entry = 0.0;
    for i in (0..n).rev() {
        if ps[i].len == 0.0 {
            continue;
        }
        exit[i] = next_entry;
        entry[i] = entry[i].min((exit[i] * exit[i] + 2.0 * ps[i].accel * ps[i].len).sqrt());
        next_entry = entry[i];
    }
    let mut prev_exit = 0.0;
    let mut last: Option<usize> = None;
    for i in 0..n {
        if ps[i].len == 0.0 {
            continue;
        }
        if last.is_none() {
            entry[i] = 0.0;
        } else {
            entry[i] = entry[i].min(prev_exit);
        }
        let reach = (entry[i] * entry[i] + 2.0 * ps[i].accel * ps[i].len).sqrt();
        exit[i] = exit[i].min(reach);
        if let Some(j) = last {
            exit[j] = exit[j].min(entry[i]);
        }
        prev_exit = exit[i];
        last = Some(i);
    }
    (0..n)
        .map(|i| {
            let p = &ps[i];
            if p.len == 0.0 {
                return 0.0;
            }
            // A move too long or fast for f64 (damaged input) counts as no time, not infinity.
            Some(trapezoid(p.len, entry[i], exit[i], p.nominal, p.accel)).filter(|t| t.is_finite()).unwrap_or(0.0)
        })
        .collect()
}

/// Seconds to cover `len` starting at `ve`, ending at `vx`, cruising at most at `vn`.
fn trapezoid(len: f64, ve: f64, vx: f64, vn: f64, a: f64) -> f64 {
    let (ve, vx) = (ve.min(vn), vx.min(vn));
    let d_up = (vn * vn - ve * ve) / (2.0 * a);
    let d_down = (vn * vn - vx * vx) / (2.0 * a);
    if d_up + d_down <= len {
        (vn - ve) / a + (vn - vx) / a + (len - d_up - d_down) / vn
    } else {
        // Never reaches cruising speed.
        let peak = ((2.0 * a * len + ve * ve + vx * vx) / 2.0).sqrt().max(ve.max(vx));
        ((peak - ve) + (peak - vx)) / a
    }
}

/// The highest speed through the corner from direction `a` to `b`.
#[allow(clippy::too_many_arguments)]
fn junction(
    a: &[f64; 4],
    b: &[f64; 4],
    va: f64,
    vb: f64,
    accel: f64,
    limits: &Limits,
    flavor: Flavor,
    jerk: f64,
) -> f64 {
    let v_max = va.min(vb);
    let use_deviation = flavor == Flavor::Klipper || limits.junction_deviation > 0.0;
    if use_deviation {
        // Klipper: square corner velocity gives the deviation; Marlin 2: it is set directly.
        let deviation = if flavor == Flavor::Klipper {
            let scv = if jerk > 0.0 { jerk } else { 5.0 };
            scv * scv * (std::f64::consts::SQRT_2 - 1.0) / accel.max(1.0)
        } else {
            limits.junction_deviation
        };
        let cos_theta = -(a[0] * b[0] + a[1] * b[1] + a[2] * b[2]);
        if cos_theta < -0.999_999 {
            // Straight on.
            return v_max;
        }
        if cos_theta > 0.999_999 {
            // Reversal.
            return 0.0;
        }
        let sin_half = ((1.0 - cos_theta) / 2.0).max(0.0).sqrt();
        let v = (accel * deviation * sin_half / (1.0 - sin_half)).max(0.0).sqrt();
        return v.min(v_max);
    }
    // Classic jerk: each axis may change speed by at most its jerk at once.
    let mut v = v_max;
    for k in 0..4 {
        let j = if k < 2 && jerk > 0.0 { jerk } else { limits.jerk[k] };
        let change = (a[k] - b[k]).abs() * v;
        if change > j && change > 0.0 {
            v *= j / change;
        }
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    fn limits() -> Limits {
        Limits {
            max_speed: [500.0, 500.0, 20.0, 100.0],
            max_accel: [10_000.0, 10_000.0, 500.0, 5000.0],
            accel_extruding: 10_000.0,
            accel_retracting: 5000.0,
            accel_travel: 10_000.0,
            jerk: [10.0, 10.0, 0.4, 2.5],
            junction_deviation: 0.0,
            min_extruding_rate: 0.0,
            min_travel_rate: 0.0,
        }
    }

    fn line(dx: f64, dy: f64, feed: f64, accel: f64) -> Block {
        Block { delta: [dx, dy, 0.0, 0.0], length: None, feed, accel, kind: BlockKind::Extrude, jerk: 0.0 }
    }

    #[test]
    fn one_long_move_is_a_trapezoid() {
        // 100 mm at 100 mm/s with 1000 mm/s²: 0.1 s (5 mm) up, 0.1 s down, 90 mm cruising.
        let t = plan([line(100.0, 0.0, 100.0, 1000.0)], &limits(), Flavor::Marlin);
        assert!((t[0] - (0.1 + 0.1 + 0.9)).abs() < 1e-9, "{t:?}");
    }

    #[test]
    fn straight_runs_keep_their_speed() {
        // Ten 10 mm pieces in a line take as long as one 100 mm move.
        let pieces: Vec<Block> = (0..10).map(|_| line(10.0, 0.0, 100.0, 1000.0)).collect();
        let total: f64 = plan(pieces.iter().copied(), &limits(), Flavor::Marlin).iter().sum();
        assert!((total - 1.1).abs() < 1e-6, "{total}");
    }

    #[test]
    fn sharp_corners_cost_time() {
        let square: Vec<Block> =
            vec![line(50.0, 0.0, 200.0, 2000.0), line(0.0, 50.0, 200.0, 2000.0), line(-50.0, 0.0, 200.0, 2000.0)];
        let marlin: f64 = plan(square.iter().copied(), &limits(), Flavor::Marlin).iter().sum();
        let straight: f64 = plan([line(150.0, 0.0, 200.0, 2000.0)], &limits(), Flavor::Marlin).iter().sum();
        assert!(marlin > straight, "{marlin} vs {straight}");
        // Klipper's junction deviation behaves the same way.
        let klipper: f64 = plan(square.iter().copied(), &limits(), Flavor::Klipper).iter().sum();
        assert!(klipper > straight);
    }

    #[test]
    fn axis_limits_slow_z_and_e() {
        let z = Block {
            delta: [0.0, 0.0, 10.0, 0.0],
            length: None,
            feed: 100.0,
            accel: 1000.0,
            kind: BlockKind::Travel,
            jerk: 0.0,
        };
        let t = plan([z], &limits(), Flavor::Marlin)[0];
        // Capped at 20 mm/s by the Z axis: at least half a second.
        assert!(t >= 0.5, "{t}");
        let retract = Block {
            delta: [0.0, 0.0, 0.0, -0.8],
            length: None,
            feed: 40.0,
            accel: 0.0,
            kind: BlockKind::Retract,
            jerk: 0.0,
        };
        assert!(plan([retract], &limits(), Flavor::Marlin)[0] > 0.8 / 40.0);
    }

    #[test]
    fn nothing_moves_in_no_time() {
        let empty =
            Block { delta: [0.0; 4], length: None, feed: 100.0, accel: 1000.0, kind: BlockKind::Travel, jerk: 0.0 };
        assert_eq!(plan([empty], &limits(), Flavor::Marlin), vec![0.0]);
        assert!(plan(std::iter::empty(), &limits(), Flavor::Marlin).is_empty());
        let bad = Block { delta: [f64::NAN, 0.0, 0.0, 0.0], ..empty };
        assert!(plan([bad], &limits(), Flavor::Marlin)[0].is_finite());
    }
}
