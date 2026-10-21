// Full-screen helper for the blurred background of `Fit::Blur`.
// mode 0: one direction of a Gaussian blur. mode 1: copy, scaled by `dim`.

struct Params {
    // Distance between taps in uv units (set in one axis only for mode 0).
    step: vec2<f32>,
    dim: f32,
    mode: u32,
};

@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var blur_sampler: sampler;
@group(0) @binding(2) var source: texture_2d<f32>;

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) index: u32) -> VsOut {
    // One triangle that covers the whole target.
    let corner = vec2<f32>(f32((index << 1u) & 2u), f32(index & 2u));
    var out: VsOut;
    out.pos = vec4<f32>(corner * 2.0 - 1.0, 0.0, 1.0);
    out.uv = vec2<f32>(corner.x, 1.0 - corner.y);
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    if (params.mode == 1u) {
        let c = textureSampleLevel(source, blur_sampler, in.uv, 0.0);
        return vec4<f32>(c.rgb * params.dim, 1.0);
    }
    // Nine taps of a Gaussian with sigma 2 (in tap units).
    var weights = array<f32, 5>(0.2270, 0.1945, 0.1216, 0.0540, 0.0162);
    var sum = textureSampleLevel(source, blur_sampler, in.uv, 0.0).rgb * weights[0];
    for (var i = 1; i < 5; i++) {
        let offset = params.step * f32(i);
        sum += textureSampleLevel(source, blur_sampler, in.uv + offset, 0.0).rgb * weights[i];
        sum += textureSampleLevel(source, blur_sampler, in.uv - offset, 0.0).rgb * weights[i];
    }
    return vec4<f32>(sum, 1.0);
}
