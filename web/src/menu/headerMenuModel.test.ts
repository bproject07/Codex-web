import { describe, expect, it } from "vitest";

import {
  firstEnabledIndex,
  lastEnabledIndex,
  menuKeyAction,
  nextEnabledIndex,
  triggerOpenAction,
} from "./headerMenuModel";

const enabled = { disabled: false };
const disabled = { disabled: true };

describe("header menu focus model", () => {
  it("finds the first and last enabled items", () => {
    expect(firstEnabledIndex([disabled, enabled, enabled])).toBe(1);
    expect(lastEnabledIndex([enabled, enabled, disabled])).toBe(1);
    expect(firstEnabledIndex([disabled, disabled])).toBe(-1);
    expect(lastEnabledIndex([])).toBe(-1);
  });

  it("moves with wrap-around and skips disabled items", () => {
    const items = [enabled, disabled, enabled, enabled];
    expect(nextEnabledIndex(items, 0, 1)).toBe(2);
    expect(nextEnabledIndex(items, 3, 1)).toBe(0);
    expect(nextEnabledIndex(items, 2, -1)).toBe(0);
    expect(nextEnabledIndex(items, 0, -1)).toBe(3);
  });

  it("stays put when it is the only enabled item", () => {
    expect(nextEnabledIndex([disabled, enabled, disabled], 1, 1)).toBe(1);
    expect(nextEnabledIndex([disabled], 0, 1)).toBe(-1);
    expect(nextEnabledIndex([], 0, 1)).toBe(-1);
  });

  it("maps menu keys to actions", () => {
    const items = [enabled, enabled, enabled];
    expect(menuKeyAction("ArrowDown", items, 0)).toEqual({
      kind: "move",
      index: 1,
    });
    expect(menuKeyAction("ArrowUp", items, 0)).toEqual({
      kind: "move",
      index: 2,
    });
    expect(menuKeyAction("Home", items, 2)).toEqual({ kind: "move", index: 0 });
    expect(menuKeyAction("End", items, 0)).toEqual({ kind: "move", index: 2 });
    expect(menuKeyAction("Escape", items, 1)).toEqual({
      kind: "close",
      refocusTrigger: true,
      preventDefault: true,
    });
    // Tab refocuses the trigger but keeps the default action, so sequential
    // traversal continues from the trigger in either direction.
    expect(menuKeyAction("Tab", items, 1)).toEqual({
      kind: "close",
      refocusTrigger: true,
      preventDefault: false,
    });
    expect(menuKeyAction("a", items, 1)).toBeNull();
    expect(menuKeyAction("Enter", items, 1)).toBeNull();
  });

  it("recovers from an unknown current index at the list ends", () => {
    const items = [enabled, enabled, disabled];
    expect(menuKeyAction("ArrowDown", items, -1)).toEqual({
      kind: "move",
      index: 0,
    });
    expect(menuKeyAction("ArrowUp", items, -1)).toEqual({
      kind: "move",
      index: 1,
    });
  });

  it("opens from the trigger with the arrow keys only", () => {
    const items = [disabled, enabled, enabled];
    expect(triggerOpenAction("ArrowDown", items)).toBe(1);
    expect(triggerOpenAction("ArrowUp", items)).toBe(2);
    expect(triggerOpenAction("Enter", items)).toBeNull();
    expect(triggerOpenAction("Escape", items)).toBeNull();
  });
});
