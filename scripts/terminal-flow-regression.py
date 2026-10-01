#!/usr/bin/env python3
"""Check bounded output delivery and restore identities against a disposable PTY."""
from __future__ import annotations

import argparse
import importlib.util
import json
import os
from pathlib import Path
import shlex
import socket
import subprocess
import sys
import tempfile
import time
import uuid
from urllib.request import ProxyHandler, Request, build_opener

spec = importlib.util.spec_from_file_location("cwt_peer_fixture", Path(__file__).with_name("peer-review-regression.py"))
assert spec and spec.loader
peer = importlib.util.module_from_spec(spec)
sys.modules[spec.name] = peer
spec.loader.exec_module(peer)

TOKEN = "synthetic-flow-regression-token"
WINDOW = 4 * 1024 * 1024


def stop_server(process: subprocess.Popen) -> None:
    if process.poll() is None:
        if os.name == "nt":
            subprocess.run(["taskkill", "/PID", str(process.pid), "/T", "/F"],
                           stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                           creationflags=subprocess.CREATE_NO_WINDOW, check=False)
        else:
            process.terminate()
        try:
            process.wait(timeout=10)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait(timeout=10)


def check_ipv6(server: Path, root: Path, command: Path, environment: dict[str, str]) -> None:
    with socket.socket(socket.AF_INET6, socket.SOCK_STREAM) as probe:
        probe.bind(("::1", 0))
        port = probe.getsockname()[1]
    process = subprocess.Popen([
        str(server.resolve()), "--host", "::1", "--port", str(port),
        "--project", str(root), "--state-dir", str(root / "state-ipv6"),
        "--command", str(command), "--no-agent-auto-detect", "--no-open-browser",
    ], env=environment, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
        creationflags=subprocess.CREATE_NO_WINDOW if os.name == "nt" else 0)
    try:
        opener = build_opener(ProxyHandler({}))
        request = Request(f"http://[::1]:{port}/api/health", headers={"Authorization": f"Bearer {TOKEN}"})
        deadline = time.monotonic() + 30
        while True:
            try:
                with opener.open(request, timeout=1) as response:
                    assert response.status == 200
                    break
            except OSError:
                if process.poll() is not None or time.monotonic() >= deadline:
                    raise AssertionError("Synthetic IPv6 server did not start") from None
                time.sleep(0.1)
        with socket.create_connection(("::1", port), timeout=5) as connection:
            connection.sendall((f"GET /ws?token={TOKEN} HTTP/1.1\r\nHost: [::1]:{port}\r\n"
                f"Origin: http://[::1]:{port}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n"
                "Sec-WebSocket-Version: 13\r\nSec-WebSocket-Key: c3ludGhldGljLWZpeHR1cg==\r\n\r\n").encode("ascii"))
            header = bytearray()
            while b"\r\n\r\n" not in header:
                chunk = connection.recv(4096)
                assert chunk, "IPv6 WebSocket closed before upgrade"
                header.extend(chunk)
                assert len(header) <= 16384
            assert b" 101 " in header.split(b"\r\n", 1)[0], "IPv6 WebSocket origin was rejected"
    finally:
        stop_server(process)
    with socket.socket(socket.AF_INET6, socket.SOCK_STREAM) as probe:
        probe.settimeout(1)
        assert probe.connect_ex(("::1", port)) != 0, "Disposable IPv6 server is still listening"


