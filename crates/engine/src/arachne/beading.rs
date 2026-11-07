//! Arachne's beading strategies: how many lines fit a feature of a given thickness, and how
//! wide each is. Orca stacks them as Limited(OuterWallInset?(Widening?(Redistribute(
//! Distributed)))); each layer copies the base fields of the one below, and a method a layer
//! does not override runs the base version on that layer's own copy, so the dispatch here is
//! spelled out per layer as Orca's C++ resolves it.

/// `BeadingStrategy::Beading`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Beading {
    pub total_thickness: i64,
    pub bead_widths: Vec<i64>,
    pub toolpath_locations: Vec<i64>,
    pub left_over: i64,
}

/// The fields `BeadingStrategy` holds, copied from layer to layer.
#[derive(Clone, Copy, Debug)]
struct Base {
    optimal_width: i64,
    split_middle: f64,
    add_middle: f64,
    transition_length: i64,
    angle: f64,
}

#[derive(Clone, Debug)]
enum Kind {
    Distributed { one_over_radius_sq: f32 },
    Redistribute { outer: i64, min_ratio: f64 },
    Widening { min_input: i64, min_output: i64 },
    OuterInset { offset: i64 },
    Limited { max_beads: i64 },
}

#[derive(Clone, Debug)]
pub struct Strategy {
    base: Base,
    kind: Kind,
    parent: Option<Box<Strategy>>,
}

/// What `BeadingStrategyFactory::makeStrategy` takes, in Orca's units.
#[derive(Clone, Copy, Debug)]
pub struct Make {
    pub outer: i64,
    pub inner: i64,
    pub transition_length: i64,
    pub angle: f32,
    pub thin_walls: bool,
    pub min_bead_width: i64,
    pub min_feature_size: i64,
    pub split_middle: f64,
    pub add_middle: f64,
    pub max_beads: i64,
    pub outer_offset: i64,
    pub distribution: i32,
}

/// `scaled<coord_t>(0.01)`.
const TEN_MICRONS: i64 = 10_000;

impl Strategy {
    /// `BeadingStrategyFactory::makeStrategy`, with Orca's minimum variable line ratio 0.5.
    pub fn make(m: Make) -> Strategy {
        let optimal = if m.max_beads <= 2 { m.outer } else { m.inner };
        let one_over_radius_sq = if m.distribution >= 2 {
            let r = (m.distribution - 1) as f32;
            1.0f32 / r * 1.0f32 / r
        } else {
            1.0
        };
        let base = Base {
            optimal_width: optimal,
            split_middle: m.split_middle,
            add_middle: m.add_middle,
            transition_length: m.transition_length,
            angle: f64::from(m.angle),
        };
        let mut s = Strategy { base, kind: Kind::Distributed { one_over_radius_sq }, parent: None };
        s = s.wrap(Kind::Redistribute { outer: m.outer, min_ratio: 0.5 });
        if m.thin_walls {
            s = s.wrap(Kind::Widening { min_input: m.min_feature_size, min_output: m.min_bead_width });
        }
        if m.outer_offset != 0 {
            s = s.wrap(Kind::OuterInset { offset: m.outer_offset });
        }
        s.wrap(Kind::Limited { max_beads: m.max_beads })
    }

    fn wrap(self, kind: Kind) -> Strategy {
        Strategy { base: self.base, kind, parent: Some(Box::new(self)) }
    }

    fn parent(&self) -> &Strategy {
        self.parent.as_deref().unwrap_or(self)
    }

    pub fn transitioning_angle(&self) -> f64 {
        self.base.angle
    }

    fn split_middle(&self) -> f64 {
        self.base.split_middle
    }

    // The base class's versions, run on this layer.

    fn base_optimal_thickness(&self, beads: i64) -> i64 {
        self.base.optimal_width.wrapping_mul(beads)
    }

    fn base_transition_thickness(&self, lower: i64) -> i64 {
        let lo = self.optimal_thickness(lower);
        let hi = self.optimal_thickness(lower + 1);
        let threshold = if lower % 2 == 1 { self.base.split_middle } else { self.base.add_middle };
        (lo as f64 + threshold * (hi - lo) as f64) as i64
    }

