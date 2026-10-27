#!/usr/bin/env python3
"""Regression for the "Continue in <agent>" context transfer.

Runs the packaged server with a synthetic agent on a disposable loopback port
and exercises the public transfer API plus the private helper protocol:

- automated preparation (source submits through `__cwt-peer transfer-submit`),
  idempotent create, conflicting create, late submit with an old attempt;
- manual override of a pending preparation and manual drafts from a stopped
  source;
- new-target start (published at reservation, selected by the browser later),
  duplicate target requests returning the same target, changed parameters
  conflicting, a second attempt refused while one is unsettled;
- existing-target binding with exact generation, wrong generation refused,
  the source refused as its own target;
- explicit Deliver with readiness/revision/generation checks, helper
  `transfer-receive` (write, flush, acknowledge) and `transfer-ack`
  (idempotent after), wrong-role helper calls refused, status-only receive
  after delivery;
- lifecycle exclusion: restart/terminate/delete of a bound target and a
  preparing source are refused; peer-thread creation on a transfer source is
  refused and vice versa;
- target end before delivery keeps the draft and settles the attempt;
- discard during delivery; abandon idempotency; capacity refusal for new
  targets while existing targets stay allowed.

Reuses the HTTP/WebSocket/server helpers from peer-review-regression.py. Only
synthetic agents are used; no real CLI, credentials, or transcripts.
"""
from __future__ import annotations

import importlib.util
import json
import os
import re
import secrets
import shlex
import subprocess
import sys
import tempfile
import time
import uuid
from pathlib import Path
from typing import Any

REPOSITORY = Path(__file__).resolve().parents[1]
_spec = importlib.util.spec_from_file_location(
    "peer_regression", REPOSITORY / "scripts" / "peer-review-regression.py"
)
peer = importlib.util.module_from_spec(_spec)
assert _spec.loader is not None
sys.modules["peer_regression"] = peer
_spec.loader.exec_module(peer)

http_request = peer.http_request
request_json = peer.request_json
sessions = peer.sessions
session_by_id = peer.session_by_id
wait_for = peer.wait_for
WebSocketAttachment = peer.WebSocketAttachment
attach_terminal = peer.attach_terminal
DEFAULT_PORT = 8828


def parse_args():
    import argparse

    executable_name = "codex-web.exe" if os.name == "nt" else "codex-web"
    default_server = REPOSITORY / "server" / "target" / "release" / executable_name
    parser = argparse.ArgumentParser(
        description="Validate the context transfer API, helper protocol, and lifecycle exclusion."
    )
    parser.add_argument("--server", type=Path, default=default_server)
    parser.add_argument("--port", type=int, default=DEFAULT_PORT)
    return parser.parse_args()


# --------------------------------------------------------------------------
# Synthetic agent: understands the transfer prompts and drives the helper.
# --------------------------------------------------------------------------

