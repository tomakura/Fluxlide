//! Prototype of the presentation window: a native window without a webview,
//! placed fullscreen on the chosen display and rendered by wgpu on its own
//! thread while the management window stays usable. It plays the show sent
//! by the management window with transitions and captions, or a test
//! pattern when the show has no slides. Video sound goes to the default audio
//! output, with music from a folder as BGM that fades out while videos with
//! sound play.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use fluxlide_engine::audio::{AudioOutput, AudioStats};
use fluxlide_engine::bgm::{Bgm, BgmConfig, BgmSource, BgmState, BgmStats};
use fluxlide_engine::media::video::Ffmpeg;
use fluxlide_engine::show::{Asset, AssetId, AssetKind, AssetMeta, AssetState, ShowDefinition};
use fluxlide_engine::{
    FrameOutcome, MediaKind, Presenter, Renderer, Scene, Slideshow, SlideshowStats,
};
use serde::Serialize;
use tauri::{AppHandle, Manager, State, WindowEvent};
use tauri_plugin_global_shortcut::{
    Code, GlobalShortcutExt, Shortcut, ShortcutEvent, ShortcutState,
};
use ts_rs::TS;

const WINDOW_LABEL: &str = "presenter";

/// The presentation window has no webview, so it receives no key events.
/// Esc is registered as a global shortcut while that window is in front:
/// from the start of a presentation (macOS reports no focus event while the
/// window moves into its fullscreen space) until it or the main window
/// reports a focus change.
fn escape_shortcut() -> Shortcut {
    Shortcut::new(None, Code::Escape)
}

pub fn set_escape_enabled(app: &AppHandle, enabled: bool) {
    let shortcuts = app.global_shortcut();
    let registered = shortcuts.is_registered(escape_shortcut());
    if enabled && !registered {
        let _ = shortcuts.register(escape_shortcut());
    } else if !enabled && registered {
        let _ = shortcuts.unregister(escape_shortcut());
    }
}

/// Global shortcut handler: Esc in the presentation window ends the presentation.
pub fn handle_shortcut(app: &AppHandle, shortcut: &Shortcut, event: ShortcutEvent) {
    if event.state() != ShortcutState::Pressed || *shortcut != escape_shortcut() {
        return;
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        if let Some(active) = app.state::<PresentationState>().take_active() {
            shut_down(active).await;
        }
    });
}

#[derive(Serialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct MonitorInfo {
    index: usize,
    name: String,
    width: u32,
    height: u32,
    x: i32,
    y: i32,
    scale_factor: f64,
    is_primary: bool,
    has_main_window: bool,
}

/// Frame timing measured on the render thread.
#[derive(Default, Clone, Serialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct FrameStats {
    frames: u64,
    /// Frames presented during the last full second.
    fps: f64,
    /// Longest gap between two presented frames during the last full second.
    worst_frame_ms: f64,
    /// Frames that took more than 1.5 times the average interval.
    slow_frames: u64,
    skipped: u64,
    width: u32,
    height: u32,
    last_error: Option<String>,
    /// Present while playing files rather than the test pattern.
    playback: Option<PlaybackStats>,
}

/// Slideshow measurements shown in the management window.
#[derive(Default, Clone, Serialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct PlaybackStats {
    current_slide: Option<u64>,
    current_file: Option<String>,
    transitions: u64,
    late_transitions: u64,
    longest_wait_ms: f64,
    last_load_ms: f64,
    video_late_frames: u64,
    video_skipped_frames: u64,
    failed_items: u64,
    skipped_slides: usize,
    last_error: Option<String>,
    audio: Option<SoundStats>,
    /// Why the audio output could not be opened, if it could not.
    audio_error: Option<String>,
    bgm: Option<MusicStats>,
    /// Why the BGM could not be started, if it could not.
    bgm_error: Option<String>,
}

