<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";

  type EngineStatus = {
    gpuName: string;
    backend: string;
    deviceType: string;
  };

  type MonitorInfo = {
    index: number;
    name: string;
    width: number;
    height: number;
    x: number;
    y: number;
    scaleFactor: number;
    isPrimary: boolean;
    hasMainWindow: boolean;
  };

  type FrameStats = {
    frames: number;
    fps: number;
    worstFrameMs: number;
    slowFrames: number;
    skipped: number;
    width: number;
    height: number;
    lastError: string | null;
  };

  type PresentationStatus = {
    running: boolean;
    monitorName: string | null;
    stats: FrameStats;
  };

  let engine = $state<EngineStatus | null>(null);
  let engineError = $state<string | null>(null);
  let monitors = $state<MonitorInfo[]>([]);
  let selected = $state<number | null>(null);
  let status = $state<PresentationStatus | null>(null);
  let busy = $state(false);
  let actionError = $state<string | null>(null);

  async function loadMonitors() {
    monitors = await invoke<MonitorInfo[]>("list_monitors");
    if (selected === null || !monitors.some((m) => m.index === selected)) {
      // Prefer a display other than the one showing this window.
      selected = (monitors.find((m) => !m.hasMainWindow) ?? monitors[0])?.index ?? null;
    }
  }

  async function start() {
    if (selected === null) return;
    busy = true;
    actionError = null;
    try {
      status = await invoke<PresentationStatus>("start_presentation", { monitorIndex: selected });
    } catch (err) {
      actionError = String(err);
    } finally {
      busy = false;
    }
  }

  async function stop() {
    busy = true;
    try {
      status = await invoke<PresentationStatus>("stop_presentation");
    } finally {
      busy = false;
    }
  }

  $effect(() => {
    invoke<EngineStatus>("engine_status")
      .then((result) => (engine = result))
      .catch((err) => (engineError = String(err)));
    loadMonitors().catch((err) => (actionError = String(err)));

    const timer = setInterval(async () => {
      status = await invoke<PresentationStatus>("presentation_status");
    }, 500);
    return () => clearInterval(timer);
  });
</script>

