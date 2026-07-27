# Repository Guide for Coding Agents

This file is the operational contract for agents working in this repository.
Read it before editing. Human-facing build and runtime details live in
[BUILDING.md](BUILDING.md) and [OPERATIONS.md](OPERATIONS.md).

## Mission

Codex Web Terminal exposes real Codex CLI, Claude Code, and Google Antigravity
CLI (`agy`) PTYs through an authenticated web interface. It must preserve
terminal semantics rather than reimplement any agent UI.

The most important properties are:

- raw PTY bytes remain raw;
- terminal input reaches exactly the selected managed session;
- reconnect replays bounded output and then resumes live output without gaps;
- tokens and terminal content never enter structured tracing;
- Windows and Unix command launch remain independently correct;
- a browser disconnect never terminates a managed PTY;
- production changes do not silently replace or restart a live server.
- agent discovery is read-only and never installs or updates host software.

## Repository map

```text
.
├── AGENTS.md                  Agent workflow and invariants
├── BUILDING.md                Windows/Linux build, test, and package guide
├── OPERATIONS.md              Runtime, UI, Tailscale, service, and upgrade guide
├── TODO.md                    Deliberately unimplemented, security-scoped ideas
├── README.md                  Product overview, architecture, API, and protocol
├── CONTRIBUTING.md            Contribution workflow and validation contract
├── SECURITY.md                Private vulnerability reporting policy
├── CODE_OF_CONDUCT.md         Community behavior and enforcement
├── THIRD_PARTY_NOTICES.md     Dependency licensing and attribution
├── LICENSE
├── .github/
│   ├── workflows/ci.yml       Windows/Linux continuous integration
│   └── ISSUE_TEMPLATE/        Privacy-safe issue forms
├── docs/
│   └── screenshots/           Sanitized, reproducible product screenshots
├── scripts/
│   ├── build.ps1              Windows production package
│   ├── run.ps1                Windows launcher
│   ├── build.sh               Linux production package
│   ├── run.sh                 Linux launcher
│   ├── fixtures/              Deterministic, synthetic demo PTY
│   ├── agent-catalog-regression.py
│   ├── android-ime-input-regression.py
│   ├── desktop-slash-regression.py
│   ├── mobile-codex-smoke.py  Browser/mobile smoke test
│   ├── mobile-resize-regression.py
│   └── session-tabs-regression.py
├── server/
│   ├── Cargo.toml
│   ├── Cargo.lock
│   ├── src/
│   │   ├── agents.rs          Agent discovery and install-guidance catalog
│   │   ├── auth.rs            Token validation, throttling, Origin checks
│   │   ├── config.rs          CLI/environment parsing and static asset lookup
│   │   ├── main.rs            Startup, listener, URLs, graceful Ctrl+C path
│   │   ├── protocol.rs        Browser control-message limits and parsing
│   │   ├── registry.rs        Up to four managed terminal entries
│   │   ├── routes.rs          Protected HTTP API and static serving
│   │   ├── session.rs         PTY lifecycle, replay buffer, process management
│   │   ├── terminal.rs        Command resolution and platform-specific PTY launch
│   │   └── websocket.rs       Authenticated attach, replay, input, live output
│   └── tests/
│       └── server_contract.rs
└── web/
    ├── package.json
    ├── package-lock.json
    ├── vite.config.ts
    └── src/
        ├── AgentPicker.tsx    Responsive agent discovery/start dialog
        ├── App.tsx            Main UI, sessions, settings, lifecycle actions
        ├── api.ts             Token/session storage and HTTP API client
        ├── sessions/          Header session tabs and navigation helpers
        ├── terminal/
        │   ├── TerminalView.tsx
        │   ├── androidImeGuard.ts
        │   ├── MobileToolbar.tsx
        │   ├── mobileKeys.ts
        │   ├── mobileResize.ts
        │   ├── protocol.ts
        │   ├── reconnect.ts
        │   ├── replay.ts
        │   └── settings.ts
        └── styles/app.css
```

Generated `dist*`, `server/target`, `web/node_modules`, and `web/dist` trees are
not source and must not be committed.

## Before changing anything

1. Read the user request and distinguish inspection from authorization to
   mutate source, services, Git, or Gitea.
2. Run:

   ```text
   git status --short
   git branch --show-current
   git log -1 --oneline
   ```

3. Preserve unrelated user changes. Never reset, clean, or check out over a
   dirty worktree merely to simplify the task.
4. Identify whether a live packaged server exists. Source edits do not update a
   running binary. Do not restart or replace a live server unless requested.
5. Never print or commit tokens, passwords, Git credentials, Codex
   authentication data, or credential-bearing local launch files.

## Platform launch invariant

`server/src/terminal.rs` has an intentional platform boundary.

Windows:

- searches for `codex.exe`, then `codex.cmd`, then an exact supplied extension;
- invokes `.cmd` through `cmd.exe /d /s /c call`;
- invokes executables through the configured PowerShell/cmd wrapper;
- uses ConPTY through `portable-pty`.

