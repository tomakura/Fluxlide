import { beforeEach, describe, expect, it, vi } from "vitest";
import type { EditOutcome } from "./bindings/EditOutcome";
import type { ShowDefinition } from "./bindings/ShowDefinition";
import type { ShowState } from "./bindings/ShowState";

// The Rust side, played by the tests.
const editShow = vi.fn<(show: ShowDefinition, session: number, revision: number) => Promise<EditOutcome>>();
vi.mock("./api", () => ({
  editShow: (show: ShowDefinition, session: number, revision: number) => editShow(show, session, revision),
  onShowChanged: () => Promise.resolve(() => {}),
  onImportProgress: () => Promise.resolve(() => {}),
}));

const { open } = await import("./show.svelte");

function state(session: number, revision: number, title: string): ShowState {
  return {
    revision,
    session,
    dir: "",
    mediaDir: "",
    show: { title } as ShowDefinition,
    canUndo: false,
    canRedo: false,
    saveError: null,
  };
}

/// A Rust side that applies edits made from the current revision.
function applying(start: ShowState) {
  let current = start;
  editShow.mockImplementation(async (show, session, revision) => {
    if (session !== current.session || revision !== current.revision) {
      return { state: current, applied: false };
    }
    current = { ...current, revision: current.revision + 1, show };
    return { state: current, applied: true };
  });
  return () => current;
}

describe("open.edit", () => {
  beforeEach(() => {
    editShow.mockReset();
    open.take(null);
    open.error = null;
  });

  it("sends edits one at a time, each made from the state before it", async () => {
    open.take(state(1, 0, ""));
    const rust = applying(state(1, 0, ""));
    const typing = ["F", "Fl", "Flu"].map((title) => open.edit((show) => (show.title = title), "title"));
    await Promise.all(typing);
    expect(rust().show.title).toBe("Flu");
    expect(editShow.mock.calls.map(([, , revision]) => revision)).toEqual([0, 1, 2]);
    expect(open.show?.title).toBe("Flu");
  });

  it("does not show an older reply while edits are still waiting", async () => {
    open.take(state(1, 0, ""));
    const rust = applying(state(1, 0, ""));
    let release!: () => void;
    const held = new Promise<void>((resolve) => (release = resolve));
    editShow.mockImplementationOnce(async (show) => {
      await held;
      return { state: { ...rust(), revision: 1, show }, applied: true };
    });
    const first = open.edit((show) => (show.title = "a"));
    const second = open.edit((show) => (show.title = "ab"));
    // The field keeps what is typed until both are answered.
    expect(open.show?.title).toBe("");
    release();
    await Promise.all([first, second]);
    expect(open.show?.title).toBe("ab");
  });

  it("makes the change again when the definition changed meanwhile", async () => {
    open.take(state(1, 0, "old"));
    // An import finished on the Rust side: revision 1 already.
    const rust = applying(state(1, 1, "imported"));
    await open.edit((show) => (show.title += "!"));
    expect(rust().show.title).toBe("imported!");
    expect(editShow).toHaveBeenCalledTimes(2);
  });

  it("drops an edit made in a show that was closed before it was sent", async () => {
    open.take(state(1, 0, "first"));
    applying(state(2, 0, "second"));
    const edit = open.edit((show) => (show.title = "changed"));
    open.take(state(2, 0, "second"));
    await edit;
    for (const [show] of editShow.mock.calls) expect(show.title).not.toBe("changed");
    expect(open.show?.title).toBe("second");
  });

  it("says so when an edit could not be made after retrying", async () => {
    open.take(state(1, 0, ""));
    editShow.mockImplementation(async () => ({ state: state(1, 0, ""), applied: false }));
    await open.edit((show) => (show.title = "x"));
    expect(open.error).not.toBeNull();
  });
});
