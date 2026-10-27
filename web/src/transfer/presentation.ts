import type { SessionSnapshot } from "../api";
import type { TransferSnapshot, TransferState } from "./types";

export function transferStateLabel(state: TransferState): string {
  switch (state) {
    case "preparing":
      return "Source is preparing the handoff";
    case "preview":
      return "Review the handoff";
    case "starting":
      return "Starting the target terminal";
    case "cleanup_pending":
      return "Target start failed; cleanup pending";
    case "target_bound":
      return "Target ready to receive";
    case "delivering":
    case "awaiting_receipt":
      return "Delivery unconfirmed";
    case "delivered":
      return "Delivered";
    case "failed":
      return "Failed";
    case "abandoned":
      return "Abandoned";
  }
}

/** Human explanation of the last server-reported error code, if any. */
export function transferErrorLabel(code: string | null): string | null {
  switch (code) {
    case null:
      return null;
    case "queue_rejected":
      return "The terminal did not accept the prompt; nothing was sent. Make sure it is running and try again.";
    case "source_ended":
      return "The source session ended before it submitted a handoff. Write the handoff by hand or restart the source and start again.";
    case "target_ended":
      return "The target session ended before the handoff was confirmed. Choose a target again; the draft was kept.";
    case "target_start_failed":
      return "The new terminal could not be started. Choose a target again; the draft was kept.";
    case "cleanup_pending":
    case "cleanup_failed":
      return "The new terminal did not start and its process could not be confirmed stopped. Retry cleanup, or abandon the transfer and close the tab yourself.";
    case "attempt_limit_reached":
      return "This transfer has used all of its target attempts. Abandon it and start a new one.";
    case "capacity_full":
      return "The terminal session limit has been reached. Close a tab or continue into an existing one.";
    case "participant_busy":
      return "That terminal is busy with a peer thread or another transfer.";
    case "generation_conflict":
      return "The terminal restarted since it was chosen. Confirm readiness again.";
    case "revision_conflict":
      return "The draft changed in another tab. Reload it before continuing.";
    case "shutting_down":
      return "The server is shutting down.";
    default:
      return `The transfer reported an error (${code}).`;
  }
}

/**
 * Ordinary running tabs other than the source are eligible targets. A tab with
 * an unrecognized agent is excluded because this bundle cannot describe it.
 */
export function eligibleExistingTargets(
  sessions: SessionSnapshot[],
  sourceTerminalId: string,
): SessionSnapshot[] {
  return sessions.filter(
    (candidate) =>
      candidate.terminalId !== sourceTerminalId &&
      candidate.purpose.kind === "interactive" &&
      candidate.status === "running" &&
      candidate.sessionId !== null &&
      candidate.agent !== null,
  );
}

/** Whether the composer should offer the draft editor. */
export function draftEditable(transfer: TransferSnapshot | null): boolean {
  return (
    transfer !== null &&
    (transfer.state === "preparing" ||
      transfer.state === "preview" ||
      transfer.state === "target_bound")
  );
}

/** The terminal the browser should select so the user can watch it start. */
export function targetTerminalToSelect(
  transfer: TransferSnapshot | null,
): string | null {
  const attempt = transfer?.attempt;
  if (!attempt) return null;
  return attempt.outcome === "starting" || attempt.outcome === "bound"
    ? attempt.target.terminalId
    : null;
}

export function generateRequestId(): string {
  if (typeof crypto !== "undefined" && typeof crypto.randomUUID === "function") {
    return crypto.randomUUID();
  }
  // Fallback for environments without randomUUID; still a valid v4 UUID.
  const bytes = new Uint8Array(16);
  if (typeof crypto !== "undefined" && typeof crypto.getRandomValues === "function") {
    crypto.getRandomValues(bytes);
  } else {
    for (let index = 0; index < bytes.length; index += 1) {
      bytes[index] = Math.floor(Math.random() * 256);
    }
  }
  bytes[6] = (bytes[6] & 0x0f) | 0x40;
  bytes[8] = (bytes[8] & 0x3f) | 0x80;
  const hex = Array.from(bytes, (byte) => byte.toString(16).padStart(2, "0"));
  return `${hex.slice(0, 4).join("")}-${hex.slice(4, 6).join("")}-${hex
    .slice(6, 8)
    .join("")}-${hex.slice(8, 10).join("")}-${hex.slice(10).join("")}`;
}
