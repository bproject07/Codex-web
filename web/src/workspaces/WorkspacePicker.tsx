import {
  useCallback,
  useEffect,
  useId,
  useReducer,
  useRef,
  type FormEvent,
  type KeyboardEvent,
  type ReactNode,
} from "react";
import {
  createWorkspaceBrowserState,
  displayDirectoryName,
  displayFavoriteName,
  errorMessage,
  findFavorite,
  isAbortError,
  normalizedManualPath,
  workspaceTabFromKey,
  workspaceBrowserReducer,
  type WorkspaceErrorDomain,
  type WorkspacePickerTab,
} from "./workspaceBrowserModel";
import type {
  WorkspaceBrowserAdapter,
  WorkspaceAgent,
  WorkspaceDirectory,
  WorkspaceDirectoryListing,
  WorkspaceLibrary,
} from "./types";

export interface WorkspacePickerProps {
  adapter: WorkspaceBrowserAdapter;
  onChoose: (
    directory: WorkspaceDirectory,
    transition: WorkspacePickerTransition,
  ) => void;
  onStart?: (
    directory: WorkspaceDirectory,
    agent: WorkspaceAgent,
    transition: WorkspacePickerTransition,
  ) => void;
  onCancel?: () => void;
  initialTab?: WorkspacePickerTab;
  initialDirectoryId?: string | null;
  initialLibrary?: WorkspaceLibrary;
  initialListing?: WorkspaceDirectoryListing | null;
  title?: string;
  description?: string;
  chooseLabel?: string;
  disabled?: boolean;
}

export interface WorkspacePickerTransition {
  /**
   * Prevents focus from briefly jumping back to the opener when this picker
   * intentionally hands control to another dialog. Call this synchronously
   * before changing the parent state that unmounts the picker.
   */
  suppressFocusReturn(): void;
}

