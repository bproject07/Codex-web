#!/usr/bin/env python3
"""Exercise scoped sharing against disposable native PTYs and optional browsers."""
from __future__ import annotations

import argparse
import base64
from concurrent.futures import ThreadPoolExecutor
import importlib.util
import json
import os
from pathlib import Path
import secrets
import shlex
import signal
import socket
import subprocess
import sys
import tempfile
import time
import uuid

spec = importlib.util.spec_from_file_location("sharing_peer_fixture", Path(__file__).with_name("peer-review-regression.py"))
assert spec and spec.loader
peer = importlib.util.module_from_spec(spec)
sys.modules[spec.name] = peer
spec.loader.exec_module(peer)
TOKEN = "synthetic-sharing-owner-token"


def fixture(root: Path) -> Path:
    program = root / "sharing.py"
    program.write_text('''import json, os, sys, time
from pathlib import Path
if "--version" in sys.argv:
    print("codex 1.2.3")
    raise SystemExit
if "--help" in sys.argv:
    print("Synthetic sharing fixture")
    raise SystemExit
path = Path(__file__).parent / (os.environ["CWT_TERMINAL_ID"] + ".jsonl")
print("SHARING-READY", flush=True)
for line in sys.stdin:
    text = line.rstrip("\\r\\n")
    with path.open("a", encoding="utf-8") as output:
        output.write(json.dumps(text) + "\\n")
    print("ACCEPTED:" + text, flush=True)
    if text == "EXIT":
        print("SHARING-FINAL-DRAIN", flush=True)
        break
    if text == "BURST":
        for i in range(1024):
            os.write(1, ("ROW-%04d:" % i + "x" * 8192 + "\\r\\n").encode())
            time.sleep(0.003)
        print("SHARING-BURST-END", flush=True)
''', encoding="utf-8")
    if os.name == "nt":
        command = root / "sharing.cmd"
        command.write_text(f'@echo off\r\n"{sys.executable}" -u "{program}" %*\r\n', encoding="utf-8")
    else:
        command = root / "sharing"
        command.write_text(f'#!/bin/sh\nexec {shlex.quote(sys.executable)} -u {shlex.quote(str(program))} "$@"\n', encoding="utf-8")
        command.chmod(0o700)
    return command


def wait(check, timeout=20):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        result = check()
        if result:
            return result
        time.sleep(0.05)
    raise AssertionError("Sharing condition did not settle")


def credential():
    return str(uuid.uuid4()) + "." + secrets.token_urlsafe(32)


def attach_guest(port, token):
    connection = socket.create_connection(("127.0.0.1", port), timeout=5)
    connection.sendall((f"GET /ws/share HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n"
        f"Origin: http://127.0.0.1:{port}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n"
        "Sec-WebSocket-Version: 13\r\nSec-WebSocket-Key: c3ludGhldGljLWZpeHR1cg==\r\n\r\n").encode())
    response = bytearray()
    while b"\r\n\r\n" not in response:
        response.extend(connection.recv(4096))
        assert len(response) < 16384
    header, buffered = bytes(response).split(b"\r\n\r\n", 1)
    assert b" 101 " in header.split(b"\r\n", 1)[0]
    attachment = peer.WebSocketAttachment(connection, bytearray(buffered))
    attachment.send(1, json.dumps({"credential": token}).encode())
    return attachment


def replay(attachment, marker=None, acknowledge=True):
    data = bytearray()
    controls = []
    ended = False
    deadline = time.monotonic() + 20
    while not ended or (marker and marker not in data):
        opcode, payload = attachment.receive_frame(deadline)
        assert opcode != 8, "Guest connection closed unexpectedly"
        if opcode == 2:
            data.extend(payload)
            if acknowledge:
                attachment.send(1, json.dumps({"type": "output_ack", "bytes": len(payload)}).encode())
        elif opcode == 1:
            control = json.loads(payload)
            controls.append(control)
            ended |= control.get("type") == "replay_end"
    return data, controls


def closed(attachment):
    deadline = time.monotonic() + 5
    try:
        while time.monotonic() < deadline:
            opcode, _ = attachment.receive_frame(deadline)
            if opcode == 8:
                return True
    except (ConnectionError, OSError):
        return True
    return False


