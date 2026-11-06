mod editor;
mod export;
mod history;
mod identify;
mod import;
mod look;
mod loudness;
mod package;
mod presentation;
mod preview;
mod project;
mod session;
mod settings;

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

/// The folder of license texts in an installed app (none while developing).
#[tauri::command]
fn licenses_dir(app: tauri::AppHandle) -> Option<String> {
    app.path()
        .resolve("licenses", tauri::path::BaseDirectory::Resource)
        .ok()
        .filter(|dir| dir.is_dir())
        .map(|dir| dir.display().to_string())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        // A second start (a package double-clicked while the app is open)
        // hands its files to this one instead of opening another window.
        .plugin(tauri_plugin_single_instance::init(|app, args, cwd| {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.unminimize();
                let _ = window.set_focus();
            }
            // A relative path is relative to where the second start ran, not
            // to this process. An absolute one is kept as it is.
            let cwd = std::path::PathBuf::from(cwd);
            package::OpenedPackages::add(app, args.into_iter().skip(1).map(|arg| cwd.join(arg)));
        }))
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(presentation::handle_shortcut)
                .build(),
        )
        .manage(PresentationState::default())
        .manage(export::ExportState::default())
        .manage(editor::EditorState::default())
        .manage(preview::PreviewState::default())
        .manage(settings::SettingsState::default())
        .manage(package::PackageState::default())
        .manage(package::OpenedPackages::default())
        .setup(|app| {
            if let Ok(dir) = app.path().app_data_dir() {
                loudness::init(&dir);
            }
            // On Windows and Linux a double-clicked package comes as an argument.
            package::OpenedPackages::add(app.handle(), std::env::args_os().skip(1).map(Into::into));
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
                // Keys pressed in the management window belong to it.
                WindowEvent::Focused(true) => {
                    presentation::set_presenter_keys_enabled(window.app_handle(), false);
                }
                WindowEvent::Destroyed => {
                    let app = window.app_handle();
                    presentation::stop_blocking(&app.state::<PresentationState>());
                    app.state::<export::ExportState>().cancel_blocking();
                    app.exit(0);
                }
                _ => {}
            }
        })
        .invoke_handler(tauri::generate_handler![
            engine_status,
            video_decoder_status,
            licenses_dir,
            editor::list_projects,
            editor::create_project,
            editor::open_project,
            editor::close_project,
            editor::delete_project,
            editor::show_state,
            editor::edit_show,
            editor::undo_show,
            editor::redo_show,
            export::export_estimate,
            export::start_export,
            export::cancel_export,
            import::import_files,
            import::import_sample,
            import::replace_asset_file,
            identify::identify_monitors,
            presentation::list_monitors,
            presentation::presentation_status,
            presentation::presentation_frame,
            presentation::end_request_answered,
            presentation::control_presentation,
            presentation::start_presentation,
            presentation::start_test_sound,
            presentation::stop_test_sound,
            look::load_transition,
            look::music_tracks,
            presentation::stop_presentation,
            preview::start_preview,
            preview::stop_preview,
            preview::preview_frame,
            settings::get_settings,
            settings::set_sync_offset,
            settings::set_output_screen,
            package::package_estimate,
            package::export_package,
            package::import_package,
            package::cancel_package,
            package::take_opened_packages,
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|_app, _event| {
            // On macOS a double-clicked package comes as an event.
            #[cfg(target_os = "macos")]
            if let tauri::RunEvent::Opened { urls } = _event {
                package::OpenedPackages::add(
                    _app,
                    urls.into_iter().filter_map(|url| url.to_file_path().ok()),
                );
            }
        });
}
