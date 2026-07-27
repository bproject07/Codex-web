/**
 * A directory on the host that runs Codex Web Terminal.
 *
 * `id` is intentionally opaque. Consumers must send it back to the adapter
 * instead of joining or interpreting platform-specific paths in the browser.
 */
export interface WorkspaceDirectory {
  id: string;
  name: string;
  path: string;
}

export type WorkspaceAgent = "codex" | "claude" | "agy";

/**
 * One non-recursive directory listing.
 *
 * A roots listing has a null `current`, null `parentId`, and exposes drives or
 * mount roots through `directories`. Regular listings contain directories
 * only; files never enter this UI contract.
 */
export interface WorkspaceDirectoryListing {
  current: WorkspaceDirectory | null;
  parentId: string | null;
  breadcrumbs: WorkspaceDirectory[];
  directories: WorkspaceDirectory[];
  /** True when the server returned its safe maximum number of directories. */
  truncated: boolean;
}

export interface WorkspaceFavorite {
  /** Stable identity of the favorite record, not the directory id. */
  id: string;
  directory: WorkspaceDirectory;
  label?: string | null;
  preferredAgent?: WorkspaceAgent | null;
  available?: boolean;
  unavailableReason?: string | null;
}

export interface WorkspaceRecent {
  directory: WorkspaceDirectory;
  /** Milliseconds since the Unix epoch, supplied by the server. */
  lastOpenedAt: number;
  lastAgent?: WorkspaceAgent | null;
  available?: boolean;
  unavailableReason?: string | null;
}

export interface WorkspaceLibrary {
  favorites: WorkspaceFavorite[];
  recent: WorkspaceRecent[];
}

export interface WorkspaceAdapterOptions {
  signal?: AbortSignal;
}

/**
 * Boundary between the picker and the authenticated HTTP client.
 *
 * This keeps transport details, bearer tokens, and server DTO migration out
 * of the view. Callers should pass a stable adapter object (for example, one
 * created with useMemo) so an open picker is not reloaded unnecessarily.
 */
export interface WorkspaceBrowserAdapter {
  loadLibrary(options?: WorkspaceAdapterOptions): Promise<WorkspaceLibrary>;
  listRoots(
    options?: WorkspaceAdapterOptions,
  ): Promise<WorkspaceDirectoryListing>;
  listDirectory(
    directoryId: string,
    options?: WorkspaceAdapterOptions,
  ): Promise<WorkspaceDirectoryListing>;
  resolvePath(
    path: string,
    options?: WorkspaceAdapterOptions,
  ): Promise<WorkspaceDirectoryListing>;
  addFavorite(
    directory: WorkspaceDirectory,
    options?: WorkspaceAdapterOptions,
  ): Promise<WorkspaceFavorite>;
  removeFavorite(
    favoriteId: string,
    options?: WorkspaceAdapterOptions,
  ): Promise<void>;
}
