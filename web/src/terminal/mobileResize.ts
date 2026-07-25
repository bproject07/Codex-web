export interface TerminalSize {
  cols: number;
  rows: number;
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
