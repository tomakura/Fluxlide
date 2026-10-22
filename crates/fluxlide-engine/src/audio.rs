//! Sound output kept in step with the picture.
//!
//! Playback time is the picture's clock: seconds since an `epoch` instant
//! shared with the render loop. Every sound (a [`Voice`]) is placed on that
//! clock by its start time. Each time the device asks for samples, the mixer
//! works out the playback time at which that buffer will be heard (now plus
//! the output latency the device reports) and compares it with how far each
//! voice has played. Small differences are left alone so playback stays
//! continuous; differences above [`RESYNC_SECS`] are corrected by skipping
//! samples or inserting silence.
//!
//! # Sync compensation
//!
//! Some outputs, Bluetooth above all, are heard later than the device
//! reports. The user states the missing delay as a sync offset in
//! milliseconds ([`AudioMixer::set_sync_offset_ms`]). A positive offset makes
//! the mixer deliver, at every moment, the sound of a later playback time, so
//! the sound is heard together with the picture. That sound lies in the
//! future of the picture, so a sound that is planned ahead of time can start
//! early (see [`AudioMixer::lookahead_secs`]) while one that cannot be
//! planned starts late and has its beginning skipped. A negative offset
//! delays the sound.
//!
//! When the offset changes while sound plays, the mixer does not jump: it
//! reads each synced voice at 1 ± 1/50 of normal speed until the new offset
//! is reached (a 10 ms change takes half a second; a change above
//! [`SLEW_LIMIT_SECS`] jumps and the voices resynchronize). The shift is
//! counted in 1/50 frames so that voices return to reading whole frames
//! exactly when it is done.
//!
//! The compensation belongs to live playback. A mixer made for export
//! ([`AudioMode::Export`], see [`OfflineMixer`]) always works at offset 0 and
//! refuses to be given one, so an exported file never carries a correction
//! meant for one particular room.
//!
//! # Sources
//!
//! Besides voices, which play decoded samples that were prepared for one
//! playback time, a [`Source`] generates its sound on the audio thread from
//! the time of every frame it renders. Background music and the test beep
//! are sources.

use std::f64::consts::FRAC_PI_2;
use std::ops::RangeInclusive;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SampleFormat, SizedSample};

use crate::EngineError;

/// Sounds are fed to the mixer as interleaved stereo `f32` samples.
pub const TRACK_CHANNELS: usize = 2;
/// Offsets between sound and picture below this are not corrected.
pub const RESYNC_SECS: f64 = 0.03;
/// Range accepted for the sync offset, in milliseconds. Positive plays the
/// sound earlier (for outputs that are heard late), negative later.
pub const SYNC_OFFSET_RANGE_MS: RangeInclusive<f64> = -500.0..=1000.0;
/// Changes of the sync offset up to this size are made gradually; larger ones
/// jump.
pub const SLEW_LIMIT_SECS: f64 = 0.2;
/// A frame is read in this many parts while the sync offset moves, which is
/// also the reciprocal of the speed change (2%).
const PHASE_UNITS: u32 = 50;

/// How a sound's volume follows a fade from 0 to 1 (and back).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FadeCurve {
    /// Volume proportional to progress. Two sounds crossfaded this way are
    /// 3 dB too quiet in the middle when they are unrelated.
    Linear,
    /// Volume follows a quarter sine wave, so that the powers of a sound
    /// fading out and an unrelated one fading in add up to a constant.
    #[default]
    EqualPower,
}

impl FadeCurve {
    /// Volume for a fade that has gone `progress` (0 to 1) of the way up. A
    /// fade down is `rise(1.0 - progress)`.
    pub fn rise(self, progress: f64) -> f32 {
        let progress = progress.clamp(0.0, 1.0);
        match self {
            Self::Linear => progress as f32,
            Self::EqualPower => (progress * FRAC_PI_2).sin() as f32,
        }
    }
}

/// What the sound is for, which decides whether sync compensation applies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioMode {
    /// Playing to a real output in a room: the sync offset applies.
    Live,
    /// Rendering for a file: the sync offset is always 0.
    Export,
}

/// The output's sample rate, which every sound is resampled to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioFormat {
    pub sample_rate: u32,
}

/// Measurements for judging whether sound follows the picture.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct AudioStats {
    pub device: String,
    pub sample_rate: u32,
    /// Time from handing samples to the device until they are heard.
    pub output_latency_ms: f64,
    /// Sync offset the user asked for. The mixer may still be moving to it.
    pub sync_offset_ms: f64,
    /// Sound minus picture position for the latest buffer; positive means
    /// the sound is behind. Measured before any correction.
    pub offset_ms: f64,
    /// Largest absolute offset seen after each sound's first buffer.
    pub max_offset_ms: f64,
    /// Corrections made because the offset exceeded [`RESYNC_SECS`].
    pub resyncs: u64,
    /// Buffers in which a sound ran out of decoded samples before its end.
    pub underruns: u64,
    /// Latest error reported by the output device.
    pub last_error: Option<String>,
}

#[derive(Default)]
struct SharedStats {
    output_latency_ms: AtomicF64,
    offset_ms: AtomicF64,
    max_offset_ms: AtomicF64,
    resyncs: AtomicU64,
    underruns: AtomicU64,
    last_error: Mutex<Option<String>>,
}

