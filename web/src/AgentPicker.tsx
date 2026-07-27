import { useEffect, useRef, useState, type KeyboardEvent } from "react";
import {
  type AgentCatalog,
  type AgentCatalogEntry,
  type AgentKind,
} from "./api";
import { AGENT_DESCRIPTIONS, AGENT_LABELS } from "./agents";
import { copyTextToClipboard } from "./clipboard";

interface AgentPickerProps {
  catalog: AgentCatalog | null;
  loading: boolean;
  error: string | null;
  creatingAgent: AgentKind | null;
  onSelect: (agent: AgentKind) => void;
  onRefresh: () => void;
  onClose: () => void;
}

interface CopyFeedback {
  agent: AgentKind;
  message: string;
  failed: boolean;
}

interface RefreshFocusTarget {
  element: HTMLButtonElement;
  agent: AgentKind | null;
}

const STATE_LABELS: Record<AgentCatalogEntry["state"], string> = {
  ready: "Ready",
  missing: "Not found",
  misconfigured: "Configuration error",
};

export function AgentPicker({
  catalog,
  loading,
  error,
  creatingAgent,
  onSelect,
  onRefresh,
  onClose,
}: AgentPickerProps) {
  const dialogRef = useRef<HTMLElement>(null);
  const returnFocusRef = useRef<HTMLElement | null>(null);
  const refreshReturnFocusRef = useRef<RefreshFocusTarget | null>(null);
  const wasLoadingRef = useRef(loading);
  const [copyFeedback, setCopyFeedback] = useState<CopyFeedback | null>(null);
  const creating = creatingAgent !== null;

  useEffect(() => {
    returnFocusRef.current =
      document.activeElement instanceof HTMLElement
        ? document.activeElement
        : null;

    const frame = window.requestAnimationFrame(() => {
      const preferred =
        dialogRef.current?.querySelector<HTMLElement>(
          "[data-agent-start]:not([disabled])",
        ) ??
        dialogRef.current?.querySelector<HTMLElement>(
          "button:not([disabled]), a[href]",
        );
      (preferred ?? dialogRef.current)?.focus({ preventScroll: true });
    });

    return () => {
      window.cancelAnimationFrame(frame);
      returnFocusRef.current?.focus({ preventScroll: true });
    };
  }, []);

  useEffect(() => {
    const refreshFinished = wasLoadingRef.current && !loading;
    wasLoadingRef.current = loading;
    if (!refreshFinished) {
      return;
    }

    const returnTarget = refreshReturnFocusRef.current;
    refreshReturnFocusRef.current = null;
    const target =
      (returnTarget?.element.isConnected ? returnTarget.element : null) ??
      (returnTarget?.agent
        ? dialogRef.current?.querySelector<HTMLButtonElement>(
            `[data-agent-start="${returnTarget.agent}"]:not([disabled])`,
          )
        : null) ??
      dialogRef.current?.querySelector<HTMLButtonElement>(
        "[data-agent-refresh]:not([disabled])",
      );
    if (
      target?.isConnected &&
      (document.activeElement === document.body ||
        document.activeElement === dialogRef.current ||
        !document.activeElement?.isConnected)
    ) {
      target.focus({ preventScroll: true });
    }
  }, [loading]);

  const close = () => {
    if (!creating) {
      onClose();
    }
  };

  const refresh = (
    returnFocus: HTMLButtonElement,
    agent: AgentKind | null = null,
  ) => {
    refreshReturnFocusRef.current = { element: returnFocus, agent };
    setCopyFeedback(null);
    onRefresh();
  };

  const handleDialogKeyDown = (event: KeyboardEvent<HTMLElement>) => {
    if (event.key === "Escape") {
      event.preventDefault();
      close();
      return;
    }

    if (event.key !== "Tab") {
      return;
    }

    const focusable = Array.from(
      dialogRef.current?.querySelectorAll<HTMLElement>(
        'button:not([disabled]), a[href], [tabindex]:not([tabindex="-1"])',
      ) ?? [],
    ).filter((element) => !element.hasAttribute("aria-hidden"));
    const first = focusable[0];
    const last = focusable.at(-1);
    if (!first || !last) {
      event.preventDefault();
      dialogRef.current?.focus({ preventScroll: true });
      return;
    }

    if (event.shiftKey && document.activeElement === first) {
      event.preventDefault();
      last.focus({ preventScroll: true });
    } else if (!event.shiftKey && document.activeElement === last) {
      event.preventDefault();
      first.focus({ preventScroll: true });
    }
  };

  const copyInstallCommand = async (agent: AgentCatalogEntry) => {
    if (!agent.install.command) {
      return;
    }
    try {
      if (!(await copyTextToClipboard(agent.install.command))) {
        throw new Error("Clipboard copy was rejected.");
      }
      setCopyFeedback({
        agent: agent.kind,
        message: `${AGENT_LABELS[agent.kind]} install command copied.`,
        failed: false,
      });
    } catch {
      setCopyFeedback({
        agent: agent.kind,
        message:
          "Copy was blocked. Touch and hold the command, or select it with the mouse.",
        failed: true,
      });
    }
  };

  const serverSummary = describeServer(catalog);
  const agents = catalog?.agents ?? [];

  return (
    <div
      className="dialog-backdrop"
      role="presentation"
      onMouseDown={close}
    >
      <section
        ref={dialogRef}
        className="agent-picker"
        role="dialog"
        aria-modal="true"
        aria-labelledby="agent-picker-title"
        aria-describedby="agent-picker-description"
        aria-busy={loading || creating}
        tabIndex={-1}
        onKeyDown={handleDialogKeyDown}
        onMouseDown={(event) => event.stopPropagation()}
      >
        <div className="agent-picker-header">
          <div className="agent-picker-title-row">
            <div>
              <h2 id="agent-picker-title">New terminal</h2>
              <p id="agent-picker-description">
                Choose a CLI installed on {serverSummary}. The agent runs there,
                not on this browser or phone.
              </p>
            </div>
            <div className="agent-picker-header-actions">
              <button
                type="button"
                data-agent-refresh
                disabled={loading || creating}
                onClick={(event) => refresh(event.currentTarget)}
                aria-label="Check installed agents again"
              >
                {loading ? "Checking…" : "Refresh"}
              </button>
              <button
                type="button"
                disabled={creating}
                onClick={close}
                aria-label="Close agent picker"
              >
                ×
              </button>
            </div>
          </div>
          <p className="agent-picker-host-warning">
            Installation commands must be run on the server host as the account
            that runs Codex Web Terminal. Review the official instructions
            before downloading and installing software.
          </p>
          <div className="agent-picker-announcement" aria-live="polite">
            {loading
              ? "Checking installed CLI agents…"
              : copyFeedback?.message ?? ""}
          </div>
          {error && (
            <div className="agent-picker-error" role="alert">
              {error}
            </div>
          )}
        </div>

        <div className="agent-options" role="list" aria-label="CLI agents">
          {!catalog && loading && (
            <div className="agent-picker-empty" role="status">
              Checking the server for installed CLI agents…
            </div>
          )}
          {!loading && agents.length === 0 && (
            <div className="agent-picker-empty" role="status">
              No supported CLI agents were reported. Check the server
              configuration and try again.
            </div>
          )}
          {agents.map((agent) => {
            const label = AGENT_LABELS[agent.kind];
            const isCreating = creatingAgent === agent.kind;
            const isReady = agent.state === "ready";
            const hasBrokenOverride =
              agent.state === "misconfigured" &&
              agent.configuration === "override";
            const feedback =
              copyFeedback?.agent === agent.kind ? copyFeedback : null;

            return (
              <article
                key={agent.kind}
                className={`agent-option agent-option--${agent.kind} agent-option--${agent.state}`}
                role="listitem"
                aria-labelledby={`agent-${agent.kind}-name`}
              >
                <div className="agent-option-heading">
                  <span
                    className={`agent-mark agent-mark--${agent.kind}`}
                    aria-hidden="true"
                  >
                    {label.slice(0, 1)}
                  </span>
                  <span className="agent-option-identity">
                    <strong id={`agent-${agent.kind}-name`}>{label}</strong>
                    <small>{AGENT_DESCRIPTIONS[agent.kind]}</small>
                  </span>
                  <span
                    className={`agent-discovery-state agent-discovery-state--${agent.state}`}
                  >
                    {STATE_LABELS[agent.state]}
                  </span>
                </div>

                <div className="agent-option-details">
                  {isReady ? (
                    <p className="agent-version">
                      {agent.version
                        ? `Installed version ${agent.version}`
                        : "Installed version unavailable"}
                    </p>
                  ) : (
                    <p className="agent-unavailable-reason">
                      {hasBrokenOverride
                        ? `The configured ${label} command could not be started. Repair that exact executable or its permissions, then check again. If the override value must change, update the server startup configuration and restart Codex Web Terminal.`
                        : agent.state === "missing"
                          ? `${label} was not found in the server account's PATH or standard per-user install locations.`
                          : `A ${label} executable was found, but its version check failed. Check permissions, PATH, or the installation.`}
                    </p>
                  )}

                  {agent.dangerouslySkipPermissions && (
                    <p className="agent-permission-warning">
                      <strong>Approvals disabled.</strong> This agent may edit
                      files and run commands without asking for confirmation.
                    </p>
                  )}
                </div>

                {isReady ? (
                  <div className="agent-option-actions agent-option-actions--start">
                    <button
                      type="button"
                      className="agent-start-button"
                      data-agent-start={agent.kind}
                      disabled={loading || creating}
                      onClick={() => onSelect(agent.kind)}
                    >
                      {isCreating ? `Starting ${label}…` : `Start ${label}`}
                    </button>
                  </div>
                ) : (
                  <div className="agent-install-guide">
                    {agent.install.command ? (
                      <>
                        <div className="agent-install-command">
                          <code>{agent.install.command}</code>
                          <button
                            type="button"
                            disabled={creating}
                            onClick={() => void copyInstallCommand(agent)}
                            aria-label={`Copy ${label} installation command for ${agent.install.shell}`}
                          >
                            {feedback && !feedback.failed ? "Copied" : "Copy"}
                          </button>
                        </div>
                        <p className="agent-install-help">
                          {hasBrokenOverride ? (
                            <>
                              The official installer will not replace an
                              authoritative command override. Repair or install
                              the executable so the configured name or path
                              resolves, then check again. If the override value
                              must change or be removed, update the server
                              configuration and restart the server.
                            </>
                          ) : (
                            <>
                              {agent.state === "missing"
                                ? "Check the server account's PATH first. If the CLI is not installed, run the command above with"
                                : "After checking permissions and PATH, reinstall if needed by running the command above with"}{" "}
                              {agent.install.shell}
                              {agent.install.verifyCommand
                                ? `, verify with “${agent.install.verifyCommand}”,`
                                : ""}
                              {" then check again."}
                            </>
                          )}
                        </p>
                      </>
                    ) : (
                      <p className="agent-install-help">
                        This server version did not provide an installation
                        command. Use the official documentation.
                      </p>
                    )}
                    <div className="agent-option-actions">
                      {agent.install.docsUrl && (
                        <a
                          className="agent-docs-link"
                          href={agent.install.docsUrl}
                          target="_blank"
                          rel="noopener noreferrer"
                          aria-label={`Open official ${label} installation documentation in a new tab`}
                        >
                          Official docs ↗
                        </a>
                      )}
                      <button
                        type="button"
                        disabled={loading || creating}
                        onClick={(event) =>
                          refresh(event.currentTarget, agent.kind)
                        }
                      >
                        {loading ? "Checking…" : "Check again"}
                      </button>
                    </div>
                  </div>
                )}
              </article>
            );
          })}
        </div>
      </section>
    </div>
  );
}

function describeServer(catalog: AgentCatalog | null): string {
  if (!catalog || catalog.server.os === "unknown") {
    return "the server host";
  }

  const os =
    catalog.server.os.charAt(0).toUpperCase() + catalog.server.os.slice(1);
  const arch =
    catalog.server.arch && catalog.server.arch !== "unknown"
      ? ` (${catalog.server.arch})`
      : "";
  return `the ${os}${arch} server host`;
}
