//! Commands for opening, editing and undoing the slideshow in the
//! management window. The engine side owns the definition: the window sends
//! each edit as a whole new definition and draws whatever comes back.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use fluxlide_engine::show::ShowDefinition;
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State};
use ts_rs::TS;

use crate::import;
use crate::project::{self, Project};
use crate::session::Session;
use crate::settings::SettingsState;

/// Sent with the current [`EditorState`] whenever the definition changes
/// other than in answer to a command (an import finishing, for example).
pub const SHOW_CHANGED: &str = "show-changed";

#[derive(Default)]
pub struct EditorState {
    session: Arc<Mutex<Option<Session>>>,
}

impl EditorState {
    pub fn session(&self) -> Arc<Mutex<Option<Session>>> {
        self.session.clone()
    }
}

/// What the management window needs to draw the open slideshow.
#[derive(Clone, Serialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct ShowState {
    /// Sent back with an edit, so that one made from an older state is
    /// refused (see [`edit_show`]).
    revision: u64,
    /// Different for every time a project is opened; revisions count from
    /// the start in each.
    session: u64,
    dir: PathBuf,
    media_dir: PathBuf,
    show: ShowDefinition,
    can_undo: bool,
    can_redo: bool,
    save_error: Option<String>,
}

impl ShowState {
    pub fn of(session: &Session) -> Self {
        Self {
            revision: session.revision(),
            session: session.epoch(),
            dir: session.dir().to_path_buf(),
            media_dir: session.media_dir(),
            show: session.show().clone(),
            can_undo: session.can_undo(),
            can_redo: session.can_redo(),
            save_error: session.save_error().map(str::to_string),
        }
    }
}

/// Tells the window about a change it did not ask for.
pub fn notify(app: &AppHandle, session: &Session) {
    let _ = app.emit(SHOW_CHANGED, ShowState::of(session));
}

#[derive(Serialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct ProjectSummary {
    dir: PathBuf,
    title: String,
    slide_count: usize,
    updated_at: i64,
    /// The thumbnail of the first slide, if the show has slides.
    thumbnail: Option<PathBuf>,
    /// Kept in the app's own folder, so that deleting it removes the files.
    in_app: bool,
}

impl ProjectSummary {
    fn new(summary: project::Summary, root: &std::path::Path) -> Self {
        let in_app = summary.dir.parent() == Some(root);
        Self {
            in_app,
            ..Self::from(summary)
        }
    }
}

impl From<project::Summary> for ProjectSummary {
    fn from(summary: project::Summary) -> Self {
        let media_dir = summary.dir.join(project::MEDIA_DIR);
        Self {
            in_app: false,
            thumbnail: summary
                .first_file
                .map(|file| import::thumbnail_path(&media_dir, &file)),
            dir: summary.dir,
            title: summary.title,
            slide_count: summary.slide_count,
            updated_at: summary.updated_at,
        }
    }
}

/// The folder new projects are made in.
pub fn projects_dir(app: &AppHandle) -> Result<PathBuf, String> {
    app.path()
        .app_data_dir()
        .map(|dir| dir.join("projects"))
        .map_err(|err| err.to_string())
}

/// The projects in the app's folder and those opened from elsewhere, most
/// recently changed first.
#[tauri::command]
pub async fn list_projects(
    app: AppHandle,
    settings: State<'_, SettingsState>,
) -> Result<Vec<ProjectSummary>, String> {
    let root = projects_dir(&app)?;
    let elsewhere = settings.get(&app).recent_dirs;
    let scope_root = root.clone();
    let summaries = tauri::async_runtime::spawn_blocking(move || {
        let mut found = project::list(&root);
        // A folder that was moved or deleted since is left out.
        found.extend(
            elsewhere
                .iter()
                .filter(|dir| !dir.starts_with(&root))
                .filter_map(|dir| project::summary(dir).ok()),
        );
        found.sort_by_key(|summary| std::cmp::Reverse(summary.updated_at));
        found.dedup_by(|a, b| a.dir == b.dir);
        found
    })
    .await
    .map_err(|err| err.to_string())?;
    let scope = app.asset_protocol_scope();
    Ok(summaries
        .into_iter()
        .map(|summary| ProjectSummary::new(summary, &scope_root))
        .inspect(|summary| {
            if let Some(thumbnail) = &summary.thumbnail {
                let _ = scope.allow_file(thumbnail);
            }
        })
        .collect())
}

#[tauri::command]
pub async fn create_project(
    app: AppHandle,
    state: State<'_, EditorState>,
    title: String,
) -> Result<ShowState, String> {
    let root = projects_dir(&app)?;
    let show = ShowDefinition {
        title,
        ..ShowDefinition::default()
    };
    let session = state.session();
    tauri::async_runtime::spawn_blocking(move || {
        close(&session);
        let project = Project::create(&root, &show).map_err(|err| err.to_string())?;
        Ok(replace(&app, &session, Session::new(project, show)))
    })
    .await
    .map_err(|err| err.to_string())?
}

