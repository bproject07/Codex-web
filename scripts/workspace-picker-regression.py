#!/usr/bin/env python3
"""Exercise the workspace APIs and folder launcher on a disposable server.

The script owns every process and file it creates. It refuses the live
development ports 8788, 8789, and 8790, uses a synthetic long-running command,
and never installs, updates, or invokes a real agent CLI.
"""

from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import re
import secrets
import signal
import socket
import subprocess
import tempfile
import time
from typing import Any
from urllib.error import HTTPError, URLError
from urllib.request import Request, urlopen

from playwright.sync_api import Browser, Locator, Page, sync_playwright


DEFAULT_PORT = 8803
PROTECTED_LIVE_PORTS = frozenset({8788, 8789, 8790})
PHONE_USER_AGENT = (
    "Mozilla/5.0 (Linux; Android 10; K) AppleWebKit/537.36 "
    "(KHTML, like Gecko) Chrome/150.0.0.0 Mobile Safari/537.36"
)
DIRECTORY_ID_PATTERN = re.compile(r"^[wus]1\.[A-Za-z0-9_-]+$")


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
            "Validate authenticated directory browsing, persisted workspace "
            "shortcuts, selected PTY working directories, and responsive dialogs."
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


def wait_for_server(
    port: int,
    token: str,
    process: subprocess.Popen[bytes],
) -> None:
    deadline = time.monotonic() + 25
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
            details = "\n".join(
                part.strip() for part in (stdout, stderr) if part.strip()
            )
            suffix = f":\n{details[-2000:]}" if details else ""
            raise RuntimeError(
                f"test server exited with code {process.returncode}{suffix}"
            )
        try:
            status, body = http_request(
                port,
                "/api/health",
                token=token,
            )
            if status == 200 and isinstance(body, dict):
                return
        except (OSError, URLError):
            pass
        time.sleep(0.1)
    raise TimeoutError("test server did not become ready")


def write_fixture_command(root: Path) -> Path:
    if os.name == "nt":
        command = root / "workspace-fixture.cmd"
        command.write_bytes(
            b"@echo off\r\n"
            b'if /I "%~1"=="--version" (\r\n'
            b"  echo workspace-fixture 1.0.0\r\n"
            b"  exit /b 0\r\n"
            b")\r\n"
            b"cd\r\n"
            b":fixture_loop\r\n"
            b"ping 127.0.0.1 -n 61 >nul\r\n"
            b"goto fixture_loop\r\n"
        )
        return command

    command = root / "workspace-fixture"
    command.write_text(
        "#!/bin/sh\n"
        'if [ "${1:-}" = "--version" ]; then\n'
        "  printf '%s\\n' 'workspace-fixture 1.0.0'\n"
        "  exit 0\n"
        "fi\n"
        "pwd\n"
        "while :; do sleep 60; done\n",
        encoding="utf-8",
    )
    command.chmod(0o700)
    return command


def isolated_server_environment(root: Path) -> dict[str, str]:
    environment = os.environ.copy()
    environment.pop("CODEX_INSTALL_DIR", None)
    environment.pop("CODEX_WEB_STATE_DIR", None)
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


def assert_directory(value: Any, expected_path: Path | None = None) -> None:
    assert isinstance(value, dict), value
    assert set(value) == {"id", "name", "path"}, value
    assert isinstance(value["id"], str) and DIRECTORY_ID_PATTERN.fullmatch(
        value["id"]
    ), value
    assert isinstance(value["name"], str) and value["name"], value
    assert isinstance(value["path"], str) and value["path"], value
    assert value["path"].casefold() not in value["id"].casefold(), value
    if expected_path is not None:
        assert paths_equal(value["path"], expected_path), value


def resolve_directory(
    port: int,
    token: str,
    path: Path,
) -> dict[str, Any]:
    listing = request_json(
        port,
        token,
        "/api/filesystem/resolve",
        method="POST",
        payload={"path": str(path)},
    )
    assert isinstance(listing, dict), listing
    assert set(listing) == {
        "current",
        "parentId",
        "breadcrumbs",
        "directories",
        "truncated",
    }, listing
    assert_directory(listing["current"], path)
    assert listing["parentId"] is None or DIRECTORY_ID_PATTERN.fullmatch(
        listing["parentId"]
    ), listing
    assert isinstance(listing["breadcrumbs"], list), listing
    assert isinstance(listing["directories"], list), listing
    assert isinstance(listing["truncated"], bool), listing
    for directory in listing["breadcrumbs"] + listing["directories"]:
        assert_directory(directory)
    return listing


