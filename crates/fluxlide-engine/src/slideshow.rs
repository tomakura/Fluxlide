//! A looping playlist of photos and videos joined by crossfades.
//!
//! While one item is on screen, the next one is loaded on a background
//! thread: photos are decoded and uploaded, videos are probed and decoded up
//! to a few frames ahead. A crossfade starts only once the next item is
//! ready, so the output never falls back to black between items. If it is
//! not ready in time, the current item stays (a finished video holds its
//! last frame) and the delay is counted in [`SlideshowStats`].
//!
//! With an [`AudioMixer`] attached, a video's sound starts with its picture
//! on the same playback clock and its volume follows the picture's opacity
//! through crossfades.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::Instant;

use crate::audio::{AudioFormat, AudioMixer, AudioStats, VoiceHandle};
use crate::media::audio::AudioTrack;
use crate::media::video::{Ffmpeg, VideoPlayer};
use crate::media::{MediaKind, MediaTexture, load_image};
use crate::presenter::Layer;
use crate::{EngineError, Renderer};

#[derive(Debug, Clone, Copy)]
pub struct SlideshowConfig {
    /// How long a photo stays, including the crossfades at both ends.
    pub image_secs: f64,
    /// Length of a crossfade. Shortened for items shorter than twice this.
    pub fade_secs: f64,
}

impl Default for SlideshowConfig {
    fn default() -> Self {
        Self {
            image_secs: 5.0,
            fade_secs: 1.0,
        }
    }
}

/// Measurements for judging whether playback is seamless.
#[derive(Debug, Default, Clone)]
pub struct SlideshowStats {
    /// Index of the item fading in or on screen.
    pub current: Option<usize>,
    pub transitions: u64,
    /// Transitions that started late because the next item was not loaded.
    pub late_transitions: u64,
    pub longest_wait_ms: f64,
    /// Time the most recent item took to load, from request to ready.
    pub last_load_ms: f64,
    /// Rendered frames where the due video frame was not decoded yet.
    pub video_late_frames: u64,
    /// Decoded video frames that were never shown.
    pub video_skipped_frames: u64,
    pub failed_items: u64,
    pub last_error: Option<String>,
}

enum Prepared {
    Image(MediaTexture),
    Video {
        // Boxed: a decoder holds process handles, which are large on Windows.
        player: Box<VideoPlayer>,
        /// Present when the file has sound and an output is attached.
        audio: Option<AudioTrack>,
    },
}

impl Prepared {
    fn texture(&self) -> &MediaTexture {
        match self {
            Self::Image(texture) => texture,
            Self::Video { player, .. } => player.texture(),
        }
    }

    fn duration(&self, config: &SlideshowConfig) -> Option<f64> {
        match self {
            Self::Image(_) => Some(config.image_secs),
            Self::Video { player, .. } => player.duration(),
        }
    }
}

struct Showing {
    index: usize,
    media: Prepared,
    /// Playback time at which this item started fading in.
    started: f64,
    /// The item's sound while it plays.
    voice: Option<VoiceHandle>,
}

struct Loading {
    index: usize,
    since: Instant,
    result: Receiver<Result<Prepared, EngineError>>,
}

struct Fade {
    start: f64,
    secs: f64,
}

pub struct Slideshow {
    renderer: Arc<Renderer>,
    ffmpeg: Result<Ffmpeg, String>,
    items: Vec<PathBuf>,
    output_size: (u32, u32),
    config: SlideshowConfig,
    current: Option<Showing>,
    outgoing: Option<Showing>,
    fade: Option<Fade>,
    loading: Option<Loading>,
    ready: Option<(usize, Prepared)>,
    next_index: usize,
    failures_in_row: usize,
    /// Time the current transition became due while the next item was not ready.
    waiting_since: Option<f64>,
    now: f64,
    stats: SlideshowStats,
    audio: Option<AudioMixer>,
}

impl Slideshow {
    /// `output_size` is the size of the output in pixels; larger photos and
    /// videos are shrunk to it when loaded.
    pub fn new(
        renderer: Arc<Renderer>,
        items: Vec<PathBuf>,
        output_size: (u32, u32),
        config: SlideshowConfig,
    ) -> Self {
        Self {
            renderer,
            ffmpeg: Ffmpeg::locate().map_err(|err| err.to_string()),
            items,
            output_size,
            config,
            current: None,
            outgoing: None,
            fade: None,
            loading: None,
            ready: None,
            next_index: 0,
            failures_in_row: 0,
            waiting_since: None,
            now: 0.0,
            stats: SlideshowStats::default(),
            audio: None,
        }
    }

