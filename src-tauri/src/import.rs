//! Copying photos and videos into the project's media folder.
//!
//! Adding files puts them in the slideshow at once, as assets that are
//! still importing, so the user can type titles while they copy (overview
//! 4.7). One worker copies them in turn: each file is written next to its
//! final name as `<name>.part`, read back to check it is a usable photo or
//! video and to make its thumbnail, and only then renamed. A file that
//! fails is marked failed with a reason, and a partial copy is never taken
//! for a finished one.

use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use fluxlide_engine::MediaKind;
use fluxlide_engine::media::video::Ffmpeg;
use fluxlide_engine::show::{Asset, AssetId, AssetKind, AssetMeta, AssetState, Slide};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager, State};
use ts_rs::TS;

use crate::editor::{self, EditorState, ShowState};
use crate::session::{Session, partial_path};

/// Sent while a file copies, at most a few times a second.
pub const IMPORT_PROGRESS: &str = "import-progress";

/// Thumbnails fit in this many pixels.
const THUMBNAIL_SIZE: (u32, u32) = (512, 512);
const PROGRESS_EVERY: Duration = Duration::from_millis(200);
const COPY_CHUNK: usize = 1 << 20;

#[derive(Clone, Serialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct ImportProgress {
    asset: AssetId,
    copied: u64,
    total: u64,
}

/// One file to copy into the media folder of the project in `dir`.
struct Job {
    /// The [`Session::epoch`] it was queued in.
    epoch: u64,
    media_dir: PathBuf,
    asset: Asset,
    source: PathBuf,
}

/// The worker that copies files, one at a time.
pub struct Importer {
    jobs: Mutex<Sender<Job>>,
    /// Held by the worker while it handles a job.
    working: Arc<Mutex<()>>,
}

impl Importer {
    pub fn start(app: AppHandle, session: Arc<Mutex<Option<Session>>>) -> Self {
        let (tx, rx) = mpsc::channel();
        let working = Arc::new(Mutex::new(()));
        let worker = working.clone();
        std::thread::Builder::new()
            .name("fluxlide-import".into())
            .spawn(move || run(app, session, rx, worker))
            .expect("the import thread starts");
        Self {
            jobs: Mutex::new(tx),
            working,
        }
    }

    /// Waits for the job being handled to end, and keeps the worker from
    /// starting another while the result is held. Closing the session first
    /// makes the job give up, and any later one is skipped, so a folder
    /// removed under this hold is not made again.
    pub fn pause(&self) -> std::sync::MutexGuard<'_, ()> {
        self.working.lock().unwrap_or_else(|err| err.into_inner())
    }

    fn queue(&self, job: Job) {
        let _ = self.jobs.lock().unwrap().send(job);
    }
}

fn run(
    app: AppHandle,
    session: Arc<Mutex<Option<Session>>>,
    jobs: Receiver<Job>,
    working: Arc<Mutex<()>>,
) {
    let ffmpeg = Ffmpeg::locate().ok();
    // A job belongs to the session that was open when it was queued.
    let still_open = |epoch: u64| {
        session
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|session| session.epoch() == epoch)
    };
    for job in jobs {
        // Taken before the check, so that a job is either skipped for a
        // project that was closed or finished before it is deleted.
        let _working = working.lock().unwrap_or_else(|err| err.into_inner());
        if !still_open(job.epoch) {
            continue;
        }
        let id = job.asset.id;
        let mut last_report = Instant::now();
        let outcome = import_file(
            &job.source,
            &job.media_dir,
            &job.asset,
            ffmpeg.as_ref(),
            &mut |copied, total| {
                if last_report.elapsed() >= PROGRESS_EVERY {
                    last_report = Instant::now();
                    let _ = app.emit(
                        IMPORT_PROGRESS,
                        ImportProgress {
                            asset: id,
                            copied,
                            total,
                        },
                    );
                }
            },
            &|| !still_open(job.epoch),
        );
        let mut asset = job.asset;
        match outcome {
            Ok(Some(facts)) => {
                asset.state = AssetState::Ready;
                asset.size = Some(facts.size);
                asset.duration_ms = facts.duration_ms;
                asset.has_audio = facts.has_audio;
            }
            Ok(None) => continue,
            Err(reason) => asset.state = AssetState::Failed { reason },
        }
        let mut guard = session.lock().unwrap();
        if let Some(session) = guard
            .as_mut()
            .filter(|session| session.epoch() == job.epoch)
        {
            session.update_file(asset);
            editor::notify(&app, session);
        }
    }
}

