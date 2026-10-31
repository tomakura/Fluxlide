<script lang="ts">
  import { open } from "#lib/show.svelte";
  const failed = $derived(open.failed > 0 || !!open.state?.saveError);
</script>

<span class="saved" class:error={failed} role="status" title={open.state?.saveError ?? open.error ?? ""}>
  {open.pending ? "保存中…" : failed ? "保存できません" : "自動保存済み"}
</span>

<style>
  .saved { font-size: 12px; color: var(--muted); white-space: nowrap; }
  .saved.error { color: var(--err); }
</style>
