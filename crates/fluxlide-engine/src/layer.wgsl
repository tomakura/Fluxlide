// Draws one picture (photo or video frame) into a rectangle of the output.
// Layers are added together over black, each scaled by its opacity, so a
// crossfade of a and b gives a * (1 - t) + b * t everywhere, including
// where only one of them covers the screen.

struct Layer {
    // Left, top, right, bottom in normalized device coordinates.
    rect: vec4<f32>,
    opacity: f32,
    // 0: sRGB RGBA in tex0. 1: Y, U, V planes in tex0..tex2 (BT.709 limited range).
    mode: u32,
    _pad: vec2<f32>,
};

@group(0) @binding(0) var<uniform> layer: Layer;
@group(0) @binding(1) var picture_sampler: sampler;
@group(0) @binding(2) var tex0: texture_2d<f32>;
@group(0) @binding(3) var tex1: texture_2d<f32>;
@group(0) @binding(4) var tex2: texture_2d<f32>;

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) index: u32) -> VsOut {
    // Triangle strip: top-left, top-right, bottom-left, bottom-right.
    let corner = vec2<f32>(f32(index & 1u), f32(index >> 1u));
    var out: VsOut;
    out.pos = vec4<f32>(
        mix(layer.rect.x, layer.rect.z, corner.x),
        mix(layer.rect.y, layer.rect.w, corner.y),
        0.0,
        1.0,
    );
    out.uv = corner;
    return out;
}

fn srgb_to_linear(c: vec3<f32>) -> vec3<f32> {
    let low = c / 12.92;
    let high = pow((c + 0.055) / 1.055, vec3<f32>(2.4));
    return select(high, low, c <= vec3<f32>(0.04045));
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let s0 = textureSample(tex0, picture_sampler, in.uv);
    let s1 = textureSample(tex1, picture_sampler, in.uv).r;
    let s2 = textureSample(tex2, picture_sampler, in.uv).r;

    var rgb = s0.rgb;
    if (layer.mode == 1u) {
        let y = (s0.r - 16.0 / 255.0) * (255.0 / 219.0);
        let u = (s1 - 128.0 / 255.0) * (255.0 / 224.0);
        let v = (s2 - 128.0 / 255.0) * (255.0 / 224.0);
        let encoded = vec3<f32>(
            y + 1.5748 * v,
            y - 0.1873 * u - 0.4681 * v,
            y + 1.8556 * u,
        );
        // The output surface is sRGB, so blending happens in linear light.
        rgb = srgb_to_linear(clamp(encoded, vec3<f32>(0.0), vec3<f32>(1.0)));
    }
    return vec4<f32>(rgb * layer.opacity, layer.opacity);
}