def exercise_workspace_api(
    port: int,
    token: str,
    *,
    project: Path,
    selected: Path,
    favorite_project: Path,
    ignored_file: Path,
    state_dir: Path,
) -> dict[str, Any]:
    protected_requests = [
        ("GET", "/api/filesystem/roots", None),
        ("POST", "/api/filesystem/list", {}),
        ("POST", "/api/filesystem/resolve", {"path": str(selected)}),
        ("GET", "/api/workspaces", None),
    ]
    for method, path, payload in protected_requests:
        status, _ = http_request(
            port,
            path,
            token=None,
            method=method,
            payload=payload,
        )
        assert status == 401, (method, path, status)

    roots = request_json(port, token, "/api/filesystem/roots")
    assert isinstance(roots, dict), roots
    assert set(roots) == {"defaultDirectory", "roots"}, roots
    assert_directory(roots["defaultDirectory"], project)
    assert isinstance(roots["roots"], list) and roots["roots"], roots
    for root in roots["roots"]:
        assert_directory(root)

    selected_listing = resolve_directory(port, token, selected)
    selected_directory = selected_listing["current"]
    listed_names = {entry["name"] for entry in selected_listing["directories"]}
    assert ignored_file.name not in listed_names, selected_listing
    assert {"folder-00", "folder-17"}.issubset(listed_names), selected_listing
    assert all(
        Path(entry["path"]).is_dir() for entry in selected_listing["directories"]
    ), selected_listing

    id_listing = request_json(
        port,
        token,
        "/api/filesystem/list",
        method="POST",
        payload={"directoryId": selected_directory["id"]},
    )
    assert id_listing["current"] == selected_directory, id_listing

    status, _ = http_request(
        port,
        "/api/filesystem/list",
        token=token,
        method="POST",
        payload={"directoryId": str(selected)},
    )
    assert status == 400, status
    status, _ = http_request(
        port,
        "/api/filesystem/resolve",
        token=token,
        method="POST",
        payload={"path": str(ignored_file)},
    )
    assert status == 400, status

    empty_library = request_json(port, token, "/api/workspaces")
    assert empty_library["version"] == 1, empty_library
    assert empty_library["favorites"] == [], empty_library
    assert len(empty_library["recent"]) == 1, empty_library
    assert paths_equal(empty_library["recent"][0]["path"], project), empty_library
    assert empty_library["recent"][0]["lastAgent"] == "codex", empty_library

    favorite = request_json(
        port,
        token,
        "/api/workspaces/favorites",
        method="PUT",
        payload={
            "directoryId": selected_directory["id"],
            "label": "Selected fixture",
            "preferredAgent": "codex",
        },
    )
    assert favorite["directoryId"] == selected_directory["id"], favorite
    assert favorite["label"] == "Selected fixture", favorite
    assert favorite["preferredAgent"] == "codex", favorite
    assert paths_equal(favorite["path"], selected), favorite

    library = request_json(port, token, "/api/workspaces")
    assert library["version"] == 1, library
    assert library["favorites"] == [favorite], library
    state_file = state_dir / "workspaces.json"
    deadline = time.monotonic() + 5
    while not state_file.is_file() and time.monotonic() < deadline:
        time.sleep(0.05)
    persisted = json.loads(state_file.read_text(encoding="utf-8"))
    assert persisted == library, (persisted, library)
    assert token not in state_file.read_text(encoding="utf-8")

    request_json(
        port,
        token,
        f"/api/workspaces/favorites/{favorite['id']}",
        method="DELETE",
        expected_status=204,
    )
    after_delete = request_json(port, token, "/api/workspaces")
    assert after_delete["favorites"] == [], after_delete

    favorite_listing = resolve_directory(port, token, favorite_project)
    pinned = request_json(
        port,
        token,
        "/api/workspaces/favorites",
        method="PUT",
        payload={
            "directoryId": favorite_listing["current"]["id"],
            "label": "Pinned fixture",
            "preferredAgent": "codex",
        },
    )

    created = request_json(
        port,
        token,
        "/api/sessions",
        method="POST",
        payload={
            "agent": "codex",
            "directoryId": selected_directory["id"],
        },
        expected_status=201,
    )
    assert created["agent"] == "codex", created
    assert created["directoryId"] == selected_directory["id"], created
    assert paths_equal(created["project"], selected), created

    status, _ = http_request(
        port,
        "/api/sessions",
        token=token,
        method="POST",
        payload={"agent": "codex", "directoryId": str(selected)},
    )
    assert status == 400, status

    request_json(
        port,
        token,
        f"/api/sessions/{created['terminalId']}",
        method="DELETE",
        expected_status=204,
    )
    sessions = request_json(port, token, "/api/sessions")
    assert len(sessions) == 1 and sessions[0]["isPrimary"], sessions

    library = request_json(port, token, "/api/workspaces")
    selected_recent = next(
        recent
        for recent in library["recent"]
        if recent["directoryId"] == selected_directory["id"]
    )
    assert selected_recent["lastAgent"] == "codex", selected_recent
    assert isinstance(selected_recent["lastOpenedAt"], int), selected_recent
    assert selected_recent["lastOpenedAt"] > 0, selected_recent

    return {
        "defaultDirectoryIdPrefix": roots["defaultDirectory"]["id"][:3],
        "selectedDirectoryIdPrefix": selected_directory["id"][:3],
        "listedDirectoryCount": len(selected_listing["directories"]),
        "ignoredFiles": 1,
        "favoriteLifecycle": "added-persisted-deleted",
        "pinnedFavorite": pinned["label"],
        "selectedSessionCwd": "snapshot-verified",
    }