/// An `f64` stored in an atomic, for numbers written by the audio thread.
#[derive(Default)]
pub(crate) struct AtomicF64(AtomicU64);

impl AtomicF64 {
    pub(crate) fn new(value: f64) -> Self {
        Self(AtomicU64::new(value.to_bits()))
    }

    pub(crate) fn load(&self) -> f64 {
        f64::from_bits(self.0.load(Ordering::Relaxed))
    }

    pub(crate) fn store(&self, value: f64) {
        self.0.store(value.to_bits(), Ordering::Relaxed);
    }
}

/// The sync offset the user asked for, shared with the audio thread.
struct SyncControl {
    mode: AudioMode,
    offset_ms: AtomicF64,
}

/// Settings the owner of a sound changes while it plays.
struct VoiceControl {
    /// Linear gain as `f32` bits.
    gain: AtomicU32,
    stopped: AtomicBool,
    /// Set by the decoder once it has written its last sample.
    finished: Arc<AtomicBool>,
}

/// Keeps a sound playing; dropping it stops the sound.
pub struct VoiceHandle {
    control: Arc<VoiceControl>,
}

impl VoiceHandle {
    /// Volume from 0 (silent) to 1. Changes are ramped over one buffer.
    pub fn set_gain(&self, gain: f32) {
        self.control
            .gain
            .store(gain.clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
    }
}

impl Drop for VoiceHandle {
    fn drop(&mut self) {
        self.control.stopped.store(true, Ordering::Relaxed);
    }
}

/// A sound being played by the mixer.
struct Voice {
    samples: rtrb::Consumer<f32>,
    /// Playback time of the first sample.
    start: f64,
    control: Arc<VoiceControl>,
    /// Frames played or skipped so far: the index of the next frame to hear,
    /// which is `current` when that is loaded.
    position: u64,
    /// How far the read position is past the frame at `position`, in
    /// `1 / PHASE_UNITS` frames. Non-zero only while the sync offset moves,
    /// and then the sound is the blend of `current` and `following`.
    phase: u32,
    /// The frame at `position`, taken from `samples` ahead of time.
    current: Option<[f32; TRACK_CHANNELS]>,
    /// The frame after `current`. Only present while `current` is.
    following: Option<[f32; TRACK_CHANNELS]>,
    /// Gain applied at the end of the previous buffer.
    gain: f32,
    /// Whether the first sample has been placed on the clock.
    started: bool,
}

impl Voice {
    fn new(
        samples: rtrb::Consumer<f32>,
        start: f64,
        control: Arc<VoiceControl>,
        gain: f32,
    ) -> Self {
        Self {
            samples,
            start,
            control,
            position: 0,
            phase: 0,
            current: None,
            following: None,
            gain,
            started: false,
        }
    }

    fn pop_frame(&mut self) -> Option<[f32; TRACK_CHANNELS]> {
        if self.samples.slots() < TRACK_CHANNELS {
            return None;
        }
        Some([
            self.samples.pop().unwrap_or(0.0),
            self.samples.pop().unwrap_or(0.0),
        ])
    }

    /// Takes the frame at `position` and the one after it from the decoder,
    /// as far as they are decoded.
    fn load(&mut self) {
        if self.current.is_none() {
            self.current = self.pop_frame();
        }
        if self.current.is_some() && self.following.is_none() {
            self.following = self.pop_frame();
        }
    }

    /// Drops the frame at `position`. Returns false if none is decoded.
    fn skip_frame(&mut self) -> bool {
        if self.current.is_none() {
            self.current = self.pop_frame();
        }
        if self.current.is_none() {
            return false;
        }
        self.current = self.following.take();
        self.position += 1;
        true
    }

    /// Whether every frame has been played and none will arrive.
    fn is_drained(&self) -> bool {
        self.current.is_none()
            && self.following.is_none()
            && self.samples.is_empty()
            && self.control.finished.load(Ordering::Relaxed)
    }
}

/// A sound generated on the audio thread.
pub trait Source: Send {
    /// Adds the sound for the frames of `timeline` to `out`, interleaved
    /// stereo with `timeline.frames()` frames. Runs on the audio thread, so it
    /// must not block.
    fn render(&mut self, out: &mut [f32], timeline: &Timeline<'_>);

    /// For export: whether the next `frames` frames can be rendered as
    /// they will sound, without waiting for a decoder. Live playback does
    /// not ask.
    fn ready_for(&mut self, _frames: usize) -> bool {
        true
    }
}

/// Keeps a [`Source`] playing; dropping it removes the source.
pub struct SourceHandle {
    stopped: Arc<AtomicBool>,
}

impl Drop for SourceHandle {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::Relaxed);
    }
}

struct SourceSlot {
    source: Box<dyn Source>,
    stopped: Arc<AtomicBool>,
}

/// The playback time of each frame of one buffer, with the sync offset in
/// effect for it.
pub struct Timeline<'a> {
    sample_rate: f64,
    /// Playback time at which the first frame is heard, before any offset.
    heard_at: f64,
    /// Sync offset at the start of every frame, in `1 / PHASE_UNITS` frames.
    offsets: &'a [i64],
    /// How many `1 / PHASE_UNITS` frames a voice moves on after each frame.
    steps: &'a [u32],
}

