# Codex Web Terminal

Codex Web Terminal runs the **real Codex CLI** in a Windows pseudo-terminal
(ConPTY) and exposes that terminal to a browser through an authenticated
WebSocket. It does not reimplement or scrape the Codex interface. ANSI output,
cursor movement, menus, approval prompts, diffs, spinners, keyboard input, and
the rest of the Codex TUI are interpreted by xterm.js from the original PTY
byte stream.

The project is an independent wrapper. It does not download, vendor, modify,
or update the OpenAI Codex source code.

> **This application provides remote terminal access.**
> **Do not expose it directly to the public internet.**

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
Up to 4 Windows ConPTY sessions
          │
          ├── cmd.exe for codex.cmd
          └── PowerShell or cmd.exe for codex.exe
                    │
                    ▼
                Codex CLI per session
```

The server owns up to four independent, web-managed Codex PTY sessions. The
browser displays one selected session in the same xterm screen and can switch
between them without terminating the others. Closing a tab or losing the
network does not terminate those sessions. A reconnecting browser resets xterm
and replays the selected session's bounded terminal output buffer before it
resumes live output.

## Current scope

- Up to four persistent, web-managed Codex processes per server
- One primary session named `Terminal 1`, started automatically
- Up to four authenticated WebSocket clients per terminal session
- Fixed project directory selected when the server starts
- One 2 MiB bounded raw terminal output buffer per session
- Initial PTY size of 120 columns by 35 rows
- Validated browser resize range: 20–500 columns and 5–300 rows
- Windows 10/11 is the primary platform
- No built-in TLS, reverse proxy, or tunnel

Clients attached to the same terminal session share its PTY and can type into
it concurrently. The authenticated URL grants access to every managed session,
so do not share it with people who should not have terminal access.

## Prerequisites

### Windows

- Windows 10 version 1809 or newer, or Windows 11, for ConPTY
- PowerShell 5.1 or newer
- A supported browser: current Chrome, Edge, Firefox, or Safari on mobile

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

Install Node.js 20.19 or newer with npm, then verify:

```powershell
node --version
npm --version
```

### Codex CLI

Codex Web Terminal expects an existing Codex installation in the normal user
environment. One supported installation method is:

```powershell
npm install --global @openai/codex
```

Alternatively, follow the current
[Codex CLI documentation](https://learn.chatgpt.com/docs/codex/cli).

Verify the entry point and authenticate it before starting the server:

```powershell
codex --version
codex login
```

`codex login` uses the normal Codex browser sign-in. Codex Web Terminal neither
copies nor reads Codex credentials. The child process inherits the current
user environment and uses the existing Codex configuration and login.

## Quick start

From the repository root:

```powershell
.\scripts\build.ps1
.\scripts\run.ps1 -Project "C:\Projects\my-app"
```

The server starts on `127.0.0.1:8787`, generates an ephemeral cryptographically
secure token, and prints an authenticated URL once:

```text
Codex Web Terminal started

Local URL:
http://127.0.0.1:8787/?token=...
```

Open that URL. The frontend moves the token into `sessionStorage` and removes it
from the visible address. The token is not put in `localStorage`.

Use **Sessions** to see the managed Codex terminals, **New** to create another
live Codex PTY, and **Attach** to display an existing one in the same xterm
screen. Attaching does not stop the previously displayed session; it continues
running and buffering output in the background.

The generated token changes whenever the server restarts. Supply `--token` or
`CODEX_WEB_TOKEN` when a stable token is required.

## Development

Use two PowerShell terminals.

Terminal 1:

```powershell
cd server
cargo run -- --project "C:\Projects\my-app" --no-open-browser
```

Terminal 2:

```powershell
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
rustup run stable-x86_64-pc-windows-gnu cargo run -- `
  --project "C:\Projects\my-app" `
  --no-open-browser
```

Replace the toolchain name with the exact installed name shown by
`rustup toolchain list`.

## Production build

The manual workflow is:

```powershell
cd web
npm install
npm run build

