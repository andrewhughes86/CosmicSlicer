//! The orbit camera: it turns about a target point, Z up, like OrcaSlicer's.

use glam::{Mat4, Vec2, Vec3};

use CosmicSlicer_model::Aabb;

/// OrcaSlicer's view presets (keys 0–6).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum View {
    Iso,
    Top,
    Bottom,
    Front,
    Rear,
    Left,
    Right,
}

impl View {
    pub const ALL: [(View, &'static str, &'static str); 7] = [
        (View::Iso, "Default View", "0"),
        (View::Top, "Top View", "1"),
        (View::Bottom, "Bottom View", "2"),
        (View::Front, "Front View", "3"),
        (View::Rear, "Rear View", "4"),
        (View::Left, "Left View", "5"),
        (View::Right, "Right View", "6"),
    ];

    /// (yaw, pitch) in degrees.
    fn angles(self) -> (f32, f32) {
        match self {
            View::Iso => (-30.0, 38.0),
            View::Top => (0.0, 89.9),
            View::Bottom => (0.0, -89.9),
            View::Front => (0.0, 0.0),
            View::Rear => (180.0, 0.0),
            View::Left => (-90.0, 0.0),
            View::Right => (90.0, 0.0),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Camera {
    pub target: Vec3,
    pub distance: f32,
    /// Turn about Z in radians; 0 looks from the front (−Y) towards +Y.
    pub yaw: f32,
    /// Elevation above the XY plane in radians.
    pub pitch: f32,
    pub fov_y: f32,
    pub perspective: bool,
}

impl Default for Camera {
    fn default() -> Self {
        let mut c = Self {
            target: Vec3::new(110.0, 110.0, 0.0),
            distance: 500.0,
            yaw: 0.0,
            pitch: 0.0,
            fov_y: 35f32.to_radians(),
            perspective: true,
        };
        c.set_view(View::Iso);
        c
    }
}

const MIN_DISTANCE: f32 = 5.0;
const MAX_DISTANCE: f32 = 50_000.0;

impl Camera {
    pub fn set_view(&mut self, view: View) {
        let (yaw, pitch) = view.angles();
        self.yaw = yaw.to_radians();
        self.pitch = pitch.to_radians();
    }

    /// Unit vector from the target towards the eye.
    pub fn back(&self) -> Vec3 {
        Vec3::new(self.yaw.sin() * self.pitch.cos(), -self.yaw.cos() * self.pitch.cos(), self.pitch.sin())
    }

    pub fn eye(&self) -> Vec3 {
        self.target + self.back() * self.distance
    }

    pub fn view(&self) -> Mat4 {
        // Straight down, "up" on screen is +Y.
        let up = if self.pitch.abs() > 89f32.to_radians() {
            Vec3::new(-self.yaw.sin(), self.yaw.cos(), 0.0) * self.pitch.signum()
        } else {
            Vec3::Z
        };
        Mat4::look_at_rh(self.eye(), self.target, up)
    }

    pub fn projection(&self, aspect: f32) -> Mat4 {
        let aspect = if aspect.is_finite() && aspect > 0.0 { aspect } else { 1.0 };
        if self.perspective {
            Mat4::perspective_rh(self.fov_y, aspect, (self.distance * 0.01).max(0.1), self.distance * 100.0 + 5000.0)
        } else {
            let h = self.distance * (self.fov_y / 2.0).tan();
            Mat4::orthographic_rh(
                -h * aspect,
                h * aspect,
                -h,
                h,
                -self.distance * 50.0 - 5000.0,
                self.distance * 50.0 + 5000.0,
            )
        }
    }

    pub fn view_proj(&self, aspect: f32) -> Mat4 {
        self.projection(aspect) * self.view()
    }

    /// A ray through `ndc` (−1..1, y up): (origin, unit direction).
    pub fn ray(&self, ndc: Vec2, aspect: f32) -> (Vec3, Vec3) {
        let inv = self.view_proj(aspect).inverse();
        let near = inv.project_point3(ndc.extend(0.0));
        let far = inv.project_point3(ndc.extend(1.0));
        let dir = (far - near).normalize_or(-self.back());
        (near, dir)
    }

    pub fn orbit(&mut self, delta: Vec2) {
        self.yaw -= delta.x * 0.008;
        self.pitch = (self.pitch + delta.y * 0.008).clamp(-89.9f32.to_radians(), 89.9f32.to_radians());
    }

    /// Moves the target by a screen-space drag of `delta` points on a canvas `height` tall.
    pub fn pan(&mut self, delta: Vec2, height: f32) {
        let per_point = 2.0 * self.distance * (self.fov_y / 2.0).tan() / height.max(1.0);
        let view = self.view();
        let right = Vec3::new(view.x_axis.x, view.y_axis.x, view.z_axis.x);
        let up = Vec3::new(view.x_axis.y, view.y_axis.y, view.z_axis.y);
        let shift = (-right * delta.x + up * delta.y) * per_point;
        if shift.is_finite() {
            self.target += shift;
        }
    }

    /// Zooms by `factor` (< 1 closer) keeping `towards` (a scene point under the mouse) still.
    pub fn zoom(&mut self, factor: f32, towards: Option<Vec3>) {
        let factor = if factor.is_finite() { factor.clamp(0.2, 5.0) } else { 1.0 };
        let new_distance = (self.distance * factor).clamp(MIN_DISTANCE, MAX_DISTANCE);
        let applied = new_distance / self.distance;
        if let Some(p) = towards.filter(|p| p.is_finite()) {
            self.target = p + (self.target - p) * applied;
        }
        self.distance = new_distance;
    }

    /// Frames `bounds` in a canvas of `aspect`.
    pub fn fit(&mut self, bounds: &Aabb, aspect: f32) {
        if !bounds.is_valid() {
            return;
        }
        let radius = (bounds.size().length() / 2.0).max(10.0);
        let half = (self.fov_y / 2.0).min(((self.fov_y / 2.0).tan() * aspect.clamp(0.3, 3.0)).atan());
        self.target = bounds.centre();
        self.distance = (radius / half.sin() * 1.05).clamp(MIN_DISTANCE, MAX_DISTANCE);
    }

    /// Where `p` lands on screen, in canvas points from the top-left; `None` behind the eye.
    pub fn project(&self, p: Vec3, size: Vec2) -> Option<Vec2> {
        let clip = self.view_proj(size.x / size.y.max(1.0)) * p.extend(1.0);
        if clip.w <= 1e-6 {
            return None;
        }
        let ndc = clip.truncate() / clip.w;
        Some(Vec2::new((ndc.x + 1.0) / 2.0 * size.x, (1.0 - ndc.y) / 2.0 * size.y))
    }
}

/// Where the ray meets the horizontal plane at height `z`.
pub fn hit_plane(origin: Vec3, dir: Vec3, z: f32) -> Option<Vec3> {
    if dir.z.abs() < 1e-6 {
        return None;
    }
    let t = (z - origin.z) / dir.z;
    (t > 0.0).then(|| origin + dir * t)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn projecting_the_target_lands_in_the_middle() {
        let c = Camera::default();
        let p = c.project(c.target, Vec2::new(800.0, 600.0));
        assert!(p.is_some_and(|p| (p - Vec2::new(400.0, 300.0)).length() < 0.5), "{p:?}");
    }

    #[test]
    fn the_centre_ray_hits_the_target() {
        let mut c = Camera::default();
        for view in View::ALL.map(|v| v.0) {
            c.set_view(view);
            let (o, d) = c.ray(Vec2::ZERO, 1.5);
            let closest = o + d * (c.target - o).dot(d);
            assert!((closest - c.target).length() < 1e-2 * c.distance, "{view:?}");
        }
    }

    #[test]
    fn zoom_stays_in_range() {
        let mut c = Camera::default();
        for _ in 0..200 {
            c.zoom(0.5, Some(Vec3::ZERO));
        }
        assert!(c.distance >= MIN_DISTANCE);
        c.zoom(f32::NAN, None);
        assert!(c.distance.is_finite());
    }
}
