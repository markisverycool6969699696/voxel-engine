//! Voxel raycasting for block interaction (mining/placement target
//! selection). Amanatides & Woo's fast voxel traversal: step through the
//! grid one cell boundary at a time along whichever axis the ray crosses
//! next, rather than marching in fixed distance increments — visits exactly
//! the cells the ray passes through, no skipped or duplicated cells.

use glam::{IVec3, Vec3};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RaycastHit {
    pub block: IVec3,
    /// Unit axis direction of the face that was hit, pointing back toward
    /// the ray origin — i.e. where a placed block should go.
    pub normal: IVec3,
    pub distance: f32,
}

fn signum(v: f32) -> i32 {
    if v > 0.0 {
        1
    } else if v < 0.0 {
        -1
    } else {
        0
    }
}

/// First solid block along the ray from `origin` in `direction`, within
/// `max_distance`. `direction` need not be normalized (zero-length returns
/// `None`). The origin's own cell is never reported as a hit, even if solid —
/// callers cast from inside empty space (e.g. the player's eye).
pub fn raycast_voxels(
    origin: Vec3,
    direction: Vec3,
    max_distance: f32,
    is_solid: impl Fn(i32, i32, i32) -> bool,
) -> Option<RaycastHit> {
    let dir = direction.normalize_or_zero();
    if dir == Vec3::ZERO || max_distance <= 0.0 {
        return None;
    }

    let mut block = IVec3::new(
        origin.x.floor() as i32,
        origin.y.floor() as i32,
        origin.z.floor() as i32,
    );
    let step = IVec3::new(signum(dir.x), signum(dir.y), signum(dir.z));

    let t_delta = Vec3::new(
        if dir.x != 0.0 { (1.0 / dir.x).abs() } else { f32::INFINITY },
        if dir.y != 0.0 { (1.0 / dir.y).abs() } else { f32::INFINITY },
        if dir.z != 0.0 { (1.0 / dir.z).abs() } else { f32::INFINITY },
    );
    let axis_t_max = |pos: f32, cell: i32, d: f32| -> f32 {
        if d > 0.0 {
            ((cell as f32 + 1.0) - pos) / d
        } else if d < 0.0 {
            (cell as f32 - pos) / d
        } else {
            f32::INFINITY
        }
    };
    let mut t_max = Vec3::new(
        axis_t_max(origin.x, block.x, dir.x),
        axis_t_max(origin.y, block.y, dir.y),
        axis_t_max(origin.z, block.z, dir.z),
    );

    // Hard bound (3 axes worst-case per unit distance + margin) so a logic
    // bug degrades to "misses" rather than an infinite loop.
    let max_steps = (max_distance.ceil() as u32) * 3 + 8;
    for _ in 0..max_steps {
        let axis = if t_max.x <= t_max.y && t_max.x <= t_max.z {
            0
        } else if t_max.y <= t_max.z {
            1
        } else {
            2
        };
        let t = t_max[axis];
        if t > max_distance {
            return None;
        }
        let normal = match axis {
            0 => {
                block.x += step.x;
                t_max.x += t_delta.x;
                IVec3::new(-step.x, 0, 0)
            }
            1 => {
                block.y += step.y;
                t_max.y += t_delta.y;
                IVec3::new(0, -step.y, 0)
            }
            _ => {
                block.z += step.z;
                t_max.z += t_delta.z;
                IVec3::new(0, 0, -step.z)
            }
        };
        if is_solid(block.x, block.y, block.z) {
            return Some(RaycastHit { block, normal, distance: t });
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn straight_down_hits_ground_with_up_normal() {
        let is_solid = |_: i32, y: i32, _: i32| y <= 0;
        let hit = raycast_voxels(Vec3::new(0.5, 5.0, 0.5), Vec3::new(0.0, -1.0, 0.0), 10.0, is_solid)
            .unwrap();
        assert_eq!(hit.block, IVec3::new(0, 0, 0));
        assert_eq!(hit.normal, IVec3::new(0, 1, 0));
        assert!((hit.distance - 4.0).abs() < 1e-4);
    }

    #[test]
    fn straight_along_x_hits_wall_with_correct_normal() {
        let is_solid = |x: i32, _: i32, _: i32| x == 5;
        let hit = raycast_voxels(Vec3::new(0.5, 0.5, 0.5), Vec3::new(1.0, 0.0, 0.0), 20.0, is_solid)
            .unwrap();
        assert_eq!(hit.block, IVec3::new(5, 0, 0));
        assert_eq!(hit.normal, IVec3::new(-1, 0, 0));
        assert!((hit.distance - 4.5).abs() < 1e-4);
    }

    #[test]
    fn misses_return_none_within_range() {
        let is_solid = |_: i32, _: i32, _: i32| false;
        assert!(raycast_voxels(Vec3::ZERO, Vec3::new(1.0, 0.0, 0.0), 10.0, is_solid).is_none());
    }

    #[test]
    fn target_beyond_max_distance_is_not_hit() {
        let is_solid = |x: i32, _: i32, _: i32| x == 100;
        assert!(raycast_voxels(Vec3::ZERO, Vec3::new(1.0, 0.0, 0.0), 10.0, is_solid).is_none());
    }

    #[test]
    fn does_not_report_origins_own_cell() {
        // Origin sits inside a solid cell; ray shouldn't immediately "hit" it.
        let is_solid = |x: i32, y: i32, z: i32| x == 0 && y == 0 && z == 0;
        let hit = raycast_voxels(Vec3::new(0.5, 0.5, 0.5), Vec3::new(1.0, 0.0, 0.0), 10.0, is_solid);
        assert!(hit.is_none());
    }

    #[test]
    fn diagonal_ray_hits_expected_cell() {
        let is_solid = |x: i32, y: i32, z: i32| x == 3 && y == 3 && z == 3;
        let hit = raycast_voxels(
            Vec3::new(0.5, 0.5, 0.5),
            Vec3::new(1.0, 1.0, 1.0),
            20.0,
            is_solid,
        )
        .unwrap();
        assert_eq!(hit.block, IVec3::new(3, 3, 3));
    }

    #[test]
    fn zero_length_direction_returns_none() {
        let is_solid = |_: i32, _: i32, _: i32| true;
        assert!(raycast_voxels(Vec3::ZERO, Vec3::ZERO, 10.0, is_solid).is_none());
    }

    #[test]
    fn axis_aligned_ray_does_not_divide_by_zero() {
        // Pure +Y ray: x/z components of direction are exactly zero.
        let is_solid = |x: i32, y: i32, z: i32| x == 0 && z == 0 && y == 7;
        let hit = raycast_voxels(Vec3::new(0.5, 0.5, 0.5), Vec3::new(0.0, 1.0, 0.0), 20.0, is_solid)
            .unwrap();
        assert_eq!(hit.block, IVec3::new(0, 7, 0));
    }
}