export function WorkspacePicker({
  adapter,
  onChoose,
  onStart,
  onCancel,
  initialTab = "favorites",
  initialDirectoryId = null,
  initialLibrary,
  initialListing = null,
  title = "Choose a project folder",
  description = "Browse folders on the server where the terminal will run.",
  chooseLabel = "Use folder",
  disabled = false,
}: WorkspacePickerProps) {
  const titleId = useId();
  const descriptionId = useId();
  const pathInputId = useId();
  const tabsId = useId();
  const dialogRef = useRef<HTMLElement>(null);
  const returnFocusRef = useRef<HTMLElement | null>(null);
  const suppressFocusReturnRef = useRef(false);
  const browseFocusFrameRef = useRef<number | null>(null);
  const navigationRequestRef = useRef<AbortController | null>(null);
  const libraryRequestRef = useRef<AbortController | null>(null);
  const mutationRequestRef = useRef<AbortController | null>(null);
  const [state, dispatch] = useReducer(
    workspaceBrowserReducer,
    undefined,
    () =>
      createWorkspaceBrowserState(
        initialTab,
        initialLibrary,
        initialListing,
      ),
  );

  useEffect(() => {
    returnFocusRef.current =
      document.activeElement instanceof HTMLElement
        ? document.activeElement
        : null;

    const frame = window.requestAnimationFrame(() => {
      const preferred =
        dialogRef.current?.querySelector<HTMLElement>(
          '[data-workspace-initial-focus="true"]:not([disabled])',
        ) ??
        dialogRef.current?.querySelector<HTMLElement>(
          'button:not([disabled]), input:not([disabled]), [tabindex]:not([tabindex="-1"])',
        );
      (preferred ?? dialogRef.current)?.focus({ preventScroll: true });
    });

    return () => {
      window.cancelAnimationFrame(frame);
      if (browseFocusFrameRef.current !== null) {
        window.cancelAnimationFrame(browseFocusFrameRef.current);
        browseFocusFrameRef.current = null;
      }
      if (
        !suppressFocusReturnRef.current &&
        returnFocusRef.current?.isConnected
      ) {
        returnFocusRef.current.focus({ preventScroll: true });
      }
    };
  }, []);

  const focusDialogBeforeReplacement = useCallback(() => {
    const target =
      dialogRef.current?.querySelector<HTMLElement>(
        '[role="tab"][aria-selected="true"]',
      ) ?? dialogRef.current;
    target?.focus({ preventScroll: true });
  }, []);

  const scheduleBrowseFocus = useCallback(() => {
    if (browseFocusFrameRef.current !== null) {
      window.cancelAnimationFrame(browseFocusFrameRef.current);
    }
    browseFocusFrameRef.current = window.requestAnimationFrame(() => {
      browseFocusFrameRef.current = null;
      const target =
        dialogRef.current?.querySelector<HTMLElement>(
          '[data-workspace-current-focus-target="true"]',
        ) ??
        dialogRef.current;
      target?.focus({ preventScroll: true });
    });
  }, []);

  const loadListing = useCallback(
    async (
      directoryId: string | null,
      focusAfterReplacement = false,
    ) => {
      navigationRequestRef.current?.abort();
      const controller = new AbortController();
      navigationRequestRef.current = controller;
      dispatch({ type: "directory_loading" });

      try {
        const listing = directoryId
          ? await adapter.listDirectory(directoryId, {
              signal: controller.signal,
            })
          : await adapter.listRoots({ signal: controller.signal });
        if (!controller.signal.aborted) {
          if (focusAfterReplacement) {
            focusDialogBeforeReplacement();
          }
          dispatch({ type: "directory_loaded", listing });
          if (focusAfterReplacement) {
            scheduleBrowseFocus();
          }
        }
      } catch (error) {
        if (!controller.signal.aborted && !isAbortError(error)) {
          dispatch({
            type: "directory_failed",
            message: errorMessage(error, "Could not open this folder."),
          });
        }
      }
    },
    [adapter, focusDialogBeforeReplacement, scheduleBrowseFocus],
  );

  const startLibraryLoad = useCallback((): AbortController | null => {
    if (libraryRequestRef.current) {
      return null;
    }

    const controller = new AbortController();
    libraryRequestRef.current = controller;
    dispatch({ type: "library_loading" });
    void adapter
      .loadLibrary({ signal: controller.signal })
      .then((library) => {
        if (!controller.signal.aborted) {
          dispatch({ type: "library_loaded", library });
        }
      })
      .catch((error: unknown) => {
        if (!controller.signal.aborted && !isAbortError(error)) {
          dispatch({
            type: "library_failed",
            message: errorMessage(
              error,
              "Could not load favorite and recent folders.",
            ),
          });
        }
      })
      .finally(() => {
        if (libraryRequestRef.current === controller) {
          libraryRequestRef.current = null;
        }
      });
    return controller;
  }, [adapter]);

  useEffect(() => {
    const libraryController = initialLibrary ? null : startLibraryLoad();

    if (!initialListing && initialTab === "browse") {
      void loadListing(initialDirectoryId);
    }

    return () => {
      libraryController?.abort();
      libraryRequestRef.current?.abort();
      libraryRequestRef.current = null;
      navigationRequestRef.current?.abort();
      mutationRequestRef.current?.abort();
    };
  }, [
    adapter,
    initialDirectoryId,
    initialLibrary,
    initialListing,
    initialTab,
    loadListing,
    startLibraryLoad,
  ]);

  const resolveManualPath = async (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault();
    const path = normalizedManualPath(state.pathInput);
    if (!path || disabled) {
      return;
    }

    navigationRequestRef.current?.abort();
    const controller = new AbortController();
    navigationRequestRef.current = controller;
    dispatch({ type: "directory_loading" });
    try {
      const listing = await adapter.resolvePath(path, {
        signal: controller.signal,
      });
      if (!controller.signal.aborted) {
        focusDialogBeforeReplacement();
        dispatch({ type: "directory_loaded", listing });
        scheduleBrowseFocus();
      }
    } catch (error) {
      if (!controller.signal.aborted && !isAbortError(error)) {
        dispatch({
          type: "directory_failed",
          message: errorMessage(error, "Could not open this path."),
        });
      }
    }
  };

  const addFavorite = async (directory: WorkspaceDirectory) => {
    if (
      disabled ||
      state.libraryLoading ||
      !state.libraryLoadedSuccessfully ||
      libraryRequestRef.current ||
      mutationRequestRef.current
    ) {
      return;
    }
    const controller = new AbortController();
    mutationRequestRef.current = controller;
    dispatch({ type: "favorite_mutating", id: directory.id });
    try {
      const favorite = await adapter.addFavorite(directory, {
        signal: controller.signal,
      });
      if (!controller.signal.aborted) {
        dispatch({ type: "favorite_added", favorite });
      }
    } catch (error) {
      if (!controller.signal.aborted && !isAbortError(error)) {
        dispatch({
          type: "favorite_failed",
          message: errorMessage(error, "Could not add this favorite."),
        });
      }
    } finally {
      if (mutationRequestRef.current === controller) {
        mutationRequestRef.current = null;
      }
    }
  };

  const removeFavorite = async (
    favoriteId: string,
    mutationId: string,
  ) => {
    if (
      disabled ||
      state.libraryLoading ||
      !state.libraryLoadedSuccessfully ||
      libraryRequestRef.current ||
      mutationRequestRef.current
    ) {
      return;
    }
    const controller = new AbortController();
    mutationRequestRef.current = controller;
    dispatch({ type: "favorite_mutating", id: mutationId });
    try {
      await adapter.removeFavorite(favoriteId, {
        signal: controller.signal,
      });
      if (!controller.signal.aborted) {
        focusDialogBeforeReplacement();
        dispatch({ type: "favorite_removed", favoriteId });
      }
    } catch (error) {
      if (!controller.signal.aborted && !isAbortError(error)) {
        dispatch({
          type: "favorite_failed",
          message: errorMessage(error, "Could not remove this favorite."),
        });
      }
    } finally {
      if (mutationRequestRef.current === controller) {
        mutationRequestRef.current = null;
      }
    }
  };

  const toggleFavorite = (directory: WorkspaceDirectory) => {
    if (
      disabled ||
      state.libraryLoading ||
      !state.libraryLoadedSuccessfully ||
      libraryRequestRef.current ||
      mutationRequestRef.current
    ) {
      return;
    }
    const favorite = findFavorite(state.library.favorites, directory.id);
    if (favorite) {
      void removeFavorite(favorite.id, directory.id);
    } else {
      void addFavorite(directory);
    }
  };

  const busy =
    disabled ||
    state.libraryLoading ||
    state.directoryLoading ||
    state.favoriteMutationId !== null;
  const currentDirectory = state.listing?.current ?? null;
  const currentFavorite = currentDirectory
    ? findFavorite(state.library.favorites, currentDirectory.id)
    : null;
  const visibleErrors = (
    ["library", "directory", "favorite"] as const
  ).flatMap((domain) => {
    const message = state.errors[domain];
    return message ? [{ domain, message }] : [];
  });
  const favoriteActionsDisabled =
    state.libraryLoading ||
    !state.libraryLoadedSuccessfully ||
    state.favoriteMutationId !== null;
  const transition: WorkspacePickerTransition = {
    suppressFocusReturn: () => {
      suppressFocusReturnRef.current = true;
    },
  };
  const startFromShortcut = onStart
    ? (directory: WorkspaceDirectory, agent: WorkspaceAgent) =>
        onStart(directory, agent, transition)
    : undefined;
  const selectTab = (tab: WorkspacePickerTab) => {
    dispatch({ type: "tab_changed", tab });
    if (
      tab === "browse" &&
      !state.listing &&
      !state.directoryLoading
    ) {
      void loadListing(initialDirectoryId);
    }
  };
  const browseFromShortcut = (directory: WorkspaceDirectory) => {
    focusDialogBeforeReplacement();
    dispatch({ type: "tab_changed", tab: "browse" });
    scheduleBrowseFocus();
    void loadListing(directory.id, true);
  };

  const handleDialogKeyDown = (event: KeyboardEvent<HTMLElement>) => {
    if (event.key === "Escape" && onCancel && !disabled) {
      event.preventDefault();
      onCancel();
      return;
    }

    if (event.key !== "Tab") {
      return;
    }

    const focusable = Array.from(
      dialogRef.current?.querySelectorAll<HTMLElement>(
        'button:not([disabled]), input:not([disabled]), select:not([disabled]), a[href], [tabindex]:not([tabindex="-1"])',
      ) ?? [],
    ).filter(
      (element) =>
        element.tabIndex >= 0 && !element.hasAttribute("aria-hidden"),
    );
    const first = focusable[0];
    const last = focusable.at(-1);

    if (!first || !last) {
      event.preventDefault();
      dialogRef.current?.focus({ preventScroll: true });
      return;
    }

    if (
      !(document.activeElement instanceof HTMLElement) ||
      !focusable.includes(document.activeElement)
    ) {
      event.preventDefault();
      (event.shiftKey ? last : first).focus({ preventScroll: true });
      return;
    }

    if (event.shiftKey && document.activeElement === first) {
      event.preventDefault();
      last.focus({ preventScroll: true });
    } else if (!event.shiftKey && document.activeElement === last) {
      event.preventDefault();
      first.focus({ preventScroll: true });
    }
  };

  return (
    <section
      ref={dialogRef}
      className="workspace-picker"
      role="dialog"
      aria-modal="true"
      aria-labelledby={titleId}
      aria-describedby={descriptionId}
      aria-busy={busy}
      tabIndex={-1}
      onKeyDown={handleDialogKeyDown}
      onMouseDown={(event) => event.stopPropagation()}
    >
      <header className="workspace-picker__header">
        <div>
          <h2 id={titleId}>{title}</h2>
          <p id={descriptionId}>{description}</p>
        </div>
        {onCancel && (
          <button
            type="button"
            className="workspace-picker__close"
            onClick={onCancel}
            disabled={disabled}
            aria-label="Close folder picker"
          >
            ×
          </button>
        )}
      </header>

      <div
        className="workspace-picker__tabs"
        role="tablist"
        aria-label="Project folder sources"
      >
        <TabButton
          tab="favorites"
          activeTab={state.activeTab}
          id={`${tabsId}-favorites-tab`}
          controlsId={`${tabsId}-favorites-panel`}
          onSelect={selectTab}
        >
          Favorites
        </TabButton>
        <TabButton
          tab="recent"
          activeTab={state.activeTab}
          id={`${tabsId}-recent-tab`}
          controlsId={`${tabsId}-recent-panel`}
          onSelect={selectTab}
        >
          Recent
        </TabButton>
        <TabButton
          tab="browse"
          activeTab={state.activeTab}
          id={`${tabsId}-browse-tab`}
          controlsId={`${tabsId}-browse-panel`}
          onSelect={selectTab}
        >
          Browse
        </TabButton>
      </div>

      <div className="workspace-picker__announcement" aria-live="polite">
        {state.libraryLoading || state.directoryLoading
          ? "Loading folders…"
          : state.favoriteMutationId
            ? "Updating favorites…"
            : ""}
      </div>

      {state.activeTab !== "favorites" && (
        <div
          id={`${tabsId}-favorites-panel`}
          role="tabpanel"
          aria-labelledby={`${tabsId}-favorites-tab`}
          hidden
        />
      )}
      {state.activeTab !== "recent" && (
        <div
          id={`${tabsId}-recent-panel`}
          role="tabpanel"
          aria-labelledby={`${tabsId}-recent-tab`}
          hidden
        />
      )}
      {state.activeTab !== "browse" && (
        <div
          id={`${tabsId}-browse-panel`}
          role="tabpanel"
          aria-labelledby={`${tabsId}-browse-tab`}
          hidden
        />
      )}

      {visibleErrors.map(({ domain, message }) => (
        <div
          key={domain}
          className={`workspace-picker__error workspace-picker__error--${domain}`}
          role="alert"
        >
          <span>{message}</span>
          {domain === "library" && (
            <button
              type="button"
              onClick={() => {
                focusDialogBeforeReplacement();
                startLibraryLoad();
              }}
              disabled={state.libraryLoading}
            >
              Retry
            </button>
          )}
          <button
            type="button"
            onClick={() => {
              focusDialogBeforeReplacement();
              dispatch({ type: "error_cleared", domain });
            }}
            aria-label={`Dismiss ${errorDomainLabel(domain)} error`}
          >
            Dismiss
          </button>
        </div>
      ))}

      {state.activeTab === "favorites" && (
        <ShortcutPanel
          kind="favorites"
          id={`${tabsId}-favorites-panel`}
          labelledBy={`${tabsId}-favorites-tab`}
          loading={state.libraryLoading}
          emptyMessage="No favorite folders yet. Browse to a folder and add it as a favorite."
        >
          {state.library.favorites.map((favorite) => (
            <ShortcutRow
              key={favorite.id}
              directory={favorite.directory}
              label={displayFavoriteName(favorite)}
              detail={favorite.directory.path}
              chooseLabel={chooseLabel}
              agent={favorite.preferredAgent ?? null}
              available={favorite.available !== false}
              unavailableReason={favorite.unavailableReason}
              disabled={disabled}
              onChoose={(directory) => onChoose(directory, transition)}
              onStart={startFromShortcut}
              onBrowse={browseFromShortcut}
              favorite
              favoriteMutationPending={favoriteActionsDisabled}
              onToggleFavorite={toggleFavorite}
            />
          ))}
        </ShortcutPanel>
      )}

      {state.activeTab === "recent" && (
        <ShortcutPanel
          kind="recent"
          id={`${tabsId}-recent-panel`}
          labelledBy={`${tabsId}-recent-tab`}
          loading={state.libraryLoading}
          emptyMessage="Folders used to start terminals will appear here."
        >
          {state.library.recent.map((recent) => {
            const favorite = findFavorite(
              state.library.favorites,
              recent.directory.id,
            );
            return (
              <ShortcutRow
                key={recent.directory.id}
                directory={recent.directory}
                label={displayDirectoryName(recent.directory)}
                detail={recent.directory.path}
                chooseLabel={chooseLabel}
                agent={recent.lastAgent ?? null}
                available={recent.available !== false}
                unavailableReason={recent.unavailableReason}
                disabled={disabled}
                onChoose={(directory) => onChoose(directory, transition)}
                onStart={startFromShortcut}
                onBrowse={browseFromShortcut}
                favorite={favorite !== null}
                favoriteMutationPending={favoriteActionsDisabled}
                onToggleFavorite={toggleFavorite}
              />
            );
          })}
        </ShortcutPanel>
      )}

      {state.activeTab === "browse" && (
        <div
          id={`${tabsId}-browse-panel`}
          className="workspace-picker__panel workspace-picker__panel--browse"
          role="tabpanel"
          aria-labelledby={`${tabsId}-browse-tab`}
        >
          <form
            className="workspace-picker__path-form"
            onSubmit={(event) => void resolveManualPath(event)}
          >
            <label htmlFor={pathInputId}>Folder path</label>
            <div>
              <input
                id={pathInputId}
                data-workspace-path-input="true"
                type="text"
                value={state.pathInput}
                onChange={(event) =>
                  dispatch({
                    type: "path_changed",
                    path: event.currentTarget.value,
                  })
                }
                placeholder="Enter a full server path"
                autoComplete="off"
                autoCapitalize="none"
                autoCorrect="off"
                enterKeyHint="go"
                spellCheck={false}
                disabled={disabled || state.directoryLoading}
              />
              <button
                type="submit"
                disabled={
                  disabled ||
                  state.directoryLoading ||
                  !normalizedManualPath(state.pathInput)
                }
              >
                Open
              </button>
            </div>
          </form>

          <nav
            className="workspace-picker__breadcrumbs"
            aria-label="Folder breadcrumbs"
          >
            <button
              type="button"
              onClick={() => void loadListing(null, true)}
              disabled={disabled || state.directoryLoading}
              aria-label="Show filesystem roots"
            >
              Roots
            </button>
            {state.listing?.breadcrumbs.map((directory) => (
              <span key={directory.id}>
                <span aria-hidden="true">/</span>
                <button
                  type="button"
                  onClick={() => void loadListing(directory.id, true)}
                  disabled={
                    disabled ||
                    state.directoryLoading ||
                    directory.id === state.listing?.current?.id
                  }
                  aria-current={
                    directory.id === state.listing?.current?.id
                      ? "location"
                      : undefined
                  }
                >
                  {displayDirectoryName(directory)}
                </button>
              </span>
            ))}
          </nav>

          <div
            className="workspace-picker__current-actions"
            data-workspace-current-focus-target="true"
            tabIndex={-1}
          >
            <button
              type="button"
              onClick={() =>
                void loadListing(state.listing?.parentId ?? null, true)
              }
              disabled={
                disabled ||
                state.directoryLoading ||
                (!state.listing?.current && !state.listing?.parentId)
              }
              aria-label="Open parent folder"
            >
              ↑ Up
            </button>
            {currentDirectory && (
              <>
                <button
                  type="button"
                  onClick={() => toggleFavorite(currentDirectory)}
                  disabled={
                    disabled || favoriteActionsDisabled
                  }
                  aria-pressed={currentFavorite !== null}
                >
                  {currentFavorite ? "★ Favorite" : "☆ Favorite"}
                </button>
                <button
                  type="button"
                  className="workspace-picker__choose-current"
                  onClick={() => onChoose(currentDirectory, transition)}
                  disabled={disabled || state.directoryLoading}
                >
                  {chooseLabel}
                </button>
              </>
            )}
          </div>

          <DirectoryList
            listing={state.listing}
            loading={state.directoryLoading}
            disabled={disabled}
            onOpen={(directory) => void loadListing(directory.id, true)}
          />
          {state.listing?.truncated && (
            <p className="workspace-picker__truncated" role="status">
              This folder has more subfolders than can be shown at once. Enter
              a full path above to open another folder.
            </p>
          )}
        </div>
      )}
    </section>
  );
}

