//! A beep at regular intervals for adjusting the sync offset by ear.
//!
//! The settings screen flashes the picture and plays a beep at the same
//! playback times; the user raises the offset until the two are
//! simultaneous. This module is the engine side of that: a [`BeepSchedule`]
//! says when beeps are due, in playback time. The picture side asks
//! [`BeepSchedule::flash_at`] with the time of the frame it is drawing, and
//! the sound side ([`start_beeps`]) renders the beeps from the same schedule
//! through the mixer, so the sync offset applies to them like to the sound
//! of a video: a beep due at time `t` is heard when the picture of time `t`
//! is shown.

use std::f64::consts::TAU;

use crate::EngineError;
use crate::audio::{AudioMixer, Source, SourceHandle, TRACK_CHANNELS, Timeline};

/// Pitch of the beep.
const BEEP_HZ: f64 = 1000.0;
/// Length of the rise and fall at the ends of a beep, which avoids clicks.
const EDGE_SECS: f64 = 0.005;
/// Length of a beep, and of the flash that goes with it.
pub const BEEP_SECS: f64 = 0.05;
/// Shortest allowed time between beeps: a beep must end before the next.
pub const MIN_INTERVAL_SECS: f64 = 0.2;

/// When beeps are due: one every `interval_secs`, the first at `start`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BeepSchedule {
    start: f64,
    interval_secs: f64,
}

impl BeepSchedule {
    pub fn new(start: f64, interval_secs: f64) -> Result<Self, EngineError> {
        if !(start.is_finite() && interval_secs.is_finite() && interval_secs >= MIN_INTERVAL_SECS) {
            return Err(EngineError::Audio(format!(
                "a beep schedule needs a finite start and an interval of at least \
                 {MIN_INTERVAL_SECS} s, not {start} and {interval_secs}"
            )));
        }
        Ok(Self {
            start,
            interval_secs,
        })
    }

    pub fn interval_secs(&self) -> f64 {
        self.interval_secs
    }

    /// Playback time at which beep number `index` (from 0) starts.
    pub fn beep_start(&self, index: u64) -> f64 {
        self.start + index as f64 * self.interval_secs
    }

    /// The beep playing at `time`: its index and start time.
    pub fn beep_at(&self, time: f64) -> Option<(u64, f64)> {
        let since = time - self.start;
        if since < 0.0 {
            return None;
        }
        let index = (since / self.interval_secs).floor() as u64;
        let start = self.beep_start(index);
        (time - start < BEEP_SECS).then_some((index, start))
    }

    /// Whether the picture should be lit at `time`.
    pub fn flash_at(&self, time: f64) -> bool {
        self.beep_at(time).is_some()
    }

    /// Start of the first beep after `time`.
    pub fn next_beep_after(&self, time: f64) -> f64 {
        let since = time - self.start;
        if since < 0.0 {
            return self.start;
        }
        self.beep_start((since / self.interval_secs).floor() as u64 + 1)
    }
}

struct Beeper {
    schedule: BeepSchedule,
    volume: f32,
}

impl Source for Beeper {
    fn render(&mut self, out: &mut [f32], timeline: &Timeline<'_>) {
        for frame in 0..timeline.frames() {
            let Some((_, start)) = self.schedule.beep_at(timeline.time(frame)) else {
                continue;
            };
            let local = timeline.time(frame) - start;
            let edge = (local / EDGE_SECS)
                .min((BEEP_SECS - local) / EDGE_SECS)
                .min(1.0);
            let sample = (local * BEEP_HZ * TAU).sin() * edge * f64::from(self.volume);
            let frame_out = &mut out[frame * TRACK_CHANNELS..][..TRACK_CHANNELS];
            frame_out[0] += sample as f32;
            frame_out[1] += sample as f32;
        }
    }
}

