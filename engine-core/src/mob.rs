//! Generic mob movement/AI scaffolding (spec §6: "mob AI basics" is
//! Sonnet-tier feature work). Deliberately contains no mob roster, spawning
//! rules, or per-species behavior — that's content, not scaffolding, and
//! stays out of engine-core (same boundary already drawn for block/item data
//! in `registry.rs`).
//!
//! [`Wander`] is one intentionally simple reusable behavior: walk a random
//! horizontal heading for a random duration, then pick a new one; a bumped
//! wall ends the current heading early instead of pushing into it forever.
//! Built on the same [`crate::physics::move_and_collide`] sweep the player
//! uses, so a mob's collision behavior is identical (and identically
//! tested) to the player's.

use crate::physics::move_and_collide;
use glam::Vec3;

const GRAVITY: f32 = -20.0;
const MAX_FALL_SPEED: f32 = -40.0;

/// Small deterministic xorshift64 PRNG. Mobs only need "randomish" wander
/// headings/timings, not cryptographic randomness — this avoids pulling in
/// a `rand` dependency for a few dice rolls, and stays fully reproducible
/// for tests.
#[derive(Clone, Debug)]
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        // xorshift64 requires a nonzero state; fold the seed so 0 still works.
        Self(seed ^ 0x9E3779B97F4A7C15)
    }

    pub fn next_u32(&mut self) -> u32 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        (x >> 32) as u32
    }

    /// Uniform in `[0, 1)`.
    pub fn next_f32(&mut self) -> f32 {
        (self.next_u32() as f32) / (u32::MAX as f32 + 1.0)
    }
}

/// A generic ground-wandering entity: gravity + AABB collision identical to
/// the player, plus a wander heading that changes periodically. Appearance,
/// species, and spawn placement are all the caller's concern.
pub struct Mob {
    /// AABB center.
    pub position: Vec3,
    pub velocity: Vec3,
    pub half: Vec3,
    pub on_ground: bool,
    heading: f32,
    wander_timer: f32,
    /// A heading imposed for the next `update` (by pathfinding, etc.),
    /// overriding wander for that tick only. Cleared once consumed, so if the
    /// caller stops steering the mob resumes wandering on its own.
    steering: Option<f32>,
    rng: Rng,
}

impl Mob {
    pub fn new(position: Vec3, half: Vec3, seed: u64) -> Self {
        let mut rng = Rng::new(seed);
        let heading = rng.next_f32() * std::f32::consts::TAU;
        Self {
            position,
            velocity: Vec3::ZERO,
            half,
            on_ground: false,
            heading,
            wander_timer: 0.0,
            steering: None,
            rng,
        }
    }

    /// Steers the mob toward horizontal direction `(dx, dz)` for the next
    /// `update` (used to follow a path). A near-zero vector is ignored. This
    /// overrides — but does not disable — wandering: stop calling it and the
    /// mob wanders again.
    pub fn steer_toward(&mut self, dx: f32, dz: f32) {
        if dx * dx + dz * dz > 1e-6 {
            self.steering = Some(dz.atan2(dx));
        }
    }