/// BGM measurements shown in the management window.
#[derive(Clone, Serialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct MusicStats {
    /// "idle", "playing" or "gap".
    #[ts(type = r#""idle" | "playing" | "gap""#)]
    state: &'static str,
    title: Option<String>,
    artist: Option<String>,
    tracks_started: u64,
    tracks_failed: u64,
    underruns: u64,
    starved_transitions: u64,
    last_error: Option<String>,
}

impl From<BgmStats> for MusicStats {
    fn from(stats: BgmStats) -> Self {
        Self {
            state: match stats.state {
                BgmState::Idle => "idle",
                BgmState::Playing => "playing",
                BgmState::Gap => "gap",
            },
            title: stats.now_playing.as_ref().map(|now| now.title.clone()),
            artist: stats.now_playing.and_then(|now| now.artist),
            tracks_started: stats.tracks_started,
            tracks_failed: stats.tracks_failed,
            underruns: stats.underruns,
            starved_transitions: stats.starved_transitions,
            last_error: stats.last_error,
        }
    }
}

/// Sound settings chosen in the management window.
pub struct SoundOptions {
    /// Folder of music played as BGM.
    pub bgm_dir: Option<PathBuf>,
    /// How much earlier the sound is delivered, for outputs such as
    /// Bluetooth speakers that are heard later than they report.
    pub sync_offset_ms: f64,
}

/// The sound side of a running presentation, owned by the render thread.
#[derive(Default)]
struct Sound {
    // The BGM is dropped first, while the output it plays through is open.
    bgm: Option<Bgm>,
    output: Option<AudioOutput>,
    audio_error: Option<String>,
    bgm_error: Option<String>,
}

/// Audio output measurements shown in the management window.
#[derive(Clone, Serialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct SoundStats {
    device: String,
    sample_rate: u32,
    output_latency_ms: f64,
    sync_offset_ms: f64,
    offset_ms: f64,
    max_offset_ms: f64,
    resyncs: u64,
    underruns: u64,
    last_error: Option<String>,
}

impl From<AudioStats> for SoundStats {
    fn from(stats: AudioStats) -> Self {
        let AudioStats {
            device,
            sample_rate,
            output_latency_ms,
            sync_offset_ms,
            offset_ms,
            max_offset_ms,
            resyncs,
            underruns,
            last_error,
        } = stats;
        Self {
            device,
            sample_rate,
            output_latency_ms,
            sync_offset_ms,
            offset_ms,
            max_offset_ms,
            resyncs,
            underruns,
            last_error,
        }
    }
}

impl PlaybackStats {
    fn from_slideshow(show: &Slideshow, sound: &Sound) -> Self {
        let SlideshowStats {
            current,
            transitions,
            late_transitions,
            longest_wait_ms,
            last_load_ms,
            video_late_frames,
            video_skipped_frames,
            failed_items,
            skipped_slides,
            last_error,
        } = show.stats().clone();
        Self {
            current_slide: current.map(|slide| slide.0),
            current_file: show
                .current_path()
                .and_then(|path| Some(path.file_name()?.to_string_lossy().into_owned())),
            transitions,
            late_transitions,
            longest_wait_ms,
            last_load_ms,
            video_late_frames,
            video_skipped_frames,
            failed_items,
            skipped_slides,
            last_error,
            audio: show.audio_stats().map(SoundStats::from),
            audio_error: sound.audio_error.clone(),
            bgm: sound.bgm.as_ref().map(|bgm| MusicStats::from(bgm.stats())),
            bgm_error: sound.bgm_error.clone(),
        }
    }
}

#[derive(Serialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct PresentationStatus {
    running: bool,
    monitor_name: Option<String>,
    stats: FrameStats,
}

struct Active {
    window: tauri::Window,
    monitor_name: String,
    stop: Arc<AtomicBool>,
    stats: Arc<Mutex<FrameStats>>,
    thread: Option<JoinHandle<()>>,
}

