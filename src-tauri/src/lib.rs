mod editor;
mod history;
mod import;
mod presentation;
mod preview;
mod project;
mod session;

use fluxlide_engine::media::video::Ffmpeg;
use presentation::PresentationState;
use serde::Serialize;
use tauri::{Manager, State, WindowEvent};
use ts_rs::TS;

/// GPU details shown in the UI so users can confirm the renderer started.
#[derive(Serialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
struct EngineStatus {
    gpu_name: String,
    backend: String,
    device_type: String,
}

#[tauri::command]
async fn engine_status(state: State<'_, PresentationState>) -> Result<EngineStatus, String> {
    let info = state.renderer().await?.info().clone();
    Ok(EngineStatus {
        gpu_name: info.name,
        backend: info.backend,
        device_type: info.device_type,
    })
}

/// Whether the FFmpeg tools used for video playback were found.
#[derive(Serialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
struct VideoDecoderStatus {
    path: Option<String>,
    version: Option<String>,
    error: Option<String>,
}

#[tauri::command]
async fn video_decoder_status() -> VideoDecoderStatus {
    let status = tauri::async_runtime::spawn_blocking(|| {
        let ffmpeg = Ffmpeg::locate()?;
        let version = ffmpeg.version()?;
        Ok::<_, fluxlide_engine::EngineError>((ffmpeg.ffmpeg_path().display().to_string(), version))
    })
    .await;
    match status {
        Ok(Ok((path, version))) => VideoDecoderStatus {
            path: Some(path),
            version: Some(version),
            error: None,
        },
        Ok(Err(err)) => VideoDecoderStatus {
            path: None,
            version: None,
            error: Some(err.to_string()),
        },
        Err(err) => VideoDecoderStatus {
            path: None,
            version: None,
            error: Some(err.to_string()),
        },
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(presentation::handle_shortcut)
                .build(),
        )
        .manage(PresentationState::default())
        .manage(editor::EditorState::default())
        .manage(preview::PreviewState::default())
        .setup(|app| {
            let session = app.state::<editor::EditorState>().session();
            app.manage(import::Importer::start(app.handle().clone(), session));
            Ok(())
        })
        .on_window_event(|window, event| {
            // Closing the management window ends the app, including any presentation.
            if window.label() != "main" {
                return;
            }
            match event {
                // Esc in the management window must not end the presentation.
                WindowEvent::Focused(true) => {
                    presentation::set_escape_enabled(window.app_handle(), false);
                }
                WindowEvent::Destroyed => {
                    let app = window.app_handle();
                    presentation::stop_blocking(&app.state::<PresentationState>());
                    app.exit(0);
                }
                _ => {}
            }
        })
        .invoke_handler(tauri::generate_handler![
            engine_status,
            video_decoder_status,
            editor::list_projects,
            editor::create_project,
            editor::open_project,
            editor::close_project,
            editor::show_state,
            editor::edit_show,
            editor::undo_show,
            editor::redo_show,
            import::import_files,
            import::replace_asset_file,
            presentation::list_monitors,
            presentation::media_assets,
            presentation::presentation_status,
            presentation::start_presentation,
            presentation::stop_presentation,
            preview::start_preview,
            preview::stop_preview,
            preview::preview_frame,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
