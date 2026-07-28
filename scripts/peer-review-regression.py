#!/usr/bin/env python3
"""Exercise the supervised peer workflow on a disposable native PTY server.

The script owns every process and file it creates. It refuses the live
development ports 8788, 8789, and 8790, uses only synthetic agent commands,
and never installs, updates, authenticates, or invokes a real agent CLI.
"""

from __future__ import annotations

if not __debug__:
    raise RuntimeError(
        "peer-review-regression.py requires assertions; do not run it with "
        "python -O or PYTHONOPTIMIZE"
    )

import argparse
import base64
from dataclasses import dataclass
import hashlib
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
from typing import Any, Callable
from urllib.error import HTTPError, URLError
from urllib.parse import urlencode
from urllib.request import Request, urlopen


DEFAULT_PORT = 8804
PROTECTED_LIVE_PORTS = frozenset({8788, 8789, 8790})
WAIT_SECONDS = 30
WEBSOCKET_GUID = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11"


@dataclass(frozen=True)
class OwnedProcess:
    pid: int
    marker: str
    label: str


@dataclass
class WebSocketAttachment:
    connection: socket.socket
    buffered: bytearray

    def send(self, opcode: int, payload: bytes) -> None:
        send_websocket_frame(self.connection, opcode, payload)

    def read_exact(self, length: int, deadline: float) -> bytes:
        while len(self.buffered) < length:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise TimeoutError("timed out reading a terminal WebSocket frame")
            self.connection.settimeout(min(remaining, 1.0))
            chunk = self.connection.recv(64 * 1024)
            if not chunk:
                raise ConnectionError("terminal WebSocket closed unexpectedly")
            self.buffered.extend(chunk)
        value = bytes(self.buffered[:length])
        del self.buffered[:length]
        return value

    def receive_frame(self, deadline: float) -> tuple[int, bytes]:
        first, second = self.read_exact(2, deadline)
        opcode = first & 0x0F
        length = second & 0x7F
        if length == 126:
            length = int.from_bytes(self.read_exact(2, deadline), "big")
        elif length == 127:
            length = int.from_bytes(self.read_exact(8, deadline), "big")
        if length > 2 * 1024 * 1024:
            raise ConnectionError("terminal WebSocket frame exceeds the test limit")
        mask = self.read_exact(4, deadline) if second & 0x80 else None
        payload = self.read_exact(length, deadline)
        if mask is not None:
            payload = bytes(
                value ^ mask[index % 4] for index, value in enumerate(payload)
            )
        if first & 0x80 == 0:
            raise ConnectionError("fragmented WebSocket frames are not expected")
        return opcode, payload

    def wait_for_error(self, code: str, timeout: float = 10) -> dict[str, Any]:
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            opcode, payload = self.receive_frame(deadline)
            if opcode == 0x8:
                raise ConnectionError("terminal WebSocket closed before its error")
            if opcode == 0x9:
                self.send(0xA, payload)
                continue
            if opcode != 0x1:
                continue
            try:
                control = json.loads(payload.decode("utf-8"))
            except (UnicodeDecodeError, json.JSONDecodeError):
                continue
            if (
                isinstance(control, dict)
                and control.get("type") == "error"
                and control.get("code") == code
            ):
                return control
        raise TimeoutError(f"terminal WebSocket did not return {code!r}")

    def close(self) -> None:
        try:
            self.connection.shutdown(socket.SHUT_RDWR)
        except OSError:
            pass
        self.connection.close()


def parse_args() -> argparse.Namespace:
    repository = Path(__file__).resolve().parents[1]
    executable_name = "codex-web.exe" if os.name == "nt" else "codex-web"
    default_server = repository / "server" / "target" / "release" / executable_name
    parser = argparse.ArgumentParser(
        description=(
            "Validate the real PTY, loopback helper, preview, review, return, "
            "same-reviewer Recheck, and owned peer cleanup workflow."
        )
    )
    parser.add_argument("--server", type=Path, default=default_server)
    parser.add_argument("--port", type=int, default=DEFAULT_PORT)
    return parser.parse_args()


def assert_isolated_port(port: int) -> None:
    if port in PROTECTED_LIVE_PORTS:
        raise ValueError(
            f"port {port} is reserved for a live server; choose a disposable port"
        )
    if not 1 <= port <= 65_535:
        raise ValueError("port must be between 1 and 65535")

    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as probe:
        probe.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        try:
            probe.bind(("127.0.0.1", port))
        except OSError as error:
            raise RuntimeError(
                f"port {port} is already in use; no process was stopped"
            ) from error


def port_is_listening(port: int) -> bool:
    try:
        with socket.create_connection(("127.0.0.1", port), timeout=0.25):
            return True
    except OSError:
        return False


