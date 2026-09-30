import { afterEach, describe, expect, it, vi } from "vitest";
import { DEFAULT_SETTINGS, loadSettings, saveSettings } from "./settings";

afterEach(() => vi.unstubAllGlobals());

function storageWith(value: unknown) {
  const storage = {
    getItem: vi.fn(() => value === undefined ? null : JSON.stringify(value)),
    setItem: vi.fn(),
  };
  vi.stubGlobal("window", { sessionStorage: storage });
  return storage;
}

describe("terminal retention preference", () => {
  it("is enabled for new users and existing preferences without the new field", () => {
    storageWith(undefined);
    expect(loadSettings().preserveTabs).toBe(true);
    storageWith({ fontSize: 18, mobileKeys: false });
    expect(loadSettings()).toMatchObject({
      preserveTabs: true, fontSize: 18, mobileKeys: false,
    });
  });

  it("preserves an explicit opt-out through storage", () => {
    const storage = storageWith({ ...DEFAULT_SETTINGS, preserveTabs: false });
    const loaded = loadSettings();
    expect(loaded.preserveTabs).toBe(false);
    saveSettings(loaded);
    expect(JSON.parse(storage.setItem.mock.calls[0][1]).preserveTabs).toBe(false);
  });

  it("uses the default for invalid or unavailable storage", () => {
    storageWith({ preserveTabs: "false" });
    expect(loadSettings().preserveTabs).toBe(true);
    storageWith(null);
    expect(loadSettings().preserveTabs).toBe(true);
    vi.stubGlobal("window", {
      sessionStorage: { getItem: () => { throw new Error("blocked"); } },
    });
    expect(loadSettings().preserveTabs).toBe(true);
  });
});
