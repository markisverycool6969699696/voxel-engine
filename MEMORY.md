# Project Memory / Running Log

Updated after each completed task. See [project.md](project.md) for the plan/decisions and
[docs/STARTER.md](docs/STARTER.md) for the full spec.

## Current Status (2026-07-20)

- **Checkpoint 1 — DONE:** Cargo workspace (`engine-core`, `render-vk`, `game`), winit 0.30 window,
  Vulkan 1.3 init via ash 0.38 (instance/surface/device/swapchain), dynamic rendering + sync2,
  2 frames in flight, per-swapchain-image `render_finished` semaphores, lazy swapchain recreation,
  clear-screen loop. Verified running on Arc 140T (Vulkan 1.4 driver, no crash, clean shutdown).

- **Checkpoint 2 — DONE:** graphics pipeline drawing a triangle from a real GPU vertex buffer.
  Shaders in WGSL (`render-vk/shaders/triangle.wgsl`), compiled to SPIR-V at renderer init via
  `naga` (`render-vk/src/shader.rs`) — no Vulkan SDK/glslc on this machine. Vertex buffer allocated
  via `vk-mem` (host-mapped, no staging buffer yet — fine at 3 vertices, revisit for real meshes),
  bound in `record_frame`, pipeline vertex input state describes the real `Vertex` layout
  (`position: [f32;2]`, `color: [f32;3]`) instead of shader-hardcoded positions. Runs clean, no
  validation-layer errors, 6s soak with no crash. Note: no explicit user visual confirmation of
  pixels on screen was given for either the original hardcoded triangle or this vertex-buffer
  version — user moved forward on the strength of "no crash + no validation errors" alone.

- **Checkpoint 3 — DONE (Fable):** palette-compressed chunk storage in
  `engine-core/src/chunk.rs`. `PalettedSection`: 16³ blocks, per-section palette + bit-packed
  indices (exact `ceil(log2(len))` widths, non-straddling u64 packing), uniform sections are
  allocation-free (bits=0, no index data), `set` grows palette/width via repack, `compact()`
  prunes stale palette entries (explicit call, O(volume)). `ChunkColumn`: sparse BTreeMap of
  sections keyed by section-y for vertical streaming; world-y `get` with euclid div/rem for
  negatives. 11 unit tests pass incl. 20k-op fuzz vs dense mirror and width-growth to 9 bits.
  Not wired to meshing/rendering yet — next subsystem.

- **Checkpoint 4 — DONE (Sonnet):** data-driven block/item registry in `engine-core/src/registry.rs`,
  per spec §4.5. `BlockDef`/`ItemDef` are JSON-deserialized (serde), indexed by both numeric id and
  string key; `BehaviorTag` enum (fluid, crop, gravity, flammable, interactable, powerable) is the
  shared vocabulary generic systems (mining/placement/crafting — not yet built) will query against
  instead of per-block match arms. `Registry<T>` is generic over `Definition + HasTags`, shared
  between blocks and items. Rejects duplicate ids/keys at load. 6 new tests (17 total in
  engine-core, all passing). Deliberately did NOT create a real `data/blocks.json` v1 content list —
  STARTER.md §8 explicitly flags "final list of initial block/item set for v1" as an open decision,
  not mine to make; tests use inline JSON fixtures instead.
  Chose this over continuing the render pipeline because greedy meshing (the next item in the
  render bring-up order) is Fable-tier per `forfab.md`, and this was independent, spec'd, Sonnet-tier
  work that didn't need to wait.

