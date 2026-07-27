import type {
  WorkspaceDirectory,
  WorkspaceDirectoryListing,
  WorkspaceFavorite,
  WorkspaceLibrary,
} from "./types";

export type WorkspacePickerTab = "favorites" | "recent" | "browse";
export type WorkspaceErrorDomain = "library" | "directory" | "favorite";

export interface WorkspaceBrowserErrors {
  library: string | null;
  directory: string | null;
  favorite: string | null;
}

export interface WorkspaceBrowserState {
  activeTab: WorkspacePickerTab;
  library: WorkspaceLibrary;
  listing: WorkspaceDirectoryListing | null;
  pathInput: string;
  libraryLoading: boolean;
  libraryLoadedSuccessfully: boolean;
  directoryLoading: boolean;
  favoriteMutationId: string | null;
  errors: WorkspaceBrowserErrors;
}

export type WorkspaceBrowserAction =
  | { type: "tab_changed"; tab: WorkspacePickerTab }
  | { type: "path_changed"; path: string }
  | { type: "library_loading" }
  | { type: "library_loaded"; library: WorkspaceLibrary }
  | { type: "library_failed"; message: string }
  | { type: "directory_loading" }
  | { type: "directory_loaded"; listing: WorkspaceDirectoryListing }
  | { type: "directory_failed"; message: string }
  | { type: "favorite_mutating"; id: string }
  | { type: "favorite_added"; favorite: WorkspaceFavorite }
  | { type: "favorite_removed"; favoriteId: string }
  | { type: "favorite_failed"; message: string }
  | { type: "error_cleared"; domain: WorkspaceErrorDomain };

const EMPTY_LIBRARY: WorkspaceLibrary = {
  favorites: [],
  recent: [],
};

const DIRECTORY_COLLATOR = new Intl.Collator(undefined, {
  numeric: true,
  sensitivity: "base",
});

export function createWorkspaceBrowserState(
  activeTab: WorkspacePickerTab = "favorites",
  library?: WorkspaceLibrary,
  listing: WorkspaceDirectoryListing | null = null,
): WorkspaceBrowserState {
  return {
    activeTab,
    library: library ?? EMPTY_LIBRARY,
    listing: listing
      ? { ...listing, directories: sortDirectories(listing.directories) }
      : null,
    pathInput: listing?.current?.path ?? "",
    libraryLoading: library === undefined,
    libraryLoadedSuccessfully: library !== undefined,
    directoryLoading: listing === null && activeTab === "browse",
    favoriteMutationId: null,
    errors: {
      library: null,
      directory: null,
      favorite: null,
    },
  };
}

export function workspaceBrowserReducer(
  state: WorkspaceBrowserState,
  action: WorkspaceBrowserAction,
): WorkspaceBrowserState {
  switch (action.type) {
    case "tab_changed":
      return { ...state, activeTab: action.tab };
    case "path_changed":
      return {
        ...state,
        pathInput: action.path,
        errors: clearDomainError(state.errors, "directory"),
      };
    case "library_loading":
      return {
        ...state,
        libraryLoading: true,
        libraryLoadedSuccessfully: false,
        errors: clearDomainError(state.errors, "library"),
      };
    case "library_loaded":
      return {
        ...state,
        library: action.library,
        libraryLoading: false,
        libraryLoadedSuccessfully: true,
        errors: clearDomainError(state.errors, "library"),
      };
    case "library_failed":
      return {
        ...state,
        libraryLoading: false,
        libraryLoadedSuccessfully: false,
        errors: setDomainError(state.errors, "library", action.message),
      };
    case "directory_loading":
      return {
        ...state,
        directoryLoading: true,
        errors: clearDomainError(state.errors, "directory"),
      };
    case "directory_loaded":
      return {
        ...state,
        listing: {
          ...action.listing,
          directories: sortDirectories(action.listing.directories),
        },
        pathInput: action.listing.current?.path ?? "",
        directoryLoading: false,
        errors: clearDomainError(state.errors, "directory"),
      };
    case "directory_failed":
      return {
        ...state,
        directoryLoading: false,
        errors: setDomainError(state.errors, "directory", action.message),
      };
    case "favorite_mutating":
      if (
        !state.libraryLoadedSuccessfully ||
        state.favoriteMutationId !== null
      ) {
        return state;
      }
      return {
        ...state,
        favoriteMutationId: action.id,
        errors: clearDomainError(state.errors, "favorite"),
      };
    case "favorite_added":
      if (
        !state.libraryLoadedSuccessfully ||
        state.favoriteMutationId === null
      ) {
        return state;
      }
      return {
        ...state,
        library: {
          ...state.library,
          favorites: replaceFavoriteForDirectory(
            state.library.favorites,
            action.favorite,
          ),
        },
        favoriteMutationId: null,
        errors: clearDomainError(state.errors, "favorite"),
      };
    case "favorite_removed":
      if (
        !state.libraryLoadedSuccessfully ||
        state.favoriteMutationId === null
      ) {
        return state;
      }
      return {
        ...state,
        library: {
          ...state.library,
          favorites: state.library.favorites.filter(
            (favorite) => favorite.id !== action.favoriteId,
          ),
        },
        favoriteMutationId: null,
        errors: clearDomainError(state.errors, "favorite"),
      };
    case "favorite_failed":
      return {
        ...state,
        favoriteMutationId: null,
        errors: setDomainError(state.errors, "favorite", action.message),
      };
    case "error_cleared":
      return {
        ...state,
        errors: clearDomainError(state.errors, action.domain),
      };
  }
}

