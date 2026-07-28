import { describe, expect, it } from "vitest";
import { isCleanTerminalId, normalizeSessionSnapshot } from "./api";

const LEGACY_SESSION = {
  sessionId: "session-id",
  status: "running" as const,
  connected: true,
  connectedClients: 1,
  startedAt: 123,
  pid: 456,
  exitCode: null,
  project: "C:\\Projects\\my-app",
  lastError: null,
};

describe("terminal session helpers", () => {
  it("accepts storage-safe terminal identifiers", () => {
    expect(isCleanTerminalId("primary")).toBe(true);
    expect(isCleanTerminalId("550e8400-e29b-41d4-a716-446655440000")).toBe(true);
    expect(isCleanTerminalId(" terminal ")).toBe(false);
    expect(isCleanTerminalId("../terminal")).toBe(false);
  });

  it("normalizes a legacy primary-session snapshot", () => {
    const session = normalizeSessionSnapshot(
      LEGACY_SESSION,
      "primary",
    );

    expect(session).toMatchObject({
      terminalId: "primary",
      name: "Primary",
      isPrimary: true,
      createdAt: 123,
      directoryId: "",
      purpose: { kind: "interactive" },
    });
  });

  it("rejects malformed purpose metadata instead of treating it as interactive", () => {
    expect(() =>
      normalizeSessionSnapshot(
        {
          ...LEGACY_SESSION,
          purpose: {
            kind: "peer",
            threadId: "../thread",
            parentTerminalId: "primary",
          },
        },
        "primary",
      ),
    ).toThrow("invalid terminal session purpose");

    expect(() =>
      normalizeSessionSnapshot(
        {
          ...LEGACY_SESSION,
          purpose: undefined,
        },
        "primary",
      ),
    ).toThrow("invalid terminal session purpose");
  });
});
