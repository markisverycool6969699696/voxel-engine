# Voxel Engine Project — Starter Document

## 1. Project Overview

A custom, from-scratch voxel sandbox engine (Minecraft-*inspired*, not derived from Minecraft source) built for:
- Personal/hobby use, open source from day one
- Two primary target platforms with very different hardware profiles
- A focus on performance, low memory footprint, and clean architecture over feature bloat at launch

**Explicitly out of scope / not permitted in this project:**
- No Mojang source code, decompiled or otherwise, is to be referenced, viewed, or used at any point
- No Mojang assets (textures, sounds, branding, logos) — original or CC0/GPL-licensed community assets only
- No word-for-word or near-literal porting of any third-party proprietary game logic

---

## 2. Target Hardware

| Platform | Hardware | OS | Graphics API |
|---|---|---|---|
| Primary dev machine | Intel Ultra 7 255H + Arc 140T, 32GB DDR5 | Windows | Vulkan |
| Secondary / real-world performance target | 2020 iMac 27" 5K, Radeon Pro 5500M | macOS | Metal |
| Future stretch goal | — | Linux | Vulkan (shared with Windows) |
| Future stretch goal | — | Android | Vulkan (shared, separate surface/input layer) |

**Memory budget target:** under 3GB RAM for the game process itself (excluding OS overhead), to comfortably fit within the iMac's constraints.

**Resolution/performance target:** 60fps stable (locked 1% lows) at the native resolution each GPU can sustain; upscale to fill higher-resolution displays (e.g. the iMac's 5K panel) rather than rendering natively at 5K everywhere.

---

## 3. Tech Stack

- **Language:** Rust (primary), with willingness to learn C++ concepts as needed since libraries/tools may reference either
- **Rendering backends:** Vulkan (`ash`) for Windows/Linux/Android, Metal for macOS — native per-platform, no cross-platform abstraction layer (no wgpu), for maximum control and learning value
- **Math:** `glam` or `nalgebra`
- **Windowing/input:** `winit`
- **Memory allocation helper (Vulkan):** `vk-mem` or equivalent, rather than hand-rolled allocator
- **Audio:** existing library (e.g. `rodio`/`kira`), not built from scratch

---

## 4. Core Architecture Decisions

### 4.1 Chunk System
- Vertical streaming: chunks initially load only a shallow surface slice (e.g. ~10 blocks deep); deeper slices generate on-demand as a player digs past a threshold, on background threads
- Horizontal streaming: standard render-distance-based loading/unloading
- World generation is fully deterministic from seed — unmodified chunks are **never stored**, only regenerated on demand
- **Only player-modified chunks are persisted** (diff-based saves), drastically reducing world file size and RAM usage

### 4.2 Data Storage / Compression
- Palette compression per chunk section: small per-chunk palette of unique block types + low-bit-width indices, instead of a full ID per block
- Run-length encoding and/or general compression (zstd/zlib) on top for on-disk storage
- In-memory chunk data also kept compressed where feasible; decompressed only during meshing

### 4.3 Rendering
- Greedy meshing: merge adjacent identical exposed faces into larger quads to minimize triangle count
- Draw call batching: group geometry by texture atlas to minimize draw calls per chunk
- Frustum culling with a small margin buffer (~5-10°) to prevent pop-in on fast camera movement
- Level of detail (LOD) systems:
  - Distance-based mip/texture resolution scaling
  - Velocity-based LOD: reduce texture resolution and render depth when player movement speed exceeds a threshold (with hysteresis to avoid flicker at the boundary)
  - Altitude-based culling: skip terrain rendering entirely above a cloud/sky threshold, with a fade transition near the boundary
- Dynamic/adaptive internal render resolution with upscaling to fill higher-resolution displays

### 4.4 Physics
- Fixed timestep physics loop, decoupled from variable render framerate, to avoid tunneling/clipping under frame hitches
- Physics simulates based on proximity to the player (a simulation radius), **not** camera visibility — off-screen loaded chunks still simulate correctly
- Simulation radius may be smaller than render/view radius to bound CPU cost

### 4.5 Items / Blocks (Data-Driven Design)
- Items and blocks defined as data (JSON or similar), not hand-coded per-item classes
- A small number of generic systems (mining, stacking, placement, crafting) read shared fields (hardness, stack size, solidity, etc.)
- Special behaviors (fluids, crops, redstone-like logic) handled via a limited set of shared "behavior tag" systems rather than unique code per item
- This design is intended to scale to hundreds/thousands of items without proportional code growth

---

## 5. Licensing

- **Engine/code license:** AGPLv3 (chosen for maximum copyleft strength, including network-use provisions, in case multiplayer/server features are added later)
- **Third-party libraries:** MIT/Apache-2.0-licensed crates are compatible and freely used (e.g. glam, winit)
- **Assets:** GPL-compatible or CC0-licensed community texture/sound packs only; each asset's source and license to be tracked in a `CREDITS.md` / `ASSETS_LICENSE.md`
- Repository will include a full `LICENSE` file (AGPLv3 text) at the root

---

## 6. AI-Assisted Development Workflow

This project is being built with AI assistance across multiple models, divided roughly by task risk/complexity:

| Model tier | Responsibility |
|---|---|
| Highest-capability tier (e.g. Fable 5) | Foundational, high-risk-of-subtle-failure work: raw Vulkan/Metal initialization, synchronization, greedy meshing core algorithm, chunk streaming/threading architecture, palette compression implementation |
| Strong reasoning tier (e.g. Opus) | Reviewing/hardening the foundation, terrain generation pipeline, biome blending, pathfinding around partially-loaded chunks |
| Mid tier (e.g. Sonnet) | Ongoing feature work on the established foundation: textures/atlas, player movement, sound integration, mob AI basics, creative mode, item system implementation, debugging, documentation |
| Fast/lightweight tier (e.g. Haiku/Flash-class models) | High-volume repetitive tasks: bulk item/block data entry, boilerplate code, doc comments |

AI-assisted commits may include standard co-author attribution in the repository as a transparency practice.

---

## 7. World Generation Approach (Public/General Technique — Not Derived From Any Proprietary Source)

General layered-noise terrain generation approach, to be implemented independently:
1. Multi-octave noise (Perlin/Simplex/OpenSimplex — public domain techniques) for base terrain shape
2. Noise output mapped to per-column height values
3. Separate noise maps for temperature/humidity, blended into biome assignment, influencing surface blocks and terrain variation
4. Secondary 3D noise field, thresholded, for cave/overhang carving
5. Deterministic, seeded structure placement (trees, ore veins, etc.)
6. Chunk-based generation supporting infinite/streaming worlds

All specific tuning constants, thresholds, and creative decisions in this system are to be original.

---

## 8. Open Items / Not Yet Decided

- Exact Rust vs. C++ split, if any, for specific subsystems
- Multiplayer/networking architecture (if pursued)
- Final list of initial block/item set for v1
- Specific CC0/GPL asset packs to be sourced
- Android/Linux port timeline

---

## 9. Explicit Non-Goals for v1

- Not attempting to reproduce Minecraft's exact terrain generation output — aiming for a structurally similar, independently-tuned result
- Not targeting commercial release at this stage
- Not including Mojang branding, names, or assets in any form
