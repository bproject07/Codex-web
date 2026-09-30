#!/usr/bin/env python3
"""Exercise xterm retention, peer UI, and mobile layout with synthetic peers.

Serves built assets on a disposable loopback port; never starts a PTY, reads
host terminal content, or attaches to an existing server.
"""

from __future__ import annotations

import argparse
from functools import partial
from http.server import SimpleHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import threading
from time import monotonic
from typing import Any, Callable
from urllib.parse import parse_qs, urlsplit
from uuid import UUID

from playwright.sync_api import Browser, Locator, Page, Route, WebSocketRoute, sync_playwright


PREFERENCE = "Keep terminals when switching tabs"
ACTIVE_PANE = ".terminal-pane:not([hidden])"
TOKEN = "synthetic-tab-cache-token"


def wait_until(page: Page, condition: Callable[[], bool]) -> None:
    deadline = monotonic() + 10
    while not condition():
        if monotonic() >= deadline:
            raise AssertionError("Synthetic browser event did not arrive")
        # Pump Playwright's event loop so routed HTTP/WS callbacks can run.
        page.wait_for_timeout(20)


def session(index: int) -> dict[str, Any]:
    return {
        "terminalId": str(UUID(int=index + 1)),
        "sessionId": str(UUID(int=100 + index)),
        "name": f"Codex {index + 1}",
        "agent": "codex",
        "isPrimary": index == 0,
        "purpose": {"kind": "interactive"},
        "createdAt": index + 1,
        "startedAt": index + 1,
        "status": "running",
        "connected": True,
        "connectedClients": 1,
        "pid": 1000 + index,
        "exitCode": None,
        "lastError": None,
        "project": f"/workspace/synthetic-{index + 1}",
        "directoryId": f"synthetic-directory-{index + 1}",
    }


class SyntheticPeers:
    def __init__(self, page: Page) -> None:
        self.page = page
        self.sessions = [session(index) for index in range(8)]
        self.connections: list[dict[str, Any]] = []
        self.health_reads = 0
        self.session_reads = 0
        self.restart_request: Route | None = None
        self.hold_sessions = False
        self.held_sessions: list[Route] = []
        self.session_error: int | None = None
        page.add_init_script("""(() => {
          window.syntheticSessionFetches = 0;
          const nativeFetch = window.fetch;
          window.fetch = function(...args) {
            const input = args[0];
            const url = input instanceof Request ? input.url : String(input);
            if (new URL(url, location.href).pathname === '/api/sessions') {
              window.syntheticSessionFetches += 1;
            }
            return nativeFetch.apply(this, args);
          };
          window.syntheticReplayEnds = {};
          window.syntheticBinaryFrames = {};
          window.syntheticSocketCloses = {};
          // Playwright installs its own WebSocket constructor after init
          // scripts. Observe its EventTarget dispatch without replacing it.
          const dispatch = EventTarget.prototype.dispatchEvent;
          EventTarget.prototype.dispatchEvent = function(event) {
            const result = dispatch.call(this, event);
            if (this instanceof WebSocket) {
              const id = new URL(this.url, location.href).searchParams.get('terminalId');
              if (event.type === 'close') {
                window.syntheticSocketCloses[id] = (window.syntheticSocketCloses[id] ?? 0) + 1;
              }
              if (event.type === 'message') {
                if (typeof event.data !== 'string') {
                  window.syntheticBinaryFrames[id] = (window.syntheticBinaryFrames[id] ?? 0) + 1;
                } else {
                  const message = JSON.parse(event.data);
                  if (message.type === 'replay_end') {
                    window.syntheticReplayEnds[id] = (window.syntheticReplayEnds[id] ?? 0) + 1;
                  }
                }
              }
            }
            return result;
          };
        })();""")
        page.route("**/api/**", self.http)
        page.route_web_socket(lambda url: urlsplit(url).path == "/ws", self.connect)

    def http(self, route: Route) -> None:
        path = urlsplit(route.request.url).path
        if path == "/api/sessions":
            self.session_reads += 1
            if self.hold_sessions:
                self.held_sessions.append(route)
                return
            if self.session_error is not None:
                route.fulfill(status=self.session_error, json={"error": "Synthetic attach rejection"})
                return
            payload: Any = self.sessions
        elif path.startswith("/api/sessions/") and path.endswith("/restart"):
            assert route.request.method == "POST"
            self.restart_request = route
            return
        elif path == "/api/peer/threads":
            payload = []
        else:
            if path == "/api/health":
                self.health_reads += 1
            route.fulfill(status=404, json={"error": "Synthetic endpoint unavailable"})
            return
        route.fulfill(status=200, json=payload)

    def connect(self, socket: WebSocketRoute) -> None:
        terminal_id = parse_qs(urlsplit(socket.url).query)["terminalId"][0]
        snapshot = next(item for item in self.sessions if item["terminalId"] == terminal_id)
        connection: dict[str, Any] = {
            "id": terminal_id, "socket": socket, "closed": False, "sent": [],
        }
        self.connections.append(connection)
        socket.on_message(lambda data: connection["sent"].append(data))
        socket.on_close(lambda *_: connection.update(closed=True))
        # No connect_to_server call: all bytes and all endpoints are synthetic.
        socket.send(json.dumps({"type": "session", "session": snapshot}))
        socket.send(json.dumps({"type": "replay_start", "sessionId": snapshot["sessionId"]}))
        history = "".join(f"history {line:04d}\r\n" for line in range(350))
        socket.send(("\x1b[2J\x1b[H" + history + "READY").encode())
        socket.send(json.dumps({"type": "replay_end", "lastSequence": 1}))

    def connection(self, index: int) -> dict[str, Any]:
        terminal_id = session(index)["terminalId"]
        return next(item for item in reversed(self.connections) if item["id"] == terminal_id)

    def frames(self, index: int) -> list[str | bytes]:
        return self.connection(index)["sent"]

    def input_bytes(self, index: int) -> bytes:
        return b"".join(frame for frame in self.frames(index) if isinstance(frame, bytes))

    def release_sessions(self) -> None:
        self.hold_sessions = False
        held, self.held_sessions = self.held_sessions, []
        for route in held:
            route.fulfill(json=self.sessions)

    def notify_generation(self, index: int) -> None:
        snapshot = self.sessions[index]
        terminal_id = snapshot["terminalId"]
        pane = self.page.locator(f'.terminal-pane[data-terminal-id="{terminal_id}"]')
        was_hidden = pane.get_attribute("hidden") is not None
        wait_for_terminal_render(self.page)
        old_rows = self.page.locator(
            f'.terminal-pane[data-terminal-id="{terminal_id}"] .xterm-rows'
        ).element_handle()
        assert old_rows is not None
        previous_text = old_rows.text_content()
        replay_ends = self.page.evaluate("id => window.syntheticReplayEnds[id] ?? 0", terminal_id)
        binary_frames = self.page.evaluate("id => window.syntheticBinaryFrames[id] ?? 0", terminal_id)
        socket = self.connection(index)["socket"]
        # Match the real server's replay-before-session generation change.
        socket.send(json.dumps({"type": "replay_start", "sessionId": snapshot["sessionId"]}))
        socket.send(b"MISMATCHED-REPLAY-GENERATION")
        socket.send(json.dumps({"type": "replay_end", "lastSequence": 2}))
        # The output branch may send live deltas before the session event.
        socket.send(b"MISMATCHED-LIVE-GENERATION")
        self.page.wait_for_function(
            """([id, replays, frames]) => (window.syntheticReplayEnds[id] ?? 0) > replays
              && (window.syntheticBinaryFrames[id] ?? 0) >= frames + 2""",
            arg=[terminal_id, replay_ends, binary_frames],
        )
        if was_hidden:
            # Reveal the OLD view before metadata can replace it. Otherwise
            # xterm may defer painting while hidden and mask a leaked write.
            self.page.locator(".session-tab").nth(index).click()
        self.page.locator(f'{ACTIVE_PANE} .terminal-restore-status').wait_for()
        wait_for_terminal_render(self.page)
        assert old_rows.evaluate("element => element.isConnected")
        assert old_rows.text_content() == previous_text
        socket.send(json.dumps({"type": "session", "session": snapshot}))
        self.page.wait_for_function("element => !element.isConnected", arg=old_rows)


