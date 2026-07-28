import type {
  WorkspaceDirectory,
  WorkspaceDirectoryListing,
  WorkspaceFavorite,
  WorkspaceLibrary,
  WorkspaceRecent,
} from "./workspaces";

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

export type SessionPurpose =
  | { kind: "interactive" }
  | {
      kind: "peer";
      threadId: string;
      parentTerminalId: string;
    };

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

export interface HealthSnapshot {
  status: "ok";
  serverVersion: string | null;
  codexInstalled: boolean;
  sessionRunning: boolean;
  connectedClients: number;
  sessionCount: number;
  runningSessions: number;
  maxSessions: number;
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
  directoryId: string;
  lastError: string | null;
  purpose: SessionPurpose;
}

export interface FilesystemRoots {
  defaultDirectory: WorkspaceDirectory;
  roots: WorkspaceDirectory[];
}

export class ApiError extends Error {
  readonly status: number;
  readonly contentType: string;

  constructor(status: number, message: string, contentType = "") {
    super(message);
    this.name = "ApiError";
    this.status = status;
    this.contentType = contentType.toLowerCase();
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
    removeTokenFromAddress(url, false);
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
  removeTokenFromAddress(new URL(window.location.href), true);
}

function removeTokenFromAddress(url: URL, navigateOnFailure: boolean): void {
  if (!url.searchParams.has("token")) {
    return;
  }
  url.searchParams.delete("token");
  const cleanAddress = `${url.pathname}${url.search}${url.hash}`;
  try {
    window.history.replaceState(null, "", cleanAddress);
  } catch {
    if (navigateOnFailure) {
      window.location.replace(cleanAddress);
    }
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

export async function getHealth(
  token: string,
  signal?: AbortSignal,
): Promise<HealthSnapshot> {
  const health = await apiRequest<unknown>("/api/health", token, { signal });
  if (
    !isRecord(health) ||
    health.status !== "ok" ||
    typeof health.codexInstalled !== "boolean" ||
    typeof health.sessionRunning !== "boolean" ||
    !isNonNegativeSafeInteger(health.connectedClients) ||
    !isNonNegativeSafeInteger(health.sessionCount) ||
    !isNonNegativeSafeInteger(health.runningSessions) ||
    typeof health.maxSessions !== "number" ||
    !Number.isSafeInteger(health.maxSessions) ||
    health.maxSessions <= 0
  ) {
    throw new ApiError(502, "The server returned an invalid health response.");
  }

  return {
    status: "ok",
    serverVersion:
      typeof health.serverVersion === "string" &&
      health.serverVersion.length > 0 &&
      health.serverVersion.length <= 128 &&
      /^[0-9A-Za-z][0-9A-Za-z.+_-]*$/.test(health.serverVersion)
        ? health.serverVersion
        : null,
    codexInstalled: health.codexInstalled,
    sessionRunning: health.sessionRunning,
    connectedClients: health.connectedClients,
    sessionCount: health.sessionCount,
    runningSessions: health.runningSessions,
    maxSessions: health.maxSessions,
  };
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

export async function getFilesystemRoots(
  token: string,
  signal?: AbortSignal,
): Promise<FilesystemRoots> {
  const roots = await apiRequest<unknown>("/api/filesystem/roots", token, {
    signal,
  });
  return normalizeFilesystemRoots(roots);
}

export async function listWorkspaceDirectory(
  token: string,
  directoryId?: string | null,
  signal?: AbortSignal,
): Promise<WorkspaceDirectoryListing> {
  const listing = await apiRequest<unknown>("/api/filesystem/list", token, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(directoryId ? { directoryId } : {}),
    signal,
  });
  return normalizeDirectoryListing(listing);
}

export async function resolveWorkspacePath(
  token: string,
  path: string,
  signal?: AbortSignal,
): Promise<WorkspaceDirectoryListing> {
  const listing = await apiRequest<unknown>("/api/filesystem/resolve", token, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ path }),
    signal,
  });
  return normalizeDirectoryListing(listing);
}

