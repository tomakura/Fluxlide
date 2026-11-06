//! A slideshow packed into one `.fluxlide` file, to open it in Fluxlide on
//! another computer.
//!
//! The file is a zip archive, stored without compression since photos,
//! videos and music are compressed already:
//!
//! ```text
//! fluxlide.json     what kind of file this is, and its format version
//! show.json         the definition, as a project stores it
//! media/            the files of the assets, as in a project folder
//!   thumbs/         their thumbnails
//! music/            the background music, when it is included
//! ```
//!
//! Music is not copied into a project, so it is named in the definition by
//! paths on this computer. In the package those paths become names under
//! `music/` (or the music is left out, and must be chosen again), and on
//! reading they become paths in the new project's own `music/` folder.
//!
//! Reading takes only the entries the definition names, at paths made from
//! names checked to be plain file names, so a package cannot write outside
//! the new project's folder. The design is in `docs/project-storage.md`.

use std::fs::File;
use std::io::{BufWriter, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use fluxlide_engine::show::{AssetState, MusicSource, ShowDefinition};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager, State};
use ts_rs::TS;
use zip::write::SimpleFileOptions;
use zip::{ZipArchive, ZipWriter};

use crate::editor::{EditorState, projects_dir};
use crate::import::thumbnail_path;
use crate::presentation::bgm_source;
use crate::project::{self, MEDIA_DIR, Project};

pub const EXTENSION: &str = "fluxlide";
pub const PROGRESS: &str = "package-progress";
/// A package was opened from outside the app (a double click, say); its
/// path waits in [`OpenedPackages`].
pub const OPENED: &str = "package-opened";

const MANIFEST: &str = "fluxlide.json";
const DEFINITION: &str = "show.json";
const MUSIC_DIR: &str = "music";
const THUMBS_DIR: &str = "thumbs";
/// Layout of the package. Raised when an older Fluxlide could not read it.
const FORMAT_VERSION: u32 = 1;
const KIND: &str = "fluxlide-show";
/// Bytes a zip entry adds besides its contents (headers twice, with the
/// zip64 fields), not counting its name.
const ENTRY_OVERHEAD: u64 = 128;
const CHUNK: usize = 1 << 20;
const PROGRESS_EVERY: Duration = Duration::from_millis(200);

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Manifest {
    kind: String,
    format: u32,
    app_version: String,
}

/// One file to put in the package.
#[derive(Debug, Clone, PartialEq)]
struct Entry {
    name: String,
    source: PathBuf,
    bytes: u64,
}

/// What writing a package of one show takes.
#[derive(Debug)]
struct Plan {
    /// The definition as the package stores it.
    show: ShowDefinition,
    entries: Vec<Entry>,
}

impl Plan {
    /// Bytes of the files copied into the package.
    fn content_bytes(&self) -> u64 {
        self.entries.iter().map(|entry| entry.bytes).sum()
    }

    /// Size of the package file, close to what is written.
    fn file_bytes(&self) -> u64 {
        let definition = serde_json::to_vec(&self.show).map_or(0, |json| json.len() as u64);
        // The manifest, its entry and the end of the archive.
        let fixed = 512 + 2 * ENTRY_OVERHEAD;
        entries_bytes(&self.entries) + definition + fixed
    }
}

/// Size the files of `entries` take in a package, with their headers.
fn entries_bytes(entries: &[Entry]) -> u64 {
    entries
        .iter()
        .map(|entry| entry.bytes + ENTRY_OVERHEAD + 2 * entry.name.len() as u64)
        .sum()
}

