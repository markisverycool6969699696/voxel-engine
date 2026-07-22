# Credits / Asset Licenses

Every asset added to this project must be listed here with its source, author, and
license (CC0 or GPL-compatible only).

| Asset | Source | Author | License |
|---|---|---|---|
| Block textures (74 PNGs, `render-vk/assets/textures/blocks/`) | [github.com/elhedran/SimpleRP](https://github.com/elhedran/SimpleRP) (commit `341c49484fc8b7138d6d5ff60f48fcf59fb4bf81`), rendered from the pack's SVG sources at 16px | elhedran | GPL v2 (full text: `render-vk/assets/textures/LICENSE-SimpleRP`) |

SimpleRP is meant to supplement vanilla Minecraft's own textures, not fully replace
them — it doesn't cover every block this project has (no water/snow/lava
equivalents), so those specific blocks still use a flat procedural color rather than
a real texture. See `engine_core::mesh::ATLAS_TILES` for exactly which block ids map
to which texture file vs. a solid-color fallback.
