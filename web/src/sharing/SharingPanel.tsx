import { useCallback, useEffect, useRef, useState } from "react";
import { Archive } from "./Archive";
import { request, displayError, newId, proposalState, singleLine, validProposal, textBytes, TEXT_LIMIT, SharingError, type OwnerView, type Proposal } from "./api";

export function SharingPanel({ token, terminalId, label, onClose }: {
  token: string; terminalId: string; label: string; onClose: () => void;
}) {
  const dialog = useRef<HTMLDialogElement>(null);
  const [view, setView] = useState<OwnerView | null>(null);
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);
  const [mode, setMode] = useState("propose");
  const [link, setLink] = useState("");
  const [address, setAddress] = useState(window.location.origin);
  const [history, setHistory] = useState(false);
  const epoch = useRef(0);
  const action = useRef(false);
  const base = "/api/sessions/" + terminalId + "/sharing";
  const refresh = useCallback(async () => {
    const version = ++epoch.current;
    const snapshot = await request<OwnerView>(base, token);
    if (version === epoch.current) setView(snapshot);
  }, [base, token]);
  useEffect(() => {
    dialog.current?.showModal();
    const previous = document.activeElement;
    return () => { if (previous instanceof HTMLElement) previous.focus(); };
  }, []);
  useEffect(() => {
    let disposed = false;
    let timer: ReturnType<typeof setTimeout>;
    const poll = async () => {
      try { await refresh(); } catch (err) { if (!disposed) setError(displayError(err)); }
      if (!disposed) timer = setTimeout(() => void poll(), 2000);
    };
    void poll();
    return () => { disposed = true; epoch.current++; clearTimeout(timer); };
  }, [refresh]);
  const mutate = async (suffix: string, method = "POST", body?: unknown) => {
    if (action.current) return;
    action.current = true; setBusy(true); setError(""); epoch.current++;
    try {
      const origin = new URL(suffix === "/invitations" ? address : window.location.origin);
      if (!["http:", "https:"].includes(origin.protocol) || origin.username || origin.password ||
        origin.pathname !== "/" || origin.search || origin.hash) {
        throw new Error("Enter the server's reachable HTTP or HTTPS address and port.");
      }
      const result = await request<{ invitation?: string }>(base + suffix, token, method, body);
      if (result?.invitation) {
        const url = new URL("/share", origin.origin);
        url.hash = new URLSearchParams({ invite: result.invitation }).toString();
        setLink(url.href);
      }
      await refresh();
    } catch (err) { setError(displayError(err)); }
    finally { action.current = false; setBusy(false); }
  };
  return <dialog ref={dialog} className="sharing-panel" aria-labelledby="sharing-title"
    onCancel={(event) => { event.preventDefault(); onClose(); }}>
    <header className="share-heading"><h2 id="sharing-title">Share · {label}</h2>
      <button type="button" autoFocus aria-label="Close sharing" onClick={onClose}>Close</button></header>
    <nav className="share-actions" aria-label="Sharing views">
      <button aria-pressed={!history} onClick={() => setHistory(false)}>Guests and proposals</button>
      <button aria-pressed={history} onClick={() => setHistory(true)}>Recorded history</button>
    </nav>
    {error && <p className="share-error" role="alert">{error}</p>}
    {history ? <Archive token={token} /> : <>
      <p>Guests see this terminal and its recorded history. Creating an invitation records terminal output on this server.
        A terminal restart ends guest access; saved history remains available to you. Recording continues across restarts until you stop it.</p>
      {view?.archiveIncomplete && <p role="status">The recording is incomplete or began after this terminal started.</p>}
      <div className="share-actions">
        <label>Address guests can reach <input value={address} type="url"
          placeholder="http://10.0.0.1:7681" onChange={(event) => setAddress(event.target.value)} /></label>
        <label>Guest permissions <select value={mode} onChange={(event) => setMode(event.target.value)}>
          <option value="propose">View and propose instructions</option><option value="view">View only</option>
        </select></label>
        <button disabled={busy || !view?.sessionId} onClick={() => void mutate("/invitations", "POST", { mode })}>Create individual invitation</button>
        {!view?.recording && <button disabled={busy} onClick={() => void mutate("/record")}>Record this terminal</button>}
        {view?.recording && <button disabled={busy} onClick={() => void mutate("/record", "DELETE")}>Stop recording</button>}
        <button disabled={busy || !view?.sessionId} onClick={() => { setLink(""); void mutate("", "DELETE"); }}>End all guest access</button>
      </div>
      <p>Stopping recording keeps saved history and live guest access. Resuming leaves a gap for output produced while recording was stopped.</p>
      {view?.usage && <details><summary>Sharing storage limits</summary>
        <p>Invitations: {view.usage.invitations}; proposals: {view.usage.proposals}; approval attempts: {view.usage.approvals}. Limit: {view.usage.limit} each.</p>
        <p>Cleanup permanently removes guest identities and proposal audit for deleted terminals and sessions from previous server runs. Recordings remain. Active terminal identities are retained to prevent duplicate sends.</p>
        <button disabled={busy} onClick={() => {
          if (!window.confirm("Permanently remove sharing identities and proposal audit for retired sessions? Recordings will remain.")) return;
          void request("/api/sharing/prune-retired", token, "POST").then(refresh).catch((err: unknown) => setError(displayError(err)));
        }}>Clean up retired session sharing data</button>
      </details>}
      {link && <label className="share-link">Invitation · valid for 15 minutes
        <input readOnly value={link} onFocus={(event) => event.target.select()} />
        <button onClick={() => { void navigator.clipboard?.writeText(link).catch(() => setError("Select and copy the invitation link.")); }}>Copy invitation</button>
      </label>}
      <section><h3>Guests</h3>
        {!view?.guests.length && <p>No guests yet. Each invitation admits one person.</p>}
        {view?.guests.map((guest) => <article className="share-guest" key={guest.grantId}>
          <div><strong>{guest.name}</strong><span> · {guest.revoked || guest.expiresAt <= Date.now() ? "Access ended" :
            guest.admitted ? guest.presence : "Waiting for admission"}</span>
            <small>{guest.mode === "view" ? "View only" : "View and propose"}</small></div>
          {!guest.revoked && guest.expiresAt > Date.now() && <div className="share-actions">
            {!guest.admitted && <button disabled={busy} onClick={() => void mutate("/guests/" + guest.grantId + "/admit")}>Admit {guest.name}</button>}
            <button disabled={busy} onClick={() => void mutate("/guests/" + guest.grantId, "DELETE")}>Revoke</button>
          </div>}
        </article>)}
      </section>
      <section><h3>Proposals</h3><p>Approval sends the reviewed instruction using the agent's current permissions.</p>
        {!view?.proposals.length && <p>No proposals yet.</p>}
        {view?.proposals.map((proposal) => <ReviewProposal key={proposal.id + ":" + proposal.revision}
          proposal={proposal} token={token} base={base} refresh={refresh} />)}
      </section>
    </>}
  </dialog>;
}
function ReviewProposal({ proposal, token, base, refresh }: {
  proposal: Proposal; token: string; base: string; refresh: () => Promise<void>;
}) {
  const [text, setText] = useState(proposal.text ?? "");
  const [ready, setReady] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const inFlight = useRef(false);
  const attempt = useRef<{ key: string; id: string } | null>(null);
  const decide = async (approve: boolean) => {
    if (inFlight.current) return;
    inFlight.current = true; setBusy(true); setError("");
    const key = JSON.stringify([proposal.revision, proposal.sessionId, text]);
    if (attempt.current?.key !== key) attempt.current = { key, id: newId() };
    try {
      await request(base + "/proposals/" + proposal.id + (approve ? "/approve" : "/reject"), token, "POST",
        approve ? { requestId: attempt.current.id, sessionId: proposal.sessionId, revision: proposal.revision, text, ready } :
          { revision: proposal.revision });
      attempt.current = null;
      await refresh();
    } catch (err) { setError(displayError(err)); }
    finally { inFlight.current = false; setBusy(false); setReady(false); }
  };
  return <article className="share-proposal">
    <strong>{proposal.author}</strong> · <span>{proposalState[proposal.state] ?? proposal.state}</span>
    <details><summary>Original proposal</summary><pre>{proposal.original}</pre></details>
    {proposal.state === "pending" ? <>
      <label>Text to send <textarea value={text} rows={3} disabled={busy}
        onChange={(event) => { setText(event.target.value); setReady(false); }} /></label>
      {!singleLine(text) && <p>Sending currently supports one line. Edit the text before approval.</p>}
      <p role="status">{textBytes(text)}/{TEXT_LIMIT} UTF-8 bytes{ textBytes(text) > TEXT_LIMIT ? " · Shorten the instruction to send it." : "" }</p>
      <label className="share-check"><input type="checkbox" checked={ready} disabled={busy}
        onChange={(event) => setReady(event.target.checked)} /> The agent is waiting at an empty prompt.</label>
      <div className="share-actions">
        <button disabled={busy || !ready || !validProposal(text) || !singleLine(text)} onClick={() => void decide(true)}>Approve and send</button>
        <button disabled={busy} onClick={() => void decide(false)}>Reject</button>
      </div>
    </> : <pre>{proposal.approvedText ?? proposal.text}</pre>}
    {proposal.approvedText && proposal.approvedText !== proposal.original && <small>Edited by the owner</small>}
    {(error || proposal.reason) && <p className="share-error" role="status">{error || proposal.reason?.replaceAll("_", " ")}</p>}
  </article>;
}

export function SharingIndicator({ token, terminalId, onOpen }: { token: string; terminalId: string; onOpen: () => void }) {
  const [notice, setNotice] = useState("");
  useEffect(() => {
    let cancelled = false;
    let timer: ReturnType<typeof setTimeout>;
    setNotice("");
    const poll = async () => {
      let delay = 30_000;
      try {
        const view = await request<{ available: boolean; guests: number; pending: number }>("/api/sessions/" + terminalId + "/sharing/status", token);
        if (!view.available) return;
        if (!cancelled) {
          const { pending, guests } = view;
          if (guests || pending) delay = 4000;
          setNotice(guests || pending ? guests + " guests · " + pending + " proposals" : "");
        }
      } catch (err) { if (err instanceof SharingError && err.status === 401) return; }
      if (!cancelled) timer = setTimeout(() => void poll(), delay);
    };
    void poll();
    return () => { cancelled = true; clearTimeout(timer); };
  }, [token, terminalId]);
  return notice ? <button className="sharing-indicator" onClick={onOpen}>{notice}</button> : null;
}