/// The files of the assets of `show`, kept in `media_dir`. Failed assets
/// have none; the package keeps them failed.
fn media_entries(show: &ShowDefinition, media_dir: &Path) -> Result<Vec<Entry>, String> {
    let mut entries = Vec::new();
    for asset in show.assets.iter().filter(|asset| asset.is_ready()) {
        let source = media_dir.join(&asset.file);
        let bytes = std::fs::metadata(&source)
            .map_err(|err| format!("「{}」を読めません: {err}", asset.original_name))?
            .len();
        entries.push(Entry {
            name: format!("{MEDIA_DIR}/{}", asset.file),
            source,
            bytes,
        });
        let thumb = thumbnail_path(media_dir, &asset.file);
        if let Ok(meta) = std::fs::metadata(&thumb) {
            entries.push(Entry {
                name: format!("{MEDIA_DIR}/{THUMBS_DIR}/{}.jpg", asset.file),
                source: thumb,
                bytes: meta.len(),
            });
        }
    }
    Ok(entries)
}

/// The music files of `source`, under unique names in `music/`, and the
/// source as the package names it.
fn music_entries(source: &MusicSource) -> Result<(Vec<Entry>, MusicSource), String> {
    let tracks = bgm_source(source)
        .tracks()
        .map_err(|err| format!("BGMを読めません: {err}"))?;
    let mut entries: Vec<Entry> = Vec::new();
    for track in tracks {
        let bytes = std::fs::metadata(&track)
            .map_err(|err| format!("BGMの「{}」を読めません: {err}", track.display()))?
            .len();
        let name = unique_name(&track, |name| {
            entries
                .iter()
                .any(|entry| entry.name == format!("{MUSIC_DIR}/{name}"))
        });
        entries.push(Entry {
            name: format!("{MUSIC_DIR}/{name}"),
            source: track,
            bytes,
        });
    }
    let packed = match source {
        MusicSource::Folder { .. } => MusicSource::Folder {
            path: MUSIC_DIR.into(),
        },
        MusicSource::Files { .. } => MusicSource::Files {
            paths: entries.iter().map(|entry| entry.name.clone()).collect(),
        },
    };
    Ok((entries, packed))
}

/// The file name of `path`, with a number added while `taken` says it is
/// used.
fn unique_name(path: &Path, taken: impl Fn(&str) -> bool) -> String {
    let stem = path
        .file_stem()
        .map_or("music".into(), |stem| stem.to_string_lossy().into_owned());
    let ext = path
        .extension()
        .map(|ext| format!(".{}", ext.to_string_lossy()))
        .unwrap_or_default();
    (1..)
        .map(|n| match n {
            1 => format!("{stem}{ext}"),
            n => format!("{stem} ({n}){ext}"),
        })
        .find(|name| !taken(name))
        .expect("an unused name")
}

fn plan(mut show: ShowDefinition, media_dir: &Path, music: bool) -> Result<Plan, String> {
    let importing = importing_count(&show);
    if importing > 0 {
        return Err(format!(
            "取り込み中の写真や動画が{importing}件あります。終わってから書き出してください。"
        ));
    }
    let mut entries = media_entries(&show, media_dir)?;
    show.music.source = match (&show.music.source, music) {
        (Some(source), true) => {
            let (music, packed) = music_entries(source)?;
            entries.extend(music);
            Some(packed)
        }
        // Paths on this computer would not lead anywhere on another.
        _ => None,
    };
    Ok(Plan { show, entries })
}

fn importing_count(show: &ShowDefinition) -> usize {
    show.assets
        .iter()
        .filter(|asset| asset.state == AssetState::Importing)
        .count()
}

/// The definition of the project in `dir`: the open one's from memory,
/// which is what was last saved or, if saving failed, newer.
fn definition_of(state: &EditorState, dir: &Path) -> Result<ShowDefinition, String> {
    if let Some(session) = state.session().lock().unwrap().as_ref()
        && session.dir() == dir
    {
        return Ok(session.show().clone());
    }
    project::read_definition(dir).map_err(|err| err.to_string())
}

// ---------------------------------------------------------------------------
// Commands

/// Only one package is written or read at a time.
#[derive(Default)]
pub struct PackageState {
    busy: AtomicBool,
    cancel: AtomicBool,
}

impl PackageState {
    fn begin(&self) -> Result<Busy<'_>, String> {
        if self.busy.swap(true, Ordering::AcqRel) {
            return Err("ほかのファイルを書き出しているか、読み込んでいるところです。".into());
        }
        self.cancel.store(false, Ordering::Release);
        Ok(Busy(self))
    }
}

