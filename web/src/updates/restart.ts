import { getHealth, readSessionToken, type HealthSnapshot } from "../api";
import type { UpdateState } from "./api";

const UPDATE_POLL_STATES: ReadonlySet<UpdateState> = new Set([
  "checking",
  "downloading",
  "verifying",
  "staged",
]);

export function isUpdatePollState(
  state: UpdateState | undefined,
): boolean {
  return state !== undefined && UPDATE_POLL_STATES.has(state);
}

export function isUpdateHandoffState(
  state: UpdateState | undefined,
): boolean {
  return state === "staged" || state === "restarting";
}

export interface WaitForServerVersionOptions {
  token: string;
  expectedVersion: string;
  signal?: AbortSignal;
  attempts?: number;
  intervalMs?: number;
  readHealth?: (token: string, signal?: AbortSignal) => Promise<HealthSnapshot>;
  wait?: (milliseconds: number, signal?: AbortSignal) => Promise<void>;
}

export interface ReloadForUpdatedServerOptions {
  token: string;
  currentUrl?: string;
  readStoredToken?: () => string;
  reload?: () => void;
  replace?: (url: string) => void;
}

export function reloadForUpdatedServer({
  token,
  currentUrl = window.location.href,
  readStoredToken = readSessionToken,
  reload = () => window.location.reload(),
  replace = (url) => window.location.replace(url),
}: ReloadForUpdatedServerOptions): void {
  if (readStoredToken() === token) {
    reload();
    return;
  }

  const url = new URL(currentUrl);
  url.searchParams.set("token", token);
  replace(`${url.pathname}${url.search}${url.hash}`);
}

export async function waitForServerVersion({
  token,
  expectedVersion,
  signal,
  attempts = 90,
  intervalMs = 1_000,
  readHealth = getHealth,
  wait = waitForDelay,
}: WaitForServerVersionOptions): Promise<boolean> {
  for (let attempt = 0; attempt < attempts; attempt += 1) {
    if (signal?.aborted) {
      return false;
    }

    if (attempt > 0) {
      try {
        await wait(intervalMs, signal);
      } catch {
        return false;
      }
    }

    try {
      const health = await readHealth(token, signal);
      if (health.serverVersion === expectedVersion) {
        return true;
      }
    } catch {
      // A short connection failure is expected while the server is replaced.
    }
  }

  return false;
}

function waitForDelay(
  milliseconds: number,
  signal?: AbortSignal,
): Promise<void> {
  return new Promise((resolve, reject) => {
    if (signal?.aborted) {
      reject(new DOMException("The update wait was cancelled.", "AbortError"));
      return;
    }

    const onAbort = () => {
      window.clearTimeout(timer);
      reject(new DOMException("The update wait was cancelled.", "AbortError"));
    };
    const timer = window.setTimeout(() => {
      signal?.removeEventListener("abort", onAbort);
      resolve();
    }, milliseconds);
    signal?.addEventListener("abort", onAbort, { once: true });
  });
}
