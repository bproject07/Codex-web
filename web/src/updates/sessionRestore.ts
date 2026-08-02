import {
  createSession,
  type AgentKind,
  type SessionSnapshot,
} from "../api";

const STORAGE_KEY = "codex-web-update-session-restore";
const SCHEMA_VERSION = 1;
const MAX_STORED_PLAN_LENGTH = 8 * 1024 * 1024;
const MAX_RESTORED_SESSIONS = 255;
const MAX_DIRECTORY_ID_LENGTH = 256 * 1024;
const CLEAN_ID = /^[A-Za-z0-9_-]{1,128}$/;
const AGENTS: ReadonlySet<AgentKind> = new Set(["codex", "claude", "agy"]);

export interface SessionRestoreStorage {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
  removeItem(key: string): void;
}

interface RestorableSession {
  sourceTerminalId: string;
  agent: AgentKind;
  directoryId: string;
}

interface RestoredSessionMapping {
  sourceTerminalId: string;
  terminalId: string;
}

interface SessionRestorePlan {
  schemaVersion: 1;
  sourceVersion: string;
  targetVersion: string;
  primaryTerminalId: string;
  selectedTerminalId: string;
  sessions: RestorableSession[];
  restored: RestoredSessionMapping[];
}

export interface StageSessionRestoreOptions {
  sourceVersion: string;
  targetVersion: string;
  sessions: SessionSnapshot[];
  selectedTerminalId: string;
  storage?: SessionRestoreStorage | null;
}

export interface StageSessionRestoreResult {
  sessionCount: number;
  saved: boolean;
}

export interface RestoreSessionTabsOptions {
  token: string;
  serverVersion: string | null;
  sessions: SessionSnapshot[];
  storage?: SessionRestoreStorage | null;
  create?: (
    token: string,
    agent: AgentKind,
    directoryId?: string | null,
  ) => Promise<SessionSnapshot>;
}

export interface RestoreSessionTabsResult {
  sessions: SessionSnapshot[];
  preferredTerminalId?: string;
  error?: string;
}

export function stageSessionRestorePlan({
  sourceVersion,
  targetVersion,
  sessions,
  selectedTerminalId,
  storage,
}: StageSessionRestoreOptions): StageSessionRestoreResult {
  const resolvedStorage = resolveStorage(storage);
  const primary = sessions.find((session) => session.isPrimary);
  const restorable = sessions
    .filter(isRestorableSession)
    .slice(0, MAX_RESTORED_SESSIONS)
    .map((session) => ({
      sourceTerminalId: session.terminalId,
      agent: session.agent,
      directoryId: session.directoryId,
    }));

  if (restorable.length === 0) {
    discardSessionRestorePlan(resolvedStorage);
    return { sessionCount: 0, saved: true };
  }
  if (
    !resolvedStorage ||
    !primary ||
    !validVersion(sourceVersion) ||
    !validVersion(targetVersion)
  ) {
    return { sessionCount: restorable.length, saved: false };
  }

  const plan: SessionRestorePlan = {
    schemaVersion: SCHEMA_VERSION,
    sourceVersion,
    targetVersion,
    primaryTerminalId: primary.terminalId,
    selectedTerminalId,
    sessions: restorable,
    restored: [],
  };
  const saved = writePlan(resolvedStorage, plan);
  if (!saved) {
    discardSessionRestorePlan(resolvedStorage);
  }
  return {
    sessionCount: restorable.length,
    saved,
  };
}

export function discardSessionRestorePlan(
  storage?: SessionRestoreStorage | null,
): void {
  const resolvedStorage = resolveStorage(storage);
  try {
    resolvedStorage?.removeItem(STORAGE_KEY);
  } catch {
    // Storage is optional; there is no durable plan to remove in this case.
  }
}

export function discardSessionRestorePlanForOriginalGeneration(
  sessions: SessionSnapshot[],
  storage?: SessionRestoreStorage | null,
): void {
  const resolvedStorage = resolveStorage(storage);
  const plan = readPlan(resolvedStorage);
  const primary = sessions.find((session) => session.isPrimary);
  if (plan && primary?.terminalId === plan.primaryTerminalId) {
    discardSessionRestorePlan(resolvedStorage);
  }
}

