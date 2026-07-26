import { describe, expect, it } from "vitest";

import {
  shouldRouteDesktopSlash,
  type DesktopSlashContext,
} from "./desktopSlash";

const DEFAULT_CONTEXT: DesktopSlashContext = {
  key: "/",
  altKey: false,
  ctrlKey: false,
  metaKey: false,
  defaultPrevented: false,
  isComposing: false,
  coarsePointer: false,
  dialogOpen: false,
  editableTarget: false,
  terminalAvailable: true,
};

describe("desktop slash routing", () => {
  it("routes an unmodified slash to an available desktop terminal", () => {
    expect(shouldRouteDesktopSlash(DEFAULT_CONTEXT)).toBe(true);
  });

  it.each([
    ["another key", { key: "?" }],
    ["Alt modifier", { altKey: true }],
    ["Ctrl modifier", { ctrlKey: true }],
    ["Meta modifier", { metaKey: true }],
    ["prevented event", { defaultPrevented: true }],
    ["IME composition", { isComposing: true }],
    ["coarse pointer", { coarsePointer: true }],
    ["open dialog", { dialogOpen: true }],
    ["editable target", { editableTarget: true }],
    ["missing terminal", { terminalAvailable: false }],
  ])("does not route when %s applies", (_name, patch) => {
    expect(
      shouldRouteDesktopSlash({ ...DEFAULT_CONTEXT, ...patch }),
    ).toBe(false);
  });
});
