import {
  useEffect,
  useMemo,
  useRef,
  useState,
  type FormEvent,
  type KeyboardEvent,
} from "react";
import type {
  AgentCatalog,
  AgentKind,
  SessionSnapshot,
} from "../api";
import { AGENT_LABELS } from "../agents";
import {
  actionsForThread,
  peerStatusLabel,
  peerThreadDisplayId,
} from "./actions";
import { WorkspacePicker } from "../workspaces/WorkspacePicker";
import type {
  WorkspaceBrowserAdapter,
  WorkspaceDirectory,
} from "../workspaces/types";
import {
  confirmHandoffDiscard,
  emptyHandoffDraft,
  handoffDraftForTurn,
  resolveInitialThreadSelection,
  synchronizeHandoffDraft,
} from "./composerState";
import type {
  CreatePeerThreadInput,
  CreatePeerTurnInput,
  DispatchPeerTurnInput,
  PeerAction,
  PeerThread,
  ReturnPeerTurnInput,
} from "./types";
import type { PeerOperation } from "./usePeerController";

const MAX_INSTRUCTION_BYTES = 4 * 1_024;
const MAX_HANDOFF_BYTES = 64 * 1_024;

interface PeerComposerProps {
  sourceSession: SessionSnapshot;
  workspaceAdapter: WorkspaceBrowserAdapter;
  initialThreadId?: string | null;
  allowNew: boolean;
  newThreadDisabledReason?: string | null;
  catalog: AgentCatalog | null;
  catalogLoading: boolean;
  catalogError: string | null;
  threads: PeerThread[];
  threadsReady: boolean;
  threadsLoading: boolean;
  operation: PeerOperation | null;
  error: string | null;
  onCreateThread: (input: CreatePeerThreadInput) => Promise<PeerThread>;
  onCreateTurn: (
    threadId: string,
    input: CreatePeerTurnInput,
  ) => Promise<PeerThread>;
  onDispatchTurn: (
    threadId: string,
    input: DispatchPeerTurnInput,
  ) => Promise<PeerThread>;
  onReturnTurn: (
    threadId: string,
    input: ReturnPeerTurnInput,
  ) => Promise<PeerThread>;
  onRefreshThreads: () => Promise<void>;
  onClearError: () => void;
  onClose: () => void;
}

