//! "各画面に番号を表示": a large number in the middle of every screen for a
//! few seconds, so the operator can tell which screen is which when
//! choosing where to present (`docs/ui-draft.md`, C-2).

use std::time::Duration;

use tauri::{AppHandle, Manager, WebviewUrl, WebviewWindowBuilder};

/// How long the numbers stay.
const SHOWN_FOR: Duration = Duration::from_secs(3);
/// Size of each number's window, in logical pixels.
const SIZE: f64 = 320.0;

/// Shows screen `n` (from 1, in the order of `list_monitors`) on each screen.
#[tauri::command]
pub async fn identify_monitors(app: AppHandle) -> Result<(), String> {
    let monitors = app.available_monitors().map_err(|err| err.to_string())?;
    let mut windows = Vec::new();
    for (index, monitor) in monitors.iter().enumerate() {
        let label = format!("identify-{index}");
        if let Some(old) = app.get_webview_window(&label) {
            let _ = old.destroy();
        }
        let scale = monitor.scale_factor();
        let position = monitor.position();
        let size = monitor.size();
        // Logical position: physical / that screen's scale (as for the
        // presentation window on macOS), centred on the screen.
        let x = f64::from(position.x) / scale + (f64::from(size.width) / scale - SIZE) / 2.0;
        let y = f64::from(position.y) / scale + (f64::from(size.height) / scale - SIZE) / 2.0;
        let url = WebviewUrl::App(format!("identify?n={}", index + 1).into());
        let window = WebviewWindowBuilder::new(&app, &label, url)
            .title("Fluxlide")
            .decorations(false)
            .resizable(false)
            .skip_taskbar(true)
            .always_on_top(true)
            .focused(false)
            .position(x, y)
            .inner_size(SIZE, SIZE)
            .build()
            .map_err(|err| err.to_string())?;
        windows.push(window);
    }
    tauri::async_runtime::spawn(async move {
        sleep(SHOWN_FOR).await;
        for window in windows {
            let _ = window.destroy();
        }
    });
    Ok(())
}

async fn sleep(duration: Duration) {
    let _ = tauri::async_runtime::spawn_blocking(move || std::thread::sleep(duration)).await;
}
