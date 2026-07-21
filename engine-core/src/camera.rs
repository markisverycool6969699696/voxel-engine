//! Free-fly camera and view/projection math.
//!
//! `glam::Mat4::perspective_rh` (the non-`_gl` variant) already targets
//! Vulkan/D3D depth range `[0,1]` at `z_near`/`z_far` — verified in
//! `near_and_far_planes_map_to_depth_zero_and_one` below, not just assumed
//! from the name.
//!
//! [`VULKAN_CLIP_CORRECTION`] is currently identity. An earlier version of
//! this file negated clip-space Y here, on the textbook-standard theory that
//! Vulkan's Y-down NDC needs correcting against glam's Y-up-clip convention
//! — internally consistent, unit-tested, but **never actually visually
//! confirmed** (this project's very first rendered triangle was accepted on
//! "no crash" alone — see MEMORY.md). Once real terrain made the world's
//! orientation unambiguous, a screenshot showed it rendering upside down
//! with that negation in place (sky at the bottom, terrain/trees hanging
//! from the top). This is the first attempt at a fix, not a confirmed one:
//! removing the negation is the direct, testable counter to "the image is a
//! clean vertical mirror," but exactly which stage was contributing the
//! extra flip (UBO layout, naga's WGSL matrix codegen, something else) was
//! not independently pinned down — if the world still renders wrong after
//! this, that uncertainty is why, and the next step should be checking
//! *this specific pipeline's* actual behavior empirically rather than
//! re-deriving Vulkan's Y convention from memory again. `render_vk`'s
//! `FrontFace` was flipped back to `CLOCKWISE` alongside this — winding-vs-
//! culling direction depends on whether a clip-space flip is present.
use glam::{Mat4, Vec3};

/// Column-major, matches `Mat4::perspective_rh`'s convention: multiply this
/// on the *left* of the projection matrix (`CORRECTION * perspective`).
pub const VULKAN_CLIP_CORRECTION: Mat4 = Mat4::from_cols_array(&[
    1.0, 0.0, 0.0, 0.0,
    0.0, 1.0, 0.0, 0.0,
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
        // This assertion direction is the empirical one, not the textbook
        // one — see this module's doc comment. Standard Vulkan viewport
        // rules (positive-height viewport, NDC y=-1 -> top row) say a point
        // above the camera landing at *negative* NDC y is what should put it
        // on screen-top; that's what the previous (removed) Y-negation
        // produced, and it rendered upside down anyway. This test currently
        // encodes the opposite (positive NDC y = up) because that's what a
        // screenshot showed this actual pipeline needs, not because the
        // textbook rule was re-derived and found wrong — if this pipeline's
        // effective Y convention changes again, fix the assertion to match
        // reality, don't "fix" reality to match a re-derivation of this
        // comment.
        let c = Camera::new(Vec3::ZERO, 1.0);
        let above = Vec3::new(0.0, 1.0, -10.0);
        let clip = c.view_proj() * above.extend(1.0);
        assert!(clip.y / clip.w > 0.0);
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
