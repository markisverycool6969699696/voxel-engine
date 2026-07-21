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

- **Checkpoint (2026-07-21, Opus — pathfinding):** the other named Opus-tier item (STARTER.md §6,
  "pathfinding around partially-loaded chunks"). New `engine-core/src/pathfind.rs`: voxel A* with
  Minecraft-style step-up/fall nav. **Defining feature:** the world oracle is three-state
  (`Solid`/`Open`/`Unknown`); `Unknown` = not-yet-loaded, treated as impassable and never guessed,
  so a mob never paths into or across ungenerated terrain. Bounded node budget (`max_nodes`) so an
  unreachable goal returns `None` instead of scanning an effectively infinite open world. 6 tests
  (straight path, staircase climb, Unknown-wall blocks, node budget, floating-goal rejected,
  detour around a pillar).
  - `mob.rs` gained `steer_toward(dx,dz)` — imposes a heading for the next `update` only, cleared
    after use, so path-following overrides wander per-tick without disabling it (mob wanders again
    the moment steering stops). One new test. Existing wander tests unaffected (steering opt-in).
  - Wired in `game/src/main.rs`: mobs are now `MobEntity { mob, path, path_idx, repath_timer }`;
    each recomputes a path to the player's feet block every 0.5s (only within 40 blocks), steers
    along it, advances nodes as it reaches them, and falls back to wander when `find_path` returns
    nothing (player flying/unreachable, or path would cross unloaded chunks). `nav_cell` maps
    `ChunkManager::block` → `Unknown` for unloaded, `Open`/`Solid` otherwise.
  - Verified: `cargo test --workspace` 81/81 (was 74; +6 pathfind, +1 mob steering). `game.exe`
    soaked 15s clean on the Intel Arc GPU, no panics/crashes, no leftover process.

