<script lang="ts">
  // Reading a .fluxlide file into a new slideshow, with how far it has got.
  import * as api from "$lib/api";
  import type { PackageProgress } from "$lib/bindings/PackageProgress";
  import { fileSize } from "$lib/format";
  import { modal } from "$lib/modal";
  import { appear, pop } from "$lib/motion";

  let {
    path,
    onread,
    onclose,
  }: { path: string; onread: (dir: string) => void; onclose: () => void } = $props();

  let progress = $state<PackageProgress | null>(null);
  let error = $state<string | null>(null);
  let running = $state(true);

  const name = $derived(path.split(/[\\/]/).pop() ?? path);
  const fraction = $derived(progress && progress.total > 0 ? progress.done / progress.total : 0);

  $effect(() => {
    let unlisten: (() => void) | null = null;
    let current = true;
    (async () => {
      unlisten = await api.onPackageProgress((next) => (progress = next));
      try {
        const dir = await api.importPackage(path);
        if (current) onread(dir);
      } catch (err) {
        if (current) error = String(err);
      } finally {
        running = false;
        unlisten?.();
      }
    })();
    return () => {
      current = false;
      unlisten?.();
    };
  });

  const close = () => !running && onclose();
</script>

<div class="backdrop" role="presentation" transition:appear|global>
  <div
    class="dialog"
    role="dialog"
    transition:pop|global
    aria-modal="true"
    aria-labelledby="package-import-title"
    tabindex="-1"
    {@attach modal(close)}
  >
    <h2 id="package-import-title">ファイルから読み込む</h2>
    <p class="hint name">{name}</p>
    {#if error}
      <p class="error">{error}</p>
      <div class="buttons">
        <button class="primary" data-autofocus onclick={onclose}>閉じる</button>
      </div>
    {:else}
      <progress max="1" value={fraction}></progress>
      <p>
        {Math.round(fraction * 100)}%
        {#if progress}<span class="hint">{fileSize(progress.done)} / {fileSize(progress.total)}</span>{/if}
      </p>
      <div class="buttons">
        <button data-autofocus onclick={() => api.cancelPackage()}>中止する</button>
      </div>
    {/if}
  </div>
</div>

<style>
  .backdrop {
    position: fixed;
    inset: 0;
    background: rgb(0 0 0 / 45%);
    display: flex;
    align-items: center;
    justify-content: center;
    z-index: 25;
  }

  .dialog {
    background: var(--surface);
    border-radius: 14px;
    padding: 24px 28px;
    width: min(460px, 92vw);
    display: flex;
    flex-direction: column;
    gap: 12px;
    box-shadow: 0 10px 40px rgb(0 0 0 / 30%);
  }

  h2 {
    margin: 0;
    font-size: 18px;
  }

  p {
    margin: 0;
  }

  .name {
    overflow-wrap: anywhere;
  }

  progress {
    width: 100%;
    height: 12px;
  }

  .buttons {
    display: flex;
    justify-content: flex-end;
    gap: 8px;
  }
</style>
