import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it, vi } from "vitest";
import type { AgentCatalog, SessionSnapshot } from "../api";
import type { WorkspaceBrowserAdapter } from "../workspaces/types";
import { PeerComposer } from "./PeerComposer";
import type { PeerThread } from "./types";

const SOURCE: SessionSnapshot = {
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

const CATALOG: AgentCatalog = {
  schemaVersion: 1,
  server: { os: "windows", arch: "x86_64", shell: "PowerShell" },
  agents: [
    {
      kind: "claude",
      state: "ready",
      configuration: "auto",
      version: "1.0.0",
      dangerouslySkipPermissions: false,
      install: {
        command: "",
        shell: "PowerShell",
        verifyCommand: "claude --version",
        updateCommand: "",
        docsUrl: "",
        requiresServerAccess: true,
      },
    },
  ],
};

const THREAD: PeerThread = {
  id: "12345678-1234-4234-8234-123456789abc",
  sourceTerminalId: SOURCE.terminalId,
  reviewerTerminalId: "22222222-2222-4222-8222-222222222222",
  targetAgent: "claude",
  status: "returned",
  currentTurn: {
    id: "33333333-3333-4333-8333-333333333333",
    sequence: 1,
    action: "review",
    instruction: "Review it.",
    status: "returned",
    handoff: "Context.",
    handoffRevision: 1,
    response: "Looks good.",
    error: null,
  },
  createdAt: 1_000,
  updatedAt: 2_000,
};

const WORKSPACE_ADAPTER = {
  loadLibrary: vi.fn(async () => ({ favorites: [], recent: [] })),
  listRoots: vi.fn(async () => ({
    current: null,
    parentId: null,
    breadcrumbs: [],
    directories: [],
    truncated: false,
  })),
  listDirectory: vi.fn(async () => ({
    current: null,
    parentId: null,
    breadcrumbs: [],
    directories: [],
    truncated: false,
  })),
  resolvePath: vi.fn(async () => ({
    current: null,
    parentId: null,
    breadcrumbs: [],
    directories: [],
    truncated: false,
  })),
  addFavorite: vi.fn(async (directory) => ({
    id: "favorite",
    directory,
  })),
  removeFavorite: vi.fn(async () => undefined),
} satisfies WorkspaceBrowserAdapter;

const CALLBACKS = {
  workspaceAdapter: WORKSPACE_ADAPTER,
  onCreateThread: vi.fn(async () => THREAD),
  onCreateTurn: vi.fn(async () => THREAD),
  onDispatchTurn: vi.fn(async () => THREAD),
  onReturnTurn: vi.fn(async () => THREAD),
  onRefreshThreads: vi.fn(async () => undefined),
  onClearError: vi.fn(),
  onClose: vi.fn(),
};

const READY_STATE = {
  threadsReady: true,
  threadsLoading: false,
  catalogError: null,
};

describe("PeerComposer", () => {
  it("offers only agent kinds and first-turn actions for a clean peer", () => {
    const html = renderToStaticMarkup(
      <PeerComposer
        sourceSession={SOURCE}
        allowNew
        catalog={CATALOG}
        catalogLoading={false}
        threads={[]}
        operation={null}
        error={null}
        {...READY_STATE}
        {...CALLBACKS}
      />,
    );

    expect(html).toContain("@cwt");
    expect(html).toContain("Dedicated reviewer");
    expect(html).toContain("Reviewer folder");
    expect(html).toContain("C:\\Projects\\demo");
    expect(html).toContain("Change folder");
    expect(html).toContain("Defaults to the source tab folder");
    expect(html).toContain("Claude");
    expect(html).toContain("Review");
    expect(html).toContain("Verify");
    expect(html).toContain("Ask");
    expect(html).toContain("Handoff");
    expect(html).not.toContain(">Recheck<");
    expect(html).not.toContain("terminal target");
    expect(html).toContain("Enter adds a new line");
    expect(html).toContain("Source ready — Prepare handoff");
    expect(html).toContain("Catalog Ready checks the executable and version");
    expect(html).toContain("@cwt never accepts those prompts for you");
  });

  it("gives actionable readiness guidance while a handoff is preparing", () => {
    const preparingThread: PeerThread = {
      ...THREAD,
      status: "preparing_handoff",
      currentTurn: {
        ...THREAD.currentTurn,
        status: "preparing_handoff",
        handoff: null,
        response: null,
      },
    };
    const html = renderToStaticMarkup(
      <PeerComposer
        sourceSession={SOURCE}
        initialThreadId={THREAD.id}
        allowNew
        catalog={CATALOG}
        catalogLoading={false}
        threads={[preparingThread]}
        operation={null}
        error={null}
        {...READY_STATE}
        {...CALLBACKS}
      />,
    );

    expect(html).toContain("Codex 1 is preparing");
    expect(html).toContain("inspect Codex 1 for an approval or an unsent prompt");
    expect(html).toContain("fresh terminal may still require sign-in");
  });

  it("reuses an existing reviewer and exposes its response and Recheck", () => {
    const html = renderToStaticMarkup(
      <PeerComposer
        sourceSession={SOURCE}
        initialThreadId={THREAD.id}
        allowNew
        catalog={CATALOG}
        catalogLoading={false}
        threads={[THREAD]}
        operation={null}
        error={null}
        {...READY_STATE}
        {...CALLBACKS}
      />,
    );

    expect(html).toContain("Looks good.");
    expect(html).toContain("Recheck");
    expect(html).toContain("R-123456");
    expect(html).toContain("Source ready — Prepare follow-up");
    expect(html).not.toContain("<legend>Dedicated reviewer</legend>");
    expect(html).not.toContain("Change folder");
  });

  it("blocks only fresh reviewers when terminal capacity is full", () => {
    const reason =
      "Session capacity reached (20 of 20). Close a terminal tab before starting a new reviewer.";
    const html = renderToStaticMarkup(
      <PeerComposer
        sourceSession={SOURCE}
        allowNew
        newThreadDisabledReason={reason}
        catalog={CATALOG}
        catalogLoading={false}
        threads={[]}
        operation={null}
        error={null}
        {...READY_STATE}
        {...CALLBACKS}
      />,
    );

    expect(html).toContain(reason);
    expect(html).toContain("+ New peer");
    expect(html).toContain('<fieldset class="peer-targets" disabled="">');
    expect(html).not.toContain("Source ready — Prepare follow-up");
  });

  it("keeps existing reviewer follow-ups available at terminal capacity", () => {
    const html = renderToStaticMarkup(
      <PeerComposer
        sourceSession={SOURCE}
        initialThreadId={THREAD.id}
        allowNew
        newThreadDisabledReason="Session capacity reached (20 of 20)."
        catalog={CATALOG}
        catalogLoading={false}
        threads={[THREAD]}
        operation={null}
        error={null}
        {...READY_STATE}
        {...CALLBACKS}
      />,
    );

    expect(html).toContain("Source ready — Prepare follow-up");
    expect(html).toContain('<fieldset class="peer-actions">');
    expect(html).toContain('title="Session capacity reached (20 of 20)."');
  });

  it("shows an editable preview before dispatch", () => {
    const previewThread: PeerThread = {
      ...THREAD,
      status: "awaiting_preview",
      currentTurn: {
        ...THREAD.currentTurn,
        status: "awaiting_preview",
        handoff: "Prepared summary.",
        response: null,
      },
    };
    const html = renderToStaticMarkup(
      <PeerComposer
        sourceSession={SOURCE}
        initialThreadId={THREAD.id}
        allowNew
        catalog={CATALOG}
        catalogLoading={false}
        threads={[previewThread]}
        operation={null}
        error={null}
        {...READY_STATE}
        {...CALLBACKS}
      />,
    );

    expect(html).toContain("Preview handoff");
    expect(html).toContain("Prepared summary.");
    expect(html).toContain("Reviewer ready — Send");
  });

  it("requires an explicit return before starting another turn", () => {
    const responseThread: PeerThread = {
      ...THREAD,
      status: "response_ready",
      currentTurn: {
        ...THREAD.currentTurn,
        status: "response_ready",
      },
    };
    const html = renderToStaticMarkup(
      <PeerComposer
        sourceSession={SOURCE}
        initialThreadId={THREAD.id}
        allowNew
        catalog={CATALOG}
        catalogLoading={false}
        threads={[responseThread]}
        operation={null}
        error={null}
        {...READY_STATE}
        {...CALLBACKS}
      />,
    );

    expect(html).toContain("Source ready — Return");
    expect(html).not.toContain("Prepare follow-up");
  });

  it("blocks new reviewers until the initial thread list is known", () => {
    const html = renderToStaticMarkup(
      <PeerComposer
        sourceSession={SOURCE}
        allowNew
        catalog={CATALOG}
        catalogLoading={false}
        catalogError={null}
        threads={[]}
        threadsReady={false}
        threadsLoading
        operation={null}
        error={null}
        {...CALLBACKS}
      />,
    );

    expect(html).toContain("Loading peer conversations");
    expect(html).not.toContain("Prepare handoff");
    expect(html).not.toContain("+ New peer");
  });

  it("shows a recoverable thread-list error without offering a duplicate reviewer", () => {
    const html = renderToStaticMarkup(
      <PeerComposer
        sourceSession={SOURCE}
        allowNew
        catalog={CATALOG}
        catalogLoading={false}
        catalogError={null}
        threads={[]}
        threadsReady={false}
        threadsLoading={false}
        operation={null}
        error="Could not load peer conversations."
        {...CALLBACKS}
      />,
    );

    expect(html).toContain("Could not load peer conversations.");
    expect(html).toContain("Retry");
    expect(html).not.toContain("Prepare handoff");
  });

  it("waits for an explicitly requested thread without exposing a sibling", () => {
    const requestedThreadId = "99999999-9999-4999-8999-999999999999";
    const html = renderToStaticMarkup(
      <PeerComposer
        sourceSession={SOURCE}
        initialThreadId={requestedThreadId}
        allowNew
        catalog={CATALOG}
        catalogLoading={false}
        catalogError={null}
        threads={[THREAD]}
        threadsReady
        threadsLoading={false}
        operation={null}
        error={null}
        {...CALLBACKS}
      />,
    );

    expect(html).toContain("Selected peer conversation unavailable");
    expect(html).toContain("without selecting another conversation");
    expect(html).toContain("Retry");
    expect(html).not.toContain("Looks good.");
    expect(html).not.toContain("Source ready — Prepare follow-up");
    expect(html).not.toContain("+ New peer");
  });

  it("surfaces agent discovery failures instead of reporting an empty catalog", () => {
    const html = renderToStaticMarkup(
      <PeerComposer
        sourceSession={SOURCE}
        allowNew
        catalog={null}
        catalogLoading={false}
        catalogError="Agent discovery failed."
        threads={[]}
        threadsReady
        threadsLoading={false}
        operation={null}
        error={null}
        {...CALLBACKS}
      />,
    );

    expect(html).toContain("Agent discovery failed.");
    expect(html).not.toContain(
      "No installed agent is ready for a dedicated review.",
    );
  });
});
