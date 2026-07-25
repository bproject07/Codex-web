import { MOBILE_KEY_SEQUENCES } from "./mobileKeys";

interface MobileKeysProps {
  ctrlMode: boolean;
  onCtrlModeChange: (active: boolean) => void;
  onSend: (data: string) => void;
  onScrollPages: (pageCount: number) => void;
  onScrollToTop: () => void;
  onScrollToBottom: () => void;
  onHide: () => void;
}

interface KeyButton {
  label: string;
  data: string;
  title: string;
}

const LEADING_KEYS: KeyButton[] = [
  { label: "Enter", data: MOBILE_KEY_SEQUENCES.enter, title: "Enter" },
  { label: "↑", data: MOBILE_KEY_SEQUENCES.arrowUp, title: "Arrow up" },
  { label: "↓", data: MOBILE_KEY_SEQUENCES.arrowDown, title: "Arrow down" },
  { label: "←", data: MOBILE_KEY_SEQUENCES.arrowLeft, title: "Arrow left" },
  { label: "→", data: MOBILE_KEY_SEQUENCES.arrowRight, title: "Arrow right" },
];

const TRAILING_KEYS: KeyButton[] = [
  { label: "Esc", data: MOBILE_KEY_SEQUENCES.escape, title: "Escape" },
  { label: "Tab", data: MOBILE_KEY_SEQUENCES.tab, title: "Tab" },
  { label: "Ctrl+C", data: MOBILE_KEY_SEQUENCES.ctrlC, title: "Interrupt" },
  { label: "Ctrl+L", data: MOBILE_KEY_SEQUENCES.ctrlL, title: "Clear screen" },
];

export function MobileToolbar({
  ctrlMode,
  onCtrlModeChange,
  onSend,
  onScrollPages,
  onScrollToTop,
  onScrollToBottom,
  onHide,
}: MobileKeysProps) {
  return (
    <div className="mobile-keys" role="toolbar" aria-label="Terminal keys">
      {LEADING_KEYS.map((key) => (
        <button
          className="key-button"
          type="button"
          title={key.title}
          key={key.title}
          onClick={() => onSend(key.data)}
        >
          {key.label}
        </button>
      ))}
      <button
        className="key-button key-button--history"
        type="button"
        title="Scroll terminal history up one page"
        onClick={() => onScrollPages(-1)}
      >
        PgUp
      </button>
      <button
        className="key-button key-button--history"
        type="button"
        title="Scroll terminal history down one page"
        onClick={() => onScrollPages(1)}
      >
        PgDn
      </button>
      <button
        className={ctrlMode ? "key-button key-button--active" : "key-button"}
        type="button"
        aria-pressed={ctrlMode}
        title="Apply Ctrl to the next letter"
        onClick={() => onCtrlModeChange(!ctrlMode)}
      >
        Ctrl
      </button>
      {TRAILING_KEYS.map((key) => (
        <button
          className="key-button"
          type="button"
          title={key.title}
          key={key.title}
          onClick={() => onSend(key.data)}
        >
          {key.label}
        </button>
      ))}
      <button
        className="key-button key-button--history"
        type="button"
        title="Go to the oldest available terminal history"
        onClick={onScrollToTop}
      >
        Top
      </button>
      <button
        className="key-button key-button--history"
        type="button"
        title="Return to the live terminal output"
        onClick={onScrollToBottom}
      >
        Live
      </button>
      <button
        className="key-button key-button--muted"
        type="button"
        onClick={onHide}
      >
        Hide
      </button>
    </div>
  );
}
