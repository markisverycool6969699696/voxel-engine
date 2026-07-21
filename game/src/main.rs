//! Platform binary: window + event loop, driving the renderer via the
//! `engine_core::Renderer` trait. Vulkan-only for now (Metal deferred).
//!
//! World content is served through `engine_core::streaming::ChunkManager`
//! (background-threaded, load/unload by radius around the player), fed by
//! `engine_core::worldgen::TerrainGenerator` — real seeded heightmap terrain
//! with biomes, water, caves, ore, and trees. The player and mobs are placed
//! on the generated surface at startup.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use engine_core::camera::Camera;
use engine_core::chunk::{BlockId, PalettedSection, AIR};
use engine_core::mesh::{greedy_mesh, triangulate, MeshVertex};
use engine_core::mob::Mob;
use engine_core::physics::PlayerController;
use engine_core::raycast::raycast_voxels;
use engine_core::registry::{BlockDef, ItemDef, Registry};
use engine_core::streaming::{ChunkManager, StreamingConfig};
use engine_core::worldgen::TerrainGenerator;
use engine_core::Renderer;
use glam::Vec3;
use render_vk::VkRenderer;
use winit::application::ApplicationHandler;
use winit::event::{DeviceEvent, DeviceId, ElementState, MouseButton, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::window::{CursorGrabMode, Window, WindowId};

mod audio;
use audio::Audio;

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

/// Hotbar (number keys 1-4): item ids resolved through the data-driven
/// `Registry<ItemDef>`/`Registry<BlockDef>` (see `data/items.json`,
/// `data/blocks.json`) instead of a raw block-id array. Still the same 4
/// debug-colored placeholder blocks as before — this wires the existing
/// registry module into actual use, it doesn't invent new content (the
/// "final v1 block/item list" is explicitly an open decision per
/// docs/STARTER.md §8, not made here).
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

/// Placeholder mob appearance/size — not a real mob roster, just enough to
/// prove wandering AI + collision + rendering work end to end.
const MOB_BLOCK: BlockId = BlockId(12);
const MOB_SIZE: Vec3 = Vec3::new(0.6, 0.8, 0.6);
const MOB_WALK_SPEED: f32 = 1.5;

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
    mobs: Vec<Mob>,
    blocks: Registry<BlockDef>,
    items: Registry<ItemDef>,
    input: Input,
    last_frame: Option<Instant>,
    selected_block: BlockId,
    /// `None` if no audio output device is available — sound is a nice-to-have.
    audio: Option<Audio>,
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
            Mob::new(
                Vec3::new(wx as f32 + 0.5, h as f32 + MOB_SIZE.y / 2.0 + 0.5, wz as f32 + 0.5),
                MOB_SIZE / 2.0,
                seed,
            )
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
        }
    }
}

impl App {
    /// Merges every loaded chunk's mesh plus every mob's box into one
    /// combined buffer and uploads it. `Renderer::set_mesh` only holds one
    /// mesh at a time, so a per-chunk/per-entity GPU resource split is
    /// future work if the streamed world (or mob count) grows enough to
    /// make full rebuilds too slow — fine for the current small scale, and
    /// mobs move every frame regardless, so this already has to run every
    /// frame rather than only on world change.
    fn rebuild_mesh(&mut self) {
        let Some(renderer) = self.renderer.as_mut() else { return };
        let mut vertices = Vec::new();
        let mut indices = Vec::new();
        for ((cx, cz), column) in self.chunks.columns() {
            for (sy, section) in column.loaded_sections() {
                let quads = greedy_mesh(section, |b| b != AIR);
                if quads.is_empty() {
                    continue;
                }
                let (mut section_vertices, section_indices) = triangulate(&quads);
                let offset = Vec3::new(
                    (cx * SECTION_DIM) as f32,
                    (sy * SECTION_DIM) as f32,
                    (cz * SECTION_DIM) as f32,
                );
                for v in &mut section_vertices {
                    v.position[0] += offset.x;
                    v.position[1] += offset.y;
                    v.position[2] += offset.z;
                }
                let base = vertices.len() as u32;
                indices.extend(section_indices.into_iter().map(|i| i + base));
                vertices.extend(section_vertices);
            }
        }
        for mob in &self.mobs {
            let (mob_vertices, mob_indices) = mob_box_mesh(mob.position, MOB_SIZE, MOB_BLOCK);
            let base = vertices.len() as u32;
            indices.extend(mob_indices.into_iter().map(|i| i + base));
            vertices.extend(mob_vertices);
        }
        if let Err(e) = renderer.set_mesh(&vertices, &indices) {
            eprintln!("mesh upload error: {e:#}");
        }
    }

    /// Moves the streaming radius to follow the player and integrates any
    /// finished background generation.
    fn update_streaming(&mut self) {
        let center = world_chunk_of(self.player.position);
        if self.streaming_center != Some(center) {
            self.streaming_center = Some(center);
            self.chunks.set_center(center.0, center.1);
        }
        self.chunks.pump();
    }

    fn update_mobs(&mut self, dt: f32) {
        let chunks = &self.chunks;
        for mob in &mut self.mobs {
            mob.update(dt, MOB_WALK_SPEED, |x, y, z| is_solid_in(chunks, x, y, z));
        }
    }

    fn update_and_render(&mut self) {
        let now = Instant::now();
        let dt = self
            .last_frame
            .map(|prev| (now - prev).as_secs_f32())
            .unwrap_or(0.0);
        self.last_frame = Some(now);

        self.camera.yaw += self.input.mouse_delta.0 as f32 * MOUSE_SENSITIVITY;
        self.camera.pitch = (self.camera.pitch - self.input.mouse_delta.1 as f32 * MOUSE_SENSITIVITY)
            .clamp(-MAX_PITCH, MAX_PITCH);
        self.input.mouse_delta = (0.0, 0.0);

        // Horizontal movement uses yaw only (not pitch) — a walking player
        // doesn't move forward-into-the-ground just from looking down.
        let yaw = self.camera.yaw;
        let forward_flat = Vec3::new(yaw.sin(), 0.0, -yaw.cos());
        let right_flat = Vec3::new(-forward_flat.z, 0.0, forward_flat.x);
        let mut wish = Vec3::ZERO;
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
        let jump = !self.player.flying && self.held(KeyCode::Space);

        let chunks = &self.chunks;
        self.player.update(dt, wish, jump, |x, y, z| is_solid_in(chunks, x, y, z));
        self.camera.position = self.player.eye_position();

        self.update_streaming();
        self.update_mobs(dt);

        if self.input.mine_requested || self.input.place_requested {
            self.handle_interaction();
        }
        self.input.mine_requested = false;
        self.input.place_requested = false;

        self.rebuild_mesh();

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
        self.chunks.set_block(cx, cz, lx, world.y, lz, block)
    }

    fn held(&self, key: KeyCode) -> bool {
        self.input.held.contains(&key)
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

        self.rebuild_mesh();
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
                }
            }
            WindowEvent::KeyboardInput { event, .. } => {
                let PhysicalKey::Code(code) = event.physical_key else { return };
                if code == KeyCode::Escape && event.state == ElementState::Pressed {
                    event_loop.exit();
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
            WindowEvent::MouseInput { state: ElementState::Pressed, button, .. } => match button {
                MouseButton::Left => self.input.mine_requested = true,
                MouseButton::Right => self.input.place_requested = true,
                _ => {}
            },
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