def process_marker(pid: int) -> str | None:
    if pid <= 0:
        return None
    if os.name == "nt":
        import ctypes
        from ctypes import wintypes

        process_query_limited_information = 0x1000
        still_active = 259
        kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
        kernel32.OpenProcess.argtypes = (
            wintypes.DWORD,
            wintypes.BOOL,
            wintypes.DWORD,
        )
        kernel32.OpenProcess.restype = wintypes.HANDLE
        kernel32.GetExitCodeProcess.argtypes = (
            wintypes.HANDLE,
            ctypes.POINTER(wintypes.DWORD),
        )
        kernel32.GetExitCodeProcess.restype = wintypes.BOOL
        kernel32.GetProcessTimes.argtypes = (
            wintypes.HANDLE,
            ctypes.POINTER(wintypes.FILETIME),
            ctypes.POINTER(wintypes.FILETIME),
            ctypes.POINTER(wintypes.FILETIME),
            ctypes.POINTER(wintypes.FILETIME),
        )
        kernel32.GetProcessTimes.restype = wintypes.BOOL
        kernel32.CloseHandle.argtypes = (wintypes.HANDLE,)
        kernel32.CloseHandle.restype = wintypes.BOOL

        handle = kernel32.OpenProcess(
            process_query_limited_information,
            False,
            pid,
        )
        if not handle:
            return None
        try:
            exit_code = wintypes.DWORD()
            if not kernel32.GetExitCodeProcess(handle, ctypes.byref(exit_code)):
                raise ctypes.WinError(ctypes.get_last_error())
            if exit_code.value != still_active:
                return None
            creation = wintypes.FILETIME()
            exit_time = wintypes.FILETIME()
            kernel_time = wintypes.FILETIME()
            user_time = wintypes.FILETIME()
            if not kernel32.GetProcessTimes(
                handle,
                ctypes.byref(creation),
                ctypes.byref(exit_time),
                ctypes.byref(kernel_time),
                ctypes.byref(user_time),
            ):
                raise ctypes.WinError(ctypes.get_last_error())
            created = (creation.dwHighDateTime << 32) | creation.dwLowDateTime
            return f"windows:{created}"
        finally:
            kernel32.CloseHandle(handle)

    try:
        stat = (Path("/proc") / str(pid) / "stat").read_text(encoding="ascii")
    except (FileNotFoundError, ProcessLookupError):
        return None
    except (OSError, UnicodeError) as error:
        raise RuntimeError(
            f"cannot identify owned Linux process PID {pid}"
        ) from error
    closing_parenthesis = stat.rfind(")")
    fields = stat[closing_parenthesis + 2 :].split()
    if closing_parenthesis < 0 or len(fields) < 20 or fields[0] == "Z":
        return None
    return f"linux:{fields[19]}"


def track_owned_process(
    owned: list[OwnedProcess],
    pid: int,
    label: str,
) -> OwnedProcess:
    marker = process_marker(pid)
    if marker is None:
        raise AssertionError(f"{label} PID {pid} is not running")
    identity = OwnedProcess(pid=pid, marker=marker, label=label)
    if all(
        existing.pid != identity.pid or existing.marker != identity.marker
        for existing in owned
    ):
        owned.append(identity)
    return identity


def owned_process_is_running(process: OwnedProcess) -> bool:
    return process_marker(process.pid) == process.marker


def http_request(
    port: int,
    path: str,
    *,
    token: str | None,
    method: str = "GET",
    payload: dict[str, Any] | None = None,
) -> tuple[int, Any]:
    data = json.dumps(payload).encode("utf-8") if payload is not None else None
    headers = {"Accept": "application/json"}
    if token is not None:
        headers["Authorization"] = f"Bearer {token}"
    if data is not None:
        headers["Content-Type"] = "application/json"
    request = Request(
        f"http://127.0.0.1:{port}{path}",
        data=data,
        method=method,
        headers=headers,
    )
    try:
        with urlopen(request, timeout=10) as response:
            status = response.status
            body = response.read()
    except HTTPError as error:
        status = error.code
        body = error.read()

    if not body:
        return status, None
    try:
        return status, json.loads(body)
    except json.JSONDecodeError:
        return status, body.decode("utf-8", errors="replace")


def request_json(
    port: int,
    token: str,
    path: str,
    *,
    method: str = "GET",
    payload: dict[str, Any] | None = None,
    expected_status: int = 200,
) -> Any:
    status, body = http_request(
        port,
        path,
        token=token,
        method=method,
        payload=payload,
    )
    if status != expected_status:
        raise AssertionError(
            f"{method} {path} returned HTTP {status}, expected "
            f"{expected_status}: {body!r}"
        )
    return body


def send_websocket_frame(
    connection: socket.socket,
    opcode: int,
    payload: bytes,
) -> None:
    if len(payload) >= 126:
        raise ValueError("regression WebSocket payload is unexpectedly large")
    mask = secrets.token_bytes(4)
    header = bytes((0x80 | opcode, 0x80 | len(payload)))
    masked = bytes(
        value ^ mask[index % len(mask)] for index, value in enumerate(payload)
    )
    connection.sendall(header + mask + masked)


def attach_terminal(
    port: int,
    token: str,
    terminal_id: str,
) -> WebSocketAttachment:
    connection = socket.create_connection(("127.0.0.1", port), timeout=10)
    try:
        key = base64.b64encode(secrets.token_bytes(16)).decode("ascii")
        query = urlencode({"token": token, "terminalId": terminal_id})
        request = (
            f"GET /ws?{query} HTTP/1.1\r\n"
            f"Host: 127.0.0.1:{port}\r\n"
            f"Origin: http://127.0.0.1:{port}\r\n"
            "Upgrade: websocket\r\n"
            "Connection: Upgrade\r\n"
            f"Sec-WebSocket-Key: {key}\r\n"
            "Sec-WebSocket-Version: 13\r\n"
            "\r\n"
        )
        connection.sendall(request.encode("ascii"))

        response = bytearray()
        while b"\r\n\r\n" not in response:
            chunk = connection.recv(4096)
            if not chunk:
                raise ConnectionError("WebSocket upgrade closed before completion")
            response.extend(chunk)
            if len(response) > 16 * 1024:
                raise ConnectionError("WebSocket upgrade response is too large")

        header_bytes, buffered = bytes(response).split(b"\r\n\r\n", 1)
        lines = header_bytes.decode("iso-8859-1").split("\r\n")
        if not lines or " 101 " not in f" {lines[0]} ":
            raise ConnectionError(
                f"terminal WebSocket upgrade failed: {lines[0] if lines else ''}"
            )
        headers: dict[str, str] = {}
        for line in lines[1:]:
            name, separator, value = line.partition(":")
            if separator:
                headers[name.strip().lower()] = value.strip()
        expected_accept = base64.b64encode(
            hashlib.sha1(
                f"{key}{WEBSOCKET_GUID}".encode("ascii"),
                usedforsecurity=False,
            ).digest()
        ).decode("ascii")
        if headers.get("sec-websocket-accept") != expected_accept:
            raise ConnectionError("terminal WebSocket returned an invalid accept key")

        attachment = WebSocketAttachment(connection, bytearray(buffered))
        attachment.send(
            0x1,
            b'{"type":"resize","cols":80,"rows":24}',
        )
        if os.name == "nt":
            # ConPTY's shell startup requests cursor position before launching
            # the command. Browser xterm answers this automatically.
            attachment.send(0x2, b"\x1b[1;1R")
        connection.settimeout(None)
        return attachment
    except Exception:
        connection.close()
        raise


