//! Writes sample frames (transitions, captions, watermarks, fit modes, the
//! title card) as
//! PNG files so they can be looked at. Usage:
//!
//! ```text
//! cargo run --release -p fluxlide-engine --example render_samples -- <output dir>
//! ```
//!
//! The pictures are generated, so no photo files are needed.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use fluxlide_engine::{
    Caption, CaptionDesign, CaptionStyle, Corner, Direction, Fit, Frame, Layer, MediaTexture,
    OffscreenTarget, Overlay, OverlayRef, Renderer, Scene, Transform, Transition, TransitionRun,
    Watermark,
};

const SIZE: (u32, u32) = (1920, 1080);

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let out_dir: PathBuf = std::env::args_os()
        .nth(1)
        .ok_or("usage: render_samples <output dir>")?
        .into();
    std::fs::create_dir_all(&out_dir)?;
    let renderer = Renderer::new_blocking()?;
    println!("GPU: {:?}", renderer.info());

    let sunset = photo(&renderer, Scene3::Sunset, 1800, 1200);
    let sea = photo(&renderer, Scene3::Sea, 1800, 1200);
    let fog = photo(&renderer, Scene3::Fog, 1800, 1200);
    let portrait = photo(&renderer, Scene3::Sunset, 800, 1200);

    transitions(&renderer, &out_dir, &sunset, &sea)?;
    captions(&renderer, &out_dir, &sunset, &fog)?;
    watermarks(&renderer, &out_dir, &sunset, &sea, &fog)?;
    fits(&renderer, &out_dir, &portrait, &sunset)?;
    title_card(&renderer, &out_dir)?;
    println!("wrote samples to {}", out_dir.display());
    Ok(())
}

fn render(
    renderer: &Renderer,
    target: &mut OffscreenTarget,
    frame: Frame<'_>,
) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    Ok(target.render_rgba(renderer, &Scene::Frame(frame))?)
}

fn save(path: &Path, size: (u32, u32), pixels: Vec<u8>) -> Result<(), Box<dyn std::error::Error>> {
    image::RgbaImage::from_raw(size.0, size.1, pixels)
        .ok_or("pixel buffer has the wrong size")?
        .save(path)?;
    Ok(())
}

/// Every transition at six steps, as one contact sheet each.
fn transitions(
    renderer: &Renderer,
    out_dir: &Path,
    from: &MediaTexture,
    to: &MediaTexture,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut target = OffscreenTarget::new(renderer, 640, 360)?;
    let all = [
        ("fade", Transition::fade()),
        ("slide-left", Transition::slide(Direction::Left)),
        ("slide-up", Transition::slide(Direction::Up)),
        ("zoom", Transition::zoom()),
        ("wipe-right", Transition::wipe(Direction::Right)),
        ("wipe-down", Transition::wipe(Direction::Down)),
    ];
    for (name, transition) in all {
        let steps = [0.0, 0.2, 0.4, 0.6, 0.8, 1.0f32];
        let mut sheet = image::RgbaImage::new(640 * 3, 360 * 2);
        for (i, progress) in steps.into_iter().enumerate() {
            let frame = Frame {
                from: Some(Layer::new(from)),
                to: Some(Layer::new(to)),
                transition: Some(TransitionRun {
                    transition: &transition,
                    progress,
                }),
                overlays: Vec::new(),
            };
            let pixels = render(renderer, &mut target, frame)?;
            let tile = image::RgbaImage::from_raw(640, 360, pixels).unwrap();
            image::imageops::replace(
                &mut sheet,
                &tile,
                i64::from(640 * (i as u32 % 3)),
                i64::from(360 * (i as u32 / 3)),
            );
        }
        sheet.save(out_dir.join(format!("transition-{name}.png")))?;
    }
    Ok(())
}

fn caption() -> Caption {
    Caption {
        title: "夕暮れの港".into(),
        author: "撮影　山田 太郎".into(),
        description:
            "朝の光が差し込む静かな港町で、漁を終えた船がゆっくりと戻ってくる様子を撮影しました。\
                      潮の香りと、かもめの声が印象的な一日でした。"
                .into(),
    }
}

