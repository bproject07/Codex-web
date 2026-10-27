import { useCallback, useEffect, useRef, useState } from "react";
import {
  abandonTransfer,
  chooseTransferTarget,
  cleanupTransfer,
  createTransfer,
  deliverTransfer,
  getTransfer,
  getTransferDraft,
  putTransferDraft,
} from "./api";
import {
  isTransferActive,
  type ChooseTargetInput,
  type CleanupTransferInput,
  type CreateTransferInput,
  type DeliverTransferInput,
  type TransferDraft,
  type TransferSnapshot,
} from "./types";

const ACTIVE_POLL_MS = 1_200;
const IDLE_POLL_MS = 5_000;

export type TransferOperation =
  | "create"
  | "draft"
  | "target"
  | "deliver"
  | "cleanup"
  | "abandon";

export interface TransferController {
  /** The transfer this controller follows, or null before creation. */
  transfer: TransferSnapshot | null;
  /** Server draft as last read; revision-keyed so a newer draft resets edits. */
  draft: TransferDraft | null;
  operation: TransferOperation | null;
  error: string | null;
  /** Start following an existing transfer (e.g. reopened composer). */
  follow: (transferId: string | null) => void;
  create: (input: CreateTransferInput) => Promise<TransferSnapshot>;
  saveDraft: (expectedRevision: number, content: string) => Promise<TransferSnapshot>;
  chooseTarget: (input: ChooseTargetInput) => Promise<TransferSnapshot>;
  deliver: (input: DeliverTransferInput) => Promise<TransferSnapshot>;
  cleanup: (input: CleanupTransferInput) => Promise<TransferSnapshot>;
  abandon: () => Promise<TransferSnapshot>;
  refresh: () => Promise<void>;
  clearError: () => void;
}

/**
 * Follows one transfer by id: polls its ledger snapshot with a monotonic
 * revision (older replies never replace newer state), reads the draft when
 * its revision changes, and serializes mutations.
 */
