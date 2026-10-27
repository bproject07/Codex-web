import { apiRequest, isAgentKind, type AgentKind } from "../api";
import type {
  ChooseTargetInput,
  CleanupTransferInput,
  CreateTransferInput,
  DeliverTransferInput,
  TransferAttempt,
  TransferAttemptOutcome,
  TransferDelivery,
  TransferDraft,
  TransferMode,
  TransferPreparation,
  TransferSnapshot,
  TransferState,
  TransferTargetMode,
} from "./types";

const CLEAN_ID = /^[A-Za-z0-9_-]{1,128}$/;
const ERROR_CODE = /^[a-z_]{1,64}$/;
const STATES = new Set<TransferState>([
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
]);
const MODES = new Set<TransferMode>(["prepare", "manual"]);
const PREPARATIONS = new Set<TransferPreparation>([
  "pending",
  "submitted",
  "overridden",
  "source_ended",
  "queue_rejected",
]);
const DELIVERIES = new Set<TransferDelivery>(["unconfirmed", "target_ended"]);
const TARGET_MODES = new Set<TransferTargetMode>(["new", "existing"]);
const OUTCOMES = new Set<TransferAttemptOutcome>([
  "starting",
  "bound",
  "start_failed",
  "cleanup_pending",
  "target_ended",
  "delivered",
  "abandoned",
]);

export async function listTransfers(
  token: string,
  filter: { source?: string; target?: string } = {},
  signal?: AbortSignal,
): Promise<TransferSnapshot[]> {
  const query = new URLSearchParams();
  if (filter.source) query.set("source", requireCleanId(filter.source));
  if (filter.target) query.set("target", requireCleanId(filter.target));
  const suffix = query.size > 0 ? `?${query.toString()}` : "";
  const value = await apiRequest<unknown>(`/api/transfers${suffix}`, token, {
    signal,
  });
  if (!Array.isArray(value)) {
    throw new Error("The server returned an invalid transfer list.");
  }
  return value.map(normalizeTransferSnapshot);
}

export async function getTransfer(
  token: string,
  transferId: string,
  signal?: AbortSignal,
): Promise<TransferSnapshot> {
  const value = await apiRequest<unknown>(
    `/api/transfers/${encodeURIComponent(requireCleanId(transferId))}`,
    token,
    { signal },
  );
  return normalizeTransferSnapshot(value);
}

export async function createTransfer(
  token: string,
  input: CreateTransferInput,
  signal?: AbortSignal,
): Promise<TransferSnapshot> {
  const value = await apiRequest<unknown>("/api/transfers", token, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(input),
    signal,
  });
  return normalizeTransferSnapshot(value);
}

export async function getTransferDraft(
  token: string,
  transferId: string,
  signal?: AbortSignal,
): Promise<TransferDraft> {
  const value = await apiRequest<unknown>(
    `/api/transfers/${encodeURIComponent(requireCleanId(transferId))}/draft`,
    token,
    { signal },
  );
  const record = requireRecord(value, "transfer draft");
  return {
    draftRevision: requireNonNegativeInteger(
      record.draftRevision,
      "draft revision",
    ),
    content: requireNullableString(record.content, "draft content"),
  };
}

export async function putTransferDraft(
  token: string,
  transferId: string,
  expectedRevision: number,
  content: string,
  signal?: AbortSignal,
): Promise<TransferSnapshot> {
  const value = await apiRequest<unknown>(
    `/api/transfers/${encodeURIComponent(requireCleanId(transferId))}/draft`,
    token,
    {
      method: "PUT",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ expectedRevision, content }),
      signal,
    },
  );
  return normalizeTransferSnapshot(value);
}

export async function chooseTransferTarget(
  token: string,
  transferId: string,
  input: ChooseTargetInput,
  signal?: AbortSignal,
): Promise<TransferSnapshot> {
  const value = await apiRequest<unknown>(
    `/api/transfers/${encodeURIComponent(requireCleanId(transferId))}/target`,
    token,
    {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(input),
      signal,
    },
  );
  return normalizeTransferSnapshot(value);
}

export async function deliverTransfer(
  token: string,
  transferId: string,
  input: DeliverTransferInput,
  signal?: AbortSignal,
): Promise<TransferSnapshot> {
  const value = await apiRequest<unknown>(
    `/api/transfers/${encodeURIComponent(requireCleanId(transferId))}/deliver`,
    token,
    {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(input),
      signal,
    },
  );
  return normalizeTransferSnapshot(value);
}

