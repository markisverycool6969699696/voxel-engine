//! Screen-space UI geometry builders — crosshair, inventory picker, start
//! menu. All produce flat-colored `UiVertex` quads in NDC space (see
//! `engine_core::mesh::UiVertex`); `App` concatenates whichever of these
//! apply to the current state into one buffer and uploads it via
//! `Renderer::set_ui_mesh`.

use engine_core::mesh::UiVertex;

/// One axis-aligned colored quad, `(center_x, center_y, half_width, half_height, rgba)`,
/// in NDC. Appends 4 vertices / 6 indices (index-offset-adjusted) into the
/// caller's buffers.
pub fn push_quad(
    vertices: &mut Vec<UiVertex>,
    indices: &mut Vec<u32>,
    cx: f32,
    cy: f32,
    hw: f32,
    hh: f32,
    color: [f32; 4],
) {
    let base = vertices.len() as u32;
    vertices.extend([
        UiVertex { position: [cx - hw, cy - hh], color },
        UiVertex { position: [cx + hw, cy - hh], color },
        UiVertex { position: [cx + hw, cy + hh], color },
        UiVertex { position: [cx - hw, cy + hh], color },
    ]);
    indices.extend([base, base + 1, base + 2, base, base + 2, base + 3]);
}

/// Outline (4 thin quads) around an axis-aligned box — used for selection
/// highlights.
pub fn push_outline(
    vertices: &mut Vec<UiVertex>,
    indices: &mut Vec<u32>,
    cx: f32,
    cy: f32,
    hw: f32,
    hh: f32,
    thickness: f32,
    color: [f32; 4],
) {
    push_quad(vertices, indices, cx, cy - hh, hw, thickness, color); // bottom
    push_quad(vertices, indices, cx, cy + hh, hw, thickness, color); // top
    push_quad(vertices, indices, cx - hw, cy, thickness, hh, color); // left
    push_quad(vertices, indices, cx + hw, cy, thickness, hh, color); // right
}

/// Small centered "+" — arm length/thickness are in Y-NDC units, converted
/// to X so both arms look the same physical length/thickness on screen
/// regardless of window aspect ratio.
pub fn crosshair(aspect: f32) -> (Vec<UiVertex>, Vec<u32>) {
    const LEN_Y: f32 = 0.028;
    const THICK_Y: f32 = 0.004;
    let (len_x, thick_x) = (LEN_Y / aspect, THICK_Y / aspect);
    const COLOR: [f32; 4] = [1.0, 1.0, 1.0, 0.85];

    let mut vertices = Vec::with_capacity(8);
    let mut indices = Vec::with_capacity(12);
    push_quad(&mut vertices, &mut indices, 0.0, 0.0, thick_x, LEN_Y, COLOR); // vertical bar
    push_quad(&mut vertices, &mut indices, 0.0, 0.0, len_x, THICK_Y, COLOR); // horizontal bar
    (vertices, indices)
}

/// Deterministic placeholder color per block id — mirrors `render-vk`'s
/// `tile_base_color` (semantic hand-picked colors for known blocks, e.g.
/// water is blue, not an arbitrary hash) so a swatch in the inventory
/// actually matches what that block looks like in the world. A fully
/// arbitrary hash was confusing in-world (see MEMORY.md — water hashed to a
/// gray/tan that read as a hole), and would be just as confusing here for
/// the same reason, so unknown ids beyond the known set still fall back to
/// a hash rather than reintroducing that problem for future block ids.
pub fn color_for_block(id: u16) -> [f32; 4] {
    let rgb: [u8; 3] = match id {
        0 => [235, 235, 235],
        1 => [130, 130, 130],
        2 => [121, 85, 58],
        3 => [86, 156, 66],
        4 => [219, 202, 138],
        5 => [64, 128, 200],
        6 => [117, 84, 51],
        7 => [58, 122, 48],
        8 => [235, 235, 240],
        9 => [40, 40, 40],
        10 => [70, 70, 70],
        11 => [176, 148, 120],
        12 => [200, 40, 40],
        _ => {
            let h = (id as u32).wrapping_mul(2654435761);
            // Floored so no fallback swatch is too close to black (hard to
            // see against the menu background) or the white grid-cell border.
            let floor = |shift: u32| (0.25 + ((h >> shift) & 0xFF) as f32 / 255.0 * 0.75) * 255.0;
            [floor(16) as u8, floor(8) as u8, floor(0) as u8]
        }
    };
    [rgb[0] as f32 / 255.0, rgb[1] as f32 / 255.0, rgb[2] as f32 / 255.0, 1.0]
}

