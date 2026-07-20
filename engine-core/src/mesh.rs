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

/// GPU-ready vertex: world/local-space position, face normal, and a color.
/// No UV/texture index yet — there's no texture atlas (spec §4.5's item/block
/// data exists, but atlas assembly doesn't), so [`triangulate`] fills `color`
/// with a debug placeholder instead. Layout matches the vertex input state
/// `render-vk`'s pipeline declares — the two must be changed together.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct MeshVertex {
    pub position: [f32; 3],
    pub normal: [f32; 3],
    pub color: [f32; 3],
}

/// Deterministic, texture-atlas-free placeholder color per block id (a
/// stand-in so different block types are visually distinguishable before
/// real texturing exists — not meant to survive contact with the atlas).
fn debug_color(block: BlockId) -> [f32; 3] {
    let h = (block.0 as u32).wrapping_mul(2654435761);
    [
        0.35 + 0.55 * ((h & 0xFF) as f32 / 255.0),
        0.35 + 0.55 * (((h >> 8) & 0xFF) as f32 / 255.0),
        0.35 + 0.55 * (((h >> 16) & 0xFF) as f32 / 255.0),
    ]
}

/// Flat per-face shading: brighter for up-facing quads, darker for
/// down-facing, midway for sides — cheap legibility without a lighting
/// system (fixed "sun from above", not a real light source).
fn face_shade(normal: [f32; 3]) -> f32 {
    0.65 + 0.35 * normal[1]
}

/// Triangulates quads into an indexed vertex buffer ((0,1,2),(0,2,3) per
/// quad, matching [`Quad`]'s documented winding), merging identical vertices
/// is deliberately skipped: at chunk-section scale the index buffer savings
/// don't justify a hash-map pass, and every quad's corners are already
/// unique to that quad (no shared-vertex smoothing wanted between
/// differently-shaded/colored faces anyway).
pub fn triangulate(quads: &[Quad]) -> (Vec<MeshVertex>, Vec<u32>) {
    let mut vertices = Vec::with_capacity(quads.len() * 4);
    let mut indices = Vec::with_capacity(quads.len() * 6);
    for quad in quads {
        let base = vertices.len() as u32;
        let color = debug_color(quad.block);
        let shade = face_shade(quad.normal);
        let shaded = [color[0] * shade, color[1] * shade, color[2] * shade];
        for corner in quad.corners {
            vertices.push(MeshVertex { position: corner, normal: quad.normal, color: shaded });
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
    fn triangulate_preserves_normals_and_colors_vary_by_block() {
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
        assert_ne!(vertices[0].color, vertices[4].color);
        // All corners of one quad share the same (shaded) color.
        assert!(vertices[0..4].iter().all(|v| v.color == vertices[0].color));
    }
}
