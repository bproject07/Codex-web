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


def paths_equal(left: str | Path, right: str | Path) -> bool:
    return os.path.normcase(os.path.realpath(str(left))) == os.path.normcase(
        os.path.realpath(str(right))
    )


def prepare_sessions(port: int, count: int) -> None:
    sessions = request_json(port)
    while len(sessions) < count:
        request_json(port, "POST")
        sessions = request_json(port)
    if len(sessions) != count:
        raise AssertionError(
            f"expected {count} sessions, received {len(sessions)}"
        )


def wait_for_tab_count(page: Page, count: int) -> None:
    page.wait_for_function(
        "expected => document.querySelectorAll('.session-tab').length"
        " === expected",
        arg=count,
    )


def wait_for_tabs(page: Page, port: int, count: int) -> None:
    page.goto(
        f"http://127.0.0.1:{port}/?token={TOKEN}",
        wait_until="domcontentloaded",
    )
    page.locator(".session-tab").first.wait_for(state="visible")
    wait_for_tab_count(page, count)
    # The identity assertions require the attached state, not just tabs.
    page.locator(".status--connected").wait_for(state="visible", timeout=20_000)


def tab_metrics(page: Page) -> dict[str, Any]:
    return page.evaluate(
        """() => {
          const header = document.querySelector(".app-header");
          const tabs = document.querySelector(".session-tabs");
          const peer = document.querySelector(".session-peer-button");
          const menuTrigger = document.querySelector(".header-menu-trigger");
          const statusDot = document.querySelector(
            ".app-identity .status-dot",
          );
          const rect = element => element?.getBoundingClientRect().toJSON();
          return {
            header: rect(header),
            tabs: rect(tabs),
            peer: rect(peer),
            menuTrigger: rect(menuTrigger),
            statusDot: rect(statusDot),
            reconnectButtons: document.querySelectorAll(
              ".header-reconnect-button",
            ).length,
            settingsButtons: document.querySelectorAll(
              ".header-settings-button",
            ).length,
            contextButtons: Array.from(
              document.querySelectorAll(".header-context button"),
              (button) => button.className,
            ),
            tabLefts: Array.from(
              document.querySelectorAll(".session-tab"),
              (tab) => tab.getBoundingClientRect().left,
            ),
            identityText:
              document.querySelector(".app-identity")?.textContent ?? "",
            identityTitle:
              document
                .querySelector(".app-context")
                ?.getAttribute("title") ?? "",
            identityStatusClass:
              document
                .querySelector(".app-identity .app-status")
                ?.className ?? "",
            documentClientWidth: document.documentElement.clientWidth,
            documentScrollWidth: document.documentElement.scrollWidth,
            standaloneStatusPills: document.querySelectorAll(
              ".header-actions .status",
            ).length,
            headerButtons: Array.from(
              document.querySelectorAll(".header-actions button"),
              (button) => button.className,
            ),
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


def assert_header_holds_only_tabs_and_peer(metrics: dict[str, Any]) -> None:
    """The tab row holds only tabs, scroll arrows, and @cwt; every other
    action lives behind the ellipsis Menu beside the identity."""
    allowed = (
        "session-tab",
        "session-scroll-button",
        "session-peer-button",
    )
    unexpected = [
        class_name
        for class_name in metrics["headerButtons"]
        if not any(token in class_name for token in allowed)
    ]
    assert not unexpected, unexpected


def assert_identity_shows_context(
    metrics: dict[str, Any], viewport_width: int, *, stacked: bool
) -> None:
    """The header shows path → agent → status dot → ellipsis Menu, then the
    left-aligned tab strip. No branding, no standalone status pill, no manual
    Reconnect, and no far-right Settings button."""
    repository = str(Path(__file__).resolve().parents[1])
    identity = metrics["identityText"]
    assert "Codex Web Terminal" not in identity, identity
    assert "Unofficial" not in identity, identity
    # The full native project path is real DOM text (it may only visually
    # ellipsize) and is repeated in the heading title. Compare with the same
    # normalization as workspace-picker-regression so a subst/junction/case
    # difference between this checkout and the server's canonicalized path
    # does not fail a correctly behaving app.
    assert os.path.normcase(repository) in os.path.normcase(identity), identity
    assert paths_equal(metrics["identityTitle"], repository), (
        metrics["identityTitle"]
    )
    assert "status--connected" in metrics["identityStatusClass"], metrics
    assert metrics["standaloneStatusPills"] == 0, metrics
    assert metrics["reconnectButtons"] == 0, metrics
    assert metrics["settingsButtons"] == 0, metrics
    # The identity/menu group may hold exactly one control: the Menu trigger.
    unexpected_context_buttons = [
        class_name
        for class_name in metrics["contextButtons"]
        if "header-menu-trigger" not in class_name
    ]
    assert not unexpected_context_buttons, unexpected_context_buttons
    dot = metrics["statusDot"]
    assert dot is not None, metrics
    assert dot["left"] >= 0 and dot["right"] <= viewport_width, dot

    tabs = metrics["tabs"]
    menu = metrics["menuTrigger"]
    assert menu is not None, metrics
    # The Menu trigger follows the dot immediately and stays tappable.
    assert menu["left"] >= dot["right"], (menu, dot)
    assert menu["left"] - dot["right"] <= 40, (menu, dot)
    assert menu["right"] <= viewport_width, menu
    minimum_target = 43 if stacked else 30
    assert menu["width"] >= minimum_target, menu
    assert menu["height"] >= minimum_target, menu
    if stacked:
        # Narrow layout: identity+Menu row on top, then the left-aligned
        # scrollable tab row.
        assert tabs["top"] >= menu["bottom"] - 1, (tabs, menu)
        assert tabs["left"] <= 60, tabs
    else:
        # Single-row layout: the tab strip starts directly after the Menu.
        assert tabs["left"] >= menu["right"], (tabs, menu)
        assert tabs["left"] - menu["right"] <= 120, (tabs, menu)
    # Tabs keep their left-to-right creation order.
    tab_lefts = metrics["tabLefts"]
    assert tab_lefts == sorted(tab_lefts), tab_lefts
    # The long path must never introduce page-level horizontal scrolling.
    assert (
        metrics["documentScrollWidth"] <= metrics["documentClientWidth"] + 1
    ), metrics


def exercise_header_menu(page: Page, viewport_width: int) -> list[str]:
    """Open the ellipsis Menu, verify contents and viewport fit, check
    Escape/focus return, and confirm the Settings item opens Settings."""
    trigger = page.locator(".header-menu-trigger")
    assert trigger.get_attribute("title") == "Menu"
    assert trigger.get_attribute("aria-haspopup") == "menu"
    assert trigger.get_attribute("aria-expanded") == "false"

    trigger.click()
    menu = page.locator("[role='menu']")
    menu.wait_for(state="visible")
    assert trigger.get_attribute("aria-expanded") == "true"
    box = menu.bounding_box()
    assert box is not None
    assert box["x"] >= -1, box
    assert box["x"] + box["width"] <= viewport_width + 1, box
    labels = menu.locator(
        "[role='menuitem'] .header-menu-item-label"
    ).all_inner_texts()
    assert labels == [
        "New terminal",
        "Settings",
        "Manage sessions (4/20)",
        "Full screen",
    ], labels

    page.keyboard.press("Escape")
    menu.wait_for(state="hidden")
    page.wait_for_function(
        "() => document.activeElement?.classList.contains("
        "'header-menu-trigger')"
    )

    trigger.click()
    menu.wait_for(state="visible")
    page.locator(".header-menu-item--settings").click()
    settings_panel = page.locator(".settings-panel")
    settings_panel.wait_for(state="visible")
    assert page.locator("[role='menu']").count() == 0
    page.get_by_role("button", name="Close settings").click()
    settings_panel.wait_for(state="hidden")
    return labels


def arrow_state(page: Page) -> dict[str, Any]:
    left = page.locator(".session-scroll-button--left")
    right = page.locator(".session-scroll-button--right")
    return {
        "leftVisible": left.count() > 0 and left.is_visible(),
        "rightVisible": right.count() > 0 and right.is_visible(),
        "leftDisabled": left.count() > 0 and left.is_disabled(),
        "rightDisabled": right.count() > 0 and right.is_disabled(),
    }


def run_desktop(browser: Browser, port: int) -> dict[str, Any]:
    context = browser.new_context(viewport={"width": 1280, "height": 720})
    page = context.new_page()
    wait_for_tabs(page, port, 4)

    # Phase 1 — four tabs fit at 1280px: the strip owns the width after the
    # identity/Menu group and shows no arrows and no reserved gutters.
    fitting = tab_metrics(page)
    fitting_arrows = arrow_state(page)
    assert fitting["selected"] == 1
    assert fitting["scrollWidth"] <= fitting["clientWidth"] + 2, fitting
    assert not fitting_arrows["leftVisible"], fitting_arrows
    assert not fitting_arrows["rightVisible"], fitting_arrows
    assert fitting["peer"]["right"] <= 1280
    # No gutter: @cwt directly follows the strip while the tabs fit.
    assert fitting["peer"]["left"] - fitting["tabs"]["right"] <= 16, fitting
    assert_header_holds_only_tabs_and_peer(fitting)
    assert_identity_shows_context(fitting, 1280, stacked=False)
    menu_labels = exercise_header_menu(page, 1280)

    # Phase 2 — shrink the window until the same four tabs genuinely
    # overflow, then widen back: the arrows must appear and disappear at the
    # measured threshold without oscillating.
    page.set_viewport_size({"width": 800, "height": 720})
    page.wait_for_function(
        "() => document.querySelectorAll('.session-scroll-button').length"
        " === 2"
    )
    narrow = tab_metrics(page)
    narrow_arrows = arrow_state(page)
    assert narrow["scrollWidth"] > narrow["clientWidth"] + 2, narrow
    assert narrow_arrows["leftVisible"] and narrow_arrows["rightVisible"], (
        narrow_arrows
    )
    assert narrow_arrows["leftDisabled"], narrow_arrows
    assert not narrow_arrows["rightDisabled"], narrow_arrows
    assert narrow["documentScrollWidth"] <= narrow["documentClientWidth"] + 1

    page.set_viewport_size({"width": 1280, "height": 720})
    page.wait_for_function(
        "() => document.querySelectorAll('.session-scroll-button').length"
        " === 0"
    )
    refit = tab_metrics(page)
    assert refit["scrollWidth"] <= refit["clientWidth"] + 2, refit

    # Phase 3 — grow to twelve tabs at 1280px: real overflow, arrow paging,
    # wheel scrolling, and edge-disable states. Sessions created through the
    # API are picked up on reload; the browser does not receive live pushes.
    prepare_sessions(port, 12)
    wait_for_tabs(page, port, 12)
    page.wait_for_function(
        "() => document.querySelectorAll('.session-scroll-button').length"
        " === 2"
    )
    before = tab_metrics(page)
    overflow_arrows = arrow_state(page)
    assert before["scrollWidth"] > before["clientWidth"] + 2, before
    assert overflow_arrows["leftVisible"] and overflow_arrows["rightVisible"]
    assert overflow_arrows["leftDisabled"], overflow_arrows
    assert not overflow_arrows["rightDisabled"], overflow_arrows
    # The overlay arrows must not reserve width: the strip still ends right
    # beside @cwt while overflowing.
    assert before["peer"]["left"] - before["tabs"]["right"] <= 16, before
    assert before["tabLefts"] == sorted(before["tabLefts"]), before

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
    assert after_wheel["scrollLeft"] > before["scrollLeft"]
    assert not arrow_state(page)["leftDisabled"]

    page.locator(".session-scroll-button--right").click()
    page.wait_for_timeout(300)
    after_arrow = tab_metrics(page)
    assert after_arrow["scrollLeft"] > after_wheel["scrollLeft"]

    page.locator(".session-tabs").evaluate(
        "element => { element.scrollLeft = element.scrollWidth; }"
    )
    page.wait_for_timeout(250)
    end_arrows = arrow_state(page)
    assert end_arrows["rightDisabled"], end_arrows
    assert not end_arrows["leftDisabled"], end_arrows

    third_tab = page.locator(".session-tab").nth(2)
    third_tab.click()
    page.wait_for_function(
        """() => document.querySelectorAll(
          '.session-tab[aria-selected="true"]',
        ).length === 1"""
    )
    selected_name = third_tab.get_attribute("title")
    assert "Codex 3" in (selected_name or "")
    # Selecting a tab scrolls it into view.
    page.wait_for_function(
        """() => {
          const active = document.querySelector('.session-tab-shell--active');
          const list = document.querySelector('.session-tabs');
          if (!active || !list) return false;
          const a = active.getBoundingClientRect();
          const l = list.getBoundingClientRect();
          return a.left >= l.left - 1 && a.right <= l.right + 1;
        }"""
    )

    result = {
        "fitting": fitting,
        "narrowArrows": narrow_arrows,
        "overflowArrows": overflow_arrows,
        "before": before,
        "afterWheel": after_wheel,
        "afterArrow": after_arrow,
        "endArrows": end_arrows,
        "selectedTitle": selected_name,
        "menuLabels": menu_labels,
    }

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
    wait_for_tabs(page, port, 4)

    before = tab_metrics(page)
    swipe_tabs(page)
    page.wait_for_timeout(350)
    after_swipe = tab_metrics(page)

    last_tab = page.locator(".session-tab").last
    last_tab.click()
    page.wait_for_timeout(250)
    after_select = tab_metrics(page)

    page.locator(".header-menu-trigger").click()
    page.locator("[role='menu']").wait_for(state="visible")
    menu_box = page.locator("[role='menu']").bounding_box()
    assert menu_box is not None
    assert menu_box["x"] >= -1 and menu_box["x"] + menu_box["width"] <= 361, (
        menu_box
    )
    new_label = page.locator(".session-new-button").inner_text()
    manage_label = page.locator(".session-manage-button").inner_text()
    page.keyboard.press("Escape")
    page.locator("[role='menu']").wait_for(state="hidden")

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
        "newLabel": new_label,
        "manageLabel": manage_label,
    }

    assert before["selected"] == 1
    assert before["scrollWidth"] > before["clientWidth"]
    assert after_swipe["scrollLeft"] > before["scrollLeft"]
    assert after_select["selected"] == 1
    assert before["pageScrollY"] == 0
    assert after_select["pageScrollY"] == 0
    # The identity row hosts the 44px ellipsis Menu trigger — the only route
    # to Settings — so the stacked header is one touch-target tall plus the
    # tab row. The keyboard-open (max-height: 560px) layout compacts it.
    assert before["header"]["height"] < 110, before["header"]
    assert not result["leftArrowVisible"]
    assert not result["rightArrowVisible"]
    assert result["newLabel"].strip() == "New terminal"
    assert result["manageLabel"].strip() == "Manage sessions (4/20)"
    assert before["peer"]["right"] <= 360
    assert_header_holds_only_tabs_and_peer(before)
    assert_identity_shows_context(before, 360, stacked=True)

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
        prepare_sessions(args.port, 4)
        with sync_playwright() as playwright:
            browser = playwright.chromium.launch(
                executable_path=str(args.chrome),
                headless=True,
                args=["--disable-background-networking"],
            )
            # Mobile runs first with four sessions; the desktop flow then
            # grows the same server to twelve for its overflow phases.
            result = {
                "mobile": run_mobile(browser, args.port),
                "desktop": run_desktop(browser, args.port),
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
