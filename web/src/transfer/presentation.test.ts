import { describe, expect, it } from "vitest";
import type { SessionSnapshot } from "../api";
import {
  draftEditable,
  eligibleExistingTargets,
  generateRequestId,
  targetTerminalToSelect,
  transferErrorLabel,
  transferStateLabel,
} from "./presentation";
import type { TransferSnapshot } from "./types";

function session(
  terminalId: string,
  overrides: Partial<SessionSnapshot> = {},
): SessionSnapshot {
  return {
    terminalId,
    name: terminalId,
    agent: "codex",
    agentLabel: "Codex",
    isPrimary: false,
    createdAt: 1,
    sessionId: "33333333-3333-4333-8333-333333333333",
    status: "running",
    connected: true,
    connectedClients: 0,
    startedAt: 1,
    pid: 1,
    exitCode: null,
    project: "/p",
    directoryId: "dir",
    lastError: null,
    purpose: { kind: "interactive" },
    ...overrides,
  };
}

function transfer(overrides: Partial<TransferSnapshot>): TransferSnapshot {
  return {
    transferId: "t",
    revision: 1,
    state: "preview",
    mode: "manual",
    sourceTerminalId: "source",
    sourceSessionId: null,
    draftRevision: 1,
    approvedRevision: null,
    preparation: null,
    attempt: null,
    attemptsUsed: 0,
    delivery: null,
    errorCode: null,
    createdAt: 1,
    updatedAt: 1,
    ...overrides,
  };
}

describe("transfer presentation", () => {
  it("offers only other running ordinary tabs with a known agent as targets", () => {
    const source = session("source");
    const eligible = session("eligible", { agent: "kilo", agentLabel: "Kilo" });
    const stopped = session("stopped", { status: "exited", sessionId: null });
    const peer = session("peer", {
      purpose: { kind: "peer", threadId: "th", parentTerminalId: "source" },
    });
    const unknown = session("unknown", { agent: null, agentLabel: "future" });

    expect(
      eligibleExistingTargets([source, eligible, stopped, peer, unknown], "source").map(
        (candidate) => candidate.terminalId,
      ),
    ).toEqual(["eligible"]);
  });

  it("labels every state and known error code without leaking raw codes", () => {
    for (const state of [
      "preparing",
      "preview",
      "starting",
      "cleanup_pending",
      "target_bound",
      "delivering",
      "awaiting_receipt",
      "delivered",
      "failed",
      "abandoned",
    ] as const) {
      expect(transferStateLabel(state)).not.toHaveLength(0);
    }
    expect(transferStateLabel("delivering")).toBe(transferStateLabel("awaiting_receipt"));
    expect(transferErrorLabel(null)).toBeNull();
    expect(transferErrorLabel("target_ended")).toContain("draft was kept");
    expect(transferErrorLabel("something_new")).toContain("something_new");
  });

  it("edits the draft only while a transfer can still change it", () => {
    expect(draftEditable(null)).toBe(false);
    expect(draftEditable(transfer({ state: "preparing" }))).toBe(true);
    expect(draftEditable(transfer({ state: "target_bound" }))).toBe(true);
    expect(draftEditable(transfer({ state: "delivering" }))).toBe(false);
    expect(draftEditable(transfer({ state: "delivered" }))).toBe(false);
  });

  it("selects the target tab only while it is starting or bound", () => {
    const attempt = {
      targetRequestId: "r",
      number: 1,
      target: {
        mode: "new" as const,
        terminalId: "target",
        sessionId: null,
        agent: "kilo" as const,
        name: "Kilo 2",
        project: "/p",
      },
      outcome: "starting" as const,
    };
    expect(targetTerminalToSelect(transfer({ attempt }))).toBe("target");
    expect(
      targetTerminalToSelect(transfer({ attempt: { ...attempt, outcome: "bound" } })),
    ).toBe("target");
    expect(
      targetTerminalToSelect(transfer({ attempt: { ...attempt, outcome: "start_failed" } })),
    ).toBeNull();
    expect(targetTerminalToSelect(null)).toBeNull();
  });

  it("generates distinct v4 request ids", () => {
    const first = generateRequestId();
    const second = generateRequestId();
    expect(first).toMatch(/^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/);
    expect(first).not.toBe(second);
  });
});