#[derive(Default)]
pub struct PresentationState {
    renderer: Mutex<Option<Arc<Renderer>>>,
    active: Mutex<Option<Active>>,
}

impl PresentationState {
    pub async fn renderer(&self) -> Result<Arc<Renderer>, String> {
        if let Some(renderer) = self.renderer.lock().unwrap().as_ref() {
            return Ok(renderer.clone());
        }
        let renderer = Arc::new(Renderer::new().await.map_err(|err| err.to_string())?);
        Ok(self
            .renderer
            .lock()
            .unwrap()
            .get_or_insert(renderer)
            .clone())
    }

    fn take_active(&self) -> Option<Active> {
        self.active.lock().unwrap().take()
    }

    fn status(&self) -> PresentationStatus {
        let active = self.active.lock().unwrap();
        match active.as_ref() {
            Some(active) => PresentationStatus {
                running: !active.stop.load(Ordering::Relaxed),
                monitor_name: Some(active.monitor_name.clone()),
                stats: active.stats.lock().unwrap().clone(),
            },
            None => PresentationStatus {
                running: false,
                monitor_name: None,
                stats: FrameStats::default(),
            },
        }
    }
}

fn monitor_name(monitor: &tauri::Monitor, index: usize) -> String {
    monitor
        .name()
        .cloned()
        .unwrap_or_else(|| format!("画面{}", index + 1))
}

#[tauri::command]
pub fn list_monitors(app: AppHandle) -> Result<Vec<MonitorInfo>, String> {
    let monitors = app.available_monitors().map_err(|err| err.to_string())?;
    let primary = app.primary_monitor().ok().flatten();
    let main_monitor = app
        .get_webview_window("main")
        .and_then(|window| window.current_monitor().ok().flatten());
    let same = |a: &tauri::Monitor, b: &Option<tauri::Monitor>| {
        b.as_ref()
            .is_some_and(|b| a.position() == b.position() && a.size() == b.size())
    };
    Ok(monitors
        .iter()
        .enumerate()
        .map(|(index, monitor)| MonitorInfo {
            index,
            name: monitor_name(monitor, index),
            width: monitor.size().width,
            height: monitor.size().height,
            x: monitor.position().x,
            y: monitor.position().y,
            scale_factor: monitor.scale_factor(),
            is_primary: same(monitor, &primary),
            has_main_window: same(monitor, &main_monitor),
        })
        .collect())
}

#[tauri::command]
pub fn presentation_status(state: State<'_, PresentationState>) -> PresentationStatus {
    state.status()
}

#[tauri::command]
pub async fn start_presentation(
    app: AppHandle,
    state: State<'_, PresentationState>,
    monitor_index: usize,
    show: ShowDefinition,
    bgm_dir: Option<PathBuf>,
    sync_offset_ms: Option<f64>,
) -> Result<PresentationStatus, String> {
    let sound = SoundOptions {
        bgm_dir,
        sync_offset_ms: sync_offset_ms.unwrap_or(0.0),
    };
    if let Some(active) = state.take_active() {
        shut_down(active).await;
    }

    let renderer = state.renderer().await?;
    let monitors = app.available_monitors().map_err(|err| err.to_string())?;
    let monitor = monitors
        .get(monitor_index)
        .ok_or_else(|| "指定した画面が見つかりません。".to_string())?;
    let name = monitor_name(monitor, monitor_index);
    let scale = monitor.scale_factor();
    let position = *monitor.position();
    let size = *monitor.size();

    // Window creation must not run in a synchronous command (WebView2 deadlock on Windows).
    let window = tauri::window::WindowBuilder::new(&app, WINDOW_LABEL)
        .title("Fluxlide 上映")
        .decorations(false)
        .resizable(false)
        .skip_taskbar(true)
        .focused(true)
        // macOS positions windows in global points: physical / that display's scale.
        .position(f64::from(position.x) / scale, f64::from(position.y) / scale)
        .inner_size(
            f64::from(size.width) / scale,
            f64::from(size.height) / scale,
        )
        .build()
        .map_err(|err| err.to_string())?;
    // Any failure after this point must close the window and release Esc,
    // otherwise the "presenter" label stays taken until the app restarts.
    match run_presentation(&app, &window, renderer, position, size, show, sound).await {
        Ok((stop, stats, thread)) => {
            *state.active.lock().unwrap() = Some(Active {
                window,
                monitor_name: name,
                stop,
                stats,
                thread: Some(thread),
            });
            Ok(state.status())
        }
        Err(err) => {
            set_escape_enabled(&app, false);
            let _ = window.destroy();
            Err(err)
        }
    }
}

