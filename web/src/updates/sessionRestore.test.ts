import { describe, expect, it, vi } from "vitest";
import { ApiError, type SessionSnapshot } from "../api";
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
      "ordinary-old",
      undefined,
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

  it.each(["response lost", "storage failed"])("reuses the request after %s without duplicating the server entry", async (failure) => {
    const storage = new MemoryStorage();
    stageSessionRestorePlan({ sourceVersion: "0.3.0", targetVersion: "0.4.0",
      sessions: [session("old-primary", { isPrimary: true }), session("old-tab")],
      selectedTerminalId: "old-tab", storage });
    const server = new Map<string, SessionSnapshot>();
    let first = true;
    const create = vi.fn(async (_token: string, _agent: SessionSnapshot["agent"], _directory?: string | null, requestId?: string) => {
      if (!requestId) throw new Error("missing request identity");
      if (!server.has(requestId)) server.set(requestId, session("new-tab"));
      if (failure === "response lost" && first) { first = false; throw new Error("lost response"); }
      return server.get(requestId)!;
    });
    const write = vi.spyOn(storage, "setItem");
    if (failure === "storage failed") write.mockImplementationOnce(() => { throw new Error("quota"); });
    const primary = session("new-primary", { isPrimary: true });
    const options = { token: "token", serverVersion: "0.4.0", storage, create };
    expect((await restoreSessionTabs({ ...options, sessions: [primary] })).error).toBeDefined();
    const restored = await restoreSessionTabs({ ...options, sessions: [primary, ...server.values()] });
    expect(server.size).toBe(1);
    expect(restored.sessions.map((entry) => entry.terminalId)).toEqual(["new-primary", "new-tab"]);
    expect(restored.preferredTerminalId).toBe("new-tab");
    expect(storage.values.size).toBe(0);
  });

  it("stops creating remaining tabs when the owner cancels", async () => {
    const storage = new MemoryStorage();
    stageSessionRestorePlan({ sourceVersion: "0.3.0", targetVersion: "0.4.0",
      sessions: [session("old-primary", { isPrimary: true }), session("one"), session("two")],
      selectedTerminalId: "one", storage });
    const controller = new AbortController();
    const create = vi.fn(async () => { controller.abort(); return session("new-one"); });
    await restoreSessionTabs({ token: "token", serverVersion: "0.4.0",
      sessions: [session("new-primary", { isPrimary: true })], storage, create, signal: controller.signal });
    expect(create).toHaveBeenCalledTimes(1);
    expect(storage.values.size).toBe(1);
  });

  it("persists a deleted-tab skip before a later capacity failure and restores the rest on reload", async () => {
    const storage = new MemoryStorage();
    stageSessionRestorePlan({
      sourceVersion: "0.3.0", targetVersion: "0.4.0",
      sessions: [session("old-primary", { isPrimary: true }), session("deleted"), session("remaining")],
      selectedTerminalId: "deleted", storage,
    });
    const primary = session("new-primary", { isPrimary: true });
    const create = vi.fn()
      .mockRejectedValueOnce(new ApiError(409, "Deleted", "application/json", "restore_deleted"))
      .mockRejectedValueOnce(new ApiError(409, "Full", "application/json", "session_capacity"))
      .mockResolvedValueOnce(session("new-remaining"));
    const options = { token: "token", serverVersion: "0.4.0", storage, create, sessions: [primary] };
    const first = await restoreSessionTabs(options);
    expect(first.error).toBeDefined();
    expect(first.preferredTerminalId).toBe(primary.terminalId);
    const saved = JSON.parse([...storage.values.values()][0]);
    expect(saved.sessions.map((entry: { sourceTerminalId: string }) => entry.sourceTerminalId)).toEqual(["remaining"]);
    const next = await restoreSessionTabs(options);
    expect(next.error).toBeUndefined();
    expect(next.sessions.map((entry) => entry.terminalId)).toEqual(["new-primary", "new-remaining"]);
    expect(create.mock.calls.map((call) => call[3])).toEqual(["deleted", "remaining", "remaining"]);
    expect(storage.values.size).toBe(0);
  });

  it("does not revive a deleted mapping after another restore succeeds and the next one fails", async () => {
    const storage = new MemoryStorage();
    stageSessionRestorePlan({
      sourceVersion: "0.3.0", targetVersion: "0.4.0",
      sessions: [session("old-primary", { isPrimary: true }), session("deleted"), session("one"), session("two")],
      selectedTerminalId: "deleted", storage,
    });
    const primary = session("new-primary", { isPrimary: true });
    const create = vi.fn()
      .mockResolvedValueOnce(session("new-deleted"))
      .mockRejectedValueOnce(new Error("unavailable"))
      .mockRejectedValueOnce(new ApiError(409, "Deleted", "application/json", "restore_deleted"))
      .mockResolvedValueOnce(session("new-one"))
      .mockRejectedValueOnce(new ApiError(409, "Full", "application/json", "session_capacity"))
      .mockResolvedValueOnce(session("new-two"));
    const options = { token: "token", serverVersion: "0.4.0", storage, create };
    expect((await restoreSessionTabs({ ...options, sessions: [primary] })).error).toBeDefined();
    // The user deleted new-deleted before reloading the partially completed plan.
    const second = await restoreSessionTabs({ ...options, sessions: [primary] });
    expect(second.error).toBeDefined();
    expect(second.preferredTerminalId).toBe(primary.terminalId);
    const saved = JSON.parse([...storage.values.values()][0]);
    expect(saved.restored).toEqual([{ sourceTerminalId: "one", terminalId: "new-one" }]);
    const last = await restoreSessionTabs({ ...options, sessions: [primary, session("new-one")] });
    expect(last.error).toBeUndefined();
    expect(last.sessions.map((entry) => entry.terminalId)).toEqual(["new-primary", "new-one", "new-two"]);
    expect(create.mock.calls.map((call) => call[3])).toEqual(["deleted", "one", "deleted", "one", "two", "two"]);
    expect(storage.values.size).toBe(0);
  });

  it.each([undefined, "session_capacity", "restore_history_full", "restore_conflict"])(
    "keeps a restore entry for conflict code %s",
    async (code) => {
      const storage = new MemoryStorage();
      stageSessionRestorePlan({
        sourceVersion: "0.3.0", targetVersion: "0.4.0",
        sessions: [session("old-primary", { isPrimary: true }), session("one"), session("two")],
        selectedTerminalId: "one", storage,
      });
      const before = [...storage.values.values()][0];
      const create = vi.fn().mockRejectedValue(new ApiError(409, "Conflict", "application/json", code));
      const result = await restoreSessionTabs({
        token: "token", serverVersion: "0.4.0", storage, create,
        sessions: [session("new-primary", { isPrimary: true })],
      });
      expect(result.error).toBeDefined();
      expect(create).toHaveBeenCalledTimes(1);
      expect([...storage.values.values()][0]).toBe(before);
    },
  );

  it("keeps the plan when recording a deleted-tab skip fails", async () => {
    const storage = new MemoryStorage();
    stageSessionRestorePlan({
      sourceVersion: "0.3.0", targetVersion: "0.4.0",
      sessions: [session("old-primary", { isPrimary: true }), session("one"), session("two")],
      selectedTerminalId: "one", storage,
    });
    vi.spyOn(storage, "setItem").mockImplementation(() => { throw new Error("quota"); });
    const create = vi.fn().mockRejectedValue(new ApiError(409, "Deleted", "application/json", "restore_deleted"));
    const result = await restoreSessionTabs({
      token: "token", serverVersion: "0.4.0", storage, create,
      sessions: [session("new-primary", { isPrimary: true })],
    });
    expect(result.error).toContain("could not save progress");
    expect(create).toHaveBeenCalledTimes(1);
    expect(storage.values.size).toBe(1);
  });
});
