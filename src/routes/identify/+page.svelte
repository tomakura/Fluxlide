<script lang="ts">
  // The number shown on one screen by "各画面に番号を表示".
  const n = new URLSearchParams(location.search).get("n") ?? "?";
</script>

<div class="number">{n}</div>

<style>
  :global(html),
  :global(body) {
    background: var(--navy);
  }

  .number {
    height: 100vh;
    display: flex;
    align-items: center;
    justify-content: center;
    color: #fff;
    background: var(--navy);
    border: 6px solid var(--coral);
    font-family: "Jost", "Futura", "Century Gothic", sans-serif;
    font-size: 200px;
    font-weight: 600;
  }
</style>
