//! Platform binary: window + event loop, driving the renderer via the
//! `engine_core::Renderer` trait. Vulkan-only for now (Metal deferred).
//!
//! Scene content is a single hardcoded, mutable `PalettedSection` (no world
//! gen yet — that's a separate, not-yet-built subsystem) run through greedy
//! meshing, with a physics-driven player (gravity, collision, jump) and
//! mouse-driven block mining/placing via voxel raycast. Enough to sanity
//! check chunk data → mesh → GPU rendering → input → physics → world edit as
//! one real loop, even though the "world" itself is still a fixed structure.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Instant;

use engine_core::camera::Camera;
use engine_core::chunk::{BlockId, PalettedSection, AIR};
use engine_core::mesh::{greedy_mesh, triangulate};
use engine_core::physics::PlayerController;
use engine_core::raycast::raycast_voxels;
use engine_core::Renderer;
use glam::Vec3;
use render_vk::VkRenderer;
use winit::application::ApplicationHandler;
use winit::event::{DeviceEvent, DeviceId, ElementState, MouseButton, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::window::{CursorGrabMode, Window, WindowId};

const SPRINT_MULTIPLIER: f32 = 1.6;
const MOUSE_SENSITIVITY: f32 = 0.0025;
const MAX_PITCH: f32 = 1.5; // just under vertical, avoids the look-direction singularity
const MAX_REACH: f32 = 6.0;

const SECTION_DIM: i32 = 16;

/// Placeholder hotbar (number keys 1-4) until there's a real inventory —
/// same debug-colored block ids used everywhere else, no new content.
const HOTBAR: [BlockId; 4] = [BlockId(1), BlockId(2), BlockId(3), BlockId(4)];

fn is_solid_in(section: &PalettedSection, x: i32, y: i32, z: i32) -> bool {
    if x < 0 || y < 0 || z < 0 || x >= SECTION_DIM || y >= SECTION_DIM || z >= SECTION_DIM {
        return false; // outside the demo section: open air/void, not solid
    }
    section.get(x as usize, y as usize, z as usize) != AIR
}

/// A small hand-built structure: base platform, border, center staircase,
/// and one floating block (to eyeball depth-test occlusion). Placeholder
/// until real world generation exists.
fn build_demo_section() -> PalettedSection {
    const STONE: BlockId = BlockId(1);
    const DIRT: BlockId = BlockId(2);

    let mut s = PalettedSection::filled(AIR);
    for z in 0..16 {
        for x in 0..16 {
            s.set(x, 0, z, STONE);
        }
    }
    for i in 0..16 {
        s.set(i, 1, 0, DIRT);
        s.set(i, 1, 15, DIRT);
        s.set(0, 1, i, DIRT);
        s.set(15, 1, i, DIRT);
    }
    for step in 0..4usize {
        let y = 1 + step;
        for x in (4 + step)..(12 - step) {
            for z in (4 + step)..(12 - step) {
                s.set(x, y, z, STONE);
            }
        }
    }
    s.set(8, 10, 8, DIRT);
    s
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
    section: PalettedSection,
    input: Input,
    last_frame: Option<Instant>,
    selected_block: BlockId,
}

impl Default for App {
    fn default() -> Self {
        // Standing on the platform near a corner, clear of the border wall
        // and the center staircase; aspect is corrected once the window
        // exists (see `resumed`).
        let player = PlayerController::new(Vec3::new(
            2.5,
            1.0 + engine_core::physics::PLAYER_HALF_HEIGHT + 0.1,
            2.5,
        ));
        let mut camera = Camera::new(player.eye_position(), 1.0);
        camera.yaw = 2.0; // facing roughly toward the staircase from this corner
        camera.pitch = -0.1;
        Self {
            window: None,
            renderer: None,
            camera,
            player,
            section: build_demo_section(),
            input: Input::default(),
            last_frame: None,
            selected_block: HOTBAR[0],
        }
    }
}

impl App {
    fn rebuild_mesh(&mut self) {
        let Some(renderer) = self.renderer.as_mut() else { return };
        let quads = greedy_mesh(&self.section, |b| b != AIR);
        let (vertices, indices) = triangulate(&quads);
        if let Err(e) = renderer.set_mesh(&vertices, &indices) {
            eprintln!("mesh upload error: {e:#}");
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
        let jump = self.held(KeyCode::Space);

        let section = &self.section;
        self.player.update(dt, wish, jump, |x, y, z| is_solid_in(section, x, y, z));
        self.camera.position = self.player.eye_position();

        if self.input.mine_requested || self.input.place_requested {
            self.handle_interaction();
        }
        self.input.mine_requested = false;
        self.input.place_requested = false;

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
        let section = &self.section;
        let hit = raycast_voxels(self.camera.position, self.camera.forward(), MAX_REACH, |x, y, z| {
            is_solid_in(section, x, y, z)
        });
        let Some(hit) = hit else { return };

        let mut edited = false;
        if self.input.mine_requested {
            if let (Ok(x), Ok(y), Ok(z)) = (
                usize::try_from(hit.block.x),
                usize::try_from(hit.block.y),
                usize::try_from(hit.block.z),
            ) {
                self.section.set(x, y, z, AIR);
                edited = true;
            }
        } else if self.input.place_requested {
            let target = hit.block + hit.normal;
            if let (Ok(x), Ok(y), Ok(z)) =
                (usize::try_from(target.x), usize::try_from(target.y), usize::try_from(target.z))
            {
                let already_solid = is_solid_in(&self.section, target.x, target.y, target.z);
                let overlaps_player = aabb_contains_cell(self.player.position, target);
                if !already_solid && !overlaps_player {
                    self.section.set(x, y, z, self.selected_block);
                    edited = true;
                }
            }
        }
        if edited {
            self.rebuild_mesh();
        }
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
                        self.selected_block = HOTBAR[slot];
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
