#!/usr/bin/env python3
"""Smoke-test the real Codex slash menu in a keyboard-sized mobile viewport."""

from __future__ import annotations

import argparse
import base64
import json
import os
from pathlib import Path
import subprocess
import time
from typing import Any
from urllib.error import URLError
from urllib.request import Request, urlopen

from playwright.sync_api import sync_playwright


TOKEN = "mobile-codex-smoke-token-2026"
USER_AGENT = (
    "Mozilla/5.0 (Linux; Android 10; K) AppleWebKit/537.36 "
    "(KHTML, like Gecko) Chrome/150.0.0.0 Mobile Safari/537.36"
)


def wait_for_server(port: int, process: subprocess.Popen[bytes]) -> None:
    request = Request(
        f"http://127.0.0.1:{port}/api/sessions",
        headers={"Authorization": f"Bearer {TOKEN}"},
    )
    deadline = time.monotonic() + 20
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


def main() -> int:
    repository = Path(__file__).resolve().parents[1]
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--server",
        type=Path,
        default=repository / "dist-batched" / "codex-web.exe",
    )
    parser.add_argument("--port", type=int, default=8792)
    parser.add_argument(
        "--screenshot",
        type=Path,
        default=Path(r"C:\tmp\codex-web-mobile-slash.png"),
    )
    args = parser.parse_args()

    creation_flags = subprocess.CREATE_NO_WINDOW if os.name == "nt" else 0
    server = subprocess.Popen(
        [
            str(args.server),
            "--host",
            "127.0.0.1",
            "--port",
            str(args.port),
            "--project",
            str(repository),
            "--command",
            "codex",
            "--token",
            TOKEN,
            "--no-open-browser",
            "--log-level",
            "warn",
        ],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
        creationflags=creation_flags,
    )

    try:
        wait_for_server(args.port, server)
        with sync_playwright() as playwright:
            browser = playwright.chromium.launch(
                executable_path=r"C:\Program Files\Google\Chrome\Application\chrome.exe",
                headless=True,
                args=["--disable-background-networking"],
            )
            context = browser.new_context(
                viewport={"width": 360, "height": 639},
                screen={"width": 360, "height": 780},
                device_scale_factor=3,
                is_mobile=True,
                has_touch=True,
                user_agent=USER_AGENT,
            )
            page = context.new_page()
            cdp = context.new_cdp_session(page)
            cdp.send("Emulation.setTouchEmulationEnabled", {"enabled": True})

            def set_height(height: int) -> None:
                cdp.send(
                    "Emulation.setDeviceMetricsOverride",
                    {
                        "width": 360,
                        "height": height,
                        "deviceScaleFactor": 3,
                        "mobile": True,
                        "screenWidth": 360,
                        "screenHeight": 780,
                    },
                )

            set_height(639)
            sent_frames: list[str | bytes] = []

            def observe_socket(socket: Any) -> None:
                socket.on("framesent", lambda payload: sent_frames.append(payload))

            page.on("websocket", observe_socket)
            page.goto(
                f"http://127.0.0.1:{args.port}/?token={TOKEN}",
                wait_until="domcontentloaded",
            )
            textarea = page.locator(".xterm-helper-textarea")
            textarea.wait_for(state="attached")
            page.wait_for_timeout(3_500)
            sent_frames.clear()

            set_height(345)
            page.wait_for_timeout(120)
            textarea.focus()
            page.keyboard.type("mobile input remains visible")
            page.wait_for_timeout(1_400)
            composer_rows = page.evaluate(
                """Array.from(
                  document.querySelectorAll(".xterm-rows > div"),
                  row => row.textContent ?? "",
                )"""
            )
            composer_screenshot = args.screenshot.with_name(
                f"{args.screenshot.stem}-composer{args.screenshot.suffix}"
            )
            composer_image = cdp.send(
                "Page.captureScreenshot",
                {"format": "png", "fromSurface": True},
            )
            composer_screenshot.write_bytes(
                base64.b64decode(composer_image["data"])
            )

            for _ in "mobile input remains visible":
                page.keyboard.press("Backspace")
            page.keyboard.type("/")
            page.wait_for_timeout(700)
            page.keyboard.press("ArrowDown")
            page.wait_for_timeout(250)

            result = page.evaluate(
                """() => {
                  const terminal = document.querySelector(".terminal-view");
                  const textarea = document.querySelector(".xterm-helper-textarea");
                  const rows = Array.from(
                    document.querySelectorAll(".xterm-rows > div"),
                    row => row.textContent ?? "",
                  );
                  return {
                    innerHeight: window.innerHeight,
                    visualHeight: window.visualViewport?.height,
                    scrollY: window.scrollY,
                    terminalRect: terminal?.getBoundingClientRect().toJSON(),
                    textareaRect: textarea?.getBoundingClientRect().toJSON(),
                    visibleRows: rows,
                    activeElement: document.activeElement?.className ?? null,
                  };
                }"""
            )
            screenshot = cdp.send(
                "Page.captureScreenshot",
                {"format": "png", "fromSurface": True},
            )
            args.screenshot.write_bytes(base64.b64decode(screenshot["data"]))
            resize_frames = []
            for payload in sent_frames:
                if not isinstance(payload, str):
                    continue
                try:
                    parsed = json.loads(payload)
                except json.JSONDecodeError:
                    continue
                if isinstance(parsed, dict) and parsed.get("type") == "resize":
                    resize_frames.append(parsed)

            result["resizeFrames"] = resize_frames
            result["composerTextVisible"] = (
                "mobile input remains visible" in " ".join(composer_rows)
            )
            result["composerScreenshot"] = str(composer_screenshot)
            result["screenshot"] = str(args.screenshot)
            print(json.dumps(result, indent=2))

            assert result["innerHeight"] == 345
            assert result["visualHeight"] == 345
            assert result["scrollY"] == 0
            assert result["activeElement"] == "xterm-helper-textarea"
            assert len(resize_frames) == 1
            assert resize_frames[0]["rows"] <= 16
            assert result["composerTextVisible"]
            assert any("/model" in row for row in result["visibleRows"])
            assert any("/fast" in row for row in result["visibleRows"])
            assert result["textareaRect"]["bottom"] <= result["terminalRect"]["bottom"] + 1

            context.close()
            browser.close()
            return 0
    finally:
        if server.poll() is None:
            subprocess.run(
                ["taskkill", "/PID", str(server.pid), "/T", "/F"],
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
                check=False,
                creationflags=creation_flags,
            )


if __name__ == "__main__":
    raise SystemExit(main())
