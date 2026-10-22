// The slideshow open in the management window, as the Rust side last
// reported it. The Rust side owns the definition (docs/ipc.md): every change
// is sent as a whole new definition, and what comes back is shown.

import * as api from "./api";
import type { Asset } from "./bindings/Asset";
import type { AssetId } from "./bindings/AssetId";
import type { ShowDefinition } from "./bindings/ShowDefinition";
import type { ShowState } from "./bindings/ShowState";
import type { Slide } from "./bindings/Slide";
import type { SlideId } from "./bindings/SlideId";

export type Copying = { copied: number; total: number };

class OpenShow {
  state = $state<ShowState | null>(null);
  /// How far each file being imported has been copied.
  copying = $state<Record<AssetId, Copying>>({});
  /// The last command that failed, for the window to show.
  error = $state<string | null>(null);
  #queue: Promise<void> = Promise.resolve();

  get show(): ShowDefinition | null {
    return this.state?.show ?? null;
  }

  /// Takes a state from the Rust side, unless one already taken from the
  /// same session is newer.
  take(state: ShowState | null) {
    if (state && this.state && state.session === this.state.session && state.revision < this.state.revision) {
      return;
    }
    this.state = state;
  }

  /// Makes `change` on a copy of the definition and sends it. Changes are
  /// sent one at a time, in order. If the definition changed meanwhile
  /// (an import finished, say), `change` is made again on the new one.
  edit(change: (show: ShowDefinition) => void, group: string | null = null): Promise<void> {
    this.#queue = this.#queue.then(async () => {
      for (let attempt = 0; attempt < 5; attempt++) {
        const current = this.state;
        if (!current) return;
        const next = $state.snapshot(current.show) as ShowDefinition;
        change(next);
        try {
          const outcome = await api.editShow(next, current.revision, group);
          this.take(outcome.state);
          if (outcome.applied) return;
        } catch (err) {
          this.error = String(err);
          return;
        }
      }
    });
    return this.#queue;
  }

  /// Runs a command that returns the new state, after the edits before it.
  run(command: () => Promise<ShowState>): Promise<void> {
    this.#queue = this.#queue.then(async () => {
      try {
        this.take(await command());
      } catch (err) {
        this.error = String(err);
      }
    });
    return this.#queue;
  }

  undo = () => this.run(api.undoShow);
  redo = () => this.run(api.redoShow);

  /// Adds `files` as slides at the end of the list. Resolves to the new
  /// assets.
  async import(files: string[], addSlides = true): Promise<AssetId[]> {
    let assets: AssetId[] = [];
    await this.run(async () => {
      const result = await api.importFiles(files, addSlides);
      assets = result.assets;
      return result.state;
    });
    return assets;
  }

  asset(id: AssetId): Asset | undefined {
    return this.show?.assets.find((asset) => asset.id === id);
  }

  slide(id: SlideId): Slide | undefined {
    return this.show?.slides.find((slide) => slide.id === id);
  }
}

export const open = new OpenShow();

// Changes the Rust side makes by itself, and the progress of copies.
api.onShowChanged((state) => open.take(state));
api.onImportProgress(({ asset, copied, total }) => {
  open.copying[asset] = { copied, total };
});
