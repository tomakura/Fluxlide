<script lang="ts">
  // このアプリについて: the version and the software Fluxlide includes, with
  // the license texts (THIRD_PARTY_LICENSES.md lists the same).
  import { getVersion } from "@tauri-apps/api/app";
  import { revealItemInDir } from "@tauri-apps/plugin-opener";
  import * as api from "$lib/api";
  import { modal } from "$lib/modal";
  import Mark from "./Mark.svelte";

  let { onclose }: { onclose: () => void } = $props();

  let version = $state("");
  let licenses = $state<string | null>(null);

  $effect(() => {
    getVersion().then((v) => (version = v));
    api.licensesDir().then((dir) => (licenses = dir));
  });
</script>

<div class="backdrop" role="presentation" onclick={(e) => e.target === e.currentTarget && onclose()}>
  <div
    class="dialog"
    role="dialog"
    aria-modal="true"
    aria-labelledby="about-title"
    tabindex="-1"
    {@attach modal(onclose)}
  >
    <div class="brand">
      <Mark size={40} />
      <div>
        <h2 id="about-title">Fluxlide</h2>
        <p class="hint">バージョン {version} · MIT License</p>
      </div>
    </div>

    <h3>含まれているソフトウェア</h3>
    <ul>
      <li>
        <strong>FFmpeg</strong> 9.0.2（LGPL v2.1 以降）— 動画と音の読み込みと書き出しに使います。改変せずに、別のプログラム（fluxlide-ffmpeg、fluxlide-ffprobe）として同梱しています。ソースは
        ffmpeg.org/releases/ffmpeg-9.0.2.tar.xz です。FFmpeg は Fabrice Bellard の商標です。
      </li>
      <li><strong>Noto Sans JP</strong>（SIL Open Font License 1.1）— キャプションと文字のウォーターマークの書体です。</li>
      <li>Tauri、wgpu、Svelte など、Rust と npm のライブラリ（MIT、Apache-2.0 など）。</li>
    </ul>
    <p class="hint">
      H.264、HEVC、AAC などの形式は、国によって特許の対象になります。Fluxlide は特許の許諾を含みません。事業で使うときは、必要な許諾を各自で確かめてください。
    </p>

    <div class="buttons">
      {#if licenses}
        <button onclick={() => licenses && revealItemInDir(licenses)}>ライセンスの全文を見る</button>
      {/if}
      <button class="primary" data-autofocus onclick={onclose}>閉じる</button>
    </div>
  </div>
</div>

<style>
  .backdrop {
    position: fixed;
    inset: 0;
    background: rgb(0 0 0 / 45%);
    display: flex;
    align-items: center;
    justify-content: center;
    z-index: 10;
  }

  .dialog {
    background: var(--surface);
    border-radius: 14px;
    padding: 24px 28px;
    width: min(560px, 92vw);
    display: flex;
    flex-direction: column;
    gap: 12px;
    box-shadow: 0 10px 40px rgb(0 0 0 / 30%);
  }

  .brand {
    display: flex;
    align-items: center;
    gap: 12px;
  }

  h2 {
    margin: 0;
    font-size: 20px;
  }

  h3 {
    margin: 4px 0 0;
    font-size: 14px;
  }

  p {
    margin: 0;
  }

  ul {
    margin: 0;
    padding-left: 20px;
    display: flex;
    flex-direction: column;
    gap: 6px;
    font-size: 13px;
  }

  .buttons {
    display: flex;
    justify-content: flex-end;
    gap: 8px;
  }
</style>
