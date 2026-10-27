<script lang="ts">
  // A question that must be answered before going on.
  import { modal } from "$lib/modal";
  import { appear, pop } from "$lib/motion";

  let {
    title,
    message,
    confirmLabel,
    onconfirm,
    oncancel,
  }: {
    title: string;
    message: string;
    confirmLabel: string;
    onconfirm: () => void;
    oncancel: () => void;
  } = $props();
</script>

<div class="backdrop" role="presentation" transition:appear|global>
  <div
    class="dialog"
    role="alertdialog"
    transition:pop|global
    aria-modal="true"
    aria-labelledby="confirm-title"
    tabindex="-1"
    {@attach modal(() => oncancel())}
  >
    <h2 id="confirm-title">{title}</h2>
    <p>{message}</p>
    <div class="buttons">
      <button onclick={oncancel}>やめる</button>
      <button class="live" data-autofocus onclick={onconfirm}>{confirmLabel}</button>
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
    z-index: 20;
  }

  .dialog {
    background: var(--surface);
    border-radius: 14px;
    padding: 22px 26px;
    width: min(420px, 90vw);
    box-shadow: 0 10px 40px rgb(0 0 0 / 30%);
  }

  h2 {
    margin: 0 0 8px;
    font-size: 17px;
  }

  p {
    margin: 0 0 20px;
    color: var(--muted);
  }

  .buttons {
    display: flex;
    justify-content: flex-end;
    gap: 8px;
  }
</style>
