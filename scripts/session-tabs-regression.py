#!/usr/bin/env python3
"""Exercise desktop and mobile session-tab navigation on a disposable server."""

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

from playwright.sync_api import Browser, Page, sync_playwright


TOKEN = "session-tabs-regression-token-2026"
PHONE_USER_AGENT = (
    "Mozilla/5.0 (Linux; Android 10; K) AppleWebKit/537.36 "
    "(KHTML, like Gecko) Chrome/150.0.0.0 Mobile Safari/537.36"
)


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
    parser.add_argument("--port", type=int, default=8798)
    return parser.parse_args()


def request_json(port: int, method: str = "GET") -> Any:
    request = Request(
        f"http://127.0.0.1:{port}/api/sessions",
        method=method,
        headers={"Authorization": f"Bearer {TOKEN}"},
    )
    with urlopen(request, timeout=10) as response:
        return json.loads(response.read())


def wait_for_server(port: int, process: subprocess.Popen[bytes]) -> None:
    deadline = time.monotonic() + 20
    while time.monotonic() < deadline:
        if process.poll() is not None:
            raise RuntimeError(f"test server exited with code {process.returncode}")
        try:
            if isinstance(request_json(port), list):
                return
        except (OSError, URLError):
            pass
        time.sleep(0.1)
    raise TimeoutError("test server did not become ready")


def prepare_sessions(port: int) -> None:
    sessions = request_json(port)
    while len(sessions) < 4:
        request_json(port, "POST")
        sessions = request_json(port)
    if len(sessions) != 4:
        raise AssertionError(f"expected four sessions, received {len(sessions)}")


def wait_for_tabs(page: Page, port: int) -> None:
    page.goto(
        f"http://127.0.0.1:{port}/?token={TOKEN}",
        wait_until="domcontentloaded",
    )
    page.locator(".session-tab").first.wait_for(state="visible")
    page.wait_for_function(
        "() => document.querySelectorAll('.session-tab').length === 4"
    )


def tab_metrics(page: Page) -> dict[str, Any]:
    return page.evaluate(
        """() => {
          const header = document.querySelector(".app-header");
          const tabs = document.querySelector(".session-tabs");
          const create = document.querySelector(".session-new-button");
          const manage = document.querySelector(".session-manage-button");
          const rect = element => element?.getBoundingClientRect().toJSON();
          return {
            header: rect(header),
            tabs: rect(tabs),
            create: rect(create),
            manage: rect(manage),
            scrollLeft: tabs?.scrollLeft ?? 0,
            scrollWidth: tabs?.scrollWidth ?? 0,
            clientWidth: tabs?.clientWidth ?? 0,
            pageScrollY: window.scrollY,
            selected: document.querySelectorAll(
              '.session-tab[aria-selected="true"]',
            ).length,
          };
        }"""
    )


def run_desktop(browser: Browser, port: int) -> dict[str, Any]:
    context = browser.new_context(viewport={"width": 1280, "height": 720})
    page = context.new_page()
    wait_for_tabs(page, port)

    before = tab_metrics(page)
    tab_box = page.locator(".session-tabs").bounding_box()
    if not tab_box:
        raise AssertionError("desktop session tab strip has no layout box")
    page.mouse.move(
        tab_box["x"] + tab_box["width"] / 2,
        tab_box["y"] + tab_box["height"] / 2,
    )
    page.mouse.wheel(0, 240)
    page.wait_for_timeout(250)
    after_wheel = tab_metrics(page)

    third_tab = page.locator(".session-tab").nth(2)
    third_tab.click()
    page.wait_for_function(
        """() => document.querySelectorAll(
          '.session-tab[aria-selected="true"]',
        ).length === 1"""
    )
    selected_name = third_tab.get_attribute("title")

    result = {
        "before": before,
        "afterWheel": after_wheel,
        "selectedTitle": selected_name,
        "leftArrowVisible": page.locator(
            ".session-scroll-button--left"
        ).is_visible(),
        "rightArrowVisible": page.locator(
            ".session-scroll-button--right"
        ).is_visible(),
    }

    assert before["selected"] == 1
    assert before["scrollWidth"] > before["clientWidth"]
    assert after_wheel["scrollLeft"] > before["scrollLeft"]
    assert result["leftArrowVisible"]
    assert result["rightArrowVisible"]
    assert "Codex 3" in (selected_name or "")
    assert before["create"]["right"] <= 1280
    assert before["manage"]["right"] <= 1280

    context.close()
    return result


def swipe_tabs(page: Page) -> None:
    cdp = page.context.new_cdp_session(page)
    box = page.locator(".session-tabs").bounding_box()
    if not box:
        raise AssertionError("mobile session tab strip has no layout box")
    y = box["y"] + box["height"] / 2
    start_x = box["x"] + box["width"] - 8
    end_x = box["x"] + 8
    cdp.send(
        "Input.dispatchTouchEvent",
        {
            "type": "touchStart",
            "touchPoints": [{"x": start_x, "y": y}],
        },
    )
    for step in range(1, 6):
        x = start_x + (end_x - start_x) * step / 5
        cdp.send(
            "Input.dispatchTouchEvent",
            {
                "type": "touchMove",
                "touchPoints": [{"x": x, "y": y}],
            },
        )
    cdp.send("Input.dispatchTouchEvent", {"type": "touchEnd", "touchPoints": []})


def run_mobile(browser: Browser, port: int) -> dict[str, Any]:
    context = browser.new_context(
        viewport={"width": 360, "height": 639},
        screen={"width": 360, "height": 780},
        device_scale_factor=3,
        is_mobile=True,
        has_touch=True,
        user_agent=PHONE_USER_AGENT,
    )
    page = context.new_page()
    wait_for_tabs(page, port)

    before = tab_metrics(page)
    swipe_tabs(page)
    page.wait_for_timeout(350)
    after_swipe = tab_metrics(page)

    last_tab = page.locator(".session-tab").last
    last_tab.click()
    page.wait_for_timeout(250)
    after_select = tab_metrics(page)

    result = {
        "before": before,
        "afterSwipe": after_swipe,
        "afterSelect": after_select,
        "leftArrowVisible": page.locator(
            ".session-scroll-button--left"
        ).is_visible(),
        "rightArrowVisible": page.locator(
            ".session-scroll-button--right"
        ).is_visible(),
        "newLabel": page.locator(".session-new-button").inner_text(),
        "manageLabel": page.locator(".session-manage-button").inner_text(),
    }

    assert before["selected"] == 1
    assert before["scrollWidth"] > before["clientWidth"]
    assert after_swipe["scrollLeft"] > before["scrollLeft"]
    assert after_select["selected"] == 1
    assert before["pageScrollY"] == 0
    assert after_select["pageScrollY"] == 0
    assert before["header"]["height"] < 90
    assert not result["leftArrowVisible"]
    assert not result["rightArrowVisible"]
    assert result["newLabel"].strip() == "+"
    assert result["manageLabel"].strip() == "4"
    assert before["create"]["right"] <= 360
    assert before["manage"]["right"] <= 360

    context.close()
    return result


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
        "--new-session-command",
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
        prepare_sessions(args.port)
        with sync_playwright() as playwright:
            browser = playwright.chromium.launch(
                executable_path=str(args.chrome),
                headless=True,
                args=["--disable-background-networking"],
            )
            result = {
                "desktop": run_desktop(browser, args.port),
                "mobile": run_mobile(browser, args.port),
            }
            browser.close()
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
