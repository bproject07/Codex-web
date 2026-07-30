import {
  type WheelEvent,
  useCallback,
  useEffect,
  useRef,
  useState,
} from "react";

import type { SessionSnapshot } from "../api";
import { AGENT_LABELS, AGENT_SHORT_LABELS } from "../agents";
import {
  peerStatusLabel,
  peerThreadDisplayId,
  type PeerThread,
} from "../peer";
import {
  compactSessionName,
  horizontalWheelDelta,
} from "./sessionTabUtils";

interface SessionTabsProps {
  sessions: SessionSnapshot[];
  selectedTerminalId: string;
  busy: boolean;
  onSelect: (session: SessionSnapshot) => void;
  onClose: (session: SessionSnapshot) => void;
  onPeer: () => void;
  peerThreads: PeerThread[];
  peerDisabledReason: string | null;
}

interface ScrollState {
  overflow: boolean;
  left: boolean;
  right: boolean;
}

const EMPTY_SCROLL_STATE: ScrollState = {
  overflow: false,
  left: false,
  right: false,
};

function scrollBehavior(): ScrollBehavior {
  return window.matchMedia("(prefers-reduced-motion: reduce)").matches
    ? "auto"
    : "smooth";
}

export function SessionTabs({
  sessions,
  selectedTerminalId,
  busy,
  onSelect,
  onClose,
  onPeer,
  peerThreads,
  peerDisabledReason,
}: SessionTabsProps) {
  const tabListRef = useRef<HTMLDivElement>(null);
  const [scrollState, setScrollState] =
    useState<ScrollState>(EMPTY_SCROLL_STATE);
  const sessionTopologyKey = sessions
    .map((session) => session.terminalId)
    .join("|");
  const tabLayoutKey = [
    ...sessions.map(
      (session) =>
        `${session.terminalId}:${session.name}:${session.status}:${session.purpose.kind}`,
    ),
    ...peerThreads.map(
      (thread) =>
        `${thread.id}:${thread.reviewerTerminalId ?? ""}:${thread.status}`,
    ),
  ].join("|");
  const peerButtonDescription =
    peerDisabledReason ??
    (!selectedTerminalId
      ? "@cwt requires a selected source terminal."
      : busy
        ? "Wait for the current terminal operation before opening @cwt."
        : "Open @cwt peer collaboration");

  const measureScroll = useCallback(() => {
    const tabList = tabListRef.current;
    if (!tabList) {
      setScrollState(EMPTY_SCROLL_STATE);
      return;
    }

    const maximum = Math.max(0, tabList.scrollWidth - tabList.clientWidth);
    const nextState = {
      overflow: maximum > 2,
      left: tabList.scrollLeft > 2,
      right: tabList.scrollLeft < maximum - 2,
    };
    tabList.dataset.fadeLeft =
      nextState.overflow && nextState.left ? "true" : "false";
    tabList.dataset.fadeRight =
      nextState.overflow && nextState.right ? "true" : "false";
    setScrollState(nextState);
  }, []);

  useEffect(() => {
    const tabList = tabListRef.current;
    if (!tabList) {
      return;
    }

    const resizeObserver = new ResizeObserver(measureScroll);
    resizeObserver.observe(tabList);
    window.addEventListener("resize", measureScroll);
    const frame = window.requestAnimationFrame(measureScroll);

    return () => {
      window.cancelAnimationFrame(frame);
      window.removeEventListener("resize", measureScroll);
      resizeObserver.disconnect();
    };
  }, [measureScroll]);

  useEffect(() => {
    const frame = window.requestAnimationFrame(() => {
      const tabList = tabListRef.current;
      const activeTabShell = tabList?.querySelector<HTMLElement>(
        ".session-tab-shell--active",
      );
      if (!tabList || !activeTabShell) {
        measureScroll();
        return;
      }

      const tabRect = activeTabShell.getBoundingClientRect();
      const listRect = tabList.getBoundingClientRect();
      const behavior = scrollBehavior();

      if (tabRect.left < listRect.left) {
        tabList.scrollTo({
          left: tabList.scrollLeft + tabRect.left - listRect.left,
          behavior,
        });
      } else if (tabRect.right > listRect.right) {
        tabList.scrollTo({
          left: tabList.scrollLeft + tabRect.right - listRect.right,
          behavior,
        });
      }
      measureScroll();
    });

    return () => window.cancelAnimationFrame(frame);
  }, [measureScroll, selectedTerminalId, sessionTopologyKey]);

  useEffect(() => {
    const frame = window.requestAnimationFrame(measureScroll);
    return () => window.cancelAnimationFrame(frame);
  }, [measureScroll, tabLayoutKey]);

  const scrollByPage = (direction: -1 | 1) => {
    const tabList = tabListRef.current;
    if (!tabList) {
      return;
    }
    const distance = Math.max(120, Math.round(tabList.clientWidth * 0.8));
    tabList.scrollBy({ left: distance * direction, behavior: scrollBehavior() });
  };

  const handleWheel = (event: WheelEvent<HTMLDivElement>) => {
    const tabList = tabListRef.current;
    if (!tabList || tabList.scrollWidth <= tabList.clientWidth) {
      return;
    }

    const delta = horizontalWheelDelta(event.deltaX, event.deltaY);
    if (delta === 0) {
      return;
    }
    event.preventDefault();
    tabList.scrollLeft += delta;
  };

  return (
    <nav className="session-switcher" aria-label="Terminal sessions">
      {/* The arrows overlay the strip's edges instead of flanking it, so
          showing them never shrinks the measured width — arrow visibility
          therefore flips at one stable overflow threshold. */}
      <div className="session-tab-scroller">
        {scrollState.overflow && (
          <button
            type="button"
            className="session-scroll-button session-scroll-button--left"
            title="Scroll sessions left"
            aria-label="Scroll sessions left"
            disabled={!scrollState.left}
            onClick={() => scrollByPage(-1)}
          >
            ‹
          </button>
        )}
        <div
          ref={tabListRef}
          className="session-tabs"
          role="tablist"
          aria-label="Open terminal sessions"
          onScroll={measureScroll}
          onWheel={handleWheel}
        >
        {sessions.map((candidate, index) => {
          const selected = candidate.terminalId === selectedTerminalId;
          const purpose = candidate.purpose;
          const reviewerThread =
            purpose.kind === "peer"
              ? peerThreads.find(
                  (thread) => thread.id === purpose.threadId,
                ) ?? null
              : null;
          const readyResponses =
            purpose.kind === "interactive"
              ? peerThreads.filter(
                  (thread) =>
                    thread.sourceTerminalId === candidate.terminalId &&
                    thread.status === "response_ready",
                ).length
              : 0;
          const peerLabel = reviewerThread
            ? `${AGENT_SHORT_LABELS[candidate.agent]}·${peerThreadDisplayId(
                reviewerThread.id,
              )}`
            : null;
          const fullLabel = reviewerThread
            ? `↳ ${AGENT_LABELS[candidate.agent]} Review · ${peerThreadDisplayId(
                reviewerThread.id,
              )}`
            : candidate.name;
          const peerState = reviewerThread?.status;
          return (
            <div
              key={candidate.terminalId}
              className={
                [
                  "session-tab-shell",
                  selected ? "session-tab-shell--active" : "",
                  candidate.isPrimary ? "session-tab-shell--primary" : "",
                  reviewerThread ? "session-tab-shell--peer" : "",
                ]
                  .filter(Boolean)
                  .join(" ")
              }
            >
              <button
                type="button"
                role="tab"
                className={`session-tab session-tab--agent-${candidate.agent}${
                  selected ? " session-tab--active" : ""
                }`}
                aria-selected={selected}
                title={`Open ${fullLabel} (${AGENT_LABELS[candidate.agent]}) — ${
                  peerState ? peerStatusLabel(peerState) : candidate.status
                }`}
                onClick={() => onSelect(candidate)}
              >
                <span
                  className={`session-tab-status session-tab-status--${
                    peerState ?? candidate.status
                  }`}
                  aria-hidden="true"
                />
                <span className="visually-hidden">
                  {peerState ? peerStatusLabel(peerState) : candidate.status}
                  {", "}
                  {AGENT_LABELS[candidate.agent]}
                </span>
                {reviewerThread && (
                  <span className="session-tab-peer-arrow" aria-hidden="true">
                    ↳
                  </span>
                )}
                <span className="session-tab-label session-tab-label--full">
                  {fullLabel}
                </span>
                <span className="session-tab-label session-tab-label--compact">
                  {peerLabel ??
                    compactSessionName(
                      candidate.name,
                      index,
                      AGENT_SHORT_LABELS[candidate.agent],
                    )}
                </span>
                {candidate.isPrimary && (
                  <span className="session-tab-primary" title="Primary terminal">
                    P
                  </span>
                )}
                {readyResponses > 0 && (
                  <span
                    className="session-tab-peer-count"
                    aria-label={`${readyResponses} peer ${
                      readyResponses === 1 ? "response" : "responses"
                    } ready`}
                  >
                    {readyResponses}
                  </span>
                )}
              </button>
              {!candidate.isPrimary && (
                <button
                  type="button"
                  className="session-tab-close"
                  aria-label={`Close ${fullLabel}`}
                  title={`Close ${fullLabel}`}
                  disabled={busy}
                  onClick={() => onClose(candidate)}
                >
                  ×
                </button>
              )}
            </div>
          );
        })}
        </div>
        {scrollState.overflow && (
          <button
            type="button"
            className="session-scroll-button session-scroll-button--right"
            title="Scroll sessions right"
            aria-label="Scroll sessions right"
            disabled={!scrollState.right}
            onClick={() => scrollByPage(1)}
          >
            ›
          </button>
        )}
      </div>
      <button
        type="button"
        className="header-action--peer session-peer-button"
        title={peerButtonDescription}
        aria-label={peerButtonDescription}
        disabled={busy || Boolean(peerDisabledReason) || !selectedTerminalId}
        onClick={onPeer}
      >
        @cwt
      </button>
    </nav>
  );
}