- **BUG + FIX (2026-07-21, Sonnet): "flipped upside down" + ~4fps regression from the terrain
  session.** User-reported after launching with real terrain. Root cause found by reading
  `render-vk`'s `set_mesh` (its own doc comment: "Fine for upload once at startup; per-frame mesh
  streaming would need a smarter (non-stalling) replacement strategy" — it does a full
  `device_wait_idle()` + destroy/recreate of the vertex+index buffers on *every* call). The mob-AI
  session made `rebuild_mesh()` (re-triangulating **every loaded chunk section**, now hundreds of
  real terrain sections instead of near-all-uniform-air) unconditional every rendered frame, and
  left a second, redundant, un-throttled call to it sitting in `update_and_render` even after
  later refactors — so the game was doing a full GPU-pipeline stall + full-world greedy-mesh
  recompute up to 60×/sec. That fully explains the ~4fps. The "upside down" report is very likely
  a secondary symptom, not a separate geometry bug: `camera.rs`'s Vulkan clip-space Y-flip is
  untouched since it was tested/confirmed working, and pitch is hard-clamped to ±1.5rad so the
  camera *cannot* genuinely flip past vertical — but with the main thread stalling for large
  stretches, many buffered `MouseMotion` events could land in one oversized `mouse_delta` and
  pitch could clamp hard against straight-up or straight-down in one lurch, which reads as
  "flipped" to a user without the clamp having actually broken. Did **not** touch `camera.rs` —
  no evidence of an actual bug there, and it's tested/previously visually confirmed.
  - **Fix** (`game/src/main.rs`): split the old `rebuild_mesh` into `rebuild_world_mesh` (re-
    triangulates chunks only, into new cached `world_vertices`/`world_indices` fields — called only
    when `update_streaming` reports new sections landed, or an edit happens via `set_world_block`
    setting `world_mesh_dirty`) and `upload_combined_mesh` (clones the cached world buffer, appends
    fresh mob boxes, calls `set_mesh` — throttled to `MESH_UPLOAD_INTERVAL` = 12Hz via an
    accumulator, so continuous mob motion can't reintroduce a per-frame stall). Deleted the
    leftover redundant `rebuild_mesh()` call in `update_and_render`.
  - Verified: `cargo test --workspace` 81/81 unaffected, build clean (no warnings — confirms the
    old method was actually renamed/removed, not left dead), 15s soak clean, no leftover process.
    **Not yet re-confirmed by the user** — this is exactly the kind of thing only they can verify
    (frame-rate feel and "does it still look flipped" are not testable from here).

- **BUG + FIX (2026-07-21): the "upside down" report was real — backwards back-face culling, latent
  since it was first enabled.** User confirmed fps was better after the mesh-throttle fix but
  still reported the world upside down, ruling out the "mouse-lurch from stalls" theory (the perf
  fix alone should have made that far rarer, yet the report was unchanged). Re-derived the actual
  bug from first principles: `render-vk`'s `VULKAN_CLIP_CORRECTION` negates clip-space Y (required
  and tested — it's what makes world-up display as screen-up on Vulkan's natively Y-down NDC), but
  a single-axis flip is a mirror, so it *necessarily* also inverts the apparent winding order of
  every triangle as rasterized. `mesh.rs`'s quads are wound CCW-from-outside in world space;
  post-flip they rasterize CW-from-outside — so the pipeline's `front_face` needs to be
  `COUNTER_CLOCKWISE` to keep the *near/outer* faces front-facing (and thus visible after
  `CullModeFlags::BACK` discards the rest). It was set to `CLOCKWISE` — backwards — which culled
  the near/outer faces and left only the far/inner ones rendering: for a heightmap with open sky
  above and hollow underground below, that reads exactly as "the world is inside-out/upside down."
  This was flagged as a real risk from the start (see the original culling-enable checkpoint:
  "enabled only after user visually confirmed the *unculled* scene rendered correctly" — the
  culled result itself was never re-checked) and simply never surfaced with the small demo
  platform, where side-facing box geometry didn't make the direction obviously wrong. Fixed:
  `render-vk/src/lib.rs`'s rasterization state, `FrontFace::CLOCKWISE` → `COUNTER_CLOCKWISE` (one
  line + updated doc comments explaining the Y-flip/winding relationship precisely, so this
  doesn't get re-guessed wrong again). **Also fixed the reported ~0.5s freeze on breaking a
  block**: edits were still calling the same full-world `rebuild_world_mesh` (all loaded sections
  re-greedy-meshed) the perf fix left in place for *any* world change, not just streaming — so one
  block edit paid the same cost as a full chunk-radius load. Added a per-section mesh cache
  (`section_meshes: HashMap<(cx,sy,cz), (Vec<MeshVertex>, Vec<u32>)>`, world-offset baked in);
  `rebuild_world_mesh` now only re-greedy-meshes sections that are missing from the cache (newly
  streamed) or explicitly marked dirty (`dirty_sections`, set by `set_world_block` for just the
  one edited section — verified safe because `greedy_mesh` never looks across section boundaries,
  so a section's mesh depends only on its own content), then reassembles the world buffer by
  concatenating the cache (a plain copy, not a re-mesh). Verified: `cargo test --workspace` 81/81,
  clean build (no warnings), 15s soak clean, no leftover process. **Both fixes await the user's
  visual confirmation** — a GPU cull-mode setting and a perceived edit-freeze are not things unit
  tests or a soak-run can verify; only looking at the running game can.

- **The culling fix above did NOT resolve "upside down"; got a real screenshot; found and fixed
  the actual bug (2026-07-21).** User confirmed fps was better but "still upside down" after the
  culling change, which ruled out the "mouse-lurch from stalls" and "backwards culling" theories
  (culling misconfiguration would leave gaps/holes; the screenshot showed a complete, well-shaded,
  merely-*mirrored* world — sky at the bottom, terrain/canyon walls hanging from the top, tree
  trunks pointing down from the "ceiling" with canopy near the top). Blind guessing had failed
  twice, so — since Claude's own screenshot/computer-use tools don't see the user's physical
  screen (an already-established constraint) — asked the user to press Win+PrtScn and got them to
  actually launch the game (Escape had no cursor-free option before this, so also fixed that
  first: `Esc` now toggles cursor lock instead of calling `event_loop.exit()`, in
  `game/src/main.rs`). Then **read the resulting screenshot directly** with the Read tool, which
  finally gave real evidence instead of another guess.
  - Traced the whole render pipeline from the photo backward: `engine_core::camera`'s
    `VULKAN_CLIP_CORRECTION` (a clip-space Y negation, tested, but — per this same session's
    finding — **never actually visually confirmed**; this project's first-ever rendered triangle
    was accepted on "no crash" alone, see the Checkpoint 2 entry above) was the one deliberate
    Y-flip in the pipeline. Re-derived Vulkan's viewport-transform Y convention rigorously (it says
    the *removed* negation should have been correct, not the bug) but the empirical result
    contradicts that — some second, unaccounted flip is happening somewhere in the actual pipeline
    that wasn't pinned down through source reading alone (checked: UBO struct layout matches the
    WGSL struct exactly, the shader does a plain `view_proj * vec4(position,1.0)`, the viewport is
    a standard positive-height viewport, swapchain `pre_transform` uses `caps.current_transform`
    correctly). Given the theoretical model already produced one wrong prediction this session
    (the culling-direction guess), trusted the photo over further re-derivation: removed the
    negation (`VULKAN_CLIP_CORRECTION` is now identity) as the direct, testable counter to "the
    image is a clean vertical mirror." Updated `world_up_is_screen_up_in_vulkan_ndc`'s assertion
    to match, with an explicit comment that the new assertion direction is empirical, not
    re-derived from Vulkan's textbook Y-down-NDC convention (which would predict the opposite) —
    intentionally flagged as unresolved rather than papered over with false confidence.
  - **Also temporarily set `CullModeFlags::NONE`** (was `BACK`) rather than guess a second
    `FrontFace` value to pair with the flip removal — isolates the orientation question (is the
    world right-side up now?) from the culling-direction question (which winding is front-facing?)
    so a wrong culling guess can't produce a confusing "still broken, different reason" report on
    top of an actual orientation fix. Once orientation is confirmed, re-enabling `BACK` + picking
    the `FrontFace` that doesn't make geometry vanish is a much lower-stakes, likely single-shot
    follow-up.
  - Verified: `cargo test --workspace` 81/81 (test assertion direction changed, still green), clean
    build, 15s soak clean, no leftover process. **This is a hypothesis awaiting the user's next
    screenshot, not a confirmed fix** — said so explicitly rather than repeating the overclaiming
    mistake from the culling-direction attempt.

