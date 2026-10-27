<script lang="ts">
  // ファイル and スライド tabs: the photo or video of the selected slide, and
  // how that slide shows it. Settings left empty follow the 全体の見た目 tab.
  import type { Fit } from "$lib/bindings/Fit";
  import type { Motion } from "$lib/bindings/Motion";
  import type { ShowDefinition } from "$lib/bindings/ShowDefinition";
  import type { Slide } from "$lib/bindings/Slide";
  import type { SlideId } from "$lib/bindings/SlideId";
  import { FITS, ZOOMS, effective, ownLength, seconds, transitionName, zoomOf } from "$lib/format";
  import { open } from "$lib/show.svelte";
  import TransitionSelect from "./TransitionSelect.svelte";

  // `file`: what belongs to the photo or video itself, wherever it is shown.
  // `slide`: how the selected slide shows it.
  let { selected, part }: { selected: SlideId | null; part: "file" | "slide" } = $props();

  const show = $derived(open.show);
  const slide = $derived(selected === null ? undefined : open.slide(selected));
  const asset = $derived(slide ? open.asset(slide.asset) : undefined);

  /// Changes the selected slide.
  function setSlide(change: (slide: Slide, show: ShowDefinition) => void, group: string | null = null) {
    const id = selected;
    open.edit((show) => {
      const slide = show.slides.find((slide) => slide.id === id);
      if (slide) change(slide, show);
    }, group && `slide:${id}:${group}`);
  }

  function setMeta(field: "title" | "author" | "description", value: string) {
    const id = asset?.id;
    open.edit((show) => {
      const asset = show.assets.find((asset) => asset.id === id);
      if (asset) asset.meta[field] = value;
    }, `asset:${id}:${field}`);
  }

  /// Seconds typed into a field, as whole milliseconds; empty for `null`.
  const ms = (value: string) => (value.trim() === "" ? null : Math.max(0, Math.round(Number(value) * 1000)));
  const secs = (ms: number | null) => (ms === null ? "" : String(ms / 1000));

  /// A position as a signed percentage, "+12%".
  const percent = (value: number) => `${value > 0 ? "+" : ""}${Math.round(value * 100)}%`;

  const ZOOM_NAMES = { in: "ズームイン", out: "ズームアウト", still: "なし" } as const;
  const zoomName = (motion: Motion | null) => {
    const key = zoomOf(motion);
    return key === "none" ? "なし" : ZOOM_NAMES[key];
  };
</script>

