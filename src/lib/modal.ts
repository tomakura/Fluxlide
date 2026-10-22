// Keyboard handling shared by the dialogs: focus moves in (to the element
// marked `data-autofocus`, if any), Esc cancels, Tab stays inside, and focus
// goes back where it was when the dialog closes.

import type { Attachment } from "svelte/attachments";

const FOCUSABLE = "button:not([disabled]), input:not([disabled]), select:not([disabled]), [tabindex]:not([tabindex='-1'])";

export function modal(oncancel: () => void): Attachment<HTMLElement> {
  return (dialog) => {
    const before = document.activeElement as HTMLElement | null;
    (dialog.querySelector<HTMLElement>("[data-autofocus]") ?? dialog).focus();
    function onkeydown(event: KeyboardEvent) {
      if (event.key === "Escape") {
        oncancel();
        event.preventDefault();
        event.stopPropagation();
      } else if (event.key === "Tab") {
        const items = [...dialog.querySelectorAll<HTMLElement>(FOCUSABLE)];
        if (items.length === 0) return;
        const first = items[0];
        const last = items[items.length - 1];
        const at = document.activeElement;
        if (event.shiftKey && (at === first || at === dialog)) {
          last.focus();
          event.preventDefault();
        } else if (!event.shiftKey && at === last) {
          first.focus();
          event.preventDefault();
        }
      }
    }
    dialog.addEventListener("keydown", onkeydown);
    return () => {
      dialog.removeEventListener("keydown", onkeydown);
      before?.focus();
    };
  };
}
