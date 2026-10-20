mod presentation;

use presentation::PresentationState;
use serde::Serialize;
use tauri::{Manager, State, WindowEvent};

/// GPU details shown in the UI so users can confirm the renderer started.
#[derive(Serialize)]
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

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(presentation::handle_shortcut)
                .build(),
        )
        .manage(PresentationState::default())
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
            presentation::list_monitors,
            presentation::presentation_status,
            presentation::start_presentation,
            presentation::stop_presentation,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
