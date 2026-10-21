// A soft edge travels across the output and reveals the incoming picture.
// fx.params.xy is the unit direction the edge travels in, in uv space
// (y points down): (1, 0) reveals from the left edge to the right edge.
fn transition(uv: vec2<f32>, progress: f32) -> vec4<f32> {
    let feather = 0.08;
    let along = dot(uv - vec2<f32>(0.5), fx.params.xy) + 0.5;
    // The edge starts and ends outside the output, feather included.
    let edge = mix(-feather, 1.0 + feather, progress);
    let revealed = 1.0 - smoothstep(edge - feather, edge + feather, along);
    return mix(from_color(uv), to_color(uv), revealed);
}
