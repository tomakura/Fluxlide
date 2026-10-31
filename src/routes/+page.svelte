<script lang="ts">
  // The management window: the home screen, the editor, and the operator's
  // screen while presenting (docs/ui-draft.md, A to D).
  import * as api from "#lib/api";
  import { onMount } from "svelte";
  import Confirm from "#lib/components/Confirm.svelte";
  import ExportDialog from "#lib/components/ExportDialog.svelte";
  import { exporting } from "#lib/exporting.svelte";
  import type { MonitorInfo } from "#lib/bindings/MonitorInfo";
  import Editor from "#lib/components/Editor.svelte";
  import Home from "#lib/components/Home.svelte";
  import OutputDialog from "#lib/components/OutputDialog.svelte";
  import Presenting from "#lib/components/Presenting.svelte";
  import { rise } from "#lib/motion";
  import { open } from "#lib/show.svelte";

  type Screen = "loading" | "home" | "editor" | "presenting";
  let screen = $state<Screen>("loading");
  let choosing = $state<MonitorInfo[] | null>(null);
  let notice = $state<string | null>(null);
  let error = $state<string | null>(null);
  let syncOffsetMs = $state(0);
  // From "▶ 上映する" until the presentation opens, so pressing it again
  // does not open a second one.
  let starting = $state(false);
  let preparing = false;
  let startTimer: ReturnType<typeof setTimeout> | null = null;
  let startTask: Promise<void> | null = null;
  let request = 0;
  let loadingError = $state<string | null>(null);
  let settingsError = $state(false);
  let leaving = $state(false);
  let discardAction = $state<"home" | "app" | null>(null);
  let exportOpen = $state(false);
  let seenFinish = exporting.finished;
  const saveProblem = $derived(open.error ?? open.state?.saveError ?? null);

  async function initialize() {
    loadingError = null;
    settingsError = false;
    try {
      await api.getSettings().catch((err) => { settingsError = true; throw err; });
      const state = await api.showState();
      open.take(state);
      screen = state ? "editor" : "home";
    } catch (err) {
      loadingError = String(err);
    }
  }

  onMount(() => {
    void initialize();
    const closing = api.onAppCloseRequested(() => void leave("app"));
    return () => {
      cancelStart();
      closing.then((stop) => stop());
    };
  });

  $effect(() => {
    const finished = exporting.finished;
    if (finished && finished !== seenFinish && !finished.cancelled) exportOpen = true;
    seenFinish = finished;
  });

  function cancelStart() {
    request++;
    if (startTimer !== null) clearTimeout(startTimer);
    startTimer = null;
    notice = null;
    choosing = null;
  }

  async function leave(action: "home" | "app", discard = false) {
    if (leaving) return;
    cancelStart();
    leaving = true;
    try {
      if (!discard) await open.flush();
      await startTask;
      if (action === "app") {
        await api.exitApp(discard);
      } else {
        await api.closeProject(discard);
        open.take(null);
        screen = "home";
      }
      discardAction = null;
    } catch (err) {
      error = String(err);
      if (open.failed || open.state?.saveError) discardAction = action;
    } finally {
      leaving = false;
    }
  }

  async function resetSettings() {
    try {
      await api.resetSettings();
      await initialize();
    } catch (err) {
      loadingError = String(err);
    }
  }

  /// "▶ 上映する": straight to the remembered screen, or after asking.
  async function present(choose: boolean) {
    if (starting || preparing || notice || leaving) return;
    preparing = true;
    const mine = ++request;
    const session = open.state?.session;
    error = null;
    try {
      await open.flush();
      const [monitors, settings] = await Promise.all([api.listMonitors(), api.getSettings()]);
      if (mine !== request || screen !== "editor" || open.state?.session !== session) return;
      syncOffsetMs = settings.syncOffsetMs;
      const remembered = monitors.find((m) => m.name === settings.outputScreen);
      if (!choose && remembered) {
        await start(remembered, false);
      } else if (!choose && monitors.length === 1) {
        // With one screen there is nothing to choose, but the operator must
        // know how to get back (docs/ui-draft.md, C-3).
        notice = "この画面に全画面で映します。終了するにはEscキーを押してください。";
        startTimer = setTimeout(() => {
          startTimer = null;
          notice = null;
          if (mine === request && screen === "editor" && open.state?.session === session)
            void start(monitors[0], false);
        }, 2500);
      } else {
        choosing = monitors;
      }
    } catch (err) {
      error = String(err);
    } finally {
      preparing = false;
    }
  }

  async function start(monitor: MonitorInfo, remember: boolean) {
    choosing = null;
    if (starting || leaving || screen !== "editor") return;
    starting = true;
    const mine = request;
    const session = open.state?.session;
    const current = () => mine === request && screen === "editor" && !leaving && open.state?.session === session;
    try {
      await open.flush();
      if (!current()) return;
      if (remember) await api.setOutputScreen(monitor.name);
      if (!current()) return;
      startTask = api.startPresentation(monitor.index).then(async () => {
        if (current()) screen = "presenting";
        else await api.stopPresentation();
      });
      await startTask;
    } catch (err) {
      if (mine === request) error = `上映を始められませんでした: ${err}`;
    } finally {
      startTask = null;
      starting = false;
    }
  }
