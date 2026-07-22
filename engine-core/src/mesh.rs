//! Greedy meshing: chunk section → merged quads.
//!
//! For each of the 6 face directions, every 16×16 slice of the section is
//! turned into a mask of exposed faces (face exists where an opaque block
//! borders a non-opaque cell; out-of-section neighbors count as non-opaque,
//! so section-boundary faces are always emitted — cross-section culling is a
//! later renderer/streaming concern). Each mask is then greedily merged into
//! maximal rectangles, largest-width-first then height, which is the standard
//! quad-count-minimizing sweep. Faces merge only when they carry the same
//! [`BlockId`] — differently-textured faces must stay separate quads.
//!
//! Opacity is a caller-supplied predicate rather than a registry lookup so
//! this module stays decoupled from the data-driven block definitions.

use crate::chunk::{BlockId, PalettedSection, SECTION_DIM};

/// One merged axis-aligned rectangle of identical exposed faces.
#[derive(Debug, Clone, PartialEq)]
pub struct Quad {
    /// Corner positions in section-local space, counter-clockwise when viewed
    /// from the outside (the side `normal` points toward). Triangulate as
    /// (0,1,2) and (0,2,3).
    pub corners: [[f32; 3]; 4],
    /// Unit axis direction the face points toward (one of ±x/±y/±z).
    pub normal: [f32; 3],
    pub block: BlockId,
}

impl Quad {
    /// Face area in block units (used by tests to check no face is lost or
    /// double-emitted; total quad area must equal total exposed unit faces).
    pub fn area(&self) -> f32 {
        let e = |a: usize, b: usize| {
            [
                self.corners[b][0] - self.corners[a][0],
                self.corners[b][1] - self.corners[a][1],
                self.corners[b][2] - self.corners[a][2],
            ]
        };
        let (u, v) = (e(0, 1), e(0, 3));
        // Axis-aligned rectangle: area = |u| * |v|, and each edge lies on one axis.
        let len = |w: [f32; 3]| w[0].abs() + w[1].abs() + w[2].abs();
        len(u) * len(v)
    }
}

/// Where an atlas tile's pixels come from. Most blocks have a real texture
/// (GPL v2, github.com/elhedran/SimpleRP — see CREDITS.md); a handful have
/// no equivalent in that pack (it's meant to supplement vanilla Minecraft,
/// not replace it) and fall back to a flat color, same idea as the original
/// fully-procedural placeholder atlas.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TileSource {
    /// File stem under `render-vk/assets/textures/blocks/<name>.png`.
    Png(&'static str),
    /// RGB, no real texture available for this block.
    Solid([u8; 3]),
}

