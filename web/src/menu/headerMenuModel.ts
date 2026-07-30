/** Pure keyboard/focus logic for the header overflow menu. The component
 * owns DOM concerns; everything decidable from plain data lives here so it
 * can be unit tested without a browser. */

export interface MenuItemState {
  disabled?: boolean;
}

export function firstEnabledIndex(items: readonly MenuItemState[]): number {
  return items.findIndex((item) => !item.disabled);
}

export function lastEnabledIndex(items: readonly MenuItemState[]): number {
  for (let index = items.length - 1; index >= 0; index -= 1) {
    if (!items[index].disabled) {
      return index;
    }
  }
  return -1;
}

/** Next enabled index in the given direction, wrapping around. Returns the
 * current index when no other item is enabled, and -1 when none are. */
export function nextEnabledIndex(
  items: readonly MenuItemState[],
  current: number,
  delta: 1 | -1,
): number {
  if (items.length === 0) {
    return -1;
  }
  for (let step = 1; step <= items.length; step += 1) {
    const candidate =
      (current + delta * step + items.length * step) % items.length;
    if (!items[candidate].disabled) {
      return candidate;
    }
  }
  return -1;
}

export type MenuKeyAction =
  | { kind: "move"; index: number }
  | { kind: "close"; refocusTrigger: boolean; preventDefault: boolean }
  | null;

/** Interpret a key pressed while focus is inside the open menu. `null`
 * means the menu does not handle the key (do not preventDefault). */
export function menuKeyAction(
  key: string,
  items: readonly MenuItemState[],
  current: number,
): MenuKeyAction {
  switch (key) {
    case "ArrowDown":
      return {
        kind: "move",
        index:
          current < 0
            ? firstEnabledIndex(items)
            : nextEnabledIndex(items, current, 1),
      };
    case "ArrowUp":
      return {
        kind: "move",
        index:
          current < 0
            ? lastEnabledIndex(items)
            : nextEnabledIndex(items, current, -1),
      };
    case "Home":
      return { kind: "move", index: firstEnabledIndex(items) };
    case "End":
      return { kind: "move", index: lastEnabledIndex(items) };
    case "Escape":
      return { kind: "close", refocusTrigger: true, preventDefault: true };
    case "Tab":
      // Tab (or Shift+Tab) leaves the menu. Close it and put focus back on
      // the trigger WITHOUT cancelling the default action, so sequential
      // traversal continues from the trigger — not from wherever focus
      // lands after the focused item unmounts.
      return { kind: "close", refocusTrigger: true, preventDefault: false };
    default:
      return null;
  }
}

/** Interpret a key pressed on the closed trigger. Returns the index the
 * opened menu should focus, or null when the key does not open the menu.
 * Enter/Space activate the button natively (click), so only the arrow keys
 * need explicit handling. */
export function triggerOpenAction(
  key: string,
  items: readonly MenuItemState[],
): number | null {
  if (key === "ArrowDown") {
    return firstEnabledIndex(items);
  }
  if (key === "ArrowUp") {
    return lastEnabledIndex(items);
  }
  return null;
}