</script>

<div inert={leaving}>
{#if screen === "loading"}
  <main class="loading" role="status">
    {#if loadingError}
      <h1>Fluxlideを開けませんでした</h1>
      <p class="error">{loadingError}</p>
      <button onclick={initialize}>もう一度試す</button>
      {#if settingsError}
        <p>元の設定ファイルを別名で残して、設定を初期化できます。</p>
        <button onclick={resetSettings}>設定を退避して初期化</button>
      {/if}
    {:else}
      <p>読み込み中…</p>
    {/if}
  </main>
{:else if screen === "home"}
  <Home onopened={() => (screen = "editor")} />
{:else if screen === "editor"}
  <Editor onhome={() => void leave("home")} onpresent={present} onexport={() => (exportOpen = true)} />
{:else if screen === "presenting"}
  <Presenting {syncOffsetMs} onend={() => (screen = "editor")} />
{/if}
</div>

{#if screen === "editor" && saveProblem}
  <div class="save-problem banner error" role="alert">
    <span>変更を保存できません: {saveProblem}</span>
    <button onclick={() => open.retrySave()}>保存を再試行</button>
  </div>
{/if}

{#if exporting.running && !exportOpen}
  <div class="export-status" role="status">
    <button onclick={() => (exportOpen = true)}>書き出し中 {Math.round(exporting.fraction * 100)}%・進み具合を開く</button>
  </div>
{/if}

{#if exportOpen}
  <ExportDialog onclose={() => (exportOpen = false)} />
{/if}

{#if discardAction}
  <Confirm
    title="保存していない変更を破棄しますか？"
    message="変更がまだ保存されていません。「やめる」で戻り、保存を再試行できます。破棄すると、保存できていない変更は失われます。"
    confirmLabel={discardAction === "app" ? "破棄して終了" : "破棄してホームへ"}
    onconfirm={() => { const action = discardAction; if (action) void leave(action, true); }}
    oncancel={() => (discardAction = null)}
  />
{/if}

{#if choosing}
  <OutputDialog monitors={choosing} onstart={start} oncancel={() => (choosing = null)} />
{/if}

{#if notice || error}
  <div class="toast" class:error={!!error} role="status" transition:rise|global={{ y: 16 }}>
    {notice ?? error}
    {#if error}<button class="quiet" onclick={() => (error = null)}>閉じる</button>{/if}
  </div>
{/if}

<style>
  .loading { padding: 48px; }
  .save-problem, .export-status {
    position: fixed;
    bottom: 16px;
    left: 16px;
    max-width: min(700px, calc(100vw - 32px));
    z-index: 9;
    display: flex;
    align-items: center;
    gap: 12px;
  }
  .save-problem { bottom: 64px; }
  .toast {
    position: fixed;
    left: 50%;
    bottom: 32px;
    transform: translateX(-50%);
    padding: 14px 20px;
    border-radius: 12px;
    background: var(--navy);
    color: #fff;
    font-size: 15px;
    display: flex;
    gap: 12px;
    align-items: center;
    z-index: 30;
    box-shadow: 0 6px 24px rgb(0 0 0 / 35%);
  }

  .toast.error {
    background: var(--err-soft);
    color: var(--err);
  }
</style>