def close_terminal_attachments(
    attachments: list[WebSocketAttachment],
) -> None:
    for attachment in reversed(attachments):
        attachment.close()


def server_diagnostics(
    process: subprocess.Popen[bytes],
    stdout_log: Path,
    stderr_log: Path,
    token: str,
) -> str:
    parts = [f"server return code: {process.poll()!r}"]
    for label, path in (("stdout", stdout_log), ("stderr", stderr_log)):
        try:
            content = path.read_text(encoding="utf-8", errors="replace")
        except OSError:
            content = ""
        content = content.replace(token, "<redacted-test-token>").strip()
        if content:
            parts.append(f"{label}:\n{content[-3000:]}")
    return "\n".join(parts)


def assert_fixture_has_no_error(events: Path) -> None:
    errors = sorted(events.glob("error-*.json"))
    if not errors:
        return
    details: list[Any] = []
    for path in errors:
        try:
            details.append(json.loads(path.read_text(encoding="utf-8")))
        except (OSError, json.JSONDecodeError):
            details.append(path.name)
    raise AssertionError(f"synthetic agent reported an error: {details!r}")


def fixture_diagnostics(events: Path) -> str:
    summaries: list[str] = []
    for path in sorted(events.glob("*.json"))[-20:]:
        try:
            value = json.loads(path.read_text(encoding="utf-8"))
        except (OSError, json.JSONDecodeError):
            value = "<unreadable>"
        summaries.append(f"{path.name}: {value!r}")
    if not summaries:
        return "synthetic events: none"
    return "synthetic events:\n" + "\n".join(summaries)


def wait_for(
    description: str,
    process: subprocess.Popen[bytes],
    events: Path,
    predicate: Callable[[], Any | None],
    *,
    stdout_log: Path,
    stderr_log: Path,
    token: str,
    timeout: float = WAIT_SECONDS,
) -> Any:
    deadline = time.monotonic() + timeout
    last_error: Exception | None = None
    while time.monotonic() < deadline:
        assert_fixture_has_no_error(events)
        if process.poll() is not None:
            raise RuntimeError(
                f"test server exited while waiting for {description}\n"
                f"{server_diagnostics(process, stdout_log, stderr_log, token)}\n"
                f"{fixture_diagnostics(events)}"
            )
        try:
            result = predicate()
            if result is not None:
                return result
        except (OSError, URLError) as error:
            last_error = error
        time.sleep(0.1)

    suffix = f"; last transient error: {last_error}" if last_error else ""
    raise TimeoutError(
        f"timed out waiting for {description}{suffix}\n"
        f"{server_diagnostics(process, stdout_log, stderr_log, token)}\n"
        f"{fixture_diagnostics(events)}"
    )


def wait_for_server(
    port: int,
    token: str,
    process: subprocess.Popen[bytes],
    events: Path,
    *,
    stdout_log: Path,
    stderr_log: Path,
) -> None:
    def ready() -> bool | None:
        status, body = http_request(port, "/api/health", token=token)
        if (
            status == 200
            and isinstance(body, dict)
            and body.get("sessionRunning") is True
        ):
            return True
        return None

    wait_for(
        "the primary synthetic PTY",
        process,
        events,
        ready,
        stdout_log=stdout_log,
        stderr_log=stderr_log,
        token=token,
    )


def wait_for_thread_status(
    port: int,
    token: str,
    process: subprocess.Popen[bytes],
    events: Path,
    thread_id: str,
    expected_status: str,
    *,
    stdout_log: Path,
    stderr_log: Path,
) -> dict[str, Any]:
    def ready() -> dict[str, Any] | None:
        status, body = http_request(
            port,
            f"/api/peer/threads/{thread_id}",
            token=token,
        )
        if status != 200 or not isinstance(body, dict):
            raise AssertionError(
                f"peer thread lookup returned HTTP {status}: {body!r}"
            )
        return body if body.get("status") == expected_status else None

    return wait_for(
        f"peer thread {thread_id} to become {expected_status}",
        process,
        events,
        ready,
        stdout_log=stdout_log,
        stderr_log=stderr_log,
        token=token,
    )


def wait_for_event(
    process: subprocess.Popen[bytes],
    events: Path,
    terminal_id: str,
    kind: str,
    sequence: int,
    *,
    stdout_log: Path,
    stderr_log: Path,
    token: str,
) -> dict[str, Any]:
    path = events / f"{terminal_id}-{kind}-{sequence}.json"

    def ready() -> dict[str, Any] | None:
        if not path.is_file():
            return None
        try:
            value = json.loads(path.read_text(encoding="utf-8"))
        except json.JSONDecodeError:
            return None
        if not isinstance(value, dict):
            raise AssertionError(f"invalid synthetic event: {value!r}")
        return value

    return wait_for(
        f"synthetic {kind} event {sequence}",
        process,
        events,
        ready,
        stdout_log=stdout_log,
        stderr_log=stderr_log,
        token=token,
    )


