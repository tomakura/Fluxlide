// Motion shared by every screen: short, eased, and none at all when the OS
// asks for reduced motion.

import { cubicOut } from "svelte/easing";
import { fade, fly, scale } from "svelte/transition";

function reduced(): boolean {
  return typeof matchMedia !== "undefined" && matchMedia("(prefers-reduced-motion: reduce)").matches;
}

/// Milliseconds, or 0 when motion is reduced.
function ms(duration: number): number {
  return reduced() ? 0 : duration;
}

/// Fades in or out: screens, backdrops, text that changes.
export function appear(node: Element, { delay = 0, duration = 180 } = {}) {
  return fade(node, { delay: ms(delay), duration: ms(duration) });
}

/// Rises into place from a little below, or sinks out: items, banners, toasts.
export function rise(node: Element, { delay = 0, y = 8, duration = 220 } = {}) {
  return fly(node, { delay: ms(delay), y, duration: ms(duration), easing: cubicOut });
}

/// Slides in from the side: what comes next in a sequence.
export function slideIn(node: Element, { x = 24, duration = 260 } = {}) {
  return fly(node, { x, duration: ms(duration), easing: cubicOut });
}

/// Grows from slightly smaller: dialogs.
export function pop(node: Element, { duration = 200 } = {}) {
  return scale(node, { start: 0.96, duration: ms(duration), easing: cubicOut });
}

/// For `animate:flip`: items moving to their new place in a list.
export function moving() {
  return { duration: ms(240), easing: cubicOut };
}
