//! Background music decoded by `ffmpeg` into the mixer's format.
//!
//! Like the sound of a video, a track runs in its own `ffmpeg` process and
//! fills a ring buffer that the mixer drains. Unlike it, a track is not
//! placed on the picture's clock: it plays from wherever the mixer starts it.
//! The buffer holds [`BUFFER_SECS`] of sound, enough for the mixer to see the
//! end of a track that far ahead and fade it out.
//!
//! A file that cannot be decoded shows up as a decoder that ends without
//! having produced any sound ([`DecodeState::failure`]).

use std::collections::VecDeque;
use std::io::Read;
use std::path::Path;
use std::process::{Child, ChildStdout, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::video::{Ffmpeg, collect_log, command};
use crate::EngineError;
use crate::audio::{AudioFormat, TRACK_CHANNELS};

/// Decoded sound kept ready ahead of playback.
pub const BUFFER_SECS: usize = 10;
/// Bytes of one frame: two `f32` samples.
const FRAME_BYTES: usize = TRACK_CHANNELS * 4;

/// How far a track's decoder has got, readable from any thread.
#[derive(Default)]
pub struct DecodeState {
    frames: AtomicU64,
    failure: Mutex<Option<String>>,
}

impl DecodeState {
    /// Frames of sound decoded so far.
    pub fn frames_decoded(&self) -> u64 {
        self.frames.load(Ordering::Relaxed)
    }

    /// Why decoding ended badly: the decoder produced no sound at all, or
    /// `ffmpeg` exited with an error (after some sound, the track is cut
    /// short). Set before the track is marked finished.
    pub fn failure(&self) -> Option<String> {
        self.failure.lock().unwrap().clone()
    }
}

/// A running `ffmpeg` process writing a music file's sound into a ring buffer.
pub struct MusicTrack {
    /// The process, until the decoder thread takes it to wait for its end.
    child: Arc<Mutex<Option<Child>>>,
    samples: Option<rtrb::Consumer<f32>>,
    finished: Arc<AtomicBool>,
    state: Arc<DecodeState>,
}

impl MusicTrack {
    /// Starts decoding the first sound stream of `path` as stereo `f32`
    /// samples at the output's rate. Cover pictures in the file are ignored.
    /// The process ends when the track's samples are dropped.
    pub fn open(ffmpeg: &Ffmpeg, path: &Path, format: AudioFormat) -> Result<Self, EngineError> {
        let rate = format.sample_rate.to_string();
        let mut child = command(ffmpeg.ffmpeg_path())
            .args(["-nostdin", "-hide_banner", "-loglevel", "error", "-i"])
            .arg(path)
            .args(["-map", "0:a:0", "-vn", "-sn", "-dn"])
            .args(["-ac", &TRACK_CHANNELS.to_string(), "-ar", &rate])
            .args(["-f", "f32le", "-"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|err| EngineError::Media(format!("could not run ffmpeg: {err}")))?;
        let pipe = child.stdout.take().expect("piped");
        let stderr = child.stderr.take().expect("piped");
        let child = Arc::new(Mutex::new(Some(child)));

        let capacity = format.sample_rate as usize * TRACK_CHANNELS * BUFFER_SECS;
        let (producer, samples) = rtrb::RingBuffer::new(capacity);
        let finished = Arc::new(AtomicBool::new(false));
        let state = Arc::new(DecodeState::default());
        {
            let (child, finished, state) = (child.clone(), finished.clone(), state.clone());
            let spawned = std::thread::Builder::new()
                .name("fluxlide-ffmpeg-music".into())
                .spawn(move || {
                    let log = Arc::new(Mutex::new(VecDeque::new()));
                    let log_reader = {
                        let log = log.clone();
                        std::thread::spawn(move || collect_log(stderr, &log))
                    };
                    read_track(child, pipe, producer, &finished, &state, &log, log_reader);
                });
            spawned.map_err(|err| EngineError::Media(err.to_string()))?;
        }
        Ok(Self {
            child,
            samples: Some(samples),
            finished,
            state,
        })
    }

    /// Hands the decoded samples over to the mixer. Returns `None` the second
    /// time. The flag is set once the decoder has written its last sample.
    pub fn take_samples(&mut self) -> Option<(rtrb::Consumer<f32>, Arc<AtomicBool>)> {
        Some((self.samples.take()?, self.finished.clone()))
    }

    pub fn state(&self) -> Arc<DecodeState> {
        self.state.clone()
    }

    /// Whether the decoder has written its last sample.
    pub fn is_finished(&self) -> bool {
        self.finished.load(Ordering::Acquire)
    }
}

impl Drop for MusicTrack {
    fn drop(&mut self) {
        // A track that was never handed over is not going to be played: stop
        // its process even if it is stuck. Once the samples have been handed
        // over, the decoder stops when they are dropped.
        if self.samples.is_some()
            && let Some(child) = self.child.lock().unwrap().as_mut()
        {
            let _ = child.kill();
        }
    }
}

fn read_track(
    child: Arc<Mutex<Option<Child>>>,
    mut pipe: ChildStdout,
    mut producer: rtrb::Producer<f32>,
    finished: &AtomicBool,
    state: &DecodeState,
    log: &Mutex<VecDeque<String>>,
    log_reader: std::thread::JoinHandle<()>,
) {
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
                    // Nobody plays this track any more.
                    if let Some(mut child) = child.lock().unwrap().take() {
                        let _ = child.kill();
                        let _ = child.wait();
                    }
                    return;
                }
                sample = rejected;
                std::thread::sleep(Duration::from_millis(5));
            }
        }
        state
            .frames
            .fetch_add((whole / FRAME_BYTES) as u64, Ordering::Relaxed);
        bytes.copy_within(whole..filled, 0);
        carried = filled - whole;
    }

    let status = match child.lock().unwrap().take() {
        Some(mut child) => child.wait(),
        None => return,
    };
    let _ = log_reader.join();
    let detail = || {
        let log = log.lock().unwrap();
        log.iter().cloned().collect::<Vec<_>>().join(" / ")
    };
    let failure = match status {
        Err(err) => Some(format!("could not wait for ffmpeg: {err}")),
        Ok(status) if !status.success() => Some(match detail().as_str() {
            "" => format!("ffmpeg failed ({status})"),
            detail => detail.to_string(),
        }),
        Ok(_) if state.frames_decoded() == 0 => Some("the file has no sound".to_string()),
        Ok(_) => None,
    };
    *state.failure.lock().unwrap() = failure;
    finished.store(true, Ordering::Release);
}

