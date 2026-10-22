//! Plays a looping slideshow for a long time, faster than real time, to see
//! whether memory or anything else grows with the hours a signage display
//! runs. Usage:
//!
//! ```text
//! cargo run --release -p fluxlide-engine --example soak -- <work dir> [hours]
//! ```
//!
//! The show (photos, a video with sound, captions, a watermark and BGM) is
//! generated, so no material is needed. Each frame is drawn off screen as on
//! the presentation window, and the sound is mixed as the audio thread would.
//! The clock moves one frame at a time, waiting for loading and decoding as
//! an export does. The memory of the process (and of it, what Rust
//! allocated) is printed every simulated ten minutes on Linux and Windows,
//! or every `SOAK_REPORT_SECS`. `SOAK_FILES` and `SOAK_PLAIN` leave parts of
//! the show out, to find where a growth comes from.

use std::alloc::{GlobalAlloc, Layout, System};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use fluxlide_engine::audio::OfflineMixer;
use fluxlide_engine::bgm::{Bgm, BgmConfig, BgmSource};
use fluxlide_engine::media::video::Ffmpeg;
use fluxlide_engine::show::{
    Asset, AssetId, AssetKind, AssetMeta, AssetState, ShowDefinition, Slide, SlideId,
    WatermarkSource, WatermarkSpec,
};
use fluxlide_engine::{Corner, OffscreenTarget, Renderer, Scene, Slideshow};

/// Counts the bytes Rust has allocated, to tell them apart from the rest of
/// the process (the GPU driver above all).
struct Counting;

static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let live = LIVE.fetch_add(layout.size(), Ordering::Relaxed) + layout.size();
        PEAK.fetch_max(live, Ordering::Relaxed);
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        LIVE.fetch_sub(layout.size(), Ordering::Relaxed);
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

