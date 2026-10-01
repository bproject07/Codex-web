export function terminalDimensions(cols: number, rows: number) {
  return {
    cols: Math.min(500, Math.max(20, Math.floor(cols))),
    rows: Math.min(300, Math.max(5, Math.floor(rows))),
  };
}
