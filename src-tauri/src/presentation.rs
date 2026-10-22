//! The presentation window: a native window without a webview, placed
//! fullscreen on the chosen display and rendered by wgpu on its own thread
//! while the management window stays usable. It plays the open slideshow,
//! starting on its title card until the operator starts it. Video sound
//! goes to the default audio output, with the show's music as BGM that
//! fades out while videos with sound play.
//!
//! The operator controls it from the management window (`control_presentation`)
//! or with keys pressed while the presentation window is in front.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use fluxlide_engine::audio::{AudioOutput, AudioStats};
use fluxlide_engine::bgm::{Bgm, BgmConfig, BgmSource, BgmState, BgmStats, BgmTuning};
use fluxlide_engine::media::video::Ffmpeg;
use fluxlide_engine::show::{MusicSource, ShowDefinition, SlideId};
use fluxlide_engine::{
    FrameOutcome, Overlay, Position, Presenter, Renderer, Scene, Slideshow, SlideshowStats,
};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager, State, WindowEvent};
use tauri_plugin_global_shortcut::{
    Code, GlobalShortcutExt, Shortcut, ShortcutEvent, ShortcutState,
};
use ts_rs::TS;

use crate::editor::EditorState;
use crate::settings::SettingsState;

const WINDOW_LABEL: &str = "presenter";

/// Sent when Esc is pressed in the presentation window while the management
/// window is on another screen: the operator confirms there.
pub const END_REQUESTED: &str = "presentation-end-requested";
/// Sent when the presentation has ended other than by `stop_presentation`.
pub const ENDED: &str = "presentation-ended";

/// Upcoming slides reported to the management window.
const UPCOMING: usize = 8;

/// What the operator can do during a presentation.
#[derive(Debug, Clone, Copy, PartialEq, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum Control {
    /// Leaves the title card for the first slide.
    Start,
    Next,
    Previous,
    TogglePause,
    ToggleBlackout,
    NextTrack,
    MusicVolume {
        volume: f32,
    },
    SyncOffset {
        ms: f64,
    },
}

/// The presentation window has no webview, so it receives no key events.
/// Its keys are registered as global shortcuts while that window is in
/// front: from the start of a presentation (macOS reports no focus event
/// while the window moves into its fullscreen space) until it or the main
/// window reports a focus change.
fn presenter_keys() -> [(Shortcut, Option<Control>); 7] {
    let key = |code| Shortcut::new(None, code);
    [
        // Ends the presentation (`None`).
        (key(Code::Escape), None),
        (key(Code::ArrowRight), Some(Control::Next)),
        // Presentation remotes send Page Down and Page Up.
        (key(Code::PageDown), Some(Control::Next)),
        (key(Code::ArrowLeft), Some(Control::Previous)),
        (key(Code::PageUp), Some(Control::Previous)),
        (key(Code::Space), Some(Control::TogglePause)),
        (key(Code::KeyB), Some(Control::ToggleBlackout)),
    ]
}

pub fn set_presenter_keys_enabled(app: &AppHandle, enabled: bool) {
    let shortcuts = app.global_shortcut();
    for (shortcut, _) in presenter_keys() {
        let registered = shortcuts.is_registered(shortcut);
        if enabled && !registered {
            let _ = shortcuts.register(shortcut);
        } else if !enabled && registered {
            let _ = shortcuts.unregister(shortcut);
        }
    }
}

/// Global shortcut handler for the keys of the presentation window.
pub fn handle_shortcut(app: &AppHandle, shortcut: &Shortcut, event: ShortcutEvent) {
    if event.state() != ShortcutState::Pressed {
        return;
    }
    let Some((_, control)) = presenter_keys()
        .into_iter()
        .find(|(key, _)| key == shortcut)
    else {
        return;
    };
    let state = app.state::<PresentationState>();
    match control {
        Some(control) => state.send(control),
        // With the management window on another screen, ending is confirmed
        // there. With one screen, the operator cannot see that window.
        None if state.confirms_end() => {
            let _ = app.emit(END_REQUESTED, ());
        }
        None => {
            let app = app.clone();
            tauri::async_runtime::spawn(async move {
                if let Some(active) = app.state::<PresentationState>().take_active() {
                    shut_down(active).await;
                    let _ = app.emit(ENDED, ());
                }
            });
        }
    }
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
    playback: Option<PlaybackStats>,
}