impl Timeline<'_> {
    pub fn frames(&self) -> usize {
        self.offsets.len()
    }

    pub fn sample_rate(&self) -> f64 {
        self.sample_rate
    }

    /// Playback time of the sound that `frame` should carry: the time it is
    /// heard plus the sync offset. Always rises by about one frame per frame.
    pub fn time(&self, frame: usize) -> f64 {
        let shift = self.offsets[frame] as f64 / f64::from(PHASE_UNITS);
        self.heard_at + (frame as f64 + shift) / self.sample_rate
    }
}

/// Sends sounds to the output. Cheap to clone and usable from any thread.
#[derive(Clone)]
pub struct AudioMixer {
    format: AudioFormat,
    device: Arc<str>,
    voices: Sender<Voice>,
    sources: Sender<SourceSlot>,
    stats: Arc<SharedStats>,
    sync: Arc<SyncControl>,
}

impl AudioMixer {
    pub fn format(&self) -> AudioFormat {
        self.format
    }

    pub fn mode(&self) -> AudioMode {
        self.sync.mode
    }

    /// Plays interleaved stereo samples from `samples`, the first one heard
    /// at playback time `start` (with the sync offset applied, so it is
    /// heard together with the picture of that time). `finished` tells
    /// silence at the end of the sound apart from a decoder falling behind.
    pub fn play(
        &self,
        samples: rtrb::Consumer<f32>,
        finished: Arc<AtomicBool>,
        start: f64,
        gain: f32,
    ) -> VoiceHandle {
        let control = Arc::new(VoiceControl {
            gain: AtomicU32::new(gain.to_bits()),
            stopped: AtomicBool::new(false),
            finished,
        });
        let _ = self
            .voices
            .send(Voice::new(samples, start, control.clone(), gain));
        VoiceHandle { control }
    }

    /// Adds a generated sound to the mix.
    pub fn add_source(&self, source: Box<dyn Source>) -> SourceHandle {
        let stopped = Arc::new(AtomicBool::new(false));
        let _ = self.sources.send(SourceSlot {
            source,
            stopped: stopped.clone(),
        });
        SourceHandle { stopped }
    }

    /// Sets the sync offset (see the module documentation). Only live output
    /// has one: an export mixer returns an error.
    pub fn set_sync_offset_ms(&self, offset_ms: f64) -> Result<(), EngineError> {
        check_sync_offset(self.sync.mode, offset_ms)?;
        self.sync.offset_ms.store(offset_ms);
        Ok(())
    }

    /// The sync offset set last, 0 for an export mixer.
    pub fn sync_offset_ms(&self) -> f64 {
        self.sync.offset_ms.load()
    }

    /// How far ahead of the picture's clock the sound being delivered now
    /// lies: the output latency plus the sync offset. A sound that should be
    /// heard with the picture at time `t` must be handed over before
    /// `t - lookahead_secs()` (plus the time the audio thread needs to pick
    /// it up). Negative if the sync offset delays the sound by more than the
    /// output latency.
    pub fn lookahead_secs(&self) -> f64 {
        self.stats.output_latency_ms.load() / 1000.0 + self.sync.offset_ms.load() / 1000.0
    }

    pub fn stats(&self) -> AudioStats {
        AudioStats {
            device: self.device.to_string(),
            sample_rate: self.format.sample_rate,
            output_latency_ms: self.stats.output_latency_ms.load(),
            sync_offset_ms: self.sync.offset_ms.load(),
            offset_ms: self.stats.offset_ms.load(),
            max_offset_ms: self.stats.max_offset_ms.load(),
            resyncs: self.stats.resyncs.load(Ordering::Relaxed),
            underruns: self.stats.underruns.load(Ordering::Relaxed),
            last_error: self.stats.last_error.lock().unwrap().clone(),
        }
    }
}

fn check_sync_offset(mode: AudioMode, offset_ms: f64) -> Result<(), EngineError> {
    if mode == AudioMode::Export {
        return Err(EngineError::Audio(
            "the sync offset applies to live playback only, not to export".into(),
        ));
    }
    if !SYNC_OFFSET_RANGE_MS.contains(&offset_ms) {
        return Err(EngineError::Audio(format!(
            "the sync offset must be between {} and {} ms, not {offset_ms}",
            SYNC_OFFSET_RANGE_MS.start(),
            SYNC_OFFSET_RANGE_MS.end(),
        )));
    }
    Ok(())
}

/// Mixes the active voices into output buffers. Runs on the audio thread.
struct MixerCore {
    sample_rate: f64,
    out_channels: usize,
    voices: Vec<Voice>,
    incoming: Receiver<Voice>,
    sources: Vec<SourceSlot>,
    incoming_sources: Receiver<SourceSlot>,
    /// Stereo mix of the sources for one buffer, reused between buffers.
    scratch: Vec<f32>,
    stats: Arc<SharedStats>,
    sync: Arc<SyncControl>,
    /// The sync offset in effect, in `1 / PHASE_UNITS` frames.
    applied: i64,
    /// Per-frame timeline of the buffer being rendered, reused between buffers.
    offsets: Vec<i64>,
    steps: Vec<u32>,
}

