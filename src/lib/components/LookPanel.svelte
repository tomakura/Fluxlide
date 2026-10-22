<script lang="ts">
  // 見た目 tab: settings shared by every slide (docs/ui-draft.md, B).
  import { open as pickFile } from "@tauri-apps/plugin-dialog";
  import * as api from "$lib/api";
  import type { CaptionDesign } from "$lib/bindings/CaptionDesign";
  import type { Corner } from "$lib/bindings/Corner";
  import type { Fit } from "$lib/bindings/Fit";
  import type { Look } from "$lib/bindings/Look";
  import type { WatermarkSpec } from "$lib/bindings/WatermarkSpec";
  import { CORNERS, FITS, PHOTO_EXTENSIONS, ZOOMS, zoomOf } from "$lib/format";
  import { dropUnusedAsset, removeCustomTransition } from "$lib/edits";
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

  const DESIGNS: [CaptionDesign, string, string][] = [
    ["band", "下帯", "下に薄い帯を敷いて文字を置く"],
    ["simple", "シンプル", "左下に細い線と文字だけ"],
    ["card", "カード", "右下に明るい札を出す"],
    ["none", "なし", "キャプションを出さない"],
  ];

  const DEFAULT_WATERMARK: Omit<WatermarkSpec, "content"> = { corner: "topRight", opacity: 0.6, size: 0.045 };

  function setWatermarkKind(kind: "none" | "text" | "image") {
    if (kind === "image") {
      chooseLogo();
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

  async function chooseLogo() {
    const file = await pickFile({ multiple: false, filters: [{ name: "画像", extensions: PHOTO_EXTENSIONS }] });
    if (typeof file !== "string") return;
    const [asset] = await open.import([file], false);
    if (asset === undefined) return;
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
    <h3>見た目（すべてのスライドに共通）</h3>

    <fieldset>
      <legend>キャプションのデザイン</legend>
      <div class="designs">
        {#each DESIGNS as [design, name, note] (design)}
          <button
            class="design"
            class:current={look.caption.design === design}
            onclick={() => setLook((look) => (look.caption.design = design))}
          >
            <strong>{name}</strong>
            <span class="hint">{note}</span>
          </button>
        {/each}
      </div>
    </fieldset>

    <fieldset>
      <legend>ウォーターマーク</legend>
      <select
        value={watermark?.content.kind ?? "none"}
        onchange={(e) => setWatermarkKind(e.currentTarget.value as "none" | "text" | "image")}
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
          大きさ
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
          value={look.transitionMs / 1000}
          oninput={(e) => {
            const ms = Math.round(Number(e.currentTarget.value) * 1000);
            if (ms > 0) setLook((look) => (look.transitionMs = ms), "transitionMs");
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
          <option value="in">すべての写真をズームイン</option>
          <option value="out">すべての写真をズームアウト</option>
        </select>
      </label>
    </div>
    <p class="hint">スライドごとの設定は「写真・動画」タブで変えられます。</p>

    <details>
      <summary>詳細設定</summary>
      <div class="details">
        <label>
          キャプションの文字の大きさ {Math.round(look.caption.textScale * 100)}%
          <input
            type="range"
            min="0.6"
            max="1.6"
            step="0.05"
            value={look.caption.textScale}
            oninput={(e) => {
              const scale = Number(e.currentTarget.value);
              setLook((look) => (look.caption.textScale = scale), "textScale");
            }}
          />
        </label>
        <p class="hint">書体は Noto Sans JP です（上映と書き出しで同じ文字になるよう、アプリに入れた書体だけを使います）。</p>

        <fieldset>
          <legend>カスタムトランジション（WGSL）</legend>
          {#each look.customTransitions as custom (custom.id)}
            <div class="custom">
              <span>{custom.name}</span>
              <button class="quiet" onclick={() => open.edit((show) => removeCustomTransition(show, custom.id))}>外す</button>
            </div>
          {:else}
            <p class="hint">まだありません。追加すると、トランジションの一覧に出ます。</p>
          {/each}
          <button disabled={adding} onclick={addTransition}>{adding ? "確かめています…" : "WGSL ファイルを追加…"}</button>
          {#if transitionError}<pre class="error">{transitionError}</pre>{/if}
          <p class="hint">
            <code>fn transition(uv: vec2&lt;f32&gt;, progress: f32) -&gt; vec4&lt;f32&gt;</code> を書いたファイルです。
            追加するときに GPU で確かめ、間違いがあれば理由を表示します。
          </p>
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

  h3 {
    margin: 0;
    font-size: 15px;
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

  .designs {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 6px;
  }

  .design {
    display: flex;
    flex-direction: column;
    align-items: flex-start;
    gap: 2px;
    text-align: left;
    padding: 8px 10px;
  }

  .design.current {
    border-color: var(--accent);
    background: var(--accent-soft);
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
