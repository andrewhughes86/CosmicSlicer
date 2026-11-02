//! Arranging objects on plates as OrcaSlicer does: each object's footprint (the convex hull of
//! its shadow on the plate, grown by half the spacing) placed largest first, each where it
//! touches what is already there so the pile stays tight and near the plate's centre (no-fit
//! polygons), turned if that packs better; what does not fit goes on to the next plate.

use glam::Vec2;

/// A closed outline, anticlockwise.
pub type Polygon = Vec<Vec2>;

/// Directions a footprint is measured in: its outline has at most this many corners.
const DIRECTIONS: usize = 32;
/// Most candidate places tried for one item on one plate at one angle.
const MAX_CANDIDATES: usize = 20_000;
/// Most items arranged in one go; the rest are left where they are.
pub const MAX_ITEMS: usize = 2_000;

/// One plate to arrange on, in its own coordinates.
#[derive(Clone, Debug, Default)]
pub struct Plate {
    /// The printable rectangle.
    pub min: Vec2,
    pub max: Vec2,
    /// What items must keep clear of: excluded areas, the prime tower, objects not arranged.
    pub obstacles: Vec<Polygon>,
}

#[derive(Clone, Copy, Debug)]
pub struct Options {
    /// Distance kept between items (and from obstacles).
    pub spacing: f32,
    /// Try turning items about Z.
    pub rotate: bool,
}

/// Where an item went: plate index (past the given plates, a new plate like the template),
/// the turn about its reference point (radians), and where the reference point now is.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Placement {
    pub plate: usize,
    pub angle: f32,
    pub at: Vec2,
}

/// The convex hull of `points`, anticlockwise; non-finite points are ignored.
pub fn hull(points: &[Vec2]) -> Polygon {
    let mut p: Vec<Vec2> = points.iter().copied().filter(|v| v.is_finite()).collect();
    p.sort_by(|a, b| a.x.total_cmp(&b.x).then(a.y.total_cmp(&b.y)));
    p.dedup_by(|a, b| (*a - *b).length_squared() < 1e-12);
    if p.len() < 3 {
        return p;
    }
    let cross = |o: Vec2, a: Vec2, b: Vec2| (a - o).perp_dot(b - o);
    let mut lower: Vec<Vec2> = Vec::new();
    for &v in &p {
        while let [.., a, b] = lower.as_slice()
            && cross(*a, *b, v) <= 0.0
        {
            lower.pop();
        }
        lower.push(v);
    }
    let mut upper: Vec<Vec2> = Vec::new();
    for &v in p.iter().rev() {
        while let [.., a, b] = upper.as_slice()
            && cross(*a, *b, v) <= 0.0
        {
            upper.pop();
        }
        upper.push(v);
    }
    lower.pop();
    upper.pop();
    lower.extend(upper);
    lower
}

/// A convex outline round `hull` grown by `grow`, with at most `DIRECTIONS` corners and never
/// inside the true grown hull (each side is a supporting line).
pub fn outline(hull: &[Vec2], grow: f32) -> Polygon {
    if hull.is_empty() {
        return Vec::new();
    }
    let grow = if grow.is_finite() { grow.max(0.0) } else { 0.0 };
    let dirs: Vec<Vec2> =
        (0..DIRECTIONS).map(|i| Vec2::from_angle(i as f32 * std::f32::consts::TAU / DIRECTIONS as f32)).collect();
    let reach: Vec<f32> =
        dirs.iter().map(|d| hull.iter().map(|p| p.dot(*d)).fold(f32::NEG_INFINITY, f32::max) + grow).collect();
    let mut out = Vec::with_capacity(DIRECTIONS);
    for i in 0..DIRECTIONS {
        let j = (i + 1) % DIRECTIONS;
        let (Some(&a), Some(&b), Some(&ha), Some(&hb)) = (dirs.get(i), dirs.get(j), reach.get(i), reach.get(j)) else {
            continue;
        };
        // a·x = ha, b·x = hb.
        let det = a.perp_dot(b);
        if det.abs() < 1e-9 {
            continue;
        }
        let x = Vec2::new(ha * b.y - hb * a.y, a.x * hb - b.x * ha) / det;
        if x.is_finite() && out.last().is_none_or(|l: &Vec2| (*l - x).length_squared() > 1e-10) {
            out.push(x);
        }
    }
    out
}