struct Busy<'a>(&'a PackageState);

impl Drop for Busy<'_> {
    fn drop(&mut self) {
        self.0.busy.store(false, Ordering::Release);
    }
}

const CANCELLED: &str = "中止しました。";

#[derive(Debug, Serialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct PackageEstimate {
    /// Size of the file without the music.
    bytes: u64,
    /// `None` when the show has no music.
    music: Option<MusicEstimate>,
    /// Photos and videos still being copied; the package must wait for them.
    importing: usize,
    /// Assets whose copy failed. They stay in the package, failed.
    failed: usize,
}

#[derive(Debug, Serialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct MusicEstimate {
    tracks: usize,
    /// Size the music adds.
    bytes: u64,
    /// Why the music cannot be included (a file is missing, say).
    error: Option<String>,
}

#[derive(Clone, Serialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct PackageProgress {
    done: u64,
    total: u64,
}

/// What a package of the project in `dir` will hold, and how large it is.
#[tauri::command]
pub async fn package_estimate(
    editor: State<'_, EditorState>,
    dir: PathBuf,
) -> Result<PackageEstimate, String> {
    let show = definition_of(&editor, &dir)?;
    tauri::async_runtime::spawn_blocking(move || {
        let media_dir = dir.join(MEDIA_DIR);
        let importing = importing_count(&show);
        let failed = show
            .assets
            .iter()
            .filter(|asset| matches!(asset.state, AssetState::Failed { .. }))
            .count();
        let without = Plan {
            entries: media_entries(&show, &media_dir)?,
            show: ShowDefinition {
                music: Default::default(),
                ..show.clone()
            },
        };
        let music = show
            .music
            .source
            .as_ref()
            .map(|source| match music_entries(source) {
                Ok((entries, _)) => MusicEstimate {
                    tracks: entries.len(),
                    bytes: entries_bytes(&entries),
                    error: None,
                },
                Err(error) => MusicEstimate {
                    tracks: 0,
                    bytes: 0,
                    error: Some(error),
                },
            });
        Ok(PackageEstimate {
            bytes: without.file_bytes(),
            music,
            importing,
            failed,
        })
    })
    .await
    .map_err(|err| err.to_string())?
}

/// Writes the project in `dir` to the package `path`, with its music if
/// `include_music`. Progress is sent as [`PROGRESS`] events.
#[tauri::command]
pub async fn export_package(
    app: AppHandle,
    editor: State<'_, EditorState>,
    dir: PathBuf,
    path: PathBuf,
    include_music: bool,
) -> Result<(), String> {
    let show = definition_of(&editor, &dir)?;
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<PackageState>();
        let _busy = state.begin()?;
        let plan = plan(show, &dir.join(MEDIA_DIR), include_music)?;
        let mut progress = Reporter::new(&app, plan.content_bytes());
        write_package(&plan, &path, &state.cancel, &mut progress)
    })
    .await
    .map_err(|err| err.to_string())?
}

/// Reads the package `path` into a new project in the app's folder and
/// returns that folder. Progress is sent as [`PROGRESS`] events.
#[tauri::command]
pub async fn import_package(app: AppHandle, path: PathBuf) -> Result<PathBuf, String> {
    let root = projects_dir(&app)?;
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<PackageState>();
        let _busy = state.begin()?;
        let reporter = |total| Reporter::new(&app, total);
        read_package(&path, &root, &state.cancel, reporter)
    })
    .await
    .map_err(|err| err.to_string())?
}

/// Stops the package being written or read; its command then fails.
#[tauri::command]
pub fn cancel_package(state: State<'_, PackageState>) {
    state.cancel.store(true, Ordering::Release);
}

/// Packages opened from outside the app, waiting for the window to read them.
#[derive(Default)]
pub struct OpenedPackages(std::sync::Mutex<Vec<PathBuf>>);