def wait_for_application(page: Page, port: int, token: str) -> None:
    page.goto(
        f"http://127.0.0.1:{port}/?token={token}",
        wait_until="domcontentloaded",
    )
    page.locator(".status--connected").wait_for(state="visible", timeout=20_000)
    page.locator(".session-new-button").wait_for(state="visible")


def assert_focus_inside(page: Page, selector: str) -> str:
    page.wait_for_function(
        """selector => {
          const container = document.querySelector(selector);
          return Boolean(
            container && document.activeElement &&
            container.contains(document.activeElement)
          );
        }""",
        arg=selector,
    )
    result = page.evaluate(
        """selector => {
          const container = document.querySelector(selector);
          const active = document.activeElement;
          return {
            inside: Boolean(container && active && container.contains(active)),
            tag: active?.tagName ?? null,
            text: active?.textContent?.trim().slice(0, 80) ?? "",
          };
        }""",
        selector,
    )
    assert result["inside"], result
    return f"{result['tag']}:{result['text']}"


def assert_workspace_focus_trap(page: Page) -> None:
    result = page.locator(".workspace-picker").evaluate(
        """dialog => {
          const focusable = [...dialog.querySelectorAll(
            'button:not([disabled]), input:not([disabled]), select:not([disabled]), ' +
            'a[href], [tabindex]:not([tabindex="-1"])'
          )].filter(element => element.tabIndex >= 0);
          focusable.at(-1)?.focus({preventScroll: true});
          return {
            count: focusable.length,
            lastText: document.activeElement?.textContent?.trim() ?? "",
          };
        }"""
    )
    assert result["count"] >= 2, result
    page.keyboard.press("Tab")
    wrapped = page.locator(".workspace-picker").evaluate(
        """dialog => {
          const focusable = [...dialog.querySelectorAll(
            'button:not([disabled]), input:not([disabled]), select:not([disabled]), ' +
            'a[href], [tabindex]:not([tabindex="-1"])'
          )].filter(element => element.tabIndex >= 0);
          return document.activeElement === focusable[0];
        }"""
    )
    assert wrapped