fn bounds(p: &[Vec2]) -> (Vec2, Vec2) {
    p.iter().fold((Vec2::splat(f32::INFINITY), Vec2::splat(f32::NEG_INFINITY)), |(lo, hi), v| (lo.min(*v), hi.max(*v)))
}

fn area(p: &[Vec2]) -> f32 {
    let n = p.len();
    (0..n).filter_map(|i| Some(p.get(i)?.perp_dot(*p.get((i + 1) % n)?))).sum::<f32>() / 2.0
}

fn turned(p: &[Vec2], angle: f32) -> Polygon {
    let r = Vec2::from_angle(angle);
    p.iter().map(|v| r.rotate(*v)).collect()
}

/// Positions `t` where `b + t` overlaps `a`: the hull of every difference (both convex).
fn no_fit(a: &[Vec2], b: &[Vec2]) -> Polygon {
    let sums: Vec<Vec2> = a.iter().flat_map(|p| b.iter().map(move |q| *p - *q)).collect();
    hull(&sums)
}

/// Whether `t` is strictly inside the convex anticlockwise `p` (touching is allowed).
fn strictly_inside(p: &[Vec2], t: Vec2) -> bool {
    let n = p.len();
    n >= 3
        && (0..n).all(|i| match (p.get(i), p.get((i + 1) % n)) {
            (Some(a), Some(b)) => {
                let e = *b - *a;
                e.perp_dot(t - *a) > 1e-3 * e.length().max(1e-6)
            }
            _ => false,
        })
}

fn segment_hit(a: Vec2, b: Vec2, c: Vec2, d: Vec2) -> Option<Vec2> {
    let (r, s) = (b - a, d - c);
    let den = r.perp_dot(s);
    if den.abs() < 1e-9 {
        return None;
    }
    let t = (c - a).perp_dot(s) / den;
    let u = (c - a).perp_dot(r) / den;
    ((-1e-6..=1.0 + 1e-6).contains(&t) && (-1e-6..=1.0 + 1e-6).contains(&u)).then(|| a + r * t)
}

/// A placed item's outline on its plate.
struct Placed {
    outline: Polygon,
}

/// The angles worth trying for `hull`: square to the plate, and square to its own smallest
/// enclosing rectangle.
fn angles(hull: &[Vec2], rotate: bool) -> Vec<f32> {
    let mut out = vec![0.0];
    if !rotate {
        return out;
    }
    out.push(std::f32::consts::FRAC_PI_2);
    let n = hull.len();
    let best = (0..n)
        .filter_map(|i| {
            let e = *hull.get((i + 1) % n)? - *hull.get(i)?;
            let a = -e.y.atan2(e.x);
            let (lo, hi) = bounds(&turned(hull, a));
            let s = hi - lo;
            (s.is_finite() && e.length() > 1e-6).then_some((a, s.x * s.y))
        })
        .min_by(|x, y| x.1.total_cmp(&y.1));
    if let Some((a, _)) = best {
        for a in [a, a + std::f32::consts::FRAC_PI_2] {
            let a = a.rem_euclid(std::f32::consts::TAU);
            let near = |b: &f32| {
                let d = (a - *b).rem_euclid(std::f32::consts::TAU);
                d.min(std::f32::consts::TAU - d) < 1f32.to_radians()
            };
            if !out.iter().any(near) {
                out.push(a);
            }
        }
    }
    out
}

