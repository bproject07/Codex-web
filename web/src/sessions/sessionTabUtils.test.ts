import { describe, expect, it } from "vitest";

import {
  compactSessionName,
  horizontalWheelDelta,
} from "./sessionTabUtils";

describe("session tab labels", () => {
  it("uses the terminal number when the name contains one", () => {
    expect(compactSessionName("Terminal 4", 0)).toBe("T4");
    expect(compactSessionName("Workspace 12", 0)).toBe("T12");
  });

  it("falls back to the ordered position", () => {
    expect(compactSessionName("Primary", 2)).toBe("T3");
  });
});

describe("session tab wheel navigation", () => {
  it("uses the dominant wheel axis for horizontal scrolling", () => {
    expect(horizontalWheelDelta(32, 4)).toBe(32);
    expect(horizontalWheelDelta(2, -48)).toBe(-48);
  });
});
