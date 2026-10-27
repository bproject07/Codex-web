import { afterEach, describe, expect, it, vi } from "vitest";
import { ApiError } from "../api";
import {
  abandonTransfer,
  chooseTransferTarget,
  cleanupTransfer,
  createTransfer,
  deliverTransfer,
  getTransferDraft,
  listTransfers,
  normalizeTransferSnapshot,
  putTransferDraft,
} from "./api";

const TRANSFER_ID = "12345678-1234-4234-8234-123456789abc";
const SOURCE_ID = "11111111-1111-4111-8111-111111111111";
const TARGET_ID = "22222222-2222-4222-8222-222222222222";
const SESSION_ID = "33333333-3333-4333-8333-333333333333";

const WIRE = {
  transferId: TRANSFER_ID,
  revision: 4,
  state: "target_bound",
  mode: "manual",
  sourceTerminalId: SOURCE_ID,
  sourceSessionId: null,
  draftRevision: 2,
  preparation: "overridden",
  attempt: {
    targetRequestId: "44444444-4444-4444-8444-444444444444",
    number: 1,
    target: {
      mode: "existing",
      terminalId: TARGET_ID,
      sessionId: SESSION_ID,
      agent: "kilo",
      name: "Kilo 2",
      project: "/home/user/project",
    },
    outcome: "bound",
  },
  attemptsUsed: 1,
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

describe("transfer API", () => {
  it("normalizes a snapshot and treats omitted optional fields as null", () => {
    const snapshot = normalizeTransferSnapshot(WIRE);
    expect(snapshot).toMatchObject({
      transferId: TRANSFER_ID,
      state: "target_bound",
      mode: "manual",
      sourceSessionId: null,
      approvedRevision: null,
      preparation: "overridden",
      delivery: null,
      errorCode: null,
      attempt: {
        outcome: "bound",
        target: { mode: "existing", agent: "kilo", sessionId: SESSION_ID },
      },
    });
    expect(
      normalizeTransferSnapshot({
        ...WIRE,
        attempt: null,
        error: { code: "queue_rejected" },
        delivery: "unconfirmed",
      }),
    ).toMatchObject({ attempt: null, errorCode: "queue_rejected", delivery: "unconfirmed" });
  });

  it("rejects unknown states, agents, and malformed ids", () => {
    expect(() => normalizeTransferSnapshot({ ...WIRE, state: "weird" })).toThrow(
      "transfer state",
    );
    expect(() =>
      normalizeTransferSnapshot({
        ...WIRE,
        attempt: { ...WIRE.attempt, target: { ...WIRE.attempt.target, agent: "future" } },
      }),
    ).toThrow("transfer agent");
    expect(() => normalizeTransferSnapshot({ ...WIRE, transferId: "../x" })).toThrow(
      "transfer id",
    );
    expect(() => normalizeTransferSnapshot({ ...WIRE, error: { code: "Bad Code!" } })).not.toThrow();
    expect(normalizeTransferSnapshot({ ...WIRE, error: { code: "Bad Code!" } }).errorCode).toBe(
      "invalid_state",
    );
  });

  it("sends each mutation to its route with the exact body", async () => {
    // A Response body can be read once, so hand out a fresh one per call.
    const fetchMock = vi.fn().mockImplementation(async () => jsonResponse(WIRE));
    vi.stubGlobal("fetch", fetchMock);
    const token = "0123456789abcdef";

    await createTransfer(token, {
      transferId: TRANSFER_ID,
      sourceTerminalId: SOURCE_ID,
      sourceSessionId: null,
      mode: "manual",
      sourceReady: false,
      instruction: "Carry on.",
    });
    await putTransferDraft(token, TRANSFER_ID, 0, "# Handoff");
    await chooseTransferTarget(token, TRANSFER_ID, {
      targetRequestId: "44444444-4444-4444-8444-444444444444",
      target: { mode: "new", agent: "kilo", directoryId: "dir" },
    });
    await deliverTransfer(token, TRANSFER_ID, {
      expectedRevision: 2,
      targetSessionId: SESSION_ID,
      targetReady: true,
    });
    await cleanupTransfer(token, TRANSFER_ID, {
      targetTerminalId: TARGET_ID,
      targetSessionId: null,
    });
    await abandonTransfer(token, TRANSFER_ID);

    const calls = fetchMock.mock.calls.map(([url, init]) => [
      url,
      (init as RequestInit).method,
      (init as RequestInit).body,
    ]);
    expect(calls).toEqual([
      [
        "/api/transfers",
        "POST",
        JSON.stringify({
          transferId: TRANSFER_ID,
          sourceTerminalId: SOURCE_ID,
          sourceSessionId: null,
          mode: "manual",
          sourceReady: false,
          instruction: "Carry on.",
        }),
      ],
      [
        `/api/transfers/${TRANSFER_ID}/draft`,
        "PUT",
        JSON.stringify({ expectedRevision: 0, content: "# Handoff" }),
      ],
      [
        `/api/transfers/${TRANSFER_ID}/target`,
        "POST",
        JSON.stringify({
          targetRequestId: "44444444-4444-4444-8444-444444444444",
          target: { mode: "new", agent: "kilo", directoryId: "dir" },
        }),
      ],
      [
        `/api/transfers/${TRANSFER_ID}/deliver`,
        "POST",
        JSON.stringify({
          expectedRevision: 2,
          targetSessionId: SESSION_ID,
          targetReady: true,
        }),
      ],
      [
        `/api/transfers/${TRANSFER_ID}/cleanup`,
        "POST",
        JSON.stringify({ targetTerminalId: TARGET_ID, targetSessionId: null }),
      ],
      [
        `/api/transfers/${TRANSFER_ID}/abandon`,
        "POST",
        JSON.stringify({ reason: "user" }),
      ],
    ]);
  });

  it("reads drafts and lists with filters, and surfaces server error codes", async () => {
    const fetchMock = vi
      .fn()
      .mockResolvedValueOnce(jsonResponse({ draftRevision: 3, content: "# Draft" }))
      .mockResolvedValueOnce(jsonResponse([WIRE]))
      .mockResolvedValueOnce(
        jsonResponse(
          { error: "The draft changed since it was last read", code: "revision_conflict" },
          409,
        ),
      );
    vi.stubGlobal("fetch", fetchMock);

    await expect(getTransferDraft("t", TRANSFER_ID)).resolves.toEqual({
      draftRevision: 3,
      content: "# Draft",
    });
    await expect(listTransfers("t", { source: SOURCE_ID })).resolves.toHaveLength(1);
    expect(fetchMock.mock.calls[1][0]).toBe(`/api/transfers?source=${SOURCE_ID}`);
    const failure = await putTransferDraft("t", TRANSFER_ID, 1, "x").catch((e) => e);
    expect(failure).toBeInstanceOf(ApiError);
    expect((failure as ApiError).status).toBe(409);
    expect((failure as ApiError).code).toBe("revision_conflict");
  });
});
