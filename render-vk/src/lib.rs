//! Vulkan rendering backend (Windows/Linux primary target).
//!
//! Checkpoint 1: instance → surface → device → swapchain → per-frame sync →
//! clear-screen render loop, using Vulkan 1.3 dynamic rendering and
//! synchronization2.
//!
//! Checkpoint 2: a graphics pipeline drawing a hardcoded triangle from a
//! `vk-mem` vertex buffer. Shaders authored in WGSL, cross-compiled to
//! SPIR-V via `naga` at init time (see `shader.rs`) — no glslc/Vulkan SDK
//! dependency.
//!
//! Checkpoint 3: real 3D scenes. `set_mesh` uploads arbitrary
//! `engine_core::mesh::MeshVertex` geometry (greedy-meshed chunks, in
//! practice); `render_frame` takes an `engine_core::camera::Camera` and
//! writes its view-projection matrix into a per-frame uniform buffer bound
//! via a descriptor set. Added a depth buffer (recreated alongside the
//! swapchain) with standard less-than depth testing.
//!
//! Back-face culling is **temporarily disabled** (`CullModeFlags::NONE`)
//! while chasing a real "upside down" rendering bug confirmed by screenshot
//! (see MEMORY.md and `engine_core::camera`'s doc comment, which just
//! removed a clip-space Y-negation as the fix). The correct `FrontFace` for
//! the post-fix pipeline depends on that flip's presence and wasn't
//! independently derivable with confidence — a first attempt at deriving it
//! for the *previous* (flipped) state was wrong (see git history), so this
//! round deliberately isolates the orientation fix from the culling-
//! direction guess rather than compounding two unconfirmed changes. See the
//! rasterization state below for the re-enable plan.
//!
//! Sync design (the part that must be right):
//! - `FRAMES_IN_FLIGHT = 2` frames, each with its own command buffer,
//!   `image_available` semaphore, and `in_flight` fence.
//! - `render_finished` semaphores are **per swapchain image**, not per frame:
//!   a present operation may still be reading its wait semaphore after the
//!   frame slot cycles, so per-frame reuse is a validation error / UB.
//! - Swapchain recreation is lazy: resize events and OUT_OF_DATE/SUBOPTIMAL
//!   results set a dirty flag; the next `render_frame` recreates.
//! - `set_mesh` calls `device_wait_idle` before replacing buffers — correct
//!   but not meant to be called every frame (no per-frame mesh streaming yet).
//!
//! Checkpoint 4: a placeholder texture atlas. No real block textures exist
//! (spec §8: sourcing a CC0/GPL texture pack is still an open decision), so
//! `generate_atlas_pixels` procedurally generates one instead of loading
//! image files — deliberately zero new dependencies here (no image-decoding
//! crate), partly because the previous attempt at a new dependency (`rodio`
//! for sound) crashed the whole binary at link time on this project's GNU
//! toolchain (see MEMORY.md) and this avoids repeating that risk. The atlas
//! is uploaded once at init via a staging buffer + one-shot command buffer
//! (`create_atlas`), matching the standard device-local-image upload pattern.
//! Texture + sampler are separate descriptor bindings (WGSL has no combined-
//! image-sampler type; naga's SPIR-V backend emits separate
//! `SAMPLED_IMAGE`/`SAMPLER` descriptors for `texture_2d`/`sampler`, so the
//! Vulkan-side layout mirrors that rather than fighting it).

use std::ffi::CStr;
use std::mem::ManuallyDrop;
use std::sync::Arc;

use anyhow::{Context, Result};
use ash::vk;
use raw_window_handle::{HasDisplayHandle, HasWindowHandle};
use vk_mem::{Alloc, AllocationCreateFlags, AllocationCreateInfo, Allocator, AllocatorCreateInfo, MemoryUsage};
use winit::window::Window;

use engine_core::camera::Camera;
use engine_core::mesh::{MeshVertex, TileSource, UiVertex, ATLAS_TILES, ATLAS_TILE_COUNT};
use engine_core::Renderer;

mod atlas_textures;
mod shader;

const FRAMES_IN_FLIGHT: usize = 2;
const VALIDATION_LAYER: &CStr = c"VK_LAYER_KHRONOS_validation";
/// Candidates in preference order; the first with optimal-tiling
/// depth-attachment support on this physical device wins.
const DEPTH_FORMAT_CANDIDATES: [vk::Format; 3] = [
    vk::Format::D32_SFLOAT,
    vk::Format::D24_UNORM_S8_UINT,
    vk::Format::D16_UNORM,
];
const ATLAS_TILE_SIZE: u32 = 16;
/// `R8G8B8A8_UNORM` is one of Vulkan's mandatory-support formats for
/// sampled + transfer-dst optimal-tiling images, unlike the depth format —
/// no capability query needed.
const ATLAS_FORMAT: vk::Format = vk::Format::R8G8B8A8_UNORM;

/// Host-side mirror of `mesh.wgsl`'s `Globals` uniform struct. WGSL's
/// uniform-address-space layout rules require the struct's total size be a
/// multiple of its largest member's alignment (16, from `mat4x4<f32>`);
/// `_pad` makes that explicit here instead of relying on `#[repr(C)]` to
/// happen to match (64 + 4 = 68, padded to 80).
#[repr(C)]
struct GlobalsUbo {
    view_proj: [f32; 16],
    atlas_tile_count: f32,
    _pad: [f32; 3],
}

/// Builds a single-row atlas: one `ATLAS_TILE_SIZE`² tile per
/// `engine_core::mesh::ATLAS_TILES` entry, in order. `TileSource::Png` tiles
/// are decoded from the real embedded GPL v2 textures (see
/// `atlas_textures`/CREDITS.md); `TileSource::Solid` tiles (water, snow,
/// the mob debug marker — no equivalent in that pack) get a flat color with
/// a coarse 4x4 checker, the same treatment the whole atlas used to get
/// before real textures existed, so they still read as "a tile" rather
/// than a flat swatch.
fn generate_atlas_pixels() -> Vec<u8> {
    let (width, height) = (ATLAS_TILE_COUNT * ATLAS_TILE_SIZE, ATLAS_TILE_SIZE);
    let mut pixels = vec![0u8; (width * height * 4) as usize];
    for (tile, &(block_id, source)) in ATLAS_TILES.iter().enumerate() {
        let tile = tile as u32;
        match source {
            TileSource::Png(name) => {
                let bytes = atlas_textures::png_bytes(name)
                    .unwrap_or_else(|| panic!("no embedded texture for {name:?} (block {block_id})"));
                let img = image::load_from_memory(bytes)
                    .unwrap_or_else(|e| panic!("failed to decode {name:?}.png: {e:#}"))
                    .to_rgba8();
                assert_eq!(
                    (img.width(), img.height()),
                    (ATLAS_TILE_SIZE, ATLAS_TILE_SIZE),
                    "{name:?}.png must be {ATLAS_TILE_SIZE}x{ATLAS_TILE_SIZE}"
                );
                for y in 0..ATLAS_TILE_SIZE {
                    for x in 0..ATLAS_TILE_SIZE {
                        let src = img.get_pixel(x, y).0;
                        let px = tile * ATLAS_TILE_SIZE + x;
                        let idx = ((y * width + px) * 4) as usize;
                        pixels[idx..idx + 4].copy_from_slice(&src);
                    }
                }
            }
            TileSource::Solid(base) => {
                for y in 0..ATLAS_TILE_SIZE {
                    for x in 0..ATLAS_TILE_SIZE {
                        let darker = ((x / 4) + (y / 4)) % 2 == 0;
                        let mul = if darker { 0.75 } else { 1.0 };
                        let px = tile * ATLAS_TILE_SIZE + x;
                        let idx = ((y * width + px) * 4) as usize;
                        pixels[idx] = (base[0] as f32 * mul) as u8;
                        pixels[idx + 1] = (base[1] as f32 * mul) as u8;
                        pixels[idx + 2] = (base[2] as f32 * mul) as u8;
                        pixels[idx + 3] = 255;
                    }
                }
            }
        }
    }
    pixels
}

