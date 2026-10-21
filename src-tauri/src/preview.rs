//! Prototype of the preview in the management window.
//!
//! The webview cannot show a wgpu surface, so the preview is drawn by the
//! same compositor into an offscreen texture, read back as RGBA and handed
//! to the page as the binary result of a command. The page asks for the
//! next frame when it has drawn the last one (on a `<canvas>`), so a slow
//! webview lowers the frame rate instead of building up a queue.
//!
//! The preview runs its own slideshow on its own clock and is silent.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use fluxlide_engine::{OffscreenTarget, Renderer, Scene, Slideshow};
use tauri::ipc::Response;
use tauri::{AppHandle, Manager, State};

use crate::presentation::{PresentationState, build_slideshow};

/// Largest preview, in pixels. The page asks for the size of its canvas.
const MAX_SIZE: (u32, u32) = (1920, 1080);

struct Preview {
    renderer: Arc<Renderer>,
    show: Option<Slideshow>,
    target: OffscreenTarget,
    start: Instant,
}

#[derive(Default)]
pub struct PreviewState {
    preview: Arc<Mutex<Option<Preview>>>,
}

/// Starts (or restarts) the preview of `files` at `width` x `height`. With
/// no files it shows the test pattern.
#[tauri::command]
pub async fn start_preview(
    presentation: State<'_, PresentationState>,
    state: State<'_, PreviewState>,
    files: Vec<PathBuf>,
    width: u32,
    height: u32,
) -> Result<(), String> {
    let renderer = presentation.renderer().await?;
    let size = (width.clamp(2, MAX_SIZE.0), height.clamp(2, MAX_SIZE.1));
    let target = OffscreenTarget::new(&renderer, size.0, size.1).map_err(|err| err.to_string())?;
    let show = (!files.is_empty()).then(|| build_slideshow(renderer.clone(), files, size));
    *state.preview.lock().unwrap() = Some(Preview {
        renderer,
        show,
        target,
        start: Instant::now(),
    });
    Ok(())
}

#[tauri::command]
pub fn stop_preview(state: State<'_, PreviewState>) {
    state.preview.lock().unwrap().take();
}

/// Draws the frame due now. The result is the RGBA pixels (sRGB, rows
/// without padding) followed by the width and the height as little-endian
/// `u32`s, so the page can wrap the pixels without copying them.
#[tauri::command]
pub async fn preview_frame(app: AppHandle) -> Result<Response, String> {
    let preview = app.state::<PreviewState>().preview.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let mut guard = preview.lock().unwrap();
        let preview = guard.as_mut().ok_or("the preview is not running")?;
        let time = preview.start.elapsed().as_secs_f64();
        let renderer = &preview.renderer;
        let scene = match preview.show.as_mut() {
            Some(show) => {
                show.update(time);
                Scene::Frame(show.frame())
            }
            None => Scene::TestPattern {
                time_secs: time as f32,
            },
        };
        preview
            .target
            .render(renderer, &scene)
            .map_err(|err| err.to_string())?;
        drop(scene);
        let (width, height) = preview.target.size();
        let mut body = Vec::with_capacity(width as usize * height as usize * 4 + 8);
        preview
            .target
            .read_rgba_into(renderer, &mut body)
            .map_err(|err| err.to_string())?;
        body.extend_from_slice(&width.to_le_bytes());
        body.extend_from_slice(&height.to_le_bytes());
        Ok(Response::new(body))
    })
    .await
    .map_err(|err| err.to_string())?
}
