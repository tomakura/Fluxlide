<script lang="ts">
  // ファイルごとの設定: the settings of one photo or video at a time. The
  // list only chooses; slides are added, moved and removed in the editor.
  import type { SlideId } from "$lib/bindings/SlideId";
  import { thumbnailUrl } from "$lib/format";
  import { appear } from "$lib/motion";
  import { open } from "$lib/show.svelte";
  import Preview from "./Preview.svelte";
  import SlidePanel from "./SlidePanel.svelte";

  let { selected = $bindable(), onback }: { selected: SlideId | null; onback: () => void } = $props();

  const show = $derived(open.show);
  const mediaDir = $derived(open.state?.mediaDir ?? "");

  function onkeydown(event: KeyboardEvent) {
    if (!show || (event.key !== "ArrowUp" && event.key !== "ArrowDown")) return;
    if ((event.target as HTMLElement).closest("input, textarea, select")) return;
    const index = show.slides.findIndex((slide) => slide.id === selected);
    const next = show.slides[index + (event.key === "ArrowUp" ? -1 : 1)];
    if (next) selected = next.id;
    event.preventDefault();
  }
</script>

<svelte:window {onkeydown} />

{#if show}
  <div class="screen" in:appear|global>
    <header>
      <button class="quiet" onclick={onback} title="編集に戻る">← 編集に戻る</button>
      <h1>ファイルごとの設定</h1>
    </header>

    <div class="columns">
      <aside class="left">
        <ol aria-label="スライドの並び">
          {#each show.slides as slide, index (slide.id)}
            {@const asset = open.asset(slide.asset)}
            <li class:selected={slide.id === selected}>
              <button class="row quiet" onclick={() => (selected = slide.id)}>
                <span class="number">{index + 1}</span>
                <span class="thumb">
                  {#if asset?.state.state === "ready"}
                    <img src={thumbnailUrl(mediaDir, asset)} alt="" loading="lazy" />
                  {/if}
                </span>
                <span class="title">{asset?.meta.title || asset?.originalName || "（見つかりません）"}</span>
              </button>
            </li>
          {/each}
        </ol>
      </aside>

      <section class="center">
        <div class="stage">
          <Preview {selected} />
        </div>
      </section>

      <aside class="right">
        <div class="settings">
          <SlidePanel {selected} />
        </div>
      </aside>
    </div>
  </div>
{/if}

<style>
  .screen {
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

  h1 {
    margin: 0;
    font-size: 16px;
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
    overflow-y: auto;
  }

  ol {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 4px;
  }

  li {
    border-radius: 8px;
    border: 1px solid transparent;
  }

  li.selected {
    background: var(--accent-soft);
    border-color: var(--accent);
  }

  .row {
    width: 100%;
    display: grid;
    grid-template-columns: 28px 72px 1fr;
    gap: 8px;
    align-items: center;
    padding: 6px 8px 6px 4px;
    text-align: left;
  }

  .number {
    text-align: center;
    color: var(--muted);
    font-variant-numeric: tabular-nums;
  }

  .thumb {
    width: 72px;
    aspect-ratio: 16 / 9;
    background: #000;
    border-radius: 4px;
    overflow: hidden;
  }

  .thumb img {
    width: 100%;
    height: 100%;
    object-fit: cover;
    display: block;
  }

  .title {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .center {
    padding: 20px;
    min-width: 0;
    overflow-y: auto;
  }

  .stage {
    width: min(100%, calc((100vh - 140px) * 16 / 9));
    margin: 0 auto;
  }

  .right {
    min-height: 0;
    border-left: 1px solid var(--line);
    background: var(--surface);
  }

  .settings {
    height: 100%;
    overflow-y: auto;
    padding: 14px;
  }
</style>
