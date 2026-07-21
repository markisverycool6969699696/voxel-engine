//! Platform-agnostic engine core. Rendering backends (Vulkan, later Metal)
//! implement [`Renderer`]; everything above the backend talks only to this trait.

pub mod camera;
pub mod chunk;
pub mod mesh;
pub mod mob;
pub mod pathfind;
pub mod physics;
pub mod raycast;
pub mod registry;
pub mod streaming;
pub mod worldgen;

/// A platform rendering backend.
///
/// Contract:
/// - `render_frame` renders and presents one frame using the most recent
///   `set_mesh` upload and the given camera. Must internally handle swapchain
///   invalidation (out-of-date/suboptimal) and window minimization (returning
///   `Ok` without rendering is valid when the surface is zero-sized).
/// - `resize` is a hint from the windowing layer; the backend recreates its
///   swapchain lazily on the next `render_frame`.
/// - `set_mesh` replaces the currently drawn geometry wholesale. One mesh for
///   now (no per-chunk placement/instancing yet) — callers bake world
///   position into vertex data themselves.
pub trait Renderer {
    fn render_frame(&mut self, camera: &camera::Camera) -> anyhow::Result<()>;
    fn resize(&mut self, width: u32, height: u32);
    fn set_mesh(&mut self, vertices: &[mesh::MeshVertex], indices: &[u32]) -> anyhow::Result<()>;
    /// Replaces the screen-space UI overlay (crosshair, inventory, menus),
    /// drawn on top of the world mesh every frame with no depth test. Same
    /// wholesale-replace, call-sparingly contract as `set_mesh` — see its
    /// docs and the backend's own notes on why this isn't safe to call every
    /// frame unconditionally.
    fn set_ui_mesh(&mut self, vertices: &[mesh::UiVertex], indices: &[u32]) -> anyhow::Result<()>;
}
