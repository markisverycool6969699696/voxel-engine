# FOROPUS — Opus Task Scope

Opus handles the strong-reasoning tier: reviewing/hardening the foundation, the terrain
generation pipeline, biome blending, and pathfinding around partially-loaded chunks (spec §6).
Minimize token usage on restating context already covered below — read this file plus the
pointers it gives you, then implement.

Full spec: `../docs/STARTER.md` (world-gen approach specifically in §7). Plan: `../project.md`.
Running dev log: `../MEMORY.md` — read it before starting, it has the full history of decisions
and why things are shaped the way they are. This scope doc is a snapshot as of 2026-07-21; treat
MEMORY.md as more current if they disagree.

## In Scope

1. **Real terrain generation pipeline** (spec §7): multi-octave noise for base terrain shape,
   height-mapped columns, temperature/humidity noise blended into biome assignment, secondary 3D
   noise for cave/overhang carving, deterministic seeded structure placement (trees, ore veins).
   All specific tuning constants/thresholds/creative decisions are to be original — not a port of
   any proprietary generator's output.
2. **Biome blending.**
3. **Pathfinding around partially-loaded chunks** — the mob AI Sonnet built
   (`engine-core/src/mob.rs`) is a flat random-walk `Wander` only; it doesn't path, and doesn't
   know or care whether neighboring chunks are loaded. Real pathfinding (and what a mob should do
   when the path runs off the edge of loaded terrain) is this tier's call.
4. **Reviewing/hardening the foundation** — Sonnet did a manual Vulkan resource-lifecycle audit of
   `render-vk` (see MEMORY.md, no leaks/UAF found) but a second independent pass is welcome,
   especially now that a real generator will put load on the streaming path that a void generator
   never did.

## Out of Scope (already done — don't redo)

- Raw Vulkan init/sync, greedy meshing core algorithm, chunk streaming/threading architecture,
  palette compression — Fable, foundation is stable and tested.
- Textures/atlas, player movement, sound integration, mob AI *basics* (movement/collision
  scaffolding — not pathfinding, see above), creative mode, item system implementation, docs,
  debugging polish — Sonnet, all done as of 2026-07-21 (see MEMORY.md "Checkpoint (2026-07-21)"
  entries). Don't re-touch `game/src/main.rs`'s flight/mob-wander/hotbar wiring or
  `engine-core/src/physics.rs`'s flight code unless terrain gen needs to change how they call in.
- Bulk data entry, boilerplate — Haiku.

## Current State / Integration Points

The world is currently **one hand-built demo structure at the origin column, void everywhere
else** — deliberately, because Sonnet was explicitly told not to improvise placeholder terrain
content. This is the actual gap you're filling.

- **`engine_core::streaming::ChunkGenerator`** (`engine-core/src/streaming.rs`) is the trait to
  implement:
  ```rust
  pub trait ChunkGenerator: Send + Sync + 'static {
      fn generate(&self, cx: i32, sy: i32, cz: i32) -> PalettedSection;
  }
  ```
  **Must be deterministic**: same `(cx, sy, cz)` → identical section, always — the whole
  only-persist-modified-chunks strategy (spec §4.1) depends on it. `ChunkManager` already handles
  threading (a worker pool calls `generate` off the game thread), load/unload by radius, vertical
  streaming (shallow slice first, `ensure_depth` for deeper), and the edit-wins-over-late-gen race.
  None of that needs to change for a real generator to drop in — it's already exercised by tests
  in `streaming.rs` and by the current void generator in production use.
- **`game/src/main.rs`**: `DemoGenerator` (currently: origin column = hand-built structure, else
  `PalettedSection::filled(AIR)`) is what you're replacing. `STREAMING` (a `StreamingConfig` const
  a few lines above it) currently uses `load_radius: 2` — small, because a void world doesn't
  reward a bigger one; revisit once real terrain exists. The hand-built demo structure
  (`build_demo_section`) can probably go away entirely once there's real ground to stand on, but
  that's your call — it's currently also what the player spawns on top of
  (`App::default`'s player-position constant), so if you remove it, the spawn point needs new
  logic (e.g. query generated height at spawn column before placing the player) rather than a
  hardcoded position.
- **`engine_core::chunk::PalettedSection`** (`engine-core/src/chunk.rs`): 16³ blocks per section,
  `set(x, y, z, BlockId)` / `get(x, y, z) -> BlockId`, `filled(BlockId)` for uniform sections
  (allocation-free — worth using for e.g. an all-air section above generated terrain, or an
  all-stone section deep underground, rather than writing every cell). `SECTION_DIM = 16`.
- **`engine_core::registry`** (`engine-core/src/registry.rs`): data-driven `BlockDef`/`ItemDef` via
  JSON, already wired into the playable game (`game/data/blocks.json`, `game/data/items.json`).
  Currently 5 debug-placeholder block ids (0=air, 1-4=debug colors). **The "final v1 block/item
  list" is explicitly an open decision (STARTER.md §8)** — if terrain gen needs real block types
  (stone/dirt/grass/water/etc. with real hardness/tags), that's content you're now positioned to
  decide, by extending `blocks.json`/`items.json`. `BehaviorTag` (fluid, crop, gravity, flammable,
  interactable, powerable) already exists for special-behavior blocks like water.
- No seed/world-config plumbing exists yet — `DemoGenerator` takes no parameters. A real generator
  will need a seed threaded in from somewhere (currently nothing calls `ChunkManager::new` with
  anything but a hardcoded generator instance in `game/src/main.rs`).

## Working Rules

- One subsystem per session, same as Fable's rule — don't cross-cut into Sonnet's/Fable's
  territory just because you're already in a file.
- Keep `ChunkGenerator::generate` cheap-ish per call relative to what a background worker thread
  can sustain — `ChunkManager` calls it once per `(cx, sy, cz)` off the game thread, but a very
  expensive generator will still show up as load-radius growth becoming laggy.
- Update `MEMORY.md` with what you did and why (running project log, read by every tier) and
  `README.md`'s Status/Working sections once real terrain exists — the whole "walk far enough and
  it's void" caveat currently in both should go away.