cd ..\server
cargo build --release
```

The automated workflow is:

```powershell
.\scripts\build.ps1
```

It checks Node.js, npm, Rust, builds both applications, and creates:

```text
dist/
├── codex-web.exe
├── web/
│   ├── index.html
│   └── assets/
├── README.md
└── LICENSE
```

Keep the `web` directory next to `codex-web.exe`. The executable serves those
assets. During `cargo run`, the backend also looks for `web/dist`.

## Command-line interface

```powershell
.\dist\codex-web.exe --help
.\dist\codex-web.exe --version
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

Trusted network or Tailscale example:

```powershell
.\dist\codex-web.exe `
  --project "C:\Projects\my-app" `
  --host 0.0.0.0 `
  --port 8787 `
  --token "replace-with-a-long-random-token"
```

Supported arguments:

| Argument | Purpose |
| --- | --- |
| `--host` | Bind address; defaults to `127.0.0.1` |
| `--port` | TCP port; defaults to `8787` |
| `--project` | Fixed Codex working directory |
| `--shell` | `powershell` or `cmd` |
| `--command` | Executable name or path; defaults to `codex` |
| `--token` | Authentication token, minimum 16 characters |
| `--no-open-browser` | Do not launch the default browser |
| `--log-level` | tracing filter such as `info` or `debug` |

The command value is treated as an executable name or file path, not as an
arbitrary shell expression. A discovered `.cmd` entry point is always invoked
through `cmd.exe /d /s /c`, which is required for the npm Codex package.

## Environment variables

CLI arguments override environment variables.

| Variable | Default |
| --- | --- |
| `CODEX_WEB_HOST` | `127.0.0.1` |
| `CODEX_WEB_PORT` | `8787` |
| `CODEX_WEB_PROJECT_DIR` | Current directory |
| `CODEX_WEB_TOKEN` | Secure random token generated at startup |
| `CODEX_WEB_COMMAND` | `codex` |
| `CODEX_WEB_SHELL` | `powershell` |
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
| `GET` | `/api/sessions` | List sanitized metadata for all managed sessions |
| `POST` | `/api/sessions` | Create and start a new managed Codex PTY |
| `GET` | `/api/sessions/{terminalId}` | Get one managed session |
| `POST` | `/api/sessions/{terminalId}/restart` | Terminate and recreate one Codex PTY |
| `POST` | `/api/sessions/{terminalId}/terminate` | Terminate one Codex PTY without removing its entry |
| `DELETE` | `/api/sessions/{terminalId}` | Terminate and remove a non-primary session |
| `GET` | `/api/session` | Legacy alias for primary-session metadata |
| `POST` | `/api/session/restart` | Legacy alias for restarting the primary session |
| `POST` | `/api/session/terminate` | Legacy alias for terminating the primary session |
| `GET` | `/ws?token=...&terminalId=...` | Attach an authenticated WebSocket to one managed terminal |

No response contains the authentication token, terminal input, terminal
output, Codex credentials, or Codex authentication files.

Each session snapshot contains a stable `terminalId`, display `name`,
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

The mobile toolbar adds Esc, Tab, arrows, Enter, Page Up/Down, Ctrl+C, and
Ctrl+L. Its Ctrl mode converts the next typed ASCII letter to the matching
control character, then automatically turns off.

The header's **Sessions**, **New**, and **Attach** controls manage independent
live PTYs. They switch which managed session feeds the same xterm screen; they
do not send `/new` or `/resume` commands into the Codex TUI.

## Security

This process has the same operating-system permissions and environment as the
user who starts it. Anyone with the authenticated URL can interact with Codex,
approve actions presented by Codex, and potentially cause commands to run in
the configured project.

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
- 2 MiB output-buffer limit
- Content Security Policy, frame denial, no-referrer policy, and MIME sniffing
  protection
- no logging of token, keys, input, or terminal output

The URL initially contains a credential. Do not paste it into chat, logs,
screenshots, analytics, issue trackers, or browser-sync services.

