use serde::Serialize;

/// GPU details shown in the UI so users can confirm the renderer started.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct EngineStatus {
    gpu_name: String,
    backend: String,
    device_type: String,
}

#[tauri::command]
async fn engine_status() -> Result<EngineStatus, String> {
    let renderer = fluxlide_engine::Renderer::new()
        .await
        .map_err(|err| err.to_string())?;
    let info = renderer.info().clone();
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
        .invoke_handler(tauri::generate_handler![engine_status])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
