#!/usr/bin/env python3
"""Reproduce Android IME duplicate terminal input without touching the PTY.

The script loads an already-running Codex Web frontend, fulfills its API calls
with synthetic data, and replaces /ws with an in-browser Playwright route. The
route records browser-to-WebSocket frames but never opens a server-side
WebSocket, so the generated input cannot reach a managed terminal.
"""

from __future__ import annotations

import argparse
import json
from pathlib import Path
from typing import Any, Callable
from urllib.parse import parse_qsl, urlencode, urlsplit, urlunsplit

from playwright.sync_api import BrowserContext, Page, Route, WebSocketRoute, sync_playwright


DEFAULT_URL = "http://127.0.0.1:8790/"
DEFAULT_TOKEN = "android-ime-input-regression-token-2026"
DEFAULT_CHROME = Path(r"C:\Program Files\Google\Chrome\Application\chrome.exe")
RESERVED_LIVE_PORT = 8789
TERMINAL_ID = "android-ime-regression"
SESSION_ID = "00000000-0000-0000-0000-000000000001"
TEXT_PAYLOAD = b"test"
GO_PAYLOAD = b"go"
LETTER_PAYLOAD = b"l"
ENTER_PAYLOAD = b"\r"
ARROW_LEFT_PAYLOAD = b"\x1b[D"
FOCUS_OUT_PAYLOAD = b"\x1b[O"
ANDROID_USER_AGENT = (
    "Mozilla/5.0 (Linux; Android 15; SM-S928B) AppleWebKit/537.36 "
    "(KHTML, like Gecko) Chrome/150.0.0.0 Mobile Safari/537.36"
)

SYNTHETIC_SESSION = {
    "terminalId": TERMINAL_ID,
    "name": "Android IME regression",
    "agent": "codex",
    "isPrimary": True,
    "createdAt": 1,
    "sessionId": SESSION_ID,
    "status": "running",
    "connected": True,
    "connectedClients": 1,
    "startedAt": 1,
    "pid": 1,
    "exitCode": None,
    "project": "synthetic-browser-fixture",
    "lastError": None,
}

INSTALL_EVENT_DRIVER = r"""() => {
  const textarea = document.querySelector(".xterm-helper-textarea");
  if (!(textarea instanceof HTMLTextAreaElement)) {
    throw new Error("xterm input textarea was not found");
  }

  window.__androidImeEvents = [];
  for (const type of [
    "keydown",
    "keypress",
    "keyup",
    "compositionstart",
    "compositionupdate",
    "compositionend",
    "beforeinput",
    "input",
  ]) {
    textarea.addEventListener(type, (event) => {
      window.__androidImeEvents.push({
        type: event.type,
        key: event.key ?? null,
        keyCode: event.keyCode ?? null,
        charCode: event.charCode ?? null,
        data: event.data ?? null,
        inputType: event.inputType ?? null,
        isComposing: event.isComposing ?? null,
        composed: event.composed,
        defaultPrevented: event.defaultPrevented,
        value: textarea.value,
      });
    });
  }

  const keyboard = (
    type,
    key,
    code,
    keyCode,
    charCode,
    isComposing,
  ) => {
    const event = new KeyboardEvent(type, {
      bubbles: true,
      cancelable: true,
      composed: true,
      key,
      code,
      isComposing,
    });
    Object.defineProperties(event, {
      keyCode: { get: () => keyCode },
      which: { get: () => keyCode },
      charCode: { get: () => charCode },
    });
    textarea.dispatchEvent(event);
  };

  const composition = (type, data) => {
    textarea.dispatchEvent(new CompositionEvent(type, {
      bubbles: true,
      cancelable: true,
      composed: true,
      data,
    }));
  };

  const input = (
    type,
    inputType,
    data,
    isComposing,
    composed,
    nextValue,
  ) => {
    if (type === "input" && nextValue !== undefined) {
      textarea.value = nextValue;
    }
    textarea.dispatchEvent(new InputEvent(type, {
      bubbles: true,
      cancelable: type === "beforeinput",
      composed,
      data,
      inputType,
      isComposing,
    }));
  };

  textarea.value = "";
  textarea.focus({ preventScroll: true });
  window.__androidIme = { textarea, keyboard, composition, input };
}"""


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description=(
            "Assert Android IME and Enter transaction payloads while keeping "
            "all synthetic input away from the PTY."
        )
    )
    parser.add_argument(
        "--url",
        default=DEFAULT_URL,
        help=f"URL serving the frontend (default: {DEFAULT_URL})",
    )
    parser.add_argument(
        "--token",
        default=DEFAULT_TOKEN,
        help="Dummy browser token; API and WebSocket requests are intercepted",
    )
    parser.add_argument(
        "--chrome",
        type=Path,
        help=(
            "Chromium executable. Defaults to system Chrome on Windows when "
            "available, otherwise Playwright Chromium."
        ),
    )
    parser.add_argument(
        "--headed",
        action="store_true",
        help="Show the browser window while running the regression.",
    )
    return parser.parse_args()


