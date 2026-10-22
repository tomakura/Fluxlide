// Typed calls to the commands and events of the Rust side
// (src-tauri/src/lib.rs). The types in ./bindings are generated from the
// Rust types by `cargo test`; see docs/ipc.md.

import { convertFileSrc, invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type { AssetId } from "./bindings/AssetId";
import type { Control } from "./bindings/Control";
import type { EditOutcome } from "./bindings/EditOutcome";
import type { ImportProgress } from "./bindings/ImportProgress";
import type { ImportResult } from "./bindings/ImportResult";
import type { LoadedTransition } from "./bindings/LoadedTransition";
import type { MonitorInfo } from "./bindings/MonitorInfo";
import type { MusicSource } from "./bindings/MusicSource";
import type { PresentationStatus } from "./bindings/PresentationStatus";
import type { ProjectSummary } from "./bindings/ProjectSummary";
import type { Settings } from "./bindings/Settings";
import type { ShowDefinition } from "./bindings/ShowDefinition";
import type { ShowState } from "./bindings/ShowState";
import type { SlideId } from "./bindings/SlideId";

export const listProjects = () => invoke<ProjectSummary[]>("list_projects");
export const createProject = (title: string) => invoke<ShowState>("create_project", { title });
export const openProject = (dir: string) => invoke<ShowState>("open_project", { dir });
export const closeProject = () => invoke<void>("close_project");
export const showState = () => invoke<ShowState | null>("show_state");

/// `show` is a change made to the state of `revision`. `group` names the
/// control being changed, so that typing or dragging in it is undone in one
/// step. An edit made from an older revision is not applied.
export const editShow = (show: ShowDefinition, session: number, revision: number, group: string | null = null) =>
  invoke<EditOutcome>("edit_show", { show, group, session, revision });
export const undoShow = () => invoke<ShowState>("undo_show");
export const redoShow = () => invoke<ShowState>("redo_show");

export const importFiles = (files: string[], addSlides = true) =>
  invoke<ImportResult>("import_files", { files, addSlides });
export const replaceAssetFile = (asset: AssetId, file: string) =>
  invoke<ShowState>("replace_asset_file", { asset, file });

export const listMonitors = () => invoke<MonitorInfo[]>("list_monitors");
/// Shows each screen's number on it for a few seconds.
export const identifyMonitors = () => invoke<void>("identify_monitors");
export const startPresentation = (monitorIndex: number) =>
  invoke<PresentationStatus>("start_presentation", { monitorIndex });
export const stopPresentation = () => invoke<PresentationStatus>("stop_presentation");
/// Flashes and beeps on a screen for setting the sound delay by ear. Ended
/// by `stopTestSound`, which leaves a show started since alone.
export const startTestSound = (monitorIndex: number) =>
  invoke<PresentationStatus>("start_test_sound", { monitorIndex });
export const stopTestSound = () => invoke<PresentationStatus>("stop_test_sound");
export const presentationStatus = () => invoke<PresentationStatus>("presentation_status");
export const controlPresentation = (control: Control) =>
  invoke<void>("control_presentation", { control });

/// Plays the open show from slide `from`, or holds it still with `still`.
export const startPreview = (width: number, height: number, from: SlideId | null, still: boolean) =>
  invoke<void>("start_preview", { width, height, from, still });
export const stopPreview = () => invoke<void>("stop_preview");
/// RGBA pixels followed by the width, the height and the slide on screen
/// (0 for none) as little-endian u32.
export const previewFrame = () => invoke<ArrayBuffer>("preview_frame");

/// Reads a WGSL transition and checks that it compiles.
export const loadTransition = (path: string) => invoke<LoadedTransition>("load_transition", { path });
/// The names of the music files `source` plays.
export const musicTracks = (source: MusicSource) => invoke<string[]>("music_tracks", { source });

export const getSettings = () => invoke<Settings>("get_settings");
export const setSyncOffset = (offsetMs: number) => invoke<Settings>("set_sync_offset", { offsetMs });
export const setOutputScreen = (name: string | null) =>
  invoke<Settings>("set_output_screen", { name });

/// The definition changed other than in answer to a command.
export const onShowChanged = (handler: (state: ShowState) => void): Promise<UnlistenFn> =>
  listen<ShowState>("show-changed", (event) => handler(event.payload));
export const onImportProgress = (handler: (progress: ImportProgress) => void): Promise<UnlistenFn> =>
  listen<ImportProgress>("import-progress", (event) => handler(event.payload));
/// Esc was pressed in the presentation window; the operator confirms here.
export const onPresentationEndRequested = (handler: () => void): Promise<UnlistenFn> =>
  listen("presentation-end-requested", () => handler());
/// The presentation ended from its own window.
export const onPresentationEnded = (handler: () => void): Promise<UnlistenFn> =>
  listen("presentation-ended", () => handler());

/// A URL the page can load a file of the open project (or a listed
/// thumbnail) from.
export const fileUrl = (path: string) => convertFileSrc(path);