/// Plays the beeps of `schedule` at `volume` (0 to 1). They stop when the
/// returned handle is dropped.
pub fn start_beeps(mixer: &AudioMixer, schedule: BeepSchedule, volume: f32) -> SourceHandle {
    mixer.add_source(Box::new(Beeper {
        schedule,
        volume: volume.clamp(0.0, 1.0),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::OfflineMixer;

    const RATE: u32 = 8000;

    #[test]
    fn the_schedule_tells_when_to_flash() {
        let schedule = BeepSchedule::new(1.0, 0.5).unwrap();
        assert!(!schedule.flash_at(0.999));
        assert!(schedule.flash_at(1.0));
        assert!(schedule.flash_at(1.049));
        assert!(!schedule.flash_at(1.05));
        assert_eq!(schedule.beep_at(3.01), Some((4, 3.0)));
        assert_eq!(schedule.beep_start(2), 2.0);
        assert_eq!(schedule.next_beep_after(0.0), 1.0);
        assert_eq!(schedule.next_beep_after(1.0), 1.5);
        assert_eq!(schedule.next_beep_after(1.2), 1.5);
    }

    #[test]
    fn bad_schedules_are_refused() {
        assert!(BeepSchedule::new(0.0, 0.1).is_err());
        assert!(BeepSchedule::new(f64::NAN, 1.0).is_err());
        assert!(BeepSchedule::new(0.0, f64::INFINITY).is_err());
    }

    /// Times at which the output rises from silence, in seconds.
    fn onsets(out: &[f32]) -> Vec<f64> {
        let mut onsets = Vec::new();
        let mut silent_for = usize::MAX;
        for (frame, stereo) in out.chunks(2).enumerate() {
            if stereo[0].abs() > 0.05 {
                // A beep is a run of loud samples with gaps of at most a half period.
                if silent_for > RATE as usize / 100 {
                    onsets.push(frame as f64 / f64::from(RATE));
                }
                silent_for = 0;
            } else {
                silent_for = silent_for.saturating_add(1);
            }
        }
        onsets
    }

    #[test]
    fn beeps_are_heard_at_their_times_and_earlier_with_an_offset() {
        let schedule = BeepSchedule::new(1.0, 0.5).unwrap();
        for (offset_ms, shift) in [(0.0, 0.0), (100.0, -0.1), (-50.0, 0.05)] {
            let mut offline = OfflineMixer::live(RATE, offset_ms).unwrap();
            let _beeps = start_beeps(&offline.mixer(), schedule, 0.5);
            let out = offline.render(RATE as usize * 3, 0.0);
            let onsets = onsets(&out);
            // Beeps are due at 1.0, 1.5, 2.0, ... and an offset moves them in the 3 seconds.
            assert!((4..=5).contains(&onsets.len()), "{onsets:?}");
            for (index, onset) in onsets.iter().enumerate() {
                let expected = schedule.beep_start(index as u64) + shift;
                assert!(
                    (onset - expected).abs() < 0.002,
                    "offset {offset_ms}: {onset} instead of {expected}"
                );
            }
        }
    }

    #[test]
    fn a_beep_is_a_short_tone_with_soft_edges() {
        let schedule = BeepSchedule::new(0.5, 1.0).unwrap();
        let mut offline = OfflineMixer::export(RATE);
        let _beeps = start_beeps(&offline.mixer(), schedule, 0.5);
        let out = offline.render(RATE as usize, 0.0);
        let left: Vec<f32> = out.chunks(2).map(|frame| frame[0]).collect();
        let beep =
            &left[(0.5 * f64::from(RATE)) as usize..][..(BEEP_SECS * f64::from(RATE)) as usize];
        // 1 kHz for 50 ms: about 50 periods, so about 100 sign changes.
        let crossings = beep
            .windows(2)
            .filter(|pair| (pair[0] < 0.0) != (pair[1] < 0.0))
            .count();
        assert!((98..=101).contains(&crossings), "{crossings}");
        let peak = beep
            .iter()
            .fold(0.0f32, |peak, sample| peak.max(sample.abs()));
        assert!((0.45..=0.5).contains(&peak), "{peak}");
        // The first and last samples are near zero.
        assert!(beep[0].abs() < 0.01 && beep[beep.len() - 1].abs() < 0.1);
        // Both channels carry it, and nothing outside the beep.
        assert!(out.chunks(2).all(|frame| frame[0] == frame[1]));
        assert!(
            left[..(0.5 * f64::from(RATE)) as usize]
                .iter()
                .all(|s| *s == 0.0)
        );
    }
}
