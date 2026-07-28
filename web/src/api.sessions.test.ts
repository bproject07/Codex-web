import { afterEach, describe, expect, it, vi } from "vitest";
import {
  ApiError,
  createSession,
  getSession,
  listAgents,
  listSessions,
  normalizeSessionSnapshot,
  type SessionSnapshot,
} from "./api";

const PRIMARY_SESSION: SessionSnapshot = {
  terminalId: "11111111-1111-4111-8111-111111111111",
  name: "Terminal 1",
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
  project: "C:\\Projects\\my-app",
  directoryId: "w1.QwA6AFwAUAByAG8AagBlAGMAdABzAFwAbQB5AC0AYQBwAHAA",
  lastError: null,
  purpose: { kind: "interactive" },
};

const SECONDARY_SESSION: SessionSnapshot = {
  ...PRIMARY_SESSION,
  terminalId: "22222222-2222-4222-8222-222222222222",
  name: "Terminal 2",
  isPrimary: false,
  createdAt: 2_000,
  sessionId: "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb",
  pid: 456,
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

describe("terminal session API selection", () => {
  it("normalizes dedicated peer-session purpose metadata", () => {
    const parentTerminalId = PRIMARY_SESSION.terminalId;
    const peer = normalizeSessionSnapshot(
      {
        ...SECONDARY_SESSION,
        purpose: {
          kind: "peer",
          threadId: "33333333-3333-4333-8333-333333333333",
          parentTerminalId,
        },
      },
      SECONDARY_SESSION.terminalId,
    );

    expect(peer.purpose).toEqual({
      kind: "peer",
      threadId: "33333333-3333-4333-8333-333333333333",
      parentTerminalId,
    });
  });

  it("returns the exact requested terminal from the session list", async () => {
    const fetchMock = vi
      .fn()
      .mockResolvedValue(jsonResponse([PRIMARY_SESSION, SECONDARY_SESSION]));
    vi.stubGlobal("fetch", fetchMock);

    await expect(
      getSession("0123456789abcdef", SECONDARY_SESSION.terminalId),
    ).resolves.toEqual(SECONDARY_SESSION);
    expect(fetchMock).toHaveBeenCalledTimes(1);
  });

  it("reports a stable 404 when the selected terminal disappeared", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn().mockResolvedValue(jsonResponse([PRIMARY_SESSION])),
    );

    const result = getSession(
      "0123456789abcdef",
      SECONDARY_SESSION.terminalId,
    );

    await expect(result).rejects.toBeInstanceOf(ApiError);
    await expect(result).rejects.toMatchObject({
      status: 404,
      message: "The selected terminal session no longer exists.",
    });
  });

  it("normalizes the legacy primary endpoint when the list route is absent", async () => {
    const {
      terminalId: _terminalId,
      name: _name,
      isPrimary: _isPrimary,
      createdAt: _createdAt,
      directoryId: _directoryId,
      purpose: _purpose,
      ...legacySession
    } = PRIMARY_SESSION;
    const fetchMock = vi
      .fn()
      .mockResolvedValueOnce(
        jsonResponse({ error: "route not found" }, 404),
      )
      .mockResolvedValueOnce(jsonResponse(legacySession));
    vi.stubGlobal("fetch", fetchMock);

    await expect(listSessions("0123456789abcdef")).resolves.toMatchObject([
      {
        terminalId: "primary",
        name: "Primary",
        isPrimary: true,
        directoryId: "",
      },
    ]);
    expect(fetchMock).toHaveBeenCalledTimes(2);
  });

  it("creates a session with an allowlisted agent id", async () => {
    const fetchMock = vi
      .fn()
      .mockResolvedValue(
        jsonResponse({ ...SECONDARY_SESSION, agent: "claude", name: "Claude 2" }),
      );
    vi.stubGlobal("fetch", fetchMock);

    await expect(
      createSession("0123456789abcdef", "claude"),
    ).resolves.toMatchObject({
      agent: "claude",
      name: "Claude 2",
    });
    expect(fetchMock).toHaveBeenCalledWith(
      "/api/sessions",
      expect.objectContaining({
        method: "POST",
        body: JSON.stringify({ agent: "claude" }),
      }),
    );
  });

  it("creates a session in the selected opaque directory", async () => {
    const fetchMock = vi
      .fn()
      .mockResolvedValue(
        jsonResponse({ ...SECONDARY_SESSION, agent: "agy", name: "Agy 2" }),
      );
    vi.stubGlobal("fetch", fetchMock);

    await expect(
      createSession(
        "0123456789abcdef",
        "agy",
        "w1.QwA6AFwAUAByAG8AagBlAGMAdABzAFwAZABlAG0AbwA",
      ),
    ).resolves.toMatchObject({
      agent: "agy",
      name: "Agy 2",
    });
    expect(fetchMock).toHaveBeenCalledWith(
      "/api/sessions",
      expect.objectContaining({
        method: "POST",
        body: JSON.stringify({
          agent: "agy",
          directoryId:
            "w1.QwA6AFwAUAByAG8AagBlAGMAdABzAFwAZABlAG0AbwA",
        }),
      }),
    );
  });

  it("preserves a current-server directory error instead of reporting an old server", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn().mockResolvedValue(
        jsonResponse(
          { error: "The selected directory was not found." },
          404,
        ),
      ),
    );

    const request = createSession(
      "0123456789abcdef",
      "codex",
      "w1.bWlzc2luZw",
    );

    await expect(request).rejects.toMatchObject({
      status: 404,
      message: "The selected directory was not found.",
    });
  });

  it("reports an outdated server when the session route itself is absent", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn().mockResolvedValue(
        new Response("Not Found", {
          status: 404,
          headers: { "Content-Type": "text/plain" },
        }),
      ),
    );

    await expect(
      createSession("0123456789abcdef", "codex"),
    ).rejects.toMatchObject({
      status: 404,
      message:
        "Creating another session is unavailable. The browser UI and server may be from different releases, or an older server may still be using this port. Restart with the executable and web folder from the same release, then reload.",
    });
  });

  it("falls back to Codex when the agent endpoint is unavailable", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn().mockResolvedValue(jsonResponse({ error: "route not found" }, 404)),
    );

    await expect(listAgents("0123456789abcdef")).resolves.toEqual(["codex"]);
  });
});
