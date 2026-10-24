//! Writing a slideshow to a video file.
//!
//! The slideshow is played once on a clock that advances by exactly one
//! frame at a time instead of following the wall clock. Each frame is drawn
//! by the same [`Compositor`](crate::Compositor) as live playback, into an
//! [`OffscreenTarget`], read back as RGBA and piped to an `ffmpeg` process
//! that encodes the picture. Rendering waits whenever an item or a video
//! frame is not ready ([`Slideshow::is_waiting`]), so the file never shows
//! the delays live playback would.
//!
//! The sound is mixed in step by an [`OfflineMixer`] made for export (no
//! sync offset) and written to a temporary file of raw samples. A second
//! `ffmpeg` run puts picture and sound into the final file. Feeding both to
//! one process would need a second pipe besides standard input, which is
//! awkward to hand to a child process on Windows.

use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::audio::{OfflineMixer, TRACK_CHANNELS};
use crate::bgm::{Bgm, BgmConfig, BgmSource};
use crate::media::video::{Ffmpeg, collect_log, command};
use crate::{EngineError, OffscreenTarget, Renderer, Scene, Slideshow};

/// Sample rate of the exported sound.
pub const EXPORT_SAMPLE_RATE: u32 = 48_000;
/// Longest time to wait for one frame's item or video frame to be ready.
const WAIT_TIMEOUT: Duration = Duration::from_secs(30);
/// Frames read back but not yet taken by the encoder.
const QUEUED_FRAMES: usize = 2;
/// The music fades out over the end of the file, as the picture fades to black.
const MUSIC_END_FADE_SECS: f64 = 2.0;

/// H.264 encoders in order of preference. The operating systems' encoders
/// come first: the LGPL build of FFmpeg that Fluxlide bundles has only
/// those (see `docs/spike-ffmpeg-bundling.md`). `libx264` is found in
/// GPL builds such as Linux distributions' packages, which are used during
/// development. `mpeg4` is FFmpeg's own encoder and always present.
const VIDEO_ENCODERS: [&str; 5] = [
    "h264_videotoolbox",
    "h264_mf",
    "libx264",
    "libopenh264",
    "mpeg4",
];

#[derive(Debug, Clone, PartialEq)]
pub struct ExportSettings {
    /// Size of the video in pixels. Both must be even (4:2:0 chroma).
    pub size: (u32, u32),
    pub fps: u32,
    /// Target bit rate of the picture. Encoders with a quality scale instead
    /// (`libx264`) use a quality that gives about this rate for photos.
    pub video_kbps: u32,
    pub audio_kbps: u32,
    /// The encoder to use, or `None` for the first available of
    /// [`VIDEO_ENCODERS`].
    pub video_encoder: Option<String>,
    pub music: Option<ExportMusic>,
}

/// The background music of an export.
#[derive(Debug, Clone, PartialEq)]
pub struct ExportMusic {
    pub source: BgmSource,
    pub config: BgmConfig,
    /// Lower the music while videos with sound play.
    pub duck_for_videos: bool,
}

impl Default for ExportSettings {
    fn default() -> Self {
        Self {
            size: (1920, 1080),
            fps: 30,
            video_kbps: 12_000,
            audio_kbps: 192,
            video_encoder: None,
            music: None,
        }
    }
}

/// How far an export has got.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ExportProgress {
    pub frames: u64,
    /// Playback time of the last frame written.
    pub media_secs: f64,
}

/// Where the time of an export went, for judging where it can be made faster.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ExportReport {
    pub encoder: String,
    pub frames: u64,
    pub media_secs: f64,
    pub wall_secs: f64,
    /// Advancing the slideshow and handing the frame's drawing to the GPU.
    pub render_secs: f64,
    /// Waiting for the GPU to finish the frame and copying it back.
    pub readback_secs: f64,
    /// Waiting for items and video frames to be loaded and decoded.
    pub wait_secs: f64,
    /// Waiting for the encoder to take frames (it is the slower side).
    pub encode_wait_secs: f64,
    /// Mixing the sound and writing it out.
    pub audio_secs: f64,
    /// Putting picture and sound into the final file.
    pub mux_secs: f64,
}