/// Builds the mixer for an output of `sample_rate` and `out_channels`.
fn build_mixer(
    sample_rate: u32,
    out_channels: usize,
    device: &str,
    mode: AudioMode,
    sync_offset_ms: f64,
) -> (MixerCore, AudioMixer) {
    let (voices, incoming) = mpsc::channel();
    let (sources, incoming_sources) = mpsc::channel();
    let stats = Arc::new(SharedStats::default());
    let sync = Arc::new(SyncControl {
        mode,
        offset_ms: AtomicF64::new(sync_offset_ms),
    });
    let rate = f64::from(sample_rate);
    let core = MixerCore {
        sample_rate: rate,
        out_channels: out_channels.max(1),
        voices: Vec::new(),
        incoming,
        sources: Vec::new(),
        incoming_sources,
        scratch: Vec::new(),
        stats: stats.clone(),
        sync: sync.clone(),
        applied: offset_in_phase_units(sync_offset_ms, rate),
        offsets: Vec::new(),
        steps: Vec::new(),
    };
    let mixer = AudioMixer {
        format: AudioFormat { sample_rate },
        device: device.into(),
        voices,
        sources,
        stats,
        sync,
    };
    (core, mixer)
}

/// `offset_ms` as a whole number of frames, in `1 / PHASE_UNITS` frames.
fn offset_in_phase_units(offset_ms: f64, sample_rate: f64) -> i64 {
    (offset_ms / 1000.0 * sample_rate).round() as i64 * i64::from(PHASE_UNITS)
}

impl MixerCore {
    /// Works out, for each frame of a buffer of `frames`, the sync offset in
    /// effect and how fast voices read: normally one frame per frame, 2%
    /// faster or slower while the offset moves towards what was asked for.
    fn plan_timeline(&mut self, frames: usize) {
        let target = offset_in_phase_units(self.sync.offset_ms.load(), self.sample_rate);
        let limit = (SLEW_LIMIT_SECS * self.sample_rate) as i64 * i64::from(PHASE_UNITS);
        if (target - self.applied).abs() > limit {
            self.applied = target;
        }
        self.offsets.clear();
        self.steps.clear();
        for _ in 0..frames {
            let direction = (target - self.applied).signum();
            self.offsets.push(self.applied);
            self.steps.push((PHASE_UNITS as i64 + direction) as u32);
            self.applied += direction;
        }
    }

    /// Fills `out` (interleaved, `out_channels` per frame) with the sound due
    /// from playback time `heard_at` on.
    fn render(&mut self, out: &mut [f32], heard_at: f64) {
        out.fill(0.0);
        self.voices.extend(self.incoming.try_iter());
        self.voices
            .retain(|voice| !voice.control.stopped.load(Ordering::Relaxed));
        self.sources.extend(self.incoming_sources.try_iter());
        self.sources
            .retain(|slot| !slot.stopped.load(Ordering::Relaxed));

        let frames = out.len() / self.out_channels;
        if frames == 0 {
            return;
        }
        self.plan_timeline(frames);
        let timeline = Timeline {
            sample_rate: self.sample_rate,
            heard_at,
            offsets: &self.offsets,
            steps: &self.steps,
        };

        let resync_frames = RESYNC_SECS * self.sample_rate;
        for voice in &mut self.voices {
            let finished = voice.control.finished.load(Ordering::Relaxed);
            if voice.is_drained() {
                // The sound has ended; its picture may still be on screen.
                continue;
            }
            // Frames the voice should have played by the start of this buffer.
            let due = (timeline.time(0) - voice.start) * self.sample_rate;
            let behind =
                due - voice.position as f64 - f64::from(voice.phase) / f64::from(PHASE_UNITS);
            let mut first_frame = 0;
            if !voice.started {
                if behind < -(frames as f64) {
                    continue; // Starts after this buffer.
                }
                voice.started = true;
                align(voice, behind, frames, &mut first_frame);
                // Read from here on in step with the moving offset.
                voice.phase = timeline.offsets[first_frame.min(frames - 1)]
                    .rem_euclid(i64::from(PHASE_UNITS)) as u32;
            } else {
                let offset_ms = behind / self.sample_rate * 1000.0;
                self.stats.offset_ms.store(offset_ms);
                if offset_ms.abs() > self.stats.max_offset_ms.load() {
                    self.stats.max_offset_ms.store(offset_ms.abs());
                }
                if behind.abs() > resync_frames {
                    self.stats.resyncs.fetch_add(1, Ordering::Relaxed);
                    align(voice, behind, frames, &mut first_frame);
                }
            }

            let target = f32::from_bits(voice.control.gain.load(Ordering::Relaxed));
            let step = (target - voice.gain) / frames.max(1) as f32;
            for frame in first_frame..frames {
                voice.load();
                let Some(current) = voice.current else {
                    if !finished {
                        self.stats.underruns.fetch_add(1, Ordering::Relaxed);
                    }
                    break;
                };
                let [left, right] = if voice.phase == 0 {
                    current
                } else {
                    match voice.following {
                        Some(next) => {
                            let blend = voice.phase as f32 / PHASE_UNITS as f32;
                            [
                                current[0] + (next[0] - current[0]) * blend,
                                current[1] + (next[1] - current[1]) * blend,
                            ]
                        }
                        // The last frame of a sound has nothing to blend with.
                        None if voice.samples.is_empty() && finished => current,
                        None => {
                            self.stats.underruns.fetch_add(1, Ordering::Relaxed);
                            break;
                        }
                    }
                };
                let gain = voice.gain + step * (frame + 1) as f32;
                let frame_out = &mut out[frame * self.out_channels..][..self.out_channels];
                add_stereo(frame_out, left * gain, right * gain);

                voice.phase += timeline.steps[frame];
                while voice.phase >= PHASE_UNITS {
                    voice.phase -= PHASE_UNITS;
                    voice.skip_frame();
                }
            }
            voice.gain = target;
        }

        if !self.sources.is_empty() {
            self.scratch.clear();
            self.scratch.resize(frames * TRACK_CHANNELS, 0.0);
            for slot in &mut self.sources {
                slot.source.render(&mut self.scratch, &timeline);
            }
            for (frame, stereo) in self
                .scratch
                .as_chunks::<TRACK_CHANNELS>()
                .0
                .iter()
                .enumerate()
            {
                let frame_out = &mut out[frame * self.out_channels..][..self.out_channels];
                add_stereo(frame_out, stereo[0], stereo[1]);
            }
        }
    }
}