pub const INV_COLS: usize = 5;
const INV_CELL_H: f32 = 0.16;
const INV_GAP: f32 = 0.03;

/// Center + half-extent of grid cell `index` (row-major, `count` total
/// cells), in NDC. Single source of truth shared by `inventory_mesh` (what
/// to draw) and `inventory_hit_test` (what a click landed on) — they must
/// never compute this independently or a click could select the wrong item.
fn inventory_cell_rect(index: usize, count: usize, aspect: f32) -> (f32, f32, f32, f32) {
    let cols = INV_COLS.min(count.max(1));
    let rows = count.div_ceil(cols);
    let cell_x = INV_CELL_H / aspect;
    let gap_x = INV_GAP / aspect;
    let total_w = cols as f32 * cell_x + (cols as f32 - 1.0) * gap_x;
    let total_h = rows as f32 * INV_CELL_H + (rows as f32 - 1.0) * INV_GAP;
    let (row, col) = (index / cols, index % cols);
    // NDC y = -1 is the top row (standard Vulkan viewport mapping — see
    // main.rs's UI pipeline notes) so row 0 belongs at the top: increasing
    // row increases y, same as normal reading order.
    let cx = -total_w / 2.0 + cell_x / 2.0 + col as f32 * (cell_x + gap_x);
    let cy = -total_h / 2.0 + INV_CELL_H / 2.0 + row as f32 * (INV_CELL_H + INV_GAP);
    (cx, cy, cell_x / 2.0, INV_CELL_H / 2.0)
}

/// Builds the inventory grid: a dim full-screen backdrop, one colored swatch
/// per `(item_id, block_id)` in `items`, and a highlight outline around
/// `selected`'s cell if it's present in the list.
pub fn inventory_mesh(
    items: &[(u16, u16)],
    selected: Option<u16>,
    aspect: f32,
) -> (Vec<UiVertex>, Vec<u32>) {
    let mut vertices = Vec::new();
    let mut indices = Vec::new();
    push_quad(&mut vertices, &mut indices, 0.0, 0.0, 1.0, 1.0, [0.05, 0.05, 0.08, 0.75]);
    for (i, &(item_id, block_id)) in items.iter().enumerate() {
        let (cx, cy, hw, hh) = inventory_cell_rect(i, items.len(), aspect);
        push_quad(&mut vertices, &mut indices, cx, cy, hw * 0.9, hh * 0.9, color_for_block(block_id));
        if selected == Some(item_id) {
            push_outline(&mut vertices, &mut indices, cx, cy, hw, hh, 0.006, [1.0, 1.0, 1.0, 1.0]);
        }
    }
    (vertices, indices)
}

/// Which item index (if any) NDC point `(x, y)` lands inside, for a grid of
/// `count` cells. Must stay in lockstep with `inventory_cell_rect`.
pub fn inventory_hit_test(x: f32, y: f32, count: usize, aspect: f32) -> Option<usize> {
    (0..count).find(|&i| {
        let (cx, cy, hw, hh) = inventory_cell_rect(i, count, aspect);
        (x - cx).abs() <= hw && (y - cy).abs() <= hh
    })
}

const MENU_BAR_W: f32 = 0.5;
const MENU_BAR_H: f32 = 0.12;
const MENU_GAP: f32 = 0.04;

/// Center + half-extent of the `index`-th of `count` stacked menu bars, in
/// NDC. Single source of truth shared by `menu_mesh` and `menu_hit_test`,
/// same reasoning as `inventory_cell_rect`.
fn menu_option_rect(index: usize, count: usize) -> (f32, f32, f32, f32) {
    let total_h = count as f32 * MENU_BAR_H + (count as f32 - 1.0) * MENU_GAP;
    let cy = -total_h / 2.0 + MENU_BAR_H / 2.0 + index as f32 * (MENU_BAR_H + MENU_GAP);
    (0.0, cy, MENU_BAR_W, MENU_BAR_H / 2.0)
}

