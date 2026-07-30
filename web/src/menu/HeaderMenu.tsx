import {
  type KeyboardEvent as ReactKeyboardEvent,
  useCallback,
  useEffect,
  useId,
  useRef,
  useState,
} from "react";

import {
  firstEnabledIndex,
  menuKeyAction,
  triggerOpenAction,
} from "./headerMenuModel";

export interface HeaderMenuItem {
  /** Stable identity, also used as an extra CSS class on the item. */
  key: string;
  /** Extra class names, e.g. the stable hooks the regression scripts use. */
  className?: string;
  label: string;
  /** Tooltip and accessible description; explains a disabled state. */
  title: string;
  disabled?: boolean;
  /** Small trailing badge, e.g. the update-available marker. */
  badge?: string;
  badgeLabel?: string;
  onSelect: () => void;
}

interface HeaderMenuProps {
  items: HeaderMenuItem[];
  /** Mirrors the open state up so App can pause slash routing and the
   * automatic terminal refocus while the menu is open. */
  onOpenChange?: (open: boolean) => void;
  /** Badge shown on the trigger itself (update available). */
  triggerBadge?: string;
  triggerBadgeLabel?: string;
  /** Test-only escape hatch: render the popover in the initial markup so
   * static-markup tests can assert its structure without firing events. */
  defaultOpen?: boolean;
}

/** Estimated popover width used only to pick the anchoring side before the
 * first paint of the open menu; the CSS min-width matches it. */
const MENU_WIDTH_PX = 232;