def authenticated_url(raw_url: str, token: str) -> str:
    parsed = urlsplit(raw_url)
    if parsed.scheme not in {"http", "https"} or not parsed.hostname:
        raise ValueError("--url must be an absolute http:// or https:// URL")
    if parsed.port == RESERVED_LIVE_PORT:
        raise ValueError(
            f"refusing to run against reserved live port {RESERVED_LIVE_PORT}"
        )

    query = dict(parse_qsl(parsed.query, keep_blank_values=True))
    query["token"] = token
    return urlunsplit(
        (parsed.scheme, parsed.netloc, parsed.path or "/", urlencode(query), parsed.fragment)
    )


def fulfill_api(route: Route) -> None:
    path = urlsplit(route.request.url).path
    if path == "/api/sessions":
        payload: Any = [SYNTHETIC_SESSION]
        status = 200
    elif path == "/api/session":
        payload = SYNTHETIC_SESSION
        status = 200
    elif path == "/api/agents":
        payload = ["codex"]
        status = 200
    else:
        payload = {"error": "synthetic endpoint unavailable"}
        status = 404

    route.fulfill(
        status=status,
        content_type="application/json",
        body=json.dumps(payload),
    )


def binary_frames(messages: list[str | bytes]) -> list[bytes]:
    return [bytes(message) for message in messages if isinstance(message, bytes)]


def prepare_page(
    context: BrowserContext,
    url: str,
    enable_focus_reporting: bool = False,
) -> tuple[Page, list[str | bytes], list[str]]:
    page = context.new_page()
    outbound: list[str | bytes] = []
    websocket_urls: list[str] = []

    page.route("**/api/**", fulfill_api)

    def mock_websocket(websocket: WebSocketRoute) -> None:
        websocket_urls.append(websocket.url)
        websocket.on_message(lambda message: outbound.append(message))
        # Intentionally do not call connect_to_server(): no frame can reach a PTY.
        if enable_focus_reporting:
            websocket.send(b"\x1b[?1004h")

    page.route_web_socket(
        lambda candidate: urlsplit(candidate).path == "/ws",
        mock_websocket,
    )
    page.goto(url, wait_until="domcontentloaded")
    page.locator(".xterm-helper-textarea").wait_for(state="attached")
    page.locator(".status--connected").wait_for(state="visible")
    page.evaluate(INSTALL_EVENT_DRIVER)
    page.wait_for_timeout(150)
    outbound.clear()
    return page, outbound, websocket_urls


def keydown_229_insert_text(page: Page) -> None:
    page.evaluate(
        r"""() => {
          const ime = window.__androidIme;
          ime.keyboard(
            "keydown", "Unidentified", "", 229, 0, true,
          );
          ime.input(
            "beforeinput", "insertText", "test", false, false,
          );
          ime.input(
            "input", "insertText", "test", false, false, "test",
          );
          ime.keyboard(
            "keyup", "Unidentified", "", 229, 0, false,
          );
        }"""
    )


def composition_end_insert_text(page: Page) -> None:
    page.evaluate(
        r"""() => {
          const ime = window.__androidIme;
          ime.composition("compositionstart", "");
          ime.composition("compositionupdate", "test");
          ime.textarea.value = "test";
        }"""
    )
    page.wait_for_timeout(20)
    page.evaluate(
        r"""() => {
          const ime = window.__androidIme;
          ime.composition("compositionend", "test");
          ime.input(
            "beforeinput", "insertText", "test", false, true,
          );
          ime.input(
            "input", "insertText", "test", false, true, "test",
          );
        }"""
    )


def keydown_keypress_enter(page: Page) -> None:
    page.evaluate(
        r"""() => {
          const ime = window.__androidIme;
          ime.keyboard("keydown", "Enter", "Enter", 13, 0, false);
          ime.keyboard("keypress", "Enter", "Enter", 13, 13, false);
          ime.keyboard("keyup", "Enter", "Enter", 13, 0, false);
        }"""
    )


def keydown_229_mutation_keyup_composed_insert_text(page: Page) -> None:
    page.evaluate(
        r"""() => {
          const ime = window.__androidIme;
          ime.keyboard(
            "keydown", "Unidentified", "", 229, 0, true,
          );
          ime.textarea.value = "test";
          ime.keyboard(
            "keyup", "Unidentified", "", 229, 0, false,
          );
          ime.input(
            "beforeinput", "insertText", "test", false, true,
          );
          ime.input(
            "input", "insertText", "test", false, true, "test",
          );
        }"""
    )