/// What reading a copied file found out.
#[derive(Debug, PartialEq)]
struct FileFacts {
    size: [u32; 2],
    duration_ms: Option<u32>,
    has_audio: bool,
}

/// Copies `source` into `media_dir` as `asset.file`, checks it and makes its
/// thumbnail. `Ok(None)` means it was given up because `cancelled` said so;
/// nothing is left behind then, nor after an error.
fn import_file(
    source: &Path,
    media_dir: &Path,
    asset: &Asset,
    ffmpeg: Option<&Ffmpeg>,
    progress: &mut dyn FnMut(u64, u64),
    cancelled: &dyn Fn() -> bool,
) -> Result<Option<FileFacts>, String> {
    let partial = partial_path(media_dir, &asset.file);
    let result = copy_and_check(
        source, &partial, media_dir, asset, ffmpeg, progress, cancelled,
    );
    match result {
        Ok(Some(facts)) => {
            std::fs::rename(&partial, media_dir.join(&asset.file))
                .map_err(|err| format!("コピーを仕上げられませんでした: {err}"))
                .inspect_err(|_| remove_partial(&partial, media_dir, &asset.file))?;
            Ok(Some(facts))
        }
        other => {
            remove_partial(&partial, media_dir, &asset.file);
            other
        }
    }
}

fn remove_partial(partial: &Path, media_dir: &Path, file: &str) {
    let _ = std::fs::remove_file(partial);
    let _ = std::fs::remove_file(thumbnail_path(media_dir, file));
}

fn copy_and_check(
    source: &Path,
    partial: &Path,
    media_dir: &Path,
    asset: &Asset,
    ffmpeg: Option<&Ffmpeg>,
    progress: &mut dyn FnMut(u64, u64),
    cancelled: &dyn Fn() -> bool,
) -> Result<Option<FileFacts>, String> {
    let copy_failed = |err: std::io::Error| format!("コピーできませんでした: {err}");
    let mut from = File::open(source).map_err(|err| match err.kind() {
        std::io::ErrorKind::NotFound => "元のファイルが見つかりません。".to_string(),
        _ => copy_failed(err),
    })?;
    let total = from.metadata().map_err(copy_failed)?.len();
    let mut to = File::create(partial).map_err(copy_failed)?;
    let mut buffer = vec![0; COPY_CHUNK];
    let mut copied = 0;
    loop {
        if cancelled() {
            return Ok(None);
        }
        let read = from.read(&mut buffer).map_err(copy_failed)?;
        if read == 0 {
            break;
        }
        to.write_all(&buffer[..read]).map_err(copy_failed)?;
        copied += read as u64;
        progress(copied, total);
    }
    to.sync_all().map_err(copy_failed)?;
    drop(to);

    let thumbnail = thumbnail_path(media_dir, &asset.file);
    std::fs::create_dir_all(thumbnail.parent().unwrap()).map_err(copy_failed)?;
    match asset.kind {
        AssetKind::Photo => {
            let thumbnail_of = |path: &Path| {
                fluxlide_engine::media::write_thumbnail(path, &thumbnail, THUMBNAIL_SIZE)
            };
            let unreadable =
                "写真として読み込めませんでした。壊れているか、対応していない形式です。";
            let (width, height) = match thumbnail_of(partial) {
                Ok(size) => size,
                // HEIC: some are JPEGs by another name, which read above; the
                // others are made JPEG by FFmpeg, in place of the copy.
                Err(_) if is_heif(source) => {
                    let ffmpeg =
                        ffmpeg.ok_or("HEIC の写真を読むための FFmpeg が見つかりません。")?;
                    let converted = partial.with_extension("converted.jpg");
                    let result = ffmpeg
                        .picture_to_jpeg(partial, &converted)
                        .map_err(|_| unreadable.to_string())
                        .and_then(|()| std::fs::rename(&converted, partial).map_err(copy_failed))
                        .and_then(|()| thumbnail_of(partial).map_err(|_| unreadable.to_string()));
                    let _ = std::fs::remove_file(&converted);
                    result?
                }
                Err(_) => return Err(unreadable.into()),
            };
            Ok(Some(FileFacts {
                size: [width, height],
                duration_ms: None,
                has_audio: false,
            }))
        }
        AssetKind::Video => {
            let ffmpeg = ffmpeg.ok_or("動画を読むための FFmpeg が見つかりません。")?;
            let unreadable = |_| {
                "動画として読み込めませんでした。壊れているか、対応していない形式です。".to_string()
            };
            let info = ffmpeg.probe(partial).map_err(unreadable)?;
            let duration = info.duration.ok_or("動画の長さが分かりませんでした。")?;
            // A picture from a little way in, as the first is often black.
            let frame = media_dir.join(format!("{}.frame.png", asset.file));
            let picture = ffmpeg
                .extract_frame(partial, (duration / 2.0).min(1.0), &frame)
                .and_then(|()| {
                    fluxlide_engine::media::write_thumbnail(&frame, &thumbnail, THUMBNAIL_SIZE)
                });
            let _ = std::fs::remove_file(&frame);
            let (width, height) = picture.map_err(unreadable)?;
            Ok(Some(FileFacts {
                size: [width, height],
                duration_ms: Some((duration * 1000.0) as u32),
                has_audio: info.has_audio,
            }))
        }
    }
}

