//! Commands for the 見た目 and 音楽 tabs that need the engine: reading a
//! custom transition (and checking that the GPU accepts it) and listing the
//! music a show will play.

use std::path::PathBuf;

use fluxlide_engine::Transition;
use fluxlide_engine::show::MusicSource;
use serde::Serialize;
use tauri::State;
use ts_rs::TS;

use crate::presentation::{PresentationState, bgm_source};

#[derive(Serialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct LoadedTransition {
    /// The file name without its extension.
    name: String,
    wgsl: String,
}

/// Reads the WGSL transition in `path` and compiles it, so that a shader
/// with a mistake is refused with the compiler's message when it is added
/// rather than when the show plays.
#[tauri::command]
pub async fn load_transition(
    presentation: State<'_, PresentationState>,
    path: PathBuf,
) -> Result<LoadedTransition, String> {
    let renderer = presentation.renderer().await?;
    tauri::async_runtime::spawn_blocking(move || {
        let wgsl =
            std::fs::read_to_string(&path).map_err(|err| format!("ファイルを読めません: {err}"))?;
        let name = path
            .file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
            .unwrap_or_else(|| "カスタム".into());
        Transition::from_wgsl(&renderer, &name, &wgsl).map_err(|err| err.to_string())?;
        Ok(LoadedTransition { name, wgsl })
    })
    .await
    .map_err(|err| err.to_string())?
}

/// The names of the music files `source` plays, in order before shuffling.
#[tauri::command]
pub async fn music_tracks(source: MusicSource) -> Result<Vec<String>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let tracks = bgm_source(&source)
            .tracks()
            .map_err(|err| err.to_string())?;
        Ok(tracks
            .iter()
            .map(|path| {
                path.file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_default()
            })
            .collect())
    })
    .await
    .map_err(|err| err.to_string())?
}