/// The H.264 (or fallback) encoder `ffmpeg` offers that comes first in
/// [`VIDEO_ENCODERS`].
pub fn pick_video_encoder(ffmpeg: &Ffmpeg) -> Result<String, EngineError> {
    let output = command(ffmpeg.ffmpeg_path())
        .args(["-hide_banner", "-encoders"])
        .output()
        .map_err(|err| EngineError::Export(format!("could not run ffmpeg: {err}")))?;
    let listed = String::from_utf8_lossy(&output.stdout);
    let available: Vec<&str> = listed
        .lines()
        .filter_map(|line| line.split_whitespace().nth(1))
        .collect();
    VIDEO_ENCODERS
        .iter()
        .find(|name| available.contains(name))
        .map(|name| name.to_string())
        .ok_or_else(|| EngineError::Export("ffmpeg offers no usable video encoder".into()))
}

/// `ffmpeg` arguments that choose the encoder and its quality.
fn encoder_args(encoder: &str, settings: &ExportSettings) -> Vec<String> {
    let rate = format!("{}k", settings.video_kbps);
    let mut args = vec!["-c:v".to_string(), encoder.to_string()];
    match encoder {
        // A constant quality gives photos the bits they need and still ones
        // (most of a slideshow) almost none.
        "libx264" => args.extend(["-preset", "medium", "-crf", "18"].map(String::from)),
        "mpeg4" => args.extend(["-q:v", "2"].map(String::from)),
        _ => args.extend(["-b:v".to_string(), rate]),
    }
    args
}

/// Plays `show` once and writes it to `out` (an `.mp4`). `show` must not
/// have been updated yet; its output size must be `settings.size`.
/// `progress` is called after every frame; setting `cancel` stops the
/// export and removes the partial files.
pub fn export_slideshow(
    renderer: &Arc<Renderer>,
    ffmpeg: &Ffmpeg,
    show: &mut Slideshow,
    settings: &ExportSettings,
    out: &Path,
    cancel: &AtomicBool,
    mut progress: impl FnMut(ExportProgress),
) -> Result<ExportReport, EngineError> {
    let (width, height) = settings.size;
    if width == 0 || height == 0 || width % 2 != 0 || height % 2 != 0 || settings.fps == 0 {
        return Err(EngineError::Export(format!(
            "cannot export {width}x{height} at {} fps: the size must be even and not zero",
            settings.fps
        )));
    }
    let encoder = match &settings.video_encoder {
        Some(name) => name.clone(),
        None => pick_video_encoder(ffmpeg)?,
    };
    let started = Instant::now();
    let temp = TempFiles::next_to(out);
    let result = render_pass(
        renderer,
        ffmpeg,
        show,
        settings,
        &encoder,
        &temp,
        cancel,
        &mut progress,
    )
    .and_then(|mut report| {
        let mux_started = Instant::now();
        mux(ffmpeg, &temp, settings, out)?;
        report.mux_secs = mux_started.elapsed().as_secs_f64();
        report.wall_secs = started.elapsed().as_secs_f64();
        report.encoder = encoder.clone();
        Ok(report)
    });
    temp.remove();
    if result.is_err() {
        let _ = std::fs::remove_file(out);
    }
    result
}

/// The intermediate files of an export, next to the output.
struct TempFiles {
    video: PathBuf,
    audio: PathBuf,
}

impl TempFiles {
    fn next_to(out: &Path) -> Self {
        let with = |suffix: &str| {
            let mut name = out.file_name().unwrap_or_default().to_os_string();
            name.push(suffix);
            out.with_file_name(name)
        };
        Self {
            video: with(".fluxlide-video.mkv"),
            audio: with(".fluxlide-audio.f32"),
        }
    }

    fn remove(&self) {
        let _ = std::fs::remove_file(&self.video);
        let _ = std::fs::remove_file(&self.audio);
    }
}

