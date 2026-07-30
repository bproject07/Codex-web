#!/usr/bin/env python3
"""Verify that desktop slash input reaches xterm instead of browser Quick Find."""

from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import subprocess
import time
from typing import Any
from urllib.error import URLError
from urllib.request import Request, urlopen

from playwright.sync_api import Browser, TimeoutError as PlaywrightTimeoutError, sync_playwright


TOKEN = "desktop-slash-regression-token-2026"


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
    parser.add_argument(
        "--system-firefox",
        type=Path,
        help=(
            "Optionally validate the Firefox Quick Find bar with Selenium and "
            "the specified Firefox executable"
        ),
    )
    parser.add_argument("--port", type=int, default=8800)
    return parser.parse_args()


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


def run_browser(
    browser: Browser,
    port: int,
    *,
    require_active_focus: bool,
) -> dict[str, Any]:
    context = browser.new_context(viewport={"width": 1100, "height": 720})
    page = context.new_page()
    sent_frames: list[str | bytes] = []

    def observe_socket(socket: Any) -> None:
        socket.on("framesent", lambda payload: sent_frames.append(payload))

    page.on("websocket", observe_socket)
    page.goto(
        f"http://127.0.0.1:{port}/?token={TOKEN}",
        wait_until="domcontentloaded",
    )
    page.locator(".status--connected").wait_for(state="visible")
    page.locator(".xterm-helper-textarea").wait_for(state="attached")
    page.wait_for_timeout(400)
    initial_active = page.evaluate("document.activeElement?.className ?? null")

    # Any non-editable header control works; the Menu trigger is always
    # present. A bare "/" does not open the menu, so routing still applies.
    header_button = page.locator(".header-menu-trigger")
    header_button.focus()
    active_before_slash = page.evaluate(
        "document.activeElement?.getAttribute('title') ?? null"
    )
    sent_frames.clear()
    page.keyboard.press("/")
    try:
        page.wait_for_function(
            """() => document.querySelector(
              ".xterm-rows",
            )?.textContent?.includes("/") ?? false""",
            timeout=3_000,
        )
    except PlaywrightTimeoutError:
        pass

    slash_frames = [
        payload
        for payload in sent_frames
        if isinstance(payload, bytes) and payload == b"/"
    ]
    active_after_slash = page.evaluate(
        "document.activeElement?.className ?? null"
    )
    terminal_contains_slash = page.locator(".xterm-rows").evaluate(
        "element => element.textContent?.includes('/') ?? false"
    )

    page.evaluate(
        """() => {
          const input = document.createElement("input");
          input.id = "slash-regression-input";
          document.body.appendChild(input);
          input.focus();
        }"""
    )
    sent_frames.clear()
    page.keyboard.press("/")
    page.wait_for_timeout(100)
    input_value = page.locator("#slash-regression-input").input_value()
    input_frames = [
        payload
        for payload in sent_frames
        if isinstance(payload, bytes) and payload == b"/"
    ]

    page.locator("#slash-regression-input").evaluate("element => element.remove()")
    header_button.focus()
    sent_frames.clear()
    page.keyboard.press("Control+/")
    page.wait_for_timeout(100)
    modified_frames = [
        payload
        for payload in sent_frames
        if isinstance(payload, bytes) and payload == b"/"
    ]

    result = {
        "initialActive": initial_active,
        "activeBeforeSlash": active_before_slash,
        "slashFrames": len(slash_frames),
        "activeAfterSlash": active_after_slash,
        "terminalContainsSlash": terminal_contains_slash,
        "inputValue": input_value,
        "inputFrames": len(input_frames),
        "modifiedFrames": len(modified_frames),
    }

    assert len(slash_frames) == 1, result
    if require_active_focus:
        assert active_after_slash == "xterm-helper-textarea", result
    assert terminal_contains_slash, result
    assert input_value == "/", result
    assert not input_frames, result
    assert not modified_frames, result

    context.close()
    return result


