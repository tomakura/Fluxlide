//! Plays a [`ShowDefinition`] by the times of its [`Timeline`].
//!
//! Each slide's timing comes from the timeline: when it starts to come in,
//! how long its transition in takes, when the next one replaces it and when
//! it is gone. Live playback, the preview and export all play through this
//! type, so they agree on those times.
//!
//! While one slide is on screen, the next one is loaded on a background
//! thread: photos are decoded and uploaded, videos are probed and decoded up
//! to a few frames ahead. A slide starts only once it is ready, so the output
//! never falls back to black between slides. If it is not ready when due, the
//! current slide stays (a finished video holds its last frame) and every
//! later slide moves back by the time waited; the delay is counted in
//! [`SlideshowStats`]. Export waits with its clock stopped, so its file
//! keeps the timeline's times exactly.
//!
//! # Live control
//!
//! For a presentation, the operator can move to the next or the previous
//! slide, pause, black out the output, and advance by hand only
//! ([`Advance::Manual`]). Pausing stops the playback clock: the picture and
//! a video's frame stay, its sound stops, and when playback resumes every
//! later slide comes that much later. The mixer's clock does not stop, so
//! playback time and mixer time differ by the time spent paused. With a
//! title card, the show can wait on it before the first slide and end on
//! it after the last.
//!
//! A video plays once its transition in is over, and ends before the next
//! slide comes in; through the transitions it stays on its first or last
//! frame. With an [`AudioMixer`] attached, its sound plays with its picture
//! on the same playback clock; if the operator moves on before the video
//! ends, the sound fades out with the picture. With a [`Ducker`] attached,
//! each video with sound is announced to the BGM as soon as the time it
//! will start is known, so the BGM can be faded out before it.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::Instant;

use crate::audio::{AudioFormat, AudioMixer, AudioStats, VoiceHandle};
use crate::bgm::{DuckWindow, Ducker, Rng, random_seed};
use crate::media::audio::AudioTrack;
use crate::media::video::{Ffmpeg, PlayRange, VideoPlayer};
use crate::media::{MediaTexture, load_image};
use crate::show::{
    Advance, AssetKind, AtEnd, Look, Order, ShowDefinition, Slide, SlideId, Timeline,
    TimelineEntry, TransitionChoice, TransitionId, WatermarkSource, WatermarkSpec,
};
use crate::{
    Caption, CaptionStyle, EngineError, Frame, Layer, Overlay, OverlayRef, Renderer, Transform,
    Transition, TransitionRun, Watermark, WatermarkContent,
};

/// Measurements for judging whether playback is seamless.
#[derive(Debug, Default, Clone)]
pub struct SlideshowStats {
    /// The slide coming in or on screen.
    pub current: Option<SlideId>,
    pub transitions: u64,
    /// Transitions that started late because the next slide was not loaded.
    pub late_transitions: u64,
    pub longest_wait_ms: f64,
    /// Time the most recent slide took to load, from request to ready.
    pub last_load_ms: f64,
    /// Rendered frames where the due video frame was not decoded yet.
    pub video_late_frames: u64,
    /// Decoded video frames that were never shown.
    pub video_skipped_frames: u64,
    pub failed_items: u64,
    /// Slides the last timeline left out (see [`Timeline::skipped`]).
    pub skipped_slides: usize,
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

    fn has_sound(&self) -> bool {
        matches!(self, Self::Video { audio: Some(_), .. })
    }
}

/// A slide loaded and waiting for its time.
struct Ready {
    /// Position of `entry` in its pass.
    index: usize,
    entry: TimelineEntry,
    slide: Slide,
    media: Prepared,
    caption: Option<Overlay>,
}

struct Showing {
    index: usize,
    entry: TimelineEntry,
    slide: Slide,
    media: Prepared,
    caption: Option<Overlay>,
    transition: Transition,
    /// Playback time of the entry's `start_ms`.
    anchor: f64,
    /// The slide's sound while it plays.
    voice: Option<VoiceHandle>,
    /// Keeps the BGM down while the slide's sound plays.
    _duck: Option<DuckWindow>,
}

impl Showing {
    /// Playback time at which a video starts to play.
    fn play_at(&self) -> f64 {
        self.anchor + ms_to_secs(u64::from(self.entry.enter_ms))
    }

    /// Playback time at which the next slide is due to come in.
    fn leave_at(&self) -> f64 {
        self.anchor + ms_to_secs(self.entry.leave_ms - self.entry.start_ms)
    }

    /// How far the transition in has gone at `now`, from 0 to 1.
    fn enter_progress(&self, now: f64) -> f32 {
        if self.entry.enter_ms == 0 {
            return 1.0;
        }
        ((now - self.anchor) / ms_to_secs(u64::from(self.entry.enter_ms))).clamp(0.0, 1.0) as f32
    }

    /// The picture at `now`, placed by its framing and slow zoom. A photo
    /// without a slow zoom of its own takes the look's.
    fn layer(&self, now: f64, look: &Look) -> Layer<'_> {
        let framing = self.slide.framing;
        let mut transform = Transform {
            scale: framing.scale,
            offset: framing.offset,
        };
        let motion = match self.media {
            Prepared::Image(_) => self.slide.motion.or(look.motion),
            Prepared::Video { .. } => self.slide.motion,
        };
        if let Some(motion) = motion {
            let span = ms_to_secs(self.entry.end_ms - self.entry.start_ms);
            let t = if span > 0.0 {
                ((now - self.anchor) / span).clamp(0.0, 1.0) as f32
            } else {
                1.0
            };
            let lerp = |from: f32, to: f32| from + (to - from) * t;
            transform.scale *= lerp(motion.from_scale, motion.to_scale);
            for axis in 0..2 {
                transform.offset[axis] += lerp(motion.from_offset[axis], motion.to_offset[axis]);
            }
        }
        let mut layer = Layer::new(self.media.texture());
        layer.fit = framing.fit.unwrap_or(look.fit);
        layer.transform = transform;
        layer
    }

    fn volume(&self) -> f32 {
        self.slide.video.volume
    }
}

/// The announcement of the ready slide's sound, made before it starts.
struct Announced {
    window: DuckWindow,
    span: (f64, f64),
}

struct Loading {
    index: usize,
    entry: TimelineEntry,
    slide: Slide,
    since: Instant,
    result: Receiver<Result<(Prepared, Option<Overlay>), EngineError>>,
}

/// The last slide fading out to black at the end of a single pass.
struct FadeOut {
    start: f64,
    secs: f64,
}

/// What follows the last slide of a pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PassEnd {
    /// The next pass starts at the last slide's `leave_ms`.
    Loop,
    /// The last slide stays on screen.
    Hold,
    /// The last slide fades to black and playback finishes.
    FadeOut,
}

pub struct Slideshow {
    renderer: Arc<Renderer>,
    ffmpeg: Result<Ffmpeg, String>,
    show: ShowDefinition,
    /// The managed folder the assets' file names are in.
    media_dir: PathBuf,
    output_size: (u32, u32),
    /// The show's custom transitions that compiled.
    custom_transitions: HashMap<TransitionId, Transition>,
    caption_style: CaptionStyle,
    watermark: Option<(Watermark, Overlay)>,
    pass_end: PassEnd,
    rng: Rng,
    /// The pass being played, and the index of its next entry to load.
    pass: Timeline,
    next_entry: usize,
    current: Option<Showing>,
    outgoing: Option<Showing>,
    fade_out: Option<FadeOut>,
    loading: Option<Loading>,
    ready: Option<Ready>,
    /// When the first slide is due: the time of the first update.
    first_due: Option<f64>,
    failures_in_row: usize,
    /// Playback time of the first update at which the next slide was due
    /// but not ready.
    waiting_since: Option<f64>,
    finished: bool,
    now: f64,
    stats: SlideshowStats,
    audio: Option<AudioMixer>,
    ducker: Option<Ducker>,
    /// For the ready slide, when it has sound and a ducker is attached.
    announced: Option<Announced>,
    /// The video frame due at the last update was not decoded yet.
    video_late: bool,
    advance_mode: Advance,
    /// The time given to the last update, on the mixer's clock.
    real: f64,
    /// Mixer time at which playback was paused.
    paused_at: Option<f64>,
    /// Mixer time spent paused so far: mixer time minus playback time.
    paused_total: f64,
    /// The operator asked for the next slide: it comes in as soon as it is ready.
    advance_now: bool,
    /// Where the first slide starts, and whether it starts fully visible.
    first_entered: bool,
    card: Option<Overlay>,
    /// Waiting on the title card for [`Slideshow::start`].
    held: bool,
    /// The first slide is coming in from the title card.
    from_card: bool,
    /// Playback time at which the show started to go to the title card at the end.
    ending: Option<f64>,
    blackout: bool,
    blackout_overlay: Overlay,
    blackout_from: f32,
    blackout_started: f64,
}

