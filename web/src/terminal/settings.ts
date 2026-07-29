import type { ITheme } from "@xterm/xterm";

export type ThemeName = "windows" | "midnight" | "high-contrast";

export interface TerminalSettings {
  fontSize: number;
  cursorBlink: boolean;
  scrollback: number;
  theme: ThemeName;
  mobileKeys: boolean;
}

export const DEFAULT_SETTINGS: TerminalSettings = {
  fontSize: 14,
  cursorBlink: true,
  scrollback: 10_000,
  theme: "windows",
  mobileKeys: true,
};

export const TERMINAL_THEMES: Record<ThemeName, ITheme> = {
  windows: {
    background: "#0c0c0c",
    foreground: "#cccccc",
    cursor: "#f2f2f2",
    cursorAccent: "#0c0c0c",
    selectionBackground: "#264f78",
    black: "#0c0c0c",
    // Red, blue, and magenta are brightened from the classic Campbell values,
    // which fall below a 4.5:1 contrast ratio on this background and made
    // agent TUI text hard to read. Hues are preserved.
    red: "#d6494d",
    green: "#13a10e",
    yellow: "#c19c00",
    blue: "#2472ff",
    magenta: "#c832e0",
    cyan: "#3a96dd",
    white: "#cccccc",
    brightBlack: "#767676",
    brightRed: "#e74856",
    brightGreen: "#16c60c",
    brightYellow: "#f9f1a5",
    brightBlue: "#3b78ff",
    brightMagenta: "#d258ef",
    brightCyan: "#61d6d6",
    brightWhite: "#f2f2f2",
  },
  midnight: {
    background: "#090b10",
    foreground: "#d6d9e0",
    cursor: "#8ab4f8",
    selectionBackground: "#24365a",
    black: "#11141b",
    red: "#f07178",
    green: "#c3e88d",
    yellow: "#ffcb6b",
    blue: "#82aaff",
    magenta: "#c792ea",
    cyan: "#89ddff",
    white: "#d6d9e0",
    brightBlack: "#596273",
    brightRed: "#ff8b92",
    brightGreen: "#d5f4a5",
    brightYellow: "#ffdc8b",
    brightBlue: "#a2c0ff",
    brightMagenta: "#d8a9f3",
    brightCyan: "#a7e8ff",
    brightWhite: "#ffffff",
  },
  "high-contrast": {
    background: "#000000",
    foreground: "#ffffff",
    cursor: "#ffffff",
    selectionBackground: "#005fcc",
    black: "#000000",
    red: "#ff4b4b",
    green: "#55ff55",
    yellow: "#ffff55",
    blue: "#5b8cff",
    magenta: "#ff55ff",
    cyan: "#55ffff",
    white: "#ffffff",
    brightBlack: "#888888",
    brightRed: "#ff7777",
    brightGreen: "#77ff77",
    brightYellow: "#ffff77",
    brightBlue: "#77a0ff",
    brightMagenta: "#ff77ff",
    brightCyan: "#77ffff",
    brightWhite: "#ffffff",
  },
};

const SETTINGS_KEY = "codex-web-terminal-settings";

export function loadSettings(): TerminalSettings {
  try {
    const raw = window.sessionStorage.getItem(SETTINGS_KEY);
    if (!raw) {
      return DEFAULT_SETTINGS;
    }
    const parsed = JSON.parse(raw) as Partial<TerminalSettings>;
    return {
      fontSize: clampNumber(parsed.fontSize, 11, 24, DEFAULT_SETTINGS.fontSize),
      cursorBlink:
        typeof parsed.cursorBlink === "boolean"
          ? parsed.cursorBlink
          : DEFAULT_SETTINGS.cursorBlink,
      scrollback: clampNumber(
        parsed.scrollback,
        1_000,
        50_000,
        DEFAULT_SETTINGS.scrollback,
      ),
      theme: isThemeName(parsed.theme) ? parsed.theme : DEFAULT_SETTINGS.theme,
      mobileKeys:
        typeof parsed.mobileKeys === "boolean"
          ? parsed.mobileKeys
          : DEFAULT_SETTINGS.mobileKeys,
    };
  } catch {
    return DEFAULT_SETTINGS;
  }
}

export function saveSettings(settings: TerminalSettings): void {
  try {
    window.sessionStorage.setItem(SETTINGS_KEY, JSON.stringify(settings));
  } catch {
    // Settings remain active in memory when session storage is unavailable.
  }
}

function clampNumber(
  value: unknown,
  minimum: number,
  maximum: number,
  fallback: number,
): number {
  if (typeof value !== "number" || !Number.isFinite(value)) {
    return fallback;
  }
  return Math.min(maximum, Math.max(minimum, Math.round(value)));
}

function isThemeName(value: unknown): value is ThemeName {
  return value === "windows" || value === "midnight" || value === "high-contrast";
}

