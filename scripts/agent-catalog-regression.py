#!/usr/bin/env python3
"""Exercise agent discovery and the picker against an isolated disposable server.

The script owns only the server process it starts and refuses to use the live
development ports 8788, 8789, and 8790. It creates all fake CLI commands in a
temporary directory and never installs or updates a real agent.
"""

from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import secrets
import signal
import socket
import subprocess
import tempfile
import time
from typing import Any
from urllib.error import HTTPError, URLError
from urllib.request import Request, urlopen

from playwright.sync_api import Browser, Page, sync_playwright


DEFAULT_PORT = 8802
PROTECTED_LIVE_PORTS = frozenset({8788, 8789, 8790})
PHONE_USER_AGENT = (
    "Mozilla/5.0 (Linux; Android 10; K) AppleWebKit/537.36 "
    "(KHTML, like Gecko) Chrome/150.0.0.0 Mobile Safari/537.36"
)
EXPECTED_VERSION = "9.8.7"
CLAUDE_VERSION = "2.1.220"
AGY_VERSION = "1.1.7"


def parse_args() -> argparse.Namespace:
    repository = Path(__file__).resolve().parents[1]
    executable_name = "codex-web.exe" if os.name == "nt" else "codex-web"
    default_server = repository / "server" / "target" / "release" / executable_name
    default_chrome = (
        Path(r"C:\Program Files\Google\Chrome\Application\chrome.exe")
        if os.name == "nt"
        else None
    )
    if default_chrome is not None and not default_chrome.is_file():
        default_chrome = None

    parser = argparse.ArgumentParser(
        description=(
            "Validate the agent catalog and responsive picker on a disposable "
            "local server."
        )
    )
    parser.add_argument("--server", type=Path, default=default_server)
    parser.add_argument(
        "--chrome",
        type=Path,
        default=default_chrome,
        help="Optional Chromium/Chrome executable; Playwright default otherwise.",
    )
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


def request_json(
    port: int,
    token: str,
    path: str,
    *,
    method: str = "GET",
    payload: dict[str, Any] | None = None,
) -> Any:
    data = json.dumps(payload).encode("utf-8") if payload is not None else None
    headers = {
        "Authorization": f"Bearer {token}",
        "Accept": "application/json",
    }
    if data is not None:
        headers["Content-Type"] = "application/json"
    request = Request(
        f"http://127.0.0.1:{port}{path}",
        data=data,
        method=method,
        headers=headers,
    )
    try:
        with urlopen(request, timeout=5) as response:
            body = response.read()
            return json.loads(body) if body else None
    except HTTPError as error:
        body = error.read().decode("utf-8", errors="replace")
        raise RuntimeError(
            f"{path} returned HTTP {error.code}: {body[:500]}"
        ) from error


def wait_for_server(
    port: int,
    token: str,
    process: subprocess.Popen[bytes],
) -> dict[str, Any]:
    deadline = time.monotonic() + 20
    while time.monotonic() < deadline:
        if process.poll() is not None:
            stdout = (
                process.stdout.read().decode("utf-8", errors="replace")
                if process.stdout
                else ""
            )
            stderr = (
                process.stderr.read().decode("utf-8", errors="replace")
                if process.stderr
                else ""
            )
            details = "\n".join(part.strip() for part in (stdout, stderr) if part.strip())
            suffix = f":\n{details[-2000:]}" if details else ""
            raise RuntimeError(
                f"test server exited with code {process.returncode}{suffix}"
            )
        try:
            catalog = request_json(port, token, "/api/agent-catalog")
            if isinstance(catalog, dict):
                return catalog
        except (OSError, URLError, RuntimeError, json.JSONDecodeError):
            pass
        time.sleep(0.1)
    raise TimeoutError("test server did not become ready")


def write_ready_command(path: Path, output: str, label: str) -> None:
    if os.name == "nt":
        path.write_text(
            "@echo off\r\n"
            'if /i "%~1"=="--version" (\r\n'
            f"  echo {output}\r\n"
            "  exit /b 0\r\n"
            ")\r\n"
            f"echo disposable {label} fixture ready\r\n"
            "ping 127.0.0.1 -t >nul\r\n",
            encoding="utf-8",
        )
        return

    path.write_text(
        "#!/bin/sh\n"
        'if [ "${1:-}" = "--version" ]; then\n'
        f"  printf '%s\\n' '{output}'\n"
        "  exit 0\n"
        "fi\n"
        f"printf '%s\\n' 'disposable {label} fixture ready'\n"
        "while :; do sleep 60; done\n",
        encoding="utf-8",
    )
    path.chmod(0o700)


