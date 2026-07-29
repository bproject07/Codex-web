import {
  forwardRef,
  useEffect,
  useImperativeHandle,
  useRef,
  useState,
} from "react";
import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import { WebLinksAddon } from "@xterm/addon-web-links";
import {
  ApiError,
  getSession,
  normalizeSessionSnapshot,
  type SessionSnapshot,
  websocketUrl,
} from "../api";
import {
  encodeControlMessage,
  encodeTerminalInput,
  parseServerControl,
} from "./protocol";
import {
  reconnectDelay,
  type ConnectionStatus,
} from "./reconnect";
import { applyCtrlToInput } from "./mobileKeys";
import { isMobileRowOnlyResize } from "./mobileResize";
import { takeReplayBatch, type BufferedReplay } from "./replay";
import {
  TERMINAL_THEMES,
  type TerminalSettings,
} from "./settings";
import {
  installAndroidImeGuard,
  shouldEnableAndroidImeGuard,
} from "./androidImeGuard";

export interface TerminalViewHandle {
  send: (data: string) => void;
  focus: () => void;
  fit: () => void;
  scrollPages: (pageCount: number) => void;
  scrollToTop: () => void;
  scrollToBottom: () => void;
  inspect: () => TerminalDiagnostics | null;
}

export interface TerminalDiagnostics {
  cols: number;
  rows: number;
  bufferType: "normal" | "alternate";
  viewportY: number;
  baseY: number;
  cursorY: number;
  bufferLength: number;
  replayCount: number;
  atomicMobileResizeCommits: number;
  androidImeGuardEnabled: boolean;
  androidImeDuplicateInputsSuppressed: number;
  androidDuplicateEntersSuppressed: number;
  androidSoftEntersTranslated: number;
  ptyCols: number | null;
  ptyRows: number | null;
}

interface TerminalViewProps {
  token: string;
  terminalId: string;
  settings: TerminalSettings;
  reconnectNonce: number;
  ctrlMode: boolean;
  onCtrlConsumed: () => void;
  onConnectionStatus: (status: ConnectionStatus) => void;
  onSession: (session: SessionSnapshot) => void;
  onSessionUnavailable: (terminalId: string) => void;
  onError: (message: string | null) => void;
}

interface MobileResizeCapture extends BufferedReplay {
  socket: WebSocket;
  quietTimer: number | null;
  hardTimer: number | null;
}

const MOBILE_RESIZE_QUIET_MS = 180;
const MOBILE_RESIZE_INITIAL_WAIT_MS = 500;
const MOBILE_RESIZE_HARD_LIMIT_MS = 2_500;

function createTerminalFreezeFrame(
  container: HTMLDivElement,
): HTMLDivElement | null {
  const terminalElement = container.querySelector<HTMLElement>(".xterm");
  if (!terminalElement) {
    return null;
  }

  const containerRect = container.getBoundingClientRect();
  const terminalRect = terminalElement.getBoundingClientRect();
  const frame = document.createElement("div");
  frame.className = "terminal-atomic-frame";
  frame.setAttribute("aria-hidden", "true");
  frame.style.left = `${terminalRect.left - containerRect.left}px`;
  frame.style.top = `${terminalRect.top - containerRect.top}px`;
  frame.style.width = `${terminalRect.width}px`;
  frame.style.height = `${terminalRect.height}px`;

  const clone = terminalElement.cloneNode(true) as HTMLElement;
  clone.querySelectorAll<HTMLElement>("textarea, input, button, a").forEach(
    (element) => {
      element.setAttribute("tabindex", "-1");
    },
  );

  const sourceCanvases =
    terminalElement.querySelectorAll<HTMLCanvasElement>("canvas");
  const clonedCanvases = clone.querySelectorAll<HTMLCanvasElement>("canvas");
  let copiedCanvasCount = 0;
  sourceCanvases.forEach((sourceCanvas, index) => {
    const clonedCanvas = clonedCanvases.item(index);
    if (!clonedCanvas || sourceCanvas.width < 1 || sourceCanvas.height < 1) {
      return;
    }

    clonedCanvas.width = sourceCanvas.width;
    clonedCanvas.height = sourceCanvas.height;
    const context = clonedCanvas.getContext("2d");
    if (!context) {
      return;
    }

    context.drawImage(sourceCanvas, 0, 0);
    copiedCanvasCount += 1;
  });
  frame.dataset.canvasCount = String(sourceCanvases.length);
  frame.dataset.copiedCanvasCount = String(copiedCanvasCount);
  frame.appendChild(clone);

  const sourceViewport =
    terminalElement.querySelector<HTMLElement>(".xterm-viewport");
  const clonedViewport = clone.querySelector<HTMLElement>(".xterm-viewport");
  if (sourceViewport && clonedViewport) {
    clonedViewport.scrollTop = sourceViewport.scrollTop;
  }

  container.appendChild(frame);
  return frame;
}