def is_cursor_report(frame: str | bytes) -> bool:
    return isinstance(frame, bytes) and frame.startswith(b"\x1b[") and frame.endswith(b"R")


def wait_for_terminal_render(page: Page) -> None:
    # Frame receipt is the transport barrier. Allow the small synthetic write
    # and DOM-render queues to settle before comparing the still-mounted rows.
    page.evaluate("""() => new Promise(resolve => setTimeout(() =>
      requestAnimationFrame(() => requestAnimationFrame(resolve)), 100))""")


def select(page: Page, index: int) -> None:
    page.locator(".session-tab").nth(index).click()
    page.locator(f'{ACTIVE_PANE}[data-terminal-id="{session(index)["terminalId"]}"]').wait_for()
    page.locator(".status--connected:visible").wait_for()
    page.locator(f"{ACTIVE_PANE} .terminal-restore-status").wait_for(state="detached")
    page.wait_for_function(
        """() => document.querySelector('.terminal-pane:not([hidden]) .xterm-rows')
          ?.textContent?.includes('history')"""
    )


def open_settings(page: Page) -> None:
    disclosure = page.locator(".mobile-header-toggle")
    if disclosure.is_visible() and disclosure.get_attribute("aria-expanded") == "false":
        disclosure.click()
    page.locator(".header-menu-trigger").click()
    page.locator(".header-menu-item--settings").click()


def remember_view(page: Page) -> None:
    page.evaluate("""() => {
      window.savedTerminal = document.querySelector(
        '.terminal-pane:not([hidden]) .xterm-helper-textarea');
    }""")


def view_is_preserved(page: Page) -> bool:
    return page.evaluate("""() => window.savedTerminal === document.querySelector(
      '.terminal-pane:not([hidden]) .xterm-helper-textarea')""")


def visible_history(page: Page) -> str:
    return page.locator(f"{ACTIVE_PANE} .xterm-rows").inner_text()