- **Checkpoints 5+6 — DONE (Fable, final session; all forfab.md scope complete):**
  - **Greedy meshing** (`engine-core/src/mesh.rs`): section → merged `Quad`s (corners CCW-from-
    outside, normal, BlockId). 6-direction sweep, per-slice 16×16 mask, greedy rectangle merge;
    merges only identical BlockIds; opacity via caller closure (decoupled from registry);
    out-of-section neighbors treated as air (cross-section culling = later renderer concern).
    9 tests: uniform/single/merge/no-merge-across-materials/checkerboard/winding + fuzz asserting
    total quad area == brute-force exposed face count.
  - **Chunk streaming** (`engine-core/src/streaming.rs`): `ChunkManager` + worker pool
    (std mpsc, shared-Mutex job queue), deterministic `ChunkGenerator` trait. Horizontal radius
    load + hysteresis-margin unload; vertical: eager surface slice (`initial_sections`) +
    `ensure_depth` for digging; `pump()` per tick integrates results on game thread (chunk data
    never locked); unmodified columns discarded on evict (regen = storage), modified ones handed
    out via `drain_evicted_modified` for the future save system; late gen results never clobber
    player edits. 8 tests incl. reload-determinism and 4-worker smoke.
  - engine-core: 34/34 tests pass, workspace builds clean, zero warnings.
  - **Fable's remit (forfab.md) is now fully done** — Vulkan init, greedy meshing, chunk
    streaming/threading, palette compression all exist. Remaining work is other tiers':
    renderer wiring/camera (Sonnet), terrain gen pipeline (Opus per tiering), etc.

- **Checkpoint 7 — code done, awaiting user visual confirmation (Sonnet):** wired greedy-meshed
  chunk data into an actual 3D-rendered scene with a flyable camera — the "renderer wiring" item
  handed off from Fable's completion note.
  - `engine_core::camera::Camera` (new `camera.rs`): free-fly yaw/pitch camera, `glam`-based
    view/projection. Verified via unit tests (not just visual guessing) that `glam::Mat4::
    perspective_rh` already outputs Vulkan-range depth `[0,1]` — confirmed by reading glam's
    source, not assumed from the function name — so the clip-space correction only needed to flip
    Y, not remap Z (an earlier version double-applied the Z remap and failed its own near/far-plane
    test, which is exactly why that test existed). 6 tests incl. near/far depth mapping and
    world-up-is-screen-up-in-Vulkan-NDC.
  - `engine_core::mesh::MeshVertex` + `triangulate()`: quads → indexed triangle list, CCW winding
    preserved, debug per-block-id color (hash-based, no atlas yet) with flat up/down/side shading
    (no lighting system — fixed "sun from above" multiplier only). 2 more tests.
  - `engine_core::Renderer` trait extended: `render_frame` now takes `&Camera`; new `set_mesh`
    replaces the drawn geometry wholesale (no per-chunk placement/instancing yet — world position
    is baked into vertex data by the caller).
  - `render-vk`: depth buffer (format probed via `get_physical_device_format_properties`,
    D32_SFLOAT preferred, falls back to D24_UNORM_S8_UINT/D16_UNORM), recreated alongside the
    swapchain; per-frame-in-flight uniform buffer + descriptor set for the view-proj matrix,
    written after the frame's fence wait (so no race with the GPU still reading the previous use of
    that frame slot's buffer — verified by re-reading the wait/record/submit ordering, not just
    assumed); `set_mesh` uploads real vertex/index buffers (host-mapped `vk-mem`, same pattern as
    the checkpoint-2 vertex buffer), replacing the hardcoded 2D triangle entirely.
  - Old `shaders/triangle.wgsl` (2D, hardcoded positions) replaced by `shaders/mesh.wgsl` (3D,
    reads a `Globals` uniform for view_proj).
  - **Known open item:** back-face culling is still OFF (`CullModeFlags::NONE`). Worked through the
    winding math (CCW-from-outside quads × right-handed view/projection × our Y-flip ⇒
    `FrontFace::CLOCKWISE` should be the correct culling direction) but left it disabled since a
    wrong direction fails silently. Rendering is now confirmed working correctly with culling off
    (see below), so enabling it is a pure perf optimization to try later, not a correctness gate.
  - `game/src/main.rs`: hardcoded demo structure (platform + border + center staircase + one
    floating block, no world gen).

- **VISUALLY CONFIRMED WORKING by user (2026-07-20):** camera, mesh rendering, depth, and controls
  all correct — "the controls are fine, i am able to place and break blocks." This confirms
  checkpoints 7 and 8 end-to-end (camera/projection math, greedy-mesh → GPU pipeline, physics
  collision, and raycast-based mining/placing all functioning together), closing out the
  "unverified foundation" risk flagged below.

