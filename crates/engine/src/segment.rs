//! Colour painting sliced into filaments (OrcaSlicer's MMU segmentation): each layer's shape
//! split among the colours painted on its surface.
//!
//! The painted faces lie over each layer as bands along its walls. With a region width, each
//! colour reaches that far in from its band; without one, every colour (and the unpainted
//! surface, which keeps the object's filament) grows inwards a step at a time until they meet,
//! so each point inside takes the colour of the nearest painted surface.

use crate::geom::{self, Join, Op, Shape};

/// Most growth steps; a shape wider than this many steps stays partly the object's filament.
const MAX_STEPS: usize = 400;

/// Splits `slice` among `bands` (filament, painted band on this layer); returns each
/// filament's region. What no colour reaches keeps the object's filament (not returned).
pub fn split(slice: &[Shape], bands: &[(usize, Vec<Shape>)], width: f64, step: f64) -> Vec<(usize, Vec<Shape>)> {
    if slice.is_empty() || bands.iter().all(|(_, b)| b.is_empty()) {
        return Vec::new();
    }
    let step = step.max(0.1);
    // Seeds: each band within the shape, earlier bands winning where they overlap.
    let mut taken: Vec<Shape> = Vec::new();
    let mut owned: Vec<(usize, Vec<Shape>)> = Vec::new();
    for (f, band) in bands.iter().filter(|(_, b)| !b.is_empty()) {
        let mut seed = geom::boolean(band, slice, Op::Intersect);
        if !taken.is_empty() {
            seed = geom::boolean(&seed, &taken, Op::Difference);
        }
        if seed.is_empty() {
            continue;
        }
        taken = geom::boolean(&taken, &seed, Op::Union);
        owned.push((*f, seed));
    }
    if owned.is_empty() {
        return Vec::new();
    }
    if width > 0.0 {
        // Each colour a fixed depth in from its band.
        let mut used: Vec<Shape> = Vec::new();
        return owned
            .into_iter()
            .filter_map(|(f, seed)| {
                let mut region = geom::boolean(&geom::offset(&seed, width, Join::Round), slice, Op::Intersect);
                if !used.is_empty() {
                    region = geom::boolean(&region, &used, Op::Difference);
                }
                used = geom::boolean(&used, &region, Op::Union);
                (!region.is_empty()).then_some((f, region))
            })
            .collect();
    }
    // The unpainted surface grows too, keeping the object's colour where it is nearest.
    let rim = geom::boolean(slice, &geom::offset(slice, -step, Join::Round), Op::Difference);
    let plain = geom::boolean(&rim, &taken, Op::Difference);
    let plain_key = usize::MAX;
    if !plain.is_empty() {
        owned.push((plain_key, plain.clone()));
        taken = geom::boolean(&taken, &plain, Op::Union);
    }
    let mut free = geom::boolean(slice, &taken, Op::Difference);
    for _ in 0..MAX_STEPS {
        if free.is_empty() {
            break;
        }
        let mut grew = false;
        for (_, region) in owned.iter_mut() {
            let grown = geom::boolean(&geom::offset(region, step, Join::Round), &free, Op::Intersect);
            if grown.is_empty() {
                continue;
            }
            *region = geom::boolean(region, &grown, Op::Union);
            free = geom::boolean(&free, &grown, Op::Difference);
            grew = true;
        }
        if !grew {
            break;
        }
    }
    owned.into_iter().filter(|(f, r)| *f != plain_key && !r.is_empty()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::DVec2;

    #[test]
    fn colours_fill_from_their_side() {
        let square = vec![geom::rectangle(DVec2::ZERO, DVec2::splat(20.0))];
        // Red painted down the left wall.
        let red = vec![geom::rectangle(DVec2::new(-0.5, 0.0), DVec2::new(0.5, 20.0))];
        let out = split(&square, &[(1, red.clone())], 0.0, 0.5);
        assert_eq!(out.len(), 1);
        let a = geom::area(&out[0].1);
        // Nearest to the left wall: the band from x 0 to about 5 (the other walls are nearer
        // beyond the diagonals), not the whole square.
        assert!(a > 40.0 && a < 160.0, "{a}");
        // With a width, exactly that deep.
        let out = split(&square, &[(1, red)], 3.0, 0.5);
        let a = geom::area(&out[0].1);
        assert!((a - 70.0).abs() < 2.0, "{a}");
        assert!(split(&square, &[], 0.0, 0.5).is_empty());
    }
}
