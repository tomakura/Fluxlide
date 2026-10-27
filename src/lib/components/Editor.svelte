<script lang="ts">
  // B. 編集: the slides on the left, the preview in the middle and the
  // settings of the chosen tab on the right (docs/ui-draft.md).
  import { untrack } from "svelte";
  import type { SlideId } from "$lib/bindings/SlideId";
  import { exporting } from "$lib/exporting.svelte";
  import { appear, rise } from "$lib/motion";
  import { open } from "$lib/show.svelte";
  import ExportDialog from "./ExportDialog.svelte";
  import LookPanel from "./LookPanel.svelte";
  import MusicPanel from "./MusicPanel.svelte";
  import Preview from "./Preview.svelte";
  import SlideList from "./SlideList.svelte";
  import SlidePanel from "./SlidePanel.svelte";

  let {
    onhome,
    onpresent,
  }: {
    onhome: () => void;
    /// `choose`: show the output screen choice even when one is remembered.
    onpresent: (choose: boolean) => void;
  } = $props();

  type Tab = "slides" | "look" | "music";
  // The tabs say how far their settings reach: `slides` only the chosen
  // photo or video, the others the whole slideshow.
  const TABS: [Tab, string][] = [
    ["slides", "選んだ写真"],
    ["look", "全体の見た目"],
    ["music", "全体の音楽"],
  ];
  const SCOPES: Record<Tab, string> = {
    slides: "左で選んだ1枚だけの設定です",
    look: "すべてのスライドに共通の設定です",
    music: "スライドショー全体に流れる音楽です",
  };

  let tab = $state<Tab>("slides");
  let exportOpen = $state(false);
  // An export that ends while its dialog is closed says so there (once:
  // not again when the editor is opened later).
  let seenFinish = untrack(() => exporting.finished);
  $effect(() => {
    const finished = exporting.finished;
    if (finished && finished !== seenFinish && !finished.cancelled) exportOpen = true;
    seenFinish = finished;
  });
  let selected = $state<SlideId | null>(open.show?.slides[0]?.id ?? null);
  let playing = $state(false);
  let playingSlide = $state<SlideId | null>(null);

  const show = $derived(open.show);
  const saveError = $derived(open.state?.saveError ?? null);

  // A selection whose slide was removed (by undo, say) moves to the first.
  $effect(() => {
    if (show && (selected === null || !show.slides.some((slide) => slide.id === selected))) {
      selected = show.slides[0]?.id ?? null;
    }
  });

  const HINT_KEY = "fluxlide.hint.closed";
  let hintClosed = $state(readHintClosed());
  function readHintClosed() {
    try {
      return localStorage.getItem(HINT_KEY) === "1";
    } catch {
      return false;
    }
  }
  function closeHint() {
    hintClosed = true;
    try {
      localStorage.setItem(HINT_KEY, "1");
    } catch {
      // Remembering it is a convenience only.
    }
  }

  const hint = $derived.by(() => {
    if (!show) return "";
    if (show.slides.length === 0) return "「＋ 追加」を押すか、写真や動画を左の欄にドロップしてください。";
    if (show.assets.some((asset) => asset.state.state === "importing"))
      return "取り込み中でも、タイトルや説明を入力できます。";
    return "準備ができたら、右上の「▶ 上映する」を押してください。";
  });

  function onkeydown(event: KeyboardEvent) {
    const target = event.target as HTMLElement;
    if (target.closest("input, textarea, select")) return;
    const mod = event.ctrlKey || event.metaKey;
    if (mod && event.key.toLowerCase() === "z") {
      (event.shiftKey ? open.redo : open.undo)();
      event.preventDefault();
    } else if (mod && event.key.toLowerCase() === "y") {
      open.redo();
      event.preventDefault();
    }
  }
</script>

<svelte:window {onkeydown} />