def run_system_firefox(firefox_path: Path, port: int) -> dict[str, Any]:
    from selenium import webdriver
    from selenium.webdriver.common.by import By
    from selenium.webdriver.firefox.options import Options
    from selenium.webdriver.firefox.service import Service
    from selenium.webdriver.support.ui import WebDriverWait

    options = Options()
    options.binary_location = str(firefox_path)
    options.add_argument("-headless")
    service = Service(service_args=["--allow-system-access"])
    driver = webdriver.Firefox(options=options, service=service)

    def read_find_bar() -> dict[str, Any]:
        driver.set_context(driver.CONTEXT_CHROME)
        try:
            return driver.execute_async_script(
                """
                const done = arguments[arguments.length - 1];
                gBrowser.getFindBar().then((findBar) => {
                  done({
                    hidden: findBar.hidden,
                    value: findBar._findField?.value ?? "",
                  });
                }, (error) => done({ error: String(error) }));
                """
            )
        finally:
            driver.set_context(driver.CONTEXT_CONTENT)

    try:
        driver.set_window_size(1100, 720)
        driver.get(f"http://127.0.0.1:{port}/?token={TOKEN}")
        wait = WebDriverWait(driver, 15)
        wait.until(
            lambda current: current.find_element(
                By.CSS_SELECTOR, ".status--connected"
            ).is_displayed()
        )
        wait.until(
            lambda current: current.find_element(
                By.CSS_SELECTOR, ".xterm-helper-textarea"
            )
        )

        initial_active = driver.execute_script(
            "return document.activeElement?.className ?? null"
        )
        header_button = driver.find_element(
            By.CSS_SELECTOR, ".header-menu-trigger"
        )
        driver.execute_script("arguments[0].focus()", header_button)
        active_before_slash = driver.execute_script(
            "return document.activeElement?.getAttribute('title') ?? null"
        )
        header_button.send_keys("/")
        wait.until(
            lambda current: "/"
            in current.find_element(By.CSS_SELECTOR, ".xterm-rows").text
        )
        active_after_slash = driver.execute_script(
            "return document.activeElement?.className ?? null"
        )
        find_bar_after_slash = read_find_bar()

        driver.execute_script(
            """
            const input = document.createElement("input");
            input.id = "slash-regression-input";
            document.body.appendChild(input);
            input.focus();
            """
        )
        slash_input = driver.find_element(By.ID, "slash-regression-input")
        slash_input.send_keys("/")
        input_value = slash_input.get_attribute("value")
        input_active = driver.execute_script(
            "return document.activeElement?.id ?? null"
        )

        result = {
            "version": driver.capabilities.get("browserVersion"),
            "initialActive": initial_active,
            "activeBeforeSlash": active_before_slash,
            "activeAfterSlash": active_after_slash,
            "findBarAfterSlash": find_bar_after_slash,
            "terminalContainsSlash": True,
            "inputValue": input_value,
            "inputActive": input_active,
        }
        assert find_bar_after_slash.get("hidden") is True, result
        assert input_value == "/", result
        assert input_active == "slash-regression-input", result
        return result
    finally:
        driver.quit()


def main() -> int:
    args = parse_args()
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
            chromium = playwright.chromium.launch(
                executable_path=str(args.chrome),
                headless=True,
                args=["--disable-background-networking"],
            )
            firefox = playwright.firefox.launch(headless=True)
            result = {
                "chromium": run_browser(
                    chromium,
                    args.port,
                    require_active_focus=True,
                ),
                "playwrightFirefox": run_browser(
                    firefox,
                    args.port,
                    require_active_focus=False,
                ),
            }
            chromium.close()
            firefox.close()
        if args.system_firefox:
            if not args.system_firefox.is_file():
                raise FileNotFoundError(args.system_firefox)
            result["systemFirefox"] = run_system_firefox(
                args.system_firefox,
                args.port,
            )
        print(json.dumps(result, indent=2))
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