/// Title and artist stored in a music file, for showing what is playing.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Tags {
    pub title: Option<String>,
    pub artist: Option<String>,
}

/// Reads the tags of `path`. Tags are only for display, so a file that has
/// none, or that `ffmpeg` cannot read at all, gives empty tags; a file that
/// cannot be played is reported when it is decoded.
pub fn read_tags(ffmpeg: &Ffmpeg, path: &Path) -> Tags {
    let output = command(ffmpeg.ffmpeg_path())
        .args(["-nostdin", "-hide_banner", "-loglevel", "error", "-i"])
        .arg(path)
        .args(["-f", "ffmetadata", "-"])
        .output();
    match output {
        Ok(output) if output.status.success() => {
            parse_tags(&String::from_utf8_lossy(&output.stdout))
        }
        _ => Tags::default(),
    }
}

/// Parses the file-level part of `ffmpeg`'s `ffmetadata` format: `key=value`
/// lines, ended by the first `[section]`, with `\` escaping `=`, `;`, `#`,
/// `\` and the end of a line.
fn parse_tags(text: &str) -> Tags {
    let mut tags = Tags::default();
    let mut lines = text.lines();
    while let Some(mut line) = lines.next() {
        if line.starts_with('[') {
            break;
        }
        // A value can continue on the next line after a trailing backslash.
        let mut joined = String::new();
        while line.ends_with('\\') && !line.ends_with("\\\\") {
            joined.push_str(&line[..line.len() - 1]);
            joined.push('\n');
            let Some(next) = lines.next() else { break };
            line = next;
        }
        joined.push_str(line);
        let Some((key, value)) = split_unescaped(&joined) else {
            continue;
        };
        let value = unescape(value);
        if value.is_empty() {
            continue;
        }
        match key.to_ascii_lowercase().as_str() {
            "title" => tags.title = Some(value),
            "artist" => tags.artist = Some(value),
            _ => {}
        }
    }
    tags
}

/// Splits at the first `=` that is not escaped.
fn split_unescaped(line: &str) -> Option<(&str, &str)> {
    let mut escaped = false;
    for (index, ch) in line.char_indices() {
        match ch {
            '\\' if !escaped => escaped = true,
            '=' if !escaped => return Some((&line[..index], &line[index + 1..])),
            _ => escaped = false,
        }
    }
    None
}

fn unescape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            out.extend(chars.next());
        } else {
            out.push(ch);
        }
    }
    out
}

