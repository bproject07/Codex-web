import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it, vi } from "vitest";

import { MobileHeaderToggle } from "./App";

describe("MobileHeaderToggle", () => {
  it("exposes the compact status and expansion action", () => {
    const html = renderToStaticMarkup(
      <MobileHeaderToggle
        collapsed
        contextId="mobile-context"
        effectiveStatus="reconnecting"
        updateAvailable
        onToggle={vi.fn()}
      />,
    );

    expect(html).toContain('aria-expanded="false"');
    expect(html).toContain('aria-controls="mobile-context"');
    expect(html).toContain('title="Show project and Menu"');
    expect(html).toContain("status--reconnecting");
    expect(html).toContain("Connection: Reconnecting.");
    expect(html).toContain("Update available.");
    expect(html).toContain("⌄");
  });

  it("offers collapse without duplicating the expanded identity status", () => {
    const html = renderToStaticMarkup(
      <MobileHeaderToggle
        collapsed={false}
        contextId="mobile-context"
        effectiveStatus="connected"
        updateAvailable={false}
        onToggle={vi.fn()}
      />,
    );

    expect(html).toContain('aria-expanded="true"');
    expect(html).toContain('title="Hide project and Menu"');
    expect(html).toContain("⌃");
    expect(html).not.toContain("status--connected");
    expect(html).not.toContain("Update available.");
  });
});
