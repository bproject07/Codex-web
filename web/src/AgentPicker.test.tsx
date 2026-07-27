import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it, vi } from "vitest";
import { AgentPicker } from "./AgentPicker";
import type { AgentCatalog } from "./api";

const INSTALL = {
  shell: "PowerShell",
  verifyCommand: "agent --version",
  updateCommand: "agent update",
  docsUrl: "https://example.com/install",
  requiresServerAccess: true as const,
};

const CATALOG: AgentCatalog = {
  schemaVersion: 1,
  server: {
    os: "windows",
    arch: "x86_64",
    shell: "PowerShell",
  },
  agents: [
    {
      kind: "codex",
      state: "ready",
      configuration: "auto",
      version: "0.145.0",
      dangerouslySkipPermissions: false,
      install: {
        ...INSTALL,
        command: "install codex",
        verifyCommand: "codex --version",
      },
    },
    {
      kind: "claude",
      state: "missing",
      configuration: "auto",
      version: null,
      dangerouslySkipPermissions: true,
      install: {
        ...INSTALL,
        command: "install claude",
        verifyCommand: "claude --version",
      },
    },
    {
      kind: "agy",
      state: "misconfigured",
      configuration: "override",
      version: null,
      dangerouslySkipPermissions: false,
      install: {
        ...INSTALL,
        command: "install agy",
        verifyCommand: "agy --version",
      },
    },
  ],
};

describe("AgentPicker", () => {
  it("renders discovery state, versions, safety guidance, and explicit actions", () => {
    const html = renderToStaticMarkup(
      <AgentPicker
        catalog={CATALOG}
        loading={false}
        error={null}
        creatingAgent={null}
        onSelect={vi.fn()}
        onRefresh={vi.fn()}
        onClose={vi.fn()}
      />,
    );

    expect(html).toContain("the Windows (x86_64) server host");
    expect(html).toContain("not on this browser or phone");
    expect(html).toContain("Installed version 0.145.0");
    expect(html).toContain("Not found");
    expect(html).toContain("Configuration error");
    expect(html).toContain("authoritative command override");
    expect(html).toContain("Approvals disabled.");
    expect(html).toContain("Start Codex");
    expect(html).not.toContain("Start Claude");
    expect(html).not.toContain("Start AGY");
    expect(html).toContain("install claude");
    expect(html).toContain("Official docs");
    expect(html).toContain("Check again");
    expect(html).toContain('rel="noopener noreferrer"');
    expect(html).not.toContain("<input");
    expect(html).not.toContain("<textarea");
  });

  it("exposes busy and inline failure state to assistive technology", () => {
    const html = renderToStaticMarkup(
      <AgentPicker
        catalog={CATALOG}
        loading={false}
        error="Claude is no longer available."
        creatingAgent="codex"
        onSelect={vi.fn()}
        onRefresh={vi.fn()}
        onClose={vi.fn()}
      />,
    );

    expect(html).toContain('aria-busy="true"');
    expect(html).toContain('role="alert"');
    expect(html).toContain("Claude is no longer available.");
    expect(html).toContain("Starting Codex");
    expect(html).toContain('aria-describedby="agent-picker-description"');
  });

  it("disables stale start actions while discovery is refreshing", () => {
    const html = renderToStaticMarkup(
      <AgentPicker
        catalog={CATALOG}
        loading
        error={null}
        creatingAgent={null}
        onSelect={vi.fn()}
        onRefresh={vi.fn()}
        onClose={vi.fn()}
      />,
    );

    expect(html).toMatch(/data-agent-start="codex"[^>]*disabled=""/);
    expect(html).toContain('aria-busy="true"');
  });
});