interface TabButtonProps {
  tab: WorkspacePickerTab;
  activeTab: WorkspacePickerTab;
  id: string;
  controlsId: string;
  onSelect: (tab: WorkspacePickerTab) => void;
  children: string;
}

function TabButton({
  tab,
  activeTab,
  id,
  controlsId,
  onSelect,
  children,
}: TabButtonProps) {
  const selected = tab === activeTab;
  const handleKeyDown = (event: KeyboardEvent<HTMLButtonElement>) => {
    const nextTab = workspaceTabFromKey(tab, event.key);
    if (!nextTab) {
      return;
    }

    event.preventDefault();
    onSelect(nextTab);
    event.currentTarget.parentElement
      ?.querySelector<HTMLButtonElement>(
        `[data-workspace-tab="${nextTab}"]`,
      )
      ?.focus({ preventScroll: true });
  };

  return (
    <button
      type="button"
      id={id}
      role="tab"
      data-workspace-tab={tab}
      data-workspace-initial-focus={selected ? "true" : undefined}
      aria-selected={selected}
      aria-controls={controlsId}
      tabIndex={selected ? 0 : -1}
      onClick={() => onSelect(tab)}
      onKeyDown={handleKeyDown}
    >
      {children}
    </button>
  );
}

interface ShortcutPanelProps {
  kind: "favorites" | "recent";
  id: string;
  labelledBy: string;
  loading: boolean;
  emptyMessage: string;
  children: ReactNode;
}

