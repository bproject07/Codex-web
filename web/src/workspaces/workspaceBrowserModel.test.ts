import { describe, expect, it } from "vitest";
import {
  createWorkspaceBrowserState,
  displayDirectoryName,
  displayFavoriteName,
  findFavorite,
  normalizedManualPath,
  sortDirectories,
  workspaceTabFromKey,
  workspaceBrowserReducer,
} from "./workspaceBrowserModel";
import type {
  WorkspaceDirectory,
  WorkspaceFavorite,
} from "./types";

const ROOT: WorkspaceDirectory = {
  id: "root-c",
  name: "C:\\",
  path: "C:\\",
};

const PROJECT: WorkspaceDirectory = {
  id: "project",
  name: "Codex-web",
  path: "C:\\Projects\\Codex-web",
};

describe("workspace browser model", () => {
  it("sorts only a copied directory list with natural, case-insensitive order", () => {
    const directories = [
      { id: "12", name: "Project 12", path: "/Project 12" },
      { id: "2", name: "project 2", path: "/project 2" },
      { id: "a", name: "Alpha", path: "/Alpha" },
    ];

    expect(sortDirectories(directories).map((item) => item.id)).toEqual([
      "a",
      "2",
      "12",
    ]);
    expect(directories.map((item) => item.id)).toEqual(["12", "2", "a"]);
  });

  it("keeps path parsing on the backend and only trims manual input", () => {
    expect(normalizedManualPath("  C:\\Projects\\app  ")).toBe(
      "C:\\Projects\\app",
    );
    expect(normalizedManualPath("  /srv/work/app  ")).toBe("/srv/work/app");
    expect(normalizedManualPath("  \\\\server\\share\\app  ")).toBe(
      "\\\\server\\share\\app",
    );
  });

  it("loads a one-level listing and synchronizes its manual path", () => {
    const initial = createWorkspaceBrowserState("browse");
    const loaded = workspaceBrowserReducer(initial, {
      type: "directory_loaded",
      listing: {
        current: ROOT,
        parentId: null,
        breadcrumbs: [ROOT],
        directories: [PROJECT],
        truncated: false,
      },
    });

    expect(loaded.directoryLoading).toBe(false);
    expect(loaded.pathInput).toBe("C:\\");
    expect(loaded.listing?.directories).toEqual([PROJECT]);
  });

  it("defers directory loading until Browse is the active source", () => {
    expect(createWorkspaceBrowserState("favorites").directoryLoading).toBe(
      false,
    );
    expect(createWorkspaceBrowserState("recent").directoryLoading).toBe(false);
    expect(createWorkspaceBrowserState("browse").directoryLoading).toBe(true);
  });

  it("adds and removes favorites without duplicating a directory", () => {
    const favorite: WorkspaceFavorite = {
      id: "favorite-1",
      directory: PROJECT,
      label: "Main project",
    };
    const renamed: WorkspaceFavorite = {
      ...favorite,
      id: "favorite-2",
      label: "Codex Web",
    };

    let state = createWorkspaceBrowserState("favorites", {
      favorites: [],
      recent: [],
    });
    state = workspaceBrowserReducer(state, {
      type: "favorite_mutating",
      id: PROJECT.id,
    });
    state = workspaceBrowserReducer(state, {
      type: "favorite_added",
      favorite,
    });
    state = workspaceBrowserReducer(state, {
      type: "favorite_mutating",
      id: PROJECT.id,
    });
    state = workspaceBrowserReducer(state, {
      type: "favorite_added",
      favorite: renamed,
    });

    expect(state.library.favorites).toEqual([renamed]);
    expect(findFavorite(state.library.favorites, PROJECT.id)).toEqual(renamed);
    expect(displayFavoriteName(renamed)).toBe("Codex Web");

    state = workspaceBrowserReducer(state, {
      type: "favorite_mutating",
      id: PROJECT.id,
    });
    state = workspaceBrowserReducer(state, {
      type: "favorite_removed",
      favoriteId: renamed.id,
    });
    expect(state.library.favorites).toEqual([]);
  });

  it("falls back to a path for directories without a display name", () => {
    expect(displayDirectoryName({ ...ROOT, name: " " })).toBe("C:\\");
  });

  it("keeps failures isolated between concurrent async domains", () => {
    let state = createWorkspaceBrowserState();
    state = workspaceBrowserReducer(state, {
      type: "library_failed",
      message: "Library unavailable",
    });
    state = workspaceBrowserReducer(state, {
      type: "favorite_failed",
      message: "Favorite update failed",
    });
    state = workspaceBrowserReducer(state, {
      type: "directory_loaded",
      listing: {
        current: ROOT,
        parentId: null,
        breadcrumbs: [ROOT],
        directories: [PROJECT],
        truncated: false,
      },
    });

    expect(state.errors).toEqual({
      library: "Library unavailable",
      directory: null,
      favorite: "Favorite update failed",
    });

    state = workspaceBrowserReducer(state, {
      type: "error_cleared",
      domain: "library",
    });
    expect(state.errors.library).toBeNull();
    expect(state.errors.favorite).toBe("Favorite update failed");
  });

  it("requires a successful library load before accepting favorite mutations", () => {
    const favorite: WorkspaceFavorite = {
      id: "favorite-1",
      directory: PROJECT,
    };
    let state = createWorkspaceBrowserState();
    state = workspaceBrowserReducer(state, {
      type: "library_failed",
      message: "Library unavailable",
    });

    expect(state.libraryLoadedSuccessfully).toBe(false);
    const failedState = state;
    state = workspaceBrowserReducer(state, {
      type: "favorite_mutating",
      id: PROJECT.id,
    });
    state = workspaceBrowserReducer(state, {
      type: "favorite_added",
      favorite,
    });
    expect(state).toBe(failedState);
    expect(state.library.favorites).toEqual([]);

    state = workspaceBrowserReducer(state, { type: "library_loading" });
    expect(state.libraryLoadedSuccessfully).toBe(false);
    state = workspaceBrowserReducer(state, {
      type: "library_loaded",
      library: { favorites: [], recent: [] },
    });
    state = workspaceBrowserReducer(state, {
      type: "favorite_mutating",
      id: PROJECT.id,
    });
    state = workspaceBrowserReducer(state, {
      type: "favorite_added",
      favorite,
    });

    expect(state.libraryLoadedSuccessfully).toBe(true);
    expect(state.library.favorites).toEqual([favorite]);
  });

  it("maps horizontal and boundary tab keys with wraparound", () => {
    expect(workspaceTabFromKey("favorites", "ArrowRight")).toBe("recent");
    expect(workspaceTabFromKey("favorites", "ArrowLeft")).toBe("browse");
    expect(workspaceTabFromKey("browse", "ArrowRight")).toBe("favorites");
    expect(workspaceTabFromKey("recent", "Home")).toBe("favorites");
    expect(workspaceTabFromKey("recent", "End")).toBe("browse");
    expect(workspaceTabFromKey("recent", "Enter")).toBeNull();
  });
});