export async function getWorkspaceLibrary(
  token: string,
  signal?: AbortSignal,
): Promise<WorkspaceLibrary> {
  const library = await apiRequest<unknown>("/api/workspaces", token, {
    signal,
  });
  return normalizeWorkspaceLibrary(library);
}

export async function addWorkspaceFavorite(
  token: string,
  directory: WorkspaceDirectory,
  signal?: AbortSignal,
): Promise<WorkspaceFavorite> {
  const favorite = await apiRequest<unknown>(
    "/api/workspaces/favorites",
    token,
    {
      method: "PUT",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ directoryId: directory.id }),
      signal,
    },
  );
  return normalizeWorkspaceFavorite(favorite);
}

export async function removeWorkspaceFavorite(
  token: string,
  favoriteId: string,
  signal?: AbortSignal,
): Promise<void> {
  await apiRequest<void>(
    `/api/workspaces/favorites/${encodeURIComponent(favoriteId)}`,
    token,
    { method: "DELETE", signal },
  );
}

export async function createSession(
  token: string,
  agent: AgentKind,
  directoryId?: string | null,
): Promise<SessionSnapshot> {
  try {
    const session = await apiRequest<SessionSnapshot>("/api/sessions", token, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({
        agent,
        ...(directoryId ? { directoryId } : {}),
      }),
    });
    return normalizeSessionSnapshot(session, session.terminalId);
  } catch (error) {
    if (endpointIsUnavailable(error)) {
      throw new ApiError(
        404,
        "Creating another session is unavailable. The browser UI and server may be from different releases, or an older server may still be using this port. Restart with the executable and web folder from the same release, then reload.",
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
  | "terminalId"
  | "name"
  | "agent"
  | "isPrimary"
  | "createdAt"
  | "directoryId"
  | "purpose"
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
    directoryId:
      typeof session.directoryId === "string" ? session.directoryId : "",
    purpose: normalizeSessionPurpose(
      session.purpose,
      Object.prototype.hasOwnProperty.call(session, "purpose"),
    ),
  };
}

function normalizeSessionPurpose(
  value: unknown,
  fieldIsPresent: boolean,
): SessionPurpose {
  if (!fieldIsPresent) {
    return { kind: "interactive" };
  }
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    throw invalidSessionPurpose();
  }

  const record = value as Record<string, unknown>;
  if (record.kind === "interactive") {
    return { kind: "interactive" };
  }
  if (record.kind === "peer") {
    const threadId = record.threadId;
    const parentTerminalId = record.parentTerminalId;
    if (
      typeof threadId === "string" &&
      CLEAN_TERMINAL_ID.test(threadId) &&
      typeof parentTerminalId === "string" &&
      CLEAN_TERMINAL_ID.test(parentTerminalId)
    ) {
      return { kind: "peer", threadId, parentTerminalId };
    }
  }
  throw invalidSessionPurpose();
}

function invalidSessionPurpose(): ApiError {
  return new ApiError(
    502,
    "The server returned an invalid terminal session purpose.",
  );
}

function normalizeFilesystemRoots(value: unknown): FilesystemRoots {
  if (
    !isRecord(value) ||
    !Array.isArray(value.roots) ||
    !isRecord(value.defaultDirectory)
  ) {
    throw invalidWorkspaceResponse();
  }

  return {
    defaultDirectory: normalizeWorkspaceDirectory(value.defaultDirectory),
    roots: value.roots.map(normalizeWorkspaceDirectory),
  };
}

function normalizeDirectoryListing(value: unknown): WorkspaceDirectoryListing {
  if (
    !isRecord(value) ||
    !isRecord(value.current) ||
    (value.parentId !== null && typeof value.parentId !== "string") ||
    !Array.isArray(value.breadcrumbs) ||
    !Array.isArray(value.directories) ||
    (value.truncated !== undefined && typeof value.truncated !== "boolean")
  ) {
    throw invalidWorkspaceResponse();
  }

  return {
    current: normalizeWorkspaceDirectory(value.current),
    parentId: value.parentId,
    breadcrumbs: value.breadcrumbs.map(normalizeWorkspaceDirectory),
    directories: value.directories.map(normalizeWorkspaceDirectory),
    truncated: value.truncated === true,
  };
}

