<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";

  type EngineStatus = {
    gpuName: string;
    backend: string;
    deviceType: string;
  };

  let status = $state<EngineStatus | null>(null);
  let error = $state<string | null>(null);

  $effect(() => {
    invoke<EngineStatus>("engine_status")
      .then((result) => (status = result))
      .catch((err) => (error = String(err)));
  });
</script>

<main>
  <div class="brand">
    <img src="/fluxlide-mark.svg" alt="" width="56" height="56" />
    <h1>Fluxlide</h1>
  </div>
  <p class="lead">写真や動画を並べるだけで、スライドショーを作れます。</p>

  <p class="engine" aria-live="polite">
    {#if status}
      <span class="dot ok"></span>描画エンジン: {status.gpuName}（{status.backend}）
    {:else if error}
      <span class="dot err"></span>描画エンジンを起動できませんでした: {error}
    {:else}
      <span class="dot"></span>描画エンジンを起動しています…
    {/if}
  </p>
</main>

<style>
  :root {
    --bg: #f4f6f9;
    --fg: #17212c;
    --muted: #5a6779;
    --ok: #21825a;
    --err: #c2382c;
    font-family: "BIZ UDPGothic", "Hiragino Sans", "Yu Gothic UI", "Meiryo", sans-serif;
    color: var(--fg);
    background: var(--bg);
  }

  @media (prefers-color-scheme: dark) {
    :root {
      --bg: #0e1318;
      --fg: #e3e8ee;
      --muted: #97a3b3;
      --ok: #4cc08a;
      --err: #f27769;
    }
  }

  main {
    min-height: 100vh;
    display: flex;
    flex-direction: column;
    justify-content: center;
    gap: 16px;
    padding: 0 64px;
  }

  .brand {
    display: flex;
    align-items: center;
    gap: 14px;
  }

  h1 {
    margin: 0;
    font-family: "Jost", "Futura", "Century Gothic", sans-serif;
    font-weight: 600;
    font-size: 40px;
  }

  .lead {
    margin: 0;
    font-size: 17px;
    color: var(--muted);
  }

  .engine {
    margin: 24px 0 0;
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
</style>
