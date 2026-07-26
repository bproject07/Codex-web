# Operating Codex Web Terminal

This guide explains how to configure, start, use, monitor, stop, upgrade, and
troubleshoot Codex Web Terminal after it has been built.

See [BUILDING.md](BUILDING.md) for compilation and packaging. See
[README.md](README.md) for architecture, protocol, and API details.

## Security model

Codex Web Terminal is remote terminal access. It runs with the permissions and
environment of the operating-system user that starts it. A browser holding the
authentication token can:

- type into the selected Codex terminal;
- create, attach to, restart, or terminate managed sessions;
- respond to approval prompts;
- cause Codex to read or modify files allowed to the server user.

Treat the authenticated URL as a credential.

Safe defaults:

- bind to `127.0.0.1`;
- use a newly generated token;
- use Tailscale for access from another device;
- restrict the Tailscale ACL to intended users and devices;
- never expose the port directly to the public Internet;
- never commit or log the token.

## Runtime prerequisites

Before starting the server as the intended operating-system user:

```text
codex --version
codex login
```

The server does not copy or manage Codex authentication. The spawned process
inherits the current user's environment and uses that user's existing Codex
configuration.

Choose the project directory deliberately. It is fixed when the server starts
and is inherited by every managed terminal:

```text
--project /absolute/path/to/project
```

The backend canonicalizes this path, verifies that it is a readable directory,
and does not allow the browser to replace it.

## First local start on Windows

Build first:

```powershell
.\scripts\build.ps1
```

Start on loopback with an automatically generated token:

```powershell
.\scripts\run.ps1 `
  -Project "C:\Projects\my-app" `
  -ListenHost "127.0.0.1" `
  -Port 8787
```

The server prints an authenticated URL. Open the complete URL in the browser.
The frontend moves the token into the current tab's `sessionStorage` and
removes it from the visible address bar.

## First local start on Linux

Build first:

```bash
./scripts/build.sh
```

Start on loopback:

```bash
./scripts/run.sh "/home/user/projects/my-app" \
  --host 127.0.0.1 \
  --port 8787
```

Or run the package directly:

```bash
./dist-linux/codex-web \
  --project "/home/user/projects/my-app" \
  --host 127.0.0.1 \
  --port 8787
```

## Authentication token lifecycle

If `--token` and `CODEX_WEB_TOKEN` are both omitted, the server generates a
strong ephemeral token. It changes on every server restart.

Use an explicit token when a bookmark, service, or planned reconnect must keep
working across restarts. Tokens must:

- contain at least 16 characters;
- contain no whitespace;
- remain at or below 512 bytes.

Generate a URL-safe 256-bit token on Windows PowerShell:

```powershell
$tokenBytes = New-Object byte[] 32
$generator = [System.Security.Cryptography.RandomNumberGenerator]::Create()
$generator.GetBytes($tokenBytes)
$generator.Dispose()
$env:CODEX_WEB_TOKEN = [Convert]::ToBase64String($tokenBytes).
  TrimEnd("=").
  Replace("+", "-").
  Replace("/", "_")
```

Generate one on Linux:

```bash
export CODEX_WEB_TOKEN="$(python3 -c 'import secrets; print(secrets.token_urlsafe(32))')"
```

Prefer an environment variable over a command-line token on multi-user
machines because process command lines can be visible to other local users.
For a service, store the token in a permission-restricted environment file.

The browser stores the token only in the current tab's `sessionStorage`.
Closing that tab or choosing **Forget token** removes the browser copy. It does
not stop the server or change the server-side token.

## Command-line options

| Option | Default | Meaning |
| --- | --- | --- |
| `--host` | `127.0.0.1` | Address on which the HTTP server listens |
| `--port` | `8787` | TCP port |
| `--project` | current directory | Fixed working directory for every managed PTY |
| `--command` | `codex` | Executable for the primary terminal |
| `--new-session-command` | `--command` | Optional executable for terminals created with **New** |
| `--shell` | `powershell` | Windows wrapper (`powershell` or `cmd`); ignored on Unix |
| `--token` | generated | Explicit authentication token |
| `--no-open-browser` | off | Prevent automatic browser launch |
| `--log-level` | `info` | Rust `tracing` filter |

Equivalent environment variables:

```text
CODEX_WEB_HOST
CODEX_WEB_PORT
CODEX_WEB_PROJECT_DIR
CODEX_WEB_COMMAND
CODEX_WEB_NEW_SESSION_COMMAND
CODEX_WEB_SHELL
CODEX_WEB_TOKEN
CODEX_WEB_LOG_LEVEL
```

Command-line arguments override environment variables.

The command values are executable names or paths, not shell expressions. Do
not pass pipes, redirections, command substitutions, or chained commands.

If the primary terminal is launched by a trusted wrapper that resumes one
specific Codex thread, also set `--new-session-command codex`. The primary
terminal will use the resume wrapper, while the **New** button will start an
independent Codex CLI process.

## Windows command resolution

For a command such as `codex`, Windows searches `PATH` in this preference:

1. `codex.exe`
2. `codex.cmd`
3. an exact extension already supplied by the caller

`.cmd` entry points are invoked through `cmd.exe /d /s /c call`.
Executable entry points normally use PowerShell unless `--shell cmd` is
selected. `codex.ps1` is intentionally not selected automatically because a
PowerShell execution policy can block npm-generated `.ps1` shims.

## Linux and Unix command resolution

Unix searches the configured `PATH` for the exact executable name. An absolute
path can also be supplied:

```bash
--command /usr/bin/codex
```

After `codex --version` succeeds, the resolved executable is started directly
inside the Unix PTY without a shell wrapper.

## Access over Tailscale

Install and connect Tailscale on both the server and browser devices:

```bash
tailscale status
tailscale ip -4
```

The safest direct bind is the server's specific Tailscale IP:

```bash
export CODEX_WEB_TOKEN="$(python3 -c 'import secrets; print(secrets.token_urlsafe(32))')"

