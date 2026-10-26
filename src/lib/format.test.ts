import { describe, expect, it } from "vitest";
import type { ShowDefinition } from "./bindings/ShowDefinition";
import type { Slide } from "./bindings/Slide";
import { clock, effective, fileSize, roughly, screenName, seconds } from "./format";

describe("format", () => {
  it("writes lengths in seconds and minutes", () => {
    expect(seconds(1500)).toBe("1.5秒");
    expect(seconds(60_000)).toBe("1分");
    expect(seconds(80_000)).toBe("1分20秒");
    // Not "1分60秒".
    expect(seconds(119_600)).toBe("2分");
  });

  it("writes running times as a clock", () => {
    expect(clock(5)).toBe("0:05");
    expect(clock(754)).toBe("12:34");
    expect(clock(3723)).toBe("1:02:03");
  });

  it("writes file sizes and rough times", () => {
    expect(fileSize(820_000)).toBe("820 KB");
    expect(fileSize(12_400_000)).toBe("12 MB");
    expect(fileSize(1_400_000_000)).toBe("1.4 GB");
    expect(roughly(20)).toBe("1分未満");
    expect(roughly(180)).toBe("約3分");
    expect(roughly(4800)).toBe("約1時間20分");
  });

  it("leaves out the names macOS makes up for screens", () => {
    expect(screenName("Monitor #41055")).toBeNull();
    expect(screenName("LG HDR 4K")).toBe("LG HDR 4K");
  });

  it("gives a transition without a length its own, unless the look sets one", () => {
    const show = { look: { transition: { kind: "paw" }, transitionMs: null } } as ShowDefinition;
    const slide = { transition: null, transitionMs: null, framing: { fit: null } } as unknown as Slide;
    expect(effective(slide, show).transitionMs).toBe(5000);
    expect(effective({ ...slide, transition: { kind: "fade" } }, show).transitionMs).toBe(1000);
    expect(effective({ ...slide, transitionMs: 3000 }, show).transitionMs).toBe(3000);
    show.look.transitionMs = 2000;
    expect(effective(slide, show).transitionMs).toBe(2000);
  });
});