def write_fake_commands(root: Path) -> tuple[Path, Path, Path, Path]:
    auto_bin = root / "auto-bin"
    auto_bin.mkdir()
    if os.name == "nt":
        ready = root / "codex-fixture.cmd"
        claude = auto_bin / "claude.cmd"
        missing_agy = root / "missing-agy.cmd"
    else:
        ready = root / "codex-fixture"
        claude = auto_bin / "claude"
        missing_agy = root / "missing-agy"
    write_ready_command(ready, f"codex-cli {EXPECTED_VERSION}", "Codex")
    return ready, auto_bin, claude, missing_agy


def isolated_server_environment(root: Path, auto_bin: Path) -> dict[str, str]:
    environment = os.environ.copy()
    environment.pop("CODEX_INSTALL_DIR", None)
    if os.name == "nt":
        system_root = Path(environment.get("SystemRoot", r"C:\Windows"))
        environment["PATH"] = os.pathsep.join(
            (str(auto_bin), str(system_root / "System32"), str(system_root))
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
        environment["PATH"] = os.pathsep.join((str(auto_bin), "/usr/bin", "/bin"))
        home = root / "isolated-home"
        home.mkdir()
        environment["HOME"] = str(home)
    return environment


def validate_catalog(
    catalog: Any,
    expected_states: dict[str, str],
) -> dict[str, Any]:
    assert isinstance(catalog, dict), catalog
    assert catalog.get("schemaVersion") == 1, catalog

    server = catalog.get("server")
    assert isinstance(server, dict), catalog
    for key in ("os", "arch", "shell"):
        assert isinstance(server.get(key), str) and server[key], server

    agents = catalog.get("agents")
    assert isinstance(agents, list) and len(agents) == 3, catalog
    by_kind = {agent.get("kind"): agent for agent in agents if isinstance(agent, dict)}
    assert set(by_kind) == {"codex", "claude", "agy"}, catalog

    assert expected_states.keys() == by_kind.keys(), expected_states
    for kind, expected_state in expected_states.items():
        agent = by_kind[kind]
        assert agent.get("state") == expected_state, agent
        expected_configuration = (
            "auto" if kind == "claude" and os.name == "nt" else "override"
        )
        assert agent.get("configuration") == expected_configuration, agent
        if expected_state != "ready":
            assert agent.get("version") is None, agent

    assert by_kind["codex"].get("version") == EXPECTED_VERSION, by_kind["codex"]
    if expected_states["claude"] == "ready":
        assert by_kind["claude"].get("version") == CLAUDE_VERSION, by_kind["claude"]
    if expected_states["agy"] == "ready":
        assert by_kind["agy"].get("version") == AGY_VERSION, by_kind["agy"]

    assert by_kind["claude"].get("dangerouslySkipPermissions") is True, by_kind[
        "claude"
    ]
    for kind, agent in by_kind.items():
        assert isinstance(agent.get("dangerouslySkipPermissions"), bool), agent
        install = agent.get("install")
        assert isinstance(install, dict), agent
        for key in (
            "command",
            "shell",
            "verifyCommand",
            "updateCommand",
            "docsUrl",
        ):
            assert isinstance(install.get(key), str) and install[key], (kind, install)
        assert install["docsUrl"].startswith("https://"), install
        assert install.get("requiresServerAccess") is True, install
        assert "path" not in agent, agent
        assert "error" not in agent, agent

    return {
        "server": server,
        "states": {kind: by_kind[kind]["state"] for kind in by_kind},
        "versions": {kind: by_kind[kind]["version"] for kind in by_kind},
    }


def picker_metrics(page: Page) -> dict[str, Any]:
    return page.evaluate(
        """() => {
          const picker = document.querySelector(".agent-picker");
          const options = document.querySelector(".agent-options");
          const rect = picker?.getBoundingClientRect();
          return {
            innerWidth: window.innerWidth,
            innerHeight: window.innerHeight,
            pageScrollX: window.scrollX,
            pageScrollY: window.scrollY,
            documentScrollWidth: document.documentElement.scrollWidth,
            documentClientWidth: document.documentElement.clientWidth,
            pickerScrollWidth: picker?.scrollWidth ?? -1,
            pickerClientWidth: picker?.clientWidth ?? -1,
            optionsScrollWidth: options?.scrollWidth ?? -1,
            optionsClientWidth: options?.clientWidth ?? -1,
            pickerRect: rect ? {
              left: rect.left,
              top: rect.top,
              right: rect.right,
              bottom: rect.bottom,
            } : null,
            activeTag: document.activeElement?.tagName ?? null,
            activeClass: document.activeElement?.className ?? null,
          };
        }"""
    )


def assert_picker_layout(metrics: dict[str, Any]) -> None:
    rect = metrics["pickerRect"]
    assert rect is not None, metrics
    assert metrics["pageScrollX"] == 0, metrics
    assert metrics["pageScrollY"] == 0, metrics
    assert metrics["documentScrollWidth"] <= metrics["documentClientWidth"] + 1, metrics
    assert metrics["pickerScrollWidth"] <= metrics["pickerClientWidth"] + 1, metrics
    assert metrics["optionsScrollWidth"] <= metrics["optionsClientWidth"] + 1, metrics
    assert rect["left"] >= -1, metrics
    assert rect["top"] >= -1, metrics
    assert rect["right"] <= metrics["innerWidth"] + 1, metrics
    assert rect["bottom"] <= metrics["innerHeight"] + 1, metrics
    assert metrics["activeTag"] in {"BUTTON", "A"}, metrics


def assert_start_actions_reachable(page: Page, picker: Any) -> None:
    starts = picker.locator("[data-agent-start]")
    containment = starts.evaluate_all(
        """buttons => buttons.map(button => {
          const card = button.closest(".agent-option");
          const buttonRect = button.getBoundingClientRect();
          const cardRect = card?.getBoundingClientRect();
          return {
            label: button.textContent,
            contained: Boolean(
              cardRect &&
              buttonRect.top >= cardRect.top - 1 &&
              buttonRect.bottom <= cardRect.bottom + 1 &&
              buttonRect.left >= cardRect.left - 1 &&
              buttonRect.right <= cardRect.right + 1
            ),
          };
        })"""
    )
    assert all(item["contained"] for item in containment), containment

    options_box = picker.locator(".agent-options").bounding_box()
    assert options_box is not None
    for index in range(starts.count()):
        button = starts.nth(index)
        button.scroll_into_view_if_needed()
        box = button.bounding_box()
        assert box is not None, containment
        assert box["y"] >= options_box["y"] - 1, (containment, box, options_box)
        assert box["y"] + box["height"] <= (
            options_box["y"] + options_box["height"] + 1
        ), (
            containment,
            box,
            options_box,
        )
        assert button.evaluate(
            """element => {
              const rect = element.getBoundingClientRect();
              const hit = document.elementFromPoint(
                rect.left + rect.width / 2,
                rect.top + rect.height / 2,
              );
              return hit === element || element.contains(hit);
            }"""
        ), (containment, box)

    picker.locator(".agent-options").evaluate("options => { options.scrollTop = 0; }")

    viewport = page.viewport_size
    if viewport and viewport["width"] <= 520 and starts.count() >= 2:
        options = picker.locator(".agent-options")
        box = options.bounding_box()
        assert box is not None
        cdp = page.context.new_cdp_session(page)
        x = box["x"] + box["width"] / 2
        start_y = box["y"] + box["height"] - 18
        end_y = box["y"] + 18
        cdp.send(
            "Input.dispatchTouchEvent",
            {
                "type": "touchStart",
                "touchPoints": [{"x": x, "y": start_y}],
            },
        )
        for step in range(1, 7):
            y = start_y + (end_y - start_y) * step / 6
            cdp.send(
                "Input.dispatchTouchEvent",
                {
                    "type": "touchMove",
                    "touchPoints": [{"x": x, "y": y}],
                },
            )
        cdp.send(
            "Input.dispatchTouchEvent",
            {"type": "touchEnd", "touchPoints": []},
        )
        page.wait_for_timeout(350)
        assert options.evaluate("element => element.scrollTop") > 0
        last_box = starts.last.bounding_box()
        assert last_box is not None
        assert last_box["y"] >= -1
        assert last_box["y"] + last_box["height"] <= viewport["height"] + 1
        assert page.evaluate("window.scrollY") == 0
        options.evaluate("element => { element.scrollTop = 0; }")


def exercise_picker(
    page: Page,
    port: int,
    token: str,
    *,
    ready_count: int,
    unavailable_count: int,
) -> dict[str, Any]:
    refresh_requests: list[str] = []
    page.on(
        "request",
        lambda request: (
            refresh_requests.append(request.url)
            if "/api/agent-catalog?refresh=true" in request.url
            else None
        ),
    )
    page.goto(
        f"http://127.0.0.1:{port}/?token={token}",
        wait_until="domcontentloaded",
    )
    page.locator(".status--connected").wait_for(state="visible")
    with page.expect_request(
        lambda request: "/api/agent-catalog?refresh=true" in request.url
    ):
        page.locator(".session-new-button").click()
    picker = page.locator(".agent-picker")
    picker.wait_for(state="visible")
    page.wait_for_function(
        "() => document.querySelectorAll('.agent-option').length === 3"
    )
    automatic_refresh = picker.get_by_role(
        "button", name="Check installed agents again"
    )
    refresh_deadline = time.monotonic() + 10
    while automatic_refresh.is_disabled():
        if time.monotonic() >= refresh_deadline:
            raise AssertionError("automatic agent refresh did not finish")
        page.wait_for_timeout(50)

    cards = picker.locator(".agent-option")
    ready_cards = picker.locator(".agent-option--ready")
    unavailable_cards = picker.locator(
        ".agent-option--missing, .agent-option--misconfigured"
    )
    starts = picker.locator("[data-agent-start]")
    assert cards.count() == 3
    assert ready_cards.count() == ready_count
    assert unavailable_cards.count() == unavailable_count
    ready_text = "\n".join(ready_cards.all_inner_texts())
    assert "Installed version" in ready_text
    assert EXPECTED_VERSION in ready_text
    if ready_count >= 2:
        assert CLAUDE_VERSION in ready_text
    if ready_count == 3:
        assert AGY_VERSION in ready_text
    assert starts.count() == ready_count
    start_labels = starts.all_inner_texts()
    assert "Start Codex" in start_labels
    if ready_count >= 2:
        assert "Start Claude" in start_labels
    if ready_count == 3:
        assert "Start AGY" in start_labels
    assert_start_actions_reachable(page, picker)
    assert picker.locator("input, textarea").count() == 0
    assert picker.locator(".agent-install-command code").count() == unavailable_count
    assert picker.locator(".agent-docs-link").count() == unavailable_count
    assert (
        picker.get_by_role("button", name="Check again").count()
        == unavailable_count
    )
    assert "server host" in picker.inner_text()
    assert "not on this browser or phone" in picker.inner_text()
    assert "Approvals disabled" in picker.inner_text()

    before_refresh = picker_metrics(page)
    assert_picker_layout(before_refresh)

    refresh = picker.get_by_role("button", name="Check installed agents again")
    with page.expect_request(
        lambda request: "/api/agent-catalog?refresh=true" in request.url
    ):
        refresh.click()
    page.wait_for_function(
        """() => {
          const button = document.querySelector(
            'button[aria-label="Check installed agents again"]',
          );
          return button && !button.disabled && button.textContent?.includes("Refresh");
        }"""
    )
    assert len(refresh_requests) == 2, refresh_requests
    assert picker.locator("input, textarea").count() == 0

    after_refresh = picker_metrics(page)
    assert_picker_layout(after_refresh)
    return {
        "cards": cards.count(),
        "readyCards": ready_cards.count(),
        "unavailableCards": unavailable_cards.count(),
        "startLabels": start_labels,
        "automaticRefreshRequests": 1,
        "totalRefreshRequests": len(refresh_requests),
        "beforeRefresh": before_refresh,
        "afterRefresh": after_refresh,
    }


def run_browser_checks(
    browser: Browser,
    port: int,
    token: str,
    *,
    ready_count: int,
    unavailable_count: int,
) -> dict[str, Any]:
    desktop_context = browser.new_context(viewport={"width": 1280, "height": 720})
    mobile_context = browser.new_context(
        viewport={"width": 360, "height": 639},
        screen={"width": 360, "height": 780},
        device_scale_factor=3,
        is_mobile=True,
        has_touch=True,
        user_agent=PHONE_USER_AGENT,
    )
    try:
        desktop = exercise_picker(
            desktop_context.new_page(),
            port,
            token,
            ready_count=ready_count,
            unavailable_count=unavailable_count,
        )
        mobile = exercise_picker(
            mobile_context.new_page(),
            port,
            token,
            ready_count=ready_count,
            unavailable_count=unavailable_count,
        )
        return {"desktop": desktop, "mobile": mobile}
    finally:
        desktop_context.close()
        mobile_context.close()


def exercise_unavailable_to_ready_focus(
    browser: Browser,
    port: int,
    token: str,
    claude_command: Path,
) -> dict[str, Any]:
    context = browser.new_context(viewport={"width": 1280, "height": 720})
    try:
        page = context.new_page()
        page.goto(
            f"http://127.0.0.1:{port}/?token={token}",
            wait_until="domcontentloaded",
        )
        page.locator(".status--connected").wait_for(state="visible")
        with page.expect_request(
            lambda request: "/api/agent-catalog?refresh=true" in request.url
        ):
            page.locator(".session-new-button").click()
        picker = page.locator(".agent-picker")
        claude_card = picker.locator(".agent-option--claude")
        check_again = claude_card.get_by_role("button", name="Check again")
        check_again.wait_for(state="visible")
        check_again.focus()

        write_ready_command(
            claude_command,
            f"{CLAUDE_VERSION} (Claude Code)",
            "Claude",
        )
        with page.expect_request(
            lambda request: "/api/agent-catalog?refresh=true" in request.url
        ):
            check_again.click()

        start = picker.locator('[data-agent-start="claude"]')
        start.wait_for(state="visible")
        page.wait_for_function(
            """() =>
              document.activeElement?.getAttribute("data-agent-start") === "claude"
            """
        )
        return {
            "state": "ready",
            "focusedAction": start.inner_text(),
            "activeAgent": page.evaluate(
                'document.activeElement?.getAttribute("data-agent-start")'
            ),
        }
    finally:
        context.close()


def stop_owned_server(process: subprocess.Popen[bytes]) -> None:
    if process.poll() is not None:
        return

    if os.name == "nt":
        subprocess.run(
            ["taskkill", "/PID", str(process.pid), "/T", "/F"],
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            check=False,
            creationflags=subprocess.CREATE_NO_WINDOW,
        )
        return

    try:
        os.killpg(process.pid, signal.SIGTERM)
        process.wait(timeout=5)
    except (ProcessLookupError, subprocess.TimeoutExpired):
        try:
            os.killpg(process.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass


def main() -> int:
    args = parse_args()
    args.server = args.server.resolve()
    if not args.server.is_file():
        raise FileNotFoundError(
            f"server build not found: {args.server}; build it before running this test"
        )
    if args.chrome is not None:
        args.chrome = args.chrome.resolve()
        if not args.chrome.is_file():
            raise FileNotFoundError(args.chrome)
    assert_isolated_port(args.port)

    token = secrets.token_urlsafe(32)
    with tempfile.TemporaryDirectory(prefix="codex-web-agent-catalog-") as temp:
        temporary_root = Path(temp)
        project = temporary_root / "project"
        project.mkdir()
        ready, auto_bin, claude_command, missing_agy = write_fake_commands(
            temporary_root
        )
        command = [
            str(args.server),
            "--host",
            "127.0.0.1",
            "--port",
            str(args.port),
            "--project",
            str(project),
            "--command",
            str(ready),
            "--new-session-command",
            str(ready),
            "--claude-dangerously-skip-permissions",
            "--agy-command",
            str(missing_agy),
            "--token",
            token,
            "--no-open-browser",
            "--log-level",
            "warn",
        ]
        if os.name == "nt":
            command.extend(["--shell", "cmd"])
        else:
            # Unix discovery intentionally checks system-wide well-known
            # directories in addition to PATH. An explicit disposable override
            # keeps this regression independent of CLIs installed on the host.
            command.extend(["--claude-command", str(claude_command)])

        creation_flags = (
            subprocess.CREATE_NO_WINDOW if os.name == "nt" else 0
        )
        server = subprocess.Popen(
            command,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            env=isolated_server_environment(temporary_root, auto_bin),
            creationflags=creation_flags,
            start_new_session=os.name != "nt",
        )
        try:
            initial_catalog = wait_for_server(args.port, token, server)
            initial_states = {
                "codex": "ready",
                "claude": "missing" if os.name == "nt" else "misconfigured",
                "agy": "misconfigured",
            }
            initial_api_result = validate_catalog(initial_catalog, initial_states)

            launch_options: dict[str, Any] = {
                "headless": True,
                "args": ["--disable-background-networking"],
            }
            if args.chrome is not None:
                launch_options["executable_path"] = str(args.chrome)
            with sync_playwright() as playwright:
                browser = playwright.chromium.launch(**launch_options)
                try:
                    initial_browser_result = run_browser_checks(
                        browser,
                        args.port,
                        token,
                        ready_count=1,
                        unavailable_count=2,
                    )

                    transition_focus_result = exercise_unavailable_to_ready_focus(
                        browser,
                        args.port,
                        token,
                        claude_command,
                    )
                    refreshed_catalog = request_json(
                        args.port,
                        token,
                        "/api/agent-catalog?refresh=true",
                    )
                    refreshed_states = {
                        "codex": "ready",
                        "claude": "ready",
                        "agy": "misconfigured",
                    }
                    refreshed_api_result = validate_catalog(
                        refreshed_catalog,
                        refreshed_states,
                    )
                    assert refreshed_api_result["states"] != initial_api_result["states"]

                    legacy_agents = request_json(args.port, token, "/api/agents")
                    assert legacy_agents == ["codex", "claude"], legacy_agents
                    created = request_json(
                        args.port,
                        token,
                        "/api/sessions",
                        method="POST",
                        payload={"agent": "claude"},
                    )
                    assert created["agent"] == "claude", created
                    assert created["status"] == "running", created
                    request_json(
                        args.port,
                        token,
                        f"/api/sessions/{created['terminalId']}",
                        method="DELETE",
                    )

                    refreshed_browser_result = run_browser_checks(
                        browser,
                        args.port,
                        token,
                        ready_count=2,
                        unavailable_count=1,
                    )

                    write_ready_command(
                        missing_agy,
                        AGY_VERSION,
                        "AGY",
                    )
                    all_ready_catalog = request_json(
                        args.port,
                        token,
                        "/api/agent-catalog?refresh=true",
                    )
                    all_ready_states = {
                        "codex": "ready",
                        "claude": "ready",
                        "agy": "ready",
                    }
                    all_ready_api_result = validate_catalog(
                        all_ready_catalog,
                        all_ready_states,
                    )
                    all_ready_agents = request_json(
                        args.port,
                        token,
                        "/api/agents",
                    )
                    assert all_ready_agents == ["codex", "claude", "agy"], (
                        all_ready_agents
                    )
                    created_agy = request_json(
                        args.port,
                        token,
                        "/api/sessions",
                        method="POST",
                        payload={"agent": "agy"},
                    )
                    assert created_agy["agent"] == "agy", created_agy
                    assert created_agy["status"] == "running", created_agy
                    request_json(
                        args.port,
                        token,
                        f"/api/sessions/{created_agy['terminalId']}",
                        method="DELETE",
                    )
                    all_ready_browser_result = run_browser_checks(
                        browser,
                        args.port,
                        token,
                        ready_count=3,
                        unavailable_count=0,
                    )
                finally:
                    browser.close()

            print(
                json.dumps(
                    {
                        "port": args.port,
                        "protectedLivePorts": sorted(PROTECTED_LIVE_PORTS),
                        "api": {
                            "initial": initial_api_result,
                            "afterInstallAndRefresh": refreshed_api_result,
                            "allReady": all_ready_api_result,
                            "legacyReadyAgents": legacy_agents,
                            "allReadyAgents": all_ready_agents,
                            "startedAgent": created["agent"],
                            "startedAllReadyAgent": created_agy["agent"],
                        },
                        "browser": {
                            "initial": initial_browser_result,
                            "transitionFocus": transition_focus_result,
                            "afterInstallAndRefresh": refreshed_browser_result,
                            "allReady": all_ready_browser_result,
                        },
                    },
                    indent=2,
                )
            )
            return 0
        finally:
            stop_owned_server(server)


if __name__ == "__main__":
    raise SystemExit(main())
