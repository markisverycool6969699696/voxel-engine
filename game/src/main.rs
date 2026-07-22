//! Platform binary: window + event loop, driving the renderer via the
//! `engine_core::Renderer` trait. Vulkan-only for now (Metal deferred).
//!
//! World content is served through `engine_core::streaming::ChunkManager`
//! (background-threaded, load/unload by radius around the player), fed by
//! `engine_core::worldgen::TerrainGenerator` — real seeded heightmap terrain
//! with biomes, water, caves, ore, and trees. The player and mobs are placed
//! on the generated surface at startup.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use engine_core::camera::Camera;
use engine_core::chunk::{BlockId, PalettedSection, AIR};
use engine_core::mesh::{greedy_mesh, triangulate, MeshVertex};
use engine_core::mob::Mob;
use engine_core::pathfind::{find_path, Cell, NavConfig};
use engine_core::physics::PlayerController;
use engine_core::raycast::raycast_voxels;
use engine_core::registry::{BlockDef, ItemDef, Registry};
use engine_core::streaming::{ChunkManager, StreamingConfig};
use engine_core::worldgen::TerrainGenerator;
use engine_core::Renderer;
use glam::{IVec3, Vec3};
use render_vk::VkRenderer;
use winit::application::ApplicationHandler;
use winit::event::{DeviceEvent, DeviceId, ElementState, MouseButton, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::window::{CursorGrabMode, Window, WindowId};

mod audio;
use audio::Audio;
mod ui;

const SPRINT_MULTIPLIER: f32 = 1.6;
const MOUSE_SENSITIVITY: f32 = 0.0025;
const MAX_PITCH: f32 = 1.5; // just under vertical, avoids the look-direction singularity
const MAX_REACH: f32 = 6.0;

const SECTION_DIM: i32 = 16;

/// Fixed world seed (a real seed-selection UI is future work).
const WORLD_SEED: u64 = 0x5EED_1234;

/// Streamed radius around the player, in columns. `initial_sections` spans
/// the full terrain height band (world y 0..127) so a column loads as solid
/// ground, not a floating surface slice.
const STREAMING: StreamingConfig = StreamingConfig {
    load_radius: 4,
    unload_margin: 2,
    initial_sections: 0..=7,
    workers: 3,
};

/// Hotbar (number keys 1-4): a handful of quick-access defaults, resolved
/// through the data-driven `Registry<ItemDef>`/`Registry<BlockDef>` (see
/// `data/items.json`, `data/blocks.json`). The full 76-item catalog is
/// reachable via the inventory picker (`E`) — the hotbar is just a
/// convenience shortcut for a few common ones, not the only way to select
/// a block.
const HOTBAR_ITEM_IDS: [u16; 4] = [1, 2, 3, 4];

/// Resolves a hotbar item id to the `BlockId` it places. Panics on a bad
/// registry/hotbar mismatch — that's a data bug in the shipped JSON, not a
/// runtime condition to recover from.
fn block_for_item(items: &Registry<ItemDef>, blocks: &Registry<BlockDef>, item_id: u16) -> BlockId {
    let item = items.get_by_id(item_id).expect("hotbar item id must exist in items.json");
    let block_key = item.places_block.as_deref().expect("hotbar item must place a block");
    let block = blocks.get(block_key).expect("places_block key must exist in blocks.json");
    BlockId(block.id)
}

/// Every placeable item as `(item_id, block_id)`, in registry order — the
/// full catalog the inventory picker shows (not just the 4-slot hotbar).
fn inventory_items(items: &Registry<ItemDef>, blocks: &Registry<BlockDef>) -> Vec<(u16, u16)> {
    items
        .iter()
        .filter_map(|item| {
            let block_key = item.places_block.as_deref()?;
            let block = blocks.get(block_key)?;
            Some((item.id, block.id))
        })
        .collect()
}

/// Placeholder mob appearance/size — not a real mob roster, just enough to
/// prove wandering AI + collision + rendering work end to end.
const MOB_BLOCK: BlockId = BlockId(12);
const MOB_SIZE: Vec3 = Vec3::new(0.6, 0.8, 0.6);
const MOB_WALK_SPEED: f32 = 1.5;
/// Mobs beyond this horizontal distance from the player don't bother
/// pathfinding (they just wander) — bounds cost and keeps far mobs idle.
const MOB_SEEK_RANGE: f32 = 40.0;
/// Seconds between path recomputes per mob — pathfinding every frame is
/// wasteful and jittery; a stale path is followed in between.
const MOB_REPATH_INTERVAL: f32 = 0.5;

/// A mob plus its current navigation state. The mob seeks the player via
/// `pathfind`, which refuses to route through unloaded chunks; when no path
/// exists (player unreachable, flying, or across ungenerated terrain) the mob
/// falls back to its built-in wander.
struct MobEntity {
    mob: Mob,
    path: Vec<IVec3>,
    path_idx: usize,
    repath_timer: f32,
}

impl MobEntity {
    fn new(mob: Mob) -> Self {
        Self { mob, path: Vec::new(), path_idx: 0, repath_timer: 0.0 }
    }
}

/// World block at `(x,y,z)` classified for navigation: `Unknown` where the
/// chunk isn't loaded (so the pathfinder never routes through it), else
/// `Solid`/`Open` by whether the block is air.
fn nav_cell(chunks: &ChunkManager, x: i32, y: i32, z: i32) -> Cell {
    let (cx, cz) = (x.div_euclid(SECTION_DIM), z.div_euclid(SECTION_DIM));
    let (lx, lz) = (x.rem_euclid(SECTION_DIM) as usize, z.rem_euclid(SECTION_DIM) as usize);
    match chunks.block(cx, cz, lx, y, lz) {
        None => Cell::Unknown,
        Some(b) if b == AIR => Cell::Open,
        Some(_) => Cell::Solid,
    }
}

/// The block a grounded entity's feet stand in (floor of its AABB base).
fn feet_block(center: Vec3, half_height: f32) -> IVec3 {
    IVec3::new(
        center.x.floor() as i32,
        (center.y - half_height + 0.001).floor() as i32,
        center.z.floor() as i32,
    )
}

/// Builds a small axis-aligned box mesh for a mob by reusing `greedy_mesh`'s
/// already-tested winding/tiling logic on a synthetic single-cell section,
/// then scaling the resulting unit cube to `size` and translating to
/// `center`. A solid placeholder box, same idea as using debug block ids for
/// the hand-built demo structure — not a real mob model.
fn mob_box_mesh(center: Vec3, size: Vec3, block: BlockId) -> (Vec<MeshVertex>, Vec<u32>) {
    let mut s = PalettedSection::filled(AIR);
    s.set(0, 0, 0, block);
    let quads = greedy_mesh(&s, |b| b != AIR);
    let (mut vertices, indices) = triangulate(&quads);
    let origin = center - size / 2.0;
    for v in &mut vertices {
        v.position[0] = v.position[0] * size.x + origin.x;
        v.position[1] = v.position[1] * size.y + origin.y;
        v.position[2] = v.position[2] * size.z + origin.z;
    }
    (vertices, indices)
}

fn is_solid_in(chunks: &ChunkManager, x: i32, y: i32, z: i32) -> bool {
    let (cx, cz) = (x.div_euclid(SECTION_DIM), z.div_euclid(SECTION_DIM));
    let (lx, lz) = (x.rem_euclid(SECTION_DIM) as usize, z.rem_euclid(SECTION_DIM) as usize);
    // Ungenerated/unloaded reads as open air/void, not solid — same fallback
    // the old single-section bounds check used.
    chunks.block(cx, cz, lx, y, lz).is_some_and(|b| b != AIR)
}

fn world_chunk_of(pos: Vec3) -> (i32, i32) {
    (
        (pos.x.floor() as i32).div_euclid(SECTION_DIM),
        (pos.z.floor() as i32).div_euclid(SECTION_DIM),
    )
}

#[derive(Default)]
struct Input {
    held: HashSet<KeyCode>,
    mouse_delta: (f64, f64),
    /// Absolute cursor position in physical pixels — only meaningful (and
    /// only tracked/used) while the cursor is unlocked, for UI click
    /// hit-testing. Mouse-look uses `mouse_delta`, not this.
    cursor_pos: (f64, f64),
    mine_requested: bool,
    place_requested: bool,
}

struct App {
    window: Option<Arc<Window>>,
    renderer: Option<VkRenderer>,
    camera: Camera,
    player: PlayerController,
    chunks: ChunkManager,
    streaming_center: Option<(i32, i32)>,
    mobs: Vec<MobEntity>,
    blocks: Registry<BlockDef>,
    items: Registry<ItemDef>,
    input: Input,
    last_frame: Option<Instant>,
    selected_block: BlockId,
    /// `None` if no audio output device is available — sound is a nice-to-have.
    audio: Option<Audio>,
    /// Per-section triangulated mesh cache, keyed by `(cx, sy, cz)`, world-
    /// offset already baked into the stored positions. Memoized so editing
    /// one block only re-triangulates *that* section instead of every loaded
    /// section — re-meshing the whole streamed world (hundreds of sections)
    /// on every single block break/place was the reported edit-freeze.
    /// Entries for sections no longer loaded are pruned in `rebuild_world_mesh`.
    section_meshes: HashMap<(i32, i32, i32), (Vec<MeshVertex>, Vec<u32>)>,
    /// Sections needing a cache refresh on the next `rebuild_world_mesh` —
    /// edits mark just the one affected section; newly streamed-in sections
    /// are picked up automatically (absent from the cache counts as dirty).
    dirty_sections: HashSet<(i32, i32, i32)>,
    /// Concatenation of every cached section (chunks only, no mobs) —
    /// rebuilt from `section_meshes` only when something in it changed.
    world_vertices: Vec<MeshVertex>,
    world_indices: Vec<u32>,
    world_mesh_dirty: bool,
    /// Throttles combined (world+mobs) GPU uploads so continuous mob motion
    /// doesn't pay `set_mesh`'s stall every frame either.
    mesh_upload_accum: f32,
    /// Mouse-look/movement/mine-place only apply while true. `Esc` toggles
    /// this (free the cursor to alt-tab/screenshot/etc.) instead of quitting
    /// — closing the window (Alt+F4 / the X button) still quits via
    /// `WindowEvent::CloseRequested`, unaffected by this.
    cursor_locked: bool,
    /// `E` toggles this — opens the block picker (frees the cursor to click
    /// a swatch, closes and re-locks on selection or `Esc`).
    inventory_open: bool,
}

impl Default for App {
    fn default() -> Self {
        let generator = Arc::new(TerrainGenerator::new(WORLD_SEED));

        // Spawn on the generated surface at the origin column, standing just
        // above the ground so gravity settles the player onto it.
        let spawn_h = generator.surface_height(8, 8);
        let player = PlayerController::new(Vec3::new(
            8.5,
            spawn_h as f32 + engine_core::physics::PLAYER_HALF_HEIGHT + 1.0,
            8.5,
        ));
        let mut camera = Camera::new(player.eye_position(), 1.0);
        camera.yaw = 2.0;
        camera.pitch = -0.1;

        let blocks = Registry::<BlockDef>::load_from_str(include_str!("../data/blocks.json"))
            .expect("data/blocks.json must parse");
        let items = Registry::<ItemDef>::load_from_str(include_str!("../data/items.json"))
            .expect("data/items.json must parse");
        let selected_block = block_for_item(&items, &blocks, HOTBAR_ITEM_IDS[0]);

        // Two mobs placed on the generated surface near spawn — not a
        // spawning system, just enough to see wander AI working.
        let mob = |wx: i32, wz: i32, seed: u64| {
            let h = generator.surface_height(wx, wz);
            MobEntity::new(Mob::new(
                Vec3::new(wx as f32 + 0.5, h as f32 + MOB_SIZE.y / 2.0 + 0.5, wz as f32 + 0.5),
                MOB_SIZE / 2.0,
                seed,
            ))
        };
        let mobs = vec![mob(12, 8, 1001), mob(5, 12, 2002)];

        Self {
            window: None,
            renderer: None,
            camera,
            player,
            chunks: ChunkManager::new(generator, STREAMING),
            streaming_center: None,
            mobs,
            blocks,
            items,
            input: Input::default(),
            last_frame: None,
            selected_block,
            audio: Audio::new(),
            section_meshes: HashMap::new(),
            dirty_sections: HashSet::new(),
            world_vertices: Vec::new(),
            world_indices: Vec::new(),
            world_mesh_dirty: true,
            mesh_upload_accum: 0.0,
            cursor_locked: true,
            inventory_open: false,
        }
    }
}

/// Combined (world+mobs) GPU upload rate cap. `set_mesh` stalls the whole
/// device (`device_wait_idle` + buffer recreate per render-vk's own docs),
/// so this is a hard ceiling on how often mob motion alone can trigger an
/// upload — 12Hz is smooth enough for slow-wandering placeholder boxes and
/// keeps that stall off the hot path.
const MESH_UPLOAD_INTERVAL: f32 = 1.0 / 12.0;

impl App {
    /// Re-triangulates only sections that need it (missing from the cache —
    /// i.e. newly streamed in — or explicitly marked dirty by an edit), then
    /// reassembles `self.world_vertices`/`world_indices` by concatenating the
    /// full per-section cache. The reassembly concatenation is a plain copy
    /// over however many sections are currently loaded — cheap relative to
    /// greedy-meshing, which is the part this cache actually avoids repeating.
    fn rebuild_world_mesh(&mut self) {
        let mut still_loaded = HashSet::with_capacity(self.section_meshes.len());
        for ((cx, cz), column) in self.chunks.columns() {
            for (sy, section) in column.loaded_sections() {
                let key = (cx, sy, cz);
                still_loaded.insert(key);
                if self.section_meshes.contains_key(&key) && !self.dirty_sections.contains(&key) {
                    continue; // cached and unchanged
                }
                let quads = greedy_mesh(section, |b| b != AIR);
                let (mut vertices, indices) = triangulate(&quads);
                let offset = Vec3::new(
                    (cx * SECTION_DIM) as f32,
                    (sy * SECTION_DIM) as f32,
                    (cz * SECTION_DIM) as f32,
                );
                for v in &mut vertices {
                    v.position[0] += offset.x;
                    v.position[1] += offset.y;
                    v.position[2] += offset.z;
                }
                self.section_meshes.insert(key, (vertices, indices));
            }
        }
        self.dirty_sections.clear();
        self.section_meshes.retain(|k, _| still_loaded.contains(k));

        let mut vertices = Vec::new();
        let mut indices = Vec::new();
        for (v, i) in self.section_meshes.values() {
            let base = vertices.len() as u32;
            indices.extend(i.iter().map(|idx| idx + base));
            vertices.extend(v.iter().copied());
        }
        self.world_vertices = vertices;
        self.world_indices = indices;
        self.world_mesh_dirty = false;
    }

    /// Copies the cached world mesh, appends fresh mob boxes at their current
    /// positions, and uploads the combined buffer. The clone+append is a
    /// plain memcpy-class cost (cheap); `set_mesh` itself is the expensive
    /// part, which is why callers throttle how often this runs rather than
    /// calling it unconditionally every frame.
    fn upload_combined_mesh(&mut self) {
        let Some(renderer) = self.renderer.as_mut() else { return };
        let mut vertices = self.world_vertices.clone();
        let mut indices = self.world_indices.clone();
        for entity in &self.mobs {
            let (mob_vertices, mob_indices) = mob_box_mesh(entity.mob.position, MOB_SIZE, MOB_BLOCK);
            let base = vertices.len() as u32;
            indices.extend(mob_indices.into_iter().map(|i| i + base));
            vertices.extend(mob_vertices);
        }
        if let Err(e) = renderer.set_mesh(&vertices, &indices) {
            eprintln!("mesh upload error: {e:#}");
        }
    }

    /// Moves the streaming radius to follow the player and integrates any
    /// finished background generation. Returns true if new sections landed
    /// (world mesh needs rebuilding).
    fn update_streaming(&mut self) -> bool {
        let center = world_chunk_of(self.player.position);
        if self.streaming_center != Some(center) {
            self.streaming_center = Some(center);
            self.chunks.set_center(center.0, center.1);
        }
        self.chunks.pump() > 0
    }

    /// Advances each mob: pathfind toward the player (recomputed on a timer,
    /// skipping unloaded chunks), steer along the current path node, then step
    /// physics. Mobs with no viable path fall back to wandering.
    fn update_mobs(&mut self, dt: f32) {
        let chunks = &self.chunks;
        let player_feet = feet_block(self.player.position, engine_core::physics::PLAYER_HALF_HEIGHT);
        for entity in &mut self.mobs {
            entity.repath_timer -= dt;
            let to_player = self.player.position - entity.mob.position;
            let in_range = to_player.x * to_player.x + to_player.z * to_player.z
                < MOB_SEEK_RANGE * MOB_SEEK_RANGE;

            if in_range && entity.repath_timer <= 0.0 {
                entity.repath_timer = MOB_REPATH_INTERVAL;
                let start = feet_block(entity.mob.position, MOB_SIZE.y / 2.0);
                entity.path = find_path(
                    start,
                    player_feet,
                    |x, y, z| nav_cell(chunks, x, y, z),
                    &NavConfig::default(),
                )
                .unwrap_or_default();
                entity.path_idx = 0;
            }
            if !in_range {
                entity.path.clear();
            }

            // Follow the path: steer toward the current node, advancing as the
            // mob reaches each. Any un-steered tick lets the mob wander.
            if let Some(node) = entity.path.get(entity.path_idx) {
                let target = Vec3::new(node.x as f32 + 0.5, entity.mob.position.y, node.z as f32 + 0.5);
                let delta = target - entity.mob.position;
                if delta.x * delta.x + delta.z * delta.z < 0.35 * 0.35 {
                    entity.path_idx += 1;
                } else {
                    entity.mob.steer_toward(delta.x, delta.z);
                }
            }
            entity.mob.update(dt, MOB_WALK_SPEED, |x, y, z| is_solid_in(chunks, x, y, z));
        }
    }

    fn update_and_render(&mut self) {
        let now = Instant::now();
        let dt = self
            .last_frame
            .map(|prev| (now - prev).as_secs_f32())
            .unwrap_or(0.0);
        self.last_frame = Some(now);

        // While the cursor is freed (Esc), the game keeps simulating (world
        // streaming, mobs, gravity) but ignores mouse-look/movement/mine-place
        // input — otherwise a mouse moved to click elsewhere on the desktop
        // would also spin the camera, and a click meant for another window
        // could register as mining.
        let mut wish = Vec3::ZERO;
        let mut jump = false;
        if self.cursor_locked {
            self.camera.yaw += self.input.mouse_delta.0 as f32 * MOUSE_SENSITIVITY;
            self.camera.pitch = (self.camera.pitch
                - self.input.mouse_delta.1 as f32 * MOUSE_SENSITIVITY)
                .clamp(-MAX_PITCH, MAX_PITCH);

            // Horizontal movement uses yaw only (not pitch) — a walking player
            // doesn't move forward-into-the-ground just from looking down.
            let yaw = self.camera.yaw;
            let forward_flat = Vec3::new(yaw.sin(), 0.0, -yaw.cos());
            let right_flat = Vec3::new(-forward_flat.z, 0.0, forward_flat.x);
            if self.held(KeyCode::KeyW) {
                wish += forward_flat;
            }
            if self.held(KeyCode::KeyS) {
                wish -= forward_flat;
            }
            if self.held(KeyCode::KeyD) {
                wish += right_flat;
            }
            if self.held(KeyCode::KeyA) {
                wish -= right_flat;
            }
            if wish.length_squared() > 0.0 {
                wish = wish.normalize();
                if self.held(KeyCode::ShiftLeft) {
                    wish *= SPRINT_MULTIPLIER;
                }
            }
            // Vertical control only means something while flying (creative
            // mode); Space otherwise means jump, handled below instead.
            if self.player.flying {
                if self.held(KeyCode::Space) {
                    wish.y += 1.0;
                }
                if self.held(KeyCode::ControlLeft) {
                    wish.y -= 1.0;
                }
            }
            jump = !self.player.flying && self.held(KeyCode::Space);
        }
        self.input.mouse_delta = (0.0, 0.0);

        let chunks = &self.chunks;
        self.player.update(dt, wish, jump, |x, y, z| is_solid_in(chunks, x, y, z));
        self.camera.position = self.player.eye_position();

        if self.update_streaming() {
            self.world_mesh_dirty = true;
        }
        self.update_mobs(dt);

        if self.cursor_locked && (self.input.mine_requested || self.input.place_requested) {
            self.handle_interaction();
        }
        self.input.mine_requested = false;
        self.input.place_requested = false;

        // Rebuild (expensive: re-triangulates the world) only when the world
        // actually changed; upload (also expensive: see MESH_UPLOAD_INTERVAL)
        // at a bounded rate rather than every frame, so continuous mob motion
        // can't reintroduce the per-frame GPU stall this replaced.
        if self.world_mesh_dirty {
            self.rebuild_world_mesh();
        }
        self.mesh_upload_accum += dt;
        if self.mesh_upload_accum >= MESH_UPLOAD_INTERVAL {
            self.mesh_upload_accum = 0.0;
            self.upload_combined_mesh();
        }

        if let Some(renderer) = self.renderer.as_mut() {
            if let Err(e) = renderer.render_frame(&self.camera) {
                eprintln!("render error: {e:#}");
            }
        }
        if let Some(window) = &self.window {
            window.request_redraw();
        }
    }

    fn handle_interaction(&mut self) {
        let chunks = &self.chunks;
        let hit = raycast_voxels(self.camera.position, self.camera.forward(), MAX_REACH, |x, y, z| {
            is_solid_in(chunks, x, y, z)
        });
        let Some(hit) = hit else { return };

        if self.input.mine_requested {
            if self.set_world_block(hit.block, AIR) {
                if let Some(audio) = &self.audio {
                    audio.play_mine();
                }
            }
        } else if self.input.place_requested {
            let target = hit.block + hit.normal;
            let already_solid = is_solid_in(&self.chunks, target.x, target.y, target.z);
            let overlaps_player = aabb_contains_cell(self.player.position, target);
            if !already_solid
                && !overlaps_player
                && self.set_world_block(target, self.selected_block)
            {
                if let Some(audio) = &self.audio {
                    audio.play_place();
                }
            }
        }
    }

    /// World-space edit, routed to the owning chunk. False (no-op) if the
    /// containing section isn't loaded — callers already gate on `is_solid_in`
    /// / a successful raycast hit, so this only fails for the current-frame
    /// unlucky case of a column being evicted between raycast and edit.
    fn set_world_block(&mut self, world: glam::IVec3, block: BlockId) -> bool {
        let (cx, cz) = (world.x.div_euclid(SECTION_DIM), world.z.div_euclid(SECTION_DIM));
        let (lx, lz) =
            (world.x.rem_euclid(SECTION_DIM) as usize, world.z.rem_euclid(SECTION_DIM) as usize);
        let edited = self.chunks.set_block(cx, cz, lx, world.y, lz, block);
        if edited {
            // Each section's mesh only depends on its own content (greedy_mesh
            // never looks across section boundaries — a solid block at a
            // section's own top/bottom slice is always treated as exposed
            // there regardless of the neighbor), so only this one section
            // needs re-meshing, not the whole world.
            let sy = world.y.div_euclid(SECTION_DIM);
            self.dirty_sections.insert((cx, sy, cz));
            self.world_mesh_dirty = true;
        }
        edited
    }

    fn held(&self, key: KeyCode) -> bool {
        self.input.held.contains(&key)
    }

    fn set_cursor_lock(&mut self, locked: bool) {
        self.cursor_locked = locked;
        let Some(window) = &self.window else { return };
        if locked {
            if window.set_cursor_grab(CursorGrabMode::Locked).is_err() {
                let _ = window.set_cursor_grab(CursorGrabMode::Confined);
            }
            window.set_cursor_visible(false);
        } else {
            let _ = window.set_cursor_grab(CursorGrabMode::None);
            window.set_cursor_visible(true);
        }
        self.rebuild_ui(); // crosshair only shows while locked
    }

    /// Rebuilds and uploads the screen-space UI overlay: the inventory grid
    /// while it's open, else the crosshair while the cursor is locked
    /// (nothing to aim at otherwise), else nothing. Call only on an actual
    /// state change (inventory toggle, cursor lock toggle, resize) — like
    /// `set_mesh`, `set_ui_mesh` has the same per-call cost as `set_mesh`,
    /// never call unconditionally every frame.
    fn rebuild_ui(&mut self) {
        let Some(renderer) = self.renderer.as_mut() else { return };
        let mut vertices = Vec::new();
        let mut indices = Vec::new();
        if self.inventory_open {
            let items = inventory_items(&self.items, &self.blocks);
            let selected_id = items
                .iter()
                .find(|&&(_, block_id)| BlockId(block_id) == self.selected_block)
                .map(|&(item_id, _)| item_id);
            let (inv_v, inv_i) = ui::inventory_mesh(
                &items,
                selected_id,
                self.camera.aspect,
                render_vk::swatch_color,
            );
            let base = vertices.len() as u32;
            indices.extend(inv_i.into_iter().map(|i| i + base));
            vertices.extend(inv_v);
        } else if self.cursor_locked {
            let (cross_v, cross_i) = ui::crosshair(self.camera.aspect);
            let base = vertices.len() as u32;
            indices.extend(cross_i.into_iter().map(|i| i + base));
            vertices.extend(cross_v);
        }
        if let Err(e) = renderer.set_ui_mesh(&vertices, &indices) {
            eprintln!("ui mesh upload error: {e:#}");
        }
    }

    /// Opens (frees the cursor) or closes (re-locks it) the inventory.
    fn set_inventory_open(&mut self, open: bool) {
        self.inventory_open = open;
        self.set_cursor_lock(!open); // set_cursor_lock already calls rebuild_ui
    }

    /// Hit-tests the last known cursor position against the inventory grid;
    /// selects the block under it and closes the inventory. No-op (leaves
    /// the inventory open) if the click missed every cell — including the
    /// dim backdrop, so clicking outside the grid doesn't silently no-op in
    /// a confusing way, it's just not a hit.
    fn pick_inventory_item_at_cursor(&mut self) {
        let Some(window) = &self.window else { return };
        let size = window.inner_size();
        if size.width == 0 || size.height == 0 {
            return;
        }
        let ndc_x = (self.input.cursor_pos.0 / size.width as f64) as f32 * 2.0 - 1.0;
        let ndc_y = (self.input.cursor_pos.1 / size.height as f64) as f32 * 2.0 - 1.0;
        let items = inventory_items(&self.items, &self.blocks);
        if let Some(idx) = ui::inventory_hit_test(ndc_x, ndc_y, items.len(), self.camera.aspect) {
            self.selected_block = BlockId(items[idx].1);
            self.set_inventory_open(false);
        }
    }
}

/// True if the player's AABB (centered at `player_pos`) overlaps unit cell `cell`.
fn aabb_contains_cell(player_pos: Vec3, cell: glam::IVec3) -> bool {
    use engine_core::physics::{PLAYER_HALF_HEIGHT, PLAYER_HALF_WIDTH};
    let half = Vec3::new(PLAYER_HALF_WIDTH, PLAYER_HALF_HEIGHT, PLAYER_HALF_WIDTH);
    let (min, max) = (player_pos - half, player_pos + half);
    let cell_min = Vec3::new(cell.x as f32, cell.y as f32, cell.z as f32);
    let cell_max = cell_min + Vec3::ONE;
    min.x < cell_max.x && max.x > cell_min.x
        && min.y < cell_max.y && max.y > cell_min.y
        && min.z < cell_max.z && max.z > cell_min.z
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let window = Arc::new(
            event_loop
                .create_window(
                    Window::default_attributes()
                        .with_title("voxel-engine")
                        .with_inner_size(winit::dpi::LogicalSize::new(1280, 720)),
                )
                .expect("failed to create window"),
        );

        // Mouse-look: hide the cursor and keep it from leaving the window.
        // `Locked` (recenters every frame) is nicer but not universally
        // supported; fall back to `Confined` rather than failing outright.
        if window.set_cursor_grab(CursorGrabMode::Locked).is_err() {
            let _ = window.set_cursor_grab(CursorGrabMode::Confined);
        }
        window.set_cursor_visible(false);

        let size = window.inner_size();
        if size.width > 0 && size.height > 0 {
            self.camera.aspect = size.width as f32 / size.height as f32;
        }

        let renderer = VkRenderer::new(window.clone()).expect("failed to init Vulkan renderer");
        self.window = Some(window);
        self.renderer = Some(renderer);

        // Block briefly for the starting neighborhood so the first frame has
        // ground under the player instead of streaming in visibly. Real
        // terrain generation is heavier than the old void generator, so give
        // it a longer (still bounded) budget; whatever hasn't finished keeps
        // streaming in normally once the loop runs.
        let center = world_chunk_of(self.player.position);
        self.streaming_center = Some(center);
        self.chunks.set_center(center.0, center.1);
        let deadline = Instant::now() + Duration::from_secs(10);
        while self.chunks.pending() > 0 && Instant::now() < deadline {
            self.chunks.pump();
            std::thread::sleep(Duration::from_millis(2));
        }
        self.chunks.pump();

        self.rebuild_world_mesh();
        self.upload_combined_mesh();
        self.rebuild_ui();
        self.mesh_upload_accum = 0.0;
        self.last_frame = Some(Instant::now());
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => {
                if let Some(r) = self.renderer.as_mut() {
                    r.resize(size.width, size.height);
                }
                if size.width > 0 && size.height > 0 {
                    self.camera.aspect = size.width as f32 / size.height as f32;
                    self.rebuild_ui(); // crosshair/inventory shape depends on aspect
                }
            }
            WindowEvent::KeyboardInput { event, .. } => {
                let PhysicalKey::Code(code) = event.physical_key else { return };
                if code == KeyCode::Escape && event.state == ElementState::Pressed {
                    if self.inventory_open {
                        self.set_inventory_open(false);
                    } else {
                        // Free the cursor instead of quitting — Alt+F4 / the
                        // window's close button still quit normally.
                        self.set_cursor_lock(!self.cursor_locked);
                    }
                    return;
                }
                if code == KeyCode::KeyE && event.state == ElementState::Pressed {
                    self.set_inventory_open(!self.inventory_open);
                    return;
                }
                if event.state == ElementState::Pressed {
                    let slot = match code {
                        KeyCode::Digit1 => Some(0),
                        KeyCode::Digit2 => Some(1),
                        KeyCode::Digit3 => Some(2),
                        KeyCode::Digit4 => Some(3),
                        _ => None,
                    };
                    if let Some(slot) = slot {
                        self.selected_block =
                            block_for_item(&self.items, &self.blocks, HOTBAR_ITEM_IDS[slot]);
                    }
                    if code == KeyCode::KeyF {
                        self.player.toggle_flying();
                    }
                }
                match event.state {
                    ElementState::Pressed => {
                        self.input.held.insert(code);
                    }
                    ElementState::Released => {
                        self.input.held.remove(&code);
                    }
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.input.cursor_pos = (position.x, position.y);
            }
            WindowEvent::MouseInput { state: ElementState::Pressed, button, .. } => {
                if self.inventory_open {
                    if button == MouseButton::Left {
                        self.pick_inventory_item_at_cursor();
                    }
                } else if !self.cursor_locked {
                    // Click back into the window to resume, instead of that
                    // click registering as mine/place.
                    self.set_cursor_lock(true);
                } else {
                    match button {
                        MouseButton::Left => self.input.mine_requested = true,
                        MouseButton::Right => self.input.place_requested = true,
                        _ => {}
                    }
                }
            }
            WindowEvent::RedrawRequested => self.update_and_render(),
            _ => {}
        }
    }

    fn device_event(&mut self, _event_loop: &ActiveEventLoop, _device_id: DeviceId, event: DeviceEvent) {
        if let DeviceEvent::MouseMotion { delta } = event {
            self.input.mouse_delta.0 += delta.0;
            self.input.mouse_delta.1 += delta.1;
        }
    }
}

fn main() -> anyhow::Result<()> {
    let event_loop = EventLoop::new()?;
    event_loop.set_control_flow(ControlFlow::Poll);
    let mut app = App::default();
    event_loop.run_app(&mut app)?;
    Ok(())
}
