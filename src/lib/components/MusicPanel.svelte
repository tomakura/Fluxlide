<script lang="ts">
  // 音楽 tab: the BGM of the show, and the sound delay of this computer's
  // output (docs/ui-draft.md, B). Music is not copied: a folder is read
  // again each time a presentation starts.
  import { open as pick } from "@tauri-apps/plugin-dialog";
  import * as api from "$lib/api";
  import type { MonitorInfo } from "$lib/bindings/MonitorInfo";
  import type { Music } from "$lib/bindings/Music";
  import { MUSIC_FILTER } from "$lib/format";
  import { open } from "$lib/show.svelte";

  const music = $derived(open.show?.music);

  let tracks = $state<string[] | null>(null);
  let tracksError = $state<string | null>(null);
  let offset = $state(0);
  let monitors = $state<MonitorInfo[]>([]);
  let testScreen = $state<number | null>(null);
  let testing = $state(false);
  let testError = $state<string | null>(null);

  $effect(() => {
    Promise.all([api.getSettings(), api.listMonitors()]).then(([settings, list]) => {
      offset = settings.syncOffsetMs;
      monitors = list;
      const remembered = list.find((m) => m.name === settings.outputScreen);
      testScreen = (remembered ?? list.find((m) => !m.hasMainWindow) ?? list[0])?.index ?? null;
    });
    // Esc in the test window ends it there.
    const ended = api.onPresentationEnded(() => (testing = false));
    // The test sound ends with this tab.
    return () => {
      ended.then((stop) => stop());
      if (testing) api.stopPresentation();
    };
  });

  // The songs the source holds now.
  $effect(() => {
    const source = music?.source;
    tracks = null;
    tracksError = null;
    if (!source) return;
    api
      .musicTracks($state.snapshot(source))
      .then((list) => (tracks = list))
      .catch((err) => (tracksError = String(err)));
  });

  function setMusic(change: (music: Music) => void, group: string | null = null) {
    open.edit((show) => change(show.music), group && `music:${group}`);
  }

  async function chooseFolder() {
    const path = await pick({ directory: true, title: "BGM のフォルダを選ぶ" });
    if (typeof path === "string") setMusic((music) => (music.source = { kind: "folder", path }));
  }

  async function chooseFiles() {
    const paths = await pick({ multiple: true, filters: [MUSIC_FILTER] });
    if (paths?.length) setMusic((music) => (music.source = { kind: "files", paths }));
  }

  const name = (path: string) => path.split(/[\\/]/).pop() ?? path;

  function setOffset(ms: number) {
    offset = Math.max(-500, Math.min(1000, Math.round(ms)));
    api.setSyncOffset(offset);
    if (testing) api.controlPresentation({ kind: "syncOffset", ms: offset });
  }

  async function toggleTest() {
    testError = null;
    if (testing) {
      await api.stopPresentation();
      testing = false;
    } else if (testScreen !== null) {
      try {
        await api.startTestSound(testScreen);
        testing = true;
      } catch (err) {
        testError = String(err);
      }
    }
  }

  const secs = (ms: number) => ms / 1000;
  const ms = (value: string) => Math.max(0, Math.round(Number(value) * 1000));
</script>

