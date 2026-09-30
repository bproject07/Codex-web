export const TERMINAL_VIEW_CACHE_LIMIT = 6;

/** Oldest visit first. Only browser views are evicted; PTYs are untouched. */
export function retainTerminalViews(
  previous: readonly string[],
  selected: string,
  available: readonly string[],
  enabled: boolean,
): readonly string[] {
  const availableIds = new Set(available);
  const retained = enabled
    ? previous.filter((id) => availableIds.has(id) && id !== selected)
    : [];
  if (availableIds.has(selected)) {
    retained.push(selected);
  }
  const next = retained.slice(-TERMINAL_VIEW_CACHE_LIMIT);
  return next.length === previous.length &&
    next.every((id, index) => id === previous[index])
    ? previous
    : next;
}