/// Renders every frame, encoding the picture into `temp.video` and writing
/// the sound to `temp.audio`.
#[allow(clippy::too_many_arguments)]
fn render_pass(
    renderer: &Arc<Renderer>,
    ffmpeg: &Ffmpeg,
    show: &mut Slideshow,
    settings: &ExportSettings,
    encoder: &str,
    temp: &TempFiles,
    cancel: &AtomicBool,
    progress: &mut impl FnMut(ExportProgress),
) -> Result<ExportReport, EngineError> {
    let (width, height) = settings.size;
    let fps = f64::from(settings.fps);
    let mut target = OffscreenTarget::new(renderer, width, height)?;
    let mut mixer = OfflineMixer::export(EXPORT_SAMPLE_RATE);
    show.set_audio(mixer.mixer());
    show.play_once();
    // Kept until the end: dropping it stops the music.
    let _bgm = match &settings.music {
        Some(music) => {
            let bgm = Bgm::start(
                ffmpeg.clone(),
                &mixer.mixer(),
                &music.source,
                music.config.clone(),
            )?;
            if music.duck_for_videos {
                show.set_ducker(bgm.ducker());
            }
            Some(bgm)
        }
        None => None,
    };

    let mut encoder_process = VideoEncoder::spawn(ffmpeg, settings, encoder, &temp.video)?;
    // Read back at the end, to fade the music out.
    let audio_file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(&temp.audio)
        .map_err(|err| EngineError::Export(format!("could not write the sound: {err}")))?;
    let mut audio_out = BufWriter::new(audio_file);

    let mut report = ExportReport::default();
    let mut frame_index: u64 = 0;
    let mut samples_written: u64 = 0;
    loop {
        if cancel.load(Ordering::Relaxed) {
            encoder_process.abort();
            return Err(EngineError::Export("the export was cancelled".into()));
        }
        let time = frame_index as f64 / fps;

        // Advance, waiting until everything due at this time is ready.
        let render_started = Instant::now();
        let mut waited = Duration::ZERO;
        loop {
            show.update(time);
            if !show.is_waiting() {
                break;
            }
            if cancel.load(Ordering::Relaxed) {
                encoder_process.abort();
                return Err(EngineError::Export("the export was cancelled".into()));
            }
            if waited > WAIT_TIMEOUT {
                encoder_process.abort();
                return Err(EngineError::Export(format!(
                    "timed out at {time:.2} s waiting for an item to load"
                )));
            }
            std::thread::sleep(Duration::from_millis(1));
            waited += Duration::from_millis(1);
        }
        report.wait_secs += waited.as_secs_f64();
        if show.is_finished() {
            break;
        }
        target.render(renderer, &Scene::Frame(show.frame()))?;
        report.render_secs += render_started.elapsed().as_secs_f64() - waited.as_secs_f64();

        let readback_started = Instant::now();
        let mut pixels = encoder_process.spare_buffer();
        target.read_rgba_into(renderer, &mut pixels)?;
        report.readback_secs += readback_started.elapsed().as_secs_f64();

        let encode_started = Instant::now();
        encoder_process.send(pixels)?;
        report.encode_wait_secs += encode_started.elapsed().as_secs_f64();

        // The sound of this frame's span of time, sample-exact over the whole file.
        let audio_started = Instant::now();
        let end = (frame_index + 1) * u64::from(EXPORT_SAMPLE_RATE) / u64::from(settings.fps);
        let frames = (end - samples_written) as usize;
        let mut audio_waited = Duration::ZERO;
        while !mixer.has_samples_for(frames) {
            if cancel.load(Ordering::Relaxed) {
                encoder_process.abort();
                return Err(EngineError::Export("the export was cancelled".into()));
            }
            if audio_waited > WAIT_TIMEOUT {
                encoder_process.abort();
                return Err(EngineError::Export(format!(
                    "timed out at {time:.2} s waiting for sound to be decoded"
                )));
            }
            std::thread::sleep(Duration::from_millis(1));
            audio_waited += Duration::from_millis(1);
        }
        report.wait_secs += audio_waited.as_secs_f64();
        let heard_at = samples_written as f64 / f64::from(EXPORT_SAMPLE_RATE);
        let samples = mixer.render(frames, heard_at);
        for sample in samples {
            audio_out
                .write_all(&sample.to_le_bytes())
                .map_err(|err| EngineError::Export(format!("could not write the sound: {err}")))?;
        }
        samples_written = end;
        report.audio_secs += audio_started.elapsed().as_secs_f64() - audio_waited.as_secs_f64();

        frame_index += 1;
        progress(ExportProgress {
            frames: frame_index,
            media_secs: time,
        });
    }
    let audio_file = audio_out
        .into_inner()
        .map_err(|err| EngineError::Export(format!("could not write the sound: {err}")))?;
    if settings.music.is_some() {
        fade_out_tail(audio_file, samples_written)
            .map_err(|err| EngineError::Export(format!("could not write the sound: {err}")))?;
    }
    let finish_started = Instant::now();
    encoder_process.finish()?;
    report.encode_wait_secs += finish_started.elapsed().as_secs_f64();
    report.frames = frame_index;
    report.media_secs = frame_index as f64 / fps;
    if frame_index == 0 {
        return Err(EngineError::Export(
            "nothing to export: no item could be loaded".into(),
        ));
    }
    Ok(report)
}