impl OpenedPackages {
    /// Keeps the packages among `args` (a command line, or the files the
    /// system asked the app to open) and tells the window.
    pub fn add(app: &AppHandle, args: impl IntoIterator<Item = PathBuf>) {
        let found: Vec<_> = args.into_iter().filter(|path| is_package(path)).collect();
        if found.is_empty() {
            return;
        }
        app.state::<Self>().0.lock().unwrap().extend(found);
        let _ = app.emit(OPENED, ());
    }
}

fn is_package(path: &Path) -> bool {
    path.extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case(EXTENSION))
        && path.is_file()
}

#[tauri::command]
pub fn take_opened_packages(opened: State<'_, OpenedPackages>) -> Vec<PathBuf> {
    std::mem::take(&mut *opened.0.lock().unwrap())
}

/// Sends [`PROGRESS`] events, a few times a second.
struct Reporter<'a> {
    app: &'a AppHandle,
    done: u64,
    total: u64,
    last: Option<Instant>,
}

impl<'a> Reporter<'a> {
    fn new(app: &'a AppHandle, total: u64) -> Self {
        let mut reporter = Self {
            app,
            done: 0,
            total,
            last: None,
        };
        reporter.send();
        reporter
    }

    fn send(&mut self) {
        self.last = Some(Instant::now());
        let _ = self.app.emit(
            PROGRESS,
            PackageProgress {
                done: self.done,
                total: self.total,
            },
        );
    }
}

impl Progress for Reporter<'_> {
    fn advance(&mut self, bytes: u64) {
        self.done += bytes;
        if self
            .last
            .is_none_or(|last| last.elapsed() >= PROGRESS_EVERY)
        {
            self.send();
        }
    }
}

trait Progress {
    fn advance(&mut self, bytes: u64);
}

// ---------------------------------------------------------------------------
// Writing and reading

/// Writes `plan` to `path`, through a `.part` file beside it, so that a
/// stopped write leaves no package that looks whole.
fn write_package(
    plan: &Plan,
    path: &Path,
    cancel: &AtomicBool,
    progress: &mut impl Progress,
) -> Result<(), String> {
    let mut partial = path.as_os_str().to_owned();
    partial.push(".part");
    let partial = PathBuf::from(partial);
    let written = write_zip(plan, &partial, cancel, progress).and_then(|()| {
        std::fs::rename(&partial, path).map_err(|err| format!("ファイルを保存できません: {err}"))
    });
    if written.is_err() {
        let _ = std::fs::remove_file(&partial);
    }
    written
}

fn write_zip(
    plan: &Plan,
    path: &Path,
    cancel: &AtomicBool,
    progress: &mut impl Progress,
) -> Result<(), String> {
    let fail = |err: &dyn std::fmt::Display| format!("ファイルを書き出せません: {err}");
    let file = File::create(path).map_err(|err| fail(&err))?;
    let mut zip = ZipWriter::new(BufWriter::new(file));
    let options = SimpleFileOptions::default().large_file(true);
    let manifest = Manifest {
        kind: KIND.into(),
        format: FORMAT_VERSION,
        app_version: env!("CARGO_PKG_VERSION").into(),
    };
    for (name, value) in [
        (MANIFEST, serde_json::to_vec_pretty(&manifest)),
        (DEFINITION, serde_json::to_vec(&plan.show)),
    ] {
        let bytes = value.map_err(|err| fail(&err))?;
        zip.start_file(name, options).map_err(|err| fail(&err))?;
        zip.write_all(&bytes).map_err(|err| fail(&err))?;
    }
    let mut buffer = vec![0; CHUNK];
    for entry in &plan.entries {
        let mut source = File::open(&entry.source)
            .map_err(|err| format!("「{}」を読めません: {err}", entry.source.display()))?;
        zip.start_file(&entry.name, options)
            .map_err(|err| fail(&err))?;
        loop {
            if cancel.load(Ordering::Acquire) {
                return Err(CANCELLED.into());
            }
            let read = source
                .read(&mut buffer)
                .map_err(|err| format!("「{}」を読めません: {err}", entry.source.display()))?;
            if read == 0 {
                break;
            }
            zip.write_all(&buffer[..read]).map_err(|err| fail(&err))?;
            progress.advance(read as u64);
        }
    }
    let mut file = zip.finish().map_err(|err| fail(&err))?;
    file.flush().map_err(|err| fail(&err))?;
    file.get_ref().sync_all().map_err(|err| fail(&err))
}

