<script lang="ts">
  // 見た目 tab: settings shared by every slide (docs/ui-draft.md, B).
  import { open as pickFile } from "@tauri-apps/plugin-dialog";
  import * as api from "$lib/api";
  import type { CaptionDesign } from "$lib/bindings/CaptionDesign";
  import type { Corner } from "$lib/bindings/Corner";
  import type { Fit } from "$lib/bindings/Fit";
  import type { Look } from "$lib/bindings/Look";
  import type { WatermarkSpec } from "$lib/bindings/WatermarkSpec";
  import { CORNERS, FITS, PHOTO_EXTENSIONS, ZOOMS, ownLength, zoomOf } from "$lib/format";
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

  const DESIGNS: [CaptionDesign, string][] = [
    ["band", "下帯"],
    ["simple", "シンプル"],
    ["card", "カード"],
    ["none", "なし"],
  ];

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
      <legend>キャプションのデザイン</legend>
      <div class="designs">
        {#each DESIGNS as [design, name] (design)}
          <button
            class="design"
            class:current={look.caption.design === design}
            aria-pressed={look.caption.design === design}
            onclick={() => setLook((look) => (look.caption.design = design))}
          >
            {name}
          </button>
        {/each}
      </div>
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

  .designs {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 6px;
  }

  .design {
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