./dist-linux/codex-web \
  --project "/home/user/projects/my-app" \
  --host "100.x.y.z" \
  --port 8787 \
  --no-open-browser
```

On Windows, use the equivalent `-ListenHost` option:

```powershell
.\scripts\run.ps1 `
  -Project "C:\Projects\my-app" `
  -ListenHost "100.x.y.z" `
  -Port 8787 `
  -NoOpenBrowser
```

`run.ps1` and the server inherit `CODEX_WEB_TOKEN`; omitting `-Token` keeps the
credential out of the process command line.

Open:

```text
http://100.x.y.z:8787/?token=YOUR_TOKEN
```

Binding to `0.0.0.0` also works, but exposes the port on every IPv4 interface.
Use it only when firewall rules and the surrounding network are understood:

```bash
./dist-linux/codex-web \
  --project "/home/user/projects/my-app" \
  --host 0.0.0.0 \
  --port 8787 \
  --no-open-browser
```

Tailscale provides private transport, but the application URL is still HTTP
unless an HTTPS reverse proxy is added. Restrict access with Tailscale ACLs and
never add a public router port-forward.

### Reverse proxy origin requirement

A backend bound to `127.0.0.1` accepts only loopback browser origins. Therefore,
an HTTPS reverse proxy serving a non-loopback hostname must use a backend bound
to either:

- the exact private or Tailscale address reachable by the proxy; or
- `0.0.0.0`, protected by a host firewall that admits only the intended
  interface and proxy.

Preserve the browser-facing `Host` header and port. Forward WebSocket Upgrade
and Connection headers and binary frames without conversion. Otherwise the
strict WebSocket Origin check rejects the connection.

### Sharing the current server

The current token is server-wide, not per-user or per-session. Anyone who
receives it has the same ability as the owner to list, view, type into, create,
restart, terminate, and remove eligible managed sessions.

Multiple browsers attached to one session share both input and PTY dimensions.
The latest valid resize wins, so desktop and mobile clients with different
viewport sizes can trigger redraw or scroll changes for each other.

Share the authenticated URL only when full read/write terminal access is
intended. Read-only links, expiring share links, per-session grants, and
revocation without rotating the server token are future work described in
[TODO.md](TODO.md).

## Browser interface

### Connection status

The header status combines the HTTP session lifecycle and the browser
WebSocket state. A healthy attached session should reach **Connected**.
Reconnect attempts use increasing delays and do not restart Codex.

### Sessions

**Sessions** opens the list of server-managed terminals.

- **Attach** switches the single browser terminal view to another managed PTY.
- Attaching does not stop the previously displayed session.
- **Refresh** reloads sanitized session metadata.
- **Remove** terminates and deletes a non-primary managed session.
- The primary `Terminal 1` entry cannot be removed.

### New

**+ New** creates another independent Codex PTY. The server allows at most four
managed sessions. Each has its own process, lifecycle, output replay buffer,
and connected-client count.

### Connect / Reconnect

This button closes and recreates only the browser WebSocket attachment. It is
safe to use after a network interruption or stale screen. It does not restart
or terminate the underlying Codex process.

### Restart

**Restart Codex** terminates and recreates the selected PTY. Its stable
`terminalId` remains, but its `sessionId`, PID, and PTY generation change.
Output from the previous generation is not treated as current live output.
On Linux, termination targets the direct PTY child and cannot guarantee cleanup
of a descendant that deliberately detached itself.

### Fullscreen

Requests browser fullscreen mode. Leaving fullscreen does not affect the
server session.

### Mobile keys

Shows or hides the mobile toolbar. Its order begins with Enter and the arrow
keys, followed by Page Up/Down, Ctrl mode, Esc, Tab, Ctrl+C, Ctrl+L, Top, Live,
and Hide.

- **PgUp/PgDn** moves through xterm's client-side scrollback.
- **Top** moves to the oldest retained client-side line.
- **Live** returns to current terminal output.
- **Ctrl** applies Ctrl to the next typed ASCII letter and then turns off.
- **Hide** hides the toolbar; it can be shown again from the header.

### Settings

Settings control font size, client scrollback, theme, cursor blinking, and the
mobile toolbar. **Copy diagnostics** captures mobile viewport measurements
after terminal focus; it does not include the authentication token, keyboard
input, or terminal text.

**Terminate Codex** stops the selected process without deleting its managed
entry. **Forget token** removes the token from the current browser tab.

## Reconnect and replay behavior

Closing the browser or losing connectivity does not terminate managed PTYs.
The server retains up to 16 MiB of raw PTY output per session. A newly attached
browser receives at most the newest 2 MiB and then:

1. receives the current sanitized session snapshot;
2. resets the xterm screen;
3. receives a bounded replay;
4. transitions to live output without a replay/live gap.

xterm also keeps client-side scrollback, 10,000 lines by default. The server
buffer contains raw ANSI bytes rather than a rendered screen model. If the
oldest ANSI state has been discarded, a very old replay can look imperfect;
causing Codex to redraw or restarting the selected session repairs it.

## Health and diagnostics

All HTTP API requests require a bearer token:

```bash
curl \
  -H "Authorization: Bearer $CODEX_WEB_TOKEN" \
  http://127.0.0.1:8787/api/health