def expired_keydown_229_then_input_only(page: Page) -> None:
    page.evaluate(
        r"""() => {
          const ime = window.__androidIme;
          ime.keyboard(
            "keydown", "Unidentified", "", 229, 0, true,
          );
          ime.keyboard(
            "keyup", "Unidentified", "", 229, 0, false,
          );
        }"""
    )
    page.wait_for_timeout(20)
    page.evaluate(
        r"""() => {
          const ime = window.__androidIme;
          ime.input(
            "beforeinput", "insertText", "test", false, true,
          );
          ime.input(
            "input", "insertText", "test", false, true, "test",
          );
        }"""
    )


def composition_enter_trailing_commit(page: Page) -> None:
    page.evaluate(
        r"""() => {
          const ime = window.__androidIme;
          ime.composition("compositionstart", "");
          ime.textarea.value = "go";
          ime.composition("compositionupdate", "go");
          ime.keyboard("keydown", "Enter", "Enter", 13, 0, true);
          ime.composition("compositionend", "go");
          ime.input(
            "beforeinput", "insertText", "go", false, true,
          );
          ime.input(
            "input", "insertText", "go", false, true, "go",
          );
          ime.keyboard("keypress", "Enter", "Enter", 13, 13, false);
          ime.keyboard("keyup", "Enter", "Enter", 13, 0, false);
        }"""
    )


def two_identical_input_only_edits(page: Page) -> None:
    page.evaluate(
        r"""() => {
          const ime = window.__androidIme;
          ime.input(
            "beforeinput", "insertText", "l", false, false,
          );
          ime.input(
            "input", "insertText", "l", false, false, "l",
          );
        }"""
    )
    page.wait_for_timeout(20)
    page.evaluate(
        r"""() => {
          const ime = window.__androidIme;
          ime.input(
            "beforeinput", "insertText", "l", false, false,
          );
          ime.input(
            "input", "insertText", "l", false, false, "ll",
          );
        }"""
    )


def two_complete_enter_cycles(page: Page) -> None:
    keydown_keypress_enter(page)
    page.wait_for_timeout(20)
    keydown_keypress_enter(page)


def keydown_229_soft_enter(page: Page) -> None:
    page.evaluate(
        r"""() => {
          const ime = window.__androidIme;
          ime.keyboard("keydown", "Enter", "Enter", 229, 0, false);
          ime.input(
            "beforeinput", "insertLineBreak", null, false, true,
          );
          ime.input(
            "input", "insertLineBreak", null, false, true, "",
          );
          ime.keyboard("keyup", "Enter", "Enter", 229, 0, false);
        }"""
    )


def keydown_229_soft_paragraph(page: Page) -> None:
    page.evaluate(
        r"""() => {
          const ime = window.__androidIme;
          ime.keyboard("keydown", "Enter", "Enter", 229, 0, false);
          ime.input(
            "beforeinput", "insertParagraph", null, false, true,
          );
          ime.input(
            "input", "insertParagraph", null, false, true, "",
          );
          ime.keyboard("keyup", "Enter", "Enter", 229, 0, false);
        }"""
    )


def standard_enter_with_trailing_linebreak(page: Page) -> None:
    page.evaluate(
        r"""() => {
          const ime = window.__androidIme;
          ime.keyboard("keydown", "Enter", "Enter", 13, 0, false);
          ime.keyboard("keypress", "Enter", "Enter", 13, 13, false);
          ime.input(
            "beforeinput", "insertLineBreak", null, false, true,
          );
          ime.input(
            "input", "insertLineBreak", null, false, true, "",
          );
          ime.keyboard("keyup", "Enter", "Enter", 13, 0, false);
        }"""
    )


def soft_enter_keypress_then_linebreak(page: Page) -> None:
    page.evaluate(
        r"""() => {
          const ime = window.__androidIme;
          ime.keyboard("keydown", "Enter", "Enter", 229, 0, false);
          ime.keyboard("keypress", "Enter", "Enter", 13, 13, false);
          ime.input(
            "beforeinput", "insertLineBreak", null, false, true,
          );
          ime.input(
            "input", "insertLineBreak", null, false, true, "",
          );
          ime.keyboard("keyup", "Enter", "Enter", 229, 0, false);
        }"""
    )


def soft_enter_linebreak_then_keypress(page: Page) -> None:
    page.evaluate(
        r"""() => {
          const ime = window.__androidIme;
          ime.keyboard("keydown", "Enter", "Enter", 229, 0, false);
          ime.input(
            "beforeinput", "insertLineBreak", null, false, true,
          );
          ime.input(
            "input", "insertLineBreak", null, false, true, "",
          );
          ime.keyboard("keypress", "Enter", "Enter", 13, 13, false);
          ime.keyboard("keyup", "Enter", "Enter", 229, 0, false);
        }"""
    )


