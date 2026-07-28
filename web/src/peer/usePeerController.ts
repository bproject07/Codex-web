import {
  useCallback,
  useEffect,
  useRef,
  useState,
} from "react";
import {
  createPeerThread,
  createPeerTurn,
  deletePeerThread,
  dispatchPeerTurn,
  listPeerThreads,
  returnPeerTurn,
} from "./api";
import { isPeerWorkPending } from "./actions";
import type {
  CreatePeerThreadInput,
  CreatePeerTurnInput,
  DispatchPeerTurnInput,
  PeerThread,
  ReturnPeerTurnInput,
} from "./types";

const ACTIVE_POLL_MS = 1_200;
const IDLE_POLL_MS = 5_000;

export type PeerOperation =
  | { kind: "create" }
  | { kind: "turn"; threadId: string }
  | { kind: "dispatch"; threadId: string }
  | { kind: "return"; threadId: string }
  | { kind: "close"; threadId: string };

export interface PeerController {
  threads: PeerThread[];
  ready: boolean;
  loading: boolean;
  operation: PeerOperation | null;
  error: string | null;
  refresh: () => Promise<void>;
  createThread: (input: CreatePeerThreadInput) => Promise<PeerThread>;
  createTurn: (
    threadId: string,
    input: CreatePeerTurnInput,
  ) => Promise<PeerThread>;
  dispatchTurn: (
    threadId: string,
    input: DispatchPeerTurnInput,
  ) => Promise<PeerThread>;
  returnTurn: (
    threadId: string,
    input: ReturnPeerTurnInput,
  ) => Promise<PeerThread>;
  closeThread: (threadId: string) => Promise<void>;
  clearError: () => void;
}

