//! Terrain generation (spec §7): layered value-noise heightmap terrain with
//! biomes, water, caves, ore, and trees. Implements
//! [`crate::streaming::ChunkGenerator`] so it drops straight into the
//! existing background-threaded streaming pipeline.
//!
//! Design goals (per the "coherent, Minecraft-like, not scattered" brief):
//! terrain is a *contiguous solid heightmap* — stone up to a per-column
//! surface height, a thin soil/surface layer on top, water filling anything
//! below sea level. It is deliberately not a 3D "block here / no block there"
//! noise field (which produces floating scattered voxels); the only 3D noise
//! is a conservative cave carve applied *below* the surface so it never
//! pockmarks the visible ground.
//!
//! Determinism (required by spec §4.1's regenerate-don't-store contract): every
//! block is a pure function of `(seed, world_x, world_y, world_z)` via a
//! hashed value-noise, so the same `(cx, sy, cz)` always yields an identical
//! section. All tuning constants below are original.

use crate::chunk::{BlockId, PalettedSection, AIR};
use crate::streaming::ChunkGenerator;

pub const STONE: BlockId = BlockId(1);
pub const DIRT: BlockId = BlockId(2);
pub const GRASS: BlockId = BlockId(3);
pub const SAND: BlockId = BlockId(4);
pub const WATER: BlockId = BlockId(5);
pub const WOOD: BlockId = BlockId(6);
pub const LEAVES: BlockId = BlockId(7);
pub const SNOW: BlockId = BlockId(8);
pub const BEDROCK: BlockId = BlockId(9);
pub const COAL_ORE: BlockId = BlockId(10);
pub const IRON_ORE: BlockId = BlockId(11);

pub const SEA_LEVEL: i32 = 64;
/// Surface height above which terrain is bare rock regardless of biome —
/// only the tallest mountain-mask peaks (see `TerrainGenerator::height`)
/// reach this, so it reads as an actual treeline, not a random override.
/// Calibrated empirically: across a wide scan, realistic peaks only reach
/// ~sea level + 18-20 (the height formula's amplitude term rarely gets
/// close to its theoretical max — fbm-summed noise clusters near its
/// midpoint), so `+30` (the original guess) was never reachable.
const MOUNTAIN_ROCK_HEIGHT: i32 = SEA_LEVEL + 12;
/// Nothing (terrain or trees) reaches this high; sections whose bottom is
/// above it are pure air and skip generation entirely.
const SKY_FLOOR: i32 = 132;
const DIM: i32 = 16;

// ---- hashing / value noise ------------------------------------------------

#[inline]
fn splitmix64(x: u64) -> u64 {
    let mut z = x.wrapping_add(0x9E3779B97F4A7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D049BB133111EB);
    z ^ (z >> 31)
}

#[inline]
fn hash2(seed: u64, x: i32, z: i32) -> u64 {
    splitmix64(seed ^ ((x as u32 as u64) | ((z as u32 as u64) << 32)))
}

#[inline]
fn hash3(seed: u64, x: i32, y: i32, z: i32) -> u64 {
    let h = splitmix64(seed ^ (x as u32 as u64));
    let h = splitmix64(h ^ (y as u32 as u64));
    splitmix64(h ^ (z as u32 as u64))
}

/// Top-bit-derived uniform value in `[0, 1)`.
#[inline]
fn r01(h: u64) -> f32 {
    ((h >> 40) as f32) / ((1u32 << 24) as f32)
}

#[inline]
fn smooth(t: f32) -> f32 {
    t * t * (3.0 - 2.0 * t)
}

#[inline]
fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

fn vnoise2(seed: u64, x: f32, z: f32) -> f32 {
    let (x0, z0) = (x.floor(), z.floor());
    let (xi, zi) = (x0 as i32, z0 as i32);
    let (xf, zf) = (smooth(x - x0), smooth(z - z0));
    let c = |dx, dz| r01(hash2(seed, xi + dx, zi + dz));
    lerp(lerp(c(0, 0), c(1, 0), xf), lerp(c(0, 1), c(1, 1), xf), zf)
}

