import { afterEach, describe, expect, it, vi } from "vitest";
import { copyTextToClipboard } from "./clipboard";

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("copyTextToClipboard", () => {
  it("uses the asynchronous Clipboard API when available", async () => {
    const writeText = vi.fn().mockResolvedValue(undefined);
    vi.stubGlobal("navigator", { clipboard: { writeText } });

    await expect(copyTextToClipboard("install agent")).resolves.toBe(true);
    expect(writeText).toHaveBeenCalledWith("install agent");
  });

  it("falls back to a temporary readonly textarea and restores focus", async () => {
    const previousFocus = vi.fn();
    const textarea = {
      value: "",
      readOnly: false,
      inputMode: "",
      tabIndex: 0,
      style: {},
      setAttribute: vi.fn(),
      focus: vi.fn(),
      select: vi.fn(),
      setSelectionRange: vi.fn(),
      remove: vi.fn(),
    };
    const appendChild = vi.fn();
    const execCommand = vi.fn().mockReturnValue(true);

    vi.stubGlobal("navigator", {
      clipboard: {
        writeText: vi.fn().mockRejectedValue(new Error("insecure context")),
      },
    });
    vi.stubGlobal("document", {
      activeElement: { focus: previousFocus },
      body: { appendChild },
      createElement: vi.fn().mockReturnValue(textarea),
      execCommand,
    });

    await expect(copyTextToClipboard("install agent")).resolves.toBe(true);
    expect(textarea.value).toBe("install agent");
    expect(textarea.readOnly).toBe(true);
    expect(textarea.inputMode).toBe("none");
    expect(textarea.setAttribute).toHaveBeenCalledWith("aria-hidden", "true");
    expect(appendChild).toHaveBeenCalledWith(textarea);
    expect(textarea.select).toHaveBeenCalledOnce();
    expect(execCommand).toHaveBeenCalledWith("copy");
    expect(textarea.remove).toHaveBeenCalledOnce();
    expect(previousFocus).toHaveBeenCalledWith({ preventScroll: true });
  });

  it("cleans up and reports failure when legacy copy is rejected", async () => {
    const textarea = {
      value: "",
      readOnly: false,
      inputMode: "",
      tabIndex: 0,
      style: {},
      setAttribute: vi.fn(),
      focus: vi.fn(),
      select: vi.fn(),
      setSelectionRange: vi.fn(),
      remove: vi.fn(),
    };
    vi.stubGlobal("navigator", {});
    vi.stubGlobal("document", {
      activeElement: null,
      body: { appendChild: vi.fn() },
      createElement: vi.fn().mockReturnValue(textarea),
      execCommand: vi.fn().mockReturnValue(false),
    });

    await expect(copyTextToClipboard("install agent")).resolves.toBe(false);
    expect(textarea.remove).toHaveBeenCalledOnce();
  });
});
