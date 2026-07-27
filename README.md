# Codex Web Terminal

[![CI](https://github.com/bproject07/Codex-web/actions/workflows/ci.yml/badge.svg)](https://github.com/bproject07/Codex-web/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-2ea44f.svg)](LICENSE)
[![Platforms](https://img.shields.io/badge/platforms-Windows%20%7C%20Linux-4c8bf5.svg)](BUILDING.md)

**Unlike structured remote-control interfaces, Codex Web Terminal provides
full, self-hosted access to the real Codex CLI experience directly from any
desktop or mobile browser.**

Codex Web Terminal runs the **real Codex CLI, Claude Code, or Google
Antigravity CLI (`agy`)** in a native pseudo-terminal (ConPTY on Windows, a
Unix PTY on Linux and macOS) and exposes that terminal to a browser through an
authenticated WebSocket. It does not reimplement or scrape an agent
interface. ANSI output, cursor movement, menus, approval prompts, diffs,
spinners, keyboard input, and the rest of each TUI are interpreted by xterm.js
from the original PTY byte stream.

The project is an independent wrapper. It does not download, vendor, modify,
or silently install or update any agent CLI.

> [!IMPORTANT]
> This is an independent, unofficial community project. It is not affiliated
> with, sponsored by, or endorsed by OpenAI, Anthropic, or Google. Agent CLIs
> are installed separately and are not included in this repository.

> **This application provides remote terminal access.**
> **Do not expose it directly to the public internet.**

## Screenshots

### Desktop terminal

![Desktop terminal showing the synthetic community demo](docs/screenshots/desktop-terminal.png)

### Session manager

![Desktop session manager showing synthetic terminal sessions](docs/screenshots/session-manager.png)

### Mobile terminal

![Mobile terminal with the touch toolbar and synthetic demo output](docs/screenshots/mobile-terminal.png)

All screenshots are generated from the repository's deterministic demo PTY.
They contain synthetic terminal text only—no live Codex conversation, model
output, credentials, tokens, account or company names, personal paths, or
private host data. See [the screenshot guide](docs/screenshots/README.md) for
the reproducible capture procedure.

## Documentation

- [BUILDING.md](BUILDING.md) — complete Windows and Linux prerequisites,
  validation, build, package, and troubleshooting instructions
- [OPERATIONS.md](OPERATIONS.md) — startup, tokens, browser controls,
  Tailscale, services, monitoring, shutdown, and upgrades
- [AGENTS.md](AGENTS.md) — repository map, invariants, test matrix, and
  definition of done for coding agents
- [TODO.md](TODO.md) — deliberately unimplemented ideas and their safety
  requirements
- [CONTRIBUTING.md](CONTRIBUTING.md) — contribution workflow and required
  cross-platform validation
- [SECURITY.md](SECURITY.md) — supported versions and private vulnerability
  reporting
- [CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md) — community expectations
- [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md) — dependency licensing and
  attribution

## Architecture

```text
Desktop or mobile browser
  React + xterm.js
          │
          │ authenticated HTTP + WebSocket
          │ binary terminal bytes / JSON control messages
          ▼
Rust server
  Tokio + Axum
          │
          │ managed session registry (maximum 4)
          ▼
Up to 4 native PTY sessions
          │
          ├── Windows: resolved .exe or .cmd entry point
          └── Unix: resolved executable launched directly
                         │
                         ▼
               Codex, Claude, or AGY per session
```

The server owns up to four independent, web-managed agent PTY sessions. The
browser displays one selected session in the same xterm screen and can switch
between them without terminating the others. Closing a tab or losing the
network does not terminate those sessions. A reconnecting browser resets xterm
and replays the selected session's bounded terminal output buffer before it
resumes live output.

## Current scope

- Up to four persistent, web-managed session entries per server; each running
  entry owns one agent process
- One primary agent session, started automatically
- Read-only discovery of Codex, Claude, and AGY with installed version and
  `ready`, `missing`, or `misconfigured` status
- Up to four authenticated WebSocket clients per terminal session
- Fixed project directory selected when the server starts
- One 16 MiB bounded raw terminal output buffer per session
- Up to the newest 2 MiB replayed to each newly attached client
- Initial PTY size of 120 columns by 35 rows
- Validated browser resize range: 20–500 columns and 5–300 rows
- Windows 10/11 and x86_64 Linux are tested platforms
- Linux runtime validation currently covers Arch Linux
- No built-in TLS, reverse proxy, or tunnel

Clients attached to the same terminal session share its PTY and can type into
it concurrently. They also share its PTY dimensions, so the most recent valid
resize from any attached browser wins. The authenticated URL grants full
read/write control over every managed session, so do not share it with anyone
who should not have terminal access.

## Prerequisites

### Windows

- Windows 10 version 1809 or newer, or Windows 11, for ConPTY
- PowerShell 5.1 or newer
- A supported browser: current Chrome, Edge, Firefox, or Safari on mobile

### Linux

- A current x86_64 Linux distribution with standard Unix PTY support
- GCC or Clang and the normal native build tools required by Rust
- A supported browser on the client device

### Rust

Install stable Rust from [rustup.rs](https://rustup.rs/).

The recommended Windows target is MSVC. Install Visual Studio Build Tools with
the **Desktop development with C++** workload. Run builds from Developer
PowerShell when `link.exe` is not already in `PATH`.

The build script can also use an installed `x86_64-pc-windows-gnu` Rust
toolchain when MinGW `gcc.exe` is available.

Verify:

```powershell
rustc --version
cargo --version
```

### Node.js

Install a Vite-supported Node.js release: Node 20.19 or newer within the Node
20 line, or Node 22.12 or newer. Then verify:

```powershell
node --version
npm --version
```

### Agent CLIs

Install at least the primary CLI on the machine that will run `codex-web`.
Codex Web Terminal detects Codex, Claude Code, and AGY in the server account's
`PATH` and documented per-user install locations. Detection runs only each
CLI's `--version` command; it never installs software or starts a login flow.

Official native install commands:

| CLI | Windows PowerShell | Linux/macOS |
| --- | --- | --- |
| Codex | `powershell -ExecutionPolicy ByPass -c "irm https://chatgpt.com/codex/install.ps1 \| iex"` | `curl -fsSL https://chatgpt.com/codex/install.sh \| sh` |
| Claude Code | `irm https://claude.ai/install.ps1 \| iex` | `curl -fsSL https://claude.ai/install.sh \| bash` |
| AGY | `irm https://antigravity.google/cli/install.ps1 \| iex` | `curl -fsSL https://antigravity.google/cli/install.sh \| bash` |

Review the current upstream instructions before executing downloaded scripts:
[Codex CLI](https://learn.chatgpt.com/docs/codex/cli),
[Claude Code](https://code.claude.com/docs/en/setup), and
[Antigravity CLI](https://antigravity.google/docs/cli/install).

Verify and authenticate each CLI directly on the server host:

```text
codex --version
claude --version
agy --version
```

Run `codex login`, `claude`, or `agy` once when that CLI requires interactive
authentication. Codex Web Terminal neither copies nor reads agent credentials.
Each child inherits the server account's environment and existing CLI
configuration.

The browser shows missing or misconfigured agents together with an official
manual command and verification command. Run the command in a trusted terminal
on the **server host**, then choose **Refresh** or **Check again**. There is
deliberately no silent browser installation: installing a host executable is
a security-sensitive operator action and may require interactive review,
authentication, package-manager policy, or elevation.

## Quick start

From the repository root:

Windows:

```powershell
.\scripts\build.ps1
.\scripts\run.ps1 -Project "C:\Projects\my-app"
```

Linux:

```bash
./scripts/build.sh
./scripts/run.sh "/path/to/my-app" --no-open-browser
```

The server starts on `127.0.0.1:8787`, generates an ephemeral cryptographically
secure token, and prints an authenticated URL. When bound to an unspecified
address, it can print both local and discovered network URLs:

```text
Codex Web Terminal started

Local URL:
http://127.0.0.1:8787/?token=...
```

Open that URL. The frontend moves the token into `sessionStorage` and removes it
from the visible address. The token is not put in `localStorage`.

Use the session tabs in the header to switch between managed terminals and
**+ New** to create another live agent PTY. The picker reports the detected
Codex, Claude, and AGY status and installed version. Every open of **+ New**
forces a fresh server-side check so an older browser tab cannot reuse stale
availability. A ready agent can be started; a missing or misconfigured agent
displays manual host-side installation guidance and **Refresh** /
**Check again** actions. Swipe the tab
strip on mobile,
or use a wheel, trackpad, or the overflow arrows on desktop. **Manage** opens
the detailed session list. Switching tabs does not stop the previously
displayed session; it continues running and buffering output in the background.

The generated token changes whenever the server restarts. Supply `--token` or
`CODEX_WEB_TOKEN` when a stable token is required.

## Development

Use two terminals.

Terminal 1:

```powershell
cd server
cargo run -- --project "C:\Projects\my-app" --no-open-browser
```

On Linux, the equivalent command is:

```bash
cd server
cargo run -- --project "/path/to/my-app" --no-open-browser
```

Terminal 2:

```text
cd web
npm install
npm run dev
```

Open the Vite URL, adding the token printed by the Rust server:

```text
http://127.0.0.1:5173/?token=TOKEN_FROM_SERVER
```

Vite proxies `/api` and `/ws` to `127.0.0.1:8787`. The backend explicitly
allows loopback development origins on another port.

If the active Rust toolchain is MSVC but Visual C++ is unavailable, use a
configured GNU toolchain for local validation:

```powershell
$gnuToolchainLine = rustup toolchain list |
  Where-Object { $_ -match "x86_64-pc-windows-gnu" } |
  Select-Object -First 1
if (-not $gnuToolchainLine) {
  throw "Install an x86_64-pc-windows-gnu Rust toolchain first."
}
$gnuToolchain = ($gnuToolchainLine -split "\s+")[0]

rustup run $gnuToolchain cargo run -- `
  --project "C:\Projects\my-app" `
  --no-open-browser
```

The command derives the exact installed GNU toolchain name instead of assuming
that it is named `stable`.

## Production build

The reproducible manual workflow is the same on every platform:

```text
cd web
npm ci
npm run build

cd ../server
cargo build --release --locked
```

On Windows, the convenience build script is:

```powershell
.\scripts\build.ps1
```

On Linux:

```bash
./scripts/build.sh
```

The scripts check the required tools, build both applications, and create:

```text
dist/                    # Windows
├── codex-web.exe
├── web/
│   ├── index.html
│   └── assets/
├── README.md
├── BUILDING.md
├── OPERATIONS.md
├── AGENTS.md
├── TODO.md
├── CONTRIBUTING.md
├── SECURITY.md
├── CODE_OF_CONDUCT.md
├── THIRD_PARTY_NOTICES.md
├── docs/
│   └── screenshots/
└── LICENSE

dist-linux/              # Linux
├── codex-web
├── web/
│   ├── index.html
│   └── assets/
├── README.md
├── BUILDING.md
├── OPERATIONS.md
├── AGENTS.md
├── TODO.md
├── CONTRIBUTING.md
├── SECURITY.md
├── CODE_OF_CONDUCT.md
├── THIRD_PARTY_NOTICES.md
├── docs/
│   └── screenshots/
└── LICENSE
```

Keep the `web` directory next to `codex-web.exe`. The executable serves those
assets. A Linux package uses the same layout with the extensionless
`codex-web` binary. During `cargo run`, the backend also looks for `web/dist`.

These are local build outputs, not published binary releases. Anyone
redistributing them must first include the complete dependency license and
NOTICE texts described in [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).

## Command-line interface

```powershell
.\dist\codex-web.exe --help
.\dist\codex-web.exe --version
```

On Linux:

```bash
./server/target/release/codex-web --help
./server/target/release/codex-web --version
```

Local example:

```powershell
.\dist\codex-web.exe `
  --project "C:\Projects\my-app" `
  --host 127.0.0.1 `
  --port 8787 `
  --shell powershell `
  --command codex
```

Tailscale example using the machine's exact Tailscale IP:

```powershell
$env:CODEX_WEB_TOKEN = "generate-and-paste-a-long-random-token"
$tailscaleIp = (tailscale ip -4 | Select-Object -First 1).Trim()
.\dist\codex-web.exe `
  --project "C:\Projects\my-app" `
  --host $tailscaleIp `
  --port 8787
```

Supported arguments:

| Argument | Purpose |
| --- | --- |
| `--host` | Bind address; defaults to `127.0.0.1` |
| `--port` | TCP port; defaults to `8787` |
| `--project` | Fixed working directory for every agent |
| `--shell` | Windows-only: `powershell` or `cmd`; ignored on Unix |
| `--command` | Explicit executable override for the primary terminal |
| `--primary-agent` | Agent represented by `--command`: `codex`, `claude`, or `agy`; defaults to `codex` |
| `--new-session-command` | Optional executable used when **New** starts the primary agent; defaults to the resolved primary command |
| `--codex-command` | Explicit Codex CLI executable override |
| `--claude-command` | Explicit Claude Code executable override |
| `--claude-dangerously-skip-permissions` | Launch Claude with `--dangerously-skip-permissions` |
| `--agy-command` | Explicit Google Antigravity CLI executable override |
| `--agy-dangerously-skip-permissions` | Launch AGY with `--dangerously-skip-permissions` |
| `--no-agent-auto-detect` | Disable automatic discovery of optional agent CLIs |
| `--token` | Authentication token, minimum 16 characters |
| `--no-open-browser` | Do not launch the default browser |
| `--log-level` | tracing filter such as `info` or `debug` |

Command values are treated as executable names or file paths, not as arbitrary
shell expressions. A discovered `.cmd` entry point is always invoked through
`cmd.exe /d /s /c` on Windows, which is required for the npm Codex package. On
Unix, the resolved executable is launched directly without a shell wrapper.
The two permission switches add one fixed argument to the selected process.
On Unix and Windows `cmd` launches it remains a distinct process argument. The
Windows PowerShell wrapper encodes it as a single-quoted literal with embedded
quotes escaped. It cannot be selected or altered by a browser client.

With auto-detection enabled (the default), the primary executable name follows
`--primary-agent`, and the server probes `codex`, `claude`, and `agy` plus
their documented per-user locations. An explicit `--command`,
`--codex-command`, `--claude-command`, or `--agy-command` is authoritative: a
typo or broken path is reported as `misconfigured` and never falls back
silently. Use
`--no-agent-auto-detect` when deployment policy requires optional agents to
have explicit profiles. The primary profile is still resolved and validated.

`--new-session-command` is useful when the primary terminal uses a wrapper
that resumes a specific Codex thread. For example, start the primary with that
trusted wrapper and pass `--new-session-command codex` so **New** always opens
an independent Codex session when the Codex (primary-agent) card is selected.
Claude and AGY cards continue to use their own override or default command.

Normally no additional flags are needed to expose installed CLIs. Explicit
paths are useful for services with a restricted `PATH`:

```powershell
.\dist\codex-web.exe `
  --project "C:\Projects\my-app" `
  --command codex `
  --claude-command "$HOME\.local\bin\claude.exe" `
  --agy-command "$env:LOCALAPPDATA\agy\bin\agy.exe"
```

Add the following switches only in a trusted, isolated environment when every
tool action should run without a permission prompt:

```text
--claude-dangerously-skip-permissions
--agy-dangerously-skip-permissions
```

They launch `claude --dangerously-skip-permissions` and
`agy --dangerously-skip-permissions`, respectively. Both upstream CLIs warn
that this bypasses their normal safety confirmations. The similar Claude flag
`--allow-dangerously-skip-permissions` merely makes bypass mode available; it
does not enable bypass mode at startup.

## Environment variables

CLI arguments override environment variables.

| Variable | Default |
| --- | --- |
| `CODEX_WEB_HOST` | `127.0.0.1` |
| `CODEX_WEB_PORT` | `8787` |
| `CODEX_WEB_PROJECT_DIR` | Current directory |
| `CODEX_WEB_TOKEN` | Secure random token generated at startup |
| `CODEX_WEB_COMMAND` | Unset; derived from `CODEX_WEB_PRIMARY_AGENT` |
| `CODEX_WEB_PRIMARY_AGENT` | `codex` |
| `CODEX_WEB_NEW_SESSION_COMMAND` | Unset; uses the resolved primary command |
| `CODEX_WEB_CODEX_COMMAND` | Unset; auto-detects `codex` |
| `CODEX_WEB_CLAUDE_COMMAND` | Unset; auto-detects `claude` |
| `CODEX_WEB_CLAUDE_DANGEROUSLY_SKIP_PERMISSIONS` | `false` |
| `CODEX_WEB_AGY_COMMAND` | Unset; auto-detects `agy` |
| `CODEX_WEB_AGY_DANGEROUSLY_SKIP_PERMISSIONS` | `false` |
| `CODEX_WEB_NO_AGENT_AUTO_DETECT` | `false` |
| `CODEX_WEB_SHELL` | `powershell`; Windows-only and ignored on Unix |
| `CODEX_WEB_LOG_LEVEL` | `info` |

The project directory is canonicalized and checked once at startup. No API or
frontend message can replace it.

## HTTP API

All API endpoints require:

```http
Authorization: Bearer YOUR_TOKEN
```

Endpoints:

| Method | Path | Purpose |
| --- | --- | --- |
| `GET` | `/api/health` | Aggregate installation, process, session, and client-count health |
| `GET` | `/api/agents` | Compatibility list of configured, ready profiles available to **New** |
| `GET` | `/api/agent-catalog` | Read-only platform, status, version, verification, install, and update metadata |
| `GET` | `/api/sessions` | List sanitized metadata for all managed sessions |
| `POST` | `/api/sessions` | Create a terminal; optional JSON body: `{"agent":"codex"}`, `{"agent":"claude"}`, or `{"agent":"agy"}` |
| `GET` | `/api/sessions/{terminalId}` | Get one managed session |
| `POST` | `/api/sessions/{terminalId}/restart` | Terminate and recreate one agent PTY |
| `POST` | `/api/sessions/{terminalId}/terminate` | Terminate one agent PTY without removing its entry |
| `DELETE` | `/api/sessions/{terminalId}` | Terminate and remove a non-primary session |
| `GET` | `/api/session` | Legacy alias for primary-session metadata |
| `POST` | `/api/session/restart` | Legacy alias for restarting the primary session |
| `POST` | `/api/session/terminate` | Legacy alias for terminating the primary session |
| `GET` | `/ws?token=...&terminalId=...` | Attach an authenticated WebSocket to one managed terminal |

No response contains the authentication token, terminal input, terminal
output, Codex credentials, or Codex authentication files.

`/api/agent-catalog` uses schema version 1 and reports each known agent as
`ready`, `missing`, or `misconfigured`. The `configuration` field is `auto` or
`override`, so a broken authoritative override can be repaired instead of
mistaken for a missing installation. Install metadata is an allowlisted
operator hint for the server operating system: it includes a shell, command,
verification command, update command, official documentation URL, and
`requiresServerAccess: true`. Fetching the catalog or pressing **Refresh** /
**Check again**
performs detection only. No API endpoint executes that command.

Each session snapshot contains a stable `terminalId`, display `name`, `agent`,
`isPrimary`, and `createdAt`. The existing `sessionId` identifies the current
PTY generation and therefore changes when that terminal is restarted.

## WebSocket protocol

The browser WebSocket API cannot attach an `Authorization` header, so the
upgrade uses the URL-encoded `token` query parameter. The server also validates
the browser `Origin` header. `terminalId` selects the managed session. Omitting
it attaches to the primary session for compatibility with older clients.
Malformed IDs are rejected with HTTP 400 and unknown IDs with HTTP 404. Each
terminal session accepts at most four simultaneous authenticated WebSocket
clients.

### Browser to server

- **Binary frame:** UTF-8 terminal input written directly to the PTY writer
- **Text frame:** JSON control message

Resize:

```json
{"type":"resize","cols":120,"rows":35}
```

Heartbeat:

```json
{"type":"ping"}
```

Restart:

```json
{"type":"restart"}
```

### Server to browser

- **Binary frame:** unmodified raw bytes read from the PTY
- **Text frame:** JSON session, replay, pong, or sanitized error event

On connection the server sends the selected session's snapshot, `replay_start`,
bounded binary output chunks, and `replay_end`. Output chunks have internal
monotonic sequence numbers and PTY-generation session IDs; those values are
used by the backend to prevent replay/live gaps and are not inserted into the
terminal byte stream.

## Terminal behavior

xterm.js is configured with:

- ANSI/VT parsing and original Codex colors
- Cascadia Mono/Cascadia Code/Consolas font fallback
- 10,000 lines of client scrollback by default
- FitAddon resize using `ResizeObserver`
- debounced PTY resize messages
- WebLinksAddon
- exponential reconnect delays of 1, 2, 4, 8, then 15 seconds

Normal xterm keyboard handling provides Enter, Escape, Backspace, Tab, arrow
keys, Home, End, Page Up/Down, Ctrl+C, Ctrl+L, Ctrl+R, paste, and other terminal
sequences.

On desktop, an unmodified `/` pressed while a non-editable header control has
focus is routed to the connected terminal. Its browser default is suppressed
so Firefox Quick Find does not replace terminal input. Form fields, dialogs,
mobile/coarse-pointer input, IME composition, and modified shortcuts keep their
normal behavior.

The mobile toolbar begins with Enter and the arrow keys, followed by Page
Up/Down, Ctrl mode, Esc, Tab, Ctrl+C, Ctrl+L, Top, Live, and Hide. Its Ctrl mode
converts the next typed ASCII letter to the matching control character, then
automatically turns off.

The header's session tabs, **+ New**, and **Manage** controls operate on
independent live PTYs. The active tab selects which managed session feeds the
same xterm screen. The tab strip scrolls horizontally when it overflows; it
does not send `/new` or `/resume` commands into the selected agent's TUI.

## Security

This process has the same operating-system permissions and environment as the
user who starts it. Anyone with the authenticated URL can interact with the
selected agent, approve actions it presents, and potentially cause commands to
run in the configured project.

Security measures in this application:

- Loopback-only bind by default
- Required authentication for HTTP APIs and WebSocket
- 256-bit token generated by the operating-system CSPRNG when omitted
- constant-time comparison for equal-length tokens
- failed-authentication throttling and a temporary per-IP block
- strict WebSocket Origin validation
- fixed startup-only project path
- four-client-per-session limit
- 64 KiB WebSocket message limit
- 4 KiB JSON control-message limit
- 16 MiB retained output-buffer limit per session
- 2 MiB maximum initial replay per browser attachment
- Content Security Policy, frame denial, no-referrer policy, and MIME sniffing
  protection
- structured tracing excludes token, keys, input, and terminal output

The startup console intentionally prints the authenticated URL. Redirected
stdout and service journals can therefore retain that URL even though
structured tracing excludes the token. Protect console output and journals.
Do not paste the URL into chat, logs, screenshots, analytics, issue trackers,
or browser-sync services.

For remote use:

1. Prefer [Tailscale](https://tailscale.com/) between devices.
2. Otherwise use a trusted private LAN.
3. Put an HTTPS reverse proxy in front of the server when transport leaves the
   local machine.
4. Add another authentication layer before considering Cloudflare Tunnel.

Do not automatically expose the port with a public tunnel or router port
forward. This project intentionally contains no such feature.

Origin validation has a deliberate deployment consequence: a backend bound to
`127.0.0.1` accepts only loopback browser origins. When an HTTPS reverse proxy
serves a non-loopback hostname, bind the backend to a specific private or
Tailscale address, or to `0.0.0.0` behind a restrictive firewall. The proxy
must preserve the public `Host` header and port and forward WebSocket Upgrade
and binary frames unchanged.

## Tailscale example

Start the server with an explicit strong token:

```powershell
$env:CODEX_WEB_TOKEN = "generate-and-paste-a-long-random-token"
$tailscaleIp = (tailscale ip -4 | Select-Object -First 1).Trim()
.\dist\codex-web.exe `
  --project "C:\Projects\my-app" `
  --host $tailscaleIp `
  --port 8787
```

Use the machine's Tailscale IP or MagicDNS name from another enrolled device:

```text
http://my-windows-pc:8787/?token=...
```

Restrict access with Tailscale ACLs. For stronger browser transport security,
put an HTTPS reverse proxy on the Tailscale interface. Binding to the exact
Tailscale IP is narrower than `0.0.0.0`; see [OPERATIONS.md](OPERATIONS.md) for
Windows, Linux, firewall, and service examples.

## Logging

`tracing` records server address, project directory, PTY startup, PID when
available, client connection/disconnection, restart, process exit, and
sanitized errors.

Structured tracing deliberately does not record:

- authentication tokens
- pressed keys or terminal input
- terminal output
- Codex credentials or authentication files

Separately, normal startup stdout prints the full authenticated URL so the
operator can open it. Capturing stdout in a file or service journal captures
that credential. Use an explicit protected token and an appropriate service
logging policy for persistent installations; see [OPERATIONS.md](OPERATIONS.md).

Use `--log-level debug` only for server-level diagnostics; terminal content is
still excluded.

## Tests

Every code change, bug fix, refactor, or dependency update must be validated on
both Windows and Linux before it is considered complete. Do not rely only on a
successful Windows build. Run the matching frontend, Rust, package, and runtime
checks from [BUILDING.md](BUILDING.md) on both platforms.

Documentation is part of the change: keep commands, required versions,
supported platforms, package layouts, UI behavior, and known limitations
accurate in the same commit. Do not preserve a statement merely because it was
true for an older build.

Frontend:

```powershell
Push-Location .\web
npm ci
npm test
npm run build
Pop-Location
```

Rust:

```powershell
Push-Location .\server
cargo fmt --all -- --check
cargo test --all-targets --locked
cargo clippy --all-targets --locked -- -D warnings
Pop-Location
```

Tests cover token validation and throttling, config validation, resize and
control-message parsing, session state transitions, the bounded output buffer,
Windows `.cmd` invocation with spaces, direct Unix PTY execution, control
encoding, reconnect state/backoff, UTF-8 input, and mobile Ctrl conversion.
The automated tests do not require a real Codex process.

## Troubleshooting

### An agent is `missing` or `misconfigured`

Run the matching checks on the same host and as the same operating-system user
that runs `codex-web`:

```powershell
where.exe codex
codex --version
where.exe claude
claude --version
where.exe agy
agy --version
```

On Linux:

```bash
command -v codex
codex --version
command -v claude
claude --version
command -v agy
agy --version
```

For an npm installation, `where.exe` should normally show `codex.cmd`. Restart
PowerShell after installation so it receives the updated `PATH`. You can also
pass a trusted absolute path with `--command`, `--codex-command`,
`--claude-command`, or `--agy-command`. On Linux, ensure the resolved file is
executable. An explicit
override never falls back to auto-detection; correct or remove it, restart the
server if its startup configuration changed, and press **Refresh** or
**Check again**. Run any displayed install command on the server host, never
in the browser developer console or on the viewing phone/laptop.

### Authentication failed

Use the complete URL printed by the current server process. A generated token
changes after every restart and is stored only in the current tab's
`sessionStorage`. After five failed attempts from one IP, wait one minute
before trying again.

### Blank terminal

Check `/api/sessions` through the UI status, confirm that the selected
`terminalId` still exists, and inspect the server log. Confirm that
the selected agent's verification command (`codex --version`,
`claude --version`, or `agy --version`) succeeds for the same operating-system
user. Try a manual Reconnect, then restart only the affected managed session.
Browser privacy extensions that block WebSockets can also cause a blank
terminal.

### Broken ANSI rendering

Use a current browser and do not put a proxy in the path that converts binary
WebSocket messages to text. `convertEol` is intentionally disabled because the
PTY owns line endings. Ensure the proxy supports WebSocket binary frames.

### Resize problems

Leave and re-enter fullscreen or press Reconnect after rotating a phone. Ensure
the terminal container has a nonzero size and the browser page is not zoomed
to an extreme value. The backend rejects sizes outside 20–500 by 5–300.

### PowerShell execution policy

The server prefers `codex.exe`, then `codex.cmd`; it does not select
`codex.ps1`. This avoids the common npm PowerShell shim policy error. If your
manual `codex --version` resolves to a blocked `.ps1`, run `codex.cmd
--version`, fix the user-level execution policy if appropriate, or pass the
`.cmd` path explicitly.

### Windows Firewall

Loopback use normally requires no inbound rule. For LAN or Tailscale use,
create the narrowest inbound rule possible for TCP 8787 and the intended
private interface/profile. Never create a public-profile Internet-wide rule.

### WebSocket connection failure

Check that the URL scheme matches the page: `ws:` for HTTP and `wss:` for
HTTPS. A reverse proxy must forward WebSocket Upgrade and Connection headers
without changing binary frames. The browser Origin hostname and port must
match the public Host header; loopback Vite development origins are the only
cross-port exception.

### Mobile keyboard issues

Tap inside the terminal before typing. Use the mobile toolbar for Escape, Tab,
arrows, Page Up/Down, and Ctrl combinations. On iPhone, rotating the device or
closing the keyboard may briefly animate the visual viewport; the terminal
debounces the resulting resize events.

### Rust cannot find `link.exe`

Install Visual Studio Build Tools with **Desktop development with C++**, then
open Developer PowerShell and rerun the build. Alternatively install a GNU
Rust toolchain and MinGW; `scripts/build.ps1` automatically uses that fallback
when available.

## Known limitations

- The four-session registry contains only PTYs created and owned by the current
  Codex Web Terminal server process.
- Agent discovery is local and read-only. The browser cannot install, update,
  authenticate, or repair a CLI; it only shows vetted host-side instructions.
- Codex Web Terminal cannot retroactively attach to an arbitrary Codex CLI or
  terminal process that was started elsewhere. It does not possess the
  existing process's PTY master handle or input/output pipes.
- Concurrent clients attached to the same managed session share input and can
  interfere with one another. They also share one PTY size; the last accepted
  resize wins, so different viewport sizes can cause redraw or scroll changes.
- The server does not implement TLS. Use a trusted proxy or private overlay
  network for remote transport.
- Replay stores raw PTY output, not a server-side terminal screen model. If
  more than 16 MiB has been retained, the oldest bytes are discarded. A new
  attachment receives only the newest 2 MiB, so a reconnect may reconstruct an
  imperfect screen when required ANSI state is older. A Codex redraw or
  restart repairs it.
- Restarting the Rust server terminates all managed PTY sessions and changes an
  automatically generated token. Saved Codex conversations may be resumed in a
  new PTY, but the previous live terminal process cannot be adopted.
- Browser and mobile operating-system shortcut interception varies. The
  desktop unmodified `/` case is handled explicitly; other reserved shortcuts
  may remain unavailable to xterm.
- Windows termination uses `taskkill /T /F` scoped to the exact PTY root PID,
  followed by the portable-pty child kill and ConPTY handle closure. A process
  that deliberately detaches and escapes that process tree is outside the
  session lifecycle.
- Linux terminate/restart acts on the direct PTY child. A descendant that
  deliberately detaches from that process is not guaranteed to be terminated;
  stopping the documented systemd service with `KillMode=control-group`
  provides service-wide cleanup.
- Native Linux build and PTY runtime are validated on x86_64 Arch Linux. Unix
  command construction is covered by automated tests; macOS has not yet been
  runtime-tested.

## License

MIT. See [LICENSE](LICENSE).
