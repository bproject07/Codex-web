import { useEffect, useState } from "react";

/**
 * Marks a horizontally scrollable element with `data-fade-left` and
 * `data-fade-right` attributes describing whether content continues past the
 * corresponding edge. CSS renders an edge fade only while there is actually
 * hidden content in that direction, so overflow stays discoverable without
 * permanent visual clutter.
 *
 * Returns a callback ref so the observers attach whenever the target element
 * mounts, remounts, or is conditionally rendered after the first commit.
 */
export function useHorizontalScrollFade<T extends HTMLElement>() {
  const [element, setElement] = useState<T | null>(null);

  useEffect(() => {
    if (!element) {
      return;
    }

    const measure = () => {
      const maximum = element.scrollWidth - element.clientWidth;
      const overflow = maximum > 2;
      const fadeLeft =
        overflow && element.scrollLeft > 2 ? "true" : "false";
      const fadeRight =
        overflow && element.scrollLeft < maximum - 2 ? "true" : "false";
      if (element.dataset.fadeLeft !== fadeLeft) {
        element.dataset.fadeLeft = fadeLeft;
      }
      if (element.dataset.fadeRight !== fadeRight) {
        element.dataset.fadeRight = fadeRight;
      }
    };

    measure();
    element.addEventListener("scroll", measure, { passive: true });
    const resizeObserver = new ResizeObserver(measure);
    resizeObserver.observe(element);
    const mutationObserver = new MutationObserver(measure);
    mutationObserver.observe(element, {
      childList: true,
      subtree: true,
      characterData: true,
    });
    return () => {
      element.removeEventListener("scroll", measure);
      resizeObserver.disconnect();
      mutationObserver.disconnect();
    };
  }, [element]);

  // useState setters are stable, so this doubles as a stable callback ref.
  return setElement;
}
