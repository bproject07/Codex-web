export type GuestControl = { type: "ping" } | { type: "output_ack"; bytes: number } |
  { type: "presence"; visible: boolean };
export type GuestServerControl =
  | { type: "shared_session"; sessionId: string; agent: string; status: string; cols: number; rows: number; archiveIncomplete: boolean }
  | { type: "replay_start"; sessionId: string | null }
  | { type: "replay_end"; lastSequence: number }
  | { type: "flow_control"; windowBytes: number }
  | { type: "pong" }
  | { type: "error"; code: string; message: string };
export function parseGuestServerControl(text: string): GuestServerControl | null {
  if (text.length > 4096) return null;
  try {
    const v: unknown = JSON.parse(text);
    if (!v || typeof v !== "object" || Array.isArray(v)) return null;
    const x = v as Record<string, unknown>;
    const integer = (value: unknown, min: number, max: number) => typeof value === "number" && Number.isSafeInteger(value) && value >= min && value <= max;
    switch (x.type) {
      case "shared_session":
        if (typeof x.sessionId !== "string" || typeof x.agent !== "string" || typeof x.status !== "string" ||
          typeof x.archiveIncomplete !== "boolean" || !integer(x.cols, 20, 500) || !integer(x.rows, 5, 300)) return null;
        break;
      case "replay_start": if (x.sessionId !== null && typeof x.sessionId !== "string") return null; break;
      case "replay_end": if (!integer(x.lastSequence, 0, Number.MAX_SAFE_INTEGER)) return null; break;
      case "flow_control": if (!integer(x.windowBytes, 1, 4 * 1024 * 1024)) return null; break;
      case "pong": break;
      case "error": if (typeof x.code !== "string" || typeof x.message !== "string") return null; break;
      default: return null;
    }
    return x as GuestServerControl;
  } catch { return null; }
}