def write_fixture_command(root: Path) -> Path:
    fixture = root / "synthetic-peer-agent.py"
    fixture_source = r'''#!/usr/bin/env python3
from __future__ import annotations

import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import time


if len(sys.argv) == 2 and sys.argv[1] == "--version":
    if "CODEX_WEB_TOKEN" in os.environ:
        print("server bearer token leaked into the version probe", file=sys.stderr)
        raise SystemExit(90)
    print("synthetic-peer-agent 1.0.0", flush=True)
    raise SystemExit(0)


TURN_PATTERN = re.compile(r"--turn ([0-9a-fA-F-]{36})")
SEQUENCE_PATTERN = re.compile(r"\[CWT peer ([0-9]+)\]")
EVENTS = Path(__CWT_EVENT_DIRECTORY__)
TERMINAL_ID = os.environ.get("CWT_TERMINAL_ID", f"missing-{os.getpid()}")
SESSION_ID = os.environ.get("CWT_SESSION_ID", "missing")
HELPER = os.environ.get("CWT_PEER_HELPER", "")
review_count = 0


def write_event(kind: str, sequence: int, payload: dict[str, object]) -> None:
    target = EVENTS / f"{TERMINAL_ID}-{kind}-{sequence}.json"
    temporary = target.with_name(f".{target.name}.{os.getpid()}.tmp")
    temporary.write_text(
        json.dumps(payload, sort_keys=True),
        encoding="utf-8",
    )
    os.replace(temporary, target)


def write_error(message: str) -> None:
    target = EVENTS / f"error-{TERMINAL_ID}-{os.getpid()}-{time.time_ns()}.json"
    target.write_text(
        json.dumps(
            {
                "terminalId": TERMINAL_ID,
                "sessionId": SESSION_ID,
                "message": message,
            },
            sort_keys=True,
        ),
        encoding="utf-8",
    )


def input_prompts():
    pending = bytearray()
    while True:
        byte = sys.stdin.buffer.read(1)
        if not byte:
            if pending:
                yield pending.decode("utf-8", errors="replace")
            return
        if byte in (b"\r", b"\n"):
            if pending:
                yield pending.decode("utf-8", errors="replace")
                pending.clear()
            continue
        pending.extend(byte)


def metadata(prompt: str) -> tuple[int, str]:
    sequence_match = SEQUENCE_PATTERN.search(prompt)
    turn_match = TURN_PATTERN.search(prompt)
    if sequence_match is None or turn_match is None:
        raise RuntimeError("automation prompt did not contain peer metadata")
    return int(sequence_match.group(1)), turn_match.group(1)


def run_helper(
    operation: str,
    turn_id: str,
    content: str | None = None,
) -> str:
    arguments = [HELPER, "__cwt-peer", operation, "--turn", turn_id]
    if operation == "submit":
        arguments.append("--stdin")
    completed = subprocess.run(
        arguments,
        input=content,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        encoding="utf-8",
        timeout=12,
        check=False,
        creationflags=(
            subprocess.CREATE_NO_WINDOW if os.name == "nt" else 0
        ),
    )
    if completed.returncode != 0:
        detail = completed.stderr.strip()[-1000:]
        raise RuntimeError(
            f"peer helper {operation} failed with "
            f"{completed.returncode}: {detail}"
        )
    return completed.stdout.rstrip("\r\n")


def submit_handoff(sequence: int, turn_id: str) -> None:
    handoff = "\n".join(
        (
            "# Synthetic peer handoff",
            f"sequence: {sequence}",
            f"source-terminal: {TERMINAL_ID}",
            f"source-session: {SESSION_ID}",
            f"workspace: {os.getcwd()}",
        )
    )
    run_helper("submit", turn_id, handoff)
    write_event(
        "handoff-submitted",
        sequence,
        {
            "turnId": turn_id,
            "handoffSha256": hashlib.sha256(
                handoff.encode("utf-8")
            ).hexdigest(),
        },
    )


def submit_review(sequence: int, turn_id: str) -> None:
    global review_count
    handoff = run_helper("receive", turn_id)
    review_count += 1
    approved = f"approved-preview-revision: {sequence}" in handoff
    response = "\n".join(
        (
            "# Synthetic reviewer response",
            f"sequence: {sequence}",
            f"review-count: {review_count}",
            f"reviewer-terminal: {TERMINAL_ID}",
            f"reviewer-session: {SESSION_ID}",
            f"reviewer-process: {os.getpid()}",
            f"approved-preview: {str(approved).lower()}",
            "handoff-sha256: "
            + hashlib.sha256(handoff.encode("utf-8")).hexdigest(),
        )
    )
    run_helper("submit", turn_id, response)
    write_event(
        "review-submitted",
        sequence,
        {
            "turnId": turn_id,
            "reviewCount": review_count,
            "approvedPreview": approved,
        },
    )


def receive_response(sequence: int, turn_id: str) -> None:
    response = run_helper("receive", turn_id)
    write_event(
        "response-received",
        sequence,
        {
            "turnId": turn_id,
            "responseSha256": hashlib.sha256(
                response.encode("utf-8")
            ).hexdigest(),
        },
    )


missing_environment = [
    name
    for name in ("CWT_TERMINAL_ID", "CWT_SESSION_ID", "CWT_PEER_HELPER")
    if name not in os.environ
]
if missing_environment:
    write_error("missing peer environment: " + ", ".join(missing_environment))
    raise SystemExit(2)
if "CODEX_WEB_TOKEN" in os.environ:
    write_error("server bearer token leaked into the managed PTY")
    raise SystemExit(2)

print(f"SYNTHETIC-AGENT-READY {TERMINAL_ID}", flush=True)
write_event(
    "ready",
    0,
    {
        "process": os.getpid(),
        "sessionId": SESSION_ID,
    },
)
for prompt in input_prompts():
    try:
        sequence_match = SEQUENCE_PATTERN.search(prompt)
        observed_sequence = int(sequence_match.group(1)) if sequence_match else 0
        write_event(
            "input-observed",
            observed_sequence,
            {
                "length": len(prompt.encode("utf-8")),
                "prepare": (
                    "Prepare a concise, self-contained Markdown handoff" in prompt
                ),
                "review": (
                    "A supervised " in prompt and " handoff is ready." in prompt
                ),
                "return": " reviewer has returned a response." in prompt,
            },
        )
        if prompt == "[CWT regression health]":
            write_event(
                "health",
                0,
                {
                    "process": os.getpid(),
                    "sessionId": SESSION_ID,
                },
            )
        elif "Prepare a concise, self-contained Markdown handoff" in prompt:
            sequence, turn_id = metadata(prompt)
            submit_handoff(sequence, turn_id)
        elif "A supervised " in prompt and " handoff is ready." in prompt:
            sequence, turn_id = metadata(prompt)
            submit_review(sequence, turn_id)
        elif " reviewer has returned a response." in prompt:
            sequence, turn_id = metadata(prompt)
            receive_response(sequence, turn_id)
    except Exception as error:
        write_error(f"{type(error).__name__}: {error}")
'''
    fixture.write_text(
        fixture_source.replace(
            "__CWT_EVENT_DIRECTORY__",
            repr(str(root / "events")),
        ),
        encoding="utf-8",
    )

    if os.name == "nt":
        command = root / "synthetic-peer-agent.cmd"
        python = subprocess.list2cmdline([sys.executable])
        script = subprocess.list2cmdline([str(fixture)])
        command.write_text(
            f"@echo off\r\n{python} -u {script} %*\r\nexit /b %ERRORLEVEL%\r\n",
            encoding="utf-8",
            newline="",
        )
        return command

    command = root / "synthetic-peer-agent"
    command.write_text(
        "#!/bin/sh\n"
        f"exec {shlex.quote(sys.executable)} -u {shlex.quote(str(fixture))} \"$@\"\n",
        encoding="utf-8",
    )
    command.chmod(0o700)
    return command


