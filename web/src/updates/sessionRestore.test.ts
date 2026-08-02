import { describe, expect, it, vi } from "vitest";
import type { SessionSnapshot } from "../api";
import {
  discardSessionRestorePlanForOriginalGeneration,
  restoreSessionTabs,
  stageSessionRestorePlan,
  type SessionRestoreStorage,
} from "./sessionRestore";

class MemoryStorage implements SessionRestoreStorage {
  readonly values = new Map<string, string>();

  getItem(key: string) {
    return this.values.get(key) ?? null;
  }

  setItem(key: string, value: string) {
    this.values.set(key, value);
  }

  removeItem(key: string) {
    this.values.delete(key);
  }
}

function session(
  terminalId: string,
  overrides: Partial<SessionSnapshot> = {},
): SessionSnapshot {
  return {
    terminalId,
    name: terminalId,
    agent: "codex",
    isPrimary: false,
    createdAt: 1,
    sessionId: `${terminalId}-generation`,
    status: "running",
    connected: false,
    connectedClients: 0,
    startedAt: 1,
    pid: 1,
    exitCode: null,
    project: "/workspace",
    directoryId: "1-d29ya3NwYWNl",
    lastError: null,
    purpose: { kind: "interactive" },
    ...overrides,
  };
}

describe("update session restoration", () => {
  it("stores only ordinary non-primary tabs", () => {
    const storage = new MemoryStorage();
    const result = stageSessionRestorePlan({
      sourceVersion: "0.3.0",
      targetVersion: "0.4.0",
      sessions: [
        session("primary", { isPrimary: true }),
        session("ordinary"),
        session("reviewer", {
          purpose: {
            kind: "peer",
            threadId: "thread",
            parentTerminalId: "ordinary",
          },
        }),
      ],
      selectedTerminalId: "ordinary",
      storage,
    });

    expect(result).toEqual({ sessionCount: 1, saved: true });
    expect([...storage.values.values()][0]).toContain("ordinary");
    expect([...storage.values.values()][0]).not.toContain("reviewer");
  });

  it("refuses a non-empty plan when browser storage is unavailable", () => {
    const result = stageSessionRestorePlan({
      sourceVersion: "0.3.0",
      targetVersion: "0.4.0",
      sessions: [
        session("primary", { isPrimary: true }),
        session("ordinary"),
      ],
      selectedTerminalId: "ordinary",
      storage: null,
    });

    expect(result).toEqual({ sessionCount: 1, saved: false });
  });

  it("discards a failed plan only while the original primary still exists", () => {
    const storage = new MemoryStorage();
    stageSessionRestorePlan({
      sourceVersion: "0.3.0",
      targetVersion: "0.4.0",
      sessions: [
        session("primary-old", { isPrimary: true }),
        session("ordinary"),
      ],
      selectedTerminalId: "ordinary",
      storage,
    });

    discardSessionRestorePlanForOriginalGeneration(
      [session("primary-new", { isPrimary: true })],
      storage,
    );
    expect(storage.values.size).toBe(1);

    discardSessionRestorePlanForOriginalGeneration(
      [session("primary-old", { isPrimary: true })],
      storage,
    );
    expect(storage.values.size).toBe(0);
  });

  it("waits for a new server generation and recreates the selected tab", async () => {
    const storage = new MemoryStorage();
    const original = [
      session("primary-old", { isPrimary: true }),
      session("ordinary-old", { agent: "claude" }),
    ];
    stageSessionRestorePlan({
      sourceVersion: "0.3.0",
      targetVersion: "0.4.0",
      sessions: original,
      selectedTerminalId: "ordinary-old",
      storage,
    });
    const create = vi
      .fn()
      .mockResolvedValue(
        session("ordinary-new", { agent: "claude", createdAt: 2 }),
      );

    const beforeRestart = await restoreSessionTabs({
      token: "token",
      serverVersion: "0.3.0",
      sessions: original,
      storage,
      create,
    });
    expect(beforeRestart.sessions).toEqual(original);
    expect(create).not.toHaveBeenCalled();

    const result = await restoreSessionTabs({
      token: "token",
      serverVersion: "0.4.0",
      sessions: [session("primary-new", { isPrimary: true })],
      storage,
      create,
    });

    expect(create).toHaveBeenCalledWith(
      "token",
      "claude",
      "1-d29ya3NwYWNl",
    );
    expect(result.preferredTerminalId).toBe("ordinary-new");
    expect(result.sessions.map((item) => item.terminalId)).toEqual([
      "primary-new",
      "ordinary-new",
    ]);
    expect(storage.values.size).toBe(0);
  });

  it("keeps a partial restore plan when a later tab cannot start", async () => {
    const storage = new MemoryStorage();
    stageSessionRestorePlan({
      sourceVersion: "0.3.0",
      targetVersion: "0.4.0",
      sessions: [
        session("primary-old", { isPrimary: true }),
        session("first-old"),
        session("second-old", { agent: "agy" }),
      ],
      selectedTerminalId: "first-old",
      storage,
    });
    const create = vi
      .fn()
      .mockResolvedValueOnce(session("first-new"))
      .mockRejectedValueOnce(new Error("unavailable"));

    const result = await restoreSessionTabs({
      token: "token",
      serverVersion: "0.4.0",
      sessions: [session("primary-new", { isPrimary: true })],
      storage,
      create,
    });

    expect(result.error).toContain("could not be recreated");
    expect(result.preferredTerminalId).toBe("first-new");
    expect(storage.values.size).toBe(1);
  });
});