/// Prefix of the folders a package is read into, inside the app's folder,
/// before it becomes a project.
const STAGING_PREFIX: &str = ".import-";

/// Reads the package `path` into a new project folder in `root`. The files
/// are written to a hidden folder first, which becomes the project only once
/// everything is in it. `progress` is made once the size is known.
fn read_package<P: Progress>(
    path: &Path,
    root: &Path,
    cancel: &AtomicBool,
    progress: impl FnOnce(u64) -> P,
) -> Result<PathBuf, String> {
    let damaged =
        |err: &dyn std::fmt::Display| format!("Fluxlide のファイルとして読めません: {err}");
    let file = File::open(path).map_err(|err| format!("ファイルを開けません: {err}"))?;
    let mut zip = ZipArchive::new(file).map_err(|err| damaged(&err))?;
    let manifest: Manifest =
        serde_json::from_slice(&read_entry(&mut zip, MANIFEST)?).map_err(|err| damaged(&err))?;
    if manifest.kind != KIND {
        return Err(damaged(&"種類が違います"));
    }
    if manifest.format > FORMAT_VERSION {
        return Err(project::ProjectError::TooNew.to_string());
    }
    let definition =
        String::from_utf8(read_entry(&mut zip, DEFINITION)?).map_err(|err| damaged(&err))?;
    let mut show = project::parse_definition(&definition).map_err(|err| err.to_string())?;
    let names = zip
        .file_names()
        .collect::<Result<Vec<_>, _>>()
        .map_err(|err| damaged(&err))?;
    let wanted = wanted_entries(&show, names.iter().map(|name| name.as_ref()))?;
    drop(names);

    std::fs::create_dir_all(root).map_err(|err| format!("フォルダを作れません: {err}"))?;
    remove_stale_staging(root);
    let staging = root.join(format!("{STAGING_PREFIX}{}", project::now_ms()));
    // The music is named by its path in the folder the project ends up in.
    let dir = project::unique_dir(root);
    let read = extract(&mut zip, &wanted, &staging, cancel, progress)
        .and_then(|()| {
            relocate_music(&mut show, &dir);
            Project::create_at(staging.clone(), &show).map_err(|err| err.to_string())
        })
        .and_then(|project| {
            // The database is closed before its folder is renamed.
            drop(project);
            std::fs::rename(&staging, &dir)
                .map(|()| dir)
                .map_err(|err| format!("フォルダを作れません: {err}"))
        });
    if read.is_err() {
        let _ = std::fs::remove_dir_all(&staging);
    }
    read
}

