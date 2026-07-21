//! Player movement/collision (spec §4.4's player-facing half — the fixed-
//! timestep/simulation-radius decoupling from render framerate is a later
//! concern once there's an actual game loop driving multiple systems).
//!
//! Collision is axis-separated (resolve X, then Z, then Y) against a voxel
//! grid, each axis swept in small fixed steps rather than solved analytically
//! for the exact contact point. Analytic AABB-vs-voxel-grid contact math is
//! the kind of thing that's easy to get subtly wrong at the boundaries
//! (off-by-one at exact integer coordinates, wrong push-out direction on a
//! corner); stepping in ~5cm increments and stopping at the last
//! non-colliding position trades a small, bounded, invisible position error
//! for something straightforward to verify by testing every case explicitly.

use glam::Vec3;

const STEP_SIZE: f32 = 0.05;

pub const PLAYER_HALF_WIDTH: f32 = 0.3;
pub const PLAYER_HALF_HEIGHT: f32 = 0.9;
/// Eye height above the AABB center (center-origin box, ~1.8 tall total).
pub const EYE_HEIGHT_OFFSET: f32 = 0.7;

const GRAVITY: f32 = -20.0;
const JUMP_SPEED: f32 = 7.0;
const MAX_FALL_SPEED: f32 = -40.0;
const WALK_SPEED: f32 = 4.5;
const FLY_SPEED: f32 = 8.0;

fn voxel_range(min: f32, max: f32) -> std::ops::RangeInclusive<i32> {
    const EPS: f32 = 1e-4;
    let lo = min.floor() as i32;
    let hi = ((max - EPS).floor() as i32).max(lo);
    lo..=hi
}

fn aabb_overlaps_solid(min: Vec3, max: Vec3, is_solid: &impl Fn(i32, i32, i32) -> bool) -> bool {
    for y in voxel_range(min.y, max.y) {
        for z in voxel_range(min.z, max.z) {
            for x in voxel_range(min.x, max.x) {
                if is_solid(x, y, z) {
                    return true;
                }
            }
        }
    }
    false
}

/// Moves `pos` (AABB center) by `delta`, axis-separated, stopping short of
/// any solid voxel. `collided[i]` is true iff axis `i` (x=0,y=1,z=2) was
/// blocked before covering its full requested delta.
pub fn move_and_collide(
    pos: Vec3,
    half: Vec3,
    delta: Vec3,
    is_solid: impl Fn(i32, i32, i32) -> bool,
) -> (Vec3, [bool; 3]) {
    let mut p = pos;
    let mut collided = [false; 3];
    // X and Z before Y: resolving horizontal first means a falling player
    // landing exactly on a step edge still gets pushed clear of the step's
    // side face before the vertical resolve runs.
    for axis in [0usize, 2, 1] {
        let amount = delta[axis];
        if amount == 0.0 {
            continue;
        }
        let steps = (amount.abs() / STEP_SIZE).ceil().max(1.0) as u32;
        let step = amount / steps as f32;
        for _ in 0..steps {
            let mut candidate = p;
            candidate[axis] += step;
            if aabb_overlaps_solid(candidate - half, candidate + half, &is_solid) {
                collided[axis] = true;
                break;
            }
            p = candidate;
        }
    }
    (p, collided)
}

pub struct PlayerController {
    /// AABB center.
    pub position: Vec3,
    pub velocity: Vec3,
    pub on_ground: bool,
    /// Creative-mode flight: gravity off, vertical speed under direct
    /// control via `wish_dir.y`. Collision stays on (this is flight, not
    /// noclip) — still resolved through the same `move_and_collide` sweep.
    pub flying: bool,
}

impl PlayerController {
    pub fn new(position: Vec3) -> Self {
        Self { position, velocity: Vec3::ZERO, on_ground: false, flying: false }
    }

    pub fn eye_position(&self) -> Vec3 {
        self.position + Vec3::new(0.0, EYE_HEIGHT_OFFSET, 0.0)
    }

    /// Toggles flight, clearing fall/jump state either way so leaving flight
    /// resumes under gravity cleanly rather than keeping stale y-velocity.
    pub fn toggle_flying(&mut self) {
        self.flying = !self.flying;
        self.velocity.y = 0.0;
        self.on_ground = false;
    }

