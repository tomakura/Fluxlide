//! Tests that draw scenes on the GPU and check the pixels read back.
//! They are skipped when no GPU is available, like the other GPU tests.

use std::sync::Arc;

use crate::caption::{Caption, CaptionDesign, CaptionStyle};
use crate::overlay::Overlay;
use crate::scene::{Fit, Frame, Layer, OverlayRef, Scene, Transform, TransitionRun};
use crate::transition::{Direction, Transition};
use crate::watermark::{Corner, Watermark};
use crate::{EngineError, MediaTexture, OffscreenTarget, Renderer, test_renderer};

/// A picture of one sRGB color.
fn solid(renderer: &Renderer, [r, g, b]: [u8; 3]) -> MediaTexture {
    let pixels: Vec<u8> = std::iter::repeat_n([r, g, b, 255], 16).flatten().collect();
    MediaTexture::from_rgba(renderer, 4, 4, &pixels)
}

/// A picture split in two: `left` colors the left half, `right` the right.
fn halves(renderer: &Renderer, left: [u8; 3], right: [u8; 3]) -> MediaTexture {
    let mut pixels = Vec::new();
    for _row in 0..4 {
        for x in 0..4 {
            let [r, g, b] = if x < 2 { left } else { right };
            pixels.extend_from_slice(&[r, g, b, 255]);
        }
    }
    MediaTexture::from_rgba(renderer, 4, 4, &pixels)
}

fn cover(texture: &MediaTexture) -> Layer<'_> {
    let mut layer = Layer::new(texture);
    layer.fit = Fit::Cover;
    layer
}

fn render(renderer: &Renderer, size: (u32, u32), frame: Frame<'_>) -> Vec<u8> {
    let mut target = OffscreenTarget::new(renderer, size.0, size.1).expect("offscreen target");
    target
        .render_rgba(renderer, &Scene::Frame(frame))
        .expect("render frame")
}

fn pixel(pixels: &[u8], width: u32, x: u32, y: u32) -> [u8; 4] {
    let at = ((y * width + x) * 4) as usize;
    pixels[at..at + 4].try_into().unwrap()
}

fn assert_close(actual: [u8; 4], expected: [u8; 3], tolerance: u8, what: &str) {
    let close = actual[..3]
        .iter()
        .zip(expected)
        .all(|(a, e)| a.abs_diff(e) <= tolerance)
        && actual[3] == 255;
    assert!(
        close,
        "{what}: {actual:?} is not {expected:?} (+-{tolerance})"
    );
}

fn transition_frame<'a>(
    from: &'a MediaTexture,
    to: &'a MediaTexture,
    transition: &'a Transition,
    progress: f32,
) -> Frame<'a> {
    Frame {
        from: Some(cover(from)),
        to: Some(cover(to)),
        transition: Some(TransitionRun {
            transition,
            progress,
        }),
        overlays: Vec::new(),
        from_overlays: Vec::new(),
    }
}

#[test]
fn a_picture_comes_back_unchanged_with_rows_that_need_padding() {
    let Some(renderer) = test_renderer() else {
        return;
    };
    // 70 * 4 bytes is not a multiple of 256, so readback rows are padded.
    let (width, height) = (70, 5);
    let red_ish = solid(&renderer, [200, 100, 50]);
    let pixels = render(
        &renderer,
        (width, height),
        Frame {
            to: Some(cover(&red_ish)),
            ..Default::default()
        },
    );
    assert_eq!(pixels.len(), (width * height * 4) as usize);
    for (i, px) in pixels.chunks(4).enumerate() {
        assert_close(
            px.try_into().unwrap(),
            [200, 100, 50],
            1,
            &format!("pixel {i}"),
        );
    }
}

#[test]
fn rendering_is_in_srgb_and_blends_in_linear_light() {
    let Some(renderer) = test_renderer() else {
        return;
    };
    let red = solid(&renderer, [255, 0, 0]);
    let blue = solid(&renderer, [0, 0, 255]);
    let fade = Transition::fade();
    // Half of each in linear light is 0.5, which is 188 once encoded as sRGB
    // (a mix of the encoded values would give 128).
    let pixels = render(
        &renderer,
        (16, 9),
        transition_frame(&red, &blue, &fade, 0.5),
    );
    assert_close(pixel(&pixels, 16, 8, 4), [188, 0, 188], 2, "fade at 0.5");
}