/// Where a presentation is, for the operator's screen.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Position {
    /// On the title card, before the first slide.
    pub before_start: bool,
    /// On (or going to) the title card after the last slide.
    pub at_end: bool,
    pub current: Option<SlideId>,
    /// The slides that come next, as far as they are known.
    pub upcoming: Vec<SlideId>,
    pub paused: bool,
    pub blackout: bool,
}

impl Slideshow {
    /// Plays `show`, whose assets' files are in `media_dir`. `output_size` is
    /// the size of the output in pixels; larger photos and videos are shrunk
    /// to it when loaded. A custom transition that does not compile is
    /// replaced by a fade and a watermark that cannot be made is left out;
    /// both are reported in [`SlideshowStats::last_error`].
    pub fn new(
        renderer: Arc<Renderer>,
        show: ShowDefinition,
        media_dir: PathBuf,
        output_size: (u32, u32),
    ) -> Self {
        let mut stats = SlideshowStats::default();
        let mut custom_transitions = HashMap::new();
        for custom in &show.look.custom_transitions {
            match Transition::from_wgsl(&renderer, &custom.name, &custom.wgsl) {
                Ok(transition) => {
                    custom_transitions.insert(custom.id, transition.with_params(custom.params));
                }
                Err(err) => stats.last_error = Some(err.to_string()),
            }
        }
        let caption_style = CaptionStyle {
            design: show.look.caption.design,
            text_scale: show.look.caption.text_scale,
        };
        let pass_end = match show.playback.at_end {
            AtEnd::Loop => PassEnd::Loop,
            AtEnd::Stop => PassEnd::Hold,
        };
        let mut slideshow = Self {
            blackout_overlay: Overlay::black(&renderer, output_size),
            renderer,
            ffmpeg: Ffmpeg::locate().map_err(|err| err.to_string()),
            show,
            media_dir,
            output_size,
            custom_transitions,
            caption_style,
            watermark: None,
            pass_end,
            rng: Rng(random_seed()),
            pass: Timeline {
                entries: Vec::new(),
                skipped: Vec::new(),
                end_ms: 0,
            },
            next_entry: 0,
            current: None,
            outgoing: None,
            fade_out: None,
            loading: None,
            ready: None,
            first_due: None,
            failures_in_row: 0,
            waiting_since: None,
            finished: false,
            now: 0.0,
            stats,
            audio: None,
            ducker: None,
            announced: None,
            video_late: false,
            advance_mode: Advance::Auto,
            real: 0.0,
            paused_at: None,
            paused_total: 0.0,
            advance_now: false,
            first_entered: false,
            card: None,
            held: false,
            from_card: false,
            ending: None,
            blackout: false,
            blackout_from: 0.0,
            blackout_started: 0.0,
        };
        slideshow.advance_mode = slideshow.show.playback.advance;
        slideshow.pass = slideshow.build_pass();
        if let Some(spec) = slideshow.show.look.watermark.clone() {
            slideshow.watermark = slideshow
                .make_watermark(&spec)
                .map_err(|err| slideshow.stats.last_error = Some(err.to_string()))
                .ok();
        }
        slideshow
    }

    /// Lays out the watermark of `spec` for the output.
    fn make_watermark(&self, spec: &WatermarkSpec) -> Result<(Watermark, Overlay), EngineError> {
        let content = match &spec.content {
            WatermarkSource::Text { text } => WatermarkContent::Text(text.clone()),
            WatermarkSource::Image { asset } => {
                let asset = self.show.asset(*asset).ok_or_else(|| {
                    EngineError::Media("the watermark's logo is not in the show".into())
                })?;
                let path = self.media_dir.join(&asset.file);
                WatermarkContent::Image(Arc::new(load_image(
                    &self.renderer,
                    &path,
                    self.output_size,
                )?))
            }
        };
        let watermark = Watermark {
            content,
            corner: spec.corner,
            opacity: spec.opacity,
            size: spec.size,
        };
        let overlay = Overlay::watermark(&self.renderer, &watermark, self.output_size)?;
        Ok((watermark, overlay))
    }

    /// The timeline of the next pass, in the show's order.
    fn build_pass(&mut self) -> Timeline {
        let mut order: Vec<usize> = (0..self.show.slides.len()).collect();
        if self.show.playback.order == Order::Shuffle {
            // Fisher-Yates.
            for index in (1..order.len()).rev() {
                let other = self.rng.below(index + 1);
                order.swap(index, other);
            }
        }
        let pass = Timeline::build(&self.show, &order);
        self.stats.skipped_slides = pass.skipped.len();
        pass
    }

    /// Tells the slideshow the output changed size. Captions and the
    /// watermark are laid out again for it; pictures keep the size they
    /// were loaded at.
    pub fn set_output_size(&mut self, output_size: (u32, u32)) -> Result<(), EngineError> {
        if output_size == self.output_size {
            return Ok(());
        }
        self.output_size = output_size;
        self.blackout_overlay = Overlay::black(&self.renderer, output_size);
        if let Some((watermark, overlay)) = &mut self.watermark {
            *overlay = Overlay::watermark(&self.renderer, watermark, output_size)?;
        }
        let relayout = |slide: &Slide| self.caption_overlay(slide);
        let current = self
            .current
            .as_ref()
            .map(|s| relayout(&s.slide))
            .transpose()?;
        let outgoing = self
            .outgoing
            .as_ref()
            .map(|s| relayout(&s.slide))
            .transpose()?;
        let ready = self
            .ready
            .as_ref()
            .map(|r| relayout(&r.slide))
            .transpose()?;
        if let (Some(showing), Some(caption)) = (&mut self.current, current) {
            showing.caption = caption;
        }
        if let (Some(showing), Some(caption)) = (&mut self.outgoing, outgoing) {
            showing.caption = caption;
        }
        if let (Some(ready), Some(caption)) = (&mut self.ready, ready) {
            ready.caption = caption;
        }
        Ok(())
    }

    /// Plays the sound of videos through `mixer`. `mixer` must count
    /// playback time from the same instant as the times given to
    /// [`Slideshow::update`]. Slides loaded before this stay silent.
    pub fn set_audio(&mut self, mixer: AudioMixer) {
        self.audio = Some(mixer);
    }

    /// Plays one pass, whatever the show's playback settings say: after the
    /// last slide the picture fades to black and [`Slideshow::is_finished`]
    /// turns true. Call before the first update.
    pub fn play_once(&mut self) {
        self.pass_end = PassEnd::FadeOut;
    }

    /// True once a single pass ([`Slideshow::play_once`]) has faded to black,
    /// or had nothing to play.
    pub fn is_finished(&self) -> bool {
        self.finished
    }

    /// Shows `card` (see [`Overlay::title_card`]) while waiting to start
    /// ([`Slideshow::hold_start`]) and after the last slide.
    pub fn set_title_card(&mut self, card: Overlay) {
        self.card = Some(card);
    }

    /// Waits on the title card until [`Slideshow::start`] (or
    /// [`Slideshow::next`]). The first slide is loaded meanwhile. Call
    /// before the first update.
    pub fn hold_start(&mut self) {
        self.held = true;
    }

    /// Ends the wait of [`Slideshow::hold_start`]: the first slide comes in
    /// from the title card as soon as it is ready.
    pub fn start(&mut self) {
        if self.held {
            self.held = false;
            self.from_card = self.card.is_some();
            self.first_due = Some(self.playback_time());
        }
    }