def isolated_server_environment(
    root: Path,
    events: Path,
    token: str,
) -> dict[str, str]:
    environment = os.environ.copy()
    for name in (
        "CODEX_INSTALL_DIR",
        "CODEX_WEB_STATE_DIR",
        "CWT_PEER_ENDPOINT",
        "CWT_PEER_HELPER",
        "CWT_TERMINAL_ID",
        "CWT_SESSION_ID",
        "CWT_PEER_CAPABILITY",
    ):
        environment.pop(name, None)
    environment["CWT_REGRESSION_EVENT_DIR"] = str(events)
    environment["PYTHONDONTWRITEBYTECODE"] = "1"
    environment["PYTHONIOENCODING"] = "utf-8"
    environment["PYTHONUTF8"] = "1"
    environment["CODEX_WEB_TOKEN"] = token

    if os.name == "nt":
        system_root = Path(environment.get("SystemRoot", r"C:\Windows"))
        environment["PATH"] = os.pathsep.join(
            (
                str(system_root / "System32"),
                str(system_root / "System32" / "WindowsPowerShell" / "v1.0"),
                str(system_root),
            )
        )
        profile = root / "isolated-profile"
        local_app_data = profile / "AppData" / "Local"
        app_data = profile / "AppData" / "Roaming"
        local_app_data.mkdir(parents=True)
        app_data.mkdir(parents=True)
        environment["USERPROFILE"] = str(profile)
        environment["LOCALAPPDATA"] = str(local_app_data)
        environment["APPDATA"] = str(app_data)
    else:
        environment["PATH"] = os.pathsep.join(("/usr/bin", "/bin"))
        home = root / "isolated-home"
        home.mkdir()
        environment["HOME"] = str(home)
        environment["XDG_STATE_HOME"] = str(root / "isolated-state-home")
    return environment


def canonical_path(path: Path | str) -> str:
    return os.path.normcase(os.path.realpath(os.fspath(path)))


def paths_equal(left: Path | str, right: Path | str) -> bool:
    return canonical_path(left) == canonical_path(right)


def artifact_field(artifact: str, name: str) -> str:
    prefix = f"{name}: "
    for line in artifact.splitlines():
        if line.startswith(prefix):
            return line[len(prefix) :]
    raise AssertionError(f"peer artifact has no {name!r} field: {artifact!r}")


def sha256_text(value: str) -> str:
    return hashlib.sha256(value.encode("utf-8")).hexdigest()


def sessions(port: int, token: str) -> list[dict[str, Any]]:
    value = request_json(port, token, "/api/sessions")
    if not isinstance(value, list) or not all(
        isinstance(session, dict) for session in value
    ):
        raise AssertionError(f"invalid session list: {value!r}")
    return value


def session_by_id(
    current: list[dict[str, Any]],
    terminal_id: str,
) -> dict[str, Any]:
    for session in current:
        if session.get("terminalId") == terminal_id:
            return session
    raise AssertionError(f"terminal {terminal_id} is absent from {current!r}")


