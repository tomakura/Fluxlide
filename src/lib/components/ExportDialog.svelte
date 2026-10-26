<script lang="ts">
  // F. 動画に書き出す: chosen by what the video is for, with the numbers as
  // a side note (docs/ui-draft.md, F). The export goes on when the dialog is
  // closed; opening it again shows how far it has got.
  import { save } from "@tauri-apps/plugin-dialog";
  import { revealItemInDir } from "@tauri-apps/plugin-opener";
  import * as api from "$lib/api";
  import type { ExportChoice } from "$lib/bindings/ExportChoice";
  import type { ExportEstimate } from "$lib/bindings/ExportEstimate";
  import type { Purpose } from "$lib/bindings/Purpose";
  import { exporting } from "$lib/exporting.svelte";
  import { clock, fileSize, roughly } from "$lib/format";
  import { modal } from "$lib/modal";
  import { open } from "$lib/show.svelte";

  let { onclose: close }: { onclose: () => void } = $props();

  // The result of the last export is shown until the dialog closes.
  function onclose() {
    if (!exporting.running) exporting.finished = null;
    close();
  }

  const PURPOSES: [Purpose, string, string][] = [
    ["tv", "テレビ・プロジェクター", "1920×1080"],
    ["web", "パソコン・動画サイト", "1920×1080 · 容量小さめ"],
    ["phone", "スマホ・SNS", "縦 1080×1920"],
    ["high", "高画質", "3840×2160"],
  ];

  let purpose = $state<Purpose>("tv");
  let fps = $state<number | null>(null);
  let videoKbps = $state<number | null>(null);
  let estimate = $state<ExportEstimate | null>(null);
  let error = $state<string | null>(null);
  let estimateError = $state<string | null>(null);

  const choice = $derived<ExportChoice>({ purpose, fps, videoKbps });

  // The estimate follows the choice and the show (its length changes as
  // slides are edited).
  $effect(() => {
    const asked = choice;
    void open.show;
    let current = true;
    api
      .exportEstimate(asked)
      .then((result) => {
        if (!current) return;
        estimate = result;
        estimateError = null;
      })
      .catch((err) => current && (estimateError = String(err)));
    return () => (current = false);
  });

  function choosePurpose(next: Purpose) {
    purpose = next;
    // The bit rate belongs to the purpose; a changed one would not fit the next.
    videoKbps = null;
  }

  async function start() {
    error = null;
    const name = (open.show?.title || "スライドショー").replace(/[\\/:*?"<>|]/g, "_");
    const path = await save({
      defaultPath: `${name}.mp4`,
      filters: [{ name: "MP4 動画", extensions: ["mp4"] }],
    });
    if (!path) return;
    try {
      await exporting.start($state.snapshot(choice), path);
    } catch (err) {
      error = String(err);
    }
  }

  const finished = $derived(exporting.finished);
</script>

<div class="backdrop" role="presentation" onclick={(e) => e.target === e.currentTarget && onclose()}>
  <div
    class="dialog"
    role="dialog"
    aria-modal="true"
    aria-labelledby="export-title"
    tabindex="-1"
    {@attach modal(onclose)}
  >
    <h2 id="export-title">動画に書き出す</h2>

    {#if exporting.running}
      <div class="progress">
        <progress max="1" value={exporting.fraction}></progress>
        <p>
          {Math.round(exporting.fraction * 100)}%
          {#if exporting.progress}
            <span class="hint">
              経過 {clock(exporting.progress.elapsedSecs)}
              {#if exporting.remainingSecs !== null}・残り {roughly(exporting.remainingSecs)}{/if}
            </span>
          {/if}
        </p>
        <p class="hint">この画面を閉じても書き出しは続きます。その間も編集できますが、書き出す内容は始めたときのままです。</p>
      </div>
      <div class="buttons">
        <button onclick={() => api.cancelExport()}>中止する</button>
        <button class="primary" data-autofocus onclick={onclose}>閉じる</button>
      </div>
    {:else}
      {#if finished && !finished.cancelled}
        {#if finished.error}
          <p class="banner error">書き出せませんでした: {finished.error}</p>
        {:else}
          <p class="banner done">
            書き出しました: {finished.path}
            <button class="quiet" onclick={() => revealItemInDir(finished.path)}>フォルダを開く</button>
          </p>
        {/if}
      {/if}

      <div class="purposes" role="radiogroup" aria-label="用途">
        {#each PURPOSES as [key, name, note] (key)}
          <button
            class="purpose"
            class:current={purpose === key}
            role="radio"
            aria-checked={purpose === key}
            onclick={() => choosePurpose(key)}
          >
            <strong>{name}</strong>
            <span class="hint">{note}</span>
          </button>
        {/each}
      </div>

      {#if estimate}
        <dl class="estimate">
          <div><dt>長さ</dt><dd>{clock(estimate.durationSecs)}</dd></div>
          <div><dt>ファイルサイズ</dt><dd>最大 {fileSize(estimate.bytes)}</dd></div>
          <div><dt>処理時間</dt><dd>{roughly(estimate.workSecs)}</dd></div>
        </dl>
        {#if estimate.skippedSlides > 0}
          <p class="hint">取り込み中か読めないスライドが {estimate.skippedSlides} 枚あり、書き出しには入りません。</p>
        {/if}
      {/if}

      <details>
        <summary>詳細設定</summary>
        <div class="details">
          <label>
            フレームレート
            <select
              value={fps ?? 30}
              onchange={(e) => (fps = Number(e.currentTarget.value))}
            >
              <option value={24}>24 fps</option>
              <option value={30}>30 fps</option>
              <option value={60}>60 fps</option>
            </select>
          </label>
          <label>
            映像のビットレート（kbps）
            <input
              type="number"
              min="500"
              max="100000"
              step="500"
              value={videoKbps ?? estimate?.videoKbps ?? ""}
              onchange={(e) => {
                const value = Number(e.currentTarget.value);
                videoKbps = value > 0 ? value : null;
              }}
            />
          </label>
        </div>
      </details>

      {#if estimateError}<p class="error">{estimateError}</p>{/if}
      {#if error}<p class="error">{error}</p>{/if}

      <div class="buttons">
        <button onclick={onclose}>やめる</button>
        <button class="primary" data-autofocus disabled={!open.show?.slides.length} onclick={start}>
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
    width: min(600px, 92vw);
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

  .purposes {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 8px;
  }

  .purpose {
    display: flex;
    flex-direction: column;
    align-items: flex-start;
    gap: 2px;
    padding: 12px 14px;
    text-align: left;
  }

  .purpose.current {
    border-color: var(--accent);
    background: var(--accent-soft);
  }

  .estimate {
    margin: 0;
    display: grid;
    grid-template-columns: repeat(3, 1fr);
    gap: 8px;
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

  details summary {
    cursor: pointer;
    color: var(--muted);
  }

  .details {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 10px;
    padding-top: 10px;
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
