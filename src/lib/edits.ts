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