    /// Advances gravity, wander AI, and collision by `dt`. `walk_speed` is
    /// caller-provided so different mobs can share this behavior at
    /// different speeds without a new type per species.
    pub fn update(&mut self, dt: f32, walk_speed: f32, is_solid: impl Fn(i32, i32, i32) -> bool) {
        if let Some(h) = self.steering.take() {
            self.heading = h; // path-following overrides wander this tick
        } else {
            self.wander_timer -= dt;
            if self.wander_timer <= 0.0 {
                self.heading = self.rng.next_f32() * std::f32::consts::TAU;
                self.wander_timer = 1.5 + self.rng.next_f32() * 2.5;
            }
        }

        self.velocity.x = self.heading.cos() * walk_speed;
        self.velocity.z = self.heading.sin() * walk_speed;
        self.velocity.y = (self.velocity.y + GRAVITY * dt).max(MAX_FALL_SPEED);

        let (new_pos, collided) =
            move_and_collide(self.position, self.half, self.velocity * dt, is_solid);
        self.position = new_pos;

        if collided[1] {
            self.on_ground = self.velocity.y <= 0.0;
            self.velocity.y = 0.0;
        } else {
            self.on_ground = false;
        }
        if collided[0] || collided[2] {
            // Bumped a wall: end this heading now rather than pushing into
            // it until the timer naturally expires.
            self.wander_timer = 0.0;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rng_is_deterministic_for_a_given_seed() {
        let mut a = Rng::new(42);
        let mut b = Rng::new(42);
        for _ in 0..100 {
            assert_eq!(a.next_u32(), b.next_u32());
        }
    }

    #[test]
    fn rng_f32_stays_in_unit_range() {
        let mut r = Rng::new(7);
        for _ in 0..10_000 {
            let v = r.next_f32();
            assert!((0.0..1.0).contains(&v), "out of range: {v}");
        }
    }

    #[test]
    fn falls_under_gravity_when_airborne() {
        let mut m = Mob::new(Vec3::new(0.0, 50.0, 0.0), Vec3::splat(0.3), 1);
        let no_ground = |_, _, _| false;
        m.update(0.1, 0.0, no_ground);
        assert!(m.velocity.y < 0.0);
        assert!(m.position.y < 50.0);
        assert!(!m.on_ground);
    }

    #[test]
    fn lands_on_flat_ground_and_stops_falling() {
        let ground = |_: i32, y: i32, _: i32| y <= 0;
        let mut m = Mob::new(Vec3::new(0.5, 5.0, 0.5), Vec3::new(0.3, 0.45, 0.3), 2);
        for _ in 0..200 {
            m.update(1.0 / 60.0, 0.0, &ground);
        }
        assert!(m.on_ground);
        assert_eq!(m.velocity.y, 0.0);
    }

    #[test]
    fn wanders_and_makes_horizontal_progress_over_time() {
        let ground = |_: i32, y: i32, _: i32| y <= 0;
        let mut m = Mob::new(Vec3::new(0.0, 5.0, 0.0), Vec3::new(0.3, 0.45, 0.3), 3);
        for _ in 0..600 {
            m.update(1.0 / 60.0, 2.0, &ground);
        }
        let horizontal_dist = (m.position.x.powi(2) + m.position.z.powi(2)).sqrt();
        assert!(horizontal_dist > 0.5, "expected wander to move the mob: dist={horizontal_dist}");
    }

    #[test]
    fn never_tunnels_through_a_wall_while_wandering() {
        // Solid wall at x >= 5 only (falls forever otherwise, which is fine
        // — this test only cares about horizontal tunneling); whichever
        // heading the mob picks, it must never end up on the far side.
        let wall = |x: i32, _: i32, _: i32| x >= 5;
        let mut m = Mob::new(Vec3::new(0.0, 100.0, 0.0), Vec3::new(0.3, 0.45, 0.3), 4);
        for _ in 0..3000 {
            m.update(1.0 / 60.0, 3.0, &wall);
            assert!(m.position.x + m.half.x < 5.0 + 1e-3, "tunneled through wall: x={}", m.position.x);
        }
    }

    #[test]
    fn steering_overrides_wander_then_releases() {
        let ground = |_: i32, y: i32, _: i32| y <= 0;
        let mut m = Mob::new(Vec3::new(0.0, 5.0, 0.0), Vec3::new(0.3, 0.45, 0.3), 5);
        // Settle on the ground first.
        for _ in 0..60 {
            m.update(1.0 / 60.0, 0.0, &ground);
        }
        // Steer hard toward +x for a while: horizontal motion should be +x.
        let start_x = m.position.x;
        for _ in 0..120 {
            m.steer_toward(1.0, 0.0);
            m.update(1.0 / 60.0, 2.0, &ground);
        }
        assert!(m.position.x > start_x + 1.0, "steering should drive +x motion");
        assert!(m.position.z.abs() < 0.5, "steering +x shouldn't drift in z");
    }

    #[test]
    fn different_seeds_diverge() {
        let ground = |_: i32, y: i32, _: i32| y <= 0;
        let mut a = Mob::new(Vec3::new(0.0, 5.0, 0.0), Vec3::splat(0.3), 10);
        let mut b = Mob::new(Vec3::new(0.0, 5.0, 0.0), Vec3::splat(0.3), 99);
        for _ in 0..300 {
            a.update(1.0 / 60.0, 2.0, &ground);
            b.update(1.0 / 60.0, 2.0, &ground);
        }
        assert_ne!(a.position, b.position);
    }
}
