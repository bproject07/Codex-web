import {
  forwardRef,
  useCallback,
  useImperativeHandle,
  useLayoutEffect,
  useRef,
  useState,
} from "react";
import type { SessionSnapshot } from "../api";
import { TerminalView, type TerminalViewHandle } from "./TerminalView";
import type { ConnectionStatus } from "./reconnect";
import type { TerminalSettings } from "./settings";
import { retainTerminalViews } from "./viewCache";

interface TerminalDeckProps {
  token: string;
  sessions: SessionSnapshot[];
  selectedTerminalId: string;
  settings: TerminalSettings;
  ctrlMode: boolean;
  onCtrlConsumed: () => void;
  onConnectionStatus: (status: ConnectionStatus) => void;
  onConnected: () => void;
  onSession: (session: SessionSnapshot) => void;
  onSessionUnavailable: (terminalId: string) => void;
  onError: (message: string | null) => void;
}

interface TerminalPaneProps extends Omit<
  TerminalDeckProps,
  "sessions" | "selectedTerminalId"
> {
  terminalId: string;
  sessionId: string | null;
  active: boolean;
}

const TerminalPane = forwardRef<TerminalViewHandle, TerminalPaneProps>(
  function TerminalPane(
    { active, onConnectionStatus, onError, ...props },
    ref,
  ) {
    const activeRef = useRef(active);
    const statusRef = useRef<ConnectionStatus>("connecting");
    const errorRef = useRef<string | null>(null);
    activeRef.current = active;

    const reportStatus = useCallback((status: ConnectionStatus) => {
      statusRef.current = status;
      if (activeRef.current) {
        onConnectionStatus(status);
      }
    }, [onConnectionStatus]);
    const reportError = useCallback((message: string | null) => {
      errorRef.current = message;
      if (activeRef.current) {
        onError(message);
      }
    }, [onError]);

    useLayoutEffect(() => {
      if (active) {
        onConnectionStatus(statusRef.current);
        onError(errorRef.current);
      }
    }, [active, onConnectionStatus, onError]);

    return (
      <div
        className="terminal-pane"
        data-terminal-id={props.terminalId}
        hidden={!active}
        aria-hidden={!active}
        inert={!active}
      >
        <TerminalView
          {...props}
          ref={ref}
          active={active}
          onConnectionStatus={reportStatus}
          onError={reportError}
        />
      </div>
    );
  },
);

export const TerminalDeck = forwardRef<TerminalViewHandle, TerminalDeckProps>(
  function TerminalDeck(
    { sessions, selectedTerminalId, settings, ...props },
    ref,
  ) {
    const [retainedIds, setRetainedIds] = useState<readonly string[]>([]);
    const nextIds = retainTerminalViews(
      retainedIds,
      selectedTerminalId,
      sessions.map((session) => session.terminalId),
      settings.preserveTabs,
    );
    // Reconcile before committing children: a switch or preference change
    // must never briefly unmount the selected view or mount a seventh view.
    if (nextIds !== retainedIds) {
      setRetainedIds(nextIds);
    }

    const handles = useRef(new Map<string, TerminalViewHandle>());
    const selectedRef = useRef(selectedTerminalId);
    selectedRef.current = selectedTerminalId;
    useImperativeHandle(ref, () => {
      const current = () => handles.current.get(selectedRef.current);
      return {
        send: (data) => current()?.send(data),
        focus: () => current()?.focus(),
        fit: () => current()?.fit(),
        scrollToTop: () => current()?.scrollToTop(),
        scrollToBottom: () => current()?.scrollToBottom(),
        inspect: () => current()?.inspect() ?? null,
      };
    }, []);

    // Key each view by PTY generation so a restart discards its old screen
    // and any queued parser writes. Keep DOM order independent of visit order.
    return sessions.filter((session) => nextIds.includes(session.terminalId)).map(
      (session) => (
        <TerminalPane
          {...props}
          key={`${session.terminalId}:${session.sessionId ?? "pending"}`}
          ref={(handle) => {
            if (handle) {
              handles.current.set(session.terminalId, handle);
            } else {
              handles.current.delete(session.terminalId);
            }
          }}
          terminalId={session.terminalId}
          sessionId={session.sessionId}
          active={session.terminalId === selectedTerminalId}
          settings={settings}
          ctrlMode={session.terminalId === selectedTerminalId && props.ctrlMode}
        />
      ),
    );
  },
);