#[test]
fn every_transition_starts_on_the_outgoing_and_ends_on_the_incoming_picture() {
    let Some(renderer) = test_renderer() else {
        return;
    };
    let red = solid(&renderer, [255, 0, 0]);
    let blue = solid(&renderer, [0, 0, 255]);
    let all = [
        Transition::fade(),
        Transition::slide(Direction::Left),
        Transition::slide(Direction::Right),
        Transition::slide(Direction::Up),
        Transition::slide(Direction::Down),
        Transition::zoom(),
        Transition::wipe(Direction::Right),
        Transition::wipe(Direction::Left),
        Transition::wipe(Direction::Down),
        Transition::wipe(Direction::Up),
        Transition::paw(),
    ];
    for transition in &all {
        let start = render(
            &renderer,
            (16, 9),
            transition_frame(&red, &blue, transition, 0.0),
        );
        let end = render(
            &renderer,
            (16, 9),
            transition_frame(&red, &blue, transition, 1.0),
        );
        for (x, y) in [(1, 1), (8, 4), (14, 7)] {
            assert_close(
                pixel(&start, 16, x, y),
                [255, 0, 0],
                1,
                &format!("{} at 0", transition.name()),
            );
            assert_close(
                pixel(&end, 16, x, y),
                [0, 0, 255],
                1,
                &format!("{} at 1", transition.name()),
            );
        }
    }
}

#[test]
fn slide_pushes_the_outgoing_picture_out_in_its_direction() {
    let Some(renderer) = test_renderer() else {
        return;
    };
    let red = solid(&renderer, [255, 0, 0]);
    let blue = solid(&renderer, [0, 0, 255]);
    let size = (40, 20);
    // Half way, the outgoing picture covers one half and the incoming one the other.
    let left = render(
        &renderer,
        size,
        transition_frame(&red, &blue, &Transition::slide(Direction::Left), 0.5),
    );
    assert_close(
        pixel(&left, 40, 5, 10),
        [255, 0, 0],
        1,
        "left half, slide left",
    );
    assert_close(
        pixel(&left, 40, 34, 10),
        [0, 0, 255],
        1,
        "right half, slide left",
    );
    let down = render(
        &renderer,
        size,
        transition_frame(&red, &blue, &Transition::slide(Direction::Down), 0.5),
    );
    assert_close(
        pixel(&down, 40, 20, 15),
        [255, 0, 0],
        1,
        "lower half, slide down",
    );
    assert_close(
        pixel(&down, 40, 20, 4),
        [0, 0, 255],
        1,
        "upper half, slide down",
    );
}

#[test]
fn wipe_reveals_the_incoming_picture_behind_an_edge_travelling_in_its_direction() {
    let Some(renderer) = test_renderer() else {
        return;
    };
    let red = solid(&renderer, [255, 0, 0]);
    let blue = solid(&renderer, [0, 0, 255]);
    let wipe = Transition::wipe(Direction::Right);
    let early = render(
        &renderer,
        (40, 20),
        transition_frame(&red, &blue, &wipe, 0.3),
    );
    assert_close(pixel(&early, 40, 2, 10), [0, 0, 255], 1, "behind the edge");
    assert_close(
        pixel(&early, 40, 37, 10),
        [255, 0, 0],
        1,
        "ahead of the edge",
    );
    // The edge is soft: right on it, the two pictures are mixed.
    let middle = pixel(&early, 40, 12, 10);
    assert!(middle[0] > 20 && middle[2] > 20, "{middle:?}");
}

#[test]
fn paw_closes_on_a_dark_paw_and_opens_on_the_incoming_picture() {
    let Some(renderer) = test_renderer() else {
        return;
    };
    let red = solid(&renderer, [255, 0, 0]);
    let blue = solid(&renderer, [0, 0, 255]);
    let paw = Transition::paw();
    let size = (64, 36);
    let yellow = [228, 202, 57];
    // Closed: the dark paw in the middle, the yellow cover around it.
    let closed = render(&renderer, size, transition_frame(&red, &blue, &paw, 0.5));
    assert_close(pixel(&closed, 64, 32, 20), [36, 16, 0], 2, "palm, closed");
    assert_close(pixel(&closed, 64, 2, 2), yellow, 2, "corner, closed");
    // Opening: the incoming picture shows through the palm.
    let opening = render(&renderer, size, transition_frame(&red, &blue, &paw, 0.65));
    assert_close(pixel(&opening, 64, 32, 21), [0, 0, 255], 2, "palm, opening");
    assert_close(pixel(&opening, 64, 2, 2), yellow, 2, "corner, opening");
    // Closing: the outgoing picture, darkening, in the hole.
    let closing = render(&renderer, size, transition_frame(&red, &blue, &paw, 0.2));
    let palm = pixel(&closing, 64, 32, 21);
    assert!(palm[0] > 60 && palm[0] < 250 && palm[2] < 5, "{palm:?}");
}

