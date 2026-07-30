import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { AppIdentity, type AppIdentityProps } from "./App";
import type { SessionSnapshot } from "./api";

const SESSION: SessionSnapshot = {
  terminalId: "11111111-1111-4111-8111-111111111111",
  name: "Codex 1",
  agent: "codex",
  isPrimary: true,
  createdAt: 1_000,
  sessionId: "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
  status: "running",
  connected: true,
  connectedClients: 1,
  startedAt: 1_100,
  pid: 123,
  exitCode: null,
  project: "C:\\Projects\\demo-app",
  directoryId: "directory",
  lastError: null,
  purpose: { kind: "interactive" },
};

function renderIdentity(overrides: Partial<AppIdentityProps> = {}): string {
  return renderToStaticMarkup(
    <AppIdentity
      session={SESSION}
      sessionsLoading={false}
      effectiveStatus="connected"
      {...overrides}
    />,
  );
}

describe("AppIdentity", () => {
  it("shows the full project path, agent, and dot — no branding or actions", () => {
    const html = renderIdentity();

    expect(html).toContain(">C:\\Projects\\demo-app</span>");
    expect(html).toContain('title="C:\\Projects\\demo-app"');
    expect(html).toContain("active-agent--codex");
    expect(html).toContain("Codex");
    expect(html).toContain("status--connected");
    expect(html).toContain("status-dot");
    expect(html).toContain("Connection: Connected.");
    expect(html).not.toContain("Codex Web Terminal");
    expect(html).not.toContain("Unofficial");
    expect(html).not.toContain(">Online<");
    expect(html).not.toContain("status-label");
    // Pure information: reconnection is automatic, so the identity area
    // renders no interactive controls at all.
    expect(html).not.toContain("<button");
    expect(html).not.toContain("Reconnect");
  });

  it("orders path, then agent, then status dot", () => {
    const html = renderIdentity();
    const path = html.indexOf("app-context-project");
    const agent = html.indexOf("active-agent");
    const status = html.indexOf("app-status");

    expect(path).toBeGreaterThan(-1);
    expect(agent).toBeGreaterThan(path);
    expect(status).toBeGreaterThan(agent);
  });

  it("follows the active session when it changes", () => {
    const claudeReviewer: SessionSnapshot = {
      ...SESSION,
      agent: "claude",
      project: "/home/user/other-project",
    };
    const html = renderIdentity({ session: claudeReviewer });

    expect(html).toContain(">/home/user/other-project</span>");
    expect(html).toContain("active-agent--claude");
    expect(html).toContain("Claude");
    expect(html).not.toContain("demo-app");
  });

  it("keeps status accessible in every non-connected state", () => {
    const exited = renderIdentity({ effectiveStatus: "codex_exited" });
    expect(exited).toContain("status--codex_exited");
    expect(exited).toContain("Connection: Agent exited.");

    const reconnecting = renderIdentity({ effectiveStatus: "reconnecting" });
    expect(reconnecting).toContain("status--reconnecting");
    expect(reconnecting).toContain("Connection: Reconnecting.");

    const offline = renderIdentity({ effectiveStatus: "disconnected" });
    expect(offline).toContain("status--disconnected");
    expect(offline).toContain("Connection: Disconnected.");
  });

  it("shows root paths in their native form", () => {
    const windowsRoot = renderIdentity({
      session: { ...SESSION, project: "C:\\" },
    });
    expect(windowsRoot).toContain(">C:\\</span>");
    expect(windowsRoot).toContain('title="C:\\"');

    const unixRoot = renderIdentity({
      session: { ...SESSION, project: "/" },
    });
    expect(unixRoot).toContain(">/</span>");
  });

  it("distinguishes loading from having no session", () => {
    const loading = renderIdentity({ session: null, sessionsLoading: true });
    expect(loading).toContain("Loading…");
    expect(loading).not.toContain("active-agent");

    const empty = renderIdentity({ session: null, sessionsLoading: false });
    expect(empty).toContain("No session");
    expect(empty).toContain('<h1 class="app-context">');
  });
});
