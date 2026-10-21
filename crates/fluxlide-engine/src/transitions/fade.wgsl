// Crossfade in linear light. With a black outgoing picture this is the
// same fade-in from black as `to * progress`.
fn transition(uv: vec2<f32>, progress: f32) -> vec4<f32> {
    return mix(from_color(uv), to_color(uv), progress);
}
