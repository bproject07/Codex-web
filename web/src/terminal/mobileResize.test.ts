import { describe, expect, it } from "vitest";
import {
  isMobileRowOnlyResize,
  terminalScrollbarWidth,
} from "./mobileResize";

describe("terminalScrollbarWidth", () => {
  it("reserves only a narrow mobile rail without changing desktop", () => {
    expect(terminalScrollbarWidth(true)).toBe(4);
    expect(terminalScrollbarWidth(false)).toBeUndefined();
  });
});

describe("isMobileRowOnlyResize", () => {
  it("detects mobile row-only shrink and growth", () => {
    expect(
      isMobileRowOnlyResize(
        { cols: 41, rows: 31 },
        { cols: 41, rows: 15 },
        true,
      ),
    ).toBe(true);
    expect(
      isMobileRowOnlyResize(
        { cols: 41, rows: 15 },
        { cols: 41, rows: 31 },
        true,
      ),
    ).toBe(true);
  });

  it("keeps width changes and desktop resizes", () => {
    expect(
      isMobileRowOnlyResize(
        { cols: 41, rows: 31 },
        { cols: 72, rows: 15 },
        true,
      ),
    ).toBe(false);
    expect(
      isMobileRowOnlyResize(
        { cols: 41, rows: 31 },
        { cols: 41, rows: 15 },
        false,
      ),
    ).toBe(false);
  });

  it("ignores unchanged sizes", () => {
    expect(
      isMobileRowOnlyResize(
        { cols: 41, rows: 31 },
        { cols: 41, rows: 31 },
        true,
      ),
    ).toBe(false);
  });
});