function ShortcutPanel({
  kind,
  id,
  labelledBy,
  loading,
  emptyMessage,
  children,
}: ShortcutPanelProps) {
  const childCount = Array.isArray(children)
    ? children.length
    : children
      ? 1
      : 0;

  return (
    <div
      id={id}
      className={`workspace-picker__panel workspace-picker__panel--${kind}`}
      role="tabpanel"
      aria-labelledby={labelledBy}
    >
      {!loading && childCount === 0 ? (
        <p className="workspace-picker__empty" role="status">
          {emptyMessage}
        </p>
      ) : (
        <ul className="workspace-picker__shortcuts">{children}</ul>
      )}
    </div>
  );
}

interface ShortcutRowProps {
  directory: WorkspaceDirectory;
  label: string;
  detail: string;
  chooseLabel: string;
  agent: WorkspaceAgent | null;
  available: boolean;
  unavailableReason?: string | null;
  disabled: boolean;
  favorite: boolean;
  favoriteMutationPending: boolean;
  onChoose: (directory: WorkspaceDirectory) => void;
  onStart?: (directory: WorkspaceDirectory, agent: WorkspaceAgent) => void;
  onBrowse: (directory: WorkspaceDirectory) => void;
  onToggleFavorite: (directory: WorkspaceDirectory) => void;
}