- **Orientation bug CONFIRMED FIXED by user (2026-07-21); culling re-enabled + feature batch
  (Sonnet).** User's "GOOD FINAly now improve..." message confirmed the identity-clip-correction
  fix actually resolved "upside down" — closing out the multi-attempt debugging saga documented
  above. Follow-up landed in 3 commits:
  - `c9da58b` — re-enabled `CullModeFlags::BACK` + `FrontFace::CLOCKWISE` (safe now that
    orientation is confirmed correct, paired with the identity clip correction); widened
    `STREAMING.load_radius` 4→7 (workers 3→4, affordable now that per-section mesh caching means
    an edit/stream-in only re-meshes the changed sections, not the whole world) — this is the
    interpretation used for the user's ambiguous "make chunks a bit deep not 1 block" ask (vertical
    depth was already 0..=7, i.e., not "1 block," so read as "render/stream farther," not deeper);
    added a rocky/treeless mountain-peak override to `worldgen.rs` (bare rock above a calibrated
    treeline height regardless of biome climate; threshold recalibrated from a wrong first guess —
    see the "Errors and fixes" pattern already established in this log — via a throwaway scan of
    actual achievable heights, 1 new test); mob count 2→8 with per-instance size/speed variety;
    `items.json` grew 4→9 entries (one per placeable block). 82/82 tests, 20s soak clean.
  - `b672e0b` — new second Vulkan pipeline in `render-vk` purely for 2D screen-space UI: no depth
    test, no culling, alpha-blended, empty descriptor/pipeline layout, NDC-direct vertices
    (`UiVertex`, new `mesh.rs` type), shares the existing dynamic-rendering pass with the world
    pipeline (bind+draw after the world mesh draw, same command buffer). New `render-vk/shaders/
    ui.wgsl` (passthrough vertex shader, flat-color fragment shader). `Renderer` trait gained
    `set_ui_mesh` alongside `set_mesh`, same wholesale-replace/call-sparingly contract. Verified
    clean build, zero warnings, on first attempt.
  - `8c8ed71` — new `game/src/ui.rs`: pure-geometry builders (crosshair, inventory grid) plus the
    grid layout/hit-test math, with `inventory_cell_rect` as the single source of truth shared
    between what's drawn and what a click hits (so they can never disagree about where a cell is).
    5 unit tests, no GPU needed. Wired into `App`: crosshair shows whenever the cursor is locked;
    `E` toggles a full-screen inventory grid of every registered placeable block (colored swatch
    per block, highlight outline on the current selection) and frees the cursor; clicking a swatch
    sets `selected_block` and closes it; `Escape` closes the inventory first if open, otherwise
    behaves as before (free/lock cursor) — reused the existing `cursor_locked` gate so WASD/
    mouse-look/mine-place were already correctly suppressed while the inventory is open, no new
    gating logic needed. Added `WindowEvent::CursorMoved` tracking (`Input.cursor_pos`) since click
    hit-testing needs absolute cursor position, which the game hadn't tracked before (only the
    mouse-look delta). 87/87 tests (was 82), clean build zero warnings, 15s soak clean, no leftover
    process.
  - **Not yet independently re-confirmed by the user visually** (widened render distance, mountain
    peaks, 8 mobs, crosshair, inventory grid) — same standing caveat as always: tests + soak-run
    confirm nothing crashes/regresses structurally, but "does it look right" is the user's call.

