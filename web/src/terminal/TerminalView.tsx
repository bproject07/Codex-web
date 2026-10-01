import {
  forwardRef,
  useEffect,
  useImperativeHandle,
  useLayoutEffect,
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
  AUTH_RETRY_DELAY_MS,
  reconnectDelay,
  type ConnectionStatus,
} from "./reconnect";
import { applyCtrlToInput } from "./mobileKeys";
import { terminalDimensions } from "./dimensions";
import { createHeartbeat } from "./heartbeat";
import {
  isMobileRowOnlyResize,
  terminalScrollbarOptions,
} from "./mobileResize";
import {
  createMobileScrollbarVisibilityController,
  type MobileScrollbarVisibilityController,
} from "./mobileScrollbar";
import { takeReplayBatch, type BufferedReplay } from "./replay";
import {
  TERMINAL_THEMES,
  type TerminalSettings,
} from "./settings";
import {
  installAndroidImeGuard,
  shouldEnableAndroidImeGuard,
  type AndroidImeGuardDisposable,
} from "./androidImeGuard";

export interface TerminalViewHandle {
  send: (data: string) => void;
  focus: () => void;
  fit: () => void;
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
  androidImeReplacementsTranslated: number;
  ptyCols: number | null;
  ptyRows: number | null;
}