function ShortcutRow({
  directory,
  label,
  detail,
  chooseLabel,
  agent,
  available,
  unavailableReason,
  disabled,
  favorite,
  favoriteMutationPending,
  onChoose,
  onStart,
  onBrowse,
  onToggleFavorite,
}: ShortcutRowProps) {
  return (
    <li className="workspace-picker__shortcut">
      <button
        type="button"
        className="workspace-picker__shortcut-main"
        onClick={() => onChoose(directory)}
        disabled={disabled || !available}
        aria-label={`${chooseLabel}: ${detail}`}
      >
        <strong>{label}</strong>
        <span>{detail}</span>
        {!available && (
          <span className="workspace-picker__shortcut-unavailable">
            {unavailableReason?.trim() || "Folder is unavailable"}
          </span>
        )}
      </button>
      <div className="workspace-picker__shortcut-actions">
        {agent && onStart && (
          <button
            type="button"
            className="workspace-picker__shortcut-start"
            onClick={() => onStart(directory, agent)}
            disabled={disabled || !available}
          >
            Start {agentLabel(agent)}
          </button>
        )}
        <button
          type="button"
          onClick={() => onBrowse(directory)}
          disabled={disabled || !available}
          aria-label={`Browse inside ${detail}`}
        >
          Browse
        </button>
        <button
          type="button"
          onClick={() => onToggleFavorite(directory)}
          disabled={disabled || favoriteMutationPending}
          aria-pressed={favorite}
          aria-label={
            favorite
              ? `Remove ${detail} from favorites`
              : `Add ${detail} to favorites`
          }
        >
          {favorite ? "★" : "☆"}
        </button>
      </div>
    </li>
  );
}