def run_turn(
    *,
    port: int,
    token: str,
    process: subprocess.Popen[bytes],
    events: Path,
    thread_id: str,
    source: dict[str, Any],
    reviewer: dict[str, Any],
    sequence: int,
    stdout_log: Path,
    stderr_log: Path,
) -> tuple[dict[str, Any], str]:
    preview = wait_for_thread_status(
        port,
        token,
        process,
        events,
        thread_id,
        "awaiting_preview",
        stdout_log=stdout_log,
        stderr_log=stderr_log,
    )
    turn = preview["currentTurn"]
    assert turn["sequence"] == sequence, preview
    assert turn["handoffRevision"] == 1, preview
    handoff = turn["handoff"]
    assert isinstance(handoff, str) and handoff, preview
    assert artifact_field(handoff, "sequence") == str(sequence), handoff
    assert artifact_field(handoff, "source-terminal") == source["terminalId"]
    assert artifact_field(handoff, "source-session") == source["sessionId"]
    assert paths_equal(artifact_field(handoff, "workspace"), source["project"])
    handoff_event = wait_for_event(
        process,
        events,
        source["terminalId"],
        "handoff-submitted",
        sequence,
        stdout_log=stdout_log,
        stderr_log=stderr_log,
        token=token,
    )
    assert handoff_event["turnId"] == turn["id"], handoff_event
    assert handoff_event["handoffSha256"] == sha256_text(handoff), handoff_event

    revised_handoff = (
        f"{handoff.rstrip()}\n\napproved-preview-revision: {sequence}"
    )
    dispatched = request_json(
        port,
        token,
        f"/api/peer/threads/{thread_id}/dispatch",
        method="POST",
        payload={
            "turnId": turn["id"],
            "handoffRevision": turn["handoffRevision"],
            "handoff": revised_handoff,
            "reviewerReady": True,
        },
    )
    assert dispatched["status"] == "reviewing", dispatched
    assert dispatched["currentTurn"]["handoffRevision"] == 2, dispatched

    response_ready = wait_for_thread_status(
        port,
        token,
        process,
        events,
        thread_id,
        "response_ready",
        stdout_log=stdout_log,
        stderr_log=stderr_log,
    )
    current_turn = response_ready["currentTurn"]
    assert current_turn["handoff"] == revised_handoff, response_ready
    response = current_turn["response"]
    assert isinstance(response, str) and response, response_ready
    assert artifact_field(response, "sequence") == str(sequence), response
    assert artifact_field(response, "reviewer-terminal") == reviewer["terminalId"]
    assert artifact_field(response, "reviewer-session") == reviewer["sessionId"]
    assert artifact_field(response, "approved-preview") == "true", response
    assert artifact_field(response, "handoff-sha256") == sha256_text(
        revised_handoff
    )
    review_event = wait_for_event(
        process,
        events,
        reviewer["terminalId"],
        "review-submitted",
        sequence,
        stdout_log=stdout_log,
        stderr_log=stderr_log,
        token=token,
    )
    assert review_event["turnId"] == current_turn["id"], review_event
    assert review_event["reviewCount"] == sequence, review_event
    assert review_event["approvedPreview"] is True, review_event

    returned = request_json(
        port,
        token,
        f"/api/peer/threads/{thread_id}/return",
        method="POST",
        payload={
            "turnId": current_turn["id"],
            "sourceReady": True,
        },
    )
    assert returned["status"] == "returned", returned
    source_event = wait_for_event(
        process,
        events,
        source["terminalId"],
        "response-received",
        sequence,
        stdout_log=stdout_log,
        stderr_log=stderr_log,
        token=token,
    )
    assert source_event["turnId"] == current_turn["id"], source_event
    assert source_event["responseSha256"] == sha256_text(response), source_event
    return response_ready, response


