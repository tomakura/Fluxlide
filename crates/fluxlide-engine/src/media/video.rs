//! Video decoding through the FFmpeg command-line tools.
//!
//! `ffmpeg` runs as a separate process and writes raw YUV 4:2:0 frames to a
//! pipe in the YUV4MPEG2 format, whose header carries the frame size and
//! rate. A reader thread keeps a few frames buffered ahead of playback and
//! the render thread uploads the frame due at the current time. `ffprobe`
//! provides the duration so a crossfade can start before the video ends.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdout, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::MediaTexture;
use crate::{EngineError, Renderer};

/// Decoded frames kept ready ahead of playback.
const BUFFERED_FRAMES: usize = 8;
/// Videos above this rate are decoded at this rate.
const MAX_FRAME_RATE: u32 = 60;
/// How long opening a video may take before it counts as failed.
const OPEN_TIMEOUT: Duration = Duration::from_secs(15);

/// Paths of the `ffmpeg` and `ffprobe` executables.
#[derive(Debug, Clone)]
pub struct Ffmpeg {
    ffmpeg: PathBuf,
    ffprobe: PathBuf,
}

impl Ffmpeg {
    /// Looks for both tools in `FLUXLIDE_FFMPEG_DIR`, next to the running
    /// executable (where the bundled copy is, named `fluxlide-ffmpeg` and
    /// `fluxlide-ffprobe`), then on `PATH`.
    pub fn locate() -> Result<Self, EngineError> {
        let dirs = SearchDirs::current();
        Ok(Self {
            ffmpeg: dirs.find("ffmpeg")?,
            ffprobe: dirs.find("ffprobe")?,
        })
    }

    pub fn ffmpeg_path(&self) -> &Path {
        &self.ffmpeg
    }