export function PeerComposer({
  sourceSession,
  workspaceAdapter,
  initialThreadId = null,
  allowNew,
  newThreadDisabledReason = null,
  catalog,
  catalogLoading,
  catalogError,
  threads,
  threadsReady,
  threadsLoading,
  operation,
  error,
  onCreateThread,
  onCreateTurn,
  onDispatchTurn,
  onReturnTurn,
  onRefreshThreads,
  onClearError,
  onClose,
}: PeerComposerProps) {
  const initialThread =
    threads.find((thread) => thread.id === initialThreadId) ?? null;
  const selectionScope = `${sourceSession.terminalId}:${initialThreadId ?? ""}`;
  const dialogRef = useRef<HTMLElement>(null);
  const [initializedSelectionScope, setInitializedSelectionScope] = useState<
    string | null
  >(() =>
    initialThread?.sourceTerminalId === sourceSession.terminalId &&
    initialThread.status !== "closed"
      ? selectionScope
      : null,
  );
  const [selectedThreadId, setSelectedThreadId] = useState<string | null>(
    initialThreadId,
  );
  const [action, setAction] = useState<PeerAction>(
    initialThreadId ? "recheck" : "review",
  );
  const [targetAgent, setTargetAgent] = useState<AgentKind>("claude");
  const [reviewerDirectory, setReviewerDirectory] =
    useState<WorkspaceDirectory>(() => directoryForSession(sourceSession));
  const [workspacePickerOpen, setWorkspacePickerOpen] = useState(false);
  const [instruction, setInstruction] = useState("");
  const [handoffDraftState, setHandoffDraftState] = useState(() =>
    handoffDraftForTurn(initialThread?.currentTurn),
  );
  const [formError, setFormError] = useState<string | null>(null);

  const activeThreads = useMemo(
    () =>
      threads
        .filter(
          (thread) =>
            thread.sourceTerminalId === sourceSession.terminalId &&
            thread.status !== "closed",
        )
        .sort(
          (left, right) =>
            right.updatedAt - left.updatedAt ||
            left.id.localeCompare(right.id),
        ),
    [sourceSession.terminalId, threads],
  );
  const selectedThread =
    activeThreads.find((thread) => thread.id === selectedThreadId) ?? null;
  const readyAgents = useMemo(
    () =>
      (catalog?.agents ?? [])
        .filter((agent) => agent.state === "ready")
        .map((agent) => agent.kind),
    [catalog],
  );

  useEffect(() => {
    setInitializedSelectionScope((current) =>
      current === selectionScope ? current : null,
    );
    setSelectedThreadId(initialThreadId);
    setAction(initialThreadId ? "recheck" : "review");
    setReviewerDirectory(directoryForSession(sourceSession));
    setWorkspacePickerOpen(false);
    setInstruction("");
    setHandoffDraftState(emptyHandoffDraft());
    setFormError(null);
  }, [
    initialThreadId,
    selectionScope,
    sourceSession.directoryId,
    sourceSession.name,
    sourceSession.project,
  ]);

  useEffect(() => {
    if (
      !threadsReady ||
      initializedSelectionScope === selectionScope
    ) {
      return;
    }
    const selection = resolveInitialThreadSelection(
      initialThreadId,
      activeThreads,
    );
    if (selection.kind === "waiting") {
      return;
    }
    setInitializedSelectionScope(selectionScope);
    setSelectedThreadId(
      selection.kind === "selected" ? selection.threadId : null,
    );
    setAction(selection.kind === "selected" ? "recheck" : "review");
  }, [
    activeThreads,
    initialThreadId,
    initializedSelectionScope,
    selectionScope,
    threadsReady,
  ]);

  useEffect(() => {
    if (readyAgents.includes(targetAgent)) {
      return;
    }
    setTargetAgent(readyAgents[0] ?? "claude");
  }, [readyAgents, targetAgent]);

  useEffect(() => {
    setHandoffDraftState((current) =>
      synchronizeHandoffDraft(current, selectedThread?.currentTurn),
    );
  }, [selectedThread]);

  useEffect(() => {
    const frame = window.requestAnimationFrame(() => {
      const preferred =
        dialogRef.current?.querySelector<HTMLElement>(
          "[data-peer-initial-focus]:not([disabled])",
        ) ??
        dialogRef.current?.querySelector<HTMLElement>(
          "button:not([disabled]), textarea:not([disabled])",
        );
      (preferred ?? dialogRef.current)?.focus({ preventScroll: true });
    });
    return () => window.cancelAnimationFrame(frame);
  }, []);

  const pending = operation !== null;
  const waitingForInitialThread =
    initialThreadId !== null &&
    initializedSelectionScope !== selectionScope;
  const handoffDraft = handoffDraftState.draft;
  const previewHandoff =
    selectedThread?.currentTurn.status === "awaiting_preview"
      ? (selectedThread.currentTurn.handoff ?? "")
      : null;
  const previewDirty =
    previewHandoff !== null && handoffDraft !== previewHandoff;
  const availableActions = actionsForThread(selectedThread);
  const freshThreadBlocked =
    !selectedThread && Boolean(newThreadDisabledReason);

  const confirmAndDiscardPreview = () => {
    const discarded = confirmHandoffDiscard(
      handoffDraftState,
      previewHandoff,
      () =>
        window.confirm(
          "Discard the unsent changes to this handoff preview?",
        ),
    );
    if (!discarded) {
      return false;
    }
    setHandoffDraftState(discarded);
    return true;
  };

  const requestClose = () => {
    if (!pending && confirmAndDiscardPreview()) {
      onClose();
    }
  };

  const chooseThread = (thread: PeerThread) => {
    if (thread.id === selectedThread?.id) {
      return;
    }
    if (!confirmAndDiscardPreview()) {
      return;
    }
    setInitializedSelectionScope(selectionScope);
    setSelectedThreadId(thread.id);
    setAction("recheck");
    setInstruction("");
    setFormError(null);
    onClearError();
  };

  const chooseNew = () => {
    if (
      !selectedThread ||
      newThreadDisabledReason ||
      !confirmAndDiscardPreview()
    ) {
      return;
    }
    setInitializedSelectionScope(selectionScope);
    setSelectedThreadId(null);
    setAction("review");
    setReviewerDirectory(directoryForSession(sourceSession));
    setInstruction("");
    setFormError(null);
    onClearError();
  };

  const submitInstruction = async (event: FormEvent) => {
    event.preventDefault();
    const nextInstruction = instruction.trim();
    if (!nextInstruction) {
      setFormError("Add the question or task for the other agent.");
      return;
    }
    if (!selectedThread && newThreadDisabledReason) {
      setFormError(newThreadDisabledReason);
      return;
    }
    if (utf8Length(nextInstruction) > MAX_INSTRUCTION_BYTES) {
      setFormError("The instruction is too long.");
      return;
    }
    if (!selectedThread && action === "recheck") {
      setFormError("Recheck requires an existing peer conversation.");
      return;
    }
    setFormError(null);
    onClearError();

    try {
      const next = selectedThread
          ? await onCreateTurn(selectedThread.id, {
              action,
              instruction: nextInstruction,
              sourceReady: true,
            })
          : await onCreateThread({
              sourceTerminalId: sourceSession.terminalId,
              directoryId: reviewerDirectory.id,
              targetAgent,
              action: action as Exclude<PeerAction, "recheck">,
              instruction: nextInstruction,
              sourceReady: true,
            });
      setInitializedSelectionScope(selectionScope);
      setSelectedThreadId(next.id);
      setInstruction("");
    } catch {
      // The controller exposes the sanitized API error in the dialog.
    }
  };

  const dispatch = async () => {
    if (!selectedThread) {
      return;
    }
    if (!handoffDraft.trim()) {
      setFormError("The handoff cannot be empty.");
      return;
    }
    if (utf8Length(handoffDraft) > MAX_HANDOFF_BYTES) {
      setFormError("The handoff is too long.");
      return;
    }
    setFormError(null);
    onClearError();
    try {
      await onDispatchTurn(selectedThread.id, {
        turnId: selectedThread.currentTurn.id,
        handoffRevision: selectedThread.currentTurn.handoffRevision,
        handoff: handoffDraft,
        reviewerReady: true,
      });
    } catch {
      // The controller exposes the sanitized API error in the dialog.
    }
  };

  const returnResponse = async () => {
    if (
      !selectedThread ||
      selectedThread.currentTurn.status !== "response_ready"
    ) {
      return;
    }
    setFormError(null);
    onClearError();
    try {
      await onReturnTurn(selectedThread.id, {
        turnId: selectedThread.currentTurn.id,
        sourceReady: true,
      });
    } catch {
      // The controller exposes the sanitized API error in the dialog.
    }
  };

  const handleDialogKeyDown = (event: KeyboardEvent<HTMLElement>) => {
    if (event.key === "Escape" && !pending) {
      event.preventDefault();
      requestClose();
      return;
    }
    if (event.key !== "Tab") {
      return;
    }
    const focusable = Array.from(
      dialogRef.current?.querySelectorAll<HTMLElement>(
        'button:not([disabled]), textarea:not([disabled]), [tabindex]:not([tabindex="-1"])',
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
          initialDirectoryId={reviewerDirectory.id}
          title="Choose reviewer folder"
          description="Start the dedicated reviewer in this server folder."
          chooseLabel="Use for reviewer"
          disabled={pending}
          onChoose={(directory, transition) => {
            transition.suppressFocusReturn();
            setReviewerDirectory(directory);
            setWorkspacePickerOpen(false);
          }}
          onCancel={() => setWorkspacePickerOpen(false)}
        />
      </div>
    );
  }

  return (
    <div
      className="dialog-backdrop peer-composer-backdrop"
      role="presentation"
      onMouseDown={() => {
        requestClose();
      }}
    >
      <section
        ref={dialogRef}
        className="peer-composer"
        role="dialog"
        aria-modal="true"
        aria-labelledby="peer-composer-title"
        aria-describedby="peer-composer-description"
        aria-busy={pending || (!threadsReady && threadsLoading)}
        tabIndex={-1}
        onKeyDown={handleDialogKeyDown}
        onMouseDown={(event) => event.stopPropagation()}
      >
        <header className="peer-composer__header">
          <div>
            <div className="peer-composer__eyebrow">@cwt</div>
            <h2 id="peer-composer-title">Ask another agent</h2>
            <p id="peer-composer-description">
              A dedicated reviewer keeps its own context. Existing terminal
              conversations are never reused.
            </p>
          </div>
          <button
            type="button"
            className="peer-composer__close"
            aria-label="Close peer composer"
            disabled={pending}
            onClick={requestClose}
          >
            ×
          </button>
        </header>

        <div className="peer-composer__source">
          <span>Source</span>
          <strong>{sourceSession.name}</strong>
          <code title={sourceSession.project}>{sourceSession.project}</code>
        </div>
        <p className="peer-composer__readiness">
          Before a readiness action, make sure the named terminal is showing an
          empty agent prompt. Catalog Ready checks the executable and version;
          a fresh terminal may still require sign-in or folder trust. @cwt never
          accepts those prompts for you.
        </p>

        {threadsReady &&
          !waitingForInitialThread &&
          (activeThreads.length > 0 || allowNew) && (
          <div
            className="peer-thread-picker"
            role="tablist"
            aria-label="Peer conversations"
          >
            {activeThreads.map((thread) => {
              const selected = thread.id === selectedThread?.id;
              return (
                <button
                  key={thread.id}
                  type="button"
                  role="tab"
                  aria-selected={selected}
                  className={
                    selected
                      ? "peer-thread-picker__item peer-thread-picker__item--active"
                      : "peer-thread-picker__item"
                  }
                  disabled={pending}
                  onClick={() => chooseThread(thread)}
                >
                  {AGENT_LABELS[thread.targetAgent]}{" "}
                  {peerThreadDisplayId(thread.id)}
                  <small>{peerStatusLabel(thread.status)}</small>
                </button>
              );
            })}
            {allowNew && (
              <button
                type="button"
                role="tab"
                aria-selected={!selectedThread}
                className={
                  selectedThread
                    ? "peer-thread-picker__item peer-thread-picker__new"
                    : "peer-thread-picker__item peer-thread-picker__item--active peer-thread-picker__new"
                }
                disabled={pending || Boolean(newThreadDisabledReason)}
                title={newThreadDisabledReason ?? undefined}
                onClick={chooseNew}
              >
                + New peer
                <small>Clean context</small>
              </button>
            )}
          </div>
        )}

        <div className="peer-composer__body">
          {(error || formError || catalogError) && threadsReady && (
            <div className="peer-composer__error" role="alert">
              {formError ?? error ?? catalogError}
            </div>
          )}

          {!threadsReady ? (
            <section className="peer-progress" role="status" aria-live="polite">
              {threadsLoading && (
                <span className="peer-progress__spinner" aria-hidden="true" />
              )}
              <div>
                <strong>
                  {threadsLoading
                    ? "Loading peer conversations"
                    : "Peer conversations are unavailable"}
                </strong>
                <p>
                  {error ??
                    "The existing peer conversations must be known before a new reviewer can be created."}
                </p>
                {!threadsLoading && (
                  <div className="peer-composer__actions">
                    <button
                      type="button"
                      disabled={pending}
                      onClick={() => void onRefreshThreads()}
                    >
                      Retry
                    </button>
                    <button type="button" disabled={pending} onClick={requestClose}>
                      Close
                    </button>
                  </div>
                )}
              </div>
            </section>
          ) : waitingForInitialThread ? (
            <section className="peer-progress" role="status" aria-live="polite">
              {threadsLoading && (
                <span className="peer-progress__spinner" aria-hidden="true" />
              )}
              <div>
                <strong>
                  {threadsLoading
                    ? "Loading selected peer conversation"
                    : "Selected peer conversation unavailable"}
                </strong>
                <p>
                  The requested dedicated thread is not present in the current
                  snapshot. Wait for the next refresh or retry without
                  selecting another conversation.
                </p>
                {!threadsLoading && (
                  <div className="peer-composer__actions">
                    <button
                      type="button"
                      disabled={pending}
                      onClick={() => void onRefreshThreads()}
                    >
                      Retry
                    </button>
                    <button type="button" disabled={pending} onClick={requestClose}>
                      Close
                    </button>
                  </div>
                )}
              </div>
            </section>
          ) : selectedThread?.currentTurn.status === "awaiting_preview" ? (
            <section className="peer-preview" aria-labelledby="peer-preview-title">
              <div className="peer-section-heading">
                <div>
                  <h3 id="peer-preview-title">Preview handoff</h3>
                  <p>
                    Review or edit the summary before it reaches the dedicated{" "}
                    {AGENT_LABELS[selectedThread.targetAgent]} session.
                  </p>
                </div>
                <span>{peerThreadDisplayId(selectedThread.id)}</span>
              </div>
              <label htmlFor="peer-handoff">Prepared context</label>
              <textarea
                id="peer-handoff"
                value={handoffDraft}
                maxLength={MAX_HANDOFF_BYTES}
                spellCheck
                disabled={pending}
                onChange={(event) =>
                  setHandoffDraftState((current) => ({
                    ...current,
                    draft: event.target.value,
                  }))
                }
              />
              <div className="peer-composer__actions">
                <button type="button" disabled={pending} onClick={requestClose}>
                  {previewDirty ? "Discard edits" : "Keep for later"}
                </button>
                <button
                  type="button"
                  className="peer-primary-action"
                  disabled={pending || !handoffDraft.trim()}
                  onClick={() => void dispatch()}
                >
                  {pending ? "Sending…" : "Reviewer ready — Send"}
                </button>
              </div>
            </section>
          ) : selectedThread &&
            (selectedThread.currentTurn.status === "preparing_handoff" ||
              selectedThread.currentTurn.status === "reviewing") ? (
            <section className="peer-progress" role="status" aria-live="polite">
              <span className="peer-progress__spinner" aria-hidden="true" />
              <div>
                <strong>
                  {peerStatusLabel(selectedThread.currentTurn.status)}
                </strong>
                <p>
                  {selectedThread.currentTurn.status === "preparing_handoff"
                    ? `${sourceSession.name} is preparing a concise handoff for preview.`
                    : `${AGENT_LABELS[selectedThread.targetAgent]} is reviewing the approved handoff.`}
                </p>
                <p>
                  {selectedThread.currentTurn.status === "preparing_handoff"
                    ? `The reviewer tab may be prepared separately. If this does not advance, close this panel and inspect ${sourceSession.name} for an approval or an unsent prompt.`
                    : `If this does not advance, inspect the dedicated ${AGENT_LABELS[selectedThread.targetAgent]} tab for an approval, sign-in, or trust prompt.`}
                </p>
              </div>
            </section>
          ) : !allowNew && !selectedThread ? (
            <section className="peer-progress" role="status" aria-live="polite">
              <div>
                <strong>Peer conversation unavailable</strong>
                <p>
                  The dedicated thread is still loading or has already been
                  closed. Return to its source tab to start another review.
                </p>
              </div>
            </section>
          ) : (
            <>
              {selectedThread?.currentTurn.response && (
                <section
                  className="peer-response"
                  aria-labelledby="peer-response-title"
                >
                  <div className="peer-section-heading">
                    <div>
                      <h3 id="peer-response-title">Reviewer response</h3>
                      <p>
                        {AGENT_LABELS[selectedThread.targetAgent]} ·{" "}
                        {peerThreadDisplayId(selectedThread.id)}
                      </p>
                    </div>
                    <span>{peerStatusLabel(selectedThread.status)}</span>
                  </div>
                  <pre>{selectedThread.currentTurn.response}</pre>
                </section>
              )}

              {selectedThread?.currentTurn.error && (
                <div className="peer-composer__error" role="alert">
                  {selectedThread.currentTurn.error}
                </div>
              )}

              {selectedThread?.currentTurn.status === "response_ready" ? (
                <div className="peer-composer__actions">
                  <button type="button" disabled={pending} onClick={requestClose}>
                    Keep for later
                  </button>
                  <button
                    type="button"
                    className="peer-primary-action"
                    disabled={
                      pending || !selectedThread.currentTurn.response?.trim()
                    }
                    onClick={() => void returnResponse()}
                  >
                    {pending ? "Returning…" : "Source ready — Return"}
                  </button>
                </div>
              ) : (
                <form
                  className="peer-instruction-form"
                  onSubmit={(event) => void submitInstruction(event)}
                >
                {freshThreadBlocked && (
                  <p className="peer-composer__readiness" role="status">
                    {newThreadDisabledReason}
                  </p>
                )}
                {!selectedThread && (
                  <div className="peer-reviewer-folder">
                    <div>
                      <span>Reviewer folder</span>
                      <code title={reviewerDirectory.path}>
                        {reviewerDirectory.path}
                      </code>
                      <small>
                        Defaults to the source tab folder. Choose another
                        project when the review belongs elsewhere.
                      </small>
                    </div>
                    <button
                      type="button"
                      disabled={pending || freshThreadBlocked}
                      onClick={() => setWorkspacePickerOpen(true)}
                    >
                      Change folder
                    </button>
                  </div>
                )}
                {!selectedThread && (
                  <fieldset
                    className="peer-targets"
                    disabled={pending || catalogLoading || freshThreadBlocked}
                  >
                    <legend>Dedicated reviewer</legend>
                    {readyAgents.length > 0 ? (
                      readyAgents.map((agent) => (
                        <label key={agent}>
                          <input
                            type="radio"
                            name="peer-target"
                            value={agent}
                            checked={targetAgent === agent}
                            onChange={() => setTargetAgent(agent)}
                          />
                          <span>{AGENT_LABELS[agent]}</span>
                        </label>
                      ))
                    ) : (
                      <p role="status">
                        {catalogError ??
                          (catalogLoading
                          ? "Checking installed agents…"
                          : "No installed agent is ready for a dedicated review.")}
                      </p>
                    )}
                  </fieldset>
                )}

                <fieldset
                  className="peer-actions"
                  disabled={pending || freshThreadBlocked}
                >
                  <legend>Action</legend>
                  <div className="peer-actions__grid">
                    {availableActions.map((descriptor, index) => (
                      <label key={descriptor.action}>
                        <input
                          type="radio"
                          name="peer-action"
                          value={descriptor.action}
                          data-peer-initial-focus={
                            index === 0 && !selectedThread ? "true" : undefined
                          }
                          checked={action === descriptor.action}
                          onChange={() => setAction(descriptor.action)}
                        />
                        <span>
                          <strong>{descriptor.label}</strong>
                          <small>{descriptor.description}</small>
                        </span>
                      </label>
                    ))}
                  </div>
                </fieldset>

                <label htmlFor="peer-instruction">
                  What should the other agent do?
                </label>
                <textarea
                  id="peer-instruction"
                  value={instruction}
                  maxLength={MAX_INSTRUCTION_BYTES}
                  placeholder={
                    action === "recheck"
                      ? "Explain what changed and what should be checked again."
                      : "Describe the question, review scope, or result to verify."
                  }
                  spellCheck
                  disabled={pending || freshThreadBlocked}
                  onChange={(event) => setInstruction(event.target.value)}
                />
                <p className="peer-instruction-form__hint">
                  Enter adds a new line. Use the button below to submit exactly
                  once.
                </p>

                <div className="peer-composer__actions">
                  <button type="button" disabled={pending} onClick={requestClose}>
                    Close
                  </button>
                  <button
                    type="submit"
                    className="peer-primary-action"
                    disabled={
                      pending ||
                      !instruction.trim() ||
                      freshThreadBlocked ||
                      (!selectedThread &&
                        (catalogLoading || !readyAgents.includes(targetAgent)))
                    }
                  >
                    {pending
                      ? "Preparing…"
                      : selectedThread
                        ? "Source ready — Prepare follow-up"
                        : "Source ready — Prepare handoff"}
                  </button>
                </div>
                </form>
              )}
            </>
          )}
        </div>
      </section>
    </div>
  );
}

function utf8Length(value: string): number {
  return new TextEncoder().encode(value).byteLength;
}

function directoryForSession(
  session: SessionSnapshot,
): WorkspaceDirectory {
  return {
    id: session.directoryId,
    name: session.name,
    path: session.project,
  };
}