/// Each caption design on a dark and on a bright picture, plus a small
/// and a 4K version to compare sizes.
fn captions(
    renderer: &Renderer,
    out_dir: &Path,
    dark: &MediaTexture,
    bright: &MediaTexture,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut target = OffscreenTarget::new(renderer, SIZE.0, SIZE.1)?;
    for (design, name) in [
        (CaptionDesign::Band, "band"),
        (CaptionDesign::Simple, "simple"),
        (CaptionDesign::Card, "card"),
    ] {
        let style = CaptionStyle {
            design,
            ..Default::default()
        };
        let overlay = Overlay::caption(renderer, &caption(), &style, SIZE)?.unwrap();
        for (photo, tag) in [(dark, "dark"), (bright, "bright")] {
            let frame = Frame {
                to: Some(Layer::new(photo)),
                overlays: vec![OverlayRef {
                    overlay: &overlay,
                    opacity: 1.0,
                }],
                ..Default::default()
            };
            let pixels = render(renderer, &mut target, frame)?;
            save(
                &out_dir.join(format!("caption-{name}-{tag}.png")),
                SIZE,
                pixels,
            )?;
        }
    }

    // Short caption, English and Japanese mixed.
    let short = Caption {
        title: "Harbor at Dusk / 港の夕暮れ".into(),
        author: "Taro Yamada".into(),
        description: String::new(),
    };
    let overlay = Overlay::caption(renderer, &short, &CaptionStyle::default(), SIZE)?.unwrap();
    let frame = Frame {
        to: Some(Layer::new(dark)),
        overlays: vec![OverlayRef {
            overlay: &overlay,
            opacity: 1.0,
        }],
        ..Default::default()
    };
    let pixels = render(renderer, &mut target, frame)?;
    save(&out_dir.join("caption-band-short.png"), SIZE, pixels)?;

    // The same caption at 960x540 and 3840x2160, cropped to the lower left
    // quarter, to compare how the design scales.
    for (width, height) in [(960u32, 540u32), (3840, 2160)] {
        let mut target = OffscreenTarget::new(renderer, width, height)?;
        let overlay = Overlay::caption(
            renderer,
            &caption(),
            &CaptionStyle::default(),
            (width, height),
        )?
        .unwrap();
        let frame = Frame {
            to: Some(Layer::new(dark)),
            overlays: vec![OverlayRef {
                overlay: &overlay,
                opacity: 1.0,
            }],
            ..Default::default()
        };
        let pixels = render(renderer, &mut target, frame)?;
        save(
            &out_dir.join(format!("caption-band-{width}x{height}.png")),
            (width, height),
            pixels,
        )?;
    }
    Ok(())
}