    /// First line of `ffmpeg -version`, e.g. "ffmpeg version 7.1 ...".
    pub fn version(&self) -> Result<String, EngineError> {
        let output = command(&self.ffmpeg)
            .arg("-version")
            .output()
            .map_err(|err| EngineError::Media(format!("could not run ffmpeg: {err}")))?;
        if !output.status.success() {
            return Err(EngineError::Media(format!(
                "ffmpeg -version failed ({}): {}",
                output.status,
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }
        let text = String::from_utf8_lossy(&output.stdout);
        Ok(text.lines().next().unwrap_or_default().to_string())
    }

    /// Reads the duration and frame rate of the first video stream and
    /// whether the file has sound.
    pub fn probe(&self, path: &Path) -> Result<VideoInfo, EngineError> {
        let output = command(&self.ffprobe)
            .args(["-v", "error", "-show_entries"])
            .arg(
                "stream=codec_type,avg_frame_rate,r_frame_rate,start_time,duration\
                 :format=start_time,duration",
            )
            .args(["-of", "flat"])
            .arg(path)
            .output()
            .map_err(|err| EngineError::Media(format!("could not run ffprobe: {err}")))?;
        if !output.status.success() {
            return Err(EngineError::Media(format!(
                "ffprobe could not read {}: {}",
                path.display(),
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }
        parse_probe(&String::from_utf8_lossy(&output.stdout))
            .ok_or_else(|| EngineError::Media(format!("{} has no video stream", path.display())))
    }
}

/// Where `ffmpeg` and `ffprobe` may be.
struct SearchDirs {
    /// Folder named by `FLUXLIDE_FFMPEG_DIR`.
    env_dir: Option<PathBuf>,
    /// Folder of the running executable, which holds the bundled copy.
    exe_dir: Option<PathBuf>,
    /// Folders searched for the plain names.
    path_dirs: Vec<PathBuf>,
}

impl SearchDirs {
    fn current() -> Self {
        let mut path_dirs: Vec<PathBuf> = std::env::var_os("PATH")
            .map(|path| std::env::split_paths(&path).collect())
            .unwrap_or_default();
        // Apps opened from the Finder do not inherit the shell's PATH.
        if cfg!(target_os = "macos") {
            path_dirs.extend(["/opt/homebrew/bin", "/usr/local/bin"].map(PathBuf::from));
        }
        Self {
            env_dir: std::env::var_os("FLUXLIDE_FFMPEG_DIR").map(PathBuf::from),
            exe_dir: std::env::current_exe()
                .ok()
                .and_then(|exe| exe.parent().map(Path::to_path_buf)),
            path_dirs,
        }
    }

    /// Candidate files for `tool`, in order of preference. The bundled copy
    /// has a prefix because the Linux package installs it into `/usr/bin`,
    /// where a plain `ffmpeg` would collide with the distribution's package.
    fn candidates(&self, tool: &str) -> Vec<PathBuf> {
        let plain = format!("{tool}{}", std::env::consts::EXE_SUFFIX);
        let bundled = format!("fluxlide-{plain}");
        let mut files = Vec::new();
        files.extend(self.env_dir.iter().map(|dir| dir.join(&plain)));
        files.extend(self.exe_dir.iter().map(|dir| dir.join(&bundled)));
        files.extend(self.path_dirs.iter().map(|dir| dir.join(&plain)));
        files
    }

    fn find(&self, tool: &str) -> Result<PathBuf, EngineError> {
        self.candidates(tool)
            .into_iter()
            .find(|path| path.is_file())
            .ok_or_else(|| EngineError::Media(format!("{tool} was not found")))
    }
}

pub(crate) fn command(program: &Path) -> Command {
    #[allow(unused_mut)]
    let mut command = Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    command.stdin(Stdio::null());
    command
}

/// The part of a file to play.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct PlayRange {
    /// Seconds into the file where playback starts.
    pub start: f64,
    /// How many seconds to play; `None` plays to the end of the file.
    pub length: Option<f64>,
}

impl PlayRange {
    /// `ffmpeg` input options that read only this range. They go before `-i`.
    pub(crate) fn input_args(&self) -> Vec<String> {
        let mut args = Vec::new();
        if self.start > 0.0 {
            args.extend(["-ss".to_string(), format!("{:.3}", self.start)]);
        }
        if let Some(length) = self.length {
            args.extend(["-t".to_string(), format!("{length:.3}")]);
        }
        args
    }

    /// How long the range plays in a file `duration` seconds long.
    fn length_in(&self, duration: f64) -> f64 {
        let rest = (duration - self.start).max(0.0);
        self.length.map_or(rest, |length| length.min(rest))
    }
}

/// What `ffprobe` reports about a video.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VideoInfo {
    /// Seconds from the start of the file to the end of the picture, if known.
    /// Decoding starts at the start of the file, so this is how long it plays.
    pub duration: Option<f64>,
    /// Rate the video is decoded at, as a fraction, capped at 60 fps.
    pub frame_rate: (u32, u32),
    pub has_audio: bool,
}

fn parse_probe(text: &str) -> Option<VideoInfo> {
    // Keys look like `streams.stream.1.codec_type` and `format.duration`.
    let mut streams: BTreeMap<usize, HashMap<&str, &str>> = BTreeMap::new();
    let mut format = HashMap::new();
    for line in text.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = value.trim().trim_matches('"');
        if let Some(field) = key.strip_prefix("format.") {
            format.insert(field, value);
        } else if let Some((index, field)) = key
            .strip_prefix("streams.stream.")
            .and_then(|rest| rest.split_once('.'))
            && let Ok(index) = index.parse()
        {
            streams.entry(index).or_default().insert(field, value);
        }
    }
    let kind = |stream: &&HashMap<&str, &str>, kind| stream.get("codec_type") == Some(&kind);
    let video = streams.values().find(|stream| kind(stream, "video"))?;
    let has_audio = streams.values().any(|stream| kind(&stream, "audio"));

    let (num, den) = parse_ratio(field(video, "avg_frame_rate"))
        .or_else(|| parse_ratio(field(video, "r_frame_rate")))
        .unwrap_or((30, 1));
    let frame_rate = if u64::from(num) > u64::from(MAX_FRAME_RATE) * u64::from(den) {
        (MAX_FRAME_RATE, 1)
    } else {
        (num, den)
    };
    let lead_in = match (
        parse_time(field(video, "start_time")),
        parse_time(field(&format, "start_time")),
    ) {
        (Some(video_start), Some(file_start)) => (video_start - file_start).max(0.0),
        _ => 0.0,
    };
    let duration = parse_secs(field(video, "duration"))
        .map(|secs| secs + lead_in)
        .or_else(|| parse_secs(field(&format, "duration")));
    Some(VideoInfo {
        duration,
        frame_rate,
        has_audio,
    })
}

fn field<'a>(fields: &HashMap<&str, &'a str>, name: &str) -> &'a str {
    fields.get(name).copied().unwrap_or("")
}

/// Parses "30000/1001"; rejects zero and unknown ("0/0") rates.
fn parse_ratio(value: &str) -> Option<(u32, u32)> {
    let (num, den) = value.split_once(['/', ':'])?;
    let num: u32 = num.parse().ok()?;
    let den: u32 = den.parse().ok()?;
    (num > 0 && den > 0).then_some((num, den))
}

/// Parses a timestamp in seconds, which may be zero or negative.
fn parse_time(value: &str) -> Option<f64> {
    value.parse::<f64>().ok().filter(|secs| secs.is_finite())
}

/// Parses a length in seconds; rejects zero and unknown ("N/A") values.
fn parse_secs(value: &str) -> Option<f64> {
    value
        .parse::<f64>()
        .ok()
        .filter(|secs| secs.is_finite() && *secs > 0.0)
}

/// Frame size and rate from a YUV4MPEG2 stream header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct StreamHeader {
    width: u32,
    height: u32,
    rate: (u32, u32),
}

