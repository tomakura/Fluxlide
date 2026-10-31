import { beforeEach, describe, expect, it, vi } from "vitest";
import type { EditOutcome } from "./bindings/EditOutcome";
import type { ShowDefinition } from "./bindings/ShowDefinition";
import type { ShowState } from "./bindings/ShowState";

// The Rust side, played by the tests.
const editShow = vi.fn<(show: ShowDefinition, session: number, revision: number) => Promise<EditOutcome>>();
const saveProject = vi.fn<(session: number) => Promise<ShowState>>();
vi.mock("./api", () => ({
  editShow: (show: ShowDefinition, session: number, revision: number) => editShow(show, session, revision),
  onShowChanged: () => Promise.resolve(() => {}),
  onImportProgress: () => Promise.resolve(() => {}),
  saveProject: (session: number) => saveProject(session),
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
    saveProject.mockReset();
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
    const apply = editShow.getMockImplementation()!;
    let release!: () => void;
    const held = new Promise<void>((resolve) => (release = resolve));
    editShow.mockImplementationOnce(async (...args) => {
      await held;
      return apply(...args);
    });
    const first = open.edit((show) => (show.title = "a"));
    const second = open.edit((show) => (show.title = "ab"));
    // The field keeps what is typed until both are answered.
    expect(open.show?.title).toBe("");
    release();
    await Promise.all([first, second]);
    expect(open.error).toBeNull();
    expect(rust().show.title).toBe("ab");
    expect(open.show?.title).toBe("ab");
  });

  it("makes each change on a copy, not on the definition received", async () => {
    const received = state(1, 0, "kept");
    open.take(received);
    applying(state(1, 0, "kept"));
    await open.edit((show) => (show.title = "changed"));
    expect(received.show.title).toBe("kept");
    const [sent] = editShow.mock.calls[0];
    expect(sent).not.toBe(received.show);
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

  it("flush waits for every queued edit before a transition can continue", async () => {
    open.take(state(1, 0, "old"));
    const rust = applying(state(1, 0, "old"));
    const apply = editShow.getMockImplementation()!;
    let release!: () => void;
    const held = new Promise<void>((resolve) => (release = resolve));
    editShow.mockImplementationOnce(async (...args) => { await held; return apply(...args); });
    void open.edit((show) => (show.title = "a"));
    void open.edit((show) => (show.title = "ab"));
    let transitioned = false;
    const transition = open.flush().then(() => (transitioned = true));
    await Promise.resolve();
    expect(open.pending).toBeGreaterThan(0);
    expect(transitioned).toBe(false);
    release();
    await transition;
    expect(rust().show.title).toBe("ab");
    expect(open.pending).toBe(0);
  });

  it("retains failed edits, retries in order, and queues new input after the retry", async () => {
    open.take(state(1, 0, "old"));
    const rust = applying(state(1, 0, "old"));
    editShow.mockRejectedValueOnce(new Error("IPC failed"));
    await open.edit((show) => (show.title = "a"));
    await open.edit((show) => (show.title = "ab"));
    expect(open.failed).toBe(2);
    await expect(open.flush()).rejects.toThrow("IPC failed");
    saveProject.mockImplementation(async () => rust());
    const retry = open.retrySave();
    const typed = open.edit((show) => (show.title = "abc"));
    await Promise.all([retry, typed]);
    await open.flush();
    expect(open.failed).toBe(0);
    expect(rust().show.title).toBe("abc");
    expect(editShow.mock.calls.map(([show]) => show.title)).toEqual(["a", "a", "ab", "abc"]);
  });

  it("refuses to leave a failed save until an explicit retry succeeds", async () => {
    open.take({ ...state(1, 0, "kept"), saveError: "disk full" });
    await expect(open.flush()).rejects.toThrow("disk full");
    saveProject.mockResolvedValue(state(1, 1, "kept"));
    await open.retrySave();
    await expect(open.flush()).resolves.toBeUndefined();
    expect(open.show?.title).toBe("kept");
  });

  it("does not apply a queued undo to a newly opened project", async () => {
    open.take(state(1, 0, "old"));
    const command = vi.fn(async () => state(1, 1, "undone"));
    const queued = open.run(command);
    open.take(state(2, 0, "new"));
    await queued;
    expect(command).not.toHaveBeenCalled();
    expect(open.show?.title).toBe("new");
  });
});
