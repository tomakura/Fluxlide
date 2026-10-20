//! The sound of a video, decoded by `ffmpeg` into the mixer's format.
//!
//! The sound runs in its own `ffmpeg` process next to the one for the
//! picture. Both start at the start of the file: the picture repeats its
//! first frame and the sound is padded with silence when one of them begins
//! later, so they share one timeline and need no offset between them.

use std::io::Read;
use std::path::Path;
use std::process::{Child, ChildStdout, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::video::{Ffmpeg, collect_log, command};
use crate::EngineError;
use crate::audio::{AudioFormat, TRACK_CHANNELS};

/// Decoded sound kept ready ahead of playback.
const BUFFER_SECS: usize = 2;

/// A running `ffmpeg` process writing a video's sound into a ring buffer.
pub struct AudioTrack {
    child: Child,
    samples: Option<rtrb::Consumer<f32>>,
    finished: Arc<AtomicBool>,
}

impl AudioTrack {
    /// Starts decoding the first sound stream of `path` as stereo `f32`
    /// samples at the output's rate.
    pub fn open(ffmpeg: &Ffmpeg, path: &Path, format: AudioFormat) -> Result<Self, EngineError> {
        let rate = format.sample_rate.to_string();
        let mut child = command(ffmpeg.ffmpeg_path())
            .args(["-nostdin", "-hide_banner", "-loglevel", "error", "-i"])
            .arg(path)
            .args(["-map", "0:a:0", "-vn", "-sn", "-dn"])
            // Pads with silence so the first sample is at the start of the file.
            .args(["-af", "aresample=async=1:first_pts=0"])
            .args(["-ac", &TRACK_CHANNELS.to_string(), "-ar", &rate])
            .args(["-f", "f32le", "-"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|err| EngineError::Media(format!("could not run ffmpeg: {err}")))?;

        if let Some(pipe) = child.stderr.take() {
            // Only drained so ffmpeg never blocks on a full pipe.
            let log = Mutex::default();
            std::thread::Builder::new()
                .name("fluxlide-ffmpeg-audio-log".into())
                .spawn(move || collect_log(pipe, &log))
                .map_err(|err| EngineError::Media(err.to_string()))?;
        }
        let pipe = child.stdout.take().expect("piped");
        let capacity = format.sample_rate as usize * TRACK_CHANNELS * BUFFER_SECS;
        let (producer, samples) = rtrb::RingBuffer::new(capacity);
        let finished = Arc::new(AtomicBool::new(false));
        {
            let finished = finished.clone();
            std::thread::Builder::new()
                .name("fluxlide-ffmpeg-audio-read".into())
                .spawn(move || read_samples(pipe, producer, &finished))
                .map_err(|err| EngineError::Media(err.to_string()))?;
        }
        Ok(Self {
            child,
            samples: Some(samples),
            finished,
        })
    }

    /// Hands the decoded samples to the mixer. Returns `None` the second time.
    pub fn take_samples(&mut self) -> Option<(rtrb::Consumer<f32>, Arc<AtomicBool>)> {
        Some((self.samples.take()?, self.finished.clone()))
    }
}

impl Drop for AudioTrack {
    fn drop(&mut self) {
        // The reader thread exits once the pipe closes or the mixer drops the samples.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn read_samples(mut pipe: ChildStdout, mut producer: rtrb::Producer<f32>, finished: &AtomicBool) {
    let mut bytes = vec![0u8; 16 * 1024];
    // Bytes of a sample split across two reads.
    let mut carried = 0;
    loop {
        let filled = match pipe.read(&mut bytes[carried..]) {
            Ok(0) | Err(_) => break,
            Ok(read) => carried + read,
        };
        let whole = filled / 4 * 4;
        for chunk in bytes[..whole].as_chunks::<4>().0 {
            let mut sample = f32::from_le_bytes(*chunk);
            // Waits while the buffer is full, i.e. while playback has not caught up.
            while let Err(rtrb::PushError::Full(rejected)) = producer.push(sample) {
                if producer.is_abandoned() {
                    return;
                }
                sample = rejected;
                std::thread::sleep(Duration::from_millis(5));
            }
        }
        bytes.copy_within(whole..filled, 0);
        carried = filled - whole;
    }
    finished.store(true, Ordering::Relaxed);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    #[test]
    fn sound_is_aligned_to_the_start_of_the_file() {
        let Ok(ffmpeg) = Ffmpeg::locate() else {
            eprintln!("skipping audio test: ffmpeg not found");
            return;
        };
        let dir = std::env::temp_dir().join(format!("fluxlide-audio-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let clip = dir.join("late-sound.mp4");
        // Picture from 0 s, a tone from 0.5 s, 2 s long in total.
        let status = std::process::Command::new(ffmpeg.ffmpeg_path())
            .args([
                "-v",
                "error",
                "-y",
                "-f",
                "lavfi",
                "-i",
                "testsrc2=size=64x64:rate=30",
            ])
            .args([
                "-itsoffset",
                "0.5",
                "-f",
                "lavfi",
                "-i",
                "sine=f=440:sample_rate=48000",
            ])
            .args([
                "-map", "0:v", "-map", "1:a", "-t", "2", "-pix_fmt", "yuv420p",
            ])
            .args(["-c:a", "aac"])
            .arg(&clip)
            .status()
            .unwrap();
        if !status.success() {
            eprintln!("skipping audio test: could not create a clip");
            return;
        }

        let rate = 8000;
        let mut track =
            AudioTrack::open(&ffmpeg, &clip, AudioFormat { sample_rate: rate }).unwrap();
        let (mut samples, finished) = track.take_samples().unwrap();
        assert!(track.take_samples().is_none());
        let mut left = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            while let (Ok(l), Ok(_)) = (samples.pop(), samples.pop()) {
                left.push(l);
            }
            if finished.load(Ordering::Relaxed) && samples.is_empty() {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        std::fs::remove_dir_all(&dir).unwrap();

        let secs = |frames: usize| frames as f64 / f64::from(rate);
        let first_sound = left.iter().position(|s| s.abs() > 0.01).unwrap();
        // The leading 0.5 s is padded with silence (AAC priming shifts it slightly).
        assert!(
            (secs(first_sound) - 0.5).abs() < 0.05,
            "{}",
            secs(first_sound)
        );
        assert!(
            (secs(left.len()) - 2.0).abs() < 0.05,
            "{}",
            secs(left.len())
        );
    }
}
