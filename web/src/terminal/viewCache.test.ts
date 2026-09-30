import { describe, expect, it } from "vitest";
import { retainTerminalViews, TERMINAL_VIEW_CACHE_LIMIT } from "./viewCache";

describe("visited terminal views", () => {
  const available = ["a", "b", "c", "d", "e", "f", "g", "h"];

  it("retains visited tabs and keeps a repeated selection unchanged", () => {
    const first = retainTerminalViews([], "a", available, true);
    const second = retainTerminalViews(first, "b", available, true);
    expect(second).toEqual(["a", "b"]);
    expect(retainTerminalViews(second, "b", available, true)).toBe(second);
    expect(retainTerminalViews(second, "a", available, true)).toEqual(["b", "a"]);
  });

  it("evicts the least recently visited view while keeping the active one", () => {
    let retained: readonly string[] = [];
    for (const selected of available.slice(0, TERMINAL_VIEW_CACHE_LIMIT)) {
      retained = retainTerminalViews(retained, selected, available, true);
    }
    retained = retainTerminalViews(retained, "a", available, true);
    retained = retainTerminalViews(retained, "g", available, true);
    expect(retained).toEqual(["c", "d", "e", "f", "a", "g"]);
    expect(retained).toHaveLength(TERMINAL_VIEW_CACHE_LIMIT);
  });

  it("releases hidden views when disabled and starts retaining visits when enabled", () => {
    const single = retainTerminalViews(["a", "b", "c"], "b", available, false);
    expect(single).toEqual(["b"]);
    expect(retainTerminalViews(single, "c", available, false)).toEqual(["c"]);
    expect(retainTerminalViews(single, "c", available, true)).toEqual(["b", "c"]);
  });

  it("prunes removed sessions and clears all views after a server replacement", () => {
    expect(retainTerminalViews(["a", "b", "c"], "b", ["a", "b"], true))
      .toEqual(["a", "b"]);
    expect(retainTerminalViews(["a", "b"], "new", ["new"], true))
      .toEqual(["new"]);
    expect(retainTerminalViews(["a", "b"], "a", [], true)).toEqual([]);
  });
});
