import { afterEach, describe, expect, it, vi } from "vitest";
import {
  ApiError,
  getAgentCatalog,
  normalizeAgentCatalog,
  type AgentCatalog,
} from "./api";

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
        command: "irm https://chatgpt.com/codex/install.ps1 | iex",
        shell: "PowerShell",
        verifyCommand: "codex --version",
        updateCommand: "",
        docsUrl: "https://github.com/openai/codex",
        requiresServerAccess: true,
      },
    },
    {
      kind: "claude",
      state: "missing",
      configuration: "auto",
      version: null,
      dangerouslySkipPermissions: true,
      install: {
        command: "irm https://claude.ai/install.ps1 | iex",
        shell: "PowerShell",
        verifyCommand: "claude --version",
        updateCommand: "claude update",
        docsUrl: "https://code.claude.com/docs/en/installation",
        requiresServerAccess: true,
      },
    },
  ],
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

describe("agent catalog API", () => {
  it("loads server-side discovery, versions, and install guidance", async () => {
    const fetchMock = vi.fn().mockResolvedValue(jsonResponse(CATALOG));
    vi.stubGlobal("fetch", fetchMock);

    await expect(getAgentCatalog("0123456789abcdef")).resolves.toEqual({
      ...CATALOG,
      agents: CATALOG.agents.map((agent) => ({
        ...agent,
        install: {
          ...agent.install,
          docsUrl: `${agent.install.docsUrl}`,
        },
      })),
    });
    expect(fetchMock).toHaveBeenCalledWith(
      "/api/agent-catalog",
      expect.objectContaining({ cache: "no-store" }),
    );
  });

  it("requests an uncached server probe when refresh is selected", async () => {
    const fetchMock = vi.fn().mockResolvedValue(jsonResponse(CATALOG));
    vi.stubGlobal("fetch", fetchMock);

    await getAgentCatalog("0123456789abcdef", { refresh: true });

    expect(fetchMock).toHaveBeenCalledWith(
      "/api/agent-catalog?refresh=true",
      expect.any(Object),
    );
  });

  it("falls back to the legacy agent endpoint", async () => {
    const fetchMock = vi
      .fn()
      .mockResolvedValueOnce(jsonResponse({ error: "route not found" }, 404))
      .mockResolvedValueOnce(jsonResponse(["codex", "agy"]));
    vi.stubGlobal("fetch", fetchMock);

    await expect(getAgentCatalog("0123456789abcdef")).resolves.toMatchObject({
      schemaVersion: 1,
      server: { os: "unknown" },
      agents: [
        { kind: "codex", state: "ready", version: null },
        { kind: "agy", state: "ready", version: null },
      ],
    });
    expect(fetchMock).toHaveBeenCalledTimes(2);
  });

  it("falls back when an older server returns its HTML app shell", async () => {
    const fetchMock = vi
      .fn()
      .mockResolvedValueOnce(
        new Response("<!doctype html><title>Codex Web Terminal</title>", {
          status: 200,
          headers: { "Content-Type": "text/html; charset=utf-8" },
        }),
      )
      .mockResolvedValueOnce(jsonResponse(["codex"]));
    vi.stubGlobal("fetch", fetchMock);

    await expect(getAgentCatalog("0123456789abcdef")).resolves.toMatchObject({
      agents: [{ kind: "codex", state: "ready" }],
    });
    expect(fetchMock).toHaveBeenCalledTimes(2);
  });

  it("does not treat malformed JSON as an unavailable endpoint", async () => {
    const fetchMock = vi.fn().mockResolvedValue(
      new Response('{"schemaVersion":1,"agents":[', {
        status: 200,
        headers: { "Content-Type": "application/json" },
      }),
    );
    vi.stubGlobal("fetch", fetchMock);

    await expect(
      getAgentCatalog("0123456789abcdef"),
    ).rejects.toThrow("invalid API response");
    expect(fetchMock).toHaveBeenCalledTimes(1);
  });

  it("rejects malformed catalog data instead of treating an agent as ready", () => {
    expect(() =>
      normalizeAgentCatalog({
        ...CATALOG,
        agents: [{ ...CATALOG.agents[0], state: "surprise" }],
      }),
    ).toThrowError(ApiError);
  });

  it("removes non-HTTPS documentation links", () => {
    const catalog = normalizeAgentCatalog({
      ...CATALOG,
      agents: [
        {
          ...CATALOG.agents[0],
          install: {
            ...CATALOG.agents[0].install,
            docsUrl: "javascript:alert(document.domain)",
          },
        },
      ],
    });

    expect(catalog.agents[0]?.install.docsUrl).toBe("");
  });

  it("does not hide catalog server errors behind the legacy fallback", async () => {
    const fetchMock = vi
      .fn()
      .mockResolvedValue(jsonResponse({ error: "probe failed" }, 500));
    vi.stubGlobal("fetch", fetchMock);

    await expect(
      getAgentCatalog("0123456789abcdef"),
    ).rejects.toMatchObject({
      status: 500,
      message: "probe failed",
    });
    expect(fetchMock).toHaveBeenCalledTimes(1);
  });
});
