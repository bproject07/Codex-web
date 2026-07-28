import {
  apiRequest,
  type AgentKind,
} from "../api";
import type {
  CreatePeerThreadInput,
  CreatePeerTurnInput,
  DispatchPeerTurnInput,
  PeerAction,
  PeerStatus,
  PeerThread,
  PeerTurn,
  ReturnPeerTurnInput,
} from "./types";

const CLEAN_ID = /^[A-Za-z0-9_-]{1,128}$/;
const PEER_ACTIONS = new Set<PeerAction>([
  "review",
  "verify",
  "ask",
  "handoff",
  "recheck",
]);
const PEER_STATUSES = new Set<PeerStatus>([
  "preparing_handoff",
  "awaiting_preview",
  "reviewing",
  "response_ready",
  "returned",
  "failed",
  "closed",
]);

export async function listPeerThreads(
  token: string,
  signal?: AbortSignal,
): Promise<PeerThread[]> {
  const value = await apiRequest<unknown>("/api/peer/threads", token, {
    signal,
  });
  if (!Array.isArray(value)) {
    throw new Error("The server returned an invalid peer thread list.");
  }
  return value.map(normalizePeerThread);
}

export async function createPeerThread(
  token: string,
  input: CreatePeerThreadInput,
  signal?: AbortSignal,
): Promise<PeerThread> {
  const value = await apiRequest<unknown>("/api/peer/threads", token, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(input),
    signal,
  });
  return normalizePeerThread(value);
}

export async function createPeerTurn(
  token: string,
  threadId: string,
  input: CreatePeerTurnInput,
  signal?: AbortSignal,
): Promise<PeerThread> {
  const value = await apiRequest<unknown>(
    `/api/peer/threads/${encodeURIComponent(requireCleanId(threadId))}/turns`,
    token,
    {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(input),
      signal,
    },
  );
  return normalizePeerThread(value);
}

export async function dispatchPeerTurn(
  token: string,
  threadId: string,
  input: DispatchPeerTurnInput,
  signal?: AbortSignal,
): Promise<PeerThread> {
  const value = await apiRequest<unknown>(
    `/api/peer/threads/${encodeURIComponent(requireCleanId(threadId))}/dispatch`,
    token,
    {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(input),
      signal,
    },
  );
  return normalizePeerThread(value);
}

export async function deletePeerThread(
  token: string,
  threadId: string,
  signal?: AbortSignal,
): Promise<void> {
  await apiRequest<void>(
    `/api/peer/threads/${encodeURIComponent(requireCleanId(threadId))}`,
    token,
    { method: "DELETE", signal },
  );
}

export async function returnPeerTurn(
  token: string,
  threadId: string,
  input: ReturnPeerTurnInput,
  signal?: AbortSignal,
): Promise<PeerThread> {
  const value = await apiRequest<unknown>(
    `/api/peer/threads/${encodeURIComponent(requireCleanId(threadId))}/return`,
    token,
    {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(input),
      signal,
    },
  );
  return normalizePeerThread(value);
}

export function normalizePeerThread(value: unknown): PeerThread {
  const record = requireRecord(value, "peer thread");
  const currentTurn = normalizePeerTurn(record.currentTurn);
  const status = requirePeerStatus(record.status);
  return {
    id: requireId(record.id, "thread id"),
    sourceTerminalId: requireId(
      record.sourceTerminalId,
      "source terminal id",
    ),
    reviewerTerminalId:
      record.reviewerTerminalId === null
        ? null
        : requireId(record.reviewerTerminalId, "reviewer terminal id"),
    targetAgent: requireAgentKind(record.targetAgent),
    status,
    currentTurn,
    createdAt: requireFiniteNumber(record.createdAt, "createdAt"),
    updatedAt: requireFiniteNumber(record.updatedAt, "updatedAt"),
  };
}

function normalizePeerTurn(value: unknown): PeerTurn {
  const record = requireRecord(value, "peer turn");
  return {
    id: requireId(record.id, "turn id"),
    sequence: requireNonNegativeInteger(record.sequence, "turn sequence"),
    action: requirePeerAction(record.action),
    instruction: requireString(record.instruction, "instruction"),
    status: requirePeerStatus(record.status),
    handoff: requireNullableString(record.handoff, "handoff"),
    handoffRevision: requireNonNegativeInteger(
      record.handoffRevision,
      "handoff revision",
    ),
    response: requireNullableString(record.response, "response"),
    error: requireNullableString(record.error, "error"),
  };
}

function requireRecord(
  value: unknown,
  description: string,
): Record<string, unknown> {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    throw new Error(`The server returned an invalid ${description}.`);
  }
  return value as Record<string, unknown>;
}

function requireId(value: unknown, description: string): string {
  if (typeof value !== "string" || !CLEAN_ID.test(value)) {
    throw new Error(`The server returned an invalid ${description}.`);
  }
  return value;
}

function requireCleanId(value: string): string {
  if (!CLEAN_ID.test(value)) {
    throw new Error("The peer thread id is invalid.");
  }
  return value;
}

function requireString(value: unknown, description: string): string {
  if (typeof value !== "string") {
    throw new Error(`The server returned an invalid ${description}.`);
  }
  return value;
}

function requireNullableString(
  value: unknown,
  description: string,
): string | null {
  if (value === null) {
    return null;
  }
  return requireString(value, description);
}

function requireFiniteNumber(value: unknown, description: string): number {
  if (typeof value !== "number" || !Number.isFinite(value)) {
    throw new Error(`The server returned an invalid ${description}.`);
  }
  return value;
}

function requireNonNegativeInteger(
  value: unknown,
  description: string,
): number {
  const number = requireFiniteNumber(value, description);
  if (!Number.isSafeInteger(number) || number < 0) {
    throw new Error(`The server returned an invalid ${description}.`);
  }
  return number;
}

function requirePeerAction(value: unknown): PeerAction {
  if (typeof value !== "string" || !PEER_ACTIONS.has(value as PeerAction)) {
    throw new Error("The server returned an invalid peer action.");
  }
  return value as PeerAction;
}

function requirePeerStatus(value: unknown): PeerStatus {
  if (typeof value !== "string" || !PEER_STATUSES.has(value as PeerStatus)) {
    throw new Error("The server returned an invalid peer status.");
  }
  return value as PeerStatus;
}

function requireAgentKind(value: unknown): AgentKind {
  if (value !== "codex" && value !== "claude" && value !== "agy") {
    throw new Error("The server returned an invalid peer agent.");
  }
  return value;
}