/// One glyph's 3-wide × 5-tall pixel bitmap, one `u8` per row (bit 2 = left
/// column, bit 1 = middle, bit 0 = right). No font atlas/asset exists (same
/// open decision as real block textures — see `docs/STARTER.md` §8), so
/// this draws letters the same way everything else placeholder in this
/// project is drawn: procedurally, as plain colored quads, not sampled from
/// an image. Only the handful of uppercase letters the menu actually needs
/// are defined; unsupported characters (including lowercase — callers should
/// pass already-uppercase text) are silently skipped by `text_mesh`, which
/// is fine for the short fixed menu labels this exists for.
fn glyph(c: char) -> Option<[u8; 5]> {
    Some(match c {
        'A' => [0b010, 0b101, 0b111, 0b101, 0b101],
        'C' => [0b011, 0b100, 0b100, 0b100, 0b011],
        'D' => [0b110, 0b101, 0b101, 0b101, 0b110],
        'E' => [0b111, 0b100, 0b110, 0b100, 0b111],
        'I' => [0b111, 0b010, 0b010, 0b010, 0b111],
        'L' => [0b100, 0b100, 0b100, 0b100, 0b111],
        'O' => [0b010, 0b101, 0b101, 0b101, 0b010],
        'R' => [0b110, 0b101, 0b110, 0b101, 0b101],
        'S' => [0b011, 0b100, 0b010, 0b001, 0b110],
        'T' => [0b111, 0b010, 0b010, 0b010, 0b010],
        'U' => [0b101, 0b101, 0b101, 0b101, 0b010],
        'V' => [0b101, 0b101, 0b101, 0b010, 0b010],
        ' ' => [0, 0, 0, 0, 0],
        _ => return None,
    })
}

const FONT_PIXEL: f32 = 0.010;
const FONT_STEP: f32 = FONT_PIXEL * 2.2;
const FONT_CHAR_ADVANCE: f32 = FONT_STEP * 4.0; // 3 glyph columns + 1 column of inter-char gap

/// Renders `text` as a horizontally-centered row of tiny pixel-quads on
/// `(cx, cy)`. Unsupported characters (see `glyph`) are skipped rather than
/// erroring — cosmetic UI text shouldn't be able to panic the game.
pub fn text_mesh(text: &str, cx: f32, cy: f32, color: [f32; 4]) -> (Vec<UiVertex>, Vec<u32>) {
    let mut vertices = Vec::new();
    let mut indices = Vec::new();
    let chars: Vec<char> = text.chars().collect();
    let total_w = chars.len() as f32 * FONT_CHAR_ADVANCE;
    let start_x = cx - total_w / 2.0 + FONT_CHAR_ADVANCE / 2.0;
    for (i, &c) in chars.iter().enumerate() {
        let Some(rows) = glyph(c.to_ascii_uppercase()) else { continue };
        let glyph_cx = start_x + i as f32 * FONT_CHAR_ADVANCE;
        for (row, bits) in rows.iter().enumerate() {
            let py = cy + FONT_STEP * 2.0 - row as f32 * FONT_STEP;
            for col in 0..3u32 {
                if bits & (1 << (2 - col)) != 0 {
                    let px = glyph_cx - FONT_STEP + col as f32 * FONT_STEP;
                    push_quad(&mut vertices, &mut indices, px, py, FONT_PIXEL, FONT_PIXEL, color);
                }
            }
        }
    }
    (vertices, indices)
}

/// Full-screen backdrop plus one colored, labeled bar per `(label, color)`
/// option.
pub fn menu_mesh(options: &[(&str, [f32; 4])]) -> (Vec<UiVertex>, Vec<u32>) {
    let mut vertices = Vec::new();
    let mut indices = Vec::new();
    push_quad(&mut vertices, &mut indices, 0.0, 0.0, 1.0, 1.0, [0.03, 0.03, 0.05, 0.92]);
    for (i, &(label, color)) in options.iter().enumerate() {
        let (cx, cy, hw, hh) = menu_option_rect(i, options.len());
        push_quad(&mut vertices, &mut indices, cx, cy, hw, hh, color);
        let (text_v, text_i) = text_mesh(label, cx, cy, [0.05, 0.05, 0.05, 1.0]);
        let base = vertices.len() as u32;
        indices.extend(text_i.into_iter().map(|i| i + base));
        vertices.extend(text_v);
    }
    (vertices, indices)
}

