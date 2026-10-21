// Push. fx.params.xy is the unit direction the pictures move in, in uv
// space (y points down): (-1, 0) moves them left, so the incoming picture
// enters from the right edge.
fn transition(uv: vec2<f32>, progress: f32) -> vec4<f32> {
    let t = progress * progress * (3.0 - 2.0 * progress);
    let direction = fx.params.xy;
    let from_uv = uv - direction * t;
    let to_uv = uv - direction * (t - 1.0);
    if (all(to_uv >= vec2<f32>(0.0)) && all(to_uv <= vec2<f32>(1.0))) {
        return to_color(to_uv);
    }
    return from_color(from_uv);
}
