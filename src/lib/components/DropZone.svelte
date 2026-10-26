<script lang="ts">
  // Where files can be dropped. The webview reports drops for the whole
  // window, so a drop counts here only while this zone is under the pointer.
  import { getCurrentWebview } from "@tauri-apps/api/webview";
  import type { Snippet } from "svelte";
  import { dropPoint, isWindows } from "$lib/drop";

  let { ondrop, children }: { ondrop: (files: string[]) => void; children: Snippet } = $props();

  let zone = $state<HTMLElement | null>(null);
  let over = $state(false);

  function inside(position: { x: number; y: number }) {
    if (!zone) return false;
    const rect = zone.getBoundingClientRect();
    const { x, y } = dropPoint(position, isWindows(), window.devicePixelRatio);
    return x >= rect.left && x <= rect.right && y >= rect.top && y <= rect.bottom;
  }

  $effect(() => {
    const unlisten = getCurrentWebview().onDragDropEvent((event) => {
      const payload = event.payload;
      if (payload.type === "over" || payload.type === "enter") {
        over = inside(payload.position);
      } else if (payload.type === "drop") {
        if (inside(payload.position) && payload.paths.length) ondrop(payload.paths);
        over = false;
      } else {
        over = false;
      }
    });
    return () => {
      unlisten.then((stop) => stop());
    };
  });
</script>

<div class="zone" class:over bind:this={zone}>
  {@render children()}
</div>

<style>
  .zone {
    border: 2px dashed var(--line);
    border-radius: var(--radius);
    padding: 22px;
    text-align: center;
    color: var(--muted);
    font-size: 13px;
    text-wrap: balance;
  }

  .over {
    border-color: var(--accent);
    background: var(--accent-soft);
    color: var(--fg);
  }
</style>
