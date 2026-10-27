import {
  useEffect,
  useMemo,
  useRef,
  useState,
  type FormEvent,
  type KeyboardEvent,
} from "react";
import { AGENT_LABELS, sessionAgentLabel, type AgentKind } from "../agents";
import type { AgentCatalog, SessionSnapshot } from "../api";
import {
  WorkspacePicker,
  type WorkspaceBrowserAdapter,
  type WorkspaceDirectory,
} from "../workspaces";
import {
  draftEditable,
  eligibleExistingTargets,
  generateRequestId,
  transferErrorLabel,
  transferStateLabel,
} from "./presentation";
import type { TransferOperation } from "./useTransferController";
import type {
  TransferDraft,
  TransferMode,
  TransferSnapshot,
  TransferTargetSelection,
} from "./types";

const MAX_INSTRUCTION_BYTES = 4 * 1024;
const MAX_HANDOFF_BYTES = 64 * 1024;

const MANUAL_TEMPLATE = `# Handoff

## Goal

## Decisions so far

## Relevant files and paths

## Done and verified

## Open items and risks
`;

export interface TransferComposerProps {
  sourceSession: SessionSnapshot;
  sessions: SessionSnapshot[];
  workspaceAdapter: WorkspaceBrowserAdapter;
  catalog: AgentCatalog | null;
  catalogLoading: boolean;
  catalogError: string | null;
  newTargetDisabledReason: string | null;
  transfer: TransferSnapshot | null;
  relatedTransfers?: TransferSnapshot[];
  draft: TransferDraft | null;
  operation: TransferOperation | null;
  error: string | null;
  onCreate: (mode: TransferMode, instruction: string) => Promise<void>;
  onSaveDraft: (expectedRevision: number, content: string) => Promise<void>;
  onChooseTarget: (selection: TransferTargetSelection) => Promise<void>;
  onDeliver: (expectedRevision: number, targetSessionId: string) => Promise<void>;
  onCleanup: (targetTerminalId: string, targetSessionId: string | null) => Promise<void>;
  onAbandon: () => Promise<void>;
  onSelectTerminal: (terminalId: string) => void;
  onClearError: () => void;
  onClose: () => void;
  onFollowTransfer?: (transferId: string | null) => void;
}

type TargetChoice =
  | { kind: "new"; agent: AgentKind }
  | { kind: "existing"; terminalId: string };