def exercise_peer_flow(
    port: int,
    token: str,
    process: subprocess.Popen[bytes],
    events: Path,
    project: Path,
    attachments: list[WebSocketAttachment],
    owned_processes: list[OwnedProcess],
    *,
    stdout_log: Path,
    stderr_log: Path,
) -> dict[str, Any]:
    unauthorized_status, _ = http_request(
        port,
        "/api/peer/threads",
        token=None,
    )
    assert unauthorized_status == 401, unauthorized_status

    initial_sessions = sessions(port, token)
    assert len(initial_sessions) == 1, initial_sessions
    source = initial_sessions[0]
    assert source["isPrimary"] is True, source
    assert source["purpose"] == {"kind": "interactive"}, source
    assert source["status"] == "running", source
    assert paths_equal(source["project"], project), source
    source_session_id = source["sessionId"]
    source_root_process = track_owned_process(
        owned_processes,
        int(source["pid"]),
        "source PTY root",
    )
    source_attachment = attach_terminal(port, token, source["terminalId"])
    attachments.append(source_attachment)
    source_ready = wait_for_event(
        process,
        events,
        source["terminalId"],
        "ready",
        0,
        stdout_log=stdout_log,
        stderr_log=stderr_log,
        token=token,
    )
    source_fixture_process = track_owned_process(
        owned_processes,
        int(source_ready["process"]),
        "source synthetic agent",
    )

    ordinary = request_json(
        port,
        token,
        "/api/sessions",
        method="POST",
        payload={"agent": "claude"},
        expected_status=201,
    )
    assert ordinary["purpose"] == {"kind": "interactive"}, ordinary
    assert ordinary["terminalId"] != source["terminalId"], ordinary
    track_owned_process(
        owned_processes,
        int(ordinary["pid"]),
        "ordinary PTY root",
    )
    ordinary_attachment = attach_terminal(
        port,
        token,
        ordinary["terminalId"],
    )
    attachments.append(ordinary_attachment)
    ordinary_ready = wait_for_event(
        process,
        events,
        ordinary["terminalId"],
        "ready",
        0,
        stdout_log=stdout_log,
        stderr_log=stderr_log,
        token=token,
    )
    track_owned_process(
        owned_processes,
        int(ordinary_ready["process"]),
        "ordinary synthetic agent",
    )

    created = request_json(
        port,
        token,
        "/api/peer/threads",
        method="POST",
        payload={
            "sourceTerminalId": source["terminalId"],
            "targetAgent": "claude",
            "action": "review",
            "instruction": "Review the synthetic implementation without assuming Git.",
            "sourceReady": True,
        },
        expected_status=201,
    )
    thread_id = created["id"]
    reviewer_terminal_id = created["reviewerTerminalId"]
    assert created["sourceTerminalId"] == source["terminalId"], created
    assert reviewer_terminal_id not in (
        source["terminalId"],
        ordinary["terminalId"],
    ), created

    after_create = sessions(port, token)
    assert len(after_create) == 3, after_create
    reviewer = session_by_id(after_create, reviewer_terminal_id)
    assert reviewer["agent"] == "claude", reviewer
    assert reviewer["purpose"] == {
        "kind": "peer",
        "threadId": thread_id,
        "parentTerminalId": source["terminalId"],
    }, reviewer
    assert paths_equal(reviewer["project"], project), reviewer
    reviewer_session_id = reviewer["sessionId"]
    reviewer_pid = reviewer["pid"]
    reviewer_root_process = track_owned_process(
        owned_processes,
        int(reviewer_pid),
        "reviewer PTY root",
    )
    reviewer_attachment = attach_terminal(port, token, reviewer_terminal_id)
    attachments.append(reviewer_attachment)
    reviewer_ready = wait_for_event(
        process,
        events,
        reviewer_terminal_id,
        "ready",
        0,
        stdout_log=stdout_log,
        stderr_log=stderr_log,
        token=token,
    )
    reviewer_fixture_process = track_owned_process(
        owned_processes,
        int(reviewer_ready["process"]),
        "reviewer synthetic agent",
    )

    source_attachment.send(
        0x1,
        b'{"type":"restart"}',
    )
    reviewer_attachment.send(
        0x1,
        b'{"type":"restart"}',
    )
    source_restart_error = source_attachment.wait_for_error("restart_failed")
    reviewer_restart_error = reviewer_attachment.wait_for_error("restart_failed")
    assert "peer" in str(source_restart_error.get("message", "")).lower()
    assert "peer" in str(reviewer_restart_error.get("message", "")).lower()
    after_guarded_restarts = sessions(port, token)
    assert (
        session_by_id(after_guarded_restarts, source["terminalId"])["sessionId"]
        == source_session_id
    ), after_guarded_restarts
    assert (
        session_by_id(after_guarded_restarts, reviewer_terminal_id)["sessionId"]
        == reviewer_session_id
    ), after_guarded_restarts

    first_ready, first_response = run_turn(
        port=port,
        token=token,
        process=process,
        events=events,
        thread_id=thread_id,
        source=source,
        reviewer=reviewer,
        sequence=1,
        stdout_log=stdout_log,
        stderr_log=stderr_log,
    )
    assert artifact_field(first_response, "review-count") == "1", first_response
    reviewer_process = artifact_field(first_response, "reviewer-process")
    reviewer_process_pid = int(reviewer_process)
    assert reviewer_process_pid == reviewer_fixture_process.pid
    assert owned_process_is_running(reviewer_root_process)
    assert owned_process_is_running(reviewer_fixture_process)

    recheck = request_json(
        port,
        token,
        f"/api/peer/threads/{thread_id}/turns",
        method="POST",
        payload={
            "action": "recheck",
            "instruction": "Recheck the revised synthetic handoff.",
            "sourceReady": True,
        },
        expected_status=201,
    )
    assert recheck["reviewerTerminalId"] == reviewer_terminal_id, recheck
    assert recheck["currentTurn"]["sequence"] == 2, recheck

    second_ready, second_response = run_turn(
        port=port,
        token=token,
        process=process,
        events=events,
        thread_id=thread_id,
        source=source,
        reviewer=reviewer,
        sequence=2,
        stdout_log=stdout_log,
        stderr_log=stderr_log,
    )
    assert artifact_field(second_response, "review-count") == "2", second_response
    assert (
        artifact_field(second_response, "reviewer-process") == reviewer_process
    ), second_response

    after_recheck = sessions(port, token)
    reviewer_after_recheck = session_by_id(after_recheck, reviewer_terminal_id)
    assert reviewer_after_recheck["sessionId"] == reviewer_session_id
    assert reviewer_after_recheck["pid"] == reviewer_pid
    source_after_recheck = session_by_id(after_recheck, source["terminalId"])
    assert source_after_recheck["sessionId"] == source_session_id
    assert source_after_recheck["status"] == "running"

    request_json(
        port,
        token,
        f"/api/peer/threads/{thread_id}",
        method="DELETE",
        expected_status=204,
    )
    wait_for(
        "the dedicated reviewer process tree to exit",
        process,
        events,
        lambda: (
            True
            if not owned_process_is_running(reviewer_root_process)
            and not owned_process_is_running(reviewer_fixture_process)
            else None
        ),
        stdout_log=stdout_log,
        stderr_log=stderr_log,
        token=token,
        timeout=10,
    )
    closed_status, _ = http_request(
        port,
        f"/api/peer/threads/{thread_id}",
        token=token,
    )
    assert closed_status == 404, closed_status
    remaining_threads = request_json(port, token, "/api/peer/threads")
    assert all(thread.get("id") != thread_id for thread in remaining_threads)

    remaining_sessions = sessions(port, token)
    remaining_ids = {session["terminalId"] for session in remaining_sessions}
    assert reviewer_terminal_id not in remaining_ids, remaining_sessions
    assert ordinary["terminalId"] in remaining_ids, remaining_sessions
    assert source["terminalId"] in remaining_ids, remaining_sessions
    source_final = session_by_id(remaining_sessions, source["terminalId"])
    assert source_final["sessionId"] == source_session_id, source_final
    assert source_final["status"] == "running", source_final
    assert owned_process_is_running(source_root_process)
    assert owned_process_is_running(source_fixture_process)
    source_attachment.send(0x2, b"[CWT regression health]\r")
    source_health = wait_for_event(
        process,
        events,
        source["terminalId"],
        "health",
        0,
        stdout_log=stdout_log,
        stderr_log=stderr_log,
        token=token,
    )
    assert source_health["sessionId"] == source_session_id, source_health
    assert int(source_health["process"]) == source_fixture_process.pid

    return {
        "threadPurged": True,
        "freshReviewer": reviewer_terminal_id != ordinary["terminalId"],
        "restartControlsBlocked": True,
        "sourceStayedRunning": True,
        "firstTurn": {
            "status": first_ready["status"],
            "handoffRevision": first_ready["currentTurn"]["handoffRevision"],
        },
        "recheck": {
            "status": second_ready["status"],
            "sameTerminal": True,
            "sameSession": True,
            "sameProcess": True,
        },
        "remainingSessionCount": len(remaining_sessions),
    }


