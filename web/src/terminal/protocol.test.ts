import { describe, expect, it } from "vitest";
import {
  encodeControlMessage,
  encodeTerminalInput,
  parseServerControl,
} from "./protocol";

describe("terminal protocol", () => {
  it("encodes resize control messages as JSON text", () => {
    expect(
      encodeControlMessage({ type: "resize", cols: 120, rows: 35 }),
    ).toBe('{"type":"resize","cols":120,"rows":35}');
  });

  it("encodes Unicode terminal input as UTF-8 binary", () => {
    expect(Array.from(encodeTerminalInput("Зд"))).toEqual([
      208, 151, 208, 180,
    ]);
  });

  it("rejects damaged server control messages", () => {
    expect(parseServerControl("{")).toBeNull();
    expect(parseServerControl('{"unexpected":true}')).toBeNull();
  });
});