def run_desktop(browser: Browser, url: str) -> None:
    context = browser.new_context(viewport={"width": 1280, "height": 720})
    try:
        page = context.new_page()
        failures: list[str] = []
        page.on("pageerror", lambda error: failures.append(str(error)))
        peers = SyntheticPeers(page)
        page.goto(url)
        select(page, 0)
        page.wait_for_timeout(250)
        first_count = len(peers.connections)
        remember_view(page)

        # Selecting an active tab must keep the real connected state.
        select(page, 0)
        page.wait_for_timeout(150)
        assert len(peers.connections) == first_count
        assert view_is_preserved(page)
        assert page.locator(".status--connected:visible").count() == 1

        # The default is on. Switching back keeps both the xterm instance and
        # its scrolled viewport, without a fresh attachment or replay.
        page.locator(f"{ACTIVE_PANE} .terminal-view").hover()
        before_scroll = visible_history(page)
        page.mouse.wheel(0, -900)
        page.wait_for_function(
            """before => document.querySelector(
              '.terminal-pane:not([hidden]) .xterm-rows')?.innerText !== before""",
            arg=before_scroll,
        )
        page.wait_for_timeout(200)
        scrolled = visible_history(page)
        select(page, 1)
        second_count = len(peers.connections)
        health_reads = peers.health_reads
        select(page, 0)
        page.wait_for_timeout(200)
        assert view_is_preserved(page)
        assert len(peers.connections) == second_count
        assert peers.health_reads == health_reads
        assert visible_history(page) == scrolled

        # Background metadata/output cannot steal identity or focus, and a
        # hidden terminal must never resize to the active viewport dimensions.
        select(page, 1)
        for item in peers.connections:
            item["sent"].clear()
        background = peers.connection(0)
        background["socket"].send(b"\r\nBACKGROUND-OUTPUT\r\n\x1b[6n")
        background["socket"].send(json.dumps({
            "type": "session", "session": {**peers.sessions[0], "status": "exited"},
        }))
        page.set_viewport_size({"width": 1040, "height": 680})
        wait_until(page, lambda: any(is_cursor_report(frame) for frame in peers.frames(0)))
        wait_until(page, lambda: any(isinstance(frame, str) and '"resize"' in frame for frame in peers.frames(1)))
        assert page.locator(".app-context-project").inner_text() == peers.sessions[1]["project"]
        assert page.locator(".status--connected:visible").count() == 1
        assert not any(isinstance(frame, str) and '"resize"' in frame for frame in peers.frames(0))
        # xterm protocol replies still reach their own background PTY.
        assert any(is_cursor_report(frame) for frame in peers.frames(0))
        peers.frames(0).clear()
        peers.frames(1).clear()
        page.locator(f"{ACTIVE_PANE} .xterm-helper-textarea").focus()
        page.keyboard.type("selected-input")
        wait_until(page, lambda: b"".join(
            frame for frame in peers.frames(1) if isinstance(frame, bytes)
        ) == b"selected-input")
        assert b"".join(frame for frame in peers.frames(1) if isinstance(frame, bytes)) == b"selected-input"
        assert not any(isinstance(frame, bytes) for frame in peers.frames(0))
        page.locator('.terminal-pane[hidden] .xterm-helper-textarea').evaluate(
            """element => {
              element.dispatchEvent(new KeyboardEvent('keydown', {
                key: 'Enter', code: 'Enter', keyCode: 13, bubbles: true, cancelable: true,
              }));
              const baseline = element.value;
              element.value = 'hidden-input';
              element.dispatchEvent(new InputEvent('input', {
                inputType: 'insertText', data: 'hidden-input', bubbles: true,
              }));
              element.value = baseline;
            }"""
        )
        peers.frames(1).clear()
        page.keyboard.type("active-sentinel")
        wait_until(page, lambda: b"active-sentinel" in peers.input_bytes(1))
        assert peers.input_bytes(1) == b"active-sentinel"
        # A reply on the hidden socket is an ordering barrier on that socket,
        # so the negative assertion cannot outrun its input callbacks.
        background["socket"].send(b"\x1b[6n")
        wait_until(page, lambda: any(is_cursor_report(frame) for frame in peers.frames(0)))
        assert len([frame for frame in peers.frames(0) if isinstance(frame, bytes)]) == 1
        background["socket"].send(json.dumps({"type": "session", "session": peers.sessions[0]}))
        select(page, 0)
        page.locator(f"{ACTIVE_PANE} .terminal-view").dispatch_event("pointerdown", {"pointerType": "mouse"})
        page.get_by_title("Return to the live terminal output").click()
        page.wait_for_function("""() => document.querySelector(
          '.terminal-pane:not([hidden]) .xterm-rows')?.textContent?.includes('BACKGROUND-OUTPUT')""")

        # A seventh visit evicts only the oldest view. PTYs are represented by
        # unchanged session identities throughout this fixture.
        for index in range(2, 7):
            select(page, index)
        assert page.locator(".terminal-pane").count() == 6
        assert peers.connection(1)["closed"]
        assert not peers.connection(0)["closed"]
        count = len(peers.connections)
        select(page, 1)
        assert len(peers.connections) == count + 1

        # Disabling takes effect now, preserves the active view, and closes
        # hidden sockets. Later switches use fresh views again.
        remember_view(page)
        open_settings(page)
        checkbox = page.get_by_label(PREFERENCE, exact=True)
        assert checkbox.is_checked()
        checkbox.uncheck()
        page.get_by_label("Close settings", exact=True).click()
        assert page.locator(".terminal-pane").count() == 1
        assert view_is_preserved(page)
        wait_until(page, lambda: sum(not item["closed"] for item in peers.connections) == 1)
        count = len(peers.connections)
        select(page, 0)
        select(page, 1)
        assert len(peers.connections) == count + 2
        assert not view_is_preserved(page)

        # The opt-out survives a page reload; enabling again retains views.
        page.reload()
        select(page, 1)
        open_settings(page)
        assert not page.get_by_label(PREFERENCE, exact=True).is_checked()
        page.get_by_label(PREFERENCE, exact=True).check()
        page.get_by_label("Close settings", exact=True).click()
        select(page, 0)
        select(page, 1)
        assert page.locator(".terminal-pane").count() == 2

        # Consecutive snapshots reset at a parser boundary. Live bytes follow
        # the finite replay even when the previous snapshot is still queued.
        active_socket = peers.connection(1)["socket"]
        replay_start = json.dumps({
            "type": "replay_start", "sessionId": peers.sessions[1]["sessionId"],
        })
        replay_end = json.dumps({"type": "replay_end", "lastSequence": 2})
        active_socket.send(replay_start)
        active_socket.send(b"STALE-REPLAY\r\n" * 40000)
        active_socket.send(replay_end)
        active_socket.send(replay_start)
        active_socket.send(b"history FRESH-REPLAY\r\n")
        active_socket.send(replay_end)
        active_socket.send(b"LIVE-AFTER-REPLAY")
        page.wait_for_function("""() => document.querySelector(
          '.terminal-pane:not([hidden]) .xterm-rows')?.textContent?.includes('LIVE-AFTER-REPLAY')""")
        page.locator(f"{ACTIVE_PANE} .terminal-restore-status").wait_for(state="detached")
        rebuilt = visible_history(page)
        assert "FRESH-REPLAY" in rebuilt
        assert "STALE-REPLAY" not in rebuilt
        assert rebuilt.index("FRESH-REPLAY") < rebuilt.index("LIVE-AFTER-REPLAY")

        # Restarting an inactive PTY invalidates only that generation's view.
        peers.sessions[0] = {**peers.sessions[0], "sessionId": str(UUID(int=999))}
        count = len(peers.connections)
        peers.notify_generation(0)
        select(page, 0)
        assert len(peers.connections) == count + 1
        select(page, 1)
        assert page.locator(".app-context-project").inner_text() == peers.sessions[1]["project"]
        assert page.locator(".status--connected:visible").count() == 1

        # Recreating the selected generation cannot take focus from @cwt.
        page.locator(".session-peer-button").click()
        page.locator(".peer-composer").wait_for()
        page.wait_for_function("""() => document.activeElement?.closest('.peer-composer') !== null""")
        peers.sessions[1] = {**peers.sessions[1], "sessionId": str(UUID(int=998))}
        peers.notify_generation(1)
        page.wait_for_function("""() => document.querySelector(
          '.terminal-pane:not([hidden]) .xterm-rows')?.textContent?.includes('READY')""")
        assert page.evaluate("""() => Boolean(document.activeElement?.closest('.peer-composer'))""")
        page.get_by_label("Close peer composer", exact=True).click()

        # A generation replacement cannot steal focus from a tab-removal
        # confirmation. Desktop slash must remain outside the terminal too.
        page.locator(".session-tab-close").first.click()
        page.get_by_role("alertdialog").wait_for()
        page.wait_for_function("() => Boolean(document.activeElement?.closest('.confirm-dialog'))")
        count = len(peers.connections)
        peers.sessions[1] = {**peers.sessions[1], "sessionId": str(UUID(int=997))}
        peers.notify_generation(1)
        wait_until(page, lambda: len(peers.connections) == count + 1)
        page.locator(f"{ACTIVE_PANE} .terminal-restore-status").wait_for(state="detached")
        page.evaluate("() => new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve)))")
        assert page.evaluate("() => Boolean(document.activeElement?.closest('.confirm-dialog'))")
        peers.frames(1).clear()
        page.keyboard.press("/")
        page.get_by_role("button", name="Cancel", exact=True).click()
        page.locator(f"{ACTIVE_PANE} .xterm-helper-textarea").focus()
        page.keyboard.type("after-dialog")
        wait_until(page, lambda: b"after-dialog" in peers.input_bytes(1))
        assert peers.input_bytes(1) == b"after-dialog"

        # Exercise the actual Restart action with either notification order.
        # A socket generation notification before the HTTP reply must not
        # cause a second attachment when that reply finally arrives.
        for notify_first, generation in [(True, 996), (False, 995)]:
            open_settings(page)
            page.get_by_role("button", name="Restart Codex", exact=True).click()
            page.get_by_role("button", name="Restart", exact=True).click()
            wait_until(page, lambda: peers.restart_request is not None)
            request = peers.restart_request
            assert request is not None
            peers.restart_request = None
            page.get_by_label("Close settings", exact=True).focus()
            count = len(peers.connections)
            peers.sessions[1] = {**peers.sessions[1], "sessionId": str(UUID(int=generation))}
            if notify_first:
                peers.notify_generation(1)
                wait_until(page, lambda: len(peers.connections) == count + 1)
            else:
                peers.hold_sessions = True
            session_reads = peers.session_reads
            fetches = page.evaluate("window.syntheticSessionFetches")
            request.fulfill(status=204)
            wait_until(page, lambda: peers.session_reads > session_reads)
            if not notify_first:
                wait_until(page, lambda: bool(peers.held_sessions))
                assert not peers.connection(1)["closed"]
                peers.notify_generation(1)
                peers.release_sessions()
            wait_until(page, lambda: len(peers.connections) >= count + 1)
            page.wait_for_function("() => !document.querySelector('[title=\"Restart Codex\"]')?.disabled")
            page.locator(f"{ACTIVE_PANE} .terminal-restore-status").wait_for(state="detached")
            page.evaluate("() => new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve)))")
            # Count fetch starts in the page, before asynchronous route
            # callbacks, so an extra attach cannot hide behind network delay.
            expected_fetches = 1 if notify_first else 2
            assert page.evaluate("window.syntheticSessionFetches") == fetches + expected_fetches
            assert len(peers.connections) == count + 1
            assert page.evaluate("() => Boolean(document.activeElement?.closest('.settings-panel'))")
            page.get_by_label("Close settings", exact=True).click()

        # A deleted background session is pruned without selecting another tab.
        peers.sessions = peers.sessions[1:]
        peers.connection(0)["socket"].close()
        page.locator(f'.terminal-pane[data-terminal-id="{session(0)["terminalId"]}"]').wait_for(state="detached")
        assert page.locator(".app-context-project").inner_text() == session(1)["project"]
        assert not failures, failures
    except Exception:
        # This fixture owns every endpoint and byte; report only its synthetic
        # screen/viewport state, never browser storage or authenticated URLs.
        print(json.dumps({"desktopFailure": page.evaluate("""() => ({
          replays: window.syntheticReplayEnds,
          binaryFrames: window.syntheticBinaryFrames,
          panes: [...document.querySelectorAll('.terminal-pane')].map(pane => ({
            id: pane.dataset.terminalId,
            hidden: pane.hidden,
            text: pane.querySelector('.xterm-rows')?.textContent,
            viewport: (() => {
              const viewport = pane.querySelector('.xterm-viewport');
              return viewport && {top: viewport.scrollTop, height: viewport.clientHeight,
                scrollHeight: viewport.scrollHeight};
            })(),
          })),
        })"""), "connections": [
            {"id": item["id"], "closed": item["closed"]} for item in peers.connections
        ]}))
        open_settings(page)
        diagnostics = page.locator('textarea[aria-label="Viewport diagnostics text"]')
        if diagnostics.count():
            samples = json.loads(diagnostics.input_value())["samples"]
            print(json.dumps({"syntheticTerminalSamples": [sample["terminal"] for sample in samples]}))
        raise
    finally:
        context.close()


