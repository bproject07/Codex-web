#!/usr/bin/env python3
"""Exercise Android keyboard viewport resize against a disposable Codex Web server."""

from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import subprocess
import sys
import time
from typing import Any
from urllib.error import URLError
from urllib.request import Request, urlopen

from playwright.sync_api import Page, sync_playwright


PHONE_USER_AGENT = (
    "Mozilla/5.0 (Linux; Android 10; K) AppleWebKit/537.36 "
    "(KHTML, like Gecko) Chrome/150.0.0.0 Mobile Safari/537.36"
)
TOKEN = "mobile-regression-token-2026"
WIDTH = 360
SCREEN_HEIGHT = 780
KEYBOARD_CLOSED_HEIGHT = 639
KEYBOARD_OPEN_HEIGHT = 345
FIXTURE_HISTORY_LINES = 8_000
EXPECTED_FIXTURE_BUFFER_LINES = FIXTURE_HISTORY_LINES + 1


def parse_args() -> argparse.Namespace:
    repository = Path(__file__).resolve().parents[1]
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--server",
        type=Path,
        default=repository / "dist-batched" / "codex-web.exe",
    )
    parser.add_argument(
        "--chrome",
        type=Path,
        default=Path(r"C:\Program Files\Google\Chrome\Application\chrome.exe"),
    )
    parser.add_argument("--port", type=int, default=8791)
    parser.add_argument("--observe", action="store_true")
    return parser.parse_args()


def wait_for_server(port: int, process: subprocess.Popen[bytes]) -> None:
    request = Request(
        f"http://127.0.0.1:{port}/api/sessions",
        headers={"Authorization": f"Bearer {TOKEN}"},
    )
    deadline = time.monotonic() + 15
    while time.monotonic() < deadline:
        if process.poll() is not None:
            raise RuntimeError(f"test server exited with code {process.returncode}")
        try:
            with urlopen(request, timeout=0.5) as response:
                if response.status == 200:
                    return
        except (OSError, URLError):
            pass
        time.sleep(0.1)
    raise TimeoutError("test server did not become ready")


def set_mobile_height(cdp: Any, height: int) -> None:
    cdp.send(
        "Emulation.setDeviceMetricsOverride",
        {
            "width": WIDTH,
            "height": height,
            "deviceScaleFactor": 3,
            "mobile": True,
            "screenWidth": WIDTH,
            "screenHeight": SCREEN_HEIGHT,
        },
    )


def begin_diagnostics(page: Page) -> None:
    page.locator(".terminal-view").evaluate(
        """element => {
          element.dispatchEvent(new PointerEvent("pointerdown", {
            bubbles: true,
            pointerType: "touch",
            isPrimary: true,
          }));
          document.querySelector(".xterm-helper-textarea")?.focus();
        }"""
    )


def read_diagnostics(page: Page) -> dict[str, Any]:
    page.locator('[title="Open terminal settings"]').click()
    details = page.locator("details.diagnostics-manual-copy")
    details.wait_for(state="attached")
    details.evaluate("element => { element.open = true; }")
    raw = page.locator(
        'textarea[aria-label="Viewport diagnostics text"]'
    ).input_value()
    return json.loads(raw)


def terminal_sample(
    samples: list[dict[str, Any]], height: int, *, last: bool
) -> dict[str, Any]:
    matches = [
        sample
        for sample in samples
        if sample.get("innerHeight") == height and sample.get("terminal")
    ]
    if not matches:
        raise AssertionError(f"no terminal sample recorded at height {height}")
    return matches[-1] if last else matches[0]


def resize_frames(payloads: list[str | bytes]) -> list[dict[str, Any]]:
    messages: list[dict[str, Any]] = []
    for payload in payloads:
        if not isinstance(payload, str):
            continue
        try:
            parsed = json.loads(payload)
        except json.JSONDecodeError:
            continue
        if isinstance(parsed, dict) and parsed.get("type") == "resize":
            messages.append(parsed)
    return messages