export function HeaderMenu({
  items,
  onOpenChange,
  triggerBadge,
  triggerBadgeLabel,
  defaultOpen = false,
}: HeaderMenuProps) {
  const [open, setOpen] = useState(defaultOpen);
  const [placement, setPlacement] = useState<{
    alignRight: boolean;
    shift: number;
  }>({ alignRight: false, shift: 0 });
  const containerRef = useRef<HTMLDivElement>(null);
  const triggerRef = useRef<HTMLButtonElement>(null);
  const menuRef = useRef<HTMLDivElement>(null);
  const pendingFocusIndexRef = useRef<number | null>(null);
  // Mirrors `open` so the parent notification stays outside the state
  // updater (StrictMode runs updaters twice) and fires once per transition.
  const openStateRef = useRef(defaultOpen);
  const menuId = useId();

  const setOpenState = useCallback(
    (nextOpen: boolean) => {
      if (openStateRef.current !== nextOpen) {
        openStateRef.current = nextOpen;
        onOpenChange?.(nextOpen);
      }
      setOpen(nextOpen);
    },
    [onOpenChange],
  );

  const focusItem = useCallback((index: number) => {
    if (index < 0) {
      return;
    }
    const buttons =
      menuRef.current?.querySelectorAll<HTMLButtonElement>(
        "[role='menuitem']",
      ) ?? [];
    buttons[index]?.focus({ preventScroll: true });
  }, []);

  const openMenu = useCallback(
    (focusIndex: number) => {
      const trigger = triggerRef.current;
      if (trigger) {
        // Prefer opening left-aligned under the trigger; fall back to
        // right-aligned near the right viewport edge; if neither fits (a
        // mid-header trigger on a narrow phone), pin the popover to the
        // left viewport margin instead of letting either edge clip.
        const rect = trigger.getBoundingClientRect();
        const fitsLeftAligned =
          rect.left + MENU_WIDTH_PX <= window.innerWidth - 8;
        const fitsRightAligned = rect.right - MENU_WIDTH_PX >= 8;
        if (fitsLeftAligned) {
          setPlacement({ alignRight: false, shift: 0 });
        } else if (fitsRightAligned) {
          setPlacement({ alignRight: true, shift: 0 });
        } else {
          setPlacement({ alignRight: false, shift: 8 - rect.left });
        }
      }
      pendingFocusIndexRef.current = focusIndex;
      setOpenState(true);
    },
    [setOpenState],
  );

  const closeMenu = useCallback(
    (refocusTrigger: boolean) => {
      setOpenState(false);
      if (refocusTrigger) {
        triggerRef.current?.focus({ preventScroll: true });
      }
    },
    [setOpenState],
  );

  // Focus the requested item once the popover exists in the DOM.
  useEffect(() => {
    if (!open) {
      return;
    }
    const index = pendingFocusIndexRef.current;
    pendingFocusIndexRef.current = null;
    if (index !== null && index >= 0) {
      const frame = window.requestAnimationFrame(() => focusItem(index));
      return () => window.cancelAnimationFrame(frame);
    }
  }, [focusItem, open]);

  // Outside pointer interaction closes without stealing focus back.
  useEffect(() => {
    if (!open) {
      return;
    }
    const onPointerDown = (event: PointerEvent) => {
      const target = event.target;
      if (
        target instanceof Node &&
        containerRef.current &&
        !containerRef.current.contains(target)
      ) {
        setOpenState(false);
      }
    };
    document.addEventListener("pointerdown", onPointerDown, true);
    return () =>
      document.removeEventListener("pointerdown", onPointerDown, true);
  }, [open, setOpenState]);

  // Escape must dismiss the open menu no matter where focus sits — on a
  // menu item, still on the trigger, or dropped to <body> after a click on
  // the popover's padding. The capture-phase document listener is the single
  // authority; it runs before the popover's own key handling.
  useEffect(() => {
    if (!open) {
      return;
    }
    const onDocumentKeyDown = (event: KeyboardEvent) => {
      if (event.key !== "Escape") {
        return;
      }
      event.preventDefault();
      event.stopPropagation();
      closeMenu(true);
    };
    document.addEventListener("keydown", onDocumentKeyDown, true);
    return () =>
      document.removeEventListener("keydown", onDocumentKeyDown, true);
  }, [closeMenu, open]);

  const handleTriggerClick = () => {
    if (open) {
      closeMenu(true);
    } else {
      openMenu(firstEnabledIndex(items));
    }
  };

  const handleTriggerKeyDown = (event: ReactKeyboardEvent<HTMLElement>) => {
    if (open) {
      return;
    }
    const focusIndex = triggerOpenAction(event.key, items);
    if (focusIndex !== null) {
      event.preventDefault();
      openMenu(focusIndex);
    }
  };

  const handleMenuKeyDown = (event: ReactKeyboardEvent<HTMLElement>) => {
    const target = event.target;
    const buttons = Array.from(
      menuRef.current?.querySelectorAll<HTMLButtonElement>(
        "[role='menuitem']",
      ) ?? [],
    );
    const current =
      target instanceof HTMLElement
        ? buttons.findIndex((button) => button === target)
        : -1;
    const action = menuKeyAction(event.key, items, current);
    if (!action) {
      return;
    }
    if (action.kind === "move") {
      event.preventDefault();
      focusItem(action.index);
      return;
    }
    if (action.preventDefault) {
      event.preventDefault();
    }
    closeMenu(action.refocusTrigger);
  };

  const handleSelect = (item: HeaderMenuItem) => {
    if (item.disabled) {
      return;
    }
    // Close first and hand focus to the trigger so the launched surface
    // records it as the return-focus target, then run the existing handler.
    closeMenu(true);
    item.onSelect();
  };

  return (
    <div className="header-menu" ref={containerRef}>
      <button
        type="button"
        ref={triggerRef}
        className="header-menu-trigger"
        title="Menu"
        aria-label={
          triggerBadgeLabel ? `Menu (${triggerBadgeLabel})` : "Menu"
        }
        aria-haspopup="menu"
        aria-expanded={open}
        aria-controls={open ? menuId : undefined}
        onClick={handleTriggerClick}
        onKeyDown={handleTriggerKeyDown}
      >
        <span className="header-menu-glyph" aria-hidden="true">
          …
        </span>
        {triggerBadge && (
          <span className="header-update-badge" aria-hidden="true">
            {triggerBadge}
          </span>
        )}
      </button>
      {open && (
        <div
          ref={menuRef}
          id={menuId}
          role="menu"
          aria-label="Menu"
          className={
            placement.alignRight
              ? "header-menu-popover header-menu-popover--right"
              : "header-menu-popover"
          }
          style={
            placement.shift !== 0 ? { left: placement.shift } : undefined
          }
          onKeyDown={handleMenuKeyDown}
        >
          {items.map((item) => (
            <button
              key={item.key}
              type="button"
              role="menuitem"
              className={[
                "header-menu-item",
                `header-menu-item--${item.key}`,
                item.className,
              ]
                .filter(Boolean)
                .join(" ")}
              title={item.title}
              disabled={item.disabled}
              aria-disabled={item.disabled || undefined}
              onClick={() => handleSelect(item)}
            >
              <span className="header-menu-item-label">{item.label}</span>
              {item.badge && (
                <span
                  className="header-update-badge"
                  aria-label={item.badgeLabel}
                >
                  {item.badge}
                </span>
              )}
            </button>
          ))}
        </div>
      )}
    </div>
  );
}
