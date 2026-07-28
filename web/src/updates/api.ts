import { ApiError, apiRequest } from "../api";

export const UPDATE_STATES = [
  "disabled",
  "checking",
  "upToDate",
  "available",
  "downloading",
  "verifying",
  "staged",
  "restarting",
  "failed",
] as const;

export type UpdateState = (typeof UPDATE_STATES)[number];

export interface UpdateStatus {
  schemaVersion: 1;
  currentVersion: string;
  latestVersion: string | null;
  state: UpdateState;
  installSupported: boolean;
  installReason: string | null;
  releaseUrl: string | null;
  progressPercent: number | null;
  error: string | null;
  checkedAt: string | null;
}

export interface ApplyUpdateRequest {
  expectedVersion: string;
  confirmSessionTermination: true;
}

export async function getUpdateStatus(
  token: string,
  signal?: AbortSignal,
): Promise<UpdateStatus> {
  const response = await apiRequest<unknown>("/api/update", token, { signal });
  return parseUpdateStatus(response);
}

export async function checkForUpdate(
  token: string,
  signal?: AbortSignal,
): Promise<UpdateStatus> {
  const response = await apiRequest<unknown>("/api/update/check", token, {
    method: "POST",
    signal,
  });
  return parseUpdateStatus(response);
}

export async function applyUpdate(
  token: string,
  expectedVersion: string,
  signal?: AbortSignal,
): Promise<UpdateStatus> {
  if (!isVersion(expectedVersion)) {
    throw new Error("A valid expected update version is required.");
  }

  const request: ApplyUpdateRequest = {
    expectedVersion,
    confirmSessionTermination: true,
  };
  const response = await apiRequest<unknown>("/api/update/apply", token, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(request),
    signal,
  });
  return parseUpdateStatus(response);
}

export function parseUpdateStatus(value: unknown): UpdateStatus {
  if (
    !isRecord(value) ||
    value.schemaVersion !== 1 ||
    !isVersion(value.currentVersion) ||
    !isNullableVersion(value.latestVersion) ||
    !isUpdateState(value.state) ||
    typeof value.installSupported !== "boolean" ||
    !isNullableBoundedText(value.installReason, 2_048) ||
    !isNullableHttpsUrl(value.releaseUrl) ||
    !isNullableProgress(value.progressPercent) ||
    !isNullableBoundedText(value.error, 8_192) ||
    !isNullableTimestamp(value.checkedAt)
  ) {
    throw new ApiError(502, "The server returned an invalid update response.");
  }

  return {
    schemaVersion: 1,
    currentVersion: value.currentVersion,
    latestVersion: value.latestVersion,
    state: value.state,
    installSupported: value.installSupported,
    installReason: value.installReason,
    releaseUrl: value.releaseUrl,
    progressPercent: value.progressPercent,
    error: value.error,
    checkedAt: value.checkedAt,
  };
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function isVersion(value: unknown): value is string {
  return (
    typeof value === "string" &&
    value.length > 0 &&
    value.length <= 128 &&
    /^[0-9A-Za-z][0-9A-Za-z.+_-]*$/.test(value)
  );
}

function isNullableVersion(value: unknown): value is string | null {
  return value === null || isVersion(value);
}

function isUpdateState(value: unknown): value is UpdateState {
  return (
    typeof value === "string" &&
    (UPDATE_STATES as readonly string[]).includes(value)
  );
}

function isNullableBoundedText(
  value: unknown,
  maxLength: number,
): value is string | null {
  return value === null || (typeof value === "string" && value.length <= maxLength);
}

function isNullableHttpsUrl(value: unknown): value is string | null {
  if (value === null) {
    return true;
  }
  if (typeof value !== "string" || value.length > 2_048) {
    return false;
  }
  try {
    return new URL(value).protocol === "https:";
  } catch {
    return false;
  }
}

function isNullableProgress(value: unknown): value is number | null {
  return (
    value === null ||
    (typeof value === "number" &&
      Number.isFinite(value) &&
      value >= 0 &&
      value <= 100)
  );
}

function isNullableTimestamp(value: unknown): value is string | null {
  return (
    value === null ||
    (typeof value === "string" &&
      value.length > 0 &&
      value.length <= 128 &&
      Number.isFinite(Date.parse(value)))
  );
}