#[test]
fn a_transition_in_the_middle_is_between_both_pictures_for_zoom() {
    let Some(renderer) = test_renderer() else {
        return;
    };
    let red = solid(&renderer, [255, 0, 0]);
    let blue = solid(&renderer, [0, 0, 255]);
    let pixels = render(
        &renderer,
        (32, 18),
        transition_frame(&red, &blue, &Transition::zoom(), 0.5),
    );
    assert_close(pixel(&pixels, 32, 16, 9), [188, 0, 188], 2, "zoom at 0.5");
    assert_close(
        pixel(&pixels, 32, 1, 1),
        [188, 0, 188],
        2,
        "zoom at 0.5, corner",
    );
}

#[test]
fn a_finished_transition_or_none_draws_only_the_incoming_picture() {
    let Some(renderer) = test_renderer() else {
        return;
    };
    let red = solid(&renderer, [255, 0, 0]);
    let blue = solid(&renderer, [0, 0, 255]);
    let none = render(
        &renderer,
        (16, 9),
        Frame {
            from: Some(cover(&red)),
            to: Some(cover(&blue)),
            ..Default::default()
        },
    );
    assert_close(pixel(&none, 16, 8, 4), [0, 0, 255], 1, "no transition");
    // The first picture fades in from black.
    let fade = Transition::fade();
    let first = render(
        &renderer,
        (16, 9),
        Frame {
            to: Some(cover(&blue)),
            transition: Some(TransitionRun {
                transition: &fade,
                progress: 0.5,
            }),
            ..Default::default()
        },
    );
    assert_close(
        pixel(&first, 16, 8, 4),
        [0, 0, 188],
        2,
        "fade in from black",
    );
}

#[test]
fn fit_modes_place_a_portrait_picture_on_a_landscape_output() {
    let Some(renderer) = test_renderer() else {
        return;
    };
    let mut portrait = Vec::new();
    for _ in 0..(8 * 16) {
        portrait.extend_from_slice(&[0, 200, 0, 255]);
    }
    let portrait = MediaTexture::from_rgba(&renderer, 8, 16, &portrait);
    let size = (64u32, 36u32);
    let green = [0, 200, 0];
    let draw = |fit| {
        let mut layer = Layer::new(&portrait);
        layer.fit = fit;
        render(
            &renderer,
            size,
            Frame {
                to: Some(layer),
                ..Default::default()
            },
        )
    };

    let contain = draw(Fit::Contain);
    assert_close(pixel(&contain, 64, 32, 18), green, 1, "contain, center");
    assert_close(pixel(&contain, 64, 2, 18), [0, 0, 0], 0, "contain, bar");

    let cropped = draw(Fit::Cover);
    assert_close(pixel(&cropped, 64, 2, 18), green, 1, "cover, left edge");
    assert_close(pixel(&cropped, 64, 62, 2), green, 1, "cover, corner");

    // Behind the picture is the same color, dimmed (0.55 of its linear light).
    let blur = draw(Fit::Blur);
    assert_close(pixel(&blur, 64, 32, 18), green, 1, "blur, center");
    let dimmed = (((0.55f32 * srgb_to_linear(200.0 / 255.0)).powf(1.0 / 2.4) * 1.055 - 0.055)
        * 255.0)
        .round() as u8;
    let behind = pixel(&blur, 64, 2, 18);
    assert!(
        behind[0] == 0 && behind[2] == 0 && behind[1].abs_diff(dimmed) <= 2,
        "{behind:?} vs {dimmed}"
    );
}

