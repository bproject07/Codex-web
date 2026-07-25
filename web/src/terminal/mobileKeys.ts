export const MOBILE_KEY_SEQUENCES = {
  escape: "\u001b",
  tab: "\t",
  arrowUp: "\u001b[A",
  arrowDown: "\u001b[B",
  arrowRight: "\u001b[C",
  arrowLeft: "\u001b[D",
  enter: "\r",
  pageUp: "\u001b[5~",
  pageDown: "\u001b[6~",
  ctrlC: "\u0003",
  ctrlL: "\u000c",
} as const;

export interface CtrlConversion {
  data: string;
  consumed: boolean;
}

export function applyCtrlToInput(input: string): CtrlConversion {
  if (input.length !== 1) {
    return { data: input, consumed: false };
  }

  const code = input.toUpperCase().charCodeAt(0);
  if (code >= 65 && code <= 90) {
    return {
      data: String.fromCharCode(code - 64),
      consumed: true,
    };
  }

  return { data: input, consumed: false };
}