fn vnoise3(seed: u64, x: f32, y: f32, z: f32) -> f32 {
    let (x0, y0, z0) = (x.floor(), y.floor(), z.floor());
    let (xi, yi, zi) = (x0 as i32, y0 as i32, z0 as i32);
    let (xf, yf, zf) = (smooth(x - x0), smooth(y - y0), smooth(z - z0));
    let c = |dx, dy, dz| r01(hash3(seed, xi + dx, yi + dy, zi + dz));
    let x00 = lerp(c(0, 0, 0), c(1, 0, 0), xf);
    let x10 = lerp(c(0, 1, 0), c(1, 1, 0), xf);
    let x01 = lerp(c(0, 0, 1), c(1, 0, 1), xf);
    let x11 = lerp(c(0, 1, 1), c(1, 1, 1), xf);
    lerp(lerp(x00, x10, yf), lerp(x01, x11, yf), zf)
}

/// Fractal Brownian motion (summed octaves), normalized back to `[0, 1]`.
fn fbm2(seed: u64, x: f32, z: f32, octaves: u32) -> f32 {
    let (mut freq, mut amp, mut sum, mut norm) = (1.0, 1.0, 0.0, 0.0);
    for i in 0..octaves {
        sum += vnoise2(seed.wrapping_add(i as u64 * 0x9E37), x * freq, z * freq) * amp;
        norm += amp;
        freq *= 2.0;
        amp *= 0.5;
    }
    sum / norm
}

fn fbm3(seed: u64, x: f32, y: f32, z: f32, octaves: u32) -> f32 {
    let (mut freq, mut amp, mut sum, mut norm) = (1.0, 1.0, 0.0, 0.0);
    for i in 0..octaves {
        sum += vnoise3(seed.wrapping_add(i as u64 * 0x9E37), x * freq, y * freq, z * freq) * amp;
        norm += amp;
        freq *= 2.0;
        amp *= 0.5;
    }
    sum / norm
}

// ---- generator ------------------------------------------------------------

/// Per-column derived data, computed once per `(wx, wz)` and reused down the
/// whole vertical column.
struct Col {
    height: i32,
    surface: BlockId,
    sub: BlockId,
    allow_tree: bool,
    humid: f32,
}

struct Tree {
    base: i32,
    trunk: i32,
}

pub struct TerrainGenerator {
    seed: u64,
}

impl TerrainGenerator {
    pub fn new(seed: u64) -> Self {
        Self { seed }
    }

    /// Surface (top solid) world-y for a column — used by callers to place
    /// the player/mobs on the ground without guessing.
    pub fn surface_height(&self, wx: i32, wz: i32) -> i32 {
        self.height(wx, wz)
    }

    fn height(&self, wx: i32, wz: i32) -> i32 {
        let s = self.seed;
        // Low-frequency continent shape + a mountain mask that sharpens
        // amplitude in some regions + fine detail. Squaring the mountain
        // mask keeps most of the world gentle with occasional real peaks,
        // rather than uniformly bumpy everywhere.
        let cont = fbm2(s ^ 0xC0, wx as f32 / 256.0, wz as f32 / 256.0, 4);
        let mnt = fbm2(s ^ 0xA1, wx as f32 / 512.0, wz as f32 / 512.0, 3);
        let det = fbm2(s ^ 0x5E, wx as f32 / 64.0, wz as f32 / 64.0, 3);
        let amp = 6.0 + mnt * mnt * 54.0;
        let h = SEA_LEVEL as f32 + (cont - 0.5) * 2.0 * amp + (det - 0.5) * 6.0;
        (h.round() as i32).clamp(2, 122)
    }

    fn climate(&self, wx: i32, wz: i32) -> (f32, f32) {
        let temp = fbm2(self.seed ^ 0x70, wx as f32 / 384.0, wz as f32 / 384.0, 3);
        let humid = fbm2(self.seed ^ 0x90, wx as f32 / 384.0, wz as f32 / 384.0, 3);
        (temp, humid)
    }

