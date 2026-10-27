import { useEffect, useRef, useState } from "react";
import { Terminal } from "@xterm/xterm";
import { parseGuestServerControl, type GuestControl } from "./protocol";

export function ReadOnlyTerminal({ credential, active = true }: { credential: string; active?: boolean }) {
  const container = useRef<HTMLDivElement>(null);
  const activeView = useRef(active);
  const updatePresence = useRef<() => void>(() => {});
  useEffect(() => { activeView.current = active; updatePresence.current(); }, [active]);
  const [status, setStatus] = useState("Connecting…");
  useEffect(() => {
    if (!container.current) return;
    const terminal = new Terminal({
      disableStdin: true, convertEol: false, scrollback: 20_000, cols: 120, rows: 35,
      fontFamily: '"Cascadia Mono", Consolas, monospace', fontSize: 13,
      theme: { background: "#10151d", foreground: "#e4e9f0" },
    });
    terminal.open(container.current);
    let disposed = false;
    let socket: WebSocket | null = null;
    let timer: ReturnType<typeof setTimeout> | undefined;
    let failures = 0;
    const connect = () => {
      if (disposed) return;
      setStatus("Connecting…");
      const url = new URL("/ws/share", window.location.href);
      url.protocol = url.protocol === "https:" ? "wss:" : "ws:";
      const ws = new WebSocket(url);
      socket = ws;
      ws.binaryType = "arraybuffer";
      let replay: Uint8Array[] | null = null;
      let replayBytes = 0;
      let lastMessage = Date.now();
      const send = (value: GuestControl | { credential: string }) => {
        if (socket === ws && ws.readyState === WebSocket.OPEN) ws.send(JSON.stringify(value));
      };
      const visible = () => send({ type: "presence", visible: activeView.current && document.visibilityState === "visible" });
      updatePresence.current = visible;
      const write = (data: Uint8Array) => terminal.write(data, () => {
        if (!disposed) send({ type: "output_ack", bytes: data.byteLength });
      });
      ws.onopen = () => { send({ credential }); visible(); };
      ws.onmessage = (event: MessageEvent<string | ArrayBuffer>) => {
        if (disposed || socket !== ws) return;
        lastMessage = Date.now();
        if (event.data instanceof ArrayBuffer) {
          const bytes = new Uint8Array(event.data);
          if (replay) {
            replayBytes += bytes.byteLength;
            if (replayBytes > 4 * 1024 * 1024) { ws.close(); return; }
            replay.push(bytes);
          } else write(bytes);
          return;
        }
        const message = parseGuestServerControl(event.data);
        if (!message) { ws.close(); return; }
        if (message.type === "shared_session") {
          terminal.resize(message.cols, message.rows);
        } else if (message.type === "replay_start") {
          replay = []; replayBytes = 0; terminal.reset(); setStatus("Loading recent history…");
        } else if (message.type === "replay_end") {
          for (const data of replay ?? []) write(data);
          replay = null; failures = 0; setStatus("Live · viewing only");
        }
      };
      document.addEventListener("visibilitychange", visible);
      const heartbeat = window.setInterval(() => {
        if (document.visibilityState === "visible" && Date.now() - lastMessage > 65_000) { ws.close(); return; }
        send({ type: "ping" }); visible();
      }, 20_000);
      ws.onclose = () => {
        window.clearInterval(heartbeat);
        document.removeEventListener("visibilitychange", visible);
        if (disposed || socket !== ws) return;
        setStatus("Disconnected · checking access");
        if (++failures <= 5) timer = setTimeout(connect, Math.min(30_000, 1000 * 2 ** failures));
      };
      ws.onerror = () => ws.close();
    };
    connect();
    return () => { disposed = true; clearTimeout(timer); socket?.close(); terminal.dispose(); };
  }, [credential]);
  return <section className="shared-terminal" aria-label="Shared terminal">
    <div className="share-connection" role="status">{status}</div>
    <div ref={container} className="share-terminal-scroll" />
  </section>;
}
