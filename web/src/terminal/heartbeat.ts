export function createHeartbeat(now: number) {
  let lastReceived = now;
  let lastCheck = now;
  return {
    received(at: number) { lastReceived = at; },
    expired(at: number, visible: boolean) {
      // A suspended/background page must get a fresh grace period on resume.
      if (!visible || at - lastCheck > 40_000) lastReceived = at;
      lastCheck = at;
      return at - lastReceived >= 60_000;
    },
  };
}
