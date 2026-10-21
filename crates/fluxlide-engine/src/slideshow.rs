//! A looping playlist of photos and videos joined by transitions.
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
//! through crossfades. With a [`Ducker`] attached, each video with sound is
//! announced to the BGM as soon as the time it will start is known, so the
//! BGM can be faded out before it.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::Instant;

use crate::audio::{AudioFormat, AudioMixer, AudioStats, VoiceHandle};
use crate::bgm::{DuckWindow, Ducker};
use crate::media::audio::AudioTrack;
use crate::media::video::{Ffmpeg, VideoPlayer};
use crate::media::{MediaKind, MediaTexture, load_image};
use crate::{
    Caption, CaptionStyle, EngineError, Fit, Frame, Layer, Overlay, OverlayRef, Renderer,
    Transition, TransitionRun, Watermark,
};

#[derive(Debug, Clone, Copy)]
pub struct SlideshowConfig {
    /// How long a photo stays, including the transitions at both ends.
    pub image_secs: f64,
    /// Length of a transition. Shortened for items shorter than twice this.
    pub fade_secs: f64,
    /// How pictures whose aspect ratio differs from the output are placed.
    pub fit: Fit,
}

impl Default for SlideshowConfig {
    fn default() -> Self {
        Self {
            image_secs: 5.0,
            fade_secs: 1.0,
            fit: Fit::default(),
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

    fn has_sound(&self) -> bool {
        matches!(self, Self::Video { audio: Some(_), .. })
    }

    /// The span of playback time the item's sound plays when the item
    /// starts at `start` (endless while the length is not known).
    fn sound_span(&self, start: f64) -> (f64, f64) {
        let end = match self {
            Self::Video { player, .. } => player.duration().map_or(f64::INFINITY, |d| start + d),
            Self::Image(_) => start,
        };
        (start, end)
    }
}

struct Showing {
    index: usize,
    media: Prepared,
    caption: Option<Overlay>,
    /// Playback time at which this item started fading in.
    started: f64,
    /// The item's sound while it plays.
    voice: Option<VoiceHandle>,
    /// Keeps the BGM down while the item's sound plays.
    _duck: Option<DuckWindow>,
}

/// The announcement of the ready item's sound, made before it starts.
struct Announced {
    window: DuckWindow,
    span: (f64, f64),
}

impl Showing {
    fn layer(&self, fit: Fit) -> Layer<'_> {
        let mut layer = Layer::new(self.media.texture());
        layer.fit = fit;
        layer
    }
}

struct Loading {
    index: usize,
    since: Instant,
    result: Receiver<Result<(Prepared, Option<Overlay>), EngineError>>,
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
    ready: Option<(usize, Prepared, Option<Overlay>)>,
    next_index: usize,
    failures_in_row: usize,
    /// Time the current transition became due while the next item was not ready.
    waiting_since: Option<f64>,
    now: f64,
    stats: SlideshowStats,
    audio: Option<AudioMixer>,
    ducker: Option<Ducker>,
    /// For the ready item, when it has sound and a ducker is attached.
    announced: Option<Announced>,
    /// False to play the playlist once and fade to black after the last item.
    looping: bool,
    /// Every item of the single pass has been handed to a loader.
    pass_loaded: bool,
    /// The video frame due at the last update was not decoded yet.
    video_late: bool,
    transition: Transition,
    /// Captions by item index; items past the end have none.
    captions: Vec<Caption>,
    caption_style: CaptionStyle,
    watermark: Option<(Watermark, Overlay)>,
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
            ducker: None,
            announced: None,
            looping: true,
            pass_loaded: false,
            video_late: false,
            transition: Transition::fade(),
            captions: Vec::new(),
            caption_style: CaptionStyle::default(),
            watermark: None,
        }
    }

    /// Sets how items are joined. Takes effect from the next transition.
    pub fn set_transition(&mut self, transition: Transition) {
        self.transition = transition;
    }

    /// Sets the caption of each item, in playlist order. Items loaded
    /// before this call keep the caption they were loaded with.
    pub fn set_captions(&mut self, captions: Vec<Caption>, style: CaptionStyle) {
        self.captions = captions;
        self.caption_style = style;
    }

    /// Shows `watermark` over every item, or removes it with `None`.
    pub fn set_watermark(&mut self, watermark: Option<Watermark>) -> Result<(), EngineError> {
        self.watermark = match watermark {
            Some(watermark) => {
                let overlay = Overlay::watermark(&self.renderer, &watermark, self.output_size)?;
                Some((watermark, overlay))
            }
            None => None,
        };
        Ok(())
    }

    /// Tells the slideshow the output changed size. Captions and the
    /// watermark are laid out again for it; pictures keep the size they
    /// were loaded at.
    pub fn set_output_size(&mut self, output_size: (u32, u32)) -> Result<(), EngineError> {
        if output_size == self.output_size {
            return Ok(());
        }
        self.output_size = output_size;
        if let Some((watermark, overlay)) = &mut self.watermark {
            *overlay = Overlay::watermark(&self.renderer, watermark, output_size)?;
        }
        let relayout = |index: usize| -> Result<Option<Overlay>, EngineError> {
            caption_overlay(
                &self.renderer,
                self.captions.get(index),
                &self.caption_style,
                output_size,
            )
        };
        let current = match &self.current {
            Some(showing) => Some(relayout(showing.index)?),
            None => None,
        };
        let outgoing = match &self.outgoing {
            Some(showing) => Some(relayout(showing.index)?),
            None => None,
        };
        let ready = match &self.ready {
            Some((index, ..)) => Some(relayout(*index)?),
            None => None,
        };
        if let (Some(showing), Some(caption)) = (&mut self.current, current) {
            showing.caption = caption;
        }
        if let (Some(showing), Some(caption)) = (&mut self.outgoing, outgoing) {
            showing.caption = caption;
        }
        if let (Some((_, _, slot)), Some(caption)) = (&mut self.ready, ready) {
            *slot = caption;
        }
        Ok(())
    }

    /// Plays the sound of videos through `mixer`. `mixer` must count
    /// playback time from the same instant as the times given to
    /// [`Slideshow::update`]. Items loaded before this stay silent.
    pub fn set_audio(&mut self, mixer: AudioMixer) {
        self.audio = Some(mixer);
    }

    /// Plays the playlist once instead of in a loop: after the last item the
    /// picture fades to black and [`Slideshow::is_finished`] turns true.
    /// Call before the first update.
    pub fn play_once(&mut self) {
        self.looping = false;
    }

    /// True once a single pass ([`Slideshow::play_once`]) has faded to black.
    pub fn is_finished(&self) -> bool {
        !self.looping
            && (self.pass_loaded || self.failures_in_row >= self.items.len())
            && self.current.is_none()
            && self.outgoing.is_none()
            && self.ready.is_none()
            && self.loading.is_none()
    }

    /// True when the last update could not show what is due at its time
    /// because an item or a video frame was still being prepared. Live
    /// playback carries on regardless; export updates again at the same
    /// time until this is false, so the file never shows a delay.
    pub fn is_waiting(&self) -> bool {
        let first_pending = self.current.is_none()
            && self.outgoing.is_none()
            && (self.loading.is_some() || self.ready.is_some());
        self.video_late || self.waiting_since.is_some() || first_pending
    }

    /// Announces the sound of videos to the BGM through `ducker`, so it is
    /// faded out while they play.
    pub fn set_ducker(&mut self, ducker: Ducker) {
        self.ducker = Some(ducker);
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

        self.video_late = false;
        for showing in self.current.iter_mut().chain(self.outgoing.iter_mut()) {
            if let Prepared::Video { player, .. } = &mut showing.media {
                let update = player.update(&self.renderer, now - showing.started);
                self.video_late |= update.late;
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
            .and_then(|(_, media, _)| media.duration(&self.config));
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
        // The next item starts at the latest when due, or once it is ready.
        self.announce_ready(due.max(now));
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
        } else if !self.looping && self.pass_loaded {
            // The single pass is over: fade the last item out to black.
            self.outgoing = self.current.take();
            self.fade = Some(Fade {
                start: now,
                secs: fade_secs,
            });
        }
    }

    /// Tells the BGM that the ready item's sound will start at `start`.
    /// Only sent again when the time changes, as when the item is late.
    fn announce_ready(&mut self, start: f64) {
        let (Some(ducker), Some((_, media, _))) = (&self.ducker, &self.ready) else {
            return;
        };
        if !start.is_finite() || !media.has_sound() {
            return;
        }
        let span = media.sound_span(start);
        match &mut self.announced {
            Some(announced) if announced.span == span => {}
            Some(announced) => {
                announced.window.set(span.0, span.1);
                announced.span = span;
            }
            None => {
                self.announced = Some(Announced {
                    window: ducker.open_window(span.0, span.1),
                    span,
                });
            }
        }
    }

    /// What to draw for the time given to the last [`Slideshow::update`].
    pub fn frame(&self) -> Frame<'_> {
        let progress = self.fade_progress();
        let fit = self.config.fit;
        let mut overlays = Vec::with_capacity(3);
        // While pictures change, the old caption fades out in the first
        // half and the new one in during the second, so the two never overlap.
        let (outgoing_opacity, incoming_opacity) = if self.fade.is_some() {
            (
                (1.0 - 2.0 * progress).max(0.0),
                (2.0 * progress - 1.0).max(0.0),
            )
        } else {
            (0.0, 1.0)
        };
        for (showing, opacity) in [
            (&self.outgoing, outgoing_opacity),
            (&self.current, incoming_opacity),
        ] {
            if let Some(overlay) = showing.as_ref().and_then(|s| s.caption.as_ref()) {
                overlays.push(OverlayRef { overlay, opacity });
            }
        }
        if let Some((_, overlay)) = &self.watermark {
            overlays.push(OverlayRef {
                overlay,
                opacity: 1.0,
            });
        }
        Frame {
            from: self.outgoing.as_ref().map(|showing| showing.layer(fit)),
            to: self.current.as_ref().map(|showing| showing.layer(fit)),
            transition: self.fade.is_some().then_some(TransitionRun {
                transition: &self.transition,
                progress,
            }),
            overlays,
        }
    }

    /// How far the current transition has gone, from 0 to 1 (1 when none).
    fn fade_progress(&self) -> f32 {
        let progress = match &self.fade {
            Some(fade) if fade.secs > 0.0 => ((self.now - fade.start) / fade.secs).clamp(0.0, 1.0),
            _ => 1.0,
        };
        progress as f32
    }

    fn begin_transition(&mut self, fade_secs: f64) {
        let Some((index, mut media, caption)) = self.ready.take() else {
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
        let duck = media.has_sound().then(|| {
            let (start, end) = media.sound_span(self.now);
            match (self.announced.take(), &self.ducker) {
                (Some(announced), _) => {
                    announced.window.set(start, end);
                    Some(announced.window)
                }
                (None, Some(ducker)) => Some(ducker.open_window(start, end)),
                (None, None) => None,
            }
        });
        self.outgoing = self.current.take();
        self.current = Some(Showing {
            index,
            media,
            caption,
            started: self.now,
            voice,
            _duck: duck.flatten(),
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
                Ok((media, mut caption)) => {
                    self.stats.last_load_ms = since.elapsed().as_secs_f64() * 1000.0;
                    self.failures_in_row = 0;
                    // Laid out for an older size if the output changed while loading.
                    if caption
                        .as_ref()
                        .is_some_and(|overlay| overlay.size() != self.output_size)
                    {
                        caption = caption_overlay(
                            &self.renderer,
                            self.captions.get(index),
                            &self.caption_style,
                            self.output_size,
                        )
                        .unwrap_or_else(|err| {
                            self.stats.last_error = Some(err.to_string());
                            None
                        });
                    }
                    self.ready = Some((index, media, caption));
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
        if self.items.is_empty() || self.failures_in_row >= self.items.len() || self.pass_loaded {
            return;
        }
        let index = self.next_index;
        self.next_index = (index + 1) % self.items.len();
        self.pass_loaded = !self.looping && self.next_index == 0;
        let path = self.items[index].clone();
        let renderer = self.renderer.clone();
        let ffmpeg = self.ffmpeg.clone();
        let output_size = self.output_size;
        let audio = self.audio.as_ref().map(AudioMixer::format);
        let caption = self.captions.get(index).cloned();
        let caption_style = self.caption_style;
        let (tx, result) = mpsc::channel();
        let spawned = std::thread::Builder::new()
            .name("fluxlide-preload".into())
            .spawn(move || {
                let loaded =
                    prepare(&renderer, ffmpeg, &path, output_size, audio).and_then(|media| {
                        let caption = caption_overlay(
                            &renderer,
                            caption.as_ref(),
                            &caption_style,
                            output_size,
                        )?;
                        Ok((media, caption))
                    });
                let _ = tx.send(loaded);
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

/// The overlay for an item's caption, laid out for `output_size`.
fn caption_overlay(
    renderer: &Renderer,
    caption: Option<&Caption>,
    style: &CaptionStyle,
    output_size: (u32, u32),
) -> Result<Option<Overlay>, EngineError> {
    match caption {
        Some(caption) => Overlay::caption(renderer, caption, style, output_size),
        None => Ok(None),
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

    /// Opacity of each picture drawn: the outgoing one fades out as the
    /// incoming one fades in.
    fn opacities(show: &Slideshow) -> Vec<f32> {
        let frame = show.frame();
        let progress = frame.transition.map_or(1.0, |run| run.progress);
        frame
            .from
            .iter()
            .map(|_| 1.0 - progress)
            .chain(frame.to.iter().map(|_| progress))
            .collect()
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
            ..Default::default()
        };
        let mut show = Slideshow::new(renderer, vec![red, missing, blue], (32, 32), config);

        // The first photo fades in from black once loaded.
        update_until(&mut show, 0.0, |show| show.current.is_some());
        assert_eq!(opacities(&show), [0.0]);
        assert_eq!(show.frame().to.unwrap().texture.size(), (32, 24));
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
    fn videos_with_sound_are_announced_to_the_bgm_before_they_start() {
        let Some(renderer) = crate::test_renderer() else {
            return;
        };
        let Ok(ffmpeg) = Ffmpeg::locate() else {
            eprintln!("skipping duck test: ffmpeg not found");
            return;
        };
        let dir = std::env::temp_dir().join(format!("fluxlide-duck-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let photo = dir.join("photo.png");
        image::RgbImage::from_pixel(32, 32, image::Rgb([255, 0, 0]))
            .save(&photo)
            .unwrap();
        let clip = dir.join("clip.mp4");
        let status = std::process::Command::new(ffmpeg.ffmpeg_path())
            .args(["-v", "error", "-y", "-f", "lavfi", "-i"])
            .arg("testsrc2=size=64x36:rate=30:duration=1")
            .args(["-f", "lavfi", "-i", "sine=frequency=440:duration=1"])
            .args(["-pix_fmt", "yuv420p", "-c:a", "aac", "-shortest"])
            .arg(&clip)
            .status()
            .unwrap();
        if !status.success() {
            eprintln!("skipping duck test: could not create a clip");
            return;
        }

        let config = SlideshowConfig {
            image_secs: 2.0,
            fade_secs: 0.5,
            ..Default::default()
        };
        let mut show = Slideshow::new(renderer, vec![photo, clip], (32, 32), config);
        let mixer = crate::audio::OfflineMixer::export(48_000);
        show.set_audio(mixer.mixer());
        let (ducker, mut probe) = crate::bgm::DuckProbe::new();
        show.set_ducker(ducker);

        update_until(&mut show, 0.0, |show| show.current.is_some());
        update_until(&mut show, 1.0, |show| show.ready.is_some());
        // Announced as soon as it is ready, for the time its fade in is due.
        let windows = probe.windows();
        assert_eq!(windows.len(), 1);
        let (start, end) = windows[0];
        assert_eq!(start, 1.5);
        assert!((end - start - 1.0).abs() < 0.05, "{windows:?}");

        // Started late: the window moves to the actual start.
        show.update(1.6);
        assert_eq!(show.stats().current, Some(1));
        let windows = probe.windows();
        assert_eq!(windows.len(), 1);
        assert_eq!(windows[0].0, 1.6);

        // Gone once the video has faded out.
        update_until(&mut show, 2.0, |show| show.ready.is_some());
        show.update(2.2);
        show.update(3.0);
        assert_eq!(show.stats().current, Some(0));
        assert!(show.outgoing.is_none());
        assert_eq!(probe.windows(), []);

        update_until(&mut show, 3.0, |show| show.loading.is_none());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn captions_and_the_watermark_follow_the_transition() {
        let Some(renderer) = crate::test_renderer() else {
            return;
        };
        let dir = std::env::temp_dir().join(format!("fluxlide-captions-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let first = dir.join("first.png");
        let second = dir.join("second.png");
        for path in [&first, &second] {
            image::RgbImage::from_pixel(64, 36, image::Rgb([90, 120, 200]))
                .save(path)
                .unwrap();
        }
        let config = SlideshowConfig {
            image_secs: 2.0,
            fade_secs: 0.5,
            fit: Fit::Blur,
        };
        let mut show = Slideshow::new(renderer, vec![first, second], (320, 180), config);
        let caption = |title: &str| Caption {
            title: title.into(),
            ..Default::default()
        };
        show.set_captions(
            vec![caption("one"), caption("two")],
            CaptionStyle::default(),
        );
        show.set_transition(Transition::slide(crate::Direction::Left));
        show.set_watermark(Some(Watermark::text(
            "FLUXLIDE",
            crate::Corner::TopRight,
            0.5,
        )))
        .unwrap();

        // Overlay opacities of the current frame, back to front.
        let overlays = |show: &Slideshow| -> Vec<f32> {
            show.frame().overlays.iter().map(|o| o.opacity).collect()
        };
        update_until(&mut show, 0.0, |show| show.current.is_some());
        // Fading in: the caption waits for the second half, the watermark never moves.
        show.update(0.125);
        assert_eq!(overlays(&show), [0.0, 1.0]);
        show.update(0.375);
        assert_eq!(overlays(&show), [0.5, 1.0]);
        update_until(&mut show, 1.0, |show| show.ready.is_some());
        assert_eq!(overlays(&show), [1.0, 1.0]);
        assert_eq!(show.frame().overlays[1].overlay.opacity(), 0.5);

        // Changing items: the old caption fades out first, then the new one in.
        show.update(1.5);
        show.update(1.5 + 0.125);
        assert_eq!(overlays(&show), [0.5, 0.0, 1.0]);
        show.update(1.5 + 0.375);
        assert_eq!(overlays(&show), [0.0, 0.5, 1.0]);
        let frame = show.frame();
        assert_eq!(frame.transition.unwrap().transition.name(), "slide");
        drop(frame);
        show.update(2.0);
        assert_eq!(overlays(&show), [1.0, 1.0]);
        assert!(show.frame().transition.is_none());

        // A new output size lays the captions and the watermark out again,
        // including the caption of an item that is still loading.
        assert!(show.loading.is_some() || show.ready.is_some());
        show.set_output_size((640, 360)).unwrap();
        update_until(&mut show, 2.0, |show| show.ready.is_some());
        let (_, _, ready_caption) = show.ready.as_ref().unwrap();
        assert_eq!(ready_caption.as_ref().unwrap().size(), (640, 360));
        let frame = show.frame();
        assert!(
            frame
                .overlays
                .iter()
                .all(|o| o.overlay.size() == (640, 360))
        );
        drop(frame);

        update_until(&mut show, 2.2, |show| show.loading.is_none());
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