/// Adds one stereo frame to an output frame: mono outputs get the average of
/// both channels and outputs with more than two channels use the first two.
fn add_stereo(out: &mut [f32], left: f32, right: f32) {
    if let [only] = out {
        *only += (left + right) * 0.5;
    } else {
        out[0] += left;
        out[1] += right;
    }
}

/// Moves a voice to where the clock says it should be: drops samples it is
/// behind by, or delays its first sample within this buffer if it is ahead.
fn align(voice: &mut Voice, behind: f64, frames: usize, first_frame: &mut usize) {
    if behind > 0.0 {
        for _ in 0..behind.round() as usize {
            if !voice.skip_frame() {
                break;
            }
        }
    } else {
        *first_frame = ((-behind).round() as usize).min(frames);
    }
}

/// An output made for rendering in memory instead of to a device: the
/// foundation for mixing the sound of an exported video, and for tests.
/// Time is whatever the caller passes to [`OfflineMixer::render`].
pub struct OfflineMixer {
    core: MixerCore,
    mixer: AudioMixer,
}

impl OfflineMixer {
    /// A mixer for export: no sync compensation.
    pub fn export(sample_rate: u32) -> Self {
        Self::new(sample_rate, AudioMode::Export, 0.0)
    }

    /// A mixer that behaves like live playback, starting at `sync_offset_ms`.
    pub fn live(sample_rate: u32, sync_offset_ms: f64) -> Result<Self, EngineError> {
        check_sync_offset(AudioMode::Live, sync_offset_ms)?;
        Ok(Self::new(sample_rate, AudioMode::Live, sync_offset_ms))
    }

    fn new(sample_rate: u32, mode: AudioMode, sync_offset_ms: f64) -> Self {
        let (core, mixer) =
            build_mixer(sample_rate, TRACK_CHANNELS, "offline", mode, sync_offset_ms);
        Self { core, mixer }
    }

    pub fn mixer(&self) -> AudioMixer {
        self.mixer.clone()
    }

    /// Whether every sound has been decoded far enough to render the next
    /// `frames` frames without running dry. Export waits for this before each
    /// [`OfflineMixer::render`], since a file must not get gaps where live
    /// playback would have an underrun.
    pub fn has_samples_for(&mut self, frames: usize) -> bool {
        let core = &mut self.core;
        core.voices.extend(core.incoming.try_iter());
        // One frame more than asked for: a voice also reads the following frame.
        let needed = (frames + 1) * TRACK_CHANNELS;
        let voices = core.voices.iter().all(|voice| {
            voice.control.stopped.load(Ordering::Relaxed)
                || voice.control.finished.load(Ordering::Relaxed)
                || voice.samples.slots() >= needed
        });
        core.sources.extend(core.incoming_sources.try_iter());
        core.sources
            .retain(|slot| !slot.stopped.load(Ordering::Relaxed));
        voices
            && core
                .sources
                .iter_mut()
                .all(|slot| slot.source.ready_for(frames))
    }

    /// Mixes `frames` frames of stereo sound whose first frame is heard at
    /// playback time `heard_at`. Pass consecutive times, one buffer after
    /// the other.
    pub fn render(&mut self, frames: usize, heard_at: f64) -> Vec<f32> {
        let mut out = vec![0.0; frames * TRACK_CHANNELS];
        self.core.render(&mut out, heard_at);
        out
    }
}

/// The open output device. Keep it alive for as long as sound should play;
/// it stays on the thread that opened it.
pub struct AudioOutput {
    _stream: cpal::Stream,
    mixer: AudioMixer,
}

impl AudioOutput {
    /// Opens the default output device. `epoch` is the instant playback time
    /// counts from.
    pub fn open(epoch: Instant) -> Result<Self, EngineError> {
        Self::open_with_sync_offset(epoch, 0.0)
    }