impl StreamHeader {
    fn frame_len(&self) -> usize {
        let (cw, ch) = super::chroma_size(self.width, self.height);
        (self.width * self.height + 2 * cw * ch) as usize
    }

    fn fps(&self) -> f64 {
        f64::from(self.rate.0) / f64::from(self.rate.1)
    }
}

fn parse_header(line: &str) -> Result<StreamHeader, String> {
    let mut tokens = line.split_ascii_whitespace();
    if tokens.next() != Some("YUV4MPEG2") {
        return Err("not a YUV4MPEG2 stream".into());
    }
    let (mut width, mut height, mut rate) = (None, None, None);
    for token in tokens {
        let (tag, value) = token.split_at(1);
        match tag {
            "W" => width = value.parse().ok(),
            "H" => height = value.parse().ok(),
            "F" => rate = parse_ratio(value),
            // Only 8-bit 4:2:0 is uploaded; the variants differ in chroma siting.
            "C" if !matches!(value, "420" | "420jpeg" | "420mpeg2" | "420paldv") => {
                return Err(format!("unsupported pixel format C{value}"));
            }
            _ => {}
        }
    }
    match (width, height, rate) {
        (Some(width), Some(height), Some(rate)) if width > 0 && height > 0 => Ok(StreamHeader {
            width,
            height,
            rate,
        }),
        _ => Err(format!("incomplete stream header: {line}")),
    }
}

/// A running `ffmpeg` process and the frames it has decoded so far.
struct VideoDecoder {
    child: Child,
    header: StreamHeader,
    frames: Receiver<Vec<u8>>,
    recycle: SyncSender<Vec<u8>>,
    stderr: Arc<Mutex<VecDeque<String>>>,
}