/// Average color of a block's atlas tile, RGBA 0..1 — for UI swatches (the
/// inventory picker) that should visually match the real in-world texture
/// rather than showing an arbitrary hash color. `Solid` tiles return their
/// exact color; `Png` tiles are decoded and averaged (a tiny 16x16 image,
/// cheap enough to redo per call — this only runs when the inventory UI
/// rebuilds, not every frame). Falls back to mid-gray for an unregistered
/// block id.
pub fn swatch_color(block_id: u16) -> [f32; 4] {
    let Some(&(_, source)) = ATLAS_TILES.iter().find(|&&(id, _)| id == block_id) else {
        return [0.6, 0.6, 0.6, 1.0];
    };
    match source {
        TileSource::Solid([r, g, b]) => {
            [r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0, 1.0]
        }
        TileSource::Png(name) => {
            let bytes = atlas_textures::png_bytes(name)
                .unwrap_or_else(|| panic!("no embedded texture for {name:?}"));
            let img = image::load_from_memory(bytes)
                .unwrap_or_else(|e| panic!("failed to decode {name:?}.png: {e:#}"))
                .to_rgba8();
            let (mut r, mut g, mut b) = (0u32, 0u32, 0u32);
            let n = (img.width() * img.height()).max(1);
            for p in img.pixels() {
                r += p[0] as u32;
                g += p[1] as u32;
                b += p[2] as u32;
            }
            [
                (r / n) as f32 / 255.0,
                (g / n) as f32 / 255.0,
                (b / n) as f32 / 255.0,
                1.0,
            ]
        }
    }
}

struct FrameSync {
    image_available: vk::Semaphore,
    in_flight: vk::Fence,
    cmd: vk::CommandBuffer,
}

pub struct VkRenderer {
    // Window kept alive for surface validity and inner_size queries on recreate.
    window: Arc<Window>,

    _entry: ash::Entry,
    instance: ash::Instance,
    surface_loader: ash::khr::surface::Instance,
    surface: vk::SurfaceKHR,

    phys_device: vk::PhysicalDevice,
    device: ash::Device,
    queue: vk::Queue,

    depth_format: vk::Format,

    swapchain_loader: ash::khr::swapchain::Device,
    swapchain: vk::SwapchainKHR,
    swapchain_format: vk::SurfaceFormatKHR,
    swapchain_extent: vk::Extent2D,
    swapchain_images: Vec<vk::Image>,
    swapchain_views: Vec<vk::ImageView>,
    /// Per swapchain image (see module docs).
    render_finished: Vec<vk::Semaphore>,

    // Depth buffer: recreated alongside the swapchain in `create_swapchain`.
    depth_image: vk::Image,
    depth_allocation: vk_mem::Allocation,
    depth_view: vk::ImageView,

    command_pool: vk::CommandPool,
    frames: Vec<FrameSync>,
    frame_index: usize,

    // Placeholder texture atlas: created once in `create_atlas`, immutable
    // thereafter (no swapchain dependency, unlike the depth buffer).
    atlas_image: vk::Image,
    atlas_allocation: vk_mem::Allocation,
    atlas_view: vk::ImageView,
    atlas_sampler: vk::Sampler,

    descriptor_set_layout: vk::DescriptorSetLayout,
    descriptor_pool: vk::DescriptorPool,
    /// Per frame-in-flight: one uniform buffer + descriptor set for the
    /// camera's view-projection matrix (written fresh every `record_frame`).
    uniform_buffers: Vec<vk::Buffer>,
    uniform_allocations: Vec<vk_mem::Allocation>,
    descriptor_sets: Vec<vk::DescriptorSet>,

    pipeline_layout: vk::PipelineLayout,
    pipeline: vk::Pipeline,

    // Screen-space UI overlay: no depth test, no culling, alpha-blended,
    // empty descriptor/pipeline layout (NDC-direct vertices, no camera/atlas
    // needed), drawn in the same render pass right after the world mesh.
    ui_pipeline_layout: vk::PipelineLayout,
    ui_pipeline: vk::Pipeline,
    ui_vertex_buffer: vk::Buffer,
    ui_vertex_allocation: vk_mem::Allocation,
    ui_index_buffer: vk::Buffer,
    ui_index_allocation: vk_mem::Allocation,
    ui_index_count: u32,

    // Dropped explicitly (before device destruction) in `Drop::drop` — see there for why.
    allocator: ManuallyDrop<Allocator>,

    // Current scene geometry, replaced wholesale by `set_mesh`. Null/0 until
    // the first `set_mesh` call — `record_frame` skips the draw call then.
    mesh_vertex_buffer: vk::Buffer,
    mesh_vertex_allocation: vk_mem::Allocation,
    mesh_index_buffer: vk::Buffer,
    mesh_index_allocation: vk_mem::Allocation,
    mesh_index_count: u32,

    swapchain_dirty: bool,
}

