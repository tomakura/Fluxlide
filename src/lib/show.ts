// The parts of the engine's `ShowDefinition` (crates/fluxlide-engine/src/show.rs)
// that the management window sends. Fields left out take the engine's
// defaults. Written by hand until the types are generated from Rust
// (stage 2, step 4 of docs/roadmap.md).

import { invoke } from "@tauri-apps/api/core";

export type AssetState =
  | { state: "importing" }
  | { state: "ready" }
  | { state: "failed"; reason: string };

export type Asset = {
  id: number;
  kind: "photo" | "video";
  file: string;
  originalName: string;
  state: AssetState;
  meta: { title: string; author: string; description: string };
  size: [number, number] | null;
  durationMs: number | null;
  hasAudio: boolean;
};

export type Slide = {
  id: number;
  asset: number;
};

export type ShowDefinition = {
  assets: Asset[];
  slides: Slide[];
};

// A show of `files`, one slide each in order. Slide `n` (from 1) shows file `n`.
export async function showOfFiles(files: string[]): Promise<ShowDefinition> {
  const assets = await invoke<Asset[]>("media_assets", { files });
  return { assets, slides: assets.map((asset) => ({ id: asset.id, asset: asset.id })) };
}