impl VideoDecoder {
    fn spawn(
        ffmpeg: &Ffmpeg,
        path: &Path,
        info: &VideoInfo,
        max_size: (u32, u32),
        range: PlayRange,
    ) -> Result<Self, EngineError> {
        let (max_w, max_h) = max_size;
        // Shrink to the output size (never enlarge), keep even dimensions for
        // 4:2:0, and convert to BT.709 limited range, which the shader expects.
        let filter = format!(
            "scale=w='min(iw,{max_w})':h='min(ih,{max_h})':force_original_aspect_ratio=decrease\
             :force_divisible_by=2:out_color_matrix=bt709:out_range=tv,format=yuv420p"
        );
        let (num, den) = info.frame_rate;
        let mut child = command(&ffmpeg.ffmpeg)
            .args(["-nostdin", "-hide_banner", "-loglevel", "error"])
            // Software decoding. With `-hwaccel auto` (VideoToolbox on macOS)
            // frames are copied back from the GPU for the pipe, and 4K playback
            // fell behind more often than without it.
            .args(range.input_args())
            .arg("-i")
            .arg(path)
            .args(["-map", "0:v:0", "-an", "-sn", "-dn", "-vf", &filter])
            .args(["-r", &format!("{num}/{den}"), "-fps_mode", "cfr"])
            .args(["-f", "yuv4mpegpipe", "-"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|err| EngineError::Media(format!("could not run ffmpeg: {err}")))?;

        let stderr = Arc::new(Mutex::new(VecDeque::new()));
        if let Some(pipe) = child.stderr.take() {
            let stderr = stderr.clone();
            std::thread::Builder::new()
                .name("fluxlide-ffmpeg-log".into())
                .spawn(move || collect_log(pipe, &stderr))
                .map_err(|err| EngineError::Media(err.to_string()))?;
        }
        let reader = BufReader::with_capacity(1 << 16, child.stdout.take().expect("piped"));

        // ffmpeg writes the header only once the first frame has been decoded,
        // so it is read on the reader thread and waited for with a timeout.
        let (header_tx, header_rx) = mpsc::channel();
        let (tx, frames) = mpsc::sync_channel(BUFFERED_FRAMES);
        let (recycle, reuse) = mpsc::sync_channel(BUFFERED_FRAMES + 2);
        let spawned = std::thread::Builder::new()
            .name("fluxlide-ffmpeg-read".into())
            .spawn(move || read_stream(reader, &header_tx, &tx, &reuse));
        let header = match spawned {
            Err(err) => Err(err.to_string()),
            Ok(_) => match header_rx.recv_timeout(OPEN_TIMEOUT) {
                Ok(result) => result,
                Err(RecvTimeoutError::Timeout) => Err("timed out".into()),
                Err(RecvTimeoutError::Disconnected) => Err("no output".into()),
            },
        };
        let header = match header {
            Ok(header) => header,
            Err(reason) => {
                // Killing first also unblocks a reader still waiting on the pipe.
                let _ = child.kill();
                let _ = child.wait();
                return Err(decode_error(path, &reason, &stderr));
            }
        };

        Ok(Self {
            child,
            header,
            frames,
            recycle,
            stderr,
        })
    }
}

impl Drop for VideoDecoder {
    fn drop(&mut self) {
        // The reader thread exits once the pipe closes or the channel drops.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn read_stream(
    mut reader: BufReader<ChildStdout>,
    header_tx: &mpsc::Sender<Result<StreamHeader, String>>,
    frames: &SyncSender<Vec<u8>>,
    reuse: &Receiver<Vec<u8>>,
) {
    let mut line = String::new();
    let header = match reader.read_line(&mut line) {
        Ok(n) if n > 0 => parse_header(line.trim_end()),
        _ => Err("no output".into()),
    };
    let frame_len = header.as_ref().ok().map(StreamHeader::frame_len);
    if header_tx.send(header).is_err() {
        return;
    }
    if let Some(frame_len) = frame_len {
        read_frames(reader, frame_len, frames, reuse);
    }
}

fn read_frames(
    mut reader: BufReader<ChildStdout>,
    frame_len: usize,
    frames: &SyncSender<Vec<u8>>,
    reuse: &Receiver<Vec<u8>>,
) {
    let mut line = Vec::new();
    loop {
        line.clear();
        match reader.read_until(b'\n', &mut line) {
            Ok(n) if n > 0 && line.starts_with(b"FRAME") => {}
            _ => return,
        }
        let mut frame = reuse.try_recv().unwrap_or_default();
        frame.resize(frame_len, 0);
        if reader.read_exact(&mut frame).is_err() || frames.send(frame).is_err() {
            return;
        }
    }
}

pub(crate) fn collect_log(pipe: impl Read, lines: &Mutex<VecDeque<String>>) {
    for line in BufReader::new(pipe).lines() {
        let Ok(line) = line else { return };
        let mut lines = lines.lock().unwrap();
        if lines.len() == 8 {
            lines.pop_front();
        }
        lines.push_back(line);
    }
}

pub(super) fn decode_error(
    path: &Path,
    reason: &str,
    stderr: &Mutex<VecDeque<String>>,
) -> EngineError {
    let log = stderr.lock().unwrap();
    let detail = if log.is_empty() {
        reason.to_string()
    } else {
        log.iter().cloned().collect::<Vec<_>>().join(" / ")
    };
    EngineError::Media(format!(
        "ffmpeg could not decode {}: {detail}",
        path.display()
    ))
}

/// Result of advancing a video to a new time.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct VideoUpdate {
    /// The frame due now was not decoded yet; the previous one stays on screen.
    pub late: bool,
    /// Frames decoded but never shown because playback had moved past them.
    pub skipped: u32,
}

/// A video being decoded into a texture in step with playback time.
pub struct VideoPlayer {
    decoder: VideoDecoder,
    texture: MediaTexture,
    duration: Option<f64>,
    has_audio: bool,
    /// Index of the next frame to take from the decoder.
    next_index: u64,
    ended: bool,
}

impl VideoPlayer {
    /// Starts decoding `range` of the file and waits until its first frame
    /// is on the GPU, so the video can be shown the moment it is due.
    ///
    /// This blocks, so call it off the render thread.
    pub fn open(
        renderer: &Renderer,
        ffmpeg: &Ffmpeg,
        path: &Path,
        max_size: (u32, u32),
        range: PlayRange,
    ) -> Result<Self, EngineError> {
        let info = ffmpeg.probe(path)?;
        let max_size = super::fit_limit(renderer, max_size);
        let decoder = VideoDecoder::spawn(ffmpeg, path, &info, max_size, range)?;
        let header = decoder.header;
        let first = match decoder.frames.recv_timeout(OPEN_TIMEOUT) {
            Ok(frame) => frame,
            Err(RecvTimeoutError::Timeout) => {
                return Err(decode_error(path, "timed out", &decoder.stderr));
            }
            Err(RecvTimeoutError::Disconnected) => {
                return Err(decode_error(path, "no frames", &decoder.stderr));
            }
        };
        let texture = MediaTexture::yuv420(renderer, header.width, header.height);
        texture.write_yuv420(renderer, &first);
        let _ = decoder.recycle.try_send(first);
        Ok(Self {
            decoder,
            texture,
            duration: info.duration.map(|duration| range.length_in(duration)),
            has_audio: info.has_audio,
            next_index: 1,
            ended: false,
        })
    }

