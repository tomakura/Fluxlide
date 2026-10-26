<script lang="ts">
  // D. 上映中の操作: shown on the operator's screen while the presentation
  // window shows only the pictures (docs/ui-draft.md).
  import { untrack } from "svelte";
  import * as api from "$lib/api";
  import type { Control } from "$lib/bindings/Control";
  import type { PresentationStatus } from "$lib/bindings/PresentationStatus";
  import type { SlideId } from "$lib/bindings/SlideId";
  import { clock, screenName, thumbnailUrl } from "$lib/format";
  import { open } from "$lib/show.svelte";
  import Confirm from "./Confirm.svelte";

  let { syncOffsetMs, onend }: { syncOffsetMs: number; onend: () => void } = $props();

  let status = $state<PresentationStatus | null>(null);
  let saveError = $state<string | null>(null);
  let confirming = $state(false);
  let offset = $state(untrack(() => syncOffsetMs));
  let volume = $state<number | null>(null);

  const playback = $derived(status?.stats.playback ?? null);
  const position = $derived(playback?.position ?? null);
  const music = $derived(playback?.bgm ?? null);
  const screen = $derived(status?.monitorName ? screenName(status.monitorName) : null);

  $effect(() => {
    let stopped = false;
    const poll = async () => {
      if (stopped) return;
      try {
        status = await api.presentationStatus();
      } catch {
        // The next poll tries again.
      }
      setTimeout(poll, 250);
    };
    poll();
    const ended = api.onPresentationEnded(onend);
    const requested = api.onPresentationEndRequested(() => (confirming = true));
    return () => {
      stopped = true;
      ended.then((stop) => stop());
      requested.then((stop) => stop());
    };
  });

  const send = (control: Control) => api.controlPresentation(control);

  async function end() {
    confirming = false;
    await api.stopPresentation();
    onend();
  }

  function setOffset(ms: number) {
    offset = Math.max(-500, Math.min(1000, Math.round(ms)));
    send({ kind: "syncOffset", ms: offset });
    api.setSyncOffset(offset).catch((err) => (saveError = `音ズレ補正を保存できません: ${err}`));
  }

  function onkeydown(event: KeyboardEvent) {
    if (confirming || (event.target as HTMLElement).closest("input")) return;
    const controls: Record<string, Control> = {
      ArrowRight: { kind: "next" },
      PageDown: { kind: "next" },
      ArrowLeft: { kind: "previous" },
      PageUp: { kind: "previous" },
      " ": { kind: position?.beforeStart ? "start" : "togglePause" },
      b: { kind: "toggleBlackout" },
      B: { kind: "toggleBlackout" },
    };
    if (event.key === "Escape") {
      confirming = true;
    } else if (controls[event.key]) {
      send(controls[event.key]);
    } else {
      return;
    }
    event.preventDefault();
  }

  function card(id: SlideId | null | undefined) {
    const slide = id == null ? undefined : open.slide(id);
    const asset = slide ? open.asset(slide.asset) : undefined;
    const mediaDir = open.state?.mediaDir ?? "";
    return {
      number: slide ? (open.show?.slides.indexOf(slide) ?? 0) + 1 : null,
      title: asset ? asset.meta.title || asset.originalName : "",
      thumbnail: asset?.state.state === "ready" ? thumbnailUrl(mediaDir, asset) : null,
    };
  }
</script>

<svelte:window {onkeydown} />

