//! Non-uniform rational B-spline curves and surfaces, evaluated with de Boor's algorithm.
//!
//! Construction validates everything a file could get wrong (degree, counts, knot order,
//! finite values, positive weights), so evaluation never indexes out of range.

use glam::{DVec3, DVec4};

/// Highest degree accepted.
pub const MAX_DEGREE: usize = 25;
/// Most control points along one direction.
pub const MAX_POINTS: usize = 100_000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NurbsError(pub String);

impl std::fmt::Display for NurbsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for NurbsError {}

fn check_knots(degree: usize, count: usize, knots: &[f64]) -> Result<(), NurbsError> {
    if degree == 0 || degree > MAX_DEGREE {
        return Err(NurbsError(format!("B-spline degree {degree} is out of range")));
    }
    if count <= degree || count > MAX_POINTS {
        return Err(NurbsError(format!("{count} control points for degree {degree}")));
    }
    if knots.len() != count + degree + 1 {
        return Err(NurbsError(format!("{} knots for {count} control points of degree {degree}", knots.len())));
    }
    if knots.iter().any(|k| !k.is_finite()) || knots.windows(2).any(|w| w[1] < w[0]) {
        return Err(NurbsError("knots are not finite and non-decreasing".into()));
    }
    if knots.get(degree) >= knots.get(count) {
        return Err(NurbsError("the B-spline's parameter range is empty".into()));
    }
    Ok(())
}

fn homogeneous(points: &[DVec3], weights: Option<&[f64]>) -> Result<Vec<DVec4>, NurbsError> {
    if let Some(w) = weights
        && w.len() != points.len()
    {
        return Err(NurbsError(format!("{} weights for {} control points", w.len(), points.len())));
    }
    points
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let w = weights.and_then(|w| w.get(i).copied()).unwrap_or(1.0);
            if !p.is_finite() || !w.is_finite() || w <= 0.0 {
                return Err(NurbsError("control points must be finite with positive weights".into()));
            }
            Ok((*p * w).extend(w))
        })
        .collect()
}

/// The knot span holding `t` (clamped to the range): the `k` with `knots[k] <= t < knots[k+1]`.
fn span(degree: usize, count: usize, knots: &[f64], t: f64) -> (usize, f64) {
    let lo = knots.get(degree).copied().unwrap_or(0.0);
    let hi = knots.get(count).copied().unwrap_or(1.0);
    let t = if t.is_finite() { t.clamp(lo, hi) } else { lo };
    // The last span with knots[k] <= t, kept inside degree..count-1 and non-empty.
    let mut k = knots.get(degree..count).map_or(degree, |s| degree + s.partition_point(|&x| x <= t).saturating_sub(1));
    while k > degree && knots.get(k) >= knots.get(k + 1) {
        k -= 1;
    }
    (k.clamp(degree, count - 1), t)
}

/// de Boor's algorithm on `d` (degree + 1 points of span `k`), in place.
fn de_boor(degree: usize, knots: &[f64], k: usize, t: f64, d: &mut [DVec4]) -> DVec4 {
    for r in 1..=degree {
        for j in (r..=degree).rev() {
            let a = knots.get(j + k - degree).copied().unwrap_or(0.0);
            let b = knots.get(j + 1 + k - r).copied().unwrap_or(0.0);
            let alpha = if b > a { (t - a) / (b - a) } else { 0.0 };
            if let (Some(&prev), Some(cur)) = (d.get(j - 1), d.get(j)) {
                let next = prev * (1.0 - alpha) + *cur * alpha;
                if let Some(slot) = d.get_mut(j) {
                    *slot = next;
                }
            }
        }
    }
    d.get(degree).copied().unwrap_or(DVec4::ZERO)
}

fn project(h: DVec4) -> DVec3 {
    if h.w.abs() > 1.0e-300 { h.truncate() / h.w } else { h.truncate() }
}

#[derive(Clone, Debug)]
pub struct NurbsCurve {
    degree: usize,
    knots: Vec<f64>,
    points: Vec<DVec4>,
}

