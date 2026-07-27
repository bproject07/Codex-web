# Operating Codex Web Terminal

This guide explains how to configure, start, use, monitor, stop, upgrade, and
troubleshoot Codex Web Terminal after it has been built.

See [BUILDING.md](BUILDING.md) for compilation and packaging. See
[README.md](README.md) for architecture, protocol, and API details.

## Security model

Codex Web Terminal is remote terminal access. It runs with the permissions and
environment of the operating-system user that starts it. A browser holding the
authentication token can:

- type into the selected agent terminal;
- create, attach to, restart, or terminate managed sessions;
- respond to approval prompts;
- cause an agent to read or modify files allowed to the server user.

Treat the authenticated URL as a credential.

Safe defaults:

- bind to `127.0.0.1`;
- use a newly generated token;
- use Tailscale for access from another device;
- restrict the Tailscale ACL to intended users and devices;
- never expose the port directly to the public Internet;
- never commit or log the token.

## Runtime prerequisites

Before starting the server, verify the primary CLI as the same
operating-system user that will run `codex-web`:

```text
codex --version
codex login
```

Claude Code and AGY are optional. When installed, verify them with:

```text
claude --version
agy --version
```

The server performs the same read-only version probes during agent discovery.
It disables each provider's documented automatic updater only for that probe,
removes parent-agent nesting markers, enforces a three-second deadline, and
publishes only a strictly validated semantic version. On Windows the process
is created suspended, assigned to a kill-on-close Job Object, and then resumed;
on Unix it runs in a dedicated process group. Descendants therefore cannot
outlive a failed or completed probe. Normal interactive agent sessions keep
their usual updater behavior. The server does not copy or manage agent
authentication. Spawned processes inherit the current user's environment and
use that user's existing CLI configuration and credentials.

Choose the project directory deliberately. It is fixed when the server starts
and is inherited by every managed terminal:

```text
--project /absolute/path/to/project
```

The backend canonicalizes this path, verifies that it is a readable directory,
and does not allow the browser to replace it.

### Installing or updating an agent CLI

Installation and updates happen on the **server host**, not on the phone,
laptop, or browser used to view the terminal. The **New** dialog reports each
agent as `ready`, `missing`, or `misconfigured`, displays its installed version
when available, and provides a platform-specific manual command. After running
that command in a trusted host terminal, select **Refresh** or **Check again**
to repeat detection.

Official native installation commands:

| CLI | Windows PowerShell | Linux/macOS |
| --- | --- | --- |
| Codex | `powershell -ExecutionPolicy ByPass -c "irm https://chatgpt.com/codex/install.ps1 \| iex"` | `curl -fsSL https://chatgpt.com/codex/install.sh \| sh` |
| Claude Code | `irm https://claude.ai/install.ps1 \| iex` | `curl -fsSL https://claude.ai/install.sh \| bash` |
| AGY | `irm https://antigravity.google/cli/install.ps1 \| iex` | `curl -fsSL https://antigravity.google/cli/install.sh \| bash` |

Official update commands:

| CLI | Native installation | Package-manager note |
| --- | --- | --- |
| Codex | `codex update` | For an npm installation: `npm install --global @openai/codex@latest` |
| Claude Code | `claude update` | WinGet: `winget upgrade Anthropic.ClaudeCode`; Homebrew: `brew upgrade claude-code` |
| AGY | Re-run the platform install command; it installs or upgrades | AGY also checks for background updates unless `AGY_CLI_DISABLE_AUTO_UPDATE=true` |