/// Text and image watermarks in the four corners, and one kept still
/// while a slide transition is half way.
fn watermarks(
    renderer: &Renderer,
    out_dir: &Path,
    a: &MediaTexture,
    b: &MediaTexture,
    bright: &MediaTexture,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut target = OffscreenTarget::new(renderer, SIZE.0, SIZE.1)?;
    let logo = Arc::new(logo(renderer));
    let corners = [
        (Corner::TopRight, "top-right"),
        (Corner::TopLeft, "top-left"),
        (Corner::BottomLeft, "bottom-left"),
        (Corner::BottomRight, "bottom-right"),
    ];
    for (corner, name) in corners {
        let text = Watermark::text("FLUXLIDE", corner, 0.7);
        let overlay = Overlay::watermark(renderer, &text, SIZE)?;
        for (photo, tag) in [(a, "dark"), (bright, "bright")] {
            let frame = Frame {
                to: Some(Layer::new(photo)),
                overlays: vec![OverlayRef {
                    overlay: &overlay,
                    opacity: 1.0,
                }],
                ..Default::default()
            };
            let pixels = render(renderer, &mut target, frame)?;
            save(
                &out_dir.join(format!("watermark-text-{name}-{tag}.png")),
                SIZE,
                pixels,
            )?;
        }
        let image = Watermark::image(logo.clone(), corner, 0.6);
        let overlay = Overlay::watermark(renderer, &image, SIZE)?;
        let frame = Frame {
            to: Some(Layer::new(a)),
            overlays: vec![OverlayRef {
                overlay: &overlay,
                opacity: 1.0,
            }],
            ..Default::default()
        };
        let pixels = render(renderer, &mut target, frame)?;
        save(
            &out_dir.join(format!("watermark-image-{name}.png")),
            SIZE,
            pixels,
        )?;
    }

    // Watermark and caption stay put while the pictures slide.
    let mark = Overlay::watermark(
        renderer,
        &Watermark::text("FLUXLIDE", Corner::TopRight, 0.8),
        SIZE,
    )?;
    let note = Overlay::caption(renderer, &caption(), &CaptionStyle::default(), SIZE)?.unwrap();
    let slide = Transition::slide(Direction::Left);
    let frame = Frame {
        from: Some(Layer::new(a)),
        to: Some(Layer::new(b)),
        transition: Some(TransitionRun {
            transition: &slide,
            progress: 0.5,
        }),
        overlays: vec![
            OverlayRef {
                overlay: &note,
                opacity: 1.0,
            },
            OverlayRef {
                overlay: &mark,
                opacity: 1.0,
            },
        ],
    };
    let pixels = render(renderer, &mut target, frame)?;
    save(&out_dir.join("watermark-during-slide.png"), SIZE, pixels)?;
    Ok(())
}

/// A portrait photo on a 16:9 output in each fit mode, then a zoomed layer.
fn fits(
    renderer: &Renderer,
    out_dir: &Path,
    portrait: &MediaTexture,
    landscape: &MediaTexture,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut target = OffscreenTarget::new(renderer, 960, 540)?;
    for (fit, name) in [
        (Fit::Contain, "contain"),
        (Fit::Cover, "cover"),
        (Fit::Blur, "blur"),
    ] {
        let mut layer = Layer::new(portrait);
        layer.fit = fit;
        let pixels = render(
            renderer,
            &mut target,
            Frame {
                to: Some(layer),
                ..Default::default()
            },
        )?;
        save(&out_dir.join(format!("fit-{name}.png")), (960, 540), pixels)?;
    }
    // Ken Burns: 1.5 times larger, shifted towards the lower right.
    let mut layer = Layer::new(landscape);
    layer.transform = Transform {
        scale: 1.5,
        offset: [-0.1, -0.05],
    };
    let pixels = render(
        renderer,
        &mut target,
        Frame {
            to: Some(layer),
            ..Default::default()
        },
    )?;
    save(&out_dir.join("transform-zoom.png"), (960, 540), pixels)?;
    Ok(())
}

enum Scene3 {
    Sunset,
    Sea,
    Fog,
}

/// A generated picture with smooth gradients, a light source and fine
/// detail, so banding, blur and text legibility show up.
/// The screen before and after a presentation.
fn title_card(renderer: &Renderer, out_dir: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let mut target = OffscreenTarget::new(renderer, SIZE.0, SIZE.1)?;
    let card = Overlay::title_card(renderer, "港の写真展 2026", SIZE)?;
    let frame = Frame {
        from: None,
        to: None,
        transition: None,
        overlays: vec![OverlayRef {
            overlay: &card,
            opacity: 1.0,
        }],
    };
    let pixels = render(renderer, &mut target, frame)?;
    save(&out_dir.join("title-card.png"), SIZE, pixels)?;
    Ok(())
}

fn photo(renderer: &Renderer, scene: Scene3, width: u32, height: u32) -> MediaTexture {
    let mut pixels = Vec::with_capacity((width * height * 4) as usize);
    for y in 0..height {
        for x in 0..width {
            let (u, v) = (x as f32 / width as f32, y as f32 / height as f32);
            let rgb = match scene {
                Scene3::Sunset => sunset(u, v),
                Scene3::Sea => sea(u, v),
                Scene3::Fog => fog(u, v),
            };
            // Fine grain, like sensor noise.
            let grain = (hash(x, y) - 0.5) * 0.02;
            for c in rgb {
                pixels.push(((c + grain).clamp(0.0, 1.0) * 255.0).round() as u8);
            }
            pixels.push(255);
        }
    }
    MediaTexture::from_rgba(renderer, width, height, &pixels)
}