export function workspaceTabFromKey(
  current: WorkspacePickerTab,
  key: string,
): WorkspacePickerTab | null {
  const tabs: readonly WorkspacePickerTab[] = [
    "favorites",
    "recent",
    "browse",
  ];
  const currentIndex = tabs.indexOf(current);

  switch (key) {
    case "ArrowRight":
      return tabs[(currentIndex + 1) % tabs.length];
    case "ArrowLeft":
      return tabs[(currentIndex - 1 + tabs.length) % tabs.length];
    case "Home":
      return tabs[0];
    case "End":
      return tabs[tabs.length - 1];
    default:
      return null;
  }
}

export function sortDirectories(
  directories: readonly WorkspaceDirectory[],
): WorkspaceDirectory[] {
  return [...directories].sort((left, right) =>
    DIRECTORY_COLLATOR.compare(left.name, right.name),
  );
}

export function findFavorite(
  favorites: readonly WorkspaceFavorite[],
  directoryId: string,
): WorkspaceFavorite | null {
  return (
    favorites.find(
      (favorite) => favorite.directory.id === directoryId,
    ) ?? null
  );
}

export function displayDirectoryName(directory: WorkspaceDirectory): string {
  return directory.name.trim() || directory.path;
}

export function displayFavoriteName(favorite: WorkspaceFavorite): string {
  return favorite.label?.trim() || displayDirectoryName(favorite.directory);
}

export function normalizedManualPath(path: string): string {
  return path.trim();
}

export function errorMessage(error: unknown, fallback: string): string {
  if (error instanceof Error && error.message.trim()) {
    return error.message;
  }
  return fallback;
}

export function isAbortError(error: unknown): boolean {
  return (
    error instanceof DOMException
      ? error.name === "AbortError"
      : error instanceof Error && error.name === "AbortError"
  );
}

function replaceFavoriteForDirectory(
  favorites: readonly WorkspaceFavorite[],
  replacement: WorkspaceFavorite,
): WorkspaceFavorite[] {
  return [
    ...favorites.filter(
      (favorite) =>
        favorite.id !== replacement.id &&
        favorite.directory.id !== replacement.directory.id,
    ),
    replacement,
  ];
}

function clearDomainError(
  errors: WorkspaceBrowserErrors,
  domain: WorkspaceErrorDomain,
): WorkspaceBrowserErrors {
  return setDomainError(errors, domain, null);
}

function setDomainError(
  errors: WorkspaceBrowserErrors,
  domain: WorkspaceErrorDomain,
  message: string | null,
): WorkspaceBrowserErrors {
  return { ...errors, [domain]: message };
}