export async function cleanupTransfer(
  token: string,
  transferId: string,
  input: CleanupTransferInput,
  signal?: AbortSignal,
): Promise<TransferSnapshot> {
  const value = await apiRequest<unknown>(
    `/api/transfers/${encodeURIComponent(requireCleanId(transferId))}/cleanup`,
    token,
    {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(input),
      signal,
    },
  );
  return normalizeTransferSnapshot(value);
}

export async function abandonTransfer(
  token: string,
  transferId: string,
  signal?: AbortSignal,
): Promise<TransferSnapshot> {
  const value = await apiRequest<unknown>(
    `/api/transfers/${encodeURIComponent(requireCleanId(transferId))}/abandon`,
    token,
    {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ reason: "user" }),
      signal,
    },
  );
  return normalizeTransferSnapshot(value);
}

export function normalizeTransferSnapshot(value: unknown): TransferSnapshot {
  const record = requireRecord(value, "transfer");
  return {
    transferId: requireId(record.transferId, "transfer id"),
    revision: requireNonNegativeInteger(record.revision, "transfer revision"),
    state: requireEnum(record.state, STATES, "transfer state"),
    mode: requireEnum(record.mode, MODES, "transfer mode"),
    sourceTerminalId: requireId(record.sourceTerminalId, "source terminal id"),
    sourceSessionId:
      record.sourceSessionId === null || record.sourceSessionId === undefined
        ? null
        : requireId(record.sourceSessionId, "source session id"),
    draftRevision: requireNonNegativeInteger(
      record.draftRevision,
      "draft revision",
    ),
    approvedRevision:
      record.approvedRevision === undefined || record.approvedRevision === null
        ? null
        : requireNonNegativeInteger(record.approvedRevision, "approved revision"),
    preparation:
      record.preparation === undefined || record.preparation === null
        ? null
        : requireEnum(record.preparation, PREPARATIONS, "transfer preparation"),
    attempt:
      record.attempt === undefined || record.attempt === null
        ? null
        : normalizeAttempt(record.attempt),
    attemptsUsed: requireNonNegativeInteger(record.attemptsUsed, "attempt count"),
    delivery:
      record.delivery === undefined || record.delivery === null
        ? null
        : requireEnum(record.delivery, DELIVERIES, "transfer delivery"),
    errorCode: normalizeErrorCode(record.error),
    createdAt: requireFiniteNumber(record.createdAt, "createdAt"),
    updatedAt: requireFiniteNumber(record.updatedAt, "updatedAt"),
  };
}

function normalizeAttempt(value: unknown): TransferAttempt {
  const record = requireRecord(value, "transfer attempt");
  const target = requireRecord(record.target, "transfer target");
  return {
    targetRequestId: requireId(record.targetRequestId, "target request id"),
    number: requireNonNegativeInteger(record.number, "attempt number"),
    outcome: requireEnum(record.outcome, OUTCOMES, "attempt outcome"),
    target: {
      mode: requireEnum(target.mode, TARGET_MODES, "target mode"),
      terminalId: requireId(target.terminalId, "target terminal id"),
      sessionId:
        target.sessionId === null || target.sessionId === undefined
          ? null
          : requireId(target.sessionId, "target session id"),
      agent: requireAgentKind(target.agent),
      name: requireString(target.name, "target name"),
      project: requireString(target.project, "target project"),
    },
  };
}

function normalizeErrorCode(value: unknown): string | null {
  if (value === undefined || value === null) return null;
  const record = requireRecord(value, "transfer error");
  const code = requireString(record.code, "transfer error code");
  return ERROR_CODE.test(code) ? code : "invalid_state";
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
    throw new Error("The transfer id is invalid.");
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
  if (value === null || value === undefined) return null;
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

function requireEnum<T extends string>(
  value: unknown,
  allowed: ReadonlySet<T>,
  description: string,
): T {
  if (typeof value !== "string" || !allowed.has(value as T)) {
    throw new Error(`The server returned an invalid ${description}.`);
  }
  return value as T;
}

function requireAgentKind(value: unknown): AgentKind {
  if (!isAgentKind(value)) {
    throw new Error("The server returned an invalid transfer agent.");
  }
  return value;
}