impl VkRenderer {
    pub fn new(window: Arc<Window>) -> Result<Self> {
        unsafe {
            let entry = ash::Entry::load().context("failed to load Vulkan loader")?;

            // --- Instance ---
            let app_info = vk::ApplicationInfo::default()
                .application_name(c"voxel-engine")
                .application_version(vk::make_api_version(0, 0, 1, 0))
                .engine_name(c"voxel-engine")
                .api_version(vk::API_VERSION_1_3);

            let display_handle = window.display_handle()?.as_raw();
            let window_handle = window.window_handle()?.as_raw();
            let required_exts = ash_window::enumerate_required_extensions(display_handle)?;

            // Validation layer in debug builds, if installed.
            let mut layers: Vec<*const std::ffi::c_char> = Vec::new();
            if cfg!(debug_assertions) {
                let available = entry.enumerate_instance_layer_properties()?;
                let have_validation = available.iter().any(|l| {
                    CStr::from_ptr(l.layer_name.as_ptr()) == VALIDATION_LAYER
                });
                if have_validation {
                    layers.push(VALIDATION_LAYER.as_ptr());
                }
            }

            let instance_info = vk::InstanceCreateInfo::default()
                .application_info(&app_info)
                .enabled_extension_names(required_exts)
                .enabled_layer_names(&layers);
            let instance = entry
                .create_instance(&instance_info, None)
                .context("vkCreateInstance failed")?;

            // --- Surface ---
            let surface_loader = ash::khr::surface::Instance::new(&entry, &instance);
            let surface = ash_window::create_surface(
                &entry,
                &instance,
                display_handle,
                window_handle,
                None,
            )?;

            // --- Physical device: needs 1.3 + a graphics queue that can present ---
            let (phys_device, queue_family) =
                pick_physical_device(&instance, &surface_loader, surface)?;
            let props = instance.get_physical_device_properties(phys_device);
            let name = CStr::from_ptr(props.device_name.as_ptr());
            println!("render-vk: using {:?} (Vulkan {}.{}.{})",
                name,
                vk::api_version_major(props.api_version),
                vk::api_version_minor(props.api_version),
                vk::api_version_patch(props.api_version));

            let depth_format = pick_depth_format(&instance, phys_device)
                .context("no supported depth-attachment format")?;

            // --- Logical device ---
            // dynamic_rendering + synchronization2 are mandatory in 1.3; enable, don't query.
            let mut features13 = vk::PhysicalDeviceVulkan13Features::default()
                .dynamic_rendering(true)
                .synchronization2(true);
            let priorities = [1.0f32];
            let queue_infos = [vk::DeviceQueueCreateInfo::default()
                .queue_family_index(queue_family)
                .queue_priorities(&priorities)];
            let device_exts = [ash::khr::swapchain::NAME.as_ptr()];
            let device_info = vk::DeviceCreateInfo::default()
                .queue_create_infos(&queue_infos)
                .enabled_extension_names(&device_exts)
                .push_next(&mut features13);
            let device = instance
                .create_device(phys_device, &device_info, None)
                .context("vkCreateDevice failed")?;
            let queue = device.get_device_queue(queue_family, 0);

            // --- Allocator ---
            let mut allocator_info = AllocatorCreateInfo::new(&instance, &device, phys_device);
            allocator_info.vulkan_api_version = vk::API_VERSION_1_3;
            let allocator = Allocator::new(allocator_info).context("vk-mem Allocator::new failed")?;

            // --- Command pool + per-frame objects ---
            let command_pool = device.create_command_pool(
                &vk::CommandPoolCreateInfo::default()
                    .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER)
                    .queue_family_index(queue_family),
                None,
            )?;
            let cmds = device.allocate_command_buffers(
                &vk::CommandBufferAllocateInfo::default()
                    .command_pool(command_pool)
                    .level(vk::CommandBufferLevel::PRIMARY)
                    .command_buffer_count(FRAMES_IN_FLIGHT as u32),
            )?;
            let mut frames = Vec::with_capacity(FRAMES_IN_FLIGHT);
            for cmd in cmds {
                frames.push(FrameSync {
                    image_available: device
                        .create_semaphore(&vk::SemaphoreCreateInfo::default(), None)?,
                    // Signaled so the first wait_for_fences doesn't deadlock.
                    in_flight: device.create_fence(
                        &vk::FenceCreateInfo::default().flags(vk::FenceCreateFlags::SIGNALED),
                        None,
                    )?,
                    cmd,
                });
            }

            let swapchain_loader = ash::khr::swapchain::Device::new(&instance, &device);

            let mut renderer = Self {
                window,
                _entry: entry,
                instance,
                surface_loader,
                surface,
                phys_device,
                device,
                queue,
                depth_format,
                allocator: ManuallyDrop::new(allocator),
                swapchain_loader,
                swapchain: vk::SwapchainKHR::null(),
                swapchain_format: vk::SurfaceFormatKHR::default(),
                swapchain_extent: vk::Extent2D::default(),
                swapchain_images: Vec::new(),
                swapchain_views: Vec::new(),
                render_finished: Vec::new(),
                depth_image: vk::Image::null(),
                depth_allocation: std::mem::zeroed(),
                depth_view: vk::ImageView::null(),
                command_pool,
                frames,
                frame_index: 0,
                atlas_image: vk::Image::null(),
                atlas_allocation: std::mem::zeroed(),
                atlas_view: vk::ImageView::null(),
                atlas_sampler: vk::Sampler::null(),
                descriptor_set_layout: vk::DescriptorSetLayout::null(),
                descriptor_pool: vk::DescriptorPool::null(),
                uniform_buffers: Vec::new(),
                uniform_allocations: Vec::new(),
                descriptor_sets: Vec::new(),
                pipeline_layout: vk::PipelineLayout::null(),
                pipeline: vk::Pipeline::null(),
                ui_pipeline_layout: vk::PipelineLayout::null(),
                ui_pipeline: vk::Pipeline::null(),
                ui_vertex_buffer: vk::Buffer::null(),
                ui_vertex_allocation: std::mem::zeroed(),
                ui_index_buffer: vk::Buffer::null(),
                ui_index_allocation: std::mem::zeroed(),
                ui_index_count: 0,
                mesh_vertex_buffer: vk::Buffer::null(),
                mesh_vertex_allocation: std::mem::zeroed(),
                mesh_index_buffer: vk::Buffer::null(),
                mesh_index_allocation: std::mem::zeroed(),
                mesh_index_count: 0,
                swapchain_dirty: false,
            };
            renderer.create_atlas()?;
            renderer.create_globals()?;
            renderer.create_swapchain()?;
            renderer.create_pipeline()?;
            renderer.create_ui_pipeline()?;
            Ok(renderer)
        }
    }

    /// Uploads the procedurally generated placeholder atlas (see module
    /// docs) to a device-local image via a staging buffer + one-shot command
    /// buffer. Runs once at init, before `create_globals` (which binds the
    /// resulting view/sampler into every frame's descriptor set) — no
    /// swapchain dependency, so unlike the depth buffer this never recreates.
    fn create_atlas(&mut self) -> Result<()> {
        unsafe {
            let pixels = generate_atlas_pixels();
            let (width, height) = (ATLAS_TILE_COUNT * ATLAS_TILE_SIZE, ATLAS_TILE_SIZE);
            let (staging_buffer, mut staging_alloc) =
                self.create_mapped_buffer_with_data(&pixels, vk::BufferUsageFlags::TRANSFER_SRC)?;

            let image_info = vk::ImageCreateInfo::default()
                .image_type(vk::ImageType::TYPE_2D)
                .format(ATLAS_FORMAT)
                .extent(vk::Extent3D { width, height, depth: 1 })
                .mip_levels(1)
                .array_layers(1)
                .samples(vk::SampleCountFlags::TYPE_1)
                .tiling(vk::ImageTiling::OPTIMAL)
                .usage(vk::ImageUsageFlags::TRANSFER_DST | vk::ImageUsageFlags::SAMPLED)
                .sharing_mode(vk::SharingMode::EXCLUSIVE)
                .initial_layout(vk::ImageLayout::UNDEFINED);
            let alloc_info =
                AllocationCreateInfo { usage: MemoryUsage::AutoPreferDevice, ..Default::default() };
            let (image, allocation) = self
                .allocator
                .create_image(&image_info, &alloc_info)
                .context("failed to create atlas image")?;

            let cmd = self.device.allocate_command_buffers(
                &vk::CommandBufferAllocateInfo::default()
                    .command_pool(self.command_pool)
                    .level(vk::CommandBufferLevel::PRIMARY)
                    .command_buffer_count(1),
            )?[0];
            self.device.begin_command_buffer(
                cmd,
                &vk::CommandBufferBeginInfo::default()
                    .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
            )?;
            image_barrier(
                &self.device,
                cmd,
                image,
                vk::ImageAspectFlags::COLOR,
                vk::PipelineStageFlags2::TOP_OF_PIPE,
                vk::AccessFlags2::empty(),
                vk::PipelineStageFlags2::TRANSFER,
                vk::AccessFlags2::TRANSFER_WRITE,
                vk::ImageLayout::UNDEFINED,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
            );
            let region = vk::BufferImageCopy::default()
                .image_subresource(vk::ImageSubresourceLayers {
                    aspect_mask: vk::ImageAspectFlags::COLOR,
                    mip_level: 0,
                    base_array_layer: 0,
                    layer_count: 1,
                })
                .image_extent(vk::Extent3D { width, height, depth: 1 });
            self.device.cmd_copy_buffer_to_image(
                cmd,
                staging_buffer,
                image,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                &[region],
            );
            image_barrier(
                &self.device,
                cmd,
                image,
                vk::ImageAspectFlags::COLOR,
                vk::PipelineStageFlags2::TRANSFER,
                vk::AccessFlags2::TRANSFER_WRITE,
                vk::PipelineStageFlags2::FRAGMENT_SHADER,
                vk::AccessFlags2::SHADER_READ,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
            );
            self.device.end_command_buffer(cmd)?;
            let cmd_info = vk::CommandBufferSubmitInfo::default().command_buffer(cmd);
            let submit = vk::SubmitInfo2::default()
                .command_buffer_infos(std::slice::from_ref(&cmd_info));
            self.device
                .queue_submit2(self.queue, std::slice::from_ref(&submit), vk::Fence::null())?;
            // One-time init cost: simplicity (no fence bookkeeping) over the
            // perf that would matter if this ran every frame, which it doesn't.
            self.device.queue_wait_idle(self.queue)?;
            self.device.free_command_buffers(self.command_pool, &[cmd]);
            self.allocator.destroy_buffer(staging_buffer, &mut staging_alloc);

            let view = self.device.create_image_view(
                &vk::ImageViewCreateInfo::default()
                    .image(image)
                    .view_type(vk::ImageViewType::TYPE_2D)
                    .format(ATLAS_FORMAT)
                    .subresource_range(aspect_subresource_range(vk::ImageAspectFlags::COLOR)),
                None,
            )?;
            let sampler = self.device.create_sampler(
                &vk::SamplerCreateInfo::default()
                    .mag_filter(vk::Filter::NEAREST)
                    .min_filter(vk::Filter::NEAREST)
                    .mipmap_mode(vk::SamplerMipmapMode::NEAREST)
                    .address_mode_u(vk::SamplerAddressMode::REPEAT)
                    .address_mode_v(vk::SamplerAddressMode::REPEAT)
                    .address_mode_w(vk::SamplerAddressMode::REPEAT),
                None,
            )?;

            self.atlas_image = image;
            self.atlas_allocation = allocation;
            self.atlas_view = view;
            self.atlas_sampler = sampler;
            Ok(())
        }
    }

    /// One-time setup independent of the swapchain: the per-frame uniform
    /// buffers holding the camera matrix + atlas tile count, the atlas
    /// texture/sampler bindings, and the descriptor plumbing to bind them
    /// all. Must run after `create_atlas` (needs its view/sampler) and
    /// before `create_pipeline` (the pipeline layout references
    /// `descriptor_set_layout`).
    fn create_globals(&mut self) -> Result<()> {
        unsafe {
            let ubo_binding = vk::DescriptorSetLayoutBinding::default()
                .binding(0)
                .descriptor_type(vk::DescriptorType::UNIFORM_BUFFER)
                .descriptor_count(1)
                .stage_flags(vk::ShaderStageFlags::VERTEX | vk::ShaderStageFlags::FRAGMENT);
            let image_binding = vk::DescriptorSetLayoutBinding::default()
                .binding(1)
                .descriptor_type(vk::DescriptorType::SAMPLED_IMAGE)
                .descriptor_count(1)
                .stage_flags(vk::ShaderStageFlags::FRAGMENT);
            let sampler_binding = vk::DescriptorSetLayoutBinding::default()
                .binding(2)
                .descriptor_type(vk::DescriptorType::SAMPLER)
                .descriptor_count(1)
                .stage_flags(vk::ShaderStageFlags::FRAGMENT);
            let bindings = [ubo_binding, image_binding, sampler_binding];
            self.descriptor_set_layout = self.device.create_descriptor_set_layout(
                &vk::DescriptorSetLayoutCreateInfo::default().bindings(&bindings),
                None,
            )?;

            let pool_sizes = [
                vk::DescriptorPoolSize::default()
                    .ty(vk::DescriptorType::UNIFORM_BUFFER)
                    .descriptor_count(FRAMES_IN_FLIGHT as u32),
                vk::DescriptorPoolSize::default()
                    .ty(vk::DescriptorType::SAMPLED_IMAGE)
                    .descriptor_count(FRAMES_IN_FLIGHT as u32),
                vk::DescriptorPoolSize::default()
                    .ty(vk::DescriptorType::SAMPLER)
                    .descriptor_count(FRAMES_IN_FLIGHT as u32),
            ];
            self.descriptor_pool = self.device.create_descriptor_pool(
                &vk::DescriptorPoolCreateInfo::default()
                    .pool_sizes(&pool_sizes)
                    .max_sets(FRAMES_IN_FLIGHT as u32),
                None,
            )?;

            let layouts = vec![self.descriptor_set_layout; FRAMES_IN_FLIGHT];
            self.descriptor_sets = self.device.allocate_descriptor_sets(
                &vk::DescriptorSetAllocateInfo::default()
                    .descriptor_pool(self.descriptor_pool)
                    .set_layouts(&layouts),
            )?;

            let ubo_size = std::mem::size_of::<GlobalsUbo>() as vk::DeviceSize;
            let image_info = vk::DescriptorImageInfo::default()
                .image_view(self.atlas_view)
                .image_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL);
            let sampler_info = vk::DescriptorImageInfo::default().sampler(self.atlas_sampler);
            for i in 0..FRAMES_IN_FLIGHT {
                let (buffer, allocation) = self.create_mapped_buffer_sized(
                    ubo_size,
                    vk::BufferUsageFlags::UNIFORM_BUFFER,
                )?;
                let buffer_info = vk::DescriptorBufferInfo::default()
                    .buffer(buffer)
                    .offset(0)
                    .range(ubo_size);
                let writes = [
                    vk::WriteDescriptorSet::default()
                        .dst_set(self.descriptor_sets[i])
                        .dst_binding(0)
                        .descriptor_type(vk::DescriptorType::UNIFORM_BUFFER)
                        .buffer_info(std::slice::from_ref(&buffer_info)),
                    vk::WriteDescriptorSet::default()
                        .dst_set(self.descriptor_sets[i])
                        .dst_binding(1)
                        .descriptor_type(vk::DescriptorType::SAMPLED_IMAGE)
                        .image_info(std::slice::from_ref(&image_info)),
                    vk::WriteDescriptorSet::default()
                        .dst_set(self.descriptor_sets[i])
                        .dst_binding(2)
                        .descriptor_type(vk::DescriptorType::SAMPLER)
                        .image_info(std::slice::from_ref(&sampler_info)),
                ];
                self.device.update_descriptor_sets(&writes, &[]);
                self.uniform_buffers.push(buffer);
                self.uniform_allocations.push(allocation);
            }
            Ok(())
        }
    }

    /// Allocates a host-visible, persistently-mapped `size`-byte buffer with
    /// `usage`. Shared helper behind the uniform buffers and `set_mesh`'s
    /// vertex/index buffers — all small, CPU-written, GPU-read data on this
    /// checkpoint's scale, so one mapped-memory path covers all of them.
    unsafe fn create_mapped_buffer_sized(
        &self,
        size: vk::DeviceSize,
        usage: vk::BufferUsageFlags,
    ) -> Result<(vk::Buffer, vk_mem::Allocation)> {
        let buffer_info = vk::BufferCreateInfo::default()
            .size(size)
            .usage(usage)
            .sharing_mode(vk::SharingMode::EXCLUSIVE);
        let alloc_info = AllocationCreateInfo {
            usage: MemoryUsage::Auto,
            flags: AllocationCreateFlags::HOST_ACCESS_SEQUENTIAL_WRITE
                | AllocationCreateFlags::MAPPED,
            ..Default::default()
        };
        self.allocator
            .create_buffer(&buffer_info, &alloc_info)
            .context("failed to create mapped buffer")
    }

    /// Like `create_mapped_buffer_sized`, but also uploads `data` and flushes
    /// (a no-op if the memory VMA picked is already host-coherent; required
    /// if it isn't — `MemoryUsage::Auto` doesn't guarantee coherency).
    unsafe fn create_mapped_buffer_with_data<T: Copy>(
        &self,
        data: &[T],
        usage: vk::BufferUsageFlags,
    ) -> Result<(vk::Buffer, vk_mem::Allocation)> {
        let size = std::mem::size_of_val(data) as vk::DeviceSize;
        let (buffer, allocation) = self.create_mapped_buffer_sized(size, usage)?;
        let mapped = self.allocator.get_allocation_info(&allocation).mapped_data;
        debug_assert!(!mapped.is_null(), "AllocationCreateFlags::MAPPED should guarantee this");
        std::ptr::copy_nonoverlapping(data.as_ptr(), mapped as *mut T, data.len());
        self.allocator
            .flush_allocation(&allocation, 0, vk::WHOLE_SIZE)
            .context("failed to flush buffer allocation")?;
        Ok((buffer, allocation))
    }

    fn destroy_ui_buffers(&mut self) {
        unsafe {
            if self.ui_vertex_buffer != vk::Buffer::null() {
                self.allocator
                    .destroy_buffer(self.ui_vertex_buffer, &mut self.ui_vertex_allocation);
                self.ui_vertex_buffer = vk::Buffer::null();
            }
            if self.ui_index_buffer != vk::Buffer::null() {
                self.allocator
                    .destroy_buffer(self.ui_index_buffer, &mut self.ui_index_allocation);
                self.ui_index_buffer = vk::Buffer::null();
            }
        }
    }

    fn destroy_mesh_buffers(&mut self) {
        unsafe {
            if self.mesh_vertex_buffer != vk::Buffer::null() {
                self.allocator
                    .destroy_buffer(self.mesh_vertex_buffer, &mut self.mesh_vertex_allocation);
                self.mesh_vertex_buffer = vk::Buffer::null();
            }
            if self.mesh_index_buffer != vk::Buffer::null() {
                self.allocator
                    .destroy_buffer(self.mesh_index_buffer, &mut self.mesh_index_allocation);
                self.mesh_index_buffer = vk::Buffer::null();
            }
        }
    }

    /// Builds the (currently single, static) graphics pipeline. Depends on
    /// `swapchain_format`/`depth_format` and `descriptor_set_layout`, so must
    /// run after the first `create_swapchain` and after `create_globals`.
    /// Not reused across swapchain recreation: attachment formats are fixed
    /// at surface-creation time and don't change on resize, so the pipeline
    /// is built once and never torn down until the renderer itself is.
    fn create_pipeline(&mut self) -> Result<()> {
        unsafe {
            let spirv = shader::compile_wgsl_to_spirv(include_str!("../shaders/mesh.wgsl"))?;
            let module = self.device.create_shader_module(
                &vk::ShaderModuleCreateInfo::default().code(&spirv),
                None,
            )?;

            let vert_stage = vk::PipelineShaderStageCreateInfo::default()
                .stage(vk::ShaderStageFlags::VERTEX)
                .module(module)
                .name(c"vs_main");
            let frag_stage = vk::PipelineShaderStageCreateInfo::default()
                .stage(vk::ShaderStageFlags::FRAGMENT)
                .module(module)
                .name(c"fs_main");
            let stages = [vert_stage, frag_stage];

            let binding_desc = vk::VertexInputBindingDescription::default()
                .binding(0)
                .stride(std::mem::size_of::<MeshVertex>() as u32)
                .input_rate(vk::VertexInputRate::VERTEX);
            let attribute_descs = [
                vk::VertexInputAttributeDescription::default()
                    .location(0)
                    .binding(0)
                    .format(vk::Format::R32G32B32_SFLOAT)
                    .offset(std::mem::offset_of!(MeshVertex, position) as u32),
                vk::VertexInputAttributeDescription::default()
                    .location(1)
                    .binding(0)
                    .format(vk::Format::R32G32B32_SFLOAT)
                    .offset(std::mem::offset_of!(MeshVertex, normal) as u32),
                vk::VertexInputAttributeDescription::default()
                    .location(2)
                    .binding(0)
                    .format(vk::Format::R32G32_SFLOAT)
                    .offset(std::mem::offset_of!(MeshVertex, uv) as u32),
                vk::VertexInputAttributeDescription::default()
                    .location(3)
                    .binding(0)
                    .format(vk::Format::R32_SFLOAT)
                    .offset(std::mem::offset_of!(MeshVertex, tile) as u32),
                vk::VertexInputAttributeDescription::default()
                    .location(4)
                    .binding(0)
                    .format(vk::Format::R32_SFLOAT)
                    .offset(std::mem::offset_of!(MeshVertex, shade) as u32),
            ];
            let vertex_input = vk::PipelineVertexInputStateCreateInfo::default()
                .vertex_binding_descriptions(std::slice::from_ref(&binding_desc))
                .vertex_attribute_descriptions(&attribute_descs);
            let input_assembly = vk::PipelineInputAssemblyStateCreateInfo::default()
                .topology(vk::PrimitiveTopology::TRIANGLE_LIST);

            // Viewport/scissor are dynamic so the pipeline survives swapchain resize.
            let dynamic_states = [vk::DynamicState::VIEWPORT, vk::DynamicState::SCISSOR];
            let dynamic_state =
                vk::PipelineDynamicStateCreateInfo::default().dynamic_states(&dynamic_states);
            let viewport_state = vk::PipelineViewportStateCreateInfo::default()
                .viewport_count(1)
                .scissor_count(1);

            // Culling TEMPORARILY OFF. Context: `engine_core::camera`'s
            // clip-space Y-flip was just removed to fix a confirmed (by
            // screenshot) upside-down render — but which `FrontFace` value
            // pairs correctly with "no flip" wasn't independently derivable
            // with confidence (see camera.rs's doc comment; the theoretical
            // model that predicted the *previous* setting was wrong once
            // already this session). Rather than guess again and risk a
            // second confusing "still broken, but now for a different
            // reason" report, cull mode is NONE here so the orientation fix
            // can be checked in isolation — a fully unculled scene should
            // just look right-side up (if inefficient), with no separate
            // culling-direction variable to confuse that check. Once
            // orientation is confirmed correct, re-enable `CullModeFlags::BACK`
            // and pick whichever `FrontFace` doesn't make geometry vanish —
            // that follow-up no longer risks another blind full round-trip
            // since the harder (orientation) variable will already be pinned
            // down.
            let rasterization = vk::PipelineRasterizationStateCreateInfo::default()
                .polygon_mode(vk::PolygonMode::FILL)
                .cull_mode(vk::CullModeFlags::NONE)
                .front_face(vk::FrontFace::CLOCKWISE)
                .line_width(1.0);
            let multisample = vk::PipelineMultisampleStateCreateInfo::default()
                .rasterization_samples(vk::SampleCountFlags::TYPE_1);

            let depth_stencil = vk::PipelineDepthStencilStateCreateInfo::default()
                .depth_test_enable(true)
                .depth_write_enable(true)
                .depth_compare_op(vk::CompareOp::LESS)
                .min_depth_bounds(0.0)
                .max_depth_bounds(1.0);

            let blend_attachment = vk::PipelineColorBlendAttachmentState::default()
                .color_write_mask(vk::ColorComponentFlags::RGBA)
                .blend_enable(false);
            let color_blend = vk::PipelineColorBlendStateCreateInfo::default()
                .attachments(std::slice::from_ref(&blend_attachment));

            let layout = self.device.create_pipeline_layout(
                &vk::PipelineLayoutCreateInfo::default()
                    .set_layouts(std::slice::from_ref(&self.descriptor_set_layout)),
                None,
            )?;

            let color_formats = [self.swapchain_format.format];
            let mut rendering_info = vk::PipelineRenderingCreateInfo::default()
                .color_attachment_formats(&color_formats)
                .depth_attachment_format(self.depth_format);

            let pipeline_info = vk::GraphicsPipelineCreateInfo::default()
                .stages(&stages)
                .vertex_input_state(&vertex_input)
                .input_assembly_state(&input_assembly)
                .viewport_state(&viewport_state)
                .rasterization_state(&rasterization)
                .multisample_state(&multisample)
                .depth_stencil_state(&depth_stencil)
                .color_blend_state(&color_blend)
                .dynamic_state(&dynamic_state)
                .layout(layout)
                .push_next(&mut rendering_info);

            let pipeline = self
                .device
                .create_graphics_pipelines(
                    vk::PipelineCache::null(),
                    std::slice::from_ref(&pipeline_info),
                    None,
                )
                .map_err(|(_, e)| e)
                .context("vkCreateGraphicsPipelines failed")?[0];

            // Shader modules aren't needed after pipeline creation.
            self.device.destroy_shader_module(module, None);

            self.pipeline_layout = layout;
            self.pipeline = pipeline;
            Ok(())
        }
    }

    /// Builds the screen-space UI overlay pipeline: no depth test (always on
    /// top), no culling (arbitrary 2D quads, not solid geometry), alpha
    /// blended, empty pipeline/descriptor layout (NDC-direct vertices need
    /// no camera transform or texture). Same lifetime as `create_pipeline` —
    /// built once, not tied to swapchain recreation.
    fn create_ui_pipeline(&mut self) -> Result<()> {
        unsafe {
            let spirv = shader::compile_wgsl_to_spirv(include_str!("../shaders/ui.wgsl"))?;
            let module = self.device.create_shader_module(
                &vk::ShaderModuleCreateInfo::default().code(&spirv),
                None,
            )?;

            let vert_stage = vk::PipelineShaderStageCreateInfo::default()
                .stage(vk::ShaderStageFlags::VERTEX)
                .module(module)
                .name(c"vs_main");
            let frag_stage = vk::PipelineShaderStageCreateInfo::default()
                .stage(vk::ShaderStageFlags::FRAGMENT)
                .module(module)
                .name(c"fs_main");
            let stages = [vert_stage, frag_stage];

            let binding_desc = vk::VertexInputBindingDescription::default()
                .binding(0)
                .stride(std::mem::size_of::<UiVertex>() as u32)
                .input_rate(vk::VertexInputRate::VERTEX);
            let attribute_descs = [
                vk::VertexInputAttributeDescription::default()
                    .location(0)
                    .binding(0)
                    .format(vk::Format::R32G32_SFLOAT)
                    .offset(std::mem::offset_of!(UiVertex, position) as u32),
                vk::VertexInputAttributeDescription::default()
                    .location(1)
                    .binding(0)
                    .format(vk::Format::R32G32B32A32_SFLOAT)
                    .offset(std::mem::offset_of!(UiVertex, color) as u32),
            ];
            let vertex_input = vk::PipelineVertexInputStateCreateInfo::default()
                .vertex_binding_descriptions(std::slice::from_ref(&binding_desc))
                .vertex_attribute_descriptions(&attribute_descs);
            let input_assembly = vk::PipelineInputAssemblyStateCreateInfo::default()
                .topology(vk::PrimitiveTopology::TRIANGLE_LIST);

            let dynamic_states = [vk::DynamicState::VIEWPORT, vk::DynamicState::SCISSOR];
            let dynamic_state =
                vk::PipelineDynamicStateCreateInfo::default().dynamic_states(&dynamic_states);
            let viewport_state = vk::PipelineViewportStateCreateInfo::default()
                .viewport_count(1)
                .scissor_count(1);

            let rasterization = vk::PipelineRasterizationStateCreateInfo::default()
                .polygon_mode(vk::PolygonMode::FILL)
                .cull_mode(vk::CullModeFlags::NONE)
                .front_face(vk::FrontFace::CLOCKWISE)
                .line_width(1.0);
            let multisample = vk::PipelineMultisampleStateCreateInfo::default()
                .rasterization_samples(vk::SampleCountFlags::TYPE_1);

            let depth_stencil = vk::PipelineDepthStencilStateCreateInfo::default()
                .depth_test_enable(false)
                .depth_write_enable(false)
                .depth_compare_op(vk::CompareOp::ALWAYS);

            let blend_attachment = vk::PipelineColorBlendAttachmentState::default()
                .color_write_mask(vk::ColorComponentFlags::RGBA)
                .blend_enable(true)
                .src_color_blend_factor(vk::BlendFactor::SRC_ALPHA)
                .dst_color_blend_factor(vk::BlendFactor::ONE_MINUS_SRC_ALPHA)
                .color_blend_op(vk::BlendOp::ADD)
                .src_alpha_blend_factor(vk::BlendFactor::ONE)
                .dst_alpha_blend_factor(vk::BlendFactor::ONE_MINUS_SRC_ALPHA)
                .alpha_blend_op(vk::BlendOp::ADD);
            let color_blend = vk::PipelineColorBlendStateCreateInfo::default()
                .attachments(std::slice::from_ref(&blend_attachment));

            // No descriptor sets: UI vertices carry their own NDC position
            // and color directly, no camera/atlas sampling needed.
            let layout = self
                .device
                .create_pipeline_layout(&vk::PipelineLayoutCreateInfo::default(), None)?;

            let color_formats = [self.swapchain_format.format];
            let mut rendering_info = vk::PipelineRenderingCreateInfo::default()
                .color_attachment_formats(&color_formats)
                .depth_attachment_format(self.depth_format);

            let pipeline_info = vk::GraphicsPipelineCreateInfo::default()
                .stages(&stages)
                .vertex_input_state(&vertex_input)
                .input_assembly_state(&input_assembly)
                .viewport_state(&viewport_state)
                .rasterization_state(&rasterization)
                .multisample_state(&multisample)
                .depth_stencil_state(&depth_stencil)
                .color_blend_state(&color_blend)
                .dynamic_state(&dynamic_state)
                .layout(layout)
                .push_next(&mut rendering_info);

            let pipeline = self
                .device
                .create_graphics_pipelines(
                    vk::PipelineCache::null(),
                    std::slice::from_ref(&pipeline_info),
                    None,
                )
                .map_err(|(_, e)| e)
                .context("vkCreateGraphicsPipelines (ui) failed")?[0];

            self.device.destroy_shader_module(module, None);

            self.ui_pipeline_layout = layout;
            self.ui_pipeline = pipeline;
            Ok(())
        }
    }

    /// (Re)creates the swapchain and everything derived from it (image
    /// views, per-image semaphores, depth buffer). Caller must ensure the
    /// device is idle if replacing an in-use swapchain.
    fn create_swapchain(&mut self) -> Result<()> {
        unsafe {
            let caps = self
                .surface_loader
                .get_physical_device_surface_capabilities(self.phys_device, self.surface)?;

            let extent = if caps.current_extent.width != u32::MAX {
                caps.current_extent
            } else {
                let size = self.window.inner_size();
                vk::Extent2D {
                    width: size.width.clamp(
                        caps.min_image_extent.width,
                        caps.max_image_extent.width,
                    ),
                    height: size.height.clamp(
                        caps.min_image_extent.height,
                        caps.max_image_extent.height,
                    ),
                }
            };
            // Minimized window: keep the old (possibly null) swapchain, stay dirty.
            if extent.width == 0 || extent.height == 0 {
                self.swapchain_extent = extent;
                return Ok(());
            }

            let formats = self
                .surface_loader
                .get_physical_device_surface_formats(self.phys_device, self.surface)?;
            let format = formats
                .iter()
                .copied()
                .find(|f| {
                    f.format == vk::Format::B8G8R8A8_SRGB
                        && f.color_space == vk::ColorSpaceKHR::SRGB_NONLINEAR
                })
                .unwrap_or(formats[0]);

            let mut image_count = caps.min_image_count + 1;
            if caps.max_image_count > 0 {
                image_count = image_count.min(caps.max_image_count);
            }

            let old_swapchain = self.swapchain;
            let info = vk::SwapchainCreateInfoKHR::default()
                .surface(self.surface)
                .min_image_count(image_count)
                .image_format(format.format)
                .image_color_space(format.color_space)
                .image_extent(extent)
                .image_array_layers(1)
                .image_usage(vk::ImageUsageFlags::COLOR_ATTACHMENT)
                .image_sharing_mode(vk::SharingMode::EXCLUSIVE)
                .pre_transform(caps.current_transform)
                .composite_alpha(vk::CompositeAlphaFlagsKHR::OPAQUE)
                // FIFO: guaranteed available, vsynced. Present-mode selection is a later concern.
                .present_mode(vk::PresentModeKHR::FIFO)
                .clipped(true)
                .old_swapchain(old_swapchain);
            let swapchain = self.swapchain_loader.create_swapchain(&info, None)?;

            // Tear down objects tied to the old swapchain.
            self.destroy_swapchain_resources();
            if old_swapchain != vk::SwapchainKHR::null() {
                self.swapchain_loader.destroy_swapchain(old_swapchain, None);
            }

            self.swapchain = swapchain;
            self.swapchain_format = format;
            self.swapchain_extent = extent;
            self.swapchain_images = self.swapchain_loader.get_swapchain_images(swapchain)?;

            for &image in &self.swapchain_images {
                let view = self.device.create_image_view(
                    &vk::ImageViewCreateInfo::default()
                        .image(image)
                        .view_type(vk::ImageViewType::TYPE_2D)
                        .format(format.format)
                        .subresource_range(aspect_subresource_range(vk::ImageAspectFlags::COLOR)),
                    None,
                )?;
                self.swapchain_views.push(view);
                self.render_finished.push(
                    self.device
                        .create_semaphore(&vk::SemaphoreCreateInfo::default(), None)?,
                );
            }

            self.create_depth_resources(extent)?;

            self.swapchain_dirty = false;
            Ok(())
        }
    }

    fn create_depth_resources(&mut self, extent: vk::Extent2D) -> Result<()> {
        unsafe {
            let image_info = vk::ImageCreateInfo::default()
                .image_type(vk::ImageType::TYPE_2D)
                .format(self.depth_format)
                .extent(vk::Extent3D { width: extent.width, height: extent.height, depth: 1 })
                .mip_levels(1)
                .array_layers(1)
                .samples(vk::SampleCountFlags::TYPE_1)
                .tiling(vk::ImageTiling::OPTIMAL)
                .usage(vk::ImageUsageFlags::DEPTH_STENCIL_ATTACHMENT)
                .sharing_mode(vk::SharingMode::EXCLUSIVE)
                .initial_layout(vk::ImageLayout::UNDEFINED);
            // Device-local: never CPU-accessed, so no host-access flags.
            let alloc_info =
                AllocationCreateInfo { usage: MemoryUsage::AutoPreferDevice, ..Default::default() };
            let (image, allocation) = self
                .allocator
                .create_image(&image_info, &alloc_info)
                .context("failed to create depth image")?;
            let view = self.device.create_image_view(
                &vk::ImageViewCreateInfo::default()
                    .image(image)
                    .view_type(vk::ImageViewType::TYPE_2D)
                    .format(self.depth_format)
                    .subresource_range(aspect_subresource_range(vk::ImageAspectFlags::DEPTH)),
                None,
            )?;
            self.depth_image = image;
            self.depth_allocation = allocation;
            self.depth_view = view;
            Ok(())
        }
    }

    /// Destroys image views, per-image semaphores, and the depth buffer (not
    /// the swapchain handle itself).
    fn destroy_swapchain_resources(&mut self) {
        unsafe {
            for view in self.swapchain_views.drain(..) {
                self.device.destroy_image_view(view, None);
            }
            for sem in self.render_finished.drain(..) {
                self.device.destroy_semaphore(sem, None);
            }
            if self.depth_view != vk::ImageView::null() {
                self.device.destroy_image_view(self.depth_view, None);
                self.depth_view = vk::ImageView::null();
            }
            if self.depth_image != vk::Image::null() {
                self.allocator.destroy_image(self.depth_image, &mut self.depth_allocation);
                self.depth_image = vk::Image::null();
            }
        }
    }

    fn recreate_swapchain(&mut self) -> Result<()> {
        unsafe { self.device.device_wait_idle()? };
        self.create_swapchain()
    }

    fn record_frame(&self, cmd: vk::CommandBuffer, image_index: usize, camera: &Camera) -> Result<()> {
        unsafe {
            let image = self.swapchain_images[image_index];
            let view = self.swapchain_views[image_index];

            self.device
                .begin_command_buffer(cmd, &vk::CommandBufferBeginInfo::default())?;

            // UNDEFINED -> COLOR_ATTACHMENT_OPTIMAL
            image_barrier(
                &self.device,
                cmd,
                image,
                vk::ImageAspectFlags::COLOR,
                vk::PipelineStageFlags2::TOP_OF_PIPE,
                vk::AccessFlags2::empty(),
                vk::PipelineStageFlags2::COLOR_ATTACHMENT_OUTPUT,
                vk::AccessFlags2::COLOR_ATTACHMENT_WRITE,
                vk::ImageLayout::UNDEFINED,
                vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
            );
            // UNDEFINED -> DEPTH_ATTACHMENT_OPTIMAL every frame: LOAD_OP_CLEAR
            // below means prior contents never need to be preserved, so there's
            // no reason to track "already transitioned" state across frames.
            image_barrier(
                &self.device,
                cmd,
                self.depth_image,
                vk::ImageAspectFlags::DEPTH,
                vk::PipelineStageFlags2::TOP_OF_PIPE,
                vk::AccessFlags2::empty(),
                vk::PipelineStageFlags2::EARLY_FRAGMENT_TESTS | vk::PipelineStageFlags2::LATE_FRAGMENT_TESTS,
                vk::AccessFlags2::DEPTH_STENCIL_ATTACHMENT_WRITE,
                vk::ImageLayout::UNDEFINED,
                vk::ImageLayout::DEPTH_ATTACHMENT_OPTIMAL,
            );

            // Camera matrix + atlas tile count for this frame slot.
            // flush_allocation is a no-op on coherent memory (the common
            // case); see create_mapped_buffer_with_data.
            let globals = GlobalsUbo {
                view_proj: camera.view_proj().to_cols_array(),
                atlas_tile_count: ATLAS_TILE_COUNT as f32,
                _pad: [0.0; 3],
            };
            let ubo_alloc = &self.uniform_allocations[self.frame_index];
            let mapped = self.allocator.get_allocation_info(ubo_alloc).mapped_data;
            debug_assert!(!mapped.is_null());
            std::ptr::copy_nonoverlapping(&globals, mapped as *mut GlobalsUbo, 1);
            self.allocator.flush_allocation(ubo_alloc, 0, vk::WHOLE_SIZE)?;

            let color_attachment = vk::RenderingAttachmentInfo::default()
                .image_view(view)
                .image_layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)
                .load_op(vk::AttachmentLoadOp::CLEAR)
                .store_op(vk::AttachmentStoreOp::STORE)
                .clear_value(vk::ClearValue {
                    color: vk::ClearColorValue {
                        float32: [0.45, 0.65, 0.85, 1.0], // sky, now that there's real geometry to see against it
                    },
                });
            let depth_attachment = vk::RenderingAttachmentInfo::default()
                .image_view(self.depth_view)
                .image_layout(vk::ImageLayout::DEPTH_ATTACHMENT_OPTIMAL)
                .load_op(vk::AttachmentLoadOp::CLEAR)
                .store_op(vk::AttachmentStoreOp::DONT_CARE)
                .clear_value(vk::ClearValue {
                    depth_stencil: vk::ClearDepthStencilValue { depth: 1.0, stencil: 0 },
                });
            let rendering_info = vk::RenderingInfo::default()
                .render_area(vk::Rect2D {
                    offset: vk::Offset2D::default(),
                    extent: self.swapchain_extent,
                })
                .layer_count(1)
                .color_attachments(std::slice::from_ref(&color_attachment))
                .depth_attachment(&depth_attachment);

            self.device.cmd_begin_rendering(cmd, &rendering_info);

            self.device
                .cmd_bind_pipeline(cmd, vk::PipelineBindPoint::GRAPHICS, self.pipeline);
            let viewport = vk::Viewport {
                x: 0.0,
                y: 0.0,
                width: self.swapchain_extent.width as f32,
                height: self.swapchain_extent.height as f32,
                min_depth: 0.0,
                max_depth: 1.0,
            };
            self.device.cmd_set_viewport(cmd, 0, &[viewport]);
            let scissor = vk::Rect2D {
                offset: vk::Offset2D::default(),
                extent: self.swapchain_extent,
            };
            self.device.cmd_set_scissor(cmd, 0, &[scissor]);
            self.device.cmd_bind_descriptor_sets(
                cmd,
                vk::PipelineBindPoint::GRAPHICS,
                self.pipeline_layout,
                0,
                &[self.descriptor_sets[self.frame_index]],
                &[],
            );

            if self.mesh_index_count > 0 {
                self.device
                    .cmd_bind_vertex_buffers(cmd, 0, &[self.mesh_vertex_buffer], &[0]);
                self.device.cmd_bind_index_buffer(
                    cmd,
                    self.mesh_index_buffer,
                    0,
                    vk::IndexType::UINT32,
                );
                self.device.cmd_draw_indexed(cmd, self.mesh_index_count, 1, 0, 0, 0);
            }

            if self.ui_index_count > 0 {
                self.device.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::GRAPHICS, self.ui_pipeline);
                self.device
                    .cmd_bind_vertex_buffers(cmd, 0, &[self.ui_vertex_buffer], &[0]);
                self.device.cmd_bind_index_buffer(
                    cmd,
                    self.ui_index_buffer,
                    0,
                    vk::IndexType::UINT32,
                );
                self.device.cmd_draw_indexed(cmd, self.ui_index_count, 1, 0, 0, 0);
            }

            self.device.cmd_end_rendering(cmd);

            // COLOR_ATTACHMENT_OPTIMAL -> PRESENT_SRC
            image_barrier(
                &self.device,
                cmd,
                image,
                vk::ImageAspectFlags::COLOR,
                vk::PipelineStageFlags2::COLOR_ATTACHMENT_OUTPUT,
                vk::AccessFlags2::COLOR_ATTACHMENT_WRITE,
                vk::PipelineStageFlags2::BOTTOM_OF_PIPE,
                vk::AccessFlags2::empty(),
                vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
                vk::ImageLayout::PRESENT_SRC_KHR,
            );

            self.device.end_command_buffer(cmd)?;
            Ok(())
        }
    }
}