const SIZE: (u32, u32) = (1280, 720);
const FPS: u32 = 30;
const SAMPLE_RATE: u32 = 48_000;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let dir: PathBuf = args.next().ok_or("usage: soak <work dir> [hours]")?.into();
    let hours: f64 = args.next().map(|h| h.parse()).transpose()?.unwrap_or(2.0);
    std::fs::create_dir_all(&dir)?;
    let renderer = Arc::new(Renderer::new_blocking()?);
    let ffmpeg = Ffmpeg::locate()?;
    println!("GPU: {:?}", renderer.info());

    let definition = make_show(&ffmpeg, &dir)?;
    let mut show = Slideshow::new(renderer.clone(), definition, dir.clone(), SIZE);
    let mut mixer = OfflineMixer::export(SAMPLE_RATE);
    show.set_audio(mixer.mixer());
    let bgm = Bgm::start(
        ffmpeg.clone(),
        &mixer.mixer(),
        &BgmSource::Files(vec![dir.join("song.wav")]),
        BgmConfig::default(),
    )?;
    show.set_ducker(bgm.ducker());
    let mut target = OffscreenTarget::new(&renderer, SIZE.0, SIZE.1)?;

    let started = Instant::now();
    let total_frames = (hours * 3600.0 * f64::from(FPS)) as u64;
    let samples_per_frame = (SAMPLE_RATE / FPS) as usize;
    let report_every: f64 = std::env::var("SOAK_REPORT_SECS")
        .ok()
        .and_then(|secs| secs.parse().ok())
        .unwrap_or(600.0);
    let mut next_report = 0.0;
    for frame in 0..total_frames {
        let time = frame as f64 / f64::from(FPS);
        loop {
            show.update(time);
            if !show.is_waiting() {
                break;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        target.render(&renderer, &Scene::Frame(show.frame()))?;
        while !mixer.has_samples_for(samples_per_frame) {
            std::thread::sleep(Duration::from_millis(1));
        }
        mixer.render(samples_per_frame, time);
        if time >= next_report {
            let stats = show.stats();
            println!(
                "{:6.1} min played in {:6.1} min: memory {}, Rust heap {} MB (peak {} MB), transitions {}, failed {}, late {}",
                time / 60.0,
                started.elapsed().as_secs_f64() / 60.0,
                memory().map_or("?".into(), |(used, committed)| format!(
                    "{} MB (committed {} MB)",
                    used / 1_000_000,
                    committed / 1_000_000
                )),
                LIVE.load(Ordering::Relaxed) / 1_000_000,
                PEAK.swap(0, Ordering::Relaxed) / 1_000_000,
                stats.transitions,
                stats.failed_items,
                stats.late_transitions,
            );
            next_report += report_every;
        }
    }
    Ok(())
}

/// A photo, a video with sound and another photo, looping, with captions
/// and a watermark; a song for the BGM.
fn make_show(ffmpeg: &Ffmpeg, dir: &Path) -> Result<ShowDefinition, Box<dyn std::error::Error>> {
    let files = ["photo1.jpg", "clip.mp4", "photo2.jpg"];
    for (index, file) in files.iter().enumerate() {
        let path = dir.join(file);
        if path.exists() {
            continue;
        }
        if file.ends_with(".jpg") {
            image::RgbImage::from_fn(4032, 3024, |x, y| {
                image::Rgb([(x / 16 + index as u32 * 60) as u8, (y / 12) as u8, 120])
            })
            .save(&path)?;
        } else {
            run(Command::new(ffmpeg.ffmpeg_path())
                .args(["-v", "error", "-y", "-f", "lavfi", "-i"])
                .arg("testsrc2=size=1280x720:rate=30:duration=5")
                .args(["-f", "lavfi", "-i", "sine=frequency=330:duration=5"])
                .args(["-pix_fmt", "yuv420p", "-c:v", "mpeg4", "-q:v", "4"])
                .args(["-c:a", "aac", "-shortest"])
                .arg(&path))?;
        }
    }
    let song = dir.join("song.wav");
    if !song.exists() {
        run(Command::new(ffmpeg.ffmpeg_path())
            .args(["-v", "error", "-y", "-f", "lavfi", "-i"])
            .arg("sine=frequency=440:duration=40")
            .arg(&song))?;
    }

    let mut show = ShowDefinition::default();
    // SOAK_FILES=photo1.jpg,photo2.jpg plays only those, to find which kind
    // of slide a growth comes from.
    let chosen = std::env::var("SOAK_FILES").ok();
    for (index, file) in files.iter().enumerate() {
        if chosen
            .as_ref()
            .is_some_and(|chosen| !chosen.split(',').any(|name| name == *file))
        {
            continue;
        }
        let id = index as u64 + 1;
        let video = file.ends_with(".mp4");
        show.assets.push(Asset {
            id: AssetId(id),
            kind: if video {
                AssetKind::Video
            } else {
                AssetKind::Photo
            },
            file: file.to_string(),
            original_name: file.to_string(),
            state: AssetState::Ready,
            meta: AssetMeta {
                title: format!("長時間の再生 {}", index + 1),
                author: "Fluxlide".into(),
                description: String::new(),
            },
            size: None,
            duration_ms: video.then_some(5000),
            has_audio: video,
        });
        show.slides.push(Slide::new(SlideId(id), AssetId(id)));
    }
    show.look.photo_ms = 3000;
    // SOAK_PLAIN=fit,caption,watermark leaves those out, for the same reason.
    let plain = std::env::var("SOAK_PLAIN").unwrap_or_default();
    if plain.contains("fit") {
        show.look.fit = fluxlide_engine::Fit::Contain;
    }
    if plain.contains("caption") {
        for slide in &mut show.slides {
            slide.show_caption = false;
        }
    }
    if !plain.contains("watermark") {
        show.look.watermark = Some(WatermarkSpec {
            content: WatermarkSource::Text {
                text: "FLUXLIDE".into(),
            },
            corner: Corner::TopRight,
            opacity: 0.6,
            size: 0.045,
        });
    }
    Ok(show)
}

/// The memory of this process in use and committed: the working set and
/// private bytes (Windows), or the resident set twice (Linux).
fn memory() -> Option<(u64, u64)> {
    #[cfg(target_os = "linux")]
    {
        let status = std::fs::read_to_string("/proc/self/status").ok()?;
        let line = status.lines().find(|line| line.starts_with("VmRSS:"))?;
        let kb: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
        Some((kb * 1000, kb * 1000))
    }
    #[cfg(target_os = "windows")]
    {
        let output = Command::new("powershell")
            .args(["-NoProfile", "-Command"])
            .arg(format!(
                "$p = Get-Process -Id {}; \"$($p.WorkingSet64) $($p.PrivateMemorySize64)\"",
                std::process::id()
            ))
            .output()
            .ok()?;
        let text = String::from_utf8_lossy(&output.stdout);
        let mut numbers = text.split_whitespace().map(|n| n.parse().ok());
        Some((numbers.next()??, numbers.next()??))
    }
    #[cfg(not(any(target_os = "linux", target_os = "windows")))]
    {
        None
    }
}

fn run(command: &mut Command) -> Result<(), Box<dyn std::error::Error>> {
    let status = command.status()?;
    if !status.success() {
        return Err(format!("{command:?} failed: {status}").into());
    }
    Ok(())
}