export function usePeerController(token: string): PeerController {
  const [threads, setThreads] = useState<PeerThread[]>([]);
  const [ready, setReady] = useState(false);
  const [loading, setLoading] = useState(Boolean(token));
  const [operation, setOperation] = useState<PeerOperation | null>(null);
  const [error, setError] = useState<string | null>(null);
  const threadsRef = useRef(threads);
  const requestEpochRef = useRef(0);
  const operationRef = useRef<PeerOperation | null>(null);
  const errorOriginRef = useRef<"read" | "operation" | null>(null);
  const wakePollRef = useRef<() => void>(() => undefined);

  threadsRef.current = threads;

  const replaceThreads = useCallback((next: PeerThread[]) => {
    threadsRef.current = next;
    setThreads(next);
  }, []);

  const replaceThread = useCallback((next: PeerThread) => {
    const merged = [
      ...threadsRef.current.filter((thread) => thread.id !== next.id),
      next,
    ].sort(
      (left, right) =>
        right.updatedAt - left.updatedAt || left.id.localeCompare(right.id),
    );
    threadsRef.current = merged;
    setThreads(merged);
  }, []);

  const removeThread = useCallback((threadId: string) => {
    const remaining = threadsRef.current.filter(
      (thread) => thread.id !== threadId,
    );
    threadsRef.current = remaining;
    setThreads(remaining);
  }, []);

  const clearError = useCallback(() => {
    errorOriginRef.current = null;
    setError(null);
  }, []);

  const refresh = useCallback(async () => {
    if (!token || operationRef.current) {
      return;
    }
    const epoch = ++requestEpochRef.current;
    setLoading(true);
    try {
      const next = await listPeerThreads(token);
      if (epoch === requestEpochRef.current) {
        replaceThreads(next);
        setReady(true);
        if (errorOriginRef.current === "read") {
          clearError();
        }
      }
    } catch (caught) {
      if (
        epoch === requestEpochRef.current &&
        errorOriginRef.current !== "operation"
      ) {
        errorOriginRef.current = "read";
        setError(peerErrorMessage(caught));
      }
    } finally {
      if (epoch === requestEpochRef.current) {
        setLoading(false);
      }
    }
  }, [clearError, replaceThreads, token]);

  useEffect(() => {
    if (!token) {
      requestEpochRef.current += 1;
      replaceThreads([]);
      setReady(false);
      setLoading(false);
      operationRef.current = null;
      setOperation(null);
      clearError();
      return;
    }

    let disposed = false;
    let timer: number | null = null;
    let controller: AbortController | null = null;

    const schedulePoll = (delay: number) => {
      if (disposed) {
        return;
      }
      if (timer !== null) {
        window.clearTimeout(timer);
      }
      timer = window.setTimeout(() => {
        timer = null;
        void poll();
      }, delay);
    };

    const poll = async () => {
      if (disposed) {
        return;
      }
      if (operationRef.current) {
        schedulePoll(ACTIVE_POLL_MS);
        return;
      }

      controller?.abort();
      const requestController = new AbortController();
      controller = requestController;
      const epoch = ++requestEpochRef.current;
      try {
        const next = await listPeerThreads(token, requestController.signal);
        if (
          !disposed &&
          !requestController.signal.aborted &&
          epoch === requestEpochRef.current &&
          !operationRef.current
        ) {
          replaceThreads(next);
          setReady(true);
          setLoading(false);
          if (errorOriginRef.current === "read") {
            clearError();
          }
        }
      } catch (caught) {
        if (
          !disposed &&
          !requestController.signal.aborted &&
          epoch === requestEpochRef.current &&
          !operationRef.current
        ) {
          setLoading(false);
          if (errorOriginRef.current !== "operation") {
            errorOriginRef.current = "read";
            setError(peerErrorMessage(caught));
          }
        }
      }
      if (disposed) {
        return;
      }
      const active = threadsRef.current.some((thread) =>
        isPeerWorkPending(thread.status),
      );
      const delay =
        document.visibilityState === "hidden"
          ? IDLE_POLL_MS
          : active
            ? ACTIVE_POLL_MS
            : IDLE_POLL_MS;
      schedulePoll(delay);
    };

    requestEpochRef.current += 1;
    replaceThreads([]);
    setReady(false);
    setLoading(true);
    clearError();
    wakePollRef.current = () => schedulePoll(0);
    void poll();
    const refreshOnVisibility = () => {
      if (document.visibilityState !== "visible") {
        return;
      }
      schedulePoll(0);
    };
    document.addEventListener("visibilitychange", refreshOnVisibility);

    return () => {
      disposed = true;
      requestEpochRef.current += 1;
      wakePollRef.current = () => undefined;
      controller?.abort();
      if (timer !== null) {
        window.clearTimeout(timer);
      }
      document.removeEventListener("visibilitychange", refreshOnVisibility);
    };
  }, [clearError, replaceThreads, token]);

  const mutate = useCallback(
    async (
      nextOperation: PeerOperation,
      request: () => Promise<PeerThread>,
    ) => {
      if (operationRef.current) {
        throw new Error("Another peer operation is already in progress.");
      }
      const epoch = ++requestEpochRef.current;
      operationRef.current = nextOperation;
      setOperation(nextOperation);
      clearError();
      try {
        const next = await request();
        if (epoch === requestEpochRef.current) {
          replaceThread(next);
        }
        return next;
      } catch (caught) {
        if (epoch === requestEpochRef.current) {
          errorOriginRef.current = "operation";
          setError(peerErrorMessage(caught));
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
    [clearError, replaceThread],
  );

  return {
    threads,
    ready,
    loading,
    operation,
    error,
    refresh,
    createThread: (input) =>
      mutate({ kind: "create" }, () => createPeerThread(token, input)),
    createTurn: (threadId, input) =>
      mutate({ kind: "turn", threadId }, () =>
        createPeerTurn(token, threadId, input),
      ),
    dispatchTurn: (threadId, input) =>
      mutate({ kind: "dispatch", threadId }, () =>
        dispatchPeerTurn(token, threadId, input),
      ),
    returnTurn: (threadId, input) =>
      mutate({ kind: "return", threadId }, () =>
        returnPeerTurn(token, threadId, input),
      ),
    closeThread: async (threadId) => {
      if (operationRef.current) {
        throw new Error("Another peer operation is already in progress.");
      }
      const nextOperation: PeerOperation = { kind: "close", threadId };
      const epoch = ++requestEpochRef.current;
      operationRef.current = nextOperation;
      setOperation(nextOperation);
      clearError();
      try {
        await deletePeerThread(token, threadId);
        if (epoch === requestEpochRef.current) {
          removeThread(threadId);
        }
      } catch (caught) {
        if (epoch === requestEpochRef.current) {
          errorOriginRef.current = "operation";
          setError(peerErrorMessage(caught));
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
    clearError,
  };
}

function peerErrorMessage(error: unknown): string {
  return error instanceof Error
    ? error.message
    : "The peer request could not be completed.";
}
