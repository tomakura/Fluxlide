<script lang="ts">
  // The slides in playing order (left column of B. 編集). Slides are moved
  // by dragging their handle; pointer events are used because the webview's
  // file drop handling takes over HTML drag and drop on Windows.
  import { open as pickFiles } from "@tauri-apps/plugin-dialog";
  import * as api from "$lib/api";
  import type { SlideId } from "$lib/bindings/SlideId";
  import { MEDIA_FILTER, effective, slideLength, thumbnailUrl, transitionName } from "$lib/format";
  import { flip } from "svelte/animate";
  import { appear, moving, rise } from "$lib/motion";
  import { open } from "$lib/show.svelte";
  import { removeSlide } from "$lib/edits";
  import DropZone from "./DropZone.svelte";

  let {
    selected = $bindable(),
    playing = null,
  }: { selected: SlideId | null; playing?: SlideId | null } = $props();

  const show = $derived(open.show);
  const mediaDir = $derived(open.state?.mediaDir ?? "");

  // Dragging: the slide being moved, and the row it would go before
  // (the number of rows for the end), counted with it still in the list.
  let dragging = $state<{ id: SlideId; to: number } | null>(null);
  let list = $state<HTMLOListElement | null>(null);

  async function add() {
    const files = await pickFiles({ multiple: true, filters: [MEDIA_FILTER] });
    if (files?.length) addFiles(files);
  }

  async function addFiles(files: string[]) {
    const before = show?.slides.length ?? 0;
    await open.import(files);
    // Select the first slide added.
    selected = open.show?.slides[before]?.id ?? selected;
  }

  function startDrag(event: PointerEvent, id: SlideId, index: number) {
    (event.currentTarget as HTMLElement).setPointerCapture(event.pointerId);
    dragging = { id, to: index };
    selected = id;
  }

  function moveDrag(event: PointerEvent) {
    if (!dragging || !list) return;
    const rows = [...list.querySelectorAll<HTMLElement>(":scope > li")];
    let to = rows.length;
    for (const [index, row] of rows.entries()) {
      const rect = row.getBoundingClientRect();
      if (event.clientY < rect.top + rect.height / 2) {
        to = index;
        break;
      }
    }
    dragging.to = to;
  }

  function endDrag() {
    if (!dragging || !show) return;
    const { id, to } = dragging;
    dragging = null;
    const from = show.slides.findIndex((slide) => slide.id === id);
    // Taking the slide out moves the rows after it up by one.
    const index = to > from ? to - 1 : to;
    if (from >= 0 && index !== from) moveTo(id, index);
  }

  function moveTo(id: SlideId, to: number) {
    open.edit((show) => {
      const from = show.slides.findIndex((slide) => slide.id === id);
      if (from < 0) return;
      const [slide] = show.slides.splice(from, 1);
      show.slides.splice(Math.max(0, Math.min(to, show.slides.length)), 0, slide);
    });
  }

  function onkeydown(event: KeyboardEvent) {
    if (!show || selected === null) return;
    const index = show.slides.findIndex((slide) => slide.id === selected);
    if (index < 0) return;
    if (event.altKey && (event.key === "ArrowUp" || event.key === "ArrowDown")) {
      moveTo(selected, index + (event.key === "ArrowUp" ? -1 : 1));
      event.preventDefault();
    } else if (event.key === "ArrowUp" || event.key === "ArrowDown") {
      const next = show.slides[index + (event.key === "ArrowUp" ? -1 : 1)];
      if (next) selected = next.id;
      event.preventDefault();
    } else if (event.key === "Delete" || event.key === "Backspace") {
      remove(selected);
      event.preventDefault();
    }
  }

  function remove(id: SlideId) {
    if (!show) return;
    const index = show.slides.findIndex((slide) => slide.id === id);
    const neighbour = show.slides[index + 1] ?? show.slides[index - 1];
    if (selected === id) selected = neighbour?.id ?? null;
    open.edit((show) => removeSlide(show, id));
  }

  async function repick(asset: number) {
    const file = await pickFiles({ multiple: false, filters: [MEDIA_FILTER] });
    if (typeof file === "string") open.run(() => api.replaceAssetFile(asset, file));
  }
</script>