impl Renderer for VkRenderer {
    fn render_frame(&mut self, camera: &Camera) -> Result<()> {
        unsafe {
            if self.swapchain_dirty {
                self.recreate_swapchain()?;
            }
            // Minimized / zero-sized surface: nothing to do.
            if self.swapchain_extent.width == 0 || self.swapchain_extent.height == 0 {
                return Ok(());
            }

            let frame = &self.frames[self.frame_index];
            let (image_available, in_flight, cmd) =
                (frame.image_available, frame.in_flight, frame.cmd);

            self.device
                .wait_for_fences(&[in_flight], true, u64::MAX)?;

            let image_index = match self.swapchain_loader.acquire_next_image(
                self.swapchain,
                u64::MAX,
                image_available,
                vk::Fence::null(),
            ) {
                Ok((index, suboptimal)) => {
                    if suboptimal {
                        self.swapchain_dirty = true;
                    }
                    index as usize
                }
                Err(vk::Result::ERROR_OUT_OF_DATE_KHR) => {
                    self.swapchain_dirty = true;
                    return Ok(());
                }
                Err(e) => return Err(e.into()),
            };

            // Only reset the fence once we know we'll submit (avoids deadlock if
            // acquire bailed out above with the fence already reset).
            self.device.reset_fences(&[in_flight])?;

            self.record_frame(cmd, image_index, camera)?;

            let render_finished = self.render_finished[image_index];
            let wait_info = vk::SemaphoreSubmitInfo::default()
                .semaphore(image_available)
                .stage_mask(vk::PipelineStageFlags2::COLOR_ATTACHMENT_OUTPUT);
            let signal_info = vk::SemaphoreSubmitInfo::default()
                .semaphore(render_finished)
                .stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS);
            let cmd_info = vk::CommandBufferSubmitInfo::default().command_buffer(cmd);
            let submit = vk::SubmitInfo2::default()
                .wait_semaphore_infos(std::slice::from_ref(&wait_info))
                .command_buffer_infos(std::slice::from_ref(&cmd_info))
                .signal_semaphore_infos(std::slice::from_ref(&signal_info));
            self.device
                .queue_submit2(self.queue, std::slice::from_ref(&submit), in_flight)?;

            let swapchains = [self.swapchain];
            let indices = [image_index as u32];
            let wait_sems = [render_finished];
            let present_info = vk::PresentInfoKHR::default()
                .wait_semaphores(&wait_sems)
                .swapchains(&swapchains)
                .image_indices(&indices);
            match self.swapchain_loader.queue_present(self.queue, &present_info) {
                Ok(suboptimal) => {
                    if suboptimal {
                        self.swapchain_dirty = true;
                    }
                }
                Err(vk::Result::ERROR_OUT_OF_DATE_KHR) => self.swapchain_dirty = true,
                Err(e) => return Err(e.into()),
            }

            self.frame_index = (self.frame_index + 1) % FRAMES_IN_FLIGHT;
            Ok(())
        }
    }

    fn resize(&mut self, _width: u32, _height: u32) {
        // Lazy: actual recreation happens at the start of the next render_frame,
        // which re-queries surface capabilities / window size itself.
        self.swapchain_dirty = true;
    }

    fn set_mesh(&mut self, vertices: &[MeshVertex], indices: &[u32]) -> Result<()> {
        unsafe {
            // Simple and correct: guarantees no in-flight command buffer is
            // still reading the buffers we're about to destroy. Fine for
            // "upload once at startup"; per-frame mesh streaming would need
            // a smarter (non-stalling) replacement strategy.
            self.device.device_wait_idle()?;
            self.destroy_mesh_buffers();

            if vertices.is_empty() || indices.is_empty() {
                self.mesh_index_count = 0;
                return Ok(());
            }

            let (vb, va) = self
                .create_mapped_buffer_with_data(vertices, vk::BufferUsageFlags::VERTEX_BUFFER)?;
            let (ib, ia) = self
                .create_mapped_buffer_with_data(indices, vk::BufferUsageFlags::INDEX_BUFFER)?;

            self.mesh_vertex_buffer = vb;
            self.mesh_vertex_allocation = va;
            self.mesh_index_buffer = ib;
            self.mesh_index_allocation = ia;
            self.mesh_index_count = indices.len() as u32;
            Ok(())
        }
    }

    fn set_ui_mesh(&mut self, vertices: &[UiVertex], indices: &[u32]) -> Result<()> {
        unsafe {
            // Same wait-idle-then-recreate contract as `set_mesh` — see its
            // comment. Called at most a few times per second (once per menu/
            // inventory toggle, once per resize), not every frame.
            self.device.device_wait_idle()?;
            self.destroy_ui_buffers();

            if vertices.is_empty() || indices.is_empty() {
                self.ui_index_count = 0;
                return Ok(());
            }

            let (vb, va) = self
                .create_mapped_buffer_with_data(vertices, vk::BufferUsageFlags::VERTEX_BUFFER)?;
            let (ib, ia) = self
                .create_mapped_buffer_with_data(indices, vk::BufferUsageFlags::INDEX_BUFFER)?;

            self.ui_vertex_buffer = vb;
            self.ui_vertex_allocation = va;
            self.ui_index_buffer = ib;
            self.ui_index_allocation = ia;
            self.ui_index_count = indices.len() as u32;
            Ok(())
        }
    }
}