Unix:

- searches for the exact executable in `PATH` or accepts a trusted path;
- runs `command --version` directly during preflight;
- launches the resolved executable directly in the Unix PTY;
- ignores the Windows-only shell selection.

Do not unify these paths through a generic shell command string. The command
configuration intentionally rejects arbitrary shell expressions.
Optional Claude and AGY dangerous-mode switches append only the fixed upstream
`--dangerously-skip-permissions` argument. Unix and Windows `cmd` launches keep
it as a distinct process argument. The Windows PowerShell wrapper must encode
each fixed argument as an independently single-quoted literal with embedded
quotes escaped. Never accept executable arguments from the browser API.

Agent auto-detection is also a platform boundary:

- it probes fixed executable names and documented per-user locations;
- it runs `--version` directly with bounded output and a timeout;
- an explicit command override is authoritative and must not silently fall
  back to another executable;
- `--no-agent-auto-detect` disables optional discovery without changing
  explicit profiles or validation of the primary profile;
- a missing or misconfigured CLI remains visible as catalog metadata, but is
  never offered as a runnable session profile;
- detection, **Refresh**, and **Check again** are read-only; they do not run an
  installer, updater, login flow, package manager, shell expression, or
  privileged command.

Installation guidance returned by the backend must be static, platform-specific
metadata from an allowlist. The browser may display or copy that guidance, but
must never supply a command or URL for the host to execute. Installation and
updates happen manually on the machine running `codex-web`, under its operating
system account.

Any change to command discovery, `ShellKind`, `ResolvedCodex`, `pty_command`,
preflight, PTY startup, or process termination requires:

- Windows compile/tests;
- Linux compile/tests;
- a real native PTY smoke test on the affected platform.

## Session invariants

- The registry contains one primary session and at most four total sessions.
- The primary entry cannot be deleted.
- `terminalId` is stable for the managed entry.
- `sessionId` identifies one PTY generation and changes on restart.
- Output from an old generation must never be appended to the active buffer.
- A browser attach changes only the displayed session.
- Managed children and version probes must not inherit the parent-session
  markers `CODEX_THREAD_ID` or `CLAUDECODE`; each terminal must start
  independently even when the server was launched from another agent session.
  Do not remove provider credentials or unrelated environment variables.
- When `--new-session-command` is configured, the primary terminal uses
  `--command`; **New** uses the distinct new-session command only when the
  primary agent is selected. Other agent cards use their own explicit override
  or default command. All commands follow the same platform resolution and
  preflight rules.
- Reconnect changes only the WebSocket attachment.
- Restart terminates and recreates the selected PTY.
- Terminate stops the process but keeps the managed entry.
- Deleting a non-primary session terminates it and removes its entry.
- A browser close or network interruption must not stop the PTY.

The server retains up to 16 MiB of raw output per session. A newly attached
browser receives at most the newest 2 MiB. WebSocket replay ordering and
sequence checks exist to prevent replay/live gaps. Preserve those properties
when changing batching or reconnect behavior.

## Browser and mobile invariants

- xterm.js owns ANSI/VT interpretation and terminal scrollback.
- Do not parse or rewrite Codex terminal text in the frontend.
- Do not enable `convertEol`; the PTY owns line endings.
- Resize messages must remain bounded to 20–500 columns and 5–300 rows.
- Mobile viewport changes are asynchronous on Android and iOS.
- Avoid resize loops that repeatedly call FitAddon while the visual viewport
  and hidden xterm textarea are moving.
- The mobile toolbar order starts with Enter and arrows, then history/control
  keys.
- Session tabs remain horizontally scrollable on overflow. **+ New** and
  **Manage** stay outside the inner tab scroller so they remain discoverable.
- On desktop, an unmodified `/` pressed outside an editable control or dialog
  is routed to the connected terminal and its browser default is suppressed.
  Do not intercept modified shortcuts, form input, dialogs, IME composition,
  or coarse-pointer/mobile input.
- `Top`, `PgUp`, `PgDn`, and `Live` manipulate client scrollback; they are not
  terminal input.
- Diagnostics must not include token, keystrokes, or terminal content.
- Screenshots must use only synthetic fixtures. They must not contain account,
  organization, company, device, or host names; credentials; tokens; private
  addresses; terminal history; or personal filesystem paths.

For mobile changes, run unit tests and the relevant Python browser regression
where the required browser tooling is available.

## Authentication and security invariants

- HTTP API uses `Authorization: Bearer`.
- WebSocket uses the URL query token because browser WebSocket APIs cannot set
  the Authorization header.
- WebSocket Origin validation must remain enabled.
- Authentication comparison remains constant-time for equal-length tokens.
- Repeated failures remain throttled per address.
- The project directory remains startup-only and canonicalized.
- Do not add public-tunnel or router-port-forward automation.
- Do not log request URLs because the first URL contains the token.
- Do not log terminal input, output, or Codex credentials.
- Do not add silent browser-triggered CLI installation or update. Installing a
  host executable is a separate, security-sensitive operator action.