/// Ordered `(block_id, tile_source)` list — index in this array *is* the
/// atlas tile index. `render-vk` builds the atlas by walking this list in
/// order, so the two must be changed together; this is the one source of
/// truth for both "how many tiles" and "which tile is which block" (no more
/// hashing a block id into an arbitrary tile — each block needs *its own*
/// specific texture now, not just a stable-but-arbitrary one).
pub const ATLAS_TILES: &[(u16, TileSource)] = &[
    (1, TileSource::Png("stone")),
    (2, TileSource::Png("dirt")),
    (3, TileSource::Png("grass_side")),
    (4, TileSource::Png("sand")),
    (5, TileSource::Solid([64, 128, 200])), // water: no texture in the pack
    (6, TileSource::Png("log_oak")),
    (7, TileSource::Png("leaves_oak_opaque")),
    (8, TileSource::Solid([235, 235, 240])), // snow: no texture in the pack
    (9, TileSource::Png("bedrock")),
    (10, TileSource::Png("coal_ore")),
    (11, TileSource::Png("iron_ore")),
    (12, TileSource::Solid([200, 40, 40])), // mob_marker: debug, not real terrain
    (13, TileSource::Png("cobblestone")),
    (14, TileSource::Png("cobblestone_mossy")),
    (15, TileSource::Png("stonebrick")),
    (16, TileSource::Png("stonebrick_cracked")),
    (17, TileSource::Png("stonebrick_mossy")),
    (18, TileSource::Png("sandstone_normal")),
    (19, TileSource::Png("gold_ore")),
    (20, TileSource::Png("diamond_ore")),
    (21, TileSource::Png("lapis_ore")),
    (22, TileSource::Png("redstone_ore")),
    (23, TileSource::Png("emerald_ore")),
    (24, TileSource::Png("obsidian")),
    (25, TileSource::Png("brick")),
    (26, TileSource::Png("clay")),
    (27, TileSource::Png("planks_oak")),
    (28, TileSource::Png("planks_birch")),
    (29, TileSource::Png("planks_spruce")),
    (30, TileSource::Png("planks_jungle")),
    (31, TileSource::Png("planks_acacia")),
    (32, TileSource::Png("planks_big_oak")),
    (33, TileSource::Png("log_birch")),
    (34, TileSource::Png("log_spruce")),
    (35, TileSource::Png("log_jungle")),
    (36, TileSource::Png("log_acacia")),
    (37, TileSource::Png("log_big_oak")),
    (38, TileSource::Png("leaves_birch_opaque")),
    (39, TileSource::Png("leaves_spruce_opaque")),
    (40, TileSource::Png("leaves_jungle_opaque")),
    (41, TileSource::Png("leaves_acacia_opaque")),
    (42, TileSource::Png("leaves_big_oak_opaque")),
    (43, TileSource::Png("glass")),
    (44, TileSource::Png("glass_black")),
    (45, TileSource::Png("glass_blue")),
    (46, TileSource::Png("glass_brown")),
    (47, TileSource::Png("glass_cyan")),
    (48, TileSource::Png("glass_gray")),
    (49, TileSource::Png("glass_green")),
    (50, TileSource::Png("glass_light_blue")),
    (51, TileSource::Png("glass_lime")),
    (52, TileSource::Png("glass_magenta")),
    (53, TileSource::Png("glass_orange")),
    (54, TileSource::Png("glass_pink")),
    (55, TileSource::Png("glass_purple")),
    (56, TileSource::Png("glass_red")),
    (57, TileSource::Png("glass_silver")),
    (58, TileSource::Png("glass_white")),
    (59, TileSource::Png("glass_yellow")),
    (60, TileSource::Png("hardened_clay")),
    (61, TileSource::Png("hardened_clay_stained_black")),
    (62, TileSource::Png("hardened_clay_stained_blue")),
    (63, TileSource::Png("hardened_clay_stained_brown")),
    (64, TileSource::Png("hardened_clay_stained_cyan")),
    (65, TileSource::Png("hardened_clay_stained_gray")),
    (66, TileSource::Png("hardened_clay_stained_green")),
    (67, TileSource::Png("hardened_clay_stained_light_blue")),
    (68, TileSource::Png("hardened_clay_stained_lime")),
    (69, TileSource::Png("hardened_clay_stained_magenta")),
    (70, TileSource::Png("hardened_clay_stained_orange")),
    (71, TileSource::Png("hardened_clay_stained_pink")),
    (72, TileSource::Png("hardened_clay_stained_purple")),
    (73, TileSource::Png("hardened_clay_stained_red")),
    (74, TileSource::Png("hardened_clay_stained_silver")),
    (75, TileSource::Png("hardened_clay_stained_white")),
    (76, TileSource::Png("hardened_clay_stained_yellow")),
    (77, TileSource::Png("cactus_side")),
];

/// Number of tiles in the atlas — `render-vk` builds the atlas image at this
/// same tile count (one tile per `ATLAS_TILES` entry, same order), the two
/// must be changed together.
pub const ATLAS_TILE_COUNT: u32 = ATLAS_TILES.len() as u32;

/// GPU-ready vertex: world/local-space position, face normal, atlas UV, which
/// atlas tile to sample, and a flat shading multiplier. Layout matches the
/// vertex input state `render-vk`'s pipeline declares — the two must be
/// changed together.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct MeshVertex {
    pub position: [f32; 3],
    pub normal: [f32; 3],
    /// Corner coordinate *within* the assigned tile, in block units (not the
    /// unit square) — see `triangulate`'s doc comment; the shader `fract()`s
    /// this so the tile repeats once per block on a merged quad instead of
    /// stretching across the whole face.
    pub uv: [f32; 2],
    /// Which atlas tile (0..ATLAS_TILE_COUNT) to sample; float because
    /// vertex attributes are, resolved to an integer offset in the shader.
    pub tile: f32,
    pub shade: f32,
}