    /// Plays the sound of videos through `mixer`. `mixer` must count
    /// playback time from the same instant as the times given to
    /// [`Slideshow::update`]. Items loaded before this stay silent.
    pub fn set_audio(&mut self, mixer: AudioMixer) {
        self.audio = Some(mixer);
    }

    pub fn audio_stats(&self) -> Option<AudioStats> {
        self.audio.as_ref().map(AudioMixer::stats)
    }

    pub fn stats(&self) -> &SlideshowStats {
        &self.stats
    }

    /// File of the item fading in or on screen.
    pub fn current_path(&self) -> Option<&Path> {
        let index = self.current.as_ref()?.index;
        self.items.get(index).map(PathBuf::as_path)
    }

    /// Advances playback to `now` seconds: starts loading, starts and ends
    /// crossfades, uploads the video frames due at this time and sets the
    /// volume of each sound to its picture's opacity.
    pub fn update(&mut self, now: f64) {
        self.advance(now);
        let progress = self.fade_progress();
        if let Some(voice) = self.outgoing.as_ref().and_then(|s| s.voice.as_ref()) {
            voice.set_gain(1.0 - progress);
        }
        if let Some(voice) = self.current.as_ref().and_then(|s| s.voice.as_ref()) {
            voice.set_gain(progress);
        }
    }

    fn advance(&mut self, now: f64) {
        self.now = now;
        self.poll_loading();

        for showing in self.current.iter_mut().chain(self.outgoing.iter_mut()) {
            if let Prepared::Video { player, .. } = &mut showing.media {
                let update = player.update(&self.renderer, now - showing.started);
                self.stats.video_late_frames += u64::from(update.late);
                self.stats.video_skipped_frames += u64::from(update.skipped);
            }
        }

        if let Some(fade) = &self.fade {
            if now - fade.start < fade.secs {
                return;
            }
            self.fade = None;
            self.outgoing = None;
        }

        let Some(current) = &self.current else {
            // Nothing shown yet: fade in from black as soon as the first item loads.
            if self.ready.is_some() {
                self.begin_transition(self.config.fade_secs);
            }
            return;
        };
        let next_duration = self
            .ready
            .as_ref()
            .and_then(|(_, media)| media.duration(&self.config));
        let fade_secs = [current.media.duration(&self.config), next_duration]
            .into_iter()
            .flatten()
            .fold(self.config.fade_secs, |fade, duration| {
                fade.min(duration / 2.0)
            });
        let due = match &current.media {
            Prepared::Video { player, .. } if player.duration().is_none() => {
                // Without a known length, fade once the last frame is out.
                if player.has_ended() {
                    now
                } else {
                    f64::INFINITY
                }
            }
            media => current.started + media.duration(&self.config).unwrap_or(0.0) - fade_secs,
        };
        if now < due {
            return;
        }
        if self.ready.is_some() {
            if let Some(since) = self.waiting_since.take() {
                self.stats.late_transitions += 1;
                self.stats.longest_wait_ms = self.stats.longest_wait_ms.max((now - since) * 1000.0);
            }
            self.begin_transition(fade_secs);
        } else if self.loading.is_some() {
            self.waiting_since.get_or_insert(due);
        }
    }

