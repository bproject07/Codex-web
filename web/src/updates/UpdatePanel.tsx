import { useEffect, useState } from "react";
import type { UpdateStatus } from "./api";
import { isUpdatePollState } from "./restart";

export type UpdateOperation = "checking" | "applying" | "reconnecting" | null;

export interface UpdatePanelProps {
  status: UpdateStatus | null;
  loading: boolean;
  operation: UpdateOperation;
  error: string | null;
  onCheck: () => void;
  onApply: (expectedVersion: string) => void;
}

const STATE_LABELS: Record<UpdateStatus["state"], string> = {
  disabled: "Updates unavailable",
  checking: "Checking for updates…",
  upToDate: "You are up to date",
  available: "Update available",
  downloading: "Downloading update…",
  verifying: "Verifying package…",
  staged: "Update ready",
  restarting: "Restarting server…",
  failed: "Update failed",
};

export function UpdatePanel({
  status,
  loading,
  operation,
  error,
  onCheck,
  onApply,
}: UpdatePanelProps) {
  const [confirmed, setConfirmed] = useState(false);
  const expectedVersion = status?.latestVersion ?? null;
  const busy = loading || operation !== null || isTransitional(status?.state);
  const canApply =
    Boolean(expectedVersion) &&
    status?.installSupported === true &&
    status.state === "available";

  useEffect(() => {
    setConfirmed(false);
  }, [expectedVersion]);

  return (
    <section className="update-settings" aria-labelledby="update-settings-title">
      <div className="update-settings__heading">
        <div>
          <strong id="update-settings-title">Software updates</strong>
          <span>
            {status
              ? `Installed version ${status.currentVersion}`
              : loading
                ? "Loading installed version…"
                : "Update status unavailable"}
          </span>
        </div>
        {status?.state === "available" && (
          <span className="update-settings__badge">Available</span>
        )}
      </div>

      {status && (
        <div className="update-settings__status" role="status" aria-live="polite">
          <span>{operationLabel(operation) ?? STATE_LABELS[status.state]}</span>
          {status.latestVersion && status.latestVersion !== status.currentVersion && (
            <strong>Latest release {status.latestVersion}</strong>
          )}
        </div>
      )}

      {(status?.progressPercent !== null &&
        status?.progressPercent !== undefined) && (
        <div className="update-settings__progress">
          <progress max="100" value={status.progressPercent}>
            {status.progressPercent}%
          </progress>
          <span>{Math.round(status.progressPercent)}%</span>
        </div>
      )}

      {(error || status?.error) && (
        <p className="update-settings__error" role="alert">
          {error ?? status?.error}
        </p>
      )}

      {status && !status.installSupported && status.installReason && (
        <p className="update-settings__reason">{status.installReason}</p>
      )}

      {status?.releaseUrl && (
        <a
          className="update-settings__release"
          href={status.releaseUrl}
          target="_blank"
          rel="noopener noreferrer"
        >
          View release details
        </a>
      )}

      {canApply && expectedVersion && (
        <div className="update-settings__confirmation">
          <p role="alert">
            Updating restarts this server and ends every running terminal and
            @cwt reviewer session. This browser tab recreates ordinary terminal
            tabs with the same agent and folder as fresh sessions; live output
            and reviewer conversations cannot be resumed. Favorites and Recent
            folders remain saved.
          </p>
          <label className="checkbox-row">
            <input
              type="checkbox"
              checked={confirmed}
              disabled={busy}
              onChange={(event) => setConfirmed(event.target.checked)}
            />
            I understand that active terminal sessions will end
          </label>
          <button
            type="button"
            className="update-settings__apply"
            disabled={busy || !confirmed}
            onClick={() => {
              setConfirmed(false);
              onApply(expectedVersion);
            }}
          >
            Update to {expectedVersion} and restart
          </button>
        </div>
      )}

      <div className="update-settings__actions">
        <button type="button" disabled={busy} onClick={onCheck}>
          {operation === "checking" || status?.state === "checking"
            ? "Checking…"
            : "Check for updates"}
        </button>
        {status?.checkedAt && (
          <time dateTime={status.checkedAt}>
            Checked {formatCheckedAt(status.checkedAt)}
          </time>
        )}
      </div>
    </section>
  );
}

function isTransitional(state: UpdateStatus["state"] | undefined): boolean {
  return isUpdatePollState(state) || state === "restarting";
}

function operationLabel(operation: UpdateOperation): string | null {
  switch (operation) {
    case "checking":
      return "Checking for updates…";
    case "applying":
      return "Preparing verified update…";
    case "reconnecting":
      return "Waiting for the updated server…";
    default:
      return null;
  }
}

function formatCheckedAt(value: string): string {
  try {
    return new Intl.DateTimeFormat(undefined, {
      dateStyle: "medium",
      timeStyle: "short",
    }).format(new Date(value));
  } catch {
    return value;
  }
}
