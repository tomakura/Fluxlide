<script lang="ts">
  // 見た目 tab: settings shared by every slide (docs/ui-draft.md, B).
  import { open as pickFile } from "@tauri-apps/plugin-dialog";
  import * as api from "$lib/api";
  import type { CaptionDesign } from "$lib/bindings/CaptionDesign";
  import type { Corner } from "$lib/bindings/Corner";
  import type { Fit } from "$lib/bindings/Fit";
  import type { Look } from "$lib/bindings/Look";
  import type { PartScales } from "$lib/bindings/PartScales";
  import type { WatermarkSpec } from "$lib/bindings/WatermarkSpec";
  import { CORNERS, FITS, PHOTO_EXTENSIONS, ZOOMS, ownLength, zoomOf } from "$lib/format";
  import { dropUnusedAsset, removeCustomTransition } from "$lib/edits";
  import { rise } from "$lib/motion";
  import { open } from "$lib/show.svelte";
  import TransitionSelect from "./TransitionSelect.svelte";

  const show = $derived(open.show);
  const look = $derived(show?.look);
  const watermark = $derived(look?.watermark ?? null);

  let transitionError = $state<string | null>(null);
  let adding = $state(false);

  function setLook(change: (look: Look) => void, group: string | null = null) {
    open.edit((show) => change(show.look), group && `look:${group}`);
  }

  // Width in pixels at a height of 1080; 0 turns the outline off.
  function setOutline(width: number) {
    if (Number.isFinite(width) && width >= 0 && width <= 20) {
      setLook((look) => (look.caption.outline = width), "outline");
    }
  }

  const DESIGNS: [CaptionDesign, string][] = [
    ["band", "下帯"],
    ["simple", "シンプル"],
    ["plain", "文字だけ"],
    ["card", "カード"],
    ["none", "なし"],
  ];

  type Part = keyof PartScales;
  const PARTS: [Part, string][] = [
    ["title", "タイトル"],
    ["author", "作者"],
    ["description", "説明"],
  ];
  // As far as the engine draws them (crates/fluxlide-engine/src/caption.rs).
  const PART_RANGE = { min: 0.5, max: 2 };
  const ALL_RANGE = { min: 0.6, max: 1.6 };

  const percent = (scale: number) => `${Math.round(scale * 100)}%`;

  /// Size of one part, or of the whole caption with "all".
  function setSize(part: Part | "all", scale: number) {
    if (!Number.isFinite(scale)) return;
    setLook((look) => {
      if (part === "all") look.caption.textScale = scale;
      else look.caption.partScales[part] = scale;
    }, `size:${part}`);
  }

  function resetSizes() {
    setLook((look) => {
      look.caption.textScale = 1;
      look.caption.partScales = { title: 1, author: 1, description: 1 };
    });
  }

  const standardSizes = $derived(
    look !== undefined &&
      look.caption.textScale === 1 &&
      PARTS.every(([part]) => look.caption.partScales[part] === 1),
  );

  const DEFAULT_WATERMARK: Omit<WatermarkSpec, "content"> = { corner: "topRight", opacity: 0.6, size: 0.045 };

  function setWatermarkKind(kind: "none" | "text" | "image", select: HTMLSelectElement) {
    if (kind === "image") {
      // Without a logo chosen nothing changes, so the menu is put back.
      chooseLogo()
        .catch((err) => {
          open.error = String(err);
          return false;
        })
        .then((chosen) => {
          if (!chosen) select.value = watermark?.content.kind ?? "none";
        });
      return;
    }
    open.edit((show) => {
      const old = show.look.watermark?.content;
      show.look.watermark =
        kind === "none"
          ? null
          : { ...DEFAULT_WATERMARK, ...show.look.watermark, content: { kind: "text", text: show.title || "FLUXLIDE" } };
      if (old?.kind === "image") dropUnusedAsset(show, old.asset);
    });
  }

  /// Resolves to whether a logo was chosen.
  async function chooseLogo(): Promise<boolean> {
    const file = await pickFile({ multiple: false, filters: [{ name: "画像", extensions: PHOTO_EXTENSIONS }] });
    if (typeof file !== "string") return false;
    const [asset] = await open.import([file], false);
    if (asset === undefined) return false;
    open.edit((show) => {
      const old = show.look.watermark?.content;
      show.look.watermark = {
        ...DEFAULT_WATERMARK,
        ...show.look.watermark,
        content: { kind: "image", asset },
        size: show.look.watermark?.content.kind === "image" ? show.look.watermark.size : 0.08,
      };
      if (old?.kind === "image" && old.asset !== asset) dropUnusedAsset(show, old.asset);
    });
    return true;
  }

  function setWatermark(change: (spec: WatermarkSpec) => void, group: string | null = null) {
    setLook((look) => {
      if (look.watermark) change(look.watermark);
    }, group && `watermark:${group}`);
  }

  async function addTransition() {
    const file = await pickFile({ multiple: false, filters: [{ name: "WGSL シェーダー", extensions: ["wgsl"] }] });
    if (typeof file !== "string") return;
    adding = true;
    transitionError = null;
    try {
      const loaded = await api.loadTransition(file);
      open.edit((show) => {
        const id = Math.max(0, ...show.look.customTransitions.map((custom) => custom.id)) + 1;
        show.look.customTransitions.push({ id, name: loaded.name, wgsl: loaded.wgsl, params: [0, 0, 0, 0] });
      });
    } catch (err) {
      transitionError = String(err);
    } finally {
      adding = false;
    }
  }

  const logo = $derived(
    watermark?.content.kind === "image" ? open.asset(watermark.content.asset) : undefined,
  );