fn hash(x: u32, y: u32) -> f32 {
    let mut h = x
        .wrapping_mul(374_761_393)
        .wrapping_add(y.wrapping_mul(668_265_263));
    h = (h ^ (h >> 13)).wrapping_mul(1_274_126_177);
    ((h ^ (h >> 16)) & 0xffff) as f32 / 65535.0
}

fn mix3(a: [f32; 3], b: [f32; 3], t: f32) -> [f32; 3] {
    let t = t.clamp(0.0, 1.0);
    [
        a[0] + (b[0] - a[0]) * t,
        a[1] + (b[1] - a[1]) * t,
        a[2] + (b[2] - a[2]) * t,
    ]
}

fn sunset(u: f32, v: f32) -> [f32; 3] {
    let horizon = 0.62;
    let mut rgb = mix3(
        [0.12, 0.16, 0.38],
        [0.98, 0.62, 0.35],
        (v / horizon).powf(1.6),
    );
    // Sun and glow.
    let d = ((u - 0.62).powi(2) + ((v - 0.5) * 0.7).powi(2)).sqrt();
    rgb = mix3(
        rgb,
        [1.0, 0.93, 0.7],
        (1.0 - d * 5.0).clamp(0.0, 1.0).powi(2),
    );
    if d < 0.035 {
        rgb = [1.0, 0.98, 0.9];
    }
    // Hills.
    let hill = horizon + 0.05 * (u * 7.0).sin() + 0.03 * (u * 19.0 + 1.0).sin();
    if v > hill {
        let t = (v - hill) / (1.0 - hill);
        rgb = mix3([0.12, 0.09, 0.12], [0.02, 0.03, 0.04], t);
    }
    rgb
}

fn sea(u: f32, v: f32) -> [f32; 3] {
    let horizon = 0.45;
    if v < horizon {
        return mix3([0.35, 0.62, 0.9], [0.85, 0.93, 0.98], v / horizon);
    }
    let wave = 0.03 * ((v * 90.0 + u * 8.0).sin() + (v * 40.0 - u * 5.0).sin());
    mix3(
        [0.05, 0.42, 0.5],
        [0.0, 0.15, 0.25],
        (v - horizon) / (1.0 - horizon) + wave,
    )
}

fn fog(u: f32, v: f32) -> [f32; 3] {
    let base = mix3([0.82, 0.84, 0.88], [0.95, 0.95, 0.94], v);
    let tree = ((u * 23.0).sin() * 0.5 + 0.5) * (1.0 - v).powi(2) * 0.25;
    [base[0] - tree, base[1] - tree, base[2] - tree * 0.8]
}

/// A small logo with transparency: a coral rounded plate with a white bar.
fn logo(renderer: &Renderer) -> MediaTexture {
    let (w, h) = (240u32, 80u32);
    let mut pixels = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            let (dx, dy) = (
                (x as f32 - 120.0).abs() - 100.0,
                (y as f32 - 40.0).abs() - 20.0,
            );
            let d = dx.max(0.0).hypot(dy.max(0.0)) + dx.max(dy).min(0.0) - 18.0;
            let alpha = (0.5 - d).clamp(0.0, 1.0);
            let bar = (60..180).contains(&x) && (34..46).contains(&y);
            let rgb = if bar {
                [255, 255, 255]
            } else {
                [0xE8, 0x55, 0x3A]
            };
            pixels.extend_from_slice(&[rgb[0], rgb[1], rgb[2], (alpha * 255.0).round() as u8]);
        }
    }
    fluxlide_engine::media::premultiply_rgba(&mut pixels);
    MediaTexture::from_rgba(renderer, w, h, &pixels)
}
