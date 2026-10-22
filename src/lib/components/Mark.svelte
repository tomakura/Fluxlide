<script lang="ts">
  // The Fluxlide mark (assets/brand/fluxlide-mark.svg). The back plates take
  // the text color, so the mark reads on light and dark backgrounds alike
  // (docs/ui-draft.md, 8).
  let { size = 40 }: { size?: number } = $props();
</script>

<svg viewBox="0 0 64 64" width={size} height={size} aria-hidden="true">
  <rect x="6" y="9" width="34" height="24" rx="4" fill="currentColor" fill-opacity=".24" />
  <rect x="15" y="18" width="34" height="24" rx="4" fill="currentColor" fill-opacity=".5" />
  <rect x="24" y="27" width="34" height="24" rx="4" fill="#E8553A" />
</svg>
