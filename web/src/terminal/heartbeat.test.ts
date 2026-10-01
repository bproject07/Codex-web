import { describe, expect, it } from "vitest";
import { createHeartbeat } from "./heartbeat";

describe("terminal heartbeat", () => {
  it("expires a silent open connection and accepts incoming traffic as liveness", () => {
    const heartbeat = createHeartbeat(0);
    expect(heartbeat.expired(20_000, true)).toBe(false);
    heartbeat.received(25_000);
    expect(heartbeat.expired(40_000, true)).toBe(false);
    expect(heartbeat.expired(60_000, true)).toBe(false);
    expect(heartbeat.expired(80_000, true)).toBe(false);
    expect(heartbeat.expired(100_000, true)).toBe(true);
  });
  it("gives a suspended or hidden page a fresh grace period", () => {
    const heartbeat = createHeartbeat(0);
    expect(heartbeat.expired(120_000, true)).toBe(false);
    expect(heartbeat.expired(140_000, false)).toBe(false);
    expect(heartbeat.expired(160_000, true)).toBe(false);
    expect(heartbeat.expired(180_000, true)).toBe(false);
    expect(heartbeat.expired(200_000, true)).toBe(true);
  });
});
