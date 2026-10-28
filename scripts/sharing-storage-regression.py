#!/usr/bin/env python3
"""Measure real SQLite space recovery and native PTY responsiveness after upgrade."""
from __future__ import annotations

import argparse
from concurrent.futures import ThreadPoolExecutor
from contextlib import closing
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import sqlite3
import statistics
import subprocess
import sys
import tempfile
import time
import uuid

spec = importlib.util.spec_from_file_location("storage_sharing_fixture", Path(__file__).with_name("sharing-regression.py"))
assert spec and spec.loader
sharing = importlib.util.module_from_spec(spec)
sys.modules[spec.name] = sharing
spec.loader.exec_module(sharing)
peer = sharing.peer
TOKEN = "synthetic-storage-owner-token"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--server", type=Path, required=True)
    parser.add_argument("--port", type=int, default=8830)
    parser.add_argument("--browser", action="store_true")
    parser.add_argument("--report", type=Path)
    args = parser.parse_args()
    peer.assert_isolated_port(args.port)
    metrics = {}
    with tempfile.TemporaryDirectory(prefix="cwt-storage-") as temporary:
        root = Path(temporary)
        state = root / "state"
        command = sharing.fixture(root)
        environment = {key: value for key, value in os.environ.items()
            if not key.upper().startswith("CWT_") and key.upper() not in {"CODEX_THREAD_ID", "CLAUDECODE"}}
        environment.update(CODEX_WEB_TOKEN=TOKEN, CODEX_WEB_UPDATE_POLICY="off")
        log = (root / "server.log").open("wb")
        process = None
        attachment = None
        capture_attachment = None
        capture_pool = None

        def api(path, method="GET", payload=None, status=200, token=TOKEN):
            code, data = peer.http_request(args.port, path, token=token, method=method, payload=payload)
            assert code == status, f"{method} {path}: HTTP {code}, expected {status}"
            return data

        def start():
            child = subprocess.Popen([str(args.server.resolve()), "--host", "127.0.0.1", "--port", str(args.port),
                "--project", str(root), "--state-dir", str(state), "--command", str(command),
                "--no-agent-auto-detect", "--no-open-browser"], env=environment, stdout=log, stderr=log,
                creationflags=subprocess.CREATE_NO_WINDOW if os.name == "nt" else 0)
            def ready():
                assert child.poll() is None, "Disposable storage server failed to start"
                try:
                    return peer.http_request(args.port, "/api/health", token=TOKEN)[0] == 200
                except OSError:
                    return False
            sharing.wait(ready)
            return child

        base = "/api/sharing/storage"
        try:
            process = start()
            assert api(base)["available"]
            sharing.stop(process)
            # Reproduce a v0.5 database on disk, without retaining private data.
            # Downgrading is confined to this disposable fixture, with the server stopped.
            database = state / "sharing.sqlite3"
            old, kept = str(uuid.uuid4()), str(uuid.uuid4())
            kept_chunk = (b"synthetic-preserved-raw-\x1b[31m\r\n" * 1024)[:16384]
            with closing(sqlite3.connect(database)) as db:
                db.executescript("DROP TABLE archive_cleanup; DROP TABLE storage_policy; ALTER TABLE generations DROP COLUMN ended_at; PRAGMA user_version=1;")
                db.execute("PRAGMA auto_vacuum=NONE")
                db.execute("VACUUM")
                for identity, count, chunk in [(old, 8192, b"x" * 16384), (kept, 512, kept_chunk)]:
                    db.execute("INSERT INTO generations(id,terminal,label,agent,created,bytes,last_event,ended,cols,rows) VALUES(?,?,'Synthetic archive','Codex',?,?,?,1,80,24)",
                        (identity, identity, 1, count * len(chunk), count))
                    db.executemany("INSERT INTO archive_events(generation,sequence,kind,data,cols,rows,at) VALUES(?,?,'output',?,0,0,1)",
                        ((identity, index, chunk) for index in range(1, count + 1)))
                db.execute("INSERT INTO guests(id,terminal,name,name_key) VALUES('reserved',?,'Reserved fixture','reserved fixture')", (old,))
                db.commit()
                db.execute("PRAGMA wal_checkpoint(TRUNCATE)")
            process = start()
            initial = api(base)
            assert initial["recordings"] == 2 and not initial["incrementalCompaction"]
            assert not initial["policy"]["enabled"], "Upgrade must not enable automatic deletion"
            api(base, status=401, token=None)
            api(base + "/cleanup", "POST", {"filter": {}, "fingerprint": "0" * 64}, status=409)
            api(base + "/policy", "POST", {"enabled": True, "retentionDays": 0, "maxBytes": 0}, status=400)
            source = api("/api/sessions")[0]
            terminal = source["terminalId"]
            attachment = peer.attach_terminal(args.port, TOKEN, terminal, flow_control=True)
            sharing.replay(attachment, b"SHARING-READY")

            def ping(index):
                marker = f"STORAGE-PING-{index}".encode()
                started = time.perf_counter()
                attachment.send(2, marker + b"\r")
                received = bytearray()
                deadline = time.monotonic() + 10
                while b"ACCEPTED:" + marker not in received:
                    opcode, payload = attachment.receive_frame(deadline)
                    if opcode == 2:
                        received.extend(payload)
                        attachment.send(1, json.dumps({"type": "output_ack", "bytes": len(payload)}).encode())
                return (time.perf_counter() - started) * 1000

            baseline = [ping(index) for index in range(5)]
            active = api("/api/sessions", "POST", {"agent": "codex", "recordHistory": True}, status=201)
            api(base + "/compact", "POST", {"confirm": True}, status=409)
            capture_attachment = peer.attach_terminal(args.port, TOKEN, active["terminalId"], flow_control=True)
            sharing.replay(capture_attachment, b"SHARING-READY")
            def capture_burst():
                capture_attachment.send(2, b"BURST\r")
                received = bytearray()
                deadline = time.monotonic() + 40
                while b"SHARING-BURST-END" not in received:
                    opcode, payload = capture_attachment.receive_frame(deadline)
                    if opcode == 2:
                        received.extend(payload)
                        capture_attachment.send(1, json.dumps({"type": "output_ack", "bytes": len(payload)}).encode())
                return len(received)
            capture_pool = ThreadPoolExecutor(max_workers=1)
            capture_future = capture_pool.submit(capture_burst)
            reviewed_filter = {"generations": [old]}
            reviewed = api(base + "/cleanup/preview", "POST", reviewed_filter)
            assert reviewed["count"] == 1 and reviewed["bytes"] == 128 * 1024 * 1024
            started = time.perf_counter()
            api(base + "/cleanup", "POST", {"filter": reviewed_filter, "fingerprint": reviewed["fingerprint"]})
            during = []
            while True:
                during.append(ping(100 + len(during)))
                if api(base)["cleanupPending"] == 0:
                    break
                assert time.perf_counter() - started < 90, "Bounded cleanup stalled"
            metrics["cleanupMs"] = round((time.perf_counter() - started) * 1000, 2)
            assert capture_future.result(timeout=45) > 8 * 1024 * 1024
            def capture_saved():
                rows = api("/api/sharing/archives")["generations"]
                row = next((item for item in rows if item["id"] == active["sessionId"]), None)
                if row and row["bytes"] > 8 * 1024 * 1024:
                    assert not row["incomplete"], "Cleanup lost concurrent recording bytes"
                    return True
                return False
            sharing.wait(capture_saved)
            api(f'/api/sessions/{active["terminalId"]}/sharing/record', "DELETE", status=204)
            with ThreadPoolExecutor(max_workers=1) as pool:
                compact = pool.submit(api, base + "/compact", "POST", {"confirm": True})
                for index in range(10):
                    during.append(ping(200 + index))
                while not compact.done():
                    during.append(ping(300 + len(during)))
                    time.sleep(0.01)
                result = compact.result()
            after = api(base)
            assert not (state / "sharing.compact.sqlite3").exists(), "Compaction left a scratch copy"
            assert after["incrementalCompaction"] and after["recordings"] == 2
            assert after["databaseBytes"] < initial["databaseBytes"] * 0.3, "Physical database failed to shrink by at least 70%"
            assert result["reclaimedBytes"] > 64 * 1024 * 1024, "Compaction must measure substantial reclaimed disk space"
            assert max(during) < max(2000, max(baseline) * 20), "Storage maintenance stalled native PTY input"
            digest = hashlib.sha256()
            cursor = 0
            while True:
                page = api(f"/api/sharing/archives/{kept}?after={cursor}")["events"]
                if not page:
                    break
                import base64
                for event in page:
                    digest.update(base64.b64decode(event["data"]))
                    cursor = event["sequence"]
            assert digest.digest() == hashlib.sha256(kept_chunk * 512).digest(), "Compaction changed retained raw bytes"
            assert api("/api/sessions")[0]["sessionId"] == source["sessionId"]
            api(base + "/policy", "POST", {"enabled": False, "retentionDays": 90, "maxBytes": 16 * 1024 * 1024}, status=204)
            if args.browser:
                browser_check(args, active)
            attachment.close()
            attachment = None
            capture_attachment.close()
            capture_attachment = None
            sharing.stop(process)
            with closing(sqlite3.connect(database)) as db:
                assert db.execute("PRAGMA integrity_check").fetchone()[0] == "ok"
                assert db.execute("SELECT name FROM guests WHERE id='reserved'").fetchone()[0] == "Reserved fixture"
                assert db.execute("SELECT retention_days FROM storage_policy").fetchone()[0] == 90
            if os.name == "nt":
                changed = subprocess.run(["icacls.exe", str(state), "/grant", "*S-1-1-0:(OI)(CI)R"], capture_output=True, creationflags=subprocess.CREATE_NO_WINDOW)
                assert changed.returncode == 0
                process = start()
                assert api(base)["code"] == "unsafe_state_directory"
                repair_source = api("/api/sessions")[0]
                before_repair = repair_source["sessionId"]
                api(base + "/repair", "POST", {"confirm": False}, status=400)
                api(base + "/repair", "POST", {"confirm": True, "path": "C:\\"}, status=400)
                if args.browser:
                    browser_check(args, repair_source, repair=True)
                else:
                    api(base + "/repair", "POST", {"confirm": True}, status=204)
                assert api(base)["available"]
                assert api("/api/sessions")[0]["sessionId"] == before_repair
                assert any(item["id"] == kept for item in api("/api/sharing/archives")["generations"])
                metrics["windowsPermissionRecovery"] = "passed without restarting PTY"
            metrics.update(databaseBeforeBytes=initial["databaseBytes"], databaseAfterBytes=after["databaseBytes"],
                reclaimedBytes=result["reclaimedBytes"], compactMs=result["elapsedMs"],
                databaseReductionPercent=round(100 * (1 - after["databaseBytes"] / initial["databaseBytes"]), 2),
                baselinePtyMedianMs=round(statistics.median(baseline), 2),
                maintenancePtyMaxMs=round(max(during), 2), maintenancePtySamples=len(during),
                retainedRawBytes=8 * 1024 * 1024, concurrentCapture="complete", integrity="ok", legacySchemaMigration="passed")
        finally:
            if attachment:
                attachment.close()
            if capture_attachment:
                capture_attachment.close()
            if capture_pool:
                capture_pool.shutdown(wait=True, cancel_futures=True)
            if process:
                sharing.stop(process)
            log.close()
        assert not peer.port_is_listening(args.port)
    print(json.dumps(metrics, sort_keys=True))
    if args.report:
        args.report.write_text(json.dumps(metrics, indent=2) + "\n", encoding="utf-8")


