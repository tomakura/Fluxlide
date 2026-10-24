// A yellow cover with a paw-shaped hole closes in on the outgoing picture,
// a dark paw stamps the hole shut while the pictures change behind it, and
// the hole opens again on the incoming picture.
//
// Of the length: 40% closing, 10% for the change of pictures, 10% for the
// dark paw to fade away, 40% opening.
//
// Coordinates are paw units (the paw is 512 wide), relative to the middle
// of the palm, x to the right, y down.

// Closed, the paw is this many units per output height, so it is 0.49 of
// the output height wide and sits in the middle.
const PAW_UNITS_PER_HEIGHT: f32 = 1043.0;
// The palm stays at this point of the output while the hole grows and
// shrinks, so that open, the palm covers the whole output.
const PAW_ANCHOR: vec2<f32> = vec2<f32>(0.5, 0.6);
const PAW_TOE_RADIUS: f32 = 55.67;
const PAW_OUTLINE: f32 = 2.3;
// Colors of the cover, the dark paw and the hole's outline, in linear light
// (#E4CA39, #241000, #231815).
const PAW_COVER: vec3<f32> = vec3<f32>(0.7758, 0.5906, 0.0409);
const PAW_STAMP: vec3<f32> = vec3<f32>(0.0176, 0.0052, 0.0);
const PAW_LINE: vec3<f32> = vec3<f32>(0.0168, 0.0091, 0.0075);

// Right half of the palm's outline, from the bottom middle round to the
// top middle. The left half is its mirror image.
var<private> PAW_PALM: array<vec2<f32>, 49> = array<vec2<f32>, 49>(
    vec2<f32>(0.00, 89.40),
    vec2<f32>(7.04, 89.64),
    vec2<f32>(14.61, 90.28),
    vec2<f32>(22.62, 91.21),
    vec2<f32>(31.02, 92.32),
    vec2<f32>(39.71, 93.48),
    vec2<f32>(48.64, 94.59),
    vec2<f32>(57.72, 95.53),
    vec2<f32>(66.89, 96.18),
    vec2<f32>(76.07, 96.44),
    vec2<f32>(85.19, 96.18),
    vec2<f32>(94.18, 95.30),
    vec2<f32>(102.95, 93.67),
    vec2<f32>(111.45, 91.19),
    vec2<f32>(119.59, 87.73),
    vec2<f32>(127.31, 83.19),
    vec2<f32>(134.53, 77.45),
    vec2<f32>(141.18, 70.40),
    vec2<f32>(147.18, 61.91),
    vec2<f32>(152.46, 51.89),
    vec2<f32>(156.95, 40.20),
    vec2<f32>(159.49, 30.79),
    vec2<f32>(161.10, 21.15),
    vec2<f32>(161.84, 11.32),
    vec2<f32>(161.72, 1.36),
    vec2<f32>(160.79, -8.70),
    vec2<f32>(159.09, -18.82),
    vec2<f32>(156.65, -28.94),
    vec2<f32>(153.51, -39.04),
    vec2<f32>(149.71, -49.06),
    vec2<f32>(145.28, -58.96),
    vec2<f32>(140.25, -68.71),
    vec2<f32>(134.68, -78.25),
    vec2<f32>(128.58, -87.54),
    vec2<f32>(122.00, -96.55),
    vec2<f32>(114.98, -105.22),
    vec2<f32>(107.56, -113.52),
    vec2<f32>(99.76, -121.41),
    vec2<f32>(91.63, -128.83),
    vec2<f32>(83.20, -135.75),
    vec2<f32>(74.51, -142.12),
    vec2<f32>(65.59, -147.90),
    vec2<f32>(56.49, -153.05),
    vec2<f32>(47.24, -157.53),
    vec2<f32>(37.87, -161.29),
    vec2<f32>(28.43, -164.29),
    vec2<f32>(18.94, -166.48),
    vec2<f32>(9.46, -167.83),
    vec2<f32>(0.00, -168.29),
);

// Signed distance to the paw's edge: negative inside.
fn paw_distance(p: vec2<f32>) -> f32 {
    let m = vec2<f32>(abs(p.x), p.y);
    var edge = 1e9;
    var inside = false;
    for (var i = 0u; i < 48u; i++) {
        let a = PAW_PALM[i];
        let b = PAW_PALM[i + 1u];
        let e = b - a;
        let w = m - a;
        let t = clamp(dot(w, e) / dot(e, e), 0.0, 1.0);
        edge = min(edge, length(w - e * t));
        // Crossings of a ray towards +x; the closing edge on x = 0 is never hit.
        if ((a.y > m.y) != (b.y > m.y) && m.x < a.x + e.x * (m.y - a.y) / e.y) {
            inside = !inside;
        }
    }
    let palm = select(edge, -edge, inside);
    let front = length(m - vec2<f32>(82.6, -248.22)) - PAW_TOE_RADIUS;
    let side = length(m - vec2<f32>(200.33, -139.25)) - PAW_TOE_RADIUS;
    return min(palm, min(front, side));
}

// Each movement starts gently and takes longer to stop: it speeds up
// (cubic) over the first 20% of its time and slows down (quadratic) over
// the rest. The two parts meet at the same speed.
const PAW_EASE_TURN: f32 = 0.2;
const PAW_EASE_AT_TURN: f32 = 0.142857;

fn paw_ease(t: f32) -> f32 {
    let x = clamp(t, 0.0, 1.0);
    if (x < PAW_EASE_TURN) {
        let a = x / PAW_EASE_TURN;
        return PAW_EASE_AT_TURN * a * a * a;
    }
    let b = (1.0 - x) / (1.0 - PAW_EASE_TURN);
    return 1.0 - (1.0 - PAW_EASE_AT_TURN) * b * b;
}

fn transition(uv: vec2<f32>, progress: f32) -> vec4<f32> {
    let closing = paw_ease(progress / 0.4);
    let opening = paw_ease((progress - 0.6) / 0.4);
    // Open, the hole is large enough for the palm to cover outputs up to
    // 16:9; wider ones open further.
    let open_scale = 4.6 * max(fx.aspect, 16.0 / 9.0);
    // The scale moves by ratio rather than by difference, so the hole
    // seems to grow and shrink at an even pace instead of rushing while
    // it is large.
    let scale = exp2(log2(open_scale) * (1.0 - closing + opening));
    let units = PAW_UNITS_PER_HEIGHT / scale;
    let p = vec2<f32>((uv.x - PAW_ANCHOR.x) * fx.aspect, uv.y - PAW_ANCHOR.y) * units;
    let d = paw_distance(p);
    // One output pixel in paw units. Edges are as soft as a pixel: what
    // part of a pixel-wide span around `d` lies outside, or on the line.
    let pixel = units / fx.resolution.y;
    let outside = clamp(d / pixel + 0.5, 0.0, 1.0);
    let near = abs(d);
    let half_line = PAW_OUTLINE * 0.5;
    let line = max(0.0, min(near + pixel * 0.5, half_line) - max(near - pixel * 0.5, -half_line)) / pixel;

    let picture = mix(from_color(uv).rgb, to_color(uv).rgb, clamp((progress - 0.4) / 0.1, 0.0, 1.0));
    let stamp = min(closing, 1.0 - paw_ease((progress - 0.5) / 0.1));
    let hole = mix(picture, PAW_STAMP, stamp);
    let color = mix(mix(hole, PAW_COVER, outside), PAW_LINE, line);
    return vec4<f32>(color, 1.0);
}
