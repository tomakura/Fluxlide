//! How loud each music file is, kept between runs ("音量をそろえる").
//!
//! Music is not copied into a project, so its loudness cannot be kept in
//! the definition like a video's. It is kept here instead, in
//! `loudness.json` in the app's data folder, under the file's path with its
//! size and time of change: a file that changes is measured again.
//!
//! The BGM asks for a track's gain just before it opens the track
//! ([`levelling`]); a track not measured yet is measured then, which delays
//! only that track. To spare the first track that wait, the tracks of a
//! show are measured in the background as soon as they are chosen
//! ([`measure_soon`]).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Sender};
use std::sync::{Mutex, OnceLock};
use std::time::UNIX_EPOCH;

use fluxlide_engine::bgm::TrackGains;
use fluxlide_engine::media::video::Ffmpeg;
use fluxlide_engine::show::Loudness;
use serde::{Deserialize, Serialize};

const FILE: &str = "loudness.json";

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Entry {
    size: u64,
    /// Time of the last change, in milliseconds since the Unix epoch.
    modified_ms: u64,
    loudness: Loudness,
}

/// The measured files and where they are kept.
struct Cache {
    /// `None` keeps the measurements in memory only.
    file: Option<PathBuf>,
    entries: Mutex<HashMap<PathBuf, Entry>>,
}

static CACHE: OnceLock<Cache> = OnceLock::new();
/// The thread that measures for [`measure_soon`].
static BACKGROUND: OnceLock<Mutex<Sender<PathBuf>>> = OnceLock::new();

/// Reads the measurements kept in `data_dir`. Called once at start; without
/// it, measurements are kept only while the app runs.
pub fn init(data_dir: &Path) {
    let file = data_dir.join(FILE);
    // A file that cannot be read is measured again, track by track.
    let entries = std::fs::read(&file)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default();
    let _ = CACHE.set(Cache::new(Some(file), entries));
}

fn cache() -> &'static Cache {
    CACHE.get_or_init(|| Cache::new(None, HashMap::new()))
}

/// The gains that level the BGM's tracks.
pub fn levelling() -> TrackGains {
    TrackGains::new(|path| cache().loudness_of(path).map(|loudness| loudness.gain()))
}

/// Measures `tracks` in the background, one after another, unless they are
/// measured already.
pub fn measure_soon(tracks: impl IntoIterator<Item = PathBuf>) {
    let background = BACKGROUND.get_or_init(|| {
        let (tx, rx) = mpsc::channel::<PathBuf>();
        std::thread::Builder::new()
            .name("fluxlide-loudness".into())
            .spawn(move || {
                for path in rx {
                    // A file that cannot be measured is reported by the BGM
                    // when its turn comes.
                    let _ = cache().loudness_of(&path);
                }
            })
            .expect("the loudness thread starts");
        Mutex::new(tx)
    });
    let background = background.lock().unwrap();
    for track in tracks {
        let _ = background.send(track);
    }
}

impl Cache {
    fn new(file: Option<PathBuf>, entries: HashMap<PathBuf, Entry>) -> Self {
        Self {
            file,
            entries: Mutex::new(entries),
        }
    }

    fn loudness_of(&self, path: &Path) -> Result<Loudness, String> {
        let (size, modified_ms) = stamp(path)?;
        let known = self.entries.lock().unwrap().get(path).copied();
        if let Some(entry) = known.filter(|e| e.size == size && e.modified_ms == modified_ms) {
            return Ok(entry.loudness);
        }
        let ffmpeg = Ffmpeg::locate().map_err(|err| err.to_string())?;
        let loudness = ffmpeg
            .measure_loudness(path)
            .map_err(|err| format!("音の大きさを測れませんでした: {err}"))?;
        let mut entries = self.entries.lock().unwrap();
        entries.insert(
            path.to_path_buf(),
            Entry {
                size,
                modified_ms,
                loudness,
            },
        );
        if let Some(file) = &self.file {
            // Kept for the next run only; the gain is right without it.
            let _ = write(file, &entries);
        }
        Ok(loudness)
    }
}

/// The size and time of change of `path`.
fn stamp(path: &Path) -> Result<(u64, u64), String> {
    let meta = std::fs::metadata(path).map_err(|err| format!("ファイルを読めません: {err}"))?;
    let modified = meta
        .modified()
        .map_err(|err| format!("ファイルを読めません: {err}"))?
        .duration_since(UNIX_EPOCH)
        .map_or(0, |time| time.as_millis() as u64);
    Ok((meta.len(), modified))
}

/// Writes `entries` to `file` through a temporary file, so that a crash
/// leaves the old one whole.
fn write(file: &Path, entries: &HashMap<PathBuf, Entry>) -> std::io::Result<()> {
    let json = serde_json::to_vec(entries).map_err(std::io::Error::other)?;
    let partial = file.with_extension("json.part");
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(&partial, json)?;
    std::fs::rename(&partial, file)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::tests::TempDir;

    fn tone(dir: &Path, name: &str, volume: f32) -> Option<PathBuf> {
        let ffmpeg = Ffmpeg::locate().ok()?;
        let path = dir.join(name);
        let made = std::process::Command::new(ffmpeg.ffmpeg_path())
            .args(["-v", "error", "-y", "-f", "lavfi", "-i", "sine=f=440:d=2"])
            .args(["-af", &format!("volume={volume}")])
            .arg(&path)
            .status()
            .ok()?;
        made.success().then_some(path)
    }

    #[test]
    fn a_measurement_is_kept_until_the_file_changes() {
        let root = TempDir::new("loudness-cache");
        let Some(path) = tone(&root.0, "a.wav", 1.0) else {
            eprintln!("skipped: no ffmpeg");
            return;
        };
        let file = root.0.join(FILE);
        let cache = Cache::new(Some(file.clone()), HashMap::new());
        let first = cache.loudness_of(&path).unwrap();

        // Read back from the file, as on the next run.
        let kept: HashMap<PathBuf, Entry> =
            serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
        assert_eq!(kept[&path].loudness, first);

        // The same path, now quieter: measured again, not taken from before.
        std::thread::sleep(std::time::Duration::from_millis(20));
        tone(&root.0, "a.wav", 0.25).unwrap();
        let second = cache.loudness_of(&path).unwrap();
        assert!(
            second.integrated_lufs < first.integrated_lufs - 10.0,
            "{first:?} then {second:?}"
        );
    }

    #[test]
    fn a_missing_file_is_an_error() {
        let cache = Cache::new(None, HashMap::new());
        assert!(cache.loudness_of(Path::new("/no/such/song.mp3")).is_err());
    }
}
