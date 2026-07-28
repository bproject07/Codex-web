import { describe, expect, it } from "vitest";
import {
  actionsForThread,
  peerStatusLabel,
  peerThreadDisplayId,
} from "./actions";
import type { PeerThread } from "./types";

const THREAD: PeerThread = {
  id: "12345678-1234-4234-8234-123456789abc",
  sourceTerminalId: "11111111-1111-4111-8111-111111111111",
  reviewerTerminalId: "22222222-2222-4222-8222-222222222222",
  targetAgent: "claude",
  status: "returned",
  currentTurn: {
    id: "33333333-3333-4333-8333-333333333333",
    sequence: 1,
    action: "review",
    instruction: "Review the proposal.",
    status: "returned",
    handoff: "A concise handoff.",
    handoffRevision: 1,
    response: "The proposal is sound.",
    error: null,
  },
  createdAt: 1_000,
  updatedAt: 2_000,
};

describe("peer action model", () => {
  it("offers recheck only after a dedicated thread exists", () => {
    expect(actionsForThread(null).map(({ action }) => action)).toEqual([
      "review",
      "verify",
      "ask",
      "handoff",
    ]);
    expect(actionsForThread(THREAD).map(({ action }) => action)).toContain(
      "recheck",
    );
  });

  it("builds stable short labels without exposing opaque ids in full", () => {
    expect(peerThreadDisplayId(THREAD.id)).toBe("R-123456");
    expect(peerStatusLabel("awaiting_preview")).toBe("Preview ready");
    expect(peerStatusLabel("response_ready")).toBe("Response ready");
  });
});
