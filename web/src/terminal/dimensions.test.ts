import { describe, expect, it } from "vitest";
import { terminalDimensions } from "./dimensions";

describe("terminal dimensions", () => {
  it.each([
    [2, 1, 20, 5], [1000, 900, 500, 300], [80, 24, 80, 24], [80.9, 24.9, 80, 24],
  ])("keeps a %s by %s viewport within the server contract", (cols, rows, expectedCols, expectedRows) => {
    expect(terminalDimensions(cols, rows)).toEqual({ cols: expectedCols, rows: expectedRows });
  });
});
