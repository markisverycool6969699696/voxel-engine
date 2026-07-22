# Voxel Engine

A custom, from-scratch voxel sandbox engine — Minecraft-*inspired*, not derived from Minecraft
source, assets, or branding. Personal/hobby project, open source from day one.

No Mojang source, decompiled or otherwise, is referenced anywhere in this project. No Mojang
assets. See [docs/STARTER.md](docs/STARTER.md) for the full project spec.

## Status

Playable, with seeded infinite terrain that streams in as you move. Coherent heightmap landscape —
rolling hills and mountains, beaches and water at sea level, biomes (plains, forest, desert,
snowy), underground caves, ore, and trees — all generated deterministically from a seed and
regenerated on demand rather than stored. You spawn on the surface and can walk, fly, and
mine/place blocks in it.

**Working:**
- Vulkan 1.3 renderer (dynamic rendering, depth testing, back-face culling) with a placeholder
  procedurally-generated texture atlas
- Palette-compressed chunk storage and greedy meshing
- **Seeded world generation**: layered value-noise heightmap terrain, temperature/humidity biomes
  (with blended elevation), water below sea level, 3D-noise caves, ore, and trees — fully
  deterministic (unmodified chunks are regenerated, never stored)
- Background chunk streaming (multi-worker, load/unload by radius) driving the generator
- Physics: gravity, AABB collision, jumping, and creative-mode flight (`F` to toggle)
- Mining and placing blocks via voxel raycasting, with a hotbar backed by the data-driven
  block/item registry (JSON)
- Synthesized sound effects for mining/placing (placeholder tones, not real samples — see
  [Known Issues](#known-issues))
- A couple of placeholder mobs (gravity + collision, rendered as solid-color boxes) that
  **pathfind toward the player** using voxel A* — and, crucially, refuse to route through
  not-yet-loaded chunks, falling back to wandering when there's no valid path. Not a real mob
  roster, but the movement/collision/pathfinding/rendering path is all real.

**Not yet built:**
- Real textures (the atlas is still procedural placeholder colors per block id) and an inventory
  UI — see `docs/STARTER.md` §8 for open decisions
- Fluid behavior (water is currently a solid, walkable block, not a flowing fluid), multiplayer,
  the macOS/Metal backend

See [MEMORY.md](MEMORY.md) for the full development log, and [project.md](project.md) for planning
notes.

## Controls

| Input | Action |
|---|---|
| `W` `A` `S` `D` | Move |
| Mouse | Look |
| `Space` | Jump (ascend, while flying) |
| `Shift` | Sprint |
| `F` | Toggle creative-mode flight |
| `Ctrl` | Descend (while flying) |
| Left click | Mine (break) the targeted block |
| Right click | Place the selected block |
| `1`–`4` | Select hotbar item |
| `Esc` | Free the cursor (click back in, or press `Esc` again, to resume) |
| Alt+F4 / window close button | Quit |

## Building

Requires a Rust toolchain (stable) and a Vulkan-capable GPU/driver.

```
cargo build
cargo run -p game
```

Run tests with `cargo test --workspace`.

### Windows toolchain note

This project builds with the MSVC Rust toolchain (`stable-x86_64-pc-windows-msvc`, pinned via
[rust-toolchain.toml](rust-toolchain.toml)) — you'll need
[MSVC Build Tools](https://visualstudio.microsoft.com/visual-studio/) (the "Desktop development
with C++" workload) installed. The project briefly used the GNU/LLVM-MinGW toolchain instead
before MSVC was available on the primary dev machine; that path required manual linker
workarounds and, worse, caused the audio backend to crash on startup (a `windows-rs`/GNU-target
ABI issue) — see [MEMORY.md](MEMORY.md) for the history if you're curious.

## Tech stack

- **Language:** Rust
- **Rendering:** native Vulkan (`ash`) on Windows/Linux; no `wgpu` — a native Metal backend is
  planned for macOS, not yet started
- **Math:** `glam`
- **Windowing/input:** `winit`
- **GPU memory allocation:** `vk-mem`
- **Shaders:** WGSL, cross-compiled to SPIR-V in-process via `naga` (no Vulkan SDK/glslc
  dependency)

## Workspace layout

```
engine-core/   platform-agnostic: chunk storage, meshing, physics, camera, raycasting,
               registry, chunk streaming, world generation, mob AI
render-vk/     Vulkan rendering backend
game/          binary crate tying engine-core + render-vk together (+ data/ block/item JSON)
```

## Known issues

- Textures are placeholder: each block id hashes to a flat-colored atlas tile, so grass/water/etc.
  aren't their "expected" colors yet. Terrain *shape* is real; block *coloring* is a stand-in until
  a real asset set is chosen (`docs/STARTER.md` §8).
- Water is a solid, walkable block, not a flowing fluid — real fluid behavior (the `fluid`
  behavior tag exists in the registry for it) is future work.
- Sound is synthesized placeholder tones (no real sound assets — see the open decision in
  `docs/STARTER.md` §8), not a bug, just not "real" content yet.
- ~~Back-face culling not independently re-confirmed by eye~~ — it was actually backwards
  (`FrontFace` was set to the wrong winding, culling the near faces instead of the far ones,
  which read as the world rendering inside-out once real terrain made orientation unambiguous).
  Fixed; see MEMORY.md for the full writeup.

## License

[AGPL-3.0-or-later](LICENSE). Third-party crate dependencies are MIT/Apache-2.0. No third-party
assets are bundled yet; see [CREDITS.md](CREDITS.md) for tracking once any are added.