fn is_heif(path: &Path) -> bool {
    path.extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("heic") || ext.eq_ignore_ascii_case("heif"))
}

/// Where the thumbnail of the media file `file` is kept.
pub fn thumbnail_path(media_dir: &Path, file: &str) -> PathBuf {
    media_dir.join("thumbs").join(format!("{file}.jpg"))
}

/// The asset for `source` with the id `id`, still to be copied. A file of a
/// kind that cannot be shown is failed at once.
fn new_asset(id: AssetId, source: &Path) -> Asset {
    let name = |part: Option<&std::ffi::OsStr>| {
        part.map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default()
    };
    let extension = source
        .extension()
        .map(|ext| ext.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    // A HEIC photo is kept as the JPEG it is made into.
    let extension = if is_heif(source) {
        "jpg".into()
    } else {
        extension
    };
    let (kind, state) = match MediaKind::from_path(source) {
        Some(MediaKind::Image) => (AssetKind::Photo, AssetState::Importing),
        Some(MediaKind::Video) => (AssetKind::Video, AssetState::Importing),
        None => (
            AssetKind::Photo,
            AssetState::Failed {
                reason:
                    "対応していない種類のファイルです（写真は jpg・png・heic、動画は mp4・mov）。"
                        .into(),
            },
        ),
    };
    Asset {
        id,
        kind,
        file: format!("{}.{extension}", id.0),
        original_name: name(source.file_name()),
        state,
        meta: AssetMeta {
            title: name(source.file_stem()),
            ..AssetMeta::default()
        },
        size: None,
        duration_ms: None,
        has_audio: false,
    }
}

#[derive(Serialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct ImportResult {
    state: ShowState,
    /// The new assets, in the order of the files given.
    assets: Vec<AssetId>,
}

/// Adds `files` to the open slideshow and starts copying them. With
/// `add_slides`, each also gets a slide at the end of the list (otherwise
/// the asset is for something else, such as the watermark). Adding them is
/// one step to undo.
#[tauri::command]
pub fn import_files(
    editor: State<'_, EditorState>,
    importer: State<'_, Importer>,
    files: Vec<PathBuf>,
    add_slides: bool,
) -> Result<ImportResult, String> {
    add_files(
        &editor,
        &importer,
        files.into_iter().map(|file| (file, None)).collect(),
        add_slides,
    )
}

/// The sample slideshow (`assets/sample/sample.json`), which says how to
/// use Fluxlide on its slides.
#[derive(Deserialize)]
struct Sample {
    slides: Vec<SampleSlide>,
}

#[derive(Deserialize)]
struct SampleSlide {
    file: String,
    title: String,
    author: String,
    description: String,
}