- **Checkpoint 8 — DONE, confirmed (Sonnet):** replaced the free-fly camera with an actual
  physics-driven player, and added mining/placing. User explicitly said "just need the game" and to
  keep going without waiting for Opus (which is reserved for terrain gen specifically) — this is
  the gameplay-feel work that doesn't touch world-gen content at all.
  - `engine_core::physics` (new): axis-separated (X, then Z, then Y) AABB-vs-voxel collision,
    swept in small fixed ~5cm steps rather than solved analytically for the exact contact point —
    deliberate: analytic boundary math is easy to get subtly wrong at exact-integer edges/corners,
    stepping trades a tiny bounded position error for something straightforward to test exhaustively.
    `PlayerController`: gravity, jump, terminal velocity, ground detection (down-collision with
    velocity.y ≤ 0 checked *before* zeroing it, so hitting a ceiling mid-jump doesn't falsely set
    on_ground). 6 tests incl. "does not tunnel through the floor at high fall speed" and "stops
    exactly at a wall boundary, never past it."
  - `engine_core::raycast` (new): Amanatides–Woo voxel DDA traversal for mining/placing target
    selection, hard step-count bound so a logic bug degrades to "misses" rather than hanging.
    8 tests incl. axis-aligned (zero-division-risk) rays, diagonal rays, and "origin's own cell is
    never reported as a hit."
  - `game/src/main.rs`: player replaces the fly camera (gravity/collision/jump via WASD+Space,
    mouse-look still direct); horizontal movement uses yaw only, not pitch (looking down doesn't
    walk you into the ground); left-click mines (raycast hit → set AIR), right-click places
    (raycast hit + face normal → adjacent cell), blocked if the target cell would overlap the
    player's own AABB; world edits trigger a full remesh (`device_wait_idle` + buffer replace via
    `set_mesh` — fine for "on click," not for high-frequency edits). 1-4 hotbar keys select which
    block to place (still debug-hash-colored ids, no real content/inventory).
  - engine-core: 56/56 tests pass, workspace builds clean, zero warnings, 5-6s runtime soaks with
    no crash/validation errors after each change.
  - Paused after this to get user visual confirmation before stacking more systems on unverified
    rendering — see the confirmation entry above. That gate has now passed.