def stop(process):
    if process.poll() is not None:
        return
    if os.name == "nt":
        subprocess.run(["taskkill", "/PID", str(process.pid), "/T", "/F"],
            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, check=False,
            creationflags=subprocess.CREATE_NO_WINDOW)
    else:
        process.send_signal(signal.SIGINT)
    try:
        process.wait(timeout=15)
    except subprocess.TimeoutExpired:
        process.kill()
        process.wait(timeout=5)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--server", type=Path, required=True)
    parser.add_argument("--port", type=int, default=8827)
    parser.add_argument("--browser", action="store_true")
    args = parser.parse_args()
    peer.assert_isolated_port(args.port)
    with tempfile.TemporaryDirectory(prefix="cwt-sharing-") as temporary:
        root = Path(temporary)
        command = fixture(root)
        environment = {key: value for key, value in os.environ.items()
            if not key.upper().startswith("CWT_") and key.upper() not in {"CODEX_THREAD_ID", "CLAUDECODE"}}
        environment["CODEX_WEB_TOKEN"] = TOKEN
        environment["CODEX_WEB_UPDATE_POLICY"] = "off"
        log = (root / "server.log").open("wb")
        def start():
            process = subprocess.Popen([str(args.server.resolve()), "--host", "127.0.0.1",
                "--port", str(args.port), "--project", str(root), "--state-dir", str(root / "state"),
                "--command", str(command), "--no-agent-auto-detect", "--no-open-browser"],
                env=environment, stdout=log, stderr=log,
                creationflags=subprocess.CREATE_NO_WINDOW if os.name == "nt" else 0)
            def ready():
                assert process.poll() is None, "Disposable server failed to start"
                try:
                    return peer.http_request(args.port, "/api/health", token=TOKEN)[0] == 200
                except OSError:
                    return False
            wait(ready)
            return process
        def api(path, method="GET", payload=None, token=TOKEN, status=200):
            code, data = peer.http_request(args.port, path, token=token, method=method, payload=payload)
            if code == 503 and status != 503:
                # Never dump startup URLs, credentials or arbitrary server logs.
                log.flush()
                diagnostic = (root / "server.log").read_text(encoding="utf-8", errors="replace")
                known = [reason for reason in ["unsafe_state_directory", "unsafe_state_file",
                    "sharing_store_in_use", "sharing_schema_too_new", "storage_unavailable"] if reason in diagnostic]
                print(json.dumps({"sharingStorageFailure": known}))
            assert code == status, f"{method} {path}: HTTP {code}, expected {status}"
            return data
        process = start()
        attachments = []
        try:
            source = api("/api/sessions", "POST", {"agent": "codex", "recordHistory": True}, status=201)
            terminal = source["terminalId"]
            generation = source["sessionId"]
            owner = peer.attach_terminal(args.port, TOKEN, terminal, flow_control=True)
            attachments.append(owner)
            replay(owner, b"SHARING-READY")
            base = f"/api/sessions/{terminal}/sharing"
            def join(name, mode="propose", admit=True):
                invitation = api(base + "/invitations", "POST", {"mode": mode})["invitation"]
                token = credential()
                body = {"invitation": invitation, "credential": token, "name": name}
                guest = api("/api/share/redeem", "POST", body, token=None)
                assert api("/api/share/redeem", "POST", body, token=None) == guest
                if admit:
                    api(base + f'/guests/{guest["grantId"]}/admit', "POST", status=204)
                return token, guest, body
            alice, guest, redemption = join("Alice", admit=False)
            assert not api("/api/share/me", token=alice)["guest"]["admitted"]
            api("/api/share/archives", token=alice, status=403)
            api("/api/share/redeem", "POST", {**redemption, "name": "Changed"}, token=None, status=409)
            api("/api/sessions", token=alice, status=401)
            api(base + f'/guests/{guest["grantId"]}/admit', "POST", status=204)
            viewer = attach_guest(args.port, alice)
            attachments.append(viewer)
            _, controls = replay(viewer, b"SHARING-READY")
            metadata = next(c for c in controls if c["type"] == "shared_session")
            assert not any(key in metadata for key in ("project", "directoryId", "pid", "token"))
            viewer.send(1, b'{"type":"presence","visible":true}')
            wait(lambda: any(g["name"] == "Alice" and g["presence"] == "visible" for g in api(base)["guests"]))
            bob, _, _ = join("Bob", "view")
            api("/api/share/proposals", "POST", {"requestId": str(uuid.uuid4()), "text": "DENIED"}, token=bob, status=403)
            api("/api/share/archives/" + str(uuid.uuid4()), token=alice, status=403)
            # A full 4 MiB replay must resume on partial ACK even with finite,
            # small output and no further PTY notification.
            owner.send(2, b"BURST\r")
            burst = bytearray()
            deadline = time.monotonic() + 30
            while b"SHARING-BURST-END" not in burst:
                opcode, payload = owner.receive_frame(deadline)
                if opcode == 2:
                    burst.extend(payload)
                    owner.send(1, json.dumps({"type": "output_ack", "bytes": len(payload)}).encode())
            lagging = attach_guest(args.port, bob)
            attachments.append(lagging)
            recent, _ = replay(lagging, acknowledge=False)
            assert 3 * 1024 * 1024 < len(recent) <= 4 * 1024 * 1024
            owner.send(2, b"AFTER-FULL-REPLAY\r")
            lagging.send(1, b'{"type":"output_ack","bytes":32768}')
            finite = bytearray()
            deadline = time.monotonic() + 10
            while b"ACCEPTED:AFTER-FULL-REPLAY" not in finite:
                opcode, payload = lagging.receive_frame(deadline)
                if opcode == 2:
                    finite.extend(payload)
                elif opcode == 1:
                    assert json.loads(payload).get("type") != "replay_start"
            assert len(finite) <= 4 * 1024 * 1024 - len(recent) + 32768
            lagging.close()
            draft = {"requestId": str(uuid.uuid4()), "text": "Original\nmultiline proposal"}
            proposal = api("/api/share/proposals", "POST", draft, token=alice)
            assert api("/api/share/proposals", "POST", draft, token=alice)["id"] == proposal["id"]
            assert api("/api/share/me", token=bob)["proposals"] == []
            decision = {"requestId": str(uuid.uuid4()), "sessionId": generation,
                "revision": proposal["revision"], "text": "APPROVED-EXACT-TEXT", "ready": True}
            approve = base + f'/proposals/{proposal["id"]}/approve'
            api(approve, "POST", {**decision, "ready": False}, status=400)
            api(approve, "POST", {**decision, "text": "one\ntwo"}, status=400)
            api(approve, "POST", {**decision, "revision": 99}, status=409)
            review = api("/api/peer/threads", "POST", {"sourceTerminalId": terminal,
                "targetAgent": "codex", "action": "review", "instruction": "Synthetic fixture only.",
                "sourceReady": True}, status=201)
            blocked = api(approve, "POST", decision)
            assert blocked["state"] == "pending" and blocked["reason"] == "terminal_busy"
            private_draft = api("/api/share/me", token=alice)["proposals"][0]
            assert private_draft["approvedText"] is None and private_draft["text"] == draft["text"]
            assert api("/api/share/proposals", "POST", draft, token=alice)["text"] == draft["text"]
            api("/api/peer/threads/" + review["id"], "DELETE", status=204)
            assert api(approve, "POST", decision)["state"] == "pending", "Old rejected attempt was re-enqueued"
            decision = {**decision, "requestId": str(uuid.uuid4()), "revision": blocked["revision"]}
            with ThreadPoolExecutor(max_workers=2) as pool:
                results = list(pool.map(lambda _: api(approve, "POST", decision), range(2)))
            assert all(result["original"] == draft["text"] for result in results)
            wait(lambda: api(base)["proposals"][0]["state"] == "written")
            inputs = root / (terminal + ".jsonl")
            wait(lambda: inputs.exists() and "APPROVED-EXACT-TEXT" in inputs.read_text(encoding="utf-8"))
            assert [json.loads(line) for line in inputs.read_text(encoding="utf-8").splitlines()].count(decision["text"]) == 1
            api(approve, "POST", {**decision, "text": "CHANGED"}, status=409)
            pending = api("/api/share/proposals", "POST", {"requestId": str(uuid.uuid4()), "text": "NEVER-SEND"}, token=alice)
            viewer.send(2, b"FORBIDDEN-KEYSTROKES\r")
            assert closed(viewer), "Guest binary input was accepted"
            viewer.close()
            viewer = attach_guest(args.port, alice)
            attachments.append(viewer)
            replay(viewer)
            api(base + f'/guests/{guest["grantId"]}', "DELETE", status=204)
            assert closed(viewer), "Revocation did not close the guest socket"
            api("/api/share/me", token=alice, status=401)
            assert next(p for p in api(base)["proposals"] if p["id"] == pending["id"])["state"] == "invalidated"
            assert "FORBIDDEN-KEYSTROKES" not in inputs.read_text(encoding="utf-8")
            renewal = api(base + "/invitations", "POST", {"mode": "propose"})["invitation"]
            renewed_token = credential()
            renewal_body = {"invitation": renewal, "credential": renewed_token,
                "previousCredential": alice, "name": "Renamed"}
            api("/api/share/redeem", "POST", renewal_body, token=None, status=409)
            renewed = api("/api/share/redeem", "POST", {**renewal_body, "name": "Alice"}, token=None)
            assert renewed["guestId"] == guest["guestId"] and not renewed["admitted"]
            api("/api/share/archives", token=renewed_token, status=403)
            # Renewing a still-valid grant rotates it atomically, keeps the
            # identity and requires fresh owner admission.
            api(base + f'/guests/{renewed["grantId"]}/admit', "POST", status=204)
            for index in range(6):
                join("Extra"+str(index), "view")
            rotating_socket = attach_guest(args.port, renewed_token)
            attachments.append(rotating_socket)
            replay(rotating_socket)
            rotation_invite = api(base + "/invitations", "POST", {"mode":"propose"})["invitation"]
            rotated_token = credential()
            rotated = api("/api/share/redeem", "POST", {"invitation":rotation_invite,
                "credential":rotated_token,"previousCredential":renewed_token,"name":"Alice"}, token=None)
            assert rotated["guestId"] == guest["guestId"] and not rotated["admitted"]
            assert closed(rotating_socket)
            api("/api/share/me", token=renewed_token, status=401)
            # Complete history, raw bytes and geometry survive generation end.
            owner.send(1, b'{"type":"resize","cols":100,"rows":30}')
            owner.send(2, b"EXIT\r")
            wait(lambda: api(f"/api/sessions/{terminal}")["status"] == "exited")
            wait(lambda: any(g["id"] == generation and g["ended"] for g in api("/api/sharing/archives")["generations"]))
            events = []
            after = 0
            while True:
                page = api("/api/sharing/archives/" + generation + f"?after={after}")["events"]
                if not page:
                    break
                events.extend(page)
                after = page[-1]["sequence"]
            raw = b"".join(base64.b64decode(event["data"]) for event in events if event["kind"] == "output")
            assert b"SHARING-FINAL-DRAIN" in raw
            assert any(event["kind"] == "resize" and event["cols"] == 100 for event in events)
            assert not next(g for g in api("/api/sharing/archives")["generations"] if g["id"] == generation)["incomplete"]
            api(f"/api/sessions/{terminal}/restart", "POST", status=204)
            restarted = api(f"/api/sessions/{terminal}")
            assert restarted["sessionId"] != generation
            api("/api/share/me", token=bob, status=409)
            api(base + "/invitations", "POST", {"mode": "view"})
            api("/api/share/me", token=bob, status=401)
            # Stop remains in effect across PTY restarts; re-enable never imports
            # output produced during the private interval.
            api(base + "/record", "DELETE", status=204)
            assert not api(base)["recording"]
            api(f"/api/sessions/{terminal}/restart", "POST", status=204)
            paused = api(f"/api/sessions/{terminal}")
            assert not paused["recording"]
            paused_owner = peer.attach_terminal(args.port, TOKEN, terminal, flow_control=True)
            attachments.append(paused_owner)
            replay(paused_owner, b"SHARING-READY")
            paused_owner.send(2, b"PRIVATE-PAUSE-MARKER\r")
            wait(lambda: "PRIVATE-PAUSE-MARKER" in inputs.read_text(encoding="utf-8"))
            # The input file precedes the output print. Observe that output on
            # the owner socket before resuming capture.
            private_output = bytearray()
            deadline = time.monotonic()+10
            while b"ACCEPTED:PRIVATE-PAUSE-MARKER" not in private_output:
                opcode, payload = paused_owner.receive_frame(deadline)
                if opcode == 2:
                    private_output.extend(payload)
                    paused_owner.send(1,json.dumps({"type":"output_ack","bytes":len(payload)}).encode())
            api(base + "/record", "POST", status=204)
            assert api(base)["recording"]
            paused_owner.send(2,b"EXIT\r")
            wait(lambda: any(g["id"]==paused["sessionId"] and g["ended"] for g in api("/api/sharing/archives")["generations"]))
            pause_events=api("/api/sharing/archives/"+paused["sessionId"])["events"]
            assert b"PRIVATE-PAUSE-MARKER" not in b"".join(base64.b64decode(e["data"]) for e in pause_events)
            assert api(base+"/status")["available"]
            if args.browser:
                browser_check(args, api, root)
            for attachment in attachments:
                attachment.close()
            attachments.clear()
            stop(process)
            process = start()
            api("/api/share/me", token=bob, status=401)
            saved = api("/api/sharing/archives/" + generation + f"?after={after - 4}")["events"]
            assert saved == events[-4:], "Server restart changed saved history"
            audit = api("/api/sharing/archives/" + generation + "/proposals")["proposals"]
            assert next(p for p in audit if p["id"] == proposal["id"])["original"] == draft["text"]
            usage=api("/api/sharing/prune-retired", "POST")
            assert usage["proposals"] == usage["approvals"] == usage["invitations"] == 0
            assert api("/api/sharing/archives/"+generation)["events"]
            api(approve,"POST",decision,status=404)
            api("/api/sharing/archives/" + generation, "DELETE", status=204)
            api("/api/sharing/archives/" + generation, status=404)
            print(json.dumps({"sharing": "passed", "nativePty": True, "exactlyOneApprovedWrite": True,
                "guestIsolation": True, "revocation": True, "archiveAfterRestart": True, "browser": args.browser}))
        finally:
            for attachment in attachments:
                attachment.close()
            stop(process)
            log.close()
        assert not peer.port_is_listening(args.port)