    /// Layers to draw for the time given to the last [`Slideshow::update`].
    pub fn layers(&self) -> Vec<Layer<'_>> {
        let progress = self.fade_progress();
        let mut layers = Vec::with_capacity(2);
        if let Some(outgoing) = &self.outgoing {
            layers.push(Layer {
                texture: outgoing.media.texture(),
                opacity: 1.0 - progress,
            });
        }
        if let Some(current) = &self.current {
            layers.push(Layer {
                texture: current.media.texture(),
                opacity: progress,
            });
        }
        layers
    }

    /// How far the current crossfade has gone, from 0 to 1 (1 when none).
    fn fade_progress(&self) -> f32 {
        let progress = match &self.fade {
            Some(fade) if fade.secs > 0.0 => ((self.now - fade.start) / fade.secs).clamp(0.0, 1.0),
            _ => 1.0,
        };
        progress as f32
    }

    fn begin_transition(&mut self, fade_secs: f64) {
        let Some((index, mut media)) = self.ready.take() else {
            return;
        };
        // The sound starts at the same playback time as the picture, silent
        // until `update` raises it with the fade.
        let voice = match (&mut media, &self.audio) {
            (
                Prepared::Video {
                    audio: Some(track), ..
                },
                Some(mixer),
            ) => track
                .take_samples()
                .map(|(samples, finished)| mixer.play(samples, finished, self.now, 0.0)),
            _ => None,
        };
        self.outgoing = self.current.take();
        self.current = Some(Showing {
            index,
            media,
            started: self.now,
            voice,
        });
        self.fade = Some(Fade {
            start: self.now,
            secs: fade_secs,
        });
        if self.outgoing.is_some() {
            self.stats.transitions += 1;
        }
        self.stats.current = Some(index);
        self.start_loading();
    }

    fn poll_loading(&mut self) {
        if let Some(loading) = &self.loading {
            let result = match loading.result.try_recv() {
                Ok(result) => result,
                Err(TryRecvError::Empty) => return,
                Err(TryRecvError::Disconnected) => {
                    Err(EngineError::Media("the loader thread stopped".into()))
                }
            };
            let (index, since) = (loading.index, loading.since);
            self.loading = None;
            match result {
                Ok(media) => {
                    self.stats.last_load_ms = since.elapsed().as_secs_f64() * 1000.0;
                    self.failures_in_row = 0;
                    self.ready = Some((index, media));
                }
                Err(err) => {
                    self.stats.failed_items += 1;
                    self.stats.last_error = Some(err.to_string());
                    self.failures_in_row += 1;
                }
            }
        }
        if self.ready.is_none() && self.loading.is_none() {
            self.start_loading();
        }
    }

    /// Starts loading the next item in the playlist, looping at the end.
    /// Stops trying once every item has failed in a row.
    fn start_loading(&mut self) {
        if self.items.is_empty() || self.failures_in_row >= self.items.len() {
            return;
        }
        let index = self.next_index;
        self.next_index = (index + 1) % self.items.len();
        let path = self.items[index].clone();
        let renderer = self.renderer.clone();
        let ffmpeg = self.ffmpeg.clone();
        let output_size = self.output_size;
        let audio = self.audio.as_ref().map(AudioMixer::format);
        let (tx, result) = mpsc::channel();
        let spawned = std::thread::Builder::new()
            .name("fluxlide-preload".into())
            .spawn(move || {
                let _ = tx.send(prepare(&renderer, ffmpeg, &path, output_size, audio));
            });
        match spawned {
            Ok(_) => {
                self.loading = Some(Loading {
                    index,
                    since: Instant::now(),
                    result,
                });
            }
            Err(err) => {
                self.stats.failed_items += 1;
                self.stats.last_error = Some(err.to_string());
                self.failures_in_row += 1;
            }
        }
    }
}

