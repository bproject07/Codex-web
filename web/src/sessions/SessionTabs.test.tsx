import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it, vi } from "vitest";
import type { SessionSnapshot } from "../api";
import type { PeerThread } from "../peer";
import { SessionTabs } from "./SessionTabs";

const PRIMARY: SessionSnapshot = {
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
  project: "C:\\Projects\\demo",
  directoryId: "directory",
  lastError: null,
  purpose: { kind: "interactive" },
};

const THREAD: PeerThread = {
  id: "12345678-1234-4234-8234-123456789abc",
  sourceTerminalId: PRIMARY.terminalId,
  reviewerTerminalId: "22222222-2222-4222-8222-222222222222",
  targetAgent: "claude",
  status: "response_ready",
  currentTurn: {
    id: "33333333-3333-4333-8333-333333333333",
    sequence: 1,
    action: "review",
    instruction: "Review.",
    status: "response_ready",
    handoff: "Context.",
    handoffRevision: 1,
    response: "Result.",
    error: null,
  },
  createdAt: 2_000,
  updatedAt: 3_000,
};

const REVIEWER: SessionSnapshot = {
  ...PRIMARY,
  terminalId: THREAD.reviewerTerminalId!,
  name: "Claude 2",
  agent: "claude",
  isPrimary: false,
  createdAt: 2_000,
  purpose: {
    kind: "peer",
    threadId: THREAD.id,
    parentTerminalId: PRIMARY.terminalId,
  },
};

describe("SessionTabs", () => {
  it("uses sibling close controls and keeps the primary protected", () => {
    const html = renderToStaticMarkup(
      <SessionTabs
        sessions={[PRIMARY, REVIEWER]}
        maxSessions={20}
        selectedTerminalId={PRIMARY.terminalId}
        busy={false}
        peerThreads={[THREAD]}
        peerDisabledReason={null}
        onSelect={vi.fn()}
        onClose={vi.fn()}
        onCreate={vi.fn()}
        onPeer={vi.fn()}
        onManage={vi.fn()}
      />,
    );

    expect(html).not.toContain("Close Codex 1");
    expect(html).toContain("Close ↳ Claude Review · R-123456");
    expect(html).toContain("session-tab-shell--peer");
    expect(html).toContain("Open @cwt peer collaboration");
    expect(html).toContain("1 peer response ready");
    expect(html).toContain(
      '</button><button type="button" class="session-tab-close"',
    );
  });

  it("explains why peer collaboration is disabled for a stopped source", () => {
    const reason =
      "@cwt is unavailable because Codex 1 is exited. A running source terminal is required.";
    const html = renderToStaticMarkup(
      <SessionTabs
        sessions={[{ ...PRIMARY, status: "exited" }]}
        maxSessions={20}
        selectedTerminalId={PRIMARY.terminalId}
        busy={false}
        peerThreads={[]}
        peerDisabledReason={reason}
        onSelect={vi.fn()}
        onClose={vi.fn()}
        onCreate={vi.fn()}
        onPeer={vi.fn()}
        onManage={vi.fn()}
      />,
    );

    expect(html).toContain(`title="${reason}"`);
    expect(html).toContain(`aria-label="${reason}"`);
    expect(html).toContain("disabled");
  });

  it("shows configured capacity and disables only new interactive sessions", () => {
    const html = renderToStaticMarkup(
      <SessionTabs
        sessions={[PRIMARY, REVIEWER]}
        maxSessions={2}
        selectedTerminalId={PRIMARY.terminalId}
        busy={false}
        peerThreads={[THREAD]}
        peerDisabledReason={null}
        onSelect={vi.fn()}
        onClose={vi.fn()}
        onCreate={vi.fn()}
        onPeer={vi.fn()}
        onManage={vi.fn()}
      />,
    );

    const newButton = html.match(
      /<button[^>]*class="[^"]*session-new-button[^"]*"[^>]*>/,
    )?.[0];
    const peerButton = html.match(
      /<button[^>]*class="[^"]*session-peer-button[^"]*"[^>]*>/,
    )?.[0];

    expect(html).toContain("Manage 2 of 2 terminal sessions");
    expect(html).toContain("Manage 2/2");
    expect(newButton).toContain("Session capacity reached (2 of 2)");
    expect(newButton).toContain("disabled");
    expect(peerButton).toContain("Open @cwt peer collaboration");
    expect(peerButton).not.toContain("disabled");
  });

  it("keeps session creation available when capacity metadata is unknown", () => {
    const html = renderToStaticMarkup(
      <SessionTabs
        sessions={[PRIMARY]}
        maxSessions={null}
        selectedTerminalId={PRIMARY.terminalId}
        busy={false}
        peerThreads={[]}
        peerDisabledReason={null}
        onSelect={vi.fn()}
        onClose={vi.fn()}
        onCreate={vi.fn()}
        onPeer={vi.fn()}
        onManage={vi.fn()}
      />,
    );
    const newButton = html.match(
      /<button[^>]*class="[^"]*session-new-button[^"]*"[^>]*>/,
    )?.[0];

    expect(html).toContain("Manage 1 terminal session");
    expect(newButton).toContain("Choose an agent for a new terminal");
    expect(newButton).not.toContain("disabled");
  });
});
