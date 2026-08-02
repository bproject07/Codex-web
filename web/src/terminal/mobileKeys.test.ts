import { describe, expect, it } from "vitest";
import { applyCtrlToInput, MOBILE_KEY_SEQUENCES } from "./mobileKeys";

describe("mobile Ctrl conversion", () => {
  it("converts letters to control characters", () => {
    expect(applyCtrlToInput("c")).toEqual({ data: "\u0003", consumed: true });
    expect(applyCtrlToInput("L")).toEqual({ data: "\u000c", consumed: true });
  });

  it("waits for a letter before consuming Ctrl mode", () => {
    expect(applyCtrlToInput("1")).toEqual({ data: "1", consumed: false });
    expect(applyCtrlToInput("ab")).toEqual({ data: "ab", consumed: false });
  });

  it("defines standard VT navigation sequences", () => {
    expect(MOBILE_KEY_SEQUENCES.arrowUp).toBe("\u001b[A");
    expect(MOBILE_KEY_SEQUENCES.pageUp).toBe("\u001b[5~");
    expect(MOBILE_KEY_SEQUENCES.pageDown).toBe("\u001b[6~");
  });
});
