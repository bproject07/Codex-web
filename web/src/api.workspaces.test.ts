import { afterEach, describe, expect, it, vi } from "vitest";
import {
  ApiError,
  addWorkspaceFavorite,
  getFilesystemRoots,
  getWorkspaceLibrary,
  listWorkspaceDirectory,
  removeWorkspaceFavorite,
  resolveWorkspacePath,
} from "./api";

const DIRECTORY = {
  id: "w1.QwA6AFwAUAHIAbwBvAHQA",
  name: "root",
  path: "C:\\root",
};

const CHILD = {
  id: "w1.QwA6AFwAUAHIAbwBvAHQAXABjAGgAaQBsAGQA",
  name: "child",
  path: "C:\\root\\child",
};

function jsonResponse(body: unknown, status = 200): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "Content-Type": "application/json" },
  });
}

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("workspace API", () => {
  it("loads roots and bounded directory listings", async () => {
    const fetchMock = vi
      .fn()
      .mockResolvedValueOnce(
        jsonResponse({ defaultDirectory: DIRECTORY, roots: [DIRECTORY] }),
      )
      .mockResolvedValueOnce(
        jsonResponse({
          current: DIRECTORY,
          parentId: null,
          breadcrumbs: [DIRECTORY],
          directories: [CHILD],
          truncated: true,
        }),
      );
    vi.stubGlobal("fetch", fetchMock);

    await expect(getFilesystemRoots("token")).resolves.toEqual({
      defaultDirectory: DIRECTORY,
      roots: [DIRECTORY],
    });
    await expect(
      listWorkspaceDirectory("token", DIRECTORY.id),
    ).resolves.toEqual({
      current: DIRECTORY,
      parentId: null,
      breadcrumbs: [DIRECTORY],
      directories: [CHILD],
      truncated: true,
    });
    expect(fetchMock).toHaveBeenNthCalledWith(
      2,
      "/api/filesystem/list",
      expect.objectContaining({
        method: "POST",
        body: JSON.stringify({ directoryId: DIRECTORY.id }),
      }),
    );
  });

  it("resolves a manually entered native path without exposing it in the URL", async () => {
    const fetchMock = vi.fn().mockResolvedValue(
      jsonResponse({
        current: CHILD,
        parentId: DIRECTORY.id,
        breadcrumbs: [DIRECTORY, CHILD],
        directories: [],
        truncated: false,
      }),
    );
    vi.stubGlobal("fetch", fetchMock);

    await resolveWorkspacePath("token", CHILD.path);

    expect(fetchMock).toHaveBeenCalledWith(
      "/api/filesystem/resolve",
      expect.objectContaining({
        method: "POST",
        body: JSON.stringify({ path: CHILD.path }),
      }),
    );
  });

  it("normalizes the flat persisted library into the picker model", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn().mockResolvedValue(
        jsonResponse({
          version: 1,
          favorites: [
            {
              id: "11111111-1111-4111-8111-111111111111",
              directoryId: CHILD.id,
              name: CHILD.name,
              path: CHILD.path,
              label: "Demo",
              preferredAgent: "claude",
            },
          ],
          recent: [
            {
              directoryId: DIRECTORY.id,
              name: DIRECTORY.name,
              path: DIRECTORY.path,
              lastAgent: "codex",
              lastOpenedAt: 1_721_234_567_890,
            },
          ],
        }),
      ),
    );

    await expect(getWorkspaceLibrary("token")).resolves.toEqual({
      favorites: [
        {
          id: "11111111-1111-4111-8111-111111111111",
          directory: CHILD,
          label: "Demo",
          preferredAgent: "claude",
        },
      ],
      recent: [
        {
          directory: DIRECTORY,
          lastAgent: "codex",
          lastOpenedAt: 1_721_234_567_890,
        },
      ],
    });
  });

  it("adds and removes favorites through authenticated opaque identifiers", async () => {
    const favorite = {
      id: "11111111-1111-4111-8111-111111111111",
      directoryId: CHILD.id,
      name: CHILD.name,
      path: CHILD.path,
      label: null,
      preferredAgent: null,
    };
    const fetchMock = vi
      .fn()
      .mockResolvedValueOnce(jsonResponse(favorite))
      .mockResolvedValueOnce(new Response(null, { status: 204 }));
    vi.stubGlobal("fetch", fetchMock);

    await expect(addWorkspaceFavorite("token", CHILD)).resolves.toEqual({
      id: favorite.id,
      directory: CHILD,
      label: null,
      preferredAgent: null,
    });
    await expect(
      removeWorkspaceFavorite("token", favorite.id),
    ).resolves.toBeUndefined();
    expect(fetchMock).toHaveBeenNthCalledWith(
      1,
      "/api/workspaces/favorites",
      expect.objectContaining({
        method: "PUT",
        body: JSON.stringify({ directoryId: CHILD.id }),
      }),
    );
    expect(fetchMock).toHaveBeenNthCalledWith(
      2,
      `/api/workspaces/favorites/${favorite.id}`,
      expect.objectContaining({ method: "DELETE" }),
    );
  });

  it("rejects malformed workspace payloads at the API boundary", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn().mockResolvedValue(
        jsonResponse({
          version: 1,
          favorites: [],
          recent: [
            {
              directoryId: DIRECTORY.id,
              name: DIRECTORY.name,
              path: DIRECTORY.path,
              lastAgent: "shell",
              lastOpenedAt: 1,
            },
          ],
        }),
      ),
    );

    const request = getWorkspaceLibrary("token");
    await expect(request).rejects.toBeInstanceOf(ApiError);
    await expect(request).rejects.toMatchObject({
      status: 502,
      message: "The server returned an invalid workspace response.",
    });
  });
});