def assert_workspace_tab_keyboard(page: Page) -> None:
    picker = page.locator(".workspace-picker")
    favorites = picker.get_by_role("tab", name="Favorites")
    recent = picker.get_by_role("tab", name="Recent")
    browse = picker.get_by_role("tab", name="Browse")

    favorites.focus()
    page.keyboard.press("End")
    assert browse.get_attribute("aria-selected") == "true"
    assert browse.evaluate("element => document.activeElement === element")

    page.keyboard.press("Home")
    assert favorites.get_attribute("aria-selected") == "true"
    assert favorites.evaluate("element => document.activeElement === element")

    page.keyboard.press("ArrowLeft")
    assert browse.get_attribute("aria-selected") == "true"
    assert browse.evaluate("element => document.activeElement === element")

    page.keyboard.press("ArrowLeft")
    assert recent.get_attribute("aria-selected") == "true"
    assert recent.evaluate("element => document.activeElement === element")

    page.keyboard.press("ArrowRight")
    assert browse.get_attribute("aria-selected") == "true"
    page.keyboard.press("Home")
    assert favorites.get_attribute("aria-selected") == "true"


def assert_browse_navigation_focus(page: Page) -> str:
    page.wait_for_function(
        """() => {
          const picker = document.querySelector(".workspace-picker");
          const target = document.querySelector(
            '[data-workspace-current-focus-target="true"]'
          );
          return Boolean(
            picker && target && document.activeElement === target &&
            picker.contains(document.activeElement)
          );
        }"""
    )
    result = page.evaluate(
        """() => ({
          tag: document.activeElement?.tagName ?? null,
          inside: Boolean(
            document.querySelector(".workspace-picker")?.contains(
              document.activeElement
            )
          ),
          isInput: document.activeElement instanceof HTMLInputElement,
        })"""
    )
    assert result["inside"], result
    assert not result["isInput"], result

    page.keyboard.press("Tab")
    assert_focus_inside(page, ".workspace-picker")
    page.keyboard.press("Shift+Tab")
    assert_focus_inside(page, ".workspace-picker")
    return str(result["tag"])


def open_workspace_picker(page: Page) -> Locator:
    page.locator(".session-new-button").click()
    picker = page.locator(".workspace-picker")
    picker.wait_for(state="visible")
    page.wait_for_function(
        "() => !document.querySelector('.workspace-picker')?.getAttribute('aria-busy')"
        " || document.querySelector('.workspace-picker')?.getAttribute('aria-busy')"
        " === 'false'"
    )
    return picker


def browse_to_path(page: Page, path: Path) -> Locator:
    picker = page.locator(".workspace-picker")
    picker.get_by_role("tab", name="Browse").click()
    path_input = picker.get_by_label("Folder path")
    path_input.fill(str(path))
    with page.expect_response(
        lambda response: response.url.endswith("/api/filesystem/resolve")
    ) as response_info:
        picker.get_by_role("button", name="Open", exact=True).click()
    assert response_info.value.status == 200
    use_folder = picker.get_by_role("button", name="Use folder", exact=True)
    use_folder.wait_for(state="visible")
    page.wait_for_function(
        """expected => {
          const button = document.querySelector(".workspace-picker__choose-current");
          const breadcrumbs = document.querySelector(
            ".workspace-picker__breadcrumbs"
          )?.textContent ?? "";
          return button && !button.disabled && breadcrumbs.includes(expected);
        }""",
        arg=path.name,
    )
    assert_browse_navigation_focus(page)
    return use_folder


def wait_for_selected_project(page: Page, expected: Path) -> None:
    deadline = time.monotonic() + 20
    while time.monotonic() < deadline:
        project = page.locator(".project-path").inner_text()
        if paths_equal(project.removeprefix("Project:").strip(), expected):
            page.locator(".status--connected").wait_for(state="visible", timeout=20_000)
            return
        page.wait_for_timeout(50)
    raise AssertionError(
        f"browser did not attach to expected project {expected}: "
        f"{page.locator('.project-path').inner_text()!r}"
    )


def wait_for_terminal_cwd(page: Page, expected: Path) -> None:
    page.wait_for_function(
        """expected => {
          const terminalText =
            document.querySelector(".xterm-rows")?.textContent ?? "";
          return terminalText.includes(expected);
        }""",
        arg=str(expected),
        timeout=20_000,
    )


