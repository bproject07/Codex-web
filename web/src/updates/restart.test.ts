import { describe, expect, it, vi } from "vitest";
import type { HealthSnapshot } from "../api";
import {
  isUpdateHandoffState,
  isUpdatePollState,
  reloadForUpdatedServer,
  waitForServerVersion,
} from "./restart";

const health = (serverVersion: string | null): HealthSnapshot => ({
  status: "ok",
  serverVersion,
  codexInstalled: true,
  sessionRunning: false,
  connectedClients: 0,
  sessionCount: 1,
  runningSessions: 0,
  maxSessions: 20,
});

describe("waitForServerVersion", () => {
  it("keeps polling through staging and starts handoff verification there", () => {
    expect(isUpdatePollState("downloading")).toBe(true);
    expect(isUpdatePollState("verifying")).toBe(true);
    expect(isUpdatePollState("staged")).toBe(true);
    expect(isUpdatePollState("available")).toBe(false);
    expect(isUpdateHandoffState("staged")).toBe(true);
    expect(isUpdateHandoffState("restarting")).toBe(true);
    expect(isUpdateHandoffState("verifying")).toBe(false);
  });

  it("tolerates a restart gap and accepts only the expected server version", async () => {
    const readHealth = vi
      .fn()
      .mockRejectedValueOnce(new TypeError("connection refused"))
      .mockResolvedValueOnce(health("0.2.0"))
      .mockResolvedValueOnce(health("0.3.0"));
    const wait = vi.fn().mockResolvedValue(undefined);

    await expect(
      waitForServerVersion({
        token: "0123456789abcdef",
        expectedVersion: "0.3.0",
        attempts: 3,
        intervalMs: 0,
        readHealth,
        wait,
      }),
    ).resolves.toBe(true);
    expect(readHealth).toHaveBeenCalledTimes(3);
  });

  it("returns false when cancelled", async () => {
    const abortController = new AbortController();
    abortController.abort();
    const readHealth = vi.fn();

    await expect(
      waitForServerVersion({
        token: "0123456789abcdef",
        expectedVersion: "0.3.0",
        signal: abortController.signal,
        readHealth,
      }),
    ).resolves.toBe(false);
    expect(readHealth).not.toHaveBeenCalled();
  });
});

describe("reloadForUpdatedServer", () => {
  it("uses a normal reload when the current tab persisted the token", () => {
    const reload = vi.fn();
    const replace = vi.fn();

    reloadForUpdatedServer({
      token: "0123456789abcdef",
      currentUrl: "https://terminal.example/app?mode=full#terminal",
      readStoredToken: () => "0123456789abcdef",
      reload,
      replace,
    });

    expect(reload).toHaveBeenCalledOnce();
    expect(replace).not.toHaveBeenCalled();
  });

  it("uses a same-origin token URL when storage could not persist the token", () => {
    const reload = vi.fn();
    const replace = vi.fn();

    reloadForUpdatedServer({
      token: "unsafe+/token=value",
      currentUrl: "https://terminal.example/app?mode=full#terminal",
      readStoredToken: () => "",
      reload,
      replace,
    });

    expect(reload).not.toHaveBeenCalled();
    expect(replace).toHaveBeenCalledWith(
      "/app?mode=full&token=unsafe%2B%2Ftoken%3Dvalue#terminal",
    );
  });
});