</script>

{#if show && look}
  <div class="panel">
    <fieldset>
      <legend>キャプション</legend>
      <p class="hint">各スライドのタイトル・作者・説明を、写真の上に重ねて表示します。</p>

      <section class="group" aria-labelledby="caption-design">
        <h4 id="caption-design">デザイン</h4>
        <div class="designs">
          {#each DESIGNS as [design, name] (design)}
            <button
              class="design"
              class:current={look.caption.design === design}
              aria-pressed={look.caption.design === design}
              onclick={() => setLook((look) => (look.caption.design = design))}
            >
              <span class="thumb {design}" aria-hidden="true">
                {#if design !== "none"}
                  <span class="mark">
                    <span class="bar title"></span>
                    <span class="bar author"></span>
                    <span class="bar description"></span>
                  </span>
                {/if}
              </span>
              {name}
            </button>
          {/each}
        </div>
        {#if look.caption.design === "none"}
          <p class="hint" transition:rise>どのスライドにもキャプションを表示しません。</p>
        {/if}
      </section>

      {#if look.caption.design !== "none"}
        <section class="group" aria-labelledby="caption-size" transition:rise>
          <div class="heading">
            <h4 id="caption-size">文字の大きさ</h4>
            <button class="quiet small" disabled={standardSizes} onclick={resetSizes}>標準に戻す</button>
          </div>
          <div class="sizes">
            <label class="size all">
              <span class="name">全体</span>
              <input
                type="range"
                min={ALL_RANGE.min}
                max={ALL_RANGE.max}
                step="0.05"
                value={look.caption.textScale}
                oninput={(e) => setSize("all", Number(e.currentTarget.value))}
              />
              <output>{percent(look.caption.textScale)}</output>
            </label>
            {#each PARTS as [part, name] (part)}
              <label class="size">
                <span class="name">{name}</span>
                <input
                  type="range"
                  min={PART_RANGE.min}
                  max={PART_RANGE.max}
                  step="0.05"
                  value={look.caption.partScales[part]}
                  oninput={(e) => setSize(part, Number(e.currentTarget.value))}
                />
                <output>{percent(look.caption.partScales[part])}</output>
              </label>
            {/each}
          </div>
          <p class="hint">「全体」を動かすと、3つの項目がまとめて大きく・小さくなります。</p>
        </section>
      {/if}

      {#if look.caption.design !== "card" && look.caption.design !== "none"}
        <label class="group" transition:rise>
          <span class="group-title">縁どり（px）</span>
          <span class="outline">
            <input
              type="range"
              min="0"
              max="10"
              step="0.5"
              value={look.caption.outline}
              oninput={(e) => setOutline(Number(e.currentTarget.value))}
            />
            <input
              type="number"
              min="0"
              max="20"
              step="0.5"
              value={look.caption.outline}
              oninput={(e) => setOutline(Number(e.currentTarget.value))}
            />
          </span>
        </label>
      {/if}
    </fieldset>

    <fieldset>
      <legend>ウォーターマーク</legend>
      <select
        value={watermark?.content.kind ?? "none"}
        onchange={(e) => setWatermarkKind(e.currentTarget.value as "none" | "text" | "image", e.currentTarget)}
      >
        <option value="none">なし</option>
        <option value="text">文字</option>
        <option value="image">画像（ロゴ）</option>
      </select>
      {#if watermark}
        {#if watermark.content.kind === "text"}
          <label>
            文字
            <input
              value={watermark.content.text}
              oninput={(e) => {
                const text = e.currentTarget.value;
                setWatermark((spec) => (spec.content = { kind: "text", text }), "text");
              }}
            />
          </label>
        {:else}
          <p class="hint">
            {logo ? logo.originalName : "画像"}
            {#if logo?.state.state === "failed"}<span class="error">（{logo.state.reason}）</span>{/if}
            <button class="quiet" onclick={chooseLogo}>選び直す</button>
          </p>
        {/if}
        <div class="pair">
          <label>
            位置
            <select
              value={watermark.corner}
              onchange={(e) => {
                const corner = e.currentTarget.value as Corner;
                setWatermark((spec) => (spec.corner = corner));
              }}
            >
              {#each Object.entries(CORNERS) as [corner, name] (corner)}
                <option value={corner}>{name}</option>
              {/each}
            </select>
          </label>
          <label>
            不透明度 {Math.round(watermark.opacity * 100)}%
            <input
              type="range"
              min="0.1"
              max="1"
              step="0.05"
              value={watermark.opacity}
              oninput={(e) => {
                const opacity = Number(e.currentTarget.value);
                setWatermark((spec) => (spec.opacity = opacity), "opacity");
              }}
            />
          </label>
        </div>
        <label>
          大きさ {Math.round(watermark.size * 1000) / 10}%
          <input
            type="range"
            min="0.02"
            max="0.2"
            step="0.005"
            value={watermark.size}
            oninput={(e) => {
              const size = Number(e.currentTarget.value);
              setWatermark((spec) => (spec.size = size), "size");
            }}
          />
        </label>
      {/if}
    </fieldset>

    <label>
      縦横比が合わない写真
      <select
        value={look.fit}
        onchange={(e) => {
          const fit = e.currentTarget.value as Fit;
          setLook((look) => (look.fit = fit));
        }}
      >
        {#each Object.entries(FITS) as [fit, name] (fit)}
          <option value={fit}>{name}</option>
        {/each}
      </select>
    </label>

    <div class="pair">
      <label>
        トランジション
        <TransitionSelect
          value={look.transition}
          {show}
          onchange={(choice) => setLook((look) => (look.transition = choice ?? { kind: "fade" }))}
        />
      </label>
      <label>
        長さ（秒）
        <input
          type="number"
          min="0.1"
          step="0.1"
          placeholder={String(ownLength(look.transition) / 1000)}
          value={look.transitionMs === null ? "" : look.transitionMs / 1000}
          oninput={(e) => {
            // Left empty, each transition takes its own length.
            const value = e.currentTarget.value;
            const ms = Math.round(Number(value) * 1000);
            if (value.trim() === "") setLook((look) => (look.transitionMs = null), "transitionMs");
            else if (ms > 0) setLook((look) => (look.transitionMs = ms), "transitionMs");
          }}
        />
      </label>
    </div>

    <div class="pair">
      <label>
        写真の表示時間（秒）
        <input
          type="number"
          min="0.5"
          step="0.5"
          value={look.photoMs / 1000}
          oninput={(e) => {
            const ms = Math.round(Number(e.currentTarget.value) * 1000);
            if (ms > 0) setLook((look) => (look.photoMs = ms), "photoMs");
          }}
        />
      </label>
      <label>
        ゆっくりズーム
        <select
          value={zoomOf(look.motion) === "still" ? "none" : zoomOf(look.motion)}
          onchange={(e) => {
            const key = e.currentTarget.value as "none" | "in" | "out";
            setLook((look) => (look.motion = key === "none" ? null : ZOOMS[key]));
          }}
        >
          <option value="none">なし</option>
          <option value="in">ズームイン</option>
          <option value="out">ズームアウト</option>
        </select>
      </label>
    </div>

    <details>
      <summary>詳細設定</summary>
      <div class="details">
        <fieldset>
          <legend>カスタムトランジション（WGSL）</legend>
          {#each look.customTransitions as custom (custom.id)}
            <div class="custom">
              <span>{custom.name}</span>
              <button class="quiet" onclick={() => open.edit((show) => removeCustomTransition(show, custom.id))}>外す</button>
            </div>
          {/each}
          <button disabled={adding} onclick={addTransition}>{adding ? "確かめています…" : "WGSL ファイルを追加…"}</button>
          {#if transitionError}<pre class="error">{transitionError}</pre>{/if}
        </fieldset>
      </div>
    </details>
  </div>
{/if}

<style>
  .panel {
    display: flex;
    flex-direction: column;
    gap: 14px;
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

  .group {
    display: flex;
    flex-direction: column;
    gap: 8px;
  }

  .group + .group {
    border-top: 1px solid var(--line);
    padding-top: 12px;
  }

  h4,
  .group-title {
    margin: 0;
    font-size: 13px;
    font-weight: 600;
    color: var(--fg);
  }

  .heading {
    display: flex;
    justify-content: space-between;
    align-items: center;
  }

  button.small {
    padding: 2px 8px;
    font-size: 12px;
    color: var(--accent);
  }

  .designs {
    display: grid;
    grid-template-columns: repeat(3, 1fr);
    gap: 6px;
  }

  .design {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 6px;
    padding: 6px 6px 7px;
    font-size: 13px;
  }

  .design.current {
    border-color: var(--accent);
    background: var(--accent-soft);
    font-weight: 600;
  }

  /* A small picture of each design: where the caption sits and what is
     behind it, drawn like crates/fluxlide-engine/src/caption.rs. */
  .thumb {
    position: relative;
    width: 100%;
    aspect-ratio: 16 / 9;
    border-radius: 4px;
    overflow: hidden;
    background: linear-gradient(160deg, #7d93ab, #3e4f63 70%);
  }

  .thumb.band::before {
    content: "";
    position: absolute;
    inset: 45% 0 0;
    background: linear-gradient(transparent, rgb(0 0 0 / 0.7));
  }

  .mark {
    position: absolute;
    left: 10%;
    bottom: 12%;
    width: 50%;
    display: flex;
    flex-direction: column;
    gap: 2px;
  }

  .thumb.simple .mark {
    padding-left: 4px;
    border-left: 1.5px solid rgb(255 255 255 / 0.9);
  }

  .thumb.card .mark {
    left: auto;
    right: 8%;
    bottom: 10%;
    width: 46%;
    padding: 4px;
    border-radius: 2px;
    background: #f5f6f8;
  }

  .bar {
    display: block;
    height: 2px;
    border-radius: 1px;
    background: #fff;
  }

  .bar.title {
    height: 4px;
    width: 80%;
  }

  .bar.author {
    width: 45%;
    opacity: 0.85;
  }

  .bar.description {
    width: 100%;
    opacity: 0.75;
  }

  .thumb.card .bar {
    background: #13213b;
  }

  .sizes {
    display: flex;
    flex-direction: column;
    gap: 6px;
  }

  .size {
    display: grid;
    grid-template-columns: 4.5em 1fr 3.2em;
    align-items: center;
    gap: 8px;
    color: var(--fg);
  }

  .size.all {
    padding-bottom: 6px;
    border-bottom: 1px dashed var(--line);
  }

  .size.all .name {
    font-weight: 600;
  }

  .size input {
    width: 100%;
  }

  output {
    text-align: right;
    font-variant-numeric: tabular-nums;
    color: var(--muted);
  }

  .outline {
    display: grid;
    grid-template-columns: 1fr 64px;
    gap: 8px;
    align-items: center;
  }

  .pair {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 10px;
  }

  .custom {
    display: flex;
    justify-content: space-between;
    align-items: center;
  }

  pre {
    white-space: pre-wrap;
    font-size: 12px;
    margin: 0;
  }

  p {
    margin: 0;
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
</style>