/// Fades the last [`MUSIC_END_FADE_SECS`] of the raw sound in `file`, which
/// holds `frames` stereo frames, to silence.
fn fade_out_tail(mut file: std::fs::File, frames: u64) -> std::io::Result<()> {
    use std::io::{Read, Seek, SeekFrom};
    let fade = ((MUSIC_END_FADE_SECS * f64::from(EXPORT_SAMPLE_RATE)) as u64).min(frames);
    if fade == 0 {
        return Ok(());
    }
    let frame_bytes = (TRACK_CHANNELS * 4) as u64;
    let start = (frames - fade) * frame_bytes;
    let mut tail = vec![0u8; (fade * frame_bytes) as usize];
    file.seek(SeekFrom::Start(start))?;
    file.read_exact(&mut tail)?;
    for (index, frame) in tail.chunks_exact_mut(frame_bytes as usize).enumerate() {
        let gain = 1.0 - (index + 1) as f32 / fade as f32;
        for sample in frame.as_chunks_mut::<4>().0 {
            *sample = (f32::from_le_bytes(*sample) * gain).to_le_bytes();
        }
    }
    file.seek(SeekFrom::Start(start))?;
    file.write_all(&tail)
}

/// An `ffmpeg` process encoding raw RGBA frames from its standard input.
/// Frames are written by a thread of their own, so the GPU can draw the
/// next frame while the pipe takes the last one.
struct VideoEncoder {
    child: Child,
    frames: Option<SyncSender<Vec<u8>>>,
    spare: Receiver<Vec<u8>>,
    writer: Option<JoinHandle<Result<(), String>>>,
    stderr: Arc<Mutex<std::collections::VecDeque<String>>>,
}