/// Adds the sample's pictures, with their captions, to the open show.
#[tauri::command]
pub fn import_sample(
    app: AppHandle,
    editor: State<'_, EditorState>,
    importer: State<'_, Importer>,
) -> Result<ImportResult, String> {
    let dir = app
        .path()
        .resolve("sample", tauri::path::BaseDirectory::Resource)
        .map_err(|err| err.to_string())?;
    let read = std::fs::read_to_string(dir.join("sample.json"))
        .map_err(|err| format!("サンプルが見つかりません: {err}"))?;
    let sample: Sample = serde_json::from_str(&read).map_err(|err| err.to_string())?;
    let items = sample
        .slides
        .into_iter()
        .map(|slide| {
            let meta = AssetMeta {
                title: slide.title,
                author: slide.author,
                description: slide.description,
            };
            (dir.join(slide.file), Some(meta))
        })
        .collect();
    add_files(&editor, &importer, items, true)
}

/// Adds the files as assets (with slides, if `add_slides`) and queues their
/// import. A file's title is its name unless `meta` is given.
fn add_files(
    editor: &EditorState,
    importer: &Importer,
    files: Vec<(PathBuf, Option<AssetMeta>)>,
    add_slides: bool,
) -> Result<ImportResult, String> {
    let session = editor.session();
    let mut guard = session.lock().unwrap();
    let session = guard.as_mut().ok_or("スライドショーが開かれていません。")?;
    let mut next = session.show().clone();
    let mut added = Vec::new();
    for (source, meta) in files {
        let mut asset = new_asset(session.next_asset_id(), &source);
        if let Some(meta) = meta {
            asset.meta = meta;
        }
        if add_slides {
            next.slides
                .push(Slide::new(session.next_slide_id(), asset.id));
        }
        next.assets.push(asset.clone());
        session.update_file(asset.clone());
        added.push((asset, source));
    }
    session.edit(next, None);
    for (asset, source) in &added {
        if asset.state == AssetState::Importing {
            importer.queue(Job {
                epoch: session.epoch(),
                media_dir: session.media_dir(),
                asset: asset.clone(),
                source: source.clone(),
            });
        }
    }
    Ok(ImportResult {
        state: ShowState::of(session),
        assets: added.into_iter().map(|(asset, _)| asset.id).collect(),
    })
}

