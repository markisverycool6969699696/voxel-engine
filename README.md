# Voxel Engine

A custom, from-scratch voxel sandbox engine — Minecraft-*inspired*, not derived from Minecraft
source, assets, or branding. Personal/hobby project, open source from day one.

No Mojang source, decompiled or otherwise, is referenced anywhere in this project. No Mojang
assets. See [docs/STARTER.md](docs/STARTER.md) for the full project spec.

## Status

Playable core loop, no world generation yet. Right now you get one hand-built demo structure
(platform, border wall, a staircase, a floating block) that you can walk around, look at with a
mouse-controlled camera, and mine/place blocks in.

**Working:**
- Vulkan 1.3 renderer (dynamic rendering, depth testing, back-face culling) with a placeholder
  procedurally-generated texture atlas
- Palette-compressed chunk storage and greedy meshing
- Physics: gravity, AABB collision, jumping
- Mining and placing blocks via voxel raycasting, with a basic hotbar
- Data-driven block/item definitions (JSON) — built and tested, not yet wired into the playable game
- Background chunk streaming (multi-worker, load/unload by radius) — built and tested, not yet
  wired into the playable game

**Not yet built:**
- World generation (terrain, biomes) — a real generator is the next major milestone
- Sound (blocked — see [Known Issues](#known-issues))
- Textures/inventory/creative mode/multiplayer — see `docs/STARTER.md` §8 for open decisions

See [MEMORY.md](MEMORY.md) for the full development log, and [project.md](project.md) for planning
notes.

## Controls

| Input | Action |
|---|---|
| `W` `A` `S` `D` | Move |
| Mouse | Look |
| `Space` | Jump |
| `Shift` | Sprint |
| Left click | Mine (break) the targeted block |
| Right click | Place the selected block |
| `1`–`4` | Select hotbar block |
| `Esc` | Quit |

## Building

Requires a Rust toolchain (stable) and a Vulkan-capable GPU/driver.

```
cargo build
cargo run -p game
```

Run tests with `cargo test --workspace`.

### Windows toolchain note

This project currently builds with the `stable-x86_64-pc-windows-gnu` Rust toolchain rather than
MSVC, because MSVC Build Tools weren't available on the primary dev machine when the project
started. `.cargo/config.toml` has GNU-specific linker workarounds as a result — see
[MEMORY.md](MEMORY.md)'s Environment Notes for details if you hit link errors. If you have MSVC
Build Tools installed, the `stable-x86_64-pc-windows-msvc` toolchain should also work (untested as
of this writing).

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
               registry, chunk streaming
render-vk/     Vulkan rendering backend
game/          binary crate tying engine-core + render-vk together
```

## Known issues

- **Sound is unimplemented.** An attempt using `rodio` crashed the binary on startup under the
  GNU/LLVM-MinGW toolchain (likely a `windows-rs`/GNU-target ABI incompatibility in `cpal`, its
  audio backend). Full details and a planned fix (switch to the MSVC toolchain) are in
  [MEMORY.md](MEMORY.md).
- Back-face culling is enabled but hasn't been independently re-confirmed by eye since being
  turned on (it *should* be a no-op visually if correct).

## License

[AGPL-3.0-or-later](LICENSE). Third-party crate dependencies are MIT/Apache-2.0. No third-party
assets are bundled yet; see [CREDITS.md](CREDITS.md) for tracking once any are added.
