//! Settings that belong to this computer rather than to a slideshow: the
//! sound delay of its audio output, the screen it presents on, and the
//! projects opened from other places. Kept as JSON in the app's config
//! folder.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager, State};
use ts_rs::TS;

const FILE: &str = "settings.json";
/// Projects opened from elsewhere that the home screen remembers.
const RECENT_LIMIT: usize = 20;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    /// "音ズレ補正": how much earlier the sound is sent, in milliseconds.
    pub sync_offset_ms: f64,
    /// The screen to present on without asking ("次回もこの画面を使う"),
    /// by its name.
    pub output_screen: Option<String>,
    /// Project folders outside the app's own folder, newest first.
    pub recent_dirs: Vec<PathBuf>,
}

impl Settings {
    /// Puts `dir` first among the recent projects.
    pub fn remember_dir(&mut self, dir: &Path) {
        self.recent_dirs.retain(|known| known != dir);
        self.recent_dirs.insert(0, dir.to_path_buf());
        self.recent_dirs.truncate(RECENT_LIMIT);
    }
}

#[derive(Default)]
pub struct SettingsState {
    settings: Mutex<Option<Settings>>,
}

impl SettingsState {
    /// The settings, read from disk the first time. Unreadable settings
    /// are replaced by the defaults.
    pub fn get(&self, app: &AppHandle) -> Settings {
        self.settings
            .lock()
            .unwrap()
            .get_or_insert_with(|| {
                path(app)
                    .and_then(|path| std::fs::read_to_string(path).ok())
                    .and_then(|json| serde_json::from_str(&json).ok())
                    .unwrap_or_default()
            })
            .clone()
    }

    /// Changes the settings and writes them to disk.
    pub fn update(
        &self,
        app: &AppHandle,
        change: impl FnOnce(&mut Settings),
    ) -> Result<Settings, String> {
        let mut settings = self.get(app);
        change(&mut settings);
        let path = path(app).ok_or("設定の保存先が見つかりません。")?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|err| err.to_string())?;
        }
        let json = serde_json::to_string_pretty(&settings).map_err(|err| err.to_string())?;
        std::fs::write(&path, json).map_err(|err| format!("設定を保存できません: {err}"))?;
        *self.settings.lock().unwrap() = Some(settings.clone());
        Ok(settings)
    }
}

fn path(app: &AppHandle) -> Option<PathBuf> {
    app.path().app_config_dir().ok().map(|dir| dir.join(FILE))
}

#[tauri::command]
pub fn get_settings(app: AppHandle, state: State<'_, SettingsState>) -> Settings {
    state.get(&app)
}

#[tauri::command]
pub fn set_sync_offset(
    app: AppHandle,
    state: State<'_, SettingsState>,
    offset_ms: f64,
) -> Result<Settings, String> {
    state.update(&app, |settings| settings.sync_offset_ms = offset_ms)
}

#[tauri::command]
pub fn set_output_screen(
    app: AppHandle,
    state: State<'_, SettingsState>,
    name: Option<String>,
) -> Result<Settings, String> {
    state.update(&app, |settings| settings.output_screen = name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reopened_project_moves_to_the_front_and_the_list_is_capped() {
        let mut settings = Settings::default();
        for n in 0..RECENT_LIMIT + 5 {
            settings.remember_dir(Path::new(&format!("/shows/{n}")));
        }
        settings.remember_dir(Path::new("/shows/10"));
        assert_eq!(settings.recent_dirs.len(), RECENT_LIMIT);
        assert_eq!(settings.recent_dirs[0], Path::new("/shows/10"));
        assert_eq!(
            settings
                .recent_dirs
                .iter()
                .filter(|dir| *dir == Path::new("/shows/10"))
                .count(),
            1
        );
    }

    #[test]
    fn missing_fields_take_their_defaults() {
        let settings: Settings = serde_json::from_str(r#"{"syncOffsetMs": 120}"#).unwrap();
        assert_eq!(settings.sync_offset_ms, 120.0);
        assert!(settings.output_screen.is_none() && settings.recent_dirs.is_empty());
    }
}
