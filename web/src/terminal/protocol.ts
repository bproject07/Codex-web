export type ClientControl =
  | { type: "resize"; cols: number; rows: number }
  | { type: "ping" }
  | { type: "restart" };

export type ServerControl =
  | {
      type: "session";
      session: import("../api").SessionSnapshot;
    }
  | { type: "replay_start"; sessionId: string | null }
  | { type: "replay_end"; lastSequence: number }
  | { type: "pong" }
  | { type: "error"; code: string; message: string };

const textEncoder = new TextEncoder();

export function encodeControlMessage(message: ClientControl): string {
  return JSON.stringify(message);
}

export function encodeTerminalInput(input: string): Uint8Array<ArrayBuffer> {
  return textEncoder.encode(input);
}

export function parseServerControl(value: string): ServerControl | null {
  try {
    const parsed = JSON.parse(value) as { type?: unknown };
    if (typeof parsed !== "object" || parsed === null || typeof parsed.type !== "string") {
      return null;
    }

    if (
      parsed.type === "session" ||
      parsed.type === "replay_start" ||
      parsed.type === "replay_end" ||
      parsed.type === "pong" ||
      parsed.type === "error"
    ) {
      return parsed as ServerControl;
    }
  } catch {
    return null;
  }

  return null;
}
