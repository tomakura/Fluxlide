//! F. 動画に書き出す: the open slideshow written to a video file, chosen by
//! what it is for rather than by format (docs/ui-draft.md, F). The export
//! runs on a thread of its own and reports its progress as events; the
//! show can be edited meanwhile, as the export works on a copy.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use fluxlide_engine::Slideshow;
use fluxlide_engine::export::{ExportMusic, ExportSettings, export_slideshow};
use fluxlide_engine::media::video::Ffmpeg;
use fluxlide_engine::show::{ShowDefinition, Timeline};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, State};
use ts_rs::TS;

use crate::editor::EditorState;
use crate::presentation::{PresentationState, bgm_config, bgm_source};

pub const PROGRESS: &str = "export-progress";
pub const FINISHED: &str = "export-finished";

/// What the video is for.
#[derive(Debug, Clone, Copy, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub enum Purpose {
    /// テレビ・プロジェクター
    Tv,
    /// パソコン・動画サイト: smaller files.
    Web,
    /// スマホ・SNS: upright.
    Phone,
    /// 高画質: 4K.
    High,
}

impl Purpose {
    fn size(self) -> (u32, u32) {
        match self {
            Purpose::Tv | Purpose::Web => (1920, 1080),
            Purpose::Phone => (1080, 1920),
            Purpose::High => (3840, 2160),
        }
    }

    fn video_kbps(self) -> u32 {
        match self {
            Purpose::Tv => 12_000,
            Purpose::Web => 6_000,
            Purpose::Phone => 8_000,
            Purpose::High => 40_000,
        }
    }
}

/// The purpose, and what 詳細設定 changes of it.
#[derive(Debug, Clone, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct ExportChoice {
    purpose: Purpose,
    fps: Option<u32>,
    video_kbps: Option<u32>,
}

const AUDIO_KBPS: u32 = 192;
const DEFAULT_FPS: u32 = 30;

impl ExportChoice {
    fn settings(&self, show: &ShowDefinition) -> ExportSettings {
        ExportSettings {
            size: self.purpose.size(),
            fps: self.fps.unwrap_or(DEFAULT_FPS).clamp(1, 60),
            video_kbps: self
                .video_kbps
                .unwrap_or(self.purpose.video_kbps())
                .clamp(500, 100_000),
            audio_kbps: AUDIO_KBPS,
            video_encoder: None,
            music: show.music.source.as_ref().map(|source| ExportMusic {
                source: bgm_source(source),
                config: bgm_config(&show.music),
                duck_for_videos: show.music.duck_for_videos,
            }),
        }
    }
}

/// What an export will make, shown before it starts.
#[derive(Serialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct ExportEstimate {
    width: u32,
    height: u32,
    fps: u32,
    video_kbps: u32,
    /// Length of the video.
    duration_secs: f64,
    /// About how large the file will be. The encoders aim at the bit rate,
    /// and photos that stay still need less, so it is an upper guess.
    bytes: u64,
    /// About how long writing it takes. It depends on the GPU and the
    /// encoder, so the window says it is a rough figure.
    work_secs: f64,
    /// Slides that will be left out (their files are not ready).
    skipped_slides: usize,
}

#[tauri::command]
pub fn export_estimate(
    editor: State<'_, EditorState>,
    choice: ExportChoice,
) -> Result<ExportEstimate, String> {
    let show = open_show(&editor)?.0;
    let settings = choice.settings(&show);
    let timeline = Timeline::build(&show, &(0..show.slides.len()).collect::<Vec<_>>());
    let duration_secs = timeline.end_ms as f64 / 1000.0;
    let kbps = u64::from(settings.video_kbps + settings.audio_kbps);
    let (width, height) = settings.size;
    // A guess of about the length of the video for 1080p at 30 fps: not yet
    // measured with a GPU (without one it took 2.6 times as long,
    // docs/spike-export.md). More pixels and frames take longer.
    let load = f64::from(width * height) / (1920.0 * 1080.0) * f64::from(settings.fps) / 30.0;
    Ok(ExportEstimate {
        width,
        height,
        fps: settings.fps,
        video_kbps: settings.video_kbps,
        duration_secs,
        bytes: (duration_secs * kbps as f64 * 1000.0 / 8.0) as u64,
        work_secs: duration_secs * load.max(0.5),
        skipped_slides: timeline.skipped.len(),
    })
}