    fn base_anchor(&self, lower: i64) -> f32 {
        let lo = self.optimal_thickness(lower);
        let at = self.transition_thickness(lower);
        let hi = self.optimal_thickness(lower + 1);
        (1.0 - f64::from((at - lo) as f32 / (hi - lo) as f32)) as f32
    }

    fn base_transitioning_length(&self, lower: i64) -> i64 {
        if lower == 0 { TEN_MICRONS } else { self.base.transition_length }
    }

    pub fn optimal_thickness(&self, beads: i64) -> i64 {
        match self.kind {
            Kind::Distributed { .. } => self.base_optimal_thickness(beads),
            Kind::Redistribute { outer, .. } => {
                let inner = (beads - 2).max(0);
                self.parent().optimal_thickness(inner) + outer * (beads - inner)
            }
            Kind::Widening { .. } | Kind::OuterInset { .. } => self.parent().optimal_thickness(beads),
            Kind::Limited { max_beads } => {
                if beads <= max_beads {
                    self.parent().optimal_thickness(beads)
                } else {
                    1_000_000_000
                }
            }
        }
    }

    pub fn transition_thickness(&self, lower: i64) -> i64 {
        match self.kind {
            Kind::Distributed { .. } => self.base_transition_thickness(lower),
            Kind::Redistribute { outer, min_ratio } => match lower {
                0 => (min_ratio * outer as f64) as i64,
                1 => ((1.0 + self.parent().split_middle()) * outer as f64) as i64,
                _ => self.parent().transition_thickness(lower - 2) + 2 * outer,
            },
            Kind::Widening { min_input, .. } => {
                if lower == 0 {
                    min_input
                } else {
                    self.parent().transition_thickness(lower)
                }
            }
            Kind::OuterInset { .. } => self.parent().transition_thickness(lower),
            Kind::Limited { max_beads } => {
                if lower < max_beads {
                    self.parent().transition_thickness(lower)
                } else if lower == max_beads {
                    self.parent().optimal_thickness(lower + 1) - TEN_MICRONS
                } else {
                    900_000_000
                }
            }
        }
    }

    pub fn optimal_bead_count(&self, thickness: i64) -> i64 {
        match self.kind {
            Kind::Distributed { .. } => {
                let w = self.base.optimal_width;
                if w <= 0 {
                    return 0;
                }
                let naive = thickness / w;
                let remainder = thickness - naive * w;
                let threshold = if naive % 2 == 1 { self.base.split_middle } else { self.base.add_middle };
                let minimum = (w as f64 * threshold) as i64;
                naive + i64::from(remainder >= minimum)
            }
            Kind::Redistribute { outer, min_ratio } => {
                if (thickness as f64) < min_ratio * outer as f64 {
                    return 0;
                }
                if thickness <= 2 * outer {
                    return if thickness as f64 > (1.0 + self.parent().split_middle()) * outer as f64 { 2 } else { 1 };
                }
                self.parent().optimal_bead_count(thickness - 2 * outer) + 2
            }
            Kind::Widening { min_input, .. } => {
                if thickness < min_input {
                    return 0;
                }
                let n = self.parent().optimal_bead_count(thickness);
                if n < 1 { 1 } else { n }
            }
            Kind::OuterInset { .. } => self.parent().optimal_bead_count(thickness),
            Kind::Limited { max_beads } => {
                let n = self.parent().optimal_bead_count(thickness);
                if n <= max_beads {
                    n
                } else if n == max_beads + 1 {
                    if thickness < self.parent().optimal_thickness(max_beads + 1) - TEN_MICRONS {
                        max_beads
                    } else {
                        max_beads + 1
                    }
                } else {
                    max_beads + 1
                }
            }
        }
    }

    pub fn transitioning_length(&self, lower: i64) -> i64 {
        match self.kind {
            Kind::Distributed { .. } => self.base_transitioning_length(lower),
            _ => self.parent().transitioning_length(lower),
        }
    }

    pub fn transition_anchor_pos(&self, lower: i64) -> f32 {
        match self.kind {
            Kind::Distributed { .. } | Kind::OuterInset { .. } => self.base_anchor(lower),
            _ => self.parent().transition_anchor_pos(lower),
        }
    }

