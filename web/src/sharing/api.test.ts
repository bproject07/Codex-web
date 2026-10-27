import { afterEach, describe, expect, it, vi } from "vitest";
import { newCredential, request, singleLine, SharingError, textBytes, validName, validProposal } from "./api";

afterEach(() => vi.unstubAllGlobals());
describe("guest isolation", () => {
  it("uses Unicode characters for names and UTF-8 bytes for proposal limits", () => {
    expect(validName("Ж".repeat(32))).toBe(true);
    expect(validName("Ж".repeat(33))).toBe(false);
    expect(validName("𐐀".repeat(32))).toBe(true);
    expect(validName("owner")).toBe(false);
    expect(validName("А\nБ")).toBe(false);
    expect(textBytes("Ж".repeat(8192))).toBe(16384);
    expect(validProposal("Ж".repeat(8192))).toBe(true);
    expect(validProposal("Ж".repeat(8193))).toBe(false);
    expect(validProposal("one\ntwo\tthree")).toBe(true);
    expect(validProposal("one\rtwo")).toBe(false);
  });
  it("sends only the scoped credential in a header and disables HTTP caching and cookies", async () => {
    const fetch = vi.fn().mockResolvedValue(new Response('{"ok":true}'));
    vi.stubGlobal("fetch", fetch);
    await request("/api/share/me", "guest-only");
    expect(fetch).toHaveBeenCalledWith("/api/share/me", expect.objectContaining({
      credentials: "omit", cache: "no-store", headers: { Authorization: "Bearer guest-only" },
    }));
    expect(fetch.mock.calls[0]![0]).not.toContain("guest-only");
  });
  it("retains the exact reviewed text and request identity", async () => {
    const fetch = vi.fn().mockResolvedValue(new Response(null, { status: 204 }));
    vi.stubGlobal("fetch", fetch);
    const proposal = { requestId: "same-attempt", text: "  Точно този текст  " };
    await request("/api/share/proposals", "guest", "POST", proposal);
    expect(JSON.parse(fetch.mock.calls[0]![1].body)).toEqual(proposal);
  });
  it("rejects control characters for automatic delivery, including line breaks and tabs", () => {
    expect(singleLine("  Прегледай кода — точно  ")).toBe(true);
    for (const text of ["one\ntwo", "one\rtwo", "one\ttwo", "\u001b[31m", "\u009b", "\u0000"]) {
      expect(singleLine(text)).toBe(false);
    }
  });
  it("returns stable bounded public errors without echoing response contents", async () => {
    vi.stubGlobal("fetch", vi.fn().mockResolvedValue(new Response(
      '{"code":"access_ended","details":"private terminal content"}', { status: 401 },
    )));
    await expect(request("/api/share/me", "revoked")).rejects.toEqual(new SharingError("access_ended", 401));
  });
  it("generates distinct UUID-bound 256-bit credentials without a secure-context-only API", () => {
    const first = newCredential();
    expect(first).toMatch(/^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}\.[\w-]{43}$/);
    expect(newCredential()).not.toBe(first);
  });
});
