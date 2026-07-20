//! Free-fly camera and view/projection math.
//!
//! `glam::Mat4::perspective_rh` (the non-`_gl` variant) already targets
//! Vulkan/D3D depth range `[0,1]` at `z_near`/`z_far` — verified in
//! `near_and_far_planes_map_to_depth_zero_and_one` below, not just assumed
//! from the name. The one thing it still gets wrong for Vulkan is the Y
//! axis: clip space Y points down in Vulkan, up in the (right-handed, OpenGL-
//! descended) convention glam's helper assumes. [`VULKAN_CLIP_CORRECTION`]
//! flips only that.

use glam::{Mat4, Vec3};

/// Column-major, matches `Mat4::perspective_rh`'s convention: multiply this
/// on the *left* of the projection matrix (`CORRECTION * perspective`).
pub const VULKAN_CLIP_CORRECTION: Mat4 = Mat4::from_cols_array(&[
    1.0, 0.0, 0.0, 0.0,
    0.0, -1.0, 0.0, 0.0,
    0.0, 0.0, 1.0, 0.0,
    0.0, 0.0, 0.0, 1.0,
]);

pub struct Camera {
    pub position: Vec3,
    /// Radians. 0 looks down -Z; positive turns toward +X.
    pub yaw: f32,
    /// Radians, clamped by callers to avoid gimbal flip (not enforced here).
    pub pitch: f32,
    pub fov_y_radians: f32,
    pub aspect: f32,
    pub near: f32,
    pub far: f32,
}

impl Camera {
    pub fn new(position: Vec3, aspect: f32) -> Self {
        Self {
            position,
            yaw: 0.0,
            pitch: 0.0,
            fov_y_radians: 70f32.to_radians(),
            aspect,
            near: 0.05,
            far: 1000.0,
        }
    }

    /// Unit forward vector from yaw/pitch (standard spherical→cartesian, yaw
    /// about +Y measured from -Z, pitch about the resulting right axis).
    pub fn forward(&self) -> Vec3 {
        let (sy, cy) = self.yaw.sin_cos();
        let (sp, cp) = self.pitch.sin_cos();
        Vec3::new(sy * cp, sp, -cy * cp).normalize()
    }

    pub fn right(&self) -> Vec3 {
        self.forward().cross(Vec3::Y).normalize()
    }

    pub fn view_matrix(&self) -> Mat4 {
        Mat4::look_to_rh(self.position, self.forward(), Vec3::Y)
    }

    pub fn projection_matrix(&self) -> Mat4 {
        VULKAN_CLIP_CORRECTION * Mat4::perspective_rh(self.fov_y_radians, self.aspect, self.near, self.far)
    }

    pub fn view_proj(&self) -> Mat4 {
        self.projection_matrix() * self.view_matrix()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forward_is_unit_length() {
        let mut c = Camera::new(Vec3::ZERO, 1.0);
        for yaw in [-2.0, -0.3, 0.0, 0.7, 3.1] {
            for pitch in [-1.4, -0.5, 0.0, 0.5, 1.4] {
                c.yaw = yaw;
                c.pitch = pitch;
                assert!((c.forward().length() - 1.0).abs() < 1e-5);
            }
        }
    }

    #[test]
    fn default_orientation_looks_down_negative_z() {
        let c = Camera::new(Vec3::ZERO, 1.0);
        let f = c.forward();
        assert!(f.dot(Vec3::NEG_Z) > 0.999);
    }

    #[test]
    fn point_ahead_projects_to_ndc_center_with_positive_depth() {
        let c = Camera::new(Vec3::ZERO, 1.0);
        let target = Vec3::new(0.0, 0.0, -10.0); // straight ahead, within near/far
        let clip = c.view_proj() * target.extend(1.0);
        let ndc = clip.truncate() / clip.w;
        assert!(ndc.x.abs() < 1e-4, "x={}", ndc.x);
        assert!(ndc.y.abs() < 1e-4, "y={}", ndc.y);
        assert!((0.0..=1.0).contains(&ndc.z), "z={} not in Vulkan depth range", ndc.z);
    }

    #[test]
    fn near_and_far_planes_map_to_depth_zero_and_one() {
        let c = Camera::new(Vec3::ZERO, 1.0);
        let clip_near = c.view_proj() * Vec3::new(0.0, 0.0, -c.near).extend(1.0);
        let clip_far = c.view_proj() * Vec3::new(0.0, 0.0, -c.far).extend(1.0);
        assert!((clip_near.z / clip_near.w).abs() < 1e-4);
        assert!(((clip_far.z / clip_far.w) - 1.0).abs() < 1e-4);
    }

    #[test]
    fn world_up_is_screen_up_in_vulkan_ndc() {
        // Vulkan NDC Y points down, so a point above the camera (world +Y)
        // must land at *negative* NDC y once the clip correction is applied.
        let c = Camera::new(Vec3::ZERO, 1.0);
        let above = Vec3::new(0.0, 1.0, -10.0);
        let clip = c.view_proj() * above.extend(1.0);
        assert!(clip.y / clip.w < 0.0);
    }

    #[test]
    fn right_is_perpendicular_to_forward_and_up() {
        let mut c = Camera::new(Vec3::ZERO, 1.0);
        c.yaw = 0.9;
        c.pitch = 0.4;
        let (f, r) = (c.forward(), c.right());
        assert!(f.dot(r).abs() < 1e-5);
        assert!((r.length() - 1.0).abs() < 1e-5);
    }
}