def run_attach_recovery(browser: Browser, url: str) -> None:
    for status, evict in [(401, False), (429, False), (429, True)]:
        context = browser.new_context(viewport={"width": 1280, "height": 720})
        try:
            page = context.new_page()
            page.clock.install()
            failures: list[str] = []
            page.on("pageerror", lambda error: failures.append(str(error)))
            peers = SyntheticPeers(page)
            page.goto(url)
            select(page, 0)
            select(page, 1)
            select(page, 0)
            remember_view(page)
            count = len(peers.connections)

            # The failed preflight belongs to a retained background pane.
            select(page, 1)
            peers.session_error = status
            reads = peers.session_reads
            terminal_id = session(0)["terminalId"]
            closes = page.evaluate("id => window.syntheticSocketCloses[id] ?? 0", terminal_id)
            peers.connection(0)["socket"].close()
            page.wait_for_function(
                "([id, before]) => (window.syntheticSocketCloses[id] ?? 0) > before",
                arg=[terminal_id, closes],
            )
            page.clock.fast_forward(1_100)
            wait_until(page, lambda: peers.session_reads > reads)
            page.locator(".session-tab").nth(0).click()
            expected_error = (
                "Authentication failed. Open the URL printed by the server."
                if status == 401 else "Connection temporarily limited. Retrying in one minute."
            )
            page.get_by_text(expected_error, exact=True).wait_for()
            failed_reads = peers.session_reads
            failed_fetches = page.evaluate("window.syntheticSessionFetches")
            select(page, 1)
            page.clock.fast_forward(30_000)
            # Re-selection must respect the cooldown as well.
            page.locator(".session-tab").nth(0).click()
            page.get_by_text(expected_error, exact=True).wait_for()
            select(page, 1)
            assert peers.session_reads == failed_reads
            assert page.evaluate("window.syntheticSessionFetches") == failed_fetches

            if evict:
                open_settings(page)
                page.get_by_label(PREFERENCE, exact=True).uncheck()
                page.get_by_label("Close settings", exact=True).click()
                page.locator('.terminal-pane[hidden]').wait_for(state="detached")
                page.evaluate("() => new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve)))")
            peers.session_error = None
            page.clock.fast_forward(31_000)
            if evict:
                # Disposing the view cancels its scheduled 429 recovery.
                assert peers.session_reads == failed_reads
                assert page.evaluate("window.syntheticSessionFetches") == failed_fetches
                assert len(peers.connections) == count
            else:
                if status == 401:
                    assert peers.session_reads == failed_reads  # No automatic bad-token retry.
                    assert page.evaluate("window.syntheticSessionFetches") == failed_fetches
                else:
                    wait_until(page, lambda: len(peers.connections) == count + 1)
                select(page, 0)
                assert peers.session_reads == failed_reads + 1
                assert len(peers.connections) == count + 1
                assert view_is_preserved(page)
            assert not failures, failures
        finally:
            context.close()


