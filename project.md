# Voxel Engine Project — Plan

Source of truth for full requirements: `STARTER.md` (to be moved into this repo root or `/docs` once scaffolding begins).

## Status
Checkpoint 1 (2026-07-20) DONE: Cargo workspace (`engine-core`, `render-vk`, `game`), winit 0.30 window,
Vulkan 1.3 init via ash 0.38 — instance/surface/device/swapchain, dynamic rendering + sync2,
2 frames in flight, per-image render_finished semaphores, lazy swapchain recreation, clear-screen loop.
Verified running on Arc 140T (Vulkan 1.4 driver). Toolchain: stable-gnu (no MSVC Build Tools on machine;
LLVM-MinGW linker + rustup self-contained libs via `.cargo/config.toml`).
Uncommitted — repo has git init only.

Checkpoint 2 (2026-07-20) DONE: graphics pipeline + real vk-mem vertex buffer drawing a triangle.
See `MEMORY.md` for full running log/detail.

Next: chunk data structure → greedy meshing → world gen (Fable-tier per `forfab.md`).

## Confirmed Decisions

1. **Working directory:** `C:\Users\mark2\ClaudeMCFR` is the repo location. `STARTER.md` (currently in `Downloads`) should move into the repo (root or `/docs`) so it's version-controlled with the code.

2. **First milestone / bring-up order:**
   windowing + device init → triangle/basic mesh render → chunk data structure → greedy meshing → world gen.

3. **Workspace structure:** Cargo workspace, split by subsystem:
   ```
   engine-core/   (chunk system, physics, world gen — platform-agnostic)
   render-vk/     (Vulkan backend)
   render-mtl/    (Metal backend)
   game/          (binary crate tying it together per-platform)
   ```
   A shared `Renderer` trait lives in `engine-core`; `render-vk` and `render-mtl` each implement it. No wgpu — native per-platform backends only.

4. **Platform bring-up order:** Vulkan-on-Windows first (primary dev machine, faster iteration), then port to Metal/macOS (iMac, tighter RAM/perf constraints) once the architecture is validated.

## Not Yet Done
- git init
- Cargo workspace scaffold
- LICENSE (AGPLv3)
- CREDITS.md
- winit + ash window/device init checkpoint

These were planned but explicitly paused — user wants Opus to do the actual coding, not have it scaffolded automatically.

## Reference
Full spec (tech stack, memory/perf budgets, chunk/data/rendering/physics architecture, licensing, AI-tiering workflow, world-gen approach, open items, non-goals): see `STARTER.md`.