#[cfg(test)]
/// Makes a music file with `ffmpeg`, or returns `None` if that is not possible.
pub(crate) fn make_tone(
    ffmpeg: &Ffmpeg,
    path: &Path,
    hz: u32,
    secs: f64,
    extra: &[&str],
) -> Option<()> {
    let status = std::process::Command::new(ffmpeg.ffmpeg_path())
        .args(["-v", "error", "-y", "-f", "lavfi", "-i"])
        .arg(format!("sine=f={hz}:sample_rate=44100:duration={secs}"))
        .args(extra)
        .arg(path)
        .status()
        .ok()?;
    status.success().then_some(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    #[test]
    fn tags_are_parsed_from_ffmetadata() {
        let text = ";FFMETADATA1\ntitle=Hello\\=World\\; x\nArtist=A B\nencoder=Lavf\n\
                    comment=line one\\\nline two\n[STREAM]\ntitle=ignored\n";
        assert_eq!(
            parse_tags(text),
            Tags {
                title: Some("Hello=World; x".into()),
                artist: Some("A B".into())
            }
        );
        assert_eq!(parse_tags(";FFMETADATA1\ntitle=\n"), Tags::default());
    }

    fn drain(track: &mut MusicTrack, frames_wanted: usize) -> Vec<f32> {
        let (mut samples, finished) = track.take_samples().unwrap();
        let mut left = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline && left.len() < frames_wanted {
            while let (Ok(l), Ok(_)) = (samples.pop(), samples.pop()) {
                left.push(l);
            }
            if finished.load(Ordering::Acquire) && samples.is_empty() {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        left
    }

    #[test]
    fn music_is_decoded_to_the_output_rate_and_tags_are_read() {
        let Ok(ffmpeg) = Ffmpeg::locate() else {
            eprintln!("skipping music test: ffmpeg not found");
            return;
        };
        let dir = std::env::temp_dir().join(format!("fluxlide-music-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let tone = dir.join("tone.m4a");
        if make_tone(
            &ffmpeg,
            &tone,
            440,
            1.0,
            &[
                "-metadata",
                "title=Tone 440",
                "-metadata",
                "artist=Test",
                "-c:a",
                "aac",
            ],
        )
        .is_none()
        {
            eprintln!("skipping music test: could not create a file");
            return;
        }

        let mut track =
            MusicTrack::open(&ffmpeg, &tone, AudioFormat { sample_rate: 8000 }).unwrap();
        let state = track.state();
        let left = drain(&mut track, usize::MAX);
        // 1 s at 8 kHz, plus or minus the codec's padding.
        assert!((7800..=8400).contains(&left.len()), "{}", left.len());
        assert_eq!(state.frames_decoded() as usize, left.len());
        assert!(track.is_finished());
        assert_eq!(state.failure(), None);
        let peak = left
            .iter()
            .fold(0.0f32, |peak, sample| peak.max(sample.abs()));
        assert!(peak > 0.05, "{peak}");
        assert!(track.take_samples().is_none());

        assert_eq!(
            read_tags(&ffmpeg, &tone),
            Tags {
                title: Some("Tone 440".into()),
                artist: Some("Test".into())
            }
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_broken_file_ends_without_sound_and_says_why() {
        let Ok(ffmpeg) = Ffmpeg::locate() else {
            eprintln!("skipping music test: ffmpeg not found");
            return;
        };
        let dir = std::env::temp_dir().join(format!("fluxlide-music-bad-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let broken = dir.join("broken.mp3");
        std::fs::write(&broken, b"this is not music").unwrap();
        let missing = dir.join("missing.mp3");

        for path in [&broken, &missing] {
            let mut track =
                MusicTrack::open(&ffmpeg, path, AudioFormat { sample_rate: 8000 }).unwrap();
            let state = track.state();
            assert!(drain(&mut track, usize::MAX).is_empty());
            assert_eq!(state.frames_decoded(), 0);
            assert!(track.is_finished());
            let failure = state.failure().expect("a failure is reported");
            assert!(!failure.is_empty(), "{failure}");
            assert_eq!(read_tags(&ffmpeg, path), Tags::default());
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn dropping_the_samples_ends_the_decoder() {
        let Ok(ffmpeg) = Ffmpeg::locate() else {
            eprintln!("skipping music test: ffmpeg not found");
            return;
        };
        let dir = std::env::temp_dir().join(format!("fluxlide-music-drop-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // Longer than the buffer, so the decoder is waiting for room.
        let long = dir.join("long.wav");
        if make_tone(&ffmpeg, &long, 440, 60.0, &[]).is_none() {
            eprintln!("skipping music test: could not create a file");
            return;
        }
        let mut track =
            MusicTrack::open(&ffmpeg, &long, AudioFormat { sample_rate: 8000 }).unwrap();
        let state = track.state();
        let (samples, finished) = track.take_samples().unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while state.frames_decoded() < 8000 * (BUFFER_SECS as u64 - 1) && Instant::now() < deadline
        {
            std::thread::sleep(Duration::from_millis(10));
        }
        drop(samples);
        // The reader notices, stops ffmpeg and never marks the track finished.
        std::thread::sleep(Duration::from_millis(200));
        assert!(!finished.load(Ordering::Acquire));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
