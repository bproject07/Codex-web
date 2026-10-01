/** A request deadline that also respects cancellation by its owner. */
export function requestTimeout(parent: AbortSignal | undefined, milliseconds: number) {
  const controller = new AbortController();
  const cancel = () => controller.abort(parent?.reason);
  if (parent?.aborted) cancel();
  else parent?.addEventListener("abort", cancel, { once: true });
  const timer = globalThis.setTimeout(() => controller.abort(new DOMException(
    "The request timed out. Check the current state before retrying.", "TimeoutError",
  )), milliseconds);
  return {
    signal: controller.signal,
    dispose() {
      globalThis.clearTimeout(timer);
      parent?.removeEventListener("abort", cancel);
    },
  };
}