def shortcut_row(page: Page, path: Path) -> Locator:
    return page.locator(".workspace-picker__shortcut").filter(
        has=page.locator(
            ".workspace-picker__shortcut-main",
            has_text=str(path),
        )
    )


def exercise_library_retry_flow(
    browser: Browser,
    port: int,
    token: str,
    *,
    selected: Path,
) -> dict[str, Any]:
    context = browser.new_context(viewport={"width": 1000, "height": 700})
    try:
        page = context.new_page()
        failed_requests = 0

        def intercept_library(route: Any) -> None:
            nonlocal failed_requests
            if route.request.method == "GET" and failed_requests == 0:
                failed_requests += 1
                route.fulfill(
                    status=503,
                    content_type="application/json",
                    body=json.dumps(
                        {"error": "Synthetic workspace library failure"}
                    ),
                )
                return
            route.continue_()

        page.route("**/api/workspaces", intercept_library)
        wait_for_application(page, port, token)
        picker = open_workspace_picker(page)
        library_error = picker.locator(".workspace-picker__error--library")
        library_error.wait_for(state="visible")

        browse_to_path(page, selected)
        favorite = picker.locator(
            ".workspace-picker__current-actions button[aria-pressed]"
        )
        assert favorite.is_disabled()

        with page.expect_response(
            lambda response: response.url.endswith("/api/workspaces")
        ) as retry_response:
            library_error.get_by_role(
                "button", name="Retry", exact=True
            ).click()
        assert retry_response.value.status == 200
        library_error.wait_for(state="hidden")
        favorite.wait_for(state="visible")
        assert favorite.is_enabled()
        assert_focus_inside(page, ".workspace-picker")

        picker.get_by_role("button", name="Close folder picker").click()
        picker.wait_for(state="hidden")
        return {
            "failedLoads": failed_requests,
            "mutationBlockedAfterFailure": True,
            "retryEnabledAfterSuccess": True,
        }
    finally:
        context.close()