def standard_enter_linebreak_then_keypress(page: Page) -> None:
    page.evaluate(
        r"""() => {
          const ime = window.__androidIme;
          ime.keyboard("keydown", "Enter", "Enter", 13, 0, false);
          ime.input(
            "beforeinput", "insertLineBreak", null, false, true,
          );
          ime.input(
            "input", "insertLineBreak", null, false, true, "",
          );
          ime.keyboard("keypress", "Enter", "Enter", 13, 13, false);
          ime.keyboard("keyup", "Enter", "Enter", 13, 0, false);
        }"""
    )


def standard_enter_delayed_linebreak(page: Page) -> None:
    page.evaluate(
        r"""() => {
          const ime = window.__androidIme;
          ime.keyboard("keydown", "Enter", "Enter", 13, 0, false);
          ime.keyboard("keypress", "Enter", "Enter", 13, 13, false);
        }"""
    )
    page.wait_for_timeout(20)
    page.evaluate(
        r"""() => {
          const ime = window.__androidIme;
          ime.input(
            "beforeinput", "insertLineBreak", null, false, true,
          );
          ime.input(
            "input", "insertLineBreak", null, false, true, "",
          );
          ime.keyboard("keyup", "Enter", "Enter", 13, 0, false);
        }"""
    )


def composition_commit_linebreak_keypress(page: Page) -> None:
    page.evaluate(
        r"""() => {
          const ime = window.__androidIme;
          ime.composition("compositionstart", "");
          ime.textarea.value = "go";
          ime.composition("compositionupdate", "go");
          ime.keyboard("keydown", "Enter", "Enter", 13, 0, true);
          ime.composition("compositionend", "go");
          ime.input(
            "beforeinput", "insertText", "go", false, true,
          );
          ime.input(
            "input", "insertText", "go", false, true, "go",
          );
          ime.input(
            "beforeinput", "insertLineBreak", null, false, true,
          );
          ime.input(
            "input", "insertLineBreak", null, false, true, "go",
          );
          ime.keyboard("keypress", "Enter", "Enter", 13, 13, false);
          ime.keyboard("keyup", "Enter", "Enter", 13, 0, false);
        }"""
    )


def composition_end_linebreak_trailing_commit(page: Page) -> None:
    page.evaluate(
        r"""() => {
          const ime = window.__androidIme;
          ime.composition("compositionstart", "");
          ime.textarea.value = "go";
          ime.composition("compositionupdate", "go");
          ime.keyboard("keydown", "Enter", "Enter", 13, 0, true);
          ime.composition("compositionend", "go");
          ime.input(
            "beforeinput", "insertLineBreak", null, false, true,
          );
          ime.input(
            "input", "insertLineBreak", null, false, true, "go",
          );
          ime.input(
            "beforeinput", "insertText", "go", false, true,
          );
          ime.input(
            "input", "insertText", "go", false, true, "go",
          );
          ime.keyboard("keypress", "Enter", "Enter", 13, 13, false);
          ime.keyboard("keyup", "Enter", "Enter", 13, 0, false);
        }"""
    )


def composition_linebreak_before_end(page: Page) -> None:
    page.evaluate(
        r"""() => {
          const ime = window.__androidIme;
          ime.composition("compositionstart", "");
          ime.textarea.value = "go";
          ime.composition("compositionupdate", "go");
          ime.keyboard("keydown", "Enter", "Enter", 13, 0, true);
          ime.input(
            "beforeinput", "insertLineBreak", null, false, true,
          );
          ime.input(
            "input", "insertLineBreak", null, false, true, "go",
          );
          ime.composition("compositionend", "go");
          ime.input(
            "beforeinput", "insertText", "go", false, true,
          );
          ime.input(
            "input", "insertText", "go", false, true, "go",
          );
          ime.keyboard("keypress", "Enter", "Enter", 13, 13, false);
          ime.keyboard("keyup", "Enter", "Enter", 13, 0, false);
        }"""
    )


def composition_enter_without_native_end(page: Page) -> None:
    page.evaluate(
        r"""() => {
          const ime = window.__androidIme;
          ime.composition("compositionstart", "");
          ime.textarea.value = "go";
          ime.composition("compositionupdate", "go");
        }"""
    )
    page.wait_for_timeout(20)
    page.evaluate(
        r"""() => {
          const ime = window.__androidIme;
          ime.keyboard("keydown", "Enter", "Enter", 13, 0, true);
          ime.input(
            "beforeinput", "insertLineBreak", null, false, true,
          );
          ime.input(
            "input", "insertLineBreak", null, false, true, "go",
          );
          ime.keyboard("keypress", "Enter", "Enter", 13, 13, false);
          ime.keyboard("keyup", "Enter", "Enter", 13, 0, false);
        }"""
    )
    page.wait_for_timeout(1_100)