impl Drop for VkRenderer {
    fn drop(&mut self) {
        unsafe {
            // Nothing may be in flight while we tear down.
            let _ = self.device.device_wait_idle();

            self.device.destroy_pipeline(self.pipeline, None);
            self.device
                .destroy_pipeline_layout(self.pipeline_layout, None);

            self.device.destroy_pipeline(self.ui_pipeline, None);
            self.device
                .destroy_pipeline_layout(self.ui_pipeline_layout, None);
            self.destroy_ui_buffers();

            self.destroy_mesh_buffers();

            for (i, &buffer) in self.uniform_buffers.iter().enumerate() {
                self.allocator.destroy_buffer(buffer, &mut self.uniform_allocations[i]);
            }
            self.device.destroy_descriptor_pool(self.descriptor_pool, None);
            self.device
                .destroy_descriptor_set_layout(self.descriptor_set_layout, None);

            self.device.destroy_sampler(self.atlas_sampler, None);
            self.device.destroy_image_view(self.atlas_view, None);
            self.allocator.destroy_image(self.atlas_image, &mut self.atlas_allocation);

            for frame in &self.frames {
                self.device.destroy_semaphore(frame.image_available, None);
                self.device.destroy_fence(frame.in_flight, None);
            }
            self.device.destroy_command_pool(self.command_pool, None);

            self.destroy_swapchain_resources();
            if self.swapchain != vk::SwapchainKHR::null() {
                self.swapchain_loader.destroy_swapchain(self.swapchain, None);
            }

            // Must run before device destruction — the allocator's own Drop impl
            // would otherwise fire too late (regular field drop order is after
            // this function returns), calling vmaDestroyAllocator on a dead device.
            ManuallyDrop::drop(&mut self.allocator);

            self.device.destroy_device(None);
            self.surface_loader.destroy_surface(self.surface, None);
            self.instance.destroy_instance(None);
        }
    }
}

