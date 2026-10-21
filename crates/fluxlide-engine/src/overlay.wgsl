// Draws one piece of an overlay (caption or watermark) onto the target:
// a rounded rectangle, a line of text, or a small picture. Output is
// premultiplied and blended "over". Colors are linear light.

struct Draw {
    // Left, top, right, bottom in normalized device coordinates.
    rect: vec4<f32>,
    // Linear light, not premultiplied.
    color: vec4<f32>,
    // Size and top-left corner in target pixels.
    size: vec2<f32>,
    origin: vec2<f32>,
    // Alpha multiplier at the top and at the bottom edge of a shape.
    gradient: vec2<f32>,
    radius: f32,
    // 0 fills a shape; above 0 draws only an outline of this width.
    border: f32,
    opacity: f32,
    // Alpha of the black shadow under text; text has its shadow in the green channel.
    shadow: f32,
    // 0: shape. 1: text mask (red: coverage, green: shadow). 2: picture.
    kind: u32,
    // Text coverage is raised to this power before blending. Blending in
    // linear light makes the anti-aliased edges of light text look heavier,
    // and those of dark text lighter, than blending in sRGB would; a power
    // above 1 thins light text, one below 1 thickens dark text.
    text_power: f32,
};

@group(0) @binding(0) var<uniform> draw: Draw;
@group(0) @binding(1) var overlay_sampler: sampler;
@group(0) @binding(2) var overlay_tex: texture_2d<f32>;

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) index: u32) -> VsOut {
    let corner = vec2<f32>(f32(index & 1u), f32(index >> 1u));
    var out: VsOut;
    out.pos = vec4<f32>(
        mix(draw.rect.x, draw.rect.z, corner.x),
        mix(draw.rect.y, draw.rect.w, corner.y),
        0.0,
        1.0,
    );
    out.uv = corner;
    return out;
}

// Signed distance to a rounded rectangle centered at the origin, in pixels.
fn rounded_box(p: vec2<f32>, half_size: vec2<f32>, radius: f32) -> f32 {
    let q = abs(p) - (half_size - vec2<f32>(radius));
    return length(max(q, vec2<f32>(0.0))) + min(max(q.x, q.y), 0.0) - radius;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    var rgb = vec3<f32>(0.0);
    var alpha = 0.0;
    if (draw.kind == 0u) {
        let p = (in.uv - vec2<f32>(0.5)) * draw.size;
        let half_size = draw.size * 0.5;
        var d = rounded_box(p, half_size, draw.radius);
        var coverage = clamp(0.5 - d, 0.0, 1.0);
        if (draw.border > 0.0) {
            let inner = rounded_box(p, half_size - vec2<f32>(draw.border), max(draw.radius - draw.border, 0.0));
            coverage = coverage * clamp(inner + 0.5, 0.0, 1.0);
        }
        let t = in.uv.y;
        let ramp = t * t * (3.0 - 2.0 * t);
        alpha = coverage * draw.color.a * mix(draw.gradient.x, draw.gradient.y, ramp);
        rgb = draw.color.rgb * alpha;
    } else if (draw.kind == 1u) {
        // The mask is drawn pixel for pixel, so read it without filtering.
        let texel = vec2<i32>(in.pos.xy - draw.origin);
        let mask = textureLoad(overlay_tex, texel, 0);
        let text = pow(mask.r, draw.text_power) * draw.color.a;
        let shadow = mask.g * draw.shadow * (1.0 - text);
        alpha = text + shadow;
        rgb = draw.color.rgb * text;
    } else {
        let c = textureSampleLevel(overlay_tex, overlay_sampler, in.uv, 0.0);
        alpha = c.a;
        rgb = c.rgb * c.a;
    }
    return vec4<f32>(rgb, alpha) * draw.opacity;
}
