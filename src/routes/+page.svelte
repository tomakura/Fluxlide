<script lang="ts">
  // The management window: the home screen, the editor, and the operator's
  // screen while presenting (docs/ui-draft.md, A to D).
  import * as api from "$lib/api";
  import type { MonitorInfo } from "$lib/bindings/MonitorInfo";
  import Editor from "$lib/components/Editor.svelte";
  import Home from "$lib/components/Home.svelte";
  import OutputDialog from "$lib/components/OutputDialog.svelte";
  import Presenting from "$lib/components/Presenting.svelte";
  import { open } from "$lib/show.svelte";

  type Screen = "loading" | "home" | "editor" | "presenting";
  let screen = $state<Screen>("loading");
  let choosing = $state<MonitorInfo[] | null>(null);
  let notice = $state<string | null>(null);
  let error = $state<string | null>(null);
  let syncOffsetMs = $state(0);

  $effect(() => {
    api.showState().then((state) => {
      open.take(state);
      screen = state ? "editor" : "home";
    });
  });

  async function home() {
    await api.closeProject();
    open.take(null);
    screen = "home";
  }

  /// "▶ 上映する": straight to the remembered screen, or after asking.
  async function present(choose: boolean) {
    error = null;
    try {
      const [monitors, settings] = await Promise.all([api.listMonitors(), api.getSettings()]);
      syncOffsetMs = settings.syncOffsetMs;
      const remembered = monitors.find((m) => m.name === settings.outputScreen);
      if (!choose && remembered) {
        await start(remembered, false);
      } else if (!choose && monitors.length === 1) {
        // With one screen there is nothing to choose, but the operator must
        // know how to get back (docs/ui-draft.md, C-3).
        notice = "この画面に全画面で映します。終了するにはEscキーを押してください。";
        setTimeout(() => {
          notice = null;
          start(monitors[0], false);
        }, 2500);
      } else {
        choosing = monitors;
      }
    } catch (err) {
      error = String(err);
    }
  }

  async function start(monitor: MonitorInfo, remember: boolean) {
    choosing = null;
    try {
      if (remember) await api.setOutputScreen(monitor.name);
      await api.startPresentation(monitor.index);
      screen = "presenting";
    } catch (err) {
      error = `上映を始められませんでした: ${err}`;
    }
  }
</script>

{#if screen === "home"}
  <Home onopened={() => (screen = "editor")} />
{:else if screen === "editor"}
  <Editor onhome={home} onpresent={present} />
{:else if screen === "presenting"}
  <Presenting {syncOffsetMs} onend={() => (screen = "editor")} />
{/if}

{#if choosing}
  <OutputDialog monitors={choosing} onstart={start} oncancel={() => (choosing = null)} />
{/if}

{#if notice || error}
  <div class="toast" class:error={!!error} role="status">
    {notice ?? error}
    {#if error}<button class="quiet" onclick={() => (error = null)}>閉じる</button>{/if}
  </div>
{/if}

<style>
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
