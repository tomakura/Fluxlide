// Copies a frame onto another texture, scaled to fit it: onto the window,
// or onto the small copy shown on the operator's screen. Four bilinear
// samples per output pixel keep a scaled-down copy from flickering; a copy
// that is not scaled down takes its pixels as they are.

@group(0) @binding(0) var source_sampler: sampler;
@group(0) @binding(1) var source: texture_2d<f32>;

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

// One triangle covering the target.
@vertex
fn vs_main(@builtin(vertex_index) index: u32) -> VsOut {
    let uv = vec2<f32>(f32((index << 1u) & 2u), f32(index & 2u));
    var out: VsOut;
    out.pos = vec4<f32>(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0, 0.0, 1.0);
    out.uv = uv;
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let footprint = fwidth(in.uv);
    if all(footprint * vec2<f32>(textureDimensions(source)) <= vec2<f32>(1.001)) {
        return textureSampleLevel(source, source_sampler, in.uv, 0.0);
    }
    // A quarter of an output pixel, in texture coordinates.
    let step = footprint * 0.25;
    let sum = textureSampleLevel(source, source_sampler, in.uv + vec2<f32>(-step.x, -step.y), 0.0)
        + textureSampleLevel(source, source_sampler, in.uv + vec2<f32>(step.x, -step.y), 0.0)
        + textureSampleLevel(source, source_sampler, in.uv + vec2<f32>(-step.x, step.y), 0.0)
        + textureSampleLevel(source, source_sampler, in.uv + vec2<f32>(step.x, step.y), 0.0);
    return sum * 0.25;
}
