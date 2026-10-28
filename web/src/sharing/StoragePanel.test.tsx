import { describe, expect, it } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";
import { StoragePanel, storageBytes } from "./StoragePanel";
import { SharingError } from "./api";

describe("storage recovery", () => {
  it("makes the destructive scope clear before loading and never renders credentials", () => {
    const html = renderToStaticMarkup(<StoragePanel token="synthetic-owner-secret" />);
    expect(html).toContain("this server");
    expect(html).toContain("permanently removes");
    expect(html).toContain("Loading storage");
    expect(html).not.toContain("synthetic-owner-secret");
  });
  it("distinguishes actionable permission, ownership, lease and maintenance errors", () => {
    expect(new SharingError("unsafe_state_directory", 503).message).toContain("Storage");
    expect(new SharingError("permissions_owner_mismatch", 409).message).toContain("another account");
    expect(new SharingError("sharing_store_in_use", 503).message).toContain("Another server");
    expect(new SharingError("recording_enabled", 409).message).toContain("Stop recording");
    expect(new SharingError("cleanup_changed", 409).message).toContain("Preview cleanup again");
  });
  it("shows meaningful physical sizes without treating missing metrics as zero", () => {
    expect(storageBytes(Number.NaN)).toBe("Unknown");
    expect(storageBytes(-1)).toBe("Unknown");
    expect(storageBytes(0)).toBe("0 B");
    expect(storageBytes(128 * 1024 * 1024)).toBe("128.0 MiB");
    expect(storageBytes(3 * 1024 ** 3)).toBe("3.00 GiB");
  });
});