<div class="column">
  <div class="head">
    <span class="hint">{show?.slides.length ?? 0}枚</span>
    <button onclick={add}>＋ 追加</button>
  </div>

  {#if show}
    <!-- The list takes the arrow and Delete keys for the selected slide. -->
    <!-- svelte-ignore a11y_no_noninteractive_tabindex, a11y_no_noninteractive_element_interactions -->
    <ol bind:this={list} tabindex="0" {onkeydown} aria-label="スライドの並び">
      {#each show.slides as slide, index (slide.id)}
        {@const asset = open.asset(slide.asset)}
        {@const copying = asset ? open.copying[asset.id] : undefined}
        <li
          animate:flip={moving()}
          in:rise
          out:appear={{ duration: 140 }}
          class:selected={slide.id === selected}
          class:playing={slide.id === playing}
          class:drop-before={dragging && dragging.to === index && dragging.id !== slide.id}
          class:drop-after={dragging && dragging.to === show.slides.length && index === show.slides.length - 1}
        >
          <button class="row quiet" onclick={() => (selected = slide.id)}>
            <span
              class="handle"
              role="presentation"
              title="ドラッグして並べ替え"
              onpointerdown={(event) => startDrag(event, slide.id, index)}
              onpointermove={moveDrag}
              onpointerup={endDrag}
              onpointercancel={() => (dragging = null)}>{index + 1}</span
            >
            <span class="thumb">
              {#if asset?.state.state === "ready"}
                <img src={thumbnailUrl(mediaDir, asset)} alt="" loading="lazy" />
              {:else if asset?.state.state === "importing"}
                <span class="badge">取り込み中</span>
              {:else}
                <span class="badge failed">！</span>
              {/if}
            </span>
            <span class="text">
              <span class="title">{asset?.meta.title || asset?.originalName || "（見つかりません）"}</span>
              <span class="hint">
                {slideLength(slide, asset, show)} · {transitionName(effective(slide, show).transition, show)}
              </span>
              {#if asset?.state.state === "importing"}
                <progress max={copying?.total ?? 1} value={copying?.copied ?? 0}></progress>
              {/if}
            </span>
          </button>
          {#if asset?.state.state === "failed"}
            <div class="failed-row">
              <p class="error">{asset.state.reason}</p>
              <div class="actions">
                <button onclick={() => repick(asset.id)}>選び直す</button>
                <button onclick={() => remove(slide.id)}>外す</button>
              </div>
            </div>
          {:else if slide.id === selected}
            <button class="remove quiet" title="このスライドを外す（Delete）" onclick={() => remove(slide.id)}>✕</button>
          {/if}
        </li>
      {/each}
    </ol>
  {/if}

  <DropZone ondrop={addFiles}>写真や動画をここにドロップして追加</DropZone>
</div>

<style>
  .column {
    display: flex;
    flex-direction: column;
    gap: 10px;
    min-height: 0;
    height: 100%;
  }

  .head {
    display: flex;
    justify-content: space-between;
    align-items: center;
  }

  ol {
    list-style: none;
    margin: 0;
    padding: 2px;
    overflow-y: auto;
    flex: 1;
    display: flex;
    flex-direction: column;
    gap: 4px;
  }

  ol:focus-visible {
    outline: 2px solid var(--accent);
  }

  li {
    position: relative;
    border-radius: 8px;
    border: 1px solid transparent;
  }

  li.selected {
    background: var(--accent-soft);
    border-color: var(--accent);
  }

  li.playing::after {
    content: "";
    position: absolute;
    left: -2px;
    top: 6px;
    bottom: 6px;
    width: 4px;
    border-radius: 2px;
    background: var(--live);
  }

  li.drop-before::before,
  li.drop-after::after {
    content: "";
    position: absolute;
    left: 0;
    right: 0;
    top: -3px;
    height: 2px;
    background: var(--accent);
  }

  li.drop-after::after {
    top: auto;
    bottom: -3px;
  }

  .row {
    width: 100%;
    display: grid;
    grid-template-columns: 28px 72px 1fr;
    gap: 8px;
    align-items: center;
    padding: 6px 28px 6px 4px;
    text-align: left;
  }

  .handle {
    cursor: grab;
    text-align: center;
    color: var(--muted);
    font-variant-numeric: tabular-nums;
    touch-action: none;
    padding: 10px 0;
  }

  .thumb {
    width: 72px;
    aspect-ratio: 16 / 9;
    background: #000;
    border-radius: 4px;
    overflow: hidden;
    display: flex;
    align-items: center;
    justify-content: center;
  }

  .thumb img {
    width: 100%;
    height: 100%;
    object-fit: cover;
  }

  .badge {
    font-size: 10px;
    color: #fff;
  }

  .badge.failed {
    color: var(--err);
    font-weight: 700;
    font-size: 16px;
  }

  .text {
    display: flex;
    flex-direction: column;
    gap: 2px;
    min-width: 0;
  }

  .title {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  progress {
    width: 100%;
    height: 4px;
  }

  .remove {
    position: absolute;
    right: 2px;
    top: 50%;
    transform: translateY(-50%);
    padding: 2px 7px;
    color: var(--muted);
  }

  .failed-row {
    padding: 0 8px 8px 40px;
    display: flex;
    flex-direction: column;
    gap: 6px;
  }

  .failed-row p {
    margin: 0;
    font-size: 12px;
  }

  .actions {
    display: flex;
    gap: 6px;
  }

  .actions button {
    padding: 4px 10px;
    font-size: 12px;
  }
</style>
