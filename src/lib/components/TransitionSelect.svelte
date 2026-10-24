<script lang="ts">
  // Chooses a transition. With `followLabel`, it can also be left to the
  // 見た目 tab (`null`).
  import type { ShowDefinition } from "$lib/bindings/ShowDefinition";
  import type { TransitionChoice } from "$lib/bindings/TransitionChoice";
  import { DIRECTIONS } from "$lib/format";

  let {
    value,
    show,
    followLabel,
    onchange,
  }: {
    value: TransitionChoice | null;
    show: ShowDefinition;
    followLabel?: string;
    onchange: (choice: TransitionChoice | null) => void;
  } = $props();

  const directions = Object.entries(DIRECTIONS) as [keyof typeof DIRECTIONS, string][];

  // Every choice as one option, keyed by a string.
  const options = $derived<[string, string, TransitionChoice][]>([
    ["fade", "フェード", { kind: "fade" }],
    ...directions.map(([direction, name]): [string, string, TransitionChoice] => [
      `slide-${direction}`,
      `スライド（${name}）`,
      { kind: "slide", direction },
    ]),
    ...directions.map(([direction, name]): [string, string, TransitionChoice] => [
      `wipe-${direction}`,
      `ワイプ（${name}）`,
      { kind: "wipe", direction },
    ]),
    ["zoom", "ズーム", { kind: "zoom" }],
    ["paw", "肉球", { kind: "paw" }],
    ["cut", "カット（すぐ切り替える）", { kind: "cut" }],
    ...show.look.customTransitions.map((custom): [string, string, TransitionChoice] => [
      `custom-${custom.id}`,
      custom.name,
      { kind: "custom", id: custom.id },
    ]),
  ]);

  function keyOf(choice: TransitionChoice | null): string {
    if (!choice) return "";
    switch (choice.kind) {
      case "slide":
      case "wipe":
        return `${choice.kind}-${choice.direction}`;
      case "custom":
        return `custom-${choice.id}`;
      default:
        return choice.kind;
    }
  }
</script>

<select
  value={keyOf(value)}
  onchange={(e) => {
    const key = e.currentTarget.value;
    onchange(options.find(([k]) => k === key)?.[2] ?? null);
  }}
>
  {#if followLabel}<option value="">{followLabel}</option>{/if}
  {#each options as [key, name] (key)}
    <option value={key}>{name}</option>
  {/each}
</select>