    /// The parent's lower bead count argument is unused all the way down: only Widening adds.
    pub fn nonlinear_thicknesses(&self) -> Vec<i64> {
        match self.kind {
            Kind::Widening { min_output, .. } => {
                let mut out = vec![min_output];
                out.extend(self.parent().nonlinear_thicknesses());
                out
            }
            _ => Vec::new(),
        }
    }

    pub fn compute(&self, thickness: i64, beads: i64) -> Beading {
        match self.kind {
            Kind::Distributed { one_over_radius_sq } => self.distributed(thickness, beads, one_over_radius_sq),
            Kind::Redistribute { outer, min_ratio } => {
                let mut ret = Beading::default();
                if beads == 0 || (thickness as f64) < min_ratio * outer as f64 {
                    ret.left_over = thickness;
                    ret.total_thickness = thickness;
                    return ret;
                }
                let (inner_beads, inner_thickness) = (beads - 2, thickness - 2 * outer);
                if inner_beads > 0 && inner_thickness > 0 {
                    ret = self.parent().compute(inner_thickness, inner_beads);
                    for t in &mut ret.toolpath_locations {
                        *t += outer;
                    }
                }
                let actual = if beads > 2 { (thickness / 2).min(outer) } else { thickness / beads };
                ret.bead_widths.insert(0, actual);
                ret.toolpath_locations.insert(0, actual / 2);
                if beads > 1 {
                    ret.bead_widths.push(actual);
                    ret.toolpath_locations.push(thickness - actual / 2);
                }
                ret.total_thickness = thickness;
                ret.left_over = thickness - ret.bead_widths.iter().sum::<i64>();
                ret
            }
            Kind::Widening { min_input, min_output } => {
                if beads <= 1 && thickness < self.transition_thickness(1) {
                    let mut ret = Beading { total_thickness: thickness, ..Beading::default() };
                    if thickness >= min_input {
                        ret.bead_widths.push(thickness.max(min_output));
                        ret.toolpath_locations.push(thickness / 2);
                    } else {
                        ret.left_over = thickness;
                    }
                    ret
                } else {
                    self.parent().compute(thickness, beads)
                }
            }
            Kind::OuterInset { offset } => {
                let mut ret = self.parent().compute(thickness, beads);
                if ret.bead_widths.iter().filter(|w| **w > 0).count() >= 2
                    && let Some(first) = ret.toolpath_locations.first_mut()
                {
                    *first = (*first + offset).min(thickness / 2);
                }
                ret
            }
            Kind::Limited { max_beads } => self.limited(thickness, beads, max_beads),
        }
    }

    fn distributed(&self, thickness: i64, beads: i64, one_over: f32) -> Beading {
        let mut ret = Beading { total_thickness: thickness, ..Beading::default() };
        let w = self.base.optimal_width;
        match beads {
            n if n > 2 => {
                let divided = thickness - n * w;
                let middle = (n - 1) as f32 / 2.0;
                let weights: Vec<f32> = (0..n)
                    .map(|i| {
                        let dev = i as f32 - middle;
                        (1.0f32 - one_over * dev * dev).max(0.0)
                    })
                    .collect();
                let total = weights.iter().fold(0.0f32, |a, b| a + b);
                let mut accumulated = 0i64;
                for (i, weight) in weights.iter().enumerate() {
                    let fraction = weight / total;
                    let share = (divided as f32 * fraction) as i64;
                    let width = if i as i64 == n - 1 { thickness - accumulated } else { w + share };
                    let at = match (ret.toolpath_locations.last(), ret.bead_widths.last()) {
                        (Some(&t), Some(&b)) => t + (b + width) / 2,
                        _ => width / 2,
                    };
                    ret.toolpath_locations.push(at);
                    ret.bead_widths.push(width);
                    accumulated += width;
                }
            }
            2 => {
                let outer = thickness / 2;
                ret.bead_widths.extend([outer, outer]);
                ret.toolpath_locations.extend([outer / 2, thickness - outer / 2]);
            }
            1 => {
                ret.bead_widths.push(thickness);
                ret.toolpath_locations.push(thickness / 2);
            }
            _ => ret.left_over = thickness,
        }
        ret
    }