impl NurbsCurve {
    pub fn new(degree: usize, knots: Vec<f64>, points: &[DVec3], weights: Option<&[f64]>) -> Result<Self, NurbsError> {
        check_knots(degree, points.len(), &knots)?;
        Ok(Self { degree, knots, points: homogeneous(points, weights)? })
    }

    pub fn degree(&self) -> usize {
        self.degree
    }

    /// The parameter range.
    pub fn domain(&self) -> (f64, f64) {
        let n = self.points.len();
        (self.knots.get(self.degree).copied().unwrap_or(0.0), self.knots.get(n).copied().unwrap_or(1.0))
    }

    /// Distinct knot values inside the range, ends included.
    pub fn breaks(&self) -> Vec<f64> {
        let (lo, hi) = self.domain();
        let mut b: Vec<f64> = self.knots.iter().copied().filter(|k| *k >= lo && *k <= hi).collect();
        b.dedup();
        b
    }

    pub fn control_points(&self) -> impl Iterator<Item = DVec3> + '_ {
        self.points.iter().map(|h| project(*h))
    }

    /// The same curve with every control point moved by `f` (an affine map keeps it exact).
    pub fn map_points(&self, f: impl Fn(DVec3) -> DVec3) -> Self {
        let points = self.points.iter().map(|h| (f(project(*h)) * h.w).extend(h.w)).collect();
        Self { points, ..self.clone() }
    }

    pub fn point(&self, t: f64) -> DVec3 {
        let (k, t) = span(self.degree, self.points.len(), &self.knots, t);
        let mut d: Vec<DVec4> =
            (0..=self.degree).map(|j| self.points.get(j + k - self.degree).copied().unwrap_or(DVec4::ZERO)).collect();
        project(de_boor(self.degree, &self.knots, k, t, &mut d))
    }
}

#[derive(Clone, Debug)]
pub struct NurbsSurface {
    du: usize,
    dv: usize,
    ku: Vec<f64>,
    kv: Vec<f64>,
    nu: usize,
    nv: usize,
    /// Row-major, `points[i * nv + j]` with `i` along u.
    points: Vec<DVec4>,
}

