import { afterEach, describe, expect, it, vi } from "vitest";
import { apiRequest } from "./api";

afterEach(() => { vi.useRealTimers(); vi.unstubAllGlobals(); });

describe("API deadlines", () => {
  it("preserves the machine-readable restore conflict code", async () => {
    vi.stubGlobal("fetch", vi.fn().mockResolvedValue(new Response(JSON.stringify({
      error: "The restored terminal was deleted.", code: "restore_deleted",
    }), { status: 409, headers: { "Content-Type": "application/json" } })));
    await expect(apiRequest("/api/sessions", "synthetic", { method: "POST" })).rejects.toMatchObject({
      status: 409, code: "restore_deleted",
    });
  });
  it("aborts a hung read and leaves no timer after cancellation", async () => {
    vi.useFakeTimers();
    vi.stubGlobal("fetch", vi.fn((_path, init) => new Promise((_resolve, reject) => {
      init.signal.addEventListener("abort", () => reject(init.signal.reason));
    })));
    const result = expect(apiRequest("/api/health", "synthetic")).rejects.toMatchObject({ name: "TimeoutError" });
    await vi.advanceTimersByTimeAsync(30_000);
    await result;
    expect(vi.getTimerCount()).toBe(0);
    const controller = new AbortController();
    const cancelled = expect(apiRequest("/api/health", "synthetic", { signal: controller.signal })).rejects.toMatchObject({ name: "AbortError" });
    controller.abort();
    await cancelled;
    expect(vi.getTimerCount()).toBe(0);
  });
  it("keeps the deadline active while the response body is pending", async () => {
    vi.useFakeTimers();
    vi.stubGlobal("fetch", vi.fn(async (_path, init) => ({
      ok: true, status: 200, headers: new Headers(),
      json: () => new Promise((_resolve, reject) => init.signal.addEventListener("abort", () => reject(init.signal.reason))),
    })));
    const result = expect(apiRequest("/api/health", "synthetic")).rejects.toMatchObject({ name: "TimeoutError" });
    await vi.advanceTimersByTimeAsync(30_000);
    await result;
    expect(vi.getTimerCount()).toBe(0);
  });
});