    fn column(&self, wx: i32, wz: i32) -> Col {
        let height = self.height(wx, wz);
        let (temp, humid) = self.climate(wx, wz);
        // Biome from temperature/humidity. Surface block is discrete; the
        // height field it sits on is continuous (see `height`), so biome
        // edges blend in elevation rather than forming cliffs — this is the
        // "biome blending" the spec asks for, done on the smooth axis.
        // Bare rock above the treeline takes priority over the
        // temperature/humidity biomes below — real mountain peaks are rock
        // regardless of the regional climate.
        let (surface, sub, allow_tree) = if height > MOUNTAIN_ROCK_HEIGHT {
            (STONE, STONE, false) // rocky mountain peak
        } else if temp > 0.62 && humid < 0.40 {
            (SAND, SAND, false) // desert
        } else if temp < 0.32 {
            (SNOW, DIRT, humid > 0.5) // snowy: grass-under-snow, sparse trees
        } else {
            (GRASS, DIRT, true) // temperate / forest
        };
        Col { height, surface, sub, allow_tree, humid }
    }

    fn cave(&self, wx: i32, wy: i32, wz: i32) -> bool {
        // Conservative threshold on the upper tail => sparse connected
        // tunnels, not swiss cheese. Squashed vertically (smaller y divisor)
        // so caves read as horizontal passages.
        fbm3(self.seed ^ 0xCA, wx as f32 / 40.0, wy as f32 / 22.0, wz as f32 / 40.0, 3) > 0.80
    }

    fn ore(&self, wx: i32, wy: i32, wz: i32, height: i32) -> Option<BlockId> {
        if wy < height - 5 {
            let r = r01(hash3(self.seed ^ 0x0E, wx, wy, wz));
            if r < 0.010 {
                return Some(if wy < SEA_LEVEL - 16 { IRON_ORE } else { COAL_ORE });
            }
        }
        None
    }

    fn block_at(&self, wx: i32, wy: i32, wz: i32, col: &Col) -> BlockId {
        if wy <= 0 {
            return BEDROCK;
        }
        if wy > col.height {
            return if wy <= SEA_LEVEL { WATER } else { AIR };
        }
        // Caves only below the top couple blocks, so the surface skin is
        // never carved (no scattered holes in the visible ground).
        if wy < col.height - 2 && self.cave(wx, wy, wz) {
            return AIR;
        }
        if wy == col.height {
            // Underwater tops get soil/sand, not grass/snow.
            return if col.height < SEA_LEVEL { col.sub } else { col.surface };
        }
        if wy >= col.height - 3 {
            return col.sub;
        }
        if let Some(o) = self.ore(wx, wy, wz, col.height) {
            return o;
        }
        STONE
    }

    fn tree_at(&self, wx: i32, wz: i32) -> Option<Tree> {
        let col = self.column(wx, wz);
        if !col.allow_tree || col.height <= SEA_LEVEL {
            return None;
        }
        let density = if col.humid > 0.55 { 0.06 } else { 0.012 };
        if r01(hash2(self.seed ^ 0x77, wx, wz)) >= density {
            return None;
        }
        let trunk = 4 + (hash2(self.seed ^ 0x78, wx, wz) % 3) as i32;
        Some(Tree { base: col.height + 1, trunk })
    }

    /// Stamps trees whose blocks fall into this section. Scans a ±2 margin so
    /// a tree rooted in a neighbouring column still contributes leaves/trunk
    /// across the section boundary — computed identically from either side,
    /// so no seams.
    fn stamp_trees(&self, s: &mut PalettedSection, cx: i32, sy: i32, cz: i32) {
        let base_y = sy * DIM;
        for tz in -2..=DIM + 1 {
            for tx in -2..=DIM + 1 {
                let (wx, wz) = (cx * DIM + tx, cz * DIM + tz);
                let Some(t) = self.tree_at(wx, wz) else { continue };
                let top = t.base + t.trunk - 1;
                for i in 0..t.trunk {
                    place(s, cx, cz, base_y, wx, t.base + i, wz, WOOD, true);
                }
                for dy in -1..=1 {
                    let wy = top + dy;
                    let r = if dy == 1 { 1 } else { 2 };
                    for dz in -r..=r {
                        for dx in -r..=r {
                            if dx * dx + dz * dz > r * r + 1 {
                                continue; // round the canopy corners off
                            }
                            place(s, cx, cz, base_y, wx + dx, wy, wz + dz, LEAVES, false);
                        }
                    }
                }
            }
        }
    }
}