impl NurbsSurface {
    /// `points[i][j]`, `i` along u; `weights` likewise.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        du: usize,
        dv: usize,
        ku: Vec<f64>,
        kv: Vec<f64>,
        points: &[Vec<DVec3>],
        weights: Option<&[Vec<f64>]>,
    ) -> Result<Self, NurbsError> {
        let nu = points.len();
        let nv = points.first().map_or(0, Vec::len);
        if points.iter().any(|row| row.len() != nv) {
            return Err(NurbsError("control point rows differ in length".into()));
        }
        if nu.saturating_mul(nv) > 4 * MAX_POINTS {
            return Err(NurbsError("too many control points".into()));
        }
        check_knots(du, nu, &ku)?;
        check_knots(dv, nv, &kv)?;
        let mut h = Vec::with_capacity(nu * nv);
        for (i, row) in points.iter().enumerate() {
            let w = match weights {
                Some(w) => Some(w.get(i).ok_or_else(|| NurbsError("missing weight rows".into()))?.as_slice()),
                None => None,
            };
            h.extend(homogeneous(row, w)?);
        }
        Ok(Self { du, dv, ku, kv, nu, nv, points: h })
    }

    pub fn degrees(&self) -> (usize, usize) {
        (self.du, self.dv)
    }

    pub fn domain(&self) -> ((f64, f64), (f64, f64)) {
        let d =
            |deg: usize, n: usize, k: &[f64]| (k.get(deg).copied().unwrap_or(0.0), k.get(n).copied().unwrap_or(1.0));
        (d(self.du, self.nu, &self.ku), d(self.dv, self.nv, &self.kv))
    }

    /// Distinct knot values inside the u and v ranges.
    pub fn breaks(&self) -> (Vec<f64>, Vec<f64>) {
        let ((u0, u1), (v0, v1)) = self.domain();
        let pick = |k: &[f64], lo: f64, hi: f64| {
            let mut b: Vec<f64> = k.iter().copied().filter(|x| *x >= lo && *x <= hi).collect();
            b.dedup();
            b
        };
        (pick(&self.ku, u0, u1), pick(&self.kv, v0, v1))
    }

    pub fn control_points(&self) -> impl Iterator<Item = DVec3> + '_ {
        self.points.iter().map(|h| project(*h))
    }

    /// The same surface with every control point moved by `f` (an affine map keeps it exact).
    pub fn map_points(&self, f: impl Fn(DVec3) -> DVec3) -> Self {
        let points = self.points.iter().map(|h| (f(project(*h)) * h.w).extend(h.w)).collect();
        Self { points, ..self.clone() }
    }

    pub fn point(&self, u: f64, v: f64) -> DVec3 {
        let (ku, u) = span(self.du, self.nu, &self.ku, u);
        let (kv, v) = span(self.dv, self.nv, &self.kv, v);
        let mut row = vec![DVec4::ZERO; self.dv + 1];
        let mut col = vec![DVec4::ZERO; self.du + 1];
        for (a, slot) in col.iter_mut().enumerate() {
            let i = a + ku - self.du;
            for (b, r) in row.iter_mut().enumerate() {
                *r = self.points.get(i * self.nv + b + kv - self.dv).copied().unwrap_or(DVec4::ZERO);
            }
            *slot = de_boor(self.dv, &self.kv, kv, v, &mut row);
        }
        project(de_boor(self.du, &self.ku, ku, u, &mut col))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_rational_quadratic_is_an_exact_circle_arc() -> Result<(), NurbsError> {
        let w = std::f64::consts::FRAC_1_SQRT_2;
        let c = NurbsCurve::new(
            2,
            vec![0.0, 0.0, 0.0, 1.0, 1.0, 1.0],
            &[DVec3::X, DVec3::new(1.0, 1.0, 0.0), DVec3::Y],
            Some(&[1.0, w, 1.0]),
        )?;
        for i in 0..=20 {
            let p = c.point(f64::from(i) / 20.0);
            assert!((p.length() - 1.0).abs() < 1e-12, "{p}");
        }
        assert!((c.point(0.0) - DVec3::X).length() < 1e-12 && (c.point(1.0) - DVec3::Y).length() < 1e-12);
        Ok(())
    }

    #[test]
    fn a_bilinear_patch_interpolates_its_corners() -> Result<(), NurbsError> {
        let s = NurbsSurface::new(
            1,
            1,
            vec![0.0, 0.0, 2.0, 2.0],
            vec![0.0, 0.0, 1.0, 1.0],
            &[vec![DVec3::ZERO, DVec3::Y], vec![DVec3::X, DVec3::new(1.0, 1.0, 1.0)]],
            None,
        )?;
        assert_eq!(s.domain(), ((0.0, 2.0), (0.0, 1.0)));
        assert!((s.point(2.0, 1.0) - DVec3::new(1.0, 1.0, 1.0)).length() < 1e-12);
        assert!((s.point(1.0, 0.5) - DVec3::new(0.5, 0.5, 0.25)).length() < 1e-12);
        Ok(())
    }

    #[test]
    fn bad_definitions_are_refused() {
        let p = [DVec3::ZERO, DVec3::X, DVec3::Y];
        assert!(NurbsCurve::new(0, vec![0.0, 1.0, 2.0], &p, None).is_err());
        assert!(NurbsCurve::new(2, vec![0.0, 0.0, 0.0, 1.0, 1.0], &p, None).is_err());
        assert!(NurbsCurve::new(2, vec![0.0, 0.0, 1.0, 0.5, 1.0, 1.0], &p, None).is_err());
        assert!(NurbsCurve::new(2, vec![0.0, 0.0, 0.0, 1.0, 1.0, 1.0], &p, Some(&[1.0, -1.0, 1.0])).is_err());
        assert!(NurbsCurve::new(2, vec![1.0; 6], &p, None).is_err());
        assert!(NurbsCurve::new(2, vec![0.0, 0.0, 0.0, f64::NAN, 1.0, 1.0], &p, None).is_err());
    }
}