fn prepare(
    renderer: &Renderer,
    ffmpeg: Result<Ffmpeg, String>,
    path: &Path,
    output_size: (u32, u32),
    audio: Option<AudioFormat>,
) -> Result<Prepared, EngineError> {
    match MediaKind::from_path(path) {
        Some(MediaKind::Image) => load_image(renderer, path, output_size).map(Prepared::Image),
        Some(MediaKind::Video) => {
            let ffmpeg = ffmpeg.map_err(EngineError::Media)?;
            let player = VideoPlayer::open(renderer, &ffmpeg, path, output_size)?;
            let audio = match audio {
                Some(format) if player.has_audio() => {
                    Some(AudioTrack::open(&ffmpeg, path, format)?)
                }
                _ => None,
            };
            Ok(Prepared::Video {
                player: Box::new(player),
                audio,
            })
        }
        None => Err(EngineError::Media(format!(
            "unsupported file type: {}",
            path.display()
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// Calls `update(now)` until `done` holds, giving loader threads time.
    fn update_until(show: &mut Slideshow, now: f64, done: impl Fn(&Slideshow) -> bool) {
        for _ in 0..500 {
            show.update(now);
            if done(show) {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("timed out at {now}: {:?}", show.stats());
    }

    fn opacities(show: &Slideshow) -> Vec<f32> {
        show.layers().iter().map(|layer| layer.opacity).collect()
    }

    #[test]
    fn photos_crossfade_without_gaps_and_skip_broken_files() {
        let Some(renderer) = crate::test_renderer() else {
            return;
        };
        let dir = std::env::temp_dir().join(format!("fluxlide-slideshow-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let red = dir.join("red.png");
        let blue = dir.join("blue.jpg");
        image::RgbImage::from_pixel(64, 48, image::Rgb([255, 0, 0]))
            .save(&red)
            .unwrap();
        image::RgbImage::from_pixel(48, 64, image::Rgb([0, 0, 255]))
            .save(&blue)
            .unwrap();
        let missing = dir.join("missing.png");

        let config = SlideshowConfig {
            image_secs: 2.0,
            fade_secs: 0.5,
        };
        let mut show = Slideshow::new(renderer, vec![red, missing, blue], (32, 32), config);

        // The first photo fades in from black once loaded.
        update_until(&mut show, 0.0, |show| show.current.is_some());
        assert_eq!(opacities(&show), [0.0]);
        assert_eq!(show.layers()[0].texture.size(), (32, 24));
        show.update(0.25);
        assert_eq!(opacities(&show), [0.5]);

        // The missing file is skipped and the next photo is preloaded.
        update_until(&mut show, 1.0, |show| show.ready.is_some());
        assert_eq!(opacities(&show), [1.0]);
        assert_eq!(show.stats().failed_items, 1);

        // Crossfade starts at 2.0 - 0.5 and both photos stay visible during it.
        show.update(1.5);
        assert_eq!(opacities(&show), [1.0, 0.0]);
        show.update(1.75);
        assert_eq!(opacities(&show), [0.5, 0.5]);
        show.update(2.0);
        assert_eq!(opacities(&show), [1.0]);
        assert_eq!(show.stats().current, Some(2));
        assert_eq!(show.stats().transitions, 1);
        assert_eq!(show.stats().late_transitions, 0);

        // If the next photo is not ready in time, the current one stays on screen.
        let (_never_sent, result) = mpsc::channel();
        show.ready = None;
        show.loading = Some(Loading {
            index: 0,
            since: Instant::now(),
            result,
        });
        show.update(3.6);
        assert_eq!(opacities(&show), [1.0]);
        show.loading = None;
        update_until(&mut show, 3.8, |show| show.fade.is_some());
        assert_eq!(show.stats().late_transitions, 1);
        assert!(show.stats().longest_wait_ms >= 799.0);

        // Let the preload thread finish before the test ends.
        update_until(&mut show, 4.0, |show| show.loading.is_none());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn video_frames_follow_playback_time() {
        let Some(renderer) = crate::test_renderer() else {
            return;
        };
        let Ok(ffmpeg) = Ffmpeg::locate() else {
            eprintln!("skipping video test: ffmpeg not found");
            return;
        };
        let dir = std::env::temp_dir().join(format!("fluxlide-video-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let clip = dir.join("clip.mp4");
        let status = std::process::Command::new(ffmpeg.ffmpeg_path())
            .args(["-v", "error", "-y", "-f", "lavfi", "-i"])
            .arg("testsrc2=size=160x90:rate=30")
            .args(["-t", "1", "-pix_fmt", "yuv420p"])
            .arg(&clip)
            .status()
            .unwrap();
        if !status.success() {
            eprintln!("skipping video test: could not create a clip");
            return;
        }

        let mut player = VideoPlayer::open(&renderer, &ffmpeg, &clip, (80, 80)).unwrap();
        // Shrunk to fit 80x80 with even dimensions for 4:2:0.
        let (width, height) = player.texture().size();
        assert!(width == 80 && height % 2 == 0 && (44..=46).contains(&height));
        assert!((player.duration().unwrap() - 1.0).abs() < 0.05);

        // Give the decoder time to fill its buffer, then jump ahead.
        std::thread::sleep(Duration::from_millis(200));
        let update = player.update(&renderer, 0.1);
        assert_eq!((update.late, update.skipped), (false, 2));
        for step in 0..50 {
            player.update(&renderer, 0.1 + f64::from(step) * 0.05);
            if player.has_ended() {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(player.has_ended());

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