def write_fixture_command(root: Path) -> Path:
    fixture = root / "synthetic-transfer-agent.py"
    fixture_source = r'''#!/usr/bin/env python3
from __future__ import annotations
import hashlib, json, os, re, subprocess, sys, time
from pathlib import Path

if len(sys.argv) == 2 and sys.argv[1] in {"--version", "--help"}:
    if "CODEX_WEB_TOKEN" in os.environ:
        print("server bearer token leaked into a CLI probe", file=sys.stderr)
        raise SystemExit(90)
    print("synthetic-transfer-agent 1.0.0" if sys.argv[1] == "--version" else "Options:\n  --no-daemon", flush=True)
    raise SystemExit(0)

EVENTS = Path(__CWT_EVENT_DIRECTORY__)
TERMINAL_ID = os.environ.get("CWT_TERMINAL_ID", f"missing-{os.getpid()}")
SESSION_ID = os.environ.get("CWT_SESSION_ID", "missing")
HELPER = os.environ.get("CWT_PEER_HELPER", "")
TRANSFER = re.compile(r"--transfer ([0-9a-fA-F-]{36})")
ATTEMPT = re.compile(r"--attempt ([0-9a-fA-F-]{36})")
counter = 0


def write_event(kind: str, key: str, payload: dict) -> None:
    target = EVENTS / f"{TERMINAL_ID}-{kind}-{key}.json"
    tmp = target.with_name(f".{target.name}.{os.getpid()}.tmp")
    tmp.write_text(json.dumps(payload, sort_keys=True), encoding="utf-8")
    os.replace(tmp, target)


def write_error(message: str) -> None:
    (EVENTS / f"error-{TERMINAL_ID}-{os.getpid()}-{time.time_ns()}.json").write_text(
        json.dumps({"terminalId": TERMINAL_ID, "sessionId": SESSION_ID, "message": message}), encoding="utf-8")


def run_helper(args: list[str], content: str | None = None) -> subprocess.CompletedProcess:
    return subprocess.run([HELPER, "__cwt-peer", *args], input=content, stdout=subprocess.PIPE,
        stderr=subprocess.PIPE, text=True, encoding="utf-8", timeout=15, check=False,
        creationflags=(subprocess.CREATE_NO_WINDOW if os.name == "nt" else 0))


def broker_status(completed) -> int | None:
    match = re.search(r"request failed with HTTP (\d{3})", completed.stderr)
    return int(match.group(1)) if match else None


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


def prepare(prompt: str) -> None:
    global counter
    counter += 1
    transfer_id = TRANSFER.search(prompt).group(1)
    attempt_id = ATTEMPT.search(prompt).group(1)
    # Wait for the test to allow/deny the submission so it can race overrides.
    deadline = time.monotonic() + 30
    while not (EVENTS / f"{transfer_id}-allow-submit").is_file():
        if (EVENTS / f"{transfer_id}-hold-submit").is_file():
            time.sleep(0.05)
            if time.monotonic() > deadline:
                raise TimeoutError("submit hold never released")
            continue
        break
    handoff = "\n".join(("# Synthetic transfer handoff", f"source-terminal: {TERMINAL_ID}",
        f"source-session: {SESSION_ID}", f"attempt: {attempt_id}", f"workspace: {os.getcwd()}"))
    first = run_helper(["transfer-submit", "--transfer", transfer_id, "--attempt", attempt_id, "--stdin"], handoff)
    # Repeat identical submit is idempotent; a different body for the consumed attempt is refused.
    second = run_helper(["transfer-submit", "--transfer", transfer_id, "--attempt", attempt_id, "--stdin"], handoff)
    third = run_helper(["transfer-submit", "--transfer", transfer_id, "--attempt", attempt_id, "--stdin"], handoff + "\nchanged")
    # The source is not a target: receive must be refused without content.
    wrong_role = run_helper(["transfer-receive", "--transfer", transfer_id])
    write_event("transfer-submitted", transfer_id, {
        "attemptId": attempt_id, "firstExit": first.returncode, "firstStatus": broker_status(first),
        "secondExit": second.returncode, "thirdExit": third.returncode, "thirdStatus": broker_status(third),
        "wrongRoleExit": wrong_role.returncode, "wrongRoleStatus": broker_status(wrong_role),
        "wrongRoleStdout": wrong_role.stdout, "handoffSha256": hashlib.sha256(handoff.encode()).hexdigest()})


def receive(prompt: str) -> None:
    transfer_id = TRANSFER.search(prompt).group(1)
    if (EVENTS / f"{transfer_id}-hold-receive").is_file():
        deadline = time.monotonic() + 30
        while (EVENTS / f"{transfer_id}-hold-receive").is_file():
            if time.monotonic() > deadline:
                raise TimeoutError("receive hold never released")
            time.sleep(0.05)
    if (EVENTS / f"{transfer_id}-expect-discarded").is_file():
        refused = run_helper(["transfer-receive", "--transfer", transfer_id])
        write_event("transfer-receive-refused", transfer_id, {"exit": refused.returncode,
            "status": broker_status(refused), "stdout": refused.stdout})
        return
    # The helper writes, flushes, then acknowledges in one call. A manual
    # transfer-ack afterwards is idempotent, and a later receive is status-only
    # (no content) because the content grant was retired at acknowledgement.
    before = run_helper(["transfer-receive", "--transfer", transfer_id])
    ack = run_helper(["transfer-ack", "--transfer", transfer_id])
    after = run_helper(["transfer-receive", "--transfer", transfer_id])
    write_event("transfer-received", transfer_id, {
        "firstExit": before.returncode, "first": before.stdout, "ackExit": ack.returncode,
        "afterExit": after.returncode, "afterStatus": broker_status(after), "afterStdout": after.stdout,
        "sha256": hashlib.sha256(before.stdout.rstrip("\r\n").encode()).hexdigest(), "pid": os.getpid()})


missing = [n for n in ("CWT_TERMINAL_ID", "CWT_SESSION_ID", "CWT_PEER_HELPER") if n not in os.environ]
if missing:
    write_error("missing peer environment: " + ", ".join(missing)); raise SystemExit(2)
if "CODEX_WEB_TOKEN" in os.environ:
    write_error("server bearer token leaked into the managed PTY"); raise SystemExit(2)
print(f"SYNTHETIC-AGENT-READY {TERMINAL_ID}", flush=True)
write_event("ready", "0", {"process": os.getpid(), "sessionId": SESSION_ID})
for prompt in input_prompts():
    try:
        if prompt == "[CWT regression health]":
            write_event("health", "0", {"process": os.getpid(), "sessionId": SESSION_ID})
        elif "[CWT transfer] Prepare a concise, self-contained Markdown handoff" in prompt:
            prepare(prompt)
        elif "[CWT transfer] Context transferred from another agent session is ready." in prompt:
            receive(prompt)
        elif "[CWT peer" in prompt:
            write_event("peer-prompt-observed", str(time.time_ns()), {"length": len(prompt)})
    except Exception as error:
        write_error(f"{type(error).__name__}: {error}")
'''
    fixture.write_text(
        fixture_source.replace("__CWT_EVENT_DIRECTORY__", repr(str(root / "events"))),
        encoding="utf-8",
    )
    if os.name == "nt":
        command = root / "synthetic-transfer-agent.cmd"
        python = subprocess.list2cmdline([sys.executable])
        script = subprocess.list2cmdline([str(fixture)])
        command.write_text(
            f"@echo off\r\n{python} -u {script} %*\r\nexit /b %ERRORLEVEL%\r\n",
            encoding="utf-8",
            newline="",
        )
        return command
    command = root / "synthetic-transfer-agent"
    command.write_text(
        "#!/bin/sh\n"
        f"exec {shlex.quote(sys.executable)} -u {shlex.quote(str(fixture))} \"$@\"\n",
        encoding="utf-8",
    )
    command.chmod(0o700)
    return command


