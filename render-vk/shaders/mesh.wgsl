struct Globals {
    view_proj: mat4x4<f32>,
    atlas_tile_count: f32,
};
@group(0) @binding(0) var<uniform> globals: Globals;
@group(0) @binding(1) var atlas_tex: texture_2d<f32>;
@group(0) @binding(2) var atlas_sampler: sampler;

struct VertexIn {
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) uv: vec2<f32>,
    @location(3) tile: f32,
    @location(4) shade: f32,
};

struct VertexOut {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) tile: f32,
    @location(2) shade: f32,
};

@vertex
fn vs_main(in: VertexIn) -> VertexOut {
    var out: VertexOut;
    out.position = globals.view_proj * vec4<f32>(in.position, 1.0);
    out.uv = in.uv;
    out.tile = in.tile;
    out.shade = in.shade;
    return out;
}

@fragment
fn fs_main(in: VertexOut) -> @location(0) vec4<f32> {
    // Atlas is a single row of `atlas_tile_count` square tiles. `in.uv` spans
    // 0..width/0..height in block units (see engine_core::mesh::triangulate),
    // not the unit square, so `fract()` it back down to one tile's worth
    // before offsetting by the tile index — this is what makes the tile
    // repeat once per block on a merged quad instead of stretching across
    // the whole face.
    let tile_uv = fract(in.uv);
    let atlas_uv = vec2<f32>((in.tile + tile_uv.x) / globals.atlas_tile_count, tile_uv.y);
    let sampled = textureSample(atlas_tex, atlas_sampler, atlas_uv);
    return vec4<f32>(sampled.rgb * in.shade, sampled.a);
}