- Startup stdout intentionally prints the authenticated URL; document that
  redirected stdout or a service journal will retain this credential.
- Any remote-access documentation must prefer loopback or Tailscale and explain
  the risk of binding `0.0.0.0`.

## Required validation

Every code change, bug fix, refactor, or dependency update requires validation
on both Windows and Linux. This rule applies even when a change looks
platform-independent. A Windows-only result is incomplete. If Linux is not
available locally, use GitHub Actions, a Linux VM, or a Linux host you control;
do not skip or simulate the platform result.

### Frontend

From `web/`:

```text
npm ci
npm test
npm run build
```

Do not run `npm update` unless dependency updates are the explicit task.

### Rust

From `server/`:

```text
cargo fmt --all -- --check
cargo test --all-targets --locked
cargo clippy --all-targets --locked -- -D warnings
cargo build --release --locked
```

On Windows without `link.exe`, use the installed GNU Rust toolchain and MinGW:

```text
rustup toolchain list
rustup component add --toolchain <installed-gnu-toolchain> clippy
rustup run <installed-gnu-toolchain> cargo test --all-targets --locked
rustup run <installed-gnu-toolchain> cargo clippy --all-targets --locked -- -D warnings
rustup run <installed-gnu-toolchain> cargo build --release --locked
```

### Package validation

Check:

- executable exists and runs `--version`;
- adjacent `web/index.html` exists;
- hashed JS and CSS assets referenced by `index.html` exist;
- root HTTP request returns 200;
- `/api/health` distinguishes command discovery from a running PTY;
- WebSocket input produces PTY output.

### Documentation validation

When CLI flags, environment variables, UI labels, session semantics, build
outputs, or supported platforms change, update all affected Markdown files.
Documentation must reflect the current source and the latest verified
Windows/Linux behavior in the same commit. Recheck commands, versions,
platform claims, package layouts, UI labels, and limitations; do not copy
stale information forward.

Agent installation and update commands are time-sensitive external facts.
Before changing them, verify the current official OpenAI, Anthropic, and Google
documentation. Never infer a package name, download URL, checksum, or
permission flag from memory. Keep manual-install guidance separate from
read-only auto-detection.

Search for stale platform-specific text:

```text
rg -n "Windows|Linux|ConPTY|PTY|build.ps1|build.sh|--shell|CODEX_WEB_" *.md
```

## Build and release paths

Windows:

```text
.\scripts\build.ps1
dist\codex-web.exe
```

Linux:

```text
./scripts/build.sh
dist-linux/codex-web
```

The executable and `web` directory are one release unit. Do not deploy only
one half.

The Windows `dist` and Linux `dist-linux` directories are ignored artifacts.
Never infer that their contents belong in a commit.

## Live-server safety

- Use a disposable high port for browser and PTY tests.
- Check the exact listener and PID before starting or stopping anything.
- Never kill a production process by a broad name match.
- Do not replace a running package in place.
- Source edits do not hot-reload a packaged backend.
- Restarting the Rust server destroys the in-memory registry and every live
  PTY; a stable token does not preserve those processes.
- Stop temporary services and verify their ports are closed unless the user
  explicitly asked to keep them running.

## Cross-layer changes

The WebSocket control and replay protocol is implemented in both Rust and
TypeScript. A protocol change must update both sides and their tests in the
same change. Do not accept a backend-only or frontend-only protocol mutation.

Changes to resize, replay, mobile focus, or viewport handling require the
frontend unit suite, production build, and the relevant real-browser
regression. Multiple clients attached to one PTY share both input and the PTY
size, so competing viewports are an intentional operational limitation.

## Git workflow

- Do not commit or push unless the user requests it.
- Do not change repository visibility or collaborator permissions unless
  explicitly requested.
- Before staging, inspect `git diff --check`, `git diff`, and `git status`.
- Stage only intended source, tests, scripts, and documentation.
- Preserve the repository's current visibility unless explicitly told
  otherwise. A public release requires a secret scan of the current tree and
  Git history before push.
- A user who only needs to clone should receive `read`, not `write` or `admin`.
- After push, verify the remote branch commit matches local `HEAD`.

## Definition of done

A change is complete only when:

1. The requested behavior is implemented in source, not only in generated
   output.
2. Relevant frontend and Rust tests pass on both Windows and Linux for every
   code change.
3. Both OS-specific paths are checked, including for changes that appear
   platform-independent.
4. A real PTY/runtime test is performed when process launch or terminal I/O
   changed.
5. Documentation contains current, verified information and matches the actual
   commands, versions, behavior, supported platforms, and package layout.
   Changes to agent discovery, catalog metadata, install guidance, or launch
   arguments are documented and validated on both Windows and Linux.
6. No secrets, generated artifacts, or unrelated changes are staged.
7. Live services created for testing are stopped unless the user asked to keep
   them running.
8. The final report distinguishes source changes, tests, commit/push state,
   deployment state, and any remaining limitations.
