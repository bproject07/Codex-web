import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it, vi } from "vitest";
import type { AgentCatalog, SessionSnapshot } from "../api";
import type { WorkspaceBrowserAdapter } from "../workspaces/types";
import { TransferComposer } from "./TransferComposer";
import type { TransferSnapshot } from "./types";

const SOURCE: SessionSnapshot = {
  terminalId: "11111111-1111-4111-8111-111111111111",
  name: "Claude 1",
  agent: "claude",
  agentLabel: "Claude",
  isPrimary: true,
  createdAt: 1_000,
  sessionId: "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
  status: "running",
  connected: true,
  connectedClients: 1,
  startedAt: 1_100,
  pid: 123,
  exitCode: null,
  project: "/home/user/project",
  directoryId: "directory",
  lastError: null,
  purpose: { kind: "interactive" },
};

const OTHER_TAB: SessionSnapshot = {
  ...SOURCE,
  terminalId: "22222222-2222-4222-8222-222222222222",
  name: "Codex 2",
  agent: "codex",
  agentLabel: "Codex",
  isPrimary: false,
  sessionId: "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb",
};

const CATALOG: AgentCatalog = {
  schemaVersion: 1,
  server: { os: "linux", arch: "x86_64", shell: "sh" },
  agents: [
    {
      kind: "kilo",
      state: "ready",
      configuration: "auto",
      version: "7.8.18",
      dangerouslySkipPermissions: false,
      install: {
        command: "npm install -g @kilocode/cli",
        shell: "sh",
        verifyCommand: "kilo --version",
        updateCommand: "kilo upgrade",
        docsUrl: "",
        requiresServerAccess: true,
      },
    },
  ],
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
  addFavorite: vi.fn(async (directory) => ({ id: "favorite", directory })),
  removeFavorite: vi.fn(async () => undefined),
} satisfies WorkspaceBrowserAdapter;

const CALLBACKS = {
  workspaceAdapter: WORKSPACE_ADAPTER,
  onCreate: vi.fn(async () => undefined),
  onSaveDraft: vi.fn(async () => undefined),
  onChooseTarget: vi.fn(async () => undefined),
  onDeliver: vi.fn(async () => undefined),
  onCleanup: vi.fn(async () => undefined),
  onAbandon: vi.fn(async () => undefined),
  onSelectTerminal: vi.fn(),
  onClearError: vi.fn(),
  onClose: vi.fn(),
};

function transfer(overrides: Partial<TransferSnapshot>): TransferSnapshot {
  return {
    transferId: "12345678-1234-4234-8234-123456789abc",
    revision: 1,
    state: "preview",
    mode: "manual",
    sourceTerminalId: SOURCE.terminalId,
    sourceSessionId: SOURCE.sessionId,
    draftRevision: 1,
    approvedRevision: null,
    preparation: null,
    attempt: null,
    attemptsUsed: 0,
    delivery: null,
    errorCode: null,
    createdAt: 1,
    updatedAt: 1,
    ...overrides,
  };
}

function render(overrides: Partial<Parameters<typeof TransferComposer>[0]>) {
  return renderToStaticMarkup(
    <TransferComposer
      sourceSession={SOURCE}
      sessions={[SOURCE, OTHER_TAB]}
      catalog={CATALOG}
      catalogLoading={false}
      catalogError={null}
      newTargetDisabledReason={null}
      transfer={null}
      draft={null}
      operation={null}
      error={null}
      {...CALLBACKS}
      {...overrides}
    />,
  );
}

