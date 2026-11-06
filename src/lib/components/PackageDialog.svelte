<script lang="ts">
  // ほかのパソコン用に書き出す: the slideshow with its photos, videos and,
  // if chosen, its music, in one .fluxlide file that Fluxlide on another
  // computer opens (docs/project-storage.md).
  import { save } from "@tauri-apps/plugin-dialog";
  import { revealItemInDir } from "@tauri-apps/plugin-opener";
  import * as api from "$lib/api";
  import type { PackageEstimate } from "$lib/bindings/PackageEstimate";
  import type { PackageProgress } from "$lib/bindings/PackageProgress";
  import { fileSize } from "$lib/format";
  import { modal } from "$lib/modal";
  import { appear, pop } from "$lib/motion";

  let { dir, title, onclose }: { dir: string; title: string; onclose: () => void } = $props();

  let estimate = $state<PackageEstimate | null>(null);
  let includeMusic = $state(true);
  let running = $state(false);
  let progress = $state<PackageProgress | null>(null);
  let error = $state<string | null>(null);
  let done = $state<string | null>(null);

  $effect(() => {
    api
      .packageEstimate(dir)
      .then((result) => (estimate = result))
      .catch((err) => (error = String(err)));
  });

  const music = $derived(estimate?.music ?? null);
  const musicUsable = $derived(!!music && !music.error);
  const withMusic = $derived(includeMusic && musicUsable);
  const bytes = $derived(estimate ? estimate.bytes + (withMusic ? music!.bytes : 0) : 0);
  const fraction = $derived(progress && progress.total > 0 ? progress.done / progress.total : 0);

  async function start() {
    error = null;
    done = null;
    const name = (title || "スライドショー").replace(/[\\/:*?"<>|]/g, "_");
    const path = await save({ defaultPath: `${name}.fluxlide`, filters: [api.PACKAGE_FILTER] });
    if (!path) return;
    running = true;
    progress = null;
    const unlisten = await api.onPackageProgress((next) => (progress = next));
    try {
      await api.exportPackage(dir, path, withMusic);
      done = path;
    } catch (err) {
      error = String(err);
    } finally {
      unlisten();
      running = false;
    }
  }

  // Closing while it is written would leave it going with nothing to show it.
  const close = () => !running && onclose();
</script>

<div class="backdrop" role="presentation" transition:appear|global onclick={(e) => e.target === e.currentTarget && close()}>
  <div
    class="dialog"
    role="dialog"
    transition:pop|global
    aria-modal="true"
    aria-labelledby="package-title"
    tabindex="-1"
    {@attach modal(close)}
  >
    <h2 id="package-title">ほかのパソコン用に書き出す</h2>
    <p class="hint">
      写真・動画・設定を1つのファイル（.fluxlide）にまとめます。ほかのパソコンの Fluxlide で、ホームの「ファイルから読み込む」か、ファイルのダブルクリックで開けます。
    </p>

    {#if running}
      <div class="progress">
        <progress max="1" value={fraction}></progress>
        <p>
          {Math.round(fraction * 100)}%
          {#if progress}<span class="hint">{fileSize(progress.done)} / {fileSize(progress.total)}</span>{/if}
        </p>
      </div>
      <div class="buttons">
        <button data-autofocus onclick={() => api.cancelPackage()}>中止する</button>
      </div>
    {:else}
      {#if done}
        <p class="banner done">
          書き出しました: {done}
          <button class="quiet" onclick={() => revealItemInDir(done!)}>フォルダを開く</button>
        </p>
      {/if}

      {#if estimate}
        {#if music}
          <label class="check" class:disabled={!musicUsable}>
            <input type="checkbox" bind:checked={includeMusic} disabled={!musicUsable} />
            <span>
              BGMも入れる
              {#if musicUsable}
                <span class="hint">（{music.tracks}曲・{fileSize(music.bytes)}）</span>
              {/if}
            </span>
          </label>
          {#if music.error}
            <p class="hint warn">{music.error} BGMは入れずに書き出します。開いた先で選び直してください。</p>
          {:else if !includeMusic}
            <p class="hint">BGMは入りません。開いた先で選び直してください。</p>
          {/if}
        {/if}

        <dl class="estimate">
          <div><dt>ファイルサイズ（推定）</dt><dd>{fileSize(bytes)}</dd></div>
        </dl>

        {#if estimate.importing > 0}
          <p class="banner error">取り込み中の写真や動画が{estimate.importing}件あります。終わってから書き出してください。</p>
        {/if}
        {#if estimate.failed > 0}
          <p class="hint">取り込めなかった素材が{estimate.failed}件あります。ファイルは入らず、開いた先でも「選び直す」が必要です。</p>
        {/if}
      {:else if !error}
        <p class="hint">大きさを調べています…</p>
      {/if}

      {#if error}<p class="error">{error}</p>{/if}

      <div class="buttons">
        <button onclick={onclose}>{done ? "閉じる" : "やめる"}</button>
        <button class="primary" data-autofocus disabled={!estimate || estimate.importing > 0} onclick={start}>
          保存先を選んで書き出す…
        </button>
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
    z-index: 10;
  }

  .dialog {
    background: var(--surface);
    border-radius: 14px;
    padding: 24px 28px;
    width: min(520px, 92vw);
    display: flex;
    flex-direction: column;
    gap: 14px;
    box-shadow: 0 10px 40px rgb(0 0 0 / 30%);
  }

  h2 {
    margin: 0;
    font-size: 18px;
  }

  p {
    margin: 0;
  }

  .check {
    display: flex;
    align-items: center;
    gap: 8px;
  }

  .check.disabled {
    color: var(--muted);
  }

  .warn {
    color: var(--err);
  }

  .estimate {
    margin: 0;
  }

  .estimate div {
    padding: 8px 12px;
    border-radius: 8px;
    background: var(--surface-2);
  }

  dt {
    font-size: 12px;
    color: var(--muted);
  }

  dd {
    margin: 2px 0 0;
    font-size: 16px;
    font-weight: 600;
    font-variant-numeric: tabular-nums;
  }

  .progress {
    display: flex;
    flex-direction: column;
    gap: 8px;
  }

  progress {
    width: 100%;
    height: 12px;
  }

  .banner {
    padding: 10px 12px;
    border-radius: 8px;
    overflow-wrap: anywhere;
  }

  .banner.done {
    background: var(--accent-soft);
  }

  .banner.error {
    background: var(--err-soft);
    color: var(--err);
  }

  .buttons {
    display: flex;
    justify-content: flex-end;
    gap: 8px;
  }
</style>