/// The slideshow of `show` as the presentation and the preview show it.
/// Until photos and videos are copied into a managed folder, each asset's
/// `file` is the absolute path of the original (see [`media_assets`]).
pub fn build_slideshow(
    renderer: Arc<Renderer>,
    show: ShowDefinition,
    output_size: (u32, u32),
) -> Slideshow {
    Slideshow::new(renderer, show, PathBuf::new(), output_size)
}

/// Describes the photos and videos at `files` as the assets of a show,
/// numbered from 1 in the given order. Each asset's `file` is the absolute
/// path and its title the file name, until importing copies files into a
/// managed folder (stage 2, step 3). Videos are probed for their length.
#[tauri::command]
pub async fn media_assets(files: Vec<PathBuf>) -> Result<Vec<Asset>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let ffmpeg = Ffmpeg::locate().map_err(|err| err.to_string());
        files
            .iter()
            .enumerate()
            .map(|(index, path)| describe_asset(index as u64 + 1, path, &ffmpeg))
            .collect()
    })
    .await
    .map_err(|err| err.to_string())
}

fn describe_asset(id: u64, path: &Path, ffmpeg: &Result<Ffmpeg, String>) -> Asset {
    let name = |part: Option<&std::ffi::OsStr>| {
        part.map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default()
    };
    let mut asset = Asset {
        id: AssetId(id),
        kind: AssetKind::Photo,
        file: path.to_string_lossy().into_owned(),
        original_name: name(path.file_name()),
        state: AssetState::Ready,
        meta: AssetMeta {
            title: name(path.file_stem()),
            ..AssetMeta::default()
        },
        size: None,
        duration_ms: None,
        has_audio: false,
    };
    match MediaKind::from_path(path) {
        Some(MediaKind::Image) => {}
        Some(MediaKind::Video) => {
            asset.kind = AssetKind::Video;
            let probed = ffmpeg
                .clone()
                .and_then(|ffmpeg| ffmpeg.probe(path).map_err(|err| err.to_string()));
            match probed {
                Ok(info) => {
                    asset.duration_ms = info.duration.map(|secs| (secs * 1000.0) as u32);
                    asset.has_audio = info.has_audio;
                }
                Err(reason) => asset.state = AssetState::Failed { reason },
            }
        }
        None => {
            asset.state = AssetState::Failed {
                reason: "対応していない種類のファイルです。".into(),
            }
        }
    }
    asset
}

type RunningPresentation = (Arc<AtomicBool>, Arc<Mutex<FrameStats>>, JoinHandle<()>);