def browser_check(args, source, repair=False):
    from playwright.sync_api import sync_playwright
    with sync_playwright() as playwright:
        browser = playwright.chromium.launch(headless=True, executable_path=os.environ.get("CWT_CHROMIUM_PATH"))
        context = browser.new_context(viewport={"width": 390, "height": 844})
        page = context.new_page()
        page.goto(f"http://127.0.0.1:{args.port}/?token={TOKEN}")
        page.get_by_role("button", name="Menu", exact=True).click()
        page.get_by_role("menuitem", name="Manage sessions", exact=False).click()
        card = page.locator(".session-card").filter(has=page.get_by_role("heading", name=source["name"], exact=True))
        card.get_by_role("button", name="Share / history", exact=False).click()
        page.get_by_role("button", name="Storage", exact=True).click()
        if repair:
            page.get_by_role("button", name="Repair sharing permissions").wait_for()
            page.once("dialog", lambda dialog: dialog.accept())
            page.get_by_role("button", name="Repair sharing permissions").click()
            page.get_by_text("Permissions repaired. Sharing is ready; no server restart is required.", exact=True).wait_for()
            page.get_by_text("Total on disk", exact=True).wait_for()
            page.get_by_role("button", name="Guests and proposals", exact=True).click()
            assert page.get_by_text("The server's data folder permissions are not private.", exact=False).count() == 0
            context.close()
            browser.close()
            return
        page.get_by_text("Total on disk", exact=True).wait_for()
        page.get_by_role("button", name="Preview cleanup").click()
        button = page.get_by_role("button", name="Delete previewed recordings")
        button.wait_for()
        page.once("dialog", lambda dialog: dialog.dismiss())
        button.click()
        assert page.get_by_role("button", name="Delete previewed recordings").count() == 1
        assert not page.get_by_label("Enable automatic cleanup of finished recordings").is_checked()
        assert page.evaluate("document.documentElement.scrollWidth <= window.innerWidth + 1")
        context.close()
        browser.close()


if __name__ == "__main__":
    main()
