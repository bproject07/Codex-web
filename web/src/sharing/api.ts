export interface Proposal {
  id: string; author: string; original: string | null; text: string | null;
  approvedText: string | null; revision: number; state: string; reason: string | null;
  sessionId: string; createdAt: number; updatedAt: number;
}
export interface Guest {
  grantId: string; guestId: string; sessionId: string; name: string;
  mode: "view" | "propose"; admitted: boolean; expiresAt: number;
}
export interface OwnerView {
  sessionId: string | null; status: string; recording: boolean; archiveIncomplete: boolean;
  guests: (Guest & { revoked: boolean; presence: string })[]; proposals: Proposal[];
  usage: { invitations: number; proposals: number; approvals: number; limit: number };
}
export interface GuestView {
  guest: Guest; status: string; agent: string; cols: number; rows: number;
  archiveIncomplete: boolean; proposals: Proposal[];
}
export interface Generation {
  id: string; terminalId?: string; label?: string; agent: string; createdAt: number;
  bytes: number; incomplete: boolean; ended: boolean; cols: number; rows: number;
}
export interface ArchiveEvent {
  sequence: number; kind: "start" | "output" | "resize" | "end";
  data: string; cols: number; rows: number;
}
const errors: Record<string, string> = {
  invalid_invitation: "This invitation has expired or is unavailable.",
  invitation_used: "This invitation has already been used.",
  name_taken: "That name is already reserved for this session. Choose another name.",
  invalid_name: "Use 1–32 letters, numbers, spaces, dashes or underscores.",
  name_locked: "Your name is fixed for this shared session.",
  sharing_capacity: "The sharing limit has been reached.",
  invalid_credential: "Guest access is unavailable. Request a new invitation.",
  access_ended: "Your access has ended.",
  generation_changed: "The terminal restarted. A new invitation is required.",
  awaiting_admission: "Waiting for the owner to admit you.",
  view_only: "This invitation allows viewing only.",
  proposal_pending: "Your previous proposal is still waiting for a decision.",
  proposal_changed: "This proposal has changed. Review its latest state.",
  request_conflict: "This request does not match the saved request.",
  rate_limited: "Please wait before trying again.",
  not_running: "The terminal is not running.",
  terminal_busy: "A lifecycle, peer review or context transfer operation is active.",
  queue_rejected: "The terminal did not accept the input. Review the terminal before trying again.",
  single_line_only: "Sending currently supports one line. Edit the proposal to one line before approval.",
  invalid_proposal: "Enter text up to 16 KiB without terminal control characters.",
  confirm_empty_prompt: "Confirm that the agent is waiting at an empty prompt.",
  storage_unavailable: "Sharing storage is unavailable. Terminal sessions continue running.",
  storage_busy: "Sharing storage is busy. Refresh the status before retrying.",
  unsafe_state_directory: "The server's data folder permissions are not private. Open Storage to repair Windows permissions.",
  unsafe_state_file: "A sharing database file has unsafe permissions or is a link. Open Storage for recovery options.",
  sharing_store_in_use: "Another server is using this sharing database. Each server needs its own data folder.",
  sharing_schema_too_new: "This sharing database requires a newer application version.",
  storage_maintenance: "Sharing storage maintenance is running. Terminal sessions continue running.",
  permissions_repair_failed: "Windows refused the permission repair. Check the server account's access to its data folder.",
  permissions_owner_mismatch: "The data folder or a database file belongs to another account. Ask the server administrator to check ownership.",
  repair_unavailable: "Automatic permission repair is unavailable for this storage problem.",
  recording_enabled: "Choose Stop recording on every recorded terminal before reclaiming disk space. Terminals can keep running.",
  delivery_pending: "A proposal delivery is still awaiting its receipt. Retry after it settles.",
  cleanup_pending: "Recording deletion is still running. Wait for it to finish before reclaiming space.",
  cleanup_changed: "The eligible recordings changed. Preview cleanup again before deleting.",
  insufficient_disk_space: "There is not enough temporary disk space to compact this older database safely.",
  invalid_storage_policy: "Choose 1–3650 days and a recording budget between 16 and 2048 MiB.",
  recording_active: "Stop the terminal and wait for its final output before deleting this recording.",
};