/// Picks a physical device supporting Vulkan 1.3 with a queue family that does
/// both graphics and present. Prefers discrete > integrated > anything else.
fn pick_physical_device(
    instance: &ash::Instance,
    surface_loader: &ash::khr::surface::Instance,
    surface: vk::SurfaceKHR,
) -> Result<(vk::PhysicalDevice, u32)> {
    unsafe {
        let mut best: Option<(vk::PhysicalDevice, u32, u32)> = None; // (pd, family, score)
        for pd in instance.enumerate_physical_devices()? {
            let props = instance.get_physical_device_properties(pd);
            if props.api_version < vk::API_VERSION_1_3 {
                continue;
            }
            let families = instance.get_physical_device_queue_family_properties(pd);
            let family = families.iter().enumerate().find_map(|(i, f)| {
                let graphics = f.queue_flags.contains(vk::QueueFlags::GRAPHICS);
                let present = surface_loader
                    .get_physical_device_surface_support(pd, i as u32, surface)
                    .unwrap_or(false);
                (graphics && present).then_some(i as u32)
            });
            let Some(family) = family else { continue };
            let score = match props.device_type {
                vk::PhysicalDeviceType::DISCRETE_GPU => 2,
                vk::PhysicalDeviceType::INTEGRATED_GPU => 1,
                _ => 0,
            };
            if best.map_or(true, |(_, _, s)| score > s) {
                best = Some((pd, family, score));
            }
        }
        best.map(|(pd, family, _)| (pd, family))
            .context("no Vulkan 1.3 device with a graphics+present queue found")
    }
}

