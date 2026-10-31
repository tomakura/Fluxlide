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
  /// The newest state, which `state` shows once no command is waiting:
  /// the reply to an earlier keystroke must not put the text being typed
  /// back as it was.
  #latest: ShowState | null = null;
  pending = $state(0);
  failed = $state(0);
  #failedEdits: { change: (show: ShowDefinition) => void; group: string | null }[] = [];

  get show(): ShowDefinition | null {
    return this.state?.show ?? null;
  }

  /// Takes a state from the Rust side, unless one already taken from the
  /// same session is newer.
  take(state: ShowState | null) {
    const latest = this.#latest;
    if (state && latest && state.session === latest.session && state.revision < latest.revision) {
      return;
    }
    if (state?.session !== latest?.session) {
      this.#failedEdits = [];
      this.failed = 0;
      this.error = null;
      this.copying = {};
    }
    this.#latest = state;
    if (this.pending === 0 || state?.session !== this.state?.session) this.state = state;
  }

  /// Runs `task` after the commands before it, keeping `state` as it is
  /// until none is left.
  #enqueue(task: () => Promise<void>): Promise<void> {
    this.pending++;
    this.#queue = this.#queue.then(async () => {
      try {
        await task();
      } finally {
        if (--this.pending === 0) this.state = this.#latest;
      }
    });
    return this.#queue;
  }

  /// Makes `change` on a copy of the definition and sends it. Changes are
  /// sent one at a time, in order. If the definition changed meanwhile
  /// (an import finished, say), `change` is made again on the new one; if
  /// another show was opened meanwhile, the change is dropped.
  edit(change: (show: ShowDefinition) => void, group: string | null = null): Promise<void> {
    const session = this.#latest?.session;
    return this.#enqueue(() => this.#applyEdit(change, group, session));
  }

  async #applyEdit(change: (show: ShowDefinition) => void, group: string | null, session: number | undefined): Promise<void> {
      if (this.#latest?.session !== session) return;
      const failed = (reason: string) => {
        this.#failedEdits.push({ change, group });
        this.failed = this.#failedEdits.length;
        this.error = reason;
      };
      if (this.failed) {
        failed(this.error ?? "変更を送信できていません。");
        return;
      }
      for (let attempt = 0; attempt < 5; attempt++) {
        const current = this.#latest;
        if (!current || current.session !== session) return;
        // `#latest` is a plain object, not reactive state, which
        // `$state.snapshot` would hand back as it is: the change must be
        // made on a copy, not on the definition last received.
        const next = structuredClone(current.show);
        change(next);
        try {
          const outcome = await api.editShow(next, current.session, current.revision, group);
          this.take(outcome.state);
          if (outcome.applied) return;
        } catch (err) {
          failed(String(err));
          return;
        }
      }
      failed("変更を反映できませんでした。保存を再試行してください。");
  }

  /// Runs a command that returns the new state, after the edits before it.
  run(command: () => Promise<ShowState>): Promise<void> {
    const session = this.#latest?.session;
    return this.#enqueue(async () => {
      if (this.#latest?.session !== session) return;
      try {
        this.take(await command());
      } catch (err) {
        this.error = String(err);
      }
    });
  }

  undo = () => this.run(api.undoShow);
  redo = () => this.run(api.redoShow);

  /// A transition must wait for the edits already typed, and must not hide
  /// an unsaved state behind a new screen or an export of older content.
  async flush(): Promise<void> {
    const session = this.#latest?.session;
    do {
      await this.#queue;
    } while (this.pending > 0);
    if (session !== this.#latest?.session) throw new Error("開いているスライドショーが変わりました。");
    if (this.failed) throw new Error(this.error ?? "変更を送信できていません。");
    if (this.#latest?.saveError) throw new Error(this.#latest.saveError);
  }

  retrySave(): Promise<void> {
    const session = this.#latest?.session;
    return this.#enqueue(async () => {
      if (session === undefined || session !== this.#latest?.session) return;
      const edits = this.#failedEdits;
      this.#failedEdits = [];
      this.failed = 0;
      this.error = null;
      for (const { change, group } of edits) await this.#applyEdit(change, group, session);
      if (this.failed) return;
      try {
        this.take(await api.saveProject(session));
      } catch (err) {
        this.error = String(err);
      }
    });
  }

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