    /// Like [`AudioOutput::open`], starting at the sync offset
    /// `sync_offset_ms` instead of moving to it gradually.
    pub fn open_with_sync_offset(epoch: Instant, sync_offset_ms: f64) -> Result<Self, EngineError> {
        check_sync_offset(AudioMode::Live, sync_offset_ms)?;
        let fail =
            |what: &str, err: &dyn std::fmt::Display| EngineError::Audio(format!("{what}: {err}"));
        let device = cpal::default_host()
            .default_output_device()
            .ok_or_else(|| EngineError::Audio("no audio output device".into()))?;
        let name = device
            .description()
            .map(|description| description.name().to_string())
            .unwrap_or_else(|_| "unknown".into());
        let supported = device
            .default_output_config()
            .map_err(|err| fail("no usable output format", &err))?;
        let config = supported.config();
        let (core, mixer) = build_mixer(
            config.sample_rate,
            usize::from(config.channels),
            &name,
            AudioMode::Live,
            sync_offset_ms,
        );
        let stream = match supported.sample_format() {
            SampleFormat::F32 => build_stream::<f32>(&device, &config, core, epoch),
            SampleFormat::I16 => build_stream::<i16>(&device, &config, core, epoch),
            SampleFormat::U16 => build_stream::<u16>(&device, &config, core, epoch),
            SampleFormat::I32 => build_stream::<i32>(&device, &config, core, epoch),
            other => {
                return Err(EngineError::Audio(format!(
                    "unsupported sample format {other}"
                )));
            }
        }
        .map_err(|err| fail("could not open the audio output", &err))?;
        stream
            .play()
            .map_err(|err| fail("could not start the audio output", &err))?;
        Ok(Self {
            _stream: stream,
            mixer,
        })
    }

    pub fn mixer(&self) -> AudioMixer {
        self.mixer.clone()
    }
}

