// A show made directly from files, for the prototype page until it opens
// projects instead. Fields left out take the engine's defaults.

import { invoke } from "@tauri-apps/api/core";
import type { Asset } from "./bindings/Asset";
import type { ShowDefinition } from "./bindings/ShowDefinition";
import type { Slide } from "./bindings/Slide";

type PartialShow = Omit<Partial<ShowDefinition>, "slides"> & {
  slides: Pick<Slide, "id" | "asset">[];
};

// A show of `files`, one slide each in order. Slide `n` (from 1) shows file `n`.
export async function showOfFiles(files: string[]): Promise<PartialShow> {
  const assets = await invoke<Asset[]>("media_assets", { files });
  return { assets, slides: assets.map((asset) => ({ id: asset.id, asset: asset.id })) };
}