export function useTransferController(token: string): TransferController {
  const [transferId, setTransferId] = useState<string | null>(null);
  const [transfer, setTransfer] = useState<TransferSnapshot | null>(null);
  const [draft, setDraft] = useState<TransferDraft | null>(null);
  const [operation, setOperation] = useState<TransferOperation | null>(null);
  const [error, setError] = useState<string | null>(null);
  const transferRef = useRef<TransferSnapshot | null>(null);
  const draftRef = useRef<TransferDraft | null>(null);
  const operationRef = useRef<TransferOperation | null>(null);
  const requestEpochRef = useRef(0);
  const errorOriginRef = useRef<"read" | "operation" | null>(null);
  const wakePollRef = useRef<() => void>(() => undefined);

  const applySnapshot = useCallback((next: TransferSnapshot) => {
    const current = transferRef.current;
    if (
      current &&
      current.transferId === next.transferId &&
      current.revision > next.revision
    ) {
      // An older reply must never overwrite newer state.
      return;
    }
    transferRef.current = next;
    setTransfer(next);
    if (!isTransferActive(next.state)) {
      draftRef.current = null;
      setDraft(null);
    }
  }, []);

  const clearError = useCallback(() => {
    errorOriginRef.current = null;
    setError(null);
  }, []);

  const follow = useCallback(
    (nextId: string | null) => {
      requestEpochRef.current += 1;
      transferRef.current = null;
      draftRef.current = null;
      setTransfer(null);
      setDraft(null);
      setTransferId(nextId);
      clearError();
    },
    [clearError],
  );

  const readDraftIfChanged = useCallback(
    async (snapshot: TransferSnapshot, signal?: AbortSignal) => {
      if (!isTransferActive(snapshot.state)) return;
      if (draftRef.current?.draftRevision === snapshot.draftRevision) return;
      const next = await getTransferDraft(token, snapshot.transferId, signal);
      draftRef.current = next;
      setDraft(next);
    },
    [token],
  );

  const refresh = useCallback(async () => {
    if (!token || !transferId || operationRef.current) return;
    const epoch = ++requestEpochRef.current;
    try {
      const next = await getTransfer(token, transferId);
      if (epoch !== requestEpochRef.current) return;
      applySnapshot(next);
      await readDraftIfChanged(next);
      if (errorOriginRef.current === "read") clearError();
    } catch (caught) {
      if (epoch === requestEpochRef.current && errorOriginRef.current !== "operation") {
        errorOriginRef.current = "read";
        setError(transferErrorMessage(caught));
      }
    }
  }, [applySnapshot, clearError, readDraftIfChanged, token, transferId]);

  useEffect(() => {
    if (!token || !transferId) {
      return;
    }
    let disposed = false;
    let timer: number | null = null;
    let controller: AbortController | null = null;

    const schedulePoll = (delay: number) => {
      if (disposed) return;
      if (timer !== null) window.clearTimeout(timer);
      timer = window.setTimeout(() => {
        timer = null;
        void poll();
      }, delay);
    };

    const poll = async () => {
      if (disposed) return;
      if (operationRef.current) {
        schedulePoll(ACTIVE_POLL_MS);
        return;
      }
      controller?.abort();
      const requestController = new AbortController();
      controller = requestController;
      const epoch = ++requestEpochRef.current;
      try {
        const next = await getTransfer(token, transferId, requestController.signal);
        if (
          !disposed &&
          !requestController.signal.aborted &&
          epoch === requestEpochRef.current &&
          !operationRef.current
        ) {
          applySnapshot(next);
          await readDraftIfChanged(next, requestController.signal);
          if (errorOriginRef.current === "read") clearError();
        }
      } catch (caught) {
        if (
          !disposed &&
          !requestController.signal.aborted &&
          epoch === requestEpochRef.current &&
          !operationRef.current &&
          errorOriginRef.current !== "operation"
        ) {
          errorOriginRef.current = "read";
          setError(transferErrorMessage(caught));
        }
      }
      if (disposed) return;
      const current = transferRef.current;
      const active = current ? isTransferActive(current.state) : true;
      const delay =
        document.visibilityState === "hidden" || !active
          ? IDLE_POLL_MS
          : ACTIVE_POLL_MS;
      schedulePoll(delay);
    };

    wakePollRef.current = () => schedulePoll(0);
    void poll();
    const refreshOnVisibility = () => {
      if (document.visibilityState === "visible") schedulePoll(0);
    };
    document.addEventListener("visibilitychange", refreshOnVisibility);
    return () => {
      disposed = true;
      requestEpochRef.current += 1;
      wakePollRef.current = () => undefined;
      controller?.abort();
      if (timer !== null) window.clearTimeout(timer);
      document.removeEventListener("visibilitychange", refreshOnVisibility);
    };
  }, [applySnapshot, clearError, readDraftIfChanged, token, transferId]);

  const mutate = useCallback(
    async (
      nextOperation: TransferOperation,
      request: () => Promise<TransferSnapshot>,
    ): Promise<TransferSnapshot> => {
      if (operationRef.current) {
        throw new Error("Another transfer operation is already in progress.");
      }
      const epoch = ++requestEpochRef.current;
      operationRef.current = nextOperation;
      setOperation(nextOperation);
      clearError();
      try {
        const next = await request();
        if (epoch === requestEpochRef.current) {
          if (transferRef.current?.transferId !== next.transferId) {
            transferRef.current = null;
            draftRef.current = null;
            setDraft(null);
            setTransferId(next.transferId);
          }
          applySnapshot(next);
          try {
            await readDraftIfChanged(next);
          } catch {
            // The snapshot already landed; the next poll re-reads the draft.
          }
        }
        return next;
      } catch (caught) {
        if (epoch === requestEpochRef.current) {
          errorOriginRef.current = "operation";
          setError(transferErrorMessage(caught));
        }
        throw caught;
      } finally {
        if (operationRef.current === nextOperation) {
          operationRef.current = null;
          setOperation(null);
          wakePollRef.current();
        }
      }
    },
    [applySnapshot, clearError, readDraftIfChanged],
  );

  return {
    transfer,
    draft,
    operation,
    error,
    follow,
    refresh,
    clearError,
    create: (input) => mutate("create", () => createTransfer(token, input)),
    saveDraft: (expectedRevision, content) =>
      mutate("draft", () => {
        const id = requireFollowedId(transferRef.current);
        return putTransferDraft(token, id, expectedRevision, content);
      }),
    chooseTarget: (input) =>
      mutate("target", () =>
        chooseTransferTarget(token, requireFollowedId(transferRef.current), input),
      ),
    deliver: (input) =>
      mutate("deliver", () =>
        deliverTransfer(token, requireFollowedId(transferRef.current), input),
      ),
    cleanup: (input) =>
      mutate("cleanup", () =>
        cleanupTransfer(token, requireFollowedId(transferRef.current), input),
      ),
    abandon: () =>
      mutate("abandon", () =>
        abandonTransfer(token, requireFollowedId(transferRef.current)),
      ),
  };
}

function requireFollowedId(transfer: TransferSnapshot | null): string {
  if (!transfer) {
    throw new Error("No transfer is in progress.");
  }
  return transfer.transferId;
}

function transferErrorMessage(error: unknown): string {
  return error instanceof Error
    ? error.message
    : "The transfer request could not be completed.";
}