def browser_check(args, api, root):
    from playwright.sync_api import sync_playwright
    source = api("/api/sessions", "POST", {"agent": "codex", "recordHistory": True}, status=201)
    terminal = source["terminalId"]
    base = f"/api/sessions/{terminal}/sharing"
    invitation = api(base + "/invitations", "POST", {"mode": "propose"})["invitation"]
    with sync_playwright() as playwright:
        executable = os.environ.get("CWT_CHROMIUM_PATH")
        browser = playwright.chromium.launch(headless=True, executable_path=executable)
        context = browser.new_context(viewport={"width": 390, "height": 844})
        page = context.new_page()
        page.goto(f"http://127.0.0.1:{args.port}/share#invite={invitation}")
        page.get_by_label("Your name").fill("Browser Guest")
        page.get_by_role("button", name="Confirm name and request access").click()
        page.get_by_text("Waiting for the owner to admit you", exact=False).wait_for()
        guest = next(g for g in api(base)["guests"] if g["name"] == "Browser Guest")
        owner_context = browser.new_context(viewport={"width": 1280, "height": 900})
        owner = owner_context.new_page()
        owner.goto(f"http://127.0.0.1:{args.port}/?token={TOKEN}")
        owner.get_by_role("button", name="Menu", exact=True).click()
        owner.get_by_role("menuitem", name="Manage sessions", exact=False).click()
        card = owner.locator(".session-card").filter(has=owner.get_by_role("heading", name=source["name"], exact=True))
        card.get_by_role("button", name="Share / history", exact=False).click()
        owner.get_by_role("button", name="Admit Browser Guest", exact=True).click()
        page.get_by_role("heading", name="Propose an instruction").wait_for()
        assert "invite" not in page.url
        page.get_by_label("Your proposal", exact=True).fill("BROWSER-PROPOSAL")
        page.get_by_role("button", name="Send for approval").click()
        page.get_by_role("button", name="A proposal is waiting for approval").wait_for()
        assert api(base)["proposals"][0]["author"] == "Browser Guest"
        owner.get_by_label("Text to send", exact=False).fill("BROWSER-APPROVED", timeout=10_000)
        owner.get_by_label("The agent is waiting at an empty prompt.", exact=True).check()
        owner.get_by_role("button", name="Approve and send", exact=True).click()
        wait(lambda: api(base)["proposals"][0]["state"] == "written")
        owner.get_by_label("Address guests can reach").fill("http://10.0.0.1:7681")
        owner.get_by_role("button", name="Create individual invitation").click()
        invitation_input = owner.locator(".share-link input")
        invitation_input.wait_for()
        assert invitation_input.input_value().startswith("http://10.0.0.1:7681/share#invite=")
        assert TOKEN not in invitation_input.input_value()
        inputs = root / (terminal + ".jsonl")
        wait(lambda: inputs.exists() and "BROWSER-APPROVED" in inputs.read_text(encoding="utf-8"))
        page.reload()
        page.get_by_role("heading", name="Propose an instruction").wait_for()
        assert page.get_by_label("Your name").count() == 0
        page.get_by_role("button", name="Recorded history", exact=True).click()
        page.get_by_role("button", name="Play to end").wait_for()
        assert page.evaluate("document.documentElement.scrollWidth <= window.innerWidth + 1")
        api(base + f'/guests/{guest["grantId"]}', "DELETE", status=204)
        page.get_by_text("Access has ended. Contact the owner for a new invitation.", exact=True).wait_for()
        context.close()
        owner_context.close()
        browser.close()


if __name__ == "__main__":
    main()