- **Checkpoint 9 — DONE (Sonnet):** back-face culling enabled (`CullModeFlags::BACK` +
  `FrontFace::CLOCKWISE` in `render-vk/src/lib.rs`'s pipeline). Was left off since checkpoint 3
  pending visual confirmation of the winding math; that confirmation happened (checkpoint 8's
  entry above), so this is now a pure perf win. Not yet independently re-confirmed visually with
  culling specifically on — nothing should disappear if the reasoning holds, worth a glance.

- **Sound integration — BLOCKED, reverted (Sonnet):** attempted `rodio` 0.22 (procedurally
  synthesized tones for mine/place feedback — no sound assets exist, so this avoided the
  asset-licensing decision STARTER.md §8 leaves open, same reasoning as the debug block colors).
  Implementation was code-complete and built clean, but **crashed the whole game on launch**
  (`STATUS_ACCESS_VIOLATION`, `cargo build` succeeds, event log shows the fault inside `game.exe`
  itself, offset ~0xc8xxxx, "unknown" faulting module). Isolated by bisection: crash reproduces even
  with `Audio::new()` never called — merely *linking* `rodio`'s `cpal`→`windows` (v0.62.2)
  dependency into the binary is enough to crash at/near process startup, before `main()`'s own logic
  runs. Removing the `rodio` dependency entirely (confirmed via a clean rebuild) restores the known-
  good, crash-free build. Root cause is almost certainly this project's unusual toolchain (see
  Environment Notes: no MSVC Build Tools, `stable-x86_64-pc-windows-gnu` + LLVM-MinGW +
  `link-self-contained` + manual `-lc++`) being incompatible with `windows-rs` v0.62's Windows API
  bindings on the GNU target — `windows-rs` has historically had GNU-target ABI issues (COM
  vtables, exception handling assume MSVC conventions). **Fully reverted**: `game/src/audio.rs`
  deleted, all call sites removed, `rodio` dependency line removed from `game/Cargo.toml`,
  `Cargo.lock` regenerated via clean rebuild. engine-core's 56 tests still pass, workspace builds
  clean with zero warnings, runtime soak confirms no crash.
  **To revisit sound:** either (a) get MSVC Build Tools installed and switch to the
  `stable-x86_64-pc-windows-msvc` toolchain (removes the underlying GNU-target-ABI risk entirely,
  and was the original recommended path before the GNU toolchain was chosen for expedience), or
  (b) try `kira` (the spec's other suggested audio crate) or an older `cpal`/`windows` version
  in case a specific version is the culprit — untested, no strong reason to expect it fixes the
  ABI-level issue though.

- **Checkpoint 10 — DONE, confirmed (Sonnet):** placeholder texture atlas. No real block textures
  exist (spec §8 open decision), so `render-vk::generate_atlas_pixels` procedurally generates a
  16-tile atlas (hashed base color per tile + coarse 4x4 checker so it reads as "textured") instead
  of loading images — deliberately zero new external crates, given the `rodio` incident just above.
  - `engine_core::mesh`: `MeshVertex.color` replaced with `uv`/`tile`/`shade`; `ATLAS_TILE_COUNT`
    constant is the single source of truth both crates read (no duplicated magic number between
    Rust and WGSL). UV is always the unit square per corner — texture stretches across a merged
    quad rather than repeating per-block; documented as a deliberate simplification, not an
    oversight (correct per-block tiling needs shader-side `fract()`, skipped for this pass).
  - `render-vk`: atlas image uploaded once at init via staging buffer + one-shot command buffer
    (standard device-local-image pattern); texture + sampler are **separate** descriptor bindings
    (`SAMPLED_IMAGE` + `SAMPLER`), not a combined-image-sampler — WGSL/naga has no combined type,
    so the Vulkan layout mirrors what naga's SPIR-V backend actually emits rather than fighting it.
    Descriptor set layout/pool grew from 1 to 3 bindings; `GlobalsUbo` (Rust-side mirror of the WGSL
    `Globals` struct) now carries `atlas_tile_count` alongside `view_proj`, padded to 80 bytes to
    match WGSL's uniform-layout size rules explicitly rather than hoping `#[repr(C)]` happens to
    agree. `mesh.wgsl` rewritten: samples `atlas_tex` at `(tile + uv.x) / atlas_tile_count, uv.y`.
  - 57/57 engine-core tests pass, workspace builds clean zero warnings.
  - **Hit an unrelated environment blocker while verifying this**: Windows Smart App Control
    started blocking `game.exe` from running at all ("Application Control policy has blocked this
    file") — not a code issue, confirmed via `cargo build`/`test` staying clean throughout. User
    resolved it (turned off Smart App Control). **After that, runtime-confirmed working**: 6s soak,
    no crash, no Vulkan validation errors, GPU detected correctly. Visual "does it actually look
    textured" confirmation from the user is still open (only crash/validation-error checked here).

- **Sound integration — DONE, FIXED (Sonnet):** the MSVC-toolchain theory from checkpoint 8 was
  correct. `rustup override set stable-x86_64-pc-windows-msvc` (repo-local — doesn't touch the
  global rustup default, so other GNU-dependent work elsewhere on this machine is unaffected) plus
  the now-installed VS Build Tools gave a clean MSVC build with **no linker workarounds needed at
  all** (unlike the GNU toolchain's `link-self-contained` + hand-resolved `-lc++` path). Verified
  in order, not skipped: (1) existing non-audio code builds/tests/runs clean under MSVC alone —
  57/57 tests, no-crash runtime soak; (2) `game/src/audio.rs` + `rodio` dependency re-added
  (identical to the reverted attempt — nothing conceptually changed, just the toolchain); (3) full
  rebuild clean, 57/57 tests pass, **`cargo test -p game` no longer crashes either** (it did before,
  same `STATUS_ACCESS_VIOLATION` root cause); (4) 6s runtime soak, no crash, no validation errors.
  Confirms the original diagnosis: `windows-rs` (pulled in via `rodio`→`cpal`) targets MSVC ABI and
  was never going to work reliably on GNU/LLVM-MinGW. Game launched for the user to actually
  see/hear — audible confirmation that the synthesized mine/place tones actually play is still
  from the user directly, not something I can verify myself.
  - Also did a full manual audit of `render-vk`'s Vulkan resource lifecycle while here (every
    `create_*`/`allocator.create_*` call cross-checked against a matching destroy, and the
    `Drop` teardown order re-verified: allocator-owned resources destroyed before
    `ManuallyDrop::drop(&mut self.allocator)`, which itself runs before `destroy_device`). Found
    **no leaks or use-after-free risks** — every resource is paired, and partial-construction
    failure paths are safe (Vulkan/vk-mem both explicitly tolerate destroying null handles, and
    `std::mem::zeroed()`-initialized `vk_mem::Allocation`s are additionally guarded by explicit
    null checks before use). No `TODO`/`FIXME` left in the codebase either. Not exhaustive (didn't
    re-derive every physics/raycast edge case from scratch — those already have dedicated test
    coverage from when they were built), but the highest-leak-risk area (manual GPU resource
    management) is now independently checked, not just "written carefully and hoped."
  - Both the GNU and MSVC toolchains remain installed; only this repo is pinned to MSVC via the
    local override (`rustup override list` shows it, doesn't affect other projects).
  - `.cargo/config.toml`'s GNU-target rustflags (scoped under `[target.x86_64-pc-windows-gnu]`)
    are moot for this repo now that it's pinned to MSVC — confirmed by the clean MSVC build not
    needing or triggering them at all, not just by reading the `[target]` scoping.

- **Repo state:** pushed to GitHub — https://github.com/markisverycool6969699696/voxel-engine
  (public, AGPLv3). Sound-integration commit landed (`fb15671`), README updated (`7b66baa`).
  MSVC targeting is committed via `rust-toolchain.toml`, so a fresh clone builds correctly without
  any local `rustup override` state.

- **Checkpoint (2026-07-21, Sonnet):** wired `engine_core::streaming::ChunkManager` into
  `game/src/main.rs` — the game now streams a multi-column world (load/unload by radius around the
  player) instead of editing one hardcoded section directly. Mesh rebuild merges every loaded
  column's sections into one combined buffer (world-offset by `cx/cz/sy * 16`); mining/placing
  route through `ChunkManager::set_block` via world↔chunk coordinate math; `ChunkManager::set_center`
  is called every frame off the player's chunk position (no-op if unchanged). Added
  `ChunkManager::columns()` (iterate the loaded set) to `engine-core/src/streaming.rs` — the one
  new engine-core API surface this needed, with a test.
  - **Did not write a terrain generator.** `DemoGenerator` (in `game/src/main.rs`) places the same
    fixed hand-built demo structure at the origin column and returns empty air for every other
    column — zero new world-content decisions, purely there so `ChunkManager` has something
    deterministic to call. Walking far from spawn currently means walking into void (expected: no
    floor exists there yet). Real terrain shape/heightmaps/biomes is still Opus's call.
  - Verified: `cargo test --workspace` 58/58 passing (was 57 — added the `columns()` test).
    `game.exe` soaked 6s clean on the Intel Arc GPU, no panics/crashes in stdout/stderr.
    Committed as `8cb6c90`, pushed.

## Environment Notes
- **This repo now targets the MSVC toolchain** (`rustup override set stable-x86_64-pc-windows-msvc`,
  local to this directory), not GNU — VS Build Tools got installed specifically to fix sound (see
  above). The GNU-toolchain notes below are historical/no longer active for this repo, kept for
  context in case the override ever needs reverting.
- System mingw (LLVM-MinGW) lacks GCC runtime libs → `.cargo/config.toml` sets
  `link-self-contained=yes` to pull them from rustup's `rust-mingw` component instead.
- No Vulkan SDK installed → shaders are WGSL, compiled in-process via `naga`, not glslc.
- `vk-mem` links a C++ (VMA) object built by this same LLVM-MinGW toolchain, which uses libc++
  (`std::__1::...`), not libstdc++ — rustc's default windows-gnu link args only add `-lstdc++`.
  Fixed in `.cargo/config.toml` with an explicit `-L` to the toolchain's `x86_64-w64-mingw32/lib`
  and `-lc++`. **Fragile:** that `-L` path is hardcoded with the current llvm-mingw version string
  (`llvm-mingw-20260602-msvcrt-x86_64`) — if that WinGet package updates, the path breaks and the
  link error `cannot find -lc++` will return; re-resolve via
  `x86_64-w64-mingw32-gcc -print-file-name=libc++.a` if so.

- **Checkpoint (2026-07-21, Sonnet, autonomous session):** worked through the remaining
  engineering-only Sonnet-tier backlog per spec §6 ("creative mode, item system implementation,
  mob AI basics") while the user was away, per their explicit "do everything until Opus/Fable is
  needed" instruction. All committed and pushed individually would have been noisy, so this
  landed as a small number of focused commits — see git log for exact boundaries.
  - **Creative-mode flight** (`engine-core/src/physics.rs`): `PlayerController` gained a `flying`
    bool + `toggle_flying()`. While flying, gravity is off and `wish_dir.y` directly drives
    vertical speed (`FLY_SPEED`); collision stays on (flight, not noclip — still resolved through
    the same `move_and_collide` sweep). 4 new tests. Wired to `F` to toggle, `Space`/`Ctrl` for
    ascend/descend in `game/src/main.rs` (Space keeps meaning jump when not flying).
  - **Mob AI basics** (new `engine-core/src/mob.rs`): generic `Mob` struct — gravity + AABB
    collision via the same `move_and_collide` the player uses, plus a `Wander` behavior (random
    heading for a random 1.5-4s duration, cut short early if it bumps a wall). Includes a small
    deterministic xorshift64 `Rng` (no `rand` dependency, fully reproducible for tests). 8 new
    tests. **No mob roster/species/spawn-rule content** — `game/src/main.rs` spawns exactly two
    fixed-position placeholder mobs on the demo platform, rendered as solid-color boxes (via a new
    `mob_box_mesh` helper that reuses `greedy_mesh`'s tested winding on a synthetic 1-cell section,
    scaled/translated — no new geometry code to get wrong).
  - **Item system wiring** (`game/src/main.rs` + new `game/data/blocks.json`/`items.json`): the
    hotbar now resolves through `Registry<ItemDef>`/`Registry<BlockDef>` (loaded via
    `include_str!` + `load_from_str`) instead of a raw `[BlockId; 4]` array. The JSON data is the
    same 4 debug-colored placeholder blocks that already existed in code, just now flowing through
    the data-driven path — deliberately did **not** invent a "real" v1 item list (still an open
    decision per `docs/STARTER.md` §8).
  - Mesh rebuild is now unconditional every frame (was previously gated on "did the world change")
    — mobs move every frame regardless, so the combined-buffer rebuild has to run every frame
    anyway; at the current small world/mob scale this is still cheap (confirmed via soak test, no
    frame-time regression visible).
  - Verified: `cargo test --workspace` 69/69 passing (was 58 — 4 new flight tests in physics.rs,
    7 new tests in mob.rs). `game.exe` soaked 8s clean on the Intel Arc GPU, no panics/crashes, no
    leftover process after kill. Visual/audio confirmation of the new mobs/flight is still the
    user's to give — computer-use screenshot tooling in this environment doesn't reflect their
    physical screen (established earlier this project), so this is verified by tests + soak-run
    only, not by eye.

- **Checkpoint (2026-07-21, Opus — terrain generation):** the world-gen milestone that was
  reserved for Opus. New `engine-core/src/worldgen.rs`: `TerrainGenerator` implementing
  `ChunkGenerator`, dropped straight into the existing streaming pipeline (no changes needed to
  `ChunkManager`/streaming — the trait boundary Sonnet set up held). Per STARTER.md §7:
  - **Heightmap terrain, not a 3D block field.** Deliberately a contiguous solid heightmap (stone
    to a per-column surface height, thin soil/surface skin, water filling below sea level) so it
    reads as coherent Minecraft-like ground, NOT scattered floating voxels. The only 3D noise is a
    conservative cave carve applied strictly *below* the surface skin, so the visible ground is
    never pockmarked. This was the explicit user ask ("not scattered everywhere").
  - Layered value noise, all hand-written (no `rand`/`noise` crate): splitmix64-hashed
    value-noise + fBm. Height = low-freq continent shape + a squared mountain mask (most of the
    world gentle, occasional real peaks) + fine detail. Biomes from temperature/humidity fBm
    (plains/forest/desert/snowy); surface block is discrete but sits on the continuous height
    field, so biome edges blend in elevation rather than forming cliffs (the "biome blending" ask,
    done on the smooth axis). Caves via 3D fBm upper-tail threshold (sparse tunnels). Ore
    (coal/iron by depth) as sparse underground hashes. Trees stamped with a ±2 column margin so
    trunks/leaves cross section boundaries seamlessly (computed identically from either side —
    determinism makes this safe). SEA_LEVEL=64, terrain clamped y2..122, seed 0x5EED_1234.
  - **Wiring** (`game/src/main.rs`): removed `DemoGenerator` + `build_demo_section` entirely;
    `ChunkManager` now fed `TerrainGenerator`. Player + mobs spawned on the real generated surface
    via `surface_height()`. `STREAMING` bumped to load_radius 4 / initial_sections 0..=7 (full
    y0..127 band so columns load as solid ground) / 3 workers. Startup blocking-load deadline
    raised 2s→10s (real gen is heavier than the void generator was).
  - **Block/item data** (`game/data/*.json`): replaced the 4 debug placeholders with the real
    terrain block set (stone/dirt/grass/sand/water/wood/leaves/snow/bedrock/coal_ore/iron_ore +
    mob_marker id 12; `MOB_BLOCK` moved 5→12 so it doesn't collide with water). Hotbar now places
    stone/dirt/sand/wood. The "final v1 block/item list" (STARTER.md §8) is now substantively
    decided for terrain purposes.
  - **Perf**: full 648-section startup set (9×9×8) generates in ~123ms release single-threaded
    (measured via a throwaway example), so the startup freeze is a fraction of that across 3
    workers — nowhere near the 10s cap.
  - Verified: `cargo test --workspace` 74/74 passing (5 new worldgen tests: determinism,
    solid-surface/open-above, pure-air sky, water-fills-low-areas, trees-only-on-land). `game.exe`
    soaked 15s clean on the Intel Arc GPU, no panics/crashes, no leftover process. As always,
    visual confirmation (does the terrain *look* right/Minecraft-like) is the user's to give — the
    screenshot tooling here doesn't reflect their physical screen. Tests confirm the terrain is
    structurally coherent (solid below, open above, water pooling correctly); eyeballing the
    aesthetics is on the user.

## Next Up
**Terrain generation is done** — the last big reserved-for-Opus milestone. The engine now has a
complete playable loop over real streamed infinite terrain. Remaining work is incremental and
mostly content/polish, not another foundational milestone:
- Real textures (atlas is placeholder flat colors per block id) — needs an asset-pack decision
  (STARTER.md §8), Sonnet-tier once decided.
- Fluid behavior for water (currently a solid walkable block; `BehaviorTag::Fluid` already exists
  in the registry as the hook) — Sonnet-tier.
- Pathfinding mobs (current `mob.rs` is flat random-walk `Wander`; pathfinding around
  partially-loaded chunks is the remaining Opus-tier item per STARTER.md §6 / for-opus/FOROPUS.md).
- Per-chunk GPU mesh buffers, inventory UI, save/load of modified chunks
  (`drain_evicted_modified` is already wired for it), Metal/macOS backend, multiplayer.
- Terrain tuning: the noise constants are a first coherent pass, not tuned to death — biome
  sizes, cave frequency, tree density, mountain sharpness are all easy knobs in `worldgen.rs` if
  the user wants a different feel.