def terminate_owned_process(process: OwnedProcess) -> None:
    if not owned_process_is_running(process):
        return

    if os.name == "nt":
        completed = subprocess.run(
            ["taskkill", "/PID", str(process.pid), "/T", "/F"],
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            check=False,
            creationflags=subprocess.CREATE_NO_WINDOW,
            text=True,
        )
        if completed.returncode != 0 and owned_process_is_running(process):
            raise RuntimeError(
                f"taskkill failed for {process.label} PID {process.pid}: "
                f"{completed.stderr.strip() or completed.stdout.strip()}"
            )
    else:
        # Verify the Linux start-time marker immediately before signaling the
        # exact PID. Never signal a process group after its leader was reaped.
        if owned_process_is_running(process):
            try:
                os.kill(process.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass

    deadline = time.monotonic() + 5
    while time.monotonic() < deadline:
        if not owned_process_is_running(process):
            return
        time.sleep(0.05)
    raise RuntimeError(
        f"{process.label} PID {process.pid} remained active after teardown"
    )


def stop_owned_server(
    process: subprocess.Popen[bytes],
    port: int,
    owned_processes: list[OwnedProcess],
) -> None:
    failures: list[str] = []
    taskkill_failure = ""
    if process.poll() is None:
        if os.name == "nt":
            completed = subprocess.run(
                ["taskkill", "/PID", str(process.pid), "/T", "/F"],
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                check=False,
                creationflags=subprocess.CREATE_NO_WINDOW,
                text=True,
            )
            if completed.returncode != 0:
                taskkill_failure = (
                    completed.stderr.strip()
                    or completed.stdout.strip()
                    or f"exit code {completed.returncode}"
                )
        else:
            try:
                process.send_signal(signal.SIGINT)
            except ProcessLookupError:
                pass

        try:
            process.wait(timeout=8)
        except subprocess.TimeoutExpired:
            try:
                process.terminate()
                process.wait(timeout=4)
            except (ProcessLookupError, subprocess.TimeoutExpired):
                try:
                    process.kill()
                    process.wait(timeout=4)
                except (ProcessLookupError, subprocess.TimeoutExpired) as error:
                    failures.append(f"server PID {process.pid} did not exit: {error}")
        if process.poll() is None and taskkill_failure:
            failures.append(f"server taskkill failed: {taskkill_failure}")

    for child in reversed(owned_processes):
        try:
            terminate_owned_process(child)
        except RuntimeError as error:
            failures.append(str(error))

    deadline = time.monotonic() + 10
    while time.monotonic() < deadline:
        remaining = [
            child
            for child in owned_processes
            if owned_process_is_running(child)
        ]
        if (
            process.poll() is not None
            and not remaining
            and not port_is_listening(port)
        ):
            if failures:
                raise RuntimeError("; ".join(failures))
            return
        time.sleep(0.1)

    remaining_labels = [
        f"{child.label} PID {child.pid}"
        for child in owned_processes
        if owned_process_is_running(child)
    ]
    failures.extend(remaining_labels)
    if port_is_listening(port):
        failures.append(f"disposable port {port} is still listening")
    if process.poll() is None:
        failures.append(f"server PID {process.pid} is still running")
    raise RuntimeError("; ".join(failures) or "owned teardown did not converge")


def main() -> int:
    args = parse_args()
    args.server = args.server.resolve()
    if not args.server.is_file():
        raise FileNotFoundError(
            f"server build not found: {args.server}; build it before running this test"
        )
    assert_isolated_port(args.port)

    token = secrets.token_urlsafe(32)
    with tempfile.TemporaryDirectory(prefix="codex-web-peer-review-") as temp:
        temporary_root = Path(temp)
        project = temporary_root / "synthetic project"
        state_dir = temporary_root / "state"
        events = temporary_root / "events"
        project.mkdir()
        events.mkdir()
        fixture = write_fixture_command(temporary_root)
        stdout_log = temporary_root / "server.stdout.log"
        stderr_log = temporary_root / "server.stderr.log"
        command = [
            str(args.server),
            "--host",
            "127.0.0.1",
            "--port",
            str(args.port),
            "--project",
            str(project),
            "--state-dir",
            str(state_dir),
            "--command",
            str(fixture),
            "--new-session-command",
            str(fixture),
            "--claude-command",
            str(fixture),
            "--agy-command",
            str(fixture),
            "--no-agent-auto-detect",
            "--no-open-browser",
            "--log-level",
            "warn",
        ]
        creation_flags = subprocess.CREATE_NO_WINDOW if os.name == "nt" else 0
        with (
            stdout_log.open("wb") as stdout_handle,
            stderr_log.open("wb") as stderr_handle,
        ):
            server = subprocess.Popen(
                command,
                stdin=subprocess.DEVNULL,
                stdout=stdout_handle,
                stderr=stderr_handle,
                env=isolated_server_environment(temporary_root, events, token),
                creationflags=creation_flags,
                start_new_session=os.name != "nt",
            )
            attachments: list[WebSocketAttachment] = []
            owned_processes: list[OwnedProcess] = []
            try:
                wait_for_server(
                    args.port,
                    token,
                    server,
                    events,
                    stdout_log=stdout_log,
                    stderr_log=stderr_log,
                )
                result = exercise_peer_flow(
                    args.port,
                    token,
                    server,
                    events,
                    project,
                    attachments,
                    owned_processes,
                    stdout_log=stdout_log,
                    stderr_log=stderr_log,
                )
                assert_fixture_has_no_error(events)
                print(
                    json.dumps(
                        {
                            "port": args.port,
                            "protectedLivePorts": sorted(PROTECTED_LIVE_PORTS),
                            "platform": os.name,
                            "syntheticAgentsOnly": True,
                            "peer": result,
                        },
                        indent=2,
                    )
                )
                return 0
            finally:
                close_terminal_attachments(attachments)
                stop_owned_server(server, args.port, owned_processes)


if __name__ == "__main__":
    raise SystemExit(main())
