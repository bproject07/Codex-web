import { describe, expect, it } from "vitest";
import { isCleanTerminalId, normalizeSessionSnapshot } from "./api";

describe("terminal session helpers", () => {
  it("accepts storage-safe terminal identifiers", () => {
    expect(isCleanTerminalId("primary")).toBe(true);
    expect(isCleanTerminalId("550e8400-e29b-41d4-a716-446655440000")).toBe(true);
    expect(isCleanTerminalId(" terminal ")).toBe(false);
    expect(isCleanTerminalId("../terminal")).toBe(false);
  });

  it("normalizes a legacy primary-session snapshot", () => {
    const session = normalizeSessionSnapshot(
      {
        sessionId: "session-id",
        status: "running",
        connected: true,
        connectedClients: 1,
        startedAt: 123,
        pid: 456,
        exitCode: null,
        project: "C:\\Projects\\my-app",
        lastError: null,
      },
      "primary",
    );

    expect(session).toMatchObject({
      terminalId: "primary",
      name: "Primary",
      isPrimary: true,
      createdAt: 123,
      directoryId: "",
    });
  });
});
