//! Background music (BGM) for a slideshow.
//!
//! A playlist of music files is decoded by `ffmpeg` one track at a time and
//! played through the mixer as a [`Source`]. The source is not placed on the
//! picture's clock: it plays one frame per frame from wherever it starts.
//! Only the dips for videos are tied to playback time (see below).
//!
//! # How a track follows another
//!
//! Each track fades in over [`BgmTuning::fade_secs`], plays, fades out over
//! the same time before its last sample, and is followed by
//! [`BgmTuning::gap_secs`] of silence before the next track fades in. Tracks
//! do not overlap: tracks of different key and tempo run into each other
//! badly, and the end of a track is only known `BUFFER_SECS` ahead of
//! playback, where a fade out can still be fitted in. The next track is
//! decoded while the current one plays, so the gap is exactly the silence
//! asked for. If a track cannot be decoded it is skipped, with an
//! [`BgmEvent::TrackFailed`] and a count in [`BgmStats::tracks_failed`].
//!
//! # Dips for video sound
//!
//! While a video with sound is on screen the BGM is faded out to silence and
//! brought back after it. The slideshow announces each such span of playback
//! time through a [`Ducker`] as soon as it knows when the video will start,
//! which is a few seconds before it does for an ordinary video. The fade out
//! is timed to be complete when the video starts ("duck before"), not to
//! start with it, because the sound of a video, often speech, then starts
//! over nothing instead of over a fading song. A video announced too late for
//! that fades the BGM out from the moment it is announced. The BGM comes back
//! after the video's end over [`BgmTuning::duck_fade_in_secs`]. Spans closer
//! together than that fade in plus the fade out are merged, so the BGM does
//! not swell between two videos.
//!
//! The dips are evaluated at the playback time of each frame including the
//! sync offset, so they line up with the sound of the video as it is heard.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::EngineError;
use crate::audio::{
    AtomicF64, AudioMixer, FadeCurve, Source, SourceHandle, TRACK_CHANNELS, Timeline,
};
use crate::media::music::{DecodeState, MusicTrack, Tags, read_tags};
use crate::media::video::Ffmpeg;

/// Longest fade, in seconds. It must stay below the decoder's buffer, since
/// a track can only be faded out once its end is in the buffer.
pub const MAX_FADE_SECS: f64 = 5.0;
/// Longest silence between tracks, in seconds.
pub const MAX_GAP_SECS: f64 = 60.0;
/// File extensions listed when a folder is the source of the playlist.
pub const MUSIC_EXTENSIONS: [&str; 11] = [
    "mp3", "m4a", "aac", "wav", "flac", "ogg", "oga", "opus", "aif", "aiff", "wma",
];
/// Time to fade out for a pause and in again after it.
const PAUSE_FADE_SECS: f64 = 0.1;
/// Longest fade out of a track that is skipped by hand.
const SKIP_FADE_SECS: f64 = 1.0;
/// Decoded sound that a track needs before it is handed to the mixer.
const READY_SECS: f64 = 0.5;
/// How long a track may take to produce its first sound.
const OPEN_TIMEOUT: Duration = Duration::from_secs(15);
/// How often the controller thread looks at the decoders and the mixer.
const POLL: Duration = Duration::from_millis(20);
/// Most events kept for [`Bgm::drain_events`]; older ones are dropped.
const MAX_EVENTS: usize = 256;

/// Where the playlist comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BgmSource {
    /// Every music file directly inside a folder, read when the BGM starts.
    /// Files starting with a dot are ignored (macOS leaves such files next to
    /// copied music).
    Folder(PathBuf),
    /// These files, in this order. They are not checked in advance: one that
    /// cannot be played is reported when its turn comes.
    Files(Vec<PathBuf>),
}

impl BgmSource {
    /// The tracks of the playlist, in order before any shuffling.
    pub fn tracks(&self) -> Result<Vec<PathBuf>, EngineError> {
        let tracks = match self {
            Self::Folder(dir) => list_music_files(dir)?,
            Self::Files(files) => files.clone(),
        };
        if tracks.is_empty() {
            return Err(EngineError::Audio(match self {
                Self::Folder(dir) => format!("no music files in {}", dir.display()),
                Self::Files(_) => "no music files were chosen".into(),
            }));
        }
        Ok(tracks)
    }
}

fn list_music_files(dir: &Path) -> Result<Vec<PathBuf>, EngineError> {
    let entries = std::fs::read_dir(dir)
        .map_err(|err| EngineError::Audio(format!("could not read {}: {err}", dir.display())))?;
    let mut files = Vec::new();
    for entry in entries {
        let path = entry
            .map_err(|err| EngineError::Audio(format!("could not read {}: {err}", dir.display())))?
            .path();
        let supported = path
            .extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| {
                MUSIC_EXTENSIONS
                    .iter()
                    .any(|known| known.eq_ignore_ascii_case(ext))
            });
        let hidden = path
            .file_name()
            .is_some_and(|name| name.to_string_lossy().starts_with('.'));
        if supported && !hidden && path.is_file() {
            files.push(path);
        }
    }
    files.sort_by_key(|path| {
        path.file_name()
            .map(|name| name.to_string_lossy().to_lowercase())
    });
    Ok(files)
}

/// Settings that can be changed while the BGM plays.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BgmTuning {
    /// Volume of the whole BGM, 0 to 1.
    pub volume: f32,
    /// Fade in at the start and fade out at the end of every track, seconds.
    pub fade_secs: f64,
    /// Silence between the fade out of a track and the fade in of the next.
    pub gap_secs: f64,
    /// Time the BGM takes to fade out before a video with sound.
    pub duck_fade_out_secs: f64,
    /// Time the BGM takes to come back after a video with sound.
    pub duck_fade_in_secs: f64,
    /// Shape of all these fades.
    pub curve: FadeCurve,
}

impl Default for BgmTuning {
    fn default() -> Self {
        Self {
            volume: 0.5,
            fade_secs: 2.0,
            gap_secs: 1.0,
            duck_fade_out_secs: 1.0,
            duck_fade_in_secs: 2.0,
            curve: FadeCurve::EqualPower,
        }
    }
}

