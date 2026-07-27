export async function copyTextToClipboard(text: string): Promise<boolean> {
  if (typeof navigator !== "undefined" && navigator.clipboard?.writeText) {
    try {
      await navigator.clipboard.writeText(text);
      return true;
    } catch {
      // HTTP/Tailscale pages commonly lack a Clipboard API secure context.
      // Fall through to the short-lived selection-based browser fallback.
    }
  }

  if (typeof document === "undefined" || !document.body) {
    return false;
  }

  const previousActive = isFocusable(document.activeElement)
    ? document.activeElement
    : null;
  const textarea = document.createElement("textarea");
  textarea.value = text;
  textarea.readOnly = true;
  textarea.inputMode = "none";
  textarea.tabIndex = -1;
  textarea.setAttribute("aria-hidden", "true");
  Object.assign(textarea.style, {
    position: "fixed",
    inset: "0 auto auto -9999px",
    width: "1px",
    height: "1px",
    opacity: "0",
    pointerEvents: "none",
  });

  document.body.appendChild(textarea);
  let copied = false;
  try {
    textarea.focus({ preventScroll: true });
    textarea.select();
    textarea.setSelectionRange(0, textarea.value.length);
    copied = document.execCommand("copy");
  } catch {
    copied = false;
  } finally {
    textarea.remove();
    previousActive?.focus({ preventScroll: true });
  }
  return copied;
}

function isFocusable(value: Element | null): value is HTMLElement {
  return Boolean(value && "focus" in value && typeof value.focus === "function");
}
