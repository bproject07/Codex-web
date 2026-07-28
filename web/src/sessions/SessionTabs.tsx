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
  maxSessions: number | null;
  selectedTerminalId: string;
  busy: boolean;
  onSelect: (session: SessionSnapshot) => void;
  onClose: (session: SessionSnapshot) => void;
  onCreate: () => void;
  onPeer: () => void;
  onManage: () => void;
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

export function SessionTabs({
  sessions,
  maxSessions,
  selectedTerminalId,
  busy,
  onSelect,
  onClose,
  onCreate,
  onPeer,
  onManage,
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
  const configuredMaxSessions =
    maxSessions !== null &&
    Number.isSafeInteger(maxSessions) &&
    maxSessions > 0
      ? maxSessions
      : null;
  const capacityReached =
    configuredMaxSessions !== null &&
    sessions.length >= configuredMaxSessions;
  const sessionCountLabel =
    configuredMaxSessions === null
      ? `${sessions.length}`
      : `${sessions.length}/${configuredMaxSessions}`;
  const createButtonDescription = capacityReached
    ? `Session capacity reached (${sessions.length} of ${configuredMaxSessions})`
    : "Choose an agent for a new terminal";
  const manageButtonDescription =
    configuredMaxSessions === null
      ? `Manage ${sessions.length} terminal ${
          sessions.length === 1 ? "session" : "sessions"
        }`
      : `Manage ${sessions.length} of ${configuredMaxSessions} terminal sessions`;

  const measureScroll = useCallback(() => {
    const tabList = tabListRef.current;
    if (!tabList) {
      setScrollState(EMPTY_SCROLL_STATE);
      return;
    }

    const maximum = Math.max(0, tabList.scrollWidth - tabList.clientWidth);
    setScrollState({
      overflow: maximum > 2,
      left: tabList.scrollLeft > 2,
      right: tabList.scrollLeft < maximum - 2,
    });
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

      if (tabRect.left < listRect.left) {
        tabList.scrollTo({
          left: tabList.scrollLeft + tabRect.left - listRect.left,
          behavior: "smooth",
        });
      } else if (tabRect.right > listRect.right) {
        tabList.scrollTo({
          left: tabList.scrollLeft + tabRect.right - listRect.right,
          behavior: "smooth",
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
    tabList.scrollBy({ left: distance * direction, behavior: "smooth" });
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
                className={
                  selected ? "session-tab session-tab--active" : "session-tab"
                }
                aria-selected={selected}
                title={`Open ${fullLabel} — ${
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
      <button
        type="button"
        className="header-action--new session-new-button"
        title={createButtonDescription}
        aria-label={createButtonDescription}
        disabled={busy || capacityReached}
        onClick={onCreate}
      >
        <span className="session-new-label--full">+ New</span>
        <span className="session-new-label--compact">+</span>
      </button>
      <button
        type="button"
        className="header-action--sessions session-manage-button"
        title={manageButtonDescription}
        aria-label={manageButtonDescription}
        onClick={onManage}
      >
        <span className="session-manage-label--full">
          Manage {sessionCountLabel}
        </span>
        <span className="session-manage-label--compact">
          {sessionCountLabel}
        </span>
      </button>
    </nav>
  );
}
