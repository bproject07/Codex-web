export const MOBILE_SCROLLBAR_IDLE_MS = 20_000;

export interface MobileScrollbarClock {
  setTimeout: (callback: () => void, delay: number) => number;
  clearTimeout: (timer: number) => void;
}

export interface MobileScrollbarVisibilityController {
  pointerDown: (pointerId: number) => void;
  pointerMove: (pointerId: number) => void;
  pointerEnd: (pointerId: number) => void;
  activity: () => void;
  dispose: () => void;
}

export function createMobileScrollbarVisibilityController(
  onVisibilityChange: (visible: boolean) => void,
  clock: MobileScrollbarClock,
): MobileScrollbarVisibilityController {
  const activePointers = new Set<number>();
  let hideTimer: number | null = null;
  let visible = false;
  let disposed = false;

  const clearHideTimer = () => {
    if (hideTimer === null) {
      return;
    }
    clock.clearTimeout(hideTimer);
    hideTimer = null;
  };

  const setVisible = (nextVisible: boolean) => {
    if (disposed || visible === nextVisible) {
      return;
    }
    visible = nextVisible;
    onVisibilityChange(nextVisible);
  };

  const reveal = () => {
    clearHideTimer();
    setVisible(true);
  };

  const hideAfterIdle = () => {
    clearHideTimer();
    if (activePointers.size > 0) {
      return;
    }
    hideTimer = clock.setTimeout(() => {
      hideTimer = null;
      setVisible(false);
    }, MOBILE_SCROLLBAR_IDLE_MS);
  };

  return {
    pointerDown: (pointerId) => {
      if (disposed) {
        return;
      }
      activePointers.add(pointerId);
      reveal();
    },
    pointerMove: (pointerId) => {
      if (!disposed && activePointers.has(pointerId)) {
        reveal();
      }
    },
    pointerEnd: (pointerId) => {
      if (disposed || !activePointers.delete(pointerId)) {
        return;
      }
      reveal();
      hideAfterIdle();
    },
    activity: () => {
      if (disposed) {
        return;
      }
      reveal();
      hideAfterIdle();
    },
    dispose: () => {
      disposed = true;
      activePointers.clear();
      clearHideTimer();
    },
  };
}