export function TransferComposer(props: TransferComposerProps) {
  const {
    sourceSession,
    sessions,
    workspaceAdapter,
    catalog,
    catalogLoading,
    catalogError,
    newTargetDisabledReason,
    transfer,
    relatedTransfers = [],
    draft,
    operation,
    error,
    onCreate,
    onSaveDraft,
    onChooseTarget,
    onDeliver,
    onCleanup,
    onAbandon,
    onSelectTerminal,
    onClearError,
    onClose,
    onFollowTransfer,
  } = props;

  const dialogRef = useRef<HTMLElement>(null);
  const folderButtonRef = useRef<HTMLButtonElement>(null);
  const returningFromPickerRef = useRef(false);
  const [mode, setMode] = useState<TransferMode>(
    sourceSession.status === "running" ? "prepare" : "manual",
  );
  const [instruction, setInstruction] = useState("");
  const [handoffDraft, setHandoffDraft] = useState("");
  const [draftKey, setDraftKey] = useState<string | null>(null);
  const [explicitTargetChoice, setTargetChoice] = useState<TargetChoice | null>(
    null,
  );
  const [targetDirectory, setTargetDirectory] = useState<WorkspaceDirectory>(
    () => directoryForSession(sourceSession),
  );
  const [workspacePickerOpen, setWorkspacePickerOpen] = useState(false);
  const [formError, setFormError] = useState<string | null>(null);
  const [submitting, setSubmitting] = useState(false);

  const readyAgents = useMemo(
    () =>
      (catalog?.agents ?? [])
        .filter((agent) => agent.state === "ready")
        .map((agent) => agent.kind),
    [catalog],
  );
  const existingTargets = useMemo(
    () => eligibleExistingTargets(sessions, sourceSession.terminalId),
    [sessions, sourceSession.terminalId],
  );
  const pending = operation !== null || submitting;
  const sourceRunning =
    sourceSession.status === "running" && sourceSession.sessionId !== null;
  // Default to the first ready agent, then the first eligible tab, until the
  // user picks explicitly. Derived synchronously so the first render is right.
  const targetChoice: TargetChoice | null = useMemo(() => {
    if (explicitTargetChoice) {
      const stillValid =
        explicitTargetChoice.kind === "new"
          ? readyAgents.includes(explicitTargetChoice.agent)
          : existingTargets.some(
              (candidate) => candidate.terminalId === explicitTargetChoice.terminalId,
            );
      if (stillValid) return explicitTargetChoice;
    }
    if (readyAgents.length > 0 && newTargetDisabledReason === null) {
      return { kind: "new", agent: readyAgents[0] };
    }
    if (existingTargets.length > 0) {
      return { kind: "existing", terminalId: existingTargets[0].terminalId };
    }
    if (readyAgents.length > 0) {
      return { kind: "new", agent: readyAgents[0] };
    }
    return null;
  }, [existingTargets, explicitTargetChoice, newTargetDisabledReason, readyAgents]);

  // Keep the local textarea in sync with the server draft by revision, so a
  // newer server revision (helper submit, other tab) replaces unsent edits.
  useEffect(() => {
    if (!draft) return;
    const key = `${transfer?.transferId ?? ""}:${draft.draftRevision}`;
    if (key !== draftKey) {
      setDraftKey(key);
      setHandoffDraft(draft.content ?? "");
    }
  }, [draft, draftKey, transfer?.transferId]);

  useEffect(() => {
    if (workspacePickerOpen) return;
    const frame = window.requestAnimationFrame(() => {
      if (returningFromPickerRef.current) {
        returningFromPickerRef.current = false;
        folderButtonRef.current?.focus({ preventScroll: true });
        return;
      }
      const initial = dialogRef.current?.querySelector<HTMLElement>(
        "[data-transfer-initial-focus]:not([disabled])",
      );
      (
        initial ??
        dialogRef.current?.querySelector<HTMLElement>(
          "button:not([disabled]), textarea:not([disabled])",
        )
      )?.focus({ preventScroll: true });
    });
    return () => window.cancelAnimationFrame(frame);
  }, [workspacePickerOpen, transfer?.state]);

  const draftDirty =
    draft !== null && handoffDraft !== (draft.content ?? "");
  const draftReady = draft !== null && transfer !== null &&
    draft.draftRevision === transfer.draftRevision &&
    draftKey === `${transfer.transferId}:${draft.draftRevision}`;

  const requestClose = () => {
    if (draftDirty && !window.confirm("Discard your unsaved edits to the handoff?")) {
      return;
    }
    onClose();
  };

  const submitCreate = async (event: FormEvent) => {
    event.preventDefault();
    setFormError(null);
    if (utf8Length(instruction) > MAX_INSTRUCTION_BYTES) {
      setFormError("The instruction must be 4 KiB or less.");
      return;
    }
    if (mode === "prepare" && !sourceRunning) {
      setFormError("Automated preparation needs a running source terminal. Write the handoff by hand instead.");
      return;
    }
    setSubmitting(true);
    try {
      await onCreate(mode, instruction.trim() || "Continue this work in another agent.");
    } catch {
      // The controller exposes the sanitized API error in the dialog.
    } finally {
      setSubmitting(false);
    }
  };

  const saveDraft = async () => {
    if (!transfer || !draftReady || !draft) return;
    setFormError(null);
    if (!handoffDraft.trim()) {
      setFormError("The handoff must not be empty.");
      return;
    }
    if (utf8Length(handoffDraft) > MAX_HANDOFF_BYTES) {
      setFormError("The handoff must be 64 KiB or less.");
      return;
    }
    setSubmitting(true);
    try {
      await onSaveDraft(draft.draftRevision, handoffDraft);
    } catch {
      // Surfaced by the controller.
    } finally {
      setSubmitting(false);
    }
  };

  const chooseTarget = async () => {
    if (!transfer || !targetChoice || !draftReady) return;
    setFormError(null);
    if (draftDirty) {
      setFormError("Save the handoff before choosing a target.");
      return;
    }
    setSubmitting(true);
    try {
      if (targetChoice.kind === "new") {
        await onChooseTarget({
          mode: "new",
          agent: targetChoice.agent,
          directoryId: targetDirectory.id,
        });
      } else {
        const candidate = existingTargets.find(
          (session) => session.terminalId === targetChoice.terminalId,
        );
        if (!candidate?.sessionId) {
          setFormError("That terminal is no longer running.");
          return;
        }
        await onChooseTarget({
          mode: "existing",
          terminalId: candidate.terminalId,
          sessionId: candidate.sessionId,
        });
      }
    } catch {
      // Surfaced by the controller.
    } finally {
      setSubmitting(false);
    }
  };

  const deliver = async () => {
    const target = transfer?.attempt?.target;
    if (!transfer || !target?.sessionId || !draftReady || !draft) return;
    setFormError(null);
    if (draftDirty) {
      setFormError("Save the handoff before delivering it.");
      return;
    }
    setSubmitting(true);
    try {
      await onDeliver(draft.draftRevision, target.sessionId);
    } catch {
      // Surfaced by the controller.
    } finally {
      setSubmitting(false);
    }
  };

  const cleanup = async () => {
    const target = transfer?.attempt?.target;
    if (!target) return;
    setSubmitting(true);
    try {
      await onCleanup(target.terminalId, target.sessionId);
    } catch {
      // Surfaced by the controller.
    } finally {
      setSubmitting(false);
    }
  };

  const abandon = async () => {
    if (!transfer) return;
    const discarding =
      transfer.state === "delivering" || transfer.state === "awaiting_receipt";
    if (
      !window.confirm(
        discarding
          ? "The prompt was already sent to the target. Discard the transfer anyway? The target will not be able to retrieve the handoff."
          : "Abandon this transfer? The handoff draft will be discarded.",
      )
    ) {
      return;
    }
    setSubmitting(true);
    try {
      await onAbandon();
    } catch {
      // Surfaced by the controller.
    } finally {
      setSubmitting(false);
    }
  };

  const handleDialogKeyDown = (event: KeyboardEvent<HTMLElement>) => {
    if (event.key === "Escape") {
      event.preventDefault();
      requestClose();
      return;
    }
    if (event.key !== "Tab") return;
    const focusable = Array.from(
      dialogRef.current?.querySelectorAll<HTMLElement>(
        'button:not([disabled]), input:not([disabled]), textarea:not([disabled]), a[href], [tabindex]:not([tabindex="-1"])',
      ) ?? [],
    ).filter(
      (element) =>
        !element.hasAttribute("hidden") && element.getClientRects().length > 0,
    );
    const first = focusable[0];
    const last = focusable.at(-1);
    if (!first || !last) {
      event.preventDefault();
      dialogRef.current?.focus({ preventScroll: true });
    } else if (!focusable.includes(document.activeElement as HTMLElement)) {
      event.preventDefault();
      (event.shiftKey ? last : first).focus({ preventScroll: true });
    } else if (event.shiftKey && document.activeElement === first) {
      event.preventDefault();
      last.focus({ preventScroll: true });
    } else if (!event.shiftKey && document.activeElement === last) {
      event.preventDefault();
      first.focus({ preventScroll: true });
    }
  };

  if (workspacePickerOpen) {
    return (
      <div
        className="dialog-backdrop"
        role="presentation"
        onMouseDown={() => setWorkspacePickerOpen(false)}
      >
        <WorkspacePicker
          adapter={workspaceAdapter}
          initialDirectoryId={targetDirectory.id || null}
          title="Choose target folder"
          description="Start the new terminal in this server folder."
          chooseLabel="Use for target"
          disabled={pending}
          onChoose={(directory, transition) => {
            transition.suppressFocusReturn();
            returningFromPickerRef.current = true;
            setTargetDirectory(directory);
            setWorkspacePickerOpen(false);
          }}
          onCancel={() => {
            returningFromPickerRef.current = true;
            setWorkspacePickerOpen(false);
          }}
        />
      </div>
    );
  }

  const state = transfer?.state ?? null;
  const target = transfer?.attempt?.target ?? null;
  const errorLabel = transferErrorLabel(transfer?.errorCode ?? null);
  const visibleError = formError ?? error ?? catalogError;

  return (
    <div
      className="dialog-backdrop transfer-composer-backdrop"
      role="presentation"
      onMouseDown={requestClose}
    >
      <section
        ref={dialogRef}
        className="peer-composer transfer-composer"
        role="dialog"
        aria-modal="true"
        aria-labelledby="transfer-composer-title"
        aria-describedby="transfer-composer-description"
        aria-busy={pending}
        tabIndex={-1}
        onKeyDown={handleDialogKeyDown}
        onMouseDown={(event) => event.stopPropagation()}
      >
        <header className="peer-composer__header">
          <div>
            <div className="peer-composer__eyebrow">Continue in…</div>
            <h2 id="transfer-composer-title">Continue this work in another agent</h2>
            <p id="transfer-composer-description">
              A written handoff moves to a new or existing terminal. It is a
              fresh conversation that starts from that handoff, not a resumed
              process.
            </p>
          </div>
          <button
            type="button"
            className="peer-composer__close"
            aria-label="Close transfer composer"
            onClick={requestClose}
          >
            ×
          </button>
        </header>

        <div className="peer-composer__source">
          <span>Source</span>
          <strong>{sourceSession.name}</strong>
          <code title={sourceSession.project}>{sourceSession.project}</code>
          <small>
            {sessionAgentLabel(sourceSession)} · {sourceSession.status}
          </small>
        </div>
        <details className="peer-composer__readiness">
          <summary>Delivery needs the target terminal at an idle, empty prompt.</summary>
          <p>
            Before you press Deliver, look at the target tab and check that it
            shows an empty agent prompt. A fresh terminal may still require
            sign-in or folder trust; the transfer never accepts those prompts
            for you, and nothing is sent until you confirm.
          </p>
        </details>

        <div className="peer-composer__body">
          {relatedTransfers.length > 0 && onFollowTransfer && (
            <section className="transfer-target" aria-label="Active transfers">
              <label htmlFor="active-transfer">Active transfers for this terminal</label>
              <select
                id="active-transfer"
                value={transfer?.transferId ?? ""}
                disabled={pending}
                onChange={(event) => {
                  if (draftDirty && !window.confirm("Discard your unsaved edits to the handoff?")) return;
                  setHandoffDraft("");
                  setDraftKey(null);
                  onFollowTransfer(event.target.value || null);
                }}
              >
                <option value="">Start another transfer</option>
                {relatedTransfers.map((item) => (
                  <option key={item.transferId} value={item.transferId}>
                    {item.attempt?.target.name ?? "No target selected"} · {transferStateLabel(item.state)} · {item.transferId.slice(0, 8)}
                  </option>
                ))}
              </select>
            </section>
          )}
          {visibleError && (
            <div className="peer-composer__error" role="alert">
              {visibleError}
              {error && (
                <button type="button" onClick={onClearError}>
                  Dismiss
                </button>
              )}
            </div>
          )}

          {transfer && (
            <p className="transfer-composer__state" role="status" aria-live="polite">
              <strong>{transferStateLabel(transfer.state)}</strong>
              {errorLabel && <span> {errorLabel}</span>}
              {transfer.attemptsUsed > 0 && (
                <small> Target attempts used: {transfer.attemptsUsed} of 8.</small>
              )}
            </p>
          )}

          {!transfer && (
            <form className="peer-instruction-form" onSubmit={submitCreate}>
              <fieldset className="peer-actions" disabled={pending}>
                <legend>How should the handoff be written?</legend>
                <div className="peer-actions__grid">
                  <label>
                    <input
                      type="radio"
                      name="transfer-mode"
                      value="prepare"
                      data-transfer-initial-focus={sourceRunning ? "true" : undefined}
                      checked={mode === "prepare"}
                      disabled={!sourceRunning}
                      onChange={() => setMode("prepare")}
                    />
                    <span>
                      <strong>Ask the source agent</strong>
                      <small>
                        The source writes the handoff from its own context and
                        submits it through the local bridge. Needs the source at
                        an empty prompt.
                      </small>
                    </span>
                  </label>
                  <label>
                    <input
                      type="radio"
                      name="transfer-mode"
                      value="manual"
                      data-transfer-initial-focus={sourceRunning ? undefined : "true"}
                      checked={mode === "manual"}
                      onChange={() => setMode("manual")}
                    />
                    <span>
                      <strong>Write it myself</strong>
                      <small>
                        Paste or type the handoff. Works even when the source
                        can no longer respond.
                      </small>
                    </span>
                  </label>
                </div>
              </fieldset>
              <label htmlFor="transfer-instruction">
                What should the next agent focus on? (optional)
              </label>
              <textarea
                id="transfer-instruction"
                value={instruction}
                maxLength={MAX_INSTRUCTION_BYTES}
                placeholder="Continue the refactor; the tests in web/src still fail."
                spellCheck
                disabled={pending}
                onChange={(event) => setInstruction(event.target.value)}
              />
              <div className="peer-composer__actions">
                <button type="button" onClick={requestClose}>
                  Close
                </button>
                <button
                  type="submit"
                  className="peer-primary-action"
                  disabled={pending || (mode === "prepare" && !sourceRunning)}
                >
                  {pending
                    ? "Starting…"
                    : mode === "prepare"
                      ? "Ask the source to prepare"
                      : "Write the handoff"}
                </button>
              </div>
            </form>
          )}

          {transfer && state === "preparing" && (
            <section className="peer-progress" role="status" aria-live="polite">
              <span className="peer-progress__spinner" aria-hidden="true" />
              <p>
                Waiting for the source agent to submit the handoff. You can
                take over and write it yourself at any time; a late submission
                from the source is then ignored.
              </p>
            </section>
          )}

          {transfer && draftEditable(transfer) && (
            <section className="peer-preview">
              <label htmlFor="transfer-handoff">Handoff</label>
              <textarea
                id="transfer-handoff"
                value={handoffDraft}
                maxLength={MAX_HANDOFF_BYTES}
                spellCheck
                disabled={pending}
                placeholder={
                  state === "preparing"
                    ? "Type here to take over from the source agent."
                    : "Write the handoff for the next agent."
                }
                onChange={(event) => setHandoffDraft(event.target.value)}
              />
              <div className="peer-composer__actions">
                {!handoffDraft.trim() && (
                  <button
                    type="button"
                    disabled={pending}
                    onClick={() => setHandoffDraft(MANUAL_TEMPLATE)}
                  >
                    Insert template
                  </button>
                )}
                <button
                  type="button"
                  className={draftDirty ? "peer-primary-action" : undefined}
                  disabled={pending || !draftReady || !draftDirty}
                  onClick={saveDraft}
                >
                  {pending && operation === "draft"
                    ? "Saving…"
                    : state === "preparing"
                      ? "Take over and save"
                      : "Save handoff"}
                </button>
              </div>
            </section>
          )}

          {transfer && state === "preview" && (
            <section className="transfer-target">
              <div className="peer-reviewer-folder">
                <div>
                  <span>Target folder (new terminal)</span>
                  <code title={targetDirectory.path}>{targetDirectory.path}</code>
                  <small>Defaults to the source tab folder.</small>
                </div>
                <button
                  ref={folderButtonRef}
                  type="button"
                  disabled={pending || targetChoice?.kind !== "new"}
                  onClick={() => setWorkspacePickerOpen(true)}
                >
                  Change folder
                </button>
              </div>
              <fieldset className="peer-targets transfer-targets" disabled={pending}>
                <legend>Continue in</legend>
                {readyAgents.map((agent) => (
                  <label key={`new-${agent}`}>
                    <input
                      type="radio"
                      name="transfer-target"
                      value={`new-${agent}`}
                      disabled={newTargetDisabledReason !== null}
                      checked={
                        targetChoice?.kind === "new" && targetChoice.agent === agent
                      }
                      onChange={() => setTargetChoice({ kind: "new", agent })}
                    />
                    <span>New {AGENT_LABELS[agent]} tab</span>
                  </label>
                ))}
                {existingTargets.map((candidate) => (
                  <label key={`existing-${candidate.terminalId}`}>
                    <input
                      type="radio"
                      name="transfer-target"
                      value={`existing-${candidate.terminalId}`}
                      checked={
                        targetChoice?.kind === "existing" &&
                        targetChoice.terminalId === candidate.terminalId
                      }
                      onChange={() =>
                        setTargetChoice({
                          kind: "existing",
                          terminalId: candidate.terminalId,
                        })
                      }
                    />
                    <span title={candidate.project}>
                      Existing tab: {candidate.name}
                    </span>
                  </label>
                ))}
                {readyAgents.length === 0 && existingTargets.length === 0 && (
                  <p role="status">
                    {catalogError ??
                      (catalogLoading
                        ? "Checking installed agents…"
                        : "No ready agent and no other running tab is available.")}
                  </p>
                )}
              </fieldset>
              {newTargetDisabledReason && (
                <p className="peer-composer__readiness" role="status">
                  {newTargetDisabledReason} Existing running tabs can still be chosen.
                </p>
              )}
              {targetChoice?.kind === "existing" && (
                <p className="peer-instruction-form__hint">
                  The handoff joins that tab's existing conversation; its agent
                  decides how to use it.
                </p>
              )}
              <div className="peer-composer__actions">
                <button type="button" disabled={pending} onClick={abandon}>
                  Abandon
                </button>
                <button
                  type="button"
                  className="peer-primary-action"
                  disabled={
                    pending ||
                    !draftReady ||
                    !targetChoice ||
                    draftDirty ||
                    !handoffDraft.trim() ||
                    (targetChoice.kind === "new" &&
                      (newTargetDisabledReason !== null || !targetDirectory.id))
                  }
                  onClick={chooseTarget}
                >
                  {pending && operation === "target"
                    ? "Choosing…"
                    : targetChoice?.kind === "new"
                      ? "Start the new tab"
                      : "Use this tab"}
                </button>
              </div>
            </section>
          )}

          {transfer && state === "starting" && (
            <section className="peer-progress" role="status" aria-live="polite">
              <span className="peer-progress__spinner" aria-hidden="true" />
              <p>Starting the target terminal…</p>
            </section>
          )}

          {transfer && state === "cleanup_pending" && target && (
            <section className="transfer-target">
              <p>
                The new terminal <strong>{target.name}</strong> did not start
                and its process could not be confirmed stopped. It stays listed
                until cleanup succeeds.
              </p>
              <div className="peer-composer__actions">
                <button type="button" disabled={pending} onClick={abandon}>
                  Abandon
                </button>
                <button
                  type="button"
                  className="peer-primary-action"
                  disabled={pending}
                  onClick={cleanup}
                >
                  {pending && operation === "cleanup" ? "Cleaning up…" : "Retry cleanup"}
                </button>
              </div>
            </section>
          )}

          {transfer && state === "target_bound" && target && (
            <section className="transfer-target">
              <p>
                Target: <strong>{target.name}</strong> ({AGENT_LABELS[target.agent]}){" "}
                <code title={target.project}>{target.project}</code>
              </p>
              <p className="peer-instruction-form__hint">
                Delivering needs the target tab at an idle, empty prompt.
                Finish sign-in or folder trust there first.
              </p>
              <div className="peer-composer__actions">
                <button type="button" disabled={pending} onClick={abandon}>
                  Abandon
                </button>
                <button
                  type="button"
                  disabled={pending}
                  onClick={() => onSelectTerminal(target.terminalId)}
                >
                  Show target tab
                </button>
                <button
                  type="button"
                  className="peer-primary-action"
                  data-transfer-deliver="true"
                  disabled={pending || !draftReady || draftDirty || !target.sessionId}
                  onClick={deliver}
                >
                  {pending && operation === "deliver" ? "Delivering…" : "Deliver"}
                </button>
              </div>
            </section>
          )}

          {transfer &&
            (state === "delivering" || state === "awaiting_receipt") &&
            target && (
              <section className="peer-progress" role="status" aria-live="polite">
                <span className="peer-progress__spinner" aria-hidden="true" />
                <p>
                  The prompt was sent to <strong>{target.name}</strong>. Waiting
                  for its agent to retrieve the handoff through the local bridge
                  and confirm the receipt. Nothing is resent automatically.
                </p>
                <div className="peer-composer__actions">
                  <button type="button" disabled={pending} onClick={abandon}>
                    Discard
                  </button>
                  <button
                    type="button"
                    disabled={pending}
                    onClick={() => onSelectTerminal(target.terminalId)}
                  >
                    Show target tab
                  </button>
                </div>
              </section>
            )}

          {transfer && state === "delivered" && target && (
            <section className="transfer-target">
              <p>
                <strong>{target.name}</strong> confirmed the handoff. Continue
                there; the source tab stays open until you close it.
              </p>
              <div className="peer-composer__actions">
                <button type="button" onClick={onClose}>
                  Close
                </button>
                <button
                  type="button"
                  className="peer-primary-action"
                  onClick={() => {
                    onSelectTerminal(target.terminalId);
                    onClose();
                  }}
                >
                  Go to {target.name}
                </button>
              </div>
            </section>
          )}

          {transfer && (state === "failed" || state === "abandoned") && (
            <div className="peer-composer__actions">
              <button type="button" className="peer-primary-action" onClick={onClose}>
                Close
              </button>
            </div>
          )}
        </div>
      </section>
    </div>
  );
}

function utf8Length(value: string): number {
  return new TextEncoder().encode(value).byteLength;
}

function directoryForSession(session: SessionSnapshot): WorkspaceDirectory {
  return {
    id: session.directoryId,
    name: session.name,
    path: session.project,
  };
}
