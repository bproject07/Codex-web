export interface TerminalSize {
  cols: number;
  rows: number;
}

export interface TerminalScrollbarOptions {
  overviewRuler?: {
    width: number;
  };
}

const MOBILE_TERMINAL_SCROLLBAR_WIDTH = 28;

export function terminalScrollbarWidth(
  coarsePointer: boolean,
): number | undefined {
  return coarsePointer ? MOBILE_TERMINAL_SCROLLBAR_WIDTH : undefined;
}

/** xterm treats an explicitly present `overviewRuler: undefined` differently
 * from an omitted option and crashes while reading `.width`. Preserve its
 * desktop defaults by returning no property at all for a fine pointer. */
export function terminalScrollbarOptions(
  coarsePointer: boolean,
): TerminalScrollbarOptions {
  const width = terminalScrollbarWidth(coarsePointer);
  return width === undefined ? {} : { overviewRuler: { width } };
}

export function isMobileRowOnlyResize(
  previous: TerminalSize,
  next: TerminalSize,
  coarsePointer: boolean,
): boolean {
  return (
    coarsePointer &&
    previous.cols === next.cols &&
    previous.rows !== next.rows
  );
}
