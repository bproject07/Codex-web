import { describe, expect, it } from "vitest";
import {
  confirmHandoffDiscard,
  handoffDraftForTurn,
  resolveInitialThreadSelection,
  synchronizeHandoffDraft,
} from "./composerState";
import type { PeerThread, PeerTurn } from "./types";

const REQUESTED_THREAD_ID = "11111111-1111-4111-8111-111111111111";

const TURN: PeerTurn = {
  id: "22222222-2222-4222-8222-222222222222",
  sequence: 1,
  action: "review",
  instruction: "Review it.",
  status: "awaiting_preview",
  handoff: "Canonical handoff.",
  handoffRevision: 1,
  response: null,
  error: null,
};

function thread(id: string, updatedAt: number): PeerThread {
  return {
    id,
    sourceTerminalId: "33333333-3333-4333-8333-333333333333",
    reviewerTerminalId: "44444444-4444-4444-8444-444444444444",
    targetAgent: "claude",
    status: "awaiting_preview",
    currentTurn: TURN,
    createdAt: 1_000,
    updatedAt,
  };
}

describe("peer composer state", () => {
  it("waits for an explicitly requested thread instead of selecting a sibling", () => {
    const sibling = thread("55555555-5555-4555-8555-555555555555", 2_000);

    expect(
      resolveInitialThreadSelection(REQUESTED_THREAD_ID, [sibling]),
    ).toEqual({
      kind: "waiting",
      threadId: REQUESTED_THREAD_ID,
    });
    expect(
      resolveInitialThreadSelection(REQUESTED_THREAD_ID, [
        sibling,
        thread(REQUESTED_THREAD_ID, 1_000),
      ]),
    ).toEqual({
      kind: "selected",
      threadId: REQUESTED_THREAD_ID,
    });
  });

  it("clears both edited text and its turn identity when a preview is discarded", () => {
    const loaded = handoffDraftForTurn(TURN);
    const edited = { ...loaded, draft: "Unsaved local edit." };
    const rejected = confirmHandoffDiscard(
      edited,
      TURN.handoff,
      () => false,
    );
    const discarded = confirmHandoffDiscard(
      edited,
      TURN.handoff,
      () => true,
    );

    expect(edited.turnKey).not.toBe("");
    expect(rejected).toBeNull();
    expect(discarded).toEqual({ draft: "", turnKey: "" });
    expect(synchronizeHandoffDraft(discarded!, TURN)).toEqual({
      draft: "Canonical handoff.",
      turnKey: `${TURN.id}:${TURN.handoffRevision}`,
    });
    expect(synchronizeHandoffDraft(discarded!, TURN).draft).not.toBe(
      edited.draft,
    );
  });
});