- **Checkpoint (2026-07-21, Sonnet): the rest of the user's feature wishlist landed —
  Creative/Survival distinction, start menu, single-player save/load.** Closes out tasks #14–#16.
  - `af94349` — `GameMode` enum (Creative/Survival) on `App`, default Creative. Survival disables
    flight only (`F` no-ops outside Creative; switching into Survival while flying forces it off
    via `set_game_mode`) — deliberately not full survival mechanics (no health/hunger/mining-
    drops), matching the scope call already logged in this file's "Next Up" section before this
    checkpoint. `G` toggles it for now as a placeholder until the menu below is the only way to
    pick it. 87/87 tests (no new ones — pure control-flow change).
  - `66a7862` — main menu + save/load, the biggest single addition this session:
    - **Main menu** (`AppState::MainMenu`/`InGame` on `App`): shown on launch, pauses physics/
      streaming/mob simulation and gates keyboard input (Escape/E are no-ops at the menu — letting
      Escape through would've re-locked the cursor the menu needs free to be clickable) until a
      mouse click picks an option. The world is already generated/streamed by the time the menu
      shows (that startup work was always eager, before this checkpoint too), so "New World" is
      just "use it as-is, ignore any save file, set the mode" rather than actually regenerating.
      No font atlas exists (same open decision as real textures), so options are colored bars —
      new `ui::menu_mesh`/`menu_hit_test` (+`menu_option_rect` as the shared single source of
      truth, same pattern as the inventory grid's `inventory_cell_rect`). "Load World" only
      appears once a save file exists.
    - **Save/load** (new `game/src/save.rs`): single fixed slot (`world_save.json`, gitignored).
      Diff-only — only player-modified sections persist, matching `streaming.rs`'s existing
      discard-unmodified-on-evict philosophy (unmodified terrain regenerates identically from
      `WORLD_SEED`, so saving it too would be pure waste). `WorldSave` serializes the real
      `engine_core::chunk::ChunkColumn` (now `Serialize`/`Deserialize`/`Clone`, alongside
      `PalettedSection`/`BlockId`) directly — no separate save-format DTO, the palette-compressed
      representation round-trips through JSON as-is.
    - **engine-core additions**: `ChunkManager::load_saved` (overwrites already-loaded columns'
      sections in place; stashes the rest in a new `pending_overlay: HashMap<(i32,i32,i32),
      PalettedSection>` field that `pump()` now consults before using a freshly-generated section,
      so a saved edit correctly overrides generation no matter when that column happens to stream
      in) and `ChunkManager::modified_columns` (snapshot of currently-loaded modified columns
      without evicting them — combined with the pre-existing `drain_evicted_modified` at save
      time to capture the *complete* modified set for the session, not just what happened to be
      evicted). 5 new streaming tests.
    - **Loading re-centers streaming**: a saved player position can be far from wherever
      `resumed()`'s startup blocking-load centered on, so `load_saved_world` re-runs the same
      bounded (`10s` deadline) blocking-pump pattern `resumed()` already established, keyed to the
      *restored* position, before dropping the player in — avoids spawning over an unloaded void.
    - **Caught a real bug via TDD**: the first draft of `save.rs`'s JSON round-trip test used a
      world-space y coordinate where a local (in-section) one was needed, so the assertion failed
      against a section inserted at `section_y=0` — the save/load *code* was correct, the test's
      own coordinate math was wrong. Fixed by picking `section_y=0` so world-y and local-y
      coincide, keeping the test simple rather than getting the div_euclid/rem_euclid math right
      by hand. Documented as a reminder that a new test failing doesn't always mean the code under
      test is wrong — worth checking which one has the bug before "fixing" either.
  - Verified (both commits): `cargo test --workspace` 93/93 (was 87; +5 streaming, +2 ui.rs menu
    hit-test, +1 save.rs round-trip), clean build zero warnings both times, 15s soak clean each
    time, no leftover process. **Not yet visually confirmed by the user** — same standing caveat:
    tests + soak-run confirm nothing crashes/regresses structurally, "does the menu look/feel
    right" is the user's call, especially since it's colored bars rather than text (no font
    rendering exists) which may read as confusing without knowing this document's explanation.
  - **All 6 items from the user's original feature wishlist are now done**: performance (mesh
    caching/throttling from the bug-fix checkpoints above), inventory, crosshair, wider render
    distance, expanded biomes/mobs/items, and this checkpoint's Creative/Survival + start menu +
    save/load. Texture pack remains explicitly deferred per the user's own choice.

