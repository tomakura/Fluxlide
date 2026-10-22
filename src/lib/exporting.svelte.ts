// The export in progress, kept while its dialog is closed: writing a long
// show takes a while, and the show can be edited meanwhile.

import * as api from "./api";
import type { ExportFinishedEvent } from "./bindings/ExportFinishedEvent";
import type { ExportProgressEvent } from "./bindings/ExportProgressEvent";

class Exporting {
  /// Whether an export is running.
  running = $state(false);
  progress = $state<ExportProgressEvent | null>(null);
  /// How the last export ended, until the next starts.
  finished = $state<ExportFinishedEvent | null>(null);

  /// From 0 to 1.
  get fraction(): number {
    const progress = this.progress;
    if (!progress || progress.totalFrames === 0) return 0;
    return Math.min(1, progress.frames / progress.totalFrames);
  }

  /// Seconds left, judged from the speed so far.
  get remainingSecs(): number | null {
    const progress = this.progress;
    const fraction = this.fraction;
    if (!progress || fraction < 0.02) return null;
    return (progress.elapsedSecs / fraction) * (1 - fraction);
  }

  async start(...args: Parameters<typeof api.startExport>) {
    this.finished = null;
    this.progress = null;
    // Before the call: a short export can end before it returns.
    this.running = true;
    try {
      await api.startExport(...args);
    } catch (err) {
      this.running = false;
      throw err;
    }
  }
}

export const exporting = new Exporting();

api.onExportProgress((progress) => {
  exporting.progress = progress;
});
api.onExportFinished((finished) => {
  exporting.running = false;
  exporting.finished = finished;
});