impl VideoEncoder {
    fn spawn(
        ffmpeg: &Ffmpeg,
        settings: &ExportSettings,
        encoder: &str,
        out: &Path,
    ) -> Result<Self, EngineError> {
        let (width, height) = settings.size;
        let mut child = command(ffmpeg.ffmpeg_path())
            .args(["-hide_banner", "-loglevel", "error", "-y"])
            .args(["-f", "rawvideo", "-pix_fmt", "rgba"])
            .args(["-s", &format!("{width}x{height}")])
            .args(["-framerate", &settings.fps.to_string()])
            .args(["-i", "pipe:0"])
            // The frames are sRGB with full-range values; videos are BT.709
            // with limited range. The conversion is spelled out instead of
            // left to ffmpeg's defaults, which use BT.601 for RGB input.
            .args([
                "-vf",
                "scale=out_color_matrix=bt709:out_range=tv,format=yuv420p",
            ])
            .args(encoder_args(encoder, settings))
            .args(["-color_primaries", "bt709", "-color_trc", "bt709"])
            .args(["-colorspace", "bt709", "-color_range", "tv"])
            .arg(out)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|err| EngineError::Export(format!("could not start ffmpeg: {err}")))?;
        let stderr = Arc::new(Mutex::new(std::collections::VecDeque::new()));
        {
            let pipe = child.stderr.take().expect("piped");
            let stderr = stderr.clone();
            std::thread::spawn(move || collect_log(pipe, &stderr));
        }
        let stdin = child.stdin.take().expect("piped");
        let (frames, incoming) = mpsc::sync_channel::<Vec<u8>>(QUEUED_FRAMES);
        let (recycle, spare) = mpsc::sync_channel(QUEUED_FRAMES + 1);
        let writer = std::thread::Builder::new()
            .name("fluxlide-export".into())
            .spawn(move || write_frames(stdin, incoming, recycle))
            .map_err(|err| EngineError::Export(err.to_string()))?;
        Ok(Self {
            child,
            frames: Some(frames),
            spare,
            writer: Some(writer),
            stderr,
        })
    }

    fn spare_buffer(&self) -> Vec<u8> {
        self.spare.try_recv().unwrap_or_default()
    }

    /// Queues a frame, blocking while the encoder is behind.
    fn send(&mut self, pixels: Vec<u8>) -> Result<(), EngineError> {
        let sent = self.frames.as_ref().map(|frames| frames.send(pixels));
        if matches!(sent, Some(Ok(()))) {
            return Ok(());
        }
        // The writer stopped: ffmpeg exited. Report why.
        self.frames = None;
        let reason = match self.writer.take().map(JoinHandle::join) {
            Some(Ok(Err(reason))) => reason,
            _ => "the encoder stopped".into(),
        };
        let _ = self.child.wait();
        Err(self.error(&reason))
    }

    /// Closes the input and waits for the file to be complete.
    fn finish(mut self) -> Result<(), EngineError> {
        self.frames = None;
        let written = self.writer.take().map(JoinHandle::join);
        let status = self
            .child
            .wait()
            .map_err(|err| EngineError::Export(err.to_string()))?;
        if let Some(Ok(Err(reason))) = written {
            return Err(self.error(&reason));
        }
        if !status.success() {
            return Err(self.error(&format!("ffmpeg exited with {status}")));
        }
        Ok(())
    }

    fn abort(&mut self) {
        self.frames = None;
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(writer) = self.writer.take() {
            let _ = writer.join();
        }
    }

    fn error(&self, reason: &str) -> EngineError {
        let log = self.stderr.lock().unwrap();
        let detail = log.iter().cloned().collect::<Vec<_>>().join(" / ");
        EngineError::Export(if detail.is_empty() {
            format!("encoding failed: {reason}")
        } else {
            format!("encoding failed: {reason}: {detail}")
        })
    }
}

impl Drop for VideoEncoder {
    fn drop(&mut self) {
        // `finish` takes the writer: anything else still running belongs to an
        // export that failed half way, and ffmpeg must not keep the file open.
        if self.writer.is_some() {
            self.abort();
        }
    }
}

fn write_frames(
    stdin: ChildStdin,
    frames: Receiver<Vec<u8>>,
    recycle: SyncSender<Vec<u8>>,
) -> Result<(), String> {
    let mut stdin = stdin;
    for frame in frames {
        stdin.write_all(&frame).map_err(|err| err.to_string())?;
        let _ = recycle.try_send(frame);
    }
    // Dropping stdin closes the pipe, which tells ffmpeg the input is over.
    Ok(())
}