export interface StoragePolicy { enabled: boolean; retentionDays: number; maxBytes: number }
export interface StorageUsage {
  available: boolean; code?: string; canRepair?: boolean; repairing?: boolean;
  databaseBytes: number; walBytes: number; sharedMemoryBytes: number; diskBytes: number;
  reusableBytes: number; archiveBytes: number; recordings: number; cleanupPending: number;
  recordingEnabledCount: number; maintenance: boolean; freeDiskBytes: number;
  incrementalCompaction: boolean; policy: StoragePolicy;
}
export interface CleanupFilter { olderThanDays?: number; generations?: string[] }
export interface CleanupPreview { count: number; bytes: number; fingerprint: string }
export interface CompactionResult { beforeBytes: number; afterBytes: number; reclaimedBytes: number; elapsedMs: number }
export class SharingError extends Error {
  constructor(public code: string, public status: number) {
    super(errors[code] ?? code.replaceAll("_", " "));
  }
}
export async function request<T>(path: string, token: string, method = "GET", body?: unknown, signal?: AbortSignal): Promise<T> {
  const response = await fetch(path, {
    method, signal, cache: "no-store", credentials: "omit",
    headers: { Authorization: "Bearer " + token, ...(body === undefined ? {} : { "Content-Type": "application/json" }) },
    ...(body === undefined ? {} : { body: JSON.stringify(body) }),
  });
  if (!response.ok) {
    const error = await response.json().catch(() => ({})) as { code?: string };
    throw new SharingError(error.code ?? "request_failed", response.status);
  }
  return response.status === 204 ? undefined as T : response.json() as Promise<T>;
}
export function newId(): string {
  const bytes = crypto.getRandomValues(new Uint8Array(16));
  bytes[6] = (bytes[6]! & 15) | 64;
  bytes[8] = (bytes[8]! & 63) | 128;
  const hex = Array.from(bytes, (b) => b.toString(16).padStart(2, "0")).join("");
  return [hex.slice(0,8),hex.slice(8,12),hex.slice(12,16),hex.slice(16,20),hex.slice(20)].join("-");
}
export function newCredential(): string {
  const secret = btoa(String.fromCharCode(...crypto.getRandomValues(new Uint8Array(32))))
    .replaceAll("+", "-").replaceAll("/", "_").replaceAll("=", "");
  return newId() + "." + secret;
}
export const proposalState: Record<string,string> = {
  pending: "Waiting for approval", approving: "Saving approval", queued: "Queued for terminal",
  written: "Written to terminal", not_written: "Not written", uncertain: "Delivery uncertain — inspect the terminal",
  rejected: "Rejected", invalidated: "No longer valid", expired: "Expired",
};
export function displayError(error: unknown): string {
  return error instanceof Error ? error.message : "The request failed. Refresh the status before retrying.";
}
export function singleLine(text: string): boolean {
  return !/[\u0000-\u001f\u007f-\u009f]/u.test(text);
}
export const TEXT_LIMIT = 16 * 1024;
export function textBytes(text: string): number { return new TextEncoder().encode(text).length; }
export function validProposal(text: string): boolean {
  return !!text.trim() && textBytes(text) <= TEXT_LIMIT && !/[\u0000-\u0008\u000b-\u001f\u007f-\u009f]/u.test(text);
}
export function validName(name: string): boolean {
  const trimmed = name.trim();
  return !!trimmed && Array.from(trimmed).length <= 32 && /^[\p{L}\p{N} _-]+$/u.test(trimmed) &&
    !["owner", "system", "admin", "собственик"].includes(trimmed.toLowerCase());
}