# --------------------------------------------------------------------------
# Test helpers
# --------------------------------------------------------------------------

class Harness:
    def __init__(self, port: int, token: str, server, events: Path, stdout_log: Path, stderr_log: Path):
        self.port, self.token, self.server, self.events = port, token, server, events
        self.stdout_log, self.stderr_log = stdout_log, stderr_log
        self.attachments: list[Any] = []

    def api(self, path: str, method: str = "GET", payload=None, expected: int | None = None):
        status, body = http_request(self.port, path, token=self.token, method=method, payload=payload)
        if expected is not None and status != expected:
            raise AssertionError(f"{method} {path} returned HTTP {status}: {body!r}")
        return status, body

    def wait(self, description: str, predicate, timeout: float = 30):
        return wait_for(description, self.server, self.events, predicate,
                        stdout_log=self.stdout_log, stderr_log=self.stderr_log, token=self.token, timeout=timeout)

    def wait_event(self, terminal_id: str, kind: str, key: str) -> dict[str, Any]:
        path = self.events / f"{terminal_id}-{kind}-{key}.json"

        def ready():
            if not path.is_file():
                return None
            try:
                return json.loads(path.read_text(encoding="utf-8"))
            except json.JSONDecodeError:
                return None

        return self.wait(f"synthetic event {kind} for {terminal_id}", ready)

    def wait_transfer(self, transfer_id: str, state: str, timeout: float = 30) -> dict[str, Any]:
        def ready():
            status, body = self.api(f"/api/transfers/{transfer_id}")
            if status != 200:
                raise AssertionError(f"transfer lookup HTTP {status}: {body!r}")
            return body if body.get("state") == state else None

        return self.wait(f"transfer {transfer_id} to reach {state}", ready, timeout)

    def wait_session_status(self, terminal_id: str, status: str) -> dict[str, Any]:
        def ready():
            found = session_by_id(sessions(self.port, self.token), terminal_id)
            return found if found and found.get("status") == status else None

        return self.wait(f"session {terminal_id} to become {status}", ready)

    def ready_agent(self, terminal_id: str) -> dict[str, Any]:
        # Attach so output flows (keeps the replay buffer draining) and wait for
        # the synthetic ready event of the *current* generation.
        self.attachments.append(attach_terminal(self.port, self.token, terminal_id))
        return self.wait_event(terminal_id, "ready", "0")

    def create(self, source: dict[str, Any], mode: str, *, transfer_id: str | None = None,
               ready: bool = True, instruction: str = "Continue the synthetic work.", expected: int = 201):
        transfer_id = transfer_id or str(uuid.uuid4())
        status, body = self.api("/api/transfers", "POST", {
            "transferId": transfer_id, "sourceTerminalId": source["terminalId"],
            "sourceSessionId": source.get("sessionId"), "mode": mode,
            "sourceReady": ready, "instruction": instruction}, expected)
        return transfer_id, body

    def draft(self, transfer_id: str, expected_revision: int, content: str, expected: int = 200):
        return self.api(f"/api/transfers/{transfer_id}/draft", "PUT",
                        {"expectedRevision": expected_revision, "content": content}, expected)

    def target(self, transfer_id: str, target: dict[str, Any], *, request_id: str | None = None, expected: int = 200):
        request_id = request_id or str(uuid.uuid4())
        status, body = self.api(f"/api/transfers/{transfer_id}/target", "POST",
                                {"targetRequestId": request_id, "target": target}, expected)
        return request_id, body

    def deliver(self, transfer_id: str, revision: int, session_id: str, *, ready: bool = True, expected: int = 200):
        return self.api(f"/api/transfers/{transfer_id}/deliver", "POST",
                        {"expectedRevision": revision, "targetSessionId": session_id, "targetReady": ready}, expected)


