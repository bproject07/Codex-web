import type { AgentKind } from "../api";

export type PeerAction =
  | "review"
  | "verify"
  | "ask"
  | "handoff"
  | "recheck";

export type PeerStatus =
  | "preparing_handoff"
  | "awaiting_preview"
  | "reviewing"
  | "response_ready"
  | "returned"
  | "failed"
  | "closed";

export interface PeerTurn {
  id: string;
  sequence: number;
  action: PeerAction;
  instruction: string;
  status: PeerStatus;
  handoff: string | null;
  handoffRevision: number;
  response: string | null;
  error: string | null;
}

export interface PeerThread {
  id: string;
  sourceTerminalId: string;
  reviewerTerminalId: string | null;
  targetAgent: AgentKind;
  status: PeerStatus;
  currentTurn: PeerTurn;
  createdAt: number;
  updatedAt: number;
}

export interface CreatePeerThreadInput {
  sourceTerminalId: string;
  directoryId: string;
  targetAgent: AgentKind;
  action: Exclude<PeerAction, "recheck">;
  instruction: string;
  sourceReady: true;
}

export interface CreatePeerTurnInput {
  action: PeerAction;
  instruction: string;
  sourceReady: true;
}

export interface DispatchPeerTurnInput {
  turnId: string;
  handoffRevision: number;
  handoff: string;
  reviewerReady: true;
}

export interface ReturnPeerTurnInput {
  turnId: string;
  sourceReady: true;
}
