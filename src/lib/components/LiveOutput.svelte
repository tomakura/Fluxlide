<script lang="ts">
  // What the presentation window shows right now, for the operator's
  // screen: small copies taken by the engine frame after frame, fetched
  // once per refresh of this screen.
  import * as api from "$lib/api";

  let canvas = $state<HTMLCanvasElement | null>(null);
  let ready = $state(false);

  $effect(() => {
    if (!canvas) return;
    const context = canvas.getContext("2d");
    if (!context) return;
    let stopped = false;
    let shown = 0;
    const pull = async () => {
      if (stopped) return;
      try {
        const buffer = await api.presentationFrame(shown);
        if (!stopped && buffer.byteLength > 12) {
          // Pixels, then width, height and number as little-endian u32.
          const tail = new DataView(buffer, buffer.byteLength - 12);
          const width = tail.getUint32(0, true);
          const height = tail.getUint32(4, true);
          shown = tail.getUint32(8, true);
          if (canvas!.width !== width || canvas!.height !== height) {
            canvas!.width = width;
            canvas!.height = height;
          }
          context.putImageData(new ImageData(new Uint8ClampedArray(buffer, 0, width * height * 4), width, height), 0, 0);
          ready = true;
        }
      } catch {
        // The next pull tries again.
      }
      requestAnimationFrame(pull);
    };
    pull();
    return () => {
      stopped = true;
    };
  });
</script>

<canvas bind:this={canvas} class:ready></canvas>

<style>
  canvas {
    width: 100%;
    height: 100%;
    display: block;
    object-fit: contain;
    opacity: 0;
    transition: opacity 0.3s ease-out;
  }

  canvas.ready {
    opacity: 1;
  }
</style>
