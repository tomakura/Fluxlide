// Typed calls to the commands and events of the Rust side
// (src-tauri/src/lib.rs). The types in ./bindings are generated from the
// Rust types by `cargo test`; see docs/ipc.md.

import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type { AssetId } from "./bindings/AssetId";
import type { ImportProgress } from "./bindings/ImportProgress";
import type { ImportResult } from "./bindings/ImportResult";
import type { ProjectSummary } from "./bindings/ProjectSummary";
import type { ShowDefinition } from "./bindings/ShowDefinition";
import type { ShowState } from "./bindings/ShowState";

export const listProjects = () => invoke<ProjectSummary[]>("list_projects");
export const createProject = (title: string) => invoke<ShowState>("create_project", { title });
export const openProject = (dir: string) => invoke<ShowState>("open_project", { dir });
export const closeProject = () => invoke<void>("close_project");
export const showState = () => invoke<ShowState | null>("show_state");

/// `group` names the control being changed, so that typing or dragging in
/// it is undone in one step.
export const editShow = (show: ShowDefinition, group: string | null = null) =>
  invoke<ShowState>("edit_show", { show, group });
export const undoShow = () => invoke<ShowState>("undo_show");
export const redoShow = () => invoke<ShowState>("redo_show");

export const importFiles = (files: string[], addSlides = true) =>
  invoke<ImportResult>("import_files", { files, addSlides });
export const replaceAssetFile = (asset: AssetId, file: string) =>
  invoke<ShowState>("replace_asset_file", { asset, file });

/// The definition changed other than in answer to a command.
export const onShowChanged = (handler: (state: ShowState) => void): Promise<UnlistenFn> =>
  listen<ShowState>("show-changed", (event) => handler(event.payload));
export const onImportProgress = (handler: (progress: ImportProgress) => void): Promise<UnlistenFn> =>
  listen<ImportProgress>("import-progress", (event) => handler(event.payload));