describe("TransferComposer", () => {
  it("offers automated or manual preparation before a transfer exists", () => {
    const html = render({});
    expect(html).toContain('role="dialog"');
    expect(html).toContain("Continue this work in another agent");
    expect(html).toContain("Ask the source agent");
    expect(html).toContain("Write it myself");
    expect(html).toContain('data-transfer-initial-focus="true"');
    expect(html).toContain("Ask the source to prepare");
    expect(html).toContain("never accepts those prompts for you");
    expect(html).not.toContain('data-transfer-deliver="true"');
  });

  it("defaults to manual mode and disables preparation for a stopped source", () => {
    const html = render({
      sourceSession: { ...SOURCE, status: "exited", sessionId: null },
    });
    expect(html).toMatch(/<input[^>]*disabled=""[^>]*value="prepare"/);
    expect(html).toMatch(/<input[^>]*checked=""[^>]*value="manual"/);
    expect(html).toContain("Write the handoff");
  });

  it("shows the draft editor with takeover while the source prepares", () => {
    const html = render({
      transfer: transfer({ state: "preparing", preparation: "pending", draftRevision: 0 }),
      draft: { draftRevision: 0, content: null },
    });
    expect(html).toContain("Source is preparing the handoff");
    expect(html).toContain("take over and write it yourself");
    expect(html).toContain('id="transfer-handoff"');
    expect(html).toContain("Take over and save");
  });

  it("lists new-agent and existing-tab targets in preview, excluding the source", () => {
    const html = render({
      transfer: transfer({ state: "preview" }),
      draft: { draftRevision: 1, content: "# Handoff" },
    });
    expect(html).toContain("Review the handoff");
    expect(html).toContain("New Kilo tab");
    expect(html).toContain("Existing tab: Codex 2");
    expect(html).not.toContain("Existing tab: Claude 1");
    expect(html).toContain("Start the new tab");
    expect(html).toContain("Change folder");
    expect(html).toContain("Abandon");
  });

  it("keeps existing tabs selectable when capacity blocks new ones", () => {
    const html = render({
      transfer: transfer({ state: "preview" }),
      draft: { draftRevision: 1, content: "# Handoff" },
      newTargetDisabledReason: "Session capacity reached (20 of 20).",
    });
    expect(html).toMatch(/<input[^>]*disabled=""[^>]*value="new-kilo"/);
    expect(html).not.toMatch(/<input[^>]*disabled=""[^>]*value="existing-22222222/);
    expect(html).toMatch(/<input[^>]*checked=""[^>]*value="existing-22222222/);
    expect(html).toContain("Existing running tabs can still be chosen.");
    expect(html).toContain("Use this tab");
  });

  it("requires the explicit Deliver action once a target is bound", () => {
    const html = render({
      transfer: transfer({
        state: "target_bound",
        attempt: {
          targetRequestId: "44444444-4444-4444-8444-444444444444",
          number: 1,
          target: {
            mode: "new",
            terminalId: OTHER_TAB.terminalId,
            sessionId: OTHER_TAB.sessionId,
            agent: "kilo",
            name: "Kilo 3",
            project: "/home/user/project",
          },
          outcome: "bound",
        },
        attemptsUsed: 1,
      }),
      draft: { draftRevision: 1, content: "# Handoff" },
    });
    expect(html).toContain("Target ready to receive");
    expect(html).toContain('data-transfer-deliver="true"');
    expect(html).toContain("Delivering needs the target tab at an idle, empty prompt.");
    expect(html).toContain("Show target tab");
    expect(html).toContain("Target attempts used: 1 of 8.");
  });

  it("labels unconfirmed delivery and explains settled target failures", () => {
    const unconfirmed = render({
      transfer: transfer({
        state: "awaiting_receipt",
        delivery: "unconfirmed",
        approvedRevision: 1,
        attempt: {
          targetRequestId: "44444444-4444-4444-8444-444444444444",
          number: 1,
          target: {
            mode: "existing",
            terminalId: OTHER_TAB.terminalId,
            sessionId: OTHER_TAB.sessionId,
            agent: "codex",
            name: "Codex 2",
            project: "/home/user/project",
          },
          outcome: "bound",
        },
        attemptsUsed: 1,
      }),
    });
    expect(unconfirmed).toContain("Delivery unconfirmed");
    expect(unconfirmed).toContain("Nothing is resent automatically.");
    expect(unconfirmed).toContain("Discard");
    expect(unconfirmed).not.toContain('id="transfer-handoff"');

    const ended = render({
      transfer: transfer({ state: "preview", errorCode: "target_ended", delivery: "target_ended" }),
      draft: { draftRevision: 1, content: "# Handoff" },
    });
    expect(ended).toContain("the draft was kept");
    expect(ended).toContain("New Kilo tab");
  });

  it("offers cleanup retry and a final Close for terminal states", () => {
    const pending = render({
      transfer: transfer({
        state: "cleanup_pending",
        errorCode: "cleanup_pending",
        attempt: {
          targetRequestId: "44444444-4444-4444-8444-444444444444",
          number: 1,
          target: {
            mode: "new",
            terminalId: "55555555-5555-4555-8555-555555555555",
            sessionId: null,
            agent: "kilo",
            name: "Kilo 4",
            project: "/home/user/project",
          },
          outcome: "cleanup_pending",
        },
        attemptsUsed: 1,
      }),
    });
    expect(pending).toContain("Retry cleanup");
    expect(pending).toContain("could not be confirmed stopped");

    const delivered = render({
      transfer: transfer({
        state: "delivered",
        attempt: {
          targetRequestId: "44444444-4444-4444-8444-444444444444",
          number: 1,
          target: {
            mode: "new",
            terminalId: OTHER_TAB.terminalId,
            sessionId: OTHER_TAB.sessionId,
            agent: "kilo",
            name: "Kilo 3",
            project: "/home/user/project",
          },
          outcome: "delivered",
        },
        attemptsUsed: 1,
      }),
    });
    expect(delivered).toContain("confirmed the handoff");
    expect(delivered).toContain("Go to Kilo 3");
    expect(render({ transfer: transfer({ state: "abandoned" }) })).toContain("Abandoned");
  });
});