Verify the current upstream instructions before executing a downloaded script:
[Codex CLI](https://learn.chatgpt.com/docs/codex/cli),
[Claude Code](https://code.claude.com/docs/en/setup), and
[Antigravity CLI](https://antigravity.google/docs/cli/install).

There is deliberately no silent browser-side Install button and no install
API. A browser session has full terminal input authority already; allowing it
to install or replace host executables would create an additional supply-chain
and privilege boundary. Downloaded scripts may change, invoke package
managers, modify `PATH`, prompt for authentication, or require local policy
review. The operator must review and run them explicitly. **Refresh** and
**Check again** perform only fixed executable discovery and bounded
`--version` probes.

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
| `--command` | derived | Explicit primary executable override; otherwise follows `--primary-agent` |
| `--primary-agent` | `codex` | Agent represented by `--command`: `codex`, `claude`, or `agy` |
| `--new-session-command` | resolved primary command | Optional executable used when **New** starts the primary agent |
| `--codex-command` | unset | Explicit Codex CLI executable override |
| `--claude-command` | unset | Explicit Claude Code executable override |
| `--claude-dangerously-skip-permissions` | off | Start Claude with permission checks bypassed |
| `--agy-command` | unset | Explicit AGY executable override |
| `--agy-dangerously-skip-permissions` | off | Start AGY with tool permission requests auto-approved |
| `--no-agent-auto-detect` | off | Disable discovery of optional agent CLIs |
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
CODEX_WEB_PRIMARY_AGENT
CODEX_WEB_NEW_SESSION_COMMAND
CODEX_WEB_CODEX_COMMAND
CODEX_WEB_CLAUDE_COMMAND
CODEX_WEB_CLAUDE_DANGEROUSLY_SKIP_PERMISSIONS
CODEX_WEB_AGY_COMMAND
CODEX_WEB_AGY_DANGEROUSLY_SKIP_PERMISSIONS
CODEX_WEB_NO_AGENT_AUTO_DETECT
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
independent Codex CLI process when the primary Codex card is selected. Other
agent cards use their own override or default command.

### Agent discovery and explicit profiles

By default, the server probes the executable implied by `--primary-agent` and
auto-detects `codex`, `claude`, and `agy` from `PATH` and their documented
per-user locations. It runs a fixed `--version` probe with bounded output and a
timeout. A ready optional agent is offered in **New**. Missing and
misconfigured agents remain visible in the catalog with manual installation
or repair guidance, but cannot create sessions.

An explicit `--command`, `--codex-command`, `--claude-command`, or
`--agy-command` is authoritative. If it does not resolve or its `--version`
probe fails, the profile is `misconfigured`; the server does not silently fall
back to another binary. `--no-agent-auto-detect` (or
`CODEX_WEB_NO_AGENT_AUTO_DETECT=true`) restricts optional profiles to explicit
configuration. The primary profile is still validated.

The browser cannot provide executable paths, arguments, URLs, or shell syntax.
For a service whose `PATH` differs from an interactive shell, configure
trusted absolute paths:

```powershell
.\scripts\run.ps1 `
  -Project "C:\Projects\my-app" `
  -CodexCommand "$env:APPDATA\npm\codex.cmd" `
  -ClaudeCommand "$HOME\.local\bin\claude.exe" `
  -AgyCommand "$env:LOCALAPPDATA\agy\bin\agy.exe"
```

Each managed terminal is a fresh agent-session boundary. The launcher removes
only the inherited nesting markers `CODEX_THREAD_ID` and `CLAUDECODE` before
version checks and PTY startup; authentication and provider environment
variables remain untouched.

To deliberately auto-approve every tool action for both optional profiles:

```powershell
.\scripts\run.ps1 `
  -Project "C:\Projects\my-app" `
  -ClaudeCommand "$HOME\.local\bin\claude.exe" `
  -ClaudeDangerouslySkipPermissions `
  -AgyCommand "$env:LOCALAPPDATA\agy\bin\agy.exe" `
  -AgyDangerouslySkipPermissions
```

The equivalent direct launches are
`claude --dangerously-skip-permissions` and
`agy --dangerously-skip-permissions`. These modes remove the normal approval
barrier for file changes, commands, network access, and other supported tools.
Use them only when the operating-system account, project directory, network,
credentials, and reachable services are intentionally placed inside the
agent's trust boundary. The switches are off by default.

## Windows command resolution

For a command such as `codex`, `claude`, or `agy`, Windows searches `PATH` and
the documented per-user locations in this extension preference:

1. `<command>.exe`
2. `<command>.cmd`
3. an exact extension already supplied by the caller

`.cmd` entry points are invoked through `cmd.exe /d /s /c call`.
Executable entry points normally use PowerShell unless `--shell cmd` is
selected. `.ps1` shims are intentionally not selected automatically because a
PowerShell execution policy can block npm-generated `.ps1` shims.

## Linux and Unix command resolution

Unix searches the configured `PATH` for the exact executable name and checks
the documented per-user locations during auto-detection. A trusted absolute
path can also be supplied:

```bash
--command /usr/bin/codex
```

After the selected CLI's `--version` succeeds, the resolved executable is
started directly inside the Unix PTY without a shell wrapper.

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
Reconnect attempts use increasing delays and do not restart the selected agent.

### Sessions

The header shows one tab for each server-managed terminal. The active tab is
highlighted and each tab includes a lifecycle-status dot.

- Select a tab to attach the single browser terminal view to that managed PTY.
- Swipe the tab strip horizontally on mobile.
- Use a wheel, trackpad, or the left/right overflow buttons on desktop.
- **+ New** stays beside the tab strip and creates another terminal.
- **Manage** opens the detailed session list.

- **Attach** in the detailed list switches to that managed PTY.
- Attaching does not stop the previously displayed session.
- **Refresh** reloads sanitized session metadata.
- **Remove** terminates and deletes a non-primary managed session.
- The primary `<agent> 1` entry (for example `Codex 1`) cannot be removed.

### New

**+ New** opens **New terminal**. The dialog identifies the server operating
system and architecture and makes clear that the CLI runs on the server host,
not in the viewing browser or phone.

Each agent card reports:

- **Ready** and `Installed version …` when the fixed version probe succeeds;
- **Not found** when no candidate executable resolves;
- **Configuration error** when an explicit override or resolved executable
  fails validation.

Only a ready card provides **Start Codex**, **Start Claude**, or **Start AGY**.
A missing or misconfigured card shows a selectable provider command, **Copy**,
**Official docs**, the required shell, the `--version` verification command,
and **Check again**. Opening **New** always requests a fresh catalog so a tab
cannot keep stale availability from an earlier server generation. The header
**Refresh** and per-card **Check again** make the same
`/api/agent-catalog?refresh=true` request; it never executes the displayed
command. Creation errors remain in the open dialog. A successful create closes
it and attaches the terminal.

On a phone, scroll vertically inside the agent-card list. Each card keeps its
own **Start** action; later cards and their buttons remain reachable without
scrolling the underlying terminal page.

The server allows at most four managed sessions. Each has its own process,
lifecycle, output replay buffer, and connected-client count.

When a dangerous-mode switch is active, the card warns that approvals are
disabled and the agent may edit files and run commands without asking for
confirmation.

### Connect / Reconnect

This button closes and recreates only the browser WebSocket attachment. It is
safe to use after a network interruption or stale screen. It does not restart
or terminate the underlying agent process.

### Restart

**Restart** terminates and recreates the selected agent's PTY. Its stable
`terminalId` remains, but its `sessionId`, PID, and PTY generation change.
Output from the previous generation is not treated as current live output.
On Linux, termination targets the direct PTY child and cannot guarantee cleanup
of a descendant that deliberately detached itself.

### Fullscreen

Requests browser fullscreen mode. Leaving fullscreen does not affect the
server session.

### Desktop slash key

On desktop, pressing an unmodified `/` while a non-editable header control has
focus sends `/` to the connected terminal. This prevents Firefox Quick Find
from taking over after using controls such as Reconnect. Slash remains normal
text in form fields and dialogs, and modified shortcuts such as `Ctrl+/` are
left to the browser or operating system.

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

**Terminate _agent_** stops the selected process without deleting its managed
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
causing the selected agent to redraw or restarting that session repairs it.

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
  the configured agent's `--version`; it is not a continuously refreshed
  installation probe. The field name is retained for API compatibility.
- `sessionRunning: true` means at least one PTY process is actually running.

The frontend can still load while a PTY is failed so diagnostics and restart
controls remain available.

Inspect the complete agent catalog:

```bash
curl \
  -H "Authorization: Bearer $CODEX_WEB_TOKEN" \
  "http://127.0.0.1:8787/api/agent-catalog?refresh=true"
```

Schema version 1 includes `server.os`, `server.arch`, `server.shell`, and one
entry per cataloged agent. All three agents are cataloged by default; optional
agents without explicit overrides are omitted when auto-detection is disabled:

```json
{
  "kind": "claude",
  "state": "ready",
  "configuration": "auto",
  "version": "2.x",
  "dangerouslySkipPermissions": false,
  "install": {
    "command": "platform-specific manual command",
    "shell": "powershell",
    "verifyCommand": "claude --version",
    "updateCommand": "claude update",
    "docsUrl": "https://code.claude.com/docs/en/setup",
    "requiresServerAccess": true
  }
}
```

`version` is `null` when the CLI is not ready. `missing` means no supported
candidate resolved. `misconfigured` means an explicit override or candidate
was found but failed validation. `configuration: "override"` means the
command value is authoritative and never falls back to another executable.
Repairing that exact file or its permissions can be followed immediately by
**Check again**. Changing or removing the startup override value requires
updating the server configuration and restarting the server; running a generic
installer alone does not replace an override. The legacy `/api/agents`
endpoint lists only ready kinds and exists for older frontends.

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

Server restarts terminate all managed PTYs. Save or finish important agent work
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
remains available after PTY startup fails. Confirm the selected agent's
`--version` command as the same OS user, inspect `/api/agent-catalog`, and
check `sessionRunning`.

### An agent is not installed or has a configuration error

Use the **New terminal** card's shell, command, and verification instructions
on the server host. **Not found** means no supported executable resolved.
**Configuration error** commonly means an explicit path is wrong, the file is
not executable, or `--version` failed. Explicit overrides never fall back.

After correcting the host installation, select **Refresh** or **Check again**.
If startup flags or service environment variables changed, restart the server
first; understand that restarting destroys its in-memory PTYs. Do not paste
the displayed install command into the browser developer console.

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
command -v claude
claude --version
command -v agy
agy --version
test -r /dev/ptmx
```

Check only the CLIs you intend to use. Also verify executable permission on the
catalog's resolved command and read access to the project directory.

### Windows reports a PowerShell policy error

Use `where.exe codex`, `where.exe claude`, or `where.exe agy`. Prefer a
discovered `.exe` or `.cmd` entry point. The application deliberately avoids
automatic `.ps1` selection.

### Mobile viewport or keyboard behavior is unusual

Use **Settings → Mobile viewport diagnostics**, tap the terminal, wait for
collection to finish, and copy the diagnostic JSON. Record the browser,
device, orientation, and exact interaction sequence. Diagnostics intentionally
exclude terminal content and credentials.

## Operational checklist

Before exposing the service to another device:

- [ ] The primary CLI is ready and authenticated for the service user.
- [ ] Optional agent status/version and manual commands match the server OS.
- [ ] Dangerous permission-bypass flags are off unless explicitly required.
- [ ] The project directory is correct and no broader than intended.
- [ ] The build and tests passed on the target platform.
- [ ] The package contains the matching executable and `web` assets.
- [ ] The token is strong, private, and not committed.
- [ ] The bind address is loopback or a private/Tailscale interface.
- [ ] Firewall and Tailscale ACL scope is understood.
- [ ] `/api/health` reports a running session.
- [ ] The browser can attach, type, reconnect, and replay.
- [ ] A precise stop procedure is known.