/// Removes folders left by a read that was stopped by the app ending. Only
/// one package is read at a time, so none of them is in use.
fn remove_stale_staging(root: &Path) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        if entry
            .file_name()
            .to_string_lossy()
            .starts_with(STAGING_PREFIX)
        {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

fn read_entry(zip: &mut ZipArchive<File>, name: &str) -> Result<Vec<u8>, String> {
    let mut entry = zip
        .by_name(name)
        .map_err(|_| format!("Fluxlide のファイルとして読めません: {name} がありません"))?;
    let mut bytes = Vec::new();
    entry
        .read_to_end(&mut bytes)
        .map_err(|err| format!("Fluxlide のファイルとして読めません: {err}"))?;
    Ok(bytes)
}

/// One file to take from a package.
#[derive(Debug, PartialEq)]
struct Wanted {
    /// Its name in the package, and its path in the project folder.
    name: String,
    /// A thumbnail may be missing; it is made again when the photo is
    /// copied anew, and the list shows none until then.
    optional: bool,
}

/// The entries `show` needs, after checking that every name in it is a
/// plain file name. A music folder is whatever plain names `names` (those
/// in the package) has under `music/`.
fn wanted_entries<'a>(
    show: &ShowDefinition,
    names: impl Iterator<Item = &'a str>,
) -> Result<Vec<Wanted>, String> {
    let mut wanted = Vec::new();
    for asset in &show.assets {
        if !is_plain_name(&asset.file) {
            return Err(format!(
                "Fluxlide のファイルとして読めません: 素材の名前「{}」が正しくありません",
                asset.file
            ));
        }
        if asset.is_ready() {
            wanted.push(Wanted {
                name: format!("{MEDIA_DIR}/{}", asset.file),
                optional: false,
            });
            wanted.push(Wanted {
                name: format!("{MEDIA_DIR}/{THUMBS_DIR}/{}.jpg", asset.file),
                optional: true,
            });
        }
    }
    match &show.music.source {
        None => {}
        Some(MusicSource::Folder { path }) if path == MUSIC_DIR => {
            let prefix = format!("{MUSIC_DIR}/");
            wanted.extend(
                names
                    .filter(|name| name.strip_prefix(&prefix).is_some_and(is_plain_name))
                    .map(|name| Wanted {
                        name: name.to_string(),
                        optional: false,
                    }),
            );
        }
        Some(MusicSource::Folder { path }) => {
            return Err(format!(
                "Fluxlide のファイルとして読めません: BGMのフォルダ「{path}」が正しくありません"
            ));
        }
        Some(MusicSource::Files { paths }) => {
            for path in paths {
                check_music_name(path)?;
                wanted.push(Wanted {
                    name: path.clone(),
                    optional: false,
                });
            }
        }
    }
    Ok(wanted)
}

/// Checks that `path` is a name under `music/`.
fn check_music_name(path: &str) -> Result<(), String> {
    match path.strip_prefix(&format!("{MUSIC_DIR}/")) {
        Some(name) if is_plain_name(name) => Ok(()),
        _ => Err(format!(
            "Fluxlide のファイルとして読めません: BGMの名前「{path}」が正しくありません"
        )),
    }
}

/// A single file name, which cannot lead out of the folder it is joined to.
fn is_plain_name(name: &str) -> bool {
    let mut components = Path::new(name).components();
    matches!(components.next(), Some(Component::Normal(_)))
        && components.next().is_none()
        && !name.contains(['/', '\\'])
}

fn extract<P: Progress>(
    zip: &mut ZipArchive<File>,
    wanted: &[Wanted],
    dir: &Path,
    cancel: &AtomicBool,
    progress: impl FnOnce(u64) -> P,
) -> Result<(), String> {
    let fail = |err: &dyn std::fmt::Display| format!("ファイルを書けません: {err}");
    for sub in [MEDIA_DIR, &format!("{MEDIA_DIR}/{THUMBS_DIR}"), MUSIC_DIR] {
        std::fs::create_dir_all(dir.join(sub)).map_err(|err| fail(&err))?;
    }
    // Every entry's size is known before any is copied, for the progress.
    let mut found = Vec::new();
    for item in wanted {
        match zip.index_for_name(&item.name) {
            Some(index) => found.push((item, index)),
            None if item.optional => {}
            None => {
                return Err(format!(
                    "Fluxlide のファイルとして読めません: {} がありません",
                    item.name
                ));
            }
        }
    }
    let mut total = 0;
    for (_, index) in &found {
        total += zip.by_index_raw(*index).map_err(|err| fail(&err))?.size();
    }
    let mut progress = progress(total);
    let mut buffer = vec![0; CHUNK];
    for (item, index) in found {
        let mut entry = zip
            .by_index(index)
            .map_err(|err| format!("Fluxlide のファイルとして読めません: {err}"))?;
        let target = dir.join(&item.name);
        let mut out = BufWriter::new(File::create(&target).map_err(|err| fail(&err))?);
        loop {
            if cancel.load(Ordering::Acquire) {
                return Err(CANCELLED.into());
            }
            let read = entry
                .read(&mut buffer)
                .map_err(|err| format!("Fluxlide のファイルとして読めません: {err}"))?;
            if read == 0 {
                break;
            }
            out.write_all(&buffer[..read]).map_err(|err| fail(&err))?;
            progress.advance(read as u64);
        }
        let file = out.into_inner().map_err(|err| fail(&err))?;
        file.sync_all().map_err(|err| fail(&err))?;
    }
    Ok(())
}