/// Writes `block` at world `(wx, wy, wz)` into section-local coords if it
/// lands inside this section. `overwrite=false` only fills air (so leaves
/// don't eat trunks or terrain).
#[allow(clippy::too_many_arguments)]
fn place(
    s: &mut PalettedSection,
    cx: i32,
    cz: i32,
    base_y: i32,
    wx: i32,
    wy: i32,
    wz: i32,
    block: BlockId,
    overwrite: bool,
) {
    let (lx, ly, lz) = (wx - cx * DIM, wy - base_y, wz - cz * DIM);
    if lx < 0 || lx >= DIM || ly < 0 || ly >= DIM || lz < 0 || lz >= DIM {
        return;
    }
    let (lx, ly, lz) = (lx as usize, ly as usize, lz as usize);
    if !overwrite && s.get(lx, ly, lz) != AIR {
        return;
    }
    s.set(lx, ly, lz, block);
}

impl ChunkGenerator for TerrainGenerator {
    fn generate(&self, cx: i32, sy: i32, cz: i32) -> PalettedSection {
        let base_y = sy * DIM;
        if base_y >= SKY_FLOOR {
            return PalettedSection::filled(AIR); // pure sky
        }
        let mut s = PalettedSection::filled(AIR);
        for lz in 0..DIM {
            for lx in 0..DIM {
                let (wx, wz) = (cx * DIM + lx, cz * DIM + lz);
                let col = self.column(wx, wz);
                for ly in 0..DIM {
                    let wy = base_y + ly;
                    let b = self.block_at(wx, wy, wz, &col);
                    if b != AIR {
                        s.set(lx as usize, ly as usize, lz as usize, b);
                    }
                }
            }
        }
        self.stamp_trees(&mut s, cx, sy, cz);
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_sections_have_no_phantom_dirt_faces_at_vertical_boundaries() {
        // Regression test for a real bug found via a screenshot report
        // ("ground looks like dirt/holes instead of grass"): SEA_LEVEL (64)
        // sits exactly on a section boundary (64 = 4 * 16), and
        // `greedy_mesh` used to treat every section's top/bottom boundary as
        // unconditionally exposed, ignoring whatever the real neighboring
        // section contained. That produced a visible DIRT-colored "phantom"
        // face wherever a column's grass cap happened to sit in the section
        // above a subsurface dirt layer -- which, given the sea-level
        // alignment, was most near-surface terrain. Fixed by
        // `greedy_mesh_with_y_neighbors` (see mesh.rs); this test exercises
        // the exact generate() -> mesh pipeline the game uses (not just the
        // isolated mesh.rs unit test) and asserts no such face survives.
        use crate::chunk::ChunkColumn;
        use crate::mesh::greedy_mesh_with_y_neighbors;
        let g = TerrainGenerator::new(0x5EED_1234);
        let opaque = |b: BlockId| b != AIR;
        for cx in -3..3 {
            for cz in -3..3 {
                let mut column = ChunkColumn::new();
                for sy in 1..7 {
                    column.insert_section(sy, g.generate(cx, sy, cz));
                }
                for sy in 1..7 {
                    let section = column.section(sy).unwrap();
                    let quads = greedy_mesh_with_y_neighbors(
                        section,
                        opaque,
                        column.section(sy - 1),
                        column.section(sy + 1),
                    );
                    for q in &quads {
                        if q.normal != [0.0, 1.0, 0.0] || q.block != DIRT {
                            continue;
                        }
                        let wy = sy * DIM + q.corners[0][1] as i32;
                        let wx = cx * DIM + q.corners[0][0] as i32;
                        let wz = cz * DIM + q.corners[0][2] as i32;
                        let col = g.column(wx, wz);
                        let has_water_or_solid_above = (wy..=(SEA_LEVEL + 1))
                            .any(|y| g.block_at(wx, y, wz, &col) != AIR);
                        assert!(
                            has_water_or_solid_above,
                            "phantom exposed dirt quad at wx={wx} wy={wy} wz={wz}, \
                             col.height={} (nothing solid/water covers it up to sea level)",
                            col.height
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn generation_is_deterministic() {
        let a = TerrainGenerator::new(1234);
        let b = TerrainGenerator::new(1234);
        for &(cx, sy, cz) in &[(0, 4, 0), (3, 4, -2), (-5, 3, 7)] {
            let sa = a.generate(cx, sy, cz);
            let sb = b.generate(cx, sy, cz);
            for z in 0..16 {
                for y in 0..16 {
                    for x in 0..16 {
                        assert_eq!(sa.get(x, y, z), sb.get(x, y, z), "cell {x},{y},{z}");
                    }
                }
            }
        }
    }

    #[test]
    fn surface_is_solid_and_open_above() {
        let g = TerrainGenerator::new(42);
        // Pick a land column (height above sea) so we're testing ground, not lake.
        let mut found_land = false;
        for wx in 0..64 {
            let h = g.surface_height(wx, 0);
            if h > SEA_LEVEL + 2 {
                found_land = true;
                let col = g.column(wx, 0);
                assert_ne!(g.block_at(wx, h, 0, &col), AIR, "surface must be solid");
                assert_eq!(g.block_at(wx, h + 1, 0, &col), AIR, "just above surface must be open");
                // A few blocks down is solid ground (allowing the odd cave cell).
                let solid_below = (1..=5).filter(|d| g.block_at(wx, h - d, 0, &col) != AIR).count();
                assert!(solid_below >= 3, "expected mostly-solid subsurface, got {solid_below}/5");
            }
        }
        assert!(found_land, "test seed produced no land in the sampled strip");
    }

    #[test]
    fn far_above_terrain_is_pure_air() {
        let g = TerrainGenerator::new(7);
        let s = g.generate(0, 20, 0); // world y 320+, well above SKY_FLOOR
        assert_eq!(s.uniform_block(), Some(AIR));
    }

    #[test]
    fn low_areas_fill_with_water_up_to_sea_level() {
        let g = TerrainGenerator::new(99);
        // Section band straddling sea level across a wide area — some column
        // in here should dip below sea and hold water.
        let mut water = 0;
        for cx in 0..12 {
            for cz in 0..12 {
                let s = g.generate(cx, SEA_LEVEL / 16, cz);
                for z in 0..16 {
                    for y in 0..16 {
                        for x in 0..16 {
                            if s.get(x, y, z) == WATER {
                                water += 1;
                            }
                        }
                    }
                }
            }
        }
        assert!(water > 0, "expected some water in low areas across the sampled region");
    }

    #[test]
    fn mountain_peaks_are_rocky_and_treeless() {
        // Mountain-mask peaks are a large, low-frequency feature (wavelength
        // ~512 blocks), so a wide-enough scan should find at least one
        // column above the treeline for a couple of different seeds.
        let mut found = false;
        for seed in [1u64, 2, 3, 4, 5] {
            let g = TerrainGenerator::new(seed);
            for wx in (-300..300).step_by(5) {
                for wz in (-300..300).step_by(5) {
                    let col = g.column(wx, wz);
                    if col.height > MOUNTAIN_ROCK_HEIGHT {
                        found = true;
                        assert_eq!(col.surface, STONE, "peak surface must be rock");
                        assert_eq!(col.sub, STONE);
                        assert!(!col.allow_tree, "no trees above the treeline");
                    }
                }
            }
        }
        assert!(found, "no seed in the sampled set produced a mountain peak");
    }

    #[test]
    fn trees_only_grow_on_land() {
        let g = TerrainGenerator::new(2024);
        // Every tree base must sit above sea level (never in/under water).
        for wx in -40..40 {
            for wz in -40..40 {
                if let Some(t) = g.tree_at(wx, wz) {
                    assert!(t.base > SEA_LEVEL, "tree rooted at/below sea: base={}", t.base);
                    assert!(t.trunk >= 4);
                }
            }
        }
    }
}