<main>
  <header>
    <div class="brand">
      <img src="/fluxlide-mark.svg" alt="" width="40" height="40" />
      <h1>Fluxlide</h1>
    </div>
    <p class="engine" aria-live="polite">
      {#if engine}
        <span class="dot ok"></span>描画エンジン: {engine.gpuName}（{engine.backend}）
      {:else if engineError}
        <span class="dot err"></span>描画エンジンを起動できませんでした: {engineError}
      {:else}
        <span class="dot"></span>描画エンジンを起動しています…
      {/if}
    </p>
  </header>

  <section>
    <div class="section-head">
      <h2>上映ウィンドウの検証</h2>
      <p>選んだ画面にテストパターンを全画面で映します。上映中もこの画面は操作できます。</p>
    </div>

    <div class="monitors" role="radiogroup" aria-label="出力先の画面">
      {#each monitors as monitor (monitor.index)}
        <label class="monitor" class:on={selected === monitor.index}>
          <input
            type="radio"
            name="monitor"
            value={monitor.index}
            bind:group={selected}
            disabled={status?.running}
          />
          <b>{monitor.name}</b>
          <small>
            {monitor.width}×{monitor.height}
            {#if monitor.hasMainWindow}· 操作用{/if}
            {#if monitor.isPrimary}· メイン{/if}
          </small>
        </label>
      {/each}
      <button class="ghost" onclick={loadMonitors} disabled={busy}>画面を再検出</button>
    </div>

    <div class="actions">
      {#if status?.running}
        <button class="stop" onclick={stop} disabled={busy}>■ 上映を終了</button>
      {:else}
        <button class="live" onclick={start} disabled={busy || selected === null}>▶ 上映を始める</button>
      {/if}
      {#if actionError}
        <p class="error">{actionError}</p>
      {/if}
    </div>

    {#if status && (status.running || status.stats.frames > 0)}
      <dl class="stats">
        <div><dt>状態</dt><dd>{status.running ? `上映中（${status.monitorName}）` : "停止"}</dd></div>
        <div><dt>出力サイズ</dt><dd class="num">{status.stats.width}×{status.stats.height}</dd></div>
        <div><dt>フレームレート</dt><dd class="num">{status.stats.fps.toFixed(1)} fps</dd></div>
        <div><dt>直近1秒の最大間隔</dt><dd class="num">{status.stats.worstFrameMs.toFixed(1)} ms</dd></div>
        <div><dt>遅れたフレーム</dt><dd class="num">{status.stats.slowFrames}</dd></div>
        <div><dt>描画を省いた回数</dt><dd class="num">{status.stats.skipped}</dd></div>
        <div><dt>表示したフレーム</dt><dd class="num">{status.stats.frames}</dd></div>
      </dl>
      {#if status.stats.lastError}
        <p class="error">最後のエラー: {status.stats.lastError}</p>
      {/if}
    {/if}
  </section>
</main>

<style>
  :root {
    --bg: #f4f6f9;
    --surface: #ffffff;
    --line: #d5dbe2;
    --fg: #17212c;
    --muted: #5a6779;
    --accent: #1e5fc4;
    --accent-soft: #e2ebfa;
    --live: #d93c25;
    --on-live: #ffffff;
    --ok: #21825a;
    --err: #c2382c;
    font-family: "BIZ UDPGothic", "Hiragino Sans", "Yu Gothic UI", "Meiryo", sans-serif;
    color: var(--fg);
    background: var(--bg);
  }

  @media (prefers-color-scheme: dark) {
    :root {
      --bg: #0e1318;
      --surface: #161c23;
      --line: #2c3644;
      --fg: #e3e8ee;
      --muted: #97a3b3;
      --accent: #72a3f2;
      --accent-soft: #1c2a42;
      --live: #ff6b4f;
      --on-live: #1c0703;
      --ok: #4cc08a;
      --err: #f27769;
    }
  }

  main {
    max-width: 880px;
    margin: 0 auto;
    padding: 32px 40px 48px;
    display: flex;
    flex-direction: column;
    gap: 32px;
  }

  header {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    justify-content: space-between;
    gap: 12px 24px;
  }

  .brand {
    display: flex;
    align-items: center;
    gap: 12px;
  }

  h1 {
    margin: 0;
    font-family: "Jost", "Futura", "Century Gothic", sans-serif;
    font-weight: 600;
    font-size: 30px;
  }

  h2 {
    margin: 0;
    font-size: 20px;
  }

  section {
    display: flex;
    flex-direction: column;
    gap: 20px;
  }

  .section-head p {
    margin: 6px 0 0;
    color: var(--muted);
    font-size: 14px;
  }

  .engine {
    margin: 0;
    font-size: 13px;
    color: var(--muted);
    display: flex;
    align-items: center;
    gap: 8px;
  }

  .dot {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: var(--muted);
  }

  .dot.ok {
    background: var(--ok);
  }

  .dot.err {
    background: var(--err);
  }

  .monitors {
    display: flex;
    flex-wrap: wrap;
    gap: 12px;
    align-items: stretch;
  }

  .monitor {
    display: flex;
    flex-direction: column;
    gap: 2px;
    min-width: 180px;
    padding: 12px 14px;
    border: 1.5px solid var(--line);
    border-radius: 10px;
    background: var(--surface);
    cursor: pointer;
  }

  .monitor.on {
    border-color: var(--accent);
    background: var(--accent-soft);
  }

  .monitor input {
    position: absolute;
    opacity: 0;
    pointer-events: none;
  }

  .monitor:has(input:focus-visible) {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }

  .monitor small {
    color: var(--muted);
    font-size: 12px;
  }

  button {
    font: inherit;
    font-weight: 700;
    border-radius: 8px;
    padding: 10px 18px;
    border: 1px solid var(--line);
    background: var(--surface);
    color: var(--fg);
    cursor: pointer;
  }

  button:disabled {
    opacity: 0.5;
    cursor: default;
  }

  button.ghost {
    background: transparent;
    align-self: center;
  }

  button.live {
    background: var(--live);
    border-color: var(--live);
    color: var(--on-live);
    font-size: 16px;
  }

  button.stop {
    border-color: var(--live);
    color: var(--live);
    font-size: 16px;
  }

  .actions {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 16px;
  }

  .error {
    margin: 0;
    color: var(--err);
    font-size: 13px;
  }

  .stats {
    margin: 0;
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(150px, 1fr));
    border: 1px solid var(--line);
    border-radius: 10px;
    background: var(--surface);
    overflow: hidden;
  }

  .stats div {
    padding: 10px 14px;
    border-bottom: 1px solid var(--line);
  }

  .stats dt {
    font-size: 12px;
    color: var(--muted);
  }

  .stats dd {
    margin: 0;
    font-size: 16px;
    font-weight: 700;
  }

  .num {
    font-variant-numeric: tabular-nums;
  }
</style>