function normalizeWorkspaceLibrary(value: unknown): WorkspaceLibrary {
  if (
    !isRecord(value) ||
    value.version !== 1 ||
    !Array.isArray(value.favorites) ||
    !Array.isArray(value.recent)
  ) {
    throw invalidWorkspaceResponse();
  }

  return {
    favorites: value.favorites.map(normalizeWorkspaceFavorite),
    recent: value.recent.map(normalizeWorkspaceRecent),
  };
}

function normalizeWorkspaceFavorite(value: unknown): WorkspaceFavorite {
  if (
    !isRecord(value) ||
    typeof value.id !== "string" ||
    typeof value.directoryId !== "string" ||
    typeof value.name !== "string" ||
    typeof value.path !== "string" ||
    (value.label !== null &&
      value.label !== undefined &&
      typeof value.label !== "string") ||
    (value.preferredAgent !== null &&
      value.preferredAgent !== undefined &&
      !isAgentKind(value.preferredAgent))
  ) {
    throw invalidWorkspaceResponse();
  }

  return {
    id: requireNonEmpty(value.id),
    directory: {
      id: requireNonEmpty(value.directoryId),
      name: requireNonEmpty(value.name),
      path: requireNonEmpty(value.path),
    },
    label: value.label,
    preferredAgent: value.preferredAgent,
  };
}

function normalizeWorkspaceRecent(value: unknown): WorkspaceRecent {
  if (
    !isRecord(value) ||
    typeof value.directoryId !== "string" ||
    typeof value.name !== "string" ||
    typeof value.path !== "string" ||
    !isAgentKind(value.lastAgent) ||
    typeof value.lastOpenedAt !== "number" ||
    !Number.isSafeInteger(value.lastOpenedAt) ||
    value.lastOpenedAt < 0
  ) {
    throw invalidWorkspaceResponse();
  }

  return {
    directory: {
      id: requireNonEmpty(value.directoryId),
      name: requireNonEmpty(value.name),
      path: requireNonEmpty(value.path),
    },
    lastAgent: value.lastAgent,
    lastOpenedAt: value.lastOpenedAt,
  };
}

function normalizeWorkspaceDirectory(value: unknown): WorkspaceDirectory {
  if (
    !isRecord(value) ||
    typeof value.id !== "string" ||
    typeof value.name !== "string" ||
    typeof value.path !== "string"
  ) {
    throw invalidWorkspaceResponse();
  }

  return {
    id: requireNonEmpty(value.id),
    name: requireNonEmpty(value.name),
    path: requireNonEmpty(value.path),
  };
}

function requireNonEmpty(value: string): string {
  if (!value) {
    throw invalidWorkspaceResponse();
  }
  return value;
}

function invalidWorkspaceResponse(): ApiError {
  return new ApiError(502, "The server returned an invalid workspace response.");
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

function isNonNegativeSafeInteger(value: unknown): value is number {
  return (
    typeof value === "number" &&
    Number.isSafeInteger(value) &&
    value >= 0
  );
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
  if (error instanceof ApiPayloadError) {
    return error.contentType.includes("text/html");
  }
  if (!(error instanceof ApiError)) {
    return false;
  }
  if (error.status === 405) {
    return true;
  }
  if (error.status !== 404) {
    return false;
  }

  const genericRouteError = /^(?:route\s+)?not found$/i.test(
    error.message.trim(),
  );
  return !error.contentType.includes("application/json") || genericRouteError;
}

export async function apiRequest<T>(
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
    const contentType = response.headers.get("Content-Type") ?? "";
    try {
      const body = (await response.json()) as { error?: string };
      if (body.error) {
        message = body.error;
      }
    } catch {
      // Keep the status-based message for empty and non-JSON error responses.
    }
    throw new ApiError(response.status, message, contentType);
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