    /// Plays from slide `index` of the list (or the first usable one after
    /// it) instead of the first. With `entered`, that slide is already fully
    /// visible at the start rather than coming in from black. Call before
    /// the first update.
    pub fn start_at(&mut self, index: usize, entered: bool) {
        let from: Vec<SlideId> = self.show.slides.iter().skip(index).map(|s| s.id).collect();
        if let Some(position) = from.iter().find_map(|id| {
            self.pass
                .entries
                .iter()
                .position(|entry| entry.slide == *id)
        }) {
            self.next_entry = position;
        }
        self.first_entered = entered;
    }

    /// Brings in the next slide now (from the title card, the first). After
    /// the last slide of a show that does not loop, goes to the title card.
    pub fn next(&mut self) {
        if self.held {
            self.start();
            return;
        }
        if self.ending.is_some() || self.finished {
            return;
        }
        self.resume();
        let no_more = self.ready.is_none()
            && self.loading.is_none()
            && self.next_entry >= self.pass.entries.len()
            && self.pass_end != PassEnd::Loop;
        if no_more && self.current.is_some() {
            if self.card.is_some() {
                self.ending = Some(self.playback_time());
            }
        } else {
            self.advance_now = true;
        }
    }

    /// Goes back to the slide before the current one (from the title card
    /// at the end, back to the last slide).
    pub fn previous(&mut self) {
        if self.ending.take().is_some() || self.held {
            return;
        }
        let Some(current) = &self.current else {
            return;
        };
        let target = match current.index.checked_sub(1) {
            Some(target) => target,
            None if self.pass_end == PassEnd::Loop => self.pass.entries.len().saturating_sub(1),
            None => return,
        };
        self.resume();
        self.ready = None;
        self.loading = None;
        self.announced = None;
        self.waiting_since = None;
        self.failures_in_row = 0;
        self.next_entry = target;
        self.start_loading();
        self.advance_now = true;
    }

    /// Stops the playback clock. The sound of a video stops with it.
    pub fn pause(&mut self) {
        if self.paused_at.is_some() || self.held || self.finished {
            return;
        }
        self.paused_at = Some(self.real);
        for showing in self.current.iter_mut().chain(self.outgoing.iter_mut()) {
            showing.voice = None;
            showing._duck = None;
        }
        self.announced = None;
    }

    /// Starts the playback clock again where it stopped. A video's sound
    /// starts again from where its picture is.
    pub fn resume(&mut self) {
        let Some(at) = self.paused_at.take() else {
            return;
        };
        self.paused_total += self.real - at;
        let now = self.playback_time();
        let Some(current) = &self.current else {
            return;
        };
        // Paused during the transition in, the sound still starts with the
        // video; paused while it played, it goes on from there.
        let played = (now - current.play_at()).max(0.0);
        let remaining = play_secs(&current.entry) - played;
        let (Some(mixer), Ok(ffmpeg), Some(asset)) = (
            &self.audio,
            &self.ffmpeg,
            self.show.asset(current.entry.asset),
        ) else {
            return;
        };
        if !current.media.has_sound() || remaining <= 0.05 {
            return;
        }
        let range = PlayRange {
            start: ms_to_secs(u64::from(current.entry.video_start_ms.unwrap_or(0))) + played,
            length: Some(remaining),
        };
        let path = self.media_dir.join(&asset.file);
        let track = AudioTrack::open(ffmpeg, &path, mixer.format(), range);
        let start = self.mixer_time(now.max(current.play_at()));
        let end = self.mixer_time(current.leave_at());
        let duck = self
            .ducker
            .as_ref()
            .map(|ducker| ducker.open_window(start, end));
        let mixer = mixer.clone();
        let current = self.current.as_mut().expect("checked above");
        match track {
            Ok(mut track) => {
                current.voice = track.take_samples().map(|(samples, finished)| {
                    mixer.play(samples, finished, start, current.volume())
                });
                current._duck = duck;
                if let Prepared::Video { audio, .. } = &mut current.media {
                    *audio = Some(track);
                }
            }
            Err(err) => self.stats.last_error = Some(err.to_string()),
        }
    }

    pub fn is_paused(&self) -> bool {
        self.paused_at.is_some()
    }

    /// Fades the entire output to black (or back) over 400 ms while playback
    /// goes on. The fade uses real time, so it also works while paused.
    pub fn set_blackout(&mut self, blackout: bool) {
        if self.blackout == blackout {
            return;
        }
        self.blackout_from = self.blackout_opacity();
        self.blackout_started = self.real;
        self.blackout = blackout;
    }

    fn blackout_opacity(&self) -> f32 {
        let t = ((self.real - self.blackout_started) / 0.4).clamp(0.0, 1.0) as f32;
        let eased = t * t * (3.0 - 2.0 * t);
        let target = if self.blackout { 1.0 } else { 0.0 };
        self.blackout_from + (target - self.blackout_from) * eased
    }

    /// Where the show is, with up to `count` upcoming slides.
    pub fn position(&self, count: usize) -> Position {
        let mut upcoming: Vec<SlideId> = self
            .ready
            .as_ref()
            .map(|ready| ready.entry.slide)
            .or(self.loading.as_ref().map(|loading| loading.entry.slide))
            .into_iter()
            .collect();
        let rest = self.pass.entries.iter().skip(self.next_entry);
        // The next pass is known only when it is not shuffled.
        let again = (self.pass_end == PassEnd::Loop && self.show.playback.order == Order::AsListed)
            .then(|| self.pass.entries.iter().cycle())
            .into_iter()
            .flatten();
        upcoming.extend(rest.chain(again).map(|entry| entry.slide).take(count));
        upcoming.truncate(count);
        Position {
            before_start: self.held,
            at_end: self.ending.is_some(),
            current: self.current.as_ref().map(|showing| showing.slide.id),
            upcoming,
            paused: self.is_paused(),
            blackout: self.blackout,
        }
    }

    /// Playback time now: mixer time less the time spent paused.
    fn playback_time(&self) -> f64 {
        self.paused_at.unwrap_or(self.real) - self.paused_total
    }

    fn mixer_time(&self, playback: f64) -> f64 {
        playback + self.paused_total
    }