/// Atlas tile index for a block id — a linear scan of `ATLAS_TILES` (< 100
/// entries, called per-quad during triangulation; not worth a hash map at
/// this size). Falls back to tile 0 for a block id with no registered tile,
/// which shouldn't happen if `blocks.json` and `ATLAS_TILES` are kept in
/// sync, but a wrong texture beats an out-of-bounds atlas sample.
fn tile_for_block(block: BlockId) -> f32 {
    ATLAS_TILES.iter().position(|&(id, _)| id == block.0).unwrap_or(0) as f32
}

/// A screen-space (NDC, i.e. already `[-1, 1]`, no camera transform applied)
/// flat-colored vertex — crosshair, inventory swatches. Layout matches
/// `render-vk`'s UI pipeline's vertex input state; the two must be changed
/// together.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct UiVertex {
    pub position: [f32; 2],
    /// RGBA, 0..1.
    pub color: [f32; 4],
}

/// Flat per-face shading: brighter for up-facing quads, darker for
/// down-facing, midway for sides — cheap legibility without a lighting
/// system (fixed "sun from above", not a real light source).
fn face_shade(normal: [f32; 3]) -> f32 {
    0.65 + 0.35 * normal[1]
}

/// Triangulates quads into an indexed vertex buffer ((0,1,2),(0,2,3) per
/// quad, matching [`Quad`]'s documented winding). Merging identical vertices
/// is deliberately skipped: at chunk-section scale the index buffer savings
/// don't justify a hash-map pass, and every quad's corners are already
/// unique to that quad (no shared-vertex smoothing wanted between
/// differently-shaded faces anyway).
///
/// UV is always the unit square per corner regardless of the quad's merged
/// size — a texture stretches to fill the whole merged face rather than
/// repeating once per block. Correct per-block tiling needs the shader to
/// `fract()` an unwrapped block-space coordinate instead; deliberately
/// skipped for this placeholder pass (no real textures to tile yet either),
/// noted here so it isn't mistaken for an oversight later.
///
/// UV spans `0..width`/`0..height` in block units (read straight off the
/// quad's own corners — a merged quad's edges are exactly its block-space
/// width/height since the underlying grid is unit blocks), not the unit
/// square: the shader `fract()`s it before sampling, so the tile repeats
/// once per block on a merged quad instead of stretching across the whole
/// face. With real textures (not just a flat placeholder color) a stretched
/// tile is much more obviously wrong, so this matters more now than it did
/// with the procedural atlas.
pub fn triangulate(quads: &[Quad]) -> (Vec<MeshVertex>, Vec<u32>) {
    let mut vertices = Vec::with_capacity(quads.len() * 4);
    let mut indices = Vec::with_capacity(quads.len() * 6);
    for quad in quads {
        let base = vertices.len() as u32;
        let tile = tile_for_block(quad.block);
        let shade = face_shade(quad.normal);
        let edge = |a: usize, b: usize| {
            [
                quad.corners[b][0] - quad.corners[a][0],
                quad.corners[b][1] - quad.corners[a][1],
                quad.corners[b][2] - quad.corners[a][2],
            ]
        };
        // Axis-aligned rectangle: exactly one component of each edge vector
        // is nonzero, so summing absolute components recovers its length.
        let len = |w: [f32; 3]| w[0].abs() + w[1].abs() + w[2].abs();
        let width = len(edge(0, 1));
        let height = len(edge(0, 3));
        let uvs: [[f32; 2]; 4] = [[0.0, 0.0], [width, 0.0], [width, height], [0.0, height]];
        for (corner, uv) in quad.corners.into_iter().zip(uvs) {
            vertices.push(MeshVertex { position: corner, normal: quad.normal, uv, tile, shade });
        }
        indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
    }
    (vertices, indices)
}

