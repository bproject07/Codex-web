import { useEffect, useRef, useState } from "react";
import { Terminal } from "@xterm/xterm";
import { request, displayError, proposalState, type Generation, type ArchiveEvent, type Proposal } from "./api";

export function Archive({ token, guest = false }: { token: string; guest?: boolean }) {
  const [generations, setGenerations] = useState<Generation[]>([]);
  const [selected, setSelected] = useState("");
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);
  const [cursor, setCursor] = useState(0);
  const [proposals, setProposals] = useState<Proposal[]>([]);
  const root = useRef<HTMLDivElement>(null);
  const terminal = useRef<Terminal | null>(null);
  const epoch = useRef(0);
  const cursorRef = useRef(0);
  const playing = useRef(false);
  const base = guest ? "/api/share/archives" : "/api/sharing/archives";
  const refresh = () => request<{ generations: Generation[] }>(base, token)
    .then((data) => { setGenerations(data.generations); setSelected((id) =>
      data.generations.some((item) => item.id === id) ? id : data.generations[0]?.id || ""); })
    .catch((err: unknown) => setError(displayError(err)));
  useEffect(() => { void refresh(); }, [token, base]);
  useEffect(() => {
    setProposals([]);
    if (guest || !selected) return;
    const controller = new AbortController();
    void request<{ proposals: Proposal[] }>(base + "/" + selected + "/proposals", token, "GET", undefined, controller.signal)
      .then((result) => { if (!controller.signal.aborted) setProposals(result.proposals); })
      .catch((err: unknown) => { if (!controller.signal.aborted) setError(displayError(err)); });
    return () => controller.abort();
  }, [guest, selected, token, base]);
  useEffect(() => {
    if (!root.current || !selected) return;
    const generation = generations.find((item) => item.id === selected);
    const view = new Terminal({
      disableStdin: true, convertEol: false, scrollback: 50_000,
      cols: generation?.cols || 120, rows: generation?.rows || 35, fontSize: 13,
      theme: { background: "#10151d", foreground: "#e4e9f0" },
    });
    view.open(root.current); terminal.current = view; cursorRef.current = 0; setCursor(0); setBusy(false);
    epoch.current++;
    return () => { epoch.current++; playing.current = false; terminal.current = null; view.dispose(); };
  }, [selected]);
  const load = async (all = false) => {
    if (busy || !selected || !terminal.current) return;
    const version = epoch.current;
    setBusy(true); setError(""); playing.current = all;
    try {
      do {
      const result = await request<{ events: ArchiveEvent[] }>(base + "/" + selected + "?after=" + cursorRef.current, token);
      if (version !== epoch.current || !terminal.current) return;
      for (const event of result.events) {
        if (event.sequence !== cursorRef.current + 1) setError("This recording contains a gap.");
        if ((event.kind === "start" || event.kind === "resize") &&
          event.cols >= 20 && event.cols <= 500 && event.rows >= 5 && event.rows <= 300) {
          terminal.current.resize(event.cols, event.rows);
        }
        if (event.kind === "output") {
          const data = Uint8Array.from(atob(event.data), (char) => char.charCodeAt(0));
          await new Promise<void>((resolve) => terminal.current?.write(data, resolve));
          if (version !== epoch.current || !terminal.current) return;
        }
        cursorRef.current = event.sequence;
      }
      setCursor(cursorRef.current);
      if (!result.events.length) {
        setError("You have reached the current end of the recording.");
        break;
      }
      } while (playing.current && version === epoch.current);
    } catch (err) { if (version === epoch.current) setError(displayError(err)); }
    finally { if (version === epoch.current) setBusy(false); }
  };
  const generation = generations.find((item) => item.id === selected);
  return <section className="share-archive">
    <h3>Recorded history</h3>
    <p>Play the recorded terminal from the beginning. This does not send input to the agent.</p>
    <div className="share-actions">
      <label>Recording <select value={selected} onChange={(event) => setSelected(event.target.value)}>
        <option value="">Choose a recording</option>
        {generations.map((item) => <option key={item.id} value={item.id}>
          {item.label || item.agent} · {new Date(item.createdAt).toLocaleString()}
        </option>)}
      </select></label>
      <button onClick={() => void refresh()}>Refresh recordings</button>
      <button disabled={busy || !selected} onClick={() => void load()}>{busy ? "Loading…" : cursor ? "Play next part" : "Play from beginning"}</button>
      <button disabled={busy || !selected} onClick={() => void load(true)}>Play to end</button>
      {busy && <button onClick={() => { playing.current = false; }}>Pause</button>}
      <button disabled={busy || !cursor} onClick={() => {
        terminal.current?.reset(); cursorRef.current = 0; setCursor(0); setError("");
      }}>Rewind</button>
      {!guest && generation?.ended && <button disabled={busy} onClick={() => {
        if (!window.confirm("Permanently delete this recording? This cannot be undone.")) return;
        void request(base + "/" + selected, token, "DELETE")
          .then(() => refresh()).catch((err: unknown) => setError(displayError(err)));
      }}>Delete recording</button>}
    </div>
    {generation?.incomplete && <p role="status">The recording is incomplete or started after the terminal was launched.</p>}
    {error && <p role="status">{error}</p>}
    {!generations.length && <p>No recorded sessions yet.</p>}
    <div ref={root} className="share-terminal-scroll" />
    {!!proposals.length && <section aria-label="Recorded proposals">
      <h3>Proposal history</h3>
      {proposals.map((proposal) => <article className="share-proposal" key={proposal.id}>
        <strong>{proposal.author}</strong> · {proposalState[proposal.state] ?? proposal.state}
        <details><summary>Original proposal</summary><pre>{proposal.original}</pre></details>
        {proposal.approvedText && <><p>Reviewed text</p><pre>{proposal.approvedText}</pre></>}
      </article>)}
    </section>}
  </section>;
}
