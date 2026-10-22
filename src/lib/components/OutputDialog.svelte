<script lang="ts">
  // C. 出力先の選択: which screen to present on, and how playback goes.
  // The playback settings belong to the show; the screen to this computer.
  import { untrack } from "svelte";
  import * as api from "$lib/api";
  import type { MonitorInfo } from "$lib/bindings/MonitorInfo";
  import type { Playback } from "$lib/bindings/Playback";
  import { modal } from "$lib/modal";
  import { open } from "$lib/show.svelte";

  let {
    monitors,
    onstart,
    oncancel,
  }: {
    monitors: MonitorInfo[];
    onstart: (monitor: MonitorInfo, remember: boolean) => void;
    oncancel: () => void;
  } = $props();

  // Another screen than the one this window is on, if there is one.
  let chosen = $state(
    untrack(() => monitors.find((m) => !m.hasMainWindow)?.index ?? monitors[0]?.index ?? 0),
  );
  let remember = $state(false);

  const playback = $derived(open.show?.playback);

  // The screens drawn to scale, as the OS arranges them.
  const layout = $derived.by(() => {
    const left = Math.min(...monitors.map((m) => m.x));
    const top = Math.min(...monitors.map((m) => m.y));
    const right = Math.max(...monitors.map((m) => m.x + m.width));
    const bottom = Math.max(...monitors.map((m) => m.y + m.height));
    const scale = Math.min(520 / (right - left), 200 / (bottom - top));
    return {
      width: (right - left) * scale,
      height: (bottom - top) * scale,
      place: (m: MonitorInfo) =>
        `left:${(m.x - left) * scale}px;top:${(m.y - top) * scale}px;width:${m.width * scale}px;height:${m.height * scale}px`,
    };
  });

  function setPlayback<K extends keyof Playback>(key: K, value: Playback[K]) {
    open.edit((show) => {
      show.playback[key] = value;
    });
  }

  function start() {
    const monitor = monitors.find((m) => m.index === chosen);
    if (monitor) onstart(monitor, remember);
  }
</script>

<div class="backdrop" role="presentation" onclick={(e) => e.target === e.currentTarget && oncancel()}>
  <div
    class="dialog"
    role="dialog"
    aria-modal="true"
    aria-labelledby="output-title"
    tabindex="-1"
    {@attach modal(() => oncancel())}
  >
    <h2 id="output-title">どの画面に映しますか？</h2>

    <div class="screens" style:width="{layout.width}px" style:height="{layout.height}px">
      {#each monitors as monitor (monitor.index)}
        <button
          class="screen"
          class:chosen={monitor.index === chosen}
          style={layout.place(monitor)}
          onclick={() => (chosen = monitor.index)}
        >
          <strong>{monitor.index + 1}</strong>
          <span>{monitor.name}</span>
          <span class="hint">{monitor.width}×{monitor.height}</span>
          {#if monitor.hasMainWindow}<span class="tag">操作用</span>{/if}
        </button>
      {/each}
    </div>
    <button class="quiet identify" onclick={() => api.identifyMonitors()}>各画面に番号を表示</button>

    {#if playback}
      <div class="options">
        <fieldset>
          <legend>進め方</legend>
          <label class="inline"><input type="radio" checked={playback.advance === "auto"} onchange={() => setPlayback("advance", "auto")} />自動</label>
          <label class="inline"><input type="radio" checked={playback.advance === "manual"} onchange={() => setPlayback("advance", "manual")} />手動（キーやボタンで進める）</label>
        </fieldset>
        <fieldset>
          <legend>最後まで再生したら</legend>
          <label class="inline"><input type="radio" checked={playback.atEnd === "loop"} onchange={() => setPlayback("atEnd", "loop")} />ループ</label>
          <label class="inline"><input type="radio" checked={playback.atEnd === "stop"} onchange={() => setPlayback("atEnd", "stop")} />最後のスライドで停止</label>
        </fieldset>
        <fieldset>
          <legend>順番</legend>
          <label class="inline"><input type="radio" checked={playback.order === "asListed"} onchange={() => setPlayback("order", "asListed")} />並び順</label>
          <label class="inline"><input type="radio" checked={playback.order === "shuffle"} onchange={() => setPlayback("order", "shuffle")} />シャッフル</label>
        </fieldset>
      </div>
    {/if}

    <footer>
      <label class="inline"><input type="checkbox" bind:checked={remember} />次回もこの画面を使う</label>
      <div class="buttons">
        <button onclick={oncancel}>やめる</button>
        <button class="live" onclick={start}>▶ 上映を始める</button>
      </div>
    </footer>
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
    width: min(640px, 92vw);
    display: flex;
    flex-direction: column;
    gap: 16px;
    box-shadow: 0 10px 40px rgb(0 0 0 / 30%);
  }

  h2 {
    margin: 0;
    font-size: 18px;
  }

  .screens {
    position: relative;
    margin: 0 auto;
  }

  .screen {
    position: absolute;
    padding: 6px;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: 2px;
    border: 2px solid var(--line);
    background: var(--surface-2);
    font-size: 12px;
    overflow: hidden;
  }

  .screen strong {
    font-size: 22px;
  }

  .screen.chosen {
    border-color: var(--live);
    background: var(--live-soft);
  }

  .tag {
    font-size: 11px;
    padding: 1px 6px;
    border-radius: 4px;
    background: var(--accent-soft);
    color: var(--accent);
  }

  .identify {
    align-self: center;
  }

  .options {
    display: grid;
    grid-template-columns: repeat(3, 1fr);
    gap: 12px;
  }

  fieldset {
    border: 1px solid var(--line);
    border-radius: 8px;
    margin: 0;
    padding: 8px 12px 10px;
    display: flex;
    flex-direction: column;
    gap: 6px;
  }

  legend {
    font-size: 13px;
    padding: 0 4px;
  }

  footer {
    display: flex;
    justify-content: space-between;
    align-items: center;
  }

  .buttons {
    display: flex;
    gap: 8px;
  }
</style>