fn srgb_to_linear(c: f32) -> f32 {
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

#[test]
fn transform_scales_and_moves_the_picture() {
    let Some(renderer) = test_renderer() else {
        return;
    };
    let white = solid(&renderer, [255, 255, 255]);
    let size = (40, 20);
    let draw = |transform| {
        let mut layer = Layer::new(&white);
        layer.fit = Fit::Contain;
        layer.transform = transform;
        render(
            &renderer,
            size,
            Frame {
                to: Some(layer),
                ..Default::default()
            },
        )
    };
    // The 4x4 picture is a square in the middle of the 40x20 output (x 10..30).
    let plain = draw(Transform::default());
    assert_close(pixel(&plain, 40, 20, 10), [255, 255, 255], 0, "center");
    assert_close(pixel(&plain, 40, 5, 10), [0, 0, 0], 0, "bar");
    // Half the size: x 15..25, y 5..15.
    let small = draw(Transform {
        scale: 0.5,
        offset: [0.0, 0.0],
    });
    assert_close(
        pixel(&small, 40, 20, 10),
        [255, 255, 255],
        0,
        "small, center",
    );
    assert_close(pixel(&small, 40, 12, 10), [0, 0, 0], 0, "small, left of it");
    // Moved a quarter of the width to the right: x 25..35.
    let moved = draw(Transform {
        scale: 0.5,
        offset: [0.25, 0.0],
    });
    assert_close(
        pixel(&moved, 40, 30, 10),
        [255, 255, 255],
        0,
        "moved, new place",
    );
    assert_close(pixel(&moved, 40, 20, 10), [0, 0, 0], 0, "moved, old place");
    // Enlarged to fill the whole output.
    let large = draw(Transform {
        scale: 2.0,
        offset: [0.0, 0.0],
    });
    assert_close(pixel(&large, 40, 2, 2), [255, 255, 255], 0, "large, corner");
}

#[test]
fn layer_opacity_shows_what_is_behind_the_picture() {
    let Some(renderer) = test_renderer() else {
        return;
    };
    let white = solid(&renderer, [255, 255, 255]);
    let mut layer = cover(&white);
    layer.opacity = 0.5;
    let pixels = render(
        &renderer,
        (16, 9),
        Frame {
            to: Some(layer),
            ..Default::default()
        },
    );
    assert_close(
        pixel(&pixels, 16, 8, 4),
        [188, 188, 188],
        2,
        "half white over black",
    );
}

#[test]
fn transparent_parts_of_a_picture_show_the_blurred_background() {
    let Some(renderer) = test_renderer() else {
        return;
    };
    // White with a transparent hole in the middle, premultiplied as
    // `load_image` uploads it. Through the hole shows the blurred
    // background, which is the white around it, blurred and dimmed: grey,
    // not the black under the hole.
    let mut pixels = Vec::new();
    for y in 0..8 {
        for x in 0..8 {
            let hole = (2..6).contains(&x) && (2..6).contains(&y);
            pixels.extend_from_slice(if hole {
                &[0, 0, 0, 0]
            } else {
                &[255, 255, 255, 255]
            });
        }
    }
    let picture = MediaTexture::from_rgba(&renderer, 8, 8, &pixels);
    let mut layer = Layer::new(&picture);
    layer.fit = Fit::Blur;
    let pixels = render(
        &renderer,
        (32, 18),
        Frame {
            to: Some(layer),
            ..Default::default()
        },
    );
    let through = pixel(&pixels, 32, 16, 9);
    assert!(
        through[0] > 40,
        "the hole shows the background: {through:?}"
    );
    assert_close(
        pixel(&pixels, 32, 16, 1),
        [255, 255, 255],
        0,
        "the picture itself",
    );
}

#[test]
fn half_transparent_colors_blend_in_linear_light() {
    let Some(renderer) = test_renderer() else {
        return;
    };
    // White at half alpha, premultiplied in linear light, over black: the
    // grey that is half the light of white.
    let mut pixels = [255, 255, 255, 128].repeat(16);
    crate::media::premultiply_rgba(&mut pixels);
    let picture = MediaTexture::from_rgba(&renderer, 4, 4, &pixels);
    let pixels = render(
        &renderer,
        (16, 9),
        Frame {
            to: Some(cover(&picture)),
            ..Default::default()
        },
    );
    assert_close(
        pixel(&pixels, 16, 8, 4),
        [188, 188, 188],
        2,
        "half white over black",
    );
}

#[test]
fn a_png_keeps_no_color_under_its_transparent_parts() {
    let dir = std::env::temp_dir().join(format!("fluxlide-png-alpha-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("cutout.png");
    // White but fully transparent on the left, red and half transparent on
    // the right.
    image::RgbaImage::from_fn(4, 2, |x, _| {
        if x < 2 {
            image::Rgba([255, 255, 255, 0])
        } else {
            image::Rgba([255, 0, 0, 128])
        }
    })
    .save(&path)
    .unwrap();
    let image = crate::media::decode_image(&path, (100, 100))
        .unwrap()
        .into_rgba8();
    std::fs::remove_dir_all(&dir).unwrap();
    assert_eq!(image.get_pixel(0, 0).0, [0, 0, 0, 0]);
    // Half the light of red, encoded back to sRGB.
    assert_eq!(image.get_pixel(3, 0).0, [188, 0, 0, 128]);
}

#[test]
fn yuv_pictures_are_converted_with_limited_range() {
    let Some(renderer) = test_renderer() else {
        return;
    };
    let (width, height) = (16u32, 8u32);
    let draw = |y: u8| {
        let video = MediaTexture::yuv420(&renderer, width, height);
        let mut frame = vec![y; (width * height) as usize];
        frame.extend(std::iter::repeat_n(128, (width * height / 2) as usize));
        video.write_yuv420(&renderer, &frame);
        render(
            &renderer,
            (width, height),
            Frame {
                to: Some(cover(&video)),
                ..Default::default()
            },
        )
    };
    assert_close(pixel(&draw(16), 16, 8, 4), [0, 0, 0], 1, "limited black");
    assert_close(
        pixel(&draw(235), 16, 8, 4),
        [255, 255, 255],
        1,
        "limited white",
    );
    assert_close(pixel(&draw(126), 16, 8, 4), [128, 128, 128], 2, "mid gray");
}

/// Pixels that differ from `background` (all other pixels are left out).
fn ink_bounds(pixels: &[u8], width: u32, background: [u8; 3]) -> Option<(u32, u32, u32, u32)> {
    let mut bounds: Option<(u32, u32, u32, u32)> = None;
    for (i, px) in pixels.chunks(4).enumerate() {
        if px[..3] != background {
            let (x, y) = (i as u32 % width, i as u32 / width);
            bounds = Some(match bounds {
                None => (x, y, x, y),
                Some((x0, y0, x1, y1)) => (x0.min(x), y0.min(y), x1.max(x), y1.max(y)),
            });
        }
    }
    bounds
}

fn sample_caption() -> Caption {
    Caption {
        title: "夕暮れの港".into(),
        author: "撮影 山田 太郎".into(),
        description:
            "朝の光が差し込む静かな港町で、漁を終えた船がゆっくりと戻ってくる様子を撮影しました。"
                .into(),
    }
}

#[test]
fn captions_appear_where_their_design_puts_them() {
    let Some(renderer) = test_renderer() else {
        return;
    };
    let black = solid(&renderer, [0, 0, 0]);
    let size = (960u32, 540u32);
    let draw = |design| {
        let style = CaptionStyle {
            design,
            ..Default::default()
        };
        let caption = Overlay::caption(&renderer, &sample_caption(), &style, size)
            .unwrap()
            .unwrap();
        render(
            &renderer,
            size,
            Frame {
                to: Some(cover(&black)),
                overlays: vec![OverlayRef {
                    overlay: &caption,
                    opacity: 1.0,
                    reveal: None,
                }],
                ..Default::default()
            },
        )
    };

    // Simple: ink only in the lower left, starting with the thin vertical
    // line at the margin (5% of the height). Text is at most 60% of the width.
    let (x0, y0, x1, y1) = ink_bounds(&draw(CaptionDesign::Simple), 960, [0, 0, 0]).unwrap();
    assert_eq!(x0, 27);
    assert!(
        x1 < 27 + 20 + 576 && y0 > 270 && y1 < 540 - 20,
        "{x0} {y0} {x1} {y1}"
    );

    // Plain: the same corner without the line, so the text starts at the margin.
    let (x0, y0, x1, y1) = ink_bounds(&draw(CaptionDesign::Plain), 960, [0, 0, 0]).unwrap();
    assert!(
        (27..27 + 8).contains(&x0) && x1 < 27 + 576 && y0 > 270 && y1 < 540 - 20,
        "{x0} {y0} {x1} {y1}"
    );

    // Band: over black the dark gradient shows nothing, so only the text
    // is ink; it starts at the margin and is at most 70% of the width wide.
    let band = draw(CaptionDesign::Band);
    let (x0, y0, x1, y1) = ink_bounds(&band, 960, [0, 0, 0]).unwrap();
    assert!(
        x0 >= 27 && x1 < 27 + 672 && y0 > 270 && y1 < 540 - 10,
        "{x0} {y0} {x1} {y1}"
    );

    // Card: a light plate at the bottom right, its corner at the margin.
    let card = draw(CaptionDesign::Card);
    let (x0, y0, x1, y1) = ink_bounds(&card, 960, [0, 0, 0]).unwrap();
    assert!(x0 > 480 && y0 > 270, "{x0} {y0}");
    assert_eq!((x1, y1), (960 - 27 - 1, 540 - 27 - 1));
    assert_close(
        pixel(&card, 960, x1 - 8, y1 - 8),
        [0xF5, 0xF6, 0xF8],
        12,
        "inside the card",
    );

    // None draws nothing, and an empty caption has no overlay.
    let style = CaptionStyle {
        design: CaptionDesign::None,
        ..Default::default()
    };
    assert!(
        Overlay::caption(&renderer, &sample_caption(), &style, size)
            .unwrap()
            .is_none()
    );
}

#[test]
fn a_caption_fades_with_its_opacity_and_is_hidden_at_zero() {
    let Some(renderer) = test_renderer() else {
        return;
    };
    let black = solid(&renderer, [0, 0, 0]);
    let size = (640u32, 360u32);
    let style = CaptionStyle {
        design: CaptionDesign::Simple,
        ..Default::default()
    };
    let caption = Overlay::caption(&renderer, &sample_caption(), &style, size)
        .unwrap()
        .unwrap();
    let brightest = |opacity: f32| {
        let pixels = render(
            &renderer,
            size,
            Frame {
                to: Some(cover(&black)),
                overlays: vec![OverlayRef {
                    overlay: &caption,
                    opacity,
                    reveal: None,
                }],
                ..Default::default()
            },
        );
        pixels.chunks(4).map(|px| px[0]).max().unwrap()
    };
    assert_eq!(brightest(0.0), 0);
    let half = brightest(0.5);
    let full = brightest(1.0);
    // The thin line is white at 0.9, so full opacity reaches about 240.
    assert!(full > 225, "{full}");
    assert!(half > 150 && half < 200, "{half}");
}

#[test]
fn a_watermark_stays_put_while_pictures_move() {
    let Some(renderer) = test_renderer() else {
        return;
    };
    let size = (320u32, 180u32);
    let red = solid(&renderer, [255, 0, 0]);
    let blue = solid(&renderer, [0, 0, 255]);
    let white = Arc::new(solid(&renderer, [255, 255, 255]));
    // An opaque white logo, so its pixels are exactly white wherever it is drawn.
    let mut mark = Watermark::image(white, Corner::TopRight, 1.0);
    mark.size = 0.2;
    let overlay = Overlay::watermark(&renderer, &mark, size).unwrap();
    let plan = crate::watermark::plan_watermark(&mark, size).unwrap();
    let crate::overlay::PlanPiece::Image { rect, .. } = &plan.pieces[0] else {
        panic!("an image watermark is one image");
    };
    let (left, top) = (rect.x as u32, rect.y as u32);
    let (right, bottom) = ((rect.x + rect.width) as u32, (rect.y + rect.height) as u32);

    for transition in [
        Transition::fade(),
        Transition::slide(Direction::Left),
        Transition::zoom(),
        Transition::wipe(Direction::Down),
    ] {
        for progress in [0.0, 0.25, 0.5, 0.75, 1.0] {
            let mut frame = transition_frame(&red, &blue, &transition, progress);
            frame.overlays.push(OverlayRef {
                overlay: &overlay,
                opacity: 1.0,
                reveal: None,
            });
            let pixels = render(&renderer, size, frame);
            let what = format!("{} at {progress}", transition.name());
            assert_close(pixel(&pixels, size.0, left, top), [255; 3], 0, &what);
            assert_close(
                pixel(&pixels, size.0, right - 1, bottom - 1),
                [255; 3],
                0,
                &what,
            );
            // Just outside is not the watermark.
            assert_ne!(
                pixel(&pixels, size.0, left - 2, top)[..3],
                [255; 3],
                "{what}"
            );
            assert_ne!(
                pixel(&pixels, size.0, left, bottom + 1)[..3],
                [255; 3],
                "{what}"
            );
        }
    }
}

#[test]
fn a_watermark_keeps_its_opacity_during_a_transition() {
    let Some(renderer) = test_renderer() else {
        return;
    };
    let size = (320u32, 180u32);
    let white = Arc::new(solid(&renderer, [255, 255, 255]));
    let mut mark = Watermark::image(white, Corner::BottomLeft, 0.5);
    mark.size = 0.2;
    let overlay = Overlay::watermark(&renderer, &mark, size).unwrap();
    let slide = Transition::slide(Direction::Left);
    // Black pictures all around, so half-transparent white is 188 everywhere
    // in the watermark and nothing else shows through.
    let black = solid(&renderer, [0, 0, 0]);
    for progress in [0.1, 0.5, 0.9] {
        let mut frame = transition_frame(&black, &black, &slide, progress);
        frame.overlays.push(OverlayRef {
            overlay: &overlay,
            opacity: 1.0,
            reveal: None,
        });
        let pixels = render(&renderer, size, frame);
        let inside = pixel(&pixels, size.0, 20, size.1 - 20);
        assert_close(inside, [188; 3], 2, &format!("at {progress}"));
    }
}

#[test]
fn a_text_watermark_is_drawn_in_its_corner() {
    let Some(renderer) = test_renderer() else {
        return;
    };
    let black = solid(&renderer, [0, 0, 0]);
    let size = (960u32, 540u32);
    for (corner, right, bottom) in [
        (Corner::TopLeft, false, false),
        (Corner::TopRight, true, false),
        (Corner::BottomLeft, false, true),
        (Corner::BottomRight, true, true),
    ] {
        let mark = Watermark::text("FLUXLIDE", corner, 1.0);
        let overlay = Overlay::watermark(&renderer, &mark, size).unwrap();
        let pixels = render(
            &renderer,
            size,
            Frame {
                to: Some(cover(&black)),
                overlays: vec![OverlayRef {
                    overlay: &overlay,
                    opacity: 1.0,
                    reveal: None,
                }],
                ..Default::default()
            },
        );
        let (x0, y0, x1, y1) = ink_bounds(&pixels, 960, [0, 0, 0]).unwrap();
        let margin = (540.0f32 * 0.035).round() as u32;
        assert_eq!(x0 < 480, !right, "{corner:?}: {x0}..{x1}");
        assert_eq!(y0 < 270, !bottom, "{corner:?}: {y0}..{y1}");
        // The frame touches the margin on its two sides.
        if right {
            assert_eq!(x1, 960 - margin - 1, "{corner:?}");
        } else {
            assert_eq!(x0, margin, "{corner:?}");
        }
        if bottom {
            assert_eq!(y1, 540 - margin - 1, "{corner:?}");
        } else {
            assert_eq!(y0, margin, "{corner:?}");
        }
    }
}

#[test]
fn an_overlay_made_for_another_size_is_refused() {
    let Some(renderer) = test_renderer() else {
        return;
    };
    let black = solid(&renderer, [0, 0, 0]);
    let overlay = Overlay::caption(
        &renderer,
        &sample_caption(),
        &CaptionStyle::default(),
        (640, 360),
    )
    .unwrap()
    .unwrap();
    let mut target = OffscreenTarget::new(&renderer, 320, 180).unwrap();
    let frame = Frame {
        to: Some(cover(&black)),
        overlays: vec![OverlayRef {
            overlay: &overlay,
            opacity: 1.0,
            reveal: None,
        }],
        ..Default::default()
    };
    let err = target.render(&renderer, &Scene::Frame(frame)).unwrap_err();
    assert!(matches!(err, EngineError::Scene(_)), "{err}");
}

const INVERT: &str = "
fn transition(uv: vec2<f32>, progress: f32) -> vec4<f32> {
    let outgoing = from_color(uv);
    let incoming = to_color(uv);
    // Shows the incoming picture inverted, scaled by fx.params.x.
    return vec4<f32>(vec3<f32>(1.0) - incoming.rgb * fx.params.x, 1.0) * progress + outgoing * (1.0 - progress);
}
";

#[test]
fn custom_wgsl_transitions_run_and_receive_their_parameters() {
    let Some(renderer) = test_renderer() else {
        return;
    };
    let red = solid(&renderer, [255, 0, 0]);
    let blue = solid(&renderer, [0, 0, 255]);
    let invert = Transition::from_wgsl(&renderer, "invert", INVERT)
        .expect("a valid shader")
        .with_params([1.0, 0.0, 0.0, 0.0]);
    assert_eq!(invert.name(), "invert");
    // At 1, the incoming blue is inverted to yellow (1 - (0, 0, 1)).
    let pixels = render(
        &renderer,
        (16, 9),
        transition_frame(&red, &blue, &invert, 1.0 - 1e-6),
    );
    assert_close(
        pixel(&pixels, 16, 8, 4),
        [255, 255, 0],
        1,
        "inverted incoming",
    );
    // With the parameter at 0 nothing is subtracted: white.
    let invert = invert.with_params([0.0; 4]);
    let pixels = render(
        &renderer,
        (16, 9),
        transition_frame(&red, &blue, &invert, 1.0 - 1e-6),
    );
    assert_close(pixel(&pixels, 16, 8, 4), [255, 255, 255], 1, "parameter 0");
}

#[test]
fn a_custom_transition_can_use_resolution_and_aspect() {
    let Some(renderer) = test_renderer() else {
        return;
    };
    let source = "
fn transition(uv: vec2<f32>, progress: f32) -> vec4<f32> {
    // Red channel: aspect ratio over 4. Green channel: width over 1000.
    return vec4<f32>(fx.aspect / 4.0, fx.resolution.x / 1000.0, progress, 1.0);
}
";
    let t = Transition::from_wgsl(&renderer, "values", source).unwrap();
    let black = solid(&renderer, [0, 0, 0]);
    let pixels = render(
        &renderer,
        (400, 200),
        transition_frame(&black, &black, &t, 0.5),
    );
    let px = pixel(&pixels, 400, 10, 10);
    // Colors are linear light here: 0.5 -> 188, 0.4 -> 168, 0.5 -> 188.
    assert_close(px, [188, 168, 188], 2, "values");
}

#[test]
fn broken_wgsl_fails_with_the_compilers_message_before_any_frame() {
    let Some(renderer) = test_renderer() else {
        return;
    };
    let cases = [
        // Not WGSL at all.
        (
            "syntax",
            "fn transition(uv: vec2<f32>, progress: f32) -> vec4<f32> { return vec4<f32>(1.0 }",
            "expected",
        ),
        // The required function is missing.
        ("missing", "fn something_else() {}", "transition"),
        // The function exists but returns the wrong type.
        (
            "wrong type",
            "fn transition(uv: vec2<f32>, progress: f32) -> f32 { return progress; }",
            "vec4<f32>",
        ),
        // An unknown name.
        (
            "unknown",
            "fn transition(uv: vec2<f32>, progress: f32) -> vec4<f32> { return nothing(uv); }",
            "nothing",
        ),
    ];
    for (name, source, expected) in cases {
        let err = match Transition::from_wgsl(&renderer, name, source) {
            Ok(_) => panic!("{name}: the shader was accepted"),
            Err(err) => err,
        };
        let EngineError::Shader {
            name: error_name,
            message,
        } = &err
        else {
            panic!("{name}: {err:?}");
        };
        assert_eq!(error_name, name);
        assert!(
            message.contains(expected),
            "{name}: the message does not mention {expected:?}: {message}"
        );
    }
}

#[test]
fn an_error_in_the_authors_code_points_at_the_authors_line() {
    let Some(renderer) = test_renderer() else {
        return;
    };
    let source = "// line 1\n// line 2\nfn transition(uv: vec2<f32>, progress: f32) -> vec4<f32> {\n    return missing_name;\n}\n";
    let err = Transition::from_wgsl(&renderer, "lines", source).unwrap_err();
    let message = err.to_string();
    assert!(
        message.contains(":4:") || message.contains("line 4"),
        "{message}"
    );
}

#[test]
fn the_built_in_shaders_compile() {
    let Some(renderer) = test_renderer() else {
        return;
    };
    let size = (8u32, 8u32);
    let gray = solid(&renderer, [128, 128, 128]);
    // Drawing builds each pipeline on the offscreen format.
    for transition in [
        Transition::fade(),
        Transition::slide(Direction::Right),
        Transition::zoom(),
        Transition::wipe(Direction::Up),
    ] {
        render(
            &renderer,
            size,
            transition_frame(&gray, &gray, &transition, 0.5),
        );
    }
}

#[test]
fn the_same_scene_gives_the_same_pixels_every_time() {
    let Some(renderer) = test_renderer() else {
        return;
    };
    let red = solid(&renderer, [255, 0, 0]);
    let blue = solid(&renderer, [0, 0, 255]);
    let slide = Transition::slide(Direction::Left);
    let caption = Overlay::caption(
        &renderer,
        &sample_caption(),
        &CaptionStyle::default(),
        (480, 270),
    )
    .unwrap()
    .unwrap();
    let draw = || {
        let mut frame = transition_frame(&red, &blue, &slide, 0.3);
        frame.overlays.push(OverlayRef {
            overlay: &caption,
            opacity: 0.8,
            reveal: None,
        });
        render(&renderer, (480, 270), frame)
    };
    assert!(draw() == draw());
}

#[test]
fn halves_are_drawn_left_to_right() {
    // Guards the texture orientation the other tests rely on.
    let Some(renderer) = test_renderer() else {
        return;
    };
    let picture = halves(&renderer, [255, 0, 0], [0, 0, 255]);
    let pixels = render(
        &renderer,
        (16, 8),
        Frame {
            to: Some(cover(&picture)),
            ..Default::default()
        },
    );
    assert_close(pixel(&pixels, 16, 2, 4), [255, 0, 0], 1, "left");
    assert_close(pixel(&pixels, 16, 13, 4), [0, 0, 255], 1, "right");
}