def assert_code(body: Any, code: str) -> None:
    assert isinstance(body, dict) and body.get("code") == code, f"expected error code {code}, got {body!r}"


# --------------------------------------------------------------------------
# Scenarios
# --------------------------------------------------------------------------

def exercise(h: Harness, project: Path, other_project: Path) -> dict[str, Any]:
    result: dict[str, Any] = {}
    primary = next(s for s in sessions(h.port, h.token) if s["isPrimary"])
    h.ready_agent(primary["terminalId"])
    primary = session_by_id(sessions(h.port, h.token), primary["terminalId"])
    source_id, source_gen = primary["terminalId"], primary["sessionId"]

    # ---- 1. Automated preparation, helper submit, idempotent/conflicting create.
    (h.events / "ignored").mkdir(exist_ok=True)
    t1, created = h.create(primary, "prepare")
    assert created["state"] == "preparing" and created["preparation"] == "pending", created
    _, again = h.create(primary, "prepare", transfer_id=t1)
    assert again["state"] == "preparing", again
    _, conflict = h.create(primary, "manual", transfer_id=t1, ready=False, expected=409)
    assert_code(conflict, "transfer_conflict")
    # While preparing: lifecycle and peer admission on the source are refused.
    status, body = h.api(f"/api/sessions/{source_id}/restart", "POST")
    assert status == 409 and body.get("code") == "participant_busy", (status, body)
    status, body = h.api("/api/peer/threads", "POST", {
        "sourceTerminalId": source_id, "directoryId": primary["directoryId"], "targetAgent": "claude",
        "action": "ask", "instruction": "x", "sourceReady": True})
    assert status == 409, (status, body)
    result["preparingBlocksLifecycleAndPeer"] = True
    submitted = h.wait_event(source_id, "transfer-submitted", t1)
    assert submitted["firstExit"] == 0, submitted
    assert submitted["secondExit"] == 0, submitted  # identical duplicate accepted
    assert submitted["thirdExit"] != 0 and submitted["thirdStatus"] == 409, submitted
    assert submitted["wrongRoleExit"] != 0 and submitted["wrongRoleStatus"] == 401 and not submitted["wrongRoleStdout"], submitted
    preview = h.wait_transfer(t1, "preview")
    assert preview["preparation"] == "submitted" and preview["draftRevision"] == 1, preview
    status, draft = h.api(f"/api/transfers/{t1}/draft", expected=200)
    assert draft["content"].startswith("# Synthetic transfer handoff"), draft
    assert f"attempt: {submitted['attemptId']}" in draft["content"]
    # The source is released: restart is allowed again (we don't do it; check peer admission instead).
    result["preparedViaHelper"] = True

    # ---- 2. Source cannot be its own target; existing target with wrong generation.
    _, body = h.target(t1, {"mode": "existing", "terminalId": source_id, "sessionId": source_gen}, expected=409)
    assert_code(body, "target_not_eligible")
    other = request_json(h.port, h.token, "/api/sessions", method="POST", payload={"agent": "codex"}, expected_status=201)
    h.ready_agent(other["terminalId"])
    other = session_by_id(sessions(h.port, h.token), other["terminalId"])
    _, body = h.target(t1, {"mode": "existing", "terminalId": other["terminalId"], "sessionId": str(uuid.uuid4())}, expected=409)
    assert_code(body, "generation_conflict")

    # ---- 3. Bind the existing tab; lifecycle refused while bound; deliver checks.
    req, bound = h.target(t1, {"mode": "existing", "terminalId": other["terminalId"], "sessionId": other["sessionId"]})
    assert bound["state"] == "target_bound" and bound["attempt"]["target"]["mode"] == "existing", bound
    _, same = h.target(t1, {"mode": "existing", "terminalId": other["terminalId"], "sessionId": other["sessionId"]}, request_id=req)
    assert same["attempt"]["targetRequestId"] == req and same["attemptsUsed"] == 1, same
    _, changed = h.target(t1, {"mode": "new", "agent": "codex", "directoryId": primary["directoryId"]}, request_id=req, expected=409)
    assert_code(changed, "transfer_conflict")
    status, body = h.api(f"/api/sessions/{other['terminalId']}", "DELETE")
    assert status == 409 and body.get("code") == "participant_busy", (status, body)
    status, body = h.api(f"/api/sessions/{other['terminalId']}/terminate", "POST")
    assert status == 409, (status, body)
    _, body = h.deliver(t1, 1, other["sessionId"], ready=False, expected=400)
    assert_code(body, "invalid_input")
    _, body = h.deliver(t1, 0, other["sessionId"], expected=409)
    assert_code(body, "revision_conflict")
    _, body = h.deliver(t1, 1, str(uuid.uuid4()), expected=409)
    assert_code(body, "generation_conflict")
    # Edit the draft while bound (allowed), then deliver with the new revision.
    _, edited = h.draft(t1, 1, draft["content"] + "\napproved-by-test: yes")
    assert edited["draftRevision"] == 2, edited
    _, delivering = h.deliver(t1, 2, other["sessionId"])
    assert delivering["state"] == "delivering" and delivering["approvedRevision"] == 2, delivering
    _, frozen = h.draft(t1, 2, "late edit", expected=409)
    assert_code(frozen, "invalid_state")
    received = h.wait_event(other["terminalId"], "transfer-received", t1)
    assert received["firstExit"] == 0, received
    assert received["ackExit"] == 0, received  # idempotent after the helper's own ack
    assert received["afterExit"] != 0 and received["afterStatus"] == 409 and not received["afterStdout"], received
    assert "approved-by-test: yes" in received["first"], received
    delivered = h.wait_transfer(t1, "delivered")
    assert delivered["attempt"]["outcome"] == "delivered", delivered
    status, after = h.api(f"/api/transfers/{t1}/draft", expected=200)
    assert after["content"] is None, after  # text erased on terminal state
    # Target is user-owned again.
    h.api(f"/api/sessions/{other['terminalId']}", "DELETE", expected=204)
    result["existingTargetDelivered"] = True

    # ---- 4. Manual override racing a pending preparation + late submit ignored.
    (h.events / f"placeholder").touch()
    t2, created = h.create(primary, "prepare")
    assert created["state"] == "preparing", created
    # Hold the synthetic submit so the override lands first.
    hold = h.events / f"{t2}-hold-submit"
    hold.touch()
    _, overridden = h.draft(t2, 0, "# Written by hand")
    assert overridden["state"] == "preview" and overridden["preparation"] == "overridden", overridden
    hold.unlink()
    late = h.wait_event(source_id, "transfer-submitted", t2)
    assert late["firstExit"] != 0 and late["firstStatus"] == 409, late  # attempt invalidated
    status, draft2 = h.api(f"/api/transfers/{t2}/draft", expected=200)
    assert draft2["content"] == "# Written by hand" and draft2["draftRevision"] == 1, draft2
    result["manualOverrideIgnoresLateSubmit"] = True

    # ---- 5. New target: published at reservation; duplicate waits/returns same; second attempt refused.
    req, starting = h.target(t2, {"mode": "new", "agent": "claude", "directoryId": primary["directoryId"]})
    # The route awaits the start, so by now it is bound (or failed).
    assert starting["state"] == "target_bound", starting
    target = starting["attempt"]["target"]
    assert target["mode"] == "new" and target["agent"] == "claude" and target["sessionId"], target
    listed = session_by_id(sessions(h.port, h.token), target["terminalId"])
    assert listed and listed["purpose"]["kind"] == "interactive" and listed["agent"] == "claude", listed
    assert listed["project"] == primary["project"], listed
    _, dup = h.target(t2, {"mode": "new", "agent": "claude", "directoryId": primary["directoryId"]}, request_id=req)
    assert dup["attempt"]["target"]["terminalId"] == target["terminalId"] and dup["attemptsUsed"] == 1, dup
    _, second = h.target(t2, {"mode": "new", "agent": "claude", "directoryId": primary["directoryId"]}, expected=409)
    assert_code(second, "invalid_state")
    h.ready_agent(target["terminalId"])
    # ---- 6. Target ends before delivery: attempt settles, draft kept, new target allowed.
    # Terminate is refused while bound; make the agent exit instead by deleting... also refused.
    status, body = h.api(f"/api/sessions/{target['terminalId']}/terminate", "POST")
    assert status == 409, (status, body)
    # Kill the synthetic agent process directly (external exit).
    ready_event = h.wait_event(target["terminalId"], "ready", "0")
    kill_pid(int(ready_event["process"]))
    ended = h.wait_transfer(t2, "preview")
    assert ended["delivery"] == "target_ended" and ended["attempt"]["outcome"] == "target_ended", ended
    assert ended["error"]["code"] == "target_ended", ended
    status, draft2 = h.api(f"/api/transfers/{t2}/draft", expected=200)
    assert draft2["content"] == "# Written by hand", draft2
    # Ended target is an ordinary stopped tab the user owns.
    h.wait_session_status(target["terminalId"], "exited")
    h.api(f"/api/sessions/{target['terminalId']}", "DELETE", expected=204)
    result["targetEndKeepsDraft"] = True

    # ---- 7. Deliver to a fresh new target, then discard mid-delivery.
    req, bound2 = h.target(t2, {"mode": "new", "agent": "kilo", "directoryId": primary["directoryId"]})
    assert bound2["state"] == "target_bound" and bound2["attemptsUsed"] == 2, bound2
    target2 = bound2["attempt"]["target"]
    h.ready_agent(target2["terminalId"])
    (h.events / f"{t2}-hold-receive").touch()
    (h.events / f"{t2}-expect-discarded").touch()
    _, delivering2 = h.deliver(t2, 1, target2["sessionId"])
    assert delivering2["state"] == "delivering", delivering2
    status, discarded = h.api(f"/api/transfers/{t2}/abandon", "POST", {"reason": "user"}, expected=200)
    assert discarded["state"] == "abandoned", discarded
    (h.events / f"{t2}-hold-receive").unlink()
    refused = h.wait_event(target2["terminalId"], "transfer-receive-refused", t2)
    assert refused["exit"] != 0 and refused["status"] == 409 and not refused["stdout"], refused
    status, idem = h.api(f"/api/transfers/{t2}/abandon", "POST", {"reason": "user"}, expected=200)
    assert idem["state"] == "abandoned", idem
    h.api(f"/api/sessions/{target2['terminalId']}", "DELETE", expected=204)
    result["discardDuringDelivery"] = True

    # ---- 8. Manual draft from a stopped source; prepare refused; peer vs transfer exclusion.
    stopped = request_json(h.port, h.token, "/api/sessions", method="POST", payload={"agent": "agy"}, expected_status=201)
    h.ready_agent(stopped["terminalId"])
    h.api(f"/api/sessions/{stopped['terminalId']}/terminate", "POST", expected=204)
    stopped = h.wait_session_status(stopped["terminalId"], "terminated")
    _, refused = h.create(stopped, "prepare", expected=409)
    assert_code(refused, "source_not_running")
    t3, manual = h.create(stopped, "manual", ready=False)
    assert manual["state"] == "preview", manual
    h.draft(t3, 0, "# From a stopped source")
    # A transfer source (while preparing) excludes peer threads and vice versa.
    other2 = request_json(h.port, h.token, "/api/sessions", method="POST", payload={"agent": "codex"}, expected_status=201)
    h.ready_agent(other2["terminalId"])
    other2 = session_by_id(sessions(h.port, h.token), other2["terminalId"])
    (h.events / f"placeholder2").touch()
    thread = request_json(h.port, h.token, "/api/peer/threads", method="POST", expected_status=201, payload={
        "sourceTerminalId": other2["terminalId"], "directoryId": other2["directoryId"], "targetAgent": "claude",
        "action": "ask", "instruction": "Synthetic peer thread.", "sourceReady": True})
    _, busy = h.create(other2, "prepare", expected=409)
    assert_code(busy, "participant_busy")
    # Manual drafting from that source is still allowed (no claim).
    t4, _ = h.create(other2, "manual", ready=False)
    h.api(f"/api/transfers/{t4}/abandon", "POST", {"reason": "user"}, expected=200)
    h.api(f"/api/peer/threads/{thread['id']}", "DELETE")
    h.api(f"/api/transfers/{t3}/abandon", "POST", {"reason": "user"}, expected=200)
    h.api(f"/api/sessions/{stopped['terminalId']}", "DELETE", expected=204)
    result["stoppedSourceManualAndPeerExclusion"] = True

    # ---- 9. Capacity: new targets refused, existing allowed. Fill to the configured limit (4).
    primary_now = session_by_id(sessions(h.port, h.token), source_id)
    t5, _ = h.create(primary_now, "manual", ready=False)
    h.draft(t5, 0, "# Capacity")
    count = len(sessions(h.port, h.token))
    fillers = []
    while count < 4:
        filler = request_json(h.port, h.token, "/api/sessions", method="POST", payload={"agent": "codex"}, expected_status=201)
        fillers.append(filler)
        count += 1
    _, full = h.target(t5, {"mode": "new", "agent": "codex", "directoryId": primary["directoryId"]}, expected=409)
    assert_code(full, "capacity_full")
    h.ready_agent(other2["terminalId"])
    other2 = session_by_id(sessions(h.port, h.token), other2["terminalId"])
    _, bound3 = h.target(t5, {"mode": "existing", "terminalId": other2["terminalId"], "sessionId": other2["sessionId"]})
    assert bound3["state"] == "target_bound", bound3
    h.api(f"/api/transfers/{t5}/abandon", "POST", {"reason": "user"}, expected=200)
    for filler in fillers:
        h.api(f"/api/sessions/{filler['terminalId']}", "DELETE", expected=204)
    h.api(f"/api/sessions/{other2['terminalId']}", "DELETE", expected=204)
    result["capacityRefusesNewAllowsExisting"] = True

    # ---- 10. Ledger listing by source/target and unknown id.
    status, listed = h.api(f"/api/transfers?source={source_id}", expected=200)
    assert {entry["transferId"] for entry in listed} >= {t1, t2, t5}, listed
    status, _ = h.api(f"/api/transfers/{uuid.uuid4()}", expected=404)
    result["ledgerListing"] = True
    return result