fn build_stream<T>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    mut core: MixerCore,
    epoch: Instant,
) -> Result<cpal::Stream, cpal::Error>
where
    T: SizedSample + FromSample<f32>,
{
    let stats = core.stats.clone();
    let errors = core.stats.clone();
    let mut mixed = Vec::new();
    device.build_output_stream::<T, _, _>(
        *config,
        move |out: &mut [T], info: &cpal::OutputCallbackInfo| {
            let timestamp = info.timestamp();
            let latency = timestamp
                .playback
                .saturating_duration_since(timestamp.callback);
            stats
                .output_latency_ms
                .store(latency.as_secs_f64() * 1000.0);
            let heard_at = (epoch.elapsed() + latency).as_secs_f64();
            // Grows once to the device's buffer size, then is reused.
            mixed.resize(out.len(), 0.0);
            core.render(&mut mixed, heard_at);
            for (out, sample) in out.iter_mut().zip(&mixed) {
                *out = T::from_sample(sample.clamp(-1.0, 1.0));
            }
        },
        move |err| {
            *errors.last_error.lock().unwrap() = Some(err.to_string());
        },
        None,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: f64 = 1000.0;

    fn core() -> (MixerCore, AudioMixer) {
        build_mixer(RATE as u32, 2, "test", AudioMode::Live, 0.0)
    }

    /// A voice whose left channel counts 1, 2, 3, ... so output shows its position.
    fn counting_voice(frames: usize, start: f64) -> (Voice, Arc<VoiceControl>) {
        let (mut producer, samples) = rtrb::RingBuffer::new(frames * TRACK_CHANNELS);
        for frame in 1..=frames {
            producer.push(frame as f32).unwrap();
            producer.push(0.0).unwrap();
        }
        let control = Arc::new(VoiceControl {
            gain: AtomicU32::new(1.0f32.to_bits()),
            stopped: AtomicBool::new(false),
            finished: Arc::new(AtomicBool::new(true)),
        });
        let voice = Voice::new(samples, start, control.clone(), 1.0);
        (voice, control)
    }

    fn left(out: &[f32]) -> Vec<f32> {
        out.chunks(2).map(|frame| frame[0]).collect()
    }

    /// Renders buffers of `buffer` frames until playback time `until`,
    /// returning the left channel.
    fn render_until(core: &mut MixerCore, from: f64, until: f64, buffer: usize) -> Vec<f32> {
        let mut all = Vec::new();
        let mut frames_done = (from * RATE).round() as usize;
        while frames_done < (until * RATE).round() as usize {
            let mut out = vec![0.0; 2 * buffer];
            core.render(&mut out, frames_done as f64 / RATE);
            all.extend(left(&out));
            frames_done += buffer;
        }
        all
    }

    #[test]
    fn voice_starts_at_its_time_within_a_buffer() {
        let (mut core, mixer) = core();
        let (voice, _control) = counting_voice(100, 1.003);
        mixer.voices.send(voice).unwrap();
        let mut out = vec![0.0; 2 * 5];
        // Buffer covering 0.995 .. 1.000: nothing yet.
        core.render(&mut out, 0.995);
        assert_eq!(left(&out), [0.0; 5]);
        // Buffer covering 1.000 .. 1.005: starts 3 frames in.
        core.render(&mut out, 1.000);
        assert_eq!(left(&out), [0.0, 0.0, 0.0, 1.0, 2.0]);
        // Continues without gaps.
        core.render(&mut out, 1.005);
        assert_eq!(left(&out), [3.0, 4.0, 5.0, 6.0, 7.0]);
        assert_eq!(core.stats.resyncs.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn late_start_skips_ahead_and_drift_is_corrected() {
        let (mut core, mixer) = core();
        let (voice, control) = counting_voice(200, 0.0);
        mixer.voices.send(voice).unwrap();
        let mut out = vec![0.0; 2 * 4];
        // First heard 10 frames after its start: the first 10 frames are skipped.
        core.render(&mut out, 0.010);
        assert_eq!(left(&out), [11.0, 12.0, 13.0, 14.0]);

        // Small jitter in the clock is not corrected.
        core.render(&mut out, 0.016);
        assert_eq!(left(&out), [15.0, 16.0, 17.0, 18.0]);
        assert!((core.stats.offset_ms.load() - 2.0).abs() < 1e-9);

        // The clock jumps 50 ms ahead: the voice skips to catch up.
        core.render(&mut out, 0.068);
        assert_eq!(left(&out), [69.0, 70.0, 71.0, 72.0]);
        assert_eq!(core.stats.resyncs.load(Ordering::Relaxed), 1);
        assert!((core.stats.max_offset_ms.load() - 50.0).abs() < 1e-9);

        // Dropping the handle removes the voice.
        drop(VoiceHandle { control });
        core.render(&mut out, 0.072);
        assert_eq!(left(&out), [0.0; 4]);
        assert!(core.voices.is_empty());
    }

    #[test]
    fn gain_ramps_across_a_buffer() {
        let (mut core, mixer) = core();
        let (voice, control) = counting_voice(100, 0.0);
        mixer.voices.send(voice).unwrap();
        let handle = VoiceHandle { control };
        handle.set_gain(0.0);
        let mut out = vec![0.0; 2 * 4];
        core.render(&mut out, 0.0);
        // Gain goes 0.75, 0.5, 0.25, 0 across the buffer.
        assert_eq!(left(&out), [0.75, 1.0, 0.75, 0.0]);
        core.render(&mut out, 0.004);
        assert_eq!(left(&out), [0.0; 4]);
    }

    #[test]
    fn a_sound_stops_after_its_last_frame_without_counting_an_underrun() {
        let (mut core, mixer) = core();
        let (voice, _control) = counting_voice(3, 0.0);
        mixer.voices.send(voice).unwrap();
        let mut out = vec![0.0; 2 * 5];
        core.render(&mut out, 0.0);
        assert_eq!(left(&out), [1.0, 2.0, 3.0, 0.0, 0.0]);
        assert_eq!(core.stats.underruns.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn a_positive_offset_plays_the_sound_of_a_later_time() {
        // Starting at 20 ms: what is heard at 0.980 is the sound of 1.000.
        let (mut core, mixer) = build_mixer(RATE as u32, 2, "test", AudioMode::Live, 20.0);
        let (voice, _control) = counting_voice(100, 1.0);
        mixer.voices.send(voice).unwrap();
        let mut out = vec![0.0; 2 * 4];
        core.render(&mut out, 0.970);
        assert_eq!(left(&out), [0.0; 4]);
        core.render(&mut out, 0.978);
        assert_eq!(left(&out), [0.0, 0.0, 1.0, 2.0]);
        assert_eq!(core.stats.resyncs.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn a_change_of_offset_is_reached_gradually_without_a_jump() {
        for (offset_ms, expected_value) in [(10.0, 1011.0), (-10.0, 991.0)] {
            let (mut core, mixer) = core();
            let (voice, _control) = counting_voice(3000, 0.0);
            mixer.voices.send(voice).unwrap();
            let before = render_until(&mut core, 0.0, 0.1, 10);
            mixer.set_sync_offset_ms(offset_ms).unwrap();
            let during = render_until(&mut core, 0.1, 0.6, 10);

            // Before the change the voice reads one frame per frame.
            assert!(before.windows(2).all(|pair| pair[1] - pair[0] == 1.0));
            // While the offset moves it reads 2% faster (or slower), never in a jump.
            let sign = offset_ms.signum() as f32;
            let steps: Vec<f32> = during.windows(2).map(|pair| pair[1] - pair[0]).collect();
            assert!(
                steps
                    .iter()
                    .all(|step| (step - (1.0 + 0.02 * sign)).abs() < 1e-3),
                "{steps:?}"
            );

            // Afterwards it reads whole frames again, `offset` frames further on.
            render_until(&mut core, 0.6, 1.0, 10);
            let after = render_until(&mut core, 1.0, 1.01, 10);
            assert_eq!(after[0], expected_value);
            assert_eq!(after[1], expected_value + 1.0);
            assert_eq!(
                core.stats.resyncs.load(Ordering::Relaxed),
                0,
                "offset {offset_ms}, max {}",
                core.stats.max_offset_ms.load()
            );
            assert!(core.stats.max_offset_ms.load() < 1.0);
        }
    }

    #[test]
    fn a_large_change_of_offset_jumps_and_resynchronizes() {
        let (mut core, mixer) = core();
        let (voice, _control) = counting_voice(3000, 0.0);
        mixer.voices.send(voice).unwrap();
        render_until(&mut core, 0.0, 0.1, 10);
        mixer.set_sync_offset_ms(300.0).unwrap();
        let out = render_until(&mut core, 0.1, 0.11, 10);
        assert_eq!(out[0], 401.0);
        assert_eq!(core.stats.resyncs.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn a_voice_starting_while_the_offset_moves_ends_on_whole_frames() {
        let (mut core, mixer) = core();
        mixer.set_sync_offset_ms(10.0).unwrap();
        // The offset is moving from 0 to 10 ms during the first 0.5 s.
        render_until(&mut core, 0.0, 0.2, 10);
        let (voice, _control) = counting_voice(3000, 0.2);
        mixer.voices.send(voice).unwrap();
        render_until(&mut core, 0.2, 0.9, 10);
        let after = render_until(&mut core, 0.9, 0.91, 10);
        // 0.700 s into the sound, 10 ms ahead, on a whole frame.
        assert_eq!(after[0], 711.0);
        assert_eq!(after[1], 712.0);
    }

    #[test]
    fn the_export_mixer_has_no_sync_offset() {
        let mixer = OfflineMixer::export(1000).mixer();
        assert_eq!(mixer.mode(), AudioMode::Export);
        assert!(mixer.set_sync_offset_ms(100.0).is_err());
        assert_eq!(mixer.sync_offset_ms(), 0.0);
        assert!(OfflineMixer::live(1000, 100.0).is_ok());
    }

    #[test]
    fn offsets_outside_the_range_are_refused() {
        let mixer = OfflineMixer::live(1000, 0.0).unwrap().mixer();
        assert!(mixer.set_sync_offset_ms(1000.5).is_err());
        assert!(mixer.set_sync_offset_ms(-500.5).is_err());
        assert!(mixer.set_sync_offset_ms(f64::NAN).is_err());
        assert!(mixer.set_sync_offset_ms(-500.0).is_ok());
        assert_eq!(mixer.sync_offset_ms(), -500.0);
        assert!(OfflineMixer::live(1000, 2000.0).is_err());
    }

    #[test]
    fn lookahead_is_the_latency_plus_the_offset() {
        let (_core, mixer) = core();
        mixer.stats.output_latency_ms.store(13.0);
        mixer.set_sync_offset_ms(200.0).unwrap();
        assert!((mixer.lookahead_secs() - 0.213).abs() < 1e-12);
        mixer.set_sync_offset_ms(-300.0).unwrap();
        assert!((mixer.lookahead_secs() + 0.287).abs() < 1e-12);
    }

    /// Writes the time of every frame it is asked for.
    struct TimeSource {
        times: Arc<Mutex<Vec<f64>>>,
    }

    impl Source for TimeSource {
        fn render(&mut self, out: &mut [f32], timeline: &Timeline<'_>) {
            let mut times = self.times.lock().unwrap();
            for frame in 0..timeline.frames() {
                times.push(timeline.time(frame));
                out[frame * TRACK_CHANNELS] += 0.5;
                out[frame * TRACK_CHANNELS + 1] += 0.25;
            }
        }
    }

    #[test]
    fn sources_are_mixed_in_and_see_the_offset_in_their_times() {
        let (mut core, mixer) = core();
        let times = Arc::new(Mutex::new(Vec::new()));
        let handle = mixer.add_source(Box::new(TimeSource {
            times: times.clone(),
        }));
        let mut out = vec![0.0; 2 * 3];
        core.render(&mut out, 1.0);
        assert_eq!(out, [0.5, 0.25, 0.5, 0.25, 0.5, 0.25]);
        assert_eq!(*times.lock().unwrap(), [1.0, 1.001, 1.002]);

        // The offset reaches the times gradually: 2% more time per frame.
        times.lock().unwrap().clear();
        mixer.set_sync_offset_ms(5.0).unwrap();
        core.render(&mut out, 1.003);
        let times = times.lock().unwrap().clone();
        let steps: Vec<f64> = times.windows(2).map(|pair| pair[1] - pair[0]).collect();
        assert!(steps.iter().all(|step| (step - 0.00102).abs() < 1e-9));
        assert!((times[0] - 1.003).abs() < 1e-9);

        // Dropping the handle removes the source.
        drop(handle);
        core.render(&mut out, 1.006);
        assert_eq!(out, [0.0; 6]);
    }

    #[test]
    fn output_channels_are_filled_by_the_stereo_mix() {
        let (mut core, mixer) = build_mixer(RATE as u32, 1, "mono", AudioMode::Export, 0.0);
        let _handle = mixer.add_source(Box::new(TimeSource {
            times: Arc::default(),
        }));
        let mut out = vec![0.0; 2];
        core.render(&mut out, 0.0);
        assert_eq!(out, [0.375, 0.375]);

        let (mut core, mixer) = build_mixer(RATE as u32, 4, "surround", AudioMode::Export, 0.0);
        let _handle = mixer.add_source(Box::new(TimeSource {
            times: Arc::default(),
        }));
        let mut out = vec![0.0; 4];
        core.render(&mut out, 0.0);
        assert_eq!(out, [0.5, 0.25, 0.0, 0.0]);
    }

    #[test]
    fn equal_power_fades_keep_the_total_power() {
        for step in 0..=20 {
            let progress = f64::from(step) / 20.0;
            let power = FadeCurve::EqualPower.rise(progress).powi(2)
                + FadeCurve::EqualPower.rise(1.0 - progress).powi(2);
            assert!((power - 1.0).abs() < 1e-6, "{progress}: {power}");
            // Linear fades add up in amplitude instead.
            let amplitude =
                FadeCurve::Linear.rise(progress) + FadeCurve::Linear.rise(1.0 - progress);
            assert!((amplitude - 1.0).abs() < 1e-6);
        }
        assert_eq!(FadeCurve::EqualPower.rise(-1.0), 0.0);
        assert_eq!(FadeCurve::EqualPower.rise(2.0), 1.0);
    }

    #[test]
    fn the_offline_mixer_renders_stereo_in_memory() {
        let mut offline = OfflineMixer::export(1000);
        let mixer = offline.mixer();
        let _handle = mixer.add_source(Box::new(TimeSource {
            times: Arc::default(),
        }));
        let out = offline.render(4, 0.0);
        assert_eq!(out.len(), 8);
        assert_eq!(&out[..2], [0.5, 0.25]);
    }
}
