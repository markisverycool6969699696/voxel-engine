# FORFAB — Fable 5 Task Scope

Fable 5 handles ONLY the hardest, highest-risk-of-subtle-failure foundational work. Minimize token usage: no exploratory chat, no restating context, no rewriting working code. Implement, don't narrate.

Full spec: `STARTER.md`. Plan: `project.md`.

## In Scope (hard only)
1. Vulkan init via `ash` — instance, device, queues, swapchain, sync primitives. Correctness of sync (semaphores/fences) is the risk, not boilerplate.
2. Greedy meshing core algorithm (chunk section → merged quads).
3. Chunk streaming/threading architecture (background thread gen, vertical streaming, load/unload).
4. Palette compression implementation (per-chunk palette + bit-packed indices).

Metal backend is deferred — Vulkan-on-Windows only for now (per bring-up order in `project.md`).

## Out of Scope (do not touch)
- Textures/atlas, player movement, sound, mob AI, creative mode, item system, docs, debugging polish — these go to Sonnet.
- Bulk data entry, boilerplate, doc comments — Haiku.
- Terrain gen pipeline, biome blending, pathfinding — Opus (review/hardening tier).

## Working Rules
- One subsystem per session. No cross-cutting refactors unless the task requires it.
- If a task turns out not to be hard (turns into boilerplate), stop and hand back — don't keep going just because you're already in the file.
- No speculative abstractions beyond what the immediate subsystem needs.