def exercise_desktop_flow(
    browser: Browser,
    port: int,
    token: str,
    *,
    selected: Path,
    favorite_project: Path,
) -> dict[str, Any]:
    context = browser.new_context(viewport={"width": 1280, "height": 720})
    try:
        page = context.new_page()
        wait_for_application(page, port, token)
        sessions_before = request_json(port, token, "/api/sessions")

        open_workspace_picker(page)
        initial_focus = assert_focus_inside(page, ".workspace-picker")
        assert_workspace_focus_trap(page)
        assert_workspace_tab_keyboard(page)

        use_folder = browse_to_path(page, selected)
        first_directory = page.locator(
            ".workspace-picker__directories button"
        ).first
        with page.expect_response(
            lambda response: response.url.endswith("/api/filesystem/list")
        ) as child_response:
            first_directory.click()
        assert child_response.value.status == 200
        child_navigation_focus = assert_browse_navigation_focus(page)

        with page.expect_response(
            lambda response: response.url.endswith("/api/filesystem/list")
        ) as parent_response:
            page.locator(".workspace-picker").get_by_role(
                "button", name="Open parent folder"
            ).click()
        assert parent_response.value.status == 200
        parent_navigation_focus = assert_browse_navigation_focus(page)
        use_folder.click()
        agent_picker = page.locator(".agent-picker")
        agent_picker.wait_for(state="visible")
        first_handoff_focus = assert_focus_inside(page, ".agent-picker")
        assert paths_equal(
            agent_picker.locator(".agent-picker-workspace code").inner_text(),
            selected,
        )

        agent_picker.get_by_role("button", name="Change folder").click()
        page.locator(".workspace-picker").wait_for(state="visible")
        return_handoff_focus = assert_focus_inside(page, ".workspace-picker")
        page.locator(".workspace-picker").get_by_role(
            "tab", name="Browse"
        ).click()
        use_folder = page.locator(
            ".workspace-picker__choose-current",
        )
        use_folder.wait_for(state="visible")
        page.wait_for_function(
            "() => !document.querySelector('.workspace-picker__choose-current')"
            "?.hasAttribute('disabled')"
        )
        use_folder.click()

        start = page.locator('[data-agent-start="codex"]')
        start.wait_for(state="visible")
        assert_focus_inside(page, ".agent-picker")
        start.click()
        agent_picker.wait_for(state="hidden")
        wait_for_selected_project(page, selected)
        wait_for_terminal_cwd(page, selected)
        sessions_after_manual = request_json(port, token, "/api/sessions")
        assert len(sessions_after_manual) == len(sessions_before) + 1, (
            sessions_before,
            sessions_after_manual,
        )

        open_workspace_picker(page)
        favorite_row = shortcut_row(page, favorite_project)
        favorite_row.wait_for(state="visible")
        with page.expect_response(
            lambda response: response.url.endswith("/api/filesystem/list")
        ) as shortcut_browse_response:
            favorite_row.get_by_role(
                "button", name=f"Browse inside {favorite_project}"
            ).click()
        assert shortcut_browse_response.value.status == 200
        shortcut_navigation_focus = assert_browse_navigation_focus(page)
        page.locator(".workspace-picker").get_by_role(
            "tab", name="Favorites"
        ).click()
        favorite_row = shortcut_row(page, favorite_project)
        favorite_row.get_by_role(
            "button", name="Start Codex", exact=True
        ).click()
        page.locator(".workspace-picker").wait_for(state="hidden")
        wait_for_selected_project(page, favorite_project)
        wait_for_terminal_cwd(page, favorite_project)

        open_workspace_picker(page)
        page.locator(".workspace-picker").get_by_role(
            "tab", name="Recent"
        ).click()
        recent_row = shortcut_row(page, selected)
        recent_row.wait_for(state="visible")
        recent_row.get_by_role(
            "button", name="Start Codex", exact=True
        ).click()
        page.locator(".workspace-picker").wait_for(state="hidden")
        wait_for_selected_project(page, selected)
        wait_for_terminal_cwd(page, selected)

        sessions_final = request_json(port, token, "/api/sessions")
        assert len(sessions_final) == len(sessions_before) + 3, sessions_final
        assert sum(
            paths_equal(session["project"], selected) for session in sessions_final
        ) == 2, sessions_final
        assert sum(
            paths_equal(session["project"], favorite_project)
            for session in sessions_final
        ) == 1, sessions_final

        return {
            "initialWorkspaceFocus": initial_focus,
            "workspaceToAgentFocus": first_handoff_focus,
            "agentToWorkspaceFocus": return_handoff_focus,
            "childNavigationFocus": child_navigation_focus,
            "parentNavigationFocus": parent_navigation_focus,
            "shortcutNavigationFocus": shortcut_navigation_focus,
            "workspaceTabKeyboard": "verified",
            "manualSelectedProject": "verified",
            "favoriteDirectStart": "verified",
            "recentDirectStart": "verified",
            "sessionCount": len(sessions_final),
        }
    finally:
        context.close()


def picker_metrics(page: Page) -> dict[str, Any]:
    return page.evaluate(
        """() => {
          const picker = document.querySelector(".workspace-picker");
          const backdrop = document.querySelector(".dialog-backdrop");
          const panel = document.querySelector(".workspace-picker__panel");
          const directories = document.querySelector(
            ".workspace-picker__directories"
          );
          const rect = element =>
            element ? element.getBoundingClientRect().toJSON() : null;
          return {
            innerWidth: window.innerWidth,
            innerHeight: window.innerHeight,
            pageScrollX: window.scrollX,
            pageScrollY: window.scrollY,
            documentClientWidth: document.documentElement.clientWidth,
            documentScrollWidth: document.documentElement.scrollWidth,
            bodyScrollWidth: document.body.scrollWidth,
            pickerRect: rect(picker),
            backdropRect: rect(backdrop),
            pickerClientWidth: picker?.clientWidth ?? 0,
            pickerScrollWidth: picker?.scrollWidth ?? 0,
            pickerClientHeight: picker?.clientHeight ?? 0,
            pickerScrollHeight: picker?.scrollHeight ?? 0,
            panelClientWidth: panel?.clientWidth ?? 0,
            panelScrollWidth: panel?.scrollWidth ?? 0,
            directoryClientHeight: directories?.clientHeight ?? 0,
            directoryScrollHeight: directories?.scrollHeight ?? 0,
          };
        }"""
    )