/// Makes `window` fullscreen, attaches a surface and starts the render thread.
async fn run_presentation(
    app: &AppHandle,
    window: &tauri::Window,
    renderer: Arc<Renderer>,
    position: tauri::PhysicalPosition<i32>,
    size: tauri::PhysicalSize<u32>,
    show: ShowDefinition,
    sound: SoundOptions,
) -> Result<RunningPresentation, String> {
    // Not `set_fullscreen_on_monitor`: it matches monitors by physical position,
    // and on macOS with mixed scale factors the physical ranges of displays
    // overlap, so it can pick the main display. Instead, move the window onto
    // the target display and make it fullscreen on the display it is on.
    // macOS already placed the window in the builder (logical position).
    #[cfg(target_os = "macos")]
    let _ = position;
    #[cfg(not(target_os = "macos"))]
    window
        .set_position(position)
        .map_err(|err| err.to_string())?;
    window.set_fullscreen(true).map_err(|err| err.to_string())?;
    let _ = window.set_cursor_visible(false);

    // Surfaces must be created on the main thread (required by AppKit on macOS).
    let (tx, rx) = mpsc::channel();
    {
        let window = window.clone();
        let renderer = renderer.clone();
        app.run_on_main_thread(move || {
            let size = window.inner_size().unwrap_or(size);
            let _ = tx.send(Presenter::new(
                &renderer,
                window.clone(),
                size.width,
                size.height,
            ));
        })
        .map_err(|err| err.to_string())?;
    }
    let presenter = match tauri::async_runtime::spawn_blocking(move || rx.recv()).await {
        Ok(Ok(result)) => result.map_err(|err| err.to_string())?,
        _ => return Err("上映ウィンドウを準備できませんでした。".to_string()),
    };

    let stop = Arc::new(AtomicBool::new(false));
    let stats = Arc::new(Mutex::new(FrameStats::default()));
    let pending_size = Arc::new(Mutex::new(None::<(u32, u32)>));
    {
        let stop = stop.clone();
        let pending_size = pending_size.clone();
        let app = app.clone();
        window.on_window_event(move |event| match event {
            WindowEvent::Focused(focused) => set_escape_enabled(&app, *focused),
            WindowEvent::Resized(size) => {
                *pending_size.lock().unwrap() = Some((size.width, size.height));
            }
            WindowEvent::ScaleFactorChanged { new_inner_size, .. } => {
                *pending_size.lock().unwrap() = Some((new_inner_size.width, new_inner_size.height));
            }
            WindowEvent::Destroyed => {
                set_escape_enabled(&app, false);
                stop.store(true, Ordering::Relaxed);
            }
            _ => {}
        });
    }

    set_escape_enabled(app, true);

    // Photos and videos are loaded at the display's full resolution.
    let slideshow = (!show.slides.is_empty())
        .then(|| build_slideshow(renderer.clone(), show, (size.width, size.height)));
    let thread = {
        let stop = stop.clone();
        let stats = stats.clone();
        std::thread::Builder::new()
            .name("fluxlide-presenter".into())
            .spawn(move || {
                render_loop(
                    renderer,
                    presenter,
                    slideshow,
                    sound,
                    stop,
                    stats,
                    pending_size,
                )
            })
            .map_err(|err| err.to_string())?
    };
    Ok((stop, stats, thread))
}

#[tauri::command]
pub async fn stop_presentation(
    state: State<'_, PresentationState>,
) -> Result<PresentationStatus, String> {
    if let Some(active) = state.take_active() {
        shut_down(active).await;
    }
    Ok(state.status())
}

/// Stops the render thread and closes the window. Used when the app exits.
pub fn stop_blocking(state: &PresentationState) {
    if let Some(mut active) = state.take_active() {
        active.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = active.thread.take() {
            let _ = thread.join();
        }
        set_escape_enabled(active.window.app_handle(), false);
        let _ = active.window.destroy();
    }
}

async fn shut_down(mut active: Active) {
    active.stop.store(true, Ordering::Relaxed);
    if let Some(thread) = active.thread.take() {
        let _ = tauri::async_runtime::spawn_blocking(move || thread.join()).await;
    }
    set_escape_enabled(active.window.app_handle(), false);
    let _ = active.window.destroy();
}