/// Where the presentation is, for the operator's screen.
#[derive(Default, Clone, Serialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct PositionInfo {
    before_start: bool,
    at_end: bool,
    current: Option<SlideId>,
    upcoming: Vec<SlideId>,
    paused: bool,
    blackout: bool,
}

impl From<Position> for PositionInfo {
    fn from(position: Position) -> Self {
        let Position {
            before_start,
            at_end,
            current,
            upcoming,
            paused,
            blackout,
        } = position;
        Self {
            before_start,
            at_end,
            current,
            upcoming,
            paused,
            blackout,
        }
    }
}

/// Slideshow measurements shown in the management window.
#[derive(Default, Clone, Serialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct PlaybackStats {
    position: PositionInfo,
    /// Time since the presentation window opened.
    elapsed_secs: f64,
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
    volume: f32,
    tracks_started: u64,
    tracks_failed: u64,
    underruns: u64,
    starved_transitions: u64,
    last_error: Option<String>,
}

impl MusicStats {
    fn of(bgm: &Bgm) -> Self {
        let stats: BgmStats = bgm.stats();
        Self {
            state: match stats.state {
                BgmState::Idle => "idle",
                BgmState::Playing => "playing",
                BgmState::Gap => "gap",
            },
            title: stats.now_playing.as_ref().map(|now| now.title.clone()),
            artist: stats.now_playing.and_then(|now| now.artist),
            volume: bgm.tuning().volume,
            tracks_started: stats.tracks_started,
            tracks_failed: stats.tracks_failed,
            underruns: stats.underruns,
            starved_transitions: stats.starved_transitions,
            last_error: stats.last_error,
        }
    }
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
    fn from_slideshow(show: &Slideshow, sound: &Sound, elapsed_secs: f64) -> Self {
        let SlideshowStats {
            current: _,
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
            position: show.position(UPCOMING).into(),
            elapsed_secs,
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
            bgm: sound.bgm.as_ref().map(MusicStats::of),
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
    /// Whether Esc in the presentation window asks the management window first.
    confirms_end: bool,
    stop: Arc<AtomicBool>,
    controls: mpsc::Sender<Control>,
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

    fn send(&self, control: Control) {
        if let Some(active) = self.active.lock().unwrap().as_ref() {
            let _ = active.controls.send(control);
        }
    }

    fn confirms_end(&self) -> bool {
        self.active
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|active| active.confirms_end)
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
pub fn control_presentation(state: State<'_, PresentationState>, control: Control) {
    state.send(control);
}

/// Opens the presentation of the open slideshow on monitor `monitor_index`.
/// It waits on the title card until [`Control::Start`].
#[tauri::command]
pub async fn start_presentation(
    app: AppHandle,
    state: State<'_, PresentationState>,
    editor: State<'_, EditorState>,
    settings: State<'_, SettingsState>,
    monitor_index: usize,
) -> Result<PresentationStatus, String> {
    let (show, media_dir) = {
        let session = editor.session();
        let guard = session.lock().unwrap();
        let session = guard.as_ref().ok_or("スライドショーが開かれていません。")?;
        (session.show().clone(), session.media_dir())
    };
    let sync_offset_ms = settings.get(&app).sync_offset_ms;
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
    // Any failure after this point must close the window and release the
    // keys, otherwise the "presenter" label stays taken until the app restarts.
    let playing = Playing {
        show,
        media_dir,
        sync_offset_ms,
    };
    match run_presentation(&app, &window, renderer, position, size, playing).await {
        Ok((stop, controls, stats, thread)) => {
            *state.active.lock().unwrap() = Some(Active {
                window,
                monitor_name: name,
                confirms_end: monitors.len() > 1,
                stop,
                controls,
                stats,
                thread: Some(thread),
            });
            Ok(state.status())
        }
        Err(err) => {
            set_presenter_keys_enabled(&app, false);
            let _ = window.destroy();
            Err(err)
        }
    }
}

/// What the presentation plays.
struct Playing {
    show: ShowDefinition,
    media_dir: PathBuf,
    sync_offset_ms: f64,
}

type RunningPresentation = (
    Arc<AtomicBool>,
    mpsc::Sender<Control>,
    Arc<Mutex<FrameStats>>,
    JoinHandle<()>,
);

/// Makes `window` fullscreen, attaches a surface and starts the render thread.
async fn run_presentation(
    app: &AppHandle,
    window: &tauri::Window,
    renderer: Arc<Renderer>,
    position: tauri::PhysicalPosition<i32>,
    size: tauri::PhysicalSize<u32>,
    playing: Playing,
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
            WindowEvent::Focused(focused) => set_presenter_keys_enabled(&app, *focused),
            WindowEvent::Resized(size) => {
                *pending_size.lock().unwrap() = Some((size.width, size.height));
            }
            WindowEvent::ScaleFactorChanged { new_inner_size, .. } => {
                *pending_size.lock().unwrap() = Some((new_inner_size.width, new_inner_size.height));
            }
            WindowEvent::Destroyed => {
                set_presenter_keys_enabled(&app, false);
                stop.store(true, Ordering::Relaxed);
            }
            _ => {}
        });
    }