    /// Shows the frame due `time_secs` after the start of the range.
    /// Past the end, the last frame stays on screen.
    pub fn update(&mut self, renderer: &Renderer, time_secs: f64) -> VideoUpdate {
        let mut update = VideoUpdate::default();
        let due = (time_secs.max(0.0) * self.decoder.header.fps()).floor() as u64;
        let mut latest = None;
        while !self.ended && self.next_index <= due {
            match self.decoder.frames.try_recv() {
                Ok(frame) => {
                    if let Some(passed) = latest.replace(frame) {
                        update.skipped += 1;
                        let _ = self.decoder.recycle.try_send(passed);
                    }
                    self.next_index += 1;
                }
                Err(TryRecvError::Empty) => {
                    update.late = true;
                    break;
                }
                Err(TryRecvError::Disconnected) => self.ended = true,
            }
        }
        if let Some(frame) = latest {
            self.texture.write_yuv420(renderer, &frame);
            let _ = self.decoder.recycle.try_send(frame);
        }
        update
    }

    pub fn texture(&self) -> &MediaTexture {
        &self.texture
    }

    /// Length of the range in seconds, from the length stated by the file.
    pub fn duration(&self) -> Option<f64> {
        self.duration
    }

    /// Whether the file has a sound stream.
    pub fn has_audio(&self) -> bool {
        self.has_audio
    }