/// Opens the project in `dir`. One outside the app's folder ("ほかの場所から
/// 開く") is remembered for the home screen.
#[tauri::command]
pub async fn open_project(
    app: AppHandle,
    state: State<'_, EditorState>,
    settings: State<'_, SettingsState>,
    dir: PathBuf,
) -> Result<ShowState, String> {
    let elsewhere = !dir.starts_with(projects_dir(&app)?);
    let session = state.session();
    let opened = {
        let app = app.clone();
        let dir = dir.clone();
        tauri::async_runtime::spawn_blocking(move || {
            // Imports of the session being closed stop before the media folder
            // is cleaned, even when it is the same project opened again.
            close(&session);
            let (project, show) = Project::open(&dir).map_err(|err| err.to_string())?;
            Ok(replace(&app, &session, Session::new(project, show)))
        })
        .await
        .map_err(|err| err.to_string())?
    };
    // Only a project that opened is remembered, and the project is open
    // even if the settings cannot be written.
    if opened.is_ok() && elsewhere {
        let _ = settings.update(&app, |settings| settings.remember_dir(&dir));
    }
    opened
}

/// Deletes the project in `dir`. One in the app's folder is removed with
/// its photos; one opened from elsewhere is only taken off the home screen,
/// since its folder is the user's own.
#[tauri::command]
pub async fn delete_project(
    app: AppHandle,
    state: State<'_, EditorState>,
    settings: State<'_, SettingsState>,
    dir: PathBuf,
) -> Result<(), String> {
    let root = projects_dir(&app)?;
    let session = state.session();
    if !is_project_folder_of(&root, &dir) {
        return settings
            .update(&app, |settings| settings.forget_dir(&dir))
            .map(|_| ());
    }
    tauri::async_runtime::spawn_blocking(move || {
        // Stops the imports of the open project, which may be this one, and
        // waits for the one being copied to give up.
        close(&session);
        let importer = app.state::<import::Importer>();
        let _idle = importer.pause();
        if !dir.join(project::DATABASE_FILE).is_file() {
            return Err(project::ProjectError::NotFound(dir).to_string());
        }
        remove_folder(&dir).map_err(|err| format!("スライドショーを削除できません: {err}"))
    })
    .await
    .map_err(|err| err.to_string())?
}

/// Whether `dir` is a folder made directly inside the app's folder `root`.
/// A path ending in `..` has `root` as its parent too, but is not one.
fn is_project_folder_of(root: &std::path::Path, dir: &std::path::Path) -> bool {
    dir.parent() == Some(root) && dir.file_name().is_some()
}

/// Removes `dir` until it is gone, since a file in it may stay open for a
/// moment (a thumbnail being shown, say).
fn remove_folder(dir: &std::path::Path) -> std::io::Result<()> {
    let mut last = Ok(());
    for _ in 0..30 {
        last = std::fs::remove_dir_all(dir);
        if last.is_ok() && !dir.exists() {
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    last.and_then(|()| Err(std::io::Error::other("フォルダーが残っています")))
}

fn close(slot: &Mutex<Option<Session>>) {
    slot.lock().unwrap().take();
}

/// Makes `session` the open one. The window may then show its media files.
fn replace(app: &AppHandle, slot: &Mutex<Option<Session>>, session: Session) -> ShowState {
    let _ = app
        .asset_protocol_scope()
        .allow_directory(session.media_dir(), true);
    let state = ShowState::of(&session);
    *slot.lock().unwrap() = Some(session);
    state
}

#[tauri::command]
pub fn close_project(state: State<'_, EditorState>) {
    close(&state.session);
}

#[tauri::command]
pub fn show_state(state: State<'_, EditorState>) -> Option<ShowState> {
    state.session.lock().unwrap().as_ref().map(ShowState::of)
}

#[derive(Serialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct EditOutcome {
    state: ShowState,
    /// False when the edit was made from an older revision than the
    /// current one. The window then makes its change again on `state`.
    applied: bool,
}

/// Replaces the definition with `show`, made from the state of `revision`.
/// `group` names the control being changed, so that typing or dragging in
/// it is undone in one step.
#[tauri::command]
pub fn edit_show(
    state: State<'_, EditorState>,
    show: ShowDefinition,
    group: Option<String>,
    session: u64,
    revision: u64,
) -> Result<EditOutcome, String> {
    let mut applied = false;
    let state = with_session(&state, |open| {
        if open.epoch() == session && open.revision() == revision {
            open.edit(show, group.as_deref());
            applied = true;
        }
    })?;
    Ok(EditOutcome { state, applied })
}

#[tauri::command]
pub fn undo_show(state: State<'_, EditorState>) -> Result<ShowState, String> {
    with_session(&state, |session| {
        session.undo();
    })
}

#[tauri::command]
pub fn redo_show(state: State<'_, EditorState>) -> Result<ShowState, String> {
    with_session(&state, |session| {
        session.redo();
    })
}

fn with_session(
    state: &EditorState,
    change: impl FnOnce(&mut Session),
) -> Result<ShowState, String> {
    let mut guard = state.session.lock().unwrap();
    let session = guard.as_mut().ok_or("スライドショーが開かれていません。")?;
    change(session);
    Ok(ShowState::of(session))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn only_a_folder_made_directly_in_the_app_folder_is_removed() {
        let root = Path::new("/data/projects");
        assert!(is_project_folder_of(
            root,
            Path::new("/data/projects/show-1")
        ));
        assert!(!is_project_folder_of(root, Path::new("/data/projects/..")));
        assert!(!is_project_folder_of(root, Path::new("/data/projects")));
        assert!(!is_project_folder_of(
            root,
            Path::new("/data/projects/show-1/media")
        ));
        assert!(!is_project_folder_of(root, Path::new("/elsewhere/show-1")));
    }
}
