// Presentation test pattern: navy background, a coral bar that sweeps
// across the screen every 4 seconds, guide lines and an edge border.
// Stutter shows up as a jumping bar; a missing border edge means the
// window does not cover the whole display.

struct Params {
    time: f32,
    width: f32,
    height: f32,
    _pad: f32,
};

@group(0) @binding(0) var<uniform> params: Params;

struct VsOut {
    @builtin(position) pos: vec4<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) index: u32) -> VsOut {
    // One triangle that covers the whole viewport.
    var corners = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>(3.0, -1.0),
        vec2<f32>(-1.0, 3.0),
    );
    var out: VsOut;
    out.pos = vec4<f32>(corners[index], 0.0, 1.0);
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    // Colors are linear values of #13213B, #E8553A and #F5F6F8.
    let navy = vec3<f32>(0.0065, 0.0152, 0.0437);
    let coral = vec3<f32>(0.807, 0.0908, 0.0423);
    let paper = vec3<f32>(0.913, 0.922, 0.939);

    let px = in.pos.xy;
    let size = vec2<f32>(params.width, params.height);
    let border = max(4.0, size.y / 180.0);

    var color = navy;

    // Ten guide lines to judge motion against.
    let cell = size.x / 10.0;
    if (abs(px.x - round(px.x / cell) * cell) < 1.0) {
        color = mix(navy, paper, 0.18);
    }

    // Sweeping bar.
    let bar_x = fract(params.time / 4.0) * size.x;
    let bar_w = size.x * 0.04;
    if (abs(px.x - bar_x) < bar_w * 0.5) {
        color = coral;
    }

    // Border touching every edge of the display.
    if (px.x < border || px.y < border || px.x > size.x - border || px.y > size.y - border) {
        color = paper;
    }

    return vec4<f32>(color, 1.0);
}