def composition_enter_without_native_end_then_blur(page: Page) -> None:
    page.evaluate(
        r"""() => {
          const ime = window.__androidIme;
          ime.composition("compositionstart", "");
          ime.textarea.value = "go";
          ime.composition("compositionupdate", "go");
        }"""
    )
    page.wait_for_timeout(20)
    page.evaluate(
        r"""() => {
          const ime = window.__androidIme;
          ime.keyboard("keydown", "Enter", "Enter", 13, 0, true);
          ime.input(
            "beforeinput", "insertLineBreak", null, false, true,
          );
          ime.input(
            "input", "insertLineBreak", null, false, true, "go",
          );
          ime.keyboard("keypress", "Enter", "Enter", 13, 13, false);
          ime.keyboard("keyup", "Enter", "Enter", 13, 0, false);
          ime.textarea.blur();
        }"""
    )
    page.wait_for_timeout(1_100)


def composition_enter_without_native_end_blur_refocus(page: Page) -> None:
    page.evaluate(
        r"""() => {
          const ime = window.__androidIme;
          ime.composition("compositionstart", "");
          ime.textarea.value = "go";
          ime.composition("compositionupdate", "go");
        }"""
    )
    page.wait_for_timeout(20)
    page.evaluate(
        r"""() => {
          const ime = window.__androidIme;
          ime.keyboard("keydown", "Enter", "Enter", 13, 0, true);
          ime.input(
            "beforeinput", "insertLineBreak", null, false, true,
          );
          ime.input(
            "input", "insertLineBreak", null, false, true, "go",
          );
          ime.keyboard("keypress", "Enter", "Enter", 13, 13, false);
          ime.keyboard("keyup", "Enter", "Enter", 13, 0, false);
          ime.textarea.blur();
          ime.textarea.focus({ preventScroll: true });
        }"""
    )
    page.wait_for_timeout(1_100)


def composition_enter_without_native_end_then_key(page: Page) -> None:
    page.evaluate(
        r"""() => {
          const ime = window.__androidIme;
          ime.composition("compositionstart", "");
          ime.textarea.value = "go";
          ime.composition("compositionupdate", "go");
        }"""
    )
    page.wait_for_timeout(20)
    page.evaluate(
        r"""() => {
          const ime = window.__androidIme;
          ime.keyboard("keydown", "Enter", "Enter", 13, 0, true);
          ime.input(
            "beforeinput", "insertLineBreak", null, false, true,
          );
          ime.input(
            "input", "insertLineBreak", null, false, true, "go",
          );
          ime.keyboard("keypress", "Enter", "Enter", 13, 13, false);
          ime.keyboard("keyup", "Enter", "Enter", 13, 0, false);
          ime.keyboard("keydown", "ArrowLeft", "ArrowLeft", 37, 0, false);
          ime.keyboard("keyup", "ArrowLeft", "ArrowLeft", 37, 0, false);
        }"""
    )


def composition_enter_immediate_without_end_then_key(page: Page) -> None:
    page.evaluate(
        r"""() => {
          const ime = window.__androidIme;
          ime.composition("compositionstart", "");
          ime.textarea.value = "go";
          ime.composition("compositionupdate", "go");
          ime.keyboard("keydown", "Enter", "Enter", 13, 0, true);
          ime.input(
            "beforeinput", "insertLineBreak", null, false, true,
          );
          ime.input(
            "input", "insertLineBreak", null, false, true, "go",
          );
          ime.keyboard("keypress", "Enter", "Enter", 13, 13, false);
          ime.keyboard("keyup", "Enter", "Enter", 13, 0, false);
          ime.keyboard("keydown", "ArrowLeft", "ArrowLeft", 37, 0, false);
          ime.keyboard("keyup", "ArrowLeft", "ArrowLeft", 37, 0, false);
        }"""
    )


def composition_enter_immediate_then_printable_key(page: Page) -> None:
    page.evaluate(
        r"""() => {
          const ime = window.__androidIme;
          ime.composition("compositionstart", "");
          ime.textarea.value = "go";
          ime.composition("compositionupdate", "go");
          ime.keyboard("keydown", "Enter", "Enter", 13, 0, true);
          ime.input(
            "beforeinput", "insertLineBreak", null, false, true,
          );
          ime.input(
            "input", "insertLineBreak", null, false, true, "go",
          );
          ime.keyboard("keypress", "Enter", "Enter", 13, 13, false);
          ime.keyboard("keyup", "Enter", "Enter", 13, 0, false);
          ime.keyboard("keydown", "x", "KeyX", 88, 0, false);
          ime.keyboard("keyup", "x", "KeyX", 88, 0, false);
        }"""
    )