    /// `wish_dir.x`/`.z` are a horizontal (caller-normalized) direction —
    /// zero for "no horizontal input". `wish_dir.y` is ignored unless
    /// `flying` is set, in which case it directly drives vertical speed
    /// (expected range roughly -1..=1: descend/hold/ascend), independent of
    /// the horizontal normalization. No acceleration/friction modeling yet
    /// (velocity snaps directly to wish speed); that's a feel tweak for
    /// later, not a correctness concern.
    pub fn update(
        &mut self,
        dt: f32,
        wish_dir: Vec3,
        jump: bool,
        is_solid: impl Fn(i32, i32, i32) -> bool,
    ) {
        self.velocity.x = wish_dir.x * WALK_SPEED;
        self.velocity.z = wish_dir.z * WALK_SPEED;

        if self.flying {
            self.velocity.y = wish_dir.y * FLY_SPEED;
        } else {
            if self.on_ground && jump {
                self.velocity.y = JUMP_SPEED;
            }
            self.velocity.y = (self.velocity.y + GRAVITY * dt).max(MAX_FALL_SPEED);
        }

        let half = Vec3::new(PLAYER_HALF_WIDTH, PLAYER_HALF_HEIGHT, PLAYER_HALF_WIDTH);
        let (new_pos, collided) = move_and_collide(self.position, half, self.velocity * dt, is_solid);
        self.position = new_pos;

        if self.flying {
            // Flight overrides grounded state entirely — landing is only a
            // "not flying" concept.
            self.on_ground = false;
        } else if collided[1] {
            // Sign checked before the zero-out below: only a downward block
            // counts as landing, not bonking your head on a ceiling.
            self.on_ground = self.velocity.y <= 0.0;
            self.velocity.y = 0.0;
        } else {
            self.on_ground = false;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flat_ground_at(y: i32) -> impl Fn(i32, i32, i32) -> bool {
        move |_, by, _| by <= y
    }

    #[test]
    fn falls_under_gravity_when_airborne() {
        let mut p = PlayerController::new(Vec3::new(0.0, 50.0, 0.0));
        let no_ground = |_, _, _| false;
        p.update(0.1, Vec3::ZERO, false, no_ground);
        assert!(p.velocity.y < 0.0);
        assert!(p.position.y < 50.0);
        assert!(!p.on_ground);
    }

    #[test]
    fn lands_on_flat_ground_and_stops_falling() {
        // Ground top surface at y=0 (solid for by<=0); player center starts
        // just above so it lands within a few steps.
        let mut p = PlayerController::new(Vec3::new(0.5, 2.0, 0.5));
        let ground = flat_ground_at(0);
        for _ in 0..200 {
            p.update(1.0 / 60.0, Vec3::ZERO, false, &ground);
        }
        assert!(p.on_ground, "expected to have landed");
        assert_eq!(p.velocity.y, 0.0);
        // Center should rest at half-height above the ground surface (y=1).
        assert!((p.position.y - (1.0 + PLAYER_HALF_HEIGHT)).abs() < STEP_SIZE * 2.0);
    }

    #[test]
    fn jump_leaves_ground_then_returns() {
        let mut p = PlayerController::new(Vec3::new(0.5, 1.0 + PLAYER_HALF_HEIGHT, 0.5));
        let ground = flat_ground_at(0);
        // Settle first.
        for _ in 0..10 {
            p.update(1.0 / 60.0, Vec3::ZERO, false, &ground);
        }
        assert!(p.on_ground);
        p.update(1.0 / 60.0, Vec3::ZERO, true, &ground);
        assert!(p.velocity.y > 0.0);
        assert!(!p.on_ground);
        for _ in 0..200 {
            p.update(1.0 / 60.0, Vec3::ZERO, false, &ground);
        }
        assert!(p.on_ground, "expected to land again after the jump arc");
    }

    #[test]
    fn horizontal_motion_is_blocked_by_a_wall() {
        // Solid wall filling x=5 at all y/z.
        let wall = |x: i32, _: i32, _: i32| x == 5;
        let mut p = PlayerController::new(Vec3::new(0.0, 10.0, 0.0));
        for _ in 0..300 {
            p.update(1.0 / 60.0, Vec3::new(1.0, 0.0, 0.0), false, &wall);
        }
        // Must stop short of the wall (center + half_width < 5.0), never inside/through it.
        assert!(p.position.x + PLAYER_HALF_WIDTH < 5.0 + 1e-3);
        assert!(p.position.x > 3.0, "expected real forward progress before blocking: got {}", p.position.x);
    }

    #[test]
    fn does_not_fall_through_ground_at_high_fall_speed() {
        let mut p = PlayerController::new(Vec3::new(0.5, 100.0, 0.5));
        p.velocity.y = -39.0; // near terminal velocity already
        let ground = flat_ground_at(0);
        for _ in 0..1000 {
            p.update(1.0 / 60.0, Vec3::ZERO, false, &ground);
            if p.on_ground {
                break;
            }
        }
        assert!(p.on_ground);
        assert!(p.position.y > 0.0, "tunneled through the floor: y={}", p.position.y);
    }

    #[test]
    fn flying_ignores_gravity_and_climbs_on_positive_y_wish() {
        let mut p = PlayerController::new(Vec3::new(0.0, 10.0, 0.0));
        p.toggle_flying();
        let no_ground = |_, _, _| false;
        for _ in 0..30 {
            p.update(1.0 / 60.0, Vec3::new(0.0, 1.0, 0.0), false, no_ground);
        }
        assert!(p.position.y > 10.0, "expected to climb while flying: y={}", p.position.y);
        assert!(!p.on_ground);
    }

    #[test]
    fn flying_holds_altitude_with_zero_vertical_wish() {
        let mut p = PlayerController::new(Vec3::new(0.0, 10.0, 0.0));
        p.toggle_flying();
        let no_ground = |_, _, _| false;
        for _ in 0..60 {
            p.update(1.0 / 60.0, Vec3::ZERO, false, no_ground);
        }
        // No gravity applied: y should not have dropped like the falling test does.
        assert!((p.position.y - 10.0).abs() < 1e-3, "expected no fall while flying: y={}", p.position.y);
    }

    #[test]
    fn leaving_flight_resumes_falling_under_gravity() {
        let mut p = PlayerController::new(Vec3::new(0.0, 10.0, 0.0));
        p.toggle_flying();
        let no_ground = |_, _, _| false;
        p.update(1.0 / 60.0, Vec3::ZERO, false, no_ground);
        p.toggle_flying(); // back to normal physics
        assert!(!p.flying);
        assert_eq!(p.velocity.y, 0.0); // toggle clears stale velocity
        p.update(1.0 / 60.0, Vec3::ZERO, false, no_ground);
        assert!(p.velocity.y < 0.0, "expected gravity to resume");
    }

    #[test]
    fn flight_still_collides_with_solids() {
        // Solid ceiling at y == 15 (block occupies [15,16)); flying straight
        // up should stop just below it, not clip through.
        let ceiling = |_: i32, y: i32, _: i32| y == 15;
        let mut p = PlayerController::new(Vec3::new(0.0, 10.0, 0.0));
        p.toggle_flying();
        for _ in 0..300 {
            p.update(1.0 / 60.0, Vec3::new(0.0, 1.0, 0.0), false, &ceiling);
        }
        assert!(p.position.y + PLAYER_HALF_HEIGHT <= 15.0 + 1e-3, "clipped into ceiling: y={}", p.position.y);
    }

    #[test]
    fn move_and_collide_stops_exactly_at_boundary_not_past_it() {
        let is_solid = |x: i32, _: i32, _: i32| x >= 10;
        let (pos, hit) = move_and_collide(
            Vec3::new(5.0, 0.5, 0.5),
            Vec3::new(0.3, 0.3, 0.3),
            Vec3::new(10.0, 0.0, 0.0),
            is_solid,
        );
        assert!(hit[0]);
        assert!(pos.x + 0.3 <= 10.0 + 1e-3);
        assert!(pos.x > 9.0);
    }
}
