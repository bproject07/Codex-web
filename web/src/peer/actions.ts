import type { PeerAction, PeerStatus, PeerThread } from "./types";

export interface PeerActionDescriptor {
  action: PeerAction;
  label: string;
  description: string;
  followUpOnly?: boolean;
}

export const PEER_ACTIONS: readonly PeerActionDescriptor[] = [
  {
    action: "review",
    label: "Review",
    description: "Independently review the discussion, decision, or completed work.",
  },
  {
    action: "verify",
    label: "Verify",
    description: "Check a specific claim, result, or implementation detail.",
  },
  {
    action: "ask",
    label: "Ask",
    description: "Ask the reviewer for another perspective or recommendation.",
  },
  {
    action: "handoff",
    label: "Handoff",
    description: "Transfer a summarized task and its relevant context.",
  },
  {
    action: "recheck",
    label: "Recheck",
    description: "Revisit an earlier result while preserving reviewer context.",
    followUpOnly: true,
  },
] as const;

export const ACTIVE_PEER_STATUSES = new Set<PeerStatus>([
  "preparing_handoff",
  "awaiting_preview",
  "reviewing",
]);

export function actionsForThread(
  thread: PeerThread | null,
): readonly PeerActionDescriptor[] {
  return PEER_ACTIONS.filter((descriptor) =>
    thread ? true : !descriptor.followUpOnly,
  );
}

export function peerThreadDisplayId(threadId: string): string {
  const compact = threadId.replace(/[^A-Za-z0-9]/g, "").slice(0, 6);
  return `R-${(compact || threadId.slice(0, 6) || "peer").toUpperCase()}`;
}

export function peerStatusLabel(status: PeerStatus): string {
  switch (status) {
    case "preparing_handoff":
      return "Preparing summary";
    case "awaiting_preview":
      return "Preview ready";
    case "reviewing":
      return "Reviewer working";
    case "response_ready":
      return "Response ready";
    case "returned":
      return "Returned";
    case "failed":
      return "Failed";
    case "closed":
      return "Closed";
  }
}

export function isPeerWorkPending(status: PeerStatus): boolean {
  return ACTIVE_PEER_STATUSES.has(status);
}