    /// True when the last update could not show what is due at its time
    /// because a slide or a video frame was still being prepared. Live
    /// playback carries on regardless; export updates again at the same
    /// time until this is false, so the file never shows a delay.
    pub fn is_waiting(&self) -> bool {
        self.video_late || self.waiting_since.is_some()
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

    /// File of the slide coming in or on screen.
    pub fn current_path(&self) -> Option<PathBuf> {
        let asset = self.show.asset(self.current.as_ref()?.entry.asset)?;
        Some(self.media_dir.join(&asset.file))
    }

    /// Advances playback to `now` seconds: starts loading, starts and ends
    /// transitions, uploads the video frames due at this time and sets the
    /// volume of each sound to its picture's opacity.
    pub fn update(&mut self, now: f64) {
        self.real = now;
        self.advance(self.playback_time());
        let progress = self.transition_progress();
        if let Some(showing) = &self.outgoing
            && let Some(voice) = &showing.voice
        {
            voice.set_gain((1.0 - progress) * showing.volume());
        }
        if let Some(showing) = &self.current
            && let Some(voice) = &showing.voice
        {
            voice.set_gain(progress * showing.volume());
        }
    }

    fn advance(&mut self, now: f64) {
        self.now = now;
        let first_due = match self.held {
            true => f64::INFINITY,
            false => *self.first_due.get_or_insert(now),
        };
        self.poll_loading();

        self.video_late = false;
        for showing in self.current.iter_mut().chain(self.outgoing.iter_mut()) {
            let play_at = showing.play_at();
            if let Prepared::Video { player, .. } = &mut showing.media {
                let update = player.update(&self.renderer, now - play_at);
                self.video_late |= update.late;
                self.stats.video_late_frames += u64::from(update.late);
                self.stats.video_skipped_frames += u64::from(update.skipped);
            }
        }

        if self.paused_at.is_some() {
            return;
        }
        if let Some(fade) = &self.fade_out {
            if now - fade.start >= fade.secs {
                self.fade_out = None;
                self.outgoing = None;
                self.finished = true;
            }
            return;
        }
        if let Some(current) = &self.current
            && current.enter_progress(now) >= 1.0
        {
            self.outgoing = None;
            self.from_card = false;
        }

        let due = match &self.current {
            _ if self.advance_now => now,
            Some(_) if self.advance_mode == Advance::Manual || self.ending.is_some() => {
                f64::INFINITY
            }
            Some(current) => current.leave_at(),
            None => first_due,
        };
        // The next slide starts at the latest when due, or once it is ready.
        self.announce_ready(due.max(now));
        if now < due {
            return;
        }
        if self.ready.is_some() {
            // Every later slide moves back by the playback time spent waiting.
            // Export waits at one time, so its slides stay where they belong.
            let waited = self.waiting_since.take().map_or(0.0, |since| now - since);
            // The first slide always waits for its load, and so does one the
            // operator asked for; only slides due by the timeline are late.
            if waited > 0.0 && self.current.is_some() && !self.advance_now {
                self.stats.late_transitions += 1;
                self.stats.longest_wait_ms = self.stats.longest_wait_ms.max(waited * 1000.0);
            }
            self.begin_slide(due + waited);
        } else if self.loading.is_some() {
            self.waiting_since.get_or_insert(now);
        } else {
            // Nothing more to play.
            self.waiting_since = None;
            match (self.pass_end, self.current.take()) {
                (PassEnd::FadeOut, Some(last)) => {
                    let secs = ms_to_secs(last.entry.end_ms - last.entry.leave_ms);
                    self.outgoing = Some(last);
                    self.fade_out = Some(FadeOut { start: due, secs });
                }
                (PassEnd::FadeOut, None) => self.finished = self.outgoing.is_none(),
                (_, last) => self.current = last,
            }
        }
    }

    /// Tells the BGM that the ready slide's sound will start at `start`.
    /// Only sent again when the time changes, as when the slide is late.
    fn announce_ready(&mut self, start: f64) {
        let (Some(ducker), Some(ready)) = (&self.ducker, &self.ready) else {
            return;
        };
        if !ready.media.has_sound() {
            return;
        }
        let start = self.mixer_time(start + ms_to_secs(u64::from(ready.entry.enter_ms)));
        // Not due yet (held on the title card): a window that never closes
        // would be kept by the ducker forever.
        if !start.is_finite() {
            return;
        }
        let span = (start, start + play_secs(&ready.entry));
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
        let opacity = self.blackout_opacity();
        if opacity >= 1.0 {
            return Frame::default();
        }
        let mut frame = self.content_frame();
        if opacity > 0.0 {
            frame.overlays.push(OverlayRef {
                overlay: &self.blackout_overlay,
                opacity,
            });
        }
        frame
    }

    fn content_frame(&self) -> Frame<'_> {
        let empty = Frame {
            from: None,
            to: None,
            transition: None,
            overlays: Vec::new(),
        };
        let card = |opacity: f32| {
            self.card
                .as_ref()
                .filter(|_| opacity > 0.0)
                .map(|overlay| OverlayRef { overlay, opacity })
        };
        if self.held {
            return Frame {
                overlays: card(1.0).into_iter().collect(),
                ..empty
            };
        }
        if let Some(start) = self.ending {
            return self.ending_frame(start, card);
        }
        let progress = self.transition_progress();
        let in_transition = progress < 1.0;
        let mut overlays = Vec::with_capacity(3);
        // While pictures change, the old caption fades out in the first
        // half and the new one in during the second, so the two never overlap.
        let (outgoing_opacity, incoming_opacity) = if in_transition {
            (
                (1.0 - 2.0 * progress).max(0.0),
                (2.0 * progress - 1.0).max(0.0),
            )
        } else {
            (0.0, 1.0)
        };
        // The outgoing slide is kept until the update after its transition ends.
        let outgoing = self.outgoing.as_ref().filter(|_| in_transition);
        for (showing, opacity) in [
            (outgoing, outgoing_opacity),
            (self.current.as_ref(), incoming_opacity),
        ] {
            if let Some(overlay) = showing.and_then(|s| s.caption.as_ref()) {
                overlays.push(OverlayRef { overlay, opacity });
            }
        }
        if let Some((_, overlay)) = &self.watermark {
            overlays.push(OverlayRef {
                overlay,
                opacity: 1.0,
            });
        }
        // Coming in from the title card, the card stays until the first
        // slide is loaded, then fades out like a caption.
        if self.from_card && self.current.is_none() {
            overlays.extend(card(1.0));
        } else if self.from_card && in_transition {
            overlays.extend(card((1.0 - 2.0 * progress).max(0.0)));
        }
        let transition = if self.fade_out.is_some() {
            Some(&*FADE_TO_BLACK)
        } else {
            self.current.as_ref().map(|showing| &showing.transition)
        };
        Frame {
            from: outgoing.map(|s| s.layer(self.now, &self.show.look)),
            to: self
                .current
                .as_ref()
                .map(|s| s.layer(self.now, &self.show.look)),
            transition: transition
                .filter(|_| in_transition)
                .map(|transition| TransitionRun {
                    transition,
                    progress,
                }),
            overlays,
        }
    }