#[derive(Clone, Serialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct ExportProgressEvent {
    frames: u64,
    total_frames: u64,
    elapsed_secs: f64,
}

#[derive(Clone, Serialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct ExportFinishedEvent {
    path: String,
    /// Why it failed; none when it was written or cancelled.
    error: Option<String>,
    cancelled: bool,
}

struct Running {
    cancel: Arc<AtomicBool>,
    thread: JoinHandle<()>,
}

#[derive(Default)]
pub struct ExportState {
    running: Mutex<Option<Running>>,
}

impl ExportState {
    /// Cancels an export and waits for it to remove its files. Used when the
    /// app exits.
    pub fn cancel_blocking(&self) {
        let running = self.running.lock().unwrap().take();
        if let Some(running) = running {
            running.cancel.store(true, Ordering::Relaxed);
            let _ = running.thread.join();
        }
    }
}

fn open_show(editor: &EditorState) -> Result<(ShowDefinition, PathBuf), String> {
    let session = editor.session();
    let guard = session.lock().unwrap();
    let session = guard.as_ref().ok_or("スライドショーが開かれていません。")?;
    Ok((session.show().clone(), session.media_dir()))
}

/// Starts writing the open show to `path` (an `.mp4`). Progress comes as
/// [`PROGRESS`] events and the end as one [`FINISHED`] event.
#[tauri::command]
pub async fn start_export(
    app: AppHandle,
    state: State<'_, ExportState>,
    editor: State<'_, EditorState>,
    presentation: State<'_, PresentationState>,
    choice: ExportChoice,
    path: PathBuf,
) -> Result<(), String> {
    let (show, media_dir) = open_show(&editor)?;
    let settings = choice.settings(&show);
    let renderer = presentation.renderer().await?;
    let ffmpeg = Ffmpeg::locate().map_err(|err| err.to_string())?;
    let total_frames = {
        let timeline = Timeline::build(&show, &(0..show.slides.len()).collect::<Vec<_>>());
        timeline.end_ms * u64::from(settings.fps) / 1000
    };
    let mut running = state.running.lock().unwrap();
    if running
        .as_ref()
        .is_some_and(|running| !running.thread.is_finished())
    {
        return Err("書き出しの途中です。".into());
    }
    let cancel = Arc::new(AtomicBool::new(false));
    let thread = {
        let cancel = cancel.clone();
        std::thread::Builder::new()
            .name("fluxlide-export".into())
            .spawn(move || {
                let started = Instant::now();
                let mut last_report = started;
                let mut slideshow =
                    Slideshow::new(renderer.clone(), show, media_dir, settings.size);
                // A panic must still end with FINISHED, or the window would
                // wait for this export forever.
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    export_slideshow(
                        &renderer,
                        &ffmpeg,
                        &mut slideshow,
                        &settings,
                        &path,
                        &cancel,
                        |progress| {
                            // A few times a second is enough for a progress bar.
                            if last_report.elapsed() < Duration::from_millis(100) {
                                return;
                            }
                            last_report = Instant::now();
                            let _ = app.emit(
                                PROGRESS,
                                ExportProgressEvent {
                                    frames: progress.frames,
                                    total_frames,
                                    elapsed_secs: started.elapsed().as_secs_f64(),
                                },
                            );
                        },
                    )
                    .map_err(|err| err.to_string())
                }))
                .unwrap_or_else(|_| Err("書き出しの途中で止まりました。".into()));
                // A cancel that comes after the file is written changes nothing.
                let cancelled = result.is_err() && cancel.load(Ordering::Relaxed);
                let _ = app.emit(
                    FINISHED,
                    ExportFinishedEvent {
                        path: path.display().to_string(),
                        error: result.err().filter(|_| !cancelled),
                        cancelled,
                    },
                );
            })
            .map_err(|err| err.to_string())?
    };
    *running = Some(Running { cancel, thread });
    Ok(())
}

/// Stops the export; it removes its partial files and sends [`FINISHED`].
#[tauri::command]
pub fn cancel_export(state: State<'_, ExportState>) {
    if let Some(running) = state.running.lock().unwrap().as_ref() {
        running.cancel.store(true, Ordering::Relaxed);
    }
}
