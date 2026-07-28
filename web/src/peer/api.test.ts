import { afterEach, describe, expect, it, vi } from "vitest";
import {
  createPeerThread,
  createPeerTurn,
  deletePeerThread,
  dispatchPeerTurn,
  listPeerThreads,
  normalizePeerThread,
  returnPeerTurn,
} from "./api";
import type { PeerThread } from "./types";

const THREAD: PeerThread = {
  id: "12345678-1234-4234-8234-123456789abc",
  sourceTerminalId: "11111111-1111-4111-8111-111111111111",
  reviewerTerminalId: null,
  targetAgent: "claude",
  status: "awaiting_preview",
  currentTurn: {
    id: "33333333-3333-4333-8333-333333333333",
    sequence: 1,
    action: "review",
    instruction: "Review the architecture.",
    status: "awaiting_preview",
    handoff: "Prepared context.",
    handoffRevision: 2,
    response: null,
    error: null,
  },
  createdAt: 1_000,
  updatedAt: 2_000,
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

describe("peer API", () => {
  it("lists and validates peer thread payloads", async () => {
    vi.stubGlobal("fetch", vi.fn().mockResolvedValue(jsonResponse([THREAD])));

    await expect(listPeerThreads("0123456789abcdef")).resolves.toEqual([
      THREAD,
    ]);
    expect(() =>
      normalizePeerThread({ ...THREAD, targetAgent: "unknown" }),
    ).toThrow("invalid peer agent");
  });

  it("creates a new dedicated thread using an agent kind, not a terminal target", async () => {
    const fetchMock = vi.fn().mockImplementation(async () => jsonResponse(THREAD));
    vi.stubGlobal("fetch", fetchMock);

    await createPeerThread("0123456789abcdef", {
      sourceTerminalId: THREAD.sourceTerminalId,
      targetAgent: "claude",
      action: "review",
      instruction: "Review the architecture.",
      sourceReady: true,
    });

    expect(fetchMock).toHaveBeenCalledWith(
      "/api/peer/threads",
      expect.objectContaining({
        method: "POST",
        body: JSON.stringify({
          sourceTerminalId: THREAD.sourceTerminalId,
          targetAgent: "claude",
          action: "review",
          instruction: "Review the architecture.",
          sourceReady: true,
        }),
      }),
    );
  });

  it("adds and dispatches a follow-up in the same thread", async () => {
    const fetchMock = vi
      .fn()
      .mockImplementation(async () => jsonResponse(THREAD));
    vi.stubGlobal("fetch", fetchMock);

    await createPeerTurn("0123456789abcdef", THREAD.id, {
      action: "recheck",
      instruction: "Check the revised design.",
      sourceReady: true,
    });
    await dispatchPeerTurn("0123456789abcdef", THREAD.id, {
      turnId: THREAD.currentTurn.id,
      handoffRevision: 2,
      handoff: "Revised handoff.",
      reviewerReady: true,
    });
    await returnPeerTurn("0123456789abcdef", THREAD.id, {
      turnId: THREAD.currentTurn.id,
      sourceReady: true,
    });

    expect(fetchMock).toHaveBeenNthCalledWith(
      1,
      `/api/peer/threads/${THREAD.id}/turns`,
      expect.objectContaining({
        body: JSON.stringify({
          action: "recheck",
          instruction: "Check the revised design.",
          sourceReady: true,
        }),
      }),
    );
    expect(fetchMock).toHaveBeenNthCalledWith(
      2,
      `/api/peer/threads/${THREAD.id}/dispatch`,
      expect.objectContaining({
        body: JSON.stringify({
          turnId: THREAD.currentTurn.id,
          handoffRevision: 2,
          handoff: "Revised handoff.",
          reviewerReady: true,
        }),
      }),
    );
    expect(fetchMock).toHaveBeenNthCalledWith(
      3,
      `/api/peer/threads/${THREAD.id}/return`,
      expect.objectContaining({
        body: JSON.stringify({
          turnId: THREAD.currentTurn.id,
          sourceReady: true,
        }),
      }),
    );
  });

  it("closes the exact peer thread", async () => {
    const fetchMock = vi
      .fn()
      .mockResolvedValue(new Response(null, { status: 204 }));
    vi.stubGlobal("fetch", fetchMock);

    await deletePeerThread("0123456789abcdef", THREAD.id);

    expect(fetchMock).toHaveBeenCalledWith(
      `/api/peer/threads/${THREAD.id}`,
      expect.objectContaining({ method: "DELETE" }),
    );
  });
});
