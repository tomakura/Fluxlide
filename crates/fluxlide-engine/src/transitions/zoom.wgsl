// The outgoing picture grows to 1.3 times its size while it fades out; the
// incoming one shrinks from 1.3 times to its own size while it fades in.
// Both stay larger than the output, so no black edge shows.
fn transition(uv: vec2<f32>, progress: f32) -> vec4<f32> {
    let t = progress * progress * (3.0 - 2.0 * progress);
    let center = vec2<f32>(0.5);
    let from_scale = 1.0 + 0.3 * t;
    let to_scale = 1.3 - 0.3 * t;
    let from_uv = center + (uv - center) / from_scale;
    let to_uv = center + (uv - center) / to_scale;
    return mix(from_color(from_uv), to_color(to_uv), t);
}
