import { describe, expect, it } from "vitest";
import { dropPoint } from "./drop";

describe("dropPoint", () => {
  it("scales physical pixels down on Windows only", () => {
    expect(dropPoint({ x: 400, y: 300 }, true, 2)).toEqual({ x: 200, y: 150 });
    // macOS and Linux report points, the same as CSS pixels.
    expect(dropPoint({ x: 400, y: 300 }, false, 2)).toEqual({ x: 400, y: 300 });
  });
});
