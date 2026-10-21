//! Commands for opening, editing and undoing the slideshow in the
//! management window. The engine side owns the definition: the window sends
//! each edit as a whole new definition and draws whatever comes back.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use fluxlide_engine::show::ShowDefinition;
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State};

use crate::import;
use crate::project::{self, Project};
use crate::session::Session;

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
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ShowState {
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

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectSummary {
    dir: PathBuf,
    title: String,
    slide_count: usize,
    updated_at: i64,
    /// The thumbnail of the first slide, if the show has slides.
    thumbnail: Option<PathBuf>,
}

impl From<project::Summary> for ProjectSummary {
    fn from(summary: project::Summary) -> Self {
        let media_dir = summary.dir.join(project::MEDIA_DIR);
        Self {
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

#[tauri::command]
pub async fn list_projects(app: AppHandle) -> Result<Vec<ProjectSummary>, String> {
    let root = projects_dir(&app)?;
    let summaries = tauri::async_runtime::spawn_blocking(move || project::list(&root))
        .await
        .map_err(|err| err.to_string())?;
    let scope = app.asset_protocol_scope();
    Ok(summaries
        .into_iter()
        .map(ProjectSummary::from)
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
        let project = Project::create(&root, &show).map_err(|err| err.to_string())?;
        Ok(replace(&app, &session, Session::new(project, show)))
    })
    .await
    .map_err(|err| err.to_string())?
}

#[tauri::command]
pub async fn open_project(
    app: AppHandle,
    state: State<'_, EditorState>,
    dir: PathBuf,
) -> Result<ShowState, String> {
    let session = state.session();
    tauri::async_runtime::spawn_blocking(move || {
        let (project, show) = Project::open(&dir).map_err(|err| err.to_string())?;
        Ok(replace(&app, &session, Session::new(project, show)))
    })
    .await
    .map_err(|err| err.to_string())?
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
    state.session.lock().unwrap().take();
}

#[tauri::command]
pub fn show_state(state: State<'_, EditorState>) -> Option<ShowState> {
    state.session.lock().unwrap().as_ref().map(ShowState::of)
}

/// Replaces the definition with `show`. `group` names the control being
/// changed, so that typing or dragging in it is undone in one step.
#[tauri::command]
pub fn edit_show(
    state: State<'_, EditorState>,
    show: ShowDefinition,
    group: Option<String>,
) -> Result<ShowState, String> {
    with_session(&state, |session| {
        session.edit(show, group.as_deref());
    })
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