    /// The last slide fading to black while the title card comes up, from
    /// playback time `start`.
    fn ending_frame<'a>(
        &'a self,
        start: f64,
        card: impl Fn(f32) -> Option<OverlayRef<'a>>,
    ) -> Frame<'a> {
        let secs = ms_to_secs(u64::from(self.show.look.to_black_ms()));
        let progress = if secs > 0.0 {
            ((self.now - start) / secs).clamp(0.0, 1.0) as f32
        } else {
            1.0
        };
        let current = self.current.as_ref().filter(|_| progress < 1.0);
        let mut overlays: Vec<OverlayRef<'_>> = current
            .and_then(|showing| showing.caption.as_ref())
            .map(|overlay| OverlayRef {
                overlay,
                opacity: (1.0 - 2.0 * progress).max(0.0),
            })
            .into_iter()
            .collect();
        if let Some((_, overlay)) = self.watermark.as_ref().filter(|_| progress < 1.0) {
            overlays.push(OverlayRef {
                overlay,
                opacity: 1.0 - progress,
            });
        }
        overlays.extend(card((2.0 * progress - 1.0).max(0.0)));
        Frame {
            from: current.map(|showing| showing.layer(self.now, &self.show.look)),
            to: None,
            transition: current.map(|_| TransitionRun {
                transition: &FADE_TO_BLACK,
                progress,
            }),
            overlays,
        }
    }

    /// How far the transition running now has gone, from 0 to 1 (1 when none).
    fn transition_progress(&self) -> f32 {
        if let Some(fade) = &self.fade_out {
            return if fade.secs > 0.0 {
                ((self.now - fade.start) / fade.secs).clamp(0.0, 1.0) as f32
            } else {
                1.0
            };
        }
        self.current
            .as_ref()
            .map_or(1.0, |current| current.enter_progress(self.now))
    }

    /// Puts the ready slide on screen with its `start_ms` at `anchor`.
    fn begin_slide(&mut self, mut anchor: f64) {
        let Some(Ready {
            index,
            entry,
            slide,
            mut media,
            caption,
        }) = self.ready.take()
        else {
            return;
        };
        self.advance_now = false;
        if std::mem::take(&mut self.first_entered) && self.current.is_none() {
            // Already fully visible: as if it had come in just before.
            anchor -= ms_to_secs(u64::from(entry.enter_ms));
        }
        // The sound starts when the video starts to play.
        let play_at = self.mixer_time(anchor + ms_to_secs(u64::from(entry.enter_ms)));
        let volume = slide.video.volume;
        let voice = match (&mut media, &self.audio) {
            (
                Prepared::Video {
                    audio: Some(track), ..
                },
                Some(mixer),
            ) => track
                .take_samples()
                .map(|(samples, finished)| mixer.play(samples, finished, play_at, volume)),
            _ => None,
        };
        let duck = media.has_sound().then(|| {
            let (start, end) = (play_at, play_at + play_secs(&entry));
            match (self.announced.take(), &self.ducker) {
                (Some(announced), _) => {
                    announced.window.set(start, end);
                    Some(announced.window)
                }
                (None, Some(ducker)) => Some(ducker.open_window(start, end)),
                (None, None) => None,
            }
        });
        let transition = self.transition_for(entry.enter);
        self.outgoing = self.current.take();
        if self.outgoing.is_some() {
            self.stats.transitions += 1;
        }
        self.stats.current = Some(slide.id);
        self.current = Some(Showing {
            index,
            entry,
            slide,
            media,
            caption,
            transition,
            anchor,
            voice,
            _duck: duck.flatten(),
        });
        self.start_loading();
    }

    fn transition_for(&self, choice: TransitionChoice) -> Transition {
        match choice {
            // A cut has no length, so its transition is never drawn.
            TransitionChoice::Cut | TransitionChoice::Fade => Transition::fade(),
            TransitionChoice::Slide { direction } => Transition::slide(direction),
            TransitionChoice::Wipe { direction } => Transition::wipe(direction),
            TransitionChoice::Zoom => Transition::zoom(),
            TransitionChoice::Paw => Transition::paw(),
            TransitionChoice::Custom { id } => self
                .custom_transitions
                .get(&id)
                .cloned()
                .unwrap_or_else(Transition::fade),
        }
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
            let Some(Loading {
                index,
                entry,
                slide,
                since,
                ..
            }) = self.loading.take()
            else {
                return;
            };
            match result {
                Ok((media, mut caption)) => {
                    self.stats.last_load_ms = since.elapsed().as_secs_f64() * 1000.0;
                    self.failures_in_row = 0;
                    // Laid out for an older size if the output changed while loading.
                    if caption
                        .as_ref()
                        .is_some_and(|overlay| overlay.size() != self.output_size)
                    {
                        caption = self.caption_overlay(&slide).unwrap_or_else(|err| {
                            self.stats.last_error = Some(err.to_string());
                            None
                        });
                    }
                    self.ready = Some(Ready {
                        index,
                        entry,
                        slide,
                        media,
                        caption,
                    });
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

    /// The next entry to play: the rest of this pass, then the next pass
    /// when the show loops.
    fn next_entry(&mut self) -> Option<(usize, TimelineEntry)> {
        if self.next_entry >= self.pass.entries.len() {
            if self.pass_end != PassEnd::Loop || self.pass.entries.is_empty() {
                return None;
            }
            self.pass = self.build_pass();
            self.next_entry = 0;
        }
        let index = self.next_entry;
        let entry = *self.pass.entries.get(index)?;
        self.next_entry += 1;
        Some((index, entry))
    }

    /// Starts loading the next slide. A slide that fails to load is skipped
    /// and the one after it takes its place. Stops trying once every slide
    /// of a pass has failed in a row.
    fn start_loading(&mut self) {
        if self.failures_in_row >= self.pass.entries.len().max(1) {
            return;
        }
        let Some((index, entry)) = self.next_entry() else {
            return;
        };
        let (Some(slide), Some(asset)) = (
            self.show
                .slides
                .iter()
                .find(|slide| slide.id == entry.slide),
            self.show.asset(entry.asset),
        ) else {
            return;
        };
        let slide = slide.clone();
        let path = self.media_dir.join(&asset.file);
        let kind = asset.kind;
        let range = PlayRange {
            start: ms_to_secs(u64::from(entry.video_start_ms.unwrap_or(0))),
            length: Some(play_secs(&entry)),
        };
        let renderer = self.renderer.clone();
        let ffmpeg = self.ffmpeg.clone();
        let output_size = self.output_size;
        let audio = self.audio.as_ref().map(AudioMixer::format);
        let caption = self.caption(&slide);
        let caption_style = self.caption_style;
        let (tx, result) = mpsc::channel();
        let spawned = std::thread::Builder::new()
            .name("fluxlide-preload".into())
            .spawn(move || {
                let loaded = prepare(&renderer, ffmpeg, &path, kind, range, output_size, audio)
                    .and_then(|media| {
                        let caption = match &caption {
                            Some(caption) => {
                                Overlay::caption(&renderer, caption, &caption_style, output_size)?
                            }
                            None => None,
                        };
                        Ok((media, caption))
                    });
                let _ = tx.send(loaded);
            });
        match spawned {
            Ok(_) => {
                self.loading = Some(Loading {
                    index,
                    entry,
                    slide,
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

    /// The caption of `slide`: its asset's title, author and description.
    fn caption(&self, slide: &Slide) -> Option<Caption> {
        if !slide.show_caption {
            return None;
        }
        let meta = &self.show.asset(slide.asset)?.meta;
        let caption = Caption {
            title: meta.title.clone(),
            author: meta.author.clone(),
            description: meta.description.clone(),
        };
        (!caption.is_empty()).then_some(caption)
    }

    /// The overlay for `slide`'s caption, laid out for the output.
    fn caption_overlay(&self, slide: &Slide) -> Result<Option<Overlay>, EngineError> {
        match self.caption(slide) {
            Some(caption) => Overlay::caption(
                &self.renderer,
                &caption,
                &self.caption_style,
                self.output_size,
            ),
            None => Ok(None),
        }
    }
}

/// The transition from the last slide to black at the end of a single pass.
static FADE_TO_BLACK: std::sync::LazyLock<Transition> = std::sync::LazyLock::new(Transition::fade);

fn ms_to_secs(ms: u64) -> f64 {
    ms as f64 / 1000.0
}

/// How long a video plays: from the end of its transition in until the
/// next slide comes in.
fn play_secs(entry: &TimelineEntry) -> f64 {
    ms_to_secs(entry.leave_ms - entry.play_start_ms())
}

fn prepare(
    renderer: &Renderer,
    ffmpeg: Result<Ffmpeg, String>,
    path: &Path,
    kind: AssetKind,
    range: PlayRange,
    output_size: (u32, u32),
    audio: Option<AudioFormat>,
) -> Result<Prepared, EngineError> {
    match kind {
        AssetKind::Photo => load_image(renderer, path, output_size).map(Prepared::Image),
        AssetKind::Video => {
            let ffmpeg = ffmpeg.map_err(EngineError::Media)?;
            let player = VideoPlayer::open(renderer, &ffmpeg, path, output_size, range)?;
            let audio = match audio {
                Some(format) if player.has_audio() => {
                    Some(AudioTrack::open(&ffmpeg, path, format, range)?)
                }
                _ => None,
            };
            Ok(Prepared::Video {
                player: Box::new(player),
                audio,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::show::{
        Asset, AssetId, AssetMeta, AssetState, Framing, Motion, Playback, VideoOptions,
    };
    use crate::{Corner, Direction, Fit};
    use std::time::Duration;

    /// A show of `files`, one slide each, with photos shown for 1 s and
    /// joined by 0.5 s fades. A video's length is `Some`.
    fn show_of(files: &[(&str, Option<u32>)]) -> ShowDefinition {
        let mut show = ShowDefinition::default();
        for (index, (file, video_ms)) in files.iter().enumerate() {
            let id = index as u64 + 1;
            show.assets.push(Asset {
                id: AssetId(id),
                kind: if video_ms.is_some() {
                    AssetKind::Video
                } else {
                    AssetKind::Photo
                },
                file: (*file).into(),
                original_name: (*file).into(),
                state: AssetState::Ready,
                meta: AssetMeta::default(),
                size: None,
                duration_ms: *video_ms,
                has_audio: video_ms.is_some(),
            });
            show.slides.push(Slide::new(SlideId(id), AssetId(id)));
        }
        show.look.photo_ms = 1_000;
        show.look.transition_ms = Some(500);
        show
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("fluxlide-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn save_photo(dir: &Path, name: &str, size: (u32, u32)) {
        image::RgbImage::from_pixel(size.0, size.1, image::Rgb([90, 120, 200]))
            .save(dir.join(name))
            .unwrap();
    }

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

    /// Updates to `now` as export does: again at the same time while waiting.
    fn update_exactly(show: &mut Slideshow, now: f64) {
        update_until(show, now, |show| !show.is_waiting());
    }

    /// Opacity of each picture drawn: the outgoing one fades out as the
    /// incoming one fades in.
    fn opacities(show: &Slideshow) -> Vec<f32> {
        let frame = show.frame();
        let progress = frame.transition.map_or(1.0, |run| run.progress);
        let from = frame.from.iter().filter(|_| progress < 1.0);
        from.map(|_| 1.0 - progress)
            .chain(frame.to.iter().map(|_| progress))
            .collect()
    }

    #[test]
    fn slides_follow_the_timeline_and_wait_when_late() {
        let Some(renderer) = crate::test_renderer() else {
            return;
        };
        let dir = temp_dir("slideshow");
        save_photo(&dir, "red.png", (64, 48));
        save_photo(&dir, "blue.jpg", (48, 64));
        let show = show_of(&[("red.png", None), ("missing.png", None), ("blue.jpg", None)]);
        let mut show = Slideshow::new(renderer, show, dir.clone(), (32, 32));

        // The first photo fades in from black, from time 0.
        update_exactly(&mut show, 0.0);
        assert_eq!(opacities(&show), [0.0]);
        assert_eq!(show.frame().to.unwrap().texture.size(), (32, 24));
        show.update(0.25);
        assert_eq!(opacities(&show), [0.5]);

        // The missing file is skipped and the next photo is preloaded.
        update_until(&mut show, 1.0, |show| show.ready.is_some());
        assert_eq!(opacities(&show), [1.0]);
        assert_eq!(show.stats().failed_items, 1);

        // Fully visible for 1 s after its 0.5 s fade in, then the next comes in.
        show.update(1.5);
        assert_eq!(opacities(&show), [1.0, 0.0]);
        show.update(1.75);
        assert_eq!(opacities(&show), [0.5, 0.5]);
        show.update(2.0);
        assert_eq!(opacities(&show), [1.0]);
        assert_eq!(show.stats().current, Some(SlideId(3)));
        assert_eq!(show.stats().transitions, 1);
        assert_eq!(show.stats().late_transitions, 0);

        // The show loops: the first photo is due again at 3.0. If it is not
        // ready then, the current one stays on screen...
        update_until(&mut show, 2.5, |show| show.ready.is_some());
        let (_never_sent, result) = mpsc::channel();
        let ready = show.ready.take().unwrap();
        show.loading = Some(Loading {
            index: ready.index,
            entry: ready.entry,
            slide: ready.slide.clone(),
            since: Instant::now(),
            result,
        });
        show.update(3.0);
        show.update(3.4);
        assert!(show.is_waiting());
        assert_eq!(opacities(&show), [1.0]);
        // ...and once it is, it comes in from the start, 0.6 s late.
        show.loading = None;
        show.ready = Some(ready);
        show.update(3.6);
        assert_eq!(show.stats().current, Some(SlideId(1)));
        assert_eq!(opacities(&show), [1.0, 0.0]);
        assert_eq!(show.stats().late_transitions, 1);
        assert!((show.stats().longest_wait_ms - 600.0).abs() < 1e-6);
        // Every later time moves back with it.
        assert_eq!(show.current.as_ref().unwrap().leave_at(), 3.6 + 1.5);

        // Let the preload thread finish before the test ends.
        update_until(&mut show, 4.0, |show| show.loading.is_none());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_single_pass_fades_to_black_and_a_stopped_show_holds_the_last_slide() {
        let Some(renderer) = crate::test_renderer() else {
            return;
        };
        let dir = temp_dir("pass-end");
        save_photo(&dir, "a.png", (32, 32));
        save_photo(&dir, "b.png", (32, 32));
        let mut definition = show_of(&[("a.png", None), ("b.png", None)]);
        // A cut into the second photo.
        definition.slides[1].transition = Some(TransitionChoice::Cut);

        // Timeline: a from 0 (cut at 1.5), b from 1.5 to 2.5, then 0.5 s to black.
        let mut show = Slideshow::new(renderer.clone(), definition.clone(), dir.clone(), (32, 32));
        show.play_once();
        update_exactly(&mut show, 0.0);
        update_exactly(&mut show, 1.5);
        assert_eq!(show.stats().current, Some(SlideId(2)));
        assert_eq!(opacities(&show), [1.0]);
        update_exactly(&mut show, 2.5);
        update_exactly(&mut show, 2.75);
        assert!(show.frame().to.is_none());
        assert_eq!(opacities(&show), [0.5]);
        assert!(!show.is_finished());
        update_exactly(&mut show, 3.0);
        assert!(show.is_finished());

        // Stopping at the end keeps the last photo on screen.
        definition.playback = Playback {
            at_end: AtEnd::Stop,
            ..Default::default()
        };
        let mut show = Slideshow::new(renderer, definition, dir.clone(), (32, 32));
        update_exactly(&mut show, 0.0);
        update_exactly(&mut show, 1.5);
        update_exactly(&mut show, 10.0);
        assert_eq!(show.stats().current, Some(SlideId(2)));
        assert_eq!(opacities(&show), [1.0]);
        assert!(!show.is_finished());

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn captions_watermark_transitions_and_framing_come_from_the_show() {
        let Some(renderer) = crate::test_renderer() else {
            return;
        };
        let dir = temp_dir("captions");
        for name in ["first.png", "second.png", "third.png"] {
            save_photo(&dir, name, (64, 36));
        }
        let mut definition = show_of(&[
            ("first.png", None),
            ("second.png", None),
            ("third.png", None),
        ]);
        for (asset, title) in definition.assets.iter_mut().zip(["one", "two", "three"]) {
            asset.meta.title = title.into();
        }
        definition.slides[1].transition = Some(TransitionChoice::Slide {
            direction: Direction::Left,
        });
        definition.slides[2].show_caption = false;
        // The first photo is on screen from 0 to 2.0 and zooms in over that time.
        definition.slides[0].framing = Framing {
            fit: Some(Fit::Cover),
            scale: 2.0,
            offset: [0.1, 0.0],
        };
        definition.slides[0].motion = Some(Motion {
            from_scale: 1.0,
            to_scale: 1.2,
            from_offset: [0.0, 0.0],
            to_offset: [0.0, 0.2],
        });
        definition.look.watermark = Some(WatermarkSpec {
            content: WatermarkSource::Text {
                text: "FLUXLIDE".into(),
            },
            corner: Corner::TopRight,
            opacity: 0.5,
            size: 0.045,
        });
        let mut show = Slideshow::new(renderer, definition, dir.clone(), (320, 180));

        // Overlay opacities of the current frame, back to front.
        let overlays = |show: &Slideshow| -> Vec<f32> {
            show.frame().overlays.iter().map(|o| o.opacity).collect()
        };
        update_exactly(&mut show, 0.0);
        // Fading in: the caption waits for the second half, the watermark never moves.
        show.update(0.125);
        assert_eq!(overlays(&show), [0.0, 1.0]);
        show.update(0.375);
        assert_eq!(overlays(&show), [0.5, 1.0]);
        update_until(&mut show, 1.0, |show| show.ready.is_some());
        assert_eq!(overlays(&show), [1.0, 1.0]);
        assert_eq!(show.frame().overlays[1].overlay.opacity(), 0.5);

        // Halfway through its time on screen, halfway through its zoom.
        let frame = show.frame();
        let layer = frame.to.as_ref().unwrap();
        assert_eq!(layer.fit, Fit::Cover);
        assert!((layer.transform.scale - 2.2).abs() < 1e-6);
        assert!((layer.transform.offset[0] - 0.1).abs() < 1e-6);
        assert!((layer.transform.offset[1] - 0.1).abs() < 1e-6);
        drop(frame);

        // The second slide slides in: the old caption fades out first, then
        // the new one in.
        show.update(1.5);
        show.update(1.5 + 0.125);
        assert_eq!(overlays(&show), [0.5, 0.0, 1.0]);
        show.update(1.5 + 0.375);
        assert_eq!(overlays(&show), [0.0, 0.5, 1.0]);
        let frame = show.frame();
        assert_eq!(frame.transition.unwrap().transition.name(), "slide");
        assert_eq!(frame.to.as_ref().unwrap().fit, Fit::Blur);
        drop(frame);
        show.update(2.0);
        assert_eq!(overlays(&show), [1.0, 1.0]);
        assert!(show.frame().transition.is_none());

        // A new output size lays the captions and the watermark out again,
        // including the caption of a slide that is still loading.
        assert!(show.loading.is_some() || show.ready.is_some());
        show.set_output_size((640, 360)).unwrap();
        update_until(&mut show, 2.0, |show| show.ready.is_some());
        let frame = show.frame();
        assert!(
            frame
                .overlays
                .iter()
                .all(|o| o.overlay.size() == (640, 360))
        );
        drop(frame);

        // The third slide has its caption turned off.
        update_exactly(&mut show, 3.5);
        assert_eq!(show.stats().current, Some(SlideId(3)));
        assert_eq!(overlays(&show), [1.0]);

        update_until(&mut show, 3.5, |show| show.loading.is_none());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn videos_play_their_range_and_are_announced_to_the_bgm() {
        let Some(renderer) = crate::test_renderer() else {
            return;
        };
        let Ok(ffmpeg) = Ffmpeg::locate() else {
            eprintln!("skipping duck test: ffmpeg not found");
            return;
        };
        let dir = temp_dir("duck");
        save_photo(&dir, "photo.png", (32, 32));
        let status = std::process::Command::new(ffmpeg.ffmpeg_path())
            .args(["-v", "error", "-y", "-f", "lavfi", "-i"])
            .arg("testsrc2=size=64x36:rate=30:duration=2")
            .args(["-f", "lavfi", "-i", "sine=frequency=440:duration=2"])
            .args(["-pix_fmt", "yuv420p", "-c:a", "aac", "-shortest"])
            .arg(dir.join("clip.mp4"))
            .status()
            .unwrap();
        if !status.success() {
            eprintln!("skipping duck test: could not create a clip");
            return;
        }

        // 1 s of the 2 s clip, from 0.5 s, at half volume.
        let mut definition = show_of(&[("photo.png", None), ("clip.mp4", Some(2_000))]);
        definition.slides[1].video = VideoOptions {
            start_ms: 500,
            end_ms: Some(1_500),
            volume: 0.5,
        };
        let mut show = Slideshow::new(renderer, definition, dir.clone(), (32, 32));
        let mixer = crate::audio::OfflineMixer::export(48_000);
        show.set_audio(mixer.mixer());
        let (ducker, mut probe) = crate::bgm::DuckProbe::new();
        show.set_ducker(ducker);

        update_exactly(&mut show, 0.0);
        update_until(&mut show, 1.0, |show| show.ready.is_some());
        // Announced as soon as it is ready, for the time its range plays:
        // after its transition in (1.5 to 2.0).
        assert_eq!(probe.windows(), [(2.0, 3.0)]);
        let Some(Ready {
            media: Prepared::Video { player, .. },
            ..
        }) = &show.ready
        else {
            panic!("the video is not ready");
        };
        assert!((player.duration().unwrap() - 1.0).abs() < 0.05);

        // Updated a little after its time, it still starts on time.
        show.update(1.6);
        assert_eq!(show.stats().current, Some(SlideId(2)));
        assert_eq!(show.current.as_ref().unwrap().anchor, 1.5);
        assert_eq!(probe.windows(), [(2.0, 3.0)]);

        // The photo comes back in once the range has played, at 3.0, and
        // the video is gone at 3.5.
        update_until(&mut show, 2.9, |show| show.ready.is_some());
        show.update(3.2);
        show.update(3.5);
        assert_eq!(show.stats().current, Some(SlideId(1)));
        assert!(show.outgoing.is_none());
        assert_eq!(probe.windows(), []);

        update_until(&mut show, 3.5, |show| show.loading.is_none());
        // Stops the decoders, which keep the clip open on Windows.
        drop(show);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Three photos, a.png to c.png, in a show of `playback`.
    fn three_photos(name: &str, playback: Playback) -> (PathBuf, ShowDefinition) {
        let dir = temp_dir(name);
        for photo in ["a.png", "b.png", "c.png"] {
            save_photo(&dir, photo, (32, 32));
        }
        let mut show = show_of(&[("a.png", None), ("b.png", None), ("c.png", None)]);
        show.playback = playback;
        (dir, show)
    }

    fn current(show: &Slideshow) -> Option<SlideId> {
        show.position(0).current
    }

    #[test]
    fn a_manual_show_moves_only_when_asked_and_can_go_back() {
        let Some(renderer) = crate::test_renderer() else {
            return;
        };
        let (dir, definition) = three_photos(
            "manual",
            Playback {
                advance: Advance::Manual,
                ..Default::default()
            },
        );
        let mut show = Slideshow::new(renderer, definition, dir.clone(), (32, 32));
        update_exactly(&mut show, 0.0);
        update_until(&mut show, 10.0, |show| show.ready.is_some());
        assert_eq!(current(&show), Some(SlideId(1)));

        // The next slide comes in when asked, from that moment.
        show.next();
        show.update(10.0);
        assert_eq!(current(&show), Some(SlideId(2)));
        assert_eq!(opacities(&show), [1.0, 0.0]);
        show.update(10.25);
        assert_eq!(opacities(&show), [0.5, 0.5]);
        assert_eq!(show.stats().late_transitions, 0);

        // Going back loads the slide before, and comes in once it is ready.
        update_until(&mut show, 11.0, |show| show.ready.is_some());
        show.previous();
        update_until(&mut show, 11.0, |show| current(show) == Some(SlideId(1)));
        // From the first slide of a looping show, back is the last.
        update_until(&mut show, 12.0, |show| show.ready.is_some());
        show.previous();
        update_until(&mut show, 12.0, |show| current(show) == Some(SlideId(3)));

        update_until(&mut show, 13.0, |show| show.loading.is_none());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn pausing_stops_the_clock_and_later_slides_come_that_much_later() {
        let Some(renderer) = crate::test_renderer() else {
            return;
        };
        let (dir, definition) = three_photos("pause", Playback::default());
        let mut show = Slideshow::new(renderer, definition, dir.clone(), (32, 32));
        update_exactly(&mut show, 0.0);
        // Paused halfway through the first fade.
        show.update(0.25);
        show.pause();
        update_until(&mut show, 5.0, |show| show.ready.is_some());
        assert!(show.is_paused());
        assert_eq!(opacities(&show), [0.5]);

        // 4.75 s paused: the second slide is due at 1.5 + 4.75.
        show.resume();
        show.update(6.0);
        assert_eq!(current(&show), Some(SlideId(1)));
        show.update(6.25);
        assert_eq!(current(&show), Some(SlideId(2)));
        assert_eq!(opacities(&show), [1.0, 0.0]);

        // Next works while paused, and resumes playback.
        update_until(&mut show, 6.5, |show| show.ready.is_some());
        show.pause();
        show.next();
        assert!(!show.is_paused());
        show.update(6.5);
        assert_eq!(current(&show), Some(SlideId(3)));

        update_until(&mut show, 7.0, |show| show.loading.is_none());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_held_show_waits_on_its_title_card_and_ends_on_it() {
        let Some(renderer) = crate::test_renderer() else {
            return;
        };
        let (dir, definition) = three_photos(
            "title-card",
            Playback {
                at_end: AtEnd::Stop,
                ..Default::default()
            },
        );
        let card = Overlay::title_card(&renderer, "港の写真", (320, 180)).unwrap();
        let mut show = Slideshow::new(renderer, definition, dir.clone(), (320, 180));
        show.set_title_card(card);
        show.hold_start();

        // Only the card, however long it waits; the first slide is loaded.
        update_until(&mut show, 30.0, |show| show.ready.is_some());
        let frame = show.frame();
        assert!(frame.to.is_none() && frame.from.is_none());
        assert_eq!(frame.overlays.len(), 1);
        drop(frame);
        assert!(show.position(3).before_start);
        assert_eq!(
            show.position(3).upcoming,
            [SlideId(1), SlideId(2), SlideId(3)]
        );

        // Started, the first slide comes in and the card fades out.
        show.next();
        show.update(30.0);
        assert_eq!(current(&show), Some(SlideId(1)));
        assert_eq!(show.frame().overlays.last().unwrap().opacity, 1.0);
        show.update(30.5);
        assert!(show.frame().overlays.is_empty());

        // Through to the last slide, which stays.
        update_exactly(&mut show, 32.0);
        update_exactly(&mut show, 33.5);
        update_exactly(&mut show, 40.0);
        assert_eq!(current(&show), Some(SlideId(3)));

        // Next goes to the card, and back returns to the last slide.
        show.next();
        assert!(show.position(0).at_end);
        show.update(40.25);
        assert_eq!(opacities(&show), [0.5]);
        show.update(41.0);
        let frame = show.frame();
        assert!(frame.from.is_none() && frame.to.is_none());
        assert_eq!(frame.overlays.len(), 1);
        assert_eq!(frame.overlays[0].opacity, 1.0);
        drop(frame);
        show.previous();
        show.update(41.0);
        assert_eq!(opacities(&show), [1.0]);
        assert!(!show.position(0).at_end);

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_show_can_start_from_a_slide_already_shown_and_black_out() {
        let Some(renderer) = crate::test_renderer() else {
            return;
        };
        let (dir, definition) = three_photos("start-at", Playback::default());
        let mut show = Slideshow::new(renderer, definition, dir.clone(), (32, 32));
        show.start_at(1, true);
        update_exactly(&mut show, 0.0);
        assert_eq!(current(&show), Some(SlideId(2)));
        assert_eq!(opacities(&show), [1.0]);
        // Fully visible for its whole duration from the start.
        assert_eq!(show.current.as_ref().unwrap().leave_at(), 1.0);
        update_until(&mut show, 0.5, |show| show.ready.is_some());
        assert_eq!(
            show.position(4).upcoming,
            [SlideId(3), SlideId(1), SlideId(2), SlideId(3)]
        );

        show.set_blackout(true);
        // The picture is still visible at the start of the fade.
        assert!(show.frame().to.is_some());
        show.update(0.9);
        let frame = show.frame();
        assert!(frame.to.is_none() && frame.overlays.is_empty());
        drop(frame);
        // Playback goes on underneath.
        show.update(1.0);
        assert_eq!(current(&show), Some(SlideId(3)));

        update_until(&mut show, 1.5, |show| show.loading.is_none());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn blackout_fades_while_paused_and_reverses_without_a_jump() {
        let Some(renderer) = crate::test_renderer() else {
            return;
        };
        let (dir, mut definition) = three_photos("blackout-fade", Playback::default());
        definition.assets[0].meta.title = "白いキャプション".into();
        let mut show = Slideshow::new(renderer.clone(), definition, dir.clone(), (32, 32));
        show.start_at(0, true);
        update_exactly(&mut show, 0.0);
        show.pause();
        let mut target = crate::OffscreenTarget::new(&renderer, 32, 32).unwrap();
        let before = target
            .render_rgba(&renderer, &crate::Scene::Frame(show.frame()))
            .unwrap();
        show.set_blackout(true);
        assert_eq!(show.blackout_opacity(), 0.0);
        show.update(0.2);
        assert!((show.blackout_opacity() - 0.5).abs() < 0.001);
        let middle = target
            .render_rgba(&renderer, &crate::Scene::Frame(show.frame()))
            .unwrap();
        assert!(middle.iter().zip(&before).any(|(m, b)| *m > 0 && m < b));
        show.update(0.4);
        let black = target
            .render_rgba(&renderer, &crate::Scene::Frame(show.frame()))
            .unwrap();
        assert!(
            black
                .as_chunks::<4>()
                .0
                .iter()
                .all(|p| *p == [0, 0, 0, 255])
        );
        show.set_blackout(false);
        show.update(0.6);
        let opacity = show.blackout_opacity();
        assert!((opacity - 0.5).abs() < 0.001);
        show.set_blackout(true);
        assert_eq!(show.blackout_opacity(), opacity);
        show.update(0.8);
        assert!((show.blackout_opacity() - 0.75).abs() < 0.001);
        show.set_blackout(false);
        show.update(1.2);
        assert!(show.blackout_opacity() < 0.001);
        let restored = target
            .render_rgba(&renderer, &crate::Scene::Frame(show.frame()))
            .unwrap();
        assert_eq!(restored, before);
        assert_eq!(current(&show), Some(SlideId(1)));
        show.set_output_size((48, 32)).unwrap();
        assert_eq!(show.blackout_overlay.size(), (48, 32));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn photos_without_a_slow_zoom_of_their_own_take_the_looks() {
        let Some(renderer) = crate::test_renderer() else {
            return;
        };
        let (dir, mut definition) = three_photos("look-motion", Playback::default());
        definition.look.motion = Some(Motion {
            from_scale: 1.0,
            to_scale: 1.2,
            from_offset: [0.0, 0.0],
            to_offset: [0.0, 0.0],
        });
        // The second photo stays still.
        definition.slides[1].motion = Some(Motion {
            from_scale: 1.0,
            to_scale: 1.0,
            from_offset: [0.0, 0.0],
            to_offset: [0.0, 0.0],
        });
        let mut show = Slideshow::new(renderer, definition, dir.clone(), (32, 32));
        let scale = |show: &Slideshow| show.frame().to.as_ref().unwrap().transform.scale;
        update_exactly(&mut show, 0.0);
        // The first photo is on screen from 0 to 2.0: halfway at 1.0.
        update_until(&mut show, 1.0, |show| show.ready.is_some());
        assert!((scale(&show) - 1.1).abs() < 1e-6, "{}", scale(&show));
        update_exactly(&mut show, 2.5);
        assert_eq!(current(&show), Some(SlideId(2)));
        assert_eq!(scale(&show), 1.0);

        update_until(&mut show, 2.5, |show| show.loading.is_none());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_paused_video_is_silent_and_its_sound_picks_up_where_its_picture_is() {
        let Some(renderer) = crate::test_renderer() else {
            return;
        };
        let Ok(ffmpeg) = Ffmpeg::locate() else {
            eprintln!("skipping pause test: ffmpeg not found");
            return;
        };
        let dir = temp_dir("pause-video");
        let status = std::process::Command::new(ffmpeg.ffmpeg_path())
            .args(["-v", "error", "-y", "-f", "lavfi", "-i"])
            .arg("testsrc2=size=64x36:rate=30:duration=2")
            .args(["-f", "lavfi", "-i", "sine=frequency=440:duration=2"])
            .args(["-pix_fmt", "yuv420p", "-c:a", "aac", "-shortest"])
            .arg(dir.join("clip.mp4"))
            .status()
            .unwrap();
        if !status.success() {
            eprintln!("skipping pause test: could not create a clip");
            return;
        }
        let definition = show_of(&[("clip.mp4", Some(2_000))]);
        let mut show = Slideshow::new(renderer, definition, dir.clone(), (32, 32));
        let mixer = crate::audio::OfflineMixer::export(48_000);
        show.set_audio(mixer.mixer());
        let (ducker, mut probe) = crate::bgm::DuckProbe::new();
        show.set_ducker(ducker);

        update_exactly(&mut show, 0.0);
        // It plays once it has come in from black, at 0.5.
        assert_eq!(probe.windows(), [(0.5, 2.5)]);
        show.update(1.0);
        show.pause();
        assert!(show.current.as_ref().unwrap().voice.is_none());
        assert_eq!(probe.windows(), []);

        // After 3 s paused, the rest of the video (1.5 s) plays from now.
        show.update(4.0);
        show.resume();
        assert!(show.current.as_ref().unwrap().voice.is_some());
        assert_eq!(probe.windows(), [(4.0, 5.5)]);

        drop(show);
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
        let dir = temp_dir("video");
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

        // Only the range from 0.5 s plays.
        let range = PlayRange {
            start: 0.5,
            length: None,
        };
        let player = VideoPlayer::open(&renderer, &ffmpeg, &clip, (80, 80), range).unwrap();
        assert!((player.duration().unwrap() - 0.5).abs() < 0.05);
        drop(player);

        let mut player =
            VideoPlayer::open(&renderer, &ffmpeg, &clip, (80, 80), PlayRange::default()).unwrap();
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