def assert_picker_layout(metrics: dict[str, Any]) -> None:
    rect = metrics["pickerRect"]
    assert rect is not None, metrics
    assert metrics["pageScrollX"] == 0, metrics
    assert metrics["pageScrollY"] == 0, metrics
    assert metrics["documentScrollWidth"] <= metrics["documentClientWidth"] + 1, (
        metrics
    )
    assert metrics["bodyScrollWidth"] <= metrics["innerWidth"] + 1, metrics
    assert metrics["pickerScrollWidth"] <= metrics["pickerClientWidth"] + 1, metrics
    assert metrics["panelScrollWidth"] <= metrics["panelClientWidth"] + 1, metrics
    assert rect["left"] >= -1, metrics
    assert rect["top"] >= -1, metrics
    assert rect["right"] <= metrics["innerWidth"] + 1, metrics
    assert rect["bottom"] <= metrics["innerHeight"] + 1, metrics


def assert_control_reachable(page: Page, control: Locator) -> dict[str, float]:
    control.scroll_into_view_if_needed()
    page.wait_for_timeout(50)
    box = control.bounding_box()
    assert box is not None, control
    viewport = page.viewport_size
    assert viewport is not None
    assert box["x"] >= -1, box
    assert box["y"] >= -1, box
    assert box["x"] + box["width"] <= viewport["width"] + 1, box
    assert box["y"] + box["height"] <= viewport["height"] + 1, box
    assert box["width"] >= 28 and box["height"] >= 28, box
    assert control.evaluate(
        """element => {
          const rect = element.getBoundingClientRect();
          const hit = document.elementFromPoint(
            rect.left + rect.width / 2,
            rect.top + rect.height / 2
          );
          return hit === element || element.contains(hit);
        }"""
    ), box
    assert page.evaluate("window.scrollX === 0 && window.scrollY === 0")
    return box


