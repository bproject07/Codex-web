import { afterEach, describe, expect, it, vi } from "vitest";
import { clearSessionToken } from "./api";

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("browser token cleanup", () => {
  it("forgets storage and removes a fallback token from the address", () => {
    const removeItem = vi.fn();
    const replaceState = vi.fn();
    vi.stubGlobal("window", {
      sessionStorage: { removeItem },
      location: {
        href: "https://terminal.example/app?mode=full&token=secret#terminal",
      },
      history: { replaceState },
    });

    clearSessionToken();

    expect(removeItem).toHaveBeenCalledWith("codex-web-token");
    expect(replaceState).toHaveBeenCalledWith(
      null,
      "",
      "/app?mode=full#terminal",
    );
  });

  it("still clears storage when the visible address has no token", () => {
    const removeItem = vi.fn();
    const replaceState = vi.fn();
    vi.stubGlobal("window", {
      sessionStorage: { removeItem },
      location: { href: "https://terminal.example/app?mode=full#terminal" },
      history: { replaceState },
    });

    clearSessionToken();

    expect(removeItem).toHaveBeenCalledWith("codex-web-token");
    expect(replaceState).not.toHaveBeenCalled();
  });

  it("navigates to a clean same-origin address if history cleanup is blocked", () => {
    const replace = vi.fn();
    vi.stubGlobal("window", {
      sessionStorage: { removeItem: vi.fn() },
      location: {
        href: "https://terminal.example/app?token=secret&mode=full#terminal",
        replace,
      },
      history: {
        replaceState: vi.fn(() => {
          throw new DOMException("blocked", "SecurityError");
        }),
      },
    });

    clearSessionToken();

    expect(replace).toHaveBeenCalledWith("/app?mode=full#terminal");
  });
});
