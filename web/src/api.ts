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

export type AgentKind = "codex" | "claude" | "agy";

export type AgentDiscoveryState = "ready" | "missing" | "misconfigured";
export type AgentConfiguration = "auto" | "override";

export interface AgentInstallGuide {
  command: string;
  shell: string;
  verifyCommand: string;
  updateCommand: string;
  docsUrl: string;
  requiresServerAccess: true;
}

export interface AgentCatalogEntry {
  kind: AgentKind;
  state: AgentDiscoveryState;
  configuration: AgentConfiguration;
  version: string | null;
  dangerouslySkipPermissions: boolean;
  install: AgentInstallGuide;
}

export interface AgentCatalog {
  schemaVersion: 1;
  server: {
    os: string;
    arch: string;
    shell: string;
  };
  agents: AgentCatalogEntry[];
}

export interface AgentCatalogOptions {
  refresh?: boolean;
  signal?: AbortSignal;
}

export interface SessionSnapshot {
  terminalId: string;
  name: string;
  agent: AgentKind;
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

class ApiPayloadError extends Error {
  readonly contentType: string;

  constructor(contentType: string) {
    super("The server returned an invalid API response.");
    this.name = "ApiPayloadError";
    this.contentType = contentType.toLowerCase();
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
    const sessions = await apiRequest<SessionSnapshot[]>("/api/sessions", token, {
      signal,
    });
    return sessions.map((session) =>
      normalizeSessionSnapshot(session, session.terminalId),
    );
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

export async function listAgents(
  token: string,
  signal?: AbortSignal,
): Promise<AgentKind[]> {
  try {
    const agents = await apiRequest<unknown>("/api/agents", token, { signal });
    if (!Array.isArray(agents)) {
      throw new ApiError(502, "The server returned an invalid agent list.");
    }
    return agents.filter(isAgentKind);
  } catch (error) {
    if (endpointIsUnavailable(error)) {
      return ["codex"];
    }
    throw error;
  }
}

export async function getAgentCatalog(
  token: string,
  options: AgentCatalogOptions = {},
): Promise<AgentCatalog> {
  const query = options.refresh ? "?refresh=true" : "";
  try {
    const catalog = await apiRequest<unknown>(
      `/api/agent-catalog${query}`,
      token,
      { signal: options.signal },
    );
    return normalizeAgentCatalog(catalog);
  } catch (error) {
    if (!endpointIsUnavailable(error)) {
      throw error;
    }
  }

  const legacyAgents = await listAgents(token, options.signal);
  return {
    schemaVersion: 1,
    server: {
      os: "unknown",
      arch: "unknown",
      shell: "server shell",
    },
    agents: legacyAgents.map((kind) => ({
      kind,
      state: "ready",
      configuration: "auto",
      version: null,
      dangerouslySkipPermissions: false,
      install: {
        command: "",
        shell: "server shell",
        verifyCommand: `${kind === "agy" ? "agy" : kind} --version`,
        updateCommand: "",
        docsUrl: "",
        requiresServerAccess: true,
      },
    })),
  };
}

export async function createSession(
  token: string,
  agent: AgentKind,
): Promise<SessionSnapshot> {
  try {
    const session = await apiRequest<SessionSnapshot>("/api/sessions", token, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ agent }),
    });
    return normalizeSessionSnapshot(session, session.terminalId);
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
  "terminalId" | "name" | "agent" | "isPrimary" | "createdAt"
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
    agent:
      session.agent === "claude" || session.agent === "agy"
        ? session.agent
        : "codex",
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

export function normalizeAgentCatalog(catalog: unknown): AgentCatalog {
  if (!isRecord(catalog) || catalog.schemaVersion !== 1) {
    throw new ApiError(502, "The server returned an invalid agent catalog.");
  }

  const server = catalog.server;
  if (
    !isRecord(server) ||
    typeof server.os !== "string" ||
    typeof server.arch !== "string" ||
    typeof server.shell !== "string" ||
    !Array.isArray(catalog.agents)
  ) {
    throw new ApiError(502, "The server returned an invalid agent catalog.");
  }

  const agents = catalog.agents.map(normalizeAgentCatalogEntry);
  return {
    schemaVersion: 1,
    server: {
      os: server.os,
      arch: server.arch,
      shell: server.shell,
    },
    agents,
  };
}

function normalizeAgentCatalogEntry(entry: unknown): AgentCatalogEntry {
  if (
    !isRecord(entry) ||
    !isAgentKind(entry.kind) ||
    !isAgentDiscoveryState(entry.state) ||
    (entry.configuration !== "auto" && entry.configuration !== "override") ||
    (entry.version !== null && typeof entry.version !== "string") ||
    typeof entry.dangerouslySkipPermissions !== "boolean" ||
    !isRecord(entry.install)
  ) {
    throw new ApiError(502, "The server returned an invalid agent catalog.");
  }

  const install = entry.install;
  if (
    typeof install.command !== "string" ||
    typeof install.shell !== "string" ||
    typeof install.verifyCommand !== "string" ||
    typeof install.updateCommand !== "string" ||
    typeof install.docsUrl !== "string" ||
    install.requiresServerAccess !== true
  ) {
    throw new ApiError(502, "The server returned an invalid agent catalog.");
  }

  return {
    kind: entry.kind,
    state: entry.state,
    configuration: entry.configuration,
    version: entry.version,
    dangerouslySkipPermissions: entry.dangerouslySkipPermissions,
    install: {
      command: install.command,
      shell: install.shell,
      verifyCommand: install.verifyCommand,
      updateCommand: install.updateCommand,
      docsUrl: normalizeHttpsUrl(install.docsUrl),
      requiresServerAccess: true,
    },
  };
}

function isAgentKind(value: unknown): value is AgentKind {
  return value === "codex" || value === "claude" || value === "agy";
}

function isAgentDiscoveryState(value: unknown): value is AgentDiscoveryState {
  return value === "ready" || value === "missing" || value === "misconfigured";
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null;
}

function normalizeHttpsUrl(value: string): string {
  if (!value) {
    return "";
  }
  try {
    const url = new URL(value);
    return url.protocol === "https:" ? url.toString() : "";
  } catch {
    return "";
  }
}

function endpointIsUnavailable(error: unknown): boolean {
  return (
    (error instanceof ApiError && (error.status === 404 || error.status === 405)) ||
    (error instanceof ApiPayloadError &&
      error.contentType.includes("text/html"))
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

  try {
    return (await response.json()) as T;
  } catch {
    throw new ApiPayloadError(response.headers.get("Content-Type") ?? "");
  }
}