{#if music}
  <div class="panel">
    <h3>音楽</h3>

    <fieldset>
      <legend>BGM</legend>
      <div class="choices">
        <button class:current={music.source?.kind === "folder"} onclick={chooseFolder}>フォルダを指定…</button>
        <button class:current={music.source?.kind === "files"} onclick={chooseFiles}>曲を個別に選ぶ…</button>
        {#if music.source}
          <button class="quiet" onclick={() => setMusic((music) => (music.source = null))}>BGM なし</button>
        {/if}
      </div>
      {#if music.source?.kind === "folder"}
        <p class="path" title={music.source.path}>📁 {music.source.path}</p>
        <p class="hint">フォルダに曲を足すと、次に上映するときから流れます。</p>
      {:else if music.source?.kind === "files"}
        <p class="hint">{music.source.paths.length}曲を選んでいます。</p>
      {:else}
        <p class="hint">BGM は流しません。</p>
      {/if}
      {#if tracks}
        <details>
          <summary>{tracks.length}曲</summary>
          <ol>
            {#each tracks as track, i (i)}<li>{name(track)}</li>{/each}
          </ol>
        </details>
      {:else if tracksError}
        <p class="error">{tracksError}</p>
      {/if}
    </fieldset>

    <label class="inline">
      <input
        type="checkbox"
        checked={music.shuffle}
        onchange={(e) => {
          const on = e.currentTarget.checked;
          setMusic((music) => (music.shuffle = on));
        }}
      />
      シャッフル再生
    </label>
    <label>
      BGM の音量 {Math.round(music.volume * 100)}%
      <input
        type="range"
        min="0"
        max="1"
        step="0.05"
        value={music.volume}
        oninput={(e) => {
          const volume = Number(e.currentTarget.value);
          setMusic((music) => (music.volume = volume), "volume");
        }}
      />
    </label>
    <label class="inline">
      <input
        type="checkbox"
        checked={music.duckForVideos}
        onchange={(e) => {
          const on = e.currentTarget.checked;
          setMusic((music) => (music.duckForVideos = on));
        }}
      />
      音のある動画の再生中は BGM を止める
    </label>

    <fieldset>
      <legend>音ズレ補正（この機器の設定）</legend>
      <p class="hint">
        Bluetooth のスピーカーなどで音が映像より遅れて聞こえるときに、音を早めます。テスト音を出して、光と音が同時になるように合わせてください。
      </p>
      <div class="offset">
        <button onclick={() => setOffset(offset - 10)}>−10</button>
        <input type="number" step="10" value={offset} onchange={(e) => setOffset(Number(e.currentTarget.value))} />
        <span>ミリ秒</span>
        <button onclick={() => setOffset(offset + 10)}>＋10</button>
      </div>
      <div class="test">
        {#if monitors.length > 1}
          <select bind:value={testScreen} disabled={testing} aria-label="テスト音を出す画面">
            {#each monitors as monitor (monitor.index)}
              <option value={monitor.index}>{monitor.index + 1}: {monitor.name}</option>
            {/each}
          </select>
        {/if}
        <button class:live={testing} onclick={toggleTest} disabled={testScreen === null}>
          {testing ? "■ テスト音を止める" : "▶ テスト音で合わせる"}
        </button>
      </div>
      {#if testing && monitors.length === 1}
        <p class="hint">テスト音は Esc キーで止められます。</p>
      {/if}
      {#if testError}<p class="error">{testError}</p>{/if}
    </fieldset>

    <details>
      <summary>詳細設定</summary>
      <div class="details">
        <label>
          曲の始めと終わりのフェード（秒）
          <input
            type="number"
            min="0"
            step="0.5"
            value={secs(music.fadeMs)}
            oninput={(e) => {
              const value = ms(e.currentTarget.value);
              setMusic((music) => (music.fadeMs = value), "fade");
            }}
          />
        </label>
        <label>
          曲間の無音（秒）
          <input
            type="number"
            min="0"
            step="0.5"
            value={secs(music.gapMs)}
            oninput={(e) => {
              const value = ms(e.currentTarget.value);
              setMusic((music) => (music.gapMs = value), "gap");
            }}
          />
        </label>
        <div class="pair">
          <label>
            動画の前に下げる時間（秒）
            <input
              type="number"
              min="0"
              step="0.5"
              value={secs(music.duckFadeOutMs)}
              oninput={(e) => {
                const value = ms(e.currentTarget.value);
                setMusic((music) => (music.duckFadeOutMs = value), "duckOut");
              }}
            />
          </label>
          <label>
            動画の後に戻す時間（秒）
            <input
              type="number"
              min="0"
              step="0.5"
              value={secs(music.duckFadeInMs)}
              oninput={(e) => {
                const value = ms(e.currentTarget.value);
                setMusic((music) => (music.duckFadeInMs = value), "duckIn");
              }}
            />
          </label>
        </div>
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

  p {
    margin: 0;
  }

  .choices,
  .test,
  .offset {
    display: flex;
    gap: 6px;
    flex-wrap: wrap;
    align-items: center;
  }

  .choices .current {
    border-color: var(--accent);
    background: var(--accent-soft);
  }

  .offset input {
    width: 80px;
  }

  .path {
    overflow-wrap: anywhere;
    font-size: 13px;
  }

  ol {
    margin: 6px 0 0;
    padding-left: 22px;
    max-height: 160px;
    overflow-y: auto;
    font-size: 12px;
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
</style>