/// Puts the encoded picture and the raw sound into `out`.
fn mux(
    ffmpeg: &Ffmpeg,
    temp: &TempFiles,
    settings: &ExportSettings,
    out: &Path,
) -> Result<(), EngineError> {
    let output = command(ffmpeg.ffmpeg_path())
        .args(["-hide_banner", "-loglevel", "error", "-y"])
        .arg("-i")
        .arg(&temp.video)
        .args(["-f", "f32le", "-ar", &EXPORT_SAMPLE_RATE.to_string()])
        .args(["-ac", &TRACK_CHANNELS.to_string(), "-i"])
        .arg(&temp.audio)
        .args(["-map", "0:v", "-map", "1:a", "-c:v", "copy"])
        .args(["-c:a", "aac", "-b:a", &format!("{}k", settings.audio_kbps)])
        .args(["-movflags", "+faststart"])
        .arg(out)
        .output()
        .map_err(|err| EngineError::Export(format!("could not start ffmpeg: {err}")))?;
    if !output.status.success() {
        return Err(EngineError::Export(format!(
            "could not put picture and sound together: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::show::{
        Asset, AssetId, AssetKind, AssetMeta, AssetState, ShowDefinition, Slide, SlideId,
    };

    #[test]
    fn a_slideshow_is_written_once_with_picture_and_sound() {
        let Some(renderer) = crate::test_renderer() else {
            return;
        };
        let Ok(ffmpeg) = Ffmpeg::locate() else {
            eprintln!("skipping export test: ffmpeg not found");
            return;
        };
        let dir = std::env::temp_dir().join(format!("fluxlide-export-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let photo = dir.join("photo.png");
        image::RgbImage::from_pixel(64, 36, image::Rgb([200, 40, 40]))
            .save(&photo)
            .unwrap();
        let clip = dir.join("clip.mp4");
        let made = command(ffmpeg.ffmpeg_path())
            .args(["-v", "error", "-y", "-f", "lavfi", "-i"])
            .arg("testsrc2=size=64x36:rate=25:duration=1")
            .args(["-f", "lavfi", "-i", "sine=frequency=440:duration=1"])
            .args(["-pix_fmt", "yuv420p", "-c:v", "mpeg4", "-c:a", "aac"])
            .arg(&clip)
            .status()
            .unwrap();
        assert!(made.success());

        // A photo shown for 0.5 s, then the video, joined by 0.25 s fades.
        let video_ms = (ffmpeg.probe(&clip).unwrap().duration.unwrap() * 1000.0) as u32;
        let mut definition = ShowDefinition::default();
        for (id, file, duration_ms) in [(1, "photo.png", None), (2, "clip.mp4", Some(video_ms))] {
            definition.assets.push(Asset {
                id: AssetId(id),
                kind: if duration_ms.is_some() {
                    AssetKind::Video
                } else {
                    AssetKind::Photo
                },
                file: file.into(),
                original_name: file.into(),
                state: AssetState::Ready,
                meta: AssetMeta::default(),
                size: None,
                duration_ms,
                has_audio: duration_ms.is_some(),
            });
            definition.slides.push(Slide::new(SlideId(id), AssetId(id)));
        }
        definition.look.photo_ms = 500;
        definition.look.transition_ms = Some(250);
        let timeline = crate::show::Timeline::build(&definition, &[0, 1]);
        let settings = ExportSettings {
            size: (64, 36),
            fps: 20,
            video_encoder: Some("mpeg4".into()),
            ..Default::default()
        };
        let mut show = Slideshow::new(
            renderer.clone(),
            definition.clone(),
            dir.clone(),
            settings.size,
        );
        let out = dir.join("out.mp4");
        let mut last = None;
        let report = export_slideshow(
            &renderer,
            &ffmpeg,
            &mut show,
            &settings,
            &out,
            &AtomicBool::new(false),
            |progress| last = Some(progress),
        )
        .unwrap();

        // The file is as long as the timeline, to the frame.
        let expected = timeline.end_ms as f64 / 1000.0;
        assert!(
            (report.media_secs - expected).abs() <= 1.0 / 20.0,
            "{report:?}"
        );
        assert_eq!(last.unwrap().frames, report.frames);
        assert!(!dir.join("out.mp4.fluxlide-video.mkv").exists());
        assert!(!dir.join("out.mp4.fluxlide-audio.f32").exists());
        let info = ffmpeg.probe(&out).unwrap();
        assert!(info.has_audio);
        let duration = info.duration.unwrap();
        assert!((duration - report.media_secs).abs() < 0.1, "{duration}");

        // A cancelled export leaves nothing behind.
        let mut show = Slideshow::new(renderer.clone(), definition, dir.clone(), settings.size);
        let cancelled = dir.join("cancelled.mp4");
        let err = export_slideshow(
            &renderer,
            &ffmpeg,
            &mut show,
            &settings,
            &cancelled,
            &AtomicBool::new(true),
            |_| {},
        );
        assert!(err.is_err());
        assert!(!cancelled.exists());
        assert!(!dir.join("cancelled.mp4.fluxlide-video.mkv").exists());

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_music_plays_through_and_fades_out_at_the_end() {
        let Some(renderer) = crate::test_renderer() else {
            return;
        };
        let Ok(ffmpeg) = Ffmpeg::locate() else {
            eprintln!("skipping export test: ffmpeg not found");
            return;
        };
        let dir =
            std::env::temp_dir().join(format!("fluxlide-export-music-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        image::RgbImage::from_pixel(64, 36, image::Rgb([40, 40, 200]))
            .save(dir.join("photo.png"))
            .unwrap();
        let song = dir.join("song.wav");
        let made = command(ffmpeg.ffmpeg_path())
            .args(["-v", "error", "-y", "-f", "lavfi", "-i"])
            .arg("sine=frequency=440:duration=10")
            .arg(&song)
            .status()
            .unwrap();
        assert!(made.success());

        let mut definition = ShowDefinition::default();
        definition.assets.push(Asset {
            id: AssetId(1),
            kind: AssetKind::Photo,
            file: "photo.png".into(),
            original_name: "photo.png".into(),
            state: AssetState::Ready,
            meta: AssetMeta::default(),
            size: None,
            duration_ms: None,
            has_audio: false,
        });
        definition.slides.push(Slide::new(SlideId(1), AssetId(1)));
        definition.look.photo_ms = 4000;
        let settings = ExportSettings {
            size: (64, 36),
            fps: 20,
            video_encoder: Some("mpeg4".into()),
            music: Some(ExportMusic {
                source: BgmSource::Files(vec![song]),
                config: BgmConfig {
                    tuning: crate::bgm::BgmTuning {
                        fade_secs: 0.0,
                        ..Default::default()
                    },
                    ..Default::default()
                },
                duck_for_videos: false,
            }),
            ..Default::default()
        };
        let mut show = Slideshow::new(renderer.clone(), definition, dir.clone(), settings.size);
        let out = dir.join("out.mp4");
        export_slideshow(
            &renderer,
            &ffmpeg,
            &mut show,
            &settings,
            &out,
            &AtomicBool::new(false),
            |_| {},
        )
        .unwrap();

        // The sound, as mono 1 kHz samples.
        let decoded = command(ffmpeg.ffmpeg_path())
            .args(["-v", "error", "-i"])
            .arg(&out)
            .args(["-f", "f32le", "-ac", "1", "-ar", "1000", "pipe:1"])
            .output()
            .unwrap();
        let samples: Vec<f32> = decoded
            .stdout
            .as_chunks::<4>()
            .0
            .iter()
            .map(|bytes| f32::from_le_bytes(*bytes))
            .collect();
        let loudness = |range: std::ops::Range<usize>| {
            let part = &samples[range];
            (part.iter().map(|s| s * s).sum::<f32>() / part.len() as f32).sqrt()
        };
        let len = samples.len();
        assert!(len > 3000, "{len}");
        // FFmpeg's sine is at 1/8 of full scale, and the BGM at half volume:
        // about 0.044 RMS. Heard from the start, with no wait in the file.
        let full = 0.125 * 0.5 / 2f32.sqrt();
        assert!(loudness(100..400) > full * 0.8, "{}", loudness(100..400));
        assert!(loudness(len / 2 - 200..len / 2 + 200) > full * 0.8);
        // Silent by the last frames.
        let end = loudness(len - 50..len);
        assert!(end < full * 0.1, "{end}");

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