export async function restoreSessionTabs({
  token,
  serverVersion,
  sessions,
  storage,
  create = createSession,
}: RestoreSessionTabsOptions): Promise<RestoreSessionTabsResult> {
  const resolvedStorage = resolveStorage(storage);
  const plan = readPlan(resolvedStorage);
  if (
    !plan ||
    !serverVersion ||
    (serverVersion !== plan.sourceVersion &&
      serverVersion !== plan.targetVersion)
  ) {
    return { sessions };
  }

  const primary = sessions.find((session) => session.isPrimary);
  if (!primary || primary.terminalId === plan.primaryTerminalId) {
    // The original server generation is still serving requests. Keep the
    // plan for the replacement generation instead of duplicating live tabs.
    return { sessions };
  }

  const nextSessions = [...sessions];
  const restored = new Map(
    plan.restored.map((mapping) => [
      mapping.sourceTerminalId,
      mapping.terminalId,
    ]),
  );
  let preferredTerminalId =
    plan.selectedTerminalId === plan.primaryTerminalId
      ? primary.terminalId
      : undefined;

  for (const entry of plan.sessions) {
    const mappedTerminalId = restored.get(entry.sourceTerminalId);
    let candidate = mappedTerminalId
      ? nextSessions.find(
          (session) =>
            session.terminalId === mappedTerminalId &&
            sessionMatchesEntry(session, entry),
        )
      : undefined;

    if (!candidate) {
      try {
        candidate = await create(token, entry.agent, entry.directoryId);
        nextSessions.push(candidate);
      } catch {
        return {
          sessions: nextSessions,
          preferredTerminalId,
          error:
            "The server restarted, but one or more terminal tabs could not be recreated. The remaining restore plan was kept for the next reload.",
        };
      }

      restored.set(entry.sourceTerminalId, candidate.terminalId);
      plan.restored = [...restored].map(
        ([sourceTerminalId, terminalId]) => ({
          sourceTerminalId,
          terminalId,
        }),
      );
      if (!writePlan(resolvedStorage, plan)) {
        return {
          sessions: nextSessions,
          preferredTerminalId,
          error:
            "A terminal tab was recreated, but the browser could not safely continue the restore plan.",
        };
      }
    }

    if (plan.selectedTerminalId === entry.sourceTerminalId) {
      preferredTerminalId = candidate.terminalId;
    }
  }

  discardSessionRestorePlan(resolvedStorage);
  return { sessions: nextSessions, preferredTerminalId };
}

function isRestorableSession(session: SessionSnapshot): boolean {
  return !session.isPrimary && session.purpose.kind === "interactive";
}

function sessionMatchesEntry(
  session: SessionSnapshot,
  entry: RestorableSession,
): boolean {
  return (
    isRestorableSession(session) &&
    session.agent === entry.agent &&
    session.directoryId === entry.directoryId
  );
}

function resolveStorage(
  storage: SessionRestoreStorage | null | undefined,
): SessionRestoreStorage | null {
  if (storage !== undefined) {
    return storage;
  }
  try {
    return window.sessionStorage;
  } catch {
    return null;
  }
}

function writePlan(
  storage: SessionRestoreStorage | null,
  plan: SessionRestorePlan,
): boolean {
  if (!storage) {
    return false;
  }
  try {
    const encoded = JSON.stringify(plan);
    if (encoded.length > MAX_STORED_PLAN_LENGTH) {
      return false;
    }
    storage.setItem(STORAGE_KEY, encoded);
    return true;
  } catch {
    return false;
  }
}

function readPlan(
  storage: SessionRestoreStorage | null,
): SessionRestorePlan | null {
  if (!storage) {
    return null;
  }
  try {
    const raw = storage.getItem(STORAGE_KEY);
    if (!raw) {
      return null;
    }
    if (raw.length > MAX_STORED_PLAN_LENGTH) {
      storage.removeItem(STORAGE_KEY);
      return null;
    }
    const parsed: unknown = JSON.parse(raw);
    if (!isSessionRestorePlan(parsed)) {
      storage.removeItem(STORAGE_KEY);
      return null;
    }
    return parsed;
  } catch {
    return null;
  }
}

function isSessionRestorePlan(value: unknown): value is SessionRestorePlan {
  if (
    !isRecord(value) ||
    value.schemaVersion !== SCHEMA_VERSION ||
    !validVersion(value.sourceVersion) ||
    !validVersion(value.targetVersion) ||
    !validId(value.primaryTerminalId) ||
    !validOptionalId(value.selectedTerminalId) ||
    !Array.isArray(value.sessions) ||
    value.sessions.length < 1 ||
    value.sessions.length > MAX_RESTORED_SESSIONS ||
    !Array.isArray(value.restored) ||
    value.restored.length > value.sessions.length
  ) {
    return false;
  }

  const sources = new Set<string>();
  for (const session of value.sessions) {
    if (
      !isRecord(session) ||
      !validId(session.sourceTerminalId) ||
      !AGENTS.has(session.agent as AgentKind) ||
      typeof session.directoryId !== "string" ||
      session.directoryId.length < 1 ||
      session.directoryId.length > MAX_DIRECTORY_ID_LENGTH ||
      sources.has(session.sourceTerminalId)
    ) {
      return false;
    }
    sources.add(session.sourceTerminalId);
  }

  const restoredSources = new Set<string>();
  for (const mapping of value.restored) {
    if (
      !isRecord(mapping) ||
      !validId(mapping.sourceTerminalId) ||
      !validId(mapping.terminalId) ||
      !sources.has(mapping.sourceTerminalId) ||
      restoredSources.has(mapping.sourceTerminalId)
    ) {
      return false;
    }
    restoredSources.add(mapping.sourceTerminalId);
  }
  return true;
}

function validVersion(value: unknown): value is string {
  return (
    typeof value === "string" &&
    value.length > 0 &&
    value.length <= 64 &&
    !/[\u0000-\u001f\u007f]/.test(value)
  );
}

function validId(value: unknown): value is string {
  return typeof value === "string" && CLEAN_ID.test(value);
}

function validOptionalId(value: unknown): value is string {
  return value === "" || validId(value);
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}
