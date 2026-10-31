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

    /// Takes `dir` off the recent projects.
    pub fn forget_dir(&mut self, dir: &Path) {
        self.recent_dirs.retain(|known| known != dir);
    }
}

#[derive(Default)]
pub struct SettingsState {
    settings: Mutex<Option<Settings>>,
}

impl SettingsState {
    /// Only a missing file means first use. An unreadable or damaged file
    /// stays untouched until the user explicitly asks to reset it.
    pub fn get(&self, app: &AppHandle) -> Result<Settings, String> {
        Ok(loaded(&mut self.settings.lock().unwrap(), app)?.clone())
    }

    /// Changes the settings and writes them to disk. The lock is held
    /// throughout, so that two changes at once do not undo each other.
    pub fn update(
        &self,
        app: &AppHandle,
        change: impl FnOnce(&mut Settings),
    ) -> Result<Settings, String> {
        let mut slot = self.settings.lock().unwrap();
        let mut settings = loaded(&mut slot, app)?.clone();
        change(&mut settings);
        write_file(&path(app)?, &settings)?;
        *slot = Some(settings.clone());
        Ok(settings)
    }

    fn reset(&self, app: &AppHandle) -> Result<Settings, String> {
        let mut slot = self.settings.lock().unwrap();
        let settings = reset_file(&path(app)?)?;
        *slot = Some(settings.clone());
        Ok(settings)
    }
}

/// The settings, read from disk the first time.
fn loaded<'a>(slot: &'a mut Option<Settings>, app: &AppHandle) -> Result<&'a Settings, String> {
    if slot.is_none() {
        *slot = Some(read_file(&path(app)?)?);
    }
    Ok(slot.as_ref().expect("loaded above"))
}

fn read_file(path: &Path) -> Result<Settings, String> {
    match std::fs::read_to_string(path) {
        Ok(json) => serde_json::from_str(&json)
            .map_err(|err| format!("設定ファイルが壊れています（{}）: {err}", path.display())),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(Settings::default()),
        Err(err) => Err(format!("設定を読めません（{}）: {err}", path.display())),
    }
}

fn write_file(path: &Path, settings: &Settings) -> Result<(), String> {
    use std::io::Write;
    let parent = path.parent().ok_or("設定の保存先が見つかりません。")?;
    std::fs::create_dir_all(parent).map_err(|err| err.to_string())?;
    let json = serde_json::to_vec_pretty(settings).map_err(|err| err.to_string())?;
    let mut temp = tempfile::NamedTempFile::new_in(parent).map_err(|err| err.to_string())?;
    temp.write_all(&json).map_err(|err| err.to_string())?;
    temp.persist(path)
        .map_err(|err| format!("設定を保存できません: {err}"))?;
    Ok(())
}

fn reset_file(path: &Path) -> Result<Settings, String> {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|err| err.to_string())?
        .as_nanos();
    match std::fs::copy(path, path.with_extension(format!("json.backup-{stamp}"))) {
        Ok(_) => {}
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => return Err(format!("元の設定を退避できません: {err}")),
    }
    let settings = Settings::default();
    write_file(path, &settings)?;
    Ok(settings)
}

fn path(app: &AppHandle) -> Result<PathBuf, String> {
    app.path()
        .app_config_dir()
        .map(|dir| dir.join(FILE))
        .map_err(|err| err.to_string())
}

#[tauri::command]
pub fn get_settings(app: AppHandle, state: State<'_, SettingsState>) -> Result<Settings, String> {
    state.get(&app)
}

#[tauri::command]
pub fn reset_settings(app: AppHandle, state: State<'_, SettingsState>) -> Result<Settings, String> {
    state.reset(&app)
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
    fn damaged_settings_are_reported_and_reset_keeps_the_original() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(FILE);
        assert_eq!(read_file(&path).unwrap(), Settings::default());
        std::fs::write(&path, b"broken settings").unwrap();
        assert!(read_file(&path).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"broken settings");
        reset_file(&path).unwrap();
        assert_eq!(read_file(&path).unwrap(), Settings::default());
        let backup = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().path())
            .find(|p| p != &path)
            .unwrap();
        assert_eq!(std::fs::read(backup).unwrap(), b"broken settings");
        assert!(
            read_file(dir.path()).is_err(),
            "an unreadable file is not first use"
        );
    }

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
    fn a_forgotten_project_leaves_the_recent_ones_and_others_stay() {
        let mut settings = Settings::default();
        settings.remember_dir(Path::new("/shows/1"));
        settings.remember_dir(Path::new("/shows/2"));
        settings.forget_dir(Path::new("/shows/1"));
        settings.forget_dir(Path::new("/shows/3"));
        assert_eq!(settings.recent_dirs, [Path::new("/shows/2")]);
    }

    #[test]
    fn missing_fields_take_their_defaults() {
        let settings: Settings = serde_json::from_str(r#"{"syncOffsetMs": 120}"#).unwrap();
        assert_eq!(settings.sync_offset_ms, 120.0);
        assert!(settings.output_screen.is_none() && settings.recent_dirs.is_empty());
    }
}
