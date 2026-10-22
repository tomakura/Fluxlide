// Changes to a definition that more than one screen makes.

import type { ShowDefinition } from "./bindings/ShowDefinition";
import type { SlideId } from "./bindings/SlideId";

/// Takes slide `id` out of the list, and its photo or video out of the
/// show if nothing else uses it. The file stays in the project folder until
/// the project is next opened, so the change can be undone.
export function removeSlide(show: ShowDefinition, id: SlideId) {
  const slide = show.slides.find((slide) => slide.id === id);
  if (!slide) return;
  show.slides = show.slides.filter((other) => other.id !== id);
  const watermark = show.look.watermark?.content;
  const stillUsed =
    show.slides.some((other) => other.asset === slide.asset) ||
    (watermark?.kind === "image" && watermark.asset === slide.asset);
  if (!stillUsed) show.assets = show.assets.filter((asset) => asset.id !== slide.asset);
}

/// Takes out asset `id` if no slide and not the watermark uses it.
export function dropUnusedAsset(show: ShowDefinition, id: number) {
  const watermark = show.look.watermark?.content;
  const used =
    show.slides.some((slide) => slide.asset === id) || (watermark?.kind === "image" && watermark.asset === id);
  if (!used) show.assets = show.assets.filter((asset) => asset.id !== id);
}

/// Takes custom transition `id` out of the look. Slides that used it follow
/// the look again, and a look that used it fades.
export function removeCustomTransition(show: ShowDefinition, id: number) {
  show.look.customTransitions = show.look.customTransitions.filter((custom) => custom.id !== id);
  const uses = (choice: { kind: string; id?: number } | null) => choice?.kind === "custom" && choice.id === id;
  if (uses(show.look.transition)) show.look.transition = { kind: "fade" };
  for (const slide of show.slides) {
    if (uses(slide.transition)) slide.transition = null;
  }
}
