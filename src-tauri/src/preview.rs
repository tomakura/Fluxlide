//! The preview in the management window.
//!
//! The webview cannot show a wgpu surface, so the preview is drawn by the
//! same compositor into an offscreen texture, read back as RGBA and handed
//! to the page as the binary result of a command. The page asks for the
//! next frame when it has drawn the last one (on a `<canvas>`), so a slow
//! webview lowers the frame rate instead of building up a queue.
//!
//! The preview plays the open slideshow on its own clock, silently. It
//! either plays from a slide or holds one slide still, fully visible, for
//! editing. When it is started again (after an edit, or on another slide),
//! the old one stays on screen until the new one shows its first slide, so
//! the preview does not flash black while editing.

use std::sync::{Arc, Mutex};
use std::time::Instant;

use fluxlide_engine::show::SlideId;
use fluxlide_engine::{OffscreenTarget, Renderer, Scene, Slideshow};
use tauri::ipc::Response;
use tauri::{AppHandle, Manager, State};

use crate::editor::EditorState;
use crate::presentation::PresentationState;

/// Largest preview, in pixels. The page asks for the size of its canvas.
const MAX_SIZE: (u32, u32) = (1920, 1080);

struct Run {
    show: Slideshow,
    start: Instant,
    /// Hold the first slide still once it is in.
    still: bool,
}

struct Preview {
    renderer: Arc<Renderer>,
    target: OffscreenTarget,
    shown: Option<Run>,
    /// Takes the place of `shown` once it has a slide on screen.
    next: Option<Run>,
}

#[derive(Default)]
pub struct PreviewState {
    preview: Arc<Mutex<Option<Preview>>>,
}

/// Starts (or restarts) the preview of the open slideshow at `width` x
/// `height`, from the slide `from` (the first if `None`). With `still`, that
/// slide is held fully visible instead of playing on.
#[tauri::command]
pub async fn start_preview(
    presentation: State<'_, PresentationState>,
    editor: State<'_, EditorState>,
    state: State<'_, PreviewState>,
    width: u32,
    height: u32,
    from: Option<SlideId>,
    still: bool,
) -> Result<(), String> {
    let (definition, media_dir) = {
        let session = editor.session();
        let guard = session.lock().unwrap();
        let session = guard.as_ref().ok_or("スライドショーが開かれていません。")?;
        (session.show().clone(), session.media_dir())
    };
    let renderer = presentation.renderer().await?;
    let size = (width.clamp(2, MAX_SIZE.0), height.clamp(2, MAX_SIZE.1));
    let index = from
        .and_then(|id| definition.slides.iter().position(|slide| slide.id == id))
        .unwrap_or(0);
    let mut show = Slideshow::new(renderer.clone(), definition, media_dir, size);
    show.start_at(index, still);
    let run = Run {
        show,
        start: Instant::now(),
        still,
    };
    let mut guard = state.preview.lock().unwrap();
    match guard
        .as_mut()
        .filter(|preview| preview.target.size() == size)
    {
        Some(preview) => preview.next = Some(run),
        None => {
            let target =
                OffscreenTarget::new(&renderer, size.0, size.1).map_err(|err| err.to_string())?;
            *guard = Some(Preview {
                renderer,
                target,
                shown: None,
                next: Some(run),
            });
        }
    }
    Ok(())
}

#[tauri::command]
pub fn stop_preview(state: State<'_, PreviewState>) {
    state.preview.lock().unwrap().take();
}

impl Run {
    fn update(&mut self) {
        self.show.update(self.start.elapsed().as_secs_f64());
        if self.still && !self.show.is_paused() && self.show.position(0).current.is_some() {
            self.show.pause();
        }
    }
}

/// Draws the frame due now. The result is the RGBA pixels (sRGB, rows
/// without padding) followed by the width, the height and the slide on
/// screen (0 for none) as little-endian `u32`s, so the page can wrap the
/// pixels without copying them.
#[tauri::command]
pub async fn preview_frame(app: AppHandle) -> Result<Response, String> {
    let preview = app.state::<PreviewState>().preview.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let mut guard = preview.lock().unwrap();
        let preview = guard.as_mut().ok_or("the preview is not running")?;
        if let Some(next) = &mut preview.next {
            next.update();
            let position = next.show.position(1);
            // A show with nothing to play replaces the old one at once.
            let nothing = position.current.is_none() && position.upcoming.is_empty();
            if position.current.is_some() || nothing || preview.shown.is_none() {
                preview.shown = preview.next.take();
            }
        }
        let Some(run) = preview.shown.as_mut() else {
            return Err("the preview is not running".to_string());
        };
        run.update();
        let slide = run.show.position(0).current.map_or(0, |id| id.0 as u32);
        preview
            .target
            .render(&preview.renderer, &Scene::Frame(run.show.frame()))
            .map_err(|err| err.to_string())?;
        let (width, height) = preview.target.size();
        let mut body = Vec::with_capacity(width as usize * height as usize * 4 + 12);
        preview
            .target
            .read_rgba_into(&preview.renderer, &mut body)
            .map_err(|err| err.to_string())?;
        body.extend_from_slice(&width.to_le_bytes());
        body.extend_from_slice(&height.to_le_bytes());
        body.extend_from_slice(&slide.to_le_bytes());
        Ok(Response::new(body))
    })
    .await
    .map_err(|err| err.to_string())?
}