def fixture(root: Path) -> Path:
    program = root / "flow.py"
    program.write_text('''import os, sys, time
if "--version" in sys.argv:
    print("codex 1.2.3")
    raise SystemExit
if "--help" in sys.argv:
    print("Synthetic flow fixture")
    raise SystemExit
print("FLOW-READY", flush=True)
for line in sys.stdin:
    if "GO" in line:
        for batch in range(1024):
            data = "".join("FLOW-%08d:%s\\r\\n" % (batch * 128 + row, "x" * 45) for row in range(128))
            os.write(1, data.encode("ascii"))
            time.sleep(0.003)
        print("FLOW-END-MARKER", flush=True)
''', encoding="utf-8")
    if os.name == "nt":
        command = root / "flow.cmd"
        command.write_text(f'@echo off\r\n"{sys.executable}" -u "{program}" %*\r\n', encoding="utf-8")
    else:
        command = root / "flow"
        command.write_text(f"#!/bin/sh\nexec {shlex.quote(sys.executable)} -u {shlex.quote(str(program))} \"$@\"\n", encoding="utf-8")
        command.chmod(0o700)
    return command


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--server", type=Path, required=True)
    parser.add_argument("--port", type=int, default=8824)
    args = parser.parse_args()
    peer.assert_isolated_port(args.port)
    with tempfile.TemporaryDirectory(prefix="cwt-flow-") as temporary:
        root = Path(temporary)
        command = fixture(root)
        environment = {key: value for key, value in os.environ.items()
                       if not key.upper().startswith("CWT_") and key.upper() not in {"CODEX_THREAD_ID", "CLAUDECODE"}}
        environment["CODEX_WEB_TOKEN"] = TOKEN
        environment["CODEX_WEB_UPDATE_POLICY"] = "off"
        process = subprocess.Popen([
            str(args.server.resolve()), "--host", "127.0.0.1", "--port", str(args.port),
            "--project", str(root), "--state-dir", str(root / "state"),
            "--command", str(command), "--no-agent-auto-detect", "--no-open-browser",
        ], env=environment, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
            creationflags=subprocess.CREATE_NO_WINDOW if os.name == "nt" else 0)
        attachment = None
        try:
            deadline = time.monotonic() + 30
            while True:
                try:
                    sessions = peer.request_json(args.port, TOKEN, "/api/sessions")
                    if sessions and sessions[0]["status"] == "running":
                        break
                except OSError:
                    pass
                if process.poll() is not None or time.monotonic() >= deadline:
                    raise AssertionError("Synthetic server did not start")
                time.sleep(0.1)
            primary = sessions[0]
            body = {"agent": "codex", "directoryId": primary["directoryId"], "restoreRequestId": str(uuid.uuid4())}
            first = peer.request_json(args.port, TOKEN, "/api/sessions", method="POST", payload=body, expected_status=201)
            retry = peer.request_json(args.port, TOKEN, "/api/sessions", method="POST", payload=body, expected_status=201)
            assert first["terminalId"] == retry["terminalId"]
            assert len(peer.request_json(args.port, TOKEN, "/api/sessions")) == 2
            conflict_body = {**body, "directoryId": peer.request_json(
                args.port, TOKEN, "/api/filesystem/resolve", method="POST",
                payload={"path": str(root / "state")},
            )["current"]["id"]}
            conflict = peer.request_json(args.port, TOKEN, "/api/sessions", method="POST",
                                         payload=conflict_body, expected_status=409)
            assert conflict["code"] == "restore_conflict"
            peer.request_json(args.port, TOKEN, f'/api/sessions/{first["terminalId"]}',
                              method="DELETE", expected_status=204)
            deleted = peer.request_json(args.port, TOKEN, "/api/sessions", method="POST",
                                        payload=body, expected_status=409)
            assert deleted["code"] == "restore_deleted"
            assert len(peer.request_json(args.port, TOKEN, "/api/sessions")) == 1
            attachment = peer.attach_terminal(args.port, TOKEN, primary["terminalId"], flow_control=True)

            def acknowledge(length: int) -> None:
                attachment.send(1, json.dumps({"type": "output_ack", "bytes": length}).encode())

            ready = bytearray()
            negotiated = False
            replay_ended = False
            deadline = time.monotonic() + 30
            while b"FLOW-READY" not in ready or not negotiated or not replay_ended:
                opcode, payload = attachment.receive_frame(deadline)
                if opcode == 2:
                    ready.extend(payload)
                    acknowledge(len(payload))
                    if b"\x1b[6n" in payload:
                        attachment.send(2, b"\x1b[1;1R")
                elif opcode == 1:
                    control = json.loads(payload)
                    negotiated |= control.get("type") == "flow_control" and control.get("windowBytes") == WINDOW
                    replay_ended |= control.get("type") == "replay_end"
            attachment.send(2, b"GO\r")
            received = bytearray()
            deadline = time.monotonic() + 30
            while time.monotonic() < deadline:
                try:
                    opcode, payload = attachment.receive_frame(time.monotonic() + 1)
                except TimeoutError:
                    if len(received) > WINDOW // 2:
                        break
                    continue
                if opcode == 2:
                    received.extend(payload)
                    assert len(received) <= WINDOW, "Output exceeded the negotiated window"
            assert WINDOW // 2 < len(received) <= WINDOW
            assert b"FLOW-END-MARKER" not in received
            attachment.send(1, b'{"type":"ping"}')
            deadline = time.monotonic() + 5
            while True:
                opcode, payload = attachment.receive_frame(deadline)
                assert opcode != 2, "Output continued without parser acknowledgement"
                if opcode == 1 and json.loads(payload).get("type") == "pong":
                    break
            paused_bytes = len(received)
            acknowledge(paused_bytes)
            deadline = time.monotonic() + 30
            while b"FLOW-END-MARKER" not in received:
                opcode, payload = attachment.receive_frame(deadline)
                if opcode == 2:
                    received.extend(payload)
                    acknowledge(len(payload))
                elif opcode == 1:
                    assert json.loads(payload).get("type") != "replay_start", "Unexpected replay gap during flow control"
            assert len(received) > paused_bytes
            attachment.close()
            attachment = None
            still_running = peer.request_json(args.port, TOKEN, f'/api/sessions/{primary["terminalId"]}')
            assert still_running["status"] == "running"
            print(json.dumps({"boundedWindow": True, "resumedWithoutReplayGap": True,
                              "restoreRetryReusedPty": True, "deletedRestoreRejected": True,
                              "disconnectPreservedPty": True}))
        finally:
            if attachment:
                attachment.close()
            stop_server(process)
        assert not peer.port_is_listening(args.port), "Disposable server port is still open"
        check_ipv6(args.server, root, command, environment)
        print(json.dumps({"ipv6LoopbackHttpAndWebSocket": True}))


if __name__ == "__main__":
    main()