{#if show && slide && asset}
  {@const uses = effective(slide, show)}
  <div class="panel">
    {#if part === "file"}
      <h3>{asset.kind === "video" ? "動画" : "写真"}の情報</h3>
      <p class="hint file">{asset.originalName}</p>
      {#if asset.kind === "video"}
        <p class="hint file">動画の長さ: {asset.durationMs === null ? "不明" : seconds(asset.durationMs)}</p>
      {/if}

      <label>
        タイトル
        <input value={asset.meta.title} oninput={(e) => setMeta("title", e.currentTarget.value)} />
      </label>
      <label>
        作者
        <input value={asset.meta.author} oninput={(e) => setMeta("author", e.currentTarget.value)} />
      </label>
      <label>
        説明
        <textarea
          rows="3"
          value={asset.meta.description}
          oninput={(e) => setMeta("description", e.currentTarget.value)}
        ></textarea>
      </label>
      <p class="hint">キャプションに出す文字です。出すかどうかは「スライド」タブで決めます。</p>
    {:else}
    <h3>このスライドの見せ方</h3>
    <p class="hint file">{asset.originalName}</p>

    <label class="inline">
      <input
        type="checkbox"
        checked={slide.showCaption}
        onchange={(e) => {
          const on = e.currentTarget.checked;
          setSlide((slide) => (slide.showCaption = on));
        }}
      />
      この写真にキャプションを出す
    </label>
    <p class="hint caption-note">出す文字は「ファイル」タブ、デザインは「全体の見た目」で決めます。</p>

    {#if asset.kind === "photo"}
      <label>
        表示時間（秒）
        <input
          type="number"
          min="0.5"
          step="0.5"
          placeholder={`${show.look.photoMs / 1000}（見た目の設定）`}
          value={secs(slide.durationMs)}
          oninput={(e) => {
            const value = ms(e.currentTarget.value);
            setSlide((slide) => (slide.durationMs = value), "duration");
          }}
        />
      </label>
    {/if}

    <label>
      このスライドに入るトランジション
      <TransitionSelect
        value={slide.transition}
        {show}
        followLabel={`見た目の設定に従う（${transitionName(show.look.transition, show)}）`}
        onchange={(choice) => setSlide((slide) => (slide.transition = choice))}
      />
    </label>

    {#if asset.kind === "video"}
      <fieldset>
        <legend>再生する範囲</legend>
        <div class="pair">
          <label>
            開始（秒）
            <input
              type="number"
              min="0"
              step="0.1"
              value={secs(slide.video.startMs)}
              oninput={(e) => {
                const value = ms(e.currentTarget.value) ?? 0;
                setSlide((slide) => (slide.video.startMs = value), "start");
              }}
            />
          </label>
          <label>
            終了（秒）
            <input
              type="number"
              min="0"
              step="0.1"
              placeholder="最後まで"
              value={secs(slide.video.endMs)}
              oninput={(e) => {
                const value = ms(e.currentTarget.value);
                setSlide((slide) => (slide.video.endMs = value), "end");
              }}
            />
          </label>
        </div>
        {#if asset.hasAudio}
          <label>
            動画の音量 {Math.round(slide.video.volume * 100)}%
            <input
              type="range"
              min="0"
              max="1"
              step="0.05"
              value={slide.video.volume}
              oninput={(e) => {
                const value = Number(e.currentTarget.value);
                setSlide((slide) => (slide.video.volume = value), "volume");
              }}
            />
          </label>
        {/if}
      </fieldset>
    {/if}

    <details>
      <summary>詳細設定</summary>
      <div class="details">
        <label>
          トランジションの長さ（秒）
          <input
            type="number"
            min="0"
            step="0.1"
            placeholder={show.look.transitionMs === null
              ? `${ownLength(uses.transition) / 1000}（${transitionName(uses.transition, show)}の長さ）`
              : `${show.look.transitionMs / 1000}（見た目の設定）`}
            disabled={uses.transition.kind === "cut"}
            value={secs(slide.transitionMs)}
            oninput={(e) => {
              const value = ms(e.currentTarget.value);
              setSlide((slide) => (slide.transitionMs = value), "transitionMs");
            }}
          />
        </label>
        <label>
          ゆっくりズーム
          <select
            value={zoomOf(slide.motion)}
            onchange={(e) => {
              const key = e.currentTarget.value as keyof typeof ZOOMS | "none";
              const motion = key === "none" ? null : ZOOMS[key];
              setSlide((slide) => (slide.motion = motion));
            }}
          >
            <option value="none">
              見た目の設定に従う（{zoomName(show.look.motion)}）
            </option>
            {#each Object.entries(ZOOM_NAMES) as [key, name] (key)}
              <option value={key}>{name}</option>
            {/each}
          </select>
        </label>
        <label>
          縦横比が合わないとき
          <select
            value={slide.framing.fit ?? ""}
            onchange={(e) => {
              const value = e.currentTarget.value;
              setSlide((slide) => (slide.framing.fit = value === "" ? null : (value as Fit)));
            }}
          >
            <option value="">見た目の設定に従う（{FITS[show.look.fit]}）</option>
            {#each Object.entries(FITS) as [fit, name] (fit)}
              <option value={fit}>{name}</option>
            {/each}
          </select>
        </label>
        <label>
          拡大 {Math.round(slide.framing.scale * 100)}%
          <input
            type="range"
            min="1"
            max="3"
            step="0.05"
            value={slide.framing.scale}
            oninput={(e) => {
              const value = Number(e.currentTarget.value);
              setSlide((slide) => (slide.framing.scale = value), "scale");
            }}
          />
        </label>
        <div class="pair">
          <label>
            左右の位置 {percent(slide.framing.offset[0])}
            <input
              type="range"
              min="-0.5"
              max="0.5"
              step="0.01"
              value={slide.framing.offset[0]}
              oninput={(e) => {
                const value = Number(e.currentTarget.value);
                setSlide((slide) => (slide.framing.offset[0] = value), "offsetX");
              }}
            />
          </label>
          <label>
            上下の位置 {percent(slide.framing.offset[1])}
            <input
              type="range"
              min="-0.5"
              max="0.5"
              step="0.01"
              value={slide.framing.offset[1]}
              oninput={(e) => {
                const value = Number(e.currentTarget.value);
                setSlide((slide) => (slide.framing.offset[1] = value), "offsetY");
              }}
            />
          </label>
        </div>
        <button
          class="quiet"
          onclick={() =>
            setSlide((slide) => {
              slide.framing = { fit: slide.framing.fit, scale: 1, offset: [0, 0] };
            })}>位置と拡大を元に戻す</button
        >
      </div>
    </details>
    {/if}
  </div>
{:else}
  <p class="hint empty">左の並びからスライドを選ぶと、ここで設定できます。</p>
{/if}

<style>
  .panel {
    display: flex;
    flex-direction: column;
    gap: 14px;
  }

  h3 {
    margin: 0;
    font-size: 15px;
  }

  .caption-note {
    margin: -8px 0 0;
  }

  .file {
    margin: -10px 0 0;
    overflow-wrap: anywhere;
  }

  fieldset {
    border: 1px solid var(--line);
    border-radius: 8px;
    padding: 10px 12px 12px;
    margin: 0;
    display: flex;
    flex-direction: column;
    gap: 10px;
  }

  legend {
    font-size: 13px;
    padding: 0 4px;
  }

  .pair {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 10px;
  }

  details summary {
    cursor: pointer;
    color: var(--muted);
  }

  .details {
    display: flex;
    flex-direction: column;
    gap: 14px;
    padding-top: 12px;
  }

  .empty {
    margin-top: 40px;
    text-align: center;
  }
</style>