/// Greedy-meshes one section. `is_opaque` decides which blocks emit faces;
/// faces are emitted where an opaque block meets a non-opaque one.
pub fn greedy_mesh(
    section: &PalettedSection,
    is_opaque: impl Fn(BlockId) -> bool,
) -> Vec<Quad> {
    let mut quads = Vec::new();
    // Fully non-opaque uniform sections (air) are the overwhelmingly common
    // empty case: nothing to emit at all.
    if let Some(b) = section.uniform_block() {
        if !is_opaque(b) {
            return quads;
        }
    }

    const N: usize = SECTION_DIM;
    let at = |p: [usize; 3]| section.get(p[0], p[1], p[2]);

    // axis = the direction faces point along; (u_axis, v_axis) span the slice.
    for axis in 0..3usize {
        let u_axis = (axis + 1) % 3;
        let v_axis = (axis + 2) % 3;
        for positive in [true, false] {
            for slice in 0..N {
                // Build the face mask for this slice/direction.
                let mut mask: [[Option<BlockId>; 16]; 16] = [[None; 16]; 16];
                for v in 0..N {
                    for u in 0..N {
                        let mut pos = [0usize; 3];
                        pos[axis] = slice;
                        pos[u_axis] = u;
                        pos[v_axis] = v;
                        let block = at(pos);
                        if !is_opaque(block) {
                            continue;
                        }
                        let exposed = if positive {
                            slice + 1 >= N || {
                                let mut n = pos;
                                n[axis] += 1;
                                !is_opaque(at(n))
                            }
                        } else {
                            slice == 0 || {
                                let mut n = pos;
                                n[axis] -= 1;
                                !is_opaque(at(n))
                            }
                        };
                        if exposed {
                            mask[v][u] = Some(block);
                        }
                    }
                }

                // Greedy rectangle merge over the mask.
                for v in 0..N {
                    let mut u = 0;
                    while u < N {
                        let Some(block) = mask[v][u] else {
                            u += 1;
                            continue;
                        };
                        let mut w = 1;
                        while u + w < N && mask[v][u + w] == Some(block) {
                            w += 1;
                        }
                        let mut h = 1;
                        'grow: while v + h < N {
                            for k in 0..w {
                                if mask[v + h][u + k] != Some(block) {
                                    break 'grow;
                                }
                            }
                            h += 1;
                        }
                        for dv in 0..h {
                            for du in 0..w {
                                mask[v + dv][u + du] = None;
                            }
                        }
                        quads.push(make_quad(
                            axis, u_axis, v_axis, positive, slice, u, v, w, h, block,
                        ));
                        u += w;
                    }
                }
            }
        }
    }
    quads
}