For remote use:

1. Prefer [Tailscale](https://tailscale.com/) between devices.
2. Otherwise use a trusted private LAN.
3. Put an HTTPS reverse proxy in front of the server when transport leaves the
   local machine.
4. Add another authentication layer before considering Cloudflare Tunnel.

Do not automatically expose the port with a public tunnel or router port
forward. This project intentionally contains no such feature.

## Tailscale example

Start the server with an explicit strong token:

```powershell
$env:CODEX_WEB_TOKEN = "generate-and-paste-a-long-random-token"
.\dist\codex-web.exe `
  --project "C:\Projects\my-app" `
  --host 0.0.0.0 `
  --port 8787
```

Use the machine's Tailscale IP or MagicDNS name from another enrolled device:

```text
http://my-windows-pc:8787/?token=...
```

Restrict access with Tailscale ACLs. For stronger browser transport security,
put an HTTPS reverse proxy on the Tailscale interface.

## Logging

`tracing` records server address, project directory, PTY startup, PID when
available, client connection/disconnection, restart, process exit, and
sanitized errors.

It deliberately does not record:

- authentication tokens
- pressed keys or terminal input
- terminal output
- Codex credentials or authentication files

Use `--log-level debug` only for server-level diagnostics; terminal content is
still excluded.

## Tests

Frontend:

```powershell
cd web
npm test
npm run build
```

Rust:

```powershell
cd server
cargo fmt --all -- --check
cargo test --all-targets
cargo clippy --all-targets -- -D warnings
```

Tests cover token validation and throttling, config validation, resize and
control-message parsing, session state transitions, the bounded output buffer,
Windows `.cmd` invocation with spaces, control encoding, reconnect
state/backoff, UTF-8 input, and mobile Ctrl conversion. The automated tests do
not require a real Codex process.

## Troubleshooting

### `codex` not found

Run:

```powershell
where.exe codex
codex --version
```

For an npm installation, `where.exe` should normally show `codex.cmd`. Restart
PowerShell after installation so it receives the updated `PATH`. You can also
pass a trusted absolute path with `--command`.

### Authentication failed

Use the complete URL printed by the current server process. A generated token
changes after every restart and is stored only in the current tab's
`sessionStorage`. After five failed attempts from one IP, wait one minute
before trying again.

### Blank terminal

Check `/api/sessions` through the UI status, confirm that the selected
`terminalId` still exists, and inspect the server log. Confirm that
`codex --version` succeeds for the same Windows user. Try a manual Reconnect,
then restart only the affected managed session. Browser privacy extensions
that block WebSockets can also cause a blank terminal.

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
- Codex Web Terminal cannot retroactively attach to an arbitrary Codex CLI or
  Windows Terminal process that was started elsewhere. It does not possess the
  existing process's ConPTY master handle or input/output pipes.
- Concurrent clients attached to the same managed session share input and can
  interfere with one another.
- The server does not implement TLS. Use a trusted proxy or private overlay
  network for remote transport.
- Replay stores raw PTY output, not a server-side terminal screen model. If
  more than 2 MiB has been emitted, the oldest ANSI state is discarded and a
  reconnect may reconstruct an imperfect screen. A Codex redraw or restart
  repairs it.
- Restarting the Rust server terminates all managed PTY sessions and changes an
  automatically generated token. Saved Codex conversations may be resumed in a
  new PTY, but the previous live terminal process cannot be adopted.
- Browser and mobile operating-system shortcut interception varies. xterm
  receives only shortcuts the browser does not reserve.
- Windows termination uses `taskkill /T /F` scoped to the exact PTY root PID,
  followed by the portable-pty child kill and ConPTY handle closure. A process
  that deliberately detaches and escapes that process tree is outside the
  session lifecycle.
- Linux and macOS may work through portable-pty, but Windows command discovery
  and ConPTY are the tested priority.

## License

MIT. See [LICENSE](LICENSE).