def run_mobile(browser: Browser, url: str) -> None:
    context = browser.new_context(
        viewport={"width": 390, "height": 740}, is_mobile=True, has_touch=True,
        user_agent="Mozilla/5.0 (Linux; Android 14) AppleWebKit/537.36 Chrome/150.0.0.0 Mobile Safari/537.36",
    )
    try:
        page = context.new_page()
        peers = SyntheticPeers(page)
        page.goto(url)
        select(page, 0)
        remember_view(page)
        select(page, 1)
        count = len(peers.connections)
        select(page, 0)
        assert view_is_preserved(page)
        assert len(peers.connections) == count
        assert page.locator(".terminal-pane:not([hidden])").count() == 1
        assert page.evaluate("""() => !document.activeElement?.classList.contains('xterm-helper-textarea')""")
        for item in peers.connections:
            item["sent"].clear()
        # A toolbar Enter belongs only to the selected PTY.
        page.get_by_role("button", name="Enter", exact=True).click()
        wait_until(page, lambda: b"\r" in peers.frames(0))
        assert not any(isinstance(frame, bytes) for frame in peers.frames(1))

        # A soft Enter queued before the React selection commit belongs to
        # its original terminal, even when its fallback timer fires hidden.
        peers.frames(0).clear()
        page.evaluate("""() => {
          const textarea = document.querySelector('.terminal-pane:not([hidden]) .xterm-helper-textarea');
          textarea.focus();
          textarea.dispatchEvent(new KeyboardEvent('keydown', {
            key: 'Enter', code: 'Enter', keyCode: 229,
            bubbles: true, cancelable: true,
          }));
          document.querySelectorAll('.session-tab')[1].click();
        }""")
        wait_until(page, lambda: b"\r" in peers.frames(0))
        page.locator(f'{ACTIVE_PANE}[data-terminal-id="{session(1)["terminalId"]}"]').wait_for()
        assert b"".join(frame for frame in peers.frames(0) if isinstance(frame, bytes)) == b"\r"
        assert not any(isinstance(frame, bytes) for frame in peers.frames(1))

        # Synthetic events bypass inert, so this also checks the capture
        # guard runs before the IME translator in a hidden pane.
        peers.frames(0).clear()
        page.locator('.terminal-pane[hidden] .xterm-helper-textarea').evaluate("""textarea => {
          textarea.dispatchEvent(new KeyboardEvent('keydown', {
            key: 'Enter', code: 'Enter', keyCode: 229,
            bubbles: true, cancelable: true,
          }));
          textarea.dispatchEvent(new InputEvent('input', {
            inputType: 'insertLineBreak', bubbles: true, cancelable: true,
          }));
        }""")
        page.wait_for_timeout(150)  # No delayed Enter may escape the guard.
        assert not any(isinstance(frame, bytes) for frame in peers.frames(0))

        # An accepted composition Send with no native compositionend must
        # finish through the guard's own event, still ordered text then CR.
        select(page, 0)
        peers.frames(0).clear()
        page.evaluate("""() => {
          const textarea = document.querySelector('.terminal-pane:not([hidden]) .xterm-helper-textarea');
          textarea.focus();
          textarea.value = '';
          textarea.dispatchEvent(new CompositionEvent('compositionstart', {bubbles: true, data: ''}));
          textarea.value = 'go';
          textarea.dispatchEvent(new CompositionEvent('compositionupdate', {bubbles: true, data: 'go'}));
        }""")
        page.evaluate("() => new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve)))")
        page.evaluate("""() => {
          const textarea = document.querySelector('.terminal-pane:not([hidden]) .xterm-helper-textarea');
          textarea.dispatchEvent(new KeyboardEvent('keydown', {
            key: 'Enter', code: 'Enter', keyCode: 13, isComposing: true,
            bubbles: true, cancelable: true,
          }));
          document.querySelectorAll('.session-tab')[1].click();
        }""")
        wait_until(page, lambda: b"\r" in peers.frames(0))
        assert b"".join(frame for frame in peers.frames(0) if isinstance(frame, bytes)) == b"go\r"
        assert not any(isinstance(frame, bytes) for frame in peers.frames(1))
    finally:
        context.close()


