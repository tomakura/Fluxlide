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
struct AtomicF64(AtomicU64);

impl AtomicF64 {
    fn load(&self) -> f64 {
        f64::from_bits(self.0.load(Ordering::Relaxed))
    }

    fn store(&self, value: f64) {
        self.0.store(value.to_bits(), Ordering::Relaxed);
    }
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
    /// Frames taken from `samples` so far.
    position: u64,
    /// Gain applied at the end of the previous buffer.
    gain: f32,
    /// Whether the first sample has been placed on the clock.
    started: bool,
}

/// Sends sounds to the output. Cheap to clone and usable from any thread.
#[derive(Clone)]
pub struct AudioMixer {
    format: AudioFormat,
    device: Arc<str>,
    voices: Sender<Voice>,
    stats: Arc<SharedStats>,
}

impl AudioMixer {
    pub fn format(&self) -> AudioFormat {
        self.format
    }

    /// Plays interleaved stereo samples from `samples`, the first one heard
    /// at playback time `start`. `finished` tells silence at the end of the
    /// sound apart from a decoder falling behind.
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
        let _ = self.voices.send(Voice {
            samples,
            start,
            control: control.clone(),
            position: 0,
            gain,
            started: false,
        });
        VoiceHandle { control }
    }

    pub fn stats(&self) -> AudioStats {
        AudioStats {
            device: self.device.to_string(),
            sample_rate: self.format.sample_rate,
            output_latency_ms: self.stats.output_latency_ms.load(),
            offset_ms: self.stats.offset_ms.load(),
            max_offset_ms: self.stats.max_offset_ms.load(),
            resyncs: self.stats.resyncs.load(Ordering::Relaxed),
            underruns: self.stats.underruns.load(Ordering::Relaxed),
            last_error: self.stats.last_error.lock().unwrap().clone(),
        }
    }
}

/// Mixes the active voices into output buffers. Runs on the audio thread.
struct MixerCore {
    sample_rate: f64,
    out_channels: usize,
    voices: Vec<Voice>,
    incoming: Receiver<Voice>,
    stats: Arc<SharedStats>,
}

impl MixerCore {
    /// Fills `out` (interleaved, `out_channels` per frame) with the sound due
    /// from playback time `heard_at` on.
    fn render(&mut self, out: &mut [f32], heard_at: f64) {
        out.fill(0.0);
        self.voices.extend(self.incoming.try_iter());
        self.voices
            .retain(|voice| !voice.control.stopped.load(Ordering::Relaxed));

        let frames = out.len() / self.out_channels;
        let resync_frames = RESYNC_SECS * self.sample_rate;
        for voice in &mut self.voices {
            let finished = voice.control.finished.load(Ordering::Relaxed);
            if finished && voice.samples.is_empty() {
                // The sound has ended; its picture may still be on screen.
                continue;
            }
            // Frames the voice should have played by the start of this buffer.
            let due = (heard_at - voice.start) * self.sample_rate;
            let behind = due - voice.position as f64;
            let mut first_frame = 0;
            if !voice.started {
                if behind < -(frames as f64) {
                    continue; // Starts after this buffer.
                }
                voice.started = true;
                align(voice, behind, frames, &mut first_frame);
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
                if voice.samples.slots() < TRACK_CHANNELS {
                    if !finished {
                        self.stats.underruns.fetch_add(1, Ordering::Relaxed);
                    }
                    break;
                }
                let left = voice.samples.pop().unwrap_or(0.0);
                let right = voice.samples.pop().unwrap_or(0.0);
                voice.position += 1;
                let gain = voice.gain + step * (frame + 1) as f32;
                let out = &mut out[frame * self.out_channels..][..self.out_channels];
                if let [only] = out {
                    *only += (left + right) * 0.5 * gain;
                } else {
                    out[0] += left * gain;
                    out[1] += right * gain;
                }
            }
            voice.gain = target;
        }
    }
}

/// Moves a voice to where the clock says it should be: drops samples it is
/// behind by, or delays its first sample within this buffer if it is ahead.
fn align(voice: &mut Voice, behind: f64, frames: usize, first_frame: &mut usize) {
    if behind > 0.0 {
        let skip = behind.round() as usize;
        let available = voice.samples.slots() / TRACK_CHANNELS;
        let skip = skip.min(available);
        for _ in 0..skip * TRACK_CHANNELS {
            let _ = voice.samples.pop();
        }
        voice.position += skip as u64;
    } else {
        *first_frame = ((-behind).round() as usize).min(frames);
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
        let (voices, incoming) = mpsc::channel();
        let stats = Arc::new(SharedStats::default());
        let core = MixerCore {
            sample_rate: f64::from(config.sample_rate),
            out_channels: usize::from(config.channels.max(1)),
            voices: Vec::new(),
            incoming,
            stats: stats.clone(),
        };
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
            mixer: AudioMixer {
                format: AudioFormat {
                    sample_rate: config.sample_rate,
                },
                device: name.into(),
                voices,
                stats,
            },
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

    fn core() -> (MixerCore, Sender<Voice>) {
        let (voices, incoming) = mpsc::channel();
        let core = MixerCore {
            sample_rate: RATE,
            out_channels: 2,
            voices: Vec::new(),
            incoming,
            stats: Arc::new(SharedStats::default()),
        };
        (core, voices)
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
        let voice = Voice {
            samples,
            start,
            control: control.clone(),
            position: 0,
            gain: 1.0,
            started: false,
        };
        (voice, control)
    }

    fn left(out: &[f32]) -> Vec<f32> {
        out.chunks(2).map(|frame| frame[0]).collect()
    }

    #[test]
    fn voice_starts_at_its_time_within_a_buffer() {
        let (mut core, voices) = core();
        let (voice, _control) = counting_voice(100, 1.003);
        voices.send(voice).unwrap();
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
        let (mut core, voices) = core();
        let (voice, control) = counting_voice(200, 0.0);
        voices.send(voice).unwrap();
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
        let (mut core, voices) = core();
        let (voice, control) = counting_voice(100, 0.0);
        voices.send(voice).unwrap();
        let handle = VoiceHandle { control };
        handle.set_gain(0.0);
        let mut out = vec![0.0; 2 * 4];
        core.render(&mut out, 0.0);
        // Gain goes 0.75, 0.5, 0.25, 0 across the buffer.
        assert_eq!(left(&out), [0.75, 1.0, 0.75, 0.0]);
        core.render(&mut out, 0.004);
        assert_eq!(left(&out), [0.0; 4]);
    }
}
