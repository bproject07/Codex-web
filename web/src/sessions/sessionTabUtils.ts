export function compactSessionName(name: string, index: number): string {
  const terminalNumber = name.match(/(?:terminal\s*)?(\d+)$/i)?.[1];
  return terminalNumber ? `T${terminalNumber}` : `T${index + 1}`;
}

export function horizontalWheelDelta(
  deltaX: number,
  deltaY: number,
): number {
  return Math.abs(deltaX) > Math.abs(deltaY) ? deltaX : deltaY;
}