/// Which menu option index (if any) NDC point `(x, y)` lands inside.
pub fn menu_hit_test(x: f32, y: f32, count: usize) -> Option<usize> {
    (0..count).find(|&i| {
        let (cx, cy, hw, hh) = menu_option_rect(i, count);
        (x - cx).abs() <= hw && (y - cy).abs() <= hh
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hit_test_finds_every_cell_at_its_own_center() {
        for count in [1usize, 4, 5, 9, 12] {
            for aspect in [1.0f32, 16.0 / 9.0, 0.75] {
                for i in 0..count {
                    let (cx, cy, _, _) = inventory_cell_rect(i, count, aspect);
                    assert_eq!(
                        inventory_hit_test(cx, cy, count, aspect),
                        Some(i),
                        "count={count} aspect={aspect} index={i}"
                    );
                }
            }
        }
    }

    #[test]
    fn hit_test_misses_far_outside_the_grid() {
        assert_eq!(inventory_hit_test(-5.0, -5.0, 9, 1.0), None);
        assert_eq!(inventory_hit_test(5.0, 5.0, 9, 1.0), None);
    }

    #[test]
    fn cells_do_not_overlap() {
        let count = 12;
        let aspect = 1.0;
        let rects: Vec<_> = (0..count).map(|i| inventory_cell_rect(i, count, aspect)).collect();
        for i in 0..rects.len() {
            for j in (i + 1)..rects.len() {
                let (ax, ay, ahw, ahh) = rects[i];
                let (bx, by, bhw, bhh) = rects[j];
                let separated = (ax - bx).abs() >= ahw + bhw || (ay - by).abs() >= ahh + bhh;
                assert!(separated, "cells {i} and {j} overlap");
            }
        }
    }

    #[test]
    fn color_for_block_is_deterministic_and_in_range() {
        for id in 0..20u16 {
            let a = color_for_block(id);
            let b = color_for_block(id);
            assert_eq!(a, b);
            for channel in a {
                assert!((0.0..=1.0).contains(&channel), "channel out of range: {channel}");
            }
        }
    }

    #[test]
    fn menu_hit_test_finds_every_option_at_its_own_center() {
        for count in [2usize, 3, 4] {
            for i in 0..count {
                let (cx, cy, _, _) = menu_option_rect(i, count);
                assert_eq!(menu_hit_test(cx, cy, count), Some(i), "count={count} index={i}");
            }
        }
    }

    #[test]
    fn menu_options_do_not_overlap() {
        let count = 3;
        let rects: Vec<_> = (0..count).map(|i| menu_option_rect(i, count)).collect();
        for i in 0..rects.len() {
            for j in (i + 1)..rects.len() {
                let (ax, ay, _, ahh) = rects[i];
                let (bx, by, _, bhh) = rects[j];
                assert!(ax == bx && (ay - by).abs() >= ahh + bhh, "options {i} and {j} overlap");
            }
        }
    }

    #[test]
    fn text_mesh_draws_every_supported_letter() {
        // One quad per lit pixel; every glyph this project actually uses
        // (CREATIVE / SURVIVAL / LOAD) has at least one lit pixel per row,
        // so a run through all of them should never come back empty.
        for word in ["CREATIVE", "SURVIVAL", "LOAD"] {
            let (vertices, indices) = text_mesh(word, 0.0, 0.0, [1.0, 1.0, 1.0, 1.0]);
            assert!(!vertices.is_empty(), "{word} produced no geometry");
            assert_eq!(indices.len(), vertices.len() / 4 * 6);
        }
    }

    #[test]
    fn text_mesh_skips_unsupported_characters_without_panicking() {
        // Lowercase and punctuation aren't in the font; must be silently
        // skipped, never panic (this is cosmetic UI text).
        let (vertices, indices) = text_mesh("xyz!?", 0.0, 0.0, [1.0, 1.0, 1.0, 1.0]);
        assert!(vertices.is_empty());
        assert!(indices.is_empty());
    }

    #[test]
    fn glyph_is_defined_for_every_letter_the_menu_uses() {
        for c in "CREATIVSULOAD ".chars() {
            assert!(glyph(c).is_some(), "no glyph for {c:?}");
        }
    }

    #[test]
    fn different_ids_usually_get_different_colors() {
        let colors: std::collections::HashSet<_> =
            (0..12u16).map(|id| color_for_block(id).map(|c| (c * 1000.0) as i32)).collect();
        assert!(colors.len() > 8, "too many hash collisions among 12 ids: {colors:?}");
    }
}
