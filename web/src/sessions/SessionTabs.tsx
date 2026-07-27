import {
  type WheelEvent,
  useCallback,
  useEffect,
  useRef,
  useState,
} from "react";

import type { SessionSnapshot } from "../api";
import { AGENT_SHORT_LABELS } from "../agents";
import {
  compactSessionName,
  horizontalWheelDelta,
} from "./sessionTabUtils";

interface SessionTabsProps {
  sessions: SessionSnapshot[];
  selectedTerminalId: string;
  busy: boolean;
  onSelect: (session: SessionSnapshot) => void;
  onCreate: () => void;
  onManage: () => void;
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
  selectedTerminalId,
  busy,
  onSelect,
  onCreate,
  onManage,
}: SessionTabsProps) {
  const tabListRef = useRef<HTMLDivElement>(null);
  const [scrollState, setScrollState] =
    useState<ScrollState>(EMPTY_SCROLL_STATE);

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
      const activeTab = tabList?.querySelector<HTMLElement>(
        '.session-tab[aria-selected="true"]',
      );
      if (!tabList || !activeTab) {
        measureScroll();
        return;
      }

      const tabRect = activeTab.getBoundingClientRect();
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
  }, [measureScroll, selectedTerminalId, sessions]);

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
          return (
            <button
              key={candidate.terminalId}
              type="button"
              role="tab"
              className={
                selected ? "session-tab session-tab--active" : "session-tab"
              }
              aria-selected={selected}
              title={`Open ${candidate.name} — ${candidate.status}`}
              onClick={() => onSelect(candidate)}
            >
              <span
                className={`session-tab-status session-tab-status--${candidate.status}`}
                aria-hidden="true"
              />
              <span className="session-tab-label session-tab-label--full">
                {candidate.name}
              </span>
              <span className="session-tab-label session-tab-label--compact">
                {compactSessionName(
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
            </button>
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
        className="header-action--new session-new-button"
        title="Choose an agent for a new terminal"
        aria-label="Choose an agent for a new terminal"
        disabled={busy}
        onClick={onCreate}
      >
        <span className="session-new-label--full">+ New</span>
        <span className="session-new-label--compact">+</span>
      </button>
      <button
        type="button"
        className="header-action--sessions session-manage-button"
        title="Manage terminal sessions"
        aria-label={`Manage ${sessions.length} terminal sessions`}
        onClick={onManage}
      >
        <span className="session-manage-label--full">Manage</span>
        <span className="session-manage-label--compact">
          {sessions.length}
        </span>
      </button>
    </nav>
  );
}