<div class="presenting">
  <header>
    <span class="live-tag">● 上映中</span>
    {#if screen}<span>出力先: {screen}</span>{/if}
    <span class="time">経過 {clock(playback?.elapsedSecs ?? 0)}</span>
    <button class="end" onclick={() => (confirming = true)}>■ 上映を終了</button>
  </header>

  {#if status?.screenLost}
    <p class="lost" role="alert">
      出力先の画面（{status.monitorName}）が見つからないので、一時停止しました。つなぎ直すと、その画面に戻ります。続けるには「再開」を押してください。
    </p>
  {/if}

  <main>
    <section class="now">
      <h2>
        {#if position?.beforeStart}
          開始前の画面を表示しています
        {:else if position?.atEnd}
          終了の画面を表示しています
        {:else if position?.blackout}
          暗転しています
        {:else if position?.paused}
          一時停止しています
        {:else}
          いま映しているスライド
        {/if}
      </h2>
      {#if position?.beforeStart}
        <div class="big start">
          <p>{open.show?.title}</p>
          <button class="live startbutton" onclick={() => send({ kind: "start" })}>▶ スライドを始める</button>
          <p class="hint">Space か → キーでも始められます</p>
        </div>
      {:else}
        {@const current = card(position?.current)}
        <figure class="big" class:dim={position?.blackout}>
          {#if current.thumbnail}<img src={current.thumbnail} alt="" />{/if}
          <figcaption>{current.number ?? ""}　{current.title}</figcaption>
        </figure>
      {/if}
    </section>

    <section class="upcoming">
      <h2>次のスライド</h2>
      {#if position && position.upcoming.length}
        {@const next = card(position.upcoming[0])}
        <figure class="next">
          {#if next.thumbnail}<img src={next.thumbnail} alt="" />{/if}
          <figcaption>{next.number}　{next.title}</figcaption>
        </figure>
        <h3>この後</h3>
        <ol>
          {#each position.upcoming.slice(1) as id, i (i)}
            {@const later = card(id)}
            <li>
              {#if later.thumbnail}<img src={later.thumbnail} alt="" />{/if}
              <span>{later.number}　{later.title}</span>
            </li>
          {/each}
        </ol>
      {:else}
        <p class="hint">このあとのスライドはありません。</p>
      {/if}
    </section>
  </main>

  <footer>
    <div class="transport">
      <button onclick={() => send({ kind: "previous" })} disabled={position?.beforeStart}>◀ 前へ</button>
      <button class="live" onclick={() => send({ kind: position?.beforeStart ? "start" : "togglePause" })}>
        {position?.paused ? "▶ 再開" : position?.beforeStart ? "▶ 始める" : "❚❚ 一時停止"}
      </button>
      <button onclick={() => send({ kind: "next" })}>次へ ▶</button>
      <button class:on={position?.blackout} onclick={() => send({ kind: "toggleBlackout" })}>
        {position?.blackout ? "暗転を戻す" : "暗転"}
      </button>
    </div>

    <div class="sound">
      {#if music}
        <span class="song" title={music.artist ?? ""}>♪ {music.title ?? "曲を準備しています"}</span>
        <button onclick={() => send({ kind: "nextTrack" })}>次の曲</button>
        <label class="inline">
          音量 {Math.round((volume ?? music.volume) * 100)}%
          <input
            type="range"
            min="0"
            max="1"
            step="0.05"
            value={volume ?? music.volume}
            oninput={(e) => {
              volume = Number(e.currentTarget.value);
              send({ kind: "musicVolume", volume });
            }}
          />
        </label>
      {:else if playback?.bgmError}
        <span class="error" title={playback.bgmError}>BGMを再生できません</span>
      {/if}
      <!-- Not a <label>: clicking its text would press the first button. -->
      <div class="offset" role="group" aria-labelledby="offset-name">
        <span id="offset-name">音ズレ補正</span>
        <button onclick={() => setOffset(offset - 10)}>−10</button>
        <input
          type="number"
          min="-500"
          max="1000"
          step="10"
          aria-labelledby="offset-name"
          value={offset}
          onchange={(e) => setOffset(Number(e.currentTarget.value))}
        />
        <span>ミリ秒</span>
        <button onclick={() => setOffset(offset + 10)}>＋10</button>
      </div>
    </div>

    <p class="keys hint">
      ← → 前へ・次へ　Space 一時停止　B 暗転　Esc 上映を終了
      {#if playback?.lastError}<span class="error">　{playback.lastError}</span>{/if}
      {#if saveError}<span class="error">　{saveError}</span>{/if}
    </p>
  </footer>
</div>

{#if confirming}
  <Confirm
    title="上映を終了しますか？"
    message="映している画面が閉じて、編集画面に戻ります。"
    confirmLabel="上映を終了"
    onconfirm={end}
    oncancel={() => (confirming = false)}
  />
{/if}

<style>
  .lost {
    margin: 0;
    padding: 10px 16px;
    background: var(--err-soft);
    color: var(--err);
  }

  .presenting {
    height: 100vh;
    display: flex;
    flex-direction: column;
  }

  header {
    display: flex;
    align-items: center;
    gap: 20px;
    padding: 10px 18px;
    background: var(--live);
    color: var(--on-live);
  }

  .live-tag {
    font-weight: 700;
  }

  .time {
    font-variant-numeric: tabular-nums;
  }

  .end {
    margin-left: auto;
    background: var(--on-live);
    color: var(--live);
    border: none;
    font-weight: 700;
  }

  main {
    flex: 1;
    min-height: 0;
    display: grid;
    grid-template-columns: 2fr 1fr;
    gap: 24px;
    padding: 20px 24px;
  }

  h2 {
    margin: 0 0 10px;
    font-size: 14px;
    color: var(--muted);
    font-weight: 600;
  }

  h3 {
    margin: 16px 0 6px;
    font-size: 13px;
    color: var(--muted);
  }

  section {
    min-height: 0;
    display: flex;
    flex-direction: column;
  }

  figure {
    margin: 0;
  }

  .big {
    aspect-ratio: 16 / 9;
    background: #000;
    border-radius: 8px;
    overflow: hidden;
    position: relative;
    max-height: 100%;
  }

  .big img,
  .next img {
    width: 100%;
    height: 100%;
    object-fit: contain;
    display: block;
  }

  .big img {
    transition: opacity 0.4s ease-in-out;
  }

  .big.dim img {
    opacity: 0.15;
  }

  figcaption {
    position: absolute;
    left: 0;
    right: 0;
    bottom: 0;
    padding: 8px 12px;
    background: linear-gradient(transparent, rgb(0 0 0 / 70%));
    color: #fff;
  }

  .start {
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: 12px;
    color: #fff;
  }

  .start p {
    margin: 0;
  }

  .startbutton {
    font-size: 20px;
    padding: 14px 28px;
  }

  .next {
    aspect-ratio: 16 / 9;
    background: #000;
    border-radius: 6px;
    overflow: hidden;
    position: relative;
  }

  ol {
    list-style: none;
    padding: 0;
    margin: 0;
    overflow-y: auto;
    display: flex;
    flex-direction: column;
    gap: 6px;
  }

  ol li {
    display: flex;
    align-items: center;
    gap: 10px;
  }

  ol img {
    width: 64px;
    aspect-ratio: 16 / 9;
    object-fit: cover;
    border-radius: 3px;
    background: #000;
  }

  footer {
    border-top: 1px solid var(--line);
    padding: 14px 24px;
    display: flex;
    flex-direction: column;
    gap: 12px;
    background: var(--surface);
  }

  .transport {
    display: flex;
    gap: 10px;
    justify-content: center;
  }

  .transport button {
    font-size: 16px;
    padding: 12px 26px;
    min-width: 140px;
  }

  .transport .on {
    background: var(--fg);
    color: var(--bg);
  }

  .sound {
    display: flex;
    align-items: center;
    justify-content: center;
    gap: 16px;
    flex-wrap: wrap;
  }

  .song {
    max-width: 280px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .offset {
    display: flex;
    align-items: center;
    gap: 8px;
  }

  .offset input {
    width: 80px;
  }

  .keys {
    margin: 0;
    text-align: center;
  }
</style>