/// Copies `file` in place of the file of `asset` ("選び直す" for a file
/// that failed). The title and the slides that show it stay.
#[tauri::command]
pub fn replace_asset_file(
    editor: State<'_, EditorState>,
    importer: State<'_, Importer>,
    asset: AssetId,
    file: PathBuf,
) -> Result<ShowState, String> {
    let session = editor.session();
    let mut guard = session.lock().unwrap();
    let session = guard.as_mut().ok_or("スライドショーが開かれていません。")?;
    let current = session
        .show()
        .asset(asset)
        .ok_or("その写真・動画はスライドショーにありません。")?;
    let mut replaced = new_asset(asset, &file);
    replaced.meta = current.meta.clone();
    session.update_file(replaced.clone());
    if replaced.state == AssetState::Importing {
        importer.queue(Job {
            epoch: session.epoch(),
            media_dir: session.media_dir(),
            asset: replaced,
            source: file,
        });
    }
    Ok(ShowState::of(session))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::tests::TempDir;

    fn import(source: &Path, media_dir: &Path, asset: &Asset) -> Result<Option<FileFacts>, String> {
        import_file(
            source,
            media_dir,
            asset,
            Ffmpeg::locate().ok().as_ref(),
            &mut |_, _| {},
            &|| false,
        )
    }

    fn leftovers(media_dir: &Path) -> Vec<String> {
        let mut names: Vec<_> = std::fs::read_dir(media_dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn a_photo_is_copied_with_its_size_and_thumbnail() {
        let root = TempDir::new("import-photo");
        let source = root.0.join("夕焼け.png");
        image::RgbaImage::from_pixel(640, 480, image::Rgba([250, 120, 30, 255]))
            .save(&source)
            .unwrap();
        let media = root.0.join("media");
        std::fs::create_dir(&media).unwrap();
        let asset = new_asset(AssetId(7), &source);
        assert_eq!(asset.file, "7.png");
        assert_eq!(asset.meta.title, "夕焼け");

        let mut reports = Vec::new();
        let facts = import_file(
            &source,
            &media,
            &asset,
            None,
            &mut |copied, total| reports.push((copied, total)),
            &|| false,
        )
        .unwrap()
        .unwrap();
        assert_eq!(facts.size, [640, 480]);
        assert_eq!(
            reports.last().map(|(copied, total)| copied == total),
            Some(true)
        );
        assert_eq!(
            std::fs::read(media.join("7.png")).unwrap(),
            std::fs::read(&source).unwrap()
        );
        assert!(thumbnail_path(&media, "7.png").is_file());
        assert_eq!(leftovers(&media), ["7.png", "thumbs"]);
    }

    #[test]
    fn a_broken_or_missing_file_fails_and_leaves_nothing_behind() {
        let root = TempDir::new("import-broken");
        let media = root.0.join("media");
        std::fs::create_dir(&media).unwrap();
        let broken = root.0.join("broken.jpg");
        std::fs::write(&broken, b"this is not a photo").unwrap();
        let reason = import(&broken, &media, &new_asset(AssetId(1), &broken)).unwrap_err();
        assert!(reason.contains("写真として読み込めません"), "{reason}");
        assert_eq!(leftovers(&media), ["thumbs"]);

        let missing = root.0.join("gone.jpg");
        let reason = import(&missing, &media, &new_asset(AssetId(2), &missing)).unwrap_err();
        assert!(reason.contains("見つかりません"), "{reason}");
        assert_eq!(leftovers(&media), ["thumbs"]);
    }

    #[test]
    fn a_cancelled_copy_leaves_nothing_behind() {
        let root = TempDir::new("import-cancel");
        let media = root.0.join("media");
        std::fs::create_dir(&media).unwrap();
        let source = root.0.join("big.jpg");
        std::fs::write(&source, vec![0u8; COPY_CHUNK * 3]).unwrap();
        let chunks = std::cell::Cell::new(0);
        let outcome = import_file(
            &source,
            &media,
            &new_asset(AssetId(1), &source),
            None,
            &mut |_, _| chunks.set(chunks.get() + 1),
            &|| chunks.get() >= 1,
        );
        assert_eq!(outcome, Ok(None));
        assert!(leftovers(&media).is_empty());
    }

    #[test]
    fn unsupported_files_fail_before_copying() {
        let asset = new_asset(AssetId(3), Path::new("/somewhere/notes.TXT"));
        assert_eq!(asset.file, "3.txt");
        assert!(matches!(asset.state, AssetState::Failed { .. }));
        assert_eq!(
            new_asset(AssetId(4), Path::new("clip.MOV")).kind,
            AssetKind::Video
        );
    }

    #[test]
    fn a_video_is_copied_with_its_length_size_and_thumbnail() {
        let Ok(ffmpeg) = Ffmpeg::locate() else {
            eprintln!("skipping video import test: ffmpeg not found");
            return;
        };
        let root = TempDir::new("import-video");
        let source = root.0.join("clip.mp4");
        let status = std::process::Command::new(ffmpeg.ffmpeg_path())
            .args(["-v", "error", "-y", "-f", "lavfi", "-i"])
            .arg("testsrc2=size=160x90:rate=30:duration=2")
            .args(["-f", "lavfi", "-i", "sine=frequency=440:duration=2"])
            .args(["-pix_fmt", "yuv420p", "-c:a", "aac", "-shortest"])
            .arg(&source)
            .status()
            .unwrap();
        assert!(status.success());
        let media = root.0.join("media");
        std::fs::create_dir(&media).unwrap();
        let facts = import(&source, &media, &new_asset(AssetId(5), &source))
            .unwrap()
            .unwrap();
        assert_eq!(facts.size, [160, 90]);
        assert!(facts.has_audio);
        assert!(
            (1_900..=2_100).contains(&facts.duration_ms.unwrap()),
            "{facts:?}"
        );
        assert!(thumbnail_path(&media, "5.mp4").is_file());
        assert_eq!(leftovers(&media), ["5.mp4", "thumbs"]);
    }

    #[test]
    fn the_sample_lists_pictures_that_are_there() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../assets/sample");
        let sample: Sample =
            serde_json::from_str(&std::fs::read_to_string(dir.join("sample.json")).unwrap())
                .unwrap();
        assert!(!sample.slides.is_empty());
        for slide in &sample.slides {
            let path = dir.join(&slide.file);
            assert_eq!(
                MediaKind::from_path(&path),
                Some(MediaKind::Image),
                "{}",
                slide.file
            );
            fluxlide_engine::media::write_thumbnail(
                &path,
                &std::env::temp_dir().join(format!("fluxlide-sample-{}.jpg", std::process::id())),
                (64, 64),
            )
            .unwrap();
            assert!(!slide.title.is_empty() && !slide.description.is_empty());
        }
    }

    #[test]
    fn a_heic_photo_is_kept_as_a_jpeg() {
        let root = TempDir::new("import-heic");
        let media = root.0.join("media");
        std::fs::create_dir(&media).unwrap();

        // Some ".HEIC" files are JPEGs (photos exported by some services).
        let named = root.0.join("IMG_0001.HEIC");
        image::RgbImage::from_pixel(320, 240, image::Rgb([30, 90, 200]))
            .save_with_format(&named, image::ImageFormat::Jpeg)
            .unwrap();
        let asset = new_asset(AssetId(8), &named);
        assert_eq!(
            (asset.kind, asset.file.as_str()),
            (AssetKind::Photo, "8.jpg")
        );
        let facts = import(&named, &media, &asset).unwrap().unwrap();
        assert_eq!(facts.size, [320, 240]);

        // The others are made JPEG by FFmpeg. A one-frame video stands in for
        // a real HEIC here: neither can be read by the image decoder.
        let Ok(ffmpeg) = Ffmpeg::locate() else {
            eprintln!("skipping HEIC conversion test: ffmpeg not found");
            return;
        };
        let packed = root.0.join("IMG_0002.heic");
        let status = std::process::Command::new(ffmpeg.ffmpeg_path())
            .args(["-v", "error", "-y", "-f", "lavfi", "-i"])
            .arg("testsrc2=size=160x90:rate=1:duration=1")
            .args(["-frames:v", "1", "-c:v", "mpeg4", "-f", "mp4"])
            .arg(&packed)
            .status()
            .unwrap();
        assert!(status.success());
        let asset = new_asset(AssetId(9), &packed);
        let facts = import(&packed, &media, &asset).unwrap().unwrap();
        assert_eq!(facts.size, [160, 90]);
        let kept = std::fs::read(media.join("9.jpg")).unwrap();
        assert_eq!(&kept[..2], [0xFF, 0xD8], "a JPEG");

        // Neither: a clear failure, and nothing left behind.
        let broken = root.0.join("IMG_0003.heic");
        std::fs::write(&broken, b"not a picture").unwrap();
        let err = import(&broken, &media, &new_asset(AssetId(10), &broken)).unwrap_err();
        assert!(err.contains("写真として読み込めませんでした"), "{err}");
        assert_eq!(leftovers(&media), ["8.jpg", "9.jpg", "thumbs"]);
    }

    #[test]
    fn a_video_shot_upright_has_its_upright_size() {
        let Ok(ffmpeg) = Ffmpeg::locate() else {
            eprintln!("skipping upright video test: ffmpeg not found");
            return;
        };
        let root = TempDir::new("import-upright");
        let flat = root.0.join("flat.mp4");
        let upright = root.0.join("upright.mp4");
        let made = std::process::Command::new(ffmpeg.ffmpeg_path())
            .args(["-v", "error", "-y", "-f", "lavfi", "-i"])
            .arg("testsrc2=size=160x90:rate=30:duration=1")
            .args(["-pix_fmt", "yuv420p", "-c:v", "mpeg4"])
            .arg(&flat)
            .status()
            .unwrap()
            .success()
            // A phone held upright records landscape frames and a rotation.
            && std::process::Command::new(ffmpeg.ffmpeg_path())
                .args(["-v", "error", "-y", "-display_rotation", "90", "-i"])
                .arg(&flat)
                .args(["-c", "copy"])
                .arg(&upright)
                .status()
                .unwrap()
                .success();
        if !made {
            eprintln!("skipping upright video test: this ffmpeg cannot set a rotation");
            return;
        }
        let media = root.0.join("media");
        std::fs::create_dir(&media).unwrap();
        let facts = import(&upright, &media, &new_asset(AssetId(11), &upright))
            .unwrap()
            .unwrap();
        assert_eq!(facts.size, [90, 160]);
    }
}
