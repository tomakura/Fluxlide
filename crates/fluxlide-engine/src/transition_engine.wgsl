// Engine side of a transition shader, appended after the author's code.
// See the module documentation of `transition.rs` for the contract.

struct FxUniforms {
    resolution: vec2<f32>,
    progress: f32,
    aspect: f32,
    params: vec4<f32>,
};

@group(0) @binding(0) var<uniform> fx: FxUniforms;
@group(0) @binding(1) var fx_sampler: sampler;
@group(0) @binding(2) var fx_from: texture_2d<f32>;
@group(0) @binding(3) var fx_to: texture_2d<f32>;

fn fx_inside(uv: vec2<f32>) -> bool {
    return all(uv >= vec2<f32>(0.0)) && all(uv <= vec2<f32>(1.0));
}

fn from_color(uv: vec2<f32>) -> vec4<f32> {
    let c = textureSampleLevel(fx_from, fx_sampler, uv, 0.0);
    return select(vec4<f32>(0.0, 0.0, 0.0, 1.0), c, fx_inside(uv));
}

fn to_color(uv: vec2<f32>) -> vec4<f32> {
    let c = textureSampleLevel(fx_to, fx_sampler, uv, 0.0);
    return select(vec4<f32>(0.0, 0.0, 0.0, 1.0), c, fx_inside(uv));
}

struct FxVsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn fx_vs(@builtin(vertex_index) index: u32) -> FxVsOut {
    // One triangle that covers the whole output.
    let corner = vec2<f32>(f32((index << 1u) & 2u), f32(index & 2u));
    var out: FxVsOut;
    out.pos = vec4<f32>(corner * 2.0 - 1.0, 0.0, 1.0);
    out.uv = vec2<f32>(corner.x, 1.0 - corner.y);
    return out;
}

@fragment
fn fx_fs(in: FxVsOut) -> @location(0) vec4<f32> {
    let color: vec4<f32> = transition(in.uv, fx.progress);
    return vec4<f32>(color.rgb, 1.0);
}
