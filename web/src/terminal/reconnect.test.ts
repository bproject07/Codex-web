import { describe, expect, it } from "vitest";
import { reconnectDelay, reduceConnectionStatus } from "./reconnect";

describe("reconnect state", () => {
  it("uses capped exponential backoff", () => {
    expect([0, 1, 2, 3, 4, 5, 20].map(reconnectDelay)).toEqual([
      1_000, 2_000, 4_000, 8_000, 15_000, 15_000, 15_000,
    ]);
  });

  it("moves through disconnect, retry, and open states", () => {
    let status = reduceConnectionStatus("connected", { type: "closed" });
    expect(status).toBe("disconnected");
    status = reduceConnectionStatus(status, { type: "retry_scheduled" });
    expect(status).toBe("reconnecting");
    status = reduceConnectionStatus(status, { type: "opened" });
    expect(status).toBe("connected");
  });

  it("does not retry through an authentication failure", () => {
    const rejected = reduceConnectionStatus("connecting", {
      type: "authentication_rejected",
    });
    expect(reduceConnectionStatus(rejected, { type: "closed" })).toBe(
      "authentication_failed",
    );
  });
});