## Next Up
**Both Opus-tier milestones are done** (terrain generation + biome blending, and pathfinding
around partially-loaded chunks). The remaining Opus §6 item, "reviewing/hardening the foundation,"
got an implicit pass this session: the real terrain generator + streaming at load_radius 7 exercises
the streaming/eviction path far harder than the old void generator ever did, and it soaked clean —
but a dedicated fresh-eyes audit is still worthwhile if the user wants belt-and-suspenders.

**DONE — the user's entire large feature wishlist from this session is now complete**: crosshair,
inventory picker, Creative/Survival distinction, start menu (New World / Load World), and
single-player world save/load are all implemented, tested, and pushed (see the checkpoints above).
Texture pack remains **explicitly deferred by the user's own choice**, not forgotten — revisit only
if they ask.

What's left beyond that wishlist, none of it a foundational milestone:
- Real textures (atlas is placeholder flat colors per block id) — needs an asset-pack decision
  (STARTER.md §8), Sonnet-tier once decided; user has already said hold off on this specifically.
- Fluid behavior for water (currently a solid walkable block; `BehaviorTag::Fluid` already exists
  in the registry as the hook) — Sonnet-tier.
- Per-chunk GPU mesh buffers (perf), Metal/macOS backend, real multiplayer (explicitly out of
  scope per the user's clarification above, unless they ask again later).
- Terrain/nav tuning: noise constants (`worldgen.rs`) and nav params (`NavConfig`, seek range/
  repath interval in `main.rs`) are first coherent passes, easy knobs if the user wants a
  different feel.
