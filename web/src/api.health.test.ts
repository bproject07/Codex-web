import { afterEach, describe, expect, it, vi } from "vitest";
import { ApiError, getHealth } from "./api";

const HEALTH = {
  status: "ok",
  serverVersion: "0.2.0",
  codexInstalled: true,
  sessionRunning: true,
  connectedClients: 1,
  sessionCount: 3,
  runningSessions: 3,
  maxSessions: 20,
  serverRestartSupported: true,
};

function jsonResponse(body: unknown): Response {
  return new Response(JSON.stringify(body), {
    status: 200,
    headers: { "Content-Type": "application/json" },
  });
}

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("health API", () => {
  it("loads authenticated configured session capacity", async () => {
    const fetchMock = vi.fn().mockResolvedValue(jsonResponse(HEALTH));
    vi.stubGlobal("fetch", fetchMock);

    await expect(getHealth("0123456789abcdef")).resolves.toEqual(HEALTH);

    expect(fetchMock).toHaveBeenCalledTimes(1);
    const [path, init] = fetchMock.mock.calls[0] as [string, RequestInit];
    expect(path).toBe("/api/health");
    expect(new Headers(init.headers).get("Authorization")).toBe(
      "Bearer 0123456789abcdef",
    );
  });

  it.each([0, -1, 1.5, "20"])(
    "rejects malformed maxSessions value %s",
    async (maxSessions) => {
      vi.stubGlobal(
        "fetch",
        vi.fn().mockResolvedValue(jsonResponse({ ...HEALTH, maxSessions })),
      );

      const result = getHealth("0123456789abcdef");
      await expect(result).rejects.toBeInstanceOf(ApiError);
      await expect(result).rejects.toMatchObject({
        status: 502,
        message: "The server returned an invalid health response.",
      });
    },
  );

  it("keeps capacity checks compatible when an older server omits its version", async () => {
    const {
      serverVersion: _serverVersion,
      serverRestartSupported: _serverRestartSupported,
      ...legacyHealth
    } = HEALTH;
    vi.stubGlobal("fetch", vi.fn().mockResolvedValue(jsonResponse(legacyHealth)));

    await expect(getHealth("0123456789abcdef")).resolves.toEqual({
      ...legacyHealth,
      serverVersion: null,
      serverRestartSupported: false,
    });
  });
});