function syncTextareaToCursor(terminal: Terminal): void {
  const textarea = terminal.textarea;
  const screen = terminal.element?.querySelector<HTMLElement>(".xterm-screen");
  const buffer = terminal.buffer.active;
  const absoluteCursorY = buffer.baseY + buffer.cursorY;
  const cursorIsVisible =
    absoluteCursorY >= buffer.viewportY &&
    absoluteCursorY < buffer.viewportY + terminal.rows;

  if (!textarea || !screen || terminal.rows < 1 || !cursorIsVisible) {
    return;
  }

  const cellHeight = screen.getBoundingClientRect().height / terminal.rows;
  if (!Number.isFinite(cellHeight) || cellHeight <= 0) {
    return;
  }

  // xterm normally moves its hidden input during the next renderer pass.
  // On Android that pass can be delayed while a large replay is being parsed,
  // leaving the focused textarea below the newly shrunken visual viewport.
  textarea.style.top = `${buffer.cursorY * cellHeight}px`;
  textarea.style.height = `${cellHeight}px`;
  textarea.style.lineHeight = `${cellHeight}px`;
}

export const TerminalView = forwardRef<TerminalViewHandle, TerminalViewProps>(
  function TerminalView(
    {
      token,
      terminalId,
      settings,
      reconnectNonce,
      ctrlMode,
      onCtrlConsumed,
      onConnectionStatus,
      onSession,
      onSessionUnavailable,
      onError,
    },
    forwardedRef,
  ) {
    const containerRef = useRef<HTMLDivElement>(null);
    const terminalRef = useRef<Terminal | null>(null);
    const fitAddonRef = useRef<FitAddon | null>(null);
    const socketRef = useRef<WebSocket | null>(null);
    const fitFrameRef = useRef<number | null>(null);
    const fitTimerRef = useRef<number | null>(null);
    const fitBurstActiveRef = useRef(false);
    const lastFitHeightRef = useRef<number | null>(null);
    const lastSentSizeRef = useRef<{
      socket: WebSocket;
      cols: number;
      rows: number;
    } | null>(null);
    const mobileResizeCaptureRef = useRef<MobileResizeCapture | null>(null);
    const atomicMobileResizeCommitsRef = useRef(0);
    const androidImeGuardEnabledRef = useRef(false);
    const androidImeDuplicateInputsSuppressedRef = useRef(0);
    const androidDuplicateEntersSuppressedRef = useRef(0);
    const androidSoftEntersTranslatedRef = useRef(0);
    const freezeFrameRef = useRef<HTMLDivElement | null>(null);
    const freezeFrameTimerRef = useRef<number | null>(null);
    const replayCountRef = useRef(0);
    const replayRef = useRef<BufferedReplay | null>(null);
    const ctrlModeRef = useRef(ctrlMode);
    const [isRestoring, setIsRestoring] = useState(false);
    const callbackRef = useRef({
      onCtrlConsumed,
      onConnectionStatus,
      onSession,
      onSessionUnavailable,
      onError,
    });

    ctrlModeRef.current = ctrlMode;
    callbackRef.current = {
      onCtrlConsumed,
      onConnectionStatus,
      onSession,
      onSessionUnavailable,
      onError,
    };

    const sendToSocket = (data: string) => {
      const socket = socketRef.current;
      if (socket?.readyState === WebSocket.OPEN) {
        socket.send(encodeTerminalInput(data));
      }
    };

    const send = (data: string) => {
      sendToSocket(data);
      terminalRef.current?.focus();
    };

    const removeFreezeFrame = () => {
      if (freezeFrameTimerRef.current !== null) {
        window.clearTimeout(freezeFrameTimerRef.current);
        freezeFrameTimerRef.current = null;
      }
      freezeFrameRef.current?.remove();
      freezeFrameRef.current = null;
    };

    const clearMobileResizeTimers = (capture: MobileResizeCapture) => {
      if (capture.quietTimer !== null) {
        window.clearTimeout(capture.quietTimer);
        capture.quietTimer = null;
      }
      if (capture.hardTimer !== null) {
        window.clearTimeout(capture.hardTimer);
        capture.hardTimer = null;
      }
    };

    const cancelMobileResizeCapture = () => {
      const capture = mobileResizeCaptureRef.current;
      if (capture) {
        clearMobileResizeTimers(capture);
        mobileResizeCaptureRef.current = null;
      }
    };

    const commitMobileResizeCapture = (capture: MobileResizeCapture) => {
      if (mobileResizeCaptureRef.current !== capture) {
        return;
      }
      mobileResizeCaptureRef.current = null;
      clearMobileResizeTimers(capture);

      const bytes = takeReplayBatch(capture, capture.byteLength);
      const terminal = terminalRef.current;
      const container = containerRef.current;
      if (bytes.byteLength === 0 || !terminal || !container) {
        return;
      }

      removeFreezeFrame();
      const frame = createTerminalFreezeFrame(container);
      freezeFrameRef.current = frame;
      if (frame) {
        freezeFrameTimerRef.current = window.setTimeout(() => {
          if (freezeFrameRef.current === frame) {
            removeFreezeFrame();
          }
        }, 5_000);
      }
      atomicMobileResizeCommitsRef.current += 1;
      terminal.write(bytes, () => {
        syncTextareaToCursor(terminal);
        window.requestAnimationFrame(() => {
          window.requestAnimationFrame(() => {
            if (freezeFrameRef.current === frame) {
              removeFreezeFrame();
            }
          });
        });
      });
    };

    const scheduleMobileResizeCommit = (
      capture: MobileResizeCapture,
      delay = MOBILE_RESIZE_QUIET_MS,
    ) => {
      if (capture.quietTimer !== null) {
        window.clearTimeout(capture.quietTimer);
      }
      capture.quietTimer = window.setTimeout(() => {
        capture.quietTimer = null;
        commitMobileResizeCapture(capture);
      }, delay);
    };

    const beginMobileResizeCapture = (socket: WebSocket) => {
      const current = mobileResizeCaptureRef.current;
      if (current?.socket === socket) {
        scheduleMobileResizeCommit(current, MOBILE_RESIZE_INITIAL_WAIT_MS);
        return;
      }
      if (current) {
        commitMobileResizeCapture(current);
      }

      const capture: MobileResizeCapture = {
        socket,
        chunks: [],
        byteLength: 0,
        quietTimer: null,
        hardTimer: null,
      };
      mobileResizeCaptureRef.current = capture;
      scheduleMobileResizeCommit(capture, MOBILE_RESIZE_INITIAL_WAIT_MS);
      capture.hardTimer = window.setTimeout(() => {
        capture.hardTimer = null;
        commitMobileResizeCapture(capture);
      }, MOBILE_RESIZE_HARD_LIMIT_MS);
    };

    const captureMobileResizeOutput = (
      socket: WebSocket,
      bytes: Uint8Array,
    ): boolean => {
      const capture = mobileResizeCaptureRef.current;
      if (!capture || capture.socket !== socket) {
        return false;
      }
      capture.chunks.push(bytes);
      capture.byteLength += bytes.byteLength;
      scheduleMobileResizeCommit(capture);
      return true;
    };

    const performFit = () => {
      const terminal = terminalRef.current;
      const fitAddon = fitAddonRef.current;
      const container = containerRef.current;
      if (!terminal || !fitAddon || !container || container.clientHeight < 1) {
        return;
      }

      try {
        fitAddon.fit();
        lastFitHeightRef.current = container.clientHeight;
        syncTextareaToCursor(terminal);
        const socket = socketRef.current;
        if (socket?.readyState === WebSocket.OPEN) {
          const lastSize = lastSentSizeRef.current;
          const sizeChanged =
            !lastSize ||
            lastSize.socket !== socket ||
            lastSize.cols !== terminal.cols ||
            lastSize.rows !== terminal.rows;

          if (sizeChanged) {
            const mobileRowOnlyResize =
              lastSize?.socket === socket &&
              isMobileRowOnlyResize(
                lastSize,
                terminal,
                window.matchMedia("(pointer: coarse)").matches,
              );
            if (mobileRowOnlyResize) {
              beginMobileResizeCapture(socket);
            }

            socket.send(
              encodeControlMessage({
                type: "resize",
                cols: terminal.cols,
                rows: terminal.rows,
              }),
            );
            lastSentSizeRef.current = {
              socket,
              cols: terminal.cols,
              rows: terminal.rows,
            };
          }
        }
      } catch {
        // A zero-sized element during mobile viewport animation is harmless;
        // the trailing fit retries after the resize burst settles.
      }
    };

    const scheduleFitFrame = () => {
      if (fitFrameRef.current !== null) {
        return;
      }
      fitFrameRef.current = window.requestAnimationFrame(() => {
        fitFrameRef.current = null;
        performFit();
      });
    };

    const fit = () => {
      if (!fitBurstActiveRef.current) {
        fitBurstActiveRef.current = true;
        const containerHeight = containerRef.current?.clientHeight ?? 0;
        const lastFitHeight = lastFitHeightRef.current;
        const viewportIsGrowing =
          lastFitHeight !== null && containerHeight > lastFitHeight + 1;

        // Shrinking must be immediate so Android keeps its focused textarea
        // inside the keyboard-sized viewport. Growing can wait until the
        // keyboard-close animation settles, avoiding two visible PTY redraws.
        if (!viewportIsGrowing) {
          scheduleFitFrame();
        }
      }

      if (fitTimerRef.current !== null) {
        window.clearTimeout(fitTimerRef.current);
      }
      fitTimerRef.current = window.setTimeout(() => {
        fitTimerRef.current = null;
        fitBurstActiveRef.current = false;
        scheduleFitFrame();
      }, 120);
    };

    useImperativeHandle(
      forwardedRef,
      () => ({
        send,
        focus: () => terminalRef.current?.focus(),
        fit,
        scrollPages: (pageCount) => terminalRef.current?.scrollPages(pageCount),
        scrollToTop: () => terminalRef.current?.scrollToTop(),
        scrollToBottom: () => terminalRef.current?.scrollToBottom(),
        inspect: () => {
          const terminal = terminalRef.current;
          if (!terminal) {
            return null;
          }
          const buffer = terminal.buffer.active;
          return {
            cols: terminal.cols,
            rows: terminal.rows,
            bufferType: buffer.type,
            viewportY: buffer.viewportY,
            baseY: buffer.baseY,
            cursorY: buffer.cursorY,
            bufferLength: buffer.length,
            replayCount: replayCountRef.current,
            atomicMobileResizeCommits:
              atomicMobileResizeCommitsRef.current,
            androidImeGuardEnabled: androidImeGuardEnabledRef.current,
            androidImeDuplicateInputsSuppressed:
              androidImeDuplicateInputsSuppressedRef.current,
            androidDuplicateEntersSuppressed:
              androidDuplicateEntersSuppressedRef.current,
            androidSoftEntersTranslated:
              androidSoftEntersTranslatedRef.current,
            ptyCols: lastSentSizeRef.current?.cols ?? null,
            ptyRows: lastSentSizeRef.current?.rows ?? null,
          };
        },
      }),
      [],
    );

    useEffect(() => {
      const container = containerRef.current;
      if (!container) {
        return;
      }

      const terminal = new Terminal({
        cursorBlink: settings.cursorBlink,
        convertEol: false,
        scrollback: settings.scrollback,
        scrollOnUserInput: false,
        smoothScrollDuration: 0,
        fontFamily:
          '"Cascadia Mono", "Cascadia Code", Consolas, "Roboto Mono", "Noto Sans Mono", "Droid Sans Mono", monospace',
        fontSize: settings.fontSize,
        theme: TERMINAL_THEMES[settings.theme],
        allowProposedApi: false,
      });
      const fitAddon = new FitAddon();
      terminal.loadAddon(fitAddon);
      terminal.loadAddon(new WebLinksAddon());
      terminal.open(container);

      terminalRef.current = terminal;
      fitAddonRef.current = fitAddon;

      const textarea = terminal.textarea;
      const androidImeGuardEnabled = shouldEnableAndroidImeGuard(
        navigator.userAgent,
      );
      androidImeGuardEnabledRef.current = androidImeGuardEnabled;
      const androidImeGuard = textarea
        ? installAndroidImeGuard(container, textarea, {
            enabled: androidImeGuardEnabled,
            onTerminalInput: (data) => {
              terminal.input(data, true);
            },
            onDuplicateInputSuppressed: () => {
              androidImeDuplicateInputsSuppressedRef.current += 1;
            },
            onDuplicateEnterSuppressed: () => {
              androidDuplicateEntersSuppressedRef.current += 1;
            },
            onSoftEnterTranslated: () => {
              androidSoftEntersTranslatedRef.current += 1;
            },
          })
        : null;

      const inputDisposable = terminal.onData((data) => {
        androidImeGuard?.observeTerminalData(data);
        if (ctrlModeRef.current) {
          const converted = applyCtrlToInput(data);
          if (converted.consumed) {
            callbackRef.current.onCtrlConsumed();
            sendToSocket(converted.data);
            return;
          }
        }
        sendToSocket(data);
      });

      const resizeObserver = new ResizeObserver(fit);
      resizeObserver.observe(container);
      void document.fonts?.ready.then(fit);
      fit();
      if (!window.matchMedia("(pointer: coarse)").matches) {
        terminal.focus();
      }

      return () => {
        androidImeGuard?.dispose();
        androidImeGuardEnabledRef.current = false;
        inputDisposable.dispose();
        resizeObserver.disconnect();
        if (fitFrameRef.current !== null) {
          window.cancelAnimationFrame(fitFrameRef.current);
          fitFrameRef.current = null;
        }
        if (fitTimerRef.current !== null) {
          window.clearTimeout(fitTimerRef.current);
          fitTimerRef.current = null;
        }
        fitBurstActiveRef.current = false;
        lastFitHeightRef.current = null;
        cancelMobileResizeCapture();
        removeFreezeFrame();
        terminal.dispose();
        terminalRef.current = null;
        fitAddonRef.current = null;
        replayRef.current = null;
        lastSentSizeRef.current = null;
      };
    }, []);

    useEffect(() => {
      const terminal = terminalRef.current;
      if (!terminal) {
        return;
      }
      terminal.options.fontSize = settings.fontSize;
      terminal.options.cursorBlink = settings.cursorBlink;
      terminal.options.scrollback = settings.scrollback;
      terminal.options.theme = TERMINAL_THEMES[settings.theme];
      fit();
    }, [
      settings.cursorBlink,
      settings.fontSize,
      settings.scrollback,
      settings.theme,
    ]);

    useEffect(() => {
      let disposed = false;
      let retryTimer: number | null = null;
      let heartbeatTimer: number | null = null;
      let activeSocket: WebSocket | null = null;
      let attempt = 0;
      let abortController: AbortController | null = null;

      const revealRestoredTerminal = () => {
        if (disposed) {
          return;
        }
        terminalRef.current?.scrollToBottom();
        fit();
        window.requestAnimationFrame(() => {
          if (!disposed) {
            setIsRestoring(false);
          }
        });
      };

      const drainReplay = (replay: BufferedReplay) => {
        if (disposed || replayRef.current !== replay) {
          return;
        }

        const bytes = takeReplayBatch(replay);
        if (bytes.byteLength === 0) {
          replayRef.current = null;
          revealRestoredTerminal();
          return;
        }

        const terminal = terminalRef.current;
        if (!terminal) {
          replayRef.current = null;
          revealRestoredTerminal();
          return;
        }
        terminal.write(bytes, () => {
          window.requestAnimationFrame(() => drainReplay(replay));
        });
      };

      const cancelReplay = () => {
        replayRef.current = null;
        if (!disposed) {
          setIsRestoring(false);
        }
      };

      const clearSocketTimers = () => {
        if (heartbeatTimer !== null) {
          window.clearInterval(heartbeatTimer);
          heartbeatTimer = null;
        }
      };

      const scheduleReconnect = () => {
        if (disposed || retryTimer !== null) {
          return;
        }
        callbackRef.current.onConnectionStatus("reconnecting");
        const delay = reconnectDelay(attempt);
        attempt += 1;
        retryTimer = window.setTimeout(() => {
          retryTimer = null;
          void connect(true);
        }, delay);
      };

      const connect = async (retry: boolean) => {
        if (disposed) {
          return;
        }

        callbackRef.current.onConnectionStatus(
          retry ? "reconnecting" : "connecting",
        );
        callbackRef.current.onError(null);
        const requestController = new AbortController();
        abortController = requestController;

        try {
          const session = await getSession(
            token,
            terminalId,
            requestController.signal,
          );
          if (disposed || abortController !== requestController) {
            return;
          }
          callbackRef.current.onSession(session);
        } catch (error) {
          if (disposed || abortController !== requestController) {
            return;
          }
          if (error instanceof ApiError && error.status === 404) {
            callbackRef.current.onConnectionStatus("disconnected");
            callbackRef.current.onError(
              "The selected terminal no longer exists. Returning to the primary terminal.",
            );
            callbackRef.current.onSessionUnavailable(terminalId);
            return;
          }
          if (error instanceof ApiError && (error.status === 401 || error.status === 429)) {
            callbackRef.current.onConnectionStatus("authentication_failed");
            callbackRef.current.onError(
              error.status === 429
                ? "Too many failed authentication attempts. Wait one minute."
                : "Authentication failed. Open the URL printed by the server.",
            );
            return;
          }
          callbackRef.current.onConnectionStatus("disconnected");
          scheduleReconnect();
          return;
        }

        const nextSocket = new WebSocket(websocketUrl(token, terminalId));
        activeSocket = nextSocket;
        nextSocket.binaryType = "arraybuffer";
        socketRef.current = nextSocket;

        nextSocket.onopen = () => {
          if (disposed || nextSocket !== socketRef.current) {
            return;
          }
          attempt = 0;
          lastSentSizeRef.current = null;
          cancelMobileResizeCapture();
          removeFreezeFrame();
          callbackRef.current.onConnectionStatus("connected");
          callbackRef.current.onError(null);
          fit();
          heartbeatTimer = window.setInterval(() => {
            if (
              nextSocket === socketRef.current &&
              nextSocket.readyState === WebSocket.OPEN
            ) {
              nextSocket.send(encodeControlMessage({ type: "ping" }));
            }
          }, 20_000);
        };

        nextSocket.onmessage = (event: MessageEvent<string | ArrayBuffer>) => {
          if (disposed || nextSocket !== socketRef.current) {
            return;
          }
          if (typeof event.data !== "string") {
            const bytes = new Uint8Array(event.data);
            const replay = replayRef.current;
            if (replay) {
              replay.chunks.push(bytes);
              replay.byteLength += bytes.byteLength;
            } else if (!captureMobileResizeOutput(nextSocket, bytes)) {
              terminalRef.current?.write(bytes);
            }
            return;
          }

          const message = parseServerControl(event.data);
          if (!message) {
            callbackRef.current.onError("The server sent an invalid control message.");
            return;
          }

          switch (message.type) {
            case "session": {
              const nextSession = normalizeSessionSnapshot(
                message.session,
                terminalId,
              );
              if (nextSession.terminalId !== terminalId) {
                callbackRef.current.onError(
                  "The server returned a different terminal session.",
                );
                return;
              }
              callbackRef.current.onSession(nextSession);
              break;
            }
            case "replay_start": {
              replayCountRef.current += 1;
              cancelMobileResizeCapture();
              removeFreezeFrame();
              const replay: BufferedReplay = {
                chunks: [],
                byteLength: 0,
              };
              replayRef.current = replay;
              containerRef.current?.classList.add("terminal-view--covered");
              setIsRestoring(true);
              terminalRef.current?.reset();
              terminalRef.current?.clear();
              break;
            }
            case "replay_end": {
              const replay = replayRef.current;
              if (replay) {
                drainReplay(replay);
              } else {
                revealRestoredTerminal();
              }
              break;
            }
            case "error":
              callbackRef.current.onError(message.message);
              break;
            case "pong":
              break;
          }
        };

        nextSocket.onerror = () => {
          // onclose performs the state transition and retry scheduling.
        };

        nextSocket.onclose = () => {
          if (disposed || nextSocket !== socketRef.current) {
            return;
          }
          clearSocketTimers();
          cancelReplay();
          cancelMobileResizeCapture();
          removeFreezeFrame();
          socketRef.current = null;
          lastSentSizeRef.current = null;
          callbackRef.current.onConnectionStatus("disconnected");
          scheduleReconnect();
        };
      };

      void connect(false);

      return () => {
        disposed = true;
        replayRef.current = null;
        setIsRestoring(false);
        cancelMobileResizeCapture();
        removeFreezeFrame();
        abortController?.abort();
        if (retryTimer !== null) {
          window.clearTimeout(retryTimer);
        }
        clearSocketTimers();
        if (activeSocket) {
          activeSocket.onmessage = null;
          activeSocket.onclose = null;
          activeSocket.close(1000, "client reconnect");
        }
        if (socketRef.current === activeSocket) {
          socketRef.current = null;
          lastSentSizeRef.current = null;
        }
      };
    }, [token, terminalId, reconnectNonce]);

    return (
      <>
        <div
          ref={containerRef}
          className={
            isRestoring
              ? "terminal-view terminal-view--covered"
              : "terminal-view"
          }
          aria-label="Codex terminal"
        />
        {isRestoring && (
          <div className="terminal-restore-status" role="status" aria-live="polite">
            <span className="terminal-restore-spinner" aria-hidden="true" />
            Restoring terminal…
          </div>
        )}
      </>
    );
  },
);
