<script lang="ts">
  // The preview in the centre of B. 編集: the same engine as the
  // presentation draws it offscreen, and frames are fetched one at a time
  // (docs/spike-preview.md). It holds the selected slide still, or plays
  // from it.
  import * as api from "$lib/api";
  import type { SlideId } from "$lib/bindings/SlideId";
  import { open } from "$lib/show.svelte";

  let {
    selected,
    playing = $bindable(false),
    onslide,
  }: {
    selected: SlideId | null;
    playing?: boolean;
    /// The slide on screen while playing.
    onslide?: (slide: SlideId | null) => void;
  } = $props();

  let canvas = $state<HTMLCanvasElement | null>(null);
  let box = $state({ width: 0, height: 0 });
  let error = $state<string | null>(null);
  // Each start gets a new number; a loop whose number is no longer current
  // (stopped or restarted meanwhile) ends quietly.
  let generation = 0;
  let pumping = false;
  let onScreen: SlideId | null = null;

  const revision = $derived(open.state?.revision ?? 0);

  // Restart after edits, a new selection or a new size. Edits come quickly
  // while typing, so the restart waits for a pause.
  $effect(() => {
    const request = {
      from: playing ? (onScreen ?? selected) : selected,
      still: !playing,
      width: box.width,
      height: box.height,
      revision,
    };
    if (!request.width || !request.height || !canvas) return;
    const timer = setTimeout(() => start(request), 150);
    return () => clearTimeout(timer);
  });

  $effect(() => () => {
    generation++;
    api.stopPreview();
  });

  async function start({ from, still, width, height }: { from: SlideId | null; still: boolean; width: number; height: number }) {
    const scale = Math.min(window.devicePixelRatio || 1, 2);
    const w = Math.round((width * scale) / 2) * 2;
    const h = Math.round((height * scale) / 2) * 2;
    try {
      await api.startPreview(w, h, from, still);
      error = null;
    } catch (err) {
      error = String(err);
      return;
    }
    if (!pumping && canvas) pump(canvas, ++generation);
  }

  async function pump(canvas: HTMLCanvasElement, mine: number) {
    const context = canvas.getContext("2d");
    if (!context) return;
    pumping = true;
    try {
      while (mine === generation) {
        let buffer: ArrayBuffer;
        try {
          buffer = await api.previewFrame();
        } catch (err) {
          if (mine === generation) error = String(err);
          return;
        }
        if (mine !== generation) return;
        // Pixels, then width, height and slide as little-endian u32.
        const tail = new DataView(buffer, buffer.byteLength - 12);
        const width = tail.getUint32(0, true);
        const height = tail.getUint32(4, true);
        const slide = tail.getUint32(8, true) || null;
        if (canvas.width !== width || canvas.height !== height) {
          canvas.width = width;
          canvas.height = height;
        }
        context.putImageData(new ImageData(new Uint8ClampedArray(buffer, 0, width * height * 4), width, height), 0, 0);
        if (slide !== onScreen) {
          onScreen = slide;
          onslide?.(slide);
        }
        // At most one frame per display refresh.
        await new Promise((resolve) => requestAnimationFrame(resolve));
      }
    } finally {
      pumping = false;
    }
  }
</script>

<div class="frame" bind:clientWidth={box.width} bind:clientHeight={box.height}>
  <canvas bind:this={canvas}></canvas>
  {#if error}<p class="error">{error}</p>{/if}
</div>

<style>
  .frame {
    position: relative;
    width: 100%;
    aspect-ratio: 16 / 9;
    background: #000;
    border-radius: 6px;
    overflow: hidden;
  }

  canvas {
    width: 100%;
    height: 100%;
    display: block;
  }

  .error {
    position: absolute;
    inset: auto 12px 12px 12px;
    margin: 0;
    padding: 8px 10px;
    border-radius: 6px;
    background: var(--err-soft);
  }
</style>