function agentLabel(agent: WorkspaceAgent): string {
  switch (agent) {
    case "codex":
      return "Codex";
    case "claude":
      return "Claude";
    case "agy":
      return "AGY";
  }
}

function errorDomainLabel(domain: WorkspaceErrorDomain): string {
  switch (domain) {
    case "library":
      return "folder library";
    case "directory":
      return "directory";
    case "favorite":
      return "favorite";
  }
}

interface DirectoryListProps {
  listing: WorkspaceDirectoryListing | null;
  loading: boolean;
  disabled: boolean;
  onOpen: (directory: WorkspaceDirectory) => void;
}

function DirectoryList({
  listing,
  loading,
  disabled,
  onOpen,
}: DirectoryListProps) {
  if (loading && !listing) {
    return (
      <p className="workspace-picker__empty" role="status">
        Loading folders…
      </p>
    );
  }

  if (!listing || listing.directories.length === 0) {
    return (
      <p className="workspace-picker__empty" role="status">
        This folder contains no subfolders.
      </p>
    );
  }

  return (
    <ul className="workspace-picker__directories" aria-label="Folders">
      {listing.directories.map((directory) => (
        <li key={directory.id}>
          <button
            type="button"
            onClick={() => onOpen(directory)}
            disabled={disabled || loading}
            aria-label={`Open folder ${directory.path}`}
          >
            <span aria-hidden="true">📁</span>
            <span>{displayDirectoryName(directory)}</span>
          </button>
        </li>
      ))}
    </ul>
  );
}