/// The best place for `outline` on a plate with `placed` items and `obstacles`, and its score
/// (lower is better), if it fits.
fn best_place(outline: &[Vec2], plate: &Plate, placed: &[Placed], from_corner: bool) -> Option<(Vec2, f32)> {
    let (blo, bhi) = bounds(outline);
    let (lo, hi) = (plate.min - blo, plate.max - bhi);
    if !(lo.is_finite() && hi.is_finite()) || lo.x > hi.x + 1e-4 || lo.y > hi.y + 1e-4 {
        return None;
    }
    let hi = hi.max(lo);
    let fences: Vec<(Polygon, Vec2, Vec2)> = placed
        .iter()
        .map(|p| &p.outline)
        .chain(plate.obstacles.iter())
        .map(|a| no_fit(a, outline))
        .filter(|n| n.len() >= 3)
        .map(|n| {
            let (a, b) = bounds(&n);
            (n, a, b)
        })
        .collect();
    // Where the pile gathers: the plate's centre, or its front left corner.
    let centre = if from_corner { plate.min } else { (plate.min + plate.max) / 2.0 };
    let mid = (blo + bhi) / 2.0;
    let ideal = (centre - mid).clamp(lo, hi);
    let rect = [lo, Vec2::new(hi.x, lo.y), hi, Vec2::new(lo.x, hi.y)];
    let mut candidates: Vec<Vec2> = vec![ideal];
    candidates.extend(rect);
    let edges = |p: &Polygon| {
        let n = p.len();
        (0..n).filter_map(move |i| Some((*p.get(i)?, *p.get((i + 1) % n)?))).collect::<Vec<_>>()
    };
    let rect_edges = edges(&rect.to_vec());
    for (k, (n, nlo, nhi)) in fences.iter().enumerate() {
        candidates.extend(n.iter().copied());
        let ne = edges(n);
        for &(a, b) in &ne {
            for &(c, d) in &rect_edges {
                candidates.extend(segment_hit(a, b, c, d));
            }
        }
        // Where two fences cross: tight spots between two neighbours.
        for (m, mlo, mhi) in fences.iter().skip(k + 1) {
            if candidates.len() > MAX_CANDIDATES {
                break;
            }
            if nlo.x > mhi.x || mlo.x > nhi.x || nlo.y > mhi.y || mlo.y > nhi.y {
                continue;
            }
            let me = edges(m);
            for &(a, b) in &ne {
                for &(c, d) in &me {
                    candidates.extend(segment_hit(a, b, c, d));
                }
            }
        }
        if candidates.len() > MAX_CANDIDATES {
            break;
        }
    }
    let pile = placed.iter().fold((Vec2::splat(f32::INFINITY), Vec2::splat(f32::NEG_INFINITY)), |(a, b), p| {
        let (l, h) = bounds(&p.outline);
        (a.min(l), b.max(h))
    });
    let edges_of: Vec<(Vec2, Vec2)> = placed.iter().map(|p| bounds(&p.outline)).collect();
    let plate_size = (plate.max - plate.min).max(Vec2::splat(1.0));
    let (plate_area, diagonal) = (plate_size.x * plate_size.y, plate_size.length());
    let mut best: Option<(Vec2, f32)> = None;
    for t in candidates {
        if !t.is_finite() || t.x < lo.x - 1e-3 || t.x > hi.x + 1e-3 || t.y < lo.y - 1e-3 || t.y > hi.y + 1e-3 {
            continue;
        }
        let t = t.clamp(lo, hi);
        let blocked =
            fences.iter().any(|(n, a, b)| t.x > a.x && t.x < b.x && t.y > a.y && t.y < b.y && strictly_inside(n, t));
        if blocked {
            continue;
        }
        // A tight pile, near the plate's centre.
        let (plo, phi) = (pile.0.min(blo + t), pile.1.max(bhi + t));
        let s = phi - plo;
        // Ties go to places lined up with a neighbour's sides, which leave room in rows.
        let (ilo, ihi) = (blo + t, bhi + t);
        let near = |a: f32, b: f32| (a - b).abs() < 0.05;
        let lined = edges_of
            .iter()
            .filter(|(l, h)| near(ilo.x, l.x) || near(ihi.x, h.x) || near(ilo.y, l.y) || near(ihi.y, h.y))
            .count();
        let score = s.x * s.y / plate_area + ((plo + phi) / 2.0 - centre).length() / diagonal
            - 1e-3 * lined as f32 / (edges_of.len().max(1) as f32);
        if best.is_none_or(|(_, b)| score < b) {
            best = Some((t, score));
        }
    }
    best
}

