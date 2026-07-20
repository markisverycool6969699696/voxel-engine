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

- **Sound integration, attempt 2 — STARTED THEN DELIBERATELY REVERTED FOR STABILITY (Sonnet):**
  user wants sound fixed properly ("we need full stable base"). Real fix per checkpoint 8's
  diagnosis: install MSVC Build Tools and get off the GNU/LLVM-MinGW toolchain, since
  `windows-rs` (pulled in by `rodio`'s `cpal` dependency) targets MSVC ABI.
  - `winget install --id Microsoft.VisualStudio.2022.BuildTools` (with the
    `Microsoft.VisualStudio.Workload.VCTools` override) **finished successfully** (background task
    completion notification, exit code 0) shortly after this entry was first written — but the
    session paused for usage limits right after, so `link.exe`/`cl.exe` availability and the actual
    MSVC toolchain switch are still unverified. Confirm `Get-Command link.exe` / `cl.exe` before
    assuming this is actually ready to use.
  - Audio code (`game/src/audio.rs`, the `rodio` dependency, and the wiring into
    `game/src/main.rs` — mod declaration, `audio: Option<Audio>` field, `Audio::new()` in
    `App::default`, `play_mine()`/`play_place()` at the two mine/place edit sites) was re-added
    ahead of the toolchain being ready, so it would be a quick rebuild+test once unblocked.
  - **Deliberately reverted before pausing** rather than leave it sitting uncommitted and
    untested: with the Build Tools install not yet done, "sound might work" wasn't a fact, and the
    user explicitly asked to make sure everything works, not to leave something half-verified.
    `game/src/audio.rs` deleted, `game/Cargo.toml` and `game/src/main.rs` restored via
    `git restore` to exactly match `origin/master`. Re-verified after reverting: clean build,
    57/57 tests, 6s runtime soak with no crash/validation errors — **confirmed back to the same
    known-good state as the last commit**, nothing regressed by the attempt.
  - **To resume:** once VS Build Tools finishes, switch this project's toolchain to MSVC
    (prefer a repo-local `rustup override set stable-x86_64-pc-windows-msvc`, not the global
    rustup default, since other GNU-dependent work may exist elsewhere on this machine), confirm
    the existing (non-audio) build/tests/runtime still work under MSVC first, *then* re-apply the
    audio changes (same shape as described above — nothing conceptually new to figure out, just
    needs the working toolchain) and verify specifically that `cpal`/`windows-rs` no longer
    crashes the binary. If it still crashes under MSVC too, that disproves the toolchain theory —
    don't assume MSVC is a guaranteed fix without checking.
  - Also unverified: `.cargo/config.toml`'s GNU-target rustflags are scoped under
    `[target.x86_64-pc-windows-gnu]` so they shouldn't affect an MSVC build, but that assumption
    hasn't actually been checked yet either.

- **Repo state:** pushed to GitHub — https://github.com/markisverycool6969699696/voxel-engine
  (public, AGPLv3). `origin/master` and the local working tree match exactly — everything through
  checkpoint 10 (texture atlas) is committed, pushed, and freshly re-verified working (clean
  build, 57/57 tests, no-crash runtime soak). No uncommitted or in-flight changes.

## Environment Notes
- No MSVC Build Tools on this machine → using `stable-x86_64-pc-windows-gnu` toolchain.
  **(In progress, see above: this may change if the MSVC toolchain switch for sound succeeds.)**
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

## Next Up
**Resume here first**: the in-progress sound/MSVC toolchain switch above. As of the pause,
`winget install ... Microsoft.VisualStudio.2022.BuildTools` had only printed "Starting package
install..." — likely still mid-install (it's a multi-GB download+install, can take 15-20+ min).
Check whether it finished (`Get-Command link.exe`), and if the background shell task is still
listed as running, before doing anything else with the toolchain.

After that's resolved (working or not), remaining Sonnet-tier work — none of these require Opus:
1. Commit + push once sound is confirmed working (or confirmed still blocked, with findings noted).
2. Wiring `engine_core::streaming::ChunkManager` into `game/src/main.rs` for a multi-chunk world.
   Caution: needs *some* `ChunkGenerator` to produce content, and deciding what a chunk generator
   should output is real world-gen content — that's Opus's call per the tiering plan, not something
   to improvise here even as a "placeholder." **This is likely the last Sonnet-tier item before
   the project genuinely needs Opus for world generation.**

Real world generation (terrain gen pipeline, biome blending) is Opus-tier per the project's own
AI-tiering plan — flag it, don't build it, once item 2 above is reached.
