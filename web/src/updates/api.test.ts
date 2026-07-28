import { afterEach, describe, expect, it, vi } from "vitest";
import { ApiError } from "../api";
import {
  applyUpdate,
  checkForUpdate,
  getUpdateStatus,
  parseUpdateStatus,
  type UpdateStatus,
} from "./api";

const STATUS: UpdateStatus = {
  schemaVersion: 1,
  currentVersion: "0.2.0",
  latestVersion: "0.3.0",
  state: "available",
  installSupported: true,
  installReason: null,
  releaseUrl:
    "https://github.com/bproject07/Codex-web/releases/tag/v0.3.0",
  progressPercent: null,
  error: null,
  checkedAt: "2026-07-28T12:00:00Z",
};

function response(body: unknown, status = 200): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "Content-Type": "application/json" },
  });
}

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("update API", () => {
  it("loads and checks update status through authenticated fixed endpoints", async () => {
    const fetchMock = vi
      .fn()
      .mockResolvedValueOnce(response(STATUS))
      .mockResolvedValueOnce(response({ ...STATUS, state: "upToDate" }));
    vi.stubGlobal("fetch", fetchMock);

    await expect(getUpdateStatus("0123456789abcdef")).resolves.toEqual(STATUS);
    await expect(checkForUpdate("0123456789abcdef")).resolves.toMatchObject({
      state: "upToDate",
    });

    expect(fetchMock.mock.calls[0]?.[0]).toBe("/api/update");
    expect(fetchMock.mock.calls[1]?.[0]).toBe("/api/update/check");
    expect(fetchMock.mock.calls[1]?.[1]).toMatchObject({ method: "POST" });
  });

  it("sends only the selected version and explicit termination confirmation", async () => {
    const fetchMock = vi
      .fn()
      .mockResolvedValue(response({ ...STATUS, state: "downloading", progressPercent: 0 }, 202));
    vi.stubGlobal("fetch", fetchMock);

    await expect(
      applyUpdate("0123456789abcdef", "0.3.0"),
    ).resolves.toMatchObject({ state: "downloading" });

    const [path, init] = fetchMock.mock.calls[0] as [string, RequestInit];
    expect(path).toBe("/api/update/apply");
    expect(init.method).toBe("POST");
    expect(JSON.parse(String(init.body))).toEqual({
      expectedVersion: "0.3.0",
      confirmSessionTermination: true,
    });
  });

  it("rejects malformed or unsafe server responses", () => {
    expect(() =>
      parseUpdateStatus({
        ...STATUS,
        releaseUrl: "http://untrusted.example/update.zip",
      }),
    ).toThrow(ApiError);
    expect(() =>
      parseUpdateStatus({ ...STATUS, progressPercent: 101 }),
    ).toThrow(ApiError);
    expect(() =>
      parseUpdateStatus({ ...STATUS, state: "installingAnything" }),
    ).toThrow(ApiError);
  });
});