def kill_pid(pid: int) -> None:
    if os.name == "nt":
        subprocess.run(["taskkill", "/PID", str(pid), "/T", "/F"], stdout=subprocess.DEVNULL,
                       stderr=subprocess.DEVNULL, check=False, creationflags=subprocess.CREATE_NO_WINDOW)
    else:
        import signal
        try:
            os.kill(pid, signal.SIGKILL)
        except ProcessLookupError:
            pass


def main() -> int:
    args = parse_args()
    args.server = args.server.resolve()
    if not args.server.is_file():
        raise FileNotFoundError(f"server build not found: {args.server}")
    peer.assert_isolated_port(args.port)
    token = secrets.token_urlsafe(32)
    with tempfile.TemporaryDirectory(prefix="codex-web-transfer-") as temp:
        root = Path(temp)
        project = root / "synthetic project"
        other_project = root / "other project"
        state_dir = root / "state"
        events = root / "events"
        project.mkdir(); other_project.mkdir(); events.mkdir()
        fixture = write_fixture_command(root)
        stdout_log, stderr_log = root / "server.stdout.log", root / "server.stderr.log"
        command = [str(args.server), "--host", "127.0.0.1", "--port", str(args.port), "--project", str(project),
                   "--state-dir", str(state_dir), "--command", str(fixture), "--new-session-command", str(fixture),
                   "--claude-command", str(fixture), "--agy-command", str(fixture), "--kilo-command", str(fixture),
                   "--max-sessions", "4", "--no-agent-auto-detect", "--no-open-browser", "--log-level", "warn"]
        creation_flags = subprocess.CREATE_NO_WINDOW if os.name == "nt" else 0
        with stdout_log.open("wb") as out, stderr_log.open("wb") as err:
            server = subprocess.Popen(command, stdin=subprocess.DEVNULL, stdout=out, stderr=err,
                                      env=peer.isolated_server_environment(root, events, token),
                                      creationflags=creation_flags, start_new_session=os.name != "nt")
            harness = Harness(args.port, token, server, events, stdout_log, stderr_log)
            owned: list[Any] = []
            try:
                peer.wait_for_server(args.port, token, server, events, stdout_log=stdout_log, stderr_log=stderr_log)
                result = exercise(harness, project, other_project)
                peer.assert_fixture_has_no_error(events)
                print(json.dumps({"port": args.port, "platform": os.name, "syntheticAgentsOnly": True,
                                  "transfer": result}, indent=2))
                return 0
            finally:
                peer.close_terminal_attachments(harness.attachments)
                peer.stop_owned_server(server, args.port, owned)


if __name__ == "__main__":
    raise SystemExit(main())