{#snippet arrow()}
  <svg viewBox="0 0 16 16" width="15" height="15" aria-hidden="true">
    <path d="M5 4.5 2.5 7 5 9.5M3 7h7a3.5 3.5 0 0 1 0 7H8" />
  </svg>
{/snippet}

{#if show}
  <div class="editor" in:appear|global>
    <header>
      <button class="quiet" onclick={onhome} title="ホームに戻る">← ホーム</button>
      <input
        class="title"
        aria-label="スライドショーの名前"
        value={show.title}
        oninput={(e) => {
          const title = e.currentTarget.value;
          open.edit((show) => (show.title = title), "title");
        }}
      />
      <span class="saved" class:error={saveError} title={saveError ?? ""}>
        {saveError ? "保存できません" : "自動保存済み"}
      </span>
      <div class="history">
        <button class="quiet" disabled={!open.state?.canUndo} onclick={open.undo} title="元に戻す（Ctrl+Z）">
          {@render arrow()}元に戻す
        </button>
        <button class="quiet redo" disabled={!open.state?.canRedo} onclick={open.redo} title="やり直す（Ctrl+Shift+Z）">
          {@render arrow()}やり直す
        </button>
      </div>
      <button class="quiet export" disabled={show.slides.length === 0 && !exporting.running} onclick={() => (exportOpen = true)}>
        {exporting.running ? `書き出し中 ${Math.round(exporting.fraction * 100)}%` : "動画に書き出す"}
      </button>
      <div class="present">
        <button class="live" disabled={show.slides.length === 0} onclick={() => onpresent(false)}>▶ 上映する</button>
        <button class="live more" disabled={show.slides.length === 0} onclick={() => onpresent(true)} title="出力先の画面を選ぶ">▾</button>
      </div>
    </header>

    {#if exportOpen}
      <ExportDialog onclose={() => (exportOpen = false)} />
    {/if}

    {#if open.error}
      <p class="banner error" transition:rise>
        {open.error}
        <button class="quiet" onclick={() => (open.error = null)}>閉じる</button>
      </p>
    {/if}

    <div class="columns">
      <aside class="left">
        <SlideList bind:selected playing={playing ? playingSlide : null} />
      </aside>

      <section class="center">
        <div class="stage">
          <Preview {selected} bind:playing onslide={(slide) => (playingSlide = slide)} />
          <div class="controls">
            {#if playing}
              <button onclick={() => (playing = false)}>■ 止める</button>
            {:else}
              <button disabled={selected === null} onclick={() => (playing = true)}>▶ このスライドから再生</button>
            {/if}
          </div>
          {#if !hintClosed && hint}
            <p class="next" transition:rise>
              {hint}
              <button class="quiet" onclick={closeHint} title="案内を閉じる">✕</button>
            </p>
          {/if}
        </div>
      </section>

      <aside class="right">
        <nav>
          {#each TABS as [key, name] (key)}
            <button class="tab" class:current={tab === key} aria-pressed={tab === key} onclick={() => (tab = key)}>{name}</button>
          {/each}
        </nav>
        <p class="scope" class:whole={tab !== "slides"}>{SCOPES[tab]}</p>
        <div class="settings">
          {#key tab}
            <div in:appear>
              {#if tab === "slides"}
                <SlidePanel {selected} />
              {:else if tab === "look"}
                <LookPanel />
              {:else}
                <MusicPanel />
              {/if}
            </div>
          {/key}
        </div>
      </aside>
    </div>
  </div>
{/if}

<style>
  .editor {
    height: 100vh;
    display: flex;
    flex-direction: column;
  }

  header {
    display: flex;
    align-items: center;
    gap: 12px;
    padding: 10px 16px;
    border-bottom: 1px solid var(--line);
    background: var(--surface);
  }

  .title {
    flex: 0 1 260px;
    min-width: 120px;
    font-weight: 600;
    border-color: transparent;
    background: transparent;
  }

  .title:hover,
  .title:focus {
    border-color: var(--line);
  }

  .saved {
    font-size: 12px;
    color: var(--muted);
    white-space: nowrap;
  }

  .saved.error {
    color: var(--err);
  }

  .history {
    display: flex;
  }

  .history button {
    font-size: 13px;
    padding: 6px 8px;
    display: flex;
    align-items: center;
    gap: 4px;
    white-space: nowrap;
  }

  .history svg {
    fill: none;
    stroke: currentColor;
    stroke-width: 1.6;
    stroke-linecap: round;
    stroke-linejoin: round;
  }

  .redo svg {
    transform: scaleX(-1);
  }

  .export {
    margin-left: auto;
    white-space: nowrap;
  }

  nav {
    display: flex;
    gap: 2px;
    margin: 14px 14px 0;
    background: var(--surface-2);
    padding: 3px;
    border-radius: 9px;
  }

  .scope {
    margin: 10px 14px 0;
    padding: 6px 10px;
    border-radius: 6px;
    font-size: 12px;
    background: var(--accent-soft);
    color: var(--accent);
  }

  .scope.whole {
    background: var(--surface-2);
    color: var(--muted);
  }

  .tab {
    flex: 1;
    white-space: nowrap;
    border: none;
    background: transparent;
    padding: 6px 8px;
  }

  .tab.current {
    background: var(--surface);
    color: var(--accent);
    font-weight: 600;
    box-shadow: 0 1px 2px rgb(0 0 0 / 15%);
  }

  .present {
    display: flex;
    flex-shrink: 0;
  }

  .present button:first-child {
    border-radius: 8px 0 0 8px;
  }

  .more {
    border-radius: 0 8px 8px 0;
    border-left: 1px solid rgb(255 255 255 / 35%);
    padding: 7px 9px;
  }

  .banner {
    margin: 0;
    padding: 8px 16px;
    background: var(--err-soft);
    display: flex;
    justify-content: space-between;
    align-items: center;
  }

  .columns {
    flex: 1;
    min-height: 0;
    display: grid;
    grid-template-columns: 300px 1fr 320px;
  }

  .left {
    padding: 14px;
    min-height: 0;
    border-right: 1px solid var(--line);
    overflow: hidden;
  }

  .right {
    min-height: 0;
    display: flex;
    flex-direction: column;
    border-left: 1px solid var(--line);
    background: var(--surface);
  }

  .settings {
    flex: 1;
    min-height: 0;
    overflow-y: auto;
    padding: 14px;
  }

  .center {
    padding: 20px;
    min-width: 0;
    overflow-y: auto;
  }

  /* As wide as the column allows while the preview, its button and the
     hint still fit in the window's height. */
  .stage {
    width: min(100%, calc((100vh - 220px) * 16 / 9));
    margin: 0 auto;
    display: flex;
    flex-direction: column;
    gap: 12px;
  }

  .controls {
    display: flex;
    align-items: center;
    gap: 14px;
  }

  .next {
    margin: 0;
    padding: 10px 14px;
    border-radius: var(--radius);
    background: var(--accent-soft);
    display: flex;
    justify-content: space-between;
    align-items: center;
    gap: 10px;
  }

  .next button {
    padding: 2px 8px;
  }
</style>
