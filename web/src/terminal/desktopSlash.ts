export interface DesktopSlashContext {
  key: string;
  altKey: boolean;
  ctrlKey: boolean;
  metaKey: boolean;
  defaultPrevented: boolean;
  isComposing: boolean;
  coarsePointer: boolean;
  dialogOpen: boolean;
  editableTarget: boolean;
  terminalAvailable: boolean;
}

export function shouldRouteDesktopSlash({
  key,
  altKey,
  ctrlKey,
  metaKey,
  defaultPrevented,
  isComposing,
  coarsePointer,
  dialogOpen,
  editableTarget,
  terminalAvailable,
}: DesktopSlashContext): boolean {
  return (
    key === "/" &&
    !altKey &&
    !ctrlKey &&
    !metaKey &&
    !defaultPrevented &&
    !isComposing &&
    !coarsePointer &&
    !dialogOpen &&
    !editableTarget &&
    terminalAvailable
  );
}
