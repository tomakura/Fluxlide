//! Exports a generated slideshow and measures it: where the time goes, and
//! how far a frame of the file is from the same frame of live playback.
//! Usage:
//!
//! ```text
//! cargo run --release -p fluxlide-engine --example export_bench -- <work dir> [encoder]
//! ```
//!
//! The photos and videos are generated with `image` and `ffmpeg`, so no
//! material is needed. Run it under `/usr/bin/time -v` (Linux) or
//! `/usr/bin/time -l` (macOS) to see the CPU time and the peak memory of the
//! whole export, `ffmpeg` included.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Instant;

use fluxlide_engine::export::{ExportSettings, export_slideshow, pick_video_encoder};
use fluxlide_engine::media::video::Ffmpeg;
use fluxlide_engine::{
    Caption, CaptionStyle, Corner, Direction, Fit, OffscreenTarget, Renderer, Scene, Slideshow,
    SlideshowConfig, Transition, Watermark,
};

const SIZE: (u32, u32) = (1920, 1080);
const FPS: u32 = 30;
/// A photo's time on screen, so that the reference time falls in the
/// middle of the first photo, with no transition running.
const IMAGE_SECS: f64 = 5.0;
const REFERENCE_SECS: f64 = 2.5;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args_os().skip(1);
    let dir: PathBuf = args
        .next()
        .ok_or("usage: export_bench <work dir> [encoder]")?
        .into();
    let encoder = args.next().map(|name| name.to_string_lossy().into_owned());
    std::fs::create_dir_all(&dir)?;
    let renderer = Arc::new(Renderer::new_blocking()?);
    let ffmpeg = Ffmpeg::locate()?;
    println!("GPU: {:?}", renderer.info());
    println!("ffmpeg: {}", ffmpeg.version()?);

    let items = make_material(&ffmpeg, &dir)?;
    let settings = ExportSettings {
        size: SIZE,
        fps: FPS,
        video_encoder: Some(match encoder {
            Some(name) => name,
            None => pick_video_encoder(&ffmpeg)?,
        }),
        ..Default::default()
    };

    // The frame live playback shows at the reference time.
    let reference = dir.join("reference.png");
    {
        let mut show = slideshow(&renderer, &items);
        show.play_once();
        // Start at 0 as the export does: the first photo fades in from there.
        for time in [0.0, REFERENCE_SECS] {
            loop {
                show.update(time);
                if !show.is_waiting() {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
        }
        let mut target = OffscreenTarget::new(&renderer, SIZE.0, SIZE.1)?;
        let pixels = target.render_rgba(&renderer, &Scene::Frame(show.frame()))?;
        image::RgbaImage::from_raw(SIZE.0, SIZE.1, pixels)
            .ok_or("bad frame")?
            .save(&reference)?;
    }

    let out = dir.join("export.mp4");
    let mut show = slideshow(&renderer, &items);
    let started = Instant::now();
    let mut next_report = 0.0;
    let report = export_slideshow(
        &renderer,
        &ffmpeg,
        &mut show,
        &settings,
        &out,
        &AtomicBool::new(false),
        |progress| {
            if progress.media_secs >= next_report {
                println!(
                    "  {:5.1} s of video after {:5.1} s",
                    progress.media_secs,
                    started.elapsed().as_secs_f64()
                );
                next_report += 5.0;
            }
        },
    )?;
    println!("{report:#?}");
    println!(
        "speed: {:.2}x real time, {:.1} ms per frame",
        report.media_secs / report.wall_secs,
        report.wall_secs * 1000.0 / report.frames as f64
    );
    println!(
        "file: {:.1} MB",
        std::fs::metadata(&out)?.len() as f64 / 1_000_000.0
    );

    // The same frame, decoded from the file.
    let frame_index = (REFERENCE_SECS * f64::from(FPS)).round() as u32;
    let exported = dir.join("exported.png");
    run(Command::new(ffmpeg.ffmpeg_path())
        .args(["-v", "error", "-y", "-i"])
        .arg(&out)
        .args(["-vf", &format!("select=eq(n\\,{frame_index})")])
        .args(["-frames:v", "1"])
        .arg(&exported))?;
    for metric in ["psnr", "ssim"] {
        let output = Command::new(ffmpeg.ffmpeg_path())
            .args(["-hide_banner", "-i"])
            .arg(&exported)
            .arg("-i")
            .arg(&reference)
            .args(["-lavfi", metric, "-f", "null", "-"])
            .output()?;
        let log = String::from_utf8_lossy(&output.stderr);
        if let Some(line) = log
            .lines()
            .find(|line| line.contains("average:") || line.contains("All:"))
        {
            println!("{metric}: {}", line.split("] ").nth(1).unwrap_or(line));
        }
    }
    Ok(())
}

fn slideshow(renderer: &Arc<Renderer>, items: &[PathBuf]) -> Slideshow {
    let config = SlideshowConfig {
        image_secs: IMAGE_SECS,
        fade_secs: 1.0,
        fit: Fit::Blur,
    };
    let mut show = Slideshow::new(renderer.clone(), items.to_vec(), SIZE, config);
    let captions = items
        .iter()
        .enumerate()
        .map(|(index, _)| Caption {
            title: format!("夕暮れの港 {}", index + 1),
            author: "撮影　山田 太郎".into(),
            description: "朝の光が差し込む静かな港町で、漁を終えた船がゆっくりと戻ってくる様子を撮影しました。".into(),
        })
        .collect();
    show.set_captions(captions, CaptionStyle::default());
    show.set_transition(Transition::slide(Direction::Left));
    show.set_watermark(Some(Watermark::text("FLUXLIDE", Corner::TopRight, 0.6)))
        .expect("watermark");
    show
}

/// Photo, video, photo, video, photo: 4032x3024 JPEGs and 1080p30 videos
/// of 6 s with sound.
fn make_material(ffmpeg: &Ffmpeg, dir: &Path) -> Result<Vec<PathBuf>, Box<dyn std::error::Error>> {
    let mut items = Vec::new();
    for index in 0..5 {
        if index % 2 == 0 {
            let path = dir.join(format!("photo{index}.jpg"));
            if !path.exists() {
                photo(index).save(&path)?;
            }
            items.push(path);
        } else {
            let path = dir.join(format!("video{index}.mp4"));
            if !path.exists() {
                let encoder = if pick_video_encoder(ffmpeg)? == "libx264" {
                    "libx264"
                } else {
                    "mpeg4"
                };
                run(Command::new(ffmpeg.ffmpeg_path())
                    .args(["-v", "error", "-y", "-f", "lavfi", "-i"])
                    .arg("testsrc2=size=1920x1080:rate=30:duration=6")
                    .args(["-f", "lavfi", "-i", "sine=frequency=330:duration=6"])
                    .args(["-pix_fmt", "yuv420p", "-c:v", encoder, "-b:v", "8M"])
                    .args(["-c:a", "aac", "-shortest"])
                    .arg(&path))?;
            }
            items.push(path);
        }
    }
    Ok(items)
}

/// A photo-like picture: smooth gradients with fine grain, which is hard
/// on an encoder in the way real photos are.
fn photo(seed: u32) -> image::RgbImage {
    let (width, height) = (4032u32, 3024u32);
    let mut state = 0x9e37_79b9u32.wrapping_mul(seed + 1);
    image::RgbImage::from_fn(width, height, |x, y| {
        state ^= state << 13;
        state ^= state >> 17;
        state ^= state << 5;
        let grain = (state % 17) as f32 - 8.0;
        let fx = x as f32 / width as f32;
        let fy = y as f32 / height as f32;
        let hill = ((fx * 9.0 + seed as f32).sin() * 0.05 + 0.65 < fy) as u8 as f32;
        let r = 40.0 + 200.0 * fy * (1.0 - hill) + 20.0 * hill + grain;
        let g = 60.0 + 120.0 * (1.0 - fy) * (1.0 - hill) + 25.0 * hill + grain;
        let b = 120.0 + 100.0 * (1.0 - fy) * (1.0 - hill) + 30.0 * hill + grain;
        image::Rgb([r, g, b].map(|v| v.clamp(0.0, 255.0) as u8))
    })
}

fn run(command: &mut Command) -> Result<(), Box<dyn std::error::Error>> {
    let status = command.status()?;
    if !status.success() {
        return Err(format!("{command:?} failed: {status}").into());
    }
    Ok(())
}
