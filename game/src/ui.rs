//! Screen-space UI geometry builders — crosshair, inventory picker. All
//! produce flat-colored `UiVertex` quads in NDC space (see
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

// 76 placeable items need a much denser grid than a small hotbar-style
// picker — 10 columns keeps ~8 rows, fitting comfortably within NDC's
// -1..1 vertical range instead of overflowing the screen the way the
// original 5-column layout (sized for ~9 items) would at this count.
pub const INV_COLS: usize = 10;
const INV_CELL_H: f32 = 0.14;
const INV_GAP: f32 = 0.02;

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
/// per `(item_id, block_id)` in `items` (color from `swatch_color`, so real
/// textures can be represented by their average color rather than an
/// arbitrary hash), and a highlight outline around `selected`'s cell if it's
/// present in the list.
pub fn inventory_mesh(
    items: &[(u16, u16)],
    selected: Option<u16>,
    aspect: f32,
    swatch_color: impl Fn(u16) -> [f32; 4],
) -> (Vec<UiVertex>, Vec<u32>) {
    let mut vertices = Vec::new();
    let mut indices = Vec::new();
    push_quad(&mut vertices, &mut indices, 0.0, 0.0, 1.0, 1.0, [0.05, 0.05, 0.08, 0.75]);
    for (i, &(item_id, block_id)) in items.iter().enumerate() {
        let (cx, cy, hw, hh) = inventory_cell_rect(i, items.len(), aspect);
        push_quad(&mut vertices, &mut indices, cx, cy, hw * 0.9, hh * 0.9, swatch_color(block_id));
        if selected == Some(item_id) {
            push_outline(&mut vertices, &mut indices, cx, cy, hw, hh, 0.005, [1.0, 1.0, 1.0, 1.0]);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hit_test_finds_every_cell_at_its_own_center() {
        for count in [1usize, 10, 76, 100] {
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
        assert_eq!(inventory_hit_test(-5.0, -5.0, 76, 1.0), None);
        assert_eq!(inventory_hit_test(5.0, 5.0, 76, 1.0), None);
    }

    #[test]
    fn cells_do_not_overlap() {
        let count = 76;
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
    fn full_76_item_grid_fits_within_ndc_bounds() {
        // The whole point of the 10-column redesign: at 76 items the grid
        // must not overflow the -1..1 NDC range the way the original
        // 5-column layout (sized for ~9 items) would have.
        let count = 76;
        for aspect in [1.0f32, 16.0 / 9.0, 4.0 / 3.0] {
            for i in 0..count {
                let (cx, cy, hw, hh) = inventory_cell_rect(i, count, aspect);
                assert!((cx - hw) >= -1.0 && (cx + hw) <= 1.0, "cell {i} out of x bounds at aspect {aspect}");
                assert!((cy - hh) >= -1.0 && (cy + hh) <= 1.0, "cell {i} out of y bounds at aspect {aspect}");
            }
        }
    }

}