/// Arranges `items` (each a set of footprint points round its reference point) on `plates`,
/// then on new plates like `template` up to `max_plates` in all. An item too large for any
/// plate gets `None`.
///
/// Items gather at the centre of each plate; when large items leave gaps that way, gathering
/// in a corner and centring the pile afterwards may need fewer plates, and is taken instead.
pub fn arrange(
    items: &[Vec<Vec2>],
    plates: &[Plate],
    template: &Plate,
    max_plates: usize,
    opts: Options,
) -> Vec<Option<Placement>> {
    let centred = pack(items, plates, template, max_plates, opts, false);
    // Better: more placed, then fewer plates, then the earlier plates fuller.
    let used = |r: &[Option<Placement>]| {
        let placed = r.iter().filter(|p| p.is_some()).count();
        let plates = r.iter().flatten().map(|p| p.plate + 1).max().unwrap_or(0);
        let mut counts = vec![0usize; plates];
        for p in r.iter().flatten() {
            if let Some(c) = counts.get_mut(p.plate) {
                *c += 1;
            }
        }
        (std::cmp::Reverse(placed), plates, std::cmp::Reverse(counts))
    };
    let first = used(&centred);
    if first.1 <= 1 && first.0.0 == items.len().min(MAX_ITEMS) {
        return centred;
    }
    let cornered = pack(items, plates, template, max_plates, opts, true);
    if used(&cornered) < first { cornered } else { centred }
}

fn pack(
    items: &[Vec<Vec2>],
    plates: &[Plate],
    template: &Plate,
    max_plates: usize,
    opts: Options,
    from_corner: bool,
) -> Vec<Option<Placement>> {
    let spacing = if opts.spacing.is_finite() { opts.spacing.clamp(0.0, 100.0) } else { 6.0 };
    let hulls: Vec<Polygon> = items.iter().take(MAX_ITEMS).map(|p| hull(p)).collect();
    let mut order: Vec<usize> = (0..hulls.len()).collect();
    order.sort_by(|&a, &b| {
        let (aa, ab) = (hulls.get(a).map_or(0.0, |h| area(h)), hulls.get(b).map_or(0.0, |h| area(h)));
        ab.total_cmp(&aa)
    });
    let max_plates = max_plates.max(plates.len()).max(1);
    let mut beds: Vec<(Plate, Vec<Placed>)> = plates.iter().map(|p| (p.clone(), Vec::new())).collect();
    let mut out: Vec<Option<Placement>> = vec![None; items.len()];
    for i in order {
        let Some(h) = hulls.get(i) else { continue };
        if h.is_empty() {
            continue;
        }
        let tries: Vec<(f32, Polygon)> =
            angles(h, opts.rotate).into_iter().map(|a| (a, outline(&turned(h, a), spacing / 2.0))).collect();
        let mut p = 0;
        while p < max_plates {
            if p >= beds.len() {
                beds.push((template.clone(), Vec::new()));
            }
            let Some((plate, placed)) = beds.get(p) else { break };
            let best = tries
                .iter()
                .filter_map(|(a, o)| best_place(o, plate, placed, from_corner).map(|(t, s)| (*a, o, t, s)))
                .min_by(|x, y| x.3.total_cmp(&y.3));
            if let Some((angle, o, at, _)) = best {
                let outline: Polygon = o.iter().map(|v| *v + at).collect();
                if let Some((_, placed)) = beds.get_mut(p) {
                    placed.push(Placed { outline });
                }
                if let Some(slot) = out.get_mut(i) {
                    *slot = Some(Placement { plate: p, angle, at });
                }
                break;
            }
            // Too large even for an empty plate: no plate will take it.
            if beds.get(p).is_some_and(|(pl, placed)| placed.is_empty() && pl.obstacles.is_empty()) {
                break;
            }
            p += 1;
        }
    }
    // Each plate's pile centred, where that keeps clear of the plate's obstacles.
    for (p, (plate, placed)) in beds.iter().enumerate() {
        if placed.is_empty() {
            continue;
        }
        let pile = placed.iter().fold((Vec2::splat(f32::INFINITY), Vec2::splat(f32::NEG_INFINITY)), |(a, b), q| {
            let (l, h) = bounds(&q.outline);
            (a.min(l), b.max(h))
        });
        let shift = (plate.min + plate.max) / 2.0 - (pile.0 + pile.1) / 2.0;
        let inside = pile.0 + shift - plate.min;
        let clear = inside.min_element() >= -1e-3
            && (plate.max - (pile.1 + shift)).min_element() >= -1e-3
            && placed.iter().all(|q| {
                let moved: Polygon = q.outline.iter().map(|v| *v + shift).collect();
                plate.obstacles.iter().all(|o| !overlap(o, &moved))
            });
        if clear && shift.is_finite() {
            for slot in out.iter_mut().flatten().filter(|s| s.plate == p) {
                slot.at += shift;
            }
        }
    }
    out
}