    /// True once every decoded frame has been shown.
    pub fn has_ended(&self) -> bool {
        self.ended
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_tools_are_found_next_to_the_executable() {
        let dirs = SearchDirs {
            env_dir: Some(PathBuf::from("/env")),
            exe_dir: Some(PathBuf::from("/app")),
            path_dirs: vec![PathBuf::from("/usr/bin")],
        };
        let suffix = std::env::consts::EXE_SUFFIX;
        assert_eq!(
            dirs.candidates("ffprobe"),
            [
                PathBuf::from("/env").join(format!("ffprobe{suffix}")),
                PathBuf::from("/app").join(format!("fluxlide-ffprobe{suffix}")),
                PathBuf::from("/usr/bin").join(format!("ffprobe{suffix}")),
            ]
        );
    }

    #[test]
    fn probe_output_is_parsed() {
        let text = "streams.stream.0.codec_type=\"video\"\n\
                    streams.stream.0.r_frame_rate=\"30/1\"\n\
                    streams.stream.0.avg_frame_rate=\"30000/1001\"\n\
                    streams.stream.0.start_time=\"0.000000\"\n\
                    streams.stream.0.duration=\"8.008000\"\n\
                    streams.stream.1.codec_type=\"audio\"\n\
                    streams.stream.1.r_frame_rate=\"0/0\"\n\
                    streams.stream.1.start_time=\"-0.021333\"\n\
                    streams.stream.1.duration=\"8.100000\"\n\
                    format.start_time=\"-0.021333\"\n\
                    format.duration=\"8.100000\"\n";
        let info = parse_probe(text).unwrap();
        // Decoding starts at the file start, 0.021 s before the first picture.
        assert!((info.duration.unwrap() - 8.029333).abs() < 1e-6);
        assert_eq!(info.frame_rate, (30000, 1001));
        assert!(info.has_audio);
    }

    #[test]
    fn probe_falls_back_and_caps_rate() {
        let text = "streams.stream.0.codec_type=\"audio\"\n\
                    streams.stream.1.codec_type=\"video\"\n\
                    streams.stream.1.r_frame_rate=\"240/1\"\n\
                    streams.stream.1.avg_frame_rate=\"0/0\"\n\
                    streams.stream.1.start_time=\"N/A\"\n\
                    streams.stream.1.duration=\"N/A\"\n\
                    format.duration=\"3.5\"\n";
        assert_eq!(
            parse_probe(text),
            Some(VideoInfo {
                duration: Some(3.5),
                frame_rate: (60, 1),
                has_audio: true,
            })
        );
        assert_eq!(
            parse_probe("streams.stream.0.codec_type=\"audio\"\nformat.duration=\"3.5\"\n"),
            None
        );
    }

    #[test]
    fn stream_header_is_parsed() {
        let header = parse_header(
            "YUV4MPEG2 W1920 H1080 F30000:1001 Ip A1:1 C420jpeg XYSCSS=420JPEG XCOLORRANGE=LIMITED",
        )
        .unwrap();
        assert_eq!(
            header,
            StreamHeader {
                width: 1920,
                height: 1080,
                rate: (30000, 1001),
            }
        );
        assert_eq!(header.frame_len(), 1920 * 1080 * 3 / 2);
        assert!(parse_header("YUV4MPEG2 W64 H64 F30:1 C444").is_err());
        assert!(parse_header("YUV4MPEG2 W64 H64 F30:1 C420p10").is_err());
        assert!(parse_header("YUV4MPEG2 W64 F30:1").is_err());
    }
}
