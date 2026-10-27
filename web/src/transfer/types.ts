import type { AgentKind } from "../agents";

export type TransferMode = "prepare" | "manual";

export type TransferState =
  | "preparing"
  | "preview"
  | "starting"
  | "cleanup_pending"
  | "target_bound"
  | "delivering"
  | "awaiting_receipt"
  | "delivered"
  | "failed"
  | "abandoned";

export type TransferPreparation =
  | "pending"
  | "submitted"
  | "overridden"
  | "source_ended"
  | "queue_rejected";

export type TransferDelivery = "unconfirmed" | "target_ended";

export type TransferTargetMode = "new" | "existing";

export type TransferAttemptOutcome =
  | "starting"
  | "bound"
  | "start_failed"
  | "cleanup_pending"
  | "target_ended"
  | "delivered"
  | "abandoned";

export interface TransferTarget {
  mode: TransferTargetMode;
  terminalId: string;
  sessionId: string | null;
  agent: AgentKind;
  name: string;
  project: string;
}

export interface TransferAttempt {
  targetRequestId: string;
  number: number;
  target: TransferTarget;
  outcome: TransferAttemptOutcome;
}

export interface TransferSnapshot {
  transferId: string;
  revision: number;
  state: TransferState;
  mode: TransferMode;
  sourceTerminalId: string;
  sourceSessionId: string | null;
  draftRevision: number;
  approvedRevision: number | null;
  preparation: TransferPreparation | null;
  attempt: TransferAttempt | null;
  attemptsUsed: number;
  delivery: TransferDelivery | null;
  errorCode: string | null;
  createdAt: number;
  updatedAt: number;
}

export interface TransferDraft {
  draftRevision: number;
  content: string | null;
}

export interface CreateTransferInput {
  transferId: string;
  sourceTerminalId: string;
  sourceSessionId: string | null;
  mode: TransferMode;
  /** Required for `prepare`; it is the explicit readiness confirmation. */
  sourceReady: boolean;
  instruction: string;
}

export type TransferTargetSelection =
  | { mode: "new"; agent: AgentKind; directoryId: string | null }
  | { mode: "existing"; terminalId: string; sessionId: string };

export interface ChooseTargetInput {
  targetRequestId: string;
  target: TransferTargetSelection;
}

export interface DeliverTransferInput {
  expectedRevision: number;
  targetSessionId: string;
  targetReady: true;
}

export interface CleanupTransferInput {
  targetTerminalId: string;
  targetSessionId: string | null;
}

/** States in which the transfer still needs user attention or polling. */
export function isTransferActive(state: TransferState): boolean {
  return (
    state !== "delivered" && state !== "failed" && state !== "abandoned"
  );
}
