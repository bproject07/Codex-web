const TOKEN_STORAGE_KEY = "codex-web-token";
const SELECTED_TERMINAL_STORAGE_KEY = "codex-web-selected-terminal";
const CLEAN_TERMINAL_ID = /^[A-Za-z0-9_-]{1,128}$/;
const LEGACY_PRIMARY_TERMINAL_ID = "primary";

export type SessionLifecycle =
  | "idle"
  | "starting"
  | "running"
  | "terminating"
  | "terminated"
  | "exited"
  | "failed";

export interface SessionSnapshot {
  terminalId: string;
  name: string;
  isPrimary: boolean;
  createdAt: number;
  sessionId: string | null;
  status: SessionLifecycle;
  connected: boolean;
  connectedClients: number;
  startedAt: number | null;
  pid: number | null;
  exitCode: number | null;
  project: string;
  lastError: string | null;
}

export class ApiError extends Error {
  readonly status: number;

  constructor(status: number, message: string) {
    super(message);
    this.name = "ApiError";
    this.status = status;
  }
}

export function consumeTokenFromUrl(): string {
  const url = new URL(window.location.href);
  const urlToken = url.searchParams.get("token");

  if (urlToken && urlToken.length <= 512) {
    writeSessionToken(urlToken);
    url.searchParams.delete("token");
    window.history.replaceState(null, "", `${url.pathname}${url.search}${url.hash}`);
    return urlToken;
  }

  return readSessionToken();
}

export function writeSessionToken(token: string): void {
  try {
    window.sessionStorage.setItem(TOKEN_STORAGE_KEY, token);
  } catch {
    // Safari private mode and hardened browsers may reject storage. The token
    // remains in React state for the lifetime of the page.
  }
}

export function readSessionToken(): string {
  try {
    return window.sessionStorage.getItem(TOKEN_STORAGE_KEY) ?? "";
  } catch {
    return "";
  }
}

export function clearSessionToken(): void {
  try {
    window.sessionStorage.removeItem(TOKEN_STORAGE_KEY);
  } catch {
    // Nothing else is required when storage is unavailable.
  }
}

export function isCleanTerminalId(terminalId: string): boolean {
  return CLEAN_TERMINAL_ID.test(terminalId);
}

export function readSelectedTerminalId(): string {
  try {
    const terminalId =
      window.sessionStorage.getItem(SELECTED_TERMINAL_STORAGE_KEY) ?? "";
    if (terminalId && isCleanTerminalId(terminalId)) {
      return terminalId;
    }
    window.sessionStorage.removeItem(SELECTED_TERMINAL_STORAGE_KEY);
  } catch {
    // Selection persistence is optional.
  }
  return "";
}

export function writeSelectedTerminalId(terminalId: string): void {
  if (!isCleanTerminalId(terminalId)) {
    return;
  }
  try {
    window.sessionStorage.setItem(SELECTED_TERMINAL_STORAGE_KEY, terminalId);
  } catch {
    // The active selection remains in React state when storage is unavailable.
  }
}

export function clearSelectedTerminalId(): void {
  try {
    window.sessionStorage.removeItem(SELECTED_TERMINAL_STORAGE_KEY);
  } catch {
    // Nothing else is required when storage is unavailable.
  }
}

export function websocketUrl(token: string, terminalId: string): string {
  const url = new URL("/ws", window.location.href);
  url.protocol = window.location.protocol === "https:" ? "wss:" : "ws:";
  url.searchParams.set("token", token);
  url.searchParams.set("terminalId", terminalId);
  return url.toString();
}

export async function listSessions(
  token: string,
  signal?: AbortSignal,
): Promise<SessionSnapshot[]> {
  try {
    return await apiRequest<SessionSnapshot[]>("/api/sessions", token, {
      signal,
    });
  } catch (error) {
    if (!endpointIsUnavailable(error)) {
      throw error;
    }
  }

  const legacy = await apiRequest<LegacySessionSnapshot>("/api/session", token, {
    signal,
  });
  return [normalizeSessionSnapshot(legacy, LEGACY_PRIMARY_TERMINAL_ID)];
}