/// Turns the music names of a package into paths in the project in `dir`.
fn relocate_music(show: &mut ShowDefinition, dir: &Path) {
    let at = |name: &str| dir.join(name).to_string_lossy().into_owned();
    show.music.source = match show.music.source.take() {
        None => None,
        Some(MusicSource::Folder { .. }) => Some(MusicSource::Folder {
            path: at(MUSIC_DIR),
        }),
        Some(MusicSource::Files { paths }) => Some(MusicSource::Files {
            paths: paths.iter().map(|path| at(path)).collect(),
        }),
    };
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::tests::{TempDir, photo_show};

    struct Silent;
    impl Progress for Silent {
        fn advance(&mut self, _: u64) {}
    }

    /// A project folder in `root` with the photos of `show` and one
    /// thumbnail.
    fn project_with_files(root: &Path, show: &ShowDefinition) -> PathBuf {
        let project = Project::create(root, show).unwrap();
        let media = project.media_dir();
        for asset in &show.assets {
            std::fs::write(media.join(&asset.file), format!("photo {}", asset.id.0)).unwrap();
        }
        std::fs::create_dir_all(media.join(THUMBS_DIR)).unwrap();
        std::fs::write(thumbnail_path(&media, &show.assets[0].file), "thumb").unwrap();
        project.dir().to_path_buf()
    }

    fn pack(show: ShowDefinition, media: &Path, music: bool, path: &Path) {
        let plan = plan(show, media, music).unwrap();
        write_package(&plan, path, &AtomicBool::new(false), &mut Silent).unwrap();
    }

    #[test]
    fn a_package_opens_as_a_new_project_with_the_same_files() {
        let root = TempDir::new("package-roundtrip");
        let music_dir = root.0.join("songs");
        std::fs::create_dir_all(&music_dir).unwrap();
        std::fs::write(music_dir.join("a.mp3"), "song a").unwrap();
        std::fs::write(music_dir.join("b.mp3"), "song b").unwrap();
        let mut show = photo_show(2);
        show.music.source = Some(MusicSource::Folder {
            path: music_dir.to_string_lossy().into_owned(),
        });
        let dir = project_with_files(&root.0.join("here"), &show);
        let path = root.0.join("show.fluxlide");
        pack(show.clone(), &dir.join(MEDIA_DIR), true, &path);

        let other = root.0.join("there");
        let opened = read_package(&path, &other, &AtomicBool::new(false), |_| Silent).unwrap();
        assert_eq!(opened.parent(), Some(other.as_path()));
        let read = project::read_definition(&opened).unwrap();
        assert_eq!(read.assets, show.assets);
        assert_eq!(read.slides, show.slides);
        assert_eq!(
            read.music.source,
            Some(MusicSource::Folder {
                path: opened.join(MUSIC_DIR).to_string_lossy().into_owned()
            })
        );
        let media = opened.join(MEDIA_DIR);
        assert_eq!(
            std::fs::read_to_string(media.join("2.jpg")).unwrap(),
            "photo 2"
        );
        assert_eq!(
            std::fs::read_to_string(thumbnail_path(&media, "1.jpg")).unwrap(),
            "thumb"
        );
        assert_eq!(
            std::fs::read_to_string(opened.join(MUSIC_DIR).join("b.mp3")).unwrap(),
            "song b"
        );
        let left: Vec<_> = std::fs::read_dir(&other).unwrap().collect();
        assert_eq!(left.len(), 1, "no staging folder is left");
    }

    #[test]
    fn music_files_with_the_same_name_keep_their_order_under_new_names() {
        let root = TempDir::new("package-music-names");
        for sub in ["x", "y"] {
            std::fs::create_dir_all(root.0.join(sub)).unwrap();
            std::fs::write(root.0.join(sub).join("song.mp3"), sub).unwrap();
        }
        let source = MusicSource::Files {
            paths: ["x", "y"]
                .map(|sub| {
                    root.0
                        .join(sub)
                        .join("song.mp3")
                        .to_string_lossy()
                        .into_owned()
                })
                .to_vec(),
        };
        let (entries, packed) = music_entries(&source).unwrap();
        assert_eq!(
            packed,
            MusicSource::Files {
                paths: vec!["music/song.mp3".into(), "music/song (2).mp3".into()]
            }
        );
        assert_eq!(entries[1].source, root.0.join("y").join("song.mp3"));
    }

    #[test]
    fn music_left_out_must_be_chosen_again() {
        let root = TempDir::new("package-no-music");
        let mut show = photo_show(1);
        show.music.source = Some(MusicSource::Files {
            paths: vec!["/nowhere/song.mp3".into()],
        });
        let dir = project_with_files(&root.0.join("here"), &show);
        let plan = plan(show, &dir.join(MEDIA_DIR), false).unwrap();
        assert_eq!(plan.show.music.source, None);
        assert!(
            plan.entries
                .iter()
                .all(|entry| entry.name.starts_with("media/"))
        );
    }

    #[test]
    fn a_show_still_importing_is_not_packed() {
        let mut show = photo_show(1);
        show.assets[0].state = AssetState::Importing;
        let err = plan(show, Path::new("/nowhere"), false).unwrap_err();
        assert!(err.contains("取り込み中"), "{err}");
    }

    #[test]
    fn names_that_lead_out_of_the_project_are_refused() {
        for name in [
            "../evil.jpg",
            "/etc/passwd",
            "a/b.jpg",
            "a\\b.jpg",
            "..",
            "",
        ] {
            assert!(!is_plain_name(name), "{name}");
        }
        assert!(is_plain_name("7.jpg"));
        let mut show = photo_show(1);
        show.assets[0].file = "../../evil.jpg".into();
        assert!(wanted_entries(&show, std::iter::empty()).is_err());
        let mut show = photo_show(1);
        show.music.source = Some(MusicSource::Files {
            paths: vec!["/home/me/song.mp3".into()],
        });
        assert!(wanted_entries(&show, std::iter::empty()).is_err());
        show.music.source = Some(MusicSource::Folder {
            path: "/home/me".into(),
        });
        assert!(wanted_entries(&show, std::iter::empty()).is_err());
    }

    #[test]
    fn a_stopped_write_leaves_no_file() {
        let root = TempDir::new("package-cancel");
        let show = photo_show(1);
        let dir = project_with_files(&root.0.join("here"), &show);
        let plan = plan(show, &dir.join(MEDIA_DIR), false).unwrap();
        let path = root.0.join("show.fluxlide");
        let err = write_package(&plan, &path, &AtomicBool::new(true), &mut Silent).unwrap_err();
        assert_eq!(err, CANCELLED);
        assert!(!path.exists());
        assert!(!root.0.join("show.fluxlide.part").exists());
    }

    #[test]
    fn the_estimate_matches_the_written_size_closely() {
        let root = TempDir::new("package-size");
        let show = photo_show(3);
        let dir = project_with_files(&root.0.join("here"), &show);
        let plan = plan(show, &dir.join(MEDIA_DIR), false).unwrap();
        let path = root.0.join("show.fluxlide");
        write_package(&plan, &path, &AtomicBool::new(false), &mut Silent).unwrap();
        let written = std::fs::metadata(&path).unwrap().len();
        let estimate = plan.file_bytes();
        assert!(
            written.abs_diff(estimate) < 512,
            "written {written}, estimated {estimate}"
        );
    }
}