impl BgmTuning {
    pub fn validate(&self) -> Result<(), EngineError> {
        let check = |name: &str, value: f64, max: f64| {
            if value.is_finite() && (0.0..=max).contains(&value) {
                Ok(())
            } else {
                Err(EngineError::Audio(format!(
                    "{name} must be between 0 and {max}, not {value}"
                )))
            }
        };
        check("the volume", f64::from(self.volume), 1.0)?;
        check("the fade", self.fade_secs, MAX_FADE_SECS)?;
        check("the gap between tracks", self.gap_secs, MAX_GAP_SECS)?;
        check(
            "the fade out for videos",
            self.duck_fade_out_secs,
            MAX_FADE_SECS,
        )?;
        check(
            "the fade in after videos",
            self.duck_fade_in_secs,
            MAX_FADE_SECS,
        )
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct BgmConfig {
    /// Play the tracks in a random order, a new one each round.
    pub shuffle: bool,
    /// Seed of the shuffle. Without one, each start shuffles differently.
    pub seed: Option<u64>,
    pub tuning: BgmTuning,
}

/// The track being played.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NowPlaying {
    /// Counts the tracks handed to the mixer, from 1.
    pub seq: u64,
    pub path: PathBuf,
    /// The title tag of the file, or its name without extension.
    pub title: String,
    pub artist: Option<String>,
}

/// Things the user of the BGM may want to be told about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BgmEvent {
    /// A track started to play (after its fade in began).
    TrackStarted(NowPlaying),
    /// A track could not be decoded and was skipped, or ended early because
    /// decoding failed half way.
    TrackFailed { path: PathBuf, reason: String },
    /// Every track failed one after the other; the BGM gives up and stays
    /// silent.
    AllTracksFailed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BgmState {
    /// Nothing is playing: before the first track is ready, or waiting for
    /// the next one.
    Idle,
    Playing,
    /// Silence between two tracks.
    Gap,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BgmStats {
    pub state: BgmState,
    pub paused: bool,
    pub now_playing: Option<NowPlaying>,
    pub tracks_started: u64,
    /// Tracks skipped or cut short because they could not be decoded.
    pub tracks_failed: u64,
    /// Buffers in which the playing track had not been decoded fast enough.
    pub underruns: u64,
    /// Times the next track was not ready when the previous one ended, so the
    /// gap was longer than asked for.
    pub starved_transitions: u64,
    pub last_error: Option<String>,
    pub all_tracks_failed: bool,
}

/// State shared between the audio thread, the controller thread and the
/// owner of the [`Bgm`].
struct Shared {
    volume: AtomicU32,
    fade_secs: AtomicF64,
    gap_secs: AtomicF64,
    duck_out_secs: AtomicF64,
    duck_in_secs: AtomicF64,
    curve: AtomicU8,
    paused: AtomicBool,
    skips: AtomicU64,
    // Written by the audio thread.
    state: AtomicU8,
    tracks_started: AtomicU64,
    current_seq: AtomicU64,
    underruns: AtomicU64,
    starved: AtomicU64,
    /// Every track failed: the BGM stays silent.
    gave_up: AtomicBool,
}

impl Shared {
    fn new(tuning: &BgmTuning) -> Self {
        let shared = Self {
            volume: AtomicU32::new(0),
            fade_secs: AtomicF64::default(),
            gap_secs: AtomicF64::default(),
            duck_out_secs: AtomicF64::default(),
            duck_in_secs: AtomicF64::default(),
            curve: AtomicU8::new(0),
            paused: AtomicBool::new(false),
            skips: AtomicU64::new(0),
            state: AtomicU8::new(0),
            tracks_started: AtomicU64::new(0),
            current_seq: AtomicU64::new(0),
            underruns: AtomicU64::new(0),
            starved: AtomicU64::new(0),
            gave_up: AtomicBool::new(false),
        };
        shared.set_tuning(tuning);
        shared
    }

    fn set_tuning(&self, tuning: &BgmTuning) {
        self.volume
            .store(tuning.volume.to_bits(), Ordering::Relaxed);
        self.fade_secs.store(tuning.fade_secs);
        self.gap_secs.store(tuning.gap_secs);
        self.duck_out_secs.store(tuning.duck_fade_out_secs);
        self.duck_in_secs.store(tuning.duck_fade_in_secs);
        self.curve.store(
            match tuning.curve {
                FadeCurve::Linear => 0,
                FadeCurve::EqualPower => 1,
            },
            Ordering::Relaxed,
        );
    }

    fn tuning(&self) -> BgmTuning {
        BgmTuning {
            volume: f32::from_bits(self.volume.load(Ordering::Relaxed)),
            fade_secs: self.fade_secs.load(),
            gap_secs: self.gap_secs.load(),
            duck_fade_out_secs: self.duck_out_secs.load(),
            duck_fade_in_secs: self.duck_in_secs.load(),
            curve: match self.curve.load(Ordering::Relaxed) {
                0 => FadeCurve::Linear,
                _ => FadeCurve::EqualPower,
            },
        }
    }
}

// ---------------------------------------------------------------------------
// Dips for video sound

enum DuckCommand {
    Set { id: u64, start: f64, end: f64 },
    Close { id: u64 },
}

/// Tells the BGM when videos with sound are on screen. Cheap to clone.
#[derive(Clone)]
pub struct Ducker {
    commands: Sender<DuckCommand>,
    next_id: Arc<AtomicU64>,
}

impl Ducker {
    /// Announces that sound plays from playback time `start` to `end`
    /// (`f64::INFINITY` while the end is not known). The window can be
    /// adjusted with [`DuckWindow::set`]. Dropping it means the sound is gone
    /// when the BGM learns of that: a window that has not started is
    /// forgotten, and one that has ends then, unless its end came first.
    pub fn open_window(&self, start: f64, end: f64) -> DuckWindow {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let window = DuckWindow {
            id,
            commands: self.commands.clone(),
        };
        window.set(start, end);
        window
    }
}

/// Reads back what a [`Ducker`] was told, for tests of its callers.
#[cfg(test)]
pub(crate) struct DuckProbe {
    commands: Receiver<DuckCommand>,
    open: Vec<(u64, f64, f64)>,
}

#[cfg(test)]
impl DuckProbe {
    pub(crate) fn new() -> (Ducker, Self) {
        let (commands, incoming) = mpsc::channel();
        let ducker = Ducker {
            commands,
            next_id: Arc::new(AtomicU64::new(1)),
        };
        let probe = Self {
            commands: incoming,
            open: Vec::new(),
        };
        (ducker, probe)
    }

    /// The windows that are open, as (start, end), in the order announced.
    pub(crate) fn windows(&mut self) -> Vec<(f64, f64)> {
        for command in self.commands.try_iter() {
            match command {
                DuckCommand::Set { id, start, end } => {
                    match self.open.iter_mut().find(|window| window.0 == id) {
                        Some(window) => (window.1, window.2) = (start, end),
                        None => self.open.push((id, start, end)),
                    }
                }
                DuckCommand::Close { id } => self.open.retain(|window| window.0 != id),
            }
        }
        self.open
            .iter()
            .map(|&(_, start, end)| (start, end))
            .collect()
    }
}

pub struct DuckWindow {
    id: u64,
    commands: Sender<DuckCommand>,
}

impl DuckWindow {
    pub fn set(&self, start: f64, end: f64) {
        let _ = self.commands.send(DuckCommand::Set {
            id: self.id,
            start,
            end,
        });
    }
}

impl Drop for DuckWindow {
    fn drop(&mut self) {
        let _ = self.commands.send(DuckCommand::Close { id: self.id });
    }
}

struct Window {
    id: u64,
    start: f64,
    end: f64,
    /// Playback time at which the BGM first heard of the window: the fade out
    /// cannot begin before it.
    announced: f64,
}

/// Works out how far the BGM is dipped at any playback time.
struct DuckState {
    commands: Receiver<DuckCommand>,
    windows: Vec<Window>,
    /// The dips as (start of the fade out, end of the sound), merged.
    spans: Vec<(f64, f64)>,
    scratch: Vec<(f64, f64)>,
    fade_out: f64,
    fade_in: f64,
    curve: FadeCurve,
}

impl DuckState {
    fn new(commands: Receiver<DuckCommand>) -> Self {
        Self {
            commands,
            windows: Vec::new(),
            spans: Vec::new(),
            scratch: Vec::new(),
            fade_out: 0.0,
            fade_in: 0.0,
            curve: FadeCurve::default(),
        }
    }

    /// Takes in announcements and fixes the dips for a buffer that starts at
    /// playback time `now`.
    fn prepare(&mut self, now: f64, tuning: &BgmTuning) {
        for command in self.commands.try_iter() {
            match command {
                DuckCommand::Set { id, start, end } => {
                    match self.windows.iter_mut().find(|window| window.id == id) {
                        Some(window) => (window.start, window.end) = (start, end),
                        None => self.windows.push(Window {
                            id,
                            start,
                            end,
                            announced: now,
                        }),
                    }
                }
                // The sound was cut short (a video skipped, or one that was
                // ready but will not play), or has ended already.
                DuckCommand::Close { id } => {
                    if let Some(index) = self.windows.iter().position(|window| window.id == id) {
                        let window = &mut self.windows[index];
                        if now < window.start {
                            self.windows.remove(index);
                        } else {
                            window.end = window.end.min(now);
                        }
                    }
                }
            }
        }
        (self.fade_out, self.fade_in, self.curve) = (
            tuning.duck_fade_out_secs,
            tuning.duck_fade_in_secs,
            tuning.curve,
        );
        // A window that is over and faded back in is forgotten, unless it
        // has not been closed (its end can still be announced).
        let fade_in = self.fade_in;
        self.windows
            .retain(|window| window.end.is_infinite() || window.end + fade_in >= now);

        self.scratch.clear();
        for window in &self.windows {
            let fade_start = (window.start - self.fade_out).max(window.announced);
            self.scratch
                .push((fade_start, window.end.max(fade_start + self.fade_out)));
        }
        self.scratch.sort_by(|a, b| a.0.total_cmp(&b.0));
        self.spans.clear();
        for &(fade_start, end) in &self.scratch {
            match self.spans.last_mut() {
                // Starts to fade out before the BGM would be back: one dip.
                Some(last) if fade_start < last.1 + self.fade_in => last.1 = last.1.max(end),
                _ => self.spans.push((fade_start, end)),
            }
        }
    }

    /// Volume factor at playback time `time`.
    fn gain(&self, time: f64) -> f32 {
        let mut gain = 1.0f32;
        for &(fade_start, end) in &self.spans {
            let factor = if time < fade_start {
                1.0
            } else if time < fade_start + self.fade_out {
                self.curve.rise(1.0 - (time - fade_start) / self.fade_out)
            } else if time <= end {
                0.0
            } else if time < end + self.fade_in {
                self.curve.rise((time - end) / self.fade_in)
            } else {
                1.0
            };
            gain = gain.min(factor);
        }
        gain
    }
}

// ---------------------------------------------------------------------------
// The source on the audio thread

/// A decoded track on its way to the mixer.
struct TrackFeed {
    seq: u64,
    samples: rtrb::Consumer<f32>,
    finished: Arc<AtomicBool>,
}

/// A track fading out because the user asked for the next one.
struct Skipping {
    position: u64,
    length: u64,
}

struct Playing {
    feed: TrackFeed,
    /// Frames played.
    played: u64,
    skipping: Option<Skipping>,
}

enum Phase {
    Idle,
    Playing(Playing),
    /// Frames of silence left.
    Gap(u64),
}

/// What one buffer's worth of settings come to, in frames.
struct BufferSettings {
    fade_frames: u64,
    gap_frames: u64,
    curve: FadeCurve,
}

enum Step {
    Frame([f32; TRACK_CHANNELS]),
    /// The track is decoded to its end and played out.
    Ended,
    /// The track has faded out after a skip.
    Skipped,
    /// The decoder has not produced the next frame yet.
    Underrun,
}

impl Playing {
    fn next_frame(&mut self, settings: &BufferSettings) -> Step {
        if let Some(skipping) = &self.skipping
            && skipping.position >= skipping.length
        {
            return Step::Skipped;
        }
        let available = (self.feed.samples.slots() / TRACK_CHANNELS) as u64;
        if available == 0 {
            return if self.feed.finished.load(Ordering::Acquire) {
                Step::Ended
            } else {
                Step::Underrun
            };
        }
        let fade = settings.fade_frames;
        let mut gain = 1.0;
        if self.played < fade {
            gain *= settings.curve.rise(self.played as f64 / fade as f64);
        }
        // The end of a track is known only once it is fully decoded.
        if available < fade && self.feed.finished.load(Ordering::Acquire) {
            gain *= settings.curve.rise(available as f64 / fade as f64);
        }
        if let Some(skipping) = &mut self.skipping {
            let progress = skipping.position as f64 / skipping.length as f64;
            gain *= settings.curve.rise(1.0 - progress);
            skipping.position += 1;
        }
        let left = self.feed.samples.pop().unwrap_or(0.0);
        let right = self.feed.samples.pop().unwrap_or(0.0);
        self.played += 1;
        Step::Frame([left * gain, right * gain])
    }
}

struct PlaylistSource {
    shared: Arc<Shared>,
    /// Of the mixer the source is in.
    sample_rate: f64,
    incoming: Receiver<TrackFeed>,
    queue: VecDeque<TrackFeed>,
    phase: Phase,
    /// Whether a track has just ended and none has started since.
    after_track: bool,
    /// Whether the mixer has already counted that the next track was late.
    starving: bool,
    skips_seen: u64,
    /// Volume applied at the end of the previous buffer.
    volume: f32,
    /// 1 while playing, 0 once paused; ramps between them.
    pause_gain: f32,
    duck: DuckState,
}

impl PlaylistSource {
    /// Starts the next queued track. Returns false if there is none.
    fn start_next(&mut self) -> bool {
        let Some(feed) = self.queue.pop_front() else {
            if self.after_track && !self.starving {
                self.starving = true;
                self.shared.starved.fetch_add(1, Ordering::Relaxed);
            }
            return false;
        };
        self.starving = false;
        self.after_track = false;
        self.shared.current_seq.store(feed.seq, Ordering::Relaxed);
        self.shared.tracks_started.fetch_add(1, Ordering::Release);
        self.phase = Phase::Playing(Playing {
            feed,
            played: 0,
            skipping: None,
        });
        true
    }

    fn next_frame(&mut self, settings: &BufferSettings, underran: &mut bool) -> [f32; 2] {
        const SILENCE: [f32; 2] = [0.0; 2];
        loop {
            match &mut self.phase {
                Phase::Idle => {
                    if !self.start_next() {
                        return SILENCE;
                    }
                }
                Phase::Gap(left) => {
                    if *left == 0 {
                        self.phase = Phase::Idle;
                        continue;
                    }
                    *left -= 1;
                    return SILENCE;
                }
                Phase::Playing(playing) => match playing.next_frame(settings) {
                    Step::Frame(frame) => return frame,
                    Step::Underrun => {
                        *underran = true;
                        return SILENCE;
                    }
                    Step::Ended => {
                        self.after_track = true;
                        self.phase = if settings.gap_frames > 0 {
                            Phase::Gap(settings.gap_frames)
                        } else {
                            Phase::Idle
                        };
                    }
                    // The user asked for the next track: no gap.
                    Step::Skipped => {
                        self.after_track = true;
                        self.phase = Phase::Idle;
                    }
                },
            }
        }
    }

    /// Reacts to a request for the next track.
    fn skip(&mut self, settings: &BufferSettings, rate: f64) {
        match &mut self.phase {
            Phase::Playing(playing) if playing.skipping.is_none() => {
                let length = settings.fade_frames.min((SKIP_FADE_SECS * rate) as u64);
                playing.skipping = Some(Skipping {
                    position: 0,
                    length,
                });
            }
            Phase::Gap(_) => self.phase = Phase::Idle,
            _ => {}
        }
    }
}

impl Source for PlaylistSource {
    fn render(&mut self, out: &mut [f32], timeline: &Timeline<'_>) {
        let frames = timeline.frames();
        let rate = timeline.sample_rate();
        self.queue.extend(self.incoming.try_iter());
        let tuning = self.shared.tuning();
        let settings = BufferSettings {
            fade_frames: (tuning.fade_secs * rate) as u64,
            gap_frames: (tuning.gap_secs * rate) as u64,
            curve: tuning.curve,
        };
        let skips = self.shared.skips.load(Ordering::Relaxed);
        if skips != self.skips_seen {
            self.skips_seen = skips;
            self.skip(&settings, rate);
        }
        self.duck.prepare(timeline.time(0), &tuning);

        let paused = self.shared.paused.load(Ordering::Relaxed);
        let pause_target = if paused { 0.0 } else { 1.0 };
        let pause_step = (1.0 / (PAUSE_FADE_SECS * rate)) as f32;
        let volume_step = (tuning.volume - self.volume) / frames as f32;
        let mut underran = false;
        for frame in 0..frames {
            self.pause_gain = if self.pause_gain < pause_target {
                (self.pause_gain + pause_step).min(pause_target)
            } else {
                (self.pause_gain - pause_step).max(pause_target)
            };
            if self.pause_gain == 0.0 {
                // Paused: the track and its fades stand still.
                continue;
            }
            let [left, right] = self.next_frame(&settings, &mut underran);
            let gain = (self.volume + volume_step * (frame + 1) as f32)
                * self.pause_gain
                * self.duck.gain(timeline.time(frame));
            out[frame * TRACK_CHANNELS] += left * gain;
            out[frame * TRACK_CHANNELS + 1] += right * gain;
        }
        self.volume = tuning.volume;
        if underran {
            self.shared.underruns.fetch_add(1, Ordering::Relaxed);
        }
        self.shared.state.store(
            match self.phase {
                Phase::Idle => 0,
                Phase::Playing(_) => 1,
                Phase::Gap(_) => 2,
            },
            Ordering::Relaxed,
        );
    }

    /// The music is ready when the track playing has decoded past these
    /// frames and its fade out (whose start is known only once its end is
    /// decoded), and the next track has, if it starts within them.
    fn ready_for(&mut self, frames: usize) -> bool {
        if self.shared.gave_up.load(Ordering::Relaxed) {
            return true;
        }
        self.queue.extend(self.incoming.try_iter());
        let tuning = self.shared.tuning();
        let rate = self.sample_rate;
        let fade = (tuning.fade_secs * rate) as usize;
        let gap = (tuning.gap_secs * rate) as usize;
        let decoded = |feed: &TrackFeed| {
            feed.finished.load(Ordering::Acquire)
                || feed.samples.slots() / TRACK_CHANNELS >= frames + fade
        };
        let next_starts = match &self.phase {
            Phase::Idle => true,
            Phase::Gap(left) => (*left as usize) < frames,
            Phase::Playing(playing) => {
                if !decoded(&playing.feed) {
                    return false;
                }
                playing.feed.finished.load(Ordering::Acquire)
                    && playing.feed.samples.slots() / TRACK_CHANNELS + gap < frames
            }
        };
        !next_starts || self.queue.front().is_some_and(decoded)
    }
}

// ---------------------------------------------------------------------------
// The order of the tracks

/// A small random number generator (SplitMix64): the shuffle needs no more
/// than this, and it makes a seeded shuffle the same on every platform.
pub(crate) struct Rng(pub(crate) u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// A number from 0 to `bound` - 1.
    pub(crate) fn below(&mut self, bound: usize) -> usize {
        ((u128::from(self.next()) * bound as u128) >> 64) as usize
    }
}

/// Endless order of tracks: the playlist round after round.
struct Playlist {
    tracks: Vec<PathBuf>,
    shuffle: bool,
    rng: Rng,
    order: Vec<usize>,
    next: usize,
    last: Option<usize>,
}

impl Playlist {
    fn new(tracks: Vec<PathBuf>, shuffle: bool, seed: u64) -> Self {
        Self {
            tracks,
            shuffle,
            rng: Rng(seed),
            order: Vec::new(),
            next: 0,
            last: None,
        }
    }

    fn len(&self) -> usize {
        self.tracks.len()
    }

    fn next_track(&mut self) -> &Path {
        if self.next >= self.order.len() {
            self.order = (0..self.tracks.len()).collect();
            if self.shuffle {
                // Fisher-Yates.
                for index in (1..self.order.len()).rev() {
                    let other = self.rng.below(index + 1);
                    self.order.swap(index, other);
                }
                // The last track of the previous round does not open this one.
                if self.order.len() > 1 && Some(self.order[0]) == self.last {
                    let other = 1 + self.rng.below(self.order.len() - 1);
                    self.order.swap(0, other);
                }
            }
            self.next = 0;
        }
        let index = self.order[self.next];
        self.next += 1;
        self.last = Some(index);
        &self.tracks[index]
    }
}

// ---------------------------------------------------------------------------
// The controller thread

#[derive(Default)]
struct Info {
    now_playing: Option<NowPlaying>,
    events: VecDeque<BgmEvent>,
    tracks_failed: u64,
    last_error: Option<String>,
    all_tracks_failed: bool,
}

impl Info {
    fn push(&mut self, event: BgmEvent) {
        if self.events.len() == MAX_EVENTS {
            self.events.pop_front();
        }
        self.events.push_back(event);
    }

    fn fail(&mut self, path: &Path, reason: String) {
        self.tracks_failed += 1;
        self.last_error = Some(format!("{}: {reason}", path.display()));
        self.push(BgmEvent::TrackFailed {
            path: path.to_path_buf(),
            reason,
        });
    }
}

/// A track being decoded that the mixer does not have yet.
struct Pending {
    track: MusicTrack,
    path: PathBuf,
    since: Instant,
}

/// A track the mixer has, kept to tell what is playing.
struct Handed {
    now_playing: NowPlaying,
    state: Arc<DecodeState>,
    failure_reported: bool,
}

struct Controller {
    ffmpeg: Ffmpeg,
    format: crate::audio::AudioFormat,
    playlist: Playlist,
    feeds: Sender<TrackFeed>,
    shared: Arc<Shared>,
    info: Arc<Mutex<Info>>,
    stop: Arc<AtomicBool>,
    pending: Option<Pending>,
    /// Tracks handed to the mixer, oldest first.
    handed: VecDeque<Handed>,
    seq: u64,
    reported_seq: u64,
    failures_in_row: usize,
    gave_up: bool,
}

impl Controller {
    fn run(mut self) {
        while !self.stop.load(Ordering::Relaxed) {
            self.poll_pending();
            self.open_next_if_needed();
            self.watch_playback();
            std::thread::sleep(POLL);
        }
    }

    /// Tracks handed over but not yet started.
    fn waiting(&self) -> u64 {
        self.seq - self.shared.tracks_started.load(Ordering::Acquire)
    }

    fn open_next_if_needed(&mut self) {
        if self.pending.is_some() || self.gave_up || self.waiting() >= 1 {
            return;
        }
        let path = self.playlist.next_track().to_path_buf();
        match MusicTrack::open(&self.ffmpeg, &path, self.format) {
            Ok(track) => {
                self.pending = Some(Pending {
                    track,
                    path,
                    since: Instant::now(),
                });
            }
            Err(err) => self.track_failed(&path, err.to_string()),
        }
    }

    fn poll_pending(&mut self) {
        let Some(pending) = &mut self.pending else {
            return;
        };
        let state = pending.track.state();
        let frames = state.frames_decoded();
        let finished = pending.track.is_finished();
        let ready_frames = (READY_SECS * f64::from(self.format.sample_rate)) as u64;
        if finished && frames == 0 {
            let reason = state
                .failure()
                .unwrap_or_else(|| "the file has no sound".into());
            let pending = self.pending.take().expect("checked above");
            self.track_failed(&pending.path, reason);
        } else if pending.since.elapsed() > OPEN_TIMEOUT && frames == 0 {
            let pending = self.pending.take().expect("checked above");
            self.track_failed(&pending.path, "decoding did not start in time".into());
        } else if finished || frames >= ready_frames {
            let mut pending = self.pending.take().expect("checked above");
            self.hand_over(&mut pending);
        }
    }

    fn hand_over(&mut self, pending: &mut Pending) {
        let Some((samples, finished)) = pending.track.take_samples() else {
            return;
        };
        let tags: Tags = read_tags(&self.ffmpeg, &pending.path);
        self.seq += 1;
        let title = tags.title.unwrap_or_else(|| {
            pending
                .path
                .file_stem()
                .map(|stem| stem.to_string_lossy().into_owned())
                .unwrap_or_default()
        });
        self.handed.push_back(Handed {
            now_playing: NowPlaying {
                seq: self.seq,
                path: pending.path.clone(),
                title,
                artist: tags.artist,
            },
            state: pending.track.state(),
            failure_reported: false,
        });
        self.failures_in_row = 0;
        let _ = self.feeds.send(TrackFeed {
            seq: self.seq,
            samples,
            finished,
        });
    }

    fn track_failed(&mut self, path: &Path, reason: String) {
        self.failures_in_row += 1;
        let mut info = self.info.lock().unwrap();
        info.fail(path, reason);
        if self.failures_in_row >= self.playlist.len() && !self.gave_up {
            self.gave_up = true;
            self.shared.gave_up.store(true, Ordering::Relaxed);
            info.all_tracks_failed = true;
            info.push(BgmEvent::AllTracksFailed);
        }
    }

    /// Follows the mixer from track to track and reports what it plays.
    fn watch_playback(&mut self) {
        let current = self.shared.current_seq.load(Ordering::Relaxed);
        if current != self.reported_seq {
            self.reported_seq = current;
            // Older tracks are over.
            while self
                .handed
                .front()
                .is_some_and(|handed| handed.now_playing.seq < current)
            {
                self.handed.pop_front();
            }
            if let Some(handed) = self.handed.front() {
                let mut info = self.info.lock().unwrap();
                info.now_playing = Some(handed.now_playing.clone());
                info.push(BgmEvent::TrackStarted(handed.now_playing.clone()));
            }
        }
        // A track that failed half way plays what was decoded, and says so.
        for handed in &mut self.handed {
            if !handed.failure_reported
                && let Some(reason) = handed.state.failure()
            {
                handed.failure_reported = true;
                let path = handed.now_playing.path.clone();
                let mut info = self.info.lock().unwrap();
                info.fail(&path, reason);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// The handle

/// The background music of a slideshow. Dropping it stops the music.
pub struct Bgm {
    shared: Arc<Shared>,
    info: Arc<Mutex<Info>>,
    ducker: Ducker,
    stop: Arc<AtomicBool>,
    controller: Option<JoinHandle<()>>,
    _source: SourceHandle,
}

impl Bgm {
    /// Lists the tracks, adds the BGM to `mixer` and starts decoding. Fails
    /// if the playlist is empty or the settings are out of range; a track
    /// that cannot be played later is skipped and reported instead.
    pub fn start(
        ffmpeg: Ffmpeg,
        mixer: &AudioMixer,
        source: &BgmSource,
        config: BgmConfig,
    ) -> Result<Self, EngineError> {
        config.tuning.validate()?;
        let tracks = source.tracks()?;
        let seed = config.seed.unwrap_or_else(random_seed);

        let shared = Arc::new(Shared::new(&config.tuning));
        let info = Arc::new(Mutex::new(Info::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let (feeds, incoming) = mpsc::channel();
        let (duck_commands, duck_incoming) = mpsc::channel();
        let playlist_source = PlaylistSource {
            shared: shared.clone(),
            sample_rate: f64::from(mixer.format().sample_rate),
            incoming,
            queue: VecDeque::new(),
            phase: Phase::Idle,
            after_track: false,
            starving: false,
            skips_seen: 0,
            volume: 0.0,
            pause_gain: 1.0,
            duck: DuckState::new(duck_incoming),
        };
        let controller = Controller {
            ffmpeg,
            format: mixer.format(),
            playlist: Playlist::new(tracks, config.shuffle, seed),
            feeds,
            shared: shared.clone(),
            info: info.clone(),
            stop: stop.clone(),
            pending: None,
            handed: VecDeque::new(),
            seq: 0,
            reported_seq: 0,
            failures_in_row: 0,
            gave_up: false,
        };
        let thread = std::thread::Builder::new()
            .name("fluxlide-bgm".into())
            .spawn(move || controller.run())
            .map_err(|err| EngineError::Audio(err.to_string()))?;
        Ok(Self {
            shared,
            info,
            ducker: Ducker {
                commands: duck_commands,
                next_id: Arc::new(AtomicU64::new(1)),
            },
            stop,
            controller: Some(thread),
            _source: mixer.add_source(Box::new(playlist_source)),
        })
    }

    /// The handle for announcing videos with sound to the BGM.
    pub fn ducker(&self) -> Ducker {
        self.ducker.clone()
    }

    /// Pauses the BGM, fading out over a tenth of a second. The track and its
    /// fades stand still until [`Bgm::resume`].
    pub fn pause(&self) {
        self.shared.paused.store(true, Ordering::Relaxed);
    }

    pub fn resume(&self) {
        self.shared.paused.store(false, Ordering::Relaxed);
    }

    pub fn is_paused(&self) -> bool {
        self.shared.paused.load(Ordering::Relaxed)
    }

    /// Fades out the current track (at most a second) and starts the next
    /// one, without the usual gap.
    pub fn next(&self) {
        self.shared.skips.fetch_add(1, Ordering::Relaxed);
    }

    /// Sets the volume of the whole BGM, 0 to 1.
    pub fn set_volume(&self, volume: f32) -> Result<(), EngineError> {
        self.set_tuning(BgmTuning {
            volume,
            ..self.tuning()
        })
    }

    pub fn tuning(&self) -> BgmTuning {
        self.shared.tuning()
    }

    /// Changes the fades, the gap and the volume. They take effect at once,
    /// for the track that is playing too.
    pub fn set_tuning(&self, tuning: BgmTuning) -> Result<(), EngineError> {
        tuning.validate()?;
        self.shared.set_tuning(&tuning);
        Ok(())
    }

    pub fn now_playing(&self) -> Option<NowPlaying> {
        self.info.lock().unwrap().now_playing.clone()
    }

    /// Takes the events that happened since the last call.
    pub fn drain_events(&self) -> Vec<BgmEvent> {
        self.info.lock().unwrap().events.drain(..).collect()
    }

    pub fn stats(&self) -> BgmStats {
        let info = self.info.lock().unwrap();
        BgmStats {
            state: match self.shared.state.load(Ordering::Relaxed) {
                1 => BgmState::Playing,
                2 => BgmState::Gap,
                _ => BgmState::Idle,
            },
            paused: self.is_paused(),
            now_playing: info.now_playing.clone(),
            tracks_started: self.shared.tracks_started.load(Ordering::Relaxed),
            tracks_failed: info.tracks_failed,
            underruns: self.shared.underruns.load(Ordering::Relaxed),
            starved_transitions: self.shared.starved.load(Ordering::Relaxed),
            last_error: info.last_error.clone(),
            all_tracks_failed: info.all_tracks_failed,
        }
    }
}

impl Drop for Bgm {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.controller.take() {
            let _ = thread.join();
        }
    }
}

pub(crate) fn random_seed() -> u64 {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_nanos() as u64);
    nanos ^ (u64::from(std::process::id()) << 32)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::OfflineMixer;
    use crate::media::music::make_tone;

    const RATE: u32 = 1000;

    /// A source with fake tracks, rendered by hand at 1 kHz.
    struct Rig {
        offline: OfflineMixer,
        feeds: Sender<TrackFeed>,
        ducker: Ducker,
        shared: Arc<Shared>,
        /// Playback time, in frames, of the next buffer.
        frames_done: usize,
        _source: SourceHandle,
    }

    impl Rig {
        fn new(tuning: BgmTuning) -> Self {
            let offline = OfflineMixer::export(RATE);
            let shared = Arc::new(Shared::new(&tuning));
            let (feeds, incoming) = mpsc::channel();
            let (duck_commands, duck_incoming) = mpsc::channel();
            let source = PlaylistSource {
                shared: shared.clone(),
                sample_rate: f64::from(RATE),
                incoming,
                queue: VecDeque::new(),
                phase: Phase::Idle,
                after_track: false,
                starving: false,
                skips_seen: 0,
                volume: tuning.volume,
                pause_gain: 1.0,
                duck: DuckState::new(duck_incoming),
            };
            let handle = offline.mixer().add_source(Box::new(source));
            Self {
                offline,
                feeds,
                ducker: Ducker {
                    commands: duck_commands,
                    next_id: Arc::new(AtomicU64::new(1)),
                },
                shared,
                frames_done: 0,
                _source: handle,
            }
        }

        /// Queues a decoded track of `frames` frames that count 1, 2, 3, ...
        /// scaled by `scale` in the left channel (and 0 on the right).
        fn queue_track(&self, seq: u64, frames: usize, scale: f32) {
            let (mut producer, samples) = rtrb::RingBuffer::new(frames * 2);
            for frame in 1..=frames {
                producer.push(frame as f32 * scale).unwrap();
                producer.push(0.0).unwrap();
            }
            self.feeds
                .send(TrackFeed {
                    seq,
                    samples,
                    finished: Arc::new(AtomicBool::new(true)),
                })
                .unwrap();
        }

        /// Queues a decoded track of `frames` frames of the same value on the left.
        fn queue_constant(&self, seq: u64, frames: usize, value: f32) {
            let (mut producer, samples) = rtrb::RingBuffer::new(frames * 2);
            for _ in 0..frames {
                producer.push(value).unwrap();
                producer.push(0.0).unwrap();
            }
            self.feeds
                .send(TrackFeed {
                    seq,
                    samples,
                    finished: Arc::new(AtomicBool::new(true)),
                })
                .unwrap();
        }

        /// Renders `frames` frames and returns the left channel.
        fn render(&mut self, frames: usize) -> Vec<f32> {
            let out = self
                .offline
                .render(frames, self.frames_done as f64 / f64::from(RATE));
            self.frames_done += frames;
            out.chunks(2).map(|frame| frame[0]).collect()
        }
    }

    #[test]
    fn export_waits_for_the_music_to_be_decoded() {
        // A fade of 100 frames: the end of a track must be known that far ahead.
        let mut rig = Rig::new(tuning(0.1, 0.0, FadeCurve::Linear));
        // Nothing handed over yet.
        assert!(!rig.offline.has_samples_for(10));

        // A track still decoding, 50 frames in: not enough to see its fade.
        let (mut producer, samples) = rtrb::RingBuffer::new(1000);
        let finished = Arc::new(AtomicBool::new(false));
        for _ in 0..50 {
            producer.push(0.5).unwrap();
            producer.push(0.0).unwrap();
        }
        rig.feeds
            .send(TrackFeed {
                seq: 1,
                samples,
                finished: finished.clone(),
            })
            .unwrap();
        assert!(!rig.offline.has_samples_for(10));
        for _ in 0..100 {
            producer.push(0.5).unwrap();
            producer.push(0.0).unwrap();
        }
        assert!(rig.offline.has_samples_for(10));

        // Decoded to its end, 150 frames: the next track is needed only once
        // the buffer reaches past it.
        finished.store(true, Ordering::Release);
        rig.render(140);
        assert!(rig.offline.has_samples_for(5));
        assert!(!rig.offline.has_samples_for(20));
        rig.queue_constant(2, 200, 0.25);
        assert!(rig.offline.has_samples_for(20));

        // With every track failed there is nothing to wait for.
        rig.render(200);
        rig.shared.gave_up.store(true, Ordering::Relaxed);
        assert!(rig.offline.has_samples_for(500));
    }

    fn tuning(fade: f64, gap: f64, curve: FadeCurve) -> BgmTuning {
        BgmTuning {
            volume: 1.0,
            fade_secs: fade,
            gap_secs: gap,
            duck_fade_out_secs: 1.0,
            duck_fade_in_secs: 2.0,
            curve,
        }
    }

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-4
    }

    #[test]
    fn tracks_fade_in_and_out_around_a_silent_gap() {
        // Two tracks of 1 s, 0.2 s fades, 0.3 s of silence between them.
        let mut rig = Rig::new(tuning(0.2, 0.3, FadeCurve::Linear));
        rig.queue_track(1, 1000, 0.001);
        rig.queue_track(2, 1000, 0.001);
        let out = rig.render(2700);

        // Fade in: the value of frame n is n * 0.001, scaled by n / 200.
        assert!(close(out[0], 0.0));
        assert!(close(out[100], 101.0 * 0.001 * 100.0 / 200.0));
        // Full level in the middle.
        assert!(close(out[500], 501.0 * 0.001));
        // Fade out: 100 frames from the end the level is one half.
        assert!(close(out[900], 901.0 * 0.001 * 100.0 / 200.0));
        assert!(out[999] > 0.0 && out[999] < 0.01);
        // Silence for the gap, then the second track fades in.
        assert!(out[1000..1300].iter().all(|sample| *sample == 0.0));
        assert!(close(out[1300], 0.0));
        assert!(close(out[1400], 101.0 * 0.001 * 100.0 / 200.0));
        assert!(close(out[1800], 501.0 * 0.001));
        // After the last track there is nothing more: the next track is late.
        assert!(out[2300..].iter().all(|sample| *sample == 0.0));
        assert_eq!(rig.shared.starved.load(Ordering::Relaxed), 1);
        assert_eq!(rig.shared.tracks_started.load(Ordering::Relaxed), 2);
        assert_eq!(rig.shared.current_seq.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn equal_power_fades_are_louder_in_the_middle_than_linear_ones() {
        let mut rig = Rig::new(tuning(0.2, 0.0, FadeCurve::EqualPower));
        rig.queue_track(1, 1000, 0.001);
        let out = rig.render(1000);
        let expected = (std::f64::consts::FRAC_PI_4).sin() as f32;
        assert!(close(out[100], 101.0 * 0.001 * expected));
    }

    #[test]
    fn without_a_gap_the_next_track_follows_at_once() {
        let mut rig = Rig::new(tuning(0.0, 0.0, FadeCurve::Linear));
        rig.queue_track(1, 10, 1.0);
        rig.queue_track(2, 10, 100.0);
        let out = rig.render(25);
        assert_eq!(&out[..10], [1., 2., 3., 4., 5., 6., 7., 8., 9., 10.]);
        assert_eq!(
            &out[10..20],
            [100., 200., 300., 400., 500., 600., 700., 800., 900., 1000.]
        );
        assert!(out[20..].iter().all(|sample| *sample == 0.0));
    }

    #[test]
    fn next_fades_the_track_out_and_starts_the_next_without_a_gap() {
        let mut rig = Rig::new(tuning(0.2, 5.0, FadeCurve::Linear));
        rig.queue_track(1, 5000, 0.0001);
        rig.queue_track(2, 5000, 0.0001);
        rig.render(1000);
        rig.shared.skips.fetch_add(1, Ordering::Relaxed);
        let out = rig.render(400);
        // The skip fade is as long as the track fade (200 frames), from the
        // level the track had.
        assert!(close(out[0], 1001.0 * 0.0001));
        assert!(close(out[100], 1101.0 * 0.0001 * 0.5));
        // Then the second track fades in, with no gap even though one is set.
        assert!(close(out[200], 0.0));
        assert!(close(out[300], 101.0 * 0.0001 * 0.5));
        assert_eq!(rig.shared.current_seq.load(Ordering::Relaxed), 2);
        // A skip during a gap ends the gap.
        let mut rig = Rig::new(tuning(0.0, 5.0, FadeCurve::Linear));
        rig.queue_track(1, 10, 1.0);
        rig.queue_track(2, 10, 1.0);
        rig.render(20);
        assert_eq!(rig.shared.state.load(Ordering::Relaxed), 2);
        rig.shared.skips.fetch_add(1, Ordering::Relaxed);
        let out = rig.render(3);
        assert_eq!(out, [1.0, 2.0, 3.0]);
    }

    #[test]
    fn pause_stands_still_and_resume_continues_where_it_left_off() {
        let mut rig = Rig::new(tuning(0.0, 0.0, FadeCurve::Linear));
        rig.queue_track(1, 2000, 1.0);
        let out = rig.render(100);
        assert_eq!(out[99], 100.0);

        // Pausing fades out over 0.1 s (100 frames); afterwards it is silent
        // and the track does not move.
        rig.shared.paused.store(true, Ordering::Relaxed);
        let out = rig.render(300);
        assert!((out[0] - 101.0 * 0.99).abs() < 0.01);
        assert!(out[0] > out[50] && out[50] > out[98]);
        assert!(out[99] < 0.01 && out[100] < 0.01);
        assert!(out[101..].iter().all(|sample| *sample == 0.0));
        assert_eq!(rig.shared.state.load(Ordering::Relaxed), 1);

        // 100 frames in all were played before the pause took hold. Resuming
        // goes on with the 201st and rises over 100 frames.
        rig.shared.paused.store(false, Ordering::Relaxed);
        let out = rig.render(200);
        assert!((out[0] - 201.0 * 0.01).abs() < 0.01);
        assert!((out[99] - 300.0).abs() < 0.01);
        assert!((out[150] - 351.0).abs() < 0.01);
    }

    #[test]
    fn volume_changes_ramp_across_a_buffer() {
        let mut rig = Rig::new(tuning(0.0, 0.0, FadeCurve::Linear));
        rig.queue_constant(1, 100, 1.0);
        let mut tuned = rig.shared.tuning();
        tuned.volume = 0.0;
        rig.shared.set_tuning(&tuned);
        assert_eq!(rig.render(4), [0.75, 0.5, 0.25, 0.0]);
        assert_eq!(rig.render(2), [0.0, 0.0]);
    }

    /// Plays a long constant track with no fades and returns the volume in
    /// every frame of `secs` seconds, after announcing windows with `setup`.
    fn duck_levels(
        fade_out: f64,
        fade_in: f64,
        secs: usize,
        setup: impl FnOnce(&mut Rig) -> Vec<DuckWindow>,
    ) -> Vec<f32> {
        let mut rig = Rig::new(BgmTuning {
            duck_fade_out_secs: fade_out,
            duck_fade_in_secs: fade_in,
            ..tuning(0.0, 0.0, FadeCurve::Linear)
        });
        rig.queue_constant(1, secs * RATE as usize, 1.0);
        let _windows = setup(&mut rig);
        rig.render(secs * RATE as usize)
    }

    #[test]
    fn the_bgm_is_silent_when_a_video_starts_and_comes_back_after_it() {
        // A video from 5 s to 8 s, announced from the start: a 1 s fade out
        // that ends at 5 s, and a 2 s fade in from 8 s.
        let levels = duck_levels(1.0, 2.0, 12, |rig| vec![rig.ducker.open_window(5.0, 8.0)]);
        let at = |secs: f64| levels[(secs * 1000.0) as usize];
        assert_eq!(at(3.9), 1.0);
        assert!(close(at(4.5), 0.5));
        assert!(at(4.999) < 0.002);
        assert_eq!(at(5.0), 0.0);
        assert_eq!(at(7.9), 0.0);
        assert!(close(at(9.0), 0.5));
        assert_eq!(at(10.5), 1.0);
    }

    #[test]
    fn a_video_announced_too_late_fades_the_bgm_out_from_the_announcement() {
        let levels = duck_levels(1.0, 1.0, 12, |rig| {
            // The window opens when playback is at 4.5 s, for a video at 5 s.
            rig.render(4500);
            vec![rig.ducker.open_window(5.0, 8.0)]
        });
        let at = |secs: f64| levels[(secs * 1000.0) as usize];
        // The first 4.5 s were rendered by `setup`; the rest of the levels
        // start at 4.5 s.
        assert_eq!(at(0.0), 1.0);
        assert!(close(at(0.5), 0.5));
        assert!(at(0.999) < 0.002);
        assert_eq!(at(2.0), 0.0);
    }

    #[test]
    fn videos_close_together_keep_the_bgm_down() {
        // 5-8 s and 9-12 s: the second fade out (from 8 s) starts before
        // the BGM would be back (8 s plus 2 s), so it stays down throughout.
        let levels = duck_levels(1.0, 2.0, 16, |rig| {
            vec![
                rig.ducker.open_window(5.0, 8.0),
                rig.ducker.open_window(9.0, 12.0),
            ]
        });
        let at = |secs: f64| levels[(secs * 1000.0) as usize];
        assert_eq!(at(8.5), 0.0);
        assert_eq!(at(10.0), 0.0);
        assert_eq!(at(11.9), 0.0);
        assert!(close(at(13.0), 0.5));
        assert_eq!(at(14.5), 1.0);

        // Far enough apart (more than the fade in), the BGM returns fully.
        let levels = duck_levels(1.0, 2.0, 20, |rig| {
            vec![
                rig.ducker.open_window(5.0, 8.0),
                rig.ducker.open_window(13.0, 15.0),
            ]
        });
        let at = |secs: f64| levels[(secs * 1000.0) as usize];
        assert_eq!(at(10.5), 1.0);
        assert!(close(at(12.5), 0.5));
        assert_eq!(at(14.0), 0.0);
    }

    #[test]
    fn a_video_cut_short_brings_the_bgm_back_when_its_window_is_dropped() {
        let mut rig = Rig::new(BgmTuning {
            duck_fade_out_secs: 0.5,
            duck_fade_in_secs: 1.0,
            ..tuning(0.0, 0.0, FadeCurve::Linear)
        });
        rig.queue_constant(1, 12_000, 1.0);
        // A video from 1 s that would end at 9 s, skipped at 3 s.
        let window = rig.ducker.open_window(1.0, 9.0);
        let out = rig.render(3000);
        assert!(out[1000..].iter().all(|sample| *sample == 0.0));
        drop(window);
        let out = rig.render(2000);
        assert!(close(out[500], 0.5));
        assert_eq!(out[1200], 1.0);
    }

    #[test]
    fn a_video_that_will_not_play_does_not_dip_the_bgm() {
        let mut rig = Rig::new(BgmTuning {
            duck_fade_out_secs: 0.5,
            duck_fade_in_secs: 1.0,
            ..tuning(0.0, 0.0, FadeCurve::Linear)
        });
        rig.queue_constant(1, 6_000, 1.0);
        // Announced for 3 s to 5 s, then given up at 1 s.
        let window = rig.ducker.open_window(3.0, 5.0);
        rig.render(1000);
        drop(window);
        let out = rig.render(5000);
        assert!(out.iter().all(|sample| *sample == 1.0));
    }

    #[test]
    fn a_window_without_an_end_keeps_the_bgm_down_until_it_is_dropped() {
        let mut rig = Rig::new(BgmTuning {
            duck_fade_out_secs: 0.5,
            duck_fade_in_secs: 1.0,
            ..tuning(0.0, 0.0, FadeCurve::Linear)
        });
        rig.queue_constant(1, 10_000, 1.0);
        let window = rig.ducker.open_window(1.0, f64::INFINITY);
        let out = rig.render(5000);
        assert_eq!(out[900], 1.0 * 0.2);
        assert!(out[1000..].iter().all(|sample| *sample == 0.0));
        // Dropped at 5 s: it ends then, and the BGM returns over a second.
        drop(window);
        let out = rig.render(2000);
        assert_eq!(out[0], 0.0);
        assert!(close(out[500], 0.5));
        assert_eq!(out[1200], 1.0);
        // The window was forgotten once it was over.
        assert!(rig.shared.tuning().duck_fade_in_secs > 0.0);
    }

    #[test]
    fn the_shuffle_visits_every_track_each_round_and_is_fair() {
        let tracks: Vec<PathBuf> = (0..6).map(|n| PathBuf::from(format!("{n}.mp3"))).collect();
        let index = |path: &Path| tracks.iter().position(|track| track == path).unwrap();

        // Without shuffling the order is the playlist's, round after round.
        let mut plain = Playlist::new(tracks.clone(), false, 1);
        let order: Vec<usize> = (0..12).map(|_| index(plain.next_track())).collect();
        assert_eq!(order, [0, 1, 2, 3, 4, 5, 0, 1, 2, 3, 4, 5]);

        // Each shuffled round has every track once; no round opens with the
        // last track of the one before; positions are evenly used.
        let rounds = 6000;
        let mut shuffled = Playlist::new(tracks.clone(), true, 42);
        let mut counts = [[0usize; 6]; 6];
        let mut previous_last = None;
        let mut identical_to_plain = 0;
        for _ in 0..rounds {
            let round: Vec<usize> = (0..6).map(|_| index(shuffled.next_track())).collect();
            let mut sorted = round.clone();
            sorted.sort_unstable();
            assert_eq!(sorted, [0, 1, 2, 3, 4, 5], "{round:?}");
            assert_ne!(Some(round[0]), previous_last);
            previous_last = round.last().copied();
            for (position, track) in round.iter().enumerate() {
                counts[position][*track] += 1;
            }
            identical_to_plain += usize::from(round == [0, 1, 2, 3, 4, 5]);
        }
        let expected = rounds / 6;
        for (position, row) in counts.iter().enumerate() {
            for (track, count) in row.iter().enumerate() {
                // Each cell is about 1000 with a spread of about 30.
                assert!(
                    count.abs_diff(expected) < expected * 12 / 100,
                    "track {track} at position {position}: {count} of {rounds}"
                );
            }
        }
        // 1 in 720 rounds would be the plain order.
        assert!(identical_to_plain < 30, "{identical_to_plain}");

        // The same seed gives the same order; another seed another one.
        let order_of = |seed| {
            let mut playlist = Playlist::new(tracks.clone(), true, seed);
            (0..12)
                .map(|_| index(playlist.next_track()))
                .collect::<Vec<_>>()
        };
        assert_eq!(order_of(7), order_of(7));
        assert_ne!(order_of(7), order_of(8));

        // One track just repeats.
        let mut single = Playlist::new(vec![PathBuf::from("only.mp3")], true, 3);
        assert_eq!(single.next_track(), Path::new("only.mp3"));
        assert_eq!(single.next_track(), Path::new("only.mp3"));
    }

    #[test]
    fn a_folder_lists_the_music_files_in_it() {
        let dir = std::env::temp_dir().join(format!("fluxlide-bgm-listing-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("album.mp3")).unwrap();
        for name in ["b.mp3", "A.FLAC", "c.txt", ".hidden.mp3", "d.m4a", "noext"] {
            std::fs::write(dir.join(name), b"x").unwrap();
        }
        let names: Vec<String> = BgmSource::Folder(dir.clone())
            .tracks()
            .unwrap()
            .iter()
            .map(|path| path.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        // Sorted by name without regard to case; the folder named like a
        // song, the hidden file and the other kinds of files are left out.
        assert_eq!(names, ["A.FLAC", "b.mp3", "d.m4a"]);

        let empty = dir.join("album.mp3");
        let error = BgmSource::Folder(empty).tracks().unwrap_err().to_string();
        assert!(error.contains("no music files"), "{error}");
        assert!(BgmSource::Folder(dir.join("missing")).tracks().is_err());
        assert!(BgmSource::Files(Vec::new()).tracks().is_err());
        // Chosen files are used as they are, whatever their names.
        let chosen = vec![dir.join("noext"), dir.join("c.txt")];
        assert_eq!(BgmSource::Files(chosen.clone()).tracks().unwrap(), chosen);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn tunings_out_of_range_are_refused() {
        assert!(BgmTuning::default().validate().is_ok());
        for bad in [
            BgmTuning {
                volume: 1.1,
                ..BgmTuning::default()
            },
            BgmTuning {
                fade_secs: -1.0,
                ..BgmTuning::default()
            },
            BgmTuning {
                fade_secs: MAX_FADE_SECS + 0.1,
                ..BgmTuning::default()
            },
            BgmTuning {
                gap_secs: f64::NAN,
                ..BgmTuning::default()
            },
            BgmTuning {
                duck_fade_in_secs: 100.0,
                ..BgmTuning::default()
            },
        ] {
            assert!(bad.validate().is_err(), "{bad:?}");
        }
    }
    /// Renders the mixer in real time, 10 ms at a time.
    struct Player {
        offline: OfflineMixer,
        rate: u32,
        frames_done: usize,
    }

    impl Player {
        fn new(rate: u32) -> Self {
            Self {
                offline: OfflineMixer::export(rate),
                rate,
                frames_done: 0,
            }
        }

        /// Plays the next 10 ms and returns their RMS level.
        fn step(&mut self) -> f32 {
            let frames = self.rate as usize / 100;
            let out = self
                .offline
                .render(frames, self.frames_done as f64 / f64::from(self.rate));
            self.frames_done += frames;
            std::thread::sleep(Duration::from_millis(10));
            (out.iter().map(|sample| sample * sample).sum::<f32>() / out.len() as f32).sqrt()
        }
    }

    /// A folder with a broken file and three music files, or `None` when
    /// `ffmpeg` is not there.
    fn music_folder(name: &str, secs: f64) -> Option<(Ffmpeg, PathBuf)> {
        let Ok(ffmpeg) = Ffmpeg::locate() else {
            eprintln!("skipping BGM test: ffmpeg not found");
            return None;
        };
        let dir = std::env::temp_dir().join(format!("fluxlide-bgm-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("0-broken.mp3"), b"this is not music").unwrap();
        let made = make_tone(&ffmpeg, &dir.join("1-a.wav"), 440, secs, &[]).is_some()
            && make_tone(&ffmpeg, &dir.join("2-b.flac"), 660, secs, &[]).is_some()
            && make_tone(
                &ffmpeg,
                &dir.join("3-c.m4a"),
                880,
                secs,
                &[
                    "-metadata",
                    "title=Third",
                    "-metadata",
                    "artist=Tester",
                    "-c:a",
                    "aac",
                ],
            )
            .is_some();
        if !made {
            eprintln!("skipping BGM test: could not create music files");
            return None;
        }
        Some((ffmpeg, dir))
    }

    fn real_time_config(fade: f64, gap: f64) -> BgmConfig {
        BgmConfig {
            tuning: tuning(fade, gap, FadeCurve::EqualPower),
            ..BgmConfig::default()
        }
    }

    #[test]
    fn a_folder_plays_through_with_gaps_skipping_the_broken_file() {
        let Some((ffmpeg, dir)) = music_folder("folder", 0.6) else {
            return;
        };
        let mut player = Player::new(8000);
        let bgm = Bgm::start(
            ffmpeg,
            &player.offline.mixer(),
            &BgmSource::Folder(dir.clone()),
            real_time_config(0.1, 0.2),
        )
        .unwrap();

        // Play until the second round has begun, 10 ms of level at a time.
        let mut levels = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(15);
        while bgm.stats().tracks_started < 4 && Instant::now() < deadline {
            levels.push(player.step());
        }
        let stats = bgm.stats();
        assert_eq!(stats.tracks_started, 4, "{stats:?}");

        // The broken file was skipped with a report, each time round. The
        // events come in the order: failure of the first file, then the
        // three tracks, then the failure again while the third one plays.
        let events = bgm.drain_events();
        let title = |event: &BgmEvent| match event {
            BgmEvent::TrackStarted(now_playing) => now_playing.title.clone(),
            other => format!("{other:?}"),
        };
        assert!(
            matches!(&events[0], BgmEvent::TrackFailed { path, reason }
                if path.ends_with("0-broken.mp3") && !reason.is_empty()),
            "{events:?}"
        );
        assert_eq!(
            events[1..4].iter().map(title).collect::<Vec<_>>(),
            ["1-a", "2-b", "Third"]
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, BgmEvent::TrackFailed { .. }))
                .count() as u64,
            stats.tracks_failed
        );
        assert!(stats.tracks_failed >= 2, "{stats:?}");
        assert!(
            stats
                .last_error
                .as_deref()
                .unwrap()
                .contains("0-broken.mp3")
        );
        let third = match &events[3] {
            BgmEvent::TrackStarted(now_playing) => now_playing.clone(),
            other => panic!("{other:?}"),
        };
        assert_eq!(third.artist.as_deref(), Some("Tester"));
        assert_eq!(third.seq, 3);

        // Three bursts of sound with silent gaps of about 0.2 s between them,
        // and nothing late: the next track was always ready.
        let audible = |level: &f32| *level > 0.005;
        let mut runs: Vec<(bool, usize)> = Vec::new();
        for level in &levels {
            match runs.last_mut() {
                Some((was, length)) if *was == audible(level) => *length += 1,
                _ => runs.push((audible(level), 1)),
            }
        }
        let secs = |length: usize| length as f64 / 100.0;
        let bursts: Vec<f64> = runs
            .iter()
            .filter(|run| run.0)
            .map(|run| secs(run.1))
            .collect();
        let silences: Vec<f64> = runs
            .iter()
            .filter(|run| !run.0)
            .map(|run| secs(run.1))
            .collect();
        assert!(bursts.len() >= 3, "{runs:?}");
        for burst in &bursts[..3] {
            assert!((0.5..0.8).contains(burst), "{bursts:?}");
        }
        // The first silence is the wait for the first track to be decoded.
        for gap in &silences[1..3] {
            assert!((0.18..0.3).contains(gap), "{silences:?}");
        }
        assert_eq!(stats.underruns, 0, "{stats:?}");
        assert_eq!(stats.starved_transitions, 0, "{stats:?}");

        drop(bgm);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn next_and_pause_and_resume_work_on_the_playing_music() {
        let Some((ffmpeg, dir)) = music_folder("controls", 30.0) else {
            return;
        };
        let mut player = Player::new(8000);
        let bgm = Bgm::start(
            ffmpeg,
            &player.offline.mixer(),
            &BgmSource::Folder(dir.clone()),
            real_time_config(0.3, 2.0),
        )
        .unwrap();
        let deadline = Instant::now() + Duration::from_secs(15);
        while bgm.now_playing().is_none() && Instant::now() < deadline {
            player.step();
        }
        assert_eq!(bgm.now_playing().unwrap().title, "1-a");
        assert_eq!(bgm.stats().state, BgmState::Playing);
        // Let it reach full level.
        for _ in 0..50 {
            player.step();
        }

        // Pausing silences it and holds the track; resuming brings it back.
        bgm.pause();
        assert!(bgm.is_paused() && bgm.stats().paused);
        let silent: Vec<f32> = (0..30).map(|_| player.step()).collect();
        assert!(silent[25..].iter().all(|level| *level == 0.0), "{silent:?}");
        bgm.resume();
        let resumed: Vec<f32> = (0..30).map(|_| player.step()).collect();
        assert!(resumed[25] > 0.05, "{resumed:?}");

        // Next fades out (0.3 s) and starts the next track although the gap
        // is 2 s; the title follows once the controller has seen it.
        bgm.next();
        let before = bgm.stats().tracks_started;
        let started = Instant::now();
        let mut levels = Vec::new();
        while bgm.stats().tracks_started == before && started.elapsed() < Duration::from_secs(3) {
            levels.push(player.step());
        }
        assert_eq!(bgm.stats().tracks_started, before + 1);
        assert!(levels.len() < 60, "{}", levels.len());
        for _ in 0..30 {
            player.step();
        }
        assert_eq!(bgm.now_playing().unwrap().title, "2-b");
        assert!(bgm.stats().tracks_failed >= 1);

        // The volume and the other settings can be changed while it plays.
        bgm.set_volume(0.25).unwrap();
        assert_eq!(bgm.tuning().volume, 0.25);
        assert!(bgm.set_volume(2.0).is_err());
        assert_eq!(bgm.tuning().volume, 0.25);
        drop(bgm);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn when_every_track_is_broken_the_bgm_says_so_and_stops_trying() {
        let Ok(ffmpeg) = Ffmpeg::locate() else {
            eprintln!("skipping BGM test: ffmpeg not found");
            return;
        };
        let dir = std::env::temp_dir().join(format!("fluxlide-bgm-broken-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let files: Vec<PathBuf> = ["one.mp3", "two.mp3"]
            .iter()
            .map(|name| {
                std::fs::write(dir.join(name), b"not music").unwrap();
                dir.join(name)
            })
            .collect();
        let offline = OfflineMixer::export(8000);
        let bgm = Bgm::start(
            ffmpeg,
            &offline.mixer(),
            &BgmSource::Files(files),
            BgmConfig::default(),
        )
        .unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while !bgm.stats().all_tracks_failed && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        std::thread::sleep(Duration::from_millis(200));
        let stats = bgm.stats();
        assert!(stats.all_tracks_failed, "{stats:?}");
        assert_eq!(stats.tracks_failed, 2, "it kept trying: {stats:?}");
        assert_eq!(stats.tracks_started, 0);
        let events = bgm.drain_events();
        assert_eq!(events.len(), 3, "{events:?}");
        assert_eq!(events[2], BgmEvent::AllTracksFailed);

        // Starting without anything to play, or with bad settings, is an error.
        let empty_dir = dir.join("empty");
        std::fs::create_dir_all(&empty_dir).unwrap();
        let ffmpeg = Ffmpeg::locate().unwrap();
        assert!(
            Bgm::start(
                ffmpeg.clone(),
                &offline.mixer(),
                &BgmSource::Folder(empty_dir),
                BgmConfig::default()
            )
            .is_err()
        );
        let bad = BgmConfig {
            tuning: BgmTuning {
                volume: -1.0,
                ..BgmTuning::default()
            },
            ..BgmConfig::default()
        };
        assert!(
            Bgm::start(
                ffmpeg,
                &offline.mixer(),
                &BgmSource::Files(vec![dir.join("one.mp3")]),
                bad
            )
            .is_err()
        );
        drop(bgm);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
