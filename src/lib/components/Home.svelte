<script lang="ts">
  // A. ホーム: start a new slideshow or open a recent one (docs/ui-draft.md).
  import { open as pickFolder } from "@tauri-apps/plugin-dialog";
  import * as api from "$lib/api";
  import type { ProjectSummary } from "$lib/bindings/ProjectSummary";
  import { open } from "$lib/show.svelte";
  import AboutDialog from "./AboutDialog.svelte";
  import DropZone from "./DropZone.svelte";
  import Mark from "./Mark.svelte";

  let { onopened }: { onopened: () => void } = $props();

  let projects = $state<ProjectSummary[] | null>(null);
  let busy = $state(false);
  let error = $state<string | null>(null);
  let about = $state(false);

  $effect(() => {
    api
      .listProjects()
      .then((list) => (projects = list))
      .catch((err) => (error = String(err)));
  });

  async function begin(task: () => Promise<void>) {
    if (busy) return;
    busy = true;
    error = null;
    try {
      await task();
      onopened();
    } catch (err) {
      error = String(err);
    } finally {
      busy = false;
    }
  }

  const create = (files: string[] = []) =>
    begin(async () => {
      open.take(await api.createProject("新しいスライドショー"));
      if (files.length) await open.import(files);
    });

  const openDir = (dir: string) => begin(async () => open.take(await api.openProject(dir)));

  async function openElsewhere() {
    const dir = await pickFolder({ directory: true, title: "スライドショーのフォルダを選ぶ" });
    if (typeof dir === "string") openDir(dir);
  }

  const date = (ms: number) =>
    new Date(ms).toLocaleString("ja-JP", { dateStyle: "medium", timeStyle: "short" });
</script>

<main>
  <section class="start">
    <div class="brand">
      <Mark size={56} />
      <h1 class="brandname">Fluxlide</h1>
    </div>
    <button class="primary new" disabled={busy} onclick={() => create()}>
      ＋ 新しいスライドショーを作る
    </button>
    <DropZone ondrop={(files) => create(files)}>
      写真や動画をここにドロップすると、新しいスライドショーを作ります
    </DropZone>
    {#if error}<p class="error">{error}</p>{/if}
    <button class="quiet about" onclick={() => (about = true)}>このアプリについて</button>
  </section>

  <section class="recent">
    <div class="head">
      <h2>最近のスライドショー</h2>
      <button class="quiet" onclick={openElsewhere} disabled={busy}>ほかの場所から開く…</button>
    </div>
    {#if projects === null}
      <p class="hint">読み込んでいます…</p>
    {:else if projects.length === 0}
      <p class="hint">まだありません。「新しいスライドショーを作る」から始めましょう。</p>
    {:else}
      <ul>
        {#each projects as project (project.dir)}
          <li>
            <button class="project" disabled={busy} onclick={() => openDir(project.dir)}>
              <span class="thumb">
                {#if project.thumbnail}
                  <img src={api.fileUrl(project.thumbnail)} alt="" />
                {/if}
              </span>
              <span class="info">
                <strong>{project.title || "無題のスライドショー"}</strong>
                <span class="hint">{date(project.updatedAt)} · {project.slideCount}枚</span>
              </span>
            </button>
          </li>
        {/each}
      </ul>
    {/if}
    <p class="hint">変更は自動で保存されます。保存の操作はいりません。</p>
  </section>
</main>

{#if about}
  <AboutDialog onclose={() => (about = false)} />
{/if}

<style>
  main {
    height: 100vh;
    display: grid;
    grid-template-columns: minmax(320px, 2fr) 3fr;
    gap: 40px;
    padding: 48px 56px;
  }

  .start {
    display: flex;
    flex-direction: column;
    gap: 24px;
    justify-content: center;
  }

  .brand {
    display: flex;
    align-items: center;
    gap: 14px;
  }

  h1 {
    margin: 0;
    font-size: 40px;
  }

  h2 {
    margin: 0;
    font-size: 17px;
  }

  .about {
    align-self: flex-start;
  }

  .new {
    padding: 18px 20px;
    font-size: 18px;
    border-radius: 12px;
  }

  .recent {
    display: flex;
    flex-direction: column;
    gap: 14px;
    min-height: 0;
  }

  .head {
    display: flex;
    justify-content: space-between;
    align-items: center;
  }

  ul {
    list-style: none;
    margin: 0;
    padding: 0;
    overflow-y: auto;
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(220px, 1fr));
    gap: 14px;
    align-content: start;
    flex: 1;
  }

  .project {
    width: 100%;
    padding: 0;
    display: flex;
    flex-direction: column;
    text-align: left;
    overflow: hidden;
    border-radius: var(--radius);
  }

  .thumb {
    aspect-ratio: 16 / 9;
    background: #000;
    display: block;
  }

  .thumb img {
    width: 100%;
    height: 100%;
    object-fit: cover;
    display: block;
  }

  .info {
    display: flex;
    flex-direction: column;
    gap: 2px;
    padding: 10px 12px;
  }
</style>