def composition_end_then_immediate_key(page: Page) -> None:
    page.evaluate(
        r"""() => {
          const ime = window.__androidIme;
          ime.composition("compositionstart", "");
          ime.textarea.value = "go";
          ime.composition("compositionupdate", "go");
          ime.keyboard("keydown", "Enter", "Enter", 13, 0, true);
          ime.composition("compositionend", "go");
          ime.input(
            "beforeinput", "insertText", "go", false, true,
          );
          ime.input(
            "input", "insertText", "go", false, true, "go",
          );
          ime.keyboard("keypress", "Enter", "Enter", 13, 13, false);
          ime.keyboard("keyup", "Enter", "Enter", 13, 0, false);
          ime.keyboard("keydown", "ArrowLeft", "ArrowLeft", 37, 0, false);
          ime.keyboard("keyup", "ArrowLeft", "ArrowLeft", 37, 0, false);
        }"""
    )


def composition_enter_without_native_end_then_enter(page: Page) -> None:
    page.evaluate(
        r"""() => {
          const ime = window.__androidIme;
          ime.composition("compositionstart", "");
          ime.textarea.value = "go";
          ime.composition("compositionupdate", "go");
        }"""
    )
    page.wait_for_timeout(20)
    page.evaluate(
        r"""() => {
          const ime = window.__androidIme;
          ime.keyboard("keydown", "Enter", "Enter", 13, 0, true);
          ime.input(
            "beforeinput", "insertLineBreak", null, false, true,
          );
          ime.input(
            "input", "insertLineBreak", null, false, true, "go",
          );
          ime.keyboard("keypress", "Enter", "Enter", 13, 13, false);
          ime.keyboard("keyup", "Enter", "Enter", 13, 0, false);
          ime.keyboard("keydown", "Enter", "Enter", 13, 0, false);
          ime.keyboard("keypress", "Enter", "Enter", 13, 13, false);
          ime.keyboard("keyup", "Enter", "Enter", 13, 0, false);
        }"""
    )


def composition_enter_without_end_then_new_composition(page: Page) -> None:
    page.evaluate(
        r"""() => {
          const ime = window.__androidIme;
          ime.composition("compositionstart", "");
          ime.textarea.value = "go";
          ime.composition("compositionupdate", "go");
          ime.keyboard("keydown", "Enter", "Enter", 13, 0, true);
          ime.input(
            "beforeinput", "insertLineBreak", null, false, true,
          );
          ime.input(
            "input", "insertLineBreak", null, false, true, "go",
          );
          ime.keyboard("keypress", "Enter", "Enter", 13, 13, false);
          ime.keyboard("keyup", "Enter", "Enter", 13, 0, false);
          ime.composition("compositionstart", "");
          ime.textarea.value = "gon";
          ime.composition("compositionupdate", "n");
        }"""
    )
    page.wait_for_timeout(50)
    page.evaluate(
        r"""() => {
          const ime = window.__androidIme;
          ime.composition("compositionend", "n");
          ime.input(
            "beforeinput", "insertText", "n", false, true,
          );
          ime.input(
            "input", "insertText", "n", false, true, "gon",
          );
        }"""
    )


def composition_enter_new_composition_then_blur(page: Page) -> None:
    page.evaluate(
        r"""() => {
          const ime = window.__androidIme;
          ime.composition("compositionstart", "");
          ime.textarea.value = "go";
          ime.composition("compositionupdate", "go");
          ime.keyboard("keydown", "Enter", "Enter", 13, 0, true);
          ime.input(
            "beforeinput", "insertLineBreak", null, false, true,
          );
          ime.input(
            "input", "insertLineBreak", null, false, true, "go",
          );
          ime.keyboard("keypress", "Enter", "Enter", 13, 13, false);
          ime.keyboard("keyup", "Enter", "Enter", 13, 0, false);
          ime.composition("compositionstart", "");
          ime.textarea.value = "gon";
          ime.composition("compositionupdate", "n");
          ime.textarea.blur();
        }"""
    )


def composition_submit_then_new_start(page: Page) -> None:
    page.evaluate(
        r"""() => {
          const ime = window.__androidIme;
          ime.composition("compositionstart", "");
          ime.textarea.value = "go";
          ime.composition("compositionupdate", "go");
          ime.keyboard("keydown", "Enter", "Enter", 13, 0, true);
          ime.composition("compositionend", "go");
          ime.input(
            "beforeinput", "insertText", "go", false, true,
          );
          ime.input(
            "input", "insertText", "go", false, true, "go",
          );
          ime.composition("compositionstart", "");
          ime.textarea.value = "gon";
          ime.composition("compositionupdate", "n");
        }"""
    )


def composition_submit_then_blur(page: Page) -> None:
    page.evaluate(
        r"""() => {
          const ime = window.__androidIme;
          ime.composition("compositionstart", "");
          ime.textarea.value = "go";
          ime.composition("compositionupdate", "go");
          ime.keyboard("keydown", "Enter", "Enter", 13, 0, true);
          ime.composition("compositionend", "go");
          ime.input(
            "beforeinput", "insertText", "go", false, true,
          );
          ime.input(
            "input", "insertText", "go", false, true, "go",
          );
          ime.textarea.blur();
        }"""
    )