class QuietHandler(SimpleHTTPRequestHandler):
    # Windows MIME registry entries must not decide whether ES modules load.
    extensions_map = {
        **SimpleHTTPRequestHandler.extensions_map,
        ".js": "text/javascript", ".mjs": "text/javascript",
        ".css": "text/css", ".json": "application/json",
        ".svg": "image/svg+xml", ".wasm": "application/wasm",
        ".woff2": "font/woff2",
    }

    def log_message(self, *_: Any) -> None:
        pass


class SyntheticPeerWorkflow(SyntheticPeers):
    def __init__(self, page: Page) -> None:
        super().__init__(page)
        self.sessions = self.sessions[:2]
        self.threads: list[dict[str, Any]] = []
        self.creations = 0
        self.discarded_turn: str | None = None

    def http(self, route: Route) -> None:
        path = urlsplit(route.request.url).path
        if path == "/api/agent-catalog":
            route.fulfill(json={
                "schemaVersion": 1,
                "server": {"os": "synthetic", "arch": "x86_64", "shell": "synthetic"},
                "agents": [{
                    "kind": "claude", "state": "ready", "configuration": "auto",
                    "version": "synthetic", "dangerouslySkipPermissions": False,
                    "install": {
                        "command": "", "shell": "", "verifyCommand": "",
                        "updateCommand": "", "docsUrl": "", "requiresServerAccess": True,
                    },
                }],
            })
            return
        if path == "/api/workspaces":
            route.fulfill(json={"version": 1, "recent": [], "favorites": [{
                "id": "synthetic-favorite", "directoryId": "synthetic-review-folder",
                "name": "Review folder", "path": "/workspace/review",
            }]})
            return
        if path == "/api/peer/threads" and route.request.method == "POST":
            self.creations += 1
            thread_id = str(UUID(int=2000 + self.creations))
            thread = {
                "id": thread_id, "sourceTerminalId": session(0)["terminalId"],
                "reviewerTerminalId": session(2)["terminalId"], "targetAgent": "claude",
                "status": "preparing_handoff", "createdAt": 10, "updatedAt": 10,
                "currentTurn": {
                    "id": str(UUID(int=3000 + self.creations)), "sequence": 1,
                    "action": "review", "instruction": "Synthetic review",
                    "status": "preparing_handoff", "handoff": None,
                    "handoffRevision": 0, "response": None, "error": None,
                },
            }
            self.threads.append(thread)
            self.sessions.append({
                **session(2), "agent": "claude", "name": "Claude review",
                "purpose": {"kind": "peer", "threadId": thread_id,
                            "parentTerminalId": session(0)["terminalId"]},
            })
            self.hold_sessions = True
            route.fulfill(status=201, json=thread)
            return
        if path == "/api/peer/threads":
            route.fulfill(json=self.threads)
            return
        if path.startswith("/api/peer/threads/") and route.request.method == "DELETE":
            body = route.request.post_data_json
            self.discarded_turn = body.get("discardUnreadTurnId") if isinstance(body, dict) else None
            self.threads = []
            self.sessions = self.sessions[:2]
            route.fulfill(status=204)
            return
        super().http(route)


