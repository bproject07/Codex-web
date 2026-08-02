import { describe, expect, it, vi } from "vitest";

import {
  loadMobileHeaderCollapsed,
  saveMobileHeaderCollapsed,
  type MobileHeaderStorage,
} from "./mobileHeader";

describe("mobile header preference", () => {
  it("defaults to collapsed without durable browser state", () => {
    expect(loadMobileHeaderCollapsed(null)).toBe(true);
    expect(
      loadMobileHeaderCollapsed({
        getItem: () => null,
        setItem: vi.fn(),
      }),
    ).toBe(true);
  });

  it("restores only an explicitly expanded header", () => {
    let value: string | null = "expanded";
    const storage = {
      getItem: vi.fn(() => value),
      setItem: vi.fn(),
    };

    expect(loadMobileHeaderCollapsed(storage)).toBe(false);
    value = "collapsed";
    expect(loadMobileHeaderCollapsed(storage)).toBe(true);
    value = "unexpected";
    expect(loadMobileHeaderCollapsed(storage)).toBe(true);
  });

  it("stores both user-selected states", () => {
    const values = new Map<string, string>();
    const storage: MobileHeaderStorage = {
      getItem: (key) => values.get(key) ?? null,
      setItem: (key, value) => values.set(key, value),
    };

    saveMobileHeaderCollapsed(false, storage);
    expect(loadMobileHeaderCollapsed(storage)).toBe(false);

    saveMobileHeaderCollapsed(true, storage);
    expect(loadMobileHeaderCollapsed(storage)).toBe(true);
  });

  it("continues when browser storage is unavailable", () => {
    const storage: MobileHeaderStorage = {
      getItem: () => {
        throw new Error("blocked");
      },
      setItem: () => {
        throw new Error("blocked");
      },
    };

    expect(loadMobileHeaderCollapsed(storage)).toBe(true);
    expect(() => saveMobileHeaderCollapsed(false, storage)).not.toThrow();
  });
});