def exercise_mobile_layout(
    browser: Browser,
    port: int,
    token: str,
    *,
    selected: Path,
) -> dict[str, Any]:
    context = browser.new_context(
        viewport={"width": 360, "height": 639},
        screen={"width": 360, "height": 780},
        device_scale_factor=3,
        is_mobile=True,
        has_touch=True,
        user_agent=PHONE_USER_AGENT,
    )
    try:
        page = context.new_page()
        wait_for_application(page, port, token)
        open_workspace_picker(page)
        initial_focus = assert_focus_inside(page, ".workspace-picker")
        browse_to_path(page, selected)
        picker = page.locator(".workspace-picker")

        regular_metrics = picker_metrics(page)
        assert_picker_layout(regular_metrics)
        regular_controls = {
            "browseTab": assert_control_reachable(
                page, picker.get_by_role("tab", name="Browse")
            ),
            "pathInput": assert_control_reachable(
                page, picker.get_by_label("Folder path")
            ),
            "useFolder": assert_control_reachable(
                page, picker.get_by_role("button", name="Use folder", exact=True)
            ),
            "firstDirectory": assert_control_reachable(
                page, picker.locator(".workspace-picker__directories button").first
            ),
            "lastDirectory": assert_control_reachable(
                page, picker.locator(".workspace-picker__directories button").last
            ),
        }
        regular_metrics_after_scroll = picker_metrics(page)
        assert_picker_layout(regular_metrics_after_scroll)

        page.set_viewport_size({"width": 360, "height": 345})
        page.wait_for_timeout(250)
        short_metrics = picker_metrics(page)
        assert_picker_layout(short_metrics)
        short_controls = {
            "browseTab": assert_control_reachable(
                page, picker.get_by_role("tab", name="Browse")
            ),
            "pathInput": assert_control_reachable(
                page, picker.get_by_label("Folder path")
            ),
            "open": assert_control_reachable(
                page, picker.get_by_role("button", name="Open", exact=True)
            ),
            "useFolder": assert_control_reachable(
                page, picker.get_by_role("button", name="Use folder", exact=True)
            ),
            "firstDirectory": assert_control_reachable(
                page, picker.locator(".workspace-picker__directories button").first
            ),
            "lastDirectory": assert_control_reachable(
                page, picker.locator(".workspace-picker__directories button").last
            ),
        }
        short_metrics_after_scroll = picker_metrics(page)
        assert_picker_layout(short_metrics_after_scroll)

        picker.get_by_role("button", name="Close folder picker").click()
        picker.wait_for(state="hidden")
        page.wait_for_function(
            "() => document.activeElement?.classList.contains("
            "'session-new-button')"
        )

        return {
            "regularViewport": regular_metrics,
            "regularControls": sorted(regular_controls),
            "shortViewport": short_metrics,
            "shortControls": sorted(short_controls),
            "initialWorkspaceFocus": initial_focus,
            "focusReturnedToNew": True,
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
        try:
            process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            pass
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
    with tempfile.TemporaryDirectory(prefix="codex-web-workspaces-") as temp:
        temporary_root = Path(temp)
        project = temporary_root / "default-project"
        selected = temporary_root / "projects" / "selected project"
        favorite_project = temporary_root / "projects" / "favorite project"
        state_dir = temporary_root / "state"
        project.mkdir()
        selected.mkdir(parents=True)
        favorite_project.mkdir(parents=True)
        for index in range(18):
            (selected / f"folder-{index:02d}").mkdir()
        ignored_file = selected / "ignored-file.txt"
        ignored_file.write_text("files must not appear in folder listings\n")
        (favorite_project / "child").mkdir()

        fixture = write_fixture_command(temporary_root)
        missing_claude = temporary_root / (
            "missing-claude.cmd" if os.name == "nt" else "missing-claude"
        )
        missing_agy = temporary_root / (
            "missing-agy.cmd" if os.name == "nt" else "missing-agy"
        )
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
            str(missing_claude),
            "--agy-command",
            str(missing_agy),
            "--no-agent-auto-detect",
            "--token",
            token,
            "--no-open-browser",
            "--log-level",
            "warn",
        ]
        creation_flags = subprocess.CREATE_NO_WINDOW if os.name == "nt" else 0
        server = subprocess.Popen(
            command,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            env=isolated_server_environment(temporary_root),
            creationflags=creation_flags,
            start_new_session=os.name != "nt",
        )
        try:
            wait_for_server(args.port, token, server)
            api_result = exercise_workspace_api(
                args.port,
                token,
                project=project,
                selected=selected,
                favorite_project=favorite_project,
                ignored_file=ignored_file,
                state_dir=state_dir,
            )

            launch_options: dict[str, Any] = {
                "headless": True,
                "args": ["--disable-background-networking"],
            }
            if args.chrome is not None:
                launch_options["executable_path"] = str(args.chrome)
            with sync_playwright() as playwright:
                browser = playwright.chromium.launch(**launch_options)
                try:
                    retry_result = exercise_library_retry_flow(
                        browser,
                        args.port,
                        token,
                        selected=selected,
                    )
                    desktop_result = exercise_desktop_flow(
                        browser,
                        args.port,
                        token,
                        selected=selected,
                        favorite_project=favorite_project,
                    )
                    mobile_result = exercise_mobile_layout(
                        browser,
                        args.port,
                        token,
                        selected=selected,
                    )
                finally:
                    browser.close()

            final_library = request_json(args.port, token, "/api/workspaces")
            assert len(final_library["favorites"]) == 1, final_library
            assert any(
                paths_equal(recent["path"], selected)
                for recent in final_library["recent"]
            ), final_library
            assert any(
                paths_equal(recent["path"], favorite_project)
                for recent in final_library["recent"]
            ), final_library

            print(
                json.dumps(
                    {
                        "port": args.port,
                        "protectedLivePorts": sorted(PROTECTED_LIVE_PORTS),
                        "platform": os.name,
                        "api": api_result,
                        "browser": {
                            "libraryRetry": retry_result,
                            "desktop": desktop_result,
                            "mobile": mobile_result,
                        },
                        "persistedFavorites": len(final_library["favorites"]),
                        "persistedRecent": len(final_library["recent"]),
                    },
                    indent=2,
                )
            )
            return 0
        finally:
            stop_owned_server(server)


if __name__ == "__main__":
    raise SystemExit(main())