def run_peer_ui(browser: Browser, url: str, mobile: bool) -> None:
    context = browser.new_context(
        viewport={"width": 390 if mobile else 1280, "height": 740},
        is_mobile=mobile, has_touch=mobile,
    )
    try:
        page = context.new_page()
        failures: list[str] = []
        page.on("pageerror", lambda error: failures.append(str(error)))
        peers = SyntheticPeerWorkflow(page)
        page.goto(url)
        select(page, 1)
        select(page, 0)
        page.locator(".session-peer-button").click()
        folder = page.get_by_role("button", name="Change folder", exact=True)
        instruction = page.get_by_label("What should the other agent do?", exact=True)
        instruction.fill("Synthetic review")

        # Choosing and cancelling a folder both return focus to the composer.
        folder.click()
        page.get_by_role("button", name="Use for reviewer: /workspace/review", exact=True).click()
        page.wait_for_function("""() => document.activeElement?.textContent?.trim() === 'Change folder'""")
        assert instruction.input_value() == "Synthetic review"
        folder.click()
        page.locator(".workspace-picker").wait_for()
        page.wait_for_function("() => Boolean(document.activeElement?.closest('.workspace-picker'))")
        page.keyboard.press("Escape")
        page.wait_for_function("""() => document.activeElement?.textContent?.trim() === 'Change folder'""")
        page.keyboard.press("Tab")
        assert page.evaluate("() => Boolean(document.activeElement?.closest('.peer-composer'))")

        # Submit twice in one event turn, then keep the follow-up session list
        # request unresolved. The composer must already show the created thread.
        page.get_by_role("button", name="Prepare summary", exact=True).wait_for()
        page.locator(".peer-instruction-form").evaluate("form => { form.requestSubmit(); form.requestSubmit(); }")
        page.get_by_text("Preparing summary", exact=True).wait_for()
        wait_until(page, lambda: bool(peers.held_sessions))
        assert peers.creations == 1
        assert page.locator(".peer-instruction-form").count() == 0
        page.get_by_label("Close peer composer", exact=True).click()
        select(page, 1)
        peers.release_sessions()
        page.wait_for_function("() => document.querySelectorAll('.session-tab').length === 3")
        assert page.locator(ACTIVE_PANE).get_attribute("data-terminal-id") == session(1)["terminalId"]
        select(page, 0)

        thread = peers.threads[0]
        thread["status"] = "returning"
        thread["currentTurn"].update(status="returning", response="Synthetic response")
        page.locator(".session-peer-button").click()
        page.get_by_text("Returning to source", exact=True).wait_for()
        assert page.get_by_role("button", name="Prepare follow-up", exact=True).count() == 0
        assert page.get_by_role("button", name="Return to source", exact=True).count() == 0

        # Polling must enable follow-up only after the helper's receipt.
        thread["status"] = "returned"
        thread["currentTurn"]["status"] = "returned"
        page.get_by_role("button", name="Prepare follow-up", exact=True).wait_for()
        page.get_by_label("What should the other agent do?", exact=True).fill("Synthetic follow-up")
        assert page.get_by_role("button", name="Prepare follow-up", exact=True).is_enabled()
        page.get_by_label("Close peer composer", exact=True).click()
        # A different unread turn still requires exact-turn discard.
        thread["status"] = "returning"
        thread["currentTurn"].update(id=str(UUID(int=3999)), sequence=2, status="returning")
        page.locator(".session-peer-button").click()
        page.get_by_text("Returning to source", exact=True).wait_for()
        page.get_by_label("Close peer composer", exact=True).click()
        page.locator(".session-tab-shell--peer .session-tab-close").click()
        page.get_by_role("button", name="Discard unread response", exact=True).click()
        page.wait_for_function("() => document.querySelectorAll('.session-tab').length === 2")
        assert peers.discarded_turn == thread["currentTurn"]["id"]
        assert not failures, failures
    finally:
        context.close()


class SyntheticMobileLayout(SyntheticPeers):
    def __init__(self, page: Page) -> None:
        super().__init__(page)
        self.sessions = self.sessions[:2]
        self.project = "/workspace/" + "nested-review-directory/" * 14 + "project"
        self.sessions[0]["project"] = self.project
        self.catalog_error = False
        self.created: list[str] = []

    def http(self, route: Route) -> None:
        path = urlsplit(route.request.url).path
        if path == "/api/agent-catalog":
            if self.catalog_error:
                route.fulfill(status=503, json={"error": "Synthetic catalog temporarily unavailable."})
                return
            route.fulfill(json={
                "schemaVersion": 1,
                "server": {"os": "synthetic", "arch": "x86_64", "shell": "synthetic"},
                "agents": [{
                    "kind": kind, "state": "ready", "configuration": "auto",
                    "version": "synthetic", "dangerouslySkipPermissions": False,
                    "install": {
                        "command": "", "shell": "", "verifyCommand": "",
                        "updateCommand": "", "docsUrl": "", "requiresServerAccess": True,
                    },
                } for kind in ("codex", "claude", "agy")],
            })
            return
        if path == "/api/workspaces":
            route.fulfill(json={"version": 1, "recent": [], "favorites": [{
                "id": "synthetic-favorite", "directoryId": self.sessions[0]["directoryId"],
                "name": "Synthetic project", "path": self.project,
            }]})
            return
        if path == "/api/sessions" and route.request.method == "POST":
            agent = route.request.post_data_json["agent"]
            self.created.append(agent)
            created = {**session(len(self.sessions)), "agent": agent, "project": self.project}
            self.sessions.append(created)
            route.fulfill(status=201, json=created)
            return
        super().http(route)


def assert_touch_control(page: Page, control: Locator) -> None:
    control.scroll_into_view_if_needed()
    box = control.bounding_box()
    viewport = page.viewport_size
    assert box is not None and viewport is not None
    assert box["width"] >= 44 and box["height"] >= 44, box
    assert box["x"] >= -1 and box["y"] >= -1, box
    assert box["x"] + box["width"] <= viewport["width"] + 1, box
    assert box["y"] + box["height"] <= viewport["height"] + 1, box
    assert control.evaluate("""element => {
      const r = element.getBoundingClientRect();
      const hit = document.elementFromPoint(r.left + r.width / 2, r.top + r.height / 2);
      return hit === element || element.contains(hit);
    }"""), box