    set_presenter_keys_enabled(app, true);

    let (controls, control_rx) = mpsc::channel();
    let thread = {
        let stop = stop.clone();
        let stats = stats.clone();
        std::thread::Builder::new()
            .name("fluxlide-presenter".into())
            .spawn(move || {
                render_loop(
                    renderer,
                    presenter,
                    playing,
                    control_rx,
                    stop,
                    stats,
                    pending_size,
                )
            })
            .map_err(|err| err.to_string())?
    };
    Ok((stop, controls, stats, thread))
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
        set_presenter_keys_enabled(active.window.app_handle(), false);
        let _ = active.window.destroy();
    }
}

async fn shut_down(mut active: Active) {
    active.stop.store(true, Ordering::Relaxed);
    if let Some(thread) = active.thread.take() {
        let _ = tauri::async_runtime::spawn_blocking(move || thread.join()).await;
    }
    set_presenter_keys_enabled(active.window.app_handle(), false);
    let _ = active.window.destroy();
}

fn render_loop(
    renderer: Arc<Renderer>,
    mut presenter: Presenter,
    playing: Playing,
    controls: mpsc::Receiver<Control>,
    stop: Arc<AtomicBool>,
    stats: Arc<Mutex<FrameStats>>,
    pending_size: Arc<Mutex<Option<(u32, u32)>>>,
) {
    let start = Instant::now();
    let title = playing.show.title.clone();
    let music = playing.show.music.clone();
    // Photos and videos are loaded at the display's full resolution.
    let mut show = Slideshow::new(
        renderer.clone(),
        playing.show,
        playing.media_dir,
        presenter.size(),
    );
    show.hold_start();
    let set_card = |show: &mut Slideshow, size| match Overlay::title_card(&renderer, &title, size) {
        Ok(card) => show.set_title_card(card),
        Err(err) => stats.lock().unwrap().last_error = Some(err.to_string()),
    };
    set_card(&mut show, presenter.size());
    // The output stays on this thread and shares the playback clock (`start`).
    let sound = open_sound(&mut show, start, &music, playing.sync_offset_ms);
    let mut last_frame: Option<Instant> = None;
    let mut window_start = start;
    let mut window_frames = 0u32;
    let mut window_worst = Duration::ZERO;
    let mut average_interval: Option<Duration> = None;

    while !stop.load(Ordering::Relaxed) {
        if let Some((width, height)) = pending_size.lock().unwrap().take() {
            presenter.resize(&renderer, width, height);
            if let Err(err) = show.set_output_size(presenter.size()) {
                stats.lock().unwrap().last_error = Some(err.to_string());
            }
            set_card(&mut show, presenter.size());
        }
        for control in controls.try_iter() {
            apply(control, &mut show, &sound);
        }

        let time = start.elapsed().as_secs_f64();
        show.update(time);
        let outcome = presenter.render(&renderer, &Scene::Frame(show.frame()));
        stats.lock().unwrap().playback = Some(PlaybackStats::from_slideshow(&show, &sound, time));

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

fn apply(control: Control, show: &mut Slideshow, sound: &Sound) {
    match control {
        Control::Start => show.start(),
        Control::Next => show.next(),
        Control::Previous => show.previous(),
        // Space on the title card starts the show, as → does.
        Control::TogglePause if show.position(0).before_start => show.start(),
        Control::TogglePause if show.is_paused() => show.resume(),
        Control::TogglePause => show.pause(),
        Control::ToggleBlackout => {
            let blackout = !show.position(0).blackout;
            show.set_blackout(blackout);
        }
        Control::NextTrack => {
            if let Some(bgm) = &sound.bgm {
                bgm.next();
            }
        }
        Control::MusicVolume { volume } => {
            if let Some(bgm) = &sound.bgm {
                let _ = bgm.set_volume(volume.clamp(0.0, 1.0));
            }
        }
        Control::SyncOffset { ms } => {
            if let Some(output) = &sound.output {
                let _ = output.mixer().set_sync_offset_ms(ms);
            }
        }
    }
    // The music pauses with the show: a pause is usually for talking. Next
    // and previous also end a pause.
    if let Some(bgm) = &sound.bgm {
        if show.is_paused() {
            bgm.pause();
        } else {
            bgm.resume();
        }
    }
}

/// The BGM settings of the show's music.
pub fn bgm_config(music: &fluxlide_engine::show::Music) -> BgmConfig {
    let secs = |ms: u32| f64::from(ms) / 1000.0;
    BgmConfig {
        shuffle: music.shuffle,
        seed: None,
        tuning: BgmTuning {
            volume: music.volume.clamp(0.0, 1.0),
            fade_secs: secs(music.fade_ms),
            gap_secs: secs(music.gap_ms),
            duck_fade_out_secs: secs(music.duck_fade_out_ms),
            duck_fade_in_secs: secs(music.duck_fade_in_ms),
            ..BgmTuning::default()
        },
    }
}

pub fn bgm_source(source: &MusicSource) -> BgmSource {
    match source {
        MusicSource::Folder { path } => BgmSource::Folder(PathBuf::from(path)),
        MusicSource::Files { paths } => BgmSource::Files(paths.iter().map(PathBuf::from).collect()),
    }
}

/// Opens the audio output for `show` and starts the show's music on it.
fn open_sound(
    show: &mut Slideshow,
    start: Instant,
    music: &fluxlide_engine::show::Music,
    sync_offset_ms: f64,
) -> Sound {
    let output = match AudioOutput::open_with_sync_offset(start, sync_offset_ms) {
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
    if let Some(source) = &music.source {
        let bgm = Ffmpeg::locate().and_then(|ffmpeg| {
            Bgm::start(
                ffmpeg,
                &output.mixer(),
                &bgm_source(source),
                bgm_config(music),
            )
        });
        match bgm {
            Ok(bgm) => {
                if music.duck_for_videos {
                    show.set_ducker(bgm.ducker());
                }
                sound.bgm = Some(bgm);
            }
            Err(err) => sound.bgm_error = Some(err.to_string()),
        }
    }
    sound.output = Some(output);
    sound
}
