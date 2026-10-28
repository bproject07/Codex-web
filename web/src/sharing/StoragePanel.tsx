import { useEffect, useRef, useState } from "react";
import { request, displayError, SharingError, type StorageUsage, type StoragePolicy,
  type CleanupFilter, type CleanupPreview, type CompactionResult, type Generation } from "./api";

export function storageBytes(bytes: number): string {
  if (!Number.isFinite(bytes) || bytes < 0) return "Unknown";
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KiB`;
  if (bytes < 1024 * 1024 * 1024) return `${(bytes / (1024 * 1024)).toFixed(1)} MiB`;
  return `${(bytes / (1024 * 1024 * 1024)).toFixed(2)} GiB`;
}

export function StoragePanel({ token, onRepaired }: { token: string; onRepaired?: () => void }) {
  const [usage, setUsage] = useState<StorageUsage | null>(null);
  const [recordings, setRecordings] = useState<Generation[]>([]);
  const [policy, setPolicy] = useState<StoragePolicy | null>(null);
  const [age, setAge] = useState("all");
  const [selected, setSelected] = useState<string[]>([]);
  const [preview, setPreview] = useState<CleanupPreview | null>(null);
  const [result, setResult] = useState<CompactionResult | null>(null);
  const [message, setMessage] = useState("");
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);
  const action = useRef(false);
  const mounted = useRef(true);
  const base = "/api/sharing/storage";
  const filter: CleanupFilter = { ...(age === "all" ? {} : { olderThanDays: Number(age) }),
    ...(selected.length ? { generations: selected } : {}) };
  useEffect(() => {
    mounted.current = true;
    const controller = new AbortController();
    let timer: ReturnType<typeof setTimeout>;
    const refresh = async () => {
      try {
        const value = await request<StorageUsage>(base, token, "GET", undefined, controller.signal);
        if (controller.signal.aborted) return;
        setUsage(value);
        if (value.available) {
          setPolicy((current) => current ?? value.policy);
          const archive = await request<{ generations: Generation[] }>("/api/sharing/archives", token, "GET", undefined, controller.signal);
          if (!controller.signal.aborted) setRecordings(archive.generations.filter((item) => item.ended));
        }
      } catch (err) { if (!controller.signal.aborted && !action.current) setError(displayError(err)); }
      if (!controller.signal.aborted) timer = setTimeout(() => void refresh(), 2000);
    };
    void refresh();
    return () => { mounted.current = false; controller.abort(); clearTimeout(timer); };
  }, [token]);
  const run = async (operation: () => Promise<void>) => {
    if (action.current) return;
    action.current = true; setBusy(true); setError(""); setMessage("");
    try { await operation(); }
    catch (err) { if (mounted.current) setError(displayError(err)); }
    finally { action.current = false; if (mounted.current) setBusy(false); }
  };
  return <section className="sharing-storage" aria-label="Sharing storage">
    <h3>Storage</h3>
    <p>Manage recorded terminal history for this server. Cleanup permanently removes selected recordings.</p>
    {!usage && <p role="status">Loading storage…</p>}
    {usage && !usage.available && <>
      <p role="alert">{new SharingError(usage.code ?? "storage_unavailable", 503).message}</p>
      {usage.canRepair && <>
        <p>Repair limits access to the server's application data folder and sharing database files to the server account, Administrators and SYSTEM. Existing content is preserved.</p>
        <button disabled={busy || usage.repairing} onClick={() => {
          if (!window.confirm("Restrict this server's application data folder and sharing database files to the server account, Administrators and SYSTEM? Existing files and recordings will be kept.")) return;
          void run(async () => { await request(base + "/repair", token, "POST", { confirm: true }); onRepaired?.(); setMessage("Permissions repaired. Sharing is ready; no server restart is required."); });
        }}>Repair sharing permissions</button>
      </>}
    </>}
    {usage?.available && <>
      <dl className="storage-sizes">
        <dt>Total on disk</dt><dd>{storageBytes(usage.diskBytes)}</dd>
        <dt>Database file</dt><dd>{storageBytes(usage.databaseBytes)}</dd>
        <dt>Pending writes file</dt><dd>{storageBytes(usage.walBytes)}</dd>
        <dt>Recorded output</dt><dd>{storageBytes(usage.archiveBytes)} in {usage.recordings} recordings</dd>
        <dt>Reusable space inside database</dt><dd>{storageBytes(usage.reusableBytes)}</dd>
        <dt>Free disk space</dt><dd>{storageBytes(usage.freeDiskBytes)}</dd>
      </dl>
      {usage.cleanupPending > 0 && <p role="status">Deleting {usage.cleanupPending} recordings in small batches…</p>}
      <h4>Clean up recordings</h4>
      <div className="share-actions">
        <label>Finished recordings <select value={age} disabled={busy} onChange={(event) => { setAge(event.target.value); setPreview(null); }}>
          <option value="all">All finished recordings</option><option value="7">Finished over 7 days ago</option>
          <option value="30">Finished over 30 days ago</option><option value="90">Finished over 90 days ago</option>
        </select></label>
        <label>Specific recordings (optional)<select multiple size={4} value={selected} disabled={busy}
          onChange={(event) => { setSelected(Array.from(event.target.selectedOptions, (option) => option.value)); setPreview(null); }}>
          {recordings.map((item) => <option key={item.id} value={item.id}>{item.label || item.agent} · {new Date(item.createdAt).toLocaleString()} · {storageBytes(item.bytes)}</option>)}
        </select></label>
        {selected.length > 0 && <button disabled={busy} onClick={() => { setSelected([]); setPreview(null); }}>Clear selection</button>}
        <button disabled={busy || usage.maintenance} onClick={() => void run(async () => { setPreview(await request<CleanupPreview>(base + "/cleanup/preview", token, "POST", filter)); })}>Preview cleanup</button>
      </div>
      <p>Active recordings and deliveries awaiting a receipt are protected. Guest identities and proposal audit remain available.</p>
      {preview && <div>
        <p>{preview.count} recordings · {storageBytes(preview.bytes)} of recorded output will be deleted.</p>
        <button disabled={busy || preview.count === 0} onClick={() => {
          if (!window.confirm(`Permanently delete ${preview.count} finished recordings (${storageBytes(preview.bytes)})? This cannot be undone.`)) return;
          void run(async () => { await request(base + "/cleanup", token, "POST", { filter, fingerprint: preview.fingerprint }); setPreview(null); setSelected([]); setMessage("Cleanup queued. You can close this dialog while it finishes."); });
        }}>Delete previewed recordings</button>
      </div>}
      <h4>Reclaim disk space</h4>
      <p>Remove unused space from the database file while preserving remaining recordings. Stop recording on all terminals first; the terminals can keep running. Sharing operations are briefly unavailable during compaction.</p>
      {usage.recordingEnabledCount > 0 && <p>{usage.recordingEnabledCount} terminals still have recording enabled.</p>}
      {!usage.incrementalCompaction && <p>This older database needs a one-time conversion for incremental space recovery. Allow at least {storageBytes(usage.databaseBytes * 2 + 64 * 1024 * 1024)} of free disk space.</p>}
      <button disabled={busy || usage.maintenance || usage.recordingEnabledCount > 0 || usage.cleanupPending > 0} onClick={() => {
        void run(async () => { setResult(await request<CompactionResult>(base + "/compact", token, "POST", { confirm: true })); });
      }}>Reclaim disk space</button>
      {result && <p role="status">Reclaimed {storageBytes(result.reclaimedBytes)}: {storageBytes(result.beforeBytes)} → {storageBytes(result.afterBytes)} in {(result.elapsedMs / 1000).toFixed(2)} seconds.</p>}
      {policy && <>
        <h4>Automatic cleanup</h4>
        <label><input type="checkbox" checked={policy.enabled} disabled={busy} onChange={(event) => setPolicy({ ...policy, enabled: event.target.checked })} />Enable automatic cleanup of finished recordings</label>
        <div className="share-actions">
          <label>Keep for days<input type="number" min={1} max={3650} value={policy.retentionDays} disabled={busy} onChange={(event) => setPolicy({ ...policy, retentionDays: Number(event.target.value) })} /></label>
          <label>Recording budget (MiB)<input type="number" min={16} max={2048} value={policy.maxBytes / (1024 * 1024)} disabled={busy} onChange={(event) => setPolicy({ ...policy, maxBytes: Number(event.target.value) * 1024 * 1024 })} /></label>
          <button disabled={busy} onClick={() => {
            if (policy.enabled && !window.confirm("Apply automatic cleanup now and on future server runs? Finished recordings exceeding the age or recording budget will be permanently deleted, oldest first. Active recordings are protected.")) return;
            void run(async () => { await request(base + "/policy", token, "POST", policy); setMessage("Storage policy saved."); });
          }}>Save cleanup policy</button>
        </div>
        <p>The budget covers recorded output, including active recordings; database overhead is additional. Active recordings may keep usage above the budget. Automatic cleanup never deletes guest identities or proposal audit.</p>
      </>}
    </>}
    {busy && <p role="status">Working… Terminal sessions continue running.</p>}
    {message && <p role="status">{message}</p>}
    {error && <p role="alert">{error}</p>}
  </section>;
}
