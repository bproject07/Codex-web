import {
  type FormEvent,
  type KeyboardEvent as ReactKeyboardEvent,
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import {
  addWorkspaceFavorite,
  clearSelectedTerminalId,
  clearSessionToken,
  consumeTokenFromUrl,
  createSession,
  deleteSession,
  getAgentCatalog,
  getFilesystemRoots,
  getHealth,
  getWorkspaceLibrary,
  listSessions,
  listWorkspaceDirectory,
  readSelectedTerminalId,
  removeWorkspaceFavorite,
  resolveWorkspacePath,
  restartSession,
  terminateSession,
  writeSelectedTerminalId,
  writeSessionToken,
  type AgentCatalog,
  type AgentKind,
  type SessionSnapshot,
} from "./api";
import { agentLabel } from "./agents";
import { AgentPicker } from "./AgentPicker";
import {
  TerminalView,
  type TerminalViewHandle,
} from "./terminal/TerminalView";
import { MobileToolbar } from "./terminal/MobileToolbar";
import type { ConnectionStatus } from "./terminal/reconnect";
import {
  loadSettings,
  saveSettings,
  type TerminalSettings,
  type ThemeName,
} from "./terminal/settings";
import { SessionTabs } from "./sessions/SessionTabs";
import { shouldRouteDesktopSlash } from "./terminal/desktopSlash";
import {
  PeerComposer,
  usePeerController,
  type CreatePeerThreadInput,
  type CreatePeerTurnInput,
  type DispatchPeerTurnInput,
  type PeerThread,
  type ReturnPeerTurnInput,
} from "./peer";
import {
  WorkspacePicker,
  type WorkspaceAgent,
  type WorkspaceBrowserAdapter,
  type WorkspaceDirectory,
  type WorkspacePickerTransition,
} from "./workspaces";

const STATUS_LABELS: Record<ConnectionStatus | "codex_exited", string> = {
  connecting: "Connecting",
  connected: "Connected",
  reconnecting: "Reconnecting",
  disconnected: "Disconnected",
  authentication_failed: "Authentication failed",
  codex_exited: "Agent exited",
};

const COMPACT_STATUS_LABELS: Record<
  ConnectionStatus | "codex_exited",
  string
> = {
  connecting: "Connecting",
  connected: "Online",
  reconnecting: "Retrying",
  disconnected: "Offline",
  authentication_failed: "Auth failed",
  codex_exited: "Exited",
};

export function App() {
  const [token, setToken] = useState(consumeTokenFromUrl);
  const [connectionStatus, setConnectionStatus] =
    useState<ConnectionStatus>("connecting");
  const [session, setSession] = useState<SessionSnapshot | null>(null);
  const [sessions, setSessions] = useState<SessionSnapshot[]>([]);
  const [selectedTerminalId, setSelectedTerminalId] = useState(
    readSelectedTerminalId,
  );
  const [settings, setSettings] = useState<TerminalSettings>(loadSettings);
  const [ctrlMode, setCtrlMode] = useState(false);
  const [reconnectNonce, setReconnectNonce] = useState(0);
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [sessionsOpen, setSessionsOpen] = useState(false);
  const [workspacePickerOpen, setWorkspacePickerOpen] = useState(false);
  const [launchDirectory, setLaunchDirectory] =
    useState<WorkspaceDirectory | null>(null);
  const [agentPickerOpen, setAgentPickerOpen] = useState(false);
  const [peerComposerOpen, setPeerComposerOpen] = useState(false);
  const [agentCatalog, setAgentCatalog] = useState<AgentCatalog | null>(null);
  const [agentCatalogLoading, setAgentCatalogLoading] = useState(false);
  const [agentCatalogError, setAgentCatalogError] = useState<string | null>(
    null,
  );
  const [creatingAgent, setCreatingAgent] = useState<AgentKind | null>(null);
  const [sessionsLoading, setSessionsLoading] = useState(false);
  const [maxSessions, setMaxSessions] = useState<number | null>(null);
  const [busy, setBusy] = useState(false);
  const [message, setMessage] = useState<string | null>(null);
  const [viewportDiagnostics, setViewportDiagnostics] = useState("");
  const [viewportDiagnosticsCollecting, setViewportDiagnosticsCollecting] =
    useState(false);
  const [viewportDiagnosticsCopyNotice, setViewportDiagnosticsCopyNotice] =
    useState<string | null>(null);
  const terminalRef = useRef<TerminalViewHandle>(null);
  const selectedTerminalIdRef = useRef(selectedTerminalId);
  const sessionsRequestEpochRef = useRef(0);
  const capacityRequestEpochRef = useRef(0);
  const agentCatalogRequestEpochRef = useRef(0);
  const suppressTerminalFocusOnceRef = useRef(false);
  const peerController = usePeerController(token);

  selectedTerminalIdRef.current = selectedTerminalId;

  const workspaceAdapter = useMemo<WorkspaceBrowserAdapter>(
    () => ({
      loadLibrary: ({ signal } = {}) => getWorkspaceLibrary(token, signal),
      listRoots: async ({ signal } = {}) => {
        const roots = await getFilesystemRoots(token, signal);
        return {
          current: null,
          parentId: null,
          breadcrumbs: [],
          directories: roots.roots,
          truncated: false,
        };
      },
      listDirectory: (directoryId, { signal } = {}) =>
        listWorkspaceDirectory(token, directoryId, signal),
      resolvePath: (path, { signal } = {}) =>
        resolveWorkspacePath(token, path, signal),
      addFavorite: (directory, { signal } = {}) =>
        addWorkspaceFavorite(token, directory, signal),
      removeFavorite: (favoriteId, { signal } = {}) =>
        removeWorkspaceFavorite(token, favoriteId, signal),
    }),
    [token],
  );

  const effectiveStatus =
    connectionStatus === "connected" &&
    session &&
    ["exited", "failed", "terminated"].includes(session.status)
      ? "codex_exited"
      : connectionStatus;
  const selectedAgentLabel = session ? agentLabel(session.agent) : "Agent";
  const peerSourceSession = useMemo(() => {
    if (!session) {
      return null;
    }
    const purpose = session.purpose;
    return purpose.kind === "peer"
      ? sessions.find(
          (candidate) => candidate.terminalId === purpose.parentTerminalId,
        ) ?? null
      : session;
  }, [session, sessions]);
  const selectedPeerThreadId =
    session?.purpose.kind === "peer" ? session.purpose.threadId : null;
  const peerUnavailableReason = !peerSourceSession
    ? "@cwt requires an available source terminal."
    : peerSourceSession.purpose.kind !== "interactive"
      ? "@cwt requires an interactive source terminal."
      : peerSourceSession.status !== "running"
        ? `@cwt is unavailable because ${peerSourceSession.name} is ${peerSourceSession.status}. A running source terminal is required.`
        : null;
  const sessionCapacityReached =
    maxSessions !== null && sessions.length >= maxSessions;
  const newPeerThreadDisabledReason = sessionCapacityReached
    ? `Session capacity reached (${sessions.length} of ${maxSessions}). Close a terminal tab before starting a new reviewer. Existing reviewer conversations remain available.`
    : null;

  useEffect(() => {
    saveSettings(settings);
  }, [settings]);

  useEffect(() => {
    if (suppressTerminalFocusOnceRef.current && !agentPickerOpen) {
      suppressTerminalFocusOnceRef.current = false;
      return;
    }

    if (
      !token ||
      connectionStatus !== "connected" ||
      !selectedTerminalId ||
      settingsOpen ||
      sessionsOpen ||
      workspacePickerOpen ||
      agentPickerOpen ||
      peerComposerOpen ||
      window.matchMedia("(pointer: coarse)").matches
    ) {
      return;
    }

    const frame = window.requestAnimationFrame(() => {
      terminalRef.current?.focus();
    });
    return () => window.cancelAnimationFrame(frame);
  }, [
    connectionStatus,
    reconnectNonce,
    selectedTerminalId,
    sessionsOpen,
    settingsOpen,
    workspacePickerOpen,
    agentPickerOpen,
    peerComposerOpen,
    token,
  ]);

  useEffect(() => {
    let routedSlashIsPending = false;

    const routeSlashToTerminal = (event: KeyboardEvent) => {
      const target = event.target instanceof HTMLElement ? event.target : null;
      const editableTarget = Boolean(
        target &&
          (target.isContentEditable ||
            target.closest('input, textarea, select, [role="textbox"]')),
      );

      if (
        !shouldRouteDesktopSlash({
          key: event.key,
          altKey: event.altKey,
          ctrlKey: event.ctrlKey,
          metaKey: event.metaKey,
          defaultPrevented: event.defaultPrevented,
          isComposing: event.isComposing,
          coarsePointer: window.matchMedia("(pointer: coarse)").matches,
          dialogOpen:
            settingsOpen ||
            sessionsOpen ||
            workspacePickerOpen ||
            agentPickerOpen ||
            peerComposerOpen,
          editableTarget,
          terminalAvailable:
            Boolean(token && selectedTerminalId) &&
            connectionStatus === "connected",
        })
      ) {
        return;
      }

      event.preventDefault();
      event.stopPropagation();
      event.stopImmediatePropagation();
      routedSlashIsPending = true;
      terminalRef.current?.focus();
      terminalRef.current?.send("/");
    };

    const suppressRoutedSlashFollowUp = (event: KeyboardEvent) => {
      if (!routedSlashIsPending || event.key !== "/") {
        return;
      }
      event.preventDefault();
      event.stopPropagation();
      event.stopImmediatePropagation();
      if (event.type === "keyup") {
        routedSlashIsPending = false;
      }
    };

    window.addEventListener("keydown", routeSlashToTerminal, true);
    window.addEventListener("keypress", suppressRoutedSlashFollowUp, true);
    window.addEventListener("keyup", suppressRoutedSlashFollowUp, true);
    return () => {
      window.removeEventListener("keydown", routeSlashToTerminal, true);
      window.removeEventListener(
        "keypress",
        suppressRoutedSlashFollowUp,
        true,
      );
      window.removeEventListener("keyup", suppressRoutedSlashFollowUp, true);
    };
  }, [
    connectionStatus,
    selectedTerminalId,
    sessionsOpen,
    settingsOpen,
    workspacePickerOpen,
    agentPickerOpen,
    peerComposerOpen,
    token,
  ]);

  const applySessionList = useCallback(
    (nextSessions: SessionSnapshot[], preferredTerminalId?: string) => {
      const orderedSessions = [...nextSessions].sort(
        (left, right) =>
          Number(right.isPrimary) - Number(left.isPrimary) ||
          left.createdAt - right.createdAt ||
          left.name.localeCompare(right.name),
      );
      setSessions(orderedSessions);

      const preferred = preferredTerminalId ?? selectedTerminalIdRef.current;
      const selected =
        orderedSessions.find((candidate) => candidate.terminalId === preferred) ??
        orderedSessions.find((candidate) => candidate.isPrimary) ??
        orderedSessions[0];

      if (!selected) {
        selectedTerminalIdRef.current = "";
        setSelectedTerminalId("");
        setSession(null);
        clearSelectedTerminalId();
        return;
      }

      selectedTerminalIdRef.current = selected.terminalId;
      setSelectedTerminalId(selected.terminalId);
      writeSelectedTerminalId(selected.terminalId);
      setSession((current) =>
        current?.terminalId === selected.terminalId ? current : selected,
      );
    },
    [],
  );

  const refreshCapacity = useCallback(
    async (signal?: AbortSignal) => {
      const requestEpoch = ++capacityRequestEpochRef.current;
      if (!token) {
        if (capacityRequestEpochRef.current === requestEpoch) {
          setMaxSessions(null);
        }
        return;
      }

      try {
        const health = await getHealth(token, signal);
        if (
          !signal?.aborted &&
          capacityRequestEpochRef.current === requestEpoch
        ) {
          setMaxSessions(health.maxSessions);
        }
      } catch {
        if (
          !signal?.aborted &&
          capacityRequestEpochRef.current === requestEpoch
        ) {
          // Capacity metadata is advisory. Keep session management usable with
          // an older, temporarily unavailable, or malformed health endpoint.
          setMaxSessions(null);
        }
      }
    },
    [token],
  );

  useEffect(() => {
    const abortController = new AbortController();
    setMaxSessions(null);
    void refreshCapacity(abortController.signal);

    return () => {
      abortController.abort();
      capacityRequestEpochRef.current += 1;
    };
  }, [refreshCapacity]);

  const refreshSessions = useCallback(
    async (preferredTerminalId?: string) => {
      if (!token) {
        return;
      }

      void refreshCapacity();
      const requestEpoch = ++sessionsRequestEpochRef.current;
      setSessionsLoading(true);
      try {
        const nextSessions = await listSessions(token);
        if (sessionsRequestEpochRef.current === requestEpoch) {
          applySessionList(nextSessions, preferredTerminalId);
        }
      } catch (error) {
        if (sessionsRequestEpochRef.current === requestEpoch) {
          setMessage(
            error instanceof Error
              ? error.message
              : "Could not load terminal sessions.",
          );
        }
      } finally {
        if (sessionsRequestEpochRef.current === requestEpoch) {
          setSessionsLoading(false);
        }
      }
    },
    [applySessionList, refreshCapacity, token],
  );

  useEffect(() => {
    if (!token) {
      sessionsRequestEpochRef.current += 1;
      setSessionsLoading(false);
      return;
    }

    const abortController = new AbortController();
    const requestEpoch = ++sessionsRequestEpochRef.current;
    setSessionsLoading(true);
    void listSessions(token, abortController.signal)
      .then((nextSessions) => {
        if (sessionsRequestEpochRef.current === requestEpoch) {
          applySessionList(nextSessions);
        }
      })
      .catch((error) => {
        if (
          !abortController.signal.aborted &&
          sessionsRequestEpochRef.current === requestEpoch
        ) {
          setMessage(
            error instanceof Error
              ? error.message
              : "Could not load terminal sessions.",
          );
        }
      })
      .finally(() => {
        if (
          !abortController.signal.aborted &&
          sessionsRequestEpochRef.current === requestEpoch
        ) {
          setSessionsLoading(false);
        }
      });

    return () => {
      abortController.abort();
      if (sessionsRequestEpochRef.current === requestEpoch) {
        sessionsRequestEpochRef.current += 1;
      }
    };
  }, [applySessionList, token]);

  const refreshAgentCatalog = useCallback(
    async (force = false, signal?: AbortSignal) => {
      if (!token) {
        return;
      }

      const requestEpoch = ++agentCatalogRequestEpochRef.current;
      setAgentCatalogLoading(true);
      setAgentCatalogError(null);
      try {
        const nextCatalog = await getAgentCatalog(token, {
          refresh: force,
          signal,
        });
        if (
          !signal?.aborted &&
          agentCatalogRequestEpochRef.current === requestEpoch
        ) {
          setAgentCatalog(nextCatalog);
        }
      } catch (error) {
        if (
          !signal?.aborted &&
          agentCatalogRequestEpochRef.current === requestEpoch
        ) {
          setAgentCatalogError(
            error instanceof Error
              ? error.message
              : "Could not check installed CLI agents.",
          );
        }
      } finally {
        if (
          !signal?.aborted &&
          agentCatalogRequestEpochRef.current === requestEpoch
        ) {
          setAgentCatalogLoading(false);
        }
      }
    },
    [token],
  );

  useEffect(() => {
    if (!token) {
      agentCatalogRequestEpochRef.current += 1;
      setAgentCatalog(null);
      setAgentCatalogLoading(false);
      setAgentCatalogError(null);
      return;
    }

    const abortController = new AbortController();
    void refreshAgentCatalog(false, abortController.signal);
    return () => {
      abortController.abort();
    };
  }, [refreshAgentCatalog, token]);

  useEffect(() => {
    const viewport = window.visualViewport;
    const coarsePointer = window.matchMedia("(pointer: coarse)").matches;
    let settleTimer: number | null = null;
    let animationFrame: number | null = null;
    let lastHeight = -1;
    let previousInnerHeight = window.innerHeight;
    let nativeLayoutResize = false;

    const stopManagedViewportHeight = () => {
      nativeLayoutResize = true;
      if (settleTimer !== null) {
        window.clearTimeout(settleTimer);
        settleTimer = null;
      }
      if (animationFrame !== null) {
        window.cancelAnimationFrame(animationFrame);
        animationFrame = null;
      }
      document.documentElement.style.removeProperty("--app-height");
    };

    const applyViewportHeight = () => {
      animationFrame = null;
      if (nativeLayoutResize) {
        return;
      }
      const height = viewport?.height ?? window.innerHeight;
      if (Math.abs(height - lastHeight) < 0.5) {
        return;
      }
      lastHeight = height;
      document.documentElement.style.setProperty("--app-height", `${height}px`);
    };

    const scheduleViewportHeight = () => {
      const innerHeight = window.innerHeight;
      const layoutHeightChanged =
        Math.abs(innerHeight - previousInnerHeight) >= 2;
      previousInnerHeight = innerHeight;

      if (
        coarsePointer &&
        viewport &&
        layoutHeightChanged &&
        Math.abs(innerHeight - viewport.height) < 2
      ) {
        stopManagedViewportHeight();
        return;
      }

      if (nativeLayoutResize) {
        return;
      }
      if (settleTimer !== null) {
        window.clearTimeout(settleTimer);
      }
      settleTimer = window.setTimeout(
        () => {
          settleTimer = null;
          if (animationFrame !== null) {
            window.cancelAnimationFrame(animationFrame);
          }
          animationFrame = window.requestAnimationFrame(applyViewportHeight);
        },
        coarsePointer ? 140 : 0,
      );
    };

    applyViewportHeight();
    viewport?.addEventListener("resize", scheduleViewportHeight);
    window.addEventListener("resize", scheduleViewportHeight);
    return () => {
      viewport?.removeEventListener("resize", scheduleViewportHeight);
      window.removeEventListener("resize", scheduleViewportHeight);
      if (settleTimer !== null) {
        window.clearTimeout(settleTimer);
      }
      if (animationFrame !== null) {
        window.cancelAnimationFrame(animationFrame);
      }
      document.documentElement.style.removeProperty("--app-height");
    };
  }, []);

  useEffect(() => {
    const viewport = window.visualViewport;
    const timers = new Set<number>();
    let active = false;
    let startedAt = 0;
    let samples: Array<Record<string, unknown>> = [];

    const round = (value: number | undefined | null) =>
      value === undefined || value === null
        ? null
        : Math.round(value * 10) / 10;

    const sample = (event: string) => {
      if (!active) {
        return;
      }
      const appRect = document
        .querySelector(".app-shell")
        ?.getBoundingClientRect();
      const terminalRect = document
        .querySelector(".terminal-view")
        ?.getBoundingClientRect();
      const textareaRect = document
        .querySelector(".xterm-helper-textarea")
        ?.getBoundingClientRect();
      const activeElement = document.activeElement;

      samples.push({
        t: Math.round(performance.now() - startedAt),
        event,
        innerHeight: window.innerHeight,
        scrollY: round(window.scrollY),
        documentScrollTop: round(document.documentElement.scrollTop),
        visualHeight: round(viewport?.height),
        visualOffsetTop: round(viewport?.offsetTop),
        visualPageTop: round(viewport?.pageTop),
        appTop: round(appRect?.top),
        appHeight: round(appRect?.height),
        terminalTop: round(terminalRect?.top),
        terminalBottom: round(terminalRect?.bottom),
        textareaTop: round(textareaRect?.top),
        activeElement:
          activeElement instanceof HTMLElement
            ? activeElement.className || activeElement.tagName
            : null,
        terminal: terminalRef.current?.inspect() ?? null,
      });
    };

    const finish = () => {
      if (!active) {
        return;
      }
      sample("finish");
      active = false;
      const viewportMeta = document.querySelector<HTMLMetaElement>(
        'meta[name="viewport"]',
      );
      setViewportDiagnostics(
        JSON.stringify(
          {
            version: 1,
            userAgent: navigator.userAgent,
            platform: navigator.platform,
            screen: {
              width: window.screen.width,
              height: window.screen.height,
              pixelRatio: window.devicePixelRatio,
            },
            viewportMeta: viewportMeta?.content ?? null,
            samples,
          },
          null,
          2,
        ),
      );
      setViewportDiagnosticsCollecting(false);
    };

    const scheduleSample = (event: string, delay: number) => {
      const timer = window.setTimeout(() => {
        timers.delete(timer);
        sample(event);
      }, delay);
      timers.add(timer);
    };

    const begin = (event: PointerEvent) => {
      const target = event.target;
      if (!(target instanceof Element) || !target.closest(".terminal-view")) {
        return;
      }

      for (const timer of timers) {
        window.clearTimeout(timer);
      }
      timers.clear();
      samples = [];
      startedAt = performance.now();
      active = true;
      setViewportDiagnostics("");
      setViewportDiagnosticsCollecting(true);
      setViewportDiagnosticsCopyNotice(null);
      sample("terminal_pointerdown");
      for (const delay of [16, 50, 100, 200, 400, 800, 1200, 1800]) {
        scheduleSample(`timer_${delay}`, delay);
      }
      const finishTimer = window.setTimeout(() => {
        timers.delete(finishTimer);
        finish();
      }, 1900);
      timers.add(finishTimer);
    };

    const onFocus = () => sample("focusin");
    const onViewportResize = () => sample("visual_resize");
    const onViewportScroll = () => sample("visual_scroll");
    const onWindowResize = () => sample("window_resize");
    const onWindowScroll = () => sample("window_scroll");

    document.addEventListener("pointerdown", begin, true);
    document.addEventListener("focusin", onFocus, true);
    viewport?.addEventListener("resize", onViewportResize);
    viewport?.addEventListener("scroll", onViewportScroll);
    window.addEventListener("resize", onWindowResize);
    window.addEventListener("scroll", onWindowScroll, true);

    return () => {
      document.removeEventListener("pointerdown", begin, true);
      document.removeEventListener("focusin", onFocus, true);
      viewport?.removeEventListener("resize", onViewportResize);
      viewport?.removeEventListener("scroll", onViewportScroll);
      window.removeEventListener("resize", onWindowResize);
      window.removeEventListener("scroll", onWindowScroll, true);
      for (const timer of timers) {
        window.clearTimeout(timer);
      }
    };
  }, []);

  const handleSession = useCallback((nextSession: SessionSnapshot) => {
    setSession(nextSession);
    setSessions((current) => {
      const existingIndex = current.findIndex(
        (candidate) => candidate.terminalId === nextSession.terminalId,
      );
      if (existingIndex === -1) {
        return [...current, nextSession];
      }
      const updated = [...current];
      updated[existingIndex] = nextSession;
      return updated;
    });
  }, []);

  const handleStatus = useCallback(
    (status: ConnectionStatus) => {
      setConnectionStatus(status);
      if (status === "connected") {
        void refreshCapacity();
      }
    },
    [refreshCapacity],
  );

  const handleError = useCallback((error: string | null) => {
    setMessage(error);
  }, []);

  const handleSessionUnavailable = useCallback(
    (terminalId: string) => {
      if (selectedTerminalIdRef.current === terminalId) {
        void refreshSessions("");
      }
    },
    [refreshSessions],
  );

  const handleRestart = async () => {
    const terminalId = selectedTerminalIdRef.current;
    if (!terminalId) {
      setMessage("No terminal session is selected.");
      return;
    }
    if (
      !window.confirm(
        `Restart ${selectedAgentLabel}? The current process will be terminated.`,
      )
    ) {
      return;
    }
    setBusy(true);
    setMessage(null);
    sessionsRequestEpochRef.current += 1;
    setSessionsLoading(false);
    try {
      await restartSession(token, terminalId);
      setReconnectNonce((value) => value + 1);
      void refreshSessions();
    } catch (error) {
      setMessage(error instanceof Error ? error.message : "Restart failed.");
    } finally {
      setBusy(false);
    }
  };

  const handleTerminate = async () => {
    const terminalId = selectedTerminalIdRef.current;
    if (!terminalId) {
      setMessage("No terminal session is selected.");
      return;
    }
    if (!window.confirm(`Terminate the active ${selectedAgentLabel} process?`)) {
      return;
    }
    setBusy(true);
    setMessage(null);
    sessionsRequestEpochRef.current += 1;
    setSessionsLoading(false);
    try {
      await terminateSession(token, terminalId);
      void refreshSessions();
    } catch (error) {
      setMessage(error instanceof Error ? error.message : "Terminate failed.");
    } finally {
      setBusy(false);
    }
  };

  const focusTerminalForFinePointer = () => {
    if (window.matchMedia("(pointer: coarse)").matches) {
      return;
    }
    window.requestAnimationFrame(() => terminalRef.current?.focus());
  };

  const closeSessions = () => {
    setSessionsOpen(false);
    focusTerminalForFinePointer();
  };

  const closePeerComposer = () => {
    setPeerComposerOpen(false);
    if (window.matchMedia("(pointer: coarse)").matches) {
      suppressTerminalFocusOnceRef.current = true;
      return;
    }
    focusTerminalForFinePointer();
  };

  const openPeerComposer = () => {
    if (!peerSourceSession || peerUnavailableReason) {
      setMessage(
        peerUnavailableReason ??
          "The source terminal for this peer request is unavailable.",
      );
      return;
    }
    setSettingsOpen(false);
    setSessionsOpen(false);
    setWorkspacePickerOpen(false);
    setAgentPickerOpen(false);
    setLaunchDirectory(null);
    setPeerComposerOpen(true);
    peerController.clearError();
    void peerController.refresh();
    void refreshAgentCatalog(true);
  };

  const closeWorkspacePicker = () => {
    if (busy) {
      return;
    }
    suppressTerminalFocusOnceRef.current = true;
    setWorkspacePickerOpen(false);
    setLaunchDirectory(null);
  };

  const chooseLaunchDirectory = (
    directory: WorkspaceDirectory,
    transition: WorkspacePickerTransition,
  ) => {
    transition.suppressFocusReturn();
    setLaunchDirectory(directory);
    setWorkspacePickerOpen(false);
    setAgentPickerOpen(true);
    setAgentCatalogError(null);
  };

  const attachSession = (nextSession: SessionSnapshot) => {
    selectedTerminalIdRef.current = nextSession.terminalId;
    setSelectedTerminalId(nextSession.terminalId);
    writeSelectedTerminalId(nextSession.terminalId);
    setSession(nextSession);
    setCtrlMode(false);
    setConnectionStatus("connecting");
    setMessage(null);
    closeSessions();
  };

  const handleCreateSession = async (
    agent: AgentKind,
    directory = launchDirectory,
  ) => {
    if (!directory) {
      setAgentCatalogError("Choose a project folder before starting an agent.");
      setWorkspacePickerOpen(true);
      setAgentPickerOpen(false);
      return;
    }

    setCreatingAgent(agent);
    setBusy(true);
    setMessage(null);
    setAgentCatalogError(null);
    sessionsRequestEpochRef.current += 1;
    setSessionsLoading(false);
    try {
      const created = await createSession(token, agent, directory.id);
      setSessions((current) => [
        ...current.filter(
          (candidate) => candidate.terminalId !== created.terminalId,
        ),
        created,
      ]);
      setWorkspacePickerOpen(false);
      setAgentPickerOpen(false);
      setLaunchDirectory(null);
      attachSession(created);
      void refreshSessions(created.terminalId);
    } catch (error) {
      setAgentCatalogError(
        error instanceof Error
          ? error.message
          : "Could not create a terminal session.",
      );
      setWorkspacePickerOpen(false);
      setAgentPickerOpen(true);
      void refreshSessions();
    } finally {
      setCreatingAgent(null);
      setBusy(false);
    }
  };

  const handleWorkspaceStart = (
    directory: WorkspaceDirectory,
    agent: WorkspaceAgent,
    transition: WorkspacePickerTransition,
  ) => {
    transition.suppressFocusReturn();
    setLaunchDirectory(directory);
    const catalogEntry = agentCatalog?.agents.find(
      (candidate) => candidate.kind === agent,
    );

    if (catalogEntry?.state === "ready") {
      void handleCreateSession(agent, directory);
      return;
    }

    setWorkspacePickerOpen(false);
    setAgentPickerOpen(true);
    setAgentCatalogError(
      catalogEntry
        ? `${agentLabel(agent)} is not ready on the server. Choose another installed agent or follow its setup instructions.`
        : "Choose an installed agent for this folder.",
    );
    if (!agentCatalog && !agentCatalogLoading) {
      void refreshAgentCatalog(true);
    }
  };

  const handleDeleteSession = async (target: SessionSnapshot) => {
    if (target.isPrimary) {
      return;
    }
    if (
      !window.confirm(
        `Remove "${target.name}"? Its ${agentLabel(target.agent)} process will be terminated.`,
      )
    ) {
      return;
    }

    setBusy(true);
    setMessage(null);
    sessionsRequestEpochRef.current += 1;
    setSessionsLoading(false);
    try {
      await deleteSession(token, target.terminalId);
      await refreshSessions(
        target.terminalId === selectedTerminalIdRef.current
          ? ""
          : selectedTerminalIdRef.current,
      );
    } catch (error) {
      setMessage(
        error instanceof Error ? error.message : "Could not remove the terminal session.",
      );
    } finally {
      setBusy(false);
    }
  };

  const handleCloseSessionTab = async (target: SessionSnapshot) => {
    if (target.isPrimary) {
      return;
    }
    if (target.purpose.kind !== "peer") {
      await handleDeleteSession(target);
      return;
    }

    const purpose = target.purpose;
    const thread = peerController.threads.find(
      (candidate) => candidate.id === purpose.threadId,
    );
    const warning =
      thread?.status === "response_ready"
        ? "Its reviewer response has not finished returning to the source."
        : thread &&
            ["preparing_handoff", "awaiting_preview", "reviewing"].includes(
              thread.status,
            )
          ? "The peer task is still in progress."
          : "Its dedicated reviewer process will be terminated.";
    if (
      !window.confirm(
        `Close "${target.name}"? ${warning}`,
      )
    ) {
      return;
    }

    setMessage(null);
    try {
      await peerController.closeThread(purpose.threadId);
      await refreshSessions(
        target.terminalId === selectedTerminalIdRef.current
          ? purpose.parentTerminalId
          : selectedTerminalIdRef.current,
      );
    } catch (error) {
      setMessage(
        error instanceof Error
          ? error.message
          : "Could not close the peer session.",
      );
    }
  };

  const syncPeerReviewer = async (next: PeerThread) => {
    if (!next.reviewerTerminalId) {
      return;
    }
    await refreshSessions(
      selectedTerminalIdRef.current || next.sourceTerminalId,
    );
  };

  const handleCreatePeerThread = async (input: CreatePeerThreadInput) => {
    const next = await peerController.createThread(input);
    await syncPeerReviewer(next);
    return next;
  };

  const handleCreatePeerTurn = async (
    threadId: string,
    input: CreatePeerTurnInput,
  ) => {
    const next = await peerController.createTurn(threadId, input);
    await syncPeerReviewer(next);
    return next;
  };

  const handleDispatchPeerTurn = async (
    threadId: string,
    input: DispatchPeerTurnInput,
  ) => {
    const next = await peerController.dispatchTurn(threadId, input);
    await syncPeerReviewer(next);
    return next;
  };

  const handleReturnPeerTurn = async (
    threadId: string,
    input: ReturnPeerTurnInput,
  ) => {
    return peerController.returnTurn(threadId, input);
  };

  const toggleFullscreen = async () => {
    try {
      if (document.fullscreenElement) {
        await document.exitFullscreen();
      } else {
        await document.documentElement.requestFullscreen();
      }
      terminalRef.current?.fit();
    } catch {
      setMessage("Fullscreen is not available in this browser.");
    }
  };

  const updateSettings = (patch: Partial<TerminalSettings>) => {
    setSettings((current) => ({ ...current, ...patch }));
  };

  const copyViewportDiagnostics = async () => {
    if (!viewportDiagnostics) {
      setViewportDiagnosticsCopyNotice(
        viewportDiagnosticsCollecting
          ? "Collecting diagnostics… wait a moment."
          : "Tap the terminal once, wait two seconds, then try again.",
      );
      return;
    }

    const copyWithLegacySelection = () => {
      const previousActive =
        document.activeElement instanceof HTMLElement
          ? document.activeElement
          : null;
      const textarea = document.createElement("textarea");
      textarea.value = viewportDiagnostics;
      textarea.setAttribute("readonly", "");
      textarea.style.position = "fixed";
      textarea.style.inset = "0";
      textarea.style.width = "1px";
      textarea.style.height = "1px";
      textarea.style.fontSize = "16px";
      textarea.style.opacity = "0";
      textarea.style.pointerEvents = "none";
      document.body.appendChild(textarea);

      try {
        textarea.focus({ preventScroll: true });
        textarea.select();
        textarea.setSelectionRange(0, textarea.value.length);
        return document.execCommand("copy");
      } finally {
        textarea.remove();
        previousActive?.focus({ preventScroll: true });
      }
    };

    try {
      let copied = false;
      if (navigator.clipboard && window.isSecureContext) {
        try {
          await navigator.clipboard.writeText(viewportDiagnostics);
          copied = true;
        } catch {
          // Fall through to the selection-based path below.
        }
      }

      if (!copied) {
        copied = copyWithLegacySelection();
      }
      if (!copied) {
        throw new Error("Clipboard copy was rejected.");
      }

      setViewportDiagnosticsCopyNotice("Copied. Paste it into the chat.");
    } catch {
      setViewportDiagnosticsCopyNotice(
        "Automatic copy was blocked. Open the text below and use Select all / Copy.",
      );
    }
  };

  if (!token) {
    return (
      <AuthenticationScreen
        onToken={(nextToken) => {
          writeSessionToken(nextToken);
          setToken(nextToken);
          setReconnectNonce((value) => value + 1);
        }}
      />
    );
  }

  return (
    <main className="app-shell">
      <header className="app-header">
        <div className="app-identity">
          <h1>
            Codex Web Terminal
            <span className="community-label">Unofficial</span>
            {session && (
              <span className={`active-agent active-agent--${session.agent}`}>
                {selectedAgentLabel}
              </span>
            )}
          </h1>
          <div className="project-path" title={session?.project}>
            <span>Project:</span> {session?.project ?? "Loading…"}
          </div>
        </div>
        <div className="header-actions">
          <SessionTabs
            sessions={sessions}
            maxSessions={maxSessions}
            selectedTerminalId={selectedTerminalId}
            busy={busy || peerController.operation !== null}
            peerThreads={peerController.threads}
            peerDisabledReason={peerUnavailableReason}
            onSelect={attachSession}
            onClose={(target) => void handleCloseSessionTab(target)}
            onPeer={openPeerComposer}
            onCreate={() => {
              setPeerComposerOpen(false);
              setSessionsOpen(false);
              setSettingsOpen(false);
              setAgentPickerOpen(false);
              setLaunchDirectory(null);
              setWorkspacePickerOpen(true);
              setAgentCatalogError(null);
              void refreshAgentCatalog(true);
            }}
            onManage={() => {
              setPeerComposerOpen(false);
              setWorkspacePickerOpen(false);
              setLaunchDirectory(null);
              setAgentPickerOpen(false);
              setSettingsOpen(false);
              setSessionsOpen(true);
              void refreshSessions();
            }}
          />
          <span className={`status status--${effectiveStatus}`}>
            <span className="status-dot" aria-hidden="true" />
            <span className="status-label status-label--full">
              {STATUS_LABELS[effectiveStatus]}
            </span>
            <span className="status-label status-label--compact">
              {COMPACT_STATUS_LABELS[effectiveStatus]}
            </span>
          </span>
          <button
            type="button"
            title="Reconnect the browser terminal"
            onClick={() => {
              if (selectedTerminalIdRef.current) {
                setReconnectNonce((value) => value + 1);
              } else {
                void refreshSessions();
              }
            }}
          >
            <span className="action-label action-label--full">Reconnect</span>
            <span className="action-label action-label--compact">Connect</span>
          </button>
          <button
            type="button"
            title={`Restart ${selectedAgentLabel}`}
            disabled={busy || session?.purpose.kind === "peer"}
            onClick={() => void handleRestart()}
          >
            <span className="action-label action-label--full">
              Restart {selectedAgentLabel}
            </span>
            <span className="action-label action-label--compact">Restart</span>
          </button>
          <button
            type="button"
            title="Toggle fullscreen"
            onClick={() => void toggleFullscreen()}
          >
            <span className="action-label action-label--full">Fullscreen</span>
            <span className="action-label action-label--compact">Full</span>
          </button>
          <button
            type="button"
            title="Show or hide terminal keys"
            aria-pressed={settings.mobileKeys}
            onClick={() => updateSettings({ mobileKeys: !settings.mobileKeys })}
          >
            <span className="action-label action-label--full">Mobile keys</span>
            <span className="action-label action-label--compact">Keys</span>
          </button>
          <button
            type="button"
            title="Open terminal settings"
            onClick={() => {
              setPeerComposerOpen(false);
              setWorkspacePickerOpen(false);
              setLaunchDirectory(null);
              setAgentPickerOpen(false);
              setSessionsOpen(false);
              setSettingsOpen(true);
            }}
          >
            <span className="action-label action-label--full">Settings</span>
            <span className="action-label action-label--compact">Setup</span>
          </button>
        </div>
      </header>

      {message && (
        <div className="message-banner" role="alert">
          <span>{message}</span>
          <button type="button" onClick={() => setMessage(null)} aria-label="Dismiss">
            ×
          </button>
        </div>
      )}

      <section className="terminal-region">
        {selectedTerminalId ? (
          <TerminalView
            key={selectedTerminalId}
            ref={terminalRef}
            token={token}
            terminalId={selectedTerminalId}
            settings={settings}
            reconnectNonce={reconnectNonce}
            ctrlMode={ctrlMode}
            onCtrlConsumed={() => setCtrlMode(false)}
            onConnectionStatus={handleStatus}
            onSession={handleSession}
            onSessionUnavailable={handleSessionUnavailable}
            onError={handleError}
          />
        ) : (
          <div className="terminal-empty" role="status" aria-live="polite">
            {sessionsLoading
              ? "Loading terminal sessions…"
              : "No terminal session is available. Start a new one to continue."}
          </div>
        )}
      </section>

      {settings.mobileKeys && (
        <MobileToolbar
          ctrlMode={ctrlMode}
          onCtrlModeChange={(active) => {
            setCtrlMode(active);
            terminalRef.current?.focus();
          }}
          onSend={(data) => terminalRef.current?.send(data)}
          onScrollPages={(pageCount) =>
            terminalRef.current?.scrollPages(pageCount)
          }
          onScrollToTop={() => terminalRef.current?.scrollToTop()}
          onScrollToBottom={() => terminalRef.current?.scrollToBottom()}
          onHide={() => updateSettings({ mobileKeys: false })}
        />
      )}

      {agentPickerOpen && (
        <AgentPicker
          catalog={agentCatalog}
          loading={agentCatalogLoading}
          error={agentCatalogError}
          creatingAgent={creatingAgent}
          workspacePath={launchDirectory?.path}
          onChangeWorkspace={() => {
            setAgentPickerOpen(false);
            setWorkspacePickerOpen(true);
            setAgentCatalogError(null);
          }}
          onSelect={(agent) => void handleCreateSession(agent)}
          onRefresh={() => void refreshAgentCatalog(true)}
          onClose={() => {
            suppressTerminalFocusOnceRef.current = true;
            setAgentPickerOpen(false);
            setLaunchDirectory(null);
            setAgentCatalogError(null);
          }}
        />
      )}

      {peerComposerOpen && peerSourceSession && (
        <PeerComposer
          sourceSession={peerSourceSession}
          initialThreadId={selectedPeerThreadId}
          allowNew={session?.purpose.kind !== "peer"}
          newThreadDisabledReason={newPeerThreadDisabledReason}
          catalog={agentCatalog}
          catalogLoading={agentCatalogLoading}
          catalogError={agentCatalogError}
          threads={peerController.threads}
          threadsReady={peerController.ready}
          threadsLoading={peerController.loading}
          operation={peerController.operation}
          error={peerController.error}
          onCreateThread={handleCreatePeerThread}
          onCreateTurn={handleCreatePeerTurn}
          onDispatchTurn={handleDispatchPeerTurn}
          onReturnTurn={handleReturnPeerTurn}
          onRefreshThreads={peerController.refresh}
          onClearError={peerController.clearError}
          onClose={closePeerComposer}
        />
      )}

      {workspacePickerOpen && (
        <div
          className="dialog-backdrop"
          role="presentation"
          onMouseDown={closeWorkspacePicker}
        >
          <WorkspacePicker
            adapter={workspaceAdapter}
            initialDirectoryId={
              launchDirectory?.id || session?.directoryId || null
            }
            disabled={busy}
            onChoose={chooseLaunchDirectory}
            onStart={handleWorkspaceStart}
            onCancel={closeWorkspacePicker}
          />
        </div>
      )}

      {settingsOpen && (
        <SettingsPanel
          settings={settings}
          busy={busy}
          agentLabel={selectedAgentLabel}
          onChange={updateSettings}
          onClose={() => {
            setSettingsOpen(false);
            focusTerminalForFinePointer();
          }}
          onTerminate={() => void handleTerminate()}
          onForgetToken={() => {
            clearSessionToken();
            clearSelectedTerminalId();
            setSettingsOpen(false);
            setSessionsOpen(false);
            setPeerComposerOpen(false);
            setWorkspacePickerOpen(false);
            setAgentPickerOpen(false);
            setLaunchDirectory(null);
            setSessions([]);
            setSelectedTerminalId("");
            setSession(null);
            setToken("");
          }}
          viewportDiagnostics={viewportDiagnostics}
          viewportDiagnosticsCollecting={viewportDiagnosticsCollecting}
          viewportDiagnosticsCopyNotice={viewportDiagnosticsCopyNotice}
          onCopyViewportDiagnostics={() => void copyViewportDiagnostics()}
        />
      )}

      {sessionsOpen && (
        <SessionsPanel
          sessions={sessions}
          selectedTerminalId={selectedTerminalId}
          busy={busy || peerController.operation !== null}
          loading={sessionsLoading}
          onAttach={attachSession}
          onRemove={(target) => void handleCloseSessionTab(target)}
          onRefresh={() => void refreshSessions()}
          onClose={closeSessions}
        />
      )}
    </main>
  );
}

const SESSION_STATUS_LABELS: Record<SessionSnapshot["status"], string> = {
  idle: "Idle",
  starting: "Starting",
  running: "Running",
  terminating: "Terminating",
  terminated: "Terminated",
  exited: "Exited",
  failed: "Failed",
};

interface SessionsPanelProps {
  sessions: SessionSnapshot[];
  selectedTerminalId: string;
  busy: boolean;
  loading: boolean;
  onAttach: (session: SessionSnapshot) => void;
  onRemove: (session: SessionSnapshot) => void;
  onRefresh: () => void;
  onClose: () => void;
}

function SessionsPanel({
  sessions,
  selectedTerminalId,
  busy,
  loading,
  onAttach,
  onRemove,
  onRefresh,
  onClose,
}: SessionsPanelProps) {
  const panelRef = useRef<HTMLElement>(null);

  return (
    <div className="dialog-backdrop" role="presentation" onMouseDown={onClose}>
      <section
        ref={panelRef}
        className="sessions-panel"
        role="dialog"
        aria-modal="true"
        aria-labelledby="sessions-title"
        aria-busy={loading}
        tabIndex={-1}
        onKeyDown={(event) =>
          handleModalKeyDown(event, panelRef.current, onClose)
        }
        onMouseDown={(event) => event.stopPropagation()}
      >
        <div className="sessions-title-row">
          <div>
            <h2 id="sessions-title">Terminal sessions</h2>
            <p>Attach without stopping the other agent terminals.</p>
          </div>
          <div className="sessions-title-actions">
            <button
              type="button"
              disabled={loading}
              onClick={onRefresh}
              aria-label="Refresh terminal sessions"
            >
              Refresh
            </button>
            <button
              type="button"
              autoFocus
              onClick={onClose}
              aria-label="Close terminal sessions"
            >
              ×
            </button>
          </div>
        </div>

        <div className="sessions-list">
          {sessions.map((candidate) => {
            const attached = candidate.terminalId === selectedTerminalId;
            return (
              <article
                key={candidate.terminalId}
                className={
                  attached
                    ? "session-card session-card--attached"
                    : "session-card"
                }
                aria-current={attached ? "true" : undefined}
              >
                <div className="session-card-main">
                  <div className="session-name-row">
                    <h3 title={candidate.name}>{candidate.name}</h3>
                    <span
                      className={`session-badge session-agent-badge session-agent-badge--${candidate.agent}`}
                    >
                      {agentLabel(candidate.agent)}
                    </span>
                    {candidate.isPrimary && (
                      <span className="session-badge">Primary</span>
                    )}
                    {candidate.purpose.kind === "peer" && (
                      <span className="session-badge session-badge--peer">
                        Peer review
                      </span>
                    )}
                    {attached && (
                      <span className="session-badge session-badge--attached">
                        Attached
                      </span>
                    )}
                  </div>
                  <div className="session-details">
                    <span
                      className={`session-lifecycle session-lifecycle--${candidate.status}`}
                    >
                      <span aria-hidden="true" />
                      {SESSION_STATUS_LABELS[candidate.status]}
                    </span>
                    <span>PID {candidate.pid ?? "—"}</span>
                    <span>
                      {candidate.connectedClients}{" "}
                      {candidate.connectedClients === 1 ? "client" : "clients"}
                    </span>
                  </div>
                  <div className="session-project" title={candidate.project}>
                    {candidate.project}
                  </div>
                </div>

                <div className="session-card-actions">
                  <button
                    type="button"
                    disabled={busy || attached}
                    onClick={() => onAttach(candidate)}
                  >
                    {attached ? "Attached" : "Attach"}
                  </button>
                  {!candidate.isPrimary && (
                    <button
                      type="button"
                      className="session-remove"
                      disabled={busy}
                      onClick={() => onRemove(candidate)}
                    >
                      {candidate.purpose.kind === "peer"
                        ? "Close review"
                        : "Remove"}
                    </button>
                  )}
                </div>
              </article>
            );
          })}

          {!loading && sessions.length === 0 && (
            <div className="sessions-empty">
              No terminal sessions are currently available.
            </div>
          )}
          {loading && sessions.length === 0 && (
            <div className="sessions-empty" role="status">
              Loading sessions…
            </div>
          )}
        </div>
      </section>
    </div>
  );
}

function AuthenticationScreen({ onToken }: { onToken: (token: string) => void }) {
  const [value, setValue] = useState("");
  const [error, setError] = useState("");

  const submit = (event: FormEvent) => {
    event.preventDefault();
    const token = value.trim();
    if (token.length < 16 || token.length > 512) {
      setError("Enter the token printed by the Codex Web Terminal server.");
      return;
    }
    onToken(token);
  };

  return (
    <main className="auth-screen">
      <form className="auth-card" onSubmit={submit}>
        <div className="auth-mark" aria-hidden="true">
          &gt;_
        </div>
        <h1>
          Codex Web Terminal
          <span className="community-label">Unofficial</span>
        </h1>
        <p>Enter the authentication token printed once in the server console.</p>
        <p className="auth-disclaimer">
          Independent community wrapper. Agent CLIs are installed separately.
        </p>
        <label htmlFor="token">Authentication token</label>
        <input
          id="token"
          name="token"
          type="password"
          autoComplete="off"
          spellCheck={false}
          value={value}
          onChange={(event) => setValue(event.target.value)}
          autoFocus
        />
        {error && <div className="form-error">{error}</div>}
        <button type="submit">Connect</button>
      </form>
    </main>
  );
}

interface SettingsPanelProps {
  settings: TerminalSettings;
  busy: boolean;
  agentLabel: string;
  onChange: (patch: Partial<TerminalSettings>) => void;
  onClose: () => void;
  onTerminate: () => void;
  onForgetToken: () => void;
  viewportDiagnostics: string;
  viewportDiagnosticsCollecting: boolean;
  viewportDiagnosticsCopyNotice: string | null;
  onCopyViewportDiagnostics: () => void;
}

function SettingsPanel({
  settings,
  busy,
  agentLabel,
  onChange,
  onClose,
  onTerminate,
  onForgetToken,
  viewportDiagnostics,
  viewportDiagnosticsCollecting,
  viewportDiagnosticsCopyNotice,
  onCopyViewportDiagnostics,
}: SettingsPanelProps) {
  const viewportDiagnosticsReady = Boolean(viewportDiagnostics);
  const panelRef = useRef<HTMLElement>(null);

  return (
    <div className="dialog-backdrop" role="presentation" onMouseDown={onClose}>
      <section
        ref={panelRef}
        className="settings-panel"
        role="dialog"
        aria-modal="true"
        aria-labelledby="settings-title"
        tabIndex={-1}
        onKeyDown={(event) =>
          handleModalKeyDown(event, panelRef.current, onClose)
        }
        onMouseDown={(event) => event.stopPropagation()}
      >
        <div className="settings-title-row">
          <h2 id="settings-title">Terminal settings</h2>
          <button
            type="button"
            autoFocus
            onClick={onClose}
            aria-label="Close settings"
          >
            ×
          </button>
        </div>

        <label>
          Font size
          <output>{settings.fontSize}px</output>
          <input
            type="range"
            min="11"
            max="24"
            value={settings.fontSize}
            onChange={(event) => onChange({ fontSize: Number(event.target.value) })}
          />
        </label>

        <label>
          Scrollback lines
          <select
            value={settings.scrollback}
            onChange={(event) => onChange({ scrollback: Number(event.target.value) })}
          >
            <option value="1000">1,000</option>
            <option value="10000">10,000</option>
            <option value="25000">25,000</option>
            <option value="50000">50,000</option>
          </select>
        </label>

        <label>
          Theme
          <select
            value={settings.theme}
            onChange={(event) =>
              onChange({ theme: event.target.value as ThemeName })
            }
          >
            <option value="windows">Windows Terminal</option>
            <option value="midnight">Midnight</option>
            <option value="high-contrast">High contrast</option>
          </select>
        </label>

        <label className="checkbox-row">
          <input
            type="checkbox"
            checked={settings.cursorBlink}
            onChange={(event) => onChange({ cursorBlink: event.target.checked })}
          />
          Blinking cursor
        </label>

        <label className="checkbox-row">
          <input
            type="checkbox"
            checked={settings.mobileKeys}
            onChange={(event) => onChange({ mobileKeys: event.target.checked })}
          />
          Show mobile keys
        </label>

        <div className="diagnostics-settings">
          <div>
            <strong>Mobile viewport diagnostics</strong>
            <span>
              {viewportDiagnosticsCopyNotice ??
                (viewportDiagnosticsCollecting
                  ? "Collecting… wait a moment"
                  : viewportDiagnosticsReady
                    ? "Ready to copy"
                    : "Tap the terminal and wait two seconds")}
            </span>
          </div>
          <button
            type="button"
            disabled={!viewportDiagnosticsReady}
            onClick={onCopyViewportDiagnostics}
          >
            Copy diagnostics
          </button>
          {viewportDiagnosticsReady && (
            <details className="diagnostics-manual-copy">
              <summary>Show diagnostics text</summary>
              <textarea
                readOnly
                value={viewportDiagnostics}
                aria-label="Viewport diagnostics text"
                onFocus={(event) => event.currentTarget.select()}
                onClick={(event) => event.currentTarget.select()}
              />
            </details>
          )}
        </div>

        <div className="settings-danger">
          <button type="button" disabled={busy} onClick={onTerminate}>
            Terminate {agentLabel}
          </button>
          <button type="button" onClick={onForgetToken}>
            Forget token
          </button>
        </div>
      </section>
    </div>
  );
}

function handleModalKeyDown(
  event: ReactKeyboardEvent<HTMLElement>,
  panel: HTMLElement | null,
  onClose: () => void,
) {
  if (event.key === "Escape") {
    event.preventDefault();
    onClose();
    return;
  }
  if (event.key !== "Tab" || !panel) {
    return;
  }

  const focusable = Array.from(
    panel.querySelectorAll<HTMLElement>(
      'button:not([disabled]), input:not([disabled]), select:not([disabled]), textarea:not([disabled]), a[href], [tabindex]:not([tabindex="-1"])',
    ),
  ).filter(
    (element) =>
      !element.hasAttribute("hidden") && element.getClientRects().length > 0,
  );
  const first = focusable[0];
  const last = focusable.at(-1);
  if (!first || !last) {
    event.preventDefault();
    panel.focus({ preventScroll: true });
    return;
  }

  if (event.shiftKey && document.activeElement === first) {
    event.preventDefault();
    last.focus({ preventScroll: true });
  } else if (!event.shiftKey && document.activeElement === last) {
    event.preventDefault();
    first.focus({ preventScroll: true });
  }
}