def run_mobile_layout(browser: Browser, url: str) -> None:
    # Resizing the layout viewport covers Android's resizes-content behavior.
    # This is Chromium touch emulation, not a real Samsung Internet/IME run.
    for width, height, text_size in [(360, 639, 16), (360, 345, 16), (800, 345, 16), (360, 345, 20)]:
        context = browser.new_context(
            viewport={"width": 360, "height": 639},
            is_mobile=True, has_touch=True,
        )
        try:
            page = context.new_page()
            failures: list[str] = []
            page.on("pageerror", lambda error: failures.append(str(error)))
            peers = SyntheticMobileLayout(page)
            if width == 800:
                peers.sessions.extend(session(index) for index in range(2, 8))
            # A fresh browser has no token. The invalid value stays client-side.
            page.goto(url.split("?", 1)[0])
            page.evaluate("size => document.documentElement.style.fontSize = `${size}px`", text_size)
            token_input = page.get_by_label("Authentication token", exact=True)
            token_input.fill("short")
            page.set_viewport_size({"width": width, "height": height})
            page.wait_for_function("""() => {
              const screen = document.querySelector('.auth-screen').getBoundingClientRect();
              return screen.bottom <= window.innerHeight + 1;
            }""")
            assert token_input.input_value() == "short"
            assert_touch_control(page, token_input)
            connect = page.get_by_role("button", name="Connect", exact=True)
            assert_touch_control(page, connect)
            connect.click()
            page.locator(".form-error").wait_for()
            assert_touch_control(page, connect)
            assert_touch_control(page, token_input)
            if height <= 345:
                assert page.locator(".auth-screen").evaluate("e => e.scrollHeight > e.clientHeight")
            page.locator(".auth-screen").evaluate("e => { e.scrollTop = 0; }")
            heading = page.locator(".auth-card h1").bounding_box()
            assert heading is not None and heading["y"] >= 0, heading
            assert page.evaluate("window.scrollY") == 0

            page.goto(url)
            page.evaluate("size => document.documentElement.style.fontSize = `${size}px`", text_size)
            select(page, 0)
            for selector in (".session-tab", ".session-tab-close", ".session-peer-button", ".key-button"):
                assert_touch_control(page, page.locator(selector).first)
            if width == 800:
                # Narrow overlay arrows must not inherit the 44 px minimum
                # intended for the surrounding header controls.
                arrow = page.get_by_role("button", name="Scroll sessions right", exact=True)
                arrow.wait_for()
                box = arrow.bounding_box()
                strip = page.locator(".session-tabs")
                strip_box = strip.bounding_box()
                peer_box = page.locator(".session-peer-button").bounding_box()
                assert box is not None and strip_box is not None and peer_box is not None
                assert 0 < box["width"] < 44, box
                assert box["x"] >= strip_box["x"] - 1, box
                assert box["x"] + box["width"] <= peer_box["x"] + 1, box
                arrow.click()
                page.wait_for_function("() => document.querySelector('.session-tabs').scrollLeft > 0")
            disclosure = page.locator(".mobile-header-toggle")
            assert_touch_control(page, disclosure)
            if disclosure.get_attribute("aria-expanded") == "false":
                disclosure.click()
            menu = page.locator(".header-menu-trigger")
            assert_touch_control(page, menu)
            menu.click()
            new_terminal = page.get_by_role("menuitem", name="New terminal", exact=True)
            assert_touch_control(page, new_terminal)
            new_terminal.click()
            page.get_by_role("button", name=f"Use folder: {peers.project}", exact=True).click()
            picker = page.locator(".agent-picker")
            picker.wait_for()
            page.wait_for_function("""() => {
              const active = document.activeElement;
              if (!active?.closest('.agent-picker')) return false;
              const r = active.getBoundingClientRect();
              const hit = document.elementFromPoint(r.left + r.width / 2, r.top + r.height / 2);
              return hit === active || active.contains(hit);
            }""")
            body = picker.locator(".agent-picker-body")
            assert body.evaluate("e => e.scrollTop") == 0
            header = picker.locator(".agent-picker-header")
            page.wait_for_function("() => !document.querySelector('[data-agent-refresh]')?.disabled")
            header_box = header.bounding_box()
            assert header_box is not None
            refresh = page.get_by_role("button", name="Check installed agents again", exact=True)
            assert_touch_control(page, refresh)
            peers.catalog_error = True
            refresh.click()
            picker.locator(".agent-picker-error").wait_for()
            peers.catalog_error = False
            refresh.click()
            picker.locator(".agent-picker-error").wait_for(state="detached")
            page.wait_for_function("() => !document.querySelector('[data-agent-refresh]')?.disabled")

            starts = picker.locator("[data-agent-start]")
            assert starts.count() == 3
            for index in range(starts.count()):
                assert_touch_control(page, starts.nth(index))
                assert header.bounding_box() == header_box
                assert page.evaluate("window.scrollY") == 0
            assert body.evaluate("e => e.scrollTop > 0")
            assert picker.locator(".agent-picker-workspace code").inner_text() == peers.project
            assert page.evaluate("document.documentElement.scrollWidth <= window.innerWidth")
            assert_touch_control(page, page.get_by_role("button", name="Close agent picker", exact=True))

            # A reachable action must also be clickable with the long path present.
            starts.last.click()
            picker.wait_for(state="detached")
            assert peers.created == ["agy"]
            assert not failures, failures
        finally:
            context.close()


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--web-dir", type=Path, required=True)
    parser.add_argument("--chrome", type=Path, default=Path(
        r"C:\Program Files\Google\Chrome\Application\chrome.exe"
        if os.name == "nt" else "/usr/bin/google-chrome"
    ))
    args = parser.parse_args()
    web_dir = args.web_dir.resolve(strict=True)
    if not (web_dir / "index.html").is_file():
        raise ValueError("--web-dir must contain the built frontend")
    server = ThreadingHTTPServer(("127.0.0.1", 0), partial(QuietHandler, directory=str(web_dir)))
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    try:
        url = f"http://127.0.0.1:{server.server_port}/?token={TOKEN}"
        with sync_playwright() as playwright:
            browser = playwright.chromium.launch(executable_path=str(args.chrome), headless=True)
            try:
                run_desktop(browser, url)
                run_attach_recovery(browser, url)
                run_mobile(browser, url)
                run_peer_ui(browser, url, mobile=False)
                run_peer_ui(browser, url, mobile=True)
                run_mobile_layout(browser, url)
            finally:
                browser.close()
        print(json.dumps({"desktop": "passed", "attachRecovery": "passed", "mobile": "passed", "peerUi": "passed", "mobileLayout": "passed", "transport": "synthetic"}))
    finally:
        server.shutdown()
        server.server_close()
        thread.join(timeout=5)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