#[allow(clippy::too_many_arguments)]
fn make_quad(
    axis: usize,
    u_axis: usize,
    v_axis: usize,
    positive: bool,
    slice: usize,
    u: usize,
    v: usize,
    w: usize,
    h: usize,
    block: BlockId,
) -> Quad {
    // Face plane: outer side of the cell for +faces, inner (cell) side for −faces.
    let plane = if positive { slice + 1 } else { slice } as f32;
    let corner = |cu: usize, cv: usize| {
        let mut p = [0f32; 3];
        p[axis] = plane;
        p[u_axis] = cu as f32;
        p[v_axis] = cv as f32;
        p
    };
    // (u,v)→(u+w,v)→(u+w,v+h)→(u,v+h) winds CCW around +axis (u_axis × v_axis
    // = axis under the cyclic (axis, axis+1, axis+2) assignment); reverse for −faces.
    let c = [
        corner(u, v),
        corner(u + w, v),
        corner(u + w, v + h),
        corner(u, v + h),
    ];
    let corners = if positive { c } else { [c[0], c[3], c[2], c[1]] };
    let mut normal = [0f32; 3];
    normal[axis] = if positive { 1.0 } else { -1.0 };
    Quad { corners, normal, block }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chunk::{AIR, SECTION_VOLUME};

    fn opaque(b: BlockId) -> bool {
        b != AIR
    }

    const STONE: BlockId = BlockId(1);
    const DIRT: BlockId = BlockId(2);

    /// Brute-force exposed unit-face count for cross-checking.
    fn exposed_faces(s: &PalettedSection) -> usize {
        let n = SECTION_DIM as isize;
        let get = |x: isize, y: isize, z: isize| {
            if x < 0 || y < 0 || z < 0 || x >= n || y >= n || z >= n {
                AIR
            } else {
                s.get(x as usize, y as usize, z as usize)
            }
        };
        let mut count = 0;
        for y in 0..n {
            for z in 0..n {
                for x in 0..n {
                    if !opaque(get(x, y, z)) {
                        continue;
                    }
                    for (dx, dy, dz) in
                        [(1, 0, 0), (-1, 0, 0), (0, 1, 0), (0, -1, 0), (0, 0, 1), (0, 0, -1)]
                    {
                        if !opaque(get(x + dx, y + dy, z + dz)) {
                            count += 1;
                        }
                    }
                }
            }
        }
        count
    }

    fn total_area(quads: &[Quad]) -> f32 {
        quads.iter().map(Quad::area).sum()
    }

    #[test]
    fn air_section_is_empty() {
        assert!(greedy_mesh(&PalettedSection::filled(AIR), opaque).is_empty());
    }

    #[test]
    fn full_section_is_six_maximal_quads() {
        let quads = greedy_mesh(&PalettedSection::filled(STONE), opaque);
        assert_eq!(quads.len(), 6);
        for q in &quads {
            assert_eq!(q.area(), 256.0);
            assert_eq!(q.block, STONE);
        }
        assert_eq!(total_area(&quads), exposed_faces(&PalettedSection::filled(STONE)) as f32);
    }

    #[test]
    fn single_block_is_six_unit_quads() {
        let mut s = PalettedSection::filled(AIR);
        s.set(7, 7, 7, STONE);
        let quads = greedy_mesh(&s, opaque);
        assert_eq!(quads.len(), 6);
        for q in &quads {
            assert_eq!(q.area(), 1.0);
        }
    }

    #[test]
    fn two_adjacent_blocks_merge_side_faces() {
        let mut s = PalettedSection::filled(AIR);
        s.set(5, 5, 5, STONE);
        s.set(6, 5, 5, STONE);
        let quads = greedy_mesh(&s, opaque);
        // 2×1×1 bar: 2 unit end caps + 4 merged 2×1 side faces.
        assert_eq!(quads.len(), 6);
        assert_eq!(total_area(&quads), 10.0);
        assert_eq!(quads.iter().filter(|q| q.area() == 2.0).count(), 4);
    }

    #[test]
    fn different_blocks_do_not_merge() {
        let mut s = PalettedSection::filled(AIR);
        s.set(5, 5, 5, STONE);
        s.set(6, 5, 5, DIRT);
        let quads = greedy_mesh(&s, opaque);
        // Same shape as above but split by material: 10 unit faces.
        assert_eq!(quads.len(), 10);
        assert_eq!(total_area(&quads), 10.0);
        assert!(quads.iter().all(|q| q.area() == 1.0));
    }

    #[test]
    fn flat_slab_top_is_one_quad() {
        let mut s = PalettedSection::filled(AIR);
        for z in 0..16 {
            for x in 0..16 {
                s.set(x, 0, z, STONE);
            }
        }
        let quads = greedy_mesh(&s, opaque);
        let up: Vec<_> = quads.iter().filter(|q| q.normal == [0.0, 1.0, 0.0]).collect();
        assert_eq!(up.len(), 1);
        assert_eq!(up[0].area(), 256.0);
        assert_eq!(total_area(&quads), exposed_faces(&s) as f32);
    }

    #[test]
    fn checkerboard_cannot_merge() {
        let mut s = PalettedSection::filled(AIR);
        for y in 0..16 {
            for z in 0..16 {
                for x in 0..16 {
                    if (x + y + z) % 2 == 0 {
                        s.set(x, y, z, STONE);
                    }
                }
            }
        }
        let quads = greedy_mesh(&s, opaque);
        assert_eq!(total_area(&quads), exposed_faces(&s) as f32);
        assert!(quads.iter().all(|q| q.area() == 1.0));
        assert_eq!(quads.len(), SECTION_VOLUME / 2 * 6);
    }

    #[test]
    fn fuzz_area_matches_brute_force() {
        let mut state = 0x9E3779B97F4A7C15u64;
        let mut rng = move || {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (state >> 33) as usize
        };
        for _ in 0..20 {
            let mut s = PalettedSection::filled(AIR);
            for _ in 0..800 {
                s.set(rng() % 16, rng() % 16, rng() % 16, BlockId((rng() % 4) as u16));
            }
            let quads = greedy_mesh(&s, opaque);
            assert_eq!(total_area(&quads), exposed_faces(&s) as f32);
        }
    }

    #[test]
    fn winding_is_ccw_from_outside() {
        // For every quad, (c1−c0)×(c3−c0) must point along the normal.
        let mut s = PalettedSection::filled(AIR);
        s.set(3, 4, 5, STONE);
        for q in greedy_mesh(&s, opaque) {
            let e = |a: usize, b: usize| {
                [
                    q.corners[b][0] - q.corners[a][0],
                    q.corners[b][1] - q.corners[a][1],
                    q.corners[b][2] - q.corners[a][2],
                ]
            };
            let (u, v) = (e(0, 1), e(0, 3));
            let cross = [
                u[1] * v[2] - u[2] * v[1],
                u[2] * v[0] - u[0] * v[2],
                u[0] * v[1] - u[1] * v[0],
            ];
            let dot = cross[0] * q.normal[0] + cross[1] * q.normal[1] + cross[2] * q.normal[2];
            assert!(dot > 0.0, "quad {q:?} wound wrong");
        }
    }

    #[test]
    fn triangulate_counts_and_winding_match_quads() {
        let mut s = PalettedSection::filled(AIR);
        s.set(3, 4, 5, STONE);
        let quads = greedy_mesh(&s, opaque);
        let (vertices, indices) = triangulate(&quads);
        assert_eq!(vertices.len(), quads.len() * 4);
        assert_eq!(indices.len(), quads.len() * 6);
        for tri in indices.chunks(6) {
            let b = tri[0];
            assert_eq!(tri, &[b, b + 1, b + 2, b, b + 2, b + 3]);
        }
        assert!(indices.iter().all(|&i| (i as usize) < vertices.len()));
    }

    #[test]
    fn triangulate_preserves_normals_and_assigns_tile_per_block() {
        // Two synthetic quads (not via greedy_mesh — its output could place
        // adjacent-block quads' corners at the same coordinate, which isn't
        // what this test is checking) with distinct blocks.
        let stone_quad = Quad {
            corners: [[0.0, 1.0, 0.0], [1.0, 1.0, 0.0], [1.0, 1.0, 1.0], [0.0, 1.0, 1.0]],
            normal: [0.0, 1.0, 0.0],
            block: STONE,
        };
        let dirt_quad = Quad { block: DIRT, ..stone_quad.clone() };
        let (vertices, _) = triangulate(&[stone_quad, dirt_quad]);
        assert_eq!(vertices.len(), 8);
        for v in &vertices[0..4] {
            assert_eq!(v.normal, [0.0, 1.0, 0.0]);
        }
        assert_ne!(vertices[0].tile, vertices[4].tile);
        assert!(vertices[0].tile < ATLAS_TILE_COUNT as f32);
        // All corners of one quad share the same tile and shade.
        assert!(vertices[0..4].iter().all(|v| v.tile == vertices[0].tile));
        assert!(vertices[0..4].iter().all(|v| v.shade == vertices[0].shade));
    }

    #[test]
    fn triangulate_uv_scales_with_merged_quad_size() {
        // A 1x1 quad still gets the unit square...
        let unit = Quad {
            corners: [[0.0, 1.0, 0.0], [1.0, 1.0, 0.0], [1.0, 1.0, 1.0], [0.0, 1.0, 1.0]],
            normal: [0.0, 1.0, 0.0],
            block: STONE,
        };
        let (v, _) = triangulate(&[unit]);
        let uvs: Vec<_> = v.iter().map(|v| v.uv).collect();
        assert_eq!(uvs, vec![[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]]);

        // ...but a 3x2 merged quad's UV spans 0..3 / 0..2, not 0..1 — the
        // shader `fract()`s this to repeat the tile once per block instead
        // of stretching it across the whole merged face.
        let merged = Quad {
            corners: [[0.0, 1.0, 0.0], [3.0, 1.0, 0.0], [3.0, 1.0, 2.0], [0.0, 1.0, 2.0]],
            normal: [0.0, 1.0, 0.0],
            block: STONE,
        };
        let (v, _) = triangulate(&[merged]);
        let uvs: Vec<_> = v.iter().map(|v| v.uv).collect();
        assert_eq!(uvs, vec![[0.0, 0.0], [3.0, 0.0], [3.0, 2.0], [0.0, 2.0]]);
    }

    #[test]
    fn every_atlas_tile_block_id_is_unique() {
        let mut ids: Vec<u16> = ATLAS_TILES.iter().map(|&(id, _)| id).collect();
        ids.sort_unstable();
        let mut deduped = ids.clone();
        deduped.dedup();
        assert_eq!(ids, deduped, "a block id appears more than once in ATLAS_TILES");
    }

    #[test]
    fn tile_for_block_finds_every_registered_block() {
        for (i, &(id, _)) in ATLAS_TILES.iter().enumerate() {
            assert_eq!(tile_for_block(BlockId(id)), i as f32, "block id {id}");
        }
    }
}