```

Healthy output has:

```json
{
  "status": "ok",
  "codexInstalled": true,
  "sessionRunning": true,
  "connectedClients": 0,
  "sessionCount": 1,
  "runningSessions": 1,
  "maxSessions": 4
}
```

Important distinction:

- `codexInstalled: true` means the most recent preflight for at least one
  registered session successfully resolved the command and ran
  `codex --version`; it is not a continuously refreshed installation probe.
- `sessionRunning: true` means at least one PTY process is actually running.

The frontend can still load while a PTY is failed so diagnostics and restart
controls remain available.

List sessions:

```bash
curl \
  -H "Authorization: Bearer $CODEX_WEB_TOKEN" \
  http://127.0.0.1:8787/api/sessions
```

Do not paste production health commands containing real tokens into tickets,
chat messages, or shared logs.

## Logging

Structured `tracing` output includes:

- bind address and project directory;
- PTY startup and PID when available;
- client connect/disconnect;
- restart and process exit;
- sanitized errors.

It deliberately excludes tokens, keyboard input, terminal output, Codex
credentials, and authentication files. The separate startup `println!`
intentionally prints the complete authenticated URL. If stdout is redirected
to a file or journal, that destination contains the token.

Select verbosity:

```bash
--log-level info
--log-level codex_web_terminal=debug
```

Debug logging remains server-level; it does not enable terminal-content
logging.

## Linux user service example

For a persistent single-user installation, place the package in a stable
directory such as:

```text
/home/alice/apps/codex-web/
├── codex-web
└── web/
```

Create a protected environment file:

```bash
install -d -m 0700 "$HOME/.config/codex-web"
umask 077
python3 -c 'import secrets; print("CODEX_WEB_TOKEN=" + secrets.token_urlsafe(32))' \
  > "$HOME/.config/codex-web/environment"
chmod 0600 "$HOME/.config/codex-web/environment"
```

Create `~/.config/systemd/user/codex-web.service`:

```ini
[Unit]
Description=Codex Web Terminal
After=network-online.target

[Service]
Type=simple
WorkingDirectory=%h/projects/my-app
EnvironmentFile=%h/.config/codex-web/environment
ExecStart=%h/apps/codex-web/codex-web --host 127.0.0.1 --port 8787 --project %h/projects/my-app --command /absolute/path/to/codex --no-open-browser
Restart=on-failure
RestartSec=3
KillSignal=SIGINT
KillMode=control-group
StandardOutput=null
StandardError=journal

