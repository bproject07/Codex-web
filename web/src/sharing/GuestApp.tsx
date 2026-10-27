import { useEffect, useRef, useState, type FormEvent } from "react";
import { Archive } from "./Archive";
import { ReadOnlyTerminal } from "./ReadOnlyTerminal";
import { request, displayError, newCredential, newId, proposalState, SharingError, validName, validProposal, textBytes, TEXT_LIMIT, type Guest, type GuestView } from "./api";

const KEY = "cwt-guest-access";
interface Access { credential: string; invitation?: string; previousCredential?: string; name?: string }
let initialAccess: Access | undefined;
function loadAccess(): Access {
  if (initialAccess) return initialAccess;
  const url = new URL(window.location.href);
  const invitation = new URLSearchParams(url.hash.slice(1)).get("invite");
  if (invitation) {
    let previous: Access | undefined;
    try { previous = JSON.parse(sessionStorage.getItem(KEY) || "null") as Access | undefined; } catch { /* Fresh visitor. */ }
    const access: Access = { credential: newCredential(), invitation,
      ...(previous?.credential && !previous.invitation ? { previousCredential: previous.credential, name: previous.name } : {}) };
    try { sessionStorage.setItem(KEY, JSON.stringify(access)); } catch { /* This page still owns its credential. */ }
    history.replaceState(null, "", url.pathname);
    return initialAccess = access;
  }
  try { return initialAccess = JSON.parse(sessionStorage.getItem(KEY) || '{"credential":""}') as Access; }
  catch { return initialAccess = { credential: "" }; }
}
export function GuestApp() {
  const [access, setAccess] = useState<Access>(loadAccess);
  const [name, setName] = useState(access.name ?? "");
  const [view, setView] = useState<GuestView | null>(null);
  const [error, setError] = useState("");
  const [ended, setEnded] = useState(false);
  const [busy, setBusy] = useState(false);
  const [text, setText] = useState("");
  const [archive, setArchive] = useState(false);
  const submission = useRef<{ text: string; id: string } | null>(null);
  const sending = useRef(false);
  useEffect(() => {
    if (!access.credential || access.invitation || ended) return;
    const controller = new AbortController();
    let timer: ReturnType<typeof setTimeout>;
    const poll = async () => {
      try {
        const snapshot = await request<GuestView>("/api/share/me", access.credential, "GET", undefined, controller.signal);
        if (!controller.signal.aborted) setView(snapshot);
      } catch (err) {
        if (controller.signal.aborted) return;
        if (err instanceof SharingError && (err.status === 401 || err.code === "generation_changed")) {
          setEnded(true); setView(null); setError(displayError(err)); return;
        }
        setError(displayError(err));
      }
      if (!controller.signal.aborted) timer = setTimeout(() => void poll(), 2000);
    };
    void poll();
    return () => { controller.abort(); clearTimeout(timer); };
  }, [access, ended]);
  const join = async (event: FormEvent) => {
    event.preventDefault();
    if (sending.current || !access.invitation) return;
    sending.current = true; setBusy(true); setError("");
    try {
      const joined = await request<Guest>("/api/share/redeem", "", "POST", { ...access, name });
      const accepted = { credential: access.credential, name: joined.name };
      try { sessionStorage.setItem(KEY, JSON.stringify(accepted)); } catch { /* Keep the in-memory grant. */ }
      setAccess(accepted);
    } catch (err) { setError(displayError(err)); }
    finally { sending.current = false; setBusy(false); }
  };
  const submit = async (event: FormEvent) => {
    event.preventDefault();
    if (sending.current) return;
    sending.current = true; setBusy(true); setError("");
    if (submission.current?.text !== text) submission.current = { text, id: newId() };
    try {
      await request("/api/share/proposals", access.credential, "POST", {
        requestId: submission.current.id, text,
      });
      submission.current = null; setText("");
      setView(await request<GuestView>("/api/share/me", access.credential));
    } catch (err) { setError(displayError(err)); }
    finally { sending.current = false; setBusy(false); }
  };
  const pending = view?.proposals.some((proposal) => proposal.state === "pending");
  return <main className="guest-app">
    <header className="share-heading"><h1>Shared terminal</h1>
      {view && <span>{view.guest.name} · {view.agent} · {view.status}</span>}
    </header>
    {error && <p className="share-error" role="alert">{error}</p>}
    {access.invitation ? <form className="share-join" onSubmit={(event) => void join(event)}>
      <h2>Join this session</h2>
      <label>Your name <input autoFocus value={name} required
        onChange={(event) => setName(event.target.value)} autoComplete="off" /></label>
      <p>Your name is fixed for this shared session. The owner must admit you before you can view it.</p>
      <p>{Array.from(name.trim()).length}/32 characters · letters, numbers, spaces, dashes or underscores.</p>
      <button disabled={busy || !validName(name)}>{busy ? "Joining…" : "Confirm name and request access"}</button>
    </form> : !access.credential ? <p>Open an invitation from the session owner.</p> :
      ended ? <p>Access has ended. Contact the owner for a new invitation.</p> :
      !view || !view.guest.admitted ? <p role="status">Waiting for the owner to admit you…</p> : <>
        <nav className="share-actions" aria-label="Shared session views">
          <button aria-pressed={!archive} onClick={() => setArchive(false)}>Live terminal</button>
          <button aria-pressed={archive} onClick={() => setArchive(true)}>Recorded history</button>
        </nav>
        {view.archiveIncomplete && <p role="status">Earlier history is incomplete. The recording began late or reached a storage limit.</p>}
        <div hidden={archive}><ReadOnlyTerminal credential={access.credential} active={!archive} /></div>
        {archive && <Archive token={access.credential} guest />}
        {view.guest.mode === "propose" && <form className="share-proposal" onSubmit={(event) => void submit(event)}>
          <h2>Propose an instruction</h2>
          <p>The owner sees your text and approves it before it reaches the terminal.</p>
          <label>Your proposal <textarea value={text} rows={4}
            onChange={(event) => setText(event.target.value)} /></label>
          <p>Multiline drafts can be reviewed. Sending currently requires a single line.</p>
          <p role="status">{textBytes(text)}/{TEXT_LIMIT} UTF-8 bytes{ textBytes(text) > TEXT_LIMIT ? " · Shorten the proposal to send it." : "" }</p>
          <button disabled={busy || pending || !validProposal(text) || view.status !== "running"}>
            {busy ? "Submitting…" : pending ? "A proposal is waiting for approval" : "Send for approval"}
          </button>
        </form>}
        <section aria-label="Your proposals" className="share-proposals">
          <h2>Your proposals</h2>
          {view.proposals.map((proposal) => <article className="share-proposal" key={proposal.id}>
            <strong>{proposal.author}</strong>
            <p>{proposalState[proposal.state] ?? proposal.state}</p>
            <pre>{proposal.original}</pre>
            {proposal.approvedText !== null && proposal.approvedText !== proposal.original && <>
              <p>Edited by the owner · approved text</p><pre>{proposal.approvedText}</pre>
            </>}
          </article>)}
        </section>
      </>}
  </main>;
}
