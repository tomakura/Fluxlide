// Draws one picture (photo or video frame) into a rectangle of the target.
// Output is premultiplied by the layer's opacity and is meant to be blended
// "over" what is already there. Colors are linear light: the targets are
// sRGB textures, so the hardware encodes on write and blends in linear light.

struct Layer {
    // Left, top, right, bottom in normalized device coordinates. May reach
    // beyond the target (cover fit, zoom); the extra part is clipped.
    rect: vec4<f32>,
    opacity: f32,
    // 0: sRGB RGBA in tex0. 1: Y, U, V planes in tex0..tex2 (BT.709 limited range).
    mode: u32,
    // Samples per axis. 1 reads the picture once per pixel; more averages a
    // grid of samples over the pixel's footprint, for large shrinking.
    supersample: u32,
    _pad: u32,
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

// Linear-light RGB of the picture at `uv`.
fn picture_at(uv: vec2<f32>) -> vec3<f32> {
    let s0 = textureSampleLevel(tex0, picture_sampler, uv, 0.0);
    if (layer.mode == 0u) {
        // The texture is sRGB, so sampling already decoded it.
        return s0.rgb;
    }
    let s1 = textureSampleLevel(tex1, picture_sampler, uv, 0.0).r;
    let s2 = textureSampleLevel(tex2, picture_sampler, uv, 0.0).r;
    let y = (s0.r - 16.0 / 255.0) * (255.0 / 219.0);
    let u = (s1 - 128.0 / 255.0) * (255.0 / 224.0);
    let v = (s2 - 128.0 / 255.0) * (255.0 / 224.0);
    let encoded = vec3<f32>(
        y + 1.5748 * v,
        y - 0.1873 * u - 0.4681 * v,
        y + 1.8556 * u,
    );
    return srgb_to_linear(clamp(encoded, vec3<f32>(0.0), vec3<f32>(1.0)));
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    // Derivatives must be taken in uniform control flow, before any branch.
    let step_x = dpdx(in.uv);
    let step_y = dpdy(in.uv);
    var rgb = vec3<f32>(0.0);
    if (layer.supersample <= 1u) {
        rgb = picture_at(in.uv);
    } else {
        let n = f32(layer.supersample);
        for (var j = 0u; j < layer.supersample; j++) {
            for (var i = 0u; i < layer.supersample; i++) {
                let offset = vec2<f32>(f32(i) + 0.5, f32(j) + 0.5) / n - vec2<f32>(0.5);
                rgb += picture_at(in.uv + step_x * offset.x + step_y * offset.y);
            }
        }
        rgb /= n * n;
    }
    return vec4<f32>(rgb * layer.opacity, layer.opacity);
}
