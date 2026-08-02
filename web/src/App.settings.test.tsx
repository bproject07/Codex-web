import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it, vi } from "vitest";
import { SettingsPanel, type SettingsPanelProps } from "./App";
import { DEFAULT_SETTINGS } from "./terminal/settings";

function renderPanel(overrides: Partial<SettingsPanelProps> = {}): string {
  return renderToStaticMarkup(
    <SettingsPanel
      settings={DEFAULT_SETTINGS}
      busy={false}
      agentLabel="Codex"
      restartDisabled={false}
      onRestart={vi.fn()}
      serverRestartDisabled={false}
      serverRestartUnavailableReason={null}
      onRestartServer={vi.fn()}
      onChange={vi.fn()}
      updateStatus={null}
      updateLoading={false}
      updateOperation={null}
      updateError={null}
      onCheckForUpdate={vi.fn()}
      onApplyUpdate={vi.fn()}
      onClose={vi.fn()}
      onTerminate={vi.fn()}
      onForgetToken={vi.fn()}
      viewportDiagnostics=""
      viewportDiagnosticsCollecting={false}
      viewportDiagnosticsCopyNotice={null}
      onCopyViewportDiagnostics={vi.fn()}
      {...overrides}
    />,
  );
}

describe("SettingsPanel", () => {
  it("keeps only true settings — general actions moved to the header Menu", () => {
    const html = renderPanel();

    expect(html).toContain("Font size");
    expect(html).toContain("Scrollback lines");
    expect(html).toContain("Theme");
    expect(html).toContain("Blinking cursor");
    expect(html).toContain("Show mobile keys");
    expect(html).toContain("Copy diagnostics");
    expect(html).toContain("Restart server");
    expect(html).toContain("Restart Codex");
    expect(html).toContain("Terminate Codex");
    expect(html).toContain("Forget token");

    expect(html).not.toContain("New terminal");
    expect(html).not.toContain("Manage sessions");
    expect(html).not.toContain("Full screen");
    expect(html).not.toContain("Fullscreen");
    expect(html).not.toContain("session-new-button");
    expect(html).not.toContain("session-manage-button");
    expect(html).not.toContain("settings-actions");
    expect(html).not.toContain("Reconnect");
  });

  it("disables restarting a dedicated peer reviewer", () => {
    const html = renderPanel({ restartDisabled: true, agentLabel: "Claude" });

    const restartButton = html.match(
      /<button[^>]*title="Restart Claude"[^>]*>/,
    )?.[0];

    expect(restartButton).toContain("disabled");
    expect(html).toContain("Terminate Claude");
  });

  it("explains when an older stable launcher cannot restart the server", () => {
    const reason =
      "Install this complete release package as the launcher before using server restart.";
    const html = renderPanel({
      serverRestartDisabled: true,
      serverRestartUnavailableReason: reason,
    });

    expect(html).toContain(reason);
    const restartButton = html.match(
      /<button[^>]*title="Install this complete release package[^"]*"[^>]*>/,
    )?.[0];
    expect(restartButton).toContain("disabled");
  });
});
