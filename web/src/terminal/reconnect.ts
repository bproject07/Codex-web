export type ConnectionStatus =
  | "connecting"
  | "connected"
  | "reconnecting"
  | "disconnected"
  | "authentication_failed";

export type ConnectionEvent =
  | { type: "connect_started"; retry: boolean }
  | { type: "opened" }
  | { type: "closed" }
  | { type: "retry_scheduled" }
  | { type: "authentication_rejected" };

const RECONNECT_DELAYS = [1_000, 2_000, 4_000, 8_000, 15_000] as const;

export function reconnectDelay(attempt: number): number {
  const index = Math.min(Math.max(attempt, 0), RECONNECT_DELAYS.length - 1);
  return RECONNECT_DELAYS[index];
}

export function reduceConnectionStatus(
  current: ConnectionStatus,
  event: ConnectionEvent,
): ConnectionStatus {
  switch (event.type) {
    case "connect_started":
      return event.retry ? "reconnecting" : "connecting";
    case "opened":
      return "connected";
    case "closed":
      return current === "authentication_failed" ? current : "disconnected";
    case "retry_scheduled":
      return "reconnecting";
    case "authentication_rejected":
      return "authentication_failed";
  }
}