export async function getSession(
  token: string,
  terminalId: string,
  signal?: AbortSignal,
): Promise<SessionSnapshot> {
  const sessions = await listSessions(token, signal);
  const session = sessions.find((candidate) => candidate.terminalId === terminalId);
  if (!session) {
    throw new ApiError(404, "The selected terminal session no longer exists.");
  }
  return session;
}

export async function createSession(token: string): Promise<SessionSnapshot> {
  try {
    return await apiRequest<SessionSnapshot>("/api/sessions", token, {
      method: "POST",
    });
  } catch (error) {
    if (endpointIsUnavailable(error)) {
      throw new ApiError(
        404,
        "Multiple sessions require the updated Codex Web Terminal server.",
      );
    }
    throw error;
  }
}

export async function restartSession(
  token: string,
  terminalId: string,
): Promise<void> {
  try {
    await apiRequest<void>(
      `/api/sessions/${encodeURIComponent(terminalId)}/restart`,
      token,
      { method: "POST" },
    );
  } catch (error) {
    if (
      terminalId !== LEGACY_PRIMARY_TERMINAL_ID ||
      !endpointIsUnavailable(error)
    ) {
      throw error;
    }
    await apiRequest<void>("/api/session/restart", token, { method: "POST" });
  }
}

export async function terminateSession(
  token: string,
  terminalId: string,
): Promise<void> {
  try {
    await apiRequest<void>(
      `/api/sessions/${encodeURIComponent(terminalId)}/terminate`,
      token,
      { method: "POST" },
    );
  } catch (error) {
    if (
      terminalId !== LEGACY_PRIMARY_TERMINAL_ID ||
      !endpointIsUnavailable(error)
    ) {
      throw error;
    }
    await apiRequest<void>("/api/session/terminate", token, { method: "POST" });
  }
}

export async function deleteSession(
  token: string,
  terminalId: string,
): Promise<void> {
  await apiRequest<void>(
    `/api/sessions/${encodeURIComponent(terminalId)}`,
    token,
    { method: "DELETE" },
  );
}

type LegacySessionSnapshot = Omit<
  SessionSnapshot,
  "terminalId" | "name" | "isPrimary" | "createdAt"
>;

export function normalizeSessionSnapshot(
  session: Partial<SessionSnapshot> & LegacySessionSnapshot,
  fallbackTerminalId: string,
): SessionSnapshot {
  const terminalId =
    typeof session.terminalId === "string" && session.terminalId
      ? session.terminalId
      : fallbackTerminalId;
  return {
    ...session,
    terminalId,
    name:
      typeof session.name === "string" && session.name.trim()
        ? session.name
        : "Primary",
    isPrimary:
      typeof session.isPrimary === "boolean"
        ? session.isPrimary
        : terminalId === LEGACY_PRIMARY_TERMINAL_ID,
    createdAt:
      typeof session.createdAt === "number"
        ? session.createdAt
        : (session.startedAt ?? Date.now()),
  };
}

function endpointIsUnavailable(error: unknown): boolean {
  return (
    (error instanceof ApiError && (error.status === 404 || error.status === 405)) ||
    error instanceof SyntaxError
  );
}

async function apiRequest<T>(
  path: string,
  token: string,
  init: RequestInit = {},
): Promise<T> {
  const headers = new Headers(init.headers);
  headers.set("Authorization", `Bearer ${token}`);
  headers.set("Accept", "application/json");

  const response = await fetch(path, {
    ...init,
    headers,
    credentials: "same-origin",
    cache: "no-store",
  });

  if (!response.ok) {
    let message = `Request failed with HTTP ${response.status}`;
    try {
      const body = (await response.json()) as { error?: string };
      if (body.error) {
        message = body.error;
      }
    } catch {
      // Keep the status-based message for empty and non-JSON error responses.
    }
    throw new ApiError(response.status, message);
  }

  if (response.status === 204) {
    return undefined as T;
  }

  return (await response.json()) as T;
}