fn render_loop(
    renderer: Arc<Renderer>,
    mut presenter: Presenter,
    mut slideshow: Option<Slideshow>,
    options: SoundOptions,
    stop: Arc<AtomicBool>,
    stats: Arc<Mutex<FrameStats>>,
    pending_size: Arc<Mutex<Option<(u32, u32)>>>,
) {
    let start = Instant::now();
    // The output stays on this thread and shares the playback clock (`start`).
    let sound = match slideshow.as_mut() {
        Some(show) => open_sound(show, start, &options),
        None => Sound::default(),
    };
    let mut last_frame: Option<Instant> = None;
    let mut window_start = start;
    let mut window_frames = 0u32;
    let mut window_worst = Duration::ZERO;
    let mut average_interval: Option<Duration> = None;

    while !stop.load(Ordering::Relaxed) {
        if let Some((width, height)) = pending_size.lock().unwrap().take() {
            presenter.resize(&renderer, width, height);
            if let Some(Err(err)) = slideshow
                .as_mut()
                .map(|show| show.set_output_size(presenter.size()))
            {
                stats.lock().unwrap().last_error = Some(err.to_string());
            }
        }

        let time = start.elapsed().as_secs_f64();
        let outcome = match slideshow.as_mut() {
            Some(show) => {
                show.update(time);
                presenter.render(&renderer, &Scene::Frame(show.frame()))
            }
            None => presenter.render(
                &renderer,
                &Scene::TestPattern {
                    time_secs: time as f32,
                },
            ),
        };
        if let Some(show) = &slideshow {
            stats.lock().unwrap().playback = Some(PlaybackStats::from_slideshow(show, &sound));
        }

        match outcome {
            Ok(FrameOutcome::Presented) => {
                let now = Instant::now();
                let mut stats = stats.lock().unwrap();
                stats.frames += 1;
                (stats.width, stats.height) = presenter.size();
                if let Some(previous) = last_frame {
                    let interval = now - previous;
                    window_worst = window_worst.max(interval);
                    if average_interval.is_some_and(|avg| interval > avg.mul_f64(1.5)) {
                        stats.slow_frames += 1;
                    }
                }
                last_frame = Some(now);
                window_frames += 1;

                let elapsed = now - window_start;
                if elapsed >= Duration::from_secs(1) {
                    stats.fps = f64::from(window_frames) / elapsed.as_secs_f64();
                    stats.worst_frame_ms = window_worst.as_secs_f64() * 1000.0;
                    average_interval = Some(elapsed / window_frames.max(1));
                    window_start = now;
                    window_frames = 0;
                    window_worst = Duration::ZERO;
                }
            }
            Ok(FrameOutcome::Skipped) => {
                stats.lock().unwrap().skipped += 1;
                // The next frame after a pause is not a stutter.
                last_frame = None;
                std::thread::sleep(Duration::from_millis(16));
            }
            Err(err) => {
                stats.lock().unwrap().last_error = Some(err.to_string());
                last_frame = None;
                std::thread::sleep(Duration::from_millis(100));
            }
        }
    }
}

/// Opens the audio output for `show` and starts the BGM on it.
fn open_sound(show: &mut Slideshow, start: Instant, options: &SoundOptions) -> Sound {
    let output = match AudioOutput::open_with_sync_offset(start, options.sync_offset_ms) {
        Ok(output) => output,
        Err(err) => {
            return Sound {
                audio_error: Some(err.to_string()),
                ..Sound::default()
            };
        }
    };
    show.set_audio(output.mixer());
    let mut sound = Sound::default();
    if let Some(dir) = &options.bgm_dir {
        let bgm = Ffmpeg::locate().and_then(|ffmpeg| {
            Bgm::start(
                ffmpeg,
                &output.mixer(),
                &BgmSource::Folder(dir.clone()),
                BgmConfig {
                    shuffle: true,
                    ..BgmConfig::default()
                },
            )
        });
        match bgm {
            Ok(bgm) => {
                show.set_ducker(bgm.ducker());
                sound.bgm = Some(bgm);
            }
            Err(err) => sound.bgm_error = Some(err.to_string()),
        }
    }
    sound.output = Some(output);
    sound
}
