import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it, vi } from "vitest";
import { WorkspacePicker } from "./WorkspacePicker";
import type {
  WorkspaceBrowserAdapter,
  WorkspaceDirectory,
  WorkspaceDirectoryListing,
  WorkspaceLibrary,
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

const LISTING: WorkspaceDirectoryListing = {
  current: ROOT,
  parentId: null,
  breadcrumbs: [ROOT],
  directories: [PROJECT],
  truncated: false,
};

const LIBRARY: WorkspaceLibrary = {
  favorites: [
    {
      id: "favorite-project",
      directory: PROJECT,
      label: "Main project",
      preferredAgent: "codex",
    },
  ],
  recent: [
    {
      directory: PROJECT,
      lastOpenedAt: 1_753_607_200_000,
      lastAgent: "claude",
    },
  ],
};

const ADAPTER: WorkspaceBrowserAdapter = {
  loadLibrary: vi.fn(async () => LIBRARY),
  listRoots: vi.fn(async () => LISTING),
  listDirectory: vi.fn(async () => LISTING),
  resolvePath: vi.fn(async () => LISTING),
  addFavorite: vi.fn(async (directory) => ({
    id: `favorite-${directory.id}`,
    directory,
  })),
  removeFavorite: vi.fn(async () => undefined),
};

describe("WorkspacePicker", () => {
  it("renders favorite shortcuts as direct choices without exposing files", () => {
    const html = renderToStaticMarkup(
      <WorkspacePicker
        adapter={ADAPTER}
        initialLibrary={LIBRARY}
        initialListing={LISTING}
        onChoose={vi.fn()}
        onStart={vi.fn()}
      />,
    );

    expect(html).toContain("Choose a project folder");
    expect(html).toContain('role="dialog"');
    expect(html).toContain('aria-modal="true"');
    expect(html).toContain('tabindex="-1"');
    expect(html).toContain('data-workspace-initial-focus="true"');
    expect(html).toContain("Main project");
    expect(html).toContain("C:\\Projects\\Codex-web");
    expect(html).toContain("Use folder");
    expect(html).toContain("Start Codex");
    expect(html).toContain("Browse");
    expect(html).toContain('aria-pressed="true"');
    expect(html).not.toContain('type="file"');
  });

  it("renders a one-level accessible folder browser with manual path input", () => {
    const html = renderToStaticMarkup(
      <WorkspacePicker
        adapter={ADAPTER}
        initialTab="browse"
        initialLibrary={LIBRARY}
        initialListing={LISTING}
        onChoose={vi.fn()}
      />,
    );

    expect(html).toContain('role="tablist"');
    expect(html).toContain('aria-selected="true"');
    expect(html).toContain("aria-controls=");
    expect(html).toContain("aria-labelledby=");
    expect(html).toContain('data-workspace-tab="favorites"');
    expect(html).toContain('data-workspace-tab="recent"');
    expect(html).toContain('data-workspace-tab="browse"');
    expect(html).toContain("Folder path");
    expect(html).toContain("Enter a full server path");
    expect(html).toContain('aria-label="Folder breadcrumbs"');
    expect(html).toContain("↑ Up");
    expect(html).toContain('aria-label="Folders"');
    expect(html).toContain('data-workspace-path-input="true"');
    expect(html).toContain('data-workspace-current-focus-target="true"');
    expect(html).toContain("Open folder C:\\Projects\\Codex-web");
    expect(html).toContain("Use folder");
  });

  it("disables all selection actions while the parent is submitting", () => {
    const html = renderToStaticMarkup(
      <WorkspacePicker
        adapter={ADAPTER}
        initialLibrary={LIBRARY}
        initialListing={LISTING}
        onChoose={vi.fn()}
        onStart={vi.fn()}
        disabled
      />,
    );

    expect(html).toContain('aria-busy="true"');
    expect(html).toMatch(
      /<button[^>]*disabled=""[^>]*aria-label="Use folder:[^"]+"/,
    );
    expect(html).toMatch(
      /<button[^>]*disabled=""[^>]*aria-label="Browse inside[^"]+"/,
    );
  });

  it("keeps an unavailable favorite removable while blocking start and use", () => {
    const unavailableLibrary: WorkspaceLibrary = {
      ...LIBRARY,
      favorites: [
        {
          ...LIBRARY.favorites[0],
          available: false,
          unavailableReason: "The folder no longer exists.",
        },
      ],
    };
    const html = renderToStaticMarkup(
      <WorkspacePicker
        adapter={ADAPTER}
        initialLibrary={unavailableLibrary}
        initialListing={LISTING}
        onChoose={vi.fn()}
        onStart={vi.fn()}
      />,
    );

    expect(html).toContain("The folder no longer exists.");
    expect(html).toMatch(
      /<button[^>]*disabled=""[^>]*aria-label="Use folder:[^"]+"/,
    );
    expect(html).toMatch(/>Start Codex<\/button>/);
    expect(html).toMatch(
      /<button type="button" aria-pressed="true" aria-label="Remove [^"]+ from favorites"/,
    );
  });

  it("explains a bounded directory listing without treating it as an error", () => {
    const html = renderToStaticMarkup(
      <WorkspacePicker
        adapter={ADAPTER}
        initialTab="browse"
        initialLibrary={LIBRARY}
        initialListing={{ ...LISTING, truncated: true }}
        onChoose={vi.fn()}
      />,
    );

    expect(html).toContain(
      "This folder has more subfolders than can be shown at once.",
    );
    expect(html).toContain("Enter a full path above");
    expect(html).not.toContain("workspace-picker__error");
  });

  it("blocks favorite changes until the initial library request completes", () => {
    const html = renderToStaticMarkup(
      <WorkspacePicker
        adapter={ADAPTER}
        initialTab="browse"
        initialListing={LISTING}
        onChoose={vi.fn()}
      />,
    );

    expect(html).toContain('aria-busy="true"');
    expect(html).toMatch(
      /<button type="button" disabled="" aria-pressed="false">☆ Favorite<\/button>/,
    );
  });

  it("keeps the manual path immutable while directory navigation is pending", () => {
    const html = renderToStaticMarkup(
      <WorkspacePicker
        adapter={ADAPTER}
        initialTab="browse"
        initialLibrary={LIBRARY}
        onChoose={vi.fn()}
      />,
    );

    expect(html).toMatch(
      /<input[^>]*placeholder="Enter a full server path"[^>]*disabled=""/,
    );
  });
});
