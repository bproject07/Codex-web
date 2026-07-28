import type { PeerThread, PeerTurn } from "./types";

export type InitialThreadSelection =
  | { kind: "selected"; threadId: string }
  | { kind: "new" }
  | { kind: "waiting"; threadId: string };

export interface HandoffDraftState {
  draft: string;
  turnKey: string;
}

export function resolveInitialThreadSelection(
  initialThreadId: string | null,
  activeThreads: readonly PeerThread[],
): InitialThreadSelection {
  if (initialThreadId !== null) {
    const requested = activeThreads.find(
      (thread) => thread.id === initialThreadId,
    );
    return requested
      ? { kind: "selected", threadId: requested.id }
      : { kind: "waiting", threadId: initialThreadId };
  }

  const mostRecent = activeThreads[0];
  return mostRecent
    ? { kind: "selected", threadId: mostRecent.id }
    : { kind: "new" };
}

export function handoffDraftForTurn(
  turn: PeerTurn | null | undefined,
): HandoffDraftState {
  if (!turn || turn.status !== "awaiting_preview") {
    return emptyHandoffDraft();
  }
  return {
    draft: turn.handoff ?? "",
    turnKey: handoffTurnKey(turn),
  };
}

export function synchronizeHandoffDraft(
  state: HandoffDraftState,
  turn: PeerTurn | null | undefined,
): HandoffDraftState {
  if (!turn || turn.status !== "awaiting_preview") {
    return state;
  }
  const turnKey = handoffTurnKey(turn);
  return state.turnKey === turnKey
    ? state
    : {
        draft: turn.handoff ?? "",
        turnKey,
      };
}

export function emptyHandoffDraft(): HandoffDraftState {
  return { draft: "", turnKey: "" };
}

export function discardHandoffDraft(
  _current: HandoffDraftState,
): HandoffDraftState {
  return emptyHandoffDraft();
}

export function confirmHandoffDiscard(
  current: HandoffDraftState,
  canonicalDraft: string | null,
  confirmDiscard: () => boolean,
): HandoffDraftState | null {
  const dirty =
    canonicalDraft !== null && current.draft !== canonicalDraft;
  if (dirty && !confirmDiscard()) {
    return null;
  }
  return discardHandoffDraft(current);
}

function handoffTurnKey(turn: PeerTurn): string {
  return `${turn.id}:${turn.handoffRevision}`;
}
