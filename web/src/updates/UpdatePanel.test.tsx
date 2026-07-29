import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it, vi } from "vitest";
import { UpdatePanel } from "./UpdatePanel";
import type { UpdateStatus } from "./api";

const AVAILABLE: UpdateStatus = {
  schemaVersion: 1,
  currentVersion: "0.2.0",
  latestVersion: "0.3.0",
  state: "available",
  installSupported: true,
  installReason: null,
  releaseUrl:
    "https://github.com/bproject07/Codex-web/releases/tag/v0.3.0",
  progressPercent: null,
  error: null,
  checkedAt: "2026-07-28T12:00:00Z",
};

const callbacks = {
  onCheck: vi.fn(),
  onApply: vi.fn(),
};

describe("UpdatePanel", () => {
  it("requires explicit confirmation and explains session loss", () => {
    const html = renderToStaticMarkup(
      <UpdatePanel
        status={AVAILABLE}
        loading={false}
        operation={null}
        error={null}
        {...callbacks}
      />,
    );

    expect(html).toContain("Update available");
    expect(html).toContain("Latest release 0.3.0");
    expect(html).toContain("ends every running terminal and");
    expect(html).toContain("Favorites and Recent folders remain saved");
    expect(html).toContain("disabled");
  });

  it("explains why source builds cannot self-install", () => {
    const html = renderToStaticMarkup(
      <UpdatePanel
        status={{
          ...AVAILABLE,
          installSupported: false,
          installReason:
            "Source/development build — install an official release package once.",
        }}
        loading={false}
        operation={null}
        error={null}
        {...callbacks}
      />,
    );

    expect(html).toContain("Source/development build");
    expect(html).not.toContain("Update to 0.3.0 and restart");
  });

  it("shows verified download progress", () => {
    const html = renderToStaticMarkup(
      <UpdatePanel
        status={{
          ...AVAILABLE,
          state: "verifying",
          progressPercent: 100,
        }}
        loading={false}
        operation={null}
        error={null}
        {...callbacks}
      />,
    );

    expect(html).toContain("Verifying package");
    expect(html).toContain('value="100"');
  });

  it("treats a staged package as an automatic handoff, not a second apply", () => {
    const html = renderToStaticMarkup(
      <UpdatePanel
        status={{
          ...AVAILABLE,
          state: "staged",
          progressPercent: 100,
        }}
        loading={false}
        operation={null}
        error={null}
        {...callbacks}
      />,
    );

    expect(html).toContain("Update ready");
    expect(html).not.toContain("Update to 0.3.0 and restart");
    expect(html).toContain("disabled");
  });

  it("reports an unavailable status instead of loading forever", () => {
    const html = renderToStaticMarkup(
      <UpdatePanel
        status={null}
        loading={false}
        operation={null}
        error="Request failed with HTTP 404"
        {...callbacks}
      />,
    );

    expect(html).toContain("Update status unavailable");
    expect(html).toContain("Request failed with HTTP 404");
    expect(html).not.toContain("Loading installed version");
  });
});
