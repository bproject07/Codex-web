import { describe, expect, it } from "vitest";
import {
  createMobileScrollbarVisibilityController,
  MOBILE_SCROLLBAR_IDLE_MS,
  type MobileScrollbarClock,
} from "./mobileScrollbar";

function manualClock(): MobileScrollbarClock & {
  delay: () => number | null;
  fire: () => void;
} {
  let nextTimer = 0;
  let pending:
    | {
        id: number;
        callback: () => void;
        delay: number;
      }
    | undefined;

  return {
    setTimeout: (callback, delay) => {
      const id = ++nextTimer;
      pending = { id, callback, delay };
      return id;
    },
    clearTimeout: (timer) => {
      if (pending?.id === timer) {
        pending = undefined;
      }
    },
    delay: () => pending?.delay ?? null,
    fire: () => {
      const callback = pending?.callback;
      pending = undefined;
      callback?.();
    },
  };
}

describe("mobile scrollbar visibility", () => {
  it("stays visible while dragged and hides after 20 seconds idle", () => {
    const clock = manualClock();
    const changes: boolean[] = [];
    const controller = createMobileScrollbarVisibilityController(
      (visible) => changes.push(visible),
      clock,
    );

    controller.pointerDown(7);
    expect(changes).toEqual([true]);
    expect(clock.delay()).toBeNull();

    controller.pointerMove(7);
    clock.fire();
    expect(changes).toEqual([true]);

    controller.pointerEnd(7);
    expect(clock.delay()).toBe(MOBILE_SCROLLBAR_IDLE_MS);
    clock.fire();
    expect(changes).toEqual([true, false]);
  });

  it("restarts the idle period after another scrollbar interaction", () => {
    const clock = manualClock();
    const changes: boolean[] = [];
    const controller = createMobileScrollbarVisibilityController(
      (visible) => changes.push(visible),
      clock,
    );

    controller.activity();
    expect(clock.delay()).toBe(MOBILE_SCROLLBAR_IDLE_MS);
    controller.pointerDown(1);
    expect(clock.delay()).toBeNull();
    controller.pointerEnd(1);
    expect(clock.delay()).toBe(MOBILE_SCROLLBAR_IDLE_MS);
    clock.fire();
    expect(changes).toEqual([true, false]);
  });

  it("cancels pending work when disposed", () => {
    const clock = manualClock();
    const changes: boolean[] = [];
    const controller = createMobileScrollbarVisibilityController(
      (visible) => changes.push(visible),
      clock,
    );

    controller.activity();
    controller.dispose();
    expect(clock.delay()).toBeNull();
    clock.fire();
    expect(changes).toEqual([true]);
  });
});
