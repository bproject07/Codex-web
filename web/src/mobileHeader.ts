const MOBILE_HEADER_COLLAPSED_KEY =
  "codex-web.mobile-header-context-collapsed.v1";

export interface MobileHeaderStorage {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
}

function browserStorage(): MobileHeaderStorage | null {
  if (typeof window === "undefined") {
    return null;
  }
  try {
    return window.localStorage;
  } catch {
    return null;
  }
}

/** Phones start with the context row collapsed unless that browser explicitly
 * remembers the expanded state. Desktop ignores this preference in CSS and
 * always renders the full header. */
export function loadMobileHeaderCollapsed(
  storage: MobileHeaderStorage | null = browserStorage(),
): boolean {
  if (!storage) {
    return true;
  }
  try {
    return storage.getItem(MOBILE_HEADER_COLLAPSED_KEY) !== "expanded";
  } catch {
    return true;
  }
}

export function saveMobileHeaderCollapsed(
  collapsed: boolean,
  storage: MobileHeaderStorage | null = browserStorage(),
): void {
  if (!storage) {
    return;
  }
  try {
    storage.setItem(
      MOBILE_HEADER_COLLAPSED_KEY,
      collapsed ? "collapsed" : "expanded",
    );
  } catch {
    // Storage can be blocked or full. The in-memory UI state still works.
  }
}