def run_browser_test(args: argparse.Namespace) -> dict[str, Any]:
    repository = Path(__file__).resolve().parents[1]
    fixture = repository / "scripts" / "fixtures" / "mobile-resize-tui.cmd"
    command = [
        str(args.server),
        "--host",
        "127.0.0.1",
        "--port",
        str(args.port),
        "--project",
        str(repository),
        "--shell",
        "cmd",
        "--command",
        str(fixture),
        "--token",
        TOKEN,
        "--no-open-browser",
        "--log-level",
        "warn",
    ]
    creation_flags = subprocess.CREATE_NO_WINDOW if os.name == "nt" else 0
    server = subprocess.Popen(
        command,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
        creationflags=creation_flags,
    )

    try:
        wait_for_server(args.port, server)
        with sync_playwright() as playwright:
            browser = playwright.chromium.launch(
                executable_path=str(args.chrome),
                headless=True,
                args=["--disable-background-networking"],
            )
            context = browser.new_context(
                viewport={"width": WIDTH, "height": KEYBOARD_CLOSED_HEIGHT},
                screen={"width": WIDTH, "height": SCREEN_HEIGHT},
                device_scale_factor=3,
                is_mobile=True,
                has_touch=True,
                user_agent=PHONE_USER_AGENT,
            )
            page = context.new_page()
            cdp = context.new_cdp_session(page)
            cdp.send("Emulation.setTouchEmulationEnabled", {"enabled": True})
            set_mobile_height(cdp, KEYBOARD_CLOSED_HEIGHT)

            sent_frames: list[str | bytes] = []

            def observe_socket(socket: Any) -> None:
                socket.on("framesent", lambda payload: sent_frames.append(payload))

            page.on("websocket", observe_socket)
            page.goto(
                f"http://127.0.0.1:{args.port}/?token={TOKEN}",
                wait_until="domcontentloaded",
            )
            page.locator(".xterm-helper-textarea").wait_for(state="attached")
            page.wait_for_timeout(1_500)
            page.evaluate(
                """() => {
                  window.__atomicFrameEvents = [];
                  const canvasStats = root => {
                    const canvases = Array.from(root?.querySelectorAll("canvas") ?? []);
                    const probe = document.createElement("canvas");
                    probe.width = 96;
                    probe.height = 96;
                    const context = probe.getContext("2d", {
                      willReadFrequently: true,
                    });
                    let nonTransparentPixels = 0;
                    let nonBlackPixels = 0;
                    let hash = 2166136261;

                    for (const canvas of canvases) {
                      context.clearRect(0, 0, probe.width, probe.height);
                      context.drawImage(canvas, 0, 0, probe.width, probe.height);
                      const pixels = context.getImageData(
                        0,
                        0,
                        probe.width,
                        probe.height,
                      ).data;
                      for (let index = 0; index < pixels.length; index += 4) {
                        const red = pixels[index];
                        const green = pixels[index + 1];
                        const blue = pixels[index + 2];
                        const alpha = pixels[index + 3];
                        if (alpha > 0) {
                          nonTransparentPixels += 1;
                          if (red > 0 || green > 0 || blue > 0) {
                            nonBlackPixels += 1;
                          }
                        }
                        hash ^= red;
                        hash = Math.imul(hash, 16777619);
                        hash ^= green;
                        hash = Math.imul(hash, 16777619);
                        hash ^= blue;
                        hash = Math.imul(hash, 16777619);
                        hash ^= alpha;
                        hash = Math.imul(hash, 16777619);
                      }
                    }

                    return {
                      canvasCount: canvases.length,
                      nonTransparentPixels,
                      nonBlackPixels,
                      hash: hash >>> 0,
                    };
                  };
                  const rowStats = root => {
                    const rows = Array.from(
                      root?.querySelectorAll(".xterm-rows > div") ?? [],
                      row => row.textContent ?? "",
                    );
                    const text = rows.join("\\n");
                    let hash = 2166136261;
                    for (let index = 0; index < text.length; index += 1) {
                      hash ^= text.charCodeAt(index);
                      hash = Math.imul(hash, 16777619);
                    }
                    return {
                      rowCount: rows.length,
                      textLength: text.length,
                      nonWhitespaceCharacters: text.replace(/\\s/g, "").length,
                      hash: hash >>> 0,
                    };
                  };
                  const terminal = document.querySelector(".terminal-view");
                  new MutationObserver(records => {
                    for (const record of records) {
                      for (const node of record.addedNodes) {
                        if (node instanceof Element &&
                            node.classList.contains("terminal-atomic-frame")) {
                          const source = Array.from(terminal.children).find(
                            child => child.classList.contains("xterm"),
                          );
                          const sourceCanvas = canvasStats(source);
                          const clonedCanvas = canvasStats(node);
                          window.__atomicFrameEvents.push({
                            type: "added",
                            t: performance.now(),
                            sourceCanvas,
                            clonedCanvas,
                            copiedCanvasCount:
                              sourceCanvas.canvasCount === clonedCanvas.canvasCount &&
                              sourceCanvas.hash === clonedCanvas.hash
                                ? sourceCanvas.canvasCount
                                : 0,
                            sourceRows: rowStats(source),
                            clonedRows: rowStats(node),
                          });
                        }
                      }
                      for (const node of record.removedNodes) {
                        if (node instanceof Element &&
                            node.classList.contains("terminal-atomic-frame")) {
                          window.__atomicFrameEvents.push({
                            type: "removed",
                            t: performance.now(),
                          });
                        }
                      }
                    }
                  }).observe(terminal, { childList: true });
                }"""
            )

            # Establish the keyboard-open layout, then let its PTY redraw settle.
            sent_frames.clear()
            set_mobile_height(cdp, KEYBOARD_OPEN_HEIGHT)
            page.wait_for_timeout(2_000)
            open_frames = resize_frames(sent_frames)
            sent_frames.clear()

            # Capture only the keyboard-hide transition.
            begin_diagnostics(page)
            page.wait_for_timeout(120)
            set_mobile_height(cdp, KEYBOARD_CLOSED_HEIGHT)
            page.wait_for_timeout(2_050)

            diagnostics = read_diagnostics(page)
            samples = diagnostics["samples"]
            opened = terminal_sample(
                samples, KEYBOARD_OPEN_HEIGHT, last=False
            )
            closed = terminal_sample(
                samples, KEYBOARD_CLOSED_HEIGHT, last=True
            )
            opened_terminal = opened["terminal"]
            closed_terminal = closed["terminal"]
            frames = resize_frames(sent_frames)
            atomic_events = page.evaluate("window.__atomicFrameEvents")

            result = {
                "screen": diagnostics["screen"],
                "viewportCycle": [
                    opened["innerHeight"],
                    closed["innerHeight"],
                ],
                "rows": [
                    opened_terminal["rows"],
                    closed_terminal["rows"],
                ],
                "bufferLength": [
                    opened_terminal["bufferLength"],
                    closed_terminal["bufferLength"],
                ],
                "baseY": [
                    opened_terminal["baseY"],
                    closed_terminal["baseY"],
                ],
                "absoluteCursorY": [
                    opened_terminal["baseY"] + opened_terminal["cursorY"],
                    closed_terminal["baseY"] + closed_terminal["cursorY"],
                ],
                "replayCount": [
                    opened_terminal["replayCount"],
                    closed_terminal["replayCount"],
                ],
                "atomicMobileResizeCommits": [
                    opened_terminal["atomicMobileResizeCommits"],
                    closed_terminal["atomicMobileResizeCommits"],
                ],
                "ptyRows": [
                    opened_terminal["ptyRows"],
                    closed_terminal["ptyRows"],
                ],
                "pageScrollY": sorted(
                    {sample["scrollY"] for sample in samples}
                ),
                "openResizeFrames": open_frames,
                "closeResizeFrames": frames,
                "atomicFrameEvents": atomic_events,
            }

            if not args.observe:
                assert result["screen"] == {
                    "width": WIDTH,
                    "height": SCREEN_HEIGHT,
                    "pixelRatio": 3,
                }
                assert result["viewportCycle"] == [
                    KEYBOARD_OPEN_HEIGHT,
                    KEYBOARD_CLOSED_HEIGHT,
                ]
                assert result["rows"][0] <= 16
                assert result["rows"][1] >= 30
                assert result["pageScrollY"] == [0]
                assert result["replayCount"][0] == result["replayCount"][1]
                assert len(result["openResizeFrames"]) == 1
                assert result["openResizeFrames"][0]["rows"] <= 16
                assert len(result["closeResizeFrames"]) == 1
                assert result["closeResizeFrames"][0]["rows"] >= 30
                assert result["ptyRows"][0] <= 16
                assert result["ptyRows"][1] >= 30
                assert (
                    result["atomicMobileResizeCommits"][1]
                    > result["atomicMobileResizeCommits"][0]
                )
                assert sum(
                    event["type"] == "added"
                    for event in result["atomicFrameEvents"]
                ) >= 2
                assert sum(
                    event["type"] == "removed"
                    for event in result["atomicFrameEvents"]
                ) >= 2
                assert abs(
                    result["bufferLength"][1] - result["bufferLength"][0]
                ) <= 32
                assert result["bufferLength"] == [
                    EXPECTED_FIXTURE_BUFFER_LINES,
                    EXPECTED_FIXTURE_BUFFER_LINES,
                ]
                assert abs(
                    result["absoluteCursorY"][1]
                    - result["absoluteCursorY"][0]
                ) <= 2
                added_frames = [
                    event
                    for event in result["atomicFrameEvents"]
                    if event["type"] == "added"
                ]
                assert all(
                    event["copiedCanvasCount"]
                    == event["sourceCanvas"]["canvasCount"]
                    for event in added_frames
                )
                assert all(
                    event["clonedRows"]["rowCount"] > 0
                    and event["clonedRows"]["nonWhitespaceCharacters"] > 0
                    for event in added_frames
                )
                assert all(
                    event["clonedRows"] == event["sourceRows"]
                    for event in added_frames
                )

            context.close()
            browser.close()
            return result
    finally:
        if server.poll() is None:
            subprocess.run(
                [
                    "taskkill",
                    "/PID",
                    str(server.pid),
                    "/T",
                    "/F",
                ],
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
                check=False,
                creationflags=creation_flags,
            )


def main() -> int:
    args = parse_args()
    try:
        result = run_browser_test(args)
    except Exception as error:
        print(f"mobile resize regression failed: {error}", file=sys.stderr)
        return 1
    print(json.dumps(result, indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