def composition_submit_then_late_blur(page: Page) -> None:
    composition_enter_trailing_commit(page)
    page.wait_for_timeout(50)
    page.evaluate("window.__androidIme.textarea.blur()")
    page.wait_for_timeout(20)
    textarea_value = page.evaluate("window.__androidIme.textarea.value")
    if textarea_value != "":
        raise AssertionError(
            "completed composition was restored into the textarea after blur"
        )


def run_case(
    context: BrowserContext,
    url: str,
    name: str,
    expected: list[bytes],
    action: Callable[[Page], None],
    enable_focus_reporting: bool = False,
) -> dict[str, Any]:
    page, outbound, websocket_urls = prepare_page(
        context,
        url,
        enable_focus_reporting,
    )
    try:
        action(page)
        page.wait_for_timeout(100)
        frames = binary_frames(outbound)
        return {
            "name": name,
            "expectedHex": [frame.hex() for frame in expected],
            "actualHex": [frame.hex() for frame in frames],
            "actualUtf8": [
                frame.decode("utf-8", errors="backslashreplace")
                for frame in frames
            ],
            "passed": frames == expected,
            "interceptedWebSockets": len(websocket_urls),
            "events": page.evaluate("window.__androidImeEvents"),
        }
    finally:
        page.close()


def main() -> int:
    args = parse_args()
    url = authenticated_url(args.url, args.token)

    chrome = args.chrome
    if chrome is None and DEFAULT_CHROME.is_file():
        chrome = DEFAULT_CHROME
    if chrome is not None and not chrome.is_file():
        raise FileNotFoundError(chrome)

    with sync_playwright() as playwright:
        launch_options: dict[str, Any] = {
            "headless": not args.headed,
            "args": ["--disable-background-networking"],
        }
        if chrome is not None:
            launch_options["executable_path"] = str(chrome)
        browser = playwright.chromium.launch(**launch_options)
        context = browser.new_context(
            viewport={"width": 360, "height": 639},
            screen={"width": 360, "height": 780},
            device_scale_factor=3,
            is_mobile=True,
            has_touch=True,
            user_agent=ANDROID_USER_AGENT,
        )
        try:
            cases = [
                run_case(
                    context,
                    url,
                    "keydown229_insertText",
                    [TEXT_PAYLOAD],
                    keydown_229_insert_text,
                ),
                run_case(
                    context,
                    url,
                    "compositionend_insertText",
                    [TEXT_PAYLOAD],
                    composition_end_insert_text,
                ),
                run_case(
                    context,
                    url,
                    "keydown_keypress_enter",
                    [ENTER_PAYLOAD],
                    keydown_keypress_enter,
                ),
                run_case(
                    context,
                    url,
                    "keydown229_keyup_composed_insertText",
                    [TEXT_PAYLOAD],
                    keydown_229_mutation_keyup_composed_insert_text,
                ),
                run_case(
                    context,
                    url,
                    "expired_keydown229_then_input_only",
                    [TEXT_PAYLOAD],
                    expired_keydown_229_then_input_only,
                ),
                run_case(
                    context,
                    url,
                    "composition_enter_trailing_commit",
                    [GO_PAYLOAD, ENTER_PAYLOAD],
                    composition_enter_trailing_commit,
                ),
                run_case(
                    context,
                    url,
                    "two_identical_input_only_edits",
                    [LETTER_PAYLOAD, LETTER_PAYLOAD],
                    two_identical_input_only_edits,
                ),
                run_case(
                    context,
                    url,
                    "two_complete_enter_cycles",
                    [ENTER_PAYLOAD, ENTER_PAYLOAD],
                    two_complete_enter_cycles,
                ),
                run_case(
                    context,
                    url,
                    "keydown229_soft_enter",
                    [ENTER_PAYLOAD],
                    keydown_229_soft_enter,
                ),
                run_case(
                    context,
                    url,
                    "keydown229_soft_paragraph",
                    [ENTER_PAYLOAD],
                    keydown_229_soft_paragraph,
                ),
                run_case(
                    context,
                    url,
                    "standard_enter_with_trailing_linebreak",
                    [ENTER_PAYLOAD],
                    standard_enter_with_trailing_linebreak,
                ),
                run_case(
                    context,
                    url,
                    "soft_enter_keypress_then_linebreak",
                    [ENTER_PAYLOAD],
                    soft_enter_keypress_then_linebreak,
                ),
                run_case(
                    context,
                    url,
                    "soft_enter_linebreak_then_keypress",
                    [ENTER_PAYLOAD],
                    soft_enter_linebreak_then_keypress,
                ),
                run_case(
                    context,
                    url,
                    "standard_enter_linebreak_then_keypress",
                    [ENTER_PAYLOAD],
                    standard_enter_linebreak_then_keypress,
                ),
                run_case(
                    context,
                    url,
                    "standard_enter_delayed_linebreak",
                    [ENTER_PAYLOAD],
                    standard_enter_delayed_linebreak,
                ),
                run_case(
                    context,
                    url,
                    "composition_commit_linebreak_keypress",
                    [GO_PAYLOAD, ENTER_PAYLOAD],
                    composition_commit_linebreak_keypress,
                ),
                run_case(
                    context,
                    url,
                    "composition_end_linebreak_trailing_commit",
                    [GO_PAYLOAD, ENTER_PAYLOAD],
                    composition_end_linebreak_trailing_commit,
                ),
                run_case(
                    context,
                    url,
                    "composition_linebreak_before_end",
                    [GO_PAYLOAD, ENTER_PAYLOAD],
                    composition_linebreak_before_end,
                ),
                run_case(
                    context,
                    url,
                    "composition_enter_without_native_end",
                    [GO_PAYLOAD, ENTER_PAYLOAD],
                    composition_enter_without_native_end,
                ),
                run_case(
                    context,
                    url,
                    "composition_enter_without_native_end_then_blur",
                    [GO_PAYLOAD, ENTER_PAYLOAD],
                    composition_enter_without_native_end_then_blur,
                ),
                run_case(
                    context,
                    url,
                    "composition_enter_blur_with_focus_reporting",
                    [FOCUS_OUT_PAYLOAD, GO_PAYLOAD, ENTER_PAYLOAD],
                    composition_enter_without_native_end_then_blur,
                    enable_focus_reporting=True,
                ),
                run_case(
                    context,
                    url,
                    "composition_enter_without_native_end_blur_refocus",
                    [GO_PAYLOAD, ENTER_PAYLOAD],
                    composition_enter_without_native_end_blur_refocus,
                ),
                run_case(
                    context,
                    url,
                    "composition_enter_without_native_end_then_key",
                    [GO_PAYLOAD, ENTER_PAYLOAD, ARROW_LEFT_PAYLOAD],
                    composition_enter_without_native_end_then_key,
                ),
                run_case(
                    context,
                    url,
                    "composition_enter_immediate_without_end_then_key",
                    [GO_PAYLOAD, ENTER_PAYLOAD, ARROW_LEFT_PAYLOAD],
                    composition_enter_immediate_without_end_then_key,
                ),
                run_case(
                    context,
                    url,
                    "composition_enter_immediate_then_printable_key",
                    [GO_PAYLOAD, ENTER_PAYLOAD, b"x"],
                    composition_enter_immediate_then_printable_key,
                ),
                run_case(
                    context,
                    url,
                    "composition_end_then_immediate_key",
                    [GO_PAYLOAD, ENTER_PAYLOAD, ARROW_LEFT_PAYLOAD],
                    composition_end_then_immediate_key,
                ),
                run_case(
                    context,
                    url,
                    "composition_enter_without_native_end_then_enter",
                    [GO_PAYLOAD, ENTER_PAYLOAD, ENTER_PAYLOAD],
                    composition_enter_without_native_end_then_enter,
                ),
                run_case(
                    context,
                    url,
                    "composition_enter_without_end_then_new_composition",
                    [GO_PAYLOAD, ENTER_PAYLOAD, b"n"],
                    composition_enter_without_end_then_new_composition,
                ),
                run_case(
                    context,
                    url,
                    "composition_enter_new_composition_then_blur",
                    [GO_PAYLOAD, ENTER_PAYLOAD],
                    composition_enter_new_composition_then_blur,
                ),
                run_case(
                    context,
                    url,
                    "composition_submit_then_new_start",
                    [GO_PAYLOAD, ENTER_PAYLOAD],
                    composition_submit_then_new_start,
                ),
                run_case(
                    context,
                    url,
                    "composition_submit_then_blur",
                    [GO_PAYLOAD, ENTER_PAYLOAD],
                    composition_submit_then_blur,
                ),
                run_case(
                    context,
                    url,
                    "composition_submit_then_late_blur",
                    [GO_PAYLOAD, ENTER_PAYLOAD],
                    composition_submit_then_late_blur,
                ),
            ]
            result = {
                "browser": browser.version,
                "frontendUrl": urlsplit(url)._replace(query="").geturl(),
                "webSocketMode": "mocked; never connected to server",
                "cases": cases,
            }
            print(json.dumps(result, indent=2, ensure_ascii=True))
        finally:
            context.close()
            browser.close()

    failures = [case["name"] for case in cases if not case["passed"]]
    if failures:
        raise AssertionError(
            "duplicate terminal input remains in: " + ", ".join(failures)
        )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
