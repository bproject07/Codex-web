export interface TerminalSize {
  cols: number;
  rows: number;
}

const MOBILE_TERMINAL_SCROLLBAR_WIDTH = 28;

export function terminalScrollbarWidth(
  coarsePointer: boolean,
): number | undefined {
  return coarsePointer ? MOBILE_TERMINAL_SCROLLBAR_WIDTH : undefined;
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