/// First candidate (see `DEPTH_FORMAT_CANDIDATES`) supporting optimal-tiling
/// depth-attachment usage on `phys_device`.
fn pick_depth_format(instance: &ash::Instance, phys_device: vk::PhysicalDevice) -> Result<vk::Format> {
    unsafe {
        DEPTH_FORMAT_CANDIDATES
            .into_iter()
            .find(|&format| {
                let props = instance.get_physical_device_format_properties(phys_device, format);
                props
                    .optimal_tiling_features
                    .contains(vk::FormatFeatureFlags::DEPTH_STENCIL_ATTACHMENT)
            })
            .context("none of the candidate depth formats are supported")
    }
}

fn aspect_subresource_range(aspect: vk::ImageAspectFlags) -> vk::ImageSubresourceRange {
    vk::ImageSubresourceRange {
        aspect_mask: aspect,
        base_mip_level: 0,
        level_count: 1,
        base_array_layer: 0,
        layer_count: 1,
    }
}

#[allow(clippy::too_many_arguments)]
fn image_barrier(
    device: &ash::Device,
    cmd: vk::CommandBuffer,
    image: vk::Image,
    aspect: vk::ImageAspectFlags,
    src_stage: vk::PipelineStageFlags2,
    src_access: vk::AccessFlags2,
    dst_stage: vk::PipelineStageFlags2,
    dst_access: vk::AccessFlags2,
    old_layout: vk::ImageLayout,
    new_layout: vk::ImageLayout,
) {
    let barrier = vk::ImageMemoryBarrier2::default()
        .src_stage_mask(src_stage)
        .src_access_mask(src_access)
        .dst_stage_mask(dst_stage)
        .dst_access_mask(dst_access)
        .old_layout(old_layout)
        .new_layout(new_layout)
        .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
        .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
        .image(image)
        .subresource_range(aspect_subresource_range(aspect));
    let dep = vk::DependencyInfo::default()
        .image_memory_barriers(std::slice::from_ref(&barrier));
    unsafe { device.cmd_pipeline_barrier2(cmd, &dep) };
}