/// Whether two convex outlines overlap (more than touching).
pub fn overlap(a: &[Vec2], b: &[Vec2]) -> bool {
    let n = no_fit(a, b);
    strictly_inside(&n, Vec2::ZERO)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(w: f32, h: f32) -> Vec<Vec2> {
        vec![
            Vec2::new(-w / 2.0, -h / 2.0),
            Vec2::new(w / 2.0, -h / 2.0),
            Vec2::new(w / 2.0, h / 2.0),
            Vec2::new(-w / 2.0, h / 2.0),
        ]
    }

    fn placed(item: &[Vec2], p: &Placement) -> Polygon {
        hull(&turned(item, p.angle).iter().map(|v| *v + p.at).collect::<Vec<_>>())
    }

    #[test]
    fn outlines_contain_their_hull() {
        let h = hull(&rect(20.0, 10.0));
        assert_eq!(h.len(), 4);
        assert!(area(&h) > 0.0);
        let o = outline(&h, 3.0);
        assert!(o.len() <= DIRECTIONS);
        for v in &h {
            assert!(!strictly_inside(&h, *v + Vec2::splat(1e-3)) || strictly_inside(&o, *v));
        }
        assert!(area(&o) >= area(&h));
        assert!(hull(&[Vec2::NAN, Vec2::ONE]).len() <= 1);
    }

    #[test]
    fn thirty_mixed_parts_pack_without_overlapping() {
        let plate = Plate { min: Vec2::ZERO, max: Vec2::splat(256.0), obstacles: Vec::new() };
        let mut items = Vec::new();
        for k in 0..30 {
            let (w, h) = (10.0 + (k * 7 % 40) as f32, 8.0 + (k * 13 % 30) as f32);
            if k % 3 == 0 {
                // A rough disc.
                items.push(
                    (0..12).map(|i| Vec2::from_angle(i as f32 * std::f32::consts::FRAC_PI_6) * w / 2.0).collect(),
                );
            } else if k % 3 == 1 {
                items.push(turned(&rect(w, h), 0.3 * k as f32));
            } else {
                items.push(vec![Vec2::ZERO, Vec2::new(w, 0.0), Vec2::new(0.0, h)]);
            }
        }
        let spacing = 6.0;
        let out = arrange(&items, std::slice::from_ref(&plate), &plate, 10, Options { spacing, rotate: true });
        let shapes: Vec<(usize, Polygon)> =
            items.iter().zip(&out).filter_map(|(item, p)| p.as_ref().map(|p| (p.plate, placed(item, p)))).collect();
        assert_eq!(shapes.len(), 30);
        // They all fit on one plate.
        assert!(shapes.iter().all(|(p, _)| *p == 0), "{:?}", shapes.iter().map(|s| s.0).collect::<Vec<_>>());
        for (i, (pa, a)) in shapes.iter().enumerate() {
            let (lo, hi) = bounds(a);
            assert!(lo.min_element() >= -1e-2 && hi.max_element() <= 256.0 + 1e-2, "{lo} {hi}");
            for (pb, b) in shapes.iter().skip(i + 1) {
                if pa == pb {
                    // Apart by the spacing, less a little for the outline's corners.
                    let grown = outline(a, spacing * 0.45);
                    assert!(!overlap(&grown, b), "items overlap");
                }
            }
        }
        // Packed tight round the centre, not spread over the plate.
        let all: Vec<Vec2> = shapes.iter().flat_map(|s| s.1.clone()).collect();
        let (lo, hi) = bounds(&all);
        let c = (lo + hi) / 2.0;
        assert!((c - Vec2::splat(128.0)).length() < 5.0, "{c}");
    }

    #[test]
    fn overflow_goes_to_new_plates_and_obstacles_are_kept_clear() {
        let plate = Plate {
            min: Vec2::ZERO,
            max: Vec2::splat(100.0),
            obstacles: vec![vec![
                Vec2::new(0.0, 0.0),
                Vec2::new(30.0, 0.0),
                Vec2::new(30.0, 30.0),
                Vec2::new(0.0, 30.0),
            ]],
        };
        let items: Vec<Vec<Vec2>> = (0..6).map(|_| rect(40.0, 40.0)).collect();
        let out = arrange(&items, std::slice::from_ref(&plate), &plate, 5, Options { spacing: 4.0, rotate: false });
        assert!(out.iter().all(Option::is_some));
        let on_first = out.iter().flatten().filter(|p| p.plate == 0).count();
        assert!((1..=3).contains(&on_first), "{on_first}");
        assert!(out.iter().flatten().any(|p| p.plate > 0));
        for (item, p) in items.iter().zip(out.iter().flatten()) {
            let o = placed(item, p);
            assert!(plate.obstacles.iter().all(|b| !overlap(b, &o)));
        }
        // Too large for any plate.
        let out = arrange(
            &[rect(500.0, 10.0)],
            std::slice::from_ref(&plate),
            &plate,
            5,
            Options { spacing: 4.0, rotate: true },
        );
        assert_eq!(out, vec![None]);
    }

    #[test]
    fn a_long_part_turns_to_fit() {
        // A narrow plate; the bar, lying askew, only fits turned square to its own sides.
        let plate = Plate { min: Vec2::ZERO, max: Vec2::new(200.0, 60.0), obstacles: Vec::new() };
        let bar = turned(&rect(150.0, 20.0), 0.7);
        let opts = |rotate| Options { spacing: 6.0, rotate };
        let out = arrange(std::slice::from_ref(&bar), std::slice::from_ref(&plate), &plate, 1, opts(true));
        assert!(out.first().is_some_and(Option::is_some), "{out:?}");
        let out = arrange(&[bar], std::slice::from_ref(&plate), &plate, 1, opts(false));
        assert_eq!(out, vec![None]);
    }

    #[test]
    fn nine_squares_fill_a_plate() {
        let plate = Plate { min: Vec2::ZERO, max: Vec2::splat(220.0), obstacles: Vec::new() };
        let sq = vec![Vec2::splat(-30.0), Vec2::new(30.0, -30.0), Vec2::splat(30.0), Vec2::new(-30.0, 30.0)];
        let items: Vec<Vec<Vec2>> = (0..12).map(|_| sq.clone()).collect();
        let out = arrange(&items, std::slice::from_ref(&plate), &plate, 5, Options { spacing: 6.0, rotate: false });
        let first = out.iter().flatten().filter(|p| p.plate == 0).count();
        assert_eq!(first, 9, "{out:?}");
    }
}