interface TerminalViewProps {
  token: string;
  terminalId: string;
  sessionId: string | null;
  active: boolean;
  settings: TerminalSettings;
  ctrlMode: boolean;
  onCtrlConsumed: () => void;
  onConnectionStatus: (status: ConnectionStatus) => void;
  onConnected: () => void;
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
      sessionId,
      active,
      settings,
      ctrlMode,
      onCtrlConsumed,
      onConnectionStatus,
      onConnected,
      onSession,
      onSessionUnavailable,
      onError,
    },
    forwardedRef,
  ) {
    const containerRef = useRef<HTMLDivElement>(null);
    const terminalRef = useRef<Terminal | null>(null);
    const flowControlledSocketsRef = useRef(new WeakSet<WebSocket>());
    const acknowledgeOutput = (socket: WebSocket | null, bytes: number) => {
      if (bytes > 0 && socket && socket === socketRef.current &&
          socket.readyState === WebSocket.OPEN && flowControlledSocketsRef.current.has(socket)) {
        socket.send(encodeControlMessage({ type: "output_ack", bytes }));
      }
    };
    const fitAddonRef = useRef<FitAddon | null>(null);
    const socketRef = useRef<WebSocket | null>(null);
    const retryAuthenticationOnActivationRef = useRef<(() => void) | null>(null);
    const fitFrameRef = useRef<number | null>(null);
    const fitTimerRef = useRef<number | null>(null);
    const fitBurstActiveRef = useRef(false);
    const scrollBoundaryRef = useRef<-1 | 1 | null>(null);
    const scrollBoundaryFrameRef = useRef<number | null>(null);
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
    const androidImeReplacementsTranslatedRef = useRef(0);
    const freezeFrameRef = useRef<HTMLDivElement | null>(null);
    const freezeFrameTimerRef = useRef<number | null>(null);
    const replayCountRef = useRef(0);
    const replayRef = useRef<BufferedReplay | null>(null);
    const ctrlModeRef = useRef(ctrlMode);
    const activeRef = useRef(active);
    const [isRestoring, setIsRestoring] = useState(false);
    const [mobileScrollbarEnabled] = useState(
      () =>
        typeof window !== "undefined" &&
        typeof window.matchMedia === "function" &&
        window.matchMedia("(pointer: coarse)").matches,
    );
    const [mobileScrollbarVisible, setMobileScrollbarVisible] =
      useState(false);
    const callbackRef = useRef({
      onCtrlConsumed,
      onConnectionStatus,
      onConnected,
      onSession,
      onSessionUnavailable,
      onError,
    });

    ctrlModeRef.current = ctrlMode;
    activeRef.current = active;
    callbackRef.current = {
      onCtrlConsumed,
      onConnectionStatus,
      onConnected,
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
      if (!activeRef.current) {
        return;
      }
      sendToSocket(data);
      terminalRef.current?.focus();
    };

    const cancelScrollBoundary = () => {
      scrollBoundaryRef.current = null;
      if (scrollBoundaryFrameRef.current !== null) {
        window.cancelAnimationFrame(scrollBoundaryFrameRef.current);
        scrollBoundaryFrameRef.current = null;
      }
    };

    const settleScrollBoundary = () => {
      if (scrollBoundaryRef.current === null || !activeRef.current) return;
      if (scrollBoundaryFrameRef.current !== null) {
        window.cancelAnimationFrame(scrollBoundaryFrameRef.current);
      }
      // xterm updates viewport scroll dimensions in its render callback.
      // Wait through that render, then apply the edge using the new dimensions.
      scrollBoundaryFrameRef.current = window.requestAnimationFrame(() => {
        scrollBoundaryFrameRef.current = window.requestAnimationFrame(() => {
          scrollBoundaryFrameRef.current = null;
          const terminal = terminalRef.current;
          const direction = scrollBoundaryRef.current;
          if (terminal && direction !== null && activeRef.current) {
            terminal.scrollLines(direction * terminal.buffer.active.length);
          }
          if (!fitBurstActiveRef.current && fitFrameRef.current === null) {
            scrollBoundaryRef.current = null;
          }
        });
      });
    };

    const scrollToBoundary = (direction: -1 | 1) => {
      const terminal = terminalRef.current;
      if (terminal) {
        // xterm 6's relative scrollToBottom delta can use a stale viewport
        // position after background output. Clamp to the requested edge using
        // the full buffer length through its public scrolling API.
        terminal.scrollLines(direction * terminal.buffer.active.length);
        if (activeRef.current) {
          scrollBoundaryRef.current = direction;
          settleScrollBoundary();
        }
      }
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
        acknowledgeOutput(capture.socket, capture.byteLength);
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
      const frame = activeRef.current ? createTerminalFreezeFrame(container) : null;
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
        acknowledgeOutput(capture.socket, bytes.byteLength);
        if (activeRef.current) {
          syncTextareaToCursor(terminal);
        }
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
      if (capture.byteLength >= 256 * 1024) {
        commitMobileResizeCapture(capture);
        return true;
      }
      scheduleMobileResizeCommit(capture);
      return true;
    };

    const performFit = () => {
      if (!activeRef.current) {
        return;
      }
      try {
        const terminal = terminalRef.current;
        const fitAddon = fitAddonRef.current;
        const container = containerRef.current;
        if (!terminal || !fitAddon || !container || container.clientHeight < 1) {
          return;
        }
        const proposed = fitAddon.proposeDimensions();
        if (!proposed || !Number.isFinite(proposed.cols) || !Number.isFinite(proposed.rows)) return;
        const size = terminalDimensions(proposed.cols, proposed.rows);
        if (size.cols === proposed.cols && size.rows === proposed.rows) {
          // Keep FitAddon's renderer invalidation when revealing a cached view.
          fitAddon.fit();
        } else {
          if (terminal.cols !== size.cols || terminal.rows !== size.rows) {
            terminal.resize(size.cols, size.rows);
          }
          // Revealing a retained clamped view still needs a renderer refresh,
          // even when its bounded dimensions have not changed.
          terminal.refresh(0, terminal.rows - 1);
        }
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
                mobileScrollbarEnabled,
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
      } finally {
        // A skipped or failed trailing fit must finish the edge request too,
        // so an unrelated later resize cannot revive a stale Top/Live intent.
        settleScrollBoundary();
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
      if (!activeRef.current) {
        return;
      }
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
        focus: () => {
          if (activeRef.current) terminalRef.current?.focus();
        },
        fit,
        scrollToTop: () => {
          if (activeRef.current) scrollToBoundary(-1);
        },
        scrollToBottom: () => {
          if (activeRef.current) scrollToBoundary(1);
        },
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
            androidImeReplacementsTranslated:
              androidImeReplacementsTranslatedRef.current,
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
        ...terminalScrollbarOptions(mobileScrollbarEnabled),
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

      // A newer manual interaction cancels an edge request still waiting on
      // layout. Never pull a user back to Live after they start scrolling up.
      const scrollIntentEvents = ["wheel", "pointerdown", "touchstart", "keydown"];
      for (const name of scrollIntentEvents) {
        container.addEventListener(name, cancelScrollBoundary, { capture: true, passive: true });
      }

      let mobileScrollbarController:
        | MobileScrollbarVisibilityController
        | null = null;
      let removeMobileScrollbarListeners: (() => void) | null = null;
      if (mobileScrollbarEnabled) {
        const scrollbar =
          terminal.element?.querySelector<HTMLElement>(
            ".xterm-scrollable-element > .scrollbar.vertical",
          ) ?? null;
        if (scrollbar) {
          mobileScrollbarController =
            createMobileScrollbarVisibilityController(
              setMobileScrollbarVisible,
              {
                setTimeout: (callback, delay) =>
                  window.setTimeout(callback, delay),
                clearTimeout: (timer) => window.clearTimeout(timer),
              },
            );

          const isScrollbarTarget = (event: Event) =>
            event.target instanceof window.Node &&
            scrollbar.contains(event.target);
          const onPointerDown = (event: PointerEvent) => {
            if (isScrollbarTarget(event)) {
              mobileScrollbarController?.pointerDown(event.pointerId);
            }
          };
          const onPointerMove = (event: PointerEvent) => {
            mobileScrollbarController?.pointerMove(event.pointerId);
          };
          const onPointerEnd = (event: PointerEvent) => {
            mobileScrollbarController?.pointerEnd(event.pointerId);
          };
          const onWheel = (event: WheelEvent) => {
            if (isScrollbarTarget(event)) {
              mobileScrollbarController?.activity();
            }
          };

          container.addEventListener("pointerdown", onPointerDown, true);
          container.addEventListener("wheel", onWheel, true);
          window.addEventListener("pointermove", onPointerMove, true);
          window.addEventListener("pointerup", onPointerEnd, true);
          window.addEventListener("pointercancel", onPointerEnd, true);
          removeMobileScrollbarListeners = () => {
            container.removeEventListener(
              "pointerdown",
              onPointerDown,
              true,
            );
            container.removeEventListener("wheel", onWheel, true);
            window.removeEventListener("pointermove", onPointerMove, true);
            window.removeEventListener("pointerup", onPointerEnd, true);
            window.removeEventListener("pointercancel", onPointerEnd, true);
          };
        }
      }

      // Gate new DOM input before either the IME guard or xterm sees it.
      // Previously accepted IME transactions may finish in their original
      // view, including Enter queued just before switching tabs.
      let androidImeGuard: AndroidImeGuardDisposable | null = null;
      const acceptsInputEvent = (event: Event) =>
        activeRef.current || androidImeGuard?.isPendingInputEvent(event) === true;
      const blockInactiveInput = (event: Event) => {
        if (!acceptsInputEvent(event)) {
          event.preventDefault();
          event.stopImmediatePropagation();
        }
      };
      const inputEvents = [
        "keydown", "keypress", "keyup", "beforeinput", "input", "paste",
        "compositionstart", "compositionupdate", "compositionend",
        "pointerdown", "mousedown", "touchstart",
      ];
      for (const name of inputEvents) {
        container.addEventListener(name, blockInactiveInput, true);
      }
      terminal.attachCustomKeyEventHandler(acceptsInputEvent);

      const textarea = terminal.textarea;
      const androidImeGuardEnabled = shouldEnableAndroidImeGuard(
        navigator.userAgent,
      );
      androidImeGuardEnabledRef.current = androidImeGuardEnabled;
      androidImeGuard = textarea
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
            onReplacementTranslated: () => {
              androidImeReplacementsTranslatedRef.current += 1;
            },
          })
        : null;

      const inputDisposable = terminal.onData((data) => {
        androidImeGuard?.observeTerminalData(data);
        if (activeRef.current && ctrlModeRef.current) {
          const converted = applyCtrlToInput(data);
          if (converted.consumed) {
            callbackRef.current.onCtrlConsumed();
            sendToSocket(converted.data);
            return;
          }
        }
        sendToSocket(data);
      });

      // Hidden panes cannot receive user events. Keep xterm's own protocol
      // replies enabled: disableStdin also suppresses cursor/device reports
      // needed by a TUI that is still running in a background tab.
      const resizeObserver = new ResizeObserver(fit);
      resizeObserver.observe(container);
      void document.fonts?.ready.then(fit);
      fit();
      // App owns focus after connection so a generation change cannot steal
      // it from an open settings or peer dialog.

      return () => {
        cancelScrollBoundary();
        for (const name of scrollIntentEvents) {
          container.removeEventListener(name, cancelScrollBoundary, true);
        }
        for (const name of inputEvents) {
          container.removeEventListener(name, blockInactiveInput, true);
        }
        removeMobileScrollbarListeners?.();
        mobileScrollbarController?.dispose();
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

    useLayoutEffect(() => {
      if (active) {
        retryAuthenticationOnActivationRef.current?.();
        fit();
        return;
      }
      terminalRef.current?.blur();
      cancelScrollBoundary();
      if (fitFrameRef.current !== null) {
        window.cancelAnimationFrame(fitFrameRef.current);
        fitFrameRef.current = null;
      }
      if (fitTimerRef.current !== null) {
        window.clearTimeout(fitTimerRef.current);
        fitTimerRef.current = null;
      }
      fitBurstActiveRef.current = false;
      // A switch must not discard bytes held during a mobile resize.
      const capture = mobileResizeCaptureRef.current;
      if (capture) commitMobileResizeCapture(capture);
      removeFreezeFrame();
    }, [active]);

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
      let replayRevision = 0;
      let replayMatchesGeneration = true;
      let authenticationRetryAt: number | null = null;

      const revealRestoredTerminal = (revision = replayRevision) => {
        if (disposed || revision !== replayRevision) {
          return;
        }
        scrollToBoundary(1);
        fit();
        window.requestAnimationFrame(() => {
          if (!disposed && revision === replayRevision) {
            setIsRestoring(false);
          }
        });
      };

      const queueReplay = (replay: BufferedReplay) => {
        if (disposed || replayRef.current !== replay) {
          return;
        }
        // Queue the finite snapshot before handling any later live frame.
        // xterm yields while parsing its FIFO; new live output must not keep
        // extending a replay queue drained at one 16 KiB batch per frame.
        replayRef.current = null;
        const revision = replayRevision;
        const terminal = terminalRef.current;
        if (!terminal) return;
        while (replay.byteLength > 0) {
          const bytes = takeReplayBatch(replay);
          const socket = activeSocket;
          terminal.write(bytes, () => acknowledgeOutput(socket, bytes.byteLength));
        }
        terminal.write(new Uint8Array(), () => revealRestoredTerminal(revision));
      };

      const cancelReplay = () => {
        replayRevision += 1;
        acknowledgeOutput(activeSocket, replayRef.current?.byteLength ?? 0);
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

      const scheduleReconnect = (delay = reconnectDelay(attempt)) => {
        if (disposed || retryTimer !== null) {
          return;
        }
        callbackRef.current.onConnectionStatus("reconnecting");
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

        authenticationRetryAt = null;
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
          if (error instanceof ApiError && error.status === 429) {
            callbackRef.current.onError(
              "Connection temporarily limited. Retrying in one minute.",
            );
            scheduleReconnect(AUTH_RETRY_DELAY_MS);
            return;
          }
          if (error instanceof ApiError && error.status === 401) {
            authenticationRetryAt = Date.now() + AUTH_RETRY_DELAY_MS;
            callbackRef.current.onConnectionStatus("authentication_failed");
            callbackRef.current.onError(
              "Authentication failed. Open the URL printed by the server.",
            );
            return;
          }
          callbackRef.current.onConnectionStatus("disconnected");
          scheduleReconnect();
          return;
        }

        const url = new URL(websocketUrl(token, terminalId));
        url.searchParams.set("flowControl", "1");
        const nextSocket = new WebSocket(url);
        activeSocket = nextSocket;
        nextSocket.binaryType = "arraybuffer";
        socketRef.current = nextSocket;
        const heartbeat = createHeartbeat(Date.now());
        const disconnected = () => {
          if (disposed || nextSocket !== socketRef.current) return;
          clearSocketTimers();
          cancelReplay();
          cancelMobileResizeCapture();
          removeFreezeFrame();
          socketRef.current = null;
          lastSentSizeRef.current = null;
          callbackRef.current.onConnectionStatus("disconnected");
          scheduleReconnect();
        };
        heartbeatTimer = window.setTimeout(() => {
          nextSocket.close(1000, "connection timeout");
          disconnected();
        }, 15_000);

        nextSocket.onopen = () => {
          if (disposed || nextSocket !== socketRef.current) {
            return;
          }
          attempt = 0;
          clearSocketTimers();
          heartbeat.received(Date.now());
          lastSentSizeRef.current = null;
          cancelMobileResizeCapture();
          removeFreezeFrame();
          callbackRef.current.onConnectionStatus("connected");
          callbackRef.current.onConnected();
          callbackRef.current.onError(null);
          fit();
          heartbeatTimer = window.setInterval(() => {
            if (
              nextSocket === socketRef.current &&
              nextSocket.readyState === WebSocket.OPEN
            ) {
              if (heartbeat.expired(Date.now(), document.visibilityState === "visible")) {
                nextSocket.close(1000, "heartbeat timeout");
                disconnected();
                return;
              }
              nextSocket.send(encodeControlMessage({ type: "ping" }));
            }
          }, 20_000);
        };

        nextSocket.onmessage = (event: MessageEvent<string | ArrayBuffer>) => {
          if (disposed || nextSocket !== socketRef.current) {
            return;
          }
          heartbeat.received(Date.now());
          if (typeof event.data !== "string") {
            const bytes = new Uint8Array(event.data);
            if (!replayMatchesGeneration) {
              acknowledgeOutput(nextSocket, bytes.byteLength);
              return;
            }
            const replay = replayRef.current;
            if (replay) {
              replay.chunks.push(bytes);
              replay.byteLength += bytes.byteLength;
            } else if (!captureMobileResizeOutput(nextSocket, bytes)) {
              terminalRef.current?.write(bytes, () => acknowledgeOutput(nextSocket, bytes.byteLength));
            }
            return;
          }

          const message = parseServerControl(event.data);
          if (!message) {
            callbackRef.current.onError("The server sent an invalid control message.");
            return;
          }

          switch (message.type) {
            case "flow_control":
              flowControlledSocketsRef.current.add(nextSocket);
              break;
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
              acknowledgeOutput(nextSocket, replayRef.current?.byteLength ?? 0);
              replayRevision += 1;
              replayCountRef.current += 1;
              cancelMobileResizeCapture();
              removeFreezeFrame();
              replayMatchesGeneration = message.sessionId === sessionId;
              if (!replayMatchesGeneration) {
                // The server follows a generation replay with its session
                // snapshot. TerminalDeck then mounts a fresh xterm/socket.
                // Never mix the new PTY's bytes with old queued writes.
                replayRef.current = null;
                setIsRestoring(true);
                break;
              }
              const replay: BufferedReplay = {
                chunks: [],
                byteLength: 0,
              };
              replayRef.current = replay;
              containerRef.current?.classList.add("terminal-view--covered");
              setIsRestoring(true);
              const terminal = terminalRef.current;
              const revision = replayRevision;
              // Reset at a parser boundary, after old queued bytes and before
              // this snapshot. A synchronous reset can leave pending writes
              // from the previous attachment in the reconstructed screen.
              terminal?.write(new Uint8Array(), () => {
                if (!disposed && revision === replayRevision) {
                  terminal.reset();
                  terminal.clear();
                }
              });
              break;
            }
            case "replay_end": {
              if (!replayMatchesGeneration) break;
              const replay = replayRef.current;
              if (replay) {
                queueReplay(replay);
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

        nextSocket.onclose = disconnected;
      };

      // A cached authentication failure must not make a tab permanently dead.
      // Permit one attempt on re-selection after the cooldown, without an
      // automatic retry loop for a rejected token. A changed token already
      // recreates this effect and cancels the old generation's work.
      const retryAuthenticationOnActivation = () => {
        if (
          !disposed &&
          authenticationRetryAt !== null &&
          Date.now() >= authenticationRetryAt
        ) {
          void connect(true);
        }
      };
      retryAuthenticationOnActivationRef.current = retryAuthenticationOnActivation;
      void connect(false);

      return () => {
        disposed = true;
        if (retryAuthenticationOnActivationRef.current === retryAuthenticationOnActivation) {
          retryAuthenticationOnActivationRef.current = null;
        }
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
    }, [token, terminalId, sessionId]);

    return (
      <>
        <div
          ref={containerRef}
          className={[
            "terminal-view",
            isRestoring ? "terminal-view--covered" : "",
            mobileScrollbarEnabled
              ? "terminal-view--mobile-scrollbar"
              : "",
            mobileScrollbarVisible
              ? "terminal-view--mobile-scrollbar-visible"
              : "",
          ]
            .filter(Boolean)
            .join(" ")}
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
