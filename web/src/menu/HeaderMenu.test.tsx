import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it, vi } from "vitest";

import { HeaderMenu, type HeaderMenuItem } from "./HeaderMenu";

function buildItems(overrides: {
  newTerminalDisabled?: boolean;
  updateAvailable?: boolean;
  sessionCountLabel?: string;
} = {}): HeaderMenuItem[] {
  const {
    newTerminalDisabled = false,
    updateAvailable = false,
    sessionCountLabel = "4/20",
  } = overrides;
  return [
    {
      key: "new-terminal",
      className: "session-new-button",
      label: "New terminal",
      title: newTerminalDisabled
        ? "Session capacity reached (20 of 20)"
        : "Choose an agent for a new terminal",
      disabled: newTerminalDisabled,
      onSelect: vi.fn(),
    },
    {
      key: "settings",
      label: "Settings",
      title: updateAvailable
        ? "Open settings — update available"
        : "Open terminal settings",
      badge: updateAvailable ? "↑" : undefined,
      badgeLabel: updateAvailable ? "Update available" : undefined,
      onSelect: vi.fn(),
    },
    {
      key: "manage-sessions",
      className: "session-manage-button",
      label: `Manage sessions (${sessionCountLabel})`,
      title: "Manage 4 of 20 terminal sessions",
      onSelect: vi.fn(),
    },
    {
      key: "fullscreen",
      label: "Full screen",
      title: "Toggle fullscreen",
      onSelect: vi.fn(),
    },
  ];
}

describe("HeaderMenu", () => {
  it("renders an accessible closed trigger labelled Menu", () => {
    const html = renderToStaticMarkup(<HeaderMenu items={buildItems()} />);

    const trigger = html.match(
      /<button[^>]*class="[^"]*header-menu-trigger[^"]*"[^>]*>/,
    )?.[0];
    expect(trigger).toContain('title="Menu"');
    expect(trigger).toContain('aria-label="Menu"');
    expect(trigger).toContain('aria-haspopup="menu"');
    expect(trigger).toContain('aria-expanded="false"');
    expect(html).toContain("…");
    expect(html).not.toContain('role="menu"');
  });

  it("renders the four actions in order when open", () => {
    const html = renderToStaticMarkup(
      <HeaderMenu items={buildItems()} defaultOpen />,
    );

    expect(html).toContain('role="menu"');
    expect(html).toContain('aria-expanded="true"');
    expect(html).toContain("aria-controls");
    const labels = [
      ...html.matchAll(
        /<span class="header-menu-item-label">([^<]*)<\/span>/g,
      ),
    ].map((match) => match[1]);
    expect(labels).toEqual([
      "New terminal",
      "Settings",
      "Manage sessions (4/20)",
      "Full screen",
    ]);
    const itemCount = (html.match(/role="menuitem"/g) ?? []).length;
    expect(itemCount).toBe(4);
    expect(html).toContain("session-new-button");
    expect(html).toContain("session-manage-button");
  });

  it("disables New terminal at capacity with the explanation", () => {
    const html = renderToStaticMarkup(
      <HeaderMenu
        items={buildItems({ newTerminalDisabled: true })}
        defaultOpen
      />,
    );

    const newItem = html.match(
      /<button[^>]*class="[^"]*session-new-button[^"]*"[^>]*>/,
    )?.[0];
    expect(newItem).toContain("disabled");
    expect(newItem).toContain('aria-disabled="true"');
    expect(newItem).toContain("Session capacity reached (20 of 20)");
  });

  it("surfaces the update badge on the trigger and the Settings item", () => {
    const html = renderToStaticMarkup(
      <HeaderMenu
        items={buildItems({ updateAvailable: true })}
        triggerBadge="↑"
        triggerBadgeLabel="Update available"
        defaultOpen
      />,
    );

    const badges = (html.match(/header-update-badge/g) ?? []).length;
    expect(badges).toBe(2);
    expect(html).toContain('aria-label="Update available"');
    expect(html).toContain("Open settings — update available");
  });
});