[Install]
WantedBy=default.target
```

Load and start it:

```bash
systemctl --user daemon-reload
systemctl --user enable --now codex-web.service
systemctl --user status codex-web.service
```

Replace `/absolute/path/to/codex` with the result of `command -v codex` for the
service user. Using an absolute path avoids differences between an interactive
shell's `PATH` and the systemd user-manager environment.

This hardened example discards stdout because startup stdout contains the
authenticated URL. It also discards the normal structured tracing stream,
which currently uses stdout. Service state is still available with:

```bash
systemctl --user status codex-web.service
```

For detailed diagnostics, stop the unit and run the same command interactively
in a private terminal. If `StandardOutput=journal` is enabled temporarily,
understand that the journal will retain the complete token-bearing startup URL
until it is rotated or vacuumed.

Stop:

```bash
systemctl --user stop codex-web.service
```

To reach this service over Tailscale, replace `127.0.0.1` in `ExecStart` with
the machine's specific Tailscale IP, then run `daemon-reload` and restart the
unit. Avoid `0.0.0.0` unless all interfaces are intentionally in scope.

## Normal shutdown

For an interactive server, press Ctrl+C in the server console. The server
stops its managed sessions and closes the HTTP listener.

For a user service:

```bash
systemctl --user stop codex-web.service
```

After shutdown, verify that the port is no longer listening:

```bash
ss -ltn
```

On Windows:

```powershell
Get-NetTCPConnection -State Listen -LocalPort 8787 -ErrorAction SilentlyContinue
```

Do not kill unrelated processes merely because they use a similar name.
Resolve the exact service, PID, and port first.

## Upgrade procedure

Server restarts terminate all managed PTYs. Save or finish important Codex work
before upgrading.

Recommended sequence:

1. Pull or check out the desired reviewed commit.
2. Run the full Windows and Linux build/test matrix from
   [BUILDING.md](BUILDING.md). Every code update must pass both platforms.
3. Keep the old package until the new one has passed validation.
4. Stop the existing server.
5. Replace the executable and the entire adjacent `web` directory together.
6. Start the new server with the same project and explicit token if continuity
   of the browser URL and credential is required. This does not preserve PTY
   processes or live terminal sessions.
7. Check `/api/health`, load the frontend, attach, and verify keyboard input.
8. Remove the old package only after the new runtime is confirmed.

Do not mix a new frontend with an older backend during deployment.
Before deployment, confirm that the Markdown documentation still matches the
current commands, versions, package contents, supported platforms, behavior,
and known limitations.

## Troubleshooting

### The URL opens but the terminal is blank

Check the session snapshot and server log. The HTTP server intentionally
remains available after PTY startup fails. Confirm `codex --version` as the
same OS user and check `sessionRunning`.

### Authentication fails

Use the URL from the current server instance. A generated token changes after
restart. Five repeated failures from one address trigger a temporary
one-minute block.

### Browser reconnect loops

Confirm:

- the server process and port are active;
- the URL host, port, and scheme are correct;
- a reverse proxy forwards WebSocket Upgrade and binary frames;
- the page Origin host matches the public Host;
- browser extensions are not blocking WebSockets.

### Tailscale URL is unreachable

Check both devices:

```bash
tailscale status
tailscale ping SERVER_NAME
```

Verify that the server bound either the exact Tailscale IP or an intentionally
broader address. Check Tailscale ACLs and the host firewall.

### Linux reports `sessionRunning: false`

Check:

```bash
command -v codex
codex --version
test -r /dev/ptmx
```

Also verify executable permission on the resolved command and read access to
the project directory.

### Windows reports a PowerShell policy error

Use `where.exe codex`. Prefer the discovered `codex.exe` or `codex.cmd` entry
point. The application deliberately avoids automatic `codex.ps1` selection.

### Mobile viewport or keyboard behavior is unusual

Use **Settings → Mobile viewport diagnostics**, tap the terminal, wait for
collection to finish, and copy the diagnostic JSON. Record the browser,
device, orientation, and exact interaction sequence. Diagnostics intentionally
exclude terminal content and credentials.

## Operational checklist

Before exposing the service to another device:

- [ ] Codex CLI is authenticated for the service user.
- [ ] The project directory is correct and no broader than intended.
- [ ] The build and tests passed on the target platform.
- [ ] The package contains the matching executable and `web` assets.
- [ ] The token is strong, private, and not committed.
- [ ] The bind address is loopback or a private/Tailscale interface.
- [ ] Firewall and Tailscale ACL scope is understood.
- [ ] `/api/health` reports a running session.
- [ ] The browser can attach, type, reconnect, and replay.
- [ ] A precise stop procedure is known.
