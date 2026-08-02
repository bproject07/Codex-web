import { useHorizontalScrollFade } from "../scrollFade";
import { MOBILE_KEY_SEQUENCES } from "./mobileKeys";

interface MobileKeysProps {
  ctrlMode: boolean;
  onCtrlModeChange: (active: boolean) => void;
  onSend: (data: string) => void;
  onScrollToTop: () => void;
  onScrollToBottom: () => void;
  onHide: () => void;
}

interface KeyButton {
  label: string;
  data: string;
  title: string;
  /**
   * Accessible name override, used only where the visible label is a symbol.
   * Text-labeled keys keep their visible text as the accessible name.
   */
  ariaLabel?: string;
}

// Enter stays leftmost with the arrows beside it; Esc and Ctrl+C follow so the
// keys that interrupt a stuck agent stay inside the first 360px screenful.
const LEADING_KEYS: KeyButton[] = [
  { label: "Enter", data: MOBILE_KEY_SEQUENCES.enter, title: "Enter" },
  {
    label: "↑",
    data: MOBILE_KEY_SEQUENCES.arrowUp,
    title: "Arrow up",
    ariaLabel: "Arrow up",
  },
  {
    label: "↓",
    data: MOBILE_KEY_SEQUENCES.arrowDown,
    title: "Arrow down",
    ariaLabel: "Arrow down",
  },
  {
    label: "←",
    data: MOBILE_KEY_SEQUENCES.arrowLeft,
    title: "Arrow left",
    ariaLabel: "Arrow left",
  },
  {
    label: "→",
    data: MOBILE_KEY_SEQUENCES.arrowRight,
    title: "Arrow right",
    ariaLabel: "Arrow right",
  },
  { label: "Esc", data: MOBILE_KEY_SEQUENCES.escape, title: "Escape" },
  { label: "Ctrl+C", data: MOBILE_KEY_SEQUENCES.ctrlC, title: "Interrupt" },
  { label: "Tab", data: MOBILE_KEY_SEQUENCES.tab, title: "Tab" },
];

const TRAILING_KEYS: KeyButton[] = [
  { label: "PgUp", data: MOBILE_KEY_SEQUENCES.pageUp, title: "Page up" },
  { label: "PgDn", data: MOBILE_KEY_SEQUENCES.pageDown, title: "Page down" },
  { label: "Ctrl+L", data: MOBILE_KEY_SEQUENCES.ctrlL, title: "Clear screen" },
];

export function MobileToolbar({
  ctrlMode,
  onCtrlModeChange,
  onSend,
  onScrollToTop,
  onScrollToBottom,
  onHide,
}: MobileKeysProps) {
  const toolbarRef = useHorizontalScrollFade<HTMLDivElement>();

  return (
    <div className="mobile-keys-bar">
      <div
        ref={toolbarRef}
        className="mobile-keys"
        role="toolbar"
        aria-label="Terminal keys"
      >
        {LEADING_KEYS.map((key) => (
          <button
            className="key-button"
            type="button"
            title={key.title}
            aria-label={key.ariaLabel}
            key={key.title}
            onClick={() => onSend(key.data)}
          >
            {key.label}
          </button>
        ))}
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
            aria-label={key.ariaLabel}
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
          title="Hide the key bar. Reopen it with the Keys header button."
          onClick={onHide}
        >
          Hide
        </button>
      </div>
    </div>
  );
}