    fn limited(&self, thickness: i64, beads: i64, max: i64) -> Beading {
        let half = (max / 2) as usize;
        let parent = self.parent();
        if beads <= max {
            let mut ret = parent.compute(thickness, beads);
            let n = ret.toolpath_locations.len() as i64;
            if n % 2 == 0
                && n == max
                && half >= 1
                && let (Some(&at), Some(&width)) = (ret.toolpath_locations.get(half - 1), ret.bead_widths.get(half - 1))
                && half <= ret.toolpath_locations.len()
                && half <= ret.bead_widths.len()
            {
                ret.toolpath_locations.insert(half, at + width / 2);
                ret.bead_widths.insert(half, 0);
            }
            return ret;
        }
        let optimal = parent.optimal_thickness(max);
        let mut ret = parent.compute(optimal, max);
        let n = ret.toolpath_locations.len();
        ret.left_over += thickness - ret.total_thickness;
        ret.total_thickness = thickness;
        if n % 2 == 1 {
            if let Some(t) = ret.toolpath_locations.get_mut(n / 2) {
                *t = thickness / 2;
            }
            if let Some(b) = ret.bead_widths.get_mut(n / 2) {
                *b = thickness - optimal;
            }
        }
        for i in 0..n.div_ceil(2) {
            let Some(&mirror) = ret.toolpath_locations.get(i) else { break };
            if let Some(t) = ret.toolpath_locations.get_mut(n - 1 - i) {
                *t = thickness - mirror;
            }
        }
        if half == 0 {
            return ret;
        }
        let (Some(&at), Some(&width)) = (ret.toolpath_locations.get(half - 1), ret.bead_widths.get(half - 1)) else {
            return ret;
        };
        if half > ret.toolpath_locations.len() || half > ret.bead_widths.len() {
            return ret;
        }
        ret.toolpath_locations.insert(half, at + width / 2);
        ret.bead_widths.insert(half, 0);
        let opposite = n.wrapping_sub(half - 1);
        let (Some(&at), Some(&width)) = (ret.toolpath_locations.get(opposite), ret.bead_widths.get(opposite)) else {
            return ret;
        };
        ret.toolpath_locations.insert(opposite, at - width / 2);
        ret.bead_widths.insert(opposite, 0);
        ret
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn concentric() -> Strategy {
        // FillConcentricInternal on a 0.4 mm nozzle, 0.42 mm lines.
        Strategy::make(Make {
            outer: 420_000,
            inner: 420_000,
            transition_length: 400_000,
            angle: 10f32.to_radians(),
            thin_walls: true,
            min_bead_width: 340_000,
            min_feature_size: 100_000,
            split_middle: 0.5,
            add_middle: 0.5,
            max_beads: 2 * 50,
            outer_offset: 0,
            distribution: 1,
        })
    }

    #[test]
    fn beads_fill_the_thickness() {
        let s = concentric();
        for thickness in [150_000, 420_000, 700_000, 840_000, 1_000_000, 2_100_000, 5_000_000] {
            let n = s.optimal_bead_count(thickness);
            let b = s.compute(thickness, n);
            // Features thinner than one bead widen to the minimum bead width.
            let sum = b.bead_widths.iter().sum::<i64>() + b.left_over;
            assert!(sum == thickness || (b.bead_widths.len() == 1 && sum == 340_000), "{thickness}: {b:?}");
            assert!(b.toolpath_locations.windows(2).all(|w| w[0] <= w[1]), "{b:?}");
        }
    }

    #[test]
    fn counts_grow_with_thickness() {
        let s = concentric();
        assert_eq!(s.optimal_bead_count(50_000), 0);
        assert_eq!(s.optimal_bead_count(420_000), 1);
        assert_eq!(s.optimal_bead_count(840_000), 2);
        assert_eq!(s.optimal_bead_count(1_260_000), 3);
    }

    #[test]
    fn garbage_does_not_panic() {
        let s = Strategy::make(Make {
            outer: 0,
            inner: 0,
            transition_length: 0,
            angle: f32::NAN,
            thin_walls: true,
            min_bead_width: 0,
            min_feature_size: 0,
            split_middle: 0.5,
            add_middle: 0.5,
            max_beads: 0,
            outer_offset: 5,
            distribution: 0,
        });
        for t in [-5, 0, 1, 1_000_000] {
            for n in 0..4 {
                let _ = s.compute(t, n);
                let _ = s.optimal_bead_count(t);
                let _ = s.transition_anchor_pos(n);
            }
        }
    }
}
